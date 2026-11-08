//! What `fillStyle` and `strokeStyle` can hold: a color, a `CanvasGradient` or a
//! `CanvasPattern`, and the tiny-skia shaders they become.
//!
//! tiny-skia 0.11 limits two things the specification asks for: its radial gradient starts at
//! radius 0 (`createRadialGradient`'s `r0` is kept but not drawn), and a pattern has one spread
//! mode for both axes (`repeat-x` and `repeat-y` repeat on both).

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::Arc;

use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Paint, Path, PathBuilder,
    Pattern, Pixmap, Point, RadialGradient, Shader, SpreadMode, Transform,
};

use crate::canvas::context2d::{Canvas2d, DrawState};
use crate::canvas::store::CanvasId;

/// What `fillStyle = value` holds: a color with its CSS spelling, or a gradient or pattern.
#[derive(Clone, PartialEq)]
pub enum PaintSource {
    Color(Color, String),
    Gradient(CanvasId),
    Pattern(CanvasId),
}

impl PaintSource {
    pub fn black() -> Self {
        PaintSource::Color(Color::BLACK, "#000000".to_owned())
    }

    /// The shader that paints with this source under `draw`'s transform.
    pub fn shader<'a>(
        &'a self,
        draw: &DrawState,
        gradients: &'a HashMap<CanvasId, GradientSpec>,
        patterns: &'a HashMap<CanvasId, PatternSpec>,
    ) -> Option<Shader<'a>> {
        Some(match self {
            PaintSource::Color(color, _) => Shader::SolidColor(*color),
            PaintSource::Gradient(id) => gradients.get(id)?.shader(draw.transform)?,
            PaintSource::Pattern(id) => patterns.get(id)?.shader(draw.transform),
        })
    }
}

/// A CSS color as the canvas holds it, with the spelling `fillStyle` reads back.
pub fn parse_color(value: &str) -> Option<(Color, String)> {
    let [r, g, b, a] = crate::css::color(value)?;
    let color = Color::from_rgba(r, g, b, a)?;
    Some((color, css_of(&color)))
}

/// A color as `fillStyle` hands it back: a hex string, or `rgba()` when it is not opaque.
pub fn css_of(color: &Color) -> String {
    let channel = |value: f32| (value * 255.).round().clamp(0., 255.) as u8;
    let (r, g, b) = (
        channel(color.red()),
        channel(color.green()),
        channel(color.blue()),
    );
    let alpha = color.alpha();
    if alpha >= 1. {
        return format!("#{r:02x}{g:02x}{b:02x}");
    }
    let alpha = format!("{alpha:.2}");
    let alpha = alpha.trim_end_matches('0').trim_end_matches('.');
    format!("rgba({r}, {g}, {b}, {alpha})")
}

/// A `CanvasGradient`'s geometry and stops, kept apart from the object so it stays live.
pub struct GradientSpec {
    pub kind: GradientKind,
    /// Ordered by offset (see [`add_stop`](Self::add_stop)).
    pub stops: Vec<(f32, Color)>,
}

/// The three gradients `createLinearGradient`, `createRadialGradient` and `createConicGradient`
/// make.
pub enum GradientKind {
    Linear {
        from: Point,
        to: Point,
    },
    Radial {
        from: Point,
        to: Point,
        to_radius: f32,
    },
    Conic {
        center: Point,
        angle: f32,
    },
}

/// How many wedges stand for a conic gradient's sweep.
const CONIC_WEDGES: usize = 90;

impl GradientSpec {
    /// Adds a color stop, keeping the list ordered by offset; stops at one offset keep the
    /// order they were added in.
    pub fn add_stop(&mut self, offset: f32, color: Color) {
        let at = self.stops.partition_point(|(known, _)| *known <= offset);
        self.stops.insert(at, (offset, color));
    }

    fn stops(&self) -> Vec<GradientStop> {
        let stops = self.stops.iter();
        stops
            .map(|(offset, color)| GradientStop::new(*offset, *color))
            .collect()
    }

    fn shader(&self, transform: Transform) -> Option<Shader<'static>> {
        let stops = self.stops();
        match self.kind {
            GradientKind::Linear { from, to } => {
                LinearGradient::new(from, to, stops, SpreadMode::Pad, transform)
            }
            GradientKind::Radial {
                from,
                to,
                to_radius,
            } => RadialGradient::new(from, to, to_radius, stops, SpreadMode::Pad, transform),
            // A conic gradient is painted as wedges, where the paint knows the shape it covers.
            GradientKind::Conic { .. } => None,
        }
    }

    /// The color at `t` around the circle, for the wedges of a conic gradient.
    fn color_at(&self, t: f32) -> Color {
        let stops = &self.stops;
        let Some(first) = stops.first() else {
            return Color::TRANSPARENT;
        };
        if t <= first.0 {
            return first.1;
        }
        let between = stops.windows(2).find(|pair| t <= pair[1].0);
        let Some([(from_at, from), (to_at, to)]) = between.map(|pair| [pair[0], pair[1]]) else {
            return stops.last().map_or(Color::BLACK, |stop| stop.1);
        };
        let span = to_at - from_at;
        let mix = if span > 0. { (t - from_at) / span } else { 1. };
        let lerp = |a: f32, b: f32| a + (b - a) * mix;
        Color::from_rgba(
            lerp(from.red(), to.red()),
            lerp(from.green(), to.green()),
            lerp(from.blue(), to.blue()),
            lerp(from.alpha(), to.alpha()),
        )
        .unwrap_or(Color::BLACK)
    }
}

/// A `CanvasPattern`: an image and how it repeats.
pub struct PatternSpec {
    pub pixmap: Arc<Pixmap>,
    pub repetition: Repetition,
    pub transform: Transform,
}

impl PatternSpec {
    fn shader(&self, transform: Transform) -> Shader<'_> {
        let spread = match self.repetition {
            Repetition::NoRepeat => SpreadMode::Pad,
            _ => SpreadMode::Repeat,
        };
        Pattern::new(
            Pixmap::as_ref(&self.pixmap),
            spread,
            FilterQuality::Bilinear,
            1.,
            transform.pre_concat(self.transform),
        )
    }
}

/// `createPattern`'s repetition argument.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Repetition {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

impl Repetition {
    pub const NAMES: &'static [(&'static str, Repetition)] = &[
        ("repeat", Repetition::Repeat),
        ("repeat-x", Repetition::RepeatX),
        ("repeat-y", Repetition::RepeatY),
        ("no-repeat", Repetition::NoRepeat),
    ];
}

impl Canvas2d {
    /// A conic gradient, painted as a fan of wedges in a layer and laid down once: tiny-skia has
    /// no conic shader, and one fill per wedge would show seams under blending.
    pub fn paint_conic(&mut self, gradient: &GradientSpec, geometry: &Path, rule: FillRule) {
        let GradientKind::Conic { center, angle } = gradient.kind else {
            return;
        };
        let Some(layer) = self.conic_layer(gradient, center, angle) else {
            return;
        };
        let wedges = Pattern::new(
            Pixmap::as_ref(&layer),
            SpreadMode::Pad,
            FilterQuality::Nearest,
            1.,
            Transform::identity(),
        );
        self.fill_geometry(wedges, geometry, rule);
    }

    /// The wedges of `gradient` around `center` (in user space), the canvas's size.
    fn conic_layer(&self, gradient: &GradientSpec, center: Point, angle: f32) -> Option<Pixmap> {
        let mut layer = Pixmap::new(self.width, self.height)?;
        let mut middle = center;
        self.draw.transform.map_point(&mut middle);
        let reach = self.farthest_corner(middle) + 2.;
        let step = TAU / CONIC_WEDGES as f32;
        for wedge in 0..CONIC_WEDGES {
            let from = angle + step * wedge as f32;
            let color = gradient.color_at((wedge as f32 + 0.5) / CONIC_WEDGES as f32);
            let mut path = PathBuilder::new();
            path.move_to(middle.x, middle.y);
            for edge in 0..=4 {
                let turn = from + step * edge as f32 / 4.;
                path.line_to(middle.x + reach * turn.cos(), middle.y + reach * turn.sin());
            }
            path.close();
            let paint = Paint {
                shader: Shader::SolidColor(color),
                anti_alias: false,
                ..Paint::default()
            };
            if let Some(path) = path.finish() {
                let rule = FillRule::Winding;
                layer.fill_path(&path, &paint, rule, Transform::identity(), None);
            }
        }
        Some(layer)
    }

    /// How far the canvas's farthest corner is from `point`.
    fn farthest_corner(&self, point: Point) -> f32 {
        let (width, height) = (self.width as f32, self.height as f32);
        [(0., 0.), (width, 0.), (0., height), (width, height)]
            .into_iter()
            .map(|(x, y)| (x - point.x).hypot(y - point.y))
            .fold(0., f32::max)
    }
}
