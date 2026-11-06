//! `linear-gradient()`, `radial-gradient()`, `conic-gradient()` and their `repeating-` forms, read
//! from the text of a `background-image` layer.
//!
//! A gradient keeps what the text said — directions as angles or corners, stop positions as
//! fractions or pixels — because what it comes to depends on the size of the box it fills;
//! [`Gradient::stops_along`] and [`Gradient::line_angle`] answer once the size is known.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::display_list::Rgba;
use crate::value::{
    Dim, Units, call_arguments, function_name, length_percent, number, split_outside, words,
};
use verve_dom::css::color_with;

/// A gradient as the text gave it.
#[derive(Clone, Debug, PartialEq)]
pub enum Gradient {
    Linear {
        direction: LinearDirection,
        stops: Vec<ColorStop>,
        repeating: bool,
    },
    Radial {
        shape: RadialShape,
        center: [GradientPosition; 2],
        stops: Vec<ColorStop>,
        repeating: bool,
    },
    Conic {
        /// Degrees clockwise from up.
        from: f32,
        center: [GradientPosition; 2],
        /// Positions are fractions of a turn.
        stops: Vec<ColorStop>,
        repeating: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinearDirection {
    /// Degrees clockwise from up: `0deg` points up and `90deg` right.
    Angle(f32),
    /// `to top right` and its kin: the line that runs to that corner of the box.
    Corner { right: bool, bottom: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadialShape {
    /// A circle of a given radius, or reaching an extent.
    Circle(Extent),
    Ellipse(EllipseSize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EllipseSize {
    Extent(Extent),
    Radii([GradientPosition; 2]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
    Radius(f32),
}

/// A place along an axis of the box: a fraction of it, or pixels from its start.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientPosition {
    Fraction(f32),
    Px(f32),
}

impl GradientPosition {
    /// Where it falls along a length.
    pub fn along(self, length: f32) -> f32 {
        match self {
            GradientPosition::Fraction(f) => f * length,
            GradientPosition::Px(px) => px,
        }
    }
}

/// A colour and, if the text said so, where it sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorStop {
    pub color: Rgba,
    pub position: Option<GradientPosition>,
}

/// Whether `text` is a gradient function.
pub fn is_gradient(text: &str) -> bool {
    function_name(text.trim()).is_some_and(|name| {
        matches!(
            name.trim_start_matches("repeating-"),
            "linear-gradient" | "radial-gradient" | "conic-gradient"
        )
    })
}

/// Reads one gradient. `current` is what `currentcolor` stands for. `None` for text that is no
/// gradient or that has fewer than two stops.
pub(crate) fn parse(text: &str, current: Rgba, units: &Units) -> Option<Gradient> {
    let name = function_name(text.trim())?;
    let repeating = name.starts_with("repeating-");
    let kind = name.trim_start_matches("repeating-");
    let arguments = call_arguments(text, &name)?;
    let parts = split_outside(arguments, ',');
    let (head, rest) = match parts.split_first() {
        Some((first, rest)) if !is_color_stop(first, current) => (Some(*first), rest),
        _ => (None, parts.as_slice()),
    };
    let stops = read_stops(rest, current, units, kind == "conic-gradient")?;
    Some(match kind {
        "linear-gradient" => Gradient::Linear {
            direction: head.map_or(Some(LinearDirection::Angle(180.)), linear_direction)?,
            stops,
            repeating,
        },
        "radial-gradient" => {
            let (shape, center) = radial_head(head.unwrap_or(""), units)?;
            Gradient::Radial {
                shape,
                center,
                stops,
                repeating,
            }
        }
        "conic-gradient" => {
            let (from, center) = conic_head(head.unwrap_or(""), units)?;
            Gradient::Conic {
                from,
                center,
                stops,
                repeating,
            }
        }
        _ => return None,
    })
}

/// Whether a comma-separated part starts a colour stop rather than being the gradient's head.
fn is_color_stop(part: &str, current: Rgba) -> bool {
    words(part)
        .first()
        .is_some_and(|word| color_with(word, current).is_some())
}

fn read_stops(
    parts: &[&str],
    current: Rgba,
    units: &Units,
    angular: bool,
) -> Option<Vec<ColorStop>> {
    let mut stops = Vec::new();
    for part in parts {
        let mut pieces = words(part);
        let Some(color) = pieces.first().and_then(|word| color_with(word, current)) else {
            // A lone position between two stops is a transition hint, which is left out.
            continue;
        };
        pieces.remove(0);
        let positions: Vec<GradientPosition> = pieces
            .iter()
            .filter_map(|word| stop_position(word, units, angular))
            .collect();
        match positions.as_slice() {
            [] => stops.push(ColorStop {
                color,
                position: None,
            }),
            [one] => stops.push(ColorStop {
                color,
                position: Some(*one),
            }),
            [first, second, ..] => {
                for position in [first, second] {
                    stops.push(ColorStop {
                        color,
                        position: Some(*position),
                    });
                }
            }
        }
    }
    (stops.len() >= 2).then_some(stops)
}

fn stop_position(word: &str, units: &Units, angular: bool) -> Option<GradientPosition> {
    if angular && let Some(degrees) = angle(word) {
        return Some(GradientPosition::Fraction(degrees / 360.));
    }
    match length_percent(word, units)? {
        Dim::Percent(f) => Some(GradientPosition::Fraction(f)),
        Dim::Px(px) => Some(GradientPosition::Px(px)),
        Dim::Auto => None,
    }
}

/// An angle in degrees: `45deg`, `0.25turn`, `1.5708rad`, `100grad`.
pub(crate) fn angle(text: &str) -> Option<f32> {
    let text = text.trim().to_ascii_lowercase();
    let units = [
        ("deg", 1.),
        ("grad", 0.9),
        ("rad", 180. / std::f32::consts::PI),
        ("turn", 360.),
    ];
    units
        .iter()
        .find_map(|(suffix, scale)| number(text.strip_suffix(suffix)?).map(|n| n * scale))
}

fn linear_direction(head: &str) -> Option<LinearDirection> {
    let head = head.trim();
    if let Some(degrees) = angle(head) {
        return Some(LinearDirection::Angle(degrees));
    }
    if head == "0" {
        return Some(LinearDirection::Angle(0.));
    }
    let sides: Vec<&str> = words(head.strip_prefix("to ")?);
    let (mut right, mut bottom) = (None, None);
    for side in &sides {
        match *side {
            "left" => right = Some(false),
            "right" => right = Some(true),
            "top" => bottom = Some(false),
            "bottom" => bottom = Some(true),
            _ => return None,
        }
    }
    Some(match (right, bottom) {
        (None, Some(false)) => LinearDirection::Angle(0.),
        (Some(true), None) => LinearDirection::Angle(90.),
        (None, Some(true)) => LinearDirection::Angle(180.),
        (Some(false), None) => LinearDirection::Angle(270.),
        (Some(right), Some(bottom)) => LinearDirection::Corner { right, bottom },
        (None, None) => return None,
    })
}

/// One place along an axis: a keyword, a length or a percentage.
fn axis_place(word: &str, units: &Units) -> Option<GradientPosition> {
    match word {
        "left" | "top" => Some(GradientPosition::Fraction(0.)),
        "right" | "bottom" => Some(GradientPosition::Fraction(1.)),
        "center" => Some(GradientPosition::Fraction(0.5)),
        other => match length_percent(other, units)? {
            Dim::Px(px) => Some(GradientPosition::Px(px)),
            Dim::Percent(f) => Some(GradientPosition::Fraction(f)),
            Dim::Auto => None,
        },
    }
}

/// The centre `at <position>` names, as the places along the two axes; the middle without it.
fn position_pair(words: &[&str], units: &Units) -> Option<[GradientPosition; 2]> {
    let half = GradientPosition::Fraction(0.5);
    match words {
        [] => Some([half, half]),
        [one] if matches!(*one, "top" | "bottom") => Some([half, axis_place(one, units)?]),
        [one] => Some([axis_place(one, units)?, half]),
        [a, b, ..] if matches!(*a, "top" | "bottom") => {
            Some([axis_place(b, units)?, axis_place(a, units)?])
        }
        [a, b, ..] => Some([axis_place(a, units)?, axis_place(b, units)?]),
    }
}

fn radial_head(head: &str, units: &Units) -> Option<(RadialShape, [GradientPosition; 2])> {
    let (before, at) = match head.split_once(" at ") {
        Some((before, at)) => (before.trim(), at.trim()),
        None => match head.strip_prefix("at ") {
            Some(at) => ("", at.trim()),
            None => (head.trim(), ""),
        },
    };
    let center = position_pair(&words(at), units)?;
    let mut circle = false;
    let mut extent = None;
    let mut radii: Vec<GradientPosition> = Vec::new();
    for word in words(before) {
        match word {
            "circle" => circle = true,
            "ellipse" => {}
            "closest-side" => extent = Some(Extent::ClosestSide),
            "farthest-side" => extent = Some(Extent::FarthestSide),
            "closest-corner" => extent = Some(Extent::ClosestCorner),
            "farthest-corner" => extent = Some(Extent::FarthestCorner),
            other => radii.push(match length_percent(other, units)? {
                Dim::Px(px) => GradientPosition::Px(px),
                Dim::Percent(f) => GradientPosition::Fraction(f),
                Dim::Auto => return None,
            }),
        }
    }
    let shape = match (circle || radii.len() == 1, radii.as_slice()) {
        (true, [GradientPosition::Px(r)]) => RadialShape::Circle(Extent::Radius(*r)),
        (true, _) => RadialShape::Circle(extent.unwrap_or(Extent::FarthestCorner)),
        (false, [x, y]) => RadialShape::Ellipse(EllipseSize::Radii([*x, *y])),
        (false, _) => RadialShape::Ellipse(EllipseSize::Extent(
            extent.unwrap_or(Extent::FarthestCorner),
        )),
    };
    Some((shape, center))
}

fn conic_head(head: &str, units: &Units) -> Option<(f32, [GradientPosition; 2])> {
    let (before, at) = match head.split_once("at ") {
        Some((before, at)) => (before.trim(), at.trim()),
        None => (head.trim(), ""),
    };
    let from = match before.strip_prefix("from ") {
        Some(text) => angle(text)?,
        None if before.is_empty() => 0.,
        None => return None,
    };
    Some((from, position_pair(&words(at), units)?))
}

impl Gradient {
    pub fn stops(&self) -> &[ColorStop] {
        match self {
            Gradient::Linear { stops, .. }
            | Gradient::Radial { stops, .. }
            | Gradient::Conic { stops, .. } => stops,
        }
    }

    /// A number that is the same for gradients that are the same, for naming the picture one
    /// rasterises to (the text is not kept, and floats are hashed by their bits).
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self {
            Gradient::Linear {
                direction,
                stops,
                repeating,
            } => {
                (0_u8, *repeating).hash(&mut hasher);
                match direction {
                    LinearDirection::Angle(degrees) => (0_u8, degrees.to_bits()).hash(&mut hasher),
                    LinearDirection::Corner { right, bottom } => {
                        (1_u8, *right, *bottom).hash(&mut hasher)
                    }
                }
                hash_stops(stops, &mut hasher);
            }
            Gradient::Radial {
                shape,
                center,
                stops,
                repeating,
            } => {
                (1_u8, *repeating).hash(&mut hasher);
                hash_shape(shape, &mut hasher);
                hash_places(center, &mut hasher);
                hash_stops(stops, &mut hasher);
            }
            Gradient::Conic {
                from,
                center,
                stops,
                repeating,
            } => {
                (2_u8, *repeating, from.to_bits()).hash(&mut hasher);
                hash_places(center, &mut hasher);
                hash_stops(stops, &mut hasher);
            }
        }
        hasher.finish()
    }

    pub fn is_repeating(&self) -> bool {
        match self {
            Gradient::Linear { repeating, .. }
            | Gradient::Radial { repeating, .. }
            | Gradient::Conic { repeating, .. } => *repeating,
        }
    }

    /// The angle of a linear gradient's line in a box of `size`, in degrees; a corner direction
    /// is the angle that makes the 50% line run through the box's other two corners.
    pub fn line_angle(&self, size: [f32; 2]) -> Option<f32> {
        let Gradient::Linear { direction, .. } = self else {
            return None;
        };
        Some(match *direction {
            LinearDirection::Angle(degrees) => degrees,
            LinearDirection::Corner { right, bottom } => {
                // The line is perpendicular to the diagonal that misses the named corner,
                // pointing at it: for `to bottom right` that is (h, w) in a y-down plane.
                let [w, h] = size;
                let (dx, dy) = match (right, bottom) {
                    (true, true) => (h, w),
                    (true, false) => (h, -w),
                    (false, true) => (-h, w),
                    (false, false) => (-h, -w),
                };
                dx.atan2(-dy).to_degrees().rem_euclid(360.)
            }
        })
    }

    /// The stops with a place along a line `length` pixels long, in `0..=1` and not going back:
    /// those without a position spread evenly between the ones with.
    pub fn stops_along(&self, length: f32) -> Vec<(Rgba, f32)> {
        let stops = self.stops();
        if stops.is_empty() {
            return Vec::new();
        }
        let mut places: Vec<Option<f32>> = stops
            .iter()
            .map(|stop| {
                stop.position.map(|position| match position {
                    GradientPosition::Fraction(f) => f,
                    GradientPosition::Px(px) if length > 0. => px / length,
                    GradientPosition::Px(_) => 0.,
                })
            })
            .collect();
        let last = places.len() - 1;
        places[0].get_or_insert(0.);
        places[last].get_or_insert(1.);
        let mut floor = f32::MIN;
        for place in places.iter_mut().flatten() {
            *place = place.max(floor);
            floor = *place;
        }
        spread(&mut places);
        stops
            .iter()
            .zip(places)
            .map(|(stop, place)| (stop.color, place.unwrap_or(0.)))
            .collect()
    }
}

fn hash_place(place: GradientPosition, hasher: &mut DefaultHasher) {
    match place {
        GradientPosition::Fraction(f) => (0_u8, f.to_bits()).hash(hasher),
        GradientPosition::Px(px) => (1_u8, px.to_bits()).hash(hasher),
    }
}

fn hash_places(places: &[GradientPosition], hasher: &mut DefaultHasher) {
    for place in places {
        hash_place(*place, hasher);
    }
}

fn hash_stops(stops: &[ColorStop], hasher: &mut DefaultHasher) {
    for stop in stops {
        stop.color.map(f32::to_bits).hash(hasher);
        match stop.position {
            Some(place) => hash_place(place, hasher),
            None => 2_u8.hash(hasher),
        }
    }
}

fn hash_shape(shape: &RadialShape, hasher: &mut DefaultHasher) {
    let extent = |extent: &Extent, hasher: &mut DefaultHasher| match extent {
        Extent::ClosestSide => 0_u8.hash(hasher),
        Extent::FarthestSide => 1_u8.hash(hasher),
        Extent::ClosestCorner => 2_u8.hash(hasher),
        Extent::FarthestCorner => 3_u8.hash(hasher),
        Extent::Radius(r) => (4_u8, r.to_bits()).hash(hasher),
    };
    match shape {
        RadialShape::Circle(e) => extent(e, hasher),
        RadialShape::Ellipse(EllipseSize::Extent(e)) => extent(e, hasher),
        RadialShape::Ellipse(EllipseSize::Radii(radii)) => hash_places(radii, hasher),
    }
}

/// Fills the gaps between known places with evenly spaced ones.
fn spread(places: &mut [Option<f32>]) {
    let mut from = 0;
    while from < places.len() {
        let Some(start) = places[from] else {
            from += 1;
            continue;
        };
        let Some(gap) = places[from + 1..].iter().position(Option::is_some) else {
            break;
        };
        let to = from + 1 + gap;
        let end = places[to].unwrap_or(start);
        for step in 1..to - from {
            places[from + step] = Some(start + (end - start) * step as f32 / (to - from) as f32);
        }
        from = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: Rgba = [0., 0., 0., 1.];

    fn units() -> Units {
        Units::new(16., 16., (800., 600.))
    }

    fn parse(text: &str) -> Option<Gradient> {
        super::parse(text, BLACK, &units())
    }

    #[test]
    fn linear_directions_are_angles_or_corners() {
        let angle_of = |text: &str| match parse(text) {
            Some(Gradient::Linear { direction, .. }) => direction,
            other => panic!("{text} read as {other:?}"),
        };
        assert_eq!(
            angle_of("linear-gradient(red, blue)"),
            LinearDirection::Angle(180.)
        );
        assert_eq!(
            angle_of("linear-gradient(to right, red, blue)"),
            LinearDirection::Angle(90.)
        );
        assert_eq!(
            angle_of("linear-gradient(45deg, red, blue)"),
            LinearDirection::Angle(45.)
        );
        assert_eq!(
            angle_of("linear-gradient(0.25turn, red, blue)"),
            LinearDirection::Angle(90.)
        );
        assert_eq!(
            angle_of("linear-gradient(to bottom left, red, blue)"),
            LinearDirection::Corner {
                right: false,
                bottom: true
            }
        );
    }

    #[test]
    fn a_corner_direction_runs_to_the_corner_of_the_box() {
        let gradient = parse("linear-gradient(to bottom right, red, blue)").unwrap();
        // A square: the diagonal is 45 degrees off the vertical, so the line is 135 degrees.
        assert!((gradient.line_angle([100., 100.]).unwrap() - 135.).abs() < 1e-3);
        let tall = gradient.line_angle([100., 300.]).unwrap();
        assert!(tall > 90. && tall < 135.);
    }

    #[test]
    fn stops_without_a_place_spread_evenly() {
        let gradient = parse("linear-gradient(90deg, red, #00f 20px, lime, black 100%)").unwrap();
        let stops = gradient.stops_along(100.);
        let places: Vec<f32> = stops.iter().map(|(_, at)| *at).collect();
        assert_eq!(places.len(), 4);
        assert!((places[0] - 0.).abs() < 1e-6);
        assert!((places[1] - 0.2).abs() < 1e-6);
        assert!((places[2] - 0.6).abs() < 1e-6);
        assert!((places[3] - 1.).abs() < 1e-6);
        assert_eq!(stops[1].0, [0., 0., 1., 1.]);
    }

    #[test]
    fn two_places_on_a_stop_make_two_stops_and_places_never_go_back() {
        let gradient = parse("linear-gradient(red 10% 30%, blue 20%)").unwrap();
        let places: Vec<f32> = gradient
            .stops_along(50.)
            .iter()
            .map(|(_, at)| *at)
            .collect();
        assert_eq!(places, vec![0.1, 0.3, 0.3]);
    }

    #[test]
    fn radial_and_conic_heads_are_read() {
        match parse("radial-gradient(circle at 25% 75%, red, blue)") {
            Some(Gradient::Radial { shape, center, .. }) => {
                assert_eq!(shape, RadialShape::Circle(Extent::FarthestCorner));
                assert_eq!(
                    center,
                    [
                        GradientPosition::Fraction(0.25),
                        GradientPosition::Fraction(0.75)
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
        match parse("radial-gradient(40px 20px at top left, red, blue)") {
            Some(Gradient::Radial { shape, center, .. }) => {
                assert_eq!(
                    shape,
                    RadialShape::Ellipse(EllipseSize::Radii([
                        GradientPosition::Px(40.),
                        GradientPosition::Px(20.)
                    ]))
                );
                assert_eq!(
                    center,
                    [
                        GradientPosition::Fraction(0.),
                        GradientPosition::Fraction(0.)
                    ]
                );
            }
            other => panic!("{other:?}"),
        }
        match parse("conic-gradient(from 90deg at center, red 0deg, blue 180deg, red)") {
            Some(Gradient::Conic { from, stops, .. }) => {
                assert_eq!(from, 90.);
                assert_eq!(stops[1].position, Some(GradientPosition::Fraction(0.5)));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn repeating_and_unreadable_gradients() {
        assert!(
            parse("repeating-linear-gradient(red, blue 10px)")
                .unwrap()
                .is_repeating()
        );
        assert!(!parse("linear-gradient(red, blue)").unwrap().is_repeating());
        assert!(parse("linear-gradient(red)").is_none());
        assert!(parse("url(a.png)").is_none());
        assert!(is_gradient("repeating-radial-gradient(red, blue)"));
        assert!(!is_gradient("url(a.png)"));
    }
}
