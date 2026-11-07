//! What the drawer is told of the document: a snapshot and what changed since the last one.

use std::sync::Arc;

use crate::{Document, NodeId, Snapshot, SnapshotData, canvas};

/// What changed since the drawer last asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Every node changed — its own state (attributes, classes, text, value) or its children.
    pub nodes: Vec<NodeId>,
    /// Every node whose computed style is not what it was.
    pub restyled: Vec<NodeId>,
    /// The partitions with something new to draw: for each changed or restyled node, the
    /// partition it is in (and its parent's, for a node that came or went).
    pub partitions: Vec<NodeId>,
    /// Nodes freed by [`Document::sweep`]: forget what was kept against them.
    pub removed: Vec<NodeId>,
    /// The stylesheets changed, so every style was worked out again.
    pub restyled_all: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
            && self.restyled.is_empty()
            && self.removed.is_empty()
            && !self.restyled_all
    }

    /// Adds what `later` says changed after this: the lists of nodes stay sorted and without
    /// repeats, and `removed` is the two in order.
    pub fn merge(&mut self, later: Changes) {
        for (mine, theirs) in [
            (&mut self.nodes, later.nodes),
            (&mut self.restyled, later.restyled),
            (&mut self.partitions, later.partitions),
        ] {
            mine.extend(theirs);
            mine.sort_unstable();
            mine.dedup();
        }
        self.removed.extend(later.removed);
        self.restyled_all |= later.restyled_all;
    }
}

/// A snapshot and what changed since the last: [`Document::update`]'s answer.
pub struct Update {
    pub snapshot: Snapshot,
    pub changes: Changes,
}

impl Document {
    /// Brings the styles up to date and answers a snapshot, with what changed since the last
    /// update. That includes what [`Document::set_layout_provider`]'s layout took on the way.
    pub fn update(&self) -> Update {
        let mut update = self.take_update();
        let mut carried = std::mem::take(&mut self.view.borrow_mut().carried);
        carried.merge(std::mem::take(&mut update.changes));
        update.changes = carried;
        update
    }

    /// [`Document::update`] without what an earlier layout took, which the caller keeps.
    pub(crate) fn take_update(&self) -> Update {
        self.absorb();
        self.sync_images();
        self.commit_canvases();
        let snapshot = self.snapshot();
        let mut view = self.view.borrow_mut();
        let mut nodes: Vec<NodeId> = view.changed.drain().collect();
        nodes.sort_unstable();
        let mut restyled: Vec<NodeId> = view.restyled.drain().collect();
        restyled.sort_unstable();
        let mut partitions: Vec<NodeId> = view.partitions.drain().collect();
        partitions.sort_unstable();
        let changes = Changes {
            nodes,
            restyled,
            partitions,
            removed: std::mem::take(&mut view.removed),
            restyled_all: std::mem::take(&mut view.restyled_all),
        };
        Update { snapshot, changes }
    }

    /// A snapshot of the document and its styles, without taking what changed.
    pub fn snapshot(&self) -> Snapshot {
        self.absorb();
        let state = self.state.borrow();
        let view = self.view.borrow();
        Snapshot(Arc::new(SnapshotData {
            dom: state.dom.clone(),
            styles: view.computed.clone(),
            revision: state.layout_key().0,
            viewport: state.layout_key().1,
            url: state.url.to_string(),
            scroll_offsets: state.scroll_offsets.clone(),
            media: state.media.snapshot(),
            canvases: canvas::snapshot_canvases(&state),
            images: state.images.snapshot(),
            pseudo: view.computed.pseudo().clone(),
        }))
    }
}
