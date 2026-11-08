//! `new BrowserWindow(options)`'s options, read as Electron 39 reads them (observed on 39.8.10,
//! Windows; the probe is `tests/electron/browser-window/observe`): leniently, a field of the wrong
//! type is left at its default and nothing throws. A size or a position is an integer (`'300'` and
//! `300.7` are no width), `x` and `y` count only together, `center` yields to them, and a window is
//! never smaller than the system makes one.
//!
//! The colours are Electron's: `#RGB`, `#ARGB`, `#RRGGBB`, `#AARRGGBB` (alpha first, not CSS's
//! last), `rgb()`, `rgba()`, `hsl()`, `hsla()` and the lower-case CSS names; anything else is the
//! default white.

use boa_engine::{Context, JsObject, JsResult};

use crate::api::define::Fields;
use crate::msg::{TitleBarStyle, WindowOptions};
use crate::window_state::{Flag, WindowState};

/// What the constructor's options say, besides the [`WindowOptions`] the shell is sent.
pub(crate) struct Read {
    pub options: WindowOptions,
    /// The flags the options set; the rest keep their start.
    pub flags: Vec<(Flag, bool)>,
    pub opacity: f64,
    /// `false` keeps a window made hidden from saying `ready-to-show` until it is shown.
    pub paint_when_initially_hidden: bool,
    /// The window `parent` names, if it is an object: whether it is a `BrowserWindow` is for the
    /// caller to say.
    pub parent: Option<JsObject>,
}

/// The options `fields` give; `title` is what a window with no title of its own is called (the
/// app's name).
pub(crate) fn read(fields: &Fields, title: &str, context: &mut Context) -> JsResult<Read> {
    let transparent = fields.bool("transparent", false, context)?;
    let kiosk = fields.bool("kiosk", false, context)?;
    let mut options = WindowOptions {
        frame: fields.bool("frame", true, context)?,
        transparent,
        show: fields.bool("show", true, context)?,
        fullscreen: fields.bool("fullscreen", false, context)? || kiosk,
        resizable: fields.bool("resizable", !transparent, context)?,
        movable: fields.bool("movable", true, context)?,
        minimizable: fields.bool("minimizable", true, context)?,
        maximizable: fields.bool("maximizable", true, context)?,
        closable: fields.bool("closable", true, context)?,
        focusable: fields.bool("focusable", true, context)?,
        always_on_top: fields.bool("alwaysOnTop", false, context)?,
        skip_taskbar: fields.bool("skipTaskbar", false, context)?,
        kiosk,
        title: fields
            .string("title", context)?
            .unwrap_or_else(|| title.to_owned()),
        icon: fields.string("icon", context)?.map(Into::into),
        title_bar_style: match fields.string("titleBarStyle", context)?.as_deref() {
            Some("hidden") => TitleBarStyle::Hidden,
            Some("hiddenInset" | "customButtonsOnHover") => TitleBarStyle::HiddenInset,
            _ => TitleBarStyle::Default,
        },
        ..WindowOptions::default()
    };
    read_geometry(fields, &mut options, context)?;
    let color = match fields.string("backgroundColor", context)? {
        Some(text) => parse_color(&text),
        None => None,
    };
    options.background_color =
        color.map(|[r, g, b, a]| [r, g, b, if transparent { a } else { 255 }]);
    let parent = fields.object("parent", context)?;
    options.modal = fields.bool("modal", false, context)? && parent.is_some();
    let flags = vec![
        (
            Flag::FullScreenable,
            fields.bool("fullscreenable", true, context)?,
        ),
        (
            Flag::HasShadow,
            fields.bool("hasShadow", !cfg!(windows), context)?,
        ),
        (
            Flag::MenuBarAutoHide,
            fields.bool("autoHideMenuBar", false, context)?,
        ),
        (
            Flag::SimpleFullScreen,
            fields.bool("simpleFullscreen", false, context)? && cfg!(target_os = "macos"),
        ),
        (
            Flag::HiddenInMissionControl,
            fields.bool("hiddenInMissionControl", false, context)?,
        ),
    ];
    Ok(Read {
        flags,
        opacity: fields.f64("opacity", 1., context)?.clamp(0., 1.),
        paint_when_initially_hidden: fields.bool("paintWhenInitiallyHidden", true, context)?,
        parent,
        options,
    })
}

/// The size and place: whole numbers only, the size of the content when `useContentSize` says so
/// and never below what the system allows; a position needs both `x` and `y`.
fn read_geometry(
    fields: &Fields,
    options: &mut WindowOptions,
    context: &mut Context,
) -> JsResult<()> {
    let mut width = fields.int("width", options.width, context)?;
    let mut height = fields.int("height", options.height, context)?;
    if fields.bool("useContentSize", false, context)? {
        (width, height) = WindowState::outside_of(width, height, options.frame);
    }
    (options.width, options.height) = WindowState::smallest((width, height));
    if let (Some(x), Some(y)) = (fields.int_opt("x", context)?, fields.int_opt("y", context)?) {
        options.x = Some(x);
        options.y = Some(y);
    }
    let limit = |name: &str, context: &mut Context| fields.int(name, 0., context);
    let min = (limit("minWidth", context)?, limit("minHeight", context)?);
    let max = (limit("maxWidth", context)?, limit("maxHeight", context)?);
    options.min_size = (min != (0., 0.)).then_some(min);
    options.max_size = (max != (0., 0.)).then_some(max);
    Ok(())
}

/// Electron's reading of a colour: `[r, g, b, a]`, or `None` for text that is none.
pub(crate) fn parse_color(text: &str) -> Option<[u8; 4]> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        return hex_color(hex);
    }
    // The names are read as they are written in lower case; `transparent` is not one.
    let name = text.chars().all(|c| c.is_ascii_alphabetic());
    if name && (text != text.to_ascii_lowercase() || text == "transparent") {
        return None;
    }
    let [r, g, b, a] = verve_dom::css::color(text)?;
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    Some([channel(r), channel(g), channel(b), channel(a)])
}

/// `#RGB`, `#ARGB`, `#RRGGBB` or `#AARRGGBB`: the alpha, when there is one, comes first.
fn hex_color(hex: &str) -> Option<[u8; 4]> {
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    let (alpha, rgb) = match digits.len() {
        3 | 6 => (None, &digits[..]),
        4 | 8 => (
            Some(&digits[..digits.len() / 4]),
            &digits[digits.len() / 4..],
        ),
        _ => return None,
    };
    let width = rgb.len() / 3;
    let value = |digits: &[u8]| {
        let number = digits.iter().fold(0u32, |sum, d| sum * 16 + u32::from(*d));
        if digits.len() == 1 {
            number * 17
        } else {
            number
        }
    };
    let channel = |at: usize| value(&rgb[at * width..(at + 1) * width]) as u8;
    let a = alpha.map_or(255, value) as u8;
    Some([channel(0), channel(1), channel(2), a])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_put_the_alpha_first() {
        assert_eq!(parse_color("#abc"), Some([0xaa, 0xbb, 0xcc, 255]));
        assert_eq!(parse_color("#8abc"), Some([0xaa, 0xbb, 0xcc, 0x88]));
        assert_eq!(parse_color("#80aabbcc"), Some([0xaa, 0xbb, 0xcc, 0x80]));
        assert_eq!(parse_color("#123456"), Some([0x12, 0x34, 0x56, 255]));
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn functions_and_lower_case_names_are_read_and_the_rest_is_not() {
        assert_eq!(parse_color("rgb(1,2,3)"), Some([1, 2, 3, 255]));
        assert_eq!(parse_color("rgba(1,2,3,0.5)"), Some([1, 2, 3, 128]));
        assert_eq!(parse_color("hsl(120,100%,50%)"), Some([0, 255, 0, 255]));
        assert_eq!(parse_color("red"), Some([255, 0, 0, 255]));
        assert_eq!(parse_color("RED"), None);
        assert_eq!(parse_color("transparent"), None);
        assert_eq!(parse_color("notacolor"), None);
    }
}
