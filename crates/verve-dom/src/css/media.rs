//! `@media` and `matchMedia`: what the viewport and the window's preferences answer.

use super::syntax::split_outside;
use super::values::length;

/// A media query, as `@media` and `matchMedia` give it.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaQuery(String);

/// What media queries are asked of: the viewport and what the window prefers.
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub width: f32,
    pub height: f32,
    pub device_pixel_ratio: f32,
    pub dark: bool,
    pub reduced_motion: bool,
    /// `print` rather than `screen`.
    pub print: bool,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            width: 1280.,
            height: 800.,
            device_pixel_ratio: 1.,
            dark: false,
            reduced_motion: false,
            print: false,
        }
    }
}

impl MediaQuery {
    pub fn new(text: &str) -> Self {
        Self(text.trim().to_owned())
    }

    pub fn text(&self) -> &str {
        &self.0
    }

    /// Whether the query holds in `env`. A list holds when any of its queries does; a feature
    /// this engine does not know holds, as a window that draws what it is given would say.
    pub fn matches(&self, env: &Environment) -> bool {
        if self.0.is_empty() {
            return true;
        }
        split_outside(&self.0, ',')
            .iter()
            .any(|query| query_matches(query.trim(), env))
    }
}

fn query_matches(query: &str, env: &Environment) -> bool {
    let lower = query.to_ascii_lowercase();
    let (negated, rest) = match lower.strip_prefix("not ") {
        Some(rest) => (true, rest.trim().to_owned()),
        None => (
            false,
            lower
                .strip_prefix("only ")
                .unwrap_or(&lower)
                .trim()
                .to_owned(),
        ),
    };
    let mut holds = true;
    for term in split_terms(&rest) {
        let term = term.trim();
        let ok = if let Some(inner) = term.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            feature_matches(inner.trim(), env)
        } else {
            match term {
                "all" | "" => true,
                "screen" => !env.print,
                "print" => env.print,
                _ => false,
            }
        };
        holds &= ok;
    }
    holds != negated
}

/// `screen and (min-width: 1px) and (hover)` as its terms.
fn split_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 && text[i..].starts_with(" and ") => {
                out.push(text[start..i].to_owned());
                i += 5;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(text[start..].to_owned());
    out
}

fn feature_matches(feature: &str, env: &Environment) -> bool {
    // Ranges: `width >= 600px`, `400px < width < 800px`.
    for op in [">=", "<=", ">", "<", "="] {
        if feature.contains(op) && !feature.contains(':') {
            let parts: Vec<&str> = feature.split(op).map(str::trim).collect();
            if parts.len() == 2 {
                let (name, value, flipped) = if let Some(v) = media_length(parts[1]) {
                    (parts[0], v, false)
                } else if let Some(v) = media_length(parts[0]) {
                    (parts[1], v, true)
                } else {
                    return true;
                };
                let have = match name {
                    "width" => env.width,
                    "height" => env.height,
                    _ => return true,
                };
                let (a, b) = if flipped {
                    (value, have)
                } else {
                    (have, value)
                };
                return match op {
                    ">=" => a >= b,
                    "<=" => a <= b,
                    ">" => a > b,
                    "<" => a < b,
                    _ => a == b,
                };
            }
        }
    }
    let (name, value) = match feature.split_once(':') {
        Some((name, value)) => (name.trim(), Some(value.trim())),
        None => (feature.trim(), None),
    };
    let width = |v: Option<&str>| v.and_then(media_length);
    match (name, value) {
        ("min-width", v) => width(v).is_none_or(|w| env.width >= w),
        ("max-width", v) => width(v).is_none_or(|w| env.width <= w),
        ("width", v) => width(v).is_none_or(|w| env.width == w),
        ("min-height", v) => width(v).is_none_or(|h| env.height >= h),
        ("max-height", v) => width(v).is_none_or(|h| env.height <= h),
        ("height", v) => width(v).is_none_or(|h| env.height == h),
        ("prefers-color-scheme", Some(v)) => (v == "dark") == env.dark,
        ("prefers-reduced-motion", Some(v)) => (v == "reduce") == env.reduced_motion,
        ("prefers-reduced-motion", None) => env.reduced_motion,
        ("prefers-contrast", Some(v)) => v == "no-preference",
        ("forced-colors", Some(v)) => v == "none",
        ("forced-colors", None) => false,
        ("hover" | "any-hover", Some(v)) => v == "hover",
        ("hover" | "any-hover", None) => true,
        ("pointer" | "any-pointer", Some(v)) => v == "fine",
        ("pointer" | "any-pointer", None) => true,
        ("orientation", Some(v)) => (v == "landscape") == (env.width >= env.height),
        ("min-resolution" | "-webkit-min-device-pixel-ratio", Some(v)) => {
            resolution(v).is_none_or(|r| env.device_pixel_ratio >= r)
        }
        ("max-resolution" | "-webkit-max-device-pixel-ratio", Some(v)) => {
            resolution(v).is_none_or(|r| env.device_pixel_ratio <= r)
        }
        ("display-mode", Some(v)) => v == "browser",
        ("scripting", Some(v)) => v == "enabled",
        _ => true,
    }
}

fn media_length(text: &str) -> Option<f32> {
    length(text, 16.)
}

fn resolution(text: &str) -> Option<f32> {
    let text = text.trim();
    text.strip_suffix("dppx")
        .or_else(|| text.strip_suffix('x'))
        .and_then(|n| n.trim().parse().ok())
        .or_else(|| {
            text.strip_suffix("dpi")
                .and_then(|n| n.trim().parse::<f32>().ok())
                .map(|d| d / 96.)
        })
        .or_else(|| text.parse().ok())
}
