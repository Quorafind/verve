//! The pictures a page shows, as the shell draws them: decoded, and looked up by key.
//!
//! The renderer decodes what a page loads into [`ImageFrames`] (straight-alpha BGRA, which is what
//! GPUI's sprite pipeline blends) and publishes the lot as an [`ImageTable`], an immutable
//! snapshot the shell reads. A picture is named by an [`ImageKey`] that is a function of its
//! address, so the display list can name a picture before it has been decoded.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use crate::display_list::ImageKey;

/// The bits of an [`ImageKey`] that tell what kind of picture it names: the top two bits are
/// `00` for a picture a page loaded (the address hashed to 62 bits, so two addresses collide
/// with a chance of 2^-62 and would then show one picture), `1x` for a rasterised gradient (P30:
/// the hash with the top bit set and bit 62 arbitrary), and `01` for a canvas surface (P35,
/// second phase).
const KIND_BITS: u64 = 0b11 << 62;

/// One decoded picture: one or more frames, each with how long it shows.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageFrames {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha BGRA, row-major, `width * 4` bytes a row.
    pub frames: Vec<(Arc<[u8]>, Duration)>,
}

impl std::fmt::Debug for ImageFrames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImageFrames {{ {}x{}, {} frames }}",
            self.width,
            self.height,
            self.frames.len()
        )
    }
}

/// The decoded pictures of a page by key: a snapshot, shared by `Arc`.
#[derive(Clone, Debug, Default)]
pub struct ImageTable {
    pictures: HashMap<ImageKey, Arc<ImageFrames>>,
}

impl ImageTable {
    pub fn new(pictures: HashMap<ImageKey, Arc<ImageFrames>>) -> Self {
        Self { pictures }
    }

    pub fn get(&self, key: ImageKey) -> Option<&Arc<ImageFrames>> {
        self.pictures.get(&key)
    }

    pub fn len(&self) -> usize {
        self.pictures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pictures.is_empty()
    }
}

/// The key of the picture at `url` (resolved): the same address is always the same key, and no
/// page address is the key of a gradient or a canvas.
pub fn key_for_url(url: &str) -> ImageKey {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    ImageKey(hasher.finish() & !KIND_BITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_picture_key_is_stable_and_apart_from_gradients() {
        let a = key_for_url("http://h/a.png");
        assert_eq!(a, key_for_url("http://h/a.png"));
        assert_ne!(a, key_for_url("http://h/b.png"));
        assert_eq!(a.0 & KIND_BITS, 0);
    }
}
