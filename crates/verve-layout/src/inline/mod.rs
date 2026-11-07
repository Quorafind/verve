//! Inline layout: the text of a block, and the inline boxes in it, broken into lines.
//!
//! A block whose content is inline (text, `<span>`, `<br>`, `inline-block`, `<img>`) is an
//! *inline formatting context*. [`collect`] gathers its content into [`Ifc`] items (white space
//! collapsed, text shaped), `chunks` cuts it at the places a line may break, and `lines` fills
//! lines with the chunks, aligns them and puts each box at its height. The block's taffy node is a
//! leaf whose measure function is [`Ifc::measure`]; the lines at the width taffy settled on are
//! read afterwards for where everything is.

mod chunks;
mod collect;
mod finish;
mod ifc;
mod lines;
mod vertical;

pub(crate) mod breaks;
pub(crate) mod space;
pub(crate) mod style;

use std::ops::Range;
use std::sync::Arc;

use verve_dom::{NodeId, Rect};

pub(crate) use collect::{Collector, Plan, has_content};
pub(crate) use ifc::{Atom, Ifc, Parts};
pub use style::{DecorationStyle, TextDecoration, TextPaint, TextShadow};

use crate::measure::ShapedRun;

/// The lines of one inline formatting context. Every position is in CSS pixels from the top left
/// of the block's content box.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineLayout {
    pub lines: Vec<LineBox>,
    /// The height of the lines together: the content height of the block.
    pub height: f32,
    /// The distance from the top to the baseline of the first line, and of the last.
    pub first_baseline: f32,
    pub last_baseline: f32,
    /// How far the widest line reaches from the left edge, overhanging text included.
    pub width: f32,
}

/// One line.
#[derive(Clone, Debug, PartialEq)]
pub struct LineBox {
    /// The line box: as wide as the block's content and as high as the line.
    pub rect: Rect,
    /// The baseline's distance from the top of the block's content.
    pub baseline: f32,
    pub fragments: Vec<Fragment>,
    /// One border box for each inline element that has content on this line, outermost first.
    pub spans: Vec<SpanBox>,
}

/// What a line holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Fragment {
    Text(TextFragment),
    /// An `inline-block`, `<img>` or other atomic inline: its border box.
    Atomic {
        node: NodeId,
        rect: Rect,
    },
}

/// Text on one line, in one font and style.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFragment {
    /// The text node; for generated text (`::before`, the ellipsis) the element it belongs to.
    pub node: NodeId,
    /// The bytes of the text node's text this covers (empty for generated text).
    pub range: Range<usize>,
    /// The shaped text. Glyph positions are from the fragment's left edge, letter spacing, word
    /// spacing and justification included.
    pub run: Arc<ShapedRun>,
    /// The left edge, from the left of the block's content.
    pub x: f32,
    pub width: f32,
    /// The baseline's distance from the top of the block's content.
    pub baseline: f32,
    pub paint: Arc<TextPaint>,
    /// Where a caret can stand: the byte of the text node and the x from the fragment's left edge,
    /// each character in turn, then the end.
    pub stops: Vec<Stop>,
    pub generated: bool,
    /// The text cannot be selected.
    pub select_none: bool,
}

/// A caret position in a text fragment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stop {
    pub byte: u32,
    pub x: f32,
}

/// The border box of an inline element on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanBox {
    pub node: NodeId,
    pub rect: Rect,
    /// The line has the element's start (its left border and padding are drawn here).
    pub starts: bool,
    /// The line has the element's end.
    pub ends: bool,
}
