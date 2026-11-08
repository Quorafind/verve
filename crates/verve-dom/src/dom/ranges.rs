//! Live ranges: the boundary points a script's `Range` objects and the document's selection hold,
//! kept up to date as the tree changes.
//!
//! The DOM standard says what each kind of change does to a boundary ("insert", "remove",
//! "replace data", "split a Text node", "normalize"); the `Dom` calls the matching hook from its
//! own mutators, so no change can skip it. A boundary is a [`Boundary`], `(node, offset)`: the
//! offset counts UTF-16 units in a text or comment node and children in any other.
//!
//! Ranges live in slots. A script's `Range` object holds one reference to its slot and gives it
//! back when the garbage collector drops it, through a queue the arena drains on its next change.
//! The selection holds one slot of its own.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::{Dom, NodeId, NodeKind};
use crate::field::utf16_offset_to_utf8;
use crate::layout_provider::Boundary;

/// The two ends of a range, `start` not after `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeData {
    pub start: Boundary,
    pub end: Boundary,
}

/// Which end of the selection moves when it is extended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    /// The anchor is the range's start.
    #[default]
    Forward,
    /// The anchor is the range's end.
    Backward,
}

/// Why a range operation was refused: the name of the `DOMException` it throws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    InvalidNodeType,
    IndexSize,
    WrongDocument,
    HierarchyRequest,
    InvalidState,
}

impl RangeError {
    /// The exception's `name`.
    pub fn name(self) -> &'static str {
        match self {
            Self::InvalidNodeType => "InvalidNodeTypeError",
            Self::IndexSize => "IndexSizeError",
            Self::WrongDocument => "WrongDocumentError",
            Self::HierarchyRequest => "HierarchyRequestError",
            Self::InvalidState => "InvalidStateError",
        }
    }
}

/// What a range does to its contents ([`Dom::copy_contents`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Contents {
    Clone,
    Extract,
    Delete,
}

#[derive(Clone, Debug)]
struct Slot {
    data: RangeData,
    refs: u32,
}

/// Every live range of a document, and its selection.
#[derive(Debug, Default)]
pub struct Ranges {
    slots: Vec<Option<Slot>>,
    free: Vec<u32>,
    live: usize,
    selection: Option<u32>,
    direction: Direction,
    /// Slots whose object was collected, to give back.
    released: Arc<Mutex<Vec<u32>>>,
    /// Moves when the selection does, so the document can tell the page (`selectionchange`).
    pub(crate) revision: u64,
    /// While true, a text change does not move boundaries: the caller moves them itself.
    pub(crate) quiet: bool,
}

/// A copy keeps the selection only, in a slot of its own with no queue shared with the live
/// document: a snapshot is read for the selection and never for the
/// script's other ranges, which would otherwise be copied with every snapshot.
impl Clone for Ranges {
    fn clone(&self) -> Self {
        let selected = self.selection.and_then(|slot| self.get(slot));
        Self {
            slots: selected
                .map(|data| Slot { data, refs: 1 })
                .into_iter()
                .map(Some)
                .collect(),
            free: Vec::new(),
            live: usize::from(selected.is_some()),
            selection: selected.map(|_| 0),
            direction: self.direction,
            released: Arc::default(),
            revision: self.revision,
            quiet: false,
        }
    }
}

impl Ranges {
    /// Where objects give their slot back.
    pub(crate) fn release_queue(&self) -> Arc<Mutex<Vec<u32>>> {
        self.released.clone()
    }

    fn drain(&mut self) {
        let given: Vec<u32> = match self.released.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue),
            Err(_) => return,
        };
        for slot in given {
            self.release(slot);
        }
    }

    /// A new slot holding `data`, with one reference.
    pub(crate) fn alloc(&mut self, data: RangeData) -> u32 {
        self.drain();
        self.live += 1;
        let slot = Some(Slot { data, refs: 1 });
        match self.free.pop() {
            Some(at) => {
                self.slots[at as usize] = slot;
                at
            }
            None => {
                self.slots.push(slot);
                (self.slots.len() - 1) as u32
            }
        }
    }

    pub(crate) fn retain(&mut self, slot: u32) {
        if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
            s.refs += 1;
        }
    }

    pub(crate) fn release(&mut self, slot: u32) {
        let Some(entry) = self.slots.get_mut(slot as usize) else {
            return;
        };
        let Some(s) = entry else { return };
        s.refs -= 1;
        if s.refs == 0 {
            *entry = None;
            self.free.push(slot);
            self.live -= 1;
        }
    }

    pub(crate) fn get(&self, slot: u32) -> Option<RangeData> {
        self.slots.get(slot as usize)?.as_ref().map(|s| s.data)
    }

    pub(crate) fn set(&mut self, slot: u32, data: RangeData) {
        if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
            s.data = data;
        }
        if self.selection == Some(slot) {
            self.revision += 1;
        }
    }

    /// How many ranges are held, after giving back those whose object was collected.
    pub(crate) fn count(&mut self) -> usize {
        self.drain();
        self.live
    }

    /// Whether no range is held, the selection included.
    pub(crate) fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The slot the selection's range is in.
    pub(crate) fn selection_slot(&self) -> Option<u32> {
        self.selection
    }

    pub(crate) fn direction(&self) -> Direction {
        self.direction
    }

    /// Makes `slot` (or nothing) the selection's range, taking a reference to it.
    pub(crate) fn select(&mut self, slot: Option<u32>, direction: Direction) {
        if let Some(slot) = slot {
            self.retain(slot);
        }
        if let Some(old) = std::mem::replace(&mut self.selection, slot) {
            self.release(old);
        }
        self.direction = direction;
        self.revision += 1;
    }

    /// Moves every boundary that `f` moves; the selection's revision moves with its range.
    fn each_boundary(&mut self, f: impl FnMut(&mut Boundary)) {
        if Self::walk(&mut self.slots, self.selection, f) {
            self.revision += 1;
        }
    }

    /// Applies `f` to both ends of every range in `slots`; whether the selection's range moved.
    fn walk(
        slots: &mut [Option<Slot>],
        selection: Option<u32>,
        mut f: impl FnMut(&mut Boundary),
    ) -> bool {
        let mut moved = false;
        for (at, slot) in slots.iter_mut().enumerate() {
            let Some(slot) = slot else { continue };
            let before = slot.data;
            f(&mut slot.data.start);
            f(&mut slot.data.end);
            moved |= before != slot.data && selection == Some(at as u32);
        }
        moved
    }

    /// The node of every boundary of every range.
    pub(crate) fn boundary_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.slots
            .iter()
            .flatten()
            .flat_map(|s| [s.data.start.0, s.data.end.0])
    }
}

/// UTF-16 units of `text`.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The part of `text` between two UTF-16 offsets.
pub(crate) fn utf16_slice(text: &str, from: usize, to: usize) -> &str {
    let from = utf16_offset_to_utf8(text, from);
    let to = utf16_offset_to_utf8(text, to).max(from);
    &text[from..to]
}

// ─── Hooks: what each change does to the boundaries ─────────────────────────────────────────

impl Dom {
    /// Nodes were put in `parent` at `index`: boundaries after that place move along.
    pub(super) fn ranges_inserted(&mut self, parent: NodeId, index: usize, count: usize) {
        self.ranges.drain();
        self.ranges.each_boundary(|b| {
            if b.0 == parent && b.1 > index {
                b.1 += count;
            }
        });
    }

    /// Moves boundaries by `f`, which reads the tree: the slots are held apart meanwhile.
    fn shift_boundaries(&mut self, mut f: impl FnMut(&Self, &mut Boundary)) {
        self.ranges.drain();
        if self.ranges.is_empty() {
            return;
        }
        let mut slots = std::mem::take(&mut self.ranges.slots);
        let moved = Ranges::walk(&mut slots, self.ranges.selection, |b| f(self, b));
        self.ranges.slots = slots;
        if moved {
            self.ranges.revision += 1;
        }
    }

    /// `node`, the child of `parent` at `index`, is about to leave: boundaries inside it move to
    /// where it was, those after it move up one.
    pub(super) fn ranges_removing(&mut self, node: NodeId, parent: NodeId, index: usize) {
        self.shift_boundaries(|dom, b| {
            if dom.contains(node, b.0) {
                *b = (parent, index);
            } else if b.0 == parent && b.1 > index {
                b.1 -= 1;
            }
        });
    }

    /// Every child of `node` is about to leave.
    pub(super) fn ranges_emptying(&mut self, node: NodeId) {
        self.shift_boundaries(|dom, b| {
            if b.0 == node && b.1 > 0 || b.0 != node && dom.contains(node, b.0) {
                *b = (node, 0);
            }
        });
    }

    /// Whether removing `removed` (whose parents are `parents`) in one go leaves every boundary
    /// where it is: none is in a removed subtree, and none is in a parent that loses children.
    pub(super) fn ranges_unaffected(
        &self,
        removed: &HashSet<NodeId>,
        parents: &HashSet<NodeId>,
    ) -> bool {
        self.ranges.boundary_nodes().all(|node| {
            let mut at = Some(node);
            while let Some(n) = at {
                if removed.contains(&n) {
                    return false;
                }
                at = self.parent(n);
            }
            !parents.contains(&node)
        })
    }

    /// The text of `node` changed: `count` units from `offset` became `added` units.
    pub(super) fn ranges_data_replaced(
        &mut self,
        node: NodeId,
        offset: usize,
        count: usize,
        added: usize,
    ) {
        self.ranges.drain();
        self.ranges.each_boundary(|b| {
            if b.0 != node || b.1 <= offset {
                return;
            }
            b.1 = if b.1 <= offset + count {
                offset
            } else {
                b.1 + added - count
            };
        });
    }
}

// ─── Text and tree positions ────────────────────────────────────────────────────────────────

impl Dom {
    /// How many places a boundary may be at in `id`: units of text, children otherwise.
    pub fn node_length(&self, id: NodeId) -> usize {
        match self.get(id).map(|n| &n.kind) {
            Some(NodeKind::Text(t) | NodeKind::Comment(t)) => utf16_len(t),
            Some(NodeKind::Doctype(_)) | None => 0,
            Some(_) => self.children(id).len(),
        }
    }

    /// Whether `id` holds characters (a text or comment node).
    pub fn is_character_data(&self, id: NodeId) -> bool {
        matches!(
            self.get(id).map(|n| &n.kind),
            Some(NodeKind::Text(_) | NodeKind::Comment(_))
        )
    }

    /// The characters of a text or comment node between two UTF-16 offsets.
    pub fn data_between(&self, id: NodeId, from: usize, to: usize) -> String {
        match self.get(id).map(|n| &n.kind) {
            Some(NodeKind::Text(t) | NodeKind::Comment(t)) => utf16_slice(t, from, to).to_owned(),
            _ => String::new(),
        }
    }

    /// Which of two boundary points of one tree comes first.
    pub fn compare_boundaries(&self, a: Boundary, b: Boundary) -> Ordering {
        if a.0 == b.0 {
            return a.1.cmp(&b.1);
        }
        if self.order(a.0, b.0) == Ordering::Greater {
            return self.compare_boundaries(b, a).reverse();
        }
        if self.contains(a.0, b.0) {
            let mut child = b.0;
            while self.parent(child) != Some(a.0) {
                match self.parent(child) {
                    Some(p) => child = p,
                    None => break,
                }
            }
            if self.index_of(a.0, child).is_some_and(|i| i < a.1) {
                return Ordering::Greater;
            }
        }
        Ordering::Less
    }

    /// Whether a boundary may be at `point`: a node that holds a position, and an offset in it.
    pub fn check_boundary(&self, point: Boundary) -> Result<(), RangeError> {
        match self.get(point.0).map(|n| &n.kind) {
            None | Some(NodeKind::Doctype(_)) => Err(RangeError::InvalidNodeType),
            _ if point.1 > self.node_length(point.0) => Err(RangeError::IndexSize),
            _ => Ok(()),
        }
    }

    /// The boundary just before `node` in its parent.
    pub fn before_node(&self, node: NodeId) -> Option<Boundary> {
        let parent = self.parent(node)?;
        Some((parent, self.index_of(parent, node)?))
    }

    /// The boundary just after `node` in its parent.
    pub fn after_node(&self, node: NodeId) -> Option<Boundary> {
        self.before_node(node).map(|(p, i)| (p, i + 1))
    }

    /// The deepest node that has both of them inside it (or is one).
    pub fn common_ancestor(&self, a: NodeId, b: NodeId) -> NodeId {
        let mut at = a;
        while !self.contains(at, b) {
            match self.parent(at) {
                Some(p) => at = p,
                None => break,
            }
        }
        at
    }
}

// ─── Range operations ───────────────────────────────────────────────────────────────────────

impl Dom {
    /// The slot's range.
    pub fn range(&self, slot: u32) -> Option<RangeData> {
        self.ranges.get(slot)
    }

    /// A new range collapsed at the start of the document.
    pub fn new_range(&mut self) -> u32 {
        let at = (self.document, 0);
        self.ranges.alloc(RangeData { start: at, end: at })
    }

    /// A new range of `data`.
    pub fn new_range_of(&mut self, data: RangeData) -> u32 {
        self.ranges.alloc(data)
    }

    /// Moves one end of a range to `point`; the other end follows when the range would end up
    /// backwards or in another tree.
    pub fn set_range_end(
        &mut self,
        slot: u32,
        point: Boundary,
        start: bool,
    ) -> Result<(), RangeError> {
        self.check_boundary(point)?;
        let Some(mut data) = self.ranges.get(slot) else {
            return Ok(());
        };
        let other = if start { data.end } else { data.start };
        let elsewhere = self.root_of(point.0) != self.root_of(other.0);
        let backwards = if start {
            self.compare_boundaries(point, other) == Ordering::Greater
        } else {
            self.compare_boundaries(point, other) == Ordering::Less
        };
        if start {
            data.start = point;
            if elsewhere || backwards {
                data.end = point;
            }
        } else {
            data.end = point;
            if elsewhere || backwards {
                data.start = point;
            }
        }
        self.ranges.set(slot, data);
        Ok(())
    }

    /// Both ends at `point`.
    pub fn collapse_range(&mut self, slot: u32, point: Boundary) {
        self.ranges.set(
            slot,
            RangeData {
                start: point,
                end: point,
            },
        );
    }

    /// The range around `node` itself (`selectNode`) or its contents (`selectNodeContents`).
    pub fn select_range_of(
        &mut self,
        slot: u32,
        node: NodeId,
        contents: bool,
    ) -> Result<(), RangeError> {
        let data = if contents {
            if matches!(self.get(node).map(|n| &n.kind), Some(NodeKind::Doctype(_))) {
                return Err(RangeError::InvalidNodeType);
            }
            RangeData {
                start: (node, 0),
                end: (node, self.node_length(node)),
            }
        } else {
            let (Some(start), Some(end)) = (self.before_node(node), self.after_node(node)) else {
                return Err(RangeError::InvalidNodeType);
            };
            RangeData { start, end }
        };
        self.ranges.set(slot, data);
        Ok(())
    }

    /// `compareBoundaryPoints`: how one end of `this` lies against one end of `other`.
    pub fn compare_range_points(
        &self,
        this: RangeData,
        how: u16,
        other: RangeData,
    ) -> Result<i32, RangeError> {
        if self.root_of(this.start.0) != self.root_of(other.start.0) {
            return Err(RangeError::WrongDocument);
        }
        let (a, b) = match how {
            0 => (this.start, other.start),
            1 => (this.end, other.start),
            2 => (this.end, other.end),
            _ => (this.start, other.end),
        };
        Ok(self.compare_boundaries(a, b) as i32)
    }

    /// `comparePoint`: -1 before the range, 0 in it, 1 after it.
    pub fn compare_point(&self, range: RangeData, point: Boundary) -> Result<i32, RangeError> {
        if self.root_of(point.0) != self.root_of(range.start.0) {
            return Err(RangeError::WrongDocument);
        }
        self.check_boundary(point)?;
        Ok(
            if self.compare_boundaries(point, range.start) == Ordering::Less {
                -1
            } else if self.compare_boundaries(point, range.end) == Ordering::Greater {
                1
            } else {
                0
            },
        )
    }

    /// Whether any part of `node` is in the range (`intersectsNode`).
    pub fn range_intersects(&self, range: RangeData, node: NodeId) -> bool {
        if self.root_of(node) != self.root_of(range.start.0) {
            return false;
        }
        let Some((parent, index)) = self.before_node(node) else {
            return true;
        };
        self.compare_boundaries((parent, index), range.end) == Ordering::Less
            && self.compare_boundaries((parent, index + 1), range.start) == Ordering::Greater
    }

    /// Whether `node` is entirely inside the range.
    pub fn range_contains(&self, range: RangeData, node: NodeId) -> bool {
        self.compare_boundaries((node, 0), range.start) == Ordering::Greater
            && self.compare_boundaries((node, self.node_length(node)), range.end) == Ordering::Less
    }

    /// The text of the range: the characters of its text nodes.
    pub fn range_text(&self, range: RangeData) -> String {
        let ((sn, so), (en, eo)) = (range.start, range.end);
        let text_of = |n| matches!(self.get(n).map(|k| &k.kind), Some(NodeKind::Text(_)));
        if sn == en && text_of(sn) {
            return self.data_between(sn, so, eo);
        }
        let mut out = String::new();
        if text_of(sn) {
            out.push_str(&self.data_between(sn, so, self.node_length(sn)));
        }
        let common = self.common_ancestor(sn, en);
        for node in self.subtree(common) {
            if node != sn && node != en && text_of(node) && self.range_contains(range, node) {
                out.push_str(&self.data_between(node, 0, self.node_length(node)));
            }
        }
        if en != sn && text_of(en) {
            out.push_str(&self.data_between(en, 0, eo));
        }
        out
    }

    /// Replaces `count` units of a text node's data from `offset` with `data`; boundaries in it
    /// follow. Clamps `offset` and `count` onto the data.
    pub fn replace_data(&mut self, id: NodeId, offset: usize, count: usize, data: &str) {
        let Some(NodeKind::Text(own) | NodeKind::Comment(own)) = self.get(id).map(|n| &n.kind)
        else {
            return;
        };
        let total = utf16_len(own);
        let offset = offset.min(total);
        let count = count.min(total - offset);
        let from = utf16_offset_to_utf8(own, offset);
        let to = utf16_offset_to_utf8(own, offset + count);
        let joined = format!("{}{data}{}", &own[..from], &own[to..]);
        self.ranges.quiet = true;
        self.set_text(id, &joined);
        self.ranges.quiet = false;
        self.ranges_data_replaced(id, offset, count, utf16_len(data));
    }

    /// `splitText`: the data after `offset` goes to a new node after this one; boundaries follow.
    pub fn split_text(&mut self, id: NodeId, offset: usize) -> Option<NodeId> {
        let Some(NodeKind::Text(own)) = self.get(id).map(|n| &n.kind) else {
            return None;
        };
        let total = utf16_len(own);
        let offset = offset.min(total);
        let tail = utf16_slice(own, offset, total).to_owned();
        let fresh = self.create_text(&tail);
        if let Some(parent) = self.parent(id) {
            let index = self.index_of(parent, id)?;
            let next = self.sibling(id, 1, false);
            self.insert_before(parent, fresh, next);
            self.ranges.each_boundary(|b| {
                if *b == (parent, index + 1) {
                    b.1 += 1;
                } else if b.0 == id && b.1 > offset {
                    *b = (fresh, b.1 - offset);
                }
            });
        }
        self.replace_data(id, offset, total - offset, "");
        Some(fresh)
    }

    /// The boundaries in the text node `next`, and those just before it in its parent, move to
    /// `node`, which `next` is being joined onto, `here` units into it.
    fn move_boundaries_into(&mut self, node: NodeId, next: NodeId, here: usize) {
        let Some(parent) = self.parent(next) else {
            return;
        };
        let at = self.index_of(parent, next).unwrap_or(0);
        self.ranges.each_boundary(|b| {
            if b.0 == next {
                *b = (node, b.1 + here);
            } else if *b == (parent, at) {
                *b = (node, here);
            }
        });
    }

    /// `normalize`: the text nodes under `root` joined where they touch, the empty ones gone.
    pub fn normalize(&mut self, root: NodeId) {
        let texts: Vec<NodeId> = self
            .subtree(root)
            .into_iter()
            .filter(|n| {
                *n != root && matches!(self.get(*n).map(|k| &k.kind), Some(NodeKind::Text(_)))
            })
            .collect();
        for node in texts {
            if self.parent(node).is_none() {
                continue;
            }
            if !matches!(self.get(node).map(|n| &n.kind), Some(NodeKind::Text(_))) {
                continue;
            }
            let mut length = self.node_length(node);
            if length == 0 {
                self.detach(node);
                continue;
            }
            let mut joined = String::new();
            while let Some(next) = self.sibling(node, 1, false) {
                if !matches!(self.get(next).map(|n| &n.kind), Some(NodeKind::Text(_))) {
                    break;
                }
                let units = self.node_length(next);
                // An empty node just goes (Electron 39.8.10 moves no boundary for it).
                if units > 0 {
                    self.move_boundaries_into(node, next, length);
                    joined.push_str(&self.data_between(next, 0, units));
                    length += units;
                }
                self.detach(next);
            }
            if !joined.is_empty() {
                let own = self.data_between(node, 0, length - utf16_len(&joined));
                self.ranges.quiet = true;
                self.set_text(node, &format!("{own}{joined}"));
                self.ranges.quiet = false;
            }
        }
    }
}

// ─── Contents ───────────────────────────────────────────────────────────────────────────────

impl Dom {
    /// Whether the range's contents may be copied, extracted or deleted: a document type among
    /// the nodes the range holds whole is a `HierarchyRequestError`.
    pub(crate) fn check_contents(&self, data: RangeData) -> Result<(), RangeError> {
        let ((sn, _), (en, _)) = (data.start, data.end);
        if sn == en && self.is_character_data(sn) {
            return Ok(());
        }
        let common = self.common_ancestor(sn, en);
        let doctype =
            |c: &NodeId| matches!(self.get(*c).map(|n| &n.kind), Some(NodeKind::Doctype(_)));
        if self
            .children(common)
            .iter()
            .any(|c| doctype(c) && self.range_contains(data, *c))
        {
            return Err(RangeError::HierarchyRequest);
        }
        Ok(())
    }

    /// What `cloneContents`, `extractContents` and `deleteContents` do to the range `data`: the
    /// fragment of what was in it, and where the range is afterwards (for `Extract` and `Delete`).
    pub(crate) fn copy_contents(&mut self, data: RangeData, mode: Contents) -> (NodeId, RangeData) {
        let fragment = self.create_fragment();
        let ((sn, so), (en, eo)) = (data.start, data.end);
        if data.start == data.end {
            return (fragment, data);
        }
        if sn == en && self.is_character_data(sn) {
            let copy = self.clone_node(sn, false);
            if let Some(copy) = copy {
                let piece = self.data_between(sn, so, eo);
                self.set_text(copy, &piece);
                self.append(fragment, copy);
            }
            if mode != Contents::Clone {
                self.replace_data(sn, so, eo - so, "");
            }
            return (fragment, data);
        }
        let common = self.common_ancestor(sn, en);
        let kids = self.children(common).to_vec();
        let partial = |this: &Self, end: NodeId, other: NodeId| {
            (!this.contains(end, other))
                .then(|| kids.iter().copied().find(|c| this.contains(*c, end)))
                .flatten()
        };
        let first = partial(self, sn, en);
        let last = partial(self, en, sn);
        let contained: Vec<NodeId> = kids
            .iter()
            .copied()
            .filter(|c| self.range_contains(data, *c))
            .collect();
        let after = if self.contains(sn, en) {
            (sn, so)
        } else {
            let mut reference = sn;
            while let Some(p) = self.parent(reference) {
                if self.contains(p, en) {
                    break;
                }
                reference = p;
            }
            self.after_node(reference).unwrap_or((sn, so))
        };
        if let Some(first) = first {
            self.copy_edge(fragment, first, (sn, so), false, mode);
        }
        for child in contained {
            if mode == Contents::Clone {
                if let Some(copy) = self.clone_node(child, true) {
                    self.append(fragment, copy);
                }
            } else if mode == Contents::Delete
                && matches!(self.get(child).map(|n| &n.kind), Some(NodeKind::Doctype(_)))
            {
                // A document type can't go in a fragment, and deleting has no check that refuses
                // it (as extracting does): it is just removed, as Electron 39.8.10 does.
                self.detach(child);
            } else {
                self.append(fragment, child);
            }
        }
        if let Some(last) = last {
            self.copy_edge(fragment, last, (en, eo), true, mode);
        }
        let collapsed = RangeData {
            start: after,
            end: after,
        };
        (
            fragment,
            if mode == Contents::Clone {
                data
            } else {
                collapsed
            },
        )
    }

    /// The partly contained child of a range's common ancestor at one end: its copy goes in
    /// `fragment`, holding the part that is in the range. `edge` is the range's own end in it,
    /// and `last` says it is the end, not the start.
    fn copy_edge(
        &mut self,
        fragment: NodeId,
        child: NodeId,
        edge: Boundary,
        last: bool,
        mode: Contents,
    ) {
        let (node, offset) = edge;
        let total = self.node_length(node);
        let (from, to) = if last { (0, offset) } else { (offset, total) };
        if self.is_character_data(node) && node == child {
            if let Some(copy) = self.clone_node(node, false) {
                let piece = self.data_between(node, from, to);
                self.set_text(copy, &piece);
                self.append(fragment, copy);
            }
            if mode != Contents::Clone {
                self.replace_data(node, from, to - from, "");
            }
            return;
        }
        let Some(copy) = self.clone_node(child, false) else {
            return;
        };
        self.append(fragment, copy);
        let inner = if last {
            RangeData {
                start: (child, 0),
                end: edge,
            }
        } else {
            RangeData {
                start: edge,
                end: (child, self.node_length(child)),
            }
        };
        let (sub, _) = self.copy_contents(inner, mode);
        self.append(copy, sub);
    }

    /// `insertNode`: `node` goes in at the range's start, splitting a text node there.
    pub fn insert_at(&mut self, slot: u32, node: NodeId) -> Result<(), RangeError> {
        let Some(data) = self.ranges.get(slot) else {
            return Ok(());
        };
        let (container, offset) = data.start;
        let text = matches!(
            self.get(container).map(|n| &n.kind),
            Some(NodeKind::Text(_))
        );
        if matches!(
            self.get(container).map(|n| &n.kind),
            Some(NodeKind::Comment(_))
        ) || text && self.parent(container).is_none()
            || container == node
        {
            return Err(RangeError::HierarchyRequest);
        }
        let mut reference = if text {
            Some(container)
        } else {
            self.children(container).get(offset).copied()
        };
        let parent = reference.map_or(container, |r| self.parent(r).unwrap_or(container));
        if !self.may_insert(parent, node) {
            return Err(RangeError::HierarchyRequest);
        }
        if text {
            reference = self.split_text(container, offset);
        }
        if reference == Some(node) {
            reference = self.sibling(node, 1, false);
        }
        let incoming = if matches!(self.get(node).map(|n| &n.kind), Some(NodeKind::Fragment)) {
            self.children(node).len()
        } else {
            1
        };
        self.detach(node);
        let end = reference.and_then(|r| self.index_of(parent, r));
        let at = end.unwrap_or_else(|| self.children(parent).len());
        self.insert_before(parent, node, reference);
        if let Some(now) = self.ranges.get(slot)
            && now.start == now.end
        {
            let point = (parent, at + incoming);
            self.ranges.set(
                slot,
                RangeData {
                    start: now.start,
                    end: point,
                },
            );
        }
        Ok(())
    }

    /// `surroundContents`: the range's contents moved into `wrapper`, which takes their place.
    pub fn surround(&mut self, slot: u32, wrapper: NodeId) -> Result<(), RangeError> {
        let Some(data) = self.ranges.get(slot) else {
            return Ok(());
        };
        let ((sn, _), (en, _)) = (data.start, data.end);
        let partly = |this: &Self, a: NodeId, b: NodeId| {
            let mut at = Some(a);
            while let Some(n) = at {
                if !this.contains(n, b)
                    && !matches!(this.get(n).map(|k| &k.kind), Some(NodeKind::Text(_)))
                {
                    return true;
                }
                at = this.parent(n);
            }
            false
        };
        if partly(self, sn, en) || partly(self, en, sn) {
            return Err(RangeError::InvalidState);
        }
        if matches!(
            self.get(wrapper).map(|n| &n.kind),
            Some(NodeKind::Document | NodeKind::Doctype(_) | NodeKind::Fragment)
        ) {
            return Err(RangeError::InvalidNodeType);
        }
        self.check_contents(data)?;
        let (fragment, _) = self.copy_contents(data, Contents::Extract);
        self.empty(wrapper);
        self.insert_at(slot, wrapper)?;
        self.append(wrapper, fragment);
        self.select_range_of(slot, wrapper, false)
    }
}
