//! `nativeTheme`: whether the app draws dark, and the setting that overrides the system's. The Hub
//! keeps the choice and tells every page (`prefers-color-scheme`) and the shell (the frames); this
//! is the `EventEmitter` a script reads and writes it through, which emits `'updated'` when
//! `shouldUseDarkColors` changes, whether a script or the person changed it.

use std::cell::Cell;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native;

use super::define::{self, Args};
use super::shared;
use crate::msg::ThemeSource;

/// The `nativeTheme` object and the value it last told the script of.
struct Theme {
    object: JsObject,
    told: Cell<bool>,
}

/// The `nativeTheme` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let emitter = define::emitter(context)?;
    let theme = emitter.construct(&[], None, context)?;
    native::accessors(
        context,
        &theme,
        &[
            ("shouldUseDarkColors", dark, None),
            ("themeSource", source, Some(set_source)),
            ("shouldUseHighContrastColors", high_contrast, None),
            (
                "shouldUseInvertedColorScheme",
                |_, _, _| Ok(false.into()),
                None,
            ),
            ("inForcedColorsMode", high_contrast, None),
            ("prefersReducedTransparency", reduced_transparency, None),
        ],
    )?;
    let told = shared(context).hub.dark();
    context.insert_data(Theme {
        object: theme.clone(),
        told: Cell::new(told),
    });
    Ok(theme.into())
}

fn dark(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(shared(context).hub.dark().into())
}

fn high_contrast(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(crate::os::high_contrast().into())
}

fn reduced_transparency(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(crate::os::reduced_transparency().into())
}

fn source(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = match shared(context).hub.theme_source() {
        ThemeSource::System => "system",
        ThemeSource::Light => "light",
        ThemeSource::Dark => "dark",
    };
    Ok(native::js_text(name))
}

fn set_source(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = Args::new(args).string(0, "themeSource", context)?;
    let source = match name.as_str() {
        "system" => ThemeSource::System,
        "light" => ThemeSource::Light,
        "dark" => ThemeSource::Dark,
        _ => {
            return Err(Args::new(args).conversion_failure(0));
        }
    };
    shared(context).hub.set_theme_source(source);
    updated(context);
    Ok(JsValue::undefined())
}

/// Emits `'updated'` when `shouldUseDarkColors` is not what the script was last told. Called when
/// the source is set and when the system's theme changes.
pub(crate) fn updated(context: &mut Context) {
    let now = shared(context).hub.dark();
    let Some(theme) = context.get_data::<Theme>() else {
        return;
    };
    if theme.told.replace(now) == now {
        return;
    }
    let object = theme.object.clone();
    let emitted = define::emit(context, &object, "updated", &[]);
    define::report(context, emitted);
}
