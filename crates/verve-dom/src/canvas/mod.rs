//! Canvas 2D and images, as the renderer sees them: the pixels a `<canvas>` holds and hands to
//! the drawer, the bytes an `<img>` was fetched as, and the objects a script draws through.
//!
//! The pixels live in the document's state on the renderer's own thread — a `fillRect` lands
//! before it returns — and reach the drawer as an immutable [`CanvasFrame`] at the end of a
//! frame. The `canvas` feature (on by default) builds the drawing; without it a page has no 2D
//! context and `<img>` still reads its size from the header.

#[cfg(feature = "canvas")]
mod bitmap;
#[cfg(feature = "canvas")]
mod commands;
#[cfg(feature = "canvas")]
mod context2d;
mod document;
#[cfg(feature = "canvas")]
mod effects;
#[cfg(feature = "canvas")]
mod element;
#[cfg(feature = "canvas")]
mod fonts;
mod image;
mod img;
#[cfg(feature = "canvas")]
mod install;
#[cfg(feature = "canvas")]
mod matrix;
#[cfg(feature = "canvas")]
mod paint;
#[cfg(feature = "canvas")]
mod path;
#[cfg(feature = "canvas")]
mod pixels;
#[cfg(feature = "canvas")]
mod properties;
#[cfg(feature = "canvas")]
mod source;
#[cfg(feature = "canvas")]
mod store;
#[cfg(feature = "canvas")]
mod text;

use std::collections::HashSet;
use std::sync::Arc;

use boa_engine::{Context, JsResult, JsValue};

use crate::bind::node::{read_dom, with_dom};
use crate::bind::node_of;
use crate::dom::{Dom, NodeId};
use crate::state::State;

#[cfg(feature = "canvas")]
pub(crate) use element::{CANVAS_ACCESSORS, CANVAS_METHODS};
#[cfg(feature = "canvas")]
pub use fonts::{FontFaceRule, FontStack, generic_families};
pub(crate) use image::ImageStore;
pub use image::{DecodeError, DecodedImage, ImageBytes, decode, image_size};
pub(crate) use img::{IMAGE_ACCESSORS, IMAGE_METHODS, IMAGE_REFLECTED, flush_events};
#[cfg(feature = "canvas")]
pub(crate) use install::install;
#[cfg(feature = "canvas")]
pub(crate) use store::{CanvasStore, commit, snapshot_canvases};

/// The font size relative lengths in canvas text properties resolve against: the document's
/// default, as `letterSpacing: '1em'` is read in a canvas with no style of its own.
#[cfg(feature = "canvas")]
pub(crate) const ROOT_FONT_SIZE: f32 = 16.;

/// The size of a `<canvas>` that sets none (HTML, "the canvas element").
const DEFAULT_WIDTH: u32 = 300;
const DEFAULT_HEIGHT: u32 = 150;

/// The first `N` arguments as numbers, or `None` when one is missing or not finite: the
/// canvas methods that ignore such a call.
#[cfg(feature = "canvas")]
pub(crate) fn finite_numbers<const N: usize>(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<Option<[f32; N]>> {
    let mut out = [0.; N];
    for (at, slot) in out.iter_mut().enumerate() {
        let value = crate::bind::arg(args, at).to_number(context)?;
        if !value.is_finite() {
            return Ok(None);
        }
        *slot = value as f32;
    }
    Ok(Some(out))
}

/// A canvas's committed pixels: premultiplied RGBA8, row-major, `width * 4` stride.
#[derive(Clone)]
pub struct CanvasFrame {
    pub width: u32,
    pub height: u32,
    /// Moves whenever the pixels do: the drawer uploads a frame it has not seen.
    pub generation: u64,
    pub pixels: Arc<[u8]>,
}

/// One side of a rectangle.
#[derive(Clone, Copy)]
pub(crate) enum Axis {
    Width,
    Height,
}

impl Axis {
    /// The attribute that sets it.
    pub(crate) fn attribute(self) -> &'static str {
        match self {
            Axis::Width => "width",
            Axis::Height => "height",
        }
    }
}

/// The size a `<canvas>` element's attributes give.
pub(crate) fn element_size(dom: &Dom, node: NodeId) -> (u32, u32) {
    let side = |axis: Axis, default| {
        let attribute = dom.get_attr(node, axis.attribute());
        attribute
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    };
    (
        side(Axis::Width, DEFAULT_WIDTH),
        side(Axis::Height, DEFAULT_HEIGHT),
    )
}

/// `canvas.width` and `canvas.height`.
pub(crate) fn element_dimension(
    this: &JsValue,
    context: &mut Context,
    axis: Axis,
) -> JsResult<JsValue> {
    let node = node_of(this)?;
    let (width, height) = read_dom(context, |dom| element_size(dom, node));
    Ok(match axis {
        Axis::Width => width,
        Axis::Height => height,
    }
    .into())
}

/// Writes the attribute `canvas.width`, `canvas.height` or `img.width` sets, and answers the
/// element and the number written.
pub(crate) fn write_dimension(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    axis: Axis,
) -> JsResult<(NodeId, u32)> {
    let node = node_of(this)?;
    let value = crate::bind::arg(args, 0).to_u32(context)?;
    with_dom(context, |dom| {
        dom.set_attr(node, axis.attribute(), &value.to_string())
    });
    Ok((node, value))
}

/// The setter of an element's `width` or `height`.
pub(crate) fn set_dimension(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    axis: Axis,
) -> JsResult<JsValue> {
    write_dimension(this, args, context, axis)?;
    Ok(JsValue::undefined())
}

/// Drops what the nodes in `gone` owned: their canvases and the images they named.
pub(crate) fn forget(state: &mut State, gone: &HashSet<NodeId>) {
    #[cfg(feature = "canvas")]
    state.canvas.forget_elements(gone);
    state.images.nodes.retain(|node, _| !gone.contains(node));
    state.images.retain_referenced();
}

/// Without the `canvas` feature a page has no 2D context: `getContext` answers `null`, as it
/// does in a browser without canvas support, and nothing is ever committed.
#[cfg(not(feature = "canvas"))]
mod absent {
    use super::*;
    use crate::bind::{Accessor, Method};
    use std::collections::HashMap;

    pub(crate) const CANVAS_METHODS: &[Method] =
        &[("getContext", 1, |_, _, _| Ok(JsValue::null()))];

    pub(crate) const CANVAS_ACCESSORS: &[Accessor] = &[
        (
            "width",
            Some(|this, _, cx| element_dimension(this, cx, Axis::Width)),
            Some(|this, args, cx| set_dimension(this, args, cx, Axis::Width)),
        ),
        (
            "height",
            Some(|this, _, cx| element_dimension(this, cx, Axis::Height)),
            Some(|this, args, cx| set_dimension(this, args, cx, Axis::Height)),
        ),
    ];

    pub(crate) fn install(_: &mut Context) -> JsResult<()> {
        Ok(())
    }

    pub(crate) fn commit(_: &mut State) -> Vec<NodeId> {
        Vec::new()
    }

    pub(crate) fn snapshot_canvases(_: &State) -> HashMap<NodeId, CanvasFrame> {
        HashMap::new()
    }
}
#[cfg(not(feature = "canvas"))]
pub(crate) use absent::*;
