//! The pixels of an Electron `nativeImage`, apart from the JavaScript object that wraps them.
//!
//! An image is one or more representations of the same picture at different scale factors
//! (`logo.png` with `logo@2x.png`), kept as non-premultiplied RGBA8. Chromium's bitmaps are
//! premultiplied BGRA on Windows and Linux (RGBA on macOS), so `toBitmap` converts on the way out
//! and `createFromBitmap` on the way in. It crosses threads as an `Arc`: the tray, the window
//! icon, a notification and a dialog's icon are drawn on the shell's thread.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::imageops::FilterType;
use image::{ImageFormat, RgbaImage};
use verve_dom::canvas::{premultiply, unpremultiply};

/// The scale factors Chromium looks for beside `logo.png`: `logo@2x.png`, `logo@1.5x.png`.
const SIBLING_SCALES: [f32; 6] = [1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// One picture at one scale factor.
#[derive(Clone, Debug, PartialEq)]
pub struct Representation {
    /// Device pixels per DIP.
    pub scale: f32,
    /// The width in pixels.
    pub width: u32,
    /// The height in pixels.
    pub height: u32,
    /// `width * height` pixels of non-premultiplied RGBA.
    pub rgba: Arc<[u8]>,
}

/// How hard `resize` works at the picture, by Electron's names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    #[default]
    Good,
    Better,
    Best,
}

impl Quality {
    fn filter(self) -> FilterType {
        match self {
            Quality::Good => FilterType::Triangle,
            Quality::Better => FilterType::CatmullRom,
            Quality::Best => FilterType::Lanczos3,
        }
    }
}

/// A `nativeImage`: nothing, or a picture at some scale factors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NativeImage {
    reps: Vec<Representation>,
    template: bool,
}

impl NativeImage {
    /// `nativeImage.createEmpty()`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// An image of one representation.
    pub fn from_rep(rep: Representation) -> Self {
        Self {
            reps: vec![rep],
            template: false,
        }
    }

    /// `createFromBuffer` / `createFromDataURL`: PNG, JPEG, BMP, ICO, GIF or WebP bytes at scale
    /// factor `scale`; empty when they are none of those.
    pub fn from_encoded(bytes: &[u8], scale: f32) -> Self {
        decode(bytes, scale).map_or_else(Self::empty, Self::from_rep)
    }

    /// `createFromBitmap`: premultiplied BGRA (RGBA on macOS) of `width` x `height` at `scale`;
    /// `None` when `bitmap` is not that many bytes.
    pub fn from_bitmap(bitmap: &[u8], width: u32, height: u32, scale: f32) -> Option<Self> {
        let size = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (size == bitmap.len()).then(|| {
            let mut rgba = bitmap.to_vec();
            for pixel in rgba.chunks_exact_mut(4) {
                from_native_order(pixel);
            }
            unpremultiply(&mut rgba);
            Self::from_rep(Representation {
                scale,
                width,
                height,
                rgba: rgba.into(),
            })
        })
    }

    /// `createFromPath`: the file at its own scale factor (`@2x` in its name says it) and, when
    /// the name says none, the files beside it that have one: Chromium's rule for icon sets.
    pub fn from_path(path: &Path) -> Self {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        if let Some(scale) = scale_of(stem) {
            return Self::from_file(path, scale);
        }
        let mut image = Self::from_file(path, 1.0);
        for scale in SIBLING_SCALES {
            let sibling = Self::from_file(&sibling_path(path, stem, scale), scale);
            image.reps.extend(sibling.reps);
        }
        image
    }

    fn from_file(path: &Path, scale: f32) -> Self {
        std::fs::read(path)
            .map_or_else(|_| Self::empty(), |bytes| Self::from_encoded(&bytes, scale))
    }

    /// Whether there is no picture.
    pub fn is_empty(&self) -> bool {
        self.reps.is_empty()
    }

    /// The scale factors it has a representation for, ascending.
    pub fn scale_factors(&self) -> Vec<f32> {
        let mut scales: Vec<f32> = self.reps.iter().map(|rep| rep.scale).collect();
        scales.sort_by(f32::total_cmp);
        scales
    }

    /// The size in DIPs: that of the first representation, whatever scale factor is asked for.
    pub fn size(&self) -> (u32, u32) {
        self.reps.first().map_or((0, 0), |rep| {
            let dip = |pixels: u32| (pixels as f32 / rep.scale).round() as u32;
            (dip(rep.width), dip(rep.height))
        })
    }

    /// The representation for `scale`: the one with that scale factor, else the nearest, the
    /// larger of two equally near.
    pub fn representation(&self, scale: f32) -> Option<&Representation> {
        self.reps.iter().min_by(|a, b| {
            let distance = |rep: &Representation| (rep.scale - scale).abs();
            distance(a)
                .total_cmp(&distance(b))
                .then(b.scale.total_cmp(&a.scale))
        })
    }

    /// `addRepresentation`: replaces the representation at the same scale factor.
    pub fn add(&mut self, rep: Representation) {
        self.reps.retain(|have| have.scale != rep.scale);
        self.reps.push(rep);
    }

    /// `setTemplateImage`.
    pub fn set_template(&mut self, template: bool) {
        self.template = template;
    }

    /// `isTemplateImage`: only a macOS menu bar has the idea, so only it keeps the flag.
    pub fn is_template(&self) -> bool {
        cfg!(target_os = "macos") && self.template
    }

    /// `toPNG`; no bytes for an empty image.
    pub fn to_png(&self, scale: f32) -> Vec<u8> {
        self.encode(scale)
    }

    /// `toJPEG`: the alpha channel is dropped, as JPEG has none.
    pub fn to_jpeg(&self, quality: u8) -> Vec<u8> {
        let Some(rep) = self.representation(1.0) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let rgb = image::DynamicImage::ImageRgba8(rgba_image(rep)).to_rgb8();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        match rgb.write_with_encoder(encoder) {
            Ok(()) => out,
            Err(_) => Vec::new(),
        }
    }

    fn encode(&self, scale: f32) -> Vec<u8> {
        let Some(rep) = self.representation(scale) else {
            return Vec::new();
        };
        let mut out = Cursor::new(Vec::new());
        match rgba_image(rep).write_to(&mut out, ImageFormat::Png) {
            Ok(()) => out.into_inner(),
            Err(_) => Vec::new(),
        }
    }

    /// `toBitmap`: the representation for `scale` as premultiplied BGRA (RGBA on macOS).
    pub fn to_bitmap(&self, scale: f32) -> Vec<u8> {
        let Some(rep) = self.representation(scale) else {
            return Vec::new();
        };
        let mut bitmap = rep.rgba.to_vec();
        premultiply(&mut bitmap);
        for pixel in bitmap.chunks_exact_mut(4) {
            to_native_order(pixel);
        }
        bitmap
    }

    /// `toDataURL`: the PNG of the representation for `scale`.
    pub fn to_data_url(&self, scale: f32) -> String {
        let png = self.to_png(scale);
        format!(
            "data:image/png;base64,{}",
            verve::encoding::base64(&png, false)
        )
    }

    /// `resize`: one representation at `width` x `height` DIPs, made from the largest picture.
    pub fn resize(&self, width: u32, height: u32, quality: Quality) -> Self {
        let Some(rep) = self.reps.iter().max_by(|a, b| a.scale.total_cmp(&b.scale)) else {
            return Self::empty();
        };
        if width == 0 || height == 0 {
            return Self::empty();
        }
        let resized = image::imageops::resize(&rgba_image(rep), width, height, quality.filter());
        let mut image = Self::from_rep(Representation {
            scale: 1.0,
            width,
            height,
            rgba: resized.into_raw().into(),
        });
        image.template = self.template;
        image
    }

    /// `crop`: the part of every representation inside the rectangle, in DIPs; what lies outside
    /// the image is left out.
    pub fn crop(&self, x: u32, y: u32, width: u32, height: u32) -> Self {
        let reps = self.reps.iter().filter_map(|rep| {
            let at = |dip: u32| (dip as f32 * rep.scale).round() as u32;
            let (left, top) = (at(x).min(rep.width), at(y).min(rep.height));
            let (w, h) = (
                at(width).min(rep.width - left),
                at(height).min(rep.height - top),
            );
            let part = image::imageops::crop_imm(&rgba_image(rep), left, top, w, h).to_image();
            (w > 0 && h > 0).then(|| Representation {
                scale: rep.scale,
                width: w,
                height: h,
                rgba: part.into_raw().into(),
            })
        });
        Self {
            reps: reps.collect(),
            template: self.template,
        }
    }
}

/// The picture in `bytes`, whichever of the formats Chromium reads it is.
pub fn decode(bytes: &[u8], scale: f32) -> Option<Representation> {
    let picture = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = picture.dimensions();
    (width > 0 && height > 0).then(|| Representation {
        scale,
        width,
        height,
        rgba: picture.into_raw().into(),
    })
}

/// A raw bitmap of `rgba` for the `image` crate to work on.
fn rgba_image(rep: &Representation) -> RgbaImage {
    RgbaImage::from_raw(rep.width, rep.height, rep.rgba.to_vec())
        .expect("a representation holds width * height pixels")
}

/// The scale factor a file name says it is for: `logo@2x` is 2.
fn scale_of(stem: &str) -> Option<f32> {
    let (_, tail) = stem.rsplit_once('@')?;
    tail.strip_suffix('x')?
        .parse()
        .ok()
        .filter(|scale| *scale > 0.0)
}

/// `logo@2x.png` for `logo.png`.
fn sibling_path(path: &Path, stem: &str, scale: f32) -> PathBuf {
    let mut name = format!("{stem}@{scale}x");
    if let Some(extension) = path.extension() {
        name = format!("{name}.{}", extension.to_string_lossy());
    }
    path.with_file_name(name)
}

/// RGBA to the byte order of Chromium's bitmaps on this platform, and back: BGRA, except on macOS.
fn to_native_order(pixel: &mut [u8]) {
    if !cfg!(target_os = "macos") {
        pixel.swap(0, 2);
    }
}

fn from_native_order(pixel: &mut [u8]) {
    to_native_order(pixel);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, scale: f32, value: u8) -> Representation {
        Representation {
            scale,
            width,
            height,
            rgba: vec![value; (width * height * 4) as usize].into(),
        }
    }

    #[test]
    fn a_bitmap_is_premultiplied_bgra_and_comes_back_as_it_was() {
        let rgba = [255, 0, 0, 128];
        let image = NativeImage::from_rep(Representation {
            scale: 1.0,
            width: 1,
            height: 1,
            rgba: rgba.to_vec().into(),
        });
        let bitmap = image.to_bitmap(1.0);
        assert_eq!(
            bitmap,
            if cfg!(target_os = "macos") {
                [128, 0, 0, 128]
            } else {
                [0, 0, 128, 128]
            }
        );
        assert_eq!(NativeImage::from_bitmap(&bitmap, 1, 1, 1.0), Some(image));
    }

    #[test]
    fn the_representation_nearest_the_scale_is_used_and_the_larger_wins_a_tie() {
        let mut image = NativeImage::from_rep(solid(4, 2, 1.0, 1));
        image.add(solid(8, 4, 2.0, 2));
        assert_eq!(image.scale_factors(), [1.0, 2.0]);
        assert_eq!(image.size(), (4, 2));
        assert_eq!(image.representation(2.0).map(|rep| rep.width), Some(8));
        assert_eq!(image.representation(1.5).map(|rep| rep.width), Some(8));
        assert_eq!(image.representation(1.2).map(|rep| rep.width), Some(4));
    }

    #[test]
    fn a_file_name_says_its_scale_factor() {
        assert_eq!(scale_of("logo@2x"), Some(2.0));
        assert_eq!(scale_of("logo@1.5x"), Some(1.5));
        assert_eq!(scale_of("logo"), None);
        assert_eq!(scale_of("user@example"), None);
    }

    #[test]
    fn crop_and_resize_work_in_dips() {
        let image = NativeImage::from_rep(solid(4, 2, 1.0, 9));
        assert_eq!(image.crop(3, 1, 5, 5).size(), (1, 1));
        assert!(image.crop(10, 10, 5, 5).is_empty());
        assert_eq!(image.resize(8, 4, Quality::Best).size(), (8, 4));
    }

    #[test]
    fn png_and_jpeg_read_back_as_pictures() {
        let image = NativeImage::from_rep(solid(4, 2, 1.0, 200));
        let png = NativeImage::from_encoded(&image.to_png(1.0), 1.0);
        assert_eq!(png, image);
        let jpeg = NativeImage::from_encoded(&image.to_jpeg(90), 1.0);
        assert_eq!(jpeg.size(), (4, 2));
        assert!(NativeImage::empty().to_png(1.0).is_empty());
    }
}
