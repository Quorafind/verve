//! `nativeImage` and `NativeImage`, as Electron 39 has them on Windows (`tests/electron/system-a`
//! holds what was observed): a picture the main process and a sandbox-free page can read, write
//! and resize, and pass to the other modules of this package.
//!
//! The pixels and their conversions are [`crate::pixels`]; this file is the objects. A
//! `NativeImage` is immutable from a script's side but for `addRepresentation` and
//! `setTemplateImage`, so the object holds the image and every method reads or replaces it.
//! [`make`] makes the module's object for whoever needs it (the main process's
//! `electron` and, for a page with `nodeIntegration`, the page's).

use std::cell::RefCell;

use boa_engine::gc::{Finalize, Trace};
use boa_engine::{Context, JsData, JsObject, JsResult, JsValue};
use verve::encoding::bytes_of;
use verve::native::{self, ClassSpec, arg};

use super::define::{self, Args, Fields};
use crate::pixels::{NativeImage, Quality, Representation, decode};

/// What a `NativeImage` object holds.
#[derive(Trace, Finalize, JsData)]
struct ImageData {
    #[unsafe_ignore_trace]
    image: RefCell<NativeImage>,
}

/// The realm's `NativeImage.prototype`, made the first time an image is.
struct Prototype(JsObject);

/// The `nativeImage` module: its `create…` functions.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("createEmpty", 0, |_, _, cx| wrap(cx, NativeImage::empty())),
            ("createFromPath", 1, create_from_path),
            ("createFromBuffer", 1, create_from_buffer),
            ("createFromDataURL", 1, create_from_data_url),
            ("createFromBitmap", 2, create_from_bitmap),
            ("createFromNamedImage", 1, |_, _, cx| {
                wrap(cx, NativeImage::empty())
            }),
            ("createThumbnailFromPath", 2, |_, _, cx| {
                define::rejected(cx, "nativeImage.createThumbnailFromPath")
            }),
        ],
    )?;
    Ok(module.into())
}

/// A `NativeImage` object for `image`.
///
/// # Errors
///
/// The class could not be made.
pub(crate) fn wrap(context: &mut Context, image: NativeImage) -> JsResult<JsValue> {
    let prototype = prototype(context)?;
    let data = ImageData {
        image: RefCell::new(image),
    };
    Ok(JsObject::from_proto_and_data(prototype, data).into())
}

/// The image a `NativeImage` object holds, when `value` is one.
pub(crate) fn image_of(value: &JsValue) -> Option<NativeImage> {
    let object = value.as_object()?;
    let data = object.downcast_ref::<ImageData>()?;
    Some(data.image.borrow().clone())
}

fn prototype(context: &mut Context) -> JsResult<JsObject> {
    if let Some(Prototype(prototype)) = context.get_data::<Prototype>() {
        return Ok(prototype.clone());
    }
    let (_, prototype) = native::class(
        context,
        ClassSpec {
            name: "NativeImage",
            methods: &[
                ("toPNG", 0, to_png),
                ("toJPEG", 1, to_jpeg),
                ("toBitmap", 0, to_bitmap),
                ("getBitmap", 0, to_bitmap),
                ("toDataURL", 0, to_data_url),
                ("getSize", 0, size),
                ("getAspectRatio", 0, aspect_ratio),
                ("getScaleFactors", 0, scale_factors),
                ("isEmpty", 0, is_empty),
                ("resize", 1, resize),
                ("crop", 1, crop),
                ("addRepresentation", 1, add_representation),
                ("setTemplateImage", 1, set_template),
                ("isTemplateImage", 0, is_template),
            ],
            accessors: &[("isMacTemplateImage", is_template, Some(set_template))],
            ..ClassSpec::default()
        },
    )?;
    context.insert_data(Prototype(prototype.clone()));
    Ok(prototype)
}

/// Runs `f` on the image `this` is; `TypeError: Illegal invocation` for anything else.
fn with_image<T>(this: &JsValue, f: impl FnOnce(&mut NativeImage) -> T) -> JsResult<T> {
    let object = this.as_object();
    let data = object
        .as_ref()
        .and_then(|o| o.downcast_ref::<ImageData>())
        .ok_or_else(|| native::type_error("Illegal invocation"))?;
    let mut image = data.image.borrow_mut();
    Ok(f(&mut image))
}

/// The scale factor an options object asks for: its `scaleFactor`, 1 by default.
fn scale_option(args: &[JsValue], context: &mut Context) -> JsResult<f32> {
    let given = Args::new(args).object_opt(0, "options", context)?;
    Fields::new(given.as_ref()).f32("scaleFactor", 1.0, context)
}

fn create_from_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = Args::new(args).string(0, "path", context)?;
    wrap(context, NativeImage::from_path(path.as_ref()))
}

fn create_from_buffer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let Some(bytes) = buffer_bytes(&arg(args, 0), context)? else {
        return Err(define::error("buffer must be a node Buffer"));
    };
    let given = Args::new(args).object_opt(1, "options", context)?;
    let fields = Fields::new(given.as_ref());
    let scale = fields.f32("scaleFactor", 1.0, context)?;
    let width = fields.f64("width", 0.0, context)?.max(0.0) as u32;
    let height = fields.f64("height", 0.0, context)?.max(0.0) as u32;
    // Encoded pictures are read as they are; with a size given, anything else is a raw bitmap.
    let image = match decode(&bytes, scale) {
        Some(rep) => NativeImage::from_rep(rep),
        None if width > 0 && height > 0 => NativeImage::from_bitmap(&bytes, width, height, scale)
            .unwrap_or_else(NativeImage::empty),
        None => NativeImage::empty(),
    };
    wrap(context, image)
}

fn create_from_data_url(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = Args::new(args).string(0, "dataURL", context)?;
    let image = data_url_bytes(&text).map_or_else(NativeImage::empty, |bytes| {
        NativeImage::from_encoded(&bytes, 1.0)
    });
    wrap(context, image)
}

fn create_from_bitmap(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(2)?;
    let Some(bitmap) = buffer_bytes(&arg(args, 0), context)? else {
        return Err(define::error("buffer must be a node Buffer"));
    };
    let options = Args::new(args).object_opt(1, "options", context)?;
    let fields = Fields::new(options.as_ref());
    let width = fields.f64("width", 0.0, context)?.max(0.0) as u32;
    let height = fields.f64("height", 0.0, context)?.max(0.0) as u32;
    let scale = fields.f32("scaleFactor", 1.0, context)?;
    match NativeImage::from_bitmap(&bitmap, width, height, scale) {
        Some(image) => wrap(context, image),
        None => Err(define::error("invalid buffer size")),
    }
}

/// The bytes of a `Buffer` argument; `None` for anything else.
fn buffer_bytes(value: &JsValue, context: &mut Context) -> JsResult<Option<Vec<u8>>> {
    if value.as_string().is_some() {
        return Ok(None);
    }
    bytes_of(value, context)
}

/// The bytes of a `data:` URL, when `text` is one.
fn data_url_bytes(text: &str) -> Option<Vec<u8>> {
    let url = url::Url::parse(text).ok()?;
    verve::encoding::data_url(&url).ok().map(|(_, bytes)| bytes)
}

fn to_png(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let scale = scale_option(args, context)?;
    let png = with_image(this, |image| image.to_png(scale))?;
    define::buffer_of(context, png)
}

fn to_jpeg(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let quality = Args::new(args)
        .f64(0, "quality", context)?
        .clamp(1.0, 100.0) as u8;
    let jpeg = with_image(this, |image| image.to_jpeg(quality))?;
    define::buffer_of(context, jpeg)
}

fn to_bitmap(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let scale = scale_option(args, context)?;
    let bitmap = with_image(this, |image| image.to_bitmap(scale))?;
    define::buffer_of(context, bitmap)
}

fn to_data_url(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let scale = scale_option(args, context)?;
    let url = with_image(this, |image| image.to_data_url(scale))?;
    Ok(native::js_text(&url))
}

fn size_record(context: &mut Context, (width, height): (u32, u32)) -> JsResult<JsValue> {
    let fields = [("width", width.into()), ("height", height.into())];
    Ok(define::record(context, &fields)?.into())
}

fn size(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let size = with_image(this, |image| image.size())?;
    size_record(context, size)
}

fn aspect_ratio(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (width, height) = with_image(this, |image| image.size())?;
    let ratio = if height == 0 {
        1.0
    } else {
        f64::from(width) / f64::from(height)
    };
    Ok(ratio.into())
}

fn scale_factors(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let scales = with_image(this, |image| image.scale_factors())?;
    let items = scales.into_iter().map(|s| f64::from(s).into()).collect();
    Ok(native::array(context, items))
}

fn is_empty(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(with_image(this, |image| image.is_empty())?.into())
}

fn resize(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let options = Args::new(args).object_opt(0, "options", context)?;
    let fields = Fields::new(options.as_ref());
    let width = fields.f64_opt("width", context)?;
    let height = fields.f64_opt("height", context)?;
    let quality = match fields.one_of("quality", &["good", "better", "best"], context)? {
        Some("best") => Quality::Best,
        Some("better") => Quality::Better,
        _ => Quality::Good,
    };
    let resized = with_image(this, |image| {
        let (w, h) = image.size();
        let (width, height) = match (width, height) {
            (Some(width), Some(height)) => (width, height),
            (Some(width), None) => (width, width * f64::from(h) / f64::from(w.max(1))),
            (None, Some(height)) => (height * f64::from(w) / f64::from(h.max(1)), height),
            (None, None) => (f64::from(w), f64::from(h)),
        };
        image.resize(
            width.round().max(0.0) as u32,
            height.round().max(0.0) as u32,
            quality,
        )
    })?;
    wrap(context, resized)
}

fn crop(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let rect = Args::new(args).object_opt(0, "rect", context)?;
    let fields = Fields::new(rect.as_ref());
    let mut part =
        |name: &str| -> JsResult<u32> { Ok(fields.f64(name, 0.0, context)?.max(0.0) as u32) };
    let (x, y, width, height) = (part("x")?, part("y")?, part("width")?, part("height")?);
    let cropped = with_image(this, |image| image.crop(x, y, width, height))?;
    wrap(context, cropped)
}

fn add_representation(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    Args::new(args).require(1)?;
    let options = Args::new(args).object_opt(0, "options", context)?;
    let fields = Fields::new(options.as_ref());
    let scale = fields.f32("scaleFactor", 1.0, context)?;
    let width = fields.f64("width", 0.0, context)?.max(0.0) as u32;
    let height = fields.f64("height", 0.0, context)?.max(0.0) as u32;
    let buffer = native::get(&arg(args, 0), "buffer", context)?;
    let bytes = buffer_bytes(&buffer, context)?;
    let data_url = fields.string("dataURL", context)?;
    let bytes = bytes.or_else(|| data_url.and_then(|url| data_url_bytes(&url)));
    let Some(bytes) = bytes else {
        return Ok(JsValue::undefined());
    };
    // Encoded pictures are read as they are; anything else is a raw bitmap of the size given.
    let rep = decode(&bytes, scale).or_else(|| {
        NativeImage::from_bitmap(&bytes, width, height, scale)
            .and_then(|image| image.representation(scale).cloned())
    });
    if let Some(rep) = rep {
        with_image(this, |image| image.add(Representation { scale, ..rep }))?;
    }
    Ok(JsValue::undefined())
}

fn set_template(this: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let template = arg(args, 0).to_boolean();
    with_image(this, |image| image.set_template(template))?;
    Ok(JsValue::undefined())
}

fn is_template(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(with_image(this, |image| image.is_template())?.into())
}
