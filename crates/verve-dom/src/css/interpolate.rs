//! Interpolation (CSS Values 4 §9, CSS Transitions 1 §2): the value part of the way from one
//! to another. Each kind of value that moves is one function of a table, tried in turn; what
//! none of them knows switches at the half.

use super::color::{color, srgb_text};
use super::syntax::{decimal, function_parts, split_outside};

/// One way to take a value part of the way to another: `None` when the two are not of the
/// kind it knows.
type Lerp = fn(&str, &str, f32) -> Option<String>;

/// The kinds of value that move between two ends, tried in turn.
const LERPS: &[Lerp] = &[number, measure, tint, visibility, structure];

/// The value part of the way from `from` to `to`. Values of a kind that moves — numbers,
/// lengths and other measures, colors, `visibility`, lists and functions of those that line up
/// (a `transform`, a `box-shadow`) — are interpolated; anything else switches at the half.
/// Colors mix in sRGB, as Chromium mixes the ones written in a legacy notation.
pub(crate) fn interpolate(from: &str, to: &str, t: f32) -> String {
    let (from, to) = (from.trim(), to.trim());
    if t <= 0. {
        return from.to_owned();
    }
    if t >= 1. {
        return to.to_owned();
    }
    LERPS
        .iter()
        .find_map(|lerp| lerp(from, to, t))
        .unwrap_or_else(|| if t < 0.5 { from } else { to }.to_owned())
}

/// Whether a value can move between `from` and `to`, rather than switch.
pub(crate) fn can_interpolate(from: &str, to: &str) -> bool {
    from != to
        && LERPS
            .iter()
            .any(|lerp| lerp(from.trim(), to.trim(), 0.5).is_some())
}

fn number(from: &str, to: &str, t: f32) -> Option<String> {
    let (a, b) = (from.parse::<f32>().ok()?, to.parse::<f32>().ok()?);
    Some(decimal(a + (b - a) * t))
}

/// A number with a unit (`12px`, `50%`, `90deg`, `0.3s`): both of the one unit, or one of them a
/// bare `0`, which is that unit's.
fn measure(from: &str, to: &str, t: f32) -> Option<String> {
    let ((a, unit_a), (b, unit_b)) = (measured(from)?, measured(to)?);
    let unit = if unit_a.is_empty() { unit_b } else { unit_a };
    let alike =
        unit_a == unit_b || (unit_a.is_empty() && a == 0.) || (unit_b.is_empty() && b == 0.);
    alike.then(|| format!("{}{unit}", decimal(a + (b - a) * t)))
}

/// A number and the unit after it: `12px` as `(12, "px")`.
fn measured(text: &str) -> Option<(f32, &str)> {
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let valid = unit.chars().all(|c| c.is_ascii_alphabetic() || c == '%');
    (valid && !number.is_empty()).then_some(())?;
    Some((number.parse().ok()?, unit))
}

fn tint(from: &str, to: &str, t: f32) -> Option<String> {
    let (a, b) = (color(from)?, color(to)?);
    let mix = |i: usize| a[i] + (b[i] - a[i]) * t;
    Some(srgb_text(mix(0), mix(1), mix(2), mix(3)))
}

/// `visibility` between `visible` and a hidden one is `visible` the whole way, so that a fade
/// that ends hidden is seen to its end.
fn visibility(from: &str, to: &str, _: f32) -> Option<String> {
    let known = |v: &str| matches!(v, "visible" | "hidden" | "collapse");
    (known(from) && known(to) && (from == "visible" || to == "visible"))
        .then(|| "visible".to_owned())
}

/// Lists and functions whose parts line up: `translateX(0px) scale(1)` to
/// `translateX(10px) scale(1.2)`, each part moving on its own. `none` is the identity of what
/// it is moved to or from.
fn structure(from: &str, to: &str, t: f32) -> Option<String> {
    let (identity_from, identity_to);
    let (from, to) = match (from, to) {
        ("none", other) => {
            identity_from = identity(other)?;
            (identity_from.as_str(), other)
        }
        (other, "none") => {
            identity_to = identity(other)?;
            (other, identity_to.as_str())
        }
        pair => pair,
    };
    let each = |text: &str, separator: char| -> Vec<String> {
        split_outside(text, separator)
            .into_iter()
            .map(|part| part.trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect()
    };
    for separator in [',', ' '] {
        let (a, b) = (each(from, separator), each(to, separator));
        if a.len() > 1 || b.len() > 1 {
            if a.len() != b.len() {
                return None;
            }
            let parts: Option<Vec<String>> = a.iter().zip(&b).map(|(x, y)| part(x, y, t)).collect();
            let joiner = if separator == ',' { ", " } else { " " };
            return parts.map(|parts| parts.join(joiner));
        }
    }
    let ((name, a), (other, b)) = (function_parts(from)?, function_parts(to)?);
    (name == other).then_some(())?;
    Some(format!(
        "{name}({})",
        structure(a, b, t).or_else(|| part(a, b, t))?
    ))
}

/// One part of a list or function: the same text, or two values that move.
fn part(from: &str, to: &str, t: f32) -> Option<String> {
    if from == to {
        return Some(from.to_owned());
    }
    LERPS.iter().find_map(|lerp| lerp(from, to, t))
}

/// The functions whose neutral argument is one, not zero.
const ONE: &[&str] = &[
    "scale",
    "scalex",
    "scaley",
    "scalez",
    "scale3d",
    "brightness",
    "contrast",
    "saturate",
    "opacity",
];

/// What `none` stands for beside `value`, a list of functions: each function with its
/// arguments made neutral — `rotate(90deg)` is `rotate(0deg)`, `scale(2)` is `scale(1)`.
fn identity(value: &str) -> Option<String> {
    let words: Vec<String> = split_outside(value, ' ')
        .into_iter()
        .map(|w| w.trim().to_owned())
        .filter(|w| !w.is_empty())
        .collect();
    let neutral: Option<Vec<String>> = words
        .iter()
        .map(|word| {
            let (name, args) = function_parts(word)?;
            let one = ONE.contains(&name.to_ascii_lowercase().as_str());
            let args: Option<Vec<String>> = split_outside(args, ',')
                .iter()
                .map(|arg| {
                    let (_, unit) = measured(arg.trim())?;
                    Some(format!("{}{unit}", if one { 1 } else { 0 }))
                })
                .collect();
            Some(format!("{name}({})", args?.join(", ")))
        })
        .collect();
    neutral.map(|words| words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_interpolate_only_where_they_share_a_kind() {
        assert_eq!(interpolate("0", "1", 0.5), "0.5");
        assert_eq!(interpolate("0px", "10px", 0.25), "2.5px");
        assert_eq!(interpolate("0", "10px", 0.5), "5px");
        assert_eq!(
            interpolate("rgba(0, 0, 0, 0)", "rgb(0, 0, 0)", 0.5),
            "rgba(0, 0, 0, 0.5)"
        );
        assert_eq!(interpolate("none", "block", 0.4), "none");
        assert_eq!(interpolate("none", "block", 0.6), "block");
        assert_eq!(interpolate("10px", "2em", 0.4), "10px");
    }

    #[test]
    fn lists_and_functions_move_part_by_part() {
        assert_eq!(
            interpolate("translateX(0px) scale(1)", "translateX(10px) scale(2)", 0.5),
            "translateX(5px) scale(1.5)"
        );
        assert_eq!(
            interpolate("0 0 4px rgb(0, 0, 0)", "0 0 8px rgb(255, 255, 255)", 0.5),
            "0 0 6px rgb(128, 128, 128)"
        );
        // Functions that do not line up switch instead.
        assert_eq!(interpolate("scale(1)", "rotate(90deg)", 0.4), "scale(1)");
        assert!(can_interpolate("translateX(0px)", "translateX(5px)"));
        assert!(!can_interpolate("block", "none"));
    }

    #[test]
    fn none_is_the_identity_of_what_it_moves_to() {
        assert_eq!(interpolate("none", "rotate(360deg)", 0.5), "rotate(180deg)");
        assert_eq!(interpolate("scale(3)", "none", 0.5), "scale(2)");
        assert_eq!(
            interpolate("none", "translate(10px, 20px) scale(2)", 0.5),
            "translate(5px, 10px) scale(1.5)"
        );
    }

    #[test]
    fn visibility_is_visible_while_either_end_is() {
        assert_eq!(interpolate("visible", "hidden", 0.1), "visible");
        assert_eq!(interpolate("hidden", "visible", 0.9), "visible");
        assert!(can_interpolate("visible", "hidden") && !can_interpolate("hidden", "collapse"));
    }
}
