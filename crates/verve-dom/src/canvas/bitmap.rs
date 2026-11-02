//! `ImageBitmap`, `OffscreenCanvas` (on the page's own thread only — a Worker's realm has no
//! document), `createImageBitmap` and `ImageBitmapRenderingContext`.

use std::sync::Arc;

use boa_engine::{Context, JsResult, JsValue};
use tiny_skia::{BlendMode, FilterQuality, IntRect, Pixmap, PixmapPaint, Transform};

use crate::bind::{
    self, Accessor, Kind, Method, arg, arg_string, dom_error, get, handle_of, native_instance,
};
use crate::canvas::context2d::{Canvas2d, CanvasOwner};
use crate::canvas::element::{export_arguments, exported_pixmap};
use crate::canvas::image::{decode, pixmap_of, premultiply};
use crate::canvas::pixels::{Encoded, encode, image_data_pixels};
use crate::canvas::source::source_pixmap;
use crate::canvas::store::{canvas_object, read_canvas, this_handle};
use crate::canvas::{Axis, finite_numbers};
use crate::state::shared;

/// An `ImageBitmap` object holding `pixmap`.
pub fn make_bitmap(context: &mut Context, pixmap: Arc<Pixmap>) -> JsValue {
    let id = shared(context).borrow_mut().canvas.add_bitmap(pixmap);
    native_instance(context, "ImageBitmap", Kind::Bitmap, id).into()
}

pub const BITMAP_METHODS: &[Method] = &[("close", 0, close_bitmap)];

/// `close()`: the bitmap lets go of its pixels and is 0 by 0 from then on.
fn close_bitmap(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Bitmap, "ImageBitmap")?;
    shared(context).borrow_mut().canvas.bitmaps.remove(&id);
    Ok(JsValue::undefined())
}

pub const BITMAP_ACCESSORS: &[Accessor] = &[
    (
        "width",
        Some(|this, _, cx| bitmap_size(this, cx, Axis::Width)),
        None,
    ),
    (
        "height",
        Some(|this, _, cx| bitmap_size(this, cx, Axis::Height)),
        None,
    ),
];

/// A closed bitmap is 0 by 0.
fn bitmap_size(this: &JsValue, context: &mut Context, axis: Axis) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Bitmap, "ImageBitmap")?;
    let state = shared(context);
    let state = state.borrow();
    let size = state.canvas.bitmaps.get(&id).map(|bitmap| match axis {
        Axis::Width => bitmap.width(),
        Axis::Height => bitmap.height(),
    });
    Ok(size.unwrap_or(0).into())
}

// ─── OffscreenCanvas ────────────────────────────────────────────────────────────────────────

pub fn construct_offscreen(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let width = arg(args, 0).to_u32(context)?;
    let height = arg(args, 1).to_u32(context)?;
    let canvas = Canvas2d::new(CanvasOwner::Offscreen, width, height, true);
    let id = shared(context).borrow_mut().canvas.add_canvas(canvas);
    Ok(canvas_object(context, id, (Kind::Offscreen, "OffscreenCanvas")).into())
}

pub const OFFSCREEN_METHODS: &[Method] = &[
    ("getContext", 1, offscreen_context),
    ("convertToBlob", 0, convert_to_blob),
    ("transferToImageBitmap", 0, transfer_to_image_bitmap),
];

pub const OFFSCREEN_ACCESSORS: &[Accessor] = &[
    (
        "width",
        Some(|this, _, cx| offscreen_size(this, cx, Axis::Width)),
        Some(|this, args, cx| set_offscreen_size(this, args, cx, Axis::Width)),
    ),
    (
        "height",
        Some(|this, _, cx| offscreen_size(this, cx, Axis::Height)),
        Some(|this, args, cx| set_offscreen_size(this, args, cx, Axis::Height)),
    ),
];

fn offscreen_size(this: &JsValue, context: &mut Context, axis: Axis) -> JsResult<JsValue> {
    let class = (Kind::Offscreen, "OffscreenCanvas");
    let size = read_canvas(this, context, class, |canvas| match axis {
        Axis::Width => canvas.width,
        Axis::Height => canvas.height,
    })?;
    Ok(size.unwrap_or(0).into())
}

fn set_offscreen_size(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    axis: Axis,
) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Offscreen, "OffscreenCanvas")?;
    let value = arg(args, 0).to_u32(context)?;
    if let Some(canvas) = shared(context).borrow_mut().canvas.canvases.get_mut(&id) {
        canvas.resize_side(axis, value);
    }
    Ok(JsValue::undefined())
}

/// `getContext('2d')`; the other kinds need a GPU.
fn offscreen_context(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Offscreen, "OffscreenCanvas")?;
    Ok(match arg_string(args, 0, context)?.as_str() {
        "2d" => canvas_object(context, id, (Kind::Context2d, "CanvasRenderingContext2D")).into(),
        _ => JsValue::null(),
    })
}

/// `convertToBlob({ type, quality })`: a promise of the blob.
fn convert_to_blob(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Offscreen, "OffscreenCanvas")?;
    let options = arg(args, 0);
    let (kind, quality) = match options.is_object() {
        true => {
            let (kind, quality) = (
                get(&options, "type", context)?,
                get(&options, "quality", context)?,
            );
            export_arguments(&kind, &quality, context)?
        }
        false => export_arguments(&JsValue::undefined(), &JsValue::undefined(), context)?,
    };
    let names = ("convertToBlob", "OffscreenCanvas");
    let pixmap = exported_pixmap(Some(id), (0, 0), names, context);
    let encoded = match pixmap {
        Ok(pixmap) => pixmap.and_then(|pixmap| encode(&pixmap, kind, quality)),
        Err(error) => return bind::rejected(error, context),
    };
    match encoded {
        Some(Encoded { mime, bytes }) => {
            let blob = verve::blob::new_blob(bytes, mime, context)?;
            bind::resolved(blob, context)
        }
        None => {
            let error = dom_error(
                context,
                "The canvas has no pixels to convert.",
                "InvalidStateError",
            );
            bind::rejected(error, context)
        }
    }
}

/// `transferToImageBitmap()`: the pixels move to a bitmap and the canvas is left empty.
fn transfer_to_image_bitmap(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::Offscreen, "OffscreenCanvas")?;
    let moved = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let canvas = state.canvas.canvases.get_mut(&id);
        canvas.and_then(|canvas| {
            let pixels = canvas.pixmap.clone();
            canvas.resize(canvas.width, canvas.height);
            pixels
        })
    };
    Ok(match moved {
        Some(pixmap) => make_bitmap(context, Arc::new(pixmap)),
        None => JsValue::null(),
    })
}

// ─── ImageBitmapRenderingContext ────────────────────────────────────────────────────────────

pub const BITMAP_RENDERER_METHODS: &[Method] =
    &[("transferFromImageBitmap", 1, transfer_from_image_bitmap)];

/// `transferFromImageBitmap(bitmap)`: the canvas takes the bitmap's size and pixels, and the
/// bitmap is detached.
fn transfer_from_image_bitmap(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = this_handle(this, Kind::BitmapRenderer, "ImageBitmapRenderingContext")?;
    let source = handle_of(&arg(args, 0), Kind::Bitmap);
    let state = shared(context);
    let mut state = state.borrow_mut();
    let store = &mut state.canvas;
    let bitmap = source.and_then(|source| store.bitmaps.remove(&source));
    if let (Some(bitmap), Some(canvas)) = (bitmap, store.canvases.get_mut(&id)) {
        canvas.resize(bitmap.width(), bitmap.height());
        if let Some(target) = canvas.pixmap.as_mut() {
            target.data_mut().copy_from_slice(bitmap.data());
        }
    }
    Ok(JsValue::undefined())
}

// ─── createImageBitmap ──────────────────────────────────────────────────────────────────────

/// `createImageBitmap(source[, sx, sy, sw, sh][, options])`: the pixels copied now and the
/// promise settled at once. A `Blob` is decoded where the call is made rather than on a
/// worker.
pub fn create_image_bitmap(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let source = arg(args, 0);
    let (crop, options) = match arg(args, 1).is_number() {
        true => (finite_numbers::<4>(&args[1..], context)?, arg(args, 5)),
        false => (None, arg(args, 1)),
    };
    let Some(mut pixmap) = bitmap_source(&source, context)? else {
        let message = "Failed to execute 'createImageBitmap': the source has no pixels.";
        return bind::rejected(dom_error(context, message, "InvalidStateError"), context);
    };
    if let Some([x, y, width, height]) = crop {
        let rect = IntRect::from_xywh(x as i32, y as i32, width as u32, height as u32);
        pixmap = rect
            .and_then(|rect| pixmap.clone_rect(rect))
            .unwrap_or(pixmap);
    }
    let pixmap = resized(pixmap, &options, context)?;
    bind::resolved(make_bitmap(context, Arc::new(pixmap)), context)
}

/// The pixels of a `Blob` decoded, an `ImageData`, or an image element's or canvas's.
fn bitmap_source(value: &JsValue, context: &mut Context) -> JsResult<Option<Pixmap>> {
    if verve::blob::is_blob(value, context) {
        let bytes = verve::blob::blob_bytes(value, context)?;
        return Ok(decode(&bytes, None)
            .ok()
            .and_then(|image| image.to_pixmap()));
    }
    if let Some(source) = source_pixmap(value, context)? {
        return Ok(Some((*source.pixmap).clone()));
    }
    if !is_image_data(value, context) {
        return Ok(None);
    }
    let Some((width, height, mut pixels)) = image_data_pixels(value, context)? else {
        return Ok(None);
    };
    premultiply(&mut pixels);
    Ok(pixmap_of(width, height, pixels))
}

fn is_image_data(value: &JsValue, context: &mut Context) -> bool {
    let prototype = shared(context).borrow().prototype("ImageData");
    value
        .as_object()
        .and_then(|object| object.prototype())
        .is_some_and(|known| known == prototype)
}

/// `createImageBitmap`'s options: `imageOrientation` and the resize.
fn resized(source: Pixmap, options: &JsValue, context: &mut Context) -> JsResult<Pixmap> {
    if !options.is_object() {
        return Ok(source);
    }
    let flip = get(options, "imageOrientation", context)?;
    let flip = flip.is_string() && bind::string(&flip, context)? == "flipY";
    let (mut width, mut height) = (source.width(), source.height());
    let requested = |name: &str, context: &mut Context| -> JsResult<Option<u32>> {
        let value = get(options, name, context)?;
        Ok(match value.is_undefined() {
            true => None,
            false => Some(value.to_u32(context)?).filter(|size| *size > 0),
        })
    };
    // One side given keeps the aspect ratio.
    match (
        requested("resizeWidth", context)?,
        requested("resizeHeight", context)?,
    ) {
        (Some(w), Some(h)) => (width, height) = (w, h),
        (Some(w), None) => (width, height) = (w, scaled_side(height, w, width)),
        (None, Some(h)) => (width, height) = (scaled_side(width, h, height), h),
        (None, None) if !flip => return Ok(source),
        (None, None) => {}
    }
    Ok(resample(&source, width.max(1), height.max(1), flip))
}

/// `side` scaled by `to / from`, keeping an image's proportions.
fn scaled_side(side: u32, to: u32, from: u32) -> u32 {
    (u64::from(side) * u64::from(to) / u64::from(from.max(1))) as u32
}

/// `source` drawn into a new pixmap of `width` by `height`, upside down when `flip`.
fn resample(source: &Pixmap, width: u32, height: u32, flip: bool) -> Pixmap {
    let mut out =
        Pixmap::new(width, height).unwrap_or_else(|| Pixmap::new(1, 1).expect("one pixel"));
    let scale_x = out.width() as f32 / source.width() as f32;
    let scale_y = out.height() as f32 / source.height() as f32;
    let transform = match flip {
        true => Transform::from_row(scale_x, 0., 0., -scale_y, 0., out.height() as f32),
        false => Transform::from_scale(scale_x, scale_y),
    };
    let paint = PixmapPaint {
        blend_mode: BlendMode::Source,
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    out.draw_pixmap(0, 0, source.as_ref(), &paint, transform, None);
    out
}
