//! The 2D drawing engine: a canvas's pixels, the state that shapes what is painted next, and
//! the operations themselves (HTML canvas, "the 2D rendering context").
//!
//! Drawing is immediate: a command paints into the canvas's own `Pixmap` before it returns, so
//! `getImageData` right after it sees the pixels. Paths are kept in user space and transformed
//! when they are filled or stroked, and a stroke is widened before the transform, so a scaled
//! context thins or thickens its lines the way the specification says.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, LineCap, LineJoin, Mask, Paint, Path, PathBuilder,
    Pixmap, PixmapPaint, Shader, Stroke, StrokeDash, Transform,
};

use crate::canvas::effects;
use crate::canvas::fonts::FontStack;
use crate::canvas::paint::{GradientKind, GradientSpec, PaintSource, PatternSpec};
use crate::canvas::path::{covers, rectangle};
use crate::canvas::store::CanvasId;
use crate::canvas::text::{Align, Baseline, Layout, Measurer, Metrics, Placement, TextStyle};
use crate::canvas::{Axis, CanvasFrame};
use crate::dom::NodeId;

/// The largest side of a canvas, and the most pixels one holds, as Chrome limits them: past
/// either the canvas draws nothing and exports `"data:,"`.
const MAX_SIDE: u32 = 16384;
const MAX_PIXELS: u64 = 268_435_456;

/// How many `save()`s are kept before further ones are ignored.
const MAX_SAVED_STATES: usize = 10_000;

/// Which canvas a context draws into: an element's, or an `OffscreenCanvas`'s.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CanvasOwner {
    Element(NodeId),
    Offscreen,
}

/// A canvas's pixels and what draws into them.
pub struct Canvas2d {
    pub owner: CanvasOwner,
    pub pixmap: Option<Pixmap>,
    pub width: u32,
    pub height: u32,
    pub draw: DrawState,
    saved: Vec<DrawState>,
    pub path: PathBuilder,
    pub generation: u64,
    /// Pixels changed since the frame the drawer was last given.
    pub dirty: bool,
    /// A cross-origin image was drawn here: `getImageData` and `toDataURL` refuse.
    pub tainted: bool,
    /// `getContext('2d', { alpha })`: a canvas without alpha starts, and clears to, black.
    pub alpha: bool,
    pub will_read_frequently: bool,
    pub frame: Option<CanvasFrame>,
}

impl Canvas2d {
    /// A canvas of a size, with the default state; `alpha` is `getContext`'s option.
    pub fn new(owner: CanvasOwner, width: u32, height: u32, alpha: bool) -> Self {
        let mut canvas = Canvas2d {
            owner,
            pixmap: None,
            width: 0,
            height: 0,
            draw: DrawState::default(),
            saved: Vec::new(),
            path: PathBuilder::new(),
            generation: 0,
            dirty: true,
            tainted: false,
            alpha,
            will_read_frequently: false,
            frame: None,
        };
        canvas.resize(width, height);
        canvas
    }

    /// A new size makes an empty canvas, as a browser's does: the state stays, the pixels go.
    /// A canvas the browser would refuse to allocate keeps no pixels at all, and draws nothing.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        let fits = width > 0
            && height > 0
            && width <= MAX_SIDE
            && height <= MAX_SIDE
            && u64::from(width) * u64::from(height) <= MAX_PIXELS;
        self.pixmap = fits.then(|| Pixmap::new(width, height)).flatten();
        if !self.alpha
            && let Some(pixmap) = &mut self.pixmap
        {
            pixmap.fill(Color::BLACK);
        }
        self.dirty = true;
    }

    /// Sets one side of the canvas, which empties it (`canvas.width = …`).
    pub fn resize_side(&mut self, axis: Axis, value: u32) {
        match axis {
            Axis::Width => self.resize(value, self.height),
            Axis::Height => self.resize(self.width, value),
        }
    }

    /// A new frame for the drawer: the pixels as they are, under a new generation.
    pub fn commit(&mut self) -> Option<NodeId> {
        let CanvasOwner::Element(node) = self.owner else {
            self.dirty = false;
            return None;
        };
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let pixmap = self.pixmap.as_ref()?;
        self.generation += 1;
        self.frame = Some(CanvasFrame {
            width: self.width,
            height: self.height,
            generation: self.generation,
            pixels: Arc::from(pixmap.data()),
        });
        Some(node)
    }

    /// `save()`: pushes the drawing state.
    pub fn save(&mut self) {
        if self.saved.len() < MAX_SAVED_STATES {
            self.saved.push(self.draw.clone());
        }
    }

    /// `restore()`: pops the drawing state, if one was saved.
    pub fn restore(&mut self) {
        if let Some(draw) = self.saved.pop() {
            self.draw = draw;
        }
    }

    /// `reset()`: the state, the path and the pixels back to a new canvas's.
    pub fn reset(&mut self) {
        self.draw = DrawState::default();
        self.saved.clear();
        self.path = PathBuilder::new();
        self.resize(self.width, self.height);
    }

    /// `clearRect`: the region emptied, through the current transform and clip but ignoring the
    /// paint, the shadow and the composite operation. A canvas without alpha is cleared to black.
    pub fn clear_rect(&mut self, rect: [f32; 4]) {
        if !self.usable() {
            return;
        }
        let geometry = rectangle(rect)
            .finish()
            .and_then(|p| p.transform(self.draw.transform));
        let mut paint = Paint {
            anti_alias: false,
            ..Paint::default()
        };
        match self.alpha {
            false => paint.set_color(Color::BLACK),
            true => paint.blend_mode = BlendMode::Clear,
        }
        let (Some(pixmap), Some(geometry)) = (self.pixmap.as_mut(), geometry) else {
            return;
        };
        let (rule, none) = (FillRule::Winding, Transform::identity());
        pixmap.fill_path(&geometry, &paint, rule, none, self.draw.clip.as_deref());
        self.dirty = true;
    }

    /// Multiplies the current transform by `transform`, applied first (`ctx.transform()`).
    pub fn concat(&mut self, transform: Transform) {
        self.draw.transform = self.draw.transform.pre_concat(transform);
    }

    /// Whether anything can be drawn: there are pixels and the clip has not emptied them.
    pub fn usable(&self) -> bool {
        self.pixmap.is_some() && !self.draw.clip_empty
    }

    /// Replaces the clip with its intersection with `path` (in user space).
    pub fn clip_path(&mut self, path: &PathBuilder, rule: FillRule) {
        let geometry = path
            .clone()
            .finish()
            .and_then(|p| p.transform(self.draw.transform));
        let mask = geometry.and_then(|geometry| match &self.draw.clip {
            Some(current) => {
                let mut mask = (**current).clone();
                mask.intersect_path(&geometry, rule, true, Transform::identity());
                Some(mask)
            }
            None => {
                let mut mask = Mask::new(self.width, self.height)?;
                mask.fill_path(&geometry, rule, true, Transform::identity());
                Some(mask)
            }
        });
        // A path that covers nothing empties the clip until the state that set it is restored.
        let mask = mask.or_else(|| {
            let mut mask = Mask::new(self.width, self.height)?;
            mask.clear();
            Some(mask)
        });
        self.draw.clip_empty = mask
            .as_ref()
            .is_none_or(|mask| mask.data().iter().all(|value| *value == 0));
        self.draw.clip = mask.map(Rc::new);
    }
}

/// Everything `save` puts back: the paint, the pen, the text and the clip.
#[derive(Clone)]
pub struct DrawState {
    pub transform: Transform,
    pub fill: PaintSource,
    pub stroke: PaintSource,
    pub alpha: f32,
    pub composite: BlendMode,
    pub line: LineStyle,
    pub text: TextStyle,
    pub align: Align,
    pub baseline: Baseline,
    pub rtl: bool,
    pub shadow: Shadow,
    pub smoothing: bool,
    pub quality: FilterQuality,
    pub filter: String,
    /// Kept as written: the getters report them, the glyph outlines do not vary with them.
    pub font_stretch: String,
    pub font_variant_caps: String,
    pub text_rendering: String,
    pub clip: Option<Rc<Mask>>,
    pub clip_empty: bool,
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            transform: Transform::identity(),
            fill: PaintSource::black(),
            stroke: PaintSource::black(),
            alpha: 1.,
            composite: BlendMode::SourceOver,
            line: LineStyle::default(),
            text: TextStyle::default(),
            align: Align::Start,
            baseline: Baseline::Alphabetic,
            rtl: false,
            shadow: Shadow::default(),
            smoothing: true,
            quality: FilterQuality::Bilinear,
            filter: "none".to_owned(),
            font_stretch: "normal".to_owned(),
            font_variant_caps: "normal".to_owned(),
            text_rendering: "auto".to_owned(),
            clip: None,
            clip_empty: false,
        }
    }
}

impl DrawState {
    /// The paint `op` uses: the stroke style for a stroke, the fill style otherwise.
    pub fn paint_for(&self, op: Op) -> PaintSource {
        match op {
            Op::Stroke => self.stroke.clone(),
            Op::Fill(_) => self.fill.clone(),
        }
    }
}

/// The pen: `lineWidth` and the rest.
#[derive(Clone)]
pub struct LineStyle {
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter: f32,
    /// `setLineDash`'s list, already of even length (an odd one is repeated, as the
    /// specification has it); empty is a solid line.
    pub dash: Vec<f32>,
    pub offset: f32,
}

impl Default for LineStyle {
    fn default() -> Self {
        Self {
            width: 1.,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.,
            dash: Vec::new(),
            offset: 0.,
        }
    }
}

/// `shadowColor`, `shadowBlur` and the offsets.
#[derive(Clone, Copy)]
pub struct Shadow {
    pub color: Color,
    pub blur: f32,
    pub x: f32,
    pub y: f32,
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow {
            color: Color::TRANSPARENT,
            blur: 0.,
            x: 0.,
            y: 0.,
        }
    }
}

impl Shadow {
    /// A shadow is drawn when its color can be seen and it moves or blurs the shape: a shadow
    /// exactly behind an opaque shape would be invisible.
    pub fn is_visible(&self) -> bool {
        self.color.alpha() > 0. && (self.blur > 0. || self.x != 0. || self.y != 0.)
    }
}

/// What a drawing command does with a path.
#[derive(Clone, Copy, PartialEq)]
pub enum Op {
    Fill(FillRule),
    Stroke,
}

impl Op {
    /// The rule that decides which points a painted path covers: a stroke's outline is filled
    /// non-zero.
    pub fn rule(self) -> FillRule {
        match self {
            Op::Fill(rule) => rule,
            Op::Stroke => FillRule::Winding,
        }
    }
}

/// One canvas, with the stores a paint reads: its shaders and the fonts.
pub struct Scene<'a> {
    pub canvas: &'a mut Canvas2d,
    pub gradients: &'a HashMap<CanvasId, GradientSpec>,
    pub patterns: &'a HashMap<CanvasId, PatternSpec>,
    pub fonts: &'a mut FontStack,
}

impl Scene<'_> {
    /// Fills or strokes `path` (in user space) with `source`.
    pub fn paint(&mut self, path: &PathBuilder, op: Op, source: &PaintSource) {
        if !self.canvas.usable() {
            return;
        }
        let Some(geometry) = device_geometry(path, &self.canvas.draw, op) else {
            return;
        };
        let conic = match source {
            PaintSource::Gradient(id) => self.gradients.get(id),
            _ => None,
        };
        if let Some(gradient) = conic.filter(|g| matches!(g.kind, GradientKind::Conic { .. })) {
            self.canvas.paint_conic(gradient, &geometry, op.rule());
            return;
        }
        let Some(shader) = source.shader(&self.canvas.draw, self.gradients, self.patterns) else {
            return;
        };
        self.canvas.fill_geometry(shader, &geometry, op.rule());
    }

    /// Paints a fetched image into the canvas, as `drawImage` does.
    pub fn draw_image(&mut self, source: &Arc<Pixmap>, tainted: bool, place: Place) {
        let canvas = &mut *self.canvas;
        if !canvas.usable() {
            return;
        }
        canvas.tainted |= tainted;
        let Some(part) = place.crop(source) else {
            return;
        };
        let draw = &canvas.draw;
        let transform = draw
            .transform
            .pre_concat(Transform::from_translate(place.x, place.y))
            .pre_concat(Transform::from_scale(
                place.width / place.source_width,
                place.height / place.source_height,
            ));
        let pixel = PixmapPaint {
            opacity: draw.alpha,
            blend_mode: draw.composite,
            quality: match draw.smoothing {
                true => draw.quality,
                false => FilterQuality::Nearest,
            },
        };
        if let Some(pixmap) = canvas.pixmap.as_mut() {
            let clip = draw.clip.as_deref();
            pixmap.draw_pixmap(0, 0, Pixmap::as_ref(&part), &pixel, transform, clip);
            canvas.dirty = true;
        }
    }

    /// Draws `content` with the state's paint, as `fillText` or `strokeText` do.
    pub fn draw_text(&mut self, content: &str, placement: Placement, op: Op) {
        if !self.canvas.usable() {
            return;
        }
        let draw = &self.canvas.draw;
        let source = draw.paint_for(op);
        let layout = Layout::of(self.fonts, content, &draw.text);
        let Some(outline) = placement.outline(self.fonts, &layout) else {
            return;
        };
        let mut builder = PathBuilder::new();
        builder.push_path(&outline);
        self.paint(&builder, op, &source);
    }

    /// `measureText`: the advances and boxes of `content` in the state's font.
    pub fn measure(&mut self, content: &str) -> Metrics {
        let style = self.canvas.draw.text.clone();
        Measurer::measure(self.fonts, content, &style)
    }
}

impl Canvas2d {
    /// Lays `shader` down over `geometry` (device space) with the state's blending, clip,
    /// shadow and filter.
    pub(crate) fn fill_geometry(&mut self, shader: Shader<'_>, geometry: &Path, rule: FillRule) {
        let draw = &self.draw;
        let mut paint = Paint {
            shader,
            blend_mode: draw.composite,
            anti_alias: true,
            ..Paint::default()
        };
        paint.shader.apply_opacity(draw.alpha);
        let Some(pixmap) = self.pixmap.as_mut() else {
            return;
        };
        if draw.shadow.is_visible() {
            effects::draw_shadow(pixmap, geometry, rule, draw);
        }
        match effects::blur_of(&draw.filter) {
            Some(sigma) => effects::draw_blurred(pixmap, geometry, &paint, rule, sigma, draw),
            None => pixmap.fill_path(
                geometry,
                &paint,
                rule,
                Transform::identity(),
                draw.clip.as_deref(),
            ),
        }
        self.dirty = true;
    }
}

/// Where a `drawImage` puts an image: the source rectangle in its own pixels, the destination in
/// user space.
#[derive(Clone, Copy)]
pub struct Place {
    pub source_x: f32,
    pub source_y: f32,
    pub source_width: f32,
    pub source_height: f32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Place {
    /// `drawImage`'s three forms, by the numbers that follow the image: a point, a point and a
    /// size, or a source rectangle and a destination one.
    pub fn from_numbers(numbers: &[f32], width: u32, height: u32) -> Option<Place> {
        let (width, height) = (width as f32, height as f32);
        let whole = Place {
            source_x: 0.,
            source_y: 0.,
            source_width: width,
            source_height: height,
            x: 0.,
            y: 0.,
            width,
            height,
        };
        Some(match *numbers {
            [x, y] => Place { x, y, ..whole },
            [x, y, w, h] => Place {
                x,
                y,
                width: w,
                height: h,
                ..whole
            },
            [sx, sy, sw, sh, x, y, w, h] => Place {
                source_x: sx,
                source_y: sy,
                source_width: sw,
                source_height: sh,
                x,
                y,
                width: w,
                height: h,
            },
            _ => return None,
        })
    }

    /// The part of `source` the source rectangle names; the whole image is shared, not copied.
    fn crop(&self, source: &Arc<Pixmap>) -> Option<Arc<Pixmap>> {
        let whole = self.source_x == 0.
            && self.source_y == 0.
            && self.source_width == source.width() as f32
            && self.source_height == source.height() as f32;
        if whole {
            return Some(source.clone());
        }
        let rect = tiny_skia::IntRect::from_xywh(
            self.source_x as i32,
            self.source_y as i32,
            self.source_width.ceil() as u32,
            self.source_height.ceil() as u32,
        )?;
        source.clone_rect(rect).map(Arc::new)
    }
}

/// The path in device space; a stroke is widened in user space first, so the transform shapes
/// the pen.
pub fn device_geometry(path: &PathBuilder, draw: &DrawState, op: Op) -> Option<Path> {
    let path = path.clone().finish()?;
    match op {
        Op::Fill(_) => path.transform(draw.transform),
        Op::Stroke => {
            let stroke = Stroke {
                width: draw.line.width,
                line_cap: draw.line.cap,
                line_join: draw.line.join,
                miter_limit: draw.line.miter,
                dash: StrokeDash::new(draw.line.dash.clone(), draw.line.offset),
            };
            let scale = draw.transform.get_scale().0.max(0.01);
            path.stroke(&stroke, scale)?.transform(draw.transform)
        }
    }
}

/// `isPointInPath` and `isPointInStroke`: whether the point, in canvas coordinates, is covered by
/// the path as `op` would paint it.
pub fn hit_test(path: &PathBuilder, draw: &DrawState, op: Op, point: (f32, f32)) -> bool {
    device_geometry(path, draw, op)
        .is_some_and(|geometry| covers(&geometry, op.rule(), point.0, point.1))
}
