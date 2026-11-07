//! The window's selection and moving it: what `Selection` reads and writes, and the steps
//! (`Selection.modify`, the caret keys of an editable region) that take a boundary one
//! character, word, line end or paragraph further.
//!
//! The selection is one live range in the arena ([`crate::dom::Ranges`]) and which end the person
//! moves. The text positions steps land on are the ones a caret can stand at: in a text node, at
//! the edges of a grapheme cluster (from `unicode-segmentation`, through [`Draft`]); in an empty
//! block or before a `<br>` that is a line of its own, at the element.

use std::cmp::Ordering;

use crate::dom::{Direction, Dom, NodeId, NodeKind, RangeData};
use crate::field::{Draft, utf8_offset_to_utf16, utf16_offset_to_utf8};
use crate::layout_provider::Boundary;

/// How far a step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Granularity {
    Character,
    Word,
    /// To the end of the line (`lineboundary`); without a layout, of the block.
    LineBoundary,
    /// To the start or end of the block (`paragraph`, `paragraphboundary`).
    Paragraph,
    /// To the start or end of the editing host or document.
    Document,
}

impl Dom {
    /// The selection's range and which end the person moves; `None` when nothing is selected.
    pub fn selection(&self) -> Option<(RangeData, Direction)> {
        let slot = self.ranges.selection_slot()?;
        Some((self.ranges.get(slot)?, self.ranges.direction()))
    }

    /// Where the selection began.
    pub fn selection_anchor(&self) -> Option<Boundary> {
        self.selection().map(|(r, d)| match d {
            Direction::Forward => r.start,
            Direction::Backward => r.end,
        })
    }

    /// Where the selection ends: the caret, when it is collapsed.
    pub fn selection_focus(&self) -> Option<Boundary> {
        self.selection().map(|(r, d)| match d {
            Direction::Forward => r.end,
            Direction::Backward => r.start,
        })
    }

    /// The selection becomes `data`, a range of its own.
    pub fn set_selection(&mut self, data: RangeData, direction: Direction) {
        let slot = self.new_range_of(data);
        self.ranges.select(Some(slot), direction);
        // The selection holds the only reference now.
        self.ranges.release(slot);
    }

    /// The selection from `anchor` to `focus`, the range in tree order.
    pub fn select_between(&mut self, anchor: Boundary, focus: Boundary) {
        if self.compare_boundaries(anchor, focus) == Ordering::Greater {
            let data = RangeData {
                start: focus,
                end: anchor,
            };
            self.set_selection(data, Direction::Backward);
        } else {
            let data = RangeData {
                start: anchor,
                end: focus,
            };
            self.set_selection(data, Direction::Forward);
        }
    }

    /// Nothing is selected.
    pub fn clear_selection(&mut self) {
        if self.ranges.selection_slot().is_some() {
            self.ranges.select(None, Direction::Forward);
        }
    }

    /// What a node's `contenteditable` says: `Some(true)` for an editable element (`""`, `true`
    /// or `plaintext-only`), `Some(false)` for `false` (any case), `None` to inherit, which an
    /// invalid value does too.
    pub fn editable_attr(&self, node: NodeId) -> Option<bool> {
        let value = self.get(node).and_then(|k| k.attr("contenteditable"))?;
        match value.to_ascii_lowercase().as_str() {
            "" | "true" | "plaintext-only" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    /// Whether `node` is in an editable region: the nearest `contenteditable` that says so
    /// decides, and a document in design mode is editable all through.
    pub fn is_editable(&self, node: NodeId) -> bool {
        let mut at = Some(node);
        while let Some(n) = at {
            if let Some(editable) = self.editable_attr(n) {
                return editable;
            }
            at = self.parent(n);
        }
        self.design_mode
    }

    /// The editing host `node` is in: the root of its editable region, which is the body under
    /// `designMode`.
    pub fn editing_host(&self, node: NodeId) -> Option<NodeId> {
        if !self.is_editable(node) {
            return None;
        }
        let mut top = node;
        while let Some(parent) = self.parent(top).filter(|p| self.is_editable(*p)) {
            top = parent;
        }
        if self.editable_attr(top) == Some(true) {
            return Some(top);
        }
        (self.design_mode && self.contains(self.html, node)).then_some(self.body)
    }

    /// Whether the region `node` is in takes only plain text.
    pub fn is_plaintext_only(&self, node: NodeId) -> bool {
        self.editing_host(node)
            .and_then(|h| self.get(h))
            .and_then(|h| h.attr("contenteditable"))
            .is_some_and(|v| v.eq_ignore_ascii_case("plaintext-only"))
    }

    /// Whether a box of `node`'s kind starts a new line of its own.
    pub fn is_block(&self, node: NodeId) -> bool {
        const BLOCKS: &[&str] = &[
            "address",
            "article",
            "aside",
            "blockquote",
            "body",
            "dd",
            "details",
            "div",
            "dl",
            "dt",
            "fieldset",
            "figcaption",
            "figure",
            "footer",
            "form",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "header",
            "hr",
            "html",
            "li",
            "main",
            "nav",
            "ol",
            "p",
            "pre",
            "section",
            "table",
            "tbody",
            "td",
            "th",
            "thead",
            "tr",
            "ul",
            "textarea",
        ];
        self.get(node).is_some_and(|n| {
            n.tag().is_some_and(|t| BLOCKS.contains(&t))
                || n.is_element() && self.editable_attr(node) == Some(true)
        })
    }

    /// The block `node` is in: the nearest ancestor (or itself) that is one.
    pub fn block_of(&self, node: NodeId) -> NodeId {
        let mut at = Some(node);
        while let Some(n) = at {
            if self.is_block(n) {
                return n;
            }
            at = self.parent(n);
        }
        self.body
    }

    fn is_empty_line_break(&self, node: NodeId, stops: &[NodeId], at: usize) -> bool {
        let br = self.get(node).and_then(|n| n.tag()) == Some("br");
        br && at.checked_sub(1).map(|p| stops[p]).is_none_or(|prev| {
            self.get(prev).and_then(|n| n.tag()) == Some("br")
                || self.block_of(prev) != self.block_of(node)
        })
    }

    /// Where a caret can stand under `root`, in tree order: text, each `<br>` that is a line of
    /// its own, and each block with nothing in it.
    fn stops(&self, root: NodeId) -> Vec<NodeId> {
        let all = self.subtree(root);
        let mut out: Vec<NodeId> = Vec::new();
        for node in all {
            let keep = match self.get(node).map(|n| &n.kind) {
                Some(NodeKind::Text(t)) => {
                    !t.is_empty() && (!t.contains('\n') || !t.trim().is_empty())
                }
                Some(NodeKind::Element(tag)) => match tag.as_str() {
                    "br" => self.is_empty_line_break(node, &out, out.len()),
                    "img" | "hr" | "input" | "video" | "canvas" => true,
                    _ => self.children(node).is_empty() && self.is_block(node),
                },
                _ => false,
            };
            if keep {
                out.push(node);
            }
        }
        out
    }

    /// The stop `point` is at: a text position stays, an element position moves to the stop
    /// after it (or the one before when none comes after).
    fn stop_at(&self, point: Boundary, stops: &[NodeId]) -> Option<(usize, usize)> {
        if self.is_character_data(point.0) {
            return stops
                .iter()
                .position(|s| *s == point.0)
                .map(|i| (i, point.1));
        }
        let mut sample = point.0;
        if let Some(child) = self.children(point.0).get(point.1) {
            sample = *child;
        } else if let Some(last) = self.children(point.0).last() {
            sample = *last;
            let tail = self
                .subtree(sample)
                .into_iter()
                .rev()
                .find(|n| stops.contains(n));
            if let Some(tail) = tail {
                let at = stops.iter().position(|s| *s == tail)?;
                return Some((at, self.stop_length(tail)));
            }
        }
        let inside = self.subtree(sample);
        if let Some(at) = stops.iter().position(|s| inside.contains(s)) {
            return Some((at, 0));
        }
        // Nothing in or after it: the last stop before it.
        stops
            .iter()
            .rposition(|s| self.compare_boundaries((*s, 0), point) == Ordering::Less)
            .map(|at| (at, self.stop_length(stops[at])))
    }

    fn stop_length(&self, stop: NodeId) -> usize {
        if self.is_character_data(stop) {
            self.node_length(stop)
        } else {
            0
        }
    }

    /// The boundary of the DOM that a stop and an offset in it are.
    fn stop_boundary(&self, stop: NodeId, offset: usize) -> Boundary {
        match self.get(stop).map(|n| &n.kind) {
            Some(NodeKind::Text(_)) => (stop, offset),
            Some(NodeKind::Element(tag)) if tag == "br" || tag == "img" || tag == "hr" => {
                self.before_node(stop).unwrap_or((stop, 0))
            }
            _ => (stop, 0),
        }
    }

    /// The boundary one `unit` from `from`, `forward` or back, staying inside `limit` (the
    /// editing host, when there is one). A boundary that can go no further is returned as it is.
    pub fn step_boundary(
        &self,
        from: Boundary,
        unit: Granularity,
        forward: bool,
        limit: Option<NodeId>,
    ) -> Boundary {
        let root = limit.unwrap_or_else(|| self.root_of(from.0));
        let stops = self.stops(root);
        let Some((at, offset)) = self.stop_at(from, &stops) else {
            return from;
        };
        let stop = stops[at];
        let to = match unit {
            Granularity::Character | Granularity::Word => {
                self.step_text(&stops, at, offset, unit == Granularity::Word, forward)
            }
            Granularity::LineBoundary | Granularity::Paragraph => {
                let block = self.block_of(stop);
                let members: Vec<usize> = (0..stops.len())
                    .filter(|i| self.block_of(stops[*i]) == block)
                    .collect();
                let (edge, off) = if forward {
                    let i = *members.last().unwrap_or(&at);
                    (i, self.stop_length(stops[i]))
                } else {
                    (*members.first().unwrap_or(&at), 0)
                };
                (edge, off)
            }
            Granularity::Document => {
                if forward {
                    let i = stops.len() - 1;
                    (i, self.stop_length(stops[i]))
                } else {
                    (0, 0)
                }
            }
        };
        self.stop_boundary(stops[to.0], to.1)
    }

    /// One character or word from offset `offset` of stop `at`.
    fn step_text(
        &self,
        stops: &[NodeId],
        at: usize,
        offset: usize,
        word: bool,
        forward: bool,
    ) -> (usize, usize) {
        let stop = stops[at];
        if let Some(NodeKind::Text(text)) = self.get(stop).map(|n| &n.kind) {
            let mut draft = Draft::new(text.as_str());
            draft.place(utf16_offset_to_utf8(text, offset));
            let moved = match (forward, word) {
                (true, false) => draft.right(),
                (false, false) => draft.left(),
                (true, true) => draft.word_right(),
                (false, true) => draft.word_left(),
            };
            let to = utf8_offset_to_utf16(text, draft.caret());
            if moved && to != offset {
                return (at, to);
            }
        }
        let next = if forward {
            at + 1 < stops.len()
        } else {
            at > 0
        };
        if !next {
            return (at, offset);
        }
        let target = if forward { at + 1 } else { at - 1 };
        let other = stops[target];
        let same_block = self.block_of(other) == self.block_of(stop);
        let len = self.stop_length(other);
        if !self.is_character_data(other) || !same_block {
            // A line break is one step; so is the start of the next block.
            return (target, if forward { 0 } else { len });
        }
        // Inline neighbours share a place: the step goes one further into the neighbour.
        self.step_text(stops, target, if forward { 0 } else { len }, word, forward)
    }

    /// `Selection.modify(alter, direction, granularity)` for the selection's focus.
    pub fn modify_selection(&mut self, extend: bool, forward: bool, unit: Granularity) {
        let Some(focus) = self.selection_focus() else {
            return;
        };
        let (anchor, range) = (self.selection_anchor(), self.selection());
        let host = self.editing_host(focus.0);
        if !extend
            && unit == Granularity::Character
            && let Some((r, _)) = range
            && r.start != r.end
        {
            let edge = if forward { r.end } else { r.start };
            self.select_between(edge, edge);
            return;
        }
        let to = self.step_boundary(focus, unit, forward, host);
        let base = if extend { anchor.unwrap_or(focus) } else { to };
        self.select_between(base, to);
    }

    /// The text of the whole selection.
    pub fn selection_text(&self) -> String {
        self.selection()
            .map(|(r, _)| self.range_text(r))
            .unwrap_or_default()
    }
}
