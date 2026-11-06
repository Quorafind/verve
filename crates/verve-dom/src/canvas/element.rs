//! `HTMLCanvasElement`: its size, `getContext`, and the exports `toDataURL` and `toBlob`.
//!
//! An element gets its pixels the first time a context is asked of it, with the size its
//! attributes give; from then on setting `width` or `height` resizes them.

use boa_engine::{Context, JsResult, JsValue};
use tiny_skia::Pixmap;

use crate::bind::node::read_dom;
use crate::bind::{Accessor, Kind, Method, dom_error, node_of};
use crate::canvas::context2d::{Canvas2d, CanvasOwner};
use crate::canvas::pixels::{DEFAULT_JPEG_QUALITY, Encoded, ImageType, encode};
use crate::canvas::store::{CanvasId, canvas_object};
use crate::canvas::{Axis, element_dimension, element_size, write_dimension};
use crate::state::shared;
use verve::native::{self, arg, arg_text, get, js_text, text_of};

pub const CANVAS_ACCESSORS: &[Accessor] = &[
    (
        "width",
        Some(|this, _, cx| element_dimension(this, cx, Axis::Width)),
        Some(|this, args, cx| set_size(this, args, cx, Axis::Width)),
    ),
    (
        "height",
        Some(|this, _, cx| element_dimension(this, cx, Axis::Height)),
        Some(|this, args, cx| set_size(this, args, cx, Axis::Height)),
    ),
];

pub const CANVAS_METHODS: &[Method] = &[
    ("getContext", 1, get_context),
    ("toDataURL", 0, to_data_url),
    ("toBlob", 1, to_blob),
    ("captureStream", 0, |_, _, cx| Ok(native::object(cx).into())),
    ("transferControlToOffscreen", 0, |_, _, cx| {
        Err(dom_error(
            cx,
            "transferControlToOffscreen is not supported by Verve",
            "NotSupportedError",
        ))
    }),
];

/// Setting `width` or `height` writes the attribute, and empties the canvas that draws into it.
fn set_size(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    axis: Axis,
) -> JsResult<JsValue> {
    let (node, value) = write_dimension(this, args, context, axis)?;
    let state = shared(context);
    let mut state = state.borrow_mut();
    let store = &mut state.canvas;
    if let Some(canvas) = store
        .elements
        .get(&node)
        .and_then(|id| store.canvases.get_mut(id))
    {
        canvas.resize_side(axis, value);
    }
    Ok(JsValue::undefined())
}

/// The canvas behind an element, made on first use with the size its attributes give.
fn canvas_of(this: &JsValue, context: &mut Context, alpha: bool) -> JsResult<CanvasId> {
    let node = node_of(this)?;
    let (width, height) = read_dom(context, |dom| element_size(dom, node));
    let state = shared(context);
    let mut state = state.borrow_mut();
    if let Some(id) = state.canvas.elements.get(&node) {
        return Ok(*id);
    }
    let canvas = Canvas2d::new(CanvasOwner::Element(node), width, height, alpha);
    let id = state.canvas.add_canvas(canvas);
    state.canvas.elements.insert(node, id);
    Ok(id)
}

/// `getContext(kind, options)`: a 2D context, a bitmap renderer, or `null` for the kinds that
/// need a GPU, as on a machine without one: libraries fall back.
fn get_context(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = arg_text(args, 0, context)?;
    let options = arg(args, 1);
    let class = match kind.as_str() {
        "2d" => (Kind::Context2d, "CanvasRenderingContext2D"),
        "bitmaprenderer" => (Kind::BitmapRenderer, "ImageBitmapRenderingContext"),
        _ => return Ok(JsValue::null()),
    };
    let alpha = flag_option(&options, "alpha", true, context)?;
    let id = canvas_of(this, context, alpha)?;
    if kind == "2d" {
        let frequently = flag_option(&options, "willReadFrequently", false, context)?;
        if let Some(canvas) = shared(context).borrow_mut().canvas.canvases.get_mut(&id) {
            canvas.will_read_frequently = frequently;
        }
    }
    Ok(canvas_object(context, id, class).into())
}

/// A boolean member of an options dictionary, `default` when absent.
fn flag_option(
    options: &JsValue,
    name: &str,
    default: bool,
    context: &mut Context,
) -> JsResult<bool> {
    if !options.is_object() {
        return Ok(default);
    }
    let value = get(options, name, context)?;
    Ok(value.is_undefined() && default || value.to_boolean())
}

/// `toDataURL`'s and `toBlob`'s type and quality arguments.
pub fn export_arguments(
    kind: &JsValue,
    quality: &JsValue,
    context: &mut Context,
) -> JsResult<(ImageType, f32)> {
    let kind = match kind.is_string() {
        true => ImageType::from_mime(&text_of(kind, context)?),
        false => ImageType::Png,
    };
    let quality = match quality.is_number() {
        true => quality.to_number(context)? as f32,
        false => DEFAULT_JPEG_QUALITY,
    };
    Ok((kind, quality))
}

/// The pixels a canvas exports. A canvas no context was asked of exports blank at its size; a
/// tainted one refuses with a `SecurityError`; one without pixels exports nothing.
pub fn exported_pixmap(
    id: Option<CanvasId>,
    blank_size: (u32, u32),
    (method, interface): (&str, &str),
    context: &mut Context,
) -> JsResult<Option<Pixmap>> {
    let Some(id) = id else {
        return Ok(Pixmap::new(blank_size.0, blank_size.1));
    };
    // The borrow ends before an error is built, which may run script.
    let (tainted, pixels) = {
        let state = shared(context);
        let state = state.borrow();
        let canvas = state.canvas.canvases.get(&id);
        let tainted = canvas.is_some_and(|canvas| canvas.tainted);
        (tainted, canvas.and_then(|canvas| canvas.pixmap.clone()))
    };
    if tainted {
        let message = format!(
            "Failed to execute '{method}' on '{interface}': Tainted canvases may not be exported."
        );
        return Err(dom_error(context, &message, "SecurityError"));
    }
    Ok(pixels)
}

/// The canvas an element draws into and its size, for exporting.
fn exported_element(
    this: &JsValue,
    context: &mut Context,
    method: &str,
) -> JsResult<Option<Pixmap>> {
    let node = node_of(this)?;
    let id = shared(context).borrow().canvas.elements.get(&node).copied();
    let size = read_dom(context, |dom| element_size(dom, node));
    exported_pixmap(id, size, (method, "HTMLCanvasElement"), context)
}

fn to_data_url(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (kind, quality) = export_arguments(&arg(args, 0), &arg(args, 1), context)?;
    let encoded = exported_element(this, context, "toDataURL")?
        .and_then(|pixmap| encode(&pixmap, kind, quality));
    Ok(match encoded {
        Some(Encoded { mime, bytes }) => js_text(&format!(
            "data:{mime};base64,{}",
            verve::encoding::base64(&bytes, false)
        )),
        None => js_text("data:,"),
    })
}

/// `toBlob(callback, type, quality)`: the callback is called as a task of its own, with the
/// blob, or `null` when there is nothing to export.
fn to_blob(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = arg(args, 0);
    let (kind, quality) = export_arguments(&arg(args, 1), &arg(args, 2), context)?;
    let encoded = exported_element(this, context, "toBlob")?
        .and_then(|pixmap| encode(&pixmap, kind, quality));
    let blob = match encoded {
        Some(Encoded { mime, bytes }) => verve::blob::new_blob(bytes, mime, context)?,
        None => JsValue::null(),
    };
    if callback.is_callable() {
        verve::set_timeout(context, callback, 0., false, vec![blob]);
    }
    Ok(JsValue::undefined())
}
