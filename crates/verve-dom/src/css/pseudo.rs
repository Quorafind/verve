//! The pseudo-elements (CSS Pseudo-Elements 4): parts of a document that have a style of their
//! own without being nodes of the tree — `::before` and `::after` with what `content`
//! generates, a list item's `::marker`, a field's `::placeholder`, `::selection`, a scrollbar's
//! parts, `::backdrop`.
//!
//! They are not in the DOM, as in a browser: `childNodes` never sees them. What a drawer needs
//! is a box and its style, which the cascade works out beside the host element's and the
//! snapshot carries.

use std::sync::Arc;

use super::cascade::Style;
use super::syntax::{function_parts, split_outside, unquote};
use crate::dom::Node;

/// A part of an element that is styled but is no node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PseudoElement {
    Before,
    After,
    /// A list item's marker.
    Marker,
    /// A field's hint, shown while it holds no value.
    Placeholder,
    /// What is selected.
    Selection,
    FirstLine,
    FirstLetter,
    /// `::backdrop`: behind a dialog or an element in fullscreen.
    Backdrop,
    /// A scrollbar's own boxes, as WebKit names them.
    WebkitScrollbar(ScrollbarPart),
}

/// Which box of a scrollbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScrollbarPart {
    /// The whole bar.
    Scrollbar,
    /// An arrow button at either end.
    Button,
    /// The part that is dragged.
    Thumb,
    /// The groove the thumb moves in.
    Track,
    /// The groove on either side of the thumb.
    TrackPiece,
    /// Where a horizontal and a vertical bar meet.
    Corner,
    /// The handle that resizes the element.
    Resizer,
}

impl PseudoElement {
    /// The pseudo-element `name` is, without its colons: `before`, `-webkit-scrollbar-thumb`.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "before" => Self::Before,
            "after" => Self::After,
            "marker" => Self::Marker,
            "placeholder" => Self::Placeholder,
            "selection" => Self::Selection,
            "first-line" => Self::FirstLine,
            "first-letter" => Self::FirstLetter,
            "backdrop" => Self::Backdrop,
            "-webkit-scrollbar" => Self::WebkitScrollbar(ScrollbarPart::Scrollbar),
            "-webkit-scrollbar-button" => Self::WebkitScrollbar(ScrollbarPart::Button),
            "-webkit-scrollbar-thumb" => Self::WebkitScrollbar(ScrollbarPart::Thumb),
            "-webkit-scrollbar-track" => Self::WebkitScrollbar(ScrollbarPart::Track),
            "-webkit-scrollbar-track-piece" => Self::WebkitScrollbar(ScrollbarPart::TrackPiece),
            "-webkit-scrollbar-corner" => Self::WebkitScrollbar(ScrollbarPart::Corner),
            "-webkit-resizer" => Self::WebkitScrollbar(ScrollbarPart::Resizer),
            _ => return None,
        })
    }

    /// The pseudo-element a `getComputedStyle` argument names: `::before`, and `:before` for
    /// the four CSS 2 wrote with one colon. `None` for anything else.
    pub(crate) fn from_argument(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        match text.strip_prefix("::") {
            Some(name) => Self::from_name(name),
            None => match text.strip_prefix(':')? {
                legacy @ ("before" | "after" | "first-line" | "first-letter") => {
                    Self::from_name(legacy)
                }
                _ => None,
            },
        }
    }

    /// Whether `node`, styled `style`, can have this part. A page that styles a part
    /// of every element (`::selection`, a bare `::placeholder`) is not given a box for each
    /// element the part means nothing to.
    pub(super) fn is_part_of(self, node: &Node, style: &Style) -> bool {
        let tag = node.tag();
        let scrolls = |axis: &str| {
            style
                .get(axis)
                .is_some_and(|value| matches!(value.as_str(), "auto" | "scroll" | "overlay"))
        };
        match self {
            Self::Marker => {
                matches!(tag, Some("li" | "summary"))
                    || style.get("display").is_some_and(|d| d == "list-item")
            }
            Self::Placeholder => matches!(tag, Some("input" | "textarea")),
            Self::Backdrop => tag == Some("dialog") || node.attr("popover").is_some(),
            Self::WebkitScrollbar(_) => {
                matches!(tag, Some("html" | "body" | "textarea"))
                    || scrolls("overflow-x")
                    || scrolls("overflow-y")
            }
            // Replaced and void elements have no content to put a box before or after.
            Self::Before | Self::After => !matches!(
                tag,
                Some(
                    "img"
                        | "input"
                        | "br"
                        | "video"
                        | "audio"
                        | "canvas"
                        | "iframe"
                        | "embed"
                        | "object"
                        | "select"
                        | "wbr"
                        | "area"
                        | "col"
                        | "source"
                        | "track"
                )
            ),
            Self::Selection | Self::FirstLine | Self::FirstLetter => true,
        }
    }
}

/// What `content` says, before the two things only a walk of the document can fill in: `attr()`
/// reads the element, `counter()` the counters where the box stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Piece {
    Text(String),
    Image(String),
    OpenQuote,
    CloseQuote,
    Attr(String),
    /// `counter(name, style)`, or with a separator `counters(name, separator, style)`.
    Counter {
        name: String,
        separator: Option<String>,
        style: String,
    },
}

/// What `content` generates, in order, as the snapshot gives it to a drawer: counters are
/// counted and `attr()` read, so all that is left is text, an image and the quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentItem {
    Text(String),
    /// `url(…)`: the drawer fetches and draws it.
    Image(String),
    OpenQuote,
    CloseQuote,
}

/// A generated box, or the style of a part that is no box: what the snapshot gives a drawer.
#[derive(Clone, Debug, PartialEq)]
pub struct PseudoBox {
    pub style: Arc<Style>,
    pub content: Vec<ContentItem>,
}

/// What a `content` value generates: `"*"`, `attr(data-x)`, `open-quote`, `url(a.png)`,
/// `counter(item)`. `none` and `normal` generate nothing.
pub(super) fn content_pieces(value: &str) -> Vec<Piece> {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("none")
        || value.eq_ignore_ascii_case("normal")
    {
        return Vec::new();
    }
    // `"a" / "an alternative for a reader"`: the text after the slash is not drawn.
    let value = split_outside(value, '/')
        .into_iter()
        .next()
        .unwrap_or_default();
    split_outside(&value.replace(['\t', '\n'], " "), ' ')
        .iter()
        .filter_map(|part| piece(part.trim()))
        .collect()
}

/// One item of a `content` value, `None` for one that generates nothing.
fn piece(part: &str) -> Option<Piece> {
    if part.starts_with(['"', '\'']) {
        return unquote(part).map(Piece::Text);
    }
    if let Some((name, args)) = function_parts(part) {
        let args = split_outside(args, ',');
        let arg = |at: usize| args.get(at).map(|arg| arg.trim());
        return match (name.to_ascii_lowercase().as_str(), arg(0)?) {
            // `attr(data-x string, "")`: the name is the first word.
            ("attr", name) => Some(Piece::Attr(
                name.split_whitespace().next().unwrap_or(name).to_owned(),
            )),
            ("url", url) => Some(Piece::Image(unquote(url).unwrap_or_else(|| url.to_owned()))),
            ("counter", name) => Some(Piece::Counter {
                name: name.to_owned(),
                separator: None,
                style: arg(1).unwrap_or("decimal").to_owned(),
            }),
            ("counters", name) => Some(Piece::Counter {
                name: name.to_owned(),
                separator: Some(arg(1).and_then(unquote).unwrap_or_default()),
                style: arg(2).unwrap_or("decimal").to_owned(),
            }),
            _ => None,
        };
    }
    match part.to_ascii_lowercase().as_str() {
        "open-quote" => Some(Piece::OpenQuote),
        "close-quote" => Some(Piece::CloseQuote),
        _ => None,
    }
}
