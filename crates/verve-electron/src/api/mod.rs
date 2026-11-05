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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native;

use crate::hub::{Hub, RendererHandle};
use crate::ids::{ContentsId, WindowId};
use crate::msg::WebPreferences;

/// What the browser process's API keeps between calls: the [`Hub`] every method goes through, and
/// for each window it made, the object standing for it and the page it shows.
pub(crate) struct MainState {
    /// The router every method of the API sends through.
    pub(crate) hub: Arc<Hub>,
    /// The app object's state: its name, paths, switches and where it is in ending.
    pub(crate) app: app::AppState,
    windows: RefCell<BTreeMap<WindowId, (JsObject, RendererHandle)>>,
}

impl MainState {
    fn new(hub: Arc<Hub>, app: app::AppState) -> Rc<Self> {
        Rc::new(Self {
            hub,
            app,
            windows: RefCell::new(BTreeMap::new()),
        })
    }

    /// Emits `'close'` on `window`'s object: answers whether a listener prevented the default,
    /// which keeps the window open.
    pub(crate) fn emit_close(&self, context: &mut Context, window: WindowId) -> bool {
        let Some(object) = self.object(window) else {
            return false;
        };
        define::emit_cancelable(context, &object, "close", &[]).unwrap_or_else(|error| {
            verve::report(context, &error);
            false
        })
    }

    /// A window is gone: the Hub forgets it, its object hears `'closed'` — after the native window
    /// is destroyed, as in Electron — and is let go. The app is then told whether it was the last.
    pub(crate) fn window_closed(&self, context: &mut Context, window: WindowId) {
        let contents = self.hub.contents_of(window);
        self.hub.remove_window(window);
        if let Some(object) = self.object(window)
            && let Err(error) = define::emit(context, &object, "closed", &[])
        {
            verve::report(context, &error);
        }
        if let Some(contents) = contents {
            contents_ipc::forget(context, contents);
        }
        self.forget(window);
    }

    /// Keeps `object` as the one standing for `window`, and `page` as the handle on what it shows.
    /// The object is held until [`MainState::forget`], as Electron keeps a window until it is
    /// closed: a script that lets go of its last reference still gets `'closed'` on it, and the
    /// same object back from `getAllWindows` and `fromId`.
    pub(crate) fn keep(&self, window: WindowId, object: &JsObject, page: RendererHandle) {
        self.windows
            .borrow_mut()
            .insert(window, (object.clone(), page));
    }

    /// The object standing for `window`, until it is forgotten.
    pub(crate) fn object(&self, window: WindowId) -> Option<JsObject> {
        Some(self.windows.borrow().get(&window)?.0.clone())
    }

    /// The page `window` shows, while the Hub still has the window: once it is gone, a method of
    /// it throws `Object has been destroyed`, as Electron's does. The Hub's registry is what
    /// says so, not this one — a window is gone from the moment it closed, its `'closed'`
    /// listeners included, and this map only lets go of the object after they have all run.
    pub(crate) fn page(&self, window: WindowId) -> Option<RendererHandle> {
        self.hub.contents_of(window)?;
        Some(self.windows.borrow().get(&window)?.1.clone())
    }

    /// Lets go of what was kept for `window`.
    pub(crate) fn forget(&self, window: WindowId) {
        self.windows.borrow_mut().remove(&window);
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
    contents: ContentsId,
    prefs: &WebPreferences,
) -> JsResult<()> {
    ipc_renderer::install(runtime.context(), hub, contents)?;
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
