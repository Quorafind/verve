//! What a window is made with: the address, the viewport, who loads its resources.

use crate::{CookieBackend, FrameClock, Loader, PermissionBackend, StorageBackend};

/// What the window is.
pub struct Options {
    /// The page's address: `location`, `document.URL`, what relative links resolve against.
    pub url: String,
    /// `navigator.userAgent`; `None` keeps Verve's own.
    pub user_agent: Option<String>,
    /// `navigator.language`; `None` keeps Verve's own (`en-US`).
    pub language: Option<String>,
    /// The viewport, in CSS pixels: `innerWidth`/`innerHeight`, what `@media` and
    /// `matchMedia` ask of.
    pub viewport: (f32, f32),
    pub device_pixel_ratio: f32,
    /// `prefers-color-scheme: dark`.
    pub dark: bool,
    /// How `requestAnimationFrame` is paced.
    pub frames: FrameClock,
    /// Whether the user agent's own sheet (`div { display: block }`, the headings' sizes…) is in
    /// the cascade.
    pub user_agent_sheet: bool,
    /// Stylesheets of the embedder's, by name, after the user agent's and before the page's.
    pub stylesheets: Vec<(String, String)>,
    /// Where `localStorage` lives between runs; `None` keeps it for this run only.
    pub storage: Option<Box<dyn StorageBackend>>,
    /// `document.title` when the page has no `<title>`.
    pub title: String,
    /// What the page loads its scripts, sheets and images through; `None` loads nothing.
    pub loader: Option<Box<dyn Loader>>,
    /// The custom schemes that are "standard" (`registerSchemesAsPrivileged`): their addresses
    /// have an origin of `scheme://host`, so `location.origin`, `a.origin` and `history`'s
    /// same-origin check take one, where any other custom scheme's origin is opaque (`"null"`).
    pub standard_schemes: Vec<String>,
    /// The custom schemes whose pages are secure contexts (`window.isSecureContext`).
    pub secure_schemes: Vec<String>,
    /// The jar `document.cookie` reads and writes; `None` keeps the page's cookies in the window.
    pub cookies: Option<Box<dyn CookieBackend>>,
    /// Who says whether the page may use a permission (`Notification`, `navigator.permissions`,
    /// `clipboard.readText`); `None` refuses them all.
    pub permissions: Option<Box<dyn PermissionBackend>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            url: "about:blank".into(),
            user_agent: None,
            language: None,
            viewport: (1280., 800.),
            device_pixel_ratio: 1.,
            dark: false,
            frames: FrameClock::Timer,
            user_agent_sheet: true,
            stylesheets: Vec::new(),
            storage: None,
            title: String::new(),
            loader: None,
            standard_schemes: Vec::new(),
            secure_schemes: Vec::new(),
            cookies: None,
            permissions: None,
        }
    }
}
