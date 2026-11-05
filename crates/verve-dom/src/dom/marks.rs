//! What the host marks on nodes: those it holds itself, and those drawn as views of their own.

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
}
