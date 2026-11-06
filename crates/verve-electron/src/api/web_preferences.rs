//! `webPreferences`, as `new BrowserWindow({ webPreferences })` reads them: leniently, over
//! Electron 39's defaults (a field of the wrong type is left at its default), into the
//! [`WebPreferences`] the Hub makes a page with.
//!
//! Two defaults depend on others: `sandbox` is on unless `nodeIntegration` is, and `webSecurity:
//! false` lets insecure content run unless it was said otherwise. `app.enableSandbox()` forces
//! the sandbox on whatever the window says.

use boa_engine::{Context, JsResult};

use crate::api::define::Fields;
use crate::msg::{FontFamilies, WebPreferences};

/// The preferences `fields` give, `sandbox_forced` being whether `app.enableSandbox()` was called.
pub(crate) fn read(
    fields: &Fields,
    sandbox_forced: bool,
    context: &mut Context,
) -> JsResult<WebPreferences> {
    let mut prefs = WebPreferences::default();
    read_flags(fields, &mut prefs, context)?;
    read_text(fields, &mut prefs, context)?;
    prefs.node_integration = fields.bool("nodeIntegration", false, context)?;
    prefs.sandbox = sandbox_forced || fields.bool("sandbox", !prefs.node_integration, context)?;
    prefs.web_security = fields.bool("webSecurity", true, context)?;
    prefs.allow_running_insecure_content =
        fields.bool("allowRunningInsecureContent", !prefs.web_security, context)?;
    prefs.zoom_factor = fields.f64("zoomFactor", prefs.zoom_factor, context)?;
    // Electron's font sizes are integers: a fraction is no size and leaves the default.
    let mut size = |name: &str, slot: &mut i32| -> JsResult<()> {
        if let Some(value) = fields.int_opt(name, context)? {
            *slot = value as i32;
        }
        Ok(())
    };
    size("defaultFontSize", &mut prefs.default_font_size)?;
    size(
        "defaultMonospaceFontSize",
        &mut prefs.default_monospace_font_size,
    )?;
    size("minimumFontSize", &mut prefs.minimum_font_size)?;
    if let Some(families) = fields.object("defaultFontFamily", context)? {
        prefs.default_font_family = font_families(&Fields::new(Some(&families)), context)?;
    }
    Ok(prefs)
}

/// The booleans of the table, each over its default.
fn read_flags(fields: &Fields, prefs: &mut WebPreferences, context: &mut Context) -> JsResult<()> {
    let mut flag = |name: &str, slot: &mut bool| -> JsResult<()> {
        *slot = fields.bool(name, *slot, context)?;
        Ok(())
    };
    flag("devTools", &mut prefs.dev_tools)?;
    flag(
        "nodeIntegrationInWorker",
        &mut prefs.node_integration_in_worker,
    )?;
    flag(
        "nodeIntegrationInSubFrames",
        &mut prefs.node_integration_in_sub_frames,
    )?;
    flag("contextIsolation", &mut prefs.context_isolation)?;
    flag("javascript", &mut prefs.javascript)?;
    flag("images", &mut prefs.images)?;
    flag("textAreasAreResizable", &mut prefs.text_areas_are_resizable)?;
    flag("webgl", &mut prefs.webgl)?;
    flag("plugins", &mut prefs.plugins)?;
    flag("experimentalFeatures", &mut prefs.experimental_features)?;
    flag("scrollBounce", &mut prefs.scroll_bounce)?;
    flag("offscreen", &mut prefs.offscreen)?;
    flag("webviewTag", &mut prefs.webview_tag)?;
    flag("safeDialogs", &mut prefs.safe_dialogs)?;
    flag("disableDialogs", &mut prefs.disable_dialogs)?;
    flag("navigateOnDragDrop", &mut prefs.navigate_on_drag_drop)?;
    flag(
        "disableHtmlFullscreenWindowResize",
        &mut prefs.disable_html_fullscreen_window_resize,
    )?;
    flag(
        "enablePreferredSizeMode",
        &mut prefs.enable_preferred_size_mode,
    )?;
    flag("backgroundThrottling", &mut prefs.background_throttling)?;
    flag("spellcheck", &mut prefs.spellcheck)?;
    flag("enableWebSQL", &mut prefs.enable_web_sql)?;
    flag("transparent", &mut prefs.transparent)
}

/// The strings of the table.
fn read_text(fields: &Fields, prefs: &mut WebPreferences, context: &mut Context) -> JsResult<()> {
    let mut text = |name: &str, slot: &mut String| text_into(fields, name, slot, context);
    text("imageAnimationPolicy", &mut prefs.image_animation_policy)?;
    text("enableBlinkFeatures", &mut prefs.enable_blink_features)?;
    text("disableBlinkFeatures", &mut prefs.disable_blink_features)?;
    text("defaultEncoding", &mut prefs.default_encoding)?;
    text("autoplayPolicy", &mut prefs.autoplay_policy)?;
    text("v8CacheOptions", &mut prefs.v8_cache_options)?;
    prefs.preload = fields
        .string("preload", context)?
        .map(std::path::PathBuf::from);
    prefs.partition = fields.string("partition", context)?;
    prefs.safe_dialogs_message = fields.string("safeDialogsMessage", context)?;
    prefs.accessible_title = fields.string("accessibleTitle", context)?;
    Ok(())
}

/// Puts the field's string in `slot`, when it has one.
fn text_into(
    fields: &Fields,
    name: &str,
    slot: &mut String,
    context: &mut Context,
) -> JsResult<()> {
    if let Some(value) = fields.string(name, context)? {
        *slot = value;
    }
    Ok(())
}

fn font_families(fields: &Fields, context: &mut Context) -> JsResult<FontFamilies> {
    let mut families = FontFamilies::default();
    let mut family = |name: &str, slot: &mut String| text_into(fields, name, slot, context);
    family("standard", &mut families.standard)?;
    family("serif", &mut families.serif)?;
    family("sansSerif", &mut families.sans_serif)?;
    family("monospace", &mut families.monospace)?;
    family("cursive", &mut families.cursive)?;
    family("fantasy", &mut families.fantasy)?;
    family("math", &mut families.math)?;
    Ok(families)
}
