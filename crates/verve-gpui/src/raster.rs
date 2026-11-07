//! Gradients drawn into pictures: the ones a display list asks the shell to rasterise
//! ([`verve_layout::RasterSpec`]) because the GPUI painter has no shader for them: radial and
//! conic gradients, and repeating ones.
//!
//! Every pixel is worked out from its centre, as the gradient function says; colors are mixed
//! premultiplied, as CSS does, and the picture is premultiplied RGBA.

use verve_layout::{EllipseSize, Extent, Gradient, RadialShape, RasterSpec, Rgba, gradient_line};

/// The most device pixels a picture has along a side. A taller or wider box is drawn at a lower
/// scale and the painter stretches the picture, so a gradient over a page-high element at a
/// scale of 2 stays a few megabytes and inside GPUI's atlas.
pub const MAX_SIDE: u32 = 2048;

/// A picture of `width` x `height` device pixels, premultiplied RGBA, row by row.
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Draws `spec`'s gradient over its box, `scale` device pixels to a CSS pixel, with no more than
/// [`MAX_SIDE`] pixels on a side.
pub fn rasterize(spec: &RasterSpec, scale: f32) -> Raster {
    let [w, h] = spec.size;
    let width = ((w * scale).ceil() as u32).clamp(1, MAX_SIDE);
    let height = ((h * scale).ceil() as u32).clamp(1, MAX_SIDE);
    let line = Line::new(&spec.gradient, spec.size);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height {
        for column in 0..width {
            // The pixel's centre, in the box's CSS pixels.
            let x = (column as f32 + 0.5) * w / width as f32;
            let y = (row as f32 + 0.5) * h / height as f32;
            let [r, g, b, a] = line.color_at((line.position)(x, y));
            rgba.extend([r, g, b, a].map(|c| (c.clamp(0., 1.) * 255.).round() as u8));
        }
    }
    Raster {
        width,
        height,
        rgba,
    }
}

/// A gradient ready to be asked about points: how far along its line a point is, and the stops.
struct Line<'a> {
    position: Box<dyn Fn(f32, f32) -> f32 + 'a>,
    /// The stops with their colors premultiplied, and where they are along the line.
    stops: Vec<(Rgba, f32)>,
    repeating: bool,
}

impl<'a> Line<'a> {
    fn new(gradient: &'a Gradient, size: [f32; 2]) -> Self {
        let [w, h] = size;
        let (position, length): (Box<dyn Fn(f32, f32) -> f32 + 'a>, f32) = match gradient {
            Gradient::Linear { .. } => {
                let angle = gradient.line_angle(size).unwrap_or(180.);
                let ([dx, dy], length) = gradient_line(angle, size);
                let (cx, cy) = (w / 2., h / 2.);
                (
                    Box::new(move |x, y| {
                        if length > 0. {
                            ((x - cx) * dx + (y - cy) * dy) / length + 0.5
                        } else {
                            0.
                        }
                    }),
                    length,
                )
            }
            Gradient::Radial { shape, center, .. } => {
                let (cx, cy) = (center[0].along(w), center[1].along(h));
                let [rx, ry] = radii(shape, [cx, cy], size);
                (
                    Box::new(move |x, y| {
                        let (dx, dy) = ((x - cx) / rx.max(1e-6), (y - cy) / ry.max(1e-6));
                        (dx * dx + dy * dy).sqrt()
                    }),
                    rx,
                )
            }
            Gradient::Conic { from, center, .. } => {
                let (cx, cy) = (center[0].along(w), center[1].along(h));
                let from = *from;
                (
                    Box::new(move |x, y| {
                        let turns = (x - cx).atan2(-(y - cy)).to_degrees() - from;
                        turns.rem_euclid(360.) / 360.
                    }),
                    1.,
                )
            }
        };
        let stops = gradient
            .stops_along(length)
            .into_iter()
            .map(|(color, at)| (premultiply(color), at))
            .collect();
        Self {
            position,
            stops,
            repeating: gradient.is_repeating(),
        }
    }

    /// The premultiplied color `t` along the line.
    fn color_at(&self, mut t: f32) -> Rgba {
        let (first, last) = (self.stops[0].1, self.stops[self.stops.len() - 1].1);
        if self.repeating && last > first {
            t = first + (t - first).rem_euclid(last - first);
        }
        if t <= first {
            return self.stops[0].0;
        }
        for pair in self.stops.windows(2) {
            let ((from, at), (to, then)) = (pair[0], pair[1]);
            if t < then {
                // A hard stop (two at one place) is a jump, not a mix.
                let mix = if then > at {
                    (t - at) / (then - at)
                } else {
                    1.
                };
                return [0, 1, 2, 3].map(|i| from[i] + (to[i] - from[i]) * mix);
            }
        }
        self.stops[self.stops.len() - 1].0
    }
}

fn premultiply([r, g, b, a]: Rgba) -> Rgba {
    [r * a, g * a, b * a, a]
}

/// The radii of a radial gradient's ending shape, in the box of `size` with its centre at `at`:
/// CSS's `closest-side`, `farthest-corner` and the rest.
fn radii(shape: &RadialShape, at: [f32; 2], size: [f32; 2]) -> [f32; 2] {
    let near = [
        at[0].abs().min((size[0] - at[0]).abs()),
        at[1].abs().min((size[1] - at[1]).abs()),
    ];
    let far = [
        at[0].abs().max((size[0] - at[0]).abs()),
        at[1].abs().max((size[1] - at[1]).abs()),
    ];
    let corner = |sides: [f32; 2]| sides.map(|side| side * std::f32::consts::SQRT_2);
    let circle = |value: f32| [value, value];
    let by = |extent: Extent, ellipse: bool| match (extent, ellipse) {
        (Extent::Radius(r), _) => circle(r),
        (Extent::ClosestSide, true) => near,
        (Extent::ClosestSide, false) => circle(near[0].min(near[1])),
        (Extent::FarthestSide, true) => far,
        (Extent::FarthestSide, false) => circle(far[0].max(far[1])),
        (Extent::ClosestCorner, true) => corner(near),
        (Extent::ClosestCorner, false) => circle(near[0].hypot(near[1])),
        (Extent::FarthestCorner, true) => corner(far),
        (Extent::FarthestCorner, false) => circle(far[0].hypot(far[1])),
    };
    match shape {
        RadialShape::Circle(extent) => by(*extent, false),
        RadialShape::Ellipse(EllipseSize::Extent(extent)) => by(*extent, true),
        RadialShape::Ellipse(EllipseSize::Radii([x, y])) => [x.along(size[0]), y.along(size[1])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verve_layout::{ColorStop, GradientPosition};

    const RED: Rgba = [1., 0., 0., 1.];
    const BLUE: Rgba = [0., 0., 1., 1.];

    fn stops() -> Vec<ColorStop> {
        [RED, BLUE]
            .into_iter()
            .map(|color| ColorStop {
                color,
                position: None,
            })
            .collect()
    }

    fn pixel(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * raster.width + x) * 4) as usize;
        [
            raster.rgba[at],
            raster.rgba[at + 1],
            raster.rgba[at + 2],
            raster.rgba[at + 3],
        ]
    }

    fn spec(gradient: Gradient, size: [f32; 2]) -> RasterSpec {
        RasterSpec {
            key: verve_layout::ImageKey(1 << 63),
            gradient,
            size,
        }
    }

    #[test]
    fn a_radial_gradient_is_the_first_color_in_the_middle_and_the_last_at_the_corners() {
        let radial = Gradient::Radial {
            shape: RadialShape::Circle(Extent::FarthestCorner),
            center: [GradientPosition::Fraction(0.5); 2],
            stops: stops(),
            repeating: false,
        };
        let raster = rasterize(&spec(radial, [21., 21.]), 1.);
        assert_eq!((raster.width, raster.height), (21, 21));
        let middle = pixel(&raster, 10, 10);
        assert!(middle[0] > 240 && middle[2] < 15, "{middle:?}");
        let corner = pixel(&raster, 0, 0);
        assert!(corner[2] > 225 && corner[0] < 30, "{corner:?}");
    }

    #[test]
    fn a_picture_has_at_most_max_side_pixels_on_a_side_and_still_covers_the_box() {
        let linear = Gradient::Linear {
            direction: verve_layout::LinearDirection::Angle(180.),
            stops: stops(),
            repeating: false,
        };
        let raster = rasterize(&spec(linear, [100., 5000.]), 2.);
        assert_eq!((raster.width, raster.height), (200, MAX_SIDE));
        assert_eq!(raster.rgba.len(), 200 * MAX_SIDE as usize * 4);
        // The last row is still the end of the gradient: blue.
        assert!(pixel(&raster, 0, MAX_SIDE - 1)[2] > 250);
        assert!(pixel(&raster, 0, 0)[0] > 250);
    }

    #[test]
    fn a_repeating_linear_gradient_starts_over() {
        let repeating = Gradient::Linear {
            direction: verve_layout::LinearDirection::Angle(90.),
            stops: vec![
                ColorStop {
                    color: RED,
                    position: Some(GradientPosition::Px(0.)),
                },
                ColorStop {
                    color: BLUE,
                    position: Some(GradientPosition::Px(10.)),
                },
            ],
            repeating: true,
        };
        let raster = rasterize(&spec(repeating, [40., 4.]), 1.);
        // 0.5 px into each 10 px period it is red, and 9.5 px in it is almost blue.
        for period in 0..4 {
            assert!(pixel(&raster, period * 10, 1)[0] > 230);
            assert!(pixel(&raster, period * 10 + 9, 1)[2] > 230);
        }
    }

    #[test]
    fn a_conic_gradient_turns_clockwise_from_up() {
        let conic = Gradient::Conic {
            from: 0.,
            center: [GradientPosition::Fraction(0.5); 2],
            stops: stops(),
            repeating: false,
        };
        let raster = rasterize(&spec(conic, [20., 20.]), 1.);
        // Just right of straight up it is red; just left of it (almost a full turn) blue.
        assert!(pixel(&raster, 10, 2)[0] > 240);
        assert!(pixel(&raster, 9, 2)[2] > 240);
    }
}
