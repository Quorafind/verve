//! The pictures a window has given GPUI: rasterised gradients and the pictures a page loaded.
//!
//! The pictures a page loaded come from the renderer's [`ImageTable`] (decoded, straight-alpha
//! BGRA) and are made into a `RenderImage` the first time a frame draws them. A picture the table
//! replaces with another is made again, and one that leaves the table (the renderer lets go of the
//! ones far from the window once its list is still) is given up at once. One that a frame did not
//! draw is kept, the least recently drawn going first, within a byte budget, so that scrolling
//! back does not upload it again; an SVG drawn at a size no frame draws is given up as a
//! gradient is.
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
use verve_layout::{ImageFrames, ImageKey, ImageTable, RasterSpec, VectorSpec, canvas_slot};

use crate::raster;

/// The pictures of one window.
#[derive(Default)]
pub(crate) struct Pictures {
    by_gradient: HashMap<(ImageKey, u32), Picture>,
    by_page: HashMap<ImageKey, (Arc<ImageFrames>, Picture)>,
    /// SVG pictures by key and the size in pixels they were drawn at.
    by_vector: HashMap<(ImageKey, u32, u32), Picture>,
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

/// How many bytes of pixels the page pictures that a frame did not draw are kept for, the least
/// recently drawn going first: scrolling a picture out of the window and back must not make the
/// window copy and upload its pixels again, but a page of hundreds of pictures must not be held
/// whole.
const IDLE_BYTES: usize = 64 << 20;

/// An SVG is not drawn larger than this many pixels a side, or in more than this many in all.
const MOST_SIDE: u32 = 8192;
const MOST_PIXELS: u64 = 64 << 20;

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
        // What the renderer has let go of (a picture far from the window, once the list was still)
        // is given up here at once, and its pixels with it.
        let gone: Vec<ImageKey> = self
            .by_page
            .keys()
            .filter(|key| table.get(**key).is_none())
            .copied()
            .collect();
        for key in gone {
            if let Some((_, mut picture)) = self.by_page.remove(&key) {
                picture.give_up(&mut self.retired);
            }
        }
        self.table = table;
    }

    /// The picture under `key` for a place `pixels` wide and high (in window pixels), shown at
    /// `opacity`: the pixels the page has for it if it has them, else the drawing in `vectors` or
    /// the table, rasterised at that size.
    pub(crate) fn page(
        &mut self,
        key: ImageKey,
        opacity: f32,
        vectors: &[VectorSpec],
        pixels: [f32; 2],
    ) -> Option<Arc<RenderImage>> {
        if let Some(picture) = self.image(key, opacity) {
            return Some(picture);
        }
        let source = vectors
            .iter()
            .find(|spec| spec.key == key)
            .map(|spec| Arc::clone(&spec.source))
            .or_else(|| self.table.vector(key).cloned())?;
        let width = pixels[0].round().clamp(1., MOST_SIDE as f32) as u32;
        let height = pixels[1].round().clamp(1., MOST_SIDE as f32) as u32;
        if u64::from(width) * u64::from(height) > MOST_PIXELS {
            return None;
        }
        // One size of a drawing at a time: the others were for a size it no longer has.
        let others: Vec<(ImageKey, u32, u32)> = self
            .by_vector
            .keys()
            .filter(|seen| seen.0 == key && (seen.1, seen.2) != (width, height))
            .copied()
            .collect();
        for other in others {
            if let Some(mut old) = self.by_vector.remove(&other) {
                old.give_up(&mut self.retired);
            }
        }
        let picture = match self.by_vector.entry((key, width, height)) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                // The sprite pipeline blends straight alpha, and the drawing is already BGRA.
                let bytes = verve_layout::rasterize_svg(&source, width, height)?;
                let image = image::RgbaImage::from_raw(width, height, bytes)?;
                slot.insert(Picture {
                    plain: Arc::new(RenderImage::new(vec![image::Frame::new(image)])),
                    faded: None,
                    drawn: false,
                    idle: 0,
                })
            }
        };
        shown(picture, opacity, &mut self.retired)
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
        // A canvas that has a newer surface lets go of the older one at once: nothing draws it
        // again, and each is as big as the canvas.
        if let Some(slot) = canvas_slot(key) {
            let older: Vec<ImageKey> = self
                .by_page
                .keys()
                .filter(|seen| **seen != key && canvas_slot(**seen) == Some(slot))
                .copied()
                .collect();
            for old in older {
                if let Some((_, mut picture)) = self.by_page.remove(&old) {
                    picture.give_up(&mut self.retired);
                }
            }
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
        // A drawing of a size or a text that a frame did not draw is no use to the next.
        self.by_vector.retain(|_, picture| picture.keep(retired, 0));
        self.by_page
            .retain(|_, (_, picture)| picture.keep(retired, u32::MAX));
        let mut idle: Vec<(ImageKey, u32, usize)> = self
            .by_page
            .iter()
            .filter(|(_, (_, picture))| picture.idle > 0)
            .map(|(key, (_, picture))| (*key, picture.idle, weight(picture)))
            .collect();
        idle.sort_by_key(|(_, idle, _)| *idle);
        let mut held = 0;
        for (key, _, bytes) in idle {
            held += bytes;
            if held > IDLE_BYTES
                && let Some((_, mut picture)) = self.by_page.remove(&key)
            {
                picture.give_up(&mut self.retired);
            }
        }
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

/// The bytes of pixels of `image`, all its frames.
fn weight(picture: &Picture) -> usize {
    let faded = picture
        .faded
        .as_ref()
        .map_or(0, |(_, faded)| bytes_of(faded));
    bytes_of(&picture.plain) + faded
}

fn bytes_of(image: &RenderImage) -> usize {
    (0..image.frame_count())
        .map(|at| {
            let size = image.size(at);
            size.width.0.max(0) as usize * size.height.0.max(0) as usize * 4
        })
        .sum()
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
            loops: 1,
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
        // Not drawn for a long while, it is kept while the table holds it and what is not drawn
        // fits the budget,
        for _ in 0..1000 {
            pictures.end_frame();
        }
        assert!(pictures.take_retired().is_empty());
        assert_eq!(pictures.by_page.len(), 1);
        // and is given up at once when the renderer lets go of it (it is out of the table).
        pictures.set_table(Arc::new(ImageTable::default()));
        assert!(pictures.by_page.is_empty());
        assert_eq!(pictures.take_retired().len(), 1);
        // Pictures not drawn that do not fit the budget: the least recently drawn goes first.
        let big = |_| {
            Arc::new(ImageFrames {
                width: 4096,
                height: 2560,
                frames: vec![(
                    Arc::from(vec![0u8; 4096 * 2560 * 4]),
                    std::time::Duration::ZERO,
                )],
                loops: 1,
            })
        };
        pictures.set_table(Arc::new(ImageTable::new(
            [(ImageKey(4), big(4)), (ImageKey(5), big(5))].into(),
        )));
        pictures.image(ImageKey(4), 1.).expect("a picture");
        pictures.end_frame();
        pictures.image(ImageKey(5), 1.).expect("a picture");
        pictures.end_frame();
        pictures.end_frame();
        assert!(
            !pictures.by_page.contains_key(&ImageKey(4)),
            "the older goes first"
        );
        assert!(pictures.by_page.contains_key(&ImageKey(5)));
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

    #[test]
    fn a_newer_generation_of_a_canvas_gives_up_the_older_at_once() {
        let mut pictures = Pictures::default();
        let surface = |color: u8| ImageFrames {
            width: 1,
            height: 1,
            frames: vec![(Arc::from(vec![color, 0, 0, 255]), std::time::Duration::ZERO)],
            loops: 1,
        };
        let (first, next) = (
            verve_layout::canvas_key(9, 1),
            verve_layout::canvas_key(9, 2),
        );
        let table =
            |key, color| Arc::new(ImageTable::new([(key, Arc::new(surface(color)))].into()));
        pictures.set_table(table(first, 1));
        let old = pictures.image(first, 1.).expect("a surface");
        pictures.set_table(table(next, 2));
        let new = pictures.image(next, 1.).expect("a surface");
        assert!(!Arc::ptr_eq(&old, &new));
        // Not after a grace period of 300 frames: when the newer one is first drawn.
        assert_eq!(pictures.take_retired().len(), 1);
        assert_eq!(pictures.by_page.len(), 1);
    }

    #[test]
    fn a_drawing_is_rasterised_for_the_size_asked_and_again_for_another() {
        let mut pictures = Pictures::default();
        let key = ImageKey(11);
        let source: Arc<[u8]> = Arc::from(
            &br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2" fill="red"/></svg>"#[..],
        );
        let spec = [VectorSpec { key, source }];
        let small = pictures.page(key, 1., &spec, [8., 4.]).expect("a picture");
        assert_eq!(small.size(0).width.0, 8);
        let again = pictures.page(key, 1., &spec, [8., 4.]).expect("a picture");
        assert!(Arc::ptr_eq(&small, &again));
        // Drawn at the size it is shown at, not stretched from the first.
        let large = pictures.page(key, 1., &spec, [16., 8.]).expect("a picture");
        assert_eq!((large.size(0).width.0, large.size(0).height.0), (16, 8));
        // Red and opaque, as BGRA.
        assert_eq!(&large.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
        // One size at a time: the 8x4 one was given up when the 16x8 one was drawn.
        assert_eq!(pictures.take_retired().len(), 1);
        assert_eq!(pictures.by_vector.len(), 1);
    }
}
