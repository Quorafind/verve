//! Shadows and `filter: blur()`: a shape painted into a layer, blurred, and laid under or over
//! the canvas.
//!
//! tiny-skia has no blur, so three box passes stand in for a Gaussian.

use tiny_skia::{FillRule, Paint, Path, Pixmap, PixmapPaint, Shader, Transform};

use crate::canvas::context2d::DrawState;

/// How far past a shape its shadow's blur reaches, in blur radii, and a margin for antialiasing.
const SHADOW_REACH: f32 = 2.;
const BLUR_REACH: f32 = 3.;
const MARGIN: f32 = 2.;

/// A shadow: the shape painted in the shadow's color, blurred, and laid under the paint.
pub fn draw_shadow(pixmap: &mut Pixmap, geometry: &Path, rule: FillRule, draw: &DrawState) {
    let shadow = &draw.shadow;
    let reach = shadow.blur * SHADOW_REACH + MARGIN;
    let Some(area) = Area::around(geometry, (shadow.x, shadow.y), reach, pixmap) else {
        return;
    };
    let Some(mut layer) = area.layer() else {
        return;
    };
    let paint = Paint {
        shader: Shader::SolidColor(shadow.color),
        anti_alias: true,
        ..Paint::default()
    };
    let shifted =
        Transform::from_translate(shadow.x - area.left as f32, shadow.y - area.top as f32);
    layer.fill_path(geometry, &paint, rule, shifted, None);
    blur_pixels(&mut layer, shadow.blur / 2.);
    let pixel = PixmapPaint {
        opacity: draw.alpha,
        blend_mode: draw.composite,
        ..PixmapPaint::default()
    };
    area.lay(pixmap, &layer, &pixel, draw.clip.as_deref());
}

/// `filter: blur(px)`: the paint drawn into a layer and blurred before it lands.
pub fn draw_blurred(
    pixmap: &mut Pixmap,
    geometry: &Path,
    paint: &Paint<'_>,
    rule: FillRule,
    sigma: f32,
    draw: &DrawState,
) {
    let Some(area) = Area::around(geometry, (0., 0.), sigma * BLUR_REACH + MARGIN, pixmap) else {
        return;
    };
    let Some(mut layer) = area.layer() else {
        return;
    };
    let shifted = Transform::from_translate(-(area.left as f32), -(area.top as f32));
    layer.fill_path(geometry, paint, rule, shifted, None);
    blur_pixels(&mut layer, sigma);
    let pixel = PixmapPaint {
        blend_mode: paint.blend_mode,
        ..PixmapPaint::default()
    };
    area.lay(pixmap, &layer, &pixel, draw.clip.as_deref());
}

/// The blur radius of the `filter` property, when it is one the canvas supports.
pub fn blur_of(filter: &str) -> Option<f32> {
    let rest = filter.trim().strip_prefix("blur(")?.strip_suffix(')')?;
    let length = crate::css::length(rest, crate::css::ROOT_FONT_SIZE)?;
    (length > 0.).then_some(length)
}

/// The part of the canvas a blurred layer covers, in whole pixels.
struct Area {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Area {
    /// The geometry's box moved by `offset` and grown by `reach`, cut to the canvas.
    fn around(geometry: &Path, offset: (f32, f32), reach: f32, pixmap: &Pixmap) -> Option<Area> {
        let bounds = geometry.bounds();
        let area = Area {
            left: (bounds.left() + offset.0 - reach).floor().max(0.) as i32,
            top: (bounds.top() + offset.1 - reach).floor().max(0.) as i32,
            right: (bounds.right() + offset.0 + reach)
                .ceil()
                .min(pixmap.width() as f32) as i32,
            bottom: (bounds.bottom() + offset.1 + reach)
                .ceil()
                .min(pixmap.height() as f32) as i32,
        };
        (area.right > area.left && area.bottom > area.top).then_some(area)
    }

    fn layer(&self) -> Option<Pixmap> {
        Pixmap::new(
            (self.right - self.left) as u32,
            (self.bottom - self.top) as u32,
        )
    }

    fn lay(
        &self,
        pixmap: &mut Pixmap,
        layer: &Pixmap,
        paint: &PixmapPaint,
        clip: Option<&tiny_skia::Mask>,
    ) {
        pixmap.draw_pixmap(
            self.left,
            self.top,
            layer.as_ref(),
            paint,
            Transform::identity(),
            clip,
        );
    }
}

/// Which way a box pass runs.
#[derive(Clone, Copy)]
enum Pass {
    Horizontal,
    Vertical,
}

/// A row or column of pixels: where it starts, how far apart its pixels are, and how many.
#[derive(Clone, Copy)]
struct Line {
    start: usize,
    stride: usize,
    length: usize,
}

/// Three box blurs standing in for a Gaussian of `sigma`, on premultiplied pixels.
pub fn blur_pixels(pixmap: &mut Pixmap, sigma: f32) {
    if sigma <= 0. {
        return;
    }
    // Three box passes of width `w` have the variance of a Gaussian with `w² = 4σ² + 1`.
    let radius = ((((4. * sigma * sigma + 1.).sqrt() - 1.) / 2.).round() as usize).max(1);
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    let mut scratch = vec![0u8; pixmap.data().len()];
    for _ in 0..3 {
        box_pass(
            pixmap.data(),
            &mut scratch,
            (width, height),
            radius,
            Pass::Horizontal,
        );
        box_pass(
            &scratch,
            pixmap.data_mut(),
            (width, height),
            radius,
            Pass::Vertical,
        );
    }
}

/// One box pass over every row (or column) of every channel.
fn box_pass(
    source: &[u8],
    out: &mut [u8],
    (width, height): (usize, usize),
    radius: usize,
    pass: Pass,
) {
    let lines = match pass {
        Pass::Horizontal => (0..height)
            .map(|row| Line {
                start: row * width,
                stride: 1,
                length: width,
            })
            .collect::<Vec<_>>(),
        Pass::Vertical => (0..width)
            .map(|column| Line {
                start: column,
                stride: width,
                length: height,
            })
            .collect(),
    };
    for line in lines.into_iter().filter(|line| line.length > 0) {
        box_line(source, out, line, radius.min(line.length - 1).max(1));
    }
}

/// One line of one pass: each pixel becomes the mean of the `2 × radius + 1` around it.
fn box_line(source: &[u8], out: &mut [u8], line: Line, radius: usize) {
    // Beyond the edges is transparent, so the divisor is always the whole window.
    let window = 2 * radius as u32 + 1;
    for channel in 0..4 {
        let at = |offset: usize| (line.start + offset * line.stride) * 4 + channel;
        let mut sum: u32 = (0..=radius)
            .map(|offset| u32::from(source[at(offset)]))
            .sum();
        for offset in 0..line.length {
            out[at(offset)] = (sum / window) as u8;
            if offset >= radius {
                sum -= u32::from(source[at(offset - radius)]);
            }
            if offset + radius + 1 < line.length {
                sum += u32::from(source[at(offset + radius + 1)]);
            }
        }
    }
}
