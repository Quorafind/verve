//! `systemPreferences`, as Electron 39 has it on Windows: the accent colour, the system colours,
//! the animation settings and the media-access status. The macOS-only methods are not defined,
//! as on Electron's Windows build (`typeof systemPreferences.getUserDefault` is `'undefined'`
//! there), so code that feature-detects them takes its other branch.

use boa_engine::{Context, JsResult, JsValue};
use verve::native;

use super::define::{self, Args};
use crate::os::SystemColor;

/// The `systemPreferences` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("getAccentColor", 0, |_, _, _| {
                Ok(native::js_text(&crate::os::accent_color()))
            }),
            ("getColor", 1, color),
            ("getAnimationSettings", 0, animation_settings),
            ("getMediaAccessStatus", 1, media_access_status),
        ],
    )?;
    Ok(module.into())
}

/// `getColor(name)`: `#RRGGBBAA`; a name Electron does not know is `Unknown color: <name>`.
fn color(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = Args::new(args).string(0, "color", context)?;
    match SystemColor::named(&name) {
        Some(color) => Ok(native::js_text(&crate::os::system_color(color))),
        None => Err(define::error(&format!("Unknown color: {name}"))),
    }
}

fn animation_settings(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let rich = crate::os::animations_enabled();
    let fields = [
        ("shouldRenderRichAnimation", rich.into()),
        ("scrollAnimationsEnabledBySystem", rich.into()),
        ("prefersReducedMotion", (!rich).into()),
    ];
    Ok(define::record(context, &fields)?.into())
}

/// `getMediaAccessStatus(mediaType)`: Windows asks the user per app in its own settings, which
/// Verve does not read, so access is `'granted'`.
fn media_access_status(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = Args::new(args).string(0, "mediaType", context)?;
    match kind.as_str() {
        "microphone" | "camera" | "screen" => Ok(native::js_text("granted")),
        _ => Err(define::error("Invalid media type")),
    }
}
