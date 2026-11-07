//! Property values: shorthands opened up, `var()`, the initial values, the user agent's sheet.

use std::collections::{HashMap, HashSet};

use super::calc::length;
use super::cascade::Style;
use super::shorthand::{expansion, pending};
use super::syntax::{decimal, split_outside};

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
    // SVG's painting properties are inherited, from an HTML ancestor too: Electron 39.8.10 paints
    // a `<rect>` red under `.btn { fill: red }` on a `<div>`, and `getComputedStyle(rect).fill`
    // is `rgb(255, 0, 0)`.
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "clip-rule",
    "paint-order",
    "shape-rendering",
    "text-anchor",
    "marker-start",
    "marker-mid",
    "marker-end",
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

/// What `1rem` is on a page that does not say otherwise: the initial `font-size`.
pub(crate) const ROOT_FONT_SIZE: f32 = 16.;

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
    format!("{}px", decimal(px))
}

/// Writes a declaration, opening up the shorthands this engine reads as their longhands, and
/// the keywords every property takes.
pub(super) fn set_declared(style: &mut Style, key: &str, value: &str, inherited: &Style) {
    let keyword = matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    );
    if keyword && expansion(key).is_some() {
        // A keyword on a shorthand is the keyword on each of its longhands.
        for longhand in super::shorthand::longhands(key) {
            if longhand != key {
                set_declared(style, &longhand, value, inherited);
            }
        }
    }
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
    match expansion(key) {
        Some(expand) => expand(style, key, value),
        None => {
            if key.starts_with("animation")
                || key.starts_with("transition")
                || key.starts_with("background")
            {
                style.remove(&pending(key));
            }
            style.insert(key.to_owned(), value.to_owned());
        }
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
        "background-image" => "none",
        "background-size" => "auto",
        "background-position" => "0% 0%",
        "background-repeat" => "repeat",
        "background-origin" => "padding-box",
        "background-clip" => "border-box",
        "font-size" => "16px",
        "font-weight" => "400",
        "font-style" => "normal",
        "line-height" => "normal",
        "overflow" | "overflow-x" | "overflow-y" => "visible",
        "float" | "-webkit-app-region" | "app-region" => "none",
        "z-index" | "width" | "height" | "top" | "left" | "right" | "bottom" => "auto",
        "box-sizing" => "content-box",
        "pointer-events" | "cursor" => "auto",
        "white-space" => "normal",
        "text-align" => "start",
        "direction" => "ltr",
        "transform" | "translate" | "rotate" | "scale" | "filter" | "backdrop-filter"
        | "box-shadow" | "text-shadow" | "max-width" | "max-height" | "border-style" => "none",
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
video { object-fit: contain }
audio:not([controls]) { display: none }
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
