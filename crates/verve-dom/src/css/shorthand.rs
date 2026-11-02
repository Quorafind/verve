//! Shorthand properties opened into their longhands: `padding` into its four sides, `border`
//! into widths, styles and colors. What this engine reads of a value is its longhands; the
//! shorthand is kept too, as written, for `getComputedStyle` to answer.

use super::calc::length;
use super::cascade::Style;
use super::color::color;
use super::syntax::split_outside;
use super::values::ROOT_FONT_SIZE;

/// Opens one shorthand: writes its longhands and itself into the style.
type Expand = fn(&mut Style, &str, &str);

/// The expansion of the shorthand `key`, if it is one.
pub(super) fn expansion(key: &str) -> Option<Expand> {
    Some(match key {
        "padding" | "margin" | "inset" | "scroll-margin" | "scroll-padding" => sides,
        "border-width" | "border-style" | "border-color" => border_sides,
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            border
        }
        "border-radius" => radius,
        "background" => background,
        "gap" | "grid-gap" => gap,
        "flex" => flex,
        "flex-flow" => flex_flow,
        "overflow" => overflow,
        "place-items" | "place-content" | "place-self" => place,
        _ => return None,
    })
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn sides(style: &mut Style, key: &str, value: &str) {
    for (side, v) in SIDES.iter().zip(box_sides(value)) {
        let name = if key == "inset" {
            (*side).to_owned()
        } else {
            format!("{key}-{side}")
        };
        style.insert(name, v);
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn border_sides(style: &mut Style, key: &str, value: &str) {
    let part = key.trim_start_matches("border-");
    for (side, v) in SIDES.iter().zip(box_sides(value)) {
        style.insert(format!("border-{side}-{part}"), v);
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn border(style: &mut Style, key: &str, value: &str) {
    let (width, line, color) = border_parts(value);
    let targets: Vec<String> = match key {
        "border" => SIDES.iter().map(|s| format!("border-{s}")).collect(),
        other => vec![other.to_owned()],
    };
    for target in &targets {
        style.insert(format!("{target}-width"), width.clone());
        style.insert(format!("{target}-style"), line.clone());
        style.insert(format!("{target}-color"), color.clone());
        style.insert(target.clone(), value.to_owned());
    }
    if key == "border" {
        style.insert("border-width".into(), width);
        style.insert("border-style".into(), line);
        style.insert("border-color".into(), color);
        style.insert("border".into(), value.to_owned());
    }
}

fn radius(style: &mut Style, key: &str, value: &str) {
    let first = value.split('/').next().unwrap_or(value);
    let corners = ["top-left", "top-right", "bottom-right", "bottom-left"];
    for (corner, v) in corners.iter().zip(box_sides(first)) {
        style.insert(format!("border-{corner}-radius"), v);
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn background(style: &mut Style, _: &str, value: &str) {
    // The color is what a drawer reads; the rest is kept whole.
    let color = split_outside(value, ' ')
        .into_iter()
        .map(|p| p.trim().to_owned())
        .rfind(|p| color(p).is_some() || p.starts_with("var(") || p == "currentcolor")
        .unwrap_or_else(|| "transparent".to_owned());
    style.insert("background-color".into(), color);
    if value.contains("url(") || value.contains("gradient(") {
        style.insert("background-image".into(), value.to_owned());
    }
    style.insert("background".into(), value.to_owned());
}

fn gap(style: &mut Style, _: &str, value: &str) {
    let mut parts = value.split_whitespace();
    let row = parts.next().unwrap_or("0").to_owned();
    let column = parts.next().map_or_else(|| row.clone(), str::to_owned);
    style.insert("row-gap".into(), row);
    style.insert("column-gap".into(), column);
    // `grid-gap` is the old name of `gap`.
    style.insert("gap".into(), value.to_owned());
}

fn flex(style: &mut Style, key: &str, value: &str) {
    let (grow, shrink, basis) = flex_parts(value);
    style.insert("flex-grow".into(), grow);
    style.insert("flex-shrink".into(), shrink);
    style.insert("flex-basis".into(), basis);
    style.insert(key.to_owned(), value.to_owned());
}

fn flex_flow(style: &mut Style, key: &str, value: &str) {
    for part in value.split_whitespace() {
        if part.contains("wrap") {
            style.insert("flex-wrap".into(), part.to_owned());
        } else {
            style.insert("flex-direction".into(), part.to_owned());
        }
    }
    style.insert(key.to_owned(), value.to_owned());
}

fn overflow(style: &mut Style, key: &str, value: &str) {
    let mut parts = value.split_whitespace();
    let x = parts.next().unwrap_or("visible").to_owned();
    let y = parts.next().map_or_else(|| x.clone(), str::to_owned);
    style.insert("overflow-x".into(), x);
    style.insert("overflow-y".into(), y);
    style.insert(key.to_owned(), value.to_owned());
}

fn place(style: &mut Style, key: &str, value: &str) {
    let part = key.trim_start_matches("place-");
    let mut parts = value.split_whitespace();
    let align = parts.next().unwrap_or("normal").to_owned();
    let justify = parts.next().map_or_else(|| align.clone(), str::to_owned);
    style.insert(format!("align-{part}"), align);
    style.insert(format!("justify-{part}"), justify);
    style.insert(key.to_owned(), value.to_owned());
}

fn border_parts(value: &str) -> (String, String, String) {
    let mut width = "medium".to_owned();
    let mut line = "none".to_owned();
    let mut color = "currentcolor".to_owned();
    for part in split_outside(value, ' ') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if length(part, ROOT_FONT_SIZE).is_some() || matches!(part, "thin" | "medium" | "thick") {
            width = part.to_owned();
        } else if matches!(
            part,
            "none"
                | "hidden"
                | "solid"
                | "dashed"
                | "dotted"
                | "double"
                | "groove"
                | "ridge"
                | "inset"
                | "outset"
        ) {
            line = part.to_owned();
        } else {
            color = part.to_owned();
        }
    }
    if line == "none" {
        width = "0".to_owned();
    }
    (width, line, color)
}

fn flex_parts(value: &str) -> (String, String, String) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["none"] => ("0".into(), "0".into(), "auto".into()),
        ["auto"] => ("1".into(), "1".into(), "auto".into()),
        [one] if one.parse::<f32>().is_ok() => ((*one).into(), "1".into(), "0%".into()),
        [one] => ("1".into(), "1".into(), (*one).into()),
        [a, b] if b.parse::<f32>().is_ok() => ((*a).into(), (*b).into(), "0%".into()),
        [a, b] => ((*a).into(), "1".into(), (*b).into()),
        [a, b, c, ..] => ((*a).into(), (*b).into(), (*c).into()),
        [] => ("0".into(), "1".into(), "auto".into()),
    }
}

fn box_sides(value: &str) -> [String; 4] {
    let parts: Vec<String> = split_outside(value, ' ')
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    let pick = |i: usize| parts.get(i).cloned().unwrap_or_else(|| "0".to_owned());
    match parts.len() {
        1 => [pick(0), pick(0), pick(0), pick(0)],
        2 => [pick(0), pick(1), pick(0), pick(1)],
        3 => [pick(0), pick(1), pick(2), pick(1)],
        _ => [pick(0), pick(1), pick(2), pick(3)],
    }
}
