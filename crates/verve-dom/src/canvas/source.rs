//! The images `drawImage`, `createPattern` and `createImageBitmap` take: an `<img>`, a
//! `<canvas>`, an `ImageBitmap` or an `OffscreenCanvas`.

use std::sync::Arc;

use boa_engine::{Context, JsResult, JsValue};
use tiny_skia::Pixmap;

use crate::bind::{Kind, handle_of, node_id};
use crate::canvas::context2d::Canvas2d;
use crate::canvas::store::{CanvasStore, warn_video_once};
use crate::dom::NodeId;
use crate::state::{State, shared};

/// An image to draw, and whether drawing it taints the canvas.
pub struct Source {
    pub pixmap: Arc<Pixmap>,
    pub tainted: bool,
}

/// The pixels behind a source — or none, which is what a `<video>` or an image that has not
/// loaded answers.
pub fn source_pixmap(value: &JsValue, context: &mut Context) -> JsResult<Option<Source>> {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let store = &state.canvas;
    if let Some(id) = handle_of(value, Kind::Bitmap) {
        let pixmap = store.bitmaps.get(&id).cloned();
        return Ok(pixmap.map(|pixmap| Source {
            pixmap,
            tainted: false,
        }));
    }
    if let Some(id) = handle_of(value, Kind::Offscreen) {
        return Ok(store.canvases.get(&id).and_then(copy_canvas));
    }
    let Some(node) = node_id(value) else {
        return Ok(None);
    };
    let tag = state.dom.get(node).and_then(|node| node.tag());
    Ok(match tag {
        Some("canvas") => element_canvas(store, node).and_then(copy_canvas),
        Some("img") => image_source(&mut state, node),
        Some("video" | "audio") => {
            warn_video_once(&mut state, context);
            None
        }
        _ => None,
    })
}

fn element_canvas(store: &CanvasStore, node: NodeId) -> Option<&Canvas2d> {
    store.canvases.get(store.elements.get(&node)?)
}

/// A copy of the canvas's pixels, so drawing it into itself reads what it was.
fn copy_canvas(canvas: &Canvas2d) -> Option<Source> {
    Some(Source {
        pixmap: Arc::new(canvas.pixmap.clone()?),
        tainted: canvas.tainted,
    })
}

fn image_source(state: &mut State, node: NodeId) -> Option<Source> {
    let url = state.images.nodes.get(&node)?.url.clone()?;
    state.images.natural(&url)?;
    let tainted = state.images.is_tainted(&url);
    let pixmap = state.images.pixmap(&url)?;
    Some(Source { pixmap, tainted })
}
