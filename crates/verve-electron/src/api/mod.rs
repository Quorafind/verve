//! `require('electron')`, as each of the app's processes has it: the browser process's API in the
//! main isolate, the page's in a renderer's. Every class and function is a native Boa function,
//! made the first time a script requires the module.

pub(crate) mod browser_window;
pub mod define;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native;

use crate::hub::{Hub, RendererHandle};
use crate::ids::WindowId;

/// What the browser process's API keeps between calls: the [`Hub`] every method goes through, and
/// for each window it made, the object standing for it and the page it shows.
pub(crate) struct MainState {
    /// The router every method of the API sends through.
    pub(crate) hub: Arc<Hub>,
    windows: RefCell<BTreeMap<WindowId, (JsObject, RendererHandle)>>,
}

impl MainState {
    fn new(hub: Arc<Hub>) -> Rc<Self> {
        Rc::new(Self {
            hub,
            windows: RefCell::new(BTreeMap::new()),
        })
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
    let state = MainState::new(hub);
    runtime.context().insert_data(Rc::clone(&state));
    runtime.define_builtin_lazy("electron", main_module);
    state
}

/// Gives a page's runtime its `require('electron')`. `ipcRenderer` and `contextBridge` are
/// P12/P21's: a P02 page has no API of its own.
pub(crate) fn install_page(runtime: &mut verve::Runtime) {
    runtime.define_builtin_lazy("electron", renderer_module);
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
    Ok(exports.into())
}

/// A page's `require('electron')`.
fn renderer_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    Ok(native::object(context).into())
}
