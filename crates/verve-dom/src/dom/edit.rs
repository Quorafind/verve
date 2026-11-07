//! What editing does to the tree: typing, deleting across blocks, splitting a block, line breaks
//! and inline formatting, with the whitespace handling an editable region gives text (a space
//! that would collapse is written as a no-break space), and the history of changes a host's
//! undo walks.
//!
//! Every function here changes the tree and says where the caret goes; none of them fires an
//! event or reads the selection: [`crate::editing`] does both around them.

use std::cmp::Ordering;

use super::{Dom, Mutation, NodeId, NodeKind, RangeData, utf16_len};
use crate::layout_provider::Boundary;

const NBSP: char = '\u{a0}';

fn is_heading(tag: &str) -> bool {
    matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// One change of the tree, as a history replays it backwards.
#[derive(Clone, Debug)]
pub(crate) enum Edit {
    /// A change to the list of children or to an attribute.
    Tree(Mutation),
    /// The `added` units at `at` in a text node were `removed` before. Only that stretch is put
    /// back, so text a script wrote elsewhere in the node since stays.
    Text {
        target: NodeId,
        at: usize,
        removed: String,
        added: usize,
    },
}

impl Edit {
    /// The node the change was made to.
    pub(crate) fn target(&self) -> NodeId {
        match self {
            Self::Tree(mutation) => mutation.target(),
            Self::Text { target, .. } => *target,
        }
    }
}

/// What turned `old` into `new`: the UTF-16 offset where they first differ, the text that was
/// there, and how many units took its place.
fn text_change(old: &str, new: &str) -> (usize, String, usize) {
    let before = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_rest, new_rest): (Vec<char>, Vec<char>) = (
        old.chars().skip(before).collect(),
        new.chars().skip(before).collect(),
    );
    let after = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let at = old.chars().take(before).map(char::len_utf16).sum();
    let removed: String = old_rest[..old_rest.len() - after].iter().collect();
    let added = new_rest[..new_rest.len() - after]
        .iter()
        .map(|c| c.len_utf16())
        .sum();
    (at, removed, added)
}

impl Dom {
    // ─── Undo ───────────────────────────────────────────────────────────────────────────────

    /// Runs `f`, and answers what it changed to the tree as the edits a history replays.
    pub(crate) fn recording<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> (R, Vec<Edit>) {
        let was = std::mem::replace(&mut self.record_mutations, true);
        let mark = self.mutations.len();
        let result = f(self);
        let made: Vec<Mutation> = self.mutations[mark..]
            .iter()
            .map(|(m, _)| m.clone())
            .collect();
        self.record_mutations = was;
        let log = made
            .iter()
            .enumerate()
            .map(|(at, mutation)| self.as_edit(mutation, &made[at + 1..]))
            .collect();
        (result, log)
    }

    /// `mutation` as an edit; `later` are the mutations made after it.
    fn as_edit(&self, mutation: &Mutation, later: &[Mutation]) -> Edit {
        let Mutation::CharacterData { target, old } = mutation else {
            return Edit::Tree(mutation.clone());
        };
        // The data it left is what the next change to the node found, or what the node holds.
        let next = later.iter().find_map(|m| match m {
            Mutation::CharacterData { target: t, old } if t == target => Some(old.as_str()),
            _ => None,
        });
        match next.or_else(|| self.get(*target).and_then(|n| n.data())) {
            Some(new) => {
                let (at, removed, added) = text_change(old, new);
                Edit::Text {
                    target: *target,
                    at,
                    removed,
                    added,
                }
            }
            None => Edit::Tree(mutation.clone()),
        }
    }

    /// Undoes `log`, newest change first, and answers what that did, to be undone in turn.
    pub(crate) fn revert(&mut self, log: &[Edit]) -> Vec<Edit> {
        self.recording(|dom| {
            for edit in log.iter().rev() {
                match edit {
                    Edit::Text {
                        target,
                        at,
                        removed,
                        added,
                    } => dom.replace_data(*target, *at, *added, removed),
                    Edit::Tree(mutation) => dom.revert_tree(mutation),
                }
            }
        })
        .1
    }

    fn revert_tree(&mut self, mutation: &Mutation) {
        match mutation {
            Mutation::CharacterData { target, old } => self.set_text(*target, old),
            Mutation::Attributes { target, name, old } => match old {
                Some(old) => self.set_attr(*target, name, old),
                None => self.remove_attr(*target, name),
            },
            Mutation::ChildList {
                target,
                added,
                removed,
                previous,
                next,
            } => {
                for node in added {
                    self.detach(*node);
                }
                let before = next
                    .filter(|n| self.parent(*n) == Some(*target))
                    .or_else(|| {
                        previous
                            .filter(|p| self.parent(*p) == Some(*target))
                            .and_then(|p| self.sibling(p, 1, false))
                    });
                for node in removed {
                    self.insert_before(*target, *node, before);
                }
            }
        }
    }

    // ─── Text ───────────────────────────────────────────────────────────────────────────────

    /// Whether anything is shown in `block`: text that is not blank, a line break, an image.
    pub fn has_content(&self, block: NodeId) -> bool {
        self.subtree(block)
            .into_iter()
            .any(|n| match self.get(n).map(|k| &k.kind) {
                Some(NodeKind::Text(t)) => !t.is_empty(),
                Some(NodeKind::Element(tag)) => matches!(tag.as_str(), "img" | "hr" | "input"),
                _ => false,
            })
    }

    /// A `<br>` in an empty block, so the block keeps a line.
    pub fn keep_line(&mut self, block: NodeId) {
        if self.has_content(block) {
            return;
        }
        if self
            .subtree(block)
            .into_iter()
            .any(|n| self.get(n).and_then(|k| k.tag()) == Some("br"))
        {
            return;
        }
        let br = self.create_element("br");
        self.append(block, br);
    }

    /// Whether no text comes before `node` in its block.
    fn starts_block(&self, node: NodeId) -> bool {
        let block = self.block_of(node);
        for other in self.subtree(block) {
            if other == node {
                return true;
            }
            if self
                .get(other)
                .and_then(|k| k.data())
                .is_some_and(|t| !t.is_empty())
                && self.is_character_data(other)
            {
                return false;
            }
        }
        true
    }

    /// Whether no text comes after `node` in its block.
    fn ends_block(&self, node: NodeId) -> bool {
        let block = self.block_of(node);
        let all = self.subtree(block);
        let at = all.iter().position(|n| *n == node).unwrap_or(0);
        !all[at + 1..]
            .iter()
            .any(|n| self.is_character_data(*n) && self.node_length(*n) > 0)
    }

    /// Rewrites the run of spaces around `offset` in a text node the way an editable region
    /// writes them: no-break where a plain space would collapse (at either end of a line, after
    /// another space), plain where it would show (Electron 39.8.10: Ctrl+Backspace on
    /// `hello world|` leaves `hello&nbsp;`).
    pub fn rebalance_spaces(&mut self, node: NodeId, offset: usize) {
        let Some(NodeKind::Text(text)) = self.get(node).map(|n| &n.kind) else {
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;
        let mut units = 0;
        while at < chars.len() && units < offset {
            units += chars[at].len_utf16();
            at += 1;
        }
        let blank = |c: char| c == ' ' || c == NBSP;
        let mut from = at.min(chars.len());
        let mut to = from;
        while from > 0 && blank(chars[from - 1]) {
            from -= 1;
        }
        while to < chars.len() && blank(chars[to]) {
            to += 1;
        }
        if from == to {
            return;
        }
        let at_start = from == 0 && self.starts_block(node);
        let at_end = to == chars.len() && self.ends_block(node);
        let mut fixed = chars.clone();
        for i in from..to {
            let first = i == from;
            let last = i == to - 1;
            let previous_plain = i > from && fixed[i - 1] == ' ';
            fixed[i] = if first && at_start || previous_plain || last && at_end {
                NBSP
            } else {
                ' '
            };
        }
        if fixed != chars {
            let changed: String = fixed.into_iter().collect();
            self.ranges.quiet = true;
            self.set_text(node, &changed);
            self.ranges.quiet = false;
        }
    }

    /// The place text typed at `at` goes: a text node and an offset in it, made if there is none.
    pub(crate) fn text_spot(&mut self, at: Boundary) -> Boundary {
        if matches!(self.get(at.0).map(|n| &n.kind), Some(NodeKind::Text(_))) {
            return at;
        }
        let (parent, index) = if self.is_character_data(at.0) {
            self.before_node(at.0).unwrap_or(at)
        } else {
            at
        };
        let kids = self.children(parent).to_vec();
        if let Some(next) = kids.get(index)
            && matches!(self.get(*next).map(|n| &n.kind), Some(NodeKind::Text(_)))
        {
            return (*next, 0);
        }
        if let Some(previous) = index.checked_sub(1).map(|i| kids[i])
            && matches!(self.get(previous).map(|n| &n.kind), Some(NodeKind::Text(_)))
        {
            return (previous, self.node_length(previous));
        }
        // A placeholder line break gives way to the first letters typed.
        if let Some(next) = kids.get(index)
            && self.get(*next).and_then(|k| k.tag()) == Some("br")
            && kids.len() == 1
        {
            self.detach(*next);
        }
        let text = self.create_text("");
        let place = self.children(parent).get(index).copied();
        self.insert_before(parent, text, place);
        (text, 0)
    }

    /// `text` typed at `at`; the caret ends up after it.
    pub fn insert_text_at(&mut self, at: Boundary, text: &str) -> Boundary {
        let (node, offset) = self.text_spot(at);
        self.replace_data(node, offset, 0, text);
        let end = offset + utf16_len(text);
        self.rebalance_spaces(node, offset);
        self.rebalance_spaces(node, end);
        (node, end)
    }

    /// Where the first stop at or after the start of `host` is: its caret position.
    pub fn caret_in(&self, host: NodeId) -> Boundary {
        let first = self.subtree(host).into_iter().find(
            |n| matches!(self.get(*n).map(|k| &k.kind), Some(NodeKind::Text(t)) if !t.is_empty()),
        );
        match first {
            Some(text) => (text, 0),
            None => (host, 0),
        }
    }

    // ─── Deleting and splitting ─────────────────────────────────────────────────────────────

    /// The contents of `range` deleted, a block the end was in joined onto the block the start
    /// was in; answers where the caret goes.
    pub fn delete_between(&mut self, range: RangeData) -> Boundary {
        let (start, end) = (range.start, range.end);
        if start == end {
            return start;
        }
        let (a, b) = (self.block_of(start.0), self.block_of(end.0));
        let (fragment, _) = self.copy_contents(range, super::Contents::Delete);
        self.empty(fragment);
        let mut caret = start;
        if a != b && self.exists(b) {
            caret = self.join_blocks(a, b, caret);
        }
        let block = self.block_of(caret.0);
        if self.is_character_data(caret.0) && self.node_length(caret.0) == 0 {
            let spot = self.before_node(caret.0);
            self.detach(caret.0);
            caret = spot.unwrap_or(caret);
        }
        if !self.has_content(block) {
            self.keep_line(block);
        }
        if let Some(NodeKind::Text(_)) = self.get(caret.0).map(|n| &n.kind) {
            self.rebalance_spaces(caret.0, caret.1);
        }
        caret
    }

    /// The blocks `a` and `b`, the second after the first, made one (Electron 39.8.10). The first
    /// line of `b` (its inline run up to a block or a `<br>`) goes to the caret in `a`, and `b`
    /// and every ancestor of it up to `a` that is left empty are removed. When `a` is inside
    /// `b`, the inline run after `a` is pulled into it instead. Answers the caret.
    fn join_blocks(&mut self, a: NodeId, b: NodeId, caret: Boundary) -> Boundary {
        if self.contains(b, a) {
            self.pull_run_into(a, b);
            // An element that wrapped nothing but the merged block goes (Electron 39.8.10:
            // `<div><p>abc|</p>def</div>` gives `<p>abcdef</p>`).
            if self.editable_attr(b) != Some(true) && self.children(b) == [a] {
                self.unwrap_element(b);
            }
            return caret;
        }
        if !self.has_content(a) && !self.contains(a, b) {
            self.empty(a);
        }
        let line: Vec<NodeId> = self
            .children(b)
            .iter()
            .copied()
            .take_while(|c| !self.is_block(*c) && self.get(*c).and_then(|n| n.tag()) != Some("br"))
            .collect();
        let at = if self.contains(a, caret.0) {
            caret
        } else {
            (a, self.children(a).len())
        };
        if self.has_content(b) {
            let (parent, before) = self.insertion_point(at);
            for node in line {
                self.insert_before(parent, node, before);
            }
            // The break that ended the line goes with it.
            let first = self.children(b).first().copied();
            if let Some(br) = first.filter(|c| self.get(*c).and_then(|n| n.tag()) == Some("br")) {
                self.detach(br);
            }
        }
        self.prune_up_to(b, a);
        at
    }

    /// `node` removed if nothing of it is left (a block with no content, an element with no
    /// children), then its parent likewise, up to the node that holds `keep`.
    fn prune_up_to(&mut self, node: NodeId, keep: NodeId) {
        let mut at = node;
        while !self.contains(at, keep) {
            let vacant = if at == node {
                !self.has_content(at)
            } else {
                self.children(at).is_empty()
            };
            if !vacant {
                break;
            }
            let Some(parent) = self.parent(at) else { break };
            self.detach(at);
            at = parent;
        }
    }

    /// The inline nodes that follow `a` inside `outer` moved to the end of `a`.
    fn pull_run_into(&mut self, a: NodeId, outer: NodeId) {
        let mut top = a;
        while let Some(parent) = self.parent(top).filter(|p| *p != outer) {
            top = parent;
        }
        let mut next = self.sibling(top, 1, false);
        while let Some(node) = next.filter(|n| !self.is_block(*n)) {
            next = self.sibling(node, 1, false);
            self.append(a, node);
        }
    }

    /// The block holding `at` split in two there (the second made of `tag`), what follows moving
    /// to the new block; answers where the caret goes. `host` is the editing host.
    pub fn split_block_at(&mut self, at: Boundary, host: NodeId, separator: &str) -> Boundary {
        let block = self.block_of(at.0);
        let bare = block == host;
        let end = (block, self.node_length(block));
        let tail = self.copy_contents(RangeData { start: at, end }, super::Contents::Extract);
        let rest = tail.0;
        // A heading keeps its tag when text follows the caret; at its end the new line is made
        // of the separator (Electron 39.8.10).
        let follows = self.has_content(rest);
        let tag = match self.get(block).and_then(|n| n.tag()).map(str::to_owned) {
            Some(t) if matches!(t.as_str(), "p" | "li" | "div" | "blockquote") && !bare => t,
            Some(t) if is_heading(&t) && !bare && follows => t,
            _ => separator.to_owned(),
        };
        let fresh = self.create_element(&tag);
        for child in self.children(rest).to_vec() {
            let blank =
                matches!(self.get(child).map(|n| &n.kind), Some(NodeKind::Text(t)) if t.is_empty());
            if !blank {
                self.append(fresh, child);
            }
        }
        // A block that comes right after the caret leaves the new line a break of its own.
        if let Some(first) = self.children(fresh).first().copied()
            && self.is_block(first)
        {
            let br = self.create_element("br");
            self.insert_before(fresh, br, Some(first));
        }
        self.keep_line(fresh);
        let parent = if bare {
            host
        } else {
            self.parent(block).unwrap_or(host)
        };
        if bare {
            // Text typed straight into the host: the line stays bare, the new one is a block
            // (Electron 39.8.10).
            self.append(host, fresh);
        } else {
            let next = self.sibling(block, 1, false);
            self.insert_before(parent, fresh, next);
            self.keep_line(block);
        }
        match self.children(fresh).first().copied() {
            Some(first) if matches!(self.get(first).map(|n| &n.kind), Some(NodeKind::Text(_))) => {
                (first, 0)
            }
            _ => (fresh, 0),
        }
    }

    /// A line break at `at`; answers where the caret goes. At the end of a block a second break
    /// keeps the new line standing.
    pub fn insert_break_at(&mut self, at: Boundary) -> Boundary {
        let (parent, index) = self.break_spot(at);
        let br = self.create_element("br");
        let before = self.children(parent).get(index).copied();
        self.insert_before(parent, br, before);
        let end = !self.has_content_after(br);
        if end {
            let extra = self.create_element("br");
            let after = self.sibling(br, 1, false);
            self.insert_before(parent, extra, after);
        }
        (parent, index + 1)
    }

    /// The parent and child index a break at `at` goes in; a text node is split there.
    fn break_spot(&mut self, at: Boundary) -> Boundary {
        if !matches!(self.get(at.0).map(|n| &n.kind), Some(NodeKind::Text(_))) {
            return at;
        }
        let parent = self.parent(at.0).unwrap_or(at.0);
        let index = self.index_of(parent, at.0).unwrap_or(0);
        if at.1 == 0 {
            return (parent, index);
        }
        if at.1 < self.node_length(at.0) {
            self.split_text(at.0, at.1);
        }
        (parent, index + 1)
    }

    fn has_content_after(&self, node: NodeId) -> bool {
        let block = self.block_of(node);
        let all = self.subtree(block);
        let at = all.iter().position(|n| *n == node).unwrap_or(0);
        all[at + 1..].iter().any(|n| {
            self.is_character_data(*n) && self.node_length(*n) > 0
                || self
                    .get(*n)
                    .and_then(|k| k.tag())
                    .is_some_and(|t| t == "br" || t == "img")
        })
    }

    // ─── Formatting ─────────────────────────────────────────────────────────────────────────

    /// The text nodes `range` covers, split so that each is covered whole.
    pub fn split_covered_text(&mut self, range: RangeData) -> Vec<NodeId> {
        let (mut start, mut end) = (range.start, range.end);
        let is_text =
            |dom: &Self, n| matches!(dom.get(n).map(|k| &k.kind), Some(NodeKind::Text(_)));
        if is_text(self, end.0) && end.1 > 0 && end.1 < self.node_length(end.0) {
            self.split_text(end.0, end.1);
        }
        if is_text(self, start.0) && start.1 > 0 && start.1 < self.node_length(start.0) {
            let same = start.0 == end.0;
            if let Some(tail) = self.split_text(start.0, start.1) {
                if same {
                    end = (tail, end.1 - start.1);
                }
                start = (tail, 0);
            }
        }
        let common = self.common_ancestor(start.0, end.0);
        self.subtree(common)
            .into_iter()
            .filter(|n| {
                is_text(self, *n)
                    && self.node_length(*n) > 0
                    && self.compare_boundaries((*n, 0), start) != Ordering::Less
                    && self.compare_boundaries((*n, self.node_length(*n)), end) != Ordering::Greater
            })
            .collect()
    }

    /// Whether `node` is inside an element of one of `tags`, below `host`.
    pub fn inside_tag(&self, node: NodeId, tags: &[&str], host: NodeId) -> Option<NodeId> {
        let mut at = self.parent(node);
        while let Some(n) = at {
            if n == host {
                return None;
            }
            if self
                .get(n)
                .and_then(|k| k.tag())
                .is_some_and(|t| tags.contains(&t))
            {
                return Some(n);
            }
            at = self.parent(n);
        }
        None
    }

    /// Wraps the covered text in `<tag>` (with `attribute`), or unwraps it when all of it already
    /// is in one of `tags`; answers the range to select afterwards.
    pub fn toggle_format(
        &mut self,
        range: RangeData,
        host: NodeId,
        tags: &[&str],
        attribute: Option<(&str, &str)>,
    ) -> Option<RangeData> {
        let texts = self.split_covered_text(range);
        let (first, last) = (*texts.first()?, *texts.last()?);
        let done = RangeData {
            start: (first, 0),
            end: (last, self.node_length(last)),
        };
        let inside: Vec<Option<NodeId>> = texts
            .iter()
            .map(|t| self.inside_tag(*t, tags, host))
            .collect();
        if inside.iter().all(Option::is_some) {
            for element in inside.into_iter().flatten() {
                self.unwrap_element(element);
            }
            return Some(done);
        }
        let mut group: Vec<NodeId> = Vec::new();
        let flush = |dom: &mut Self, group: &mut Vec<NodeId>| {
            let Some(first) = group.first().copied() else {
                return;
            };
            let wrapper = dom.create_element(tags[0]);
            if let Some((name, value)) = attribute {
                dom.set_attr(wrapper, name, value);
            }
            dom.insert_before(dom.parent(first).unwrap_or(host), wrapper, Some(first));
            for t in group.drain(..) {
                dom.append(wrapper, t);
            }
        };
        for (text, wrapped) in texts.iter().zip(&inside) {
            let adjacent = group
                .last()
                .is_none_or(|prev| self.sibling(*prev, 1, false) == Some(*text));
            if wrapped.is_some() || !adjacent {
                flush(self, &mut group);
            }
            if wrapped.is_none() {
                group.push(*text);
            }
        }
        flush(self, &mut group);
        Some(done)
    }

    /// `element` replaced by its children.
    pub fn unwrap_element(&mut self, element: NodeId) {
        let Some(parent) = self.parent(element) else {
            return;
        };
        for child in self.children(element).to_vec() {
            self.insert_before(parent, child, Some(element));
        }
        self.detach(element);
    }

    /// The block `at` is in replaced by an element of `tag` with the same content.
    pub fn change_block(&mut self, at: Boundary, host: NodeId, tag: &str) -> Option<NodeId> {
        let block = self.block_of(at.0);
        if block == host {
            return None;
        }
        let fresh = self.create_element(tag);
        let parent = self.parent(block)?;
        self.insert_before(parent, fresh, Some(block));
        for child in self.children(block).to_vec() {
            self.append(fresh, child);
        }
        self.detach(block);
        Some(fresh)
    }
}

impl Dom {
    /// Text typed at `at` into a region that keeps its spaces as typed (`plaintext-only`).
    pub fn insert_plain_at(&mut self, at: Boundary, text: &str) -> Boundary {
        let (node, offset) = self.text_spot(at);
        self.replace_data(node, offset, 0, text);
        (node, offset + utf16_len(text))
    }

    /// Where a node inserted at `at` goes: its parent, and the node it goes before. A text node
    /// is split there.
    pub fn insertion_point(&mut self, at: Boundary) -> (NodeId, Option<NodeId>) {
        if !self.is_character_data(at.0) {
            return (at.0, self.children(at.0).get(at.1).copied());
        }
        let parent = self.parent(at.0).unwrap_or(at.0);
        if at.1 == 0 {
            return (parent, Some(at.0));
        }
        if at.1 >= self.node_length(at.0) {
            return (parent, self.sibling(at.0, 1, false));
        }
        (parent, self.split_text(at.0, at.1))
    }

    /// The block `at` is in wrapped in a list of `tag`, as Electron 39.8.10 does it (probe1:
    /// `<p><ol><li>hello world</li></ol></p>`): the block's content in a new `<li>`, and the list
    /// inside the block.
    pub fn wrap_block_in_list(&mut self, at: Boundary, tag: &str) -> Option<NodeId> {
        let block = self.block_of(at.0);
        let list = self.create_element(tag);
        let item = self.create_element("li");
        for child in self.children(block).to_vec() {
            self.append(item, child);
        }
        self.append(list, item);
        self.append(block, list);
        Some(list)
    }
}
