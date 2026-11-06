//! `BrowserWindow`, Electron 39's: a window with a page, its options, its methods, its events and
//! the statics that find one.
//!
//! The window itself lives in the shell, on the OS thread; this object is the handle the main
//! process's script holds, and every method of it is a message or a read of the cached
//! [state](crate::window_state). What it accepts is Electron's `BrowserWindowConstructorOptions`
//! ([`window_options`], [`web_preferences`]). Like Electron's, the class is native — calling it
//! without `new` throws — and `class extends BrowserWindow` works.
//!
//! The members are in tables: [`window_methods`] and [`window_geometry`] for the prototype,
//! [`window_events`] and [`window_lifecycle`] for what the window says and how it ends.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::events::emitter_init;
use verve::native::{self, ClassSpec, NewOnly, arg};

use super::contents_ipc::{self, WEB_CONTENTS};
use super::define::{self, Args, Fields, Kind};
use super::page_state::PageState;
use super::windows::{Closing, WindowRecord};
use super::{
    app, navigation, shared, web_contents, web_preferences, window_methods, window_options,
};
use crate::hub::RendererHandle;
use crate::ids::WindowId;
use crate::msg::{ToShell, WindowOp};
use crate::window_state::WindowState;

/// The kind of object a `BrowserWindow` is, in [`define`]'s table.
pub(crate) const WINDOW: Kind = Kind("BrowserWindow");

/// The window a `BrowserWindow` object stands for, if `value` is one.
pub(crate) fn window_id_of(value: &JsValue, context: &mut Context) -> Option<WindowId> {
    define::id_of(value, WINDOW, context)
}

/// Makes `BrowserWindow`, extending `EventEmitter`, and answers the constructor.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let emitter = define::emitter(context)?;
    let (window, prototype) = native::class(
        context,
        ClassSpec {
            name: "BrowserWindow",
            length: 0,
            init,
            parent: Some(&emitter),
            methods: &[
                ("loadURL", 1, load_url),
                ("loadFile", 1, load_file),
                ("reload", 0, reload),
                ("capturePage", 0, |this, _, cx| {
                    page_of(this, cx)?;
                    define::rejected(cx, "capturePage")
                }),
            ],
            accessors: &[("id", id, None), ("webContents", web_contents, None)],
            statics: &[
                ("getAllWindows", 0, all_windows),
                ("getFocusedWindow", 0, focused_window),
                ("fromId", 1, from_id),
                ("fromWebContents", 1, from_web_contents),
                ("fromBrowserView", 1, |_, _, _| Ok(JsValue::null())),
            ],
            new_only: Some(NewOnly::Native),
        },
    )?;
    window_methods::install(context, &prototype)?;
    *shared(context).window_class.borrow_mut() = Some(window.clone());
    Ok(window)
}

/// `new BrowserWindow(options)`: a window id and a page of its own, and the shell asked for the
/// window. Options that are not an object are no options, as in Electron.
fn init(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    let given = args.first().and_then(JsValue::as_object);
    let fields = Fields::new(given.as_ref());
    let state = shared(context);
    let mut read = window_options::read(&fields, &state.app.name(), context)?;
    let preferences = fields.object("webPreferences", context)?;
    let forced = state.app.sandbox_forced();
    let prefs = web_preferences::read(&Fields::new(preferences.as_ref()), forced, context)?;
    // A parent that is destroyed is no parent: `setParentWindow` throws for it, the constructor
    // leaves it out.
    let parent = match &read.parent {
        Some(parent) => window_id_of(&parent.clone().into(), context),
        None => None,
    };
    let parent = parent.filter(|parent| state.windows.live(*parent).is_some());
    read.options.parent = parent;
    read.options.modal &= parent.is_some();
    let hub = Arc::clone(&state.hub);
    let (window, contents) = hub.new_window_ids();
    hub.add_window(window, contents);
    let accessible_title = prefs.accessible_title.clone();
    let page = hub.create_renderer(contents, prefs);
    let record = record_for(window, this, page, &read, parent, &hub.primary_display());
    *record.accessible_title.borrow_mut() = accessible_title;
    state.windows.insert(record);
    define::bind(this, WINDOW, window, context);
    let options = read.options.clone();
    hub.to_shell(ToShell::CreateWindow {
        window,
        contents,
        options,
    });
    if read.opacity != 1. {
        let op = WindowOp::SetOpacity(read.opacity);
        hub.to_shell(ToShell::Window { window, op });
    }
    if let Some(record) = state.windows.get(window) {
        state.follow_modal(&record);
    }
    // The page's `webContents` is heard of first, then the window that shows it.
    let page = web_contents(&this.clone().into(), &[], context)?;
    app::emit(context, "web-contents-created", &[page])?;
    app::emit(context, "browser-window-created", &[this.clone().into()])?;
    Ok(())
}

/// The record of a window just made: the state its options give.
fn record_for(
    window: WindowId,
    object: &JsObject,
    page: RendererHandle,
    read: &window_options::Read,
    parent: Option<WindowId>,
    display: &crate::msg::Display,
) -> WindowRecord {
    let mut state = WindowState::new(&read.options, display);
    for (flag, on) in &read.flags {
        state.set_flag(*flag, *on);
    }
    state.opacity = read.opacity;
    if let Some([r, g, b, _]) = read.options.background_color {
        state.background = [r, g, b];
    }
    WindowRecord {
        id: window,
        object: object.clone(),
        page,
        state: RefCell::new(state),
        paint_when_hidden: read.paint_when_initially_hidden,
        modal: read.options.modal,
        parent: Cell::new(parent),
        dead: Cell::new(false),
        closing: Cell::new(Closing::Idle),
        asked_at: Cell::new(None),
        contents_destroyed: Cell::new(false),
        ready: Cell::default(),
        page_state: PageState::default(),
        accessible_title: RefCell::default(),
    }
}

/// `loadURL(url[, options])`: the promise settles when the page has loaded or failed.
fn load_url(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = window_methods::record_of(this, context)?;
    web_contents::load_url(&record, args, context)
}

/// `loadFile(path[, { query, search, hash }])`: `loadURL` of the file's `file:` address.
fn load_file(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = window_methods::record_of(this, context)?;
    web_contents::load_file(&record, args, context)
}

/// `reload()`: the page's address again.
fn reload(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = window_methods::record_of(this, context)?;
    navigation::reload(context, &record)?;
    Ok(JsValue::undefined())
}

/// `id`, which a window keeps answering after it is closed.
fn id(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    match define::id_of(this, WINDOW, context) {
        Some(window) => Ok(JsValue::new(window)),
        None => Err(define::destroyed()),
    }
}

fn web_contents(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (_, page) = page_of(this, context)?;
    Ok(contents_ipc::contents_object(context, page.contents)?.into())
}

/// The window `this` stands for and the page it shows: `Illegal invocation` for an instance of
/// another class, `Object has been destroyed` once its window is gone.
fn page_of(this: &JsValue, context: &mut Context) -> JsResult<(WindowId, RendererHandle)> {
    let state = shared(context);
    define::live(this, WINDOW, context, |window| state.page(window))
}

/// `BrowserWindow.getAllWindows()`: the windows a script can use, the newest first.
fn all_windows(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let windows = shared(context).windows.newest_first();
    let objects = windows.iter().map(|w| w.object.clone().into()).collect();
    Ok(native::array(context, objects))
}

/// `BrowserWindow.getFocusedWindow()`: the window that has the focus, or `null`.
fn focused_window(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let windows = shared(context).windows.newest_first();
    let focused = windows.into_iter().find(|w| w.state.borrow().focused);
    Ok(focused.map_or(JsValue::null(), |w| w.object.clone().into()))
}

/// `BrowserWindow.fromId(id)`: the window with that id, or `null`.
fn from_id(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = Args::new(args).f64(0, "id", context)?;
    let found = shared(context).windows.live(id as WindowId);
    Ok(found.map_or(JsValue::null(), |w| w.object.clone().into()))
}

/// `BrowserWindow.fromWebContents(contents)`: the window that shows it; `Object has been
/// destroyed` for one whose window is gone.
fn from_web_contents(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(contents) = define::id_of(&arg(args, 0), WEB_CONTENTS, context) else {
        return Ok(JsValue::null());
    };
    let state = shared(context);
    let window = state
        .hub
        .window_of(contents)
        .ok_or_else(define::destroyed)?;
    Ok(state
        .windows
        .live(window)
        .map_or(JsValue::null(), |w| w.object.clone().into()))
}
