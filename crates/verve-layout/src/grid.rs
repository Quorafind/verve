//! CSS grid's text: the track lists of `grid-template-columns`, the named areas of
//! `grid-template-areas`, and where an item says it goes.
//!
//! Taffy takes tracks as typed values and placements as line numbers, so the text is read here.
//! Named areas become line numbers (taffy 0.13 has no `grid-area: name`), and named lines go to
//! taffy's own line-name tables.

use std::collections::HashMap;

use taffy::style_helpers::{line, span};
use taffy::{
    GridPlacement, GridTemplateComponent, GridTemplateRepetition, MaxTrackSizingFunction, MinMax,
    MinTrackSizingFunction, RepetitionCount, TrackSizingFunction,
};

use crate::value::{Dim, Units, call_arguments, length_percent, number, split_outside, words};

type Placement = GridPlacement<String>;

/// A track list as taffy takes it, with the names of the lines between the tracks.
#[derive(Default)]
pub(crate) struct Template {
    pub tracks: Vec<GridTemplateComponent<String>>,
    /// `names[i]` are the names of line `i` (the line before track `i`), counting from 0.
    pub names: Vec<Vec<String>>,
}

/// A track list: `200px 1fr`, `repeat(3, minmax(0, 1fr))`, `[full-start] 1fr [full-end]`.
pub(crate) fn template(text: &str, units: &Units) -> Template {
    let mut result = Template::default();
    let mut pending: Vec<String> = Vec::new();
    for word in words(text) {
        if let Some(inner) = word.strip_prefix('[').and_then(|w| w.strip_suffix(']')) {
            pending.extend(inner.split_whitespace().map(str::to_owned));
        } else if let Some(arguments) = call_arguments(word, "repeat") {
            repeat(arguments, units, &mut result, &mut pending);
        } else if let Some(track) = track(word, units) {
            push_track(
                &mut result,
                &mut pending,
                GridTemplateComponent::Single(track),
            );
        } else {
            // `none`, `subgrid`, `masonry`, or text that is no track list.
            return Template::default();
        }
    }
    result.names.resize(result.tracks.len() + 1, Vec::new());
    let last = result.tracks.len();
    result.names[last].append(&mut pending);
    result
}

fn push_track(
    result: &mut Template,
    pending: &mut Vec<String>,
    track: GridTemplateComponent<String>,
) {
    result.names.resize(result.tracks.len() + 1, Vec::new());
    let at = result.tracks.len();
    result.names[at].append(pending);
    result.tracks.push(track);
}

fn repeat(arguments: &str, units: &Units, result: &mut Template, pending: &mut Vec<String>) {
    let Some((count, body)) = arguments.split_once(',') else {
        return;
    };
    let tracks: Vec<TrackSizingFunction> = words(body)
        .into_iter()
        .filter(|word| !word.starts_with('['))
        .filter_map(|word| track(word, units))
        .collect();
    if tracks.is_empty() {
        return;
    }
    let kind = match count.trim() {
        "auto-fill" => Some(RepetitionCount::AutoFill),
        "auto-fit" => Some(RepetitionCount::AutoFit),
        _ => None,
    };
    if let Some(count) = kind {
        let names = vec![Vec::new(); tracks.len() + 1];
        push_track(
            result,
            pending,
            GridTemplateComponent::Repeat(GridTemplateRepetition {
                count,
                tracks,
                line_names: names,
            }),
        );
        return;
    }
    let times = number(count).map_or(0, |n| n.clamp(0., 1000.) as usize);
    for _ in 0..times {
        for track in &tracks {
            push_track(result, pending, GridTemplateComponent::Single(*track));
        }
    }
}

/// The tracks of `grid-auto-rows` / `grid-auto-columns`.
pub(crate) fn auto_tracks(text: &str, units: &Units) -> Vec<TrackSizingFunction> {
    words(text)
        .into_iter()
        .filter_map(|word| track(word, units))
        .collect()
}

fn minmax(min: MinTrackSizingFunction, max: MaxTrackSizingFunction) -> TrackSizingFunction {
    MinMax { min, max }
}

fn track(word: &str, units: &Units) -> Option<TrackSizingFunction> {
    if let Some(arguments) = call_arguments(word, "minmax") {
        let (min, max) = arguments.split_once(',')?;
        return Some(minmax(
            min_track(min.trim(), units)?,
            max_track(max.trim(), units)?,
        ));
    }
    if let Some(argument) = call_arguments(word, "fit-content") {
        let limit = match length_percent(argument, units)? {
            Dim::Px(px) => MaxTrackSizingFunction::fit_content_px(px),
            Dim::Percent(f) => MaxTrackSizingFunction::fit_content_percent(f),
            Dim::Auto => return None,
        };
        return Some(minmax(MinTrackSizingFunction::auto(), limit));
    }
    if let Some(fr) = word.strip_suffix("fr") {
        return Some(minmax(
            MinTrackSizingFunction::auto(),
            MaxTrackSizingFunction::fr(number(fr)?),
        ));
    }
    Some(minmax(min_track(word, units)?, max_track(word, units)?))
}

fn min_track(word: &str, units: &Units) -> Option<MinTrackSizingFunction> {
    Some(match word {
        "auto" => MinTrackSizingFunction::auto(),
        "min-content" => MinTrackSizingFunction::min_content(),
        "max-content" => MinTrackSizingFunction::max_content(),
        other => match length_percent(other, units)? {
            Dim::Px(px) => MinTrackSizingFunction::length(px),
            Dim::Percent(f) => MinTrackSizingFunction::percent(f),
            Dim::Auto => return None,
        },
    })
}

fn max_track(word: &str, units: &Units) -> Option<MaxTrackSizingFunction> {
    if let Some(fr) = word.strip_suffix("fr") {
        return Some(MaxTrackSizingFunction::fr(number(fr)?));
    }
    Some(match word {
        "auto" => MaxTrackSizingFunction::auto(),
        "min-content" => MaxTrackSizingFunction::min_content(),
        "max-content" => MaxTrackSizingFunction::max_content(),
        other => match length_percent(other, units)? {
            Dim::Px(px) => MaxTrackSizingFunction::length(px),
            Dim::Percent(f) => MaxTrackSizingFunction::percent(f),
            Dim::Auto => return None,
        },
    })
}

/// The named areas of a grid, as the lines they run between (counted from 1).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Areas {
    areas: HashMap<String, Area>,
}

/// An area's first and one-past-last rows and columns, as line numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    row: [i16; 2],
    column: [i16; 2],
}

/// `grid-template-areas`: `"head head" "side main"`.
pub(crate) fn areas(text: &str) -> Areas {
    let mut found: HashMap<String, Area> = HashMap::new();
    let rows = quoted_rows(text);
    for (r, row) in rows.iter().enumerate() {
        for (c, name) in row.split_whitespace().enumerate() {
            if name.chars().all(|ch| ch == '.') {
                continue;
            }
            let (r, c) = (r as i16 + 1, c as i16 + 1);
            let area = found.entry(name.to_owned()).or_insert(Area {
                row: [r, r + 1],
                column: [c, c + 1],
            });
            area.row = [area.row[0].min(r), area.row[1].max(r + 1)];
            area.column = [area.column[0].min(c), area.column[1].max(c + 1)];
        }
    }
    Areas { areas: found }
}

fn quoted_rows(text: &str) -> Vec<String> {
    let mut rows = Vec::new();
    let mut open: Option<(char, String)> = None;
    for ch in text.chars() {
        match (&mut open, ch) {
            (None, '"' | '\'') => open = Some((ch, String::new())),
            (Some((quote, row)), c) if c == *quote => {
                rows.push(std::mem::take(row));
                open = None;
            }
            (Some((_, row)), c) => row.push(c),
            (None, _) => {}
        }
    }
    rows
}

impl Areas {
    pub(crate) fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }
}

/// An item's four placements, in taffy's terms.
pub(crate) struct Lines {
    pub row: taffy::Line<Placement>,
    pub column: taffy::Line<Placement>,
}

/// What an item's `grid-*` properties put it: `area` is `grid-area`, `row` and `column` the
/// shorthands, and the starts and ends the longhands, each as written. Later forms win over earlier
/// ones, the longhands over the shorthands.
pub(crate) struct Raw<'a> {
    pub area: Option<&'a str>,
    pub row: Option<&'a str>,
    pub column: Option<&'a str>,
    pub row_start: Option<&'a str>,
    pub row_end: Option<&'a str>,
    pub column_start: Option<&'a str>,
    pub column_end: Option<&'a str>,
}

pub(crate) fn lines(raw: &Raw, areas: &Areas) -> Lines {
    let mut row = [Placement::Auto, Placement::Auto];
    let mut column = [Placement::Auto, Placement::Auto];
    if let Some(area) = raw.area {
        area_lines(area, areas, &mut row, &mut column);
    }
    if let Some(text) = raw.row {
        pair(text, areas, Axis::Row, &mut row);
    }
    if let Some(text) = raw.column {
        pair(text, areas, Axis::Column, &mut column);
    }
    let longhands = [
        (raw.row_start, Axis::Row, 0),
        (raw.row_end, Axis::Row, 1),
        (raw.column_start, Axis::Column, 0),
        (raw.column_end, Axis::Column, 1),
    ];
    for (text, axis, end) in longhands {
        let Some(text) = text else { continue };
        let target = if axis == Axis::Row {
            &mut row
        } else {
            &mut column
        };
        target[end] = one(text, areas, axis, end == 1);
    }
    let [row_start, row_end] = row;
    let [column_start, column_end] = column;
    Lines {
        row: taffy::Line {
            start: row_start,
            end: row_end,
        },
        column: taffy::Line {
            start: column_start,
            end: column_end,
        },
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Row,
    Column,
}

fn area_lines(text: &str, areas: &Areas, row: &mut [Placement; 2], column: &mut [Placement; 2]) {
    let parts = split_outside(text, '/');
    if let [name] = parts.as_slice() {
        if let Some(area) = areas.areas.get(*name) {
            *row = [line(area.row[0]), line(area.row[1])];
            *column = [line(area.column[0]), line(area.column[1])];
            return;
        }
        *row = [
            one(name, areas, Axis::Row, false),
            one(name, areas, Axis::Row, true),
        ];
        *column = [
            one(name, areas, Axis::Column, false),
            one(name, areas, Axis::Column, true),
        ];
        return;
    }
    let at = |i: usize, axis, end| {
        parts
            .get(i)
            .map_or(Placement::Auto, |p| one(p, areas, axis, end))
    };
    *row = [at(0, Axis::Row, false), at(2, Axis::Row, true)];
    *column = [at(1, Axis::Column, false), at(3, Axis::Column, true)];
}

/// `grid-row` / `grid-column`: `start / end`, or one value for the start.
fn pair(text: &str, areas: &Areas, axis: Axis, target: &mut [Placement; 2]) {
    let parts = split_outside(text, '/');
    let Some(start) = parts.first() else { return };
    target[0] = one(start, areas, axis, false);
    target[1] = match parts.get(1) {
        Some(end) => one(end, areas, axis, true),
        // A name alone names both ends; a number or `span` leaves the end open.
        None if is_name(start) => one(start, areas, axis, true),
        None => Placement::Auto,
    };
}

fn is_name(word: &str) -> bool {
    word != "auto"
        && !word.starts_with("span")
        && word
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// One placement: `auto`, `3`, `-1`, `span 2`, `span name`, `name`, `name 2`. A name that an area
/// has is the area's line at that end.
fn one(text: &str, areas: &Areas, axis: Axis, end: bool) -> Placement {
    let text = text.trim();
    if text == "auto" {
        return Placement::Auto;
    }
    if let Some(rest) = text.strip_prefix("span") {
        let rest = rest.trim();
        return match rest.parse::<u16>() {
            Ok(count) if count > 0 => span(count),
            Ok(_) => Placement::Auto,
            Err(_) if rest.is_empty() => Placement::Auto,
            Err(_) => Placement::NamedSpan(rest.to_owned(), 1),
        };
    }
    if let Ok(number) = text.parse::<i16>() {
        return if number == 0 {
            Placement::Auto
        } else {
            line(number)
        };
    }
    if let Some(area) = areas.areas.get(text) {
        let lines = if axis == Axis::Row {
            area.row
        } else {
            area.column
        };
        return line(lines[usize::from(end)]);
    }
    Placement::NamedLine(text.to_owned(), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units() -> Units {
        Units::new(16., 16., (800., 600.))
    }

    #[test]
    fn tracks_mix_lengths_fractions_and_functions() {
        let t = template("200px 1fr minmax(10px, 2fr) auto [end]", &units());
        assert_eq!(t.tracks.len(), 4);
        assert_eq!(t.names.len(), 5);
        assert_eq!(t.names[4], vec!["end".to_owned()]);
    }

    #[test]
    fn repeat_counts_expand_and_auto_repeats_stay_whole() {
        assert_eq!(template("repeat(3, 100px 1fr)", &units()).tracks.len(), 6);
        assert_eq!(
            template("repeat(auto-fill, minmax(100px, 1fr)) 50px", &units())
                .tracks
                .len(),
            2
        );
        assert!(template("none", &units()).tracks.is_empty());
        assert!(template("subgrid", &units()).tracks.is_empty());
    }

    #[test]
    fn areas_are_the_rectangles_their_names_cover() {
        let a = areas("'head head' \"side main\" 'foot foot'");
        assert_eq!(
            a.areas["head"],
            Area {
                row: [1, 2],
                column: [1, 3]
            }
        );
        assert_eq!(
            a.areas["main"],
            Area {
                row: [2, 3],
                column: [2, 3]
            }
        );
        assert_eq!(
            a.areas["foot"],
            Area {
                row: [3, 4],
                column: [1, 3]
            }
        );
        assert!(areas("none").is_empty());
        assert!(areas("'. .'").is_empty());
    }

    fn raw_area(area: &str) -> Raw<'_> {
        Raw {
            area: Some(area),
            row: None,
            column: None,
            row_start: None,
            row_end: None,
            column_start: None,
            column_end: None,
        }
    }

    #[test]
    fn grid_area_names_put_an_item_on_the_area_s_lines() {
        let a = areas("'head head' 'side main'");
        let lines = lines(&raw_area("main"), &a);
        assert_eq!(lines.row.start, line::<Placement>(2));
        assert_eq!(lines.row.end, line::<Placement>(3));
        assert_eq!(lines.column.start, line::<Placement>(2));
        assert_eq!(lines.column.end, line::<Placement>(3));
        let four = lines_of("2 / 1 / 4 / span 2");
        assert_eq!(four.row.start, line::<Placement>(2));
        assert_eq!(four.column.start, line::<Placement>(1));
        assert_eq!(four.row.end, line::<Placement>(4));
        assert_eq!(four.column.end, span::<Placement>(2));
    }

    fn lines_of(area: &str) -> Lines {
        lines(&raw_area(area), &Areas::default())
    }

    #[test]
    fn row_and_column_shorthands_and_longhands() {
        let raw = Raw {
            area: None,
            row: Some("2"),
            column: Some("span 2"),
            row_start: None,
            row_end: Some("-1"),
            column_start: None,
            column_end: None,
        };
        let l = lines(&raw, &Areas::default());
        assert_eq!(l.row.start, line::<Placement>(2));
        assert_eq!(l.row.end, line::<Placement>(-1));
        assert_eq!(l.column.start, span::<Placement>(2));
        assert_eq!(l.column.end, Placement::Auto);
    }
}
