//! `calc()` and its family (CSS Values 4): what a mathematical value comes to, in pixels and
//! a share of the basis the drawer resolves it against, and the one table of units every
//! length, angle and duration in this engine is read with.
//!
//! A value that is all absolute lengths is computed to `14px`; one with `%` in it keeps its
//! percentage — `calc(50% - 12px)` — for whoever knows what it is a percentage of. `var()` is
//! substituted before a value gets here. Only a value that is a single dimension or a single
//! mathematical function is read at all: `16 / 9` and `1 / -1`, a `grid-column` and an
//! `aspect-ratio`, are not arithmetic.

use super::media::Environment;
use super::syntax::{decimal, matching_parenthesis};
use super::values::ROOT_FONT_SIZE;

/// The largest length, in pixels: what an infinite one is, as in Chrome.
const MAX_LENGTH: f32 = 33_554_400.;

/// A length as the drawer resolves it against its containing block: `px + percent% of basis`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LengthCalc {
    /// The part that is already pixels.
    pub px: f32,
    /// The part that is a percentage of the basis, `50.` for `50%`.
    pub percent: f32,
}

/// A length in pixels: `12px`, `0.5em` against `font_size`, `1rem` against 16, `pt`, a bare
/// number or `0`, or a `calc()` that comes to one. A percentage, which is of something, is
/// not a length here.
pub fn length(text: &str, font_size: f32) -> Option<f32> {
    let length = length_calc(text, font_size, ROOT_FONT_SIZE, &Environment::default())?;
    (length.percent == 0.).then_some(length.px)
}

/// A length as `px + percent%`, computed from `text`: `12px`, `1.5em`, `calc(100% - 2rem)`.
/// Answers `None` for a value this engine cannot resolve without a layout — a container unit —
/// or that is not a length at all.
pub fn length_calc(
    text: &str,
    font_size: f32,
    root_font_size: f32,
    env: &Environment,
) -> Option<LengthCalc> {
    match value(text, font_size, root_font_size, env)? {
        Amount::Length(length) => Some(length),
        Amount::Number(number) => Some(LengthCalc {
            px: number,
            percent: 0.,
        }),
        Amount::Angle(_) | Amount::Time(_) => None,
    }
}

/// The computed text of a value that is a mathematical function: `14px` when it is all
/// absolute, `25%` when it is a share alone, and `calc(50% - 12px)` when it is both. `None`
/// for any other value, and for a function this engine cannot work out, whose text is then
/// kept as written.
pub(super) fn math_text(
    text: &str,
    font_size: f32,
    root_font_size: f32,
    env: &Environment,
) -> Option<String> {
    if !is_function(text.trim()) {
        return None;
    }
    Some(match value(text, font_size, root_font_size, env)? {
        Amount::Length(LengthCalc { px, percent }) => {
            // Chrome writes an infinite length as its largest, `3.35544e+07px`, and `NaN` as 0.
            let px = if px.is_nan() {
                0.
            } else {
                px.clamp(-MAX_LENGTH, MAX_LENGTH)
            };
            if percent == 0. {
                format!("{}px", decimal(px))
            } else if px == 0. {
                format!("{}%", decimal(percent))
            } else if px > 0. {
                format!("calc({}% + {}px)", decimal(percent), decimal(px))
            } else {
                format!("calc({}% - {}px)", decimal(percent), decimal(-px))
            }
        }
        Amount::Number(number) => decimal(number),
        Amount::Angle(degrees) => format!("{}deg", decimal(degrees)),
        Amount::Time(seconds) => format!("{}s", decimal(seconds)),
    })
}

/// Whether `text` is one call of a mathematical function, as a whole.
fn is_function(text: &str) -> bool {
    let Some(open) = text.find('(') else {
        return false;
    };
    let name = text[..open].to_ascii_lowercase();
    let closes_last = matching_parenthesis(text, open) == Some(text.len() - 1);
    closes_last
        && matches!(
            name.as_str(),
            "calc" | "min" | "max" | "clamp" | "round" | "mod" | "rem" | "abs" | "sign"
        )
}

/// What a mathematical value comes to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Amount {
    /// Pixels, and a percentage of whatever it is measured against.
    Length(LengthCalc),
    Number(f32),
    /// Degrees.
    Angle(f32),
    /// Seconds.
    Time(f32),
}

impl Amount {
    fn add(self, other: Self) -> Option<Self> {
        Some(match (self, other) {
            (Amount::Length(a), Amount::Length(b)) => Amount::Length(LengthCalc {
                px: a.px + b.px,
                percent: a.percent + b.percent,
            }),
            (Amount::Number(a), Amount::Number(b)) => Amount::Number(a + b),
            (Amount::Angle(a), Amount::Angle(b)) => Amount::Angle(a + b),
            (Amount::Time(a), Amount::Time(b)) => Amount::Time(a + b),
            _ => return None,
        })
    }

    fn scale(self, by: f32) -> Self {
        // Nothing times infinity is nothing, not `NaN`: `calc(infinity * 1px)` has no percentage.
        let times = |part: f32| if part == 0. { 0. } else { part * by };
        match self {
            Amount::Length(a) => Amount::Length(LengthCalc {
                px: times(a.px),
                percent: times(a.percent),
            }),
            Amount::Number(a) => Amount::Number(times(a)),
            Amount::Angle(a) => Amount::Angle(times(a)),
            Amount::Time(a) => Amount::Time(times(a)),
        }
    }

    /// The one number it is, when it has no percentage in it: what `min()`, `round()` and the
    /// rest need to compare two of them.
    fn plain(self) -> Option<f32> {
        match self {
            Amount::Length(a) if a.percent == 0. => Some(a.px),
            Amount::Number(a) | Amount::Angle(a) | Amount::Time(a) => Some(a),
            Amount::Length(_) => None,
        }
    }

    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    /// `value` as the kind of amount `self` is.
    fn of_kind(self, value: f32) -> Self {
        match self {
            Amount::Number(_) => Amount::Number(value),
            Amount::Length(_) => Amount::Length(LengthCalc {
                px: value,
                percent: 0.,
            }),
            Amount::Angle(_) => Amount::Angle(value),
            Amount::Time(_) => Amount::Time(value),
        }
    }
}

/// What a number of a unit is, before the font sizes and the viewport it may be measured
/// against are known.
#[derive(Clone, Copy)]
enum Unit {
    /// A number of times `scale` of what `basis` is in pixels.
    Length {
        basis: Basis,
        scale: f32,
    },
    Percent,
    Number,
    /// Degrees in one of the unit.
    Angle(f32),
    /// Seconds in one of the unit.
    Time(f32),
}

/// What a length unit is measured in.
#[derive(Clone, Copy)]
enum Basis {
    Pixel,
    FontSize,
    RootFontSize,
    ViewportWidth,
    ViewportHeight,
    ViewportMin,
    ViewportMax,
}

/// The one table of units. `None` for one that is the layout's to know — a container unit.
fn unit(name: &str) -> Option<Unit> {
    let length = |basis, scale| Unit::Length { basis, scale };
    Some(match name.to_ascii_lowercase().as_str() {
        "" => Unit::Number,
        "%" => Unit::Percent,
        "px" => length(Basis::Pixel, 1.),
        "pt" => length(Basis::Pixel, 4. / 3.),
        "pc" => length(Basis::Pixel, 16.),
        "in" => length(Basis::Pixel, 96.),
        "cm" => length(Basis::Pixel, 96. / 2.54),
        "mm" => length(Basis::Pixel, 96. / 25.4),
        "q" => length(Basis::Pixel, 96. / 25.4 / 4.),
        "em" => length(Basis::FontSize, 1.),
        // Half an em stands in for the x-height and the advance of a `0`, which a font knows.
        "ex" | "ch" => length(Basis::FontSize, 0.5),
        "rem" => length(Basis::RootFontSize, 1.),
        "vw" | "svw" | "lvw" | "dvw" => length(Basis::ViewportWidth, 1.),
        "vh" | "svh" | "lvh" | "dvh" => length(Basis::ViewportHeight, 1.),
        "vmin" | "svmin" | "lvmin" | "dvmin" => length(Basis::ViewportMin, 1.),
        "vmax" | "svmax" | "lvmax" | "dvmax" => length(Basis::ViewportMax, 1.),
        "deg" => Unit::Angle(1.),
        "grad" => Unit::Angle(0.9),
        "rad" => Unit::Angle(180. / std::f32::consts::PI),
        "turn" => Unit::Angle(360.),
        "s" => Unit::Time(1.),
        "ms" => Unit::Time(0.001),
        _ => return None,
    })
}

/// A value of one dimension or one function, which is what a length or a mathematical value
/// is, never bare arithmetic.
fn value(text: &str, font_size: f32, root_font_size: f32, env: &Environment) -> Option<Amount> {
    let mut reader = Reader {
        text: text.trim(),
        at: 0,
        font_size,
        root_font_size,
        env,
    };
    let amount = reader.unary()?;
    reader.skip_space();
    (reader.at == reader.text.len()).then_some(amount)
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
    font_size: f32,
    root_font_size: f32,
    env: &'a Environment,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            return true;
        }
        false
    }

    fn sum(&mut self) -> Option<Amount> {
        let mut left = self.product()?;
        loop {
            self.skip_space();
            let sign = match self.peek() {
                Some(b'+') => 1.,
                Some(b'-') => -1.,
                _ => return Some(left),
            };
            self.at += 1;
            left = left.add(self.product()?.scale(sign))?;
        }
    }

    fn product(&mut self) -> Option<Amount> {
        let mut left = self.unary()?;
        loop {
            self.skip_space();
            let divide = match self.peek() {
                Some(b'*') => false,
                Some(b'/') => true,
                _ => return Some(left),
            };
            self.at += 1;
            let right = self.unary()?;
            left = if divide {
                // Only a number divides: `10px / 2px` is not a length (Chrome drops it as invalid), and
                // dividing by zero is infinity.
                match right {
                    Amount::Number(by) => left.scale(1. / by),
                    _ => return None,
                }
            } else {
                match (left, right) {
                    (Amount::Number(by), other) | (other, Amount::Number(by)) => other.scale(by),
                    _ => return None,
                }
            };
        }
    }

    fn unary(&mut self) -> Option<Amount> {
        self.skip_space();
        if self.eat(b'-') {
            return Some(self.unary()?.scale(-1.));
        }
        self.eat(b'+');
        self.primary()
    }

    fn primary(&mut self) -> Option<Amount> {
        self.skip_space();
        if self.eat(b'(') {
            let inner = self.sum()?;
            self.skip_space();
            return self.eat(b')').then_some(inner);
        }
        if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            return self.function();
        }
        self.dimension()
    }

    /// `12px`, `.5em`, `50%`, `2`, `45deg`, `200ms`.
    fn dimension(&mut self) -> Option<Amount> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
            self.at += 1;
        }
        if self.at == start {
            return None;
        }
        let number: f32 = self.text[start..self.at].parse().ok()?;
        let unit_start = self.at;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'%')
        {
            self.at += 1;
        }
        Some(self.scaled(number, unit(&self.text[unit_start..self.at])?))
    }

    /// `number` of `unit`, as the amount it is.
    fn scaled(&self, number: f32, unit: Unit) -> Amount {
        match unit {
            Unit::Number => Amount::Number(number),
            Unit::Percent => Amount::Length(LengthCalc {
                px: 0.,
                percent: number,
            }),
            Unit::Length { basis, scale } => {
                let basis = match basis {
                    Basis::Pixel => 1.,
                    Basis::FontSize => self.font_size,
                    Basis::RootFontSize => self.root_font_size,
                    Basis::ViewportWidth => self.env.width / 100.,
                    Basis::ViewportHeight => self.env.height / 100.,
                    Basis::ViewportMin => self.env.width.min(self.env.height) / 100.,
                    Basis::ViewportMax => self.env.width.max(self.env.height) / 100.,
                };
                Amount::Length(LengthCalc {
                    px: number * scale * basis,
                    percent: 0.,
                })
            }
            Unit::Angle(degrees) => Amount::Angle(number * degrees),
            Unit::Time(seconds) => Amount::Time(number * seconds),
        }
    }

    /// `calc(…)`, `min(…)`, `max(…)`, `clamp(…)`, `round(…)`, `mod(…)`, `rem(…)`, `abs(…)`,
    /// `sign(…)`.
    fn function(&mut self) -> Option<Amount> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.at += 1;
        }
        let name = self.text[start..self.at].to_ascii_lowercase();
        // The constants of CSS Values 4 §10.7: `infinity` is what Tailwind's `rounded-full`
        // multiplies a length by.
        let constant = match name.as_str() {
            "infinity" => Some(f32::INFINITY),
            "pi" => Some(std::f32::consts::PI),
            "e" => Some(std::f32::consts::E),
            "nan" => Some(f32::NAN),
            _ => None,
        };
        if let Some(constant) = constant {
            return Some(Amount::Number(constant));
        }
        self.skip_space();
        if !self.eat(b'(') {
            return None;
        }
        let mut args = Vec::new();
        loop {
            args.push(self.sum()?);
            self.skip_space();
            if self.eat(b')') {
                break;
            }
            if !self.eat(b',') {
                return None;
            }
        }
        apply(&name, &args)
    }
}

/// The function `name` of `args`. Most compare numbers, which a percentage is not until a
/// layout says what it is a percentage of: `min(50%, 10px)` keeps its text.
fn apply(name: &str, args: &[Amount]) -> Option<Amount> {
    if name == "calc" {
        return match args {
            [one] => Some(*one),
            _ => None,
        };
    }
    let (kind, numbers) = numbers(args)?;
    match (name, numbers[..].as_ref()) {
        ("min", _) => Some(kind.of_kind(numbers.iter().copied().fold(f32::INFINITY, f32::min))),
        ("max", _) => Some(kind.of_kind(numbers.iter().copied().fold(f32::NEG_INFINITY, f32::max))),
        // The value held between the bounds, and the lower bound when they cross (CSS Values 4
        // §10.3: `clamp(MIN, VAL, MAX)` is `max(MIN, min(VAL, MAX))`).
        ("clamp", &[least, value, most]) => Some(kind.of_kind(value.min(most).max(least))),
        ("abs", &[one]) => Some(kind.of_kind(one.abs())),
        ("sign", &[one]) => Some(Amount::Number(one.signum())),
        // `round(<strategy>, value)` has a keyword, which this reader does not take as a
        // number, so it never gets here.
        ("round", &[one]) => Some(kind.of_kind(one.round())),
        ("round", &[value, step]) if step != 0. => {
            Some(kind.of_kind((value / step).round() * step))
        }
        ("mod", &[a, b]) if b != 0. => Some(kind.of_kind(a - b * (a / b).floor())),
        ("rem", &[a, b]) if b != 0. => Some(kind.of_kind(a % b)),
        _ => None,
    }
}

/// The arguments as plain numbers, and the kind they all are.
fn numbers(args: &[Amount]) -> Option<(Amount, Vec<f32>)> {
    let first = *args.first()?;
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        if !arg.same_kind(first) {
            return None;
        }
        out.push(arg.plain()?);
    }
    Some((first, out))
}

/// An angle in degrees: a number and one of the angle units, or a bare number.
pub(super) fn degrees(token: &str) -> Option<f32> {
    let token = token.trim();
    let split = token
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(token.len());
    let number: f32 = token[..split].parse().ok()?;
    match unit(&token[split..])? {
        Unit::Angle(degrees) => Some(number * degrees),
        Unit::Number => Some(number),
        _ => None,
    }
}

/// A time in milliseconds: `150ms`, `0.15s`. A bare number is no time, nor is a word that
/// happens to end in `s` (`spin-forms`).
pub(super) fn milliseconds(token: &str) -> Option<f64> {
    let token = token.trim();
    let split = token.find(|c: char| c.is_ascii_alphabetic())?;
    let number: f64 = token[..split].parse().ok()?;
    match unit(&token[split..])? {
        Unit::Time(seconds) => Some((number * f64::from(seconds) * 1e6).round() / 1e3),
        _ => None,
    }
}
