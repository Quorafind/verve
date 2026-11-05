//! Giving nodes back to the arena: the roots of detached trees that may be freed, and freeing
//! them with everything that was kept against them.

use std::collections::HashSet;
use std::sync::Arc;

use super::{Dom, MAX_GENERATION, NodeId, split};

impl Dom {
    /// The roots of detached trees, the candidates for [`Dom::free`].
    pub fn detached_roots(&self) -> Vec<NodeId> {
        self.roots
            .iter()
            .copied()
            .filter(|id| *id != self.document && !self.pinned.contains(id))
            .collect()
    }

    /// Forgets the node and everything in it. Only for what no handle can reach any more:
    /// answers the ids freed, so whoever kept something against them can let it go.
    pub fn free(&mut self, id: NodeId) -> Vec<NodeId> {
        self.detach(id);
        let mut freed = Vec::new();
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            let (index, generation) = split(at);
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            if slot.generation != generation {
                continue;
            }
            if let Some(node) = slot.node.take() {
                match Arc::try_unwrap(node) {
                    Ok(node) => stack.extend(node.children),
                    Err(shared) => stack.extend(shared.children.iter().copied()),
                }
                stack.extend(self.shadows.forget(at));
                slot.connected = false;
                // A place whose generations ran out is retired: no id is ever given twice.
                if slot.generation < MAX_GENERATION {
                    slot.generation += 1;
                    self.free.push(index as u32);
                }
                self.live -= 1;
                self.roots.remove(&at);
                self.partitions.remove(&at);
                self.styles.remove(&at);
                self.pinned.remove(&at);
                self.started.remove(&at);
                self.note_slot_input(None);
                freed.push(at);
            }
        }
        let gone: HashSet<NodeId> = freed.iter().copied().collect();
        self.hovered.retain(|h| !gone.contains(h));
        if self.focused.is_some_and(|f| gone.contains(&f)) {
            self.focused = None;
        }
        if self.active.is_some_and(|a| gone.contains(&a)) {
            self.active = None;
        }
        let mut was_changed = false;
        for id in &freed {
            was_changed |= self.changed_set.remove(id);
        }
        if was_changed {
            self.changed.retain(|c| !gone.contains(c));
        }
        self.relinked.retain(|r| !gone.contains(r));
        self.moved.retain(|(id, _)| !gone.contains(id));
        self.mutations.retain(|(m, _)| !gone.contains(&m.target()));
        self.revision = self.revision.wrapping_add(1);
        freed
    }
}
