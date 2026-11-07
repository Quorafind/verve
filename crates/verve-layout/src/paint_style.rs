//! What a box looks like, read from its computed style: backgrounds, border, corner radii,
//! shadows and outline.
//!
//! Sizes that depend on the box (a `50%` radius, a gradient's line) are kept as the style gave
//! them and resolved when the box has its final size, in [`Paint::radii`] and [`Paint::fills`].

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use verve_dom::css::color_with;
use verve_dom::{NodeId, Snapshot};

use crate::background::{Layer, read_layers};
use crate::display_list::{Border, BorderLine, Fill, ImageKey, RasterSpec, Rgba, Shadow};
use crate::gradient::{Gradient, gradient_line};
use crate::shadow;
use crate::value::{Dim, Units, length_percent, pixels, words};

/// Set in the `ImageKey` of a rasterised gradient.
pub(crate) const RASTER_KEY_BIT: u64 = 1 << 63;

const CORNERS: [&str; 4] = ["top-left", "top-right", "bottom-right", "bottom-left"];
const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// A line around the outside of a box that takes no room.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Outline {
    pub width: f32,
    pub line: BorderLine,
    pub color: Rgba,
    pub offset: f32,
}

/// The look of one box.
pub(crate) struct Paint {
    pub background_color: Rgba,
    /// The gradients of `background-image`, the first layer on top.
    pub layers: Vec<Layer>,
    pub border: Border,
    /// Each corner's horizontal and vertical radius.
    radii: [(Dim, Dim); 4],
    pub shadows: Vec<Shadow>,
    pub outline: Option<Outline>,
}

impl Paint {
    /// The look of a box that draws nothing: the anonymous boxes that hold inline content.
    pub(crate) fn none() -> Paint {
        Paint {
            background_color: [0.; 4],
            layers: Vec::new(),
            border: Border::NONE,
            radii: [(Dim::Px(0.), Dim::Px(0.)); 4],
            shadows: Vec::new(),
            outline: None,
        }
    }

    pub(crate) fn read(snapshot: &Snapshot, node: NodeId, units: &Units, color: Rgba) -> Paint {
        let text = |property: &str| snapshot.style(node, property);
        let colored = |property: &str| {
            text(property)
                .and_then(|value| color_with(value, color))
                .unwrap_or(color)
        };
        let mut border = Border::NONE;
        for (side, name) in SIDES.iter().enumerate() {
            let line = line_of(text(&format!("border-{name}-style")));
            border.styles[side] = line;
            border.widths[side] = border_side_width(
                text(&format!("border-{name}-width")),
                text(&format!("border-{name}-style")),
                units,
            );
            border.colors[side] = colored(&format!("border-{name}-color"));
        }
        let background_color = text("background-color")
            .and_then(|value| color_with(value, color))
            .unwrap_or([0.; 4]);
        let layers = read_layers(snapshot, node, color, units);
        let radii = CORNERS.map(|corner| radius(text(&format!("border-{corner}-radius")), units));
        let shadows = text("box-shadow")
            .map(|value| shadow::parse(value, color, units))
            .unwrap_or_default();
        Paint {
            background_color,
            layers,
            border,
            radii,
            shadows,
            outline: outline(&text, units, color),
        }
    }

    /// The corner radii in a box of `width` by `height`: an elliptical corner takes the smaller of
    /// its two radii, and radii that would overlap are scaled down together (CSS Backgrounds 3,
    /// "Corner Overlap").
    pub(crate) fn radii(&self, width: f32, height: f32) -> [f32; 4] {
        let mut resolved = self.radii.map(|(x, y)| {
            let x = resolve(x, width);
            let y = resolve(y, height);
            x.min(y).max(0.)
        });
        let [tl, tr, br, bl] = resolved;
        let ratios = [
            width / (tl + tr),
            height / (tr + br),
            width / (br + bl),
            height / (bl + tl),
        ];
        let scale = ratios
            .into_iter()
            .filter(|ratio| ratio.is_finite())
            .fold(1_f32, f32::min);
        if scale < 1. {
            resolved = resolved.map(|radius| radius * scale);
        }
        resolved
    }

    /// The corner radii that are lengths, in pixels; a percentage is none. An inline element has no
    /// one box to take a percentage of.
    pub(crate) fn px_radii(&self) -> [f32; 4] {
        self.radii.map(|(x, y)| {
            let px = |dim: Dim| if let Dim::Px(px) = dim { px } else { 0. };
            px(x).min(px(y)).max(0.)
        })
    }

    /// The backgrounds back to front for a box of `size`. Gradients the shell cannot paint as
    /// they are become [`Fill::Raster`] with a spec added to `rasters`. `padding` is what layout
    /// gave the box (top, right, bottom, left), where `background-origin: content-box` starts.
    pub(crate) fn fills(
        &self,
        size: [f32; 2],
        padding: [f32; 4],
        rasters: &mut Vec<RasterSpec>,
    ) -> Vec<Fill> {
        let mut fills = Vec::new();
        if self.background_color[3] > 0. {
            fills.push(Fill::Color(self.background_color));
        }
        if size[0] <= 0. || size[1] <= 0. {
            return fills;
        }
        for layer in self.layers.iter().rev() {
            match layer {
                Layer::Gradient(gradient) => fills.push(fill_of(gradient, size, rasters)),
                Layer::Picture(picture) => {
                    let [top, right, bottom, left] = self.border.widths;
                    fills.extend(picture.fill(size, [top, right, bottom, left], padding));
                }
            }
        }
        fills
    }
}

fn resolve(dim: Dim, basis: f32) -> f32 {
    match dim {
        Dim::Px(px) => px,
        Dim::Percent(f) => f * basis,
        Dim::Auto => 0.,
    }
}

fn fill_of(layer: &Gradient, size: [f32; 2], rasters: &mut Vec<RasterSpec>) -> Fill {
    if let Some(fill) = linear_fill(layer, size) {
        return fill;
    }
    let mut hasher = DefaultHasher::new();
    layer.fingerprint().hash(&mut hasher);
    size.map(f32::to_bits).hash(&mut hasher);
    // The top bit keeps these apart from the pictures the page loads.
    let key = ImageKey(hasher.finish() | RASTER_KEY_BIT);
    if !rasters.iter().any(|spec| spec.key == key) {
        rasters.push(RasterSpec {
            key,
            gradient: layer.clone(),
            size,
        });
    }
    Fill::Raster(key)
}

/// A linear gradient the shell paints as it is: two stops at any angle, or more along an axis.
fn linear_fill(layer: &Gradient, size: [f32; 2]) -> Option<Fill> {
    if layer.is_repeating() {
        return None;
    }
    let angle = layer.line_angle(size)?;
    let axis_aligned = (angle.rem_euclid(90.)).abs() < 1e-3;
    if layer.stops().len() != 2 && !axis_aligned {
        return None;
    }
    let (_, length) = gradient_line(angle, size);
    Some(Fill::Linear {
        angle,
        stops: layer.stops_along(length),
    })
}

fn line_of(text: Option<&str>) -> BorderLine {
    match text.map(str::trim) {
        Some("solid") => BorderLine::Solid,
        Some("dashed") => BorderLine::Dashed,
        Some("dotted") => BorderLine::Dotted,
        Some("double") => BorderLine::Double,
        Some("groove") => BorderLine::Groove,
        Some("ridge") => BorderLine::Ridge,
        Some("inset") => BorderLine::Inset,
        Some("outset") => BorderLine::Outset,
        Some("hidden") => BorderLine::Hidden,
        _ => BorderLine::None,
    }
}

/// A border side's width. It is nothing when its style is `none` or `hidden` (or missing, which
/// is `none`), and `thin`, `medium`, `thick` are 1, 3 and 5 pixels.
pub(crate) fn border_side_width(width: Option<&str>, line: Option<&str>, units: &Units) -> f32 {
    if matches!(line_of(line), BorderLine::None | BorderLine::Hidden) {
        return 0.;
    }
    match width.map(str::trim) {
        None | Some("medium") => 3.,
        Some("thin") => 1.,
        Some("thick") => 5.,
        Some(text) => pixels(text, units).filter(|px| *px >= 0.).unwrap_or(0.),
    }
}

fn radius(text: Option<&str>, units: &Units) -> (Dim, Dim) {
    let Some(text) = text else {
        return (Dim::Px(0.), Dim::Px(0.));
    };
    let parts = words(text);
    let read = |at: usize| {
        parts
            .get(at)
            .and_then(|part| length_percent(part, units))
            .filter(|dim| !matches!(dim, Dim::Px(px) if *px < 0.))
    };
    let x = read(0).unwrap_or(Dim::Px(0.));
    (x, read(1).unwrap_or(x))
}

fn outline<'a>(
    text: &impl Fn(&str) -> Option<&'a str>,
    units: &Units,
    color: Rgba,
) -> Option<Outline> {
    // `auto` is the focus ring: drawn as a solid line.
    let style = text("outline-style").map(|style| {
        if style.trim() == "auto" {
            "solid"
        } else {
            style
        }
    });
    let line = line_of(style);
    let width = border_side_width(text("outline-width"), style, units);
    if line == BorderLine::None || line == BorderLine::Hidden || width <= 0. {
        return None;
    }
    Some(Outline {
        width,
        line,
        color: text("outline-color")
            .and_then(|value| color_with(value, color))
            .unwrap_or(color),
        offset: text("outline-offset")
            .and_then(|value| pixels(value, units))
            .unwrap_or(0.),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gradient::{self, LinearDirection};

    fn direction_of(layer: &Gradient) -> Option<LinearDirection> {
        match layer {
            Gradient::Linear { direction, .. } => Some(*direction),
            _ => None,
        }
    }

    fn units() -> Units {
        Units::new(16., 16., (800., 600.))
    }

    fn gradient(text: &str) -> Gradient {
        gradient::parse(text, [0., 0., 0., 1.], &units()).expect("a gradient")
    }

    #[test]
    fn border_widths_follow_their_style_and_keywords() {
        let units = units();
        assert_eq!(border_side_width(Some("2px"), Some("solid"), &units), 2.);
        assert_eq!(border_side_width(Some("2px"), Some("none"), &units), 0.);
        assert_eq!(border_side_width(Some("2px"), None, &units), 0.);
        assert_eq!(border_side_width(None, Some("dashed"), &units), 3.);
        assert_eq!(border_side_width(Some("thin"), Some("solid"), &units), 1.);
        assert_eq!(border_side_width(Some("thick"), Some("solid"), &units), 5.);
        assert_eq!(border_side_width(Some("0.5em"), Some("solid"), &units), 8.);
    }

    fn paint_with_radii(radii: [(Dim, Dim); 4]) -> Paint {
        Paint {
            background_color: [0.; 4],
            layers: Vec::new(),
            border: Border::NONE,
            radii,
            shadows: Vec::new(),
            outline: None,
        }
    }

    #[test]
    fn radii_take_the_smaller_ellipse_axis_and_never_overlap() {
        let round = (Dim::Percent(0.5), Dim::Percent(0.5));
        let paint = paint_with_radii([round; 4]);
        assert_eq!(paint.radii(100., 100.), [50.; 4]);
        // 50% of a 100 by 40 box is 50 across and 20 down: the corner takes 20.
        assert_eq!(paint.radii(100., 40.), [20.; 4]);
        let big = (Dim::Px(30.), Dim::Px(30.));
        let overlapping = paint_with_radii([big; 4]);
        assert_eq!(overlapping.radii(40., 40.), [20.; 4]);
        let ellipse = paint_with_radii([(Dim::Px(10.), Dim::Px(20.)); 4]);
        assert_eq!(ellipse.radii(100., 100.), [10.; 4]);
    }

    #[test]
    fn two_stop_gradients_and_axis_aligned_ones_stay_linear() {
        let paint = Paint {
            layers: [
                "linear-gradient(45deg, red, blue)",
                "linear-gradient(to right, red, lime, blue)",
                "linear-gradient(30deg, red, lime, blue)",
                "radial-gradient(red, blue)",
                "repeating-linear-gradient(red, blue 10px)",
            ]
            .map(|text| Layer::Gradient(gradient(text)))
            .to_vec(),
            ..paint_with_radii([(Dim::Px(0.), Dim::Px(0.)); 4])
        };
        let mut rasters = Vec::new();
        let fills = paint.fills([100., 50.], [0.; 4], &mut rasters);
        // Back to front: the last layer is first.
        assert_eq!(fills.len(), 5);
        assert!(matches!(fills[0], Fill::Raster(_)));
        assert!(matches!(fills[1], Fill::Raster(_)));
        assert!(matches!(fills[2], Fill::Raster(_)));
        assert!(matches!(fills[3], Fill::Linear { .. }));
        assert!(matches!(fills[4], Fill::Linear { .. }));
        assert_eq!(rasters.len(), 3);
        assert_eq!(rasters[0].size, [100., 50.]);
        // The same gradient at the same size is the same picture.
        let mut again = Vec::new();
        paint.fills([100., 50.], [0.; 4], &mut again);
        assert_eq!(again, rasters);
    }

    #[test]
    fn a_gradient_with_a_corner_direction_is_read_against_the_box() {
        let layer = gradient("linear-gradient(to bottom right, red, blue)");
        assert_eq!(
            direction_of(&layer),
            Some(LinearDirection::Corner {
                right: true,
                bottom: true
            })
        );
        let mut rasters = Vec::new();
        let Fill::Linear { angle, .. } = fill_of(&layer, [100., 100.], &mut rasters) else {
            panic!("two stops stay linear");
        };
        assert!((angle - 135.).abs() < 1e-3);
    }
}
