//! Reading the text of a computed value: lengths against a font size, numbers, and the splitting
//! that keeps `rgb(0, 0, 0)` and `"a b"` in one piece (verve-dom's own, so quotes and escapes are
//! read as the cascade reads them).
//!
//! The cascade leaves a value as text (`12px`, `0.67em`, `calc(100% - 4px)`, `50%`); what a length
//! comes to is read here, with `verve_dom::css::length_calc` doing the arithmetic and the units.

use verve_dom::css::{Environment, function_parts, length_calc, split_slices};

/// What a length is measured against: the element's font size, the root's, and the viewport.
#[derive(Clone, Debug)]
pub(crate) struct Units {
    pub font_size: f32,
    pub root_font_size: f32,
    pub env: Environment,
}

impl Units {
    pub(crate) fn new(font_size: f32, root_font_size: f32, viewport: (f32, f32)) -> Self {
        Self {
            font_size,
            root_font_size,
            env: Environment {
                width: viewport.0,
                height: viewport.1,
                ..Environment::default()
            },
        }
    }
}

/// A size along one axis, as layout takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Dim {
    Auto,
    Px(f32),
    /// A fraction of the containing block: `0.5` for `50%`.
    Percent(f32),
}

impl Dim {
    /// What it comes to against a containing block `basis` wide (an unbounded basis makes a
    /// percentage nothing; `auto` is nothing too).
    pub(crate) fn resolve(self, basis: f32) -> f32 {
        match self {
            Dim::Px(px) => px,
            Dim::Percent(share) if basis.is_finite() => share * basis,
            Dim::Percent(_) | Dim::Auto => 0.,
        }
    }
}

/// A length or a percentage: `None` for a value that is neither, and for `calc()` that mixes a
/// length with a percentage, which taffy 0.13 cannot hold without a calc resolver.
pub(crate) fn length_percent(text: &str, units: &Units) -> Option<Dim> {
    let amount = length_calc(
        text.trim(),
        units.font_size,
        units.root_font_size,
        &units.env,
    )?;
    match (amount.px, amount.percent) {
        (px, 0.) => Some(Dim::Px(px)),
        (0., percent) => Some(Dim::Percent(percent / 100.)),
        _ => None,
    }
}

/// A length in pixels; a percentage is not one.
pub(crate) fn pixels(text: &str, units: &Units) -> Option<f32> {
    match length_percent(text, units)? {
        Dim::Px(px) => Some(px),
        _ => None,
    }
}

/// A plain number: `2`, `0.5`, `-1`.
pub(crate) fn number(text: &str) -> Option<f32> {
    text.trim().parse::<f32>().ok().filter(|n| n.is_finite())
}

/// `text` cut at every `separator` outside parentheses, brackets and quotes, trimmed, without the
/// empty pieces.
pub(crate) fn split_outside(text: &str, separator: char) -> Vec<&str> {
    pieces(text, |c| c == separator)
}

/// The whitespace-separated words of `text`, with a word's parentheses kept whole.
pub(crate) fn words(text: &str) -> Vec<&str> {
    pieces(text, char::is_whitespace)
}

fn pieces(text: &str, is_separator: impl Fn(char) -> bool) -> Vec<&str> {
    split_slices(text, is_separator)
        .into_iter()
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .collect()
}

/// The arguments of `name(…)` when `text` is one call of it.
pub(crate) fn call_arguments<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let (called, arguments) = function_parts(text.trim())?;
    called.eq_ignore_ascii_case(name).then_some(arguments)
}

/// The name of the function `text` starts with, lower-cased: `linear-gradient` for
/// `linear-gradient(…)`.
pub(crate) fn function_name(text: &str) -> Option<String> {
    let (name, _) = function_parts(text.trim())?;
    (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .then(|| name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units() -> Units {
        Units::new(20., 16., (800., 600.))
    }

    #[test]
    fn lengths_read_against_the_font_and_the_viewport() {
        let units = units();
        assert_eq!(length_percent("12px", &units), Some(Dim::Px(12.)));
        assert_eq!(length_percent("1.5em", &units), Some(Dim::Px(30.)));
        assert_eq!(length_percent("2rem", &units), Some(Dim::Px(32.)));
        assert_eq!(length_percent("10vw", &units), Some(Dim::Px(80.)));
        assert_eq!(length_percent("50%", &units), Some(Dim::Percent(0.5)));
        assert_eq!(length_percent("0", &units), Some(Dim::Px(0.)));
        assert_eq!(length_percent("auto", &units), None);
    }

    #[test]
    fn a_calc_of_both_a_length_and_a_percentage_is_not_held() {
        let units = units();
        assert_eq!(length_percent("calc(100% - 10px)", &units), None);
        assert_eq!(
            length_percent("calc(10px + 5px)", &units),
            Some(Dim::Px(15.))
        );
        assert_eq!(
            length_percent("calc(50% * 2)", &units),
            Some(Dim::Percent(1.))
        );
        assert_eq!(pixels("50%", &units), None);
    }

    #[test]
    fn splitting_keeps_calls_and_quotes_whole() {
        assert_eq!(
            split_outside("rgb(1, 2, 3) 4px, red", ','),
            vec!["rgb(1, 2, 3) 4px", "red"]
        );
        assert_eq!(
            words("1px solid rgb(0, 0, 0)"),
            vec!["1px", "solid", "rgb(0, 0, 0)"]
        );
        assert_eq!(split_outside("'a, b', c", ','), vec!["'a, b'", "c"]);
        assert_eq!(
            call_arguments("minmax(1px, 2fr)", "minmax"),
            Some("1px, 2fr")
        );
        assert_eq!(call_arguments("repeat (2, 1px)", "repeat"), Some("2, 1px"));
        assert_eq!(call_arguments("repeat(2, 1px) 1px", "repeat"), None);
        assert_eq!(
            function_name("Linear-Gradient(red, blue)").as_deref(),
            Some("linear-gradient")
        );
        assert_eq!(function_name("red"), None);
    }
}
