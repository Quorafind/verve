//! What the drawer reads of the document: the arena and every node's computed style, as they
//! were when the snapshot was taken.

use std::collections::HashMap;
use std::sync::Arc;

use crate::canvas;
use crate::css::Computed;
use crate::dom::{Dom, NodeId};
use crate::media::MediaState;

/// What the drawer reads: the document and every node's style, as they were when it was taken.
/// Cheap to take and to clone — the nodes and styles are shared with the document's own until
/// they change — and `Send`, for a drawer on another thread.
#[derive(Clone)]
pub struct Snapshot(pub(crate) Arc<SnapshotData>);

pub struct SnapshotData {
    pub dom: Dom,
    pub styles: Computed,
    /// Moves whenever anything here may have changed.
    pub revision: u64,
    pub viewport: (f32, f32),
    pub url: String,
    pub scroll_offsets: std::collections::HashMap<NodeId, [f32; 2]>,
    /// 活跃媒体代的元数据；移除、空源和回收的节点不在这里，不保存像素。
    pub media: HashMap<NodeId, MediaState>,
    /// Each canvas's last committed frame: premultiplied RGBA8, row-major, `width * 4` stride.
    pub canvases: HashMap<NodeId, canvas::CanvasFrame>,
    /// Each image the page has bytes for, by resolved address, for the drawer to decode.
    pub images: HashMap<String, canvas::ImageBytes>,
}

impl std::ops::Deref for Snapshot {
    type Target = SnapshotData;

    fn deref(&self) -> &SnapshotData {
        &self.0
    }
}

impl Snapshot {
    /// A node's computed value of `property`.
    pub fn style(&self, node: NodeId, property: &str) -> Option<&str> {
        self.styles.get(node, property)
    }

    /// Whether the node is drawn at all: connected, and neither it nor anything it is in
    /// `display: none`.
    pub fn is_rendered(&self, node: NodeId) -> bool {
        let mut at = Some(node);
        while let Some(id) = at {
            if self.styles.get(id, "display") == Some("none") {
                return false;
            }
            at = self.dom.parent(id);
        }
        self.dom.is_connected(node)
    }
}
