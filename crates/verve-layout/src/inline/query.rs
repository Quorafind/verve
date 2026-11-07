//! What a script and a drawer ask of the lines: the boxes of an inline element on each line, the
//! boxes of a range of text, and the text position under a point.
//!
//! Positions in the lines are bytes of the text node (UTF-8); the DOM counts UTF-16 units, so
//! every question and answer here is converted at the edge.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use verve_dom::{NodeId, Rect, Snapshot, utf8_offset_to_utf16, utf16_offset_to_utf8};

use super::{Fragment, InlineLayout, LineBox, Stop, TextFragment};
use crate::measure::TextMeasurer;

/// The lines of one inline formatting context, placed on the screen.
#[derive(Clone, Debug)]
pub(crate) struct Placement {
    /// The box that holds the lines.
    pub holder: usize,
    /// The top left of the block's content box, in viewport coordinates.
    pub origin: [f32; 2],
    pub layout: Arc<InlineLayout>,
    /// The block's border box, which a point has to be in to be asked about its text.
    pub bounds: Rect,
}

/// What the questions are asked of.
pub(crate) struct View<'a> {
    pub snapshot: &'a Snapshot,
    pub measurer: &'a dyn TextMeasurer,
    pub blocks: &'a [Placement],
    /// The document order of the nodes, worked out once for each layout.
    pub ranks: &'a OnceLock<HashMap<NodeId, usize>>,
}

impl Placement {
    fn shifted(&self, rect: Rect) -> Rect {
        Rect::new(
            rect.x + self.origin[0],
            rect.y + self.origin[1],
            rect.width,
            rect.height,
        )
    }
}

/// The border box of every inline element on every line it has content on, by element.
pub(crate) fn span_rects(blocks: &[Placement]) -> HashMap<NodeId, Vec<Rect>> {
    let mut found: HashMap<NodeId, Vec<Rect>> = HashMap::new();
    for block in blocks {
        for line in &block.layout.lines {
            for span in &line.spans {
                found
                    .entry(span.node)
                    .or_default()
                    .push(block.shifted(span.rect));
            }
        }
    }
    found
}

fn node_text(snapshot: &Snapshot, node: NodeId) -> Option<&str> {
    snapshot.dom.get(node)?.data()
}

/// A DOM boundary point as a position in a text node: a text node as it is, an element's child
/// index as the start of the text at or after that child (for a range start) or the end of the
/// text before it (for an end).
pub(crate) fn text_position(
    snapshot: &Snapshot,
    (node, offset): (NodeId, usize),
    is_start: bool,
) -> Option<(NodeId, usize)> {
    let dom = &snapshot.dom;
    let found = dom.get(node)?;
    if found.is_text() {
        return Some((node, offset));
    }
    let children = dom.flat_children(node);
    let texts = |nodes: &[NodeId]| -> Vec<NodeId> {
        nodes
            .iter()
            .flat_map(|child| dom.subtree(*child))
            .filter(|n| dom.get(*n).is_some_and(|found| found.is_text()))
            .collect()
    };
    if is_start {
        let after = children.get(offset..).unwrap_or(&[]);
        let first = texts(after).into_iter().next()?;
        Some((first, 0))
    } else {
        let before = &children[..offset.min(children.len())];
        let last = texts(before).into_iter().next_back()?;
        let end = node_text(snapshot, last).map_or(0, |text| text.encode_utf16().count());
        Some((last, end))
    }
}

/// The document order of the nodes under the root, as a rank for each.
fn ranks<'a>(view: &View<'a>) -> &'a HashMap<NodeId, usize> {
    view.ranks.get_or_init(|| order_of(view.snapshot))
}

fn order_of(snapshot: &Snapshot) -> HashMap<NodeId, usize> {
    let dom = &snapshot.dom;
    let mut order = HashMap::new();
    let mut stack = vec![dom.document];
    while let Some(node) = stack.pop() {
        order.insert(node, order.len());
        let mut children = dom.flat_children(node);
        children.reverse();
        stack.extend(children);
    }
    order
}

/// The boxes of a range of text, one for each line it covers (and each piece of text on it), from
/// `start` to `end`, positions in text nodes in UTF-16 units.
pub(crate) fn range_rects(view: &View, start: (NodeId, usize), end: (NodeId, usize)) -> Vec<Rect> {
    let order = ranks(view);
    let rank = |node: NodeId| order.get(&node).copied().unwrap_or(usize::MAX);
    let from = (rank(start.0), start.1);
    let to = (rank(end.0), end.1);
    let mut rects = Vec::new();
    if from == to {
        // A collapsed range is a caret: one box without width, where the text there is.
        rects.extend(caret_rect(view, &rank, from));
        return rects;
    }
    if from > to {
        return rects;
    }
    for block in view.blocks {
        for line in &block.layout.lines {
            for fragment in &line.fragments {
                let Fragment::Text(text) = fragment else {
                    continue;
                };
                if let Some(rect) = covered(view, block, text, &rank, (from, to)) {
                    rects.push(rect);
                }
            }
        }
    }
    rects
}

/// The box of a caret at `at` (a rank and a UTF-16 offset): the first fragment that has the
/// position inside it, or else the last one that ends there.
fn caret_rect(view: &View, rank: &dyn Fn(NodeId) -> usize, at: (usize, usize)) -> Option<Rect> {
    let mut found = None;
    for block in view.blocks {
        for line in &block.layout.lines {
            for fragment in &line.fragments {
                let Fragment::Text(text) = fragment else {
                    continue;
                };
                if text.generated || rank(text.node) != at.0 {
                    continue;
                }
                let source = node_text(view.snapshot, text.node)?;
                let begin = utf8_offset_to_utf16(source, text.range.start);
                let finish = utf8_offset_to_utf16(source, text.range.end);
                if (begin..finish).contains(&at.1) {
                    return Some(caret_in(view, block, text, source, at.1));
                }
                if finish == at.1 {
                    found = Some(caret_in(view, block, text, source, at.1));
                }
            }
        }
    }
    found
}

fn caret_in(
    view: &View,
    block: &Placement,
    fragment: &TextFragment,
    source: &str,
    offset: usize,
) -> Rect {
    let x = x_at(fragment, utf16_offset_to_utf8(source, offset));
    let metrics = view.measurer.metrics(fragment.run.font, fragment.run.size);
    block.shifted(Rect::new(
        fragment.x + x,
        fragment.baseline - metrics.ascent,
        0.,
        metrics.ascent + metrics.descent,
    ))
}

/// The part of a text fragment between `from` and `to` (a rank and a UTF-16 offset each), as a
/// box the height of the font's ascent and descent.
fn covered(
    view: &View,
    block: &Placement,
    fragment: &TextFragment,
    rank: &dyn Fn(NodeId) -> usize,
    span: ((usize, usize), (usize, usize)),
) -> Option<Rect> {
    let (x0, x1) = covered_x(view, fragment, rank, span)?;
    let metrics = view.measurer.metrics(fragment.run.font, fragment.run.size);
    let rect = Rect::new(
        x0,
        fragment.baseline - metrics.ascent,
        (x1 - x0).max(0.),
        metrics.ascent + metrics.descent,
    );
    Some(block.shifted(rect))
}

/// Where the part of a text fragment between `from` and `to` starts and ends, from the left of
/// the block's content.
fn covered_x(
    view: &View,
    fragment: &TextFragment,
    rank: &dyn Fn(NodeId) -> usize,
    (from, to): ((usize, usize), (usize, usize)),
) -> Option<(f32, f32)> {
    if fragment.generated {
        return None;
    }
    let at = rank(fragment.node);
    // A node outside the span's ranks is out before its offsets are worked out.
    if at < from.0 || at > to.0 {
        return None;
    }
    let source = node_text(view.snapshot, fragment.node)?;
    let begin = (at, utf8_offset_to_utf16(source, fragment.range.start));
    let finish = (at, utf8_offset_to_utf16(source, fragment.range.end));
    if finish <= from || begin >= to {
        return None;
    }
    let low = from.max(begin);
    let high = to.min(finish);
    let x0 = x_at(fragment, utf16_offset_to_utf8(source, low.1));
    let x1 = x_at(fragment, utf16_offset_to_utf8(source, high.1));
    Some((fragment.x + x0, fragment.x + x1))
}

/// The selected part of each block's lines, by the block's box: a rectangle for each piece of
/// text, from the left of the block's content, as high as its line (the way a selection is
/// painted) rather than as high as the font.
pub(crate) fn selection_rects(
    view: &View,
    start: (NodeId, usize),
    end: (NodeId, usize),
) -> HashMap<usize, Vec<Rect>> {
    let order = ranks(view);
    let rank = |node: NodeId| order.get(&node).copied().unwrap_or(usize::MAX);
    let span = ((rank(start.0), start.1), (rank(end.0), end.1));
    let mut found: HashMap<usize, Vec<Rect>> = HashMap::new();
    if span.0 >= span.1 {
        return found;
    }
    // Text that cannot be selected is not highlighted.
    let texts = view.blocks.iter().flat_map(|block| {
        block.layout.lines.iter().flat_map(move |line| {
            line.fragments
                .iter()
                .filter_map(move |fragment| match fragment {
                    Fragment::Text(text) if !text.select_none => {
                        Some((block.holder, line.rect, text))
                    }
                    _ => None,
                })
        })
    });
    for (holder, line, text) in texts {
        let Some((x0, x1)) = covered_x(view, text, &rank, span) else {
            continue;
        };
        if x1 > x0 {
            found
                .entry(holder)
                .or_default()
                .push(Rect::new(x0, line.y, x1 - x0, line.height));
        }
    }
    found
}

/// How far from the fragment's left edge the caret stands before byte `byte` of the node's text.
pub(crate) fn x_at(fragment: &TextFragment, byte: usize) -> f32 {
    let stops: &[Stop] = &fragment.stops;
    let mut at = 0.;
    for stop in stops {
        if stop.byte as usize <= byte {
            at = stop.x;
        } else {
            break;
        }
    }
    at
}

/// The text position nearest the point `(x, y)`, in viewport coordinates: the text node and the
/// UTF-16 offset of the caret position closest to the point on the line the point is on (the
/// nearest line when it is between or beyond them).
pub(crate) fn position_at(view: &View, x: f32, y: f32) -> Option<(NodeId, usize)> {
    let block = view
        .blocks
        .iter()
        .rev()
        .find(|block| block.bounds.contains(x, y))?;
    let line = nearest_line(&block.layout, y - block.origin[1])?;
    let local = x - block.origin[0];
    let fragment = nearest_fragment(line, local)?;
    let source = node_text(view.snapshot, fragment.node)?;
    let byte = nearest_stop(fragment, local)?;
    Some((fragment.node, utf8_offset_to_utf16(source, byte as usize)))
}

fn nearest_line(layout: &InlineLayout, y: f32) -> Option<&LineBox> {
    layout
        .lines
        .iter()
        .find(|line| y < line.rect.y + line.rect.height)
        .or(layout.lines.last())
}

/// The text of the line the point is over, or the one next to it, that can be selected.
fn nearest_fragment(line: &LineBox, x: f32) -> Option<&TextFragment> {
    let texts = line.fragments.iter().filter_map(|fragment| match fragment {
        Fragment::Text(text) if !text.generated && !text.select_none => Some(text),
        _ => None,
    });
    texts.min_by(|a, b| {
        distance(a, x)
            .partial_cmp(&distance(b, x))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn distance(fragment: &TextFragment, x: f32) -> f32 {
    if x < fragment.x {
        fragment.x - x
    } else if x > fragment.x + fragment.width {
        x - fragment.x - fragment.width
    } else {
        0.
    }
}

/// The byte of the stop nearest `x` (from the block's left edge).
fn nearest_stop(fragment: &TextFragment, x: f32) -> Option<u32> {
    let local = x - fragment.x;
    fragment
        .stops
        .iter()
        .min_by(|a, b| {
            (a.x - local)
                .abs()
                .partial_cmp(&(b.x - local).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|stop| stop.byte)
}
