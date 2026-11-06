//! The layout engine: a snapshot in, geometry and a display list out.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use verve_dom::{Align, Changes, NodeId, Rect, ScrollRequest, Snapshot};

use crate::display_list::DisplayList;
use crate::layout::{Laid, settle};
use crate::measure::TextMeasurer;
use crate::paint;
use crate::place::{Placed, padding_box, place};
use crate::tree::{BoxTree, build};

/// The number of the next list: lists are numbered across the process, so that a page that
/// replaces another one (a navigation makes a new engine) never repeats the number of a list a
/// window may still be showing.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// Lays a document out and answers questions about the result.
///
/// [`LayoutEngine::update`] lays the whole tree out again whenever the document changed (the
/// incremental engine is P33's); a scroll only moves what is already laid out.
pub struct LayoutEngine {
    measurer: Arc<dyn TextMeasurer>,
    state: Option<State>,
}

struct State {
    /// The snapshot's revision and the viewport it was laid out for.
    key: (u64, [u32; 2]),
    /// The scroll offsets the snapshot had when this was last brought up to date.
    snapshot_offsets: HashMap<NodeId, [f32; 2]>,
    tree: BoxTree,
    laid: Laid,
    /// The offsets scroll containers were asked to be at.
    scroll: HashMap<NodeId, [f32; 2]>,
    placed: Placed,
    rects: HashMap<NodeId, Rect>,
    list: Arc<DisplayList>,
}

impl LayoutEngine {
    pub fn new(measurer: Arc<dyn TextMeasurer>) -> Self {
        Self {
            measurer,
            state: None,
        }
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
        let mut tree = build(snapshot);
        let laid = settle(&mut tree);
        let scroll = snapshot.scroll_offsets.clone();
        let placed = place(&tree, &laid, &scroll);
        let list = self.adopt(list_for(&tree, &laid, &placed));
        self.state = Some(State {
            key,
            snapshot_offsets: snapshot.scroll_offsets.clone(),
            rects: rects_of(&tree, &placed),
            tree,
            laid,
            scroll,
            placed,
            list,
        });
        true
    }

    /// Places the laid-out boxes again, for new scroll offsets. Answers whether the list changed.
    fn replace(&mut self) -> bool {
        let Some(state) = self.state.take() else {
            return false;
        };
        let placed = place(&state.tree, &state.laid, &state.scroll);
        let built = list_for(&state.tree, &state.laid, &placed);
        let changed = built.items != state.list.items;
        let list = if changed {
            self.adopt(built)
        } else {
            state.list
        };
        self.state = Some(State {
            rects: rects_of(&state.tree, &placed),
            placed,
            list,
            ..state
        });
        changed
    }

    /// Numbers a list that is not the one before it, which keeps its `Arc` when nothing changed.
    fn adopt(&mut self, mut list: DisplayList) -> Arc<DisplayList> {
        list.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
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

    /// The boxes of an element that `getClientRects` lists: one for a block-level element. An
    /// inline element split over lines has one a line (P31).
    pub fn client_rects(&self, node: NodeId) -> Vec<Rect> {
        self.rect(node).into_iter().collect()
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

fn list_for(tree: &BoxTree, laid: &Laid, placed: &Placed) -> DisplayList {
    let (items, rasters) = paint::build(tree, laid, placed);
    DisplayList {
        revision: 0,
        items,
        rasters,
    }
}

fn rects_of(tree: &BoxTree, placed: &Placed) -> HashMap<NodeId, Rect> {
    tree.boxes
        .iter()
        .zip(&placed.rect)
        .map(|(node, rect)| (node.node, *rect))
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
