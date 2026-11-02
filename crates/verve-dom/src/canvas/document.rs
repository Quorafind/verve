//! The document's side of canvases and images: what a frame does for them, and the entry points
//! the embedder's loader answers through.

use std::sync::Arc;

use crate::Document;
use crate::canvas::image::{Keep, Status, image_size};
use crate::canvas::img::{self, UNDECODABLE, UNFETCHABLE};
use crate::dom::NodeId;

impl Document {
    /// Reads what the images this frame's changes name: an `<img>` whose `src` moved is a load,
    /// and images no element names any more are dropped.
    pub(crate) fn sync_images(&self) {
        let changed: Vec<NodeId> = {
            let state = self.state.borrow();
            let view = self.view.borrow();
            let is_img = |id: &NodeId| state.dom.get(*id).is_some_and(|n| n.tag() == Some("img"));
            view.changed.iter().copied().filter(is_img).collect()
        };
        if changed.is_empty() {
            return;
        }
        let mut state = self.state.borrow_mut();
        for node in changed {
            img::sync(&mut state, node);
        }
        state.images.retain_referenced();
    }

    /// Hands the drawer the canvases painted since the last frame, as changes of their nodes.
    pub(crate) fn commit_canvases(&self) {
        let committed = crate::canvas::commit(&mut self.state.borrow_mut());
        let state = self.state.borrow();
        let mut view = self.view.borrow_mut();
        for node in committed.into_iter().filter(|node| state.dom.exists(*node)) {
            view.changed.insert(node);
            view.partitions.insert(state.dom.partition_of(node));
        }
    }

    /// An image's bytes, from the embedder's loader (the answer to a [`Request::Image`](crate::Request::Image)):
    /// what `<img>` reports from now on and what `drawImage` decodes. The elements naming
    /// `url` get `load`, or `error` when the bytes are no image, and their `decode()` promises
    /// settle.
    pub fn set_image_bytes(
        &self,
        runtime: &mut verve::Runtime,
        url: &str,
        mime: Option<&str>,
        bytes: Arc<[u8]>,
    ) {
        let (waiting, failure) = {
            let mut state = self.state.borrow_mut();
            if image_size(&bytes).is_none() {
                state.images.mark_failed(url);
                (
                    img::settle(&mut state, url, Status::Broken),
                    Some(UNDECODABLE),
                )
            } else {
                let named = state
                    .images
                    .nodes
                    .values()
                    .any(|img| img.url.as_deref() == Some(url));
                let keep = if named {
                    Keep::WhileNamed
                } else {
                    Keep::Always
                };
                state
                    .images
                    .insert(url, mime.map(str::to_owned), bytes, keep);
                #[cfg(feature = "canvas")]
                if img::is_cross_origin(&state.url, url) {
                    state.images.mark_tainted(url);
                }
                (img::settle(&mut state, url, Status::Loaded), None)
            }
        };
        img::deliver(runtime.context(), waiting, failure);
    }

    /// The loader could not fetch an image: the elements naming `url` get `error`, and their
    /// `decode()` promises reject with an `EncodingError`.
    pub fn fail_image(&self, runtime: &mut verve::Runtime, url: &str) {
        let waiting = {
            let mut state = self.state.borrow_mut();
            state.images.mark_failed(url);
            img::settle(&mut state, url, Status::Broken)
        };
        img::deliver(runtime.context(), waiting, Some(UNFETCHABLE));
    }

    /// A page's `@font-face` bytes under the family it declared, for canvas text to draw with.
    #[cfg(feature = "canvas")]
    pub fn add_web_font(
        &self,
        family: &str,
        descriptors: &crate::canvas::FontFaceRule,
        bytes: Arc<[u8]>,
    ) {
        let mut state = self.state.borrow_mut();
        let fonts = state
            .canvas
            .fonts
            .get_or_insert_with(crate::canvas::FontStack::system);
        fonts.add_web_font(family, descriptors, bytes);
    }
}
