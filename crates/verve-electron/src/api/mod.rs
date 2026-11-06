//! `require('electron')`, as each of the app's processes has it: the browser process's API in the
//! main isolate, the page's in a renderer's. Every class and function is a native Boa function,
//! made the first time a script requires the module.

pub(crate) mod app;
pub(crate) mod browser_window;
pub(crate) mod clipboard;
pub(crate) mod command_line;
pub(crate) mod contents_history;
pub(crate) mod contents_ipc;
pub(crate) mod contents_page;
pub(crate) mod context_bridge;
pub mod define;
pub(crate) mod dialog;
pub(crate) mod input_events;
pub(crate) mod ipc_main;
pub(crate) mod ipc_renderer;
pub(crate) mod message_port_main;
pub(crate) mod native_image;
pub(crate) mod native_theme;
pub(crate) mod navigation;
pub(crate) mod notification;
pub(crate) mod page_calls;
pub(crate) mod page_events;
pub(crate) mod page_state;
pub(crate) mod power;
pub(crate) mod process;
pub(crate) mod safe_storage;
pub(crate) mod screen;
pub(crate) mod shell;
pub(crate) mod system_preferences;
pub(crate) mod web_contents;
pub(crate) mod web_frame;
pub(crate) mod web_frame_main;
pub(crate) mod web_preferences;
pub(crate) mod web_utils;
pub(crate) mod window_events;
pub(crate) mod window_geometry;
pub(crate) mod window_lifecycle;
pub(crate) mod window_methods;
pub(crate) mod window_open;
pub(crate) mod window_options;
pub(crate) mod windows;

use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsResult, JsString, JsValue, js_string};
use verve::native;

use crate::hub::{Hub, RendererHandle};
use crate::ids::{ContentsId, WindowId};
use crate::msg::WebPreferences;

/// What the browser process's API keeps between calls: the [`Hub`] every method goes through, and
/// for each window it made, the object standing for it, the page it shows and its cached state.
pub(crate) struct MainState {
    /// The router every method of the API sends through.
    pub(crate) hub: Arc<Hub>,
    /// The app object's state: its name, paths, switches and where it is in ending.
    pub(crate) app: app::AppState,
    /// The windows the app made, until their `'closed'` has been heard.
    pub(crate) windows: windows::Windows,
    /// The dialogs asked for and not yet answered.
    pub(crate) dialogs: dialog::Pending,
    /// The scripts asked of pages and not yet answered.
    pub(crate) calls: page_calls::Calls,
    /// The legacy `webContents` methods already warned about.
    pub(crate) deprecated: std::cell::RefCell<Vec<String>>,
    /// The `BrowserWindow` class, for the windows `window.open` makes.
    pub(crate) window_class: std::cell::RefCell<Option<boa_engine::JsObject>>,
}

impl MainState {
    fn new(hub: Arc<Hub>, app: app::AppState) -> Rc<Self> {
        Rc::new(Self {
            hub,
            app,
            windows: windows::Windows::default(),
            dialogs: dialog::Pending::default(),
            calls: page_calls::Calls::default(),
            deprecated: std::cell::RefCell::new(Vec::new()),
            window_class: std::cell::RefCell::new(None),
        })
    }

    /// The page `window` shows, while a script can still use the window: once it is destroyed, a
    /// method of it throws `Object has been destroyed`, as Electron's does. The record is kept
    /// until the window's `'closed'` listeners have all run.
    pub(crate) fn page(&self, window: WindowId) -> Option<RendererHandle> {
        self.hub.contents_of(window)?;
        Some(self.windows.live(window)?.page.clone())
    }

    /// The record of the window that shows page `contents`, while a script can use it.
    pub(crate) fn showing(&self, contents: ContentsId) -> Option<Rc<windows::WindowRecord>> {
        self.windows.live(self.hub.window_of(contents)?)
    }
}

/// Gives `runtime` the browser process's `require('electron')`.
pub fn install(runtime: &mut verve::Runtime, hub: Arc<Hub>) {
    install_state(runtime, hub);
}

/// [`install`], answering what the API keeps.
pub(crate) fn install_state(runtime: &mut verve::Runtime, hub: Arc<Hub>) -> Rc<MainState> {
    let state = MainState::new(Arc::clone(&hub), app::AppState::new(&hub.launch()));
    runtime.context().insert_data(Rc::clone(&state));
    runtime.define_builtin_lazy("electron", main_module);
    state
}

/// Gives a page's runtime its `require('electron')`, and, for a page with `nodeIntegration`, no sandbox and no
/// context isolation, the `require` to ask for it with. A page with isolation gets its API through
/// a preload and `contextBridge`, and its worlds are P21's.
pub(crate) fn install_page(
    runtime: &mut verve::Runtime,
    hub: Arc<Hub>,
    page: (ContentsId, u64),
    prefs: &WebPreferences,
) -> JsResult<()> {
    ipc_renderer::install(runtime.context(), hub, page)?;
    // Electron 39 takes `clipboard` from a sandboxed renderer; a page with Node still has it, and
    // `nativeImage`, which P21 gives a sandboxed preload by `native_image::make`.
    let module = if prefs.node_integration && !prefs.sandbox {
        renderer_module_with_system
    } else {
        renderer_module
    };
    runtime.define_builtin_lazy("electron", module);
    if prefs.node_integration && !prefs.context_isolation && !prefs.sandbox {
        let require = runtime.global_require();
        native::define_global(runtime.context(), "require", require)?;
    }
    Ok(())
}

/// What the browser process's API keeps, from inside one of its native functions.
///
/// # Panics
///
/// The realm was not given [`install`]: no native function of this API can run in it.
pub(crate) fn shared(context: &Context) -> Rc<MainState> {
    context
        .get_data::<Rc<MainState>>()
        .cloned()
        .expect("the electron module is installed before its classes are used")
}

/// Makes one of the modules the main process's `electron` holds.
type ModuleMaker = fn(&mut Context) -> JsResult<JsValue>;

/// The main process's `require('electron')`.
fn main_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let exports = native::object(context);
    let window = browser_window::make(context)?;
    exports.set(js_string!("BrowserWindow"), window, false, context)?;
    exports.set(js_string!("app"), app::make(context)?, false, context)?;
    ipc_main::add_exports(context, &exports)?;
    let modules: [(&str, ModuleMaker); 13] = [
        ("webContents", web_contents::make_module),
        ("webFrameMain", web_frame_main::make_module),
        ("dialog", dialog::make),
        ("Notification", notification::make),
        ("shell", shell::make),
        ("clipboard", clipboard::make),
        ("nativeImage", native_image::make),
        ("nativeTheme", native_theme::make),
        ("screen", screen::make),
        ("powerSaveBlocker", power::make_blocker),
        ("powerMonitor", power::make_monitor),
        ("safeStorage", safe_storage::make),
        ("systemPreferences", system_preferences::make),
    ];
    for (name, make) in modules {
        exports.set(JsString::from(name), make(context)?, false, context)?;
    }
    Ok(exports.into())
}

/// The `require('electron')` of a page with `nodeIntegration` and no sandbox: the page's, and the
/// modules that need no main process.
fn renderer_module_with_system(context: &mut Context, name: &str) -> JsResult<JsValue> {
    let exports = renderer_module(context, name)?;
    if let Some(object) = exports.as_object() {
        object.set(
            js_string!("clipboard"),
            clipboard::make(context)?,
            false,
            context,
        )?;
        let image = native_image::make(context)?;
        object.set(js_string!("nativeImage"), image, false, context)?;
    }
    Ok(exports)
}

/// A page's `require('electron')`: the members the world that asks has.
fn renderer_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    Ok(crate::preload::electron_exports(context)?.into())
}
