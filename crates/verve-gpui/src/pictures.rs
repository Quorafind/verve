//! The pictures a window has given GPUI: rasterised gradients and the pictures a page loaded.
//!
//! The pictures a page loaded come from the renderer's [`ImageTable`] (decoded, straight-alpha
//! BGRA) and are made into a `RenderImage` the first time a frame draws them; a picture the table
//! replaces with another is made again, and one that leaves the table, or that a frame did not
//! draw, is given up as a gradient is.
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
use verve_layout::{ImageFrames, ImageKey, ImageTable, RasterSpec};

use crate::raster;

/// The pictures of one window.
#[derive(Default)]
pub(crate) struct Pictures {
    by_gradient: HashMap<(ImageKey, u32), Picture>,
    by_page: HashMap<ImageKey, (Arc<ImageFrames>, Picture)>,
    table: Arc<ImageTable>,
    retired: Vec<Arc<RenderImage>>,
}

struct Picture {
    plain: Arc<RenderImage>,
    /// The copy for an opacity below 1, and the opacity's bits.
    faded: Option<(u32, Arc<RenderImage>)>,
    drawn: bool,
    /// How many frames in a row have not drawn it.
    idle: u32,
}

/// How many frames a page picture that is not drawn is kept for: scrolling it out of the window
/// and back must not make the window copy and upload its pixels again.
const GRACE: u32 = 300;

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
                idle: 0,
            }),
        };
        shown(picture, opacity, &mut self.retired)
    }

    /// The pictures the page has decoded, as of now.
    pub(crate) fn set_table(&mut self, table: Arc<ImageTable>) {
        self.table = table;
    }

    /// The picture the page loaded under `key`, shown at `opacity`; `None` before it is decoded.
    pub(crate) fn image(&mut self, key: ImageKey, opacity: f32) -> Option<Arc<RenderImage>> {
        let frames = Arc::clone(self.table.get(key)?);
        let stale = self
            .by_page
            .get(&key)
            .is_some_and(|(seen, _)| !Arc::ptr_eq(seen, &frames));
        if stale && let Some((_, mut old)) = self.by_page.remove(&key) {
            old.give_up(&mut self.retired);
        }
        let entry = match self.by_page.entry(key) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                let plain = Arc::new(page_picture(&frames)?);
                slot.insert((
                    frames,
                    Picture {
                        plain,
                        faded: None,
                        drawn: false,
                        idle: 0,
                    },
                ))
            }
        };
        shown(&mut entry.1, opacity, &mut self.retired)
    }

    /// The frame was drawn: the pictures it did not draw are given up.
    pub(crate) fn end_frame(&mut self) {
        let retired = &mut self.retired;
        self.by_gradient
            .retain(|_, picture| picture.keep(retired, 0));
        self.by_page
            .retain(|_, (_, picture)| picture.keep(retired, GRACE));
    }

    /// The pictures given up before this frame, for it to give back to GPUI.
    pub(crate) fn take_retired(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.retired)
    }
}

impl Picture {
    /// Whether the picture is kept: a frame drew it, or fewer than `grace` frames have not.
    fn keep(&mut self, retired: &mut Vec<Arc<RenderImage>>, grace: u32) -> bool {
        if std::mem::take(&mut self.drawn) {
            self.idle = 0;
            return true;
        }
        self.idle += 1;
        let keep = self.idle <= grace;
        if !keep {
            self.give_up(retired);
        }
        keep
    }

    fn give_up(&mut self, retired: &mut Vec<Arc<RenderImage>>) {
        retired.push(Arc::clone(&self.plain));
        retired.extend(self.faded.take().map(|(_, faded)| faded));
    }
}

/// The picture ready to draw at `opacity`: the plain one, or the copy for that opacity.
fn shown(
    picture: &mut Picture,
    opacity: f32,
    retired: &mut Vec<Arc<RenderImage>>,
) -> Option<Arc<RenderImage>> {
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
    retired.extend(old.map(|(_, old)| old));
    Some(faded)
}

/// What the page decoded, as a GPUI picture with all its frames.
fn page_picture(frames: &ImageFrames) -> Option<RenderImage> {
    let all: Option<Vec<image::Frame>> = frames
        .frames
        .iter()
        .map(|(bytes, _)| {
            let image = image::RgbaImage::from_raw(frames.width, frames.height, bytes.to_vec())?;
            Some(image::Frame::new(image))
        })
        .collect();
    let all = all.filter(|all| !all.is_empty())?;
    Some(RenderImage::new(all))
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
    let mut faded = Vec::new();
    for at in 0..plain.frame_count() {
        let size = plain.size(at);
        let mut bytes = plain.as_bytes(at)?.to_vec();
        for pixel in bytes.chunks_exact_mut(4) {
            pixel[3] = (f32::from(pixel[3]) * opacity).round() as u8;
        }
        let image = image::RgbaImage::from_raw(
            u32::try_from(size.width.0).ok()?,
            u32::try_from(size.height.0).ok()?,
            bytes,
        )?;
        faded.push(image::Frame::from_parts(
            image,
            0,
            0,
            image::Delay::from_numer_denom_ms(100, 1),
        ));
    }
    Some(RenderImage::new(faded))
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

    fn table_of(color: u8) -> Arc<ImageTable> {
        let bytes: Arc<[u8]> = Arc::from(vec![color, 0, 0, 255]);
        let frames = ImageFrames {
            width: 1,
            height: 1,
            frames: vec![(bytes, std::time::Duration::ZERO)],
        };
        Arc::new(ImageTable::new([(ImageKey(3), Arc::new(frames))].into()))
    }

    #[test]
    fn a_page_picture_is_made_once_remade_when_replaced_and_given_up_when_not_drawn() {
        let mut pictures = Pictures::default();
        assert!(pictures.image(ImageKey(3), 1.).is_none());
        pictures.set_table(table_of(1));
        let first = pictures.image(ImageKey(3), 1.).expect("a picture");
        let again = pictures.image(ImageKey(3), 1.).expect("a picture");
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(first.frame_count(), 1);
        // The table replaced the picture: the old one is given up, the new one is made.
        pictures.set_table(table_of(2));
        let next = pictures.image(ImageKey(3), 1.).expect("a picture");
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(pictures.take_retired().len(), 1);
        pictures.end_frame();
        // Drawn once more within the grace period it is the same picture.
        assert!(Arc::ptr_eq(
            &next,
            &pictures.image(ImageKey(3), 1.).unwrap()
        ));
        for _ in 0..=GRACE {
            pictures.end_frame();
        }
        assert!(pictures.take_retired().is_empty());
        pictures.end_frame();
        assert_eq!(pictures.take_retired().len(), 1);
        assert!(pictures.by_page.is_empty());
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
