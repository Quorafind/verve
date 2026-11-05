//! What the host marks on nodes: those it holds itself, those drawn as views of their own, and
//! the `<script>`s that have run or never will.

use super::{Dom, NodeId};

impl Dom {
    /// The host holds `id` itself: it is never reclaimed.
    pub fn pin(&mut self, id: NodeId) {
        self.pinned.insert(id);
    }

    pub fn unpin(&mut self, id: NodeId) {
        self.pinned.remove(&id);
    }

    pub fn is_pinned(&self, id: NodeId) -> bool {
        self.pinned.contains(&id)
    }

    /// Draws `id`'s subtree as a view of its own, redrawn only when something in it changes.
    pub fn set_partition(&mut self, id: NodeId, on: bool) {
        if on {
            self.partitions.insert(id);
        } else {
            self.partitions.remove(&id);
        }
    }

    pub fn is_partition(&self, id: NodeId) -> bool {
        self.partitions.contains(&id)
    }

    /// Marks the `<script>` `id` as one that has run or never will (HTML's "already started"):
    /// the parser's own, and those `innerHTML` made.
    pub fn mark_started(&mut self, id: NodeId) {
        self.started.insert(id);
    }

    /// Whether the `<script>` `id` has run or never will.
    pub fn is_started(&self, id: NodeId) -> bool {
        self.started.contains(&id)
    }
}
