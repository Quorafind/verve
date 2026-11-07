//! The layout engine: a snapshot in, geometry and a display list out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use verve_dom::{Align, Changes, NodeId, Rect, ScrollRequest, Snapshot};

use crate::display_list::DisplayList;
use crate::fonts::FontRegistry;
use crate::inline::query::{self, Placement, View};
use crate::layout::{Laid, settle};
use crate::measure::TextMeasurer;
use crate::paint::{self, Text};
use crate::place::{Placed, padding_box, place};
use crate::svg::{self, Drawing};
use crate::tree::{BoxTree, build};

/// The number of the next list: lists are numbered across the process, so that a page that
/// replaces another one (a navigation makes a new engine) never repeats the number of a list a
/// window may still be showing.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// A revision no list has had in this process.
pub(crate) fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

/// Lays a document out and answers questions about the result.
///
/// [`LayoutEngine::update`] lays the whole tree out again whenever the document changed (the
/// incremental engine is P33's); a scroll only moves what is already laid out.
pub struct LayoutEngine {
    measurer: Arc<dyn TextMeasurer>,
    fonts: FontRegistry,
    /// The document's selection, as the DOM says its ends.
    selection: Option<Selection>,
    /// The boxes inside the inline SVGs of the last layout, by the text of the drawing.
    svg_cache: svg::Cache,
    state: Option<State>,
}

/// The two ends of a selection: a node and an offset each.
type Selection = ((NodeId, usize), (NodeId, usize));

struct State {
    /// The snapshot's revision, the viewport and the faces it was laid out for.
    key: (u64, [u32; 2], u64),
    /// The document the boxes were made from, for the text of its nodes.
    snapshot: Snapshot,
    /// The lines of every block of inline content, placed on the screen.
    blocks: Vec<Placement>,
    /// The border box of an inline element on each line it has content on.
    spans: HashMap<NodeId, Vec<Rect>>,
    /// The document order of the nodes, for the questions about ranges; worked out at the first.
    ranks: OnceLock<HashMap<NodeId, usize>>,
    /// The scroll offsets the snapshot had when this was last brought up to date.
    snapshot_offsets: HashMap<NodeId, [f32; 2]>,
    tree: BoxTree,
    laid: Laid,
    /// The offsets scroll containers were asked to be at.
    scroll: HashMap<NodeId, [f32; 2]>,
    placed: Placed,
    rects: HashMap<NodeId, Rect>,
    /// The inline SVGs, by box.
    drawings: HashMap<usize, Drawing>,
    list: Arc<DisplayList>,
}

impl LayoutEngine {
    pub fn new(measurer: Arc<dyn TextMeasurer>) -> Self {
        Self {
            measurer,
            fonts: FontRegistry::default(),
            selection: None,
            svg_cache: svg::Cache::new(),
            state: None,
        }
    }

    /// Notes a face the page registered with `@font-face`; every later layout asks for it by its
    /// family. Answers whether the face is new, in which case the next [`LayoutEngine::update`]
    /// lays everything out again (text laid out in a fallback font is shaped again).
    pub fn register_font(&mut self, family: &str, weight: f32, italic: bool) -> bool {
        self.fonts.register(family, weight, italic)
    }

    /// The measurer text is shaped with.
    pub fn measurer(&self) -> &Arc<dyn TextMeasurer> {
        &self.measurer
    }

    /// Brings the layout up to date with `snapshot`, `changes` being what moved since the last
    /// call. Answers whether anything is not what it was.
    pub fn update(&mut self, snapshot: &Snapshot, changes: &Changes) -> bool {
        let key = (
            snapshot.revision,
            [snapshot.viewport.0.to_bits(), snapshot.viewport.1.to_bits()],
            self.fonts.generation(),
        );
        if let Some(state) = &mut self.state
            && state.key == key
            && changes.is_empty()
        {
            if state.snapshot_offsets == snapshot.scroll_offsets {
                return false;
            }
            state.snapshot_offsets = snapshot.scroll_offsets.clone();
            state.scroll = snapshot.scroll_offsets.clone();
            return self.replace();
        }
        let mut tree = build(snapshot, &self.measurer, &self.fonts);
        let laid = settle(&mut tree);
        let scroll = snapshot.scroll_offsets.clone();
        let placed = place(&tree, &laid, &scroll);
        let blocks = placements(&tree, &laid, &placed);
        let ranks = OnceLock::new();
        let selection = self.selection_boxes(snapshot, &blocks, &ranks);
        let text = Text {
            measurer: &*self.measurer,
            snapshot,
            selection: &selection,
        };
        let drawings = svg::drawings(snapshot, &tree, &laid, &placed, &mut self.svg_cache);
        let list = self.adopt(list_for(&tree, &laid, &placed, &text, &drawings));
        let spans = query::span_rects(&blocks);
        self.state = Some(State {
            key,
            snapshot: snapshot.clone(),
            rects: rects_of(&tree, &laid, &placed, &spans, &drawings),
            drawings,
            blocks,
            spans,
            ranks,
            snapshot_offsets: snapshot.scroll_offsets.clone(),
            tree,
            laid,
            scroll,
            placed,
            list,
        });
        self.measurer.finish_layout();
        true
    }

    /// Places the laid-out boxes again, for new scroll offsets. Answers whether the list changed.
    fn replace(&mut self) -> bool {
        let Some(state) = self.state.take() else {
            return false;
        };
        let placed = place(&state.tree, &state.laid, &state.scroll);
        let blocks = placements(&state.tree, &state.laid, &placed);
        let selection = self.selection_boxes(&state.snapshot, &blocks, &state.ranks);
        let text = Text {
            measurer: &*self.measurer,
            snapshot: &state.snapshot,
            selection: &selection,
        };
        let built = list_for(&state.tree, &state.laid, &placed, &text, &state.drawings);
        let changed = built.items != state.list.items;
        let list = if changed {
            self.adopt(built)
        } else {
            state.list
        };
        let spans = query::span_rects(&blocks);
        self.state = Some(State {
            rects: rects_of(&state.tree, &state.laid, &placed, &spans, &state.drawings),
            blocks,
            spans,
            placed,
            list,
            ..state
        });
        self.measurer.finish_layout();
        changed
    }

    /// Sets the document's selection: the display list shows it from now on. Answers whether the
    /// list changed.
    pub fn set_selection(&mut self, selection: Option<Selection>) -> bool {
        if self.selection == selection {
            return false;
        }
        self.selection = selection;
        self.replace()
    }

    /// The selected parts of the lines of each block, by the block's box.
    fn selection_boxes(
        &self,
        snapshot: &Snapshot,
        blocks: &[Placement],
        ranks: &OnceLock<HashMap<NodeId, usize>>,
    ) -> HashMap<usize, Vec<Rect>> {
        let Some((start, end)) = self.selection else {
            return HashMap::new();
        };
        let (Some(from), Some(to)) = (
            query::text_position(snapshot, start, true),
            query::text_position(snapshot, end, false),
        ) else {
            return HashMap::new();
        };
        let view = View {
            snapshot,
            measurer: &*self.measurer,
            blocks,
            ranks,
        };
        query::selection_rects(&view, from, to)
    }

    /// Numbers a list that is not the one before it, which keeps its `Arc` when nothing changed.
    fn adopt(&mut self, mut list: DisplayList) -> Arc<DisplayList> {
        list.revision = next_revision();
        Arc::new(list)
    }

    /// The border box of every element that has a box, in viewport coordinates (scrolling and
    /// `transform` applied).
    pub fn rects(&self) -> impl Iterator<Item = (NodeId, Rect)> + '_ {
        self.state
            .iter()
            .flat_map(|state| state.rects.iter().map(|(node, rect)| (*node, *rect)))
    }

    /// One element's border box.
    pub fn rect(&self, node: NodeId) -> Option<Rect> {
        self.state.as_ref()?.rects.get(&node).copied()
    }

    /// The boxes of an element that `getClientRects` lists: one for a block-level element, and
    /// for an inline element one on each line it has content on.
    pub fn client_rects(&self, node: NodeId) -> Vec<Rect> {
        match self.state.as_ref().and_then(|state| state.spans.get(&node)) {
            Some(lines) => lines.clone(),
            None => self.rect(node).into_iter().collect(),
        }
    }

    /// The lines of a block that holds inline content (text, inline elements, atomic inlines),
    /// laid out: `None` for an element that does not.
    pub fn inline_layout(&self, node: NodeId) -> Option<Arc<crate::InlineLayout>> {
        let state = self.state.as_ref()?;
        let index = *state.tree.by_node.get(&node)?;
        state.tree.inline[index].clone()
    }

    /// The boxes inline elements have on each of their lines, by element: what `getClientRects`
    /// of an element split over lines answers.
    pub fn fragments(&self) -> impl Iterator<Item = (NodeId, Vec<Rect>)> + '_ {
        self.state.iter().flat_map(|state| {
            state
                .spans
                .iter()
                .map(|(node, rects)| (*node, rects.clone()))
        })
    }

    /// The text position nearest the point `(x, y)` in the window: the text node and the offset
    /// in it, in UTF-16 units. `None` where there is no text to be near.
    pub fn text_position_at(&self, x: f32, y: f32) -> Option<(NodeId, usize)> {
        let state = self.state.as_ref()?;
        query::position_at(&self.view(state), x, y)
    }

    /// The boxes of the text between two positions, one for each piece of text on each line: what
    /// a range's `getClientRects` answers. A position is a node and an offset, a UTF-16 offset in
    /// a text node and a child index in an element.
    pub fn range_rects(&self, start: (NodeId, usize), end: (NodeId, usize)) -> Vec<Rect> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let snapshot = &state.snapshot;
        let (Some(from), Some(to)) = (
            query::text_position(snapshot, start, true),
            query::text_position(snapshot, end, false),
        ) else {
            return Vec::new();
        };
        query::range_rects(&self.view(state), from, to)
    }

    fn view<'a>(&'a self, state: &'a State) -> View<'a> {
        View {
            snapshot: &state.snapshot,
            measurer: &*self.measurer,
            blocks: &state.blocks,
            ranks: &state.ranks,
        }
    }

    /// `[scrollWidth, scrollHeight]` of an element, and `None` for one with no box.
    pub fn scroll_extent(&self, node: NodeId) -> Option<[f32; 2]> {
        let state = self.state.as_ref()?;
        let index = *state.tree.by_node.get(&node)?;
        Some(if state.placed.scrolls(&state.tree, index) {
            state.placed.content[index]
        } else {
            state.laid.scroll_size(index)
        })
    }

    /// How far an element is scrolled; `[0, 0]` for one that does not scroll.
    pub fn scroll_offset(&self, node: NodeId) -> [f32; 2] {
        self.state
            .as_ref()
            .and_then(|state| Some(state.placed.offset[*state.tree.by_node.get(&node)?]))
            .unwrap_or([0.; 2])
    }

    /// The scroll offsets in effect: every scroll container and the page, by node.
    pub fn scroll_offsets(&self) -> HashMap<NodeId, [f32; 2]> {
        self.state
            .as_ref()
            .map(|state| state.placed.offsets(&state.tree))
            .unwrap_or_default()
    }

    /// Does a scroll a script asked for: the offset clamped to what the box can scroll to.
    /// Answers the boxes that moved, with their new offsets. The caller reports them to the
    /// document ([`verve_dom::Document::set_scroll_offsets`]).
    pub fn apply_scroll(&mut self, request: &ScrollRequest) -> Vec<(NodeId, [f32; 2])> {
        let Some(before) = self
            .state
            .as_ref()
            .map(|state| state.placed.offsets(&state.tree))
        else {
            return Vec::new();
        };
        match request {
            ScrollRequest::To { node, left, top } => {
                let current = self.scroll_offset(*node);
                self.set_scroll(
                    *node,
                    [left.unwrap_or(current[0]), top.unwrap_or(current[1])],
                );
            }
            ScrollRequest::By { node, left, top } => {
                let current = self.scroll_offset(*node);
                self.set_scroll(*node, [current[0] + left, current[1] + top]);
            }
            ScrollRequest::Reveal {
                node,
                block,
                inline,
                ..
            } => self.reveal(*node, *block, *inline),
        }
        let after = self.scroll_offsets();
        let mut moved: Vec<(NodeId, [f32; 2])> = after
            .into_iter()
            .filter(|(node, offset)| before.get(node) != Some(offset))
            .collect();
        moved.sort_by_key(|(node, _)| *node);
        moved
    }

    fn set_scroll(&mut self, node: NodeId, offset: [f32; 2]) {
        let Some(state) = &mut self.state else { return };
        let Some(&index) = state.tree.by_node.get(&node) else {
            return;
        };
        if !state.placed.scrolls(&state.tree, index) {
            return;
        }
        state.scroll.insert(node, offset);
        self.replace();
        // What is kept is where the box came to rest, not where it was asked to go.
        if let Some(state) = &mut self.state {
            state.scroll.insert(node, state.placed.offset[index]);
        }
    }

    /// `scrollIntoView`: scrolls the containers above `node`, innermost first, until it shows.
    fn reveal(&mut self, node: NodeId, block: Align, inline: Align) {
        let Some(state) = &self.state else { return };
        let Some(&target) = state.tree.by_node.get(&node) else {
            return;
        };
        let tree = &state.tree;
        let mut chain = Vec::new();
        let mut at = tree.boxes[target].parent;
        while let Some(index) = at {
            if tree.boxes[index].props.clips() && Some(index) != tree.html {
                chain.push(tree.boxes[index].node);
            }
            at = tree.boxes[index].parent;
        }
        chain.extend(tree.html.map(|html| tree.boxes[html].node));
        for container in chain {
            let Some(state) = &self.state else { return };
            let Some(&index) = state.tree.by_node.get(&container) else {
                continue;
            };
            let port = if Some(index) == state.tree.html {
                Rect::new(0., 0., state.tree.viewport.0, state.tree.viewport.1)
            } else {
                padding_box(&state.tree, &state.laid, state.placed.rect[index], index).0
            };
            let shown = state.placed.rect[target];
            let current = state.placed.offset[index];
            let wanted = [
                aligned(
                    inline,
                    shown.x - port.x,
                    shown.width,
                    port.width,
                    current[0],
                ),
                aligned(
                    block,
                    shown.y - port.y,
                    shown.height,
                    port.height,
                    current[1],
                ),
            ];
            self.set_scroll(container, wanted);
        }
    }

    /// The element painted topmost at `(x, y)`, in viewport coordinates; the root element for a
    /// point inside the window that nothing else covers.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        let state = self.state.as_ref()?;
        state.list.hit_test(x, y, &|_| None).or_else(|| {
            let (width, height) = state.tree.viewport;
            let inside = x >= 0. && y >= 0. && x < width && y < height;
            let html = state.tree.html?;
            inside.then_some(state.tree.boxes[html].node)
        })
    }

    /// The box of each element inside an inline SVG in the drawing's own pixels: what `getBBox`
    /// answers.
    pub fn svg_boxes(&self) -> impl Iterator<Item = (NodeId, Rect)> + '_ {
        self.state
            .iter()
            .flat_map(|state| state.drawings.values())
            .flat_map(|drawing| drawing.bboxes.iter().copied())
    }

    /// The display list to paint.
    pub fn display_list(&self) -> Arc<DisplayList> {
        self.state
            .as_ref()
            .map_or_else(Arc::default, |state| state.list.clone())
    }

    /// How many style values the page used that layout could not read (see `BoxProps`).
    pub fn unsupported(&self) -> u32 {
        self.state
            .as_ref()
            .map_or(0, |state| state.tree.unsupported)
    }
}

fn list_for(
    tree: &BoxTree,
    laid: &Laid,
    placed: &Placed,
    text: &Text,
    drawings: &HashMap<usize, Drawing>,
) -> DisplayList {
    let paint::Painted {
        items,
        rasters,
        vectors,
    } = paint::build(tree, laid, placed, text, drawings);
    let root = tree
        .html
        .filter(|html| placed.frame[*html])
        .map(|html| tree.boxes[html].node);
    DisplayList {
        revision: 0,
        items,
        rasters,
        vectors,
        root,
    }
}

fn rects_of(
    tree: &BoxTree,
    laid: &Laid,
    placed: &Placed,
    spans: &HashMap<NodeId, Vec<Rect>>,
    drawings: &HashMap<usize, Drawing>,
) -> HashMap<NodeId, Rect> {
    let mut rects: HashMap<NodeId, Rect> = spans
        .iter()
        .filter_map(|(node, lines)| {
            let first = *lines.first()?;
            Some((*node, lines.iter().fold(first, |all, line| all.union(line))))
        })
        .collect();
    rects.extend(
        tree.boxes
            .iter()
            .zip(&placed.rect)
            .filter(|(node, _)| !node.anonymous)
            .map(|(node, rect)| (node.node, *rect)),
    );
    rects.extend(svg::client_rects(laid, placed, drawings));
    rects
}

/// The lines of every block of inline content, placed in the window.
fn placements(tree: &BoxTree, laid: &Laid, placed: &Placed) -> Vec<Placement> {
    tree.holders
        .iter()
        .filter_map(|&index| {
            let layout = tree.inline[index].clone()?;
            let rect = placed.rect[index];
            let [top, _, _, left] = laid.border[index];
            let [pad_top, _, _, pad_left] = laid.padding[index];
            Some(Placement {
                holder: index,
                origin: [rect.x + left + pad_left, rect.y + top + pad_top],
                layout,
                bounds: rect,
            })
        })
        .collect()
}

/// The scroll offset that lines a box of `length` at `start` (from the start of the scrollport,
/// which is `port` long) up as `align` asks, given the offset `current`.
fn aligned(align: Align, start: f32, length: f32, port: f32, current: f32) -> f32 {
    match align {
        Align::Start => current + start,
        Align::Center => current + start + (length - port) / 2.,
        Align::End => current + start + length - port,
        Align::Nearest if start < 0. => current + start,
        Align::Nearest if start + length > port && length <= port => {
            current + start + length - port
        }
        Align::Nearest if start + length > port && start > 0. => current + start,
        Align::Nearest => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_scrolls_the_least_that_shows_the_box() {
        // A 20 long box at 150 in a port of 100, scrolled to 0: nearest brings its end in.
        assert_eq!(aligned(Align::Nearest, 150., 20., 100., 0.), 70.);
        // Already in sight: nothing.
        assert_eq!(aligned(Align::Nearest, 10., 20., 100., 30.), 30.);
        // Above the port: its start comes down to the edge.
        assert_eq!(aligned(Align::Nearest, -15., 20., 100., 30.), 15.);
        assert_eq!(aligned(Align::Start, 150., 20., 100., 0.), 150.);
        assert_eq!(aligned(Align::Center, 150., 20., 100., 0.), 110.);
        assert_eq!(aligned(Align::End, 150., 20., 100., 0.), 70.);
    }
}
