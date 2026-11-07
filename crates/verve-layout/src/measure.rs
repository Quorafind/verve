//! Measuring text without a text system: the trait layout asks, and the stand-in tests answer with.
//!
//! Fonts and glyphs belong to whoever draws (GPUI's `TextSystem` on a desktop), and layout runs on
//! another thread, so it only sees them through [`TextMeasurer`]: a font is an opaque
//! [`FontKey`], a run of text comes back shaped. [`HeadlessMeasurer`] is the measurer for runs with
//! no window: every character half an em wide, the ascent four fifths of an em.

use std::sync::Arc;

/// A font, as the measurer that resolved it names it: opaque, and good on any thread (GPUI's
/// implementation keeps a `gpui::FontId` in it).
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct FontKey(pub u64);

/// What a font measures at one size, in CSS pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FontMetrics {
    /// The distance from the baseline up to the top of the line the font asks for.
    pub ascent: f32,
    /// The distance from the baseline down to the bottom of that line.
    pub descent: f32,
    /// The space between lines the font asks for on top of ascent and descent.
    pub line_gap: f32,
    pub x_height: f32,
    pub cap_height: f32,
}

/// One glyph of a shaped run.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PlacedGlyph {
    /// The font's own number for the glyph.
    pub id: u32,
    /// Where the glyph starts along the run, in CSS pixels.
    pub x: f32,
    /// The UTF-8 byte of the text the glyph is for.
    pub byte: u32,
    pub is_emoji: bool,
    /// The font that draws the glyph. When the requested font lacks a character the platform
    /// takes another one for it, so this differs from [`ShapedRun::font`] for such a glyph.
    pub font: FontKey,
}

/// A run of text in one font, shaped.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedRun {
    pub font: FontKey,
    pub size: f32,
    pub glyphs: Arc<[PlacedGlyph]>,
    /// The run's advance.
    pub width: f32,
}

/// What layout asks of whoever owns the fonts. Calls come from the renderer's thread; an
/// implementation caches, because layout asks for the same run again after every change.
pub trait TextMeasurer: Send + Sync + 'static {
    /// The font a `font-family` list (already split) names at a weight and slant; when none of
    /// them is found, the fallback chain's.
    fn resolve(&self, families: &[String], weight: f32, italic: bool) -> FontKey;
    /// What `font` measures at `size`.
    fn metrics(&self, font: FontKey, size: f32) -> FontMetrics;
    /// `text` shaped in one font.
    fn shape(&self, text: &str, font: FontKey, size: f32) -> Arc<ShapedRun>;
    /// The measurer a page uses: its own faces, over what the measurer shares among pages (the
    /// text system and its caches). A measurer with nothing per page is its own.
    fn for_page(self: Arc<Self>) -> Arc<dyn TextMeasurer>;
    /// Says a layout is finished, the moment to let go of what shaping it kept (GPUI's
    /// `WindowTextSystem::finish_frame` once that is public). Called at the end of every
    /// [`crate::LayoutEngine::update`] that laid out again, and of every placing again.
    fn finish_layout(&self) {}
    /// Takes the bytes of a face a page declared (`FontFace`, `@font-face`) to draw with as
    /// `family`; answers whether they were a font it can use. What a measurer without fonts
    /// takes nothing of.
    fn add_face(&self, _family: &str, _weight: f32, _italic: bool, _bytes: Arc<[u8]>) -> bool {
        false
    }
}

/// The measurer for headless runs and tests: no fonts, so every character is half an em wide and
/// every font's ascent is four fifths of an em. The same numbers on every machine.
#[derive(Copy, Clone, Debug, Default)]
pub struct HeadlessMeasurer;

impl HeadlessMeasurer {
    /// How wide a character is, in ems.
    pub const ADVANCE: f32 = 0.5;
    /// How far above the baseline a line reaches, in ems.
    pub const ASCENT: f32 = 0.8;
}

impl TextMeasurer for HeadlessMeasurer {
    fn for_page(self: Arc<Self>) -> Arc<dyn TextMeasurer> {
        self
    }

    fn resolve(&self, _families: &[String], weight: f32, italic: bool) -> FontKey {
        FontKey((u64::from(weight.round() as u32) << 1) | u64::from(italic))
    }

    fn metrics(&self, _font: FontKey, size: f32) -> FontMetrics {
        FontMetrics {
            ascent: size * Self::ASCENT,
            descent: size * (1. - Self::ASCENT),
            line_gap: 0.,
            x_height: size * 0.5,
            cap_height: size * 0.7,
        }
    }

    fn shape(&self, text: &str, font: FontKey, size: f32) -> Arc<ShapedRun> {
        let advance = size * Self::ADVANCE;
        let glyphs: Vec<PlacedGlyph> = text
            .char_indices()
            .enumerate()
            .map(|(place, (byte, ch))| PlacedGlyph {
                id: u32::from(ch),
                x: place as f32 * advance,
                byte: byte as u32,
                is_emoji: false,
                font,
            })
            .collect();
        Arc::new(ShapedRun {
            font,
            size,
            width: glyphs.len() as f32 * advance,
            glyphs: glyphs.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_runs_are_half_an_em_a_character() {
        let measurer = HeadlessMeasurer;
        let font = measurer.resolve(&["serif".into()], 400., false);
        let run = measurer.shape("héllo", font, 20.);
        assert_eq!(run.width, 50.);
        assert_eq!(run.glyphs.len(), 5);
        assert_eq!(run.glyphs[2].byte, 3);
        assert_eq!(run.glyphs[2].x, 20.);
        assert_eq!(run.glyphs[2].font, font);
        assert_eq!(measurer.metrics(font, 10.).ascent, 8.);
    }

    #[test]
    fn headless_fonts_differ_by_weight_and_slant() {
        let measurer = HeadlessMeasurer;
        let plain = measurer.resolve(&[], 400., false);
        assert_ne!(plain, measurer.resolve(&[], 700., false));
        assert_ne!(plain, measurer.resolve(&[], 400., true));
        assert_eq!(plain, measurer.resolve(&["a".into()], 400., false));
    }
}
