//! `webPreferences`, as a page is made with them.

use std::path::PathBuf;

/// What `webPreferences` says of a page. A page is made with these and, when P20 makes its
/// isolate anew on a navigation, made with the same ones. The fields from `image_animation_policy`
/// on are the ones only P13 reads: they are kept, and handed to whoever draws or loads.
#[derive(Clone, Debug, PartialEq)]
pub struct WebPreferences {
    /// `devTools`.
    pub dev_tools: bool,
    /// `nodeIntegration`: whether the page's scripts see Node's modules.
    pub node_integration: bool,
    /// `nodeIntegrationInWorker`.
    pub node_integration_in_worker: bool,
    /// `nodeIntegrationInSubFrames`.
    pub node_integration_in_sub_frames: bool,
    /// `preload`: the script run before the page's own, an absolute path.
    pub preload: Option<PathBuf>,
    /// `sandbox`: on unless `nodeIntegration` is, or the app said otherwise.
    pub sandbox: bool,
    /// `partition`: the session's name; `None` is the default session.
    pub partition: Option<String>,
    /// `zoomFactor`.
    pub zoom_factor: f64,
    /// `javascript`: whether the page's scripts run at all.
    pub javascript: bool,
    /// `webSecurity`.
    pub web_security: bool,
    /// `allowRunningInsecureContent`.
    pub allow_running_insecure_content: bool,
    /// `images`: whether the page loads images.
    pub images: bool,
    /// `contextIsolation`: whether the page's scripts run in a world of their own, apart from
    /// the preload's. With it (the default) the page has no `require`, `nodeIntegration` or not.
    pub context_isolation: bool,
    /// `imageAnimationPolicy`: `animate`, `animateOnce` or `noAnimation`.
    pub image_animation_policy: String,
    /// `textAreasAreResizable`.
    pub text_areas_are_resizable: bool,
    /// `webgl`: kept; Verve has none.
    pub webgl: bool,
    /// `plugins`.
    pub plugins: bool,
    /// `experimentalFeatures`.
    pub experimental_features: bool,
    /// `scrollBounce`.
    pub scroll_bounce: bool,
    /// `offscreen`.
    pub offscreen: bool,
    /// `webviewTag`.
    pub webview_tag: bool,
    /// `safeDialogs`.
    pub safe_dialogs: bool,
    /// `safeDialogsMessage`.
    pub safe_dialogs_message: Option<String>,
    /// `disableDialogs`.
    pub disable_dialogs: bool,
    /// `navigateOnDragDrop`.
    pub navigate_on_drag_drop: bool,
    /// `disableHtmlFullscreenWindowResize`.
    pub disable_html_fullscreen_window_resize: bool,
    /// `enablePreferredSizeMode`.
    pub enable_preferred_size_mode: bool,
    /// `enableBlinkFeatures`.
    pub enable_blink_features: String,
    /// `disableBlinkFeatures`.
    pub disable_blink_features: String,
    /// `defaultFontFamily`, for the font fallback of P31.
    pub default_font_family: FontFamilies,
    /// `defaultFontSize`.
    pub default_font_size: i32,
    /// `defaultMonospaceFontSize`.
    pub default_monospace_font_size: i32,
    /// `minimumFontSize`.
    pub minimum_font_size: i32,
    /// `defaultEncoding`.
    pub default_encoding: String,
    /// `backgroundThrottling`: timers at 1 Hz and no animation frames while hidden.
    pub background_throttling: bool,
    /// `autoplayPolicy`.
    pub autoplay_policy: String,
    /// `accessibleTitle`.
    pub accessible_title: Option<String>,
    /// `spellcheck`: kept; Verve has none.
    pub spellcheck: bool,
    /// `enableWebSQL`: kept; Verve has none.
    pub enable_web_sql: bool,
    /// `v8CacheOptions`: `none`, `code`, `bypassHeatCheck`, `bypassHeatCheckAndEagerCompile`.
    pub v8_cache_options: String,
    /// `transparent`.
    pub transparent: bool,
    /// `additionalArguments`: strings added to the renderer's `process.argv`.
    pub additional_arguments: Vec<String>,
}

/// `defaultFontFamily`: the family each generic name of CSS stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFamilies {
    pub standard: String,
    pub serif: String,
    pub sans_serif: String,
    pub monospace: String,
    pub cursive: String,
    pub fantasy: String,
    pub math: String,
}

impl Default for FontFamilies {
    fn default() -> Self {
        let family = |name: &str| name.to_owned();
        Self {
            standard: family("Times New Roman"),
            serif: family("Times New Roman"),
            sans_serif: family("Arial"),
            monospace: family("Courier New"),
            cursive: family("Script"),
            fantasy: family("Impact"),
            math: family("Latin Modern Math"),
        }
    }
}

impl Default for WebPreferences {
    fn default() -> Self {
        Self {
            dev_tools: true,
            node_integration: false,
            node_integration_in_worker: false,
            node_integration_in_sub_frames: false,
            preload: None,
            sandbox: true,
            partition: None,
            zoom_factor: 1.0,
            javascript: true,
            web_security: true,
            allow_running_insecure_content: false,
            images: true,
            context_isolation: true,
            image_animation_policy: "animate".into(),
            text_areas_are_resizable: true,
            webgl: true,
            plugins: false,
            experimental_features: false,
            scroll_bounce: false,
            offscreen: false,
            webview_tag: false,
            safe_dialogs: false,
            safe_dialogs_message: None,
            disable_dialogs: false,
            navigate_on_drag_drop: false,
            disable_html_fullscreen_window_resize: false,
            enable_preferred_size_mode: false,
            enable_blink_features: String::new(),
            disable_blink_features: String::new(),
            default_font_family: FontFamilies::default(),
            default_font_size: 16,
            default_monospace_font_size: 13,
            minimum_font_size: 0,
            default_encoding: "ISO-8859-1".into(),
            background_throttling: true,
            autoplay_policy: "no-user-gesture-required".into(),
            accessible_title: None,
            spellcheck: true,
            enable_web_sql: true,
            v8_cache_options: "code".into(),
            transparent: true,
            additional_arguments: Vec::new(),
        }
    }
}
