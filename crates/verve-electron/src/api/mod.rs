//! `require('electron')`, as each of the app's processes has it: the browser process's API in the
//! main isolate, the page's in a renderer's. Every class and function is a native Boa function,
//! made the first time a script requires the module.

pub(crate) mod app;
pub(crate) mod browser_window;
pub(crate) mod command_line;
pub(crate) mod contents_ipc;
pub mod define;
pub(crate) mod ipc_main;
pub(crate) mod ipc_renderer;
pub(crate) mod message_port_main;
pub(crate) mod process;
pub(crate) mod web_frame_main;
pub(crate) mod web_preferences;
pub(crate) mod window_events;
pub(crate) mod window_geometry;
pub(crate) mod window_lifecycle;
pub(crate) mod window_methods;
pub(crate) mod window_options;
pub(crate) mod windows;

use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsResult, JsValue, js_string};
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
}

impl MainState {
    fn new(hub: Arc<Hub>, app: app::AppState) -> Rc<Self> {
        Rc::new(Self {
            hub,
            app,
            windows: windows::Windows::default(),
        })
    }

    /// The page `window` shows, while a script can still use the window: once it is destroyed, a
    /// method of it throws `Object has been destroyed`, as Electron's does. The record is kept
    /// until the window's `'closed'` listeners have all run.
    pub(crate) fn page(&self, window: WindowId) -> Option<RendererHandle> {
        self.hub.contents_of(window)?;
        Some(self.windows.live(window)?.page.clone())
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
    runtime.define_builtin_lazy("electron", renderer_module);
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

/// The main process's `require('electron')`.
fn main_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let exports = native::object(context);
    let window = browser_window::make(context)?;
    exports.set(js_string!("BrowserWindow"), window, false, context)?;
    exports.set(js_string!("app"), app::make(context)?, false, context)?;
    ipc_main::add_exports(context, &exports)?;
    Ok(exports.into())
}

/// A page's `require('electron')`.
fn renderer_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let exports = native::object(context);
    let ipc_renderer = ipc_renderer::ipc_renderer(context);
    exports.set(js_string!("ipcRenderer"), ipc_renderer, false, context)?;
    Ok(exports.into())
}
