//! Pixels at the script boundary: `ImageData` (straight, non-premultiplied RGBA, as the
//! interface is) against the canvas's premultiplied ones, and the PNG and JPEG a canvas exports.

use ::image::ImageEncoder;
use boa_engine::object::builtins::JsUint8ClampedArray;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use tiny_skia::Pixmap;

use crate::bind::{Kind, handle_instance, native_instance, set};
use crate::canvas::context2d::Canvas2d;
use crate::canvas::image::{premultiply, unpremultiply};
use verve::native::{arg, get, js_text};

/// An `ImageData` object of a size and straight RGBA bytes.
pub fn make_image_data(
    context: &mut Context,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
) -> JsResult<JsValue> {
    let object = native_instance(context, "ImageData", Kind::Plain, 0);
    let data = JsUint8ClampedArray::from_iter(pixels, context)?;
    fill_image_data(&object, width, height, data, context)?;
    Ok(object.into())
}

fn fill_image_data(
    object: &JsObject,
    width: u32,
    height: u32,
    pixels: impl Into<JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    let members = [
        ("width", JsValue::from(width)),
        ("height", JsValue::from(height)),
        ("data", pixels.into()),
        ("colorSpace", js_text("srgb")),
    ];
    for (name, value) in members {
        set(object, name, value, context)?;
    }
    Ok(())
}

/// The bytes of a blank `ImageData` of a size.
pub fn blank_bytes(width: u32, height: u32) -> Vec<u8> {
    vec![0; width as usize * height as usize * 4]
}

/// An `ImageData`'s size and straight RGBA bytes, padded or cut to its size.
pub fn image_data_pixels(
    value: &JsValue,
    context: &mut Context,
) -> JsResult<Option<(u32, u32, Vec<u8>)>> {
    if !value.is_object() {
        return Ok(None);
    }
    let width = get(value, "width", context)?.to_number(context)?;
    let height = get(value, "height", context)?.to_number(context)?;
    if !(width.is_finite() && height.is_finite() && width > 0. && height > 0.) {
        return Ok(None);
    }
    let (width, height) = (width as u32, height as u32);
    let data = get(value, "data", context)?;
    let mut pixels = verve::encoding::bytes_of(&data, context)?.unwrap_or_default();
    pixels.resize(width as usize * height as usize * 4, 0);
    Ok(Some((width, height, pixels)))
}

/// `new ImageData(data, width, height?)` and `new ImageData(width, height)`.
pub fn construct_image_data(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let first = arg(args, 0);
    let (data, width, height) = match first.is_object() {
        true => {
            let width = arg(args, 1).to_u32(context)?;
            let height = match arg(args, 2) {
                value if value.is_undefined() => {
                    let length = get(&first, "length", context)?.to_u32(context)?;
                    length / 4 / width.max(1)
                }
                value => value.to_u32(context)?,
            };
            (first, width, height)
        }
        false => {
            let (width, height) = (first.to_u32(context)?, arg(args, 1).to_u32(context)?);
            let bytes = JsUint8ClampedArray::from_iter(blank_bytes(width, height), context)?;
            (bytes.into(), width, height)
        }
    };
    let object = handle_instance(new_target, Kind::Plain, 0, context)?;
    fill_image_data(&object, width, height, data, context)?;
    Ok(object.into())
}

/// A rectangle of canvas pixels a `getImageData` or `putImageData` names.
#[derive(Clone, Copy)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    /// The region `getImageData(x, y, width, height)` means: coordinates are truncated, and a
    /// negative size extends the region the other way.
    pub fn from_rect([x, y, width, height]: [f32; 4]) -> Region {
        let (x, y, width, height) = (x as i32, y as i32, width as i32, height as i32);
        Region {
            x: x + width.min(0),
            y: y + height.min(0),
            width: width.unsigned_abs(),
            height: height.unsigned_abs(),
        }
    }

    /// The runs of bytes shared by the region's buffer and a canvas of `size`, one per row.
    fn spans(&self, (canvas_width, canvas_height): (u32, u32)) -> Vec<Span> {
        let Some((first, at, count)) = overlap(self.x, self.width, canvas_width) else {
            return Vec::new();
        };
        (0..i64::from(self.height))
            .filter_map(|row| {
                let y = i64::from(self.y) + row;
                (0..i64::from(canvas_height)).contains(&y).then(|| Span {
                    region: (row as usize * self.width as usize + first) * 4,
                    canvas: (y as usize * canvas_width as usize + at) * 4,
                    bytes: count * 4,
                })
            })
            .collect()
    }
}

/// A row's run of pixels in a region's buffer and in the canvas, in bytes.
struct Span {
    region: usize,
    canvas: usize,
    bytes: usize,
}

/// Where `length` pixels from `origin` meet `0..limit`: the offset into the run, the offset in
/// the canvas and the count.
fn overlap(origin: i32, length: u32, limit: u32) -> Option<(usize, usize, usize)> {
    let start = i64::from(origin).max(0);
    let end = (i64::from(origin) + i64::from(length)).min(i64::from(limit));
    (start < end).then(|| {
        let first = (start - i64::from(origin)) as usize;
        (first, start as usize, (end - start) as usize)
    })
}

impl Canvas2d {
    /// The pixels of `region` as straight RGBA; what lies outside the canvas is transparent.
    pub fn read_region(&self, region: Region) -> Vec<u8> {
        let mut out = blank_bytes(region.width, region.height);
        let Some(pixmap) = &self.pixmap else {
            return out;
        };
        for span in region.spans((pixmap.width(), pixmap.height())) {
            let row = &mut out[span.region..span.region + span.bytes];
            row.copy_from_slice(&pixmap.data()[span.canvas..span.canvas + span.bytes]);
            unpremultiply(row);
        }
        out
    }

    /// `putImageData`: straight pixels written as they are, through no transform, clip or blend.
    pub fn write_region(&mut self, region: Region, pixels: &[u8]) {
        let Some(pixmap) = self.pixmap.as_mut() else {
            return;
        };
        let spans = region.spans((pixmap.width(), pixmap.height()));
        for span in spans {
            let from = &pixels[span.region..span.region + span.bytes];
            let row = &mut pixmap.data_mut()[span.canvas..span.canvas + span.bytes];
            row.copy_from_slice(from);
            premultiply(row);
        }
        self.dirty = true;
    }
}

/// An image type a canvas can export. WebP asks for a lossy encoder Verve does not have, so it
/// is PNG, which the specification allows for any type the browser cannot encode.
#[derive(Clone, Copy, PartialEq)]
pub enum ImageType {
    Png,
    Jpeg,
}

impl ImageType {
    pub fn from_mime(mime: &str) -> ImageType {
        match mime.to_ascii_lowercase().as_str() {
            "image/jpeg" => ImageType::Jpeg,
            _ => ImageType::Png,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            ImageType::Png => "image/png",
            ImageType::Jpeg => "image/jpeg",
        }
    }
}

/// The quality `toDataURL('image/jpeg')` uses when none is given.
pub const DEFAULT_JPEG_QUALITY: f32 = 0.92;

/// An exported image: its type and its bytes.
pub struct Encoded {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// `pixmap` as a PNG, or a JPEG of `quality` (0 to 1) over black.
pub fn encode(pixmap: &Pixmap, kind: ImageType, quality: f32) -> Option<Encoded> {
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut bytes = Vec::new();
    match kind {
        ImageType::Jpeg => {
            // Premultiplied pixels are already composited over black, as JPEG needs.
            let rgb: Vec<u8> = pixmap
                .data()
                .chunks_exact(4)
                .flat_map(|pixel| pixel[..3].iter().copied())
                .collect();
            let percent = match quality.is_finite() {
                true => (quality.clamp(0., 1.) * 100.).round() as u8,
                false => (DEFAULT_JPEG_QUALITY * 100.) as u8,
            };
            let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, percent);
            encoder
                .write_image(&rgb, width, height, ::image::ExtendedColorType::Rgb8)
                .ok()?;
        }
        ImageType::Png => {
            let mut rgba = pixmap.data().to_vec();
            unpremultiply(&mut rgba);
            ::image::codecs::png::PngEncoder::new(&mut bytes)
                .write_image(&rgba, width, height, ::image::ExtendedColorType::Rgba8)
                .ok()?;
        }
    }
    Some(Encoded {
        mime: kind.mime(),
        bytes,
    })
}
