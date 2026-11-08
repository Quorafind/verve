//! `innerText`'s getter: the text as it is rendered — what the cascade leaves out is left out,
//! whitespace collapses as `white-space` says, `text-transform` applies, and block boxes, `<br>`,
//! paragraphs and table cells and rows put line breaks and tabs between what they hold. This
//! follows the HTML specification's "rendered text collection steps", walking the flat tree as
//! the boxes are.

use crate::css::{Style, value_or_initial};
use crate::dom::{Dom, NodeId, NodeKind};

/// A piece of the collected text.
enum Item {
    /// Text, and whether its whitespace is the kind that collapses.
    Text(String, bool),
    /// At least this many line breaks here, whatever else asks for some.
    Breaks(usize),
}

/// What `white-space` does to the whitespace of a text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhiteSpace {
    Collapse,
    /// Collapses, but keeps newlines.
    PreLine,
    Preserve,
}

/// One element's computed style, with the initial values behind it.
struct Styles {
    style: Style,
    tag: Option<String>,
}

impl Styles {
    fn get(&self, property: &str) -> &str {
        value_or_initial(&self.style, property, self.tag.as_deref())
    }

    fn display(&self) -> &str {
        self.get("display")
    }

    fn white_space(&self) -> WhiteSpace {
        match self.get("white-space") {
            "pre" | "pre-wrap" | "break-spaces" => WhiteSpace::Preserve,
            "pre-line" => WhiteSpace::PreLine,
            _ => WhiteSpace::Collapse,
        }
    }
}

/// What `innerText` reads for `id`, given each node's computed style: the rendered text, or
/// `textContent` when `id` is not being rendered at all.
pub(crate) fn inner_text(dom: &Dom, id: NodeId, style_of: &dyn Fn(NodeId) -> Style) -> String {
    let styles = |node: NodeId| Styles {
        style: style_of(node),
        tag: dom.get(node).and_then(|n| n.tag()).map(str::to_owned),
    };
    if !dom.is_connected(id) || styles(id).display() == "none" {
        return dom.text_content(id);
    }
    let mut items = Vec::new();
    collect(dom, &styles, id, &mut items);
    join(items)
}

fn collect(dom: &Dom, styles: &impl Fn(NodeId) -> Styles, id: NodeId, out: &mut Vec<Item>) {
    let Some(node) = dom.get(id) else { return };
    let element = match &node.kind {
        NodeKind::Text(text) => {
            let parent = dom.flat_parent(id).map(styles);
            if let Some(parent) = parent.filter(|p| p.get("visibility") == "visible") {
                let collapses = parent.white_space() != WhiteSpace::Preserve;
                out.push(Item::Text(transform(text, &parent), collapses));
            }
            return;
        }
        NodeKind::Element(tag) => tag.as_str(),
        _ => "",
    };
    let own = (!element.is_empty()).then(|| styles(id));
    if let Some(own) = &own
        && (own.display() == "none" || matches!(element, "script" | "style" | "template" | "head"))
    {
        return;
    }
    if element == "br" {
        out.push(Item::Text("\n".into(), false));
        return;
    }
    let display = own.as_ref().map_or("", Styles::display);
    let block = !display.is_empty()
        && !display.starts_with("inline")
        && !matches!(display, "contents" | "table-cell" | "table-row" | "ruby")
        && !display.starts_with("table-column")
        && !display.ends_with("-group");
    let breaks = match element {
        "p" => 2,
        _ if block => 1,
        _ => 0,
    };
    out.push(Item::Breaks(breaks));
    for child in dom.flat_children(id) {
        collect(dom, styles, child, out);
    }
    out.push(Item::Breaks(breaks));
    // A cell is followed by a tab and a row by a line break, unless it is the last.
    let next = dom.sibling(id, 1, true);
    let next_display = next.map(|n| styles(n).display().to_owned());
    match (display, next_display.as_deref()) {
        ("table-cell", Some("table-cell")) => out.push(Item::Text("\t".into(), false)),
        ("table-row", Some("table-row")) => out.push(Item::Breaks(1)),
        _ => {}
    }
}

/// `text` with its whitespace collapsed and its case changed as the style of its element says.
fn transform(text: &str, style: &Styles) -> String {
    let collapsed = match style.white_space() {
        WhiteSpace::Preserve => text.to_owned(),
        // The newlines `pre-line` keeps are kept; the rest of the whitespace still collapses.
        WhiteSpace::PreLine => text
            .split('\n')
            .map(collapse)
            .collect::<Vec<_>>()
            .join("\n"),
        WhiteSpace::Collapse => collapse(text),
    };
    match style.get("text-transform") {
        "uppercase" => collapsed.to_uppercase(),
        "lowercase" => collapsed.to_lowercase(),
        "capitalize" => capitalize(&collapsed),
        _ => collapsed,
    }
}

/// Each run of spaces, tabs and newlines as one space.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn capitalize(text: &str) -> String {
    let mut start = true;
    text.chars()
        .map(|c| {
            let up = start && c.is_alphabetic();
            start = !c.is_alphanumeric();
            if up {
                c.to_uppercase().collect::<String>()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// The items as one string: the line breaks asked for between two pieces of text are the most
/// any of them asked for, none at either end, and a collapsible space before a break or at the
/// start of a line goes.
fn join(items: Vec<Item>) -> String {
    let mut out = String::new();
    let mut pending = 0;
    let mut loose_end = false;
    for item in items {
        match item {
            Item::Breaks(n) => pending = pending.max(n),
            Item::Text(text, collapsible) => {
                if pending > 0 && !out.is_empty() {
                    if loose_end {
                        out.truncate(out.trim_end_matches(' ').len());
                    }
                    out.push_str(&"\n".repeat(pending));
                }
                pending = 0;
                let line_start = out.is_empty() || out.ends_with(['\n', ' ']);
                let text = if collapsible && line_start {
                    text.trim_start_matches(' ')
                } else {
                    &text
                };
                out.push_str(text);
                loose_end = collapsible;
            }
        }
    }
    if loose_end {
        out.truncate(out.trim_end_matches(' ').len());
    }
    out
}
