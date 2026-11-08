//! `transform`, `translate`: the part of them layout carries out.
//!
//! A box moves by the translation of its `transform` list; the offset changes where it is
//! reported and hit, never the layout of what is around it. Scaling, rotation and skew are not
//! drawn (P35 draws them on pictures); a list that has one still moves by its translations, and
//! says so through [`Translation::lossy`] so the renderer can warn once.

use verve_dom::css::{LengthCalc, length_calc};

use crate::value::{Units, call_arguments, function_name, split_outside, words};

/// How far a box moves: pixels plus a share of its own size, on each axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Translation {
    x: LengthCalc,
    y: LengthCalc,
    /// The list had a function that was not carried out.
    pub lossy: bool,
}

impl Translation {
    /// The move in pixels for a box of `width` by `height`.
    pub(crate) fn resolve(&self, width: f32, height: f32) -> [f32; 2] {
        [
            self.x.px + self.x.percent / 100. * width,
            self.y.px + self.y.percent / 100. * height,
        ]
    }

    /// The move of `self` and then `other`.
    pub(crate) fn then(mut self, other: Translation) -> Translation {
        self.add(other.x, other.y);
        self.lossy |= other.lossy;
        self
    }

    fn add(&mut self, x: LengthCalc, y: LengthCalc) {
        self.x.px += x.px;
        self.x.percent += x.percent;
        self.y.px += y.px;
        self.y.percent += y.percent;
    }
}

/// The translation of a `transform` value; `None` for `none`.
pub(crate) fn parse_transform(text: &str, units: &Units) -> Option<Translation> {
    let text = text.trim();
    if text.is_empty() || text == "none" {
        return None;
    }
    let mut total = Translation::default();
    for call in functions(text) {
        let Some(name) = function_name(call) else {
            total.lossy = true;
            continue;
        };
        let arguments = call_arguments(call, &name).unwrap_or_default();
        match translation_of(&name, arguments, units) {
            Some((x, y, exact)) => {
                total.add(x, y);
                total.lossy |= !exact;
            }
            None if is_ignored(&name) => {}
            None => total.lossy = true,
        }
    }
    Some(total)
}

/// The `translate` property: one or two lengths, and a third for `z` that is left out.
pub(crate) fn parse_translate(text: &str, units: &Units) -> Option<Translation> {
    let text = text.trim();
    if text.is_empty() || text == "none" {
        return None;
    }
    let parts = words(text);
    let amount = |at: usize| match parts.get(at) {
        Some(part) => length_calc(part, units.font_size, units.root_font_size, &units.env),
        None => Some(LengthCalc::default()),
    };
    let mut total = Translation::default();
    total.add(amount(0)?, amount(1)?);
    Some(total)
}

/// The calls in a transform list: `translate(1px) rotate(2deg)` is two.
fn functions(text: &str) -> Vec<&str> {
    words(text)
}

/// A function that leaves the box where it is without being a loss: the identity-like ones.
fn is_ignored(name: &str) -> bool {
    matches!(name, "perspective" | "translatez")
}

/// What a transform function moves the box by, and whether that is all it does. A `matrix()`
/// keeps its translation (computed and animated transforms arrive as matrices) and is not exact
/// when it also scales, rotates or skews.
fn translation_of(
    name: &str,
    arguments: &str,
    units: &Units,
) -> Option<(LengthCalc, LengthCalc, bool)> {
    let read = |part: &str| length_calc(part, units.font_size, units.root_font_size, &units.env);
    let args = split_outside(arguments, ',');
    let zero = LengthCalc::default();
    match name {
        "translate" | "translate3d" => {
            let x = read(args.first()?)?;
            let y = args.get(1).map_or(Some(zero), |part| read(part))?;
            Some((x, y, true))
        }
        "translatex" => Some((read(args.first()?)?, zero, true)),
        "translatey" => Some((zero, read(args.first()?)?, true)),
        "matrix" | "matrix3d" => {
            let numbers: Vec<f32> = args.iter().filter_map(|part| part.parse().ok()).collect();
            let (a, b, c, d) = match (name, numbers.as_slice()) {
                ("matrix", [a, b, c, d, ..]) => (*a, *b, *c, *d),
                ("matrix3d", [a, b, _, _, c, d, ..]) => (*a, *b, *c, *d),
                _ => return None,
            };
            let identity = a == 1. && b == 0. && c == 0. && d == 1.;
            let (e, f) = match (name, numbers.as_slice()) {
                ("matrix", [.., e, f]) => (*e, *f),
                ("matrix3d", [.., x, y, _, _]) => (*x, *y),
                _ => return None,
            };
            Some((
                LengthCalc { px: e, percent: 0. },
                LengthCalc { px: f, percent: 0. },
                identity,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units() -> Units {
        Units::new(10., 16., (800., 600.))
    }

    #[test]
    fn translations_add_up_across_the_list() {
        let t = parse_transform(
            "translate(30px, 20px) translateX(10px) translateY(2em)",
            &units(),
        )
        .unwrap();
        assert_eq!(t.resolve(100., 50.), [40., 40.]);
        assert!(!t.lossy);
    }

    #[test]
    fn percentages_are_of_the_box_itself() {
        let t = parse_transform("translate(10%, 20%)", &units()).unwrap();
        assert_eq!(t.resolve(100., 50.), [10., 10.]);
    }

    #[test]
    fn matrix_carries_its_translation_when_it_only_translates() {
        let t = parse_transform("matrix(1, 0, 0, 1, 7, 9)", &units()).unwrap();
        assert_eq!(t.resolve(1., 1.), [7., 9.]);
        let scaled = parse_transform("matrix(2, 0, 0, 2, 7, 9)", &units()).unwrap();
        assert!(scaled.lossy);
        assert_eq!(scaled.resolve(1., 1.), [7., 9.]);
    }

    #[test]
    fn rotation_and_scale_are_not_carried_out_and_say_so() {
        let t = parse_transform("translateX(5px) rotate(10deg) scale(2)", &units()).unwrap();
        assert!(t.lossy);
        assert_eq!(t.resolve(1., 1.), [5., 0.]);
        assert!(parse_transform("none", &units()).is_none());
    }

    #[test]
    fn the_translate_property_takes_one_or_two_lengths() {
        assert_eq!(
            parse_translate("10px 20px", &units())
                .unwrap()
                .resolve(0., 0.),
            [10., 20.]
        );
        assert_eq!(
            parse_translate("5px", &units()).unwrap().resolve(0., 0.),
            [5., 0.]
        );
        assert!(parse_translate("none", &units()).is_none());
    }
}
