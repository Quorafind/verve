//! `box-shadow`, read from the computed text: any number of shadows, each with its offsets, blur,
//! spread, colour and `inset`.

use verve_dom::css::color_with;

use crate::display_list::{Rgba, Shadow};
use crate::value::{Units, pixels, split_outside, words};

/// The shadows `text` names, first on top as in CSS. Text that is `none` or that no shadow can be
/// read from gives none; a shadow with fewer than two lengths is dropped, as a browser drops it.
pub(crate) fn parse(text: &str, current: Rgba, units: &Units) -> Vec<Shadow> {
    if text.trim() == "none" {
        return Vec::new();
    }
    split_outside(text, ',')
        .into_iter()
        .filter_map(|part| one(part, current, units))
        .collect()
}

fn one(text: &str, current: Rgba, units: &Units) -> Option<Shadow> {
    let mut inset = false;
    let mut color = None;
    let mut lengths: Vec<f32> = Vec::new();
    for word in words(text) {
        if word == "inset" {
            inset = true;
        } else if let Some(px) = pixels(word, units) {
            lengths.push(px);
        } else if let Some(found) = color_with(word, current) {
            color = Some(found);
        } else {
            return None;
        }
    }
    let [x, y, rest @ ..] = lengths.as_slice() else {
        return None;
    };
    let blur = rest.first().copied().unwrap_or(0.);
    if blur < 0. || rest.len() > 2 {
        return None;
    }
    Some(Shadow {
        offset: [*x, *y],
        blur,
        spread: rest.get(1).copied().unwrap_or(0.),
        color: color.unwrap_or(current),
        inset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: Rgba = [0., 0., 0., 1.];

    fn parsed(text: &str) -> Vec<Shadow> {
        parse(text, BLACK, &Units::new(10., 16., (800., 600.)))
    }

    #[test]
    fn shadows_read_offsets_blur_spread_and_colour_in_any_order() {
        let shadows = parsed("0 2px 4px rgba(0, 0, 0, 0.5), inset red 1px 1px 0 2px");
        assert_eq!(shadows.len(), 2);
        assert_eq!(shadows[0].offset, [0., 2.]);
        assert_eq!(shadows[0].blur, 4.);
        assert_eq!(shadows[0].color, [0., 0., 0., 0.5]);
        assert!(!shadows[0].inset);
        assert!(shadows[1].inset);
        assert_eq!(shadows[1].spread, 2.);
        assert_eq!(shadows[1].color, [1., 0., 0., 1.]);
    }

    #[test]
    fn a_shadow_without_a_colour_takes_the_current_one() {
        let shadows = parsed("1em 0.5em");
        assert_eq!(shadows[0].offset, [10., 5.]);
        assert_eq!(shadows[0].color, BLACK);
    }

    #[test]
    fn none_and_nonsense_have_no_shadows() {
        assert!(parsed("none").is_empty());
        assert!(parsed("3px").is_empty());
        assert!(parsed("1px 1px -2px red").is_empty());
        assert!(parsed("1px 1px banana").is_empty());
    }
}
