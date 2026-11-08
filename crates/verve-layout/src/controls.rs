//! Form controls (P32): which elements the shell draws in the platform's way, the sizes they
//! have before any author's CSS, the text they show, and the part of them that is no box or text:
//! a caret, a selection, a check, a slider.
//!
//! The text a field shows is not drawn here. It goes through the inline layout as text generated
//! in the field's box (P31), so it is shaped, clipped and painted like any other, and a
//! [`PaintItem::Control`](crate::PaintItem) only adds what lies beside it.
//!
//! The sizes were measured in Electron 39.8.10 (Chromium 142.0.7444.265) in a hidden window with
//! `getBoundingClientRect`, at a device pixel ratio of 1: the fixed ones are in the user agent's
//! sheet (`verve_dom::css::USER_AGENT_SHEET`), the ones that follow the font or an attribute are
//! worked out here. At 1.25 only the sizes that follow text change, because Chromium rounds the
//! font's metrics to physical pixels; that rounding is not reproduced.

use std::sync::Arc;

use taffy::Dimension;
use taffy::style_helpers::{TaffyAuto, length};
use verve_dom::css::{Style, color_with};
use verve_dom::{NodeId, Rect, Snapshot};

use crate::display_list::Rgba;
use crate::inline::query::x_at;
use crate::inline::{Fragment, InlineLayout, LineBox, TextFragment};
use crate::measure::TextMeasurer;
use crate::props::{BaselineRule, BoxProps};
use crate::value::{Units, pixels};

/// The font size the measurements below were taken at: `font: 13.3333px Arial`.
const BASE_FONT: f32 = 13.3333;
/// A character of a text field is this many font sizes wide: 7px at the base size.
const CHARACTER: f32 = 7. / BASE_FONT;
/// What a text field adds to its characters, in font sizes: 29px at the base size.
const FIELD_EXTRA: f32 = 29. / BASE_FONT;
/// A line of a text field, in font sizes: 15px at the base size.
const LINE: f32 = 15. / BASE_FONT;
/// The width of a `<textarea>`'s scrollbar, which its width leaves room for.
const SCROLLBAR: f32 = 15.;
/// What a drop-down adds to its widest label: the arrow and the room around the text.
const SELECT_ARROW: f32 = 20.;
/// What a list box adds to its widest label: the room for its scroll bar.
const SELECT_LIST_BAR: f32 = 19.;
/// Chromium's default accent colour for form controls.
const ACCENT: Rgba = [0., 117. / 255., 1., 1.];
/// The colour of a placeholder.
const PLACEHOLDER: &str = "#757575";
/// What a character of a password is shown as.
const BULLET: &str = "\u{2022}";

/// What the shell draws of a control besides its box and text.
#[derive(Clone, Debug, PartialEq)]
pub enum ControlKind {
    /// A text field. The rectangles are in the frame the field's text is in, inside its clip and
    /// its scroll: the caret has no width, and `marked` is what an input method is composing. The
    /// selection is painted by layout under the text.
    TextField {
        caret: Option<Rect>,
        marked: Vec<Rect>,
        multiline: bool,
        focused: bool,
    },
    Checkbox {
        checked: bool,
        indeterminate: bool,
    },
    Radio {
        checked: bool,
    },
    /// A drop-down (or a list box); the popup is the shell's.
    Select {
        options: Arc<[SelectOption]>,
        multiple: bool,
        /// A list box shows its options in the box itself, one row each, this far apart; a
        /// drop-down (`None`) shows the chosen one and a chevron.
        list: Option<f32>,
    },
    Range {
        min: f64,
        max: f64,
        value: f64,
    },
    /// `None` is a bar whose progress is not known.
    Progress {
        value: Option<f64>,
    },
    Meter {
        value: f64,
        min: f64,
        max: f64,
        low: f64,
        high: f64,
        optimum: f64,
    },
    Color {
        value: Rgba,
    },
    Date {
        kind: DateKind,
    },
    File {
        multiple: bool,
    },
}

/// One option of a [`ControlKind::Select`].
#[derive(Clone, Debug, PartialEq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
    pub disabled: bool,
    /// The label of the `<optgroup>` it is in.
    pub group: Option<String>,
}

/// Which of the date and time inputs a [`ControlKind::Date`] is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DateKind {
    Date,
    Time,
    DateTimeLocal,
}

/// How a control is told apart from the plain boxes around it.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlLook {
    /// `accent-color`, or the default blue.
    pub accent: Rgba,
    /// The text colour, which a caret and a composition's underline take (`caret-color` if set).
    pub color: Rgba,
    pub disabled: bool,
}

/// The sorts of element this module knows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sort {
    Text {
        password: bool,
    },
    Textarea,
    Checkbox,
    Radio,
    Range,
    Color,
    Date(DateKind),
    File,
    Select,
    Progress,
    Meter,
    /// `<input type=button|submit|reset>`: a plain box with the `value` as its text.
    Push,
}

/// What kind of control `node` is, if it is one of these.
pub(crate) fn sort_of(snapshot: &Snapshot, node: NodeId) -> Option<Sort> {
    let found = snapshot.dom.get(node)?;
    Some(match found.tag()? {
        "textarea" => Sort::Textarea,
        "select" => Sort::Select,
        "progress" => Sort::Progress,
        "meter" => Sort::Meter,
        "input" => match found
            .attr("type")
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "password" => Sort::Text { password: true },
            "checkbox" => Sort::Checkbox,
            "radio" => Sort::Radio,
            "range" => Sort::Range,
            "color" => Sort::Color,
            "date" => Sort::Date(DateKind::Date),
            "time" => Sort::Date(DateKind::Time),
            "datetime-local" => Sort::Date(DateKind::DateTimeLocal),
            "file" => Sort::File,
            "button" | "submit" | "reset" => Sort::Push,
            "hidden" | "image" => return None,
            _ => Sort::Text { password: false },
        },
        _ => return None,
    })
}

/// Whether the element shows what it shows and nothing of what is inside it: its children are
/// options, a default value or a fallback.
pub(crate) fn draws_itself(snapshot: &Snapshot, node: NodeId) -> bool {
    sort_of(snapshot, node).is_some()
}

/// Whether the element is a text `<input>` or a `<textarea>`: the controls with a caret.
pub(crate) fn is_text_field(snapshot: &Snapshot, node: NodeId) -> bool {
    matches!(
        sort_of(snapshot, node),
        Some(Sort::Text { .. } | Sort::Textarea)
    )
}

/// The properties of a field that its text takes.
const INHERITED: &[&str] = &[
    "font-family",
    "font-weight",
    "font-style",
    "color",
    "letter-spacing",
    "word-spacing",
    "text-transform",
    "text-indent",
    "line-height",
    "tab-size",
    "direction",
];

/// How the text of a control keeps its white space. A single-line field never wraps whatever
/// the author wrote; a `<textarea>` keeps its line breaks (Electron 39.8.10: `normal` wraps
/// like `pre-wrap`, `nowrap` like `pre`); a button wraps as its style says.
fn white_space(snapshot: &Snapshot, node: NodeId) -> String {
    let own = snapshot
        .style(node, "white-space")
        .map_or("normal", str::trim);
    match sort_of(snapshot, node) {
        Some(Sort::Textarea) => match own {
            "normal" => "pre-wrap",
            "nowrap" => "pre",
            other => other,
        },
        Some(Sort::Text { .. } | Sort::Date(_) | Sort::Select) => "pre",
        _ => own,
    }
    .to_owned()
}

/// The text the element shows, and the style it takes besides its owner's.
pub(crate) fn shown_text(snapshot: &Snapshot, node: NodeId) -> Option<(String, Arc<Style>)> {
    let dom = &snapshot.dom;
    let found = dom.get(node)?;
    // Generated text reads only the style it is given, so what it takes from the field (its
    // font, colour, spacing and white space) is handed on.
    let mut base = Style::new();
    for property in INHERITED {
        if let Some(value) = snapshot.style(node, property) {
            base.insert((*property).into(), value.into());
        }
    }
    base.insert("white-space".into(), white_space(snapshot, node));
    let plain = Arc::new(base.clone());
    let (text, placeholder) = match sort_of(snapshot, node)? {
        Sort::Text { password } => {
            let value = dom.value(node);
            let shown = if password {
                BULLET.repeat(value.chars().count())
            } else {
                value
            };
            (shown, found.attr("placeholder"))
        }
        Sort::Textarea => (dom.value(node), found.attr("placeholder")),
        Sort::Select if is_list(snapshot, node) => {
            // One line to an option, each a pixel of padding above and below its text.
            let row = list_row(snapshot, node);
            base.insert("line-height".into(), format!("{row}px"));
            let labels: Vec<String> = (dom.listed_options(node).into_iter())
                .map(|option| option_label(snapshot, option))
                .collect();
            return Some((labels.join("\n"), Arc::new(base)));
        }
        Sort::Select => (
            dom.selected_option(node)
                .map(|option| option_label(snapshot, option))
                .unwrap_or_default(),
            None,
        ),
        Sort::Push => {
            let fallback = match found.attr("type").unwrap_or("") {
                t if t.eq_ignore_ascii_case("submit") => "Submit",
                t if t.eq_ignore_ascii_case("reset") => "Reset",
                _ => "",
            };
            (found.attr("value").unwrap_or(fallback).to_owned(), None)
        }
        Sort::Date(kind) => (date_text(&dom.value(node), kind), None),
        _ => return None,
    };
    if text.is_empty()
        && let Some(hint) = placeholder.filter(|hint| !hint.is_empty())
    {
        let mut style = base;
        style.insert("color".into(), PLACEHOLDER.into());
        return Some((hint.to_owned(), Arc::new(style)));
    }
    Some((text, plain))
}

/// What a date or time field shows: its value the way a US locale writes it, or the pattern.
fn date_text(value: &str, kind: DateKind) -> String {
    let pattern = match kind {
        DateKind::Date => "mm/dd/yyyy",
        DateKind::Time => "--:-- --",
        DateKind::DateTimeLocal => "mm/dd/yyyy, --:-- --",
    };
    if kind == DateKind::Date
        && let [year, month, day] = value.split('-').collect::<Vec<_>>()[..]
        && !year.is_empty()
    {
        return format!("{month}/{day}/{year}");
    }
    pattern.to_owned()
}

/// An option's label: its `label` attribute, or its text with the white space collapsed.
fn option_label(snapshot: &Snapshot, option: NodeId) -> String {
    let dom = &snapshot.dom;
    if let Some(label) = dom.get(option).and_then(|node| node.attr("label")) {
        return label.to_owned();
    }
    dom.text_content(option)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Gives a control the sizes that follow its font and attributes, where its style has none. In
/// a flex or grid container (`grows`) the style keeps `auto` and the sizes are the box's content
/// size, so that the control stretches there as it does in Chromium and stays at its own size in
/// a row that is too short; elsewhere they become the style's `width` and `height`.
pub(crate) fn adjust(
    snapshot: &Snapshot,
    measurer: &dyn TextMeasurer,
    node: NodeId,
    props: &mut BoxProps,
    grows: bool,
) {
    let Some(sort) = sort_of(snapshot, node) else {
        return;
    };
    let size = font_size(snapshot, node);
    let attribute = |name: &str| {
        snapshot
            .dom
            .get(node)
            .and_then(|found| found.attr(name))
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|count| *count > 0)
    };
    let line = line_height(snapshot, node, size);
    let (mut width, mut height) = (None, None);
    let list = matches!(sort, Sort::Select) && is_list(snapshot, node);
    props.baseline = match sort {
        Sort::Checkbox | Sort::Radio | Sort::Range => BaselineRule::BorderBottom,
        // Observed in Electron 39.8.10: 21 of 27 for a colour well, and the last row of a list
        // box (67 of 70, 33 of 36 with `size=2`).
        Sort::Color => BaselineRule::FromBottom(6.),
        Sort::Select if list => BaselineRule::FromBottom(3.),
        Sort::Text { .. } | Sort::Push | Sort::Date(_) | Sort::Select | Sort::File => {
            BaselineRule::Text
        }
        _ => BaselineRule::Last,
    };
    match sort {
        Sort::Text { .. } => {
            let count = attribute("size").unwrap_or(20) as f32;
            width = Some((count * size * CHARACTER + size * FIELD_EXTRA).round());
            height = Some(line);
        }
        Sort::Textarea => {
            let columns = attribute("cols").unwrap_or(20) as f32;
            let rows = attribute("rows").unwrap_or(2) as f32;
            // A column is the width of a "0" in the field's font, and the bar takes 15px more.
            let zero = text_width(snapshot, measurer, node, "0", size);
            width = Some((columns * zero).ceil() + SCROLLBAR);
            height = Some(rows * line);
        }
        Sort::Select => {
            let multiple = snapshot
                .dom
                .get(node)
                .is_some_and(|found| found.attr("multiple").is_some());
            let rows = attribute("size").unwrap_or(if multiple { 4 } else { 1 });
            let list = multiple || rows > 1;
            height = Some(if list { rows as f32 } else { 1. } * (line + 2.));
            width = Some(select_width(snapshot, measurer, node, size, list));
        }
        // A check box with no appearance has nothing to be the size of (Electron 39.8.10: 0 by 0).
        Sort::Checkbox if is_plain(snapshot, node) => (width, height) = (Some(0.), Some(0.)),
        Sort::Checkbox | Sort::Radio => (width, height) = (Some(13.), Some(13.)),
        Sort::Range => (width, height) = (Some(129.), Some(16.)),
        // 50 by 27 with a 1px border and 1px 2px of padding.
        Sort::Color => (width, height) = (Some(44.), Some(23.)),
        Sort::Date(kind) => {
            (width, height) = (Some(date_width(kind, size)), Some(date_height(kind, size)));
        }
        Sort::File => (width, height) = (Some(1. + 18.9 * size), Some(5. + 1.2 * size)),
        _ => {}
    }
    props.intrinsic = [width, height];
    let style = &mut props.style;
    let border_box = style.box_sizing == taffy::BoxSizing::BorderBox;
    if grows {
        // A flex or grid item does not shrink below its content (`min-width: auto`), which a
        // clipping box would otherwise let it.
        let wanted = [
            width.filter(|_| style.size.width == Dimension::AUTO),
            height.filter(|_| style.size.height == Dimension::AUTO),
        ];
        write_sizes(snapshot, node, border_box, &mut style.min_size, wanted);
    } else {
        write_sizes(snapshot, node, border_box, &mut style.size, [width, height]);
    }
}

/// Makes the content sizes of a control the `width` and `height` of its style, where the style
/// has none: a border-box element is given the sizes with its padding and border added.
fn write_sizes(
    snapshot: &Snapshot,
    node: NodeId,
    border_box: bool,
    target: &mut taffy::Size<Dimension>,
    size: [Option<f32>; 2],
) {
    let extra = |sides: [&str; 4]| -> f32 {
        if !border_box {
            return 0.;
        }
        sides
            .iter()
            .map(|side| edge(snapshot, node, side))
            .sum::<f32>()
    };
    if target.width == Dimension::AUTO
        && let Some(width) = size[0]
    {
        let wide = extra([
            "padding-left",
            "padding-right",
            "border-left-width",
            "border-right-width",
        ]);
        target.width = length(width + wide);
    }
    if target.height == Dimension::AUTO
        && let Some(height) = size[1]
    {
        let high = extra([
            "padding-top",
            "padding-bottom",
            "border-top-width",
            "border-bottom-width",
        ]);
        target.height = length(height + high);
    }
}

/// Whether a `<select>` is a list box: `multiple`, or a `size` over 1.
fn is_list(snapshot: &Snapshot, node: NodeId) -> bool {
    let found = snapshot.dom.get(node);
    let attr = |name: &str| found.and_then(|n| n.attr(name));
    attr("multiple").is_some()
        || attr("size")
            .and_then(|text| text.trim().parse::<u32>().ok())
            .is_some_and(|rows| rows > 1)
}

/// The content width of a date or time field, fitted to the two widths Chromium gave at 13.33px
/// and 20px (Electron 39.8.10, locale en-US): its text and the parts around it follow the font.
fn date_width(kind: DateKind, size: f32) -> f32 {
    match kind {
        DateKind::Date => 10. + 7.6 * size,
        DateKind::Time => 6.975 * size,
        DateKind::DateTimeLocal => 13.75 * size,
    }
}

fn date_height(kind: DateKind, size: f32) -> f32 {
    match kind {
        DateKind::Time => 1.5 * size,
        _ => 2. + 1.15 * size,
    }
}

/// The distance between the rows of a list box: a line of its font and 2px.
fn list_row(snapshot: &Snapshot, node: NodeId) -> f32 {
    line_height(snapshot, node, font_size(snapshot, node)) + 2.
}

/// The line the text of `node` stands on: its `line-height`, or the font's normal one.
fn line_height(snapshot: &Snapshot, node: NodeId, size: f32) -> f32 {
    let units = Units::new(size, 16., snapshot.viewport);
    let normal = (size * LINE).round();
    match snapshot.style(node, "line-height").map(str::trim) {
        None | Some("normal") => normal,
        Some(text) => text
            .parse::<f32>()
            .map(|factor| factor * size)
            .ok()
            .or_else(|| pixels(text, &units))
            .unwrap_or(normal),
    }
}

/// How wide `text` is in the font of `node` at `size`.
fn text_width(
    snapshot: &Snapshot,
    measurer: &dyn TextMeasurer,
    node: NodeId,
    text: &str,
    size: f32,
) -> f32 {
    let families = crate::fonts::split_families(snapshot.style(node, "font-family").unwrap_or(""));
    let font = measurer.resolve(&families, 400., false);
    measurer.shape(text, font, size).width
}

/// The content width of a `<select>`: its widest label (one in an `<optgroup>` is indented by
/// four spaces) and an arrow, rounded up; a list box is not rounded and has a thinner bar.
fn select_width(
    snapshot: &Snapshot,
    measurer: &dyn TextMeasurer,
    node: NodeId,
    size: f32,
    list: bool,
) -> f32 {
    let families = crate::fonts::split_families(snapshot.style(node, "font-family").unwrap_or(""));
    let font = measurer.resolve(&families, 400., false);
    let dom = &snapshot.dom;
    let widest = dom
        .options(node)
        .into_iter()
        .map(|option| {
            let label = option_label(snapshot, option);
            let grouped = dom
                .parent(option)
                .is_some_and(|parent| dom.get(parent).and_then(|n| n.tag()) == Some("optgroup"));
            let label = if grouped {
                format!("    {label}")
            } else {
                label
            };
            measurer.shape(&label, font, size).width
        })
        .fold(0., f32::max);
    if list {
        widest + SELECT_LIST_BAR
    } else {
        widest.ceil() + SELECT_ARROW
    }
}

fn font_size(snapshot: &Snapshot, node: NodeId) -> f32 {
    let units = Units::new(16., 16., snapshot.viewport);
    snapshot
        .style(node, "font-size")
        .and_then(|text| pixels(text, &units))
        .unwrap_or(BASE_FONT)
}

/// A computed length of `node` in pixels, 0 when it has none.
fn edge(snapshot: &Snapshot, node: NodeId, property: &str) -> f32 {
    let units = Units::new(16., 16., snapshot.viewport);
    snapshot
        .style(node, property)
        .and_then(|text| pixels(text, &units))
        .unwrap_or(0.)
}

/// How a control looks: its `accent-color` and whether it is disabled.
pub(crate) fn look(snapshot: &Snapshot, node: NodeId) -> ControlLook {
    ControlLook {
        accent: snapshot
            .style(node, "accent-color")
            .and_then(|text| color_with(text, [0., 0., 0., 1.]))
            .unwrap_or(ACCENT),
        color: ["caret-color", "color"]
            .iter()
            .find_map(|property| color_with(snapshot.style(node, property)?, [0., 0., 0., 1.]))
            .unwrap_or([0., 0., 0., 1.]),
        disabled: snapshot
            .dom
            .get(node)
            .is_some_and(|found| found.attr("disabled").is_some()),
    }
}

/// Whether the style of `node` makes it a plain box: `appearance: none`.
pub(crate) fn is_plain(snapshot: &Snapshot, node: NodeId) -> bool {
    matches!(
        snapshot.style(node, "appearance").map(str::trim),
        Some("none")
    ) || matches!(
        snapshot.style(node, "-webkit-appearance").map(str::trim),
        Some("none")
    )
}

/// The [`ControlKind`] of `node`, if the shell draws something beside its box and text. A text
/// field's rectangles are laid out from `lines` (its text, shaped) with the content origin.
pub(crate) fn kind_of(
    snapshot: &Snapshot,
    node: NodeId,
    lines: Option<&InlineLayout>,
    origin: [f32; 2],
) -> Option<ControlKind> {
    let dom = &snapshot.dom;
    let found = dom.get(node)?;
    let number = |name: &str| {
        found
            .attr(name)
            .and_then(|text| text.trim().parse::<f64>().ok())
    };
    Some(match sort_of(snapshot, node)? {
        Sort::Text { .. } | Sort::Textarea => {
            return Some(text_field(snapshot, node, lines, origin));
        }
        Sort::Checkbox => ControlKind::Checkbox {
            checked: found.checked,
            indeterminate: false,
        },
        Sort::Radio => ControlKind::Radio {
            checked: found.checked,
        },
        Sort::Select => ControlKind::Select {
            options: options_of(snapshot, node),
            multiple: found.attr("multiple").is_some(),
            list: is_list(snapshot, node).then(|| list_row(snapshot, node)),
        },
        Sort::Range => {
            let (min, max, _) = dom.range_limits(node);
            ControlKind::Range {
                min,
                max,
                value: dom.value(node).trim().parse().unwrap_or(min),
            }
        }
        Sort::Progress => {
            let max = number("max").filter(|max| *max > 0.).unwrap_or(1.);
            ControlKind::Progress {
                value: number("value").map(|value| (value / max).clamp(0., 1.)),
            }
        }
        Sort::Meter => {
            let (min, max) = (number("min").unwrap_or(0.), number("max").unwrap_or(1.));
            let max = max.max(min);
            let clamp = |value: f64| value.clamp(min, max);
            ControlKind::Meter {
                value: clamp(number("value").unwrap_or(0.)),
                min,
                max,
                low: clamp(number("low").unwrap_or(min)),
                high: clamp(number("high").unwrap_or(max)),
                optimum: clamp(number("optimum").unwrap_or((min + max) / 2.)),
            }
        }
        Sort::Color => ControlKind::Color {
            value: color_with(dom.value(node).trim(), [0., 0., 0., 1.]).unwrap_or([0., 0., 0., 1.]),
        },
        Sort::Date(kind) => ControlKind::Date { kind },
        Sort::File => ControlKind::File {
            multiple: found.attr("multiple").is_some(),
        },
        Sort::Push => return None,
    })
}

fn options_of(snapshot: &Snapshot, select: NodeId) -> Arc<[SelectOption]> {
    let dom = &snapshot.dom;
    // A list box has no choice until a person or a `selected` attribute makes one, where a
    // drop-down shows its first option (Chromium's list box shades no row by default).
    let unchosen = is_list(snapshot, select)
        && dom.get(select).is_some_and(|n| n.selected.is_none())
        && dom
            .get(select)
            .is_some_and(|n| n.attr("multiple").is_none());
    dom.listed_options(select)
        .into_iter()
        .map(|option| {
            let group = dom
                .parent(option)
                .filter(|parent| dom.get(*parent).and_then(|n| n.tag()) == Some("optgroup"))
                .and_then(|group| dom.get(group)?.attr("label").map(str::to_owned));
            SelectOption {
                value: dom.option_value(option),
                label: option_label(snapshot, option),
                selected: dom.option_chosen(option)
                    && (!unchosen
                        || dom
                            .get(option)
                            .is_some_and(|n| n.attr("selected").is_some())),
                disabled: dom.option_disabled(option),
                group,
            }
        })
        .collect()
}

/// The caret and composition of a text field, placed over its shaped text. The selection is not
/// here: layout paints it under the glyphs, as it does for the page's own text.
fn text_field(
    snapshot: &Snapshot,
    node: NodeId,
    lines: Option<&InlineLayout>,
    origin: [f32; 2],
) -> ControlKind {
    let dom = &snapshot.dom;
    let found = dom.get(node);
    let multiline = found.and_then(|n| n.tag()) == Some("textarea");
    let focused = dom.focused == Some(node);
    let none = ControlKind::TextField {
        caret: None,
        marked: Vec::new(),
        multiline,
        focused,
    };
    let Some(draft) = found.and_then(|n| n.draft.as_ref()) else {
        return none;
    };
    let field = Field::of(snapshot, node, lines, draft.text());
    // A range selected shows no caret.
    let caret = (focused && draft.selection().is_empty())
        .then(|| field.caret(draft.caret()))
        .map(|spot| Rect::new(origin[0] + spot.x, origin[1] + spot.top, 0., spot.height));
    let marked = draft
        .marked()
        .map(|range| field.span(range.start, range.end))
        .unwrap_or_default();
    ControlKind::TextField {
        caret,
        marked: marked
            .into_iter()
            .map(|rect| shifted(rect, origin))
            .collect(),
        multiline,
        focused,
    }
}

fn shifted(rect: Rect, origin: [f32; 2]) -> Rect {
    Rect::new(
        rect.x + origin[0],
        rect.y + origin[1],
        rect.width,
        rect.height,
    )
}

/// The boxes of the selected text of the field `node`, from the left and top of its content: one
/// to a line and piece, as high as the line. Empty unless the field has the focus.
pub(crate) fn field_selection(
    snapshot: &Snapshot,
    node: NodeId,
    lines: &InlineLayout,
) -> Vec<Rect> {
    let dom = &snapshot.dom;
    let Some(draft) = dom.get(node).and_then(|found| found.draft.as_ref()) else {
        return Vec::new();
    };
    let selection = draft.selection();
    if dom.focused != Some(node) || selection.is_empty() {
        return Vec::new();
    }
    Field::of(snapshot, node, Some(lines), draft.text()).span(selection.start, selection.end)
}

/// The bytes of the value of the field `node` that the point `(x, y)` (from the left and top of
/// its content, scroll included) is nearest to and over.
pub(crate) fn field_hit(
    snapshot: &Snapshot,
    node: NodeId,
    lines: &InlineLayout,
    (x, y): (f32, f32),
) -> Option<(usize, usize)> {
    let draft = snapshot.dom.get(node)?.draft.as_ref()?;
    Some(Field::of(snapshot, node, Some(lines), draft.text()).hit(x, y))
}

/// Where the caret of the focused field `node` stands, from the left and top of its content: a
/// zero-width box. A selection has the caret at its moving end.
pub(crate) fn caret_spot(
    snapshot: &Snapshot,
    node: NodeId,
    lines: Option<&InlineLayout>,
) -> Option<Rect> {
    let dom = &snapshot.dom;
    let draft = dom.get(node)?.draft.as_ref()?;
    if dom.focused != Some(node) {
        return None;
    }
    let spot = Field::of(snapshot, node, lines, draft.text()).caret(draft.caret());
    Some(Rect::new(spot.x, spot.top, 0., spot.height))
}

/// The text a field shows, laid out, and how the bytes of its value map to that text.
struct Field<'a> {
    lines: Option<&'a InlineLayout>,
    owner: NodeId,
    /// What is shown: the value, or a bullet for each character of a password.
    shown: String,
    password: bool,
    /// Whether it is an `<input>`: one row, wherever a point is vertically.
    single: bool,
    value: &'a str,
    /// The height of a line when there is none yet.
    fallback: f32,
}

/// Where a caret stands, from the top left of the field's content.
struct Spot {
    x: f32,
    top: f32,
    height: f32,
}

impl<'a> Field<'a> {
    fn of(
        snapshot: &Snapshot,
        node: NodeId,
        lines: Option<&'a InlineLayout>,
        value: &'a str,
    ) -> Self {
        let password = matches!(sort_of(snapshot, node), Some(Sort::Text { password: true }));
        let single = matches!(sort_of(snapshot, node), Some(Sort::Text { .. }));
        let shown = if password {
            BULLET.repeat(value.chars().count())
        } else {
            value.to_owned()
        };
        Self {
            lines,
            owner: node,
            shown,
            password,
            single,
            value,
            fallback: line_height(snapshot, node, font_size(snapshot, node)),
        }
    }

    /// The byte of the shown text that byte `byte` of the value stands for: a character of a
    /// password is one bullet of three bytes.
    fn shown_byte(&self, byte: usize) -> usize {
        if self.password {
            self.value
                .get(..byte)
                .map_or(0, |head| head.chars().count() * BULLET.len())
        } else {
            byte
        }
    }

    /// The text of the field on each line: the line and the piece of its text.
    fn pieces(&self) -> impl Iterator<Item = (usize, &'a LineBox, &'a TextFragment)> + use<'a, '_> {
        let owner = self.owner;
        self.lines
            .into_iter()
            .flat_map(|layout| layout.lines.iter().enumerate())
            .flat_map(move |(at, line)| {
                line.fragments
                    .iter()
                    .filter_map(move |fragment| match fragment {
                        // The ellipsis of `text-overflow` is generated text of the field too: it has
                        // two stops, both at byte 0.
                        Fragment::Text(text)
                            if text.generated
                                && text.node == owner
                                && !(text.stops.len() == 2 && text.stops[1].byte == 0) =>
                        {
                            Some((at, line, text))
                        }
                        _ => None,
                    })
            })
    }

    /// The caret before byte `byte` of the value.
    fn caret(&self, byte: usize) -> Spot {
        let at = self.shown_byte(byte);
        let lines = self.lines.map_or(&[][..], |layout| &layout.lines[..]);
        // The last piece that ends at or before the caret, with the line it is on.
        let mut before: Option<(usize, usize, Spot)> = None;
        for (index, line, piece) in self.pieces() {
            let (Some(first), Some(last)) = (piece.stops.first(), piece.stops.last()) else {
                continue;
            };
            let (first, last) = (first.byte as usize, last.byte as usize);
            let spot_at = |at: usize| Spot {
                x: piece.x + x_at(piece, at),
                top: line.rect.y,
                height: line.rect.height,
            };
            if (first..last).contains(&at) {
                return spot_at(at);
            }
            if last <= at {
                before = Some((index, last, spot_at(last)));
            }
        }
        let (index, end, spot) = before.unwrap_or_else(|| {
            let first = lines.first();
            let spot = Spot {
                x: 0.,
                top: first.map_or(0., |line| line.rect.y),
                height: first.map_or(self.fallback, |line| line.rect.height),
            };
            (0, 0, spot)
        });
        // Each line break between the piece and the caret moves it a line down: to an empty
        // line, or to the one a trailing break leaves under the last.
        let breaks = self
            .shown
            .get(end..at)
            .map_or(0, |between| between.matches('\n').count());
        if breaks == 0 {
            return spot;
        }
        match lines.get(index + breaks) {
            Some(line) => Spot {
                x: 0.,
                top: line.rect.y,
                height: line.rect.height,
            },
            None => {
                let (top, height) = lines
                    .last()
                    .map_or((spot.top, spot.height), |l| (l.rect.y, l.rect.height));
                let past = (index + breaks + 1).saturating_sub(lines.len().max(1));
                Spot {
                    x: 0.,
                    top: top + height * past as f32,
                    height,
                }
            }
        }
    }

    /// The value byte nearest the point `(x, y)` (from the content's origin) and the value byte
    /// of the character the point is over, which a double click selects the word of. A point
    /// above the first line is on it and a point beyond the end of a line is at its end. A
    /// single-line field has one row, whatever `y` is; a text area takes a `y` below its last line
    /// as that line, and goes by `x` (Electron 39.8.10, `p32c-review/fld-out.json`), but a point
    /// under the empty line a trailing break leaves is at the end of the text.
    fn hit(&self, x: f32, y: f32) -> (usize, usize) {
        let lines = self.lines.map_or(&[][..], |layout| &layout.lines[..]);
        let end = self.value.len();
        let row = if self.single {
            0
        } else {
            match lines
                .iter()
                .position(|line| y < line.rect.y + line.rect.height)
            {
                Some(row) => row,
                None if self.shown.ends_with('\n') => return (end, self.under_end()),
                None => lines.len().saturating_sub(1),
            }
        };
        let pieces: Vec<_> = self.pieces().collect();
        let on_row = pieces.iter().filter(|(at, ..)| *at == row);
        let near = on_row.min_by(|a, b| {
            let gap = |piece: &TextFragment| (piece.x - x).max(x - piece.x - piece.width).max(0.);
            gap(a.2).total_cmp(&gap(b.2))
        });
        let Some(&(_, _, piece)) = near else {
            let behind = pieces.iter().rev().find(|(at, ..)| *at < row);
            let from = behind.map_or((0, row), |(at, _, p)| {
                (p.stops.last().map_or(0, |s| s.byte as usize), row - at)
            });
            let spot = self.after_breaks(from.0, from.1);
            return (self.value_byte(spot), self.value_byte(spot));
        };
        let local = x - piece.x;
        let nearest = (piece.stops.iter())
            .min_by(|a, b| (a.x - local).abs().total_cmp(&(b.x - local).abs()))
            .map_or(0, |stop| stop.byte as usize);
        let last = piece.stops.last().map_or(0, |stop| stop.byte as usize);
        // A line that ends in a break: its end is before the break, not after it.
        let nearest = if nearest == last && last > 0 && self.shown[..last].ends_with('\n') {
            last - 1
        } else {
            nearest
        };
        let before = piece.stops.iter().rev().find(|stop| stop.x <= local);
        let mut under = before.map_or(0, |stop| stop.byte as usize);
        if under >= last {
            under = if self.shown[last..].starts_with('\n') {
                last
            } else {
                piece
                    .stops
                    .iter()
                    .rev()
                    .nth(1)
                    .map_or(0, |s| s.byte as usize)
            };
        }
        (self.value_byte(nearest), self.value_byte(under))
    }

    /// The character at the end of the text: the last one.
    fn under_end(&self) -> usize {
        self.value
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    /// The byte after the `count`th line break at or after `from` of the shown text: where an
    /// empty line is.
    fn after_breaks(&self, from: usize, count: usize) -> usize {
        if count == 0 {
            return from;
        }
        let tail = self.shown.get(from..).unwrap_or("");
        let mut seen = 0;
        for (at, character) in tail.char_indices() {
            if character == '\n' {
                seen += 1;
                if seen == count {
                    return from + at + 1;
                }
            }
        }
        self.shown.len()
    }

    /// The byte of the value that byte `byte` of the shown text stands for.
    fn value_byte(&self, byte: usize) -> usize {
        if self.password {
            self.value
                .char_indices()
                .nth(byte / BULLET.len())
                .map_or(self.value.len(), |(at, _)| at)
        } else {
            byte
        }
    }

    /// The boxes of the text from byte `from` to `to` of the value, from the content's origin.
    fn span(&self, from: usize, to: usize) -> Vec<Rect> {
        let (from, to) = (self.shown_byte(from), self.shown_byte(to));
        self.pieces()
            .filter_map(|(_, line, piece)| {
                let (first, last) = (piece.stops.first()?, piece.stops.last()?);
                let (low, high) = (from.max(first.byte as usize), to.min(last.byte as usize));
                (high > low).then(|| {
                    let (x0, x1) = (piece.x + x_at(piece, low), piece.x + x_at(piece, high));
                    Rect::new(x0, line.rect.y, (x1 - x0).max(0.), line.rect.height)
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_field_grows_with_its_font() {
        // The two widths Chromium gave, at 13.3333px and at 20px.
        // The border box less 1px of padding and 4px of border: 116.33 x 21.33 and 167 x 29.
        assert!((date_width(DateKind::Date, BASE_FONT) + 5. - 116.33).abs() < 0.05);
        assert!((date_width(DateKind::Date, 20.) + 5. - 167.).abs() < 0.05);
        assert!((date_height(DateKind::Date, 20.) + 4. - 29.).abs() < 0.05);
    }

    #[test]
    fn the_measured_sizes_of_a_field_follow_its_font() {
        // Observed in Electron 39.8.10: 20 characters of 7px and 29px over, at 13.3333px.
        assert_eq!(
            (20. * BASE_FONT * CHARACTER + BASE_FONT * FIELD_EXTRA).round(),
            169.
        );
        assert_eq!((BASE_FONT * LINE).round(), 15.);
        // A line of 23 at 20px: a list box row is 25 there, and 4 rows and the border are 102.
        assert_eq!((20_f32 * LINE).round() + 2., 25.);
    }
}
