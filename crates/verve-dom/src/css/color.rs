//! Colours: the notations CSS has and the spaces they name, turned into the sRGB red, green,
//! blue and alpha a drawer paints with — out-of-gamut channels clipped, as a display does.
//!
//! The legacy notations (`#hex`, `rgb()`, `hsl()` and the named colours) are sRGB already;
//! `hwb()`, `lab()`, `lch()`, `oklab()`, `oklch()`, `color()` and `color-mix()` are turned
//! into it through the matrices CSS Color 4 gives. A channel may be written relative to another
//! colour (`oklch(from var(--brand) calc(l * 0.8) c h)`), which is read by converting that
//! colour into the function's own space first.

use super::calc::degrees;
use super::syntax::{decimal, function_parts, matching_parenthesis, split_outside};

/// What `currentcolor` stands for: the colour of the element a value is on.
type Current = [f32; 4];

/// A colour as red, green, blue and alpha in `0..=1`; `currentcolor` stands for black.
pub fn color(text: &str) -> Option<[f32; 4]> {
    color_with(text, [0., 0., 0., 1.])
}

/// [`color`], with what `currentcolor` stands for: the colour of the element the value is on.
pub fn color_with(text: &str, current: Current) -> Option<[f32; 4]> {
    let text = text.trim().to_ascii_lowercase();
    if text == "currentcolor" {
        return Some(current);
    }
    if text == "transparent" {
        return Some([0., 0., 0., 0.]);
    }
    if let Some(hex) = text.strip_prefix('#') {
        return hex_color(hex);
    }
    if let Some((name, args)) = function_parts(&text) {
        return function_color(name, args, current).map(clipped);
    }
    named_color(&text)
}

/// The computed text of a colour `getComputedStyle` answers: `rgb(0, 128, 0)` for the notations
/// that are sRGB already, `rgba(0, 0, 0, 0.5)` with alpha, and `None` for one of the newer
/// spaces — which keeps the text it was written with, its own space and all.
pub(super) fn color_text(text: &str) -> Option<String> {
    let lower = text.trim().to_ascii_lowercase();
    let legacy = match function_parts(&lower) {
        Some((name, args)) => {
            matches!(name, "rgb" | "rgba" | "hsl" | "hsla" | "hwb") || args.starts_with("from ")
        }
        None => lower.starts_with('#') || lower == "transparent" || named_color(&lower).is_some(),
    };
    if !legacy {
        return None;
    }
    let [r, g, b, a] = color(&lower)?;
    Some(srgb_text(r, g, b, a))
}

/// `rgb(r, g, b)`, or `rgba(r, g, b, a)` when it is not opaque.
fn srgb_text(r: f32, g: f32, b: f32, a: f32) -> String {
    let channel = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    let (r, g, b) = (channel(r), channel(g), channel(b));
    if a >= 1. {
        return format!("rgb({r}, {g}, {b})");
    }
    format!("rgba({r}, {g}, {b}, {})", decimal(a.clamp(0., 1.)))
}

fn clipped([r, g, b, a]: [f32; 4]) -> [f32; 4] {
    [
        r.clamp(0., 1.),
        g.clamp(0., 1.),
        b.clamp(0., 1.),
        a.clamp(0., 1.),
    ]
}

fn hex_color(hex: &str) -> Option<[f32; 4]> {
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    let pair = |a: u8, b: u8| f32::from(a * 16 + b) / 255.;
    let single = |a: u8| f32::from(a * 17) / 255.;
    Some(match digits.len() {
        3 => [single(digits[0]), single(digits[1]), single(digits[2]), 1.],
        4 => [
            single(digits[0]),
            single(digits[1]),
            single(digits[2]),
            single(digits[3]),
        ],
        6 => [
            pair(digits[0], digits[1]),
            pair(digits[2], digits[3]),
            pair(digits[4], digits[5]),
            1.,
        ],
        8 => [
            pair(digits[0], digits[1]),
            pair(digits[2], digits[3]),
            pair(digits[4], digits[5]),
            pair(digits[6], digits[7]),
        ],
        _ => return None,
    })
}

fn named_color(text: &str) -> Option<[f32; 4]> {
    let hex = NAMED_COLORS
        .iter()
        .find(|(name, _)| *name == text)
        .map(|(_, hex)| *hex)?;
    hex_color(&format!("{hex:06x}"))
}

/// The channels of a colour function and its alpha, `from <colour>` resolved first: the colour
/// the channels are relative to, turned into the function's own space.
fn function_color(name: &str, args: &str, current: Current) -> Option<[f32; 4]> {
    if name == "color-mix" {
        return color_mix(args, current);
    }
    let (space, rest) = (name, args);
    if let Some(relative) = rest.strip_prefix("from ") {
        let (base, channels) = split_color(relative)?;
        let base = color_with(base, current)?;
        let channels = relative_channels(space, base, channels)?;
        return absolute(space, &channels);
    }
    absolute(space, rest)
}

/// The channels of `rest` with any that name one of `base`'s written out.
fn relative_channels(space: &str, base: [f32; 4], rest: &str) -> Option<String> {
    let (parts, alpha) = split_alpha(rest);
    let coords = coordinates(space, base)?;
    let mut out = Vec::new();
    for part in &parts {
        let name = part.to_ascii_lowercase();
        let index = match (space, name.as_str()) {
            ("rgb" | "rgba", "r") => Some(0),
            ("rgb" | "rgba", "g") => Some(1),
            ("rgb" | "rgba", "b") => Some(2),
            ("hsl" | "hsla" | "hwb", "h") => Some(0),
            ("hsl" | "hsla", "s") => Some(1),
            ("hsl" | "hsla", "l") => Some(2),
            ("hwb", "w") => Some(1),
            ("hwb", "b") => Some(2),
            ("oklab" | "lab", "l") | ("oklch" | "lch", "l") => Some(0),
            ("oklab" | "lab", "a") | ("oklch" | "lch", "c") => Some(1),
            ("oklab" | "lab" | "oklch" | "lch", "b") => Some(2),
            ("oklch" | "lch", "h") => Some(2),
            (_, "alpha") => {
                out.push(decimal(base[3]));
                continue;
            }
            _ => None,
        };
        // A channel names one of the colour's own; anything else is written out as it is.
        out.push(match index {
            Some(index) => coords[index].clone(),
            None => part.clone(),
        });
    }
    Some(match alpha {
        Some(alpha) => format!("{} / {alpha}", out.join(" ")),
        None => out.join(" "),
    })
}

/// `base` in the coordinates `space` writes its channels in.
fn coordinates(space: &str, base: [f32; 4]) -> Option<Vec<String>> {
    let [r, g, b, _] = base;
    Some(match space {
        "rgb" | "rgba" => vec![decimal(r * 255.), decimal(g * 255.), decimal(b * 255.)],
        "hsl" | "hsla" => {
            let (h, s, l) = hsl_of(r, g, b);
            vec![
                decimal(h),
                format!("{}%", decimal(s * 100.)),
                format!("{}%", decimal(l * 100.)),
            ]
        }
        "hwb" => {
            let (h, w, bl) = hwb_of(r, g, b);
            vec![
                decimal(h),
                format!("{}%", decimal(w * 100.)),
                format!("{}%", decimal(bl * 100.)),
            ]
        }
        "oklab" => {
            let (l, a, bb) = oklab_of(r, g, b);
            vec![decimal(l), decimal(a), decimal(bb)]
        }
        "oklch" => {
            let (l, a, bb) = oklab_of(r, g, b);
            let (c, h) = polar(a, bb);
            vec![decimal(l), decimal(c), decimal(h)]
        }
        _ => return None,
    })
}

/// The colour a function of `space` names, its channels written out.
fn absolute(space: &str, args: &str) -> Option<[f32; 4]> {
    let (parts, alpha) = split_alpha(args);
    if space == "color" {
        return color_function(&parts, alpha);
    }
    if parts.len() < 3 {
        return None;
    }
    // The alpha is the fourth channel with the commas of the legacy syntax, or after a `/`.
    let alpha = match (&alpha, parts.get(3)) {
        (Some(token), _) => alpha_of(Some(token)),
        (None, Some(token)) if parts.len() == 4 => alpha_of(Some(token)),
        _ => 1.,
    };
    let value = |at: usize| parts.get(at).map(String::as_str).unwrap_or("0");
    Some(match space {
        "rgb" | "rgba" => [
            rgb_channel(value(0)),
            rgb_channel(value(1)),
            rgb_channel(value(2)),
            alpha,
        ],
        "hsl" | "hsla" => {
            let [r, g, b] = hsl_to_rgb(angle(value(0)), percentage(value(1)), percentage(value(2)));
            [r, g, b, alpha]
        }
        "hwb" => {
            let [r, g, b] = hwb_to_rgb(angle(value(0)), percentage(value(1)), percentage(value(2)));
            [r, g, b, alpha]
        }
        "lab" | "lch" => {
            let (l, a, b) = match space {
                "lab" => (
                    scaled(value(0), 100.),
                    scaled(value(1), 125.),
                    scaled(value(2), 125.),
                ),
                _ => {
                    let (c, h) = (scaled(value(1), 150.), angle(value(2)));
                    let radians = h.to_radians();
                    (scaled(value(0), 100.), c * radians.cos(), c * radians.sin())
                }
            };
            let [r, g, b] = lab_to_rgb(l, a, b);
            [r, g, b, alpha]
        }
        "oklab" | "oklch" => {
            let (l, a, b) = match space {
                "oklab" => (
                    scaled(value(0), 1.),
                    scaled(value(1), 0.4),
                    scaled(value(2), 0.4),
                ),
                _ => {
                    let (c, h) = (scaled(value(1), 0.4), angle(value(2)));
                    let radians = h.to_radians();
                    (scaled(value(0), 1.), c * radians.cos(), c * radians.sin())
                }
            };
            let [r, g, b] = oklab_to_rgb(l, a, b);
            [r, g, b, alpha]
        }
        _ => return None,
    })
}

/// `color(srgb 1 0 0)`: the space it is written in and its channels, with the space as the
/// first part.
fn color_function(parts: &[String], alpha: Option<String>) -> Option<[f32; 4]> {
    if parts.len() < 4 {
        return None;
    }
    let space = parts[0].as_str();
    let alpha = alpha_of(alpha.as_deref());
    let channel = |at: usize| scaled(&parts[at], 1.);
    let [r, g, b] = [channel(1), channel(2), channel(3)];
    Some(match space {
        "srgb" => [r, g, b, alpha],
        "srgb-linear" => [gamma(r), gamma(g), gamma(b), alpha],
        "display-p3" => {
            // Over sRGB, as a display that cannot show it would: the primaries are close.
            let [r, g, b] = p3_to_srgb([r, g, b]);
            [r, g, b, alpha]
        }
        _ => return None,
    })
}

/// The spaces `color-mix()` mixes in. Any other (`lab`, `hwb`, `srgb-linear`…) is mixed as
/// sRGB, channel by channel.
#[derive(Clone, Copy, PartialEq)]
enum MixSpace {
    Srgb,
    Oklab,
    Oklch,
    Hsl,
}

impl MixSpace {
    fn named(name: &str) -> Self {
        match name {
            "oklab" => Self::Oklab,
            "oklch" => Self::Oklch,
            "hsl" | "hsla" => Self::Hsl,
            _ => Self::Srgb,
        }
    }

    /// Which of the three coordinates is a hue, if one is.
    fn hue(self) -> Option<usize> {
        match self {
            Self::Oklch => Some(2),
            Self::Hsl => Some(0),
            Self::Srgb | Self::Oklab => None,
        }
    }

    /// The coordinates of an sRGB colour in this space, and whether its hue means nothing
    /// because it has no chroma (a grey, black, `transparent`): then the other colour's hue is
    /// taken as it is.
    fn coordinates(self, [r, g, b, _]: [f32; 4]) -> ([f32; 3], bool) {
        match self {
            Self::Srgb => ([r, g, b], false),
            Self::Oklab => {
                let (l, a, b) = oklab_of(r, g, b);
                ([l, a, b], false)
            }
            Self::Oklch => {
                let (l, a, b) = oklab_of(r, g, b);
                let (c, h) = polar(a, b);
                ([l, c, h], c < 1e-4)
            }
            Self::Hsl => {
                let (h, s, l) = hsl_of(r, g, b);
                ([h, s, l], s < 1e-4)
            }
        }
    }

    fn to_rgb(self, [x, y, z]: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Srgb => [x, y, z],
            Self::Oklab => oklab_to_rgb(x, y, z),
            Self::Oklch => {
                let radians = z.to_radians();
                oklab_to_rgb(x, y * radians.cos(), y * radians.sin())
            }
            Self::Hsl => hsl_to_rgb(x, y, z),
        }
    }
}

/// `color-mix(in oklab, red 30%, blue)`: the two colours mixed in a space, by weight. The
/// colours are mixed premultiplied by their alpha (CSS Color 4 §13), so that mixing a colour
/// with `transparent` leaves the colour, only fainter.
fn color_mix(args: &str, current: Current) -> Option<[f32; 4]> {
    let parts = split_outside(args, ',');
    let [space, first, second] = parts.as_slice() else {
        return None;
    };
    let space = MixSpace::named(
        space
            .trim()
            .strip_prefix("in ")?
            .split_whitespace()
            .next()?,
    );
    let ((a, wa), (b, wb)) = (mix_operand(first, current)?, mix_operand(second, current)?);
    let (wa, wb) = match (wa, wb) {
        (None, None) => (50., 50.),
        (None, Some(w)) => (100. - w, w),
        (Some(w), None) => (w, 100. - w),
        (Some(wa), Some(wb)) => (wa, wb),
    };
    let total = (wa + wb) / 100.;
    if total <= 0. {
        return None;
    }
    let (wa, wb) = (wa / 100. / total, wb / 100. / total);
    let alpha = a[3] * wa + b[3] * wb;
    let ((ca, hue_a), (cb, hue_b)) = (space.coordinates(a), space.coordinates(b));
    let mut mixed = [0.; 3];
    for (at, out) in mixed.iter_mut().enumerate() {
        *out = if space.hue() == Some(at) {
            mix_hue((ca[at], hue_a), (cb[at], hue_b), wa, wb)
        } else if alpha > 0. {
            (ca[at] * a[3] * wa + cb[at] * b[3] * wb) / alpha
        } else {
            0.
        };
    }
    let [r, g, b] = space.to_rgb(mixed);
    // Weights adding up to less than 100% leave the mix that much more transparent.
    Some([r, g, b, alpha * total.min(1.)])
}

/// One operand of `color-mix`: the colour, and the weight it gave itself, written before or
/// after it.
fn mix_operand(text: &str, current: Current) -> Option<([f32; 4], Option<f32>)> {
    let text = text.trim();
    let pieces = split_outside(text, ' ');
    let percent = |piece: &str| piece.trim().strip_suffix('%')?.trim().parse::<f32>().ok();
    let (color, weight) = match (pieces.first(), pieces.last()) {
        (Some(first), _) if pieces.len() > 1 && percent(first).is_some() => {
            (text[first.len()..].trim(), percent(first))
        }
        (_, Some(last)) if pieces.len() > 1 && percent(last).is_some() => {
            (text[..text.len() - last.len()].trim(), percent(last))
        }
        _ => (text, None),
    };
    Some((color_with(color, current)?, weight))
}

/// The hue between two, along the shorter way round, each with whether it means anything.
fn mix_hue(a: (f32, bool), b: (f32, bool), wa: f32, wb: f32) -> f32 {
    match (a.1, b.1) {
        (true, true) => 0.,
        (true, false) => b.0,
        (false, true) => a.0,
        (false, false) => {
            let difference = ((b.0 - a.0) % 360. + 540.) % 360. - 180.;
            (a.0 + difference * wb / (wa + wb)).rem_euclid(360.)
        }
    }
}

/// The distance and the angle in degrees of an `a`/`b` pair.
fn polar(a: f32, b: f32) -> (f32, f32) {
    let c = a.hypot(b);
    let h = if c == 0. {
        0.
    } else {
        b.atan2(a).to_degrees().rem_euclid(360.)
    };
    (c, h)
}

/// A colour function's channels written out, and its alpha (`/ 50%`).
fn split_alpha(text: &str) -> (Vec<String>, Option<String>) {
    let parts = split_outside(text, '/');
    match parts.as_slice() {
        [channels, alpha] => (components(channels), Some(alpha.trim().to_owned())),
        _ => (components(text), None),
    }
}

/// `10 20 30`, `10, 20, 30` as its parts, a part with parentheses (`calc(1 + 2)`) kept whole.
fn components(text: &str) -> Vec<String> {
    split_outside(&text.replace(['\t', '\n'], " "), ',')
        .iter()
        .flat_map(|list| split_outside(list, ' '))
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}

/// The colour a relative notation is relative to: what is written first, through the end of
/// its own parentheses when it has them.
fn split_color(text: &str) -> Option<(&str, &str)> {
    let text = text.trim();
    let end = match text.find('(') {
        Some(open) => matching_parenthesis(text, open)? + 1,
        None => text.find(char::is_whitespace).unwrap_or(text.len()),
    };
    let rest = text[end..].trim();
    (!rest.is_empty()).then(|| (text[..end].trim(), rest))
}

fn alpha_of(token: Option<&str>) -> f32 {
    match token {
        Some(token) => fraction(token),
        None => 1.,
    }
}

/// A number: `0.5`, `-3`, `12deg`.
fn number(token: &str) -> f32 {
    token.trim().parse().unwrap_or(0.)
}

/// A number that may be a percentage of `full`: `50%` of 1 is `0.5`.
fn scaled(token: &str, full: f32) -> f32 {
    let token = token.trim();
    match token.strip_suffix('%') {
        Some(percent) => number(percent) / 100. * full,
        None => number(token),
    }
}

/// A saturation, lightness, whiteness or blackness: `50%`, or `50` as `hsl()` and `hwb()` take
/// a bare number, as a percentage without its sign.
fn percentage(token: &str) -> f32 {
    number(token.trim().trim_end_matches('%')) / 100.
}

/// A fraction, `50%` or `0.5`.
fn fraction(token: &str) -> f32 {
    scaled(token, 1.)
}

/// A red, green or blue channel written as a number of 255 or as a percentage.
fn rgb_channel(token: &str) -> f32 {
    let token = token.trim();
    let value = match token.strip_suffix('%') {
        Some(percent) => number(percent) / 100.,
        None => number(token) / 255.,
    };
    value.clamp(0., 1.)
}

/// An angle in degrees, from `deg`, `rad`, `grad` or `turn`.
fn angle(token: &str) -> f32 {
    degrees(token).unwrap_or(0.)
}

fn gamma(value: f32) -> f32 {
    if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}

fn linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn hsl_to_rgb(hue: f32, s: f32, l: f32) -> [f32; 3] {
    let h = hue.rem_euclid(360.) / 360.;
    let s = s.clamp(0., 1.);
    let l = l.clamp(0., 1.);
    let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
    let p = 2. * l - q;
    let channel = |mut t: f32| {
        if t < 0. {
            t += 1.;
        }
        if t > 1. {
            t -= 1.;
        }
        if t < 1. / 6. {
            p + (q - p) * 6. * t
        } else if t < 0.5 {
            q
        } else if t < 2. / 3. {
            p + (q - p) * (2. / 3. - t) * 6.
        } else {
            p
        }
    };
    [channel(h + 1. / 3.), channel(h), channel(h - 1. / 3.)]
}

/// The hue, saturation and lightness of an sRGB colour, in degrees and `0..=1`.
fn hsl_of(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.;
    let delta = max - min;
    if delta <= 0. {
        return (0., 0., l);
    }
    let s = if l <= 0.5 {
        delta / (max + min)
    } else {
        delta / (2. - max - min)
    };
    let h = if max == r {
        ((g - b) / delta).rem_euclid(6.)
    } else if max == g {
        (b - r) / delta + 2.
    } else {
        (r - g) / delta + 4.
    };
    (h * 60., s, l)
}

fn hwb_to_rgb(hue: f32, w: f32, b: f32) -> [f32; 3] {
    let (w, b) = (w.clamp(0., 1.), b.clamp(0., 1.));
    if w + b >= 1. {
        let grey = w / (w + b);
        return [grey, grey, grey];
    }
    let mut rgb = hsl_to_rgb(hue, 1., 0.5);
    for channel in &mut rgb {
        *channel = *channel * (1. - w - b) + w;
    }
    rgb
}

/// The hue, whiteness and blackness of an sRGB colour.
fn hwb_of(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (h, _, _) = hsl_of(r, g, b);
    let white = r.min(g).min(b);
    let black = 1. - r.max(g).max(b);
    (h, white, black)
}

fn oklab_to_rgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    let l_ = l + 0.39633778 * a + 0.21580376 * b;
    let m_ = l - 0.105561346 * a - 0.06385417 * b;
    let s_ = l - 0.08948418 * a - 1.2914855 * b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        gamma(4.0767417 * l - 3.3077116 * m + 0.23096994 * s),
        gamma(-1.268438 * l + 2.6097574 * m - 0.34131938 * s),
        gamma(-0.0041960864 * l - 0.7034186 * m + 1.7076147 * s),
    ]
}

fn oklab_of(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (linear(r), linear(g), linear(b));
    let l = 0.41222146 * r + 0.53633255 * g + 0.051445995 * b;
    let m = 0.2119035 * r + 0.6806995 * g + 0.10739696 * b;
    let s = 0.08830246 * r + 0.28171885 * g + 0.6299787 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.21045426 * l + 0.7936178 * m - 0.004072047 * s,
        1.9779985 * l - 2.4285922 * m + 0.4505937 * s,
        0.025904037 * l + 0.78277177 * m - 0.80867577 * s,
    )
}

fn lab_to_rgb(l: f32, a: f32, b: f32) -> [f32; 3] {
    const KAPPA: f32 = 24389. / 27.;
    const EPSILON: f32 = 216. / 24389.;
    let inverse = |f: f32| {
        let cube = f * f * f;
        if cube > EPSILON {
            cube
        } else {
            (116. * f - 16.) / KAPPA
        }
    };
    let f1 = (l + 16.) / 116.;
    let x = 0.96422 * inverse(a / 500. + f1);
    // A lightness under 8 is not `f`'s cube but the linear part of the curve.
    let y = if l > 8. { inverse(f1) } else { l / KAPPA };
    let z = 0.82521 * inverse(f1 - b / 200.);
    [
        gamma(3.133856 * x - 1.6168667 * y - 0.4906146 * z),
        gamma(-0.9787684 * x + 1.9161415 * y + 0.0334540 * z),
        gamma(0.0719453 * x - 0.2289914 * y + 1.4052427 * z),
    ]
}

/// Display-p3 over sRGB: its primaries turned into sRGB's, the way a convertor would.
fn p3_to_srgb(rgb: [f32; 3]) -> [f32; 3] {
    let r = linear(rgb[0]);
    let g = linear(rgb[1]);
    let b = linear(rgb[2]);
    let x = 0.4865709 * r + 0.2656677 * g + 0.1982173 * b;
    let y = 0.2289746 * r + 0.6917385 * g + 0.0792869 * b;
    let z = 0.0000000 * r + 0.0451134 * g + 1.0439444 * b;
    [
        gamma(3.24097 * x - 1.5373832 * y - 0.4986108 * z),
        gamma(-0.9692436 * x + 1.8759675 * y + 0.0415551 * z),
        gamma(0.0556301 * x - 0.203977 * y + 1.0569715 * z),
    ]
}

/// CSS's named colours.
const NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];
