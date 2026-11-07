//! The pictures of rasterised gradients a window has given GPUI.
//!
//! A gradient the GPUI painter has no shader for ([`verve_layout::RasterSpec`]) is drawn here, once
//! for the window's scale, into straight-alpha BGRA, which is what GPUI's sprite pipeline blends.
//! The picture of a box that is faded is a copy with only its alpha channel scaled; a picture keeps
//! one such copy, for the opacity it was last drawn at. A picture that a frame did not draw is
//! given up at the end of that frame, and what is given up reaches GPUI's atlas a frame later
//! ([`Pictures::take_retired`]), when no scene refers to it any more.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use gpui::RenderImage;
use verve_layout::{ImageKey, RasterSpec};

use crate::raster;

/// The pictures of one window.
#[derive(Default)]
pub(crate) struct Pictures {
    by_gradient: HashMap<(ImageKey, u32), Picture>,
    retired: Vec<Arc<RenderImage>>,
}

struct Picture {
    plain: Arc<RenderImage>,
    /// The copy for an opacity below 1, and the opacity's bits.
    faded: Option<(u32, Arc<RenderImage>)>,
    drawn: bool,
}

impl Pictures {
    /// The picture of `spec` for a window at `scale`, shown at `opacity`.
    pub(crate) fn get(
        &mut self,
        spec: &RasterSpec,
        scale: f32,
        opacity: f32,
    ) -> Option<Arc<RenderImage>> {
        let id = (spec.key, scale as u32);
        let picture = match self.by_gradient.entry(id) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => slot.insert(Picture {
                plain: Arc::new(draw(spec, scale)?),
                faded: None,
                drawn: false,
            }),
        };
        picture.drawn = true;
        if opacity >= 1. {
            return Some(Arc::clone(&picture.plain));
        }
        let bits = opacity.to_bits();
        if let Some((seen, faded)) = &picture.faded
            && *seen == bits
        {
            return Some(Arc::clone(faded));
        }
        let faded = Arc::new(fade(&picture.plain, opacity)?);
        let old = picture.faded.replace((bits, Arc::clone(&faded)));
        self.retired.extend(old.map(|(_, old)| old));
        Some(faded)
    }

    /// The frame was drawn: the pictures it did not draw are given up.
    pub(crate) fn end_frame(&mut self) {
        let retired = &mut self.retired;
        self.by_gradient.retain(|_, picture| {
            let keep = std::mem::take(&mut picture.drawn);
            if !keep {
                retired.push(Arc::clone(&picture.plain));
                retired.extend(picture.faded.take().map(|(_, faded)| faded));
            }
            keep
        });
    }

    /// The pictures given up before this frame, for it to give back to GPUI.
    pub(crate) fn take_retired(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }
}

/// `spec` drawn at `scale`, as straight-alpha BGRA.
fn draw(spec: &RasterSpec, scale: f32) -> Option<RenderImage> {
    let drawn = raster::rasterize(spec, scale);
    let mut bytes = drawn.rgba;
    // The raster is premultiplied, as CSS mixes colors; the sprite pipeline wants straight alpha.
    verve_dom::canvas::unpremultiply(&mut bytes);
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(drawn.width, drawn.height, bytes)?;
    Some(RenderImage::new(vec![image::Frame::new(image)]))
}

/// A copy of `plain` that shows through at `opacity`: only the alpha channel changes.
fn fade(plain: &RenderImage, opacity: f32) -> Option<RenderImage> {
    let size = plain.size(0);
    let mut bytes = plain.as_bytes(0)?.to_vec();
    for pixel in bytes.chunks_exact_mut(4) {
        pixel[3] = (f32::from(pixel[3]) * opacity).round() as u8;
    }
    let image = image::RgbaImage::from_raw(
        u32::try_from(size.width.0).ok()?,
        u32::try_from(size.height.0).ok()?,
        bytes,
    )?;
    Some(RenderImage::new(vec![image::Frame::new(image)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use verve_layout::{ColorStop, Gradient, LinearDirection};

    fn spec() -> RasterSpec {
        let stop = |color| ColorStop {
            color,
            position: None,
        };
        RasterSpec {
            key: ImageKey(1 << 63),
            gradient: Gradient::Linear {
                direction: LinearDirection::Angle(90.),
                stops: vec![stop([1., 0., 0., 0.5]), stop([0., 0., 1., 0.5])],
                repeating: false,
            },
            size: [4., 2.],
        }
    }

    fn bytes(picture: &RenderImage) -> Vec<u8> {
        picture.as_bytes(0).expect("pixels").to_vec()
    }

    #[test]
    fn a_translucent_gradient_is_straight_alpha_and_fading_scales_only_alpha() {
        let mut pictures = Pictures::default();
        let plain = pictures.get(&spec(), 1., 1.).expect("a picture");
        let first = bytes(&plain)[..4].to_vec();
        // BGRA of a half-transparent, almost red pixel: its color is not darkened by its alpha.
        assert!(first[2] > 200 && first[0] < 60, "{first:?}");
        assert!((120..=135).contains(&first[3]), "{first:?}");
        let faded = pictures.get(&spec(), 1., 0.5).expect("a picture");
        let second = bytes(&faded)[..4].to_vec();
        assert_eq!(second[..3], first[..3]);
        assert_eq!(
            u32::from(second[3]),
            (f32::from(first[3]) * 0.5).round() as u32
        );
    }

    #[test]
    fn a_faded_copy_is_kept_for_one_opacity_and_a_picture_not_drawn_is_given_up() {
        let mut pictures = Pictures::default();
        let a = pictures.get(&spec(), 1., 0.5).expect("a picture");
        let b = pictures.get(&spec(), 1., 0.5).expect("a picture");
        assert!(Arc::ptr_eq(&a, &b));
        let c = pictures.get(&spec(), 1., 0.25).expect("a picture");
        assert!(!Arc::ptr_eq(&a, &c));
        // The copy for 0.5 is given up at once (a frame later it reaches the atlas).
        assert_eq!(pictures.take_retired().len(), 1);
        // A frame that draws nothing gives up the picture and its copy.
        pictures.end_frame();
        pictures.end_frame();
        assert_eq!(pictures.take_retired().len(), 2);
        assert!(pictures.by_gradient.is_empty());
    }
}
