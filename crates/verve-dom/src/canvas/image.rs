//! Images as bytes: the size an `<img>` reports from the header, the pixels `drawImage` needs
//! decoded, and the store of what the loader fetched.
//!
//! A page's images arrive as bytes from the embedder's loader. The drawer decodes the ones it
//! shows; here they are decoded only on demand for `drawImage`, `createImageBitmap` and
//! `decode()`, premultiplied as tiny-skia draws them and kept up to a budget — a page that
//! scrolls past a hundred thumbnails must not keep them all.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use ::image::ImageDecoder;
use boa_engine::object::WeakJsObject;
#[cfg(feature = "canvas")]
use tiny_skia::Pixmap;

use crate::dom::NodeId;

/// How many bytes of decoded pixels are kept for `drawImage` before the least recently drawn
/// images are dropped.
#[cfg(feature = "canvas")]
const DECODE_BUDGET: usize = 64 << 20;

/// The size of a drawing with no intrinsic size, as CSS gives an `<img>` of an SVG with none.
const DEFAULT_OBJECT_SIZE: (u32, u32) = (300, 150);

/// Reads only the header: what the loader asks as bytes arrive, to lay the page out before the
/// image is drawn.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if let Some(size) = svg_size(bytes) {
        return Some(size);
    }
    ::image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
        .filter(|(width, height)| *width > 0 && *height > 0)
}

/// The pixels of `bytes`: premultiplied RGBA8, row-major, the first frame of an animation, turned
/// as an EXIF orientation asks (`image-orientation: from-image`). `mime` is the type the loader
/// saw: it settles the format only when the bytes do not name it.
pub fn decode(bytes: &[u8], mime: Option<&str>) -> Result<DecodedImage, DecodeError> {
    if svg_size(bytes).is_some() {
        return Err(DecodeError::Unsupported("SVG"));
    }
    let broken = |error: ::image::ImageError| DecodeError::Broken(error.to_string());
    let mut reader = ::image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| DecodeError::Broken(error.to_string()))?;
    if reader.format().is_none() {
        let hinted = mime.and_then(::image::ImageFormat::from_mime_type);
        reader.set_format(hinted.ok_or(DecodeError::Unsupported("unrecognised image"))?);
    }
    let mut decoder = reader.into_decoder().map_err(broken)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(::image::metadata::Orientation::NoTransforms);
    let mut image = ::image::DynamicImage::from_decoder(decoder).map_err(broken)?;
    image.apply_orientation(orientation);
    let image = image.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Err(DecodeError::Broken("the image has no pixels".into()));
    }
    let mut pixels = image.into_raw();
    premultiply(&mut pixels);
    Ok(DecodedImage {
        width,
        height,
        pixels: Arc::from(pixels),
    })
}

/// An image's pixels as the canvas draws them: premultiplied, so a half-transparent white is
/// `[128, 128, 128, 128]`.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

#[cfg(feature = "canvas")]
impl DecodedImage {
    /// The pixels as a tiny-skia pixmap, which owns a copy.
    pub(crate) fn to_pixmap(&self) -> Option<Pixmap> {
        pixmap_of(self.width, self.height, self.pixels.to_vec())
    }
}

/// A pixmap of premultiplied RGBA8 `pixels`, `width` by `height`.
#[cfg(feature = "canvas")]
pub(crate) fn pixmap_of(width: u32, height: u32, pixels: Vec<u8>) -> Option<Pixmap> {
    Pixmap::from_vec(pixels, tiny_skia::IntSize::from_wh(width, height)?)
}

/// Why bytes did not decode.
#[derive(Debug)]
pub enum DecodeError {
    /// A format the canvas does not draw: SVG (the drawer rasterises it), AVIF.
    Unsupported(&'static str),
    /// Bytes that are not an image of the format they claim.
    Broken(String),
}

/// Straight RGBA bytes made premultiplied, in place.
pub fn premultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// Premultiplied RGBA bytes made straight, in place.
pub fn unpremultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

/// An SVG's size from its header: `width`/`height`, else the `viewBox`'s, else the 300×150 the
/// CSS default object size gives a drawing with no intrinsic size.
fn svg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let head = &bytes[..bytes.len().min(4096)];
    let text = std::str::from_utf8(head).ok()?;
    let start = text.find("<svg")?;
    let tag = &text[start..text[start..].find('>').map(|end| start + end)?];
    let attribute = |name: &str| -> Option<&str> {
        let at = tag.find(name)?;
        let rest = tag[at + name.len()..].trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let rest = &rest[1..];
        rest.split(quote).next()
    };
    let length = |value: Option<&str>| -> Option<f32> {
        let value = value?.trim();
        let number = value.trim_end_matches("px").trim_end_matches("pt").trim();
        number.parse::<f32>().ok().filter(|n| *n > 0.)
    };
    if let (Some(width), Some(height)) = (length(attribute("width")), length(attribute("height"))) {
        return Some((width.round() as u32, height.round() as u32));
    }
    if let Some(view) = attribute("viewBox") {
        let numbers: Vec<f32> = view
            .split([' ', ','])
            .filter_map(|n| n.parse::<f32>().ok())
            .collect();
        if let [_, _, width, height] = numbers[..]
            && width > 0.
            && height > 0.
        {
            return Some((width.round() as u32, height.round() as u32));
        }
    }
    Some(DEFAULT_OBJECT_SIZE)
}

/// An image as the drawer receives it: the bytes as fetched, for it to decode.
#[derive(Clone)]
pub struct ImageBytes {
    /// The type the loader reported, if it did.
    pub mime: Option<String>,
    pub bytes: Arc<[u8]>,
    /// The size the header gives.
    pub natural: (u32, u32),
}

/// What an `<img>` knows: the address it is fetching, what came back, and the `decode()`
/// promises waiting on it.
#[derive(Default)]
pub struct ElementImage {
    pub url: Option<String>,
    pub status: Status,
    /// `decode()`'s promises, held weakly: only JavaScript keeps them alive.
    pub waiting: Vec<WeakJsObject>,
    /// The pixel density of the source `srcset` chose (`0` until one was chosen: read it with
    /// [`ElementImage::density`]).
    pub density: f32,
}

impl ElementImage {
    /// How many image pixels make a CSS pixel.
    pub fn density(&self) -> f32 {
        if self.density > 0. { self.density } else { 1. }
    }
}

/// How far an `<img>`'s load has got.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    /// No `src`, or one that is not an image to fetch.
    #[default]
    Idle,
    Loading,
    Loaded,
    Broken,
}

impl Status {
    /// `complete`: nothing is being fetched and nothing failed mid-way.
    pub fn is_complete(self) -> bool {
        self != Status::Loading
    }
}

struct Entry {
    mime: Option<String>,
    bytes: Arc<[u8]>,
    natural: Option<(u32, u32)>,
}

/// Who needs an image's bytes, which decides how long they are kept.
#[derive(Clone, Copy, PartialEq)]
pub enum Keep {
    /// Until no `<img>` names the address.
    WhileNamed,
    /// For as long as the loader's other users (a stylesheet's `url()`) may want them.
    Always,
}

/// The images the page has bytes for, and what each `<img>` names.
#[derive(Default)]
pub struct ImageStore {
    entries: HashMap<String, Entry>,
    /// What each `<img>` names and how its load has gone.
    pub nodes: HashMap<NodeId, ElementImage>,
    /// The addresses that came from an `<img src>`: the only ones dropped when the element stops
    /// naming them, since a stylesheet's are the loader's to keep or forget.
    element_urls: HashSet<String>,
    /// The most recently drawn decoded images, oldest first.
    #[cfg(feature = "canvas")]
    decoded: Vec<(String, Arc<Pixmap>)>,
    #[cfg(feature = "canvas")]
    decoded_bytes: usize,
    /// Addresses whose bytes were not an image: asked for once, not once per frame.
    pub failed: HashSet<String>,
    /// The addresses a stylesheet or a `poster` asked the embedder for: asked once.
    pub asked: HashSet<String>,
    /// The addresses whose pixels taint a canvas.
    #[cfg(feature = "canvas")]
    tainted: HashSet<String>,
    /// `load` (for [`Status::Loaded`]) and `error` (for [`Status::Broken`]) events owed to
    /// elements, queued by `img::flush_events`.
    pub events: Vec<(NodeId, Status)>,
}

impl ImageStore {
    /// An image's bytes arrived: what `<img>` reads from now on, and what `drawImage` decodes.
    pub fn insert(&mut self, url: &str, mime: Option<String>, bytes: Arc<[u8]>, keep: Keep) {
        if keep == Keep::WhileNamed {
            self.element_urls.insert(url.to_owned());
        }
        let natural = image_size(&bytes);
        let entry = Entry {
            mime,
            bytes,
            natural,
        };
        self.entries.insert(url.to_owned(), entry);
        self.forget_decoded(url);
    }

    /// Keeps the image at `url` for as long as a stylesheet's `url()` or a `poster` may want it,
    /// whichever `<img>` stops naming it.
    pub fn keep_always(&mut self, url: &str) {
        self.element_urls.remove(url);
    }

    /// Forgets the image at `url`.
    pub fn remove(&mut self, url: &str) {
        self.entries.remove(url);
        self.element_urls.remove(url);
        #[cfg(feature = "canvas")]
        self.tainted.remove(url);
        self.forget_decoded(url);
    }

    /// An address that is not an image: its elements stay broken instead of asking again.
    pub fn mark_failed(&mut self, url: &str) {
        self.failed.insert(url.to_owned());
        self.forget_decoded(url);
    }

    #[cfg(feature = "canvas")]
    fn forget_decoded(&mut self, url: &str) {
        self.decoded.retain(|(known, _)| known != url);
    }

    #[cfg(not(feature = "canvas"))]
    fn forget_decoded(&mut self, _url: &str) {}

    /// Whether the bytes at `url` have arrived.
    pub fn has(&self, url: &str) -> bool {
        self.entries.contains_key(url)
    }

    /// Notes that drawing the image at `url` taints the canvas it is drawn on: it came from
    /// another origin.
    #[cfg(feature = "canvas")]
    pub fn mark_tainted(&mut self, url: &str) {
        self.tainted.insert(url.to_owned());
    }

    /// Whether drawing the image at `url` taints the canvas it is drawn on.
    #[cfg(feature = "canvas")]
    pub fn is_tainted(&self, url: &str) -> bool {
        self.tainted.contains(url)
    }

    /// The size the header gave.
    pub fn natural(&self, url: &str) -> Option<(u32, u32)> {
        self.entries.get(url)?.natural
    }

    /// The image's pixels, decoded now or from the cache.
    #[cfg(feature = "canvas")]
    pub fn pixmap(&mut self, url: &str) -> Option<Arc<Pixmap>> {
        if let Some(at) = self.decoded.iter().position(|(known, _)| known == url) {
            let hit = self.decoded.remove(at);
            let pixmap = hit.1.clone();
            self.decoded.push(hit);
            return Some(pixmap);
        }
        let entry = self.entries.get(url)?;
        let decoded = decode(&entry.bytes, entry.mime.as_deref()).ok()?;
        let pixmap = Arc::new(decoded.to_pixmap()?);
        self.decoded_bytes += pixmap.data().len();
        self.decoded.push((url.to_owned(), pixmap.clone()));
        while self.decoded_bytes > DECODE_BUDGET && self.decoded.len() > 1 {
            let (_, dropped) = self.decoded.remove(0);
            self.decoded_bytes = self.decoded_bytes.saturating_sub(dropped.data().len());
        }
        Some(pixmap)
    }

    #[cfg(feature = "canvas")]
    fn pixels_decode(&mut self, url: &str) -> bool {
        self.pixmap(url).is_some()
    }

    #[cfg(not(feature = "canvas"))]
    fn pixels_decode(&mut self, url: &str) -> bool {
        let entry = self.entries.get(url);
        entry.is_some_and(|entry| decode(&entry.bytes, entry.mime.as_deref()).is_ok())
    }

    /// Whether the image at `url` can be drawn: its pixels decode, or it is an SVG the drawer
    /// rasterises. The header read that made the element `complete` cannot tell a truncated file.
    pub fn decodes(&mut self, url: Option<&str>) -> bool {
        let Some(url) = url else {
            return false;
        };
        self.pixels_decode(url)
            || self
                .entries
                .get(url)
                .is_some_and(|entry| svg_size(&entry.bytes).is_some())
    }

    /// Drops the images no `<img>` names any more, so a page that swaps its pictures does not
    /// keep the old ones for the drawer.
    pub fn retain_referenced(&mut self) {
        let live: HashSet<&str> = self
            .nodes
            .values()
            .filter_map(|node| node.url.as_deref())
            .collect();
        let stale: Vec<String> = self
            .element_urls
            .iter()
            .filter(|url| !live.contains(url.as_str()))
            .cloned()
            .collect();
        for url in stale {
            self.remove(&url);
        }
    }

    /// The address each `<img>` shows now, with the density of the source.
    pub fn sources(&self) -> HashMap<NodeId, (String, f32)> {
        self.nodes
            .iter()
            .filter_map(|(node, img)| Some((*node, (img.url.clone()?, img.density()))))
            .collect()
    }

    /// What the drawer is handed: every image's bytes and size.
    pub fn snapshot(&self) -> HashMap<String, ImageBytes> {
        let image = |entry: &Entry| {
            Some(ImageBytes {
                mime: entry.mime.clone(),
                bytes: entry.bytes.clone(),
                natural: entry.natural?,
            })
        };
        self.entries
            .iter()
            .filter_map(|(url, entry)| Some((url.clone(), image(entry)?)))
            .collect()
    }
}
