//! Canvas text: the `font` shorthand, shaping, glyph outlines and the numbers `measureText`
//! answers.
//!
//! Everything here works in the canvas's user space with y down and the baseline at y = 0;
//! `skrifa`'s outlines are y up, so the pen flips them.

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};
use tiny_skia::PathBuilder;

use crate::canvas::fonts::FontStack;
use crate::css;
use crate::css::ROOT_FONT_SIZE;

/// Where the hanging baseline sits, as a fraction of the ascent.
pub const HANGING_BASELINE: f32 = 0.8;

/// The ascent, as a fraction of the size, of a face whose metrics cannot be read.
const FALLBACK_ASCENT: f32 = 0.8;

/// A parsed canvas `font`: the CSS shorthand, with the values shaping needs.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub families: Vec<String>,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub kerning: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            families: vec!["sans-serif".to_owned()],
            size: 10.,
            weight: 400,
            italic: false,
            letter_spacing: 0.,
            word_spacing: 0.,
            kerning: true,
        }
    }
}

impl TextStyle {
    /// Reads the `font` shorthand: `[style] [variant] [weight] [stretch] size[/line-height]
    /// family`, as a CSS font. `None` for what a browser would ignore.
    pub fn parse(font: &str) -> Option<TextStyle> {
        let longhands = css::font_longhands(font)?;
        let mut style = TextStyle::default();
        for (property, value) in longhands {
            match property {
                "font-style" => style.italic = matches!(value.as_str(), "italic" | "oblique"),
                "font-weight" => style.weight = weight_of(&value),
                "font-size" => style.size = font_size(&value)?,
                "font-family" => style.families = family_list(&value),
                _ => {}
            }
        }
        if style.families.is_empty() || style.size <= 0. {
            return None;
        }
        Some(style)
    }
}

/// A font size in pixels against the document's default: the keywords, percentages and lengths
/// `font-size` takes, as the cascade reads them.
fn font_size(value: &str) -> Option<f32> {
    css::font_size_px(value, ROOT_FONT_SIZE).or_else(|| css::length(value, ROOT_FONT_SIZE))
}

fn weight_of(value: &str) -> u16 {
    match value.trim() {
        "normal" => 400,
        "bold" | "bolder" => 700,
        "lighter" => 300,
        other => other.parse().unwrap_or(400),
    }
}

/// The families of a `font-family`, without their quotes.
fn family_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|family| {
            family
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .trim()
                .to_owned()
        })
        .filter(|family| !family.is_empty())
        .collect()
}

/// The advances and boxes of a laid-out string, in canvas pixels: what `TextMetrics` reports.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub width: f32,
    pub actual_left: f32,
    pub actual_right: f32,
    pub actual_ascent: f32,
    pub actual_descent: f32,
    pub font_ascent: f32,
    pub font_descent: f32,
}

/// What measurement is asked of: the renderer's own font stack now; P30 may answer with the
/// layout engine's measurer so canvas text and DOM text agree to the pixel.
pub trait Measurer {
    /// The advances and boxes of `text` set in `style`.
    fn measure(&mut self, text: &str, style: &TextStyle) -> Metrics;
}

impl Measurer for FontStack {
    fn measure(&mut self, text: &str, style: &TextStyle) -> Metrics {
        Layout::of(self, text, style).metrics()
    }
}

/// A placed glyph: its id in the face, and where its origin sits relative to the text's.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// One face's glyphs.
pub struct Run {
    pub face: fontdb::ID,
    pub size: f32,
    pub glyphs: Vec<Glyph>,
}

/// A shaped string: what measurement reads and what drawing walks.
pub struct Layout {
    pub runs: Vec<Run>,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// The ink's box: `(left, top, right, bottom)` from the origin, y up, the empty box at zero.
    pub ink: Option<[f32; 4]>,
}

impl Layout {
    pub fn of(fonts: &FontStack, text: &str, style: &TextStyle) -> Layout {
        let families: Vec<&str> = style.families.iter().map(String::as_str).collect();
        let mut runs = Vec::new();
        let mut width = 0.;
        let mut ink = Bounds::default();
        for (face, segment) in segments(fonts, &families, style, text) {
            let Some((glyphs, advance, bounds)) = shape(fonts, face, segment, style, width) else {
                continue;
            };
            ink.extend(bounds);
            width += advance;
            runs.push(Run {
                face,
                size: style.size,
                glyphs,
            });
        }
        let (ascent, descent) = runs
            .first()
            .and_then(|run| face_metrics(fonts, run.face, run.size))
            .unwrap_or((
                style.size * FALLBACK_ASCENT,
                style.size * (1. - FALLBACK_ASCENT),
            ));
        Layout {
            runs,
            width,
            ascent,
            descent,
            ink: ink.into_box(),
        }
    }

    pub fn metrics(&self) -> Metrics {
        let ink = self.ink.unwrap_or([0.; 4]);
        Metrics {
            width: self.width,
            actual_left: -ink[0],
            actual_right: ink[2],
            actual_ascent: ink[1],
            actual_descent: -ink[3],
            font_ascent: self.ascent,
            font_descent: self.descent,
        }
    }
}

/// The text split into faces, by whether each character's face covers it.
fn segments<'a>(
    fonts: &FontStack,
    families: &[&str],
    style: &TextStyle,
    text: &'a str,
) -> Vec<(fontdb::ID, &'a str)> {
    let mut out: Vec<(fontdb::ID, &str)> = Vec::new();
    let mut start = 0;
    let mut current: Option<fontdb::ID> = None;
    for (at, ch) in text.char_indices() {
        let face = fonts.resolve(families, style.weight, style.italic, ch);
        match (face, current) {
            (Some(face), Some(current)) if face == current => {}
            (face, _) => {
                if let Some(current) = current
                    && start < at
                {
                    out.push((current, &text[start..at]));
                }
                current = face;
                start = at;
            }
        }
    }
    if let Some(current) = current
        && start < text.len()
    {
        out.push((current, &text[start..]));
    }
    out
}

/// Shapes one face's segment, its glyphs placed from `origin` along the baseline.
fn shape(
    fonts: &FontStack,
    face: fontdb::ID,
    segment: &str,
    style: &TextStyle,
    origin: f32,
) -> Option<(Vec<Glyph>, f32, Bounds)> {
    let shaped = fonts.with_face(face, |data, index| {
        let buzz = rustybuzz::Face::from_slice(data, index)?;
        let scale = style.size / buzz.units_per_em() as f32;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(segment);
        let features: Vec<rustybuzz::Feature> = if style.kerning {
            Vec::new()
        } else {
            vec![rustybuzz::Feature::new(
                rustybuzz::ttf_parser::Tag::from_bytes(b"kern"),
                0,
                ..,
            )]
        };
        let shaped = rustybuzz::shape(&buzz, &features, buffer);
        let mut glyphs = Vec::with_capacity(shaped.len());
        let mut pen = 0.;
        for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            glyphs.push(Glyph {
                id: info.glyph_id,
                x: origin + pen + position.x_offset as f32 * scale,
                y: -position.y_offset as f32 * scale,
            });
            pen += position.x_advance as f32 * scale + style.letter_spacing;
            let space = segment
                .get(info.cluster as usize..)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| c == ' ');
            if space {
                pen += style.word_spacing;
            }
        }
        let bounds = outline_bounds(data, index, &glyphs, style.size);
        Some((glyphs, pen, bounds))
    });
    shaped.flatten()
}

/// The box the glyphs' outlines cover, in canvas coordinates.
fn outline_bounds(data: &[u8], index: u32, glyphs: &[Glyph], size: f32) -> Bounds {
    let Ok(font) = FontRef::from_index(data, index) else {
        return Bounds::default();
    };
    let outlines = font.outline_glyphs();
    let settings = || DrawSettings::unhinted(Size::new(size), LocationRef::default());
    let mut ink = Bounds::default();
    for glyph in glyphs {
        let Some(outline) = outlines.get(skrifa::GlyphId::new(glyph.id)) else {
            continue;
        };
        let mut pen = BoundsPen {
            bounds: Bounds::default(),
            x: glyph.x,
            y: glyph.y,
        };
        if outline.draw(settings(), &mut pen).is_err() {
            continue;
        }
        ink.extend(pen.bounds);
    }
    ink
}

#[derive(Clone, Copy, Default)]
struct Bounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    seen: bool,
}

impl Bounds {
    fn add(&mut self, x: f32, y: f32) {
        if !self.seen {
            *self = Bounds {
                min_x: x,
                min_y: y,
                max_x: x,
                max_y: y,
                seen: true,
            };
            return;
        }
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }

    fn extend(&mut self, other: Bounds) {
        if other.seen {
            self.add(other.min_x, other.min_y);
            self.add(other.max_x, other.max_y);
        }
    }

    /// `[left, top, right, bottom]` with y up, or `None` when nothing was added.
    fn into_box(self) -> Option<[f32; 4]> {
        self.seen
            .then_some([self.min_x, self.max_y, self.max_x, self.min_y])
    }
}

/// Collects an outline's box.
struct BoundsPen {
    bounds: Bounds,
    x: f32,
    y: f32,
}

impl OutlinePen for BoundsPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.bounds.add(self.x + x, self.y + y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.bounds.add(self.x + x, self.y + y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.bounds.add(self.x + cx0, self.y + cy0);
        self.bounds.add(self.x + x, self.y + y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.bounds.add(self.x + cx0, self.y + cy0);
        self.bounds.add(self.x + cx1, self.y + cy1);
        self.bounds.add(self.x + x, self.y + y);
    }

    fn close(&mut self) {}
}

/// The face's ascent and descent in pixels, from its own metrics.
pub fn face_metrics(fonts: &FontStack, face: fontdb::ID, size: f32) -> Option<(f32, f32)> {
    fonts.with_face(face, |data, index| {
        let font = FontRef::from_index(data, index).ok()?;
        let metrics = font.metrics(Size::new(size), LocationRef::default());
        Some((metrics.ascent, -metrics.descent))
    })?
}

/// Where a text is drawn, as `fillText` receives it.
#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub align: Align,
    pub baseline: Baseline,
    /// `fillText`'s `maxWidth`: the text is condensed to it.
    pub max_width: Option<f32>,
}

/// `textAlign`: where the text sits against the point it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Left,
    Center,
    Right,
    End,
}

/// `textBaseline`: which line of the text sits at the point it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Baseline {
    Top,
    Hanging,
    Middle,
    Alphabetic,
    Ideographic,
    Bottom,
}

impl Placement {
    /// The glyph outlines in user space, ready to be filled or stroked.
    pub fn outline(&self, fonts: &FontStack, layout: &Layout) -> Option<tiny_skia::Path> {
        let scale_x = match self.max_width {
            Some(max) if layout.width > 0. => max / layout.width,
            _ => 1.,
        };
        let origin_x = match self.align {
            Align::Start | Align::Left => 0.,
            Align::Center => -layout.width * scale_x / 2.,
            Align::Right | Align::End => -layout.width * scale_x,
        };
        let origin_y = match self.baseline {
            Baseline::Alphabetic => 0.,
            Baseline::Top => layout.ascent,
            Baseline::Hanging => layout.ascent * HANGING_BASELINE,
            Baseline::Middle => (layout.ascent - layout.descent) / 2.,
            Baseline::Ideographic | Baseline::Bottom => -layout.descent,
        };
        let origin = (self.x + origin_x, self.y + origin_y);
        let mut builder = PathBuilder::new();
        for run in &layout.runs {
            outline_run(fonts, run, origin, scale_x, &mut builder)?;
        }
        builder.finish()
    }
}

/// Adds the outlines of one face's glyphs to `builder`, placed from `origin` and condensed by
/// `scale_x`. `None` when the face cannot be read or a glyph's outline cannot be drawn.
fn outline_run(
    fonts: &FontStack,
    run: &Run,
    origin: (f32, f32),
    scale_x: f32,
    builder: &mut PathBuilder,
) -> Option<()> {
    fonts.with_face(run.face, |data, index| {
        let font = FontRef::from_index(data, index).ok()?;
        let outlines = font.outline_glyphs();
        let settings = || DrawSettings::unhinted(Size::new(run.size), LocationRef::default());
        let mut pen = CanvasPen {
            builder,
            x: 0.,
            y: 0.,
        };
        for glyph in &run.glyphs {
            pen.x = origin.0 + glyph.x * scale_x;
            pen.y = origin.1 - glyph.y;
            if let Some(outline) = outlines.get(skrifa::GlyphId::new(glyph.id)) {
                outline.draw(settings(), &mut pen).ok()?;
            }
        }
        Some(())
    })?
}

/// Writes a glyph's outline into a path, y flipped and the origin at the glyph's place.
struct CanvasPen<'a> {
    builder: &'a mut PathBuilder,
    x: f32,
    y: f32,
}

impl OutlinePen for CanvasPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(self.x + x, self.y - y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.x + x, self.y - y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder
            .quad_to(self.x + cx0, self.y - cy0, self.x + x, self.y - y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_to(
            self.x + cx0,
            self.y - cy0,
            self.x + cx1,
            self.y - cy1,
            self.x + x,
            self.y - y,
        );
    }

    fn close(&mut self) {
        self.builder.close();
    }
}
