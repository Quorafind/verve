//! Property values: shorthands opened up, `var()`, the initial values, lengths and colours.

use std::collections::{HashMap, HashSet};

use super::cascade::Style;
use super::syntax::split_outside;

/// The properties a child takes from its parent unless something says otherwise.
const INHERITED: &[&str] = &[
    "color",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "font-stretch",
    "font-feature-settings",
    "font-variation-settings",
    "line-height",
    "letter-spacing",
    "word-spacing",
    "text-align",
    "text-indent",
    "text-transform",
    "text-shadow",
    "text-rendering",
    "white-space",
    "word-break",
    "word-wrap",
    "overflow-wrap",
    "hyphens",
    "tab-size",
    "direction",
    "writing-mode",
    "visibility",
    "cursor",
    "caret-color",
    "list-style",
    "list-style-type",
    "list-style-position",
    "list-style-image",
    "quotes",
    "pointer-events",
    "border-collapse",
    "border-spacing",
    "empty-cells",
    "caption-side",
    "color-scheme",
    "-webkit-font-smoothing",
    "-moz-osx-font-smoothing",
    "user-select",
    "-webkit-user-select",
];

/// Whether `property` is inherited.
pub fn is_inherited(property: &str) -> bool {
    property.starts_with("--") || INHERITED.contains(&property)
}

/// What the `font` shorthand sets.
pub(super) const FONT_LONGHANDS: &[&str] = &[
    "font-style",
    "font-variant",
    "font-weight",
    "font-stretch",
    "font-size",
    "line-height",
    "font-family",
];

/// The `font` shorthand opened into its longhands: `[style] [variant] [weight] [stretch]
/// size[/line-height] family`, every longhand it leaves out reset to `normal`. A system font
/// (`caption`, `menu`…) or a value with no size answers `None`, and the shorthand is left as
/// written.
pub(crate) fn font_longhands(value: &str) -> Option<Vec<(&'static str, String)>> {
    let mut style = "normal".to_owned();
    let mut variant = "normal".to_owned();
    let mut weight = "normal".to_owned();
    let mut stretch = "normal".to_owned();
    let words = split_outside(value.trim(), ' ');
    let mut words = words.iter().map(|w| w.trim()).filter(|w| !w.is_empty());
    for word in words.by_ref() {
        match word {
            "normal" => {}
            "italic" | "oblique" => style = word.to_owned(),
            "small-caps" => variant = word.to_owned(),
            "bold" | "bolder" | "lighter" => weight = word.to_owned(),
            "condensed" | "expanded" | "semi-condensed" | "semi-expanded" | "extra-condensed"
            | "extra-expanded" | "ultra-condensed" | "ultra-expanded" => stretch = word.to_owned(),
            w if w.parse::<f32>().is_ok_and(|n| (1. ..=1000.).contains(&n)) => {
                weight = w.to_owned()
            }
            size => {
                let (size, line_height) = match size.split_once('/') {
                    Some((size, line)) => (size.to_owned(), line.to_owned()),
                    None => (size.to_owned(), "normal".to_owned()),
                };
                if size.is_empty() {
                    return None;
                }
                let family: Vec<&str> = words.collect();
                if family.is_empty() {
                    return None;
                }
                return Some(vec![
                    ("font-style", style),
                    ("font-variant", variant),
                    ("font-weight", weight),
                    ("font-stretch", stretch),
                    ("font-size", size),
                    ("line-height", line_height),
                    ("font-family", family.join(" ")),
                ]);
            }
        }
    }
    None
}

/// A font size in pixels, against the parent's: lengths, percentages, `em`, `rem`, and the
/// keywords.
pub(crate) fn font_size_px(value: &str, parent: f32) -> Option<f32> {
    let value = value.trim();
    let keyword = match value {
        "xx-small" => Some(9.),
        "x-small" => Some(10.),
        "small" => Some(13.),
        "medium" => Some(16.),
        "large" => Some(18.),
        "x-large" => Some(24.),
        "xx-large" => Some(32.),
        "xxx-large" => Some(48.),
        "smaller" => Some(parent / 1.2),
        "larger" => Some(parent * 1.2),
        _ => None,
    };
    if keyword.is_some() {
        return keyword;
    }
    if let Some(percent) = value.strip_suffix('%') {
        return percent
            .trim()
            .parse::<f32>()
            .ok()
            .map(|p| p / 100. * parent);
    }
    if value.ends_with("px") && !value.contains('(') {
        return None;
    }
    length(value, parent)
}

pub(super) fn format_px(px: f32) -> String {
    let rounded = (px * 1000.).round() / 1000.;
    if rounded.fract() == 0. {
        format!("{}px", rounded as i64)
    } else {
        format!("{rounded}px")
    }
}

/// Writes a declaration, opening up the shorthands this engine reads as their longhands, and
/// the keywords every property takes.
pub(super) fn set_declared(style: &mut Style, key: &str, value: &str, inherited: &Style) {
    match value.trim().to_ascii_lowercase().as_str() {
        "inherit" => {
            match inherited.get(key) {
                Some(parent) => style.insert(key.to_owned(), parent.clone()),
                None => style.remove(key),
            };
            return;
        }
        "initial" | "revert" | "revert-layer" => {
            style.remove(key);
            return;
        }
        "unset" => {
            if is_inherited(key) {
                match inherited.get(key) {
                    Some(parent) => style.insert(key.to_owned(), parent.clone()),
                    None => style.remove(key),
                };
            } else {
                style.remove(key);
            }
            return;
        }
        _ => {}
    }
    let sides = ["top", "right", "bottom", "left"];
    match key {
        "padding" | "margin" | "inset" | "scroll-margin" | "scroll-padding" => {
            for (side, v) in sides.iter().zip(box_sides(value)) {
                let name = if key == "inset" {
                    (*side).to_owned()
                } else {
                    format!("{key}-{side}")
                };
                style.insert(name, v);
            }
            style.insert(key.to_owned(), value.to_owned());
        }
        "border-width" | "border-style" | "border-color" => {
            let part = key.trim_start_matches("border-");
            for (side, v) in sides.iter().zip(box_sides(value)) {
                style.insert(format!("border-{side}-{part}"), v);
            }
            style.insert(key.to_owned(), value.to_owned());
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            let (width, line, colour) = border_parts(value);
            let targets: Vec<String> = match key {
                "border" => sides.iter().map(|s| format!("border-{s}")).collect(),
                other => vec![other.to_owned()],
            };
            for target in &targets {
                style.insert(format!("{target}-width"), width.clone());
                style.insert(format!("{target}-style"), line.clone());
                style.insert(format!("{target}-color"), colour.clone());
                style.insert(target.clone(), value.to_owned());
            }
            if key == "border" {
                style.insert("border-width".into(), width);
                style.insert("border-style".into(), line);
                style.insert("border-color".into(), colour);
                style.insert("border".into(), value.to_owned());
            }
        }
        "border-radius" => {
            let first = value.split('/').next().unwrap_or(value);
            let corners = ["top-left", "top-right", "bottom-right", "bottom-left"];
            for (corner, v) in corners.iter().zip(box_sides(first)) {
                style.insert(format!("border-{corner}-radius"), v);
            }
            style.insert(key.to_owned(), value.to_owned());
        }
        "background" => {
            // The colour is what a drawer reads; the rest is kept whole.
            let colour = split_outside(value, ' ')
                .into_iter()
                .map(|p| p.trim().to_owned())
                .rfind(|p| color(p).is_some() || p.starts_with("var(") || p == "currentcolor")
                .unwrap_or_else(|| "transparent".to_owned());
            style.insert("background-color".into(), colour);
            if value.contains("url(") || value.contains("gradient(") {
                style.insert("background-image".into(), value.to_owned());
            }
            style.insert("background".into(), value.to_owned());
        }
        "gap" | "grid-gap" => {
            let mut parts = value.split_whitespace();
            let row = parts.next().unwrap_or("0").to_owned();
            let column = parts.next().map_or_else(|| row.clone(), str::to_owned);
            style.insert("row-gap".into(), row);
            style.insert("column-gap".into(), column);
            style.insert("gap".into(), value.to_owned());
        }
        "flex" => {
            let (grow, shrink, basis) = flex_parts(value);
            style.insert("flex-grow".into(), grow);
            style.insert("flex-shrink".into(), shrink);
            style.insert("flex-basis".into(), basis);
            style.insert("flex".into(), value.to_owned());
        }
        "flex-flow" => {
            for part in value.split_whitespace() {
                if part.contains("wrap") {
                    style.insert("flex-wrap".into(), part.to_owned());
                } else {
                    style.insert("flex-direction".into(), part.to_owned());
                }
            }
            style.insert("flex-flow".into(), value.to_owned());
        }
        "overflow" => {
            let mut parts = value.split_whitespace();
            let x = parts.next().unwrap_or("visible").to_owned();
            let y = parts.next().map_or_else(|| x.clone(), str::to_owned);
            style.insert("overflow-x".into(), x);
            style.insert("overflow-y".into(), y);
            style.insert("overflow".into(), value.to_owned());
        }
        "place-items" | "place-content" | "place-self" => {
            let part = key.trim_start_matches("place-");
            let mut parts = value.split_whitespace();
            let align = parts.next().unwrap_or("normal").to_owned();
            let justify = parts.next().map_or_else(|| align.clone(), str::to_owned);
            style.insert(format!("align-{part}"), align);
            style.insert(format!("justify-{part}"), justify);
            style.insert(key.to_owned(), value.to_owned());
        }
        _ => {
            style.insert(key.to_owned(), value.to_owned());
        }
    }
}

fn border_parts(value: &str) -> (String, String, String) {
    let mut width = "medium".to_owned();
    let mut line = "none".to_owned();
    let mut colour = "currentcolor".to_owned();
    for part in split_outside(value, ' ') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if length(part, 16.).is_some() || matches!(part, "thin" | "medium" | "thick") {
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
            colour = part.to_owned();
        }
    }
    if line == "none" {
        width = "0".to_owned();
    }
    (width, line, colour)
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

// One substitution yields at most 64 KiB; a cycle or an overrun makes the value invalid, so
// the blow-up is never passed on to descendants.
const MAX_VARIABLE_BYTES: usize = 64 * 1024;

fn next_var(value: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (offset, c) in value[from..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if matches!(c, '\'' | '"') {
            quote = Some(c);
        } else if value[from + offset..].starts_with("var(") {
            return Some(from + offset);
        }
    }
    None
}

pub(super) fn resolve_custom(custom: &HashMap<String, String>) -> HashMap<String, String> {
    let dependencies: HashMap<String, Vec<String>> = custom
        .iter()
        .map(|(name, value)| {
            let mut dependencies = Vec::new();
            let mut from = 0;
            while let Some(start) = next_var(value, from) {
                from = start + 4;
                let name = value[from..].split([',', ')']).next().unwrap_or("").trim();
                if custom.contains_key(name) {
                    dependencies.push(name.to_owned());
                }
            }
            (name.clone(), dependencies)
        })
        .collect();
    let mut visited = HashSet::new();
    let mut order = Vec::new();
    for name in custom.keys() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let mut stack = vec![(name.clone(), 0)];
        while let Some((name, index)) = stack.last().cloned() {
            if let Some(next) = dependencies[&name].get(index) {
                stack.last_mut().expect("the stack was just read from").1 += 1;
                if visited.insert(next.clone()) {
                    stack.push((next.clone(), 0));
                }
            } else {
                stack.pop();
                order.push(name);
            }
        }
    }
    // Walk back over the whole strongly connected component, not only the path one back edge
    // came along.
    let mut reversed = HashMap::<String, Vec<String>>::new();
    for (name, dependencies) in &dependencies {
        for dependency in dependencies {
            reversed
                .entry(dependency.clone())
                .or_default()
                .push(name.clone());
        }
    }
    visited.clear();
    let mut invalid = HashSet::new();
    for name in order.iter().rev() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let mut component = Vec::new();
        let mut stack = vec![name.clone()];
        while let Some(at) = stack.pop() {
            component.push(at.clone());
            if let Some(next) = reversed.get(&at) {
                for next in next {
                    if visited.insert(next.clone()) {
                        stack.push(next.clone());
                    }
                }
            }
        }
        if component.len() > 1 || dependencies[name].contains(name) {
            invalid.extend(component);
        }
    }
    let mut resolved = HashMap::new();
    for name in order {
        if !invalid.contains(&name)
            && let Some(value) = substitute(&custom[&name], &resolved, 0)
        {
            resolved.insert(name, value);
        }
    }
    resolved
}

pub(super) fn substitute(
    value: &str,
    custom: &HashMap<String, String>,
    depth: usize,
) -> Option<String> {
    if value.len() > MAX_VARIABLE_BYTES || depth >= 64 {
        return None;
    }
    let mut out = String::new();
    let mut from = 0;
    while let Some(start) = next_var(value, from) {
        out.push_str(&value[from..start]);
        let inner_start = start + 4;
        let mut paren = 1;
        let mut end = None;
        let mut quote = None;
        let mut escaped = false;
        for (offset, c) in value[inner_start..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '\'' | '"' => quote = Some(c),
                '(' => paren += 1,
                ')' => {
                    paren -= 1;
                    if paren == 0 {
                        end = Some(inner_start + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        let inner = &value[inner_start..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
            None => (inner.trim(), None),
        };
        if !name.starts_with("--") {
            return None;
        }
        let replacement = match custom.get(name) {
            Some(value) => value.clone(),
            None => substitute(fallback?, custom, depth + 1)?,
        };
        if out.len().saturating_add(replacement.len()) > MAX_VARIABLE_BYTES {
            return None;
        }
        out.push_str(&replacement);
        from = end + 1;
    }
    if out.len().saturating_add(value.len() - from) > MAX_VARIABLE_BYTES {
        return None;
    }
    out.push_str(&value[from..]);
    Some(out)
}

/// `property` of `style`, or its initial value for an element of `tag` when nothing set it.
pub(crate) fn value_or_initial<'a>(style: &'a Style, property: &str, tag: Option<&str>) -> &'a str {
    let declared = style.get(property).map(String::as_str);
    declared
        .or_else(|| initial_value(property, tag))
        .unwrap_or("")
}

/// What a property is when nothing set it, as `getComputedStyle` answers it.
pub fn initial_value(property: &str, tag: Option<&str>) -> Option<&'static str> {
    Some(match property {
        "display" => match tag {
            Some(tag) if BLOCK_TAGS.contains(&tag) => "block",
            Some("li") => "list-item",
            Some("table") => "table",
            Some("tr") => "table-row",
            Some("td" | "th") => "table-cell",
            Some("thead") => "table-header-group",
            Some("tbody") => "table-row-group",
            Some("tfoot") => "table-footer-group",
            Some("head" | "script" | "style" | "template" | "title" | "meta" | "link") => "none",
            _ => "inline",
        },
        "position" => "static",
        "visibility" => "visible",
        "opacity" => "1",
        "color" => "rgb(0, 0, 0)",
        "background-color" => "rgba(0, 0, 0, 0)",
        "font-size" => "16px",
        "font-weight" => "400",
        "font-style" => "normal",
        "line-height" => "normal",
        "overflow" | "overflow-x" | "overflow-y" => "visible",
        "float" => "none",
        "z-index" | "width" | "height" | "top" | "left" | "right" | "bottom" => "auto",
        "box-sizing" => "content-box",
        "pointer-events" | "cursor" => "auto",
        "white-space" => "normal",
        "text-align" => "start",
        "direction" => "ltr",
        "transform" | "max-width" | "max-height" | "border-style" => "none",
        "min-width" | "min-height" => "0px",
        "margin-top"
        | "margin-right"
        | "margin-bottom"
        | "margin-left"
        | "padding-top"
        | "padding-right"
        | "padding-bottom"
        | "padding-left"
        | "border-top-width"
        | "border-right-width"
        | "border-bottom-width"
        | "border-left-width" => "0px",
        "flex-direction" => "row",
        "flex-wrap" => "nowrap",
        "flex-grow" => "0",
        "flex-shrink" => "1",
        "flex-basis" => "auto",
        "transition-duration" | "animation-duration" | "transition-delay" | "animation-delay" => {
            "0s"
        }
        "animation-name" => "none",
        _ => return None,
    })
}

const BLOCK_TAGS: &[&str] = &[
    "html",
    "body",
    "div",
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "pre",
    "blockquote",
    "section",
    "article",
    "aside",
    "header",
    "footer",
    "nav",
    "main",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "address",
    "details",
    "summary",
    "hr",
    "legend",
    "menu",
    "dialog",
    "search",
    "hgroup",
    "center",
    "optgroup",
];

/// The user agent's own sheet: what an element looks like before any author says otherwise —
/// enough of a browser's for a drawer to lay a page out by (`display`, the headings' sizes, the
/// lists' indents, what is never drawn).
pub const USER_AGENT_SHEET: &str = r#"
html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, dl, dt, dd, pre, blockquote, section, article,
aside, header, footer, nav, main, form, fieldset, figure, figcaption, address, details, summary,
hr, legend, menu, dialog, search, hgroup, center, optgroup { display: block }
head, script, style, template, title, meta, link, base, noscript, datalist, param, area { display: none }
[hidden] { display: none }
li { display: list-item }
table { display: table; border-collapse: separate; border-spacing: 2px }
caption { display: table-caption; text-align: center }
thead { display: table-header-group }
tbody { display: table-row-group }
tfoot { display: table-footer-group }
tr { display: table-row }
td, th { display: table-cell; padding: 1px }
th { font-weight: bold; text-align: center }
col { display: table-column }
colgroup { display: table-column-group }
input, select, textarea, button, img, video, canvas, iframe, svg, audio, progress, meter { display: inline-block }
body { margin: 8px }
p, blockquote, figure, dl, menu { margin-top: 1em; margin-bottom: 1em }
ul, ol { margin-top: 1em; margin-bottom: 1em; padding-left: 40px }
ul { list-style-type: disc }
ol { list-style-type: decimal }
blockquote, figure { margin-left: 40px; margin-right: 40px }
dd { margin-left: 40px }
h1 { font-size: 2em; margin-top: 0.67em; margin-bottom: 0.67em; font-weight: bold }
h2 { font-size: 1.5em; margin-top: 0.83em; margin-bottom: 0.83em; font-weight: bold }
h3 { font-size: 1.17em; margin-top: 1em; margin-bottom: 1em; font-weight: bold }
h4 { margin-top: 1.33em; margin-bottom: 1.33em; font-weight: bold }
h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em; font-weight: bold }
h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em; font-weight: bold }
b, strong { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small { font-size: smaller }
big { font-size: larger }
sub { vertical-align: sub; font-size: smaller }
sup { vertical-align: super; font-size: smaller }
code, kbd, samp, tt, pre { font-family: monospace }
pre { white-space: pre; margin-top: 1em; margin-bottom: 1em }
textarea { white-space: pre-wrap }
a:any-link { color: #0000ee; text-decoration: underline; cursor: pointer }
mark { background-color: yellow; color: black }
hr { border: 1px inset; margin-top: 0.5em; margin-bottom: 0.5em }
button { cursor: default }
center { text-align: center }
"#;

/// A length in pixels: `12px`, `0.5em` against `font_size`, `1rem` against 16, `pt`, a bare
/// number or `0`.
pub fn length(text: &str, font_size: f32) -> Option<f32> {
    let text = text.trim();
    if text == "0" {
        return Some(0.);
    }
    let number = |suffix: &str| text.strip_suffix(suffix)?.trim().parse::<f32>().ok();
    number("px")
        .or_else(|| number("rem").map(|n| n * 16.))
        .or_else(|| number("em").map(|n| n * font_size))
        .or_else(|| number("pt").map(|n| n * 4. / 3.))
        .or_else(|| text.parse::<f32>().ok())
}

/// A colour as red, green, blue and alpha in `0..=1`: hex, `rgb()`/`rgba()`, `hsl()`/`hsla()`
/// and the named colours.
pub fn color(text: &str) -> Option<[f32; 4]> {
    let text = text.trim().to_ascii_lowercase();
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let pair = |a: u8, b: u8| f32::from(a * 16 + b) / 255.;
        let single = |a: u8| f32::from(a * 17) / 255.;
        return match digits.len() {
            3 => Some([single(digits[0]), single(digits[1]), single(digits[2]), 1.]),
            4 => Some([
                single(digits[0]),
                single(digits[1]),
                single(digits[2]),
                single(digits[3]),
            ]),
            6 => Some([
                pair(digits[0], digits[1]),
                pair(digits[2], digits[3]),
                pair(digits[4], digits[5]),
                1.,
            ]),
            8 => Some([
                pair(digits[0], digits[1]),
                pair(digits[2], digits[3]),
                pair(digits[4], digits[5]),
                pair(digits[6], digits[7]),
            ]),
            _ => None,
        };
    }
    let function = |prefixes: &[&str]| -> Option<Vec<(f32, bool)>> {
        let args = prefixes
            .iter()
            .find_map(|p| text.strip_prefix(p))
            .and_then(|rest| rest.strip_suffix(')'))?;
        Some(
            args.split([',', ' ', '/'])
                .filter(|p| !p.is_empty())
                .filter_map(|p| match p.strip_suffix('%') {
                    Some(percent) => percent.parse::<f32>().ok().map(|v| (v, true)),
                    None => p
                        .trim_end_matches("deg")
                        .parse::<f32>()
                        .ok()
                        .map(|v| (v, false)),
                })
                .collect(),
        )
    };
    if let Some(parts) = function(&["rgba(", "rgb("]) {
        if parts.len() < 3 {
            return None;
        }
        let channel = |(v, percent): (f32, bool)| if percent { v / 100. } else { v / 255. };
        let alpha = parts
            .get(3)
            .map_or(1., |(v, percent)| if *percent { v / 100. } else { *v });
        return Some([
            channel(parts[0]),
            channel(parts[1]),
            channel(parts[2]),
            alpha,
        ]);
    }
    if let Some(parts) = function(&["hsla(", "hsl("]) {
        if parts.len() < 3 {
            return None;
        }
        let h = parts[0].0.rem_euclid(360.) / 360.;
        let s = parts[1].0 / 100.;
        let l = parts[2].0 / 100.;
        let alpha = parts
            .get(3)
            .map_or(1., |(v, percent)| if *percent { v / 100. } else { *v });
        let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
        let p = 2. * l - q;
        let hue = |mut t: f32| {
            if t < 0. {
                t += 1.;
            }
            if t > 1. {
                t -= 1.;
            }
            if t < 1. / 6. {
                p + (q - p) * 6. * t
            } else if t < 0.5 {
                q
            } else if t < 2. / 3. {
                p + (q - p) * (2. / 3. - t) * 6.
            } else {
                p
            }
        };
        return Some([hue(h + 1. / 3.), hue(h), hue(h - 1. / 3.), alpha]);
    }
    if text == "transparent" {
        return Some([0., 0., 0., 0.]);
    }
    let hex = NAMED_COLORS
        .iter()
        .find(|(name, _)| *name == text)
        .map(|(_, hex)| *hex)?;
    color(&format!("#{hex:06x}"))
}

/// CSS's named colours.
const NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];
