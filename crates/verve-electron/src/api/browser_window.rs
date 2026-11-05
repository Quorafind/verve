//! `BrowserWindow`, as far as P02 needs it: a window with a page, `loadURL`, `webContents.id`
//! and `'closed'`. P13 makes it the whole of Electron's.
//!
//! The window itself lives in the shell, on the OS thread; this object is the handle the main
//! process's script holds, and every method of it is a message. What it accepts is Electron's
//! `BrowserWindowConstructorOptions`, read as Electron reads them: leniently, with Electron's
//! defaults. Like Electron's, the class is native — calling it without `new` throws.

use std::path::PathBuf;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::events::emitter_init;
use verve::native::{self, ClassSpec, NewOnly};

use crate::api::define::{self, Args, Fields, Kind};
use crate::api::{app, contents_ipc, shared};
use crate::hub::RendererHandle;
use crate::ids::WindowId;
use crate::msg::{LoadOptions, ToRenderer, ToShell, WebPreferences, WindowOp, WindowOptions};

/// The kind of object a `BrowserWindow` is, in [`define`]'s table.
pub(crate) const WINDOW: Kind = Kind("BrowserWindow");

/// Makes `BrowserWindow`, extending `EventEmitter`, and answers the constructor.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let emitter = define::emitter(context)?;
    let (window, _) = native::class(
        context,
        ClassSpec {
            name: "BrowserWindow",
            length: 0,
            init,
            parent: Some(&emitter),
            methods: &[("loadURL", 1, load_url), ("close", 0, close)],
            accessors: &[("id", id, None), ("webContents", web_contents, None)],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    Ok(window)
}

/// `new BrowserWindow(options)`: a window id and a page of its own, and the shell asked for the
/// window. Options that are not an object are no options, as in Electron.
fn init(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    let given = args.first().and_then(JsValue::as_object);
    let fields = Fields::new(given.as_ref());
    let options = window_options(&fields, context)?;
    let prefs = match fields.object("webPreferences", context)? {
        Some(object) => web_preferences(&Fields::new(Some(&object)), context)?,
        None => WebPreferences::default(),
    };
    let state = shared(context);
    let hub = Arc::clone(&state.hub);
    let (window, contents) = hub.new_window_ids();
    hub.add_window(window, contents);
    let page = hub.create_renderer(contents, prefs);
    define::bind(this, WINDOW, window, context);
    state.keep(window, this, page);
    hub.to_shell(ToShell::CreateWindow {
        window,
        contents,
        options,
    });
    // The page's `webContents` is heard of first, then the window that shows it.
    let page = web_contents(&this.clone().into(), &[], context)?;
    app::emit(context, "web-contents-created", &[page])?;
    app::emit(context, "browser-window-created", &[this.clone().into()])?;
    Ok(())
}

/// The options P02 reads, over Electron's defaults. A size is an integer, as Electron's
/// `Integer`; anything else is left at the default.
fn window_options(fields: &Fields, context: &mut Context) -> JsResult<WindowOptions> {
    let mut options = WindowOptions::default();
    options.width = fields.int("width", options.width, context)?;
    options.height = fields.int("height", options.height, context)?;
    options.show = fields.bool("show", options.show, context)?;
    if let Some(title) = fields.string("title", context)? {
        options.title = title;
    }
    Ok(options)
}

/// The `webPreferences` P02 reads, over Electron's defaults. `sandbox` is on unless
/// `nodeIntegration` is, or the app says otherwise.
fn web_preferences(fields: &Fields, context: &mut Context) -> JsResult<WebPreferences> {
    let mut prefs = WebPreferences::default();
    prefs.dev_tools = fields.bool("devTools", prefs.dev_tools, context)?;
    prefs.node_integration = fields.bool("nodeIntegration", prefs.node_integration, context)?;
    prefs.node_integration_in_worker = fields.bool(
        "nodeIntegrationInWorker",
        prefs.node_integration_in_worker,
        context,
    )?;
    prefs.node_integration_in_sub_frames = fields.bool(
        "nodeIntegrationInSubFrames",
        prefs.node_integration_in_sub_frames,
        context,
    )?;
    prefs.preload = fields.string("preload", context)?.map(PathBuf::from);
    prefs.sandbox = fields.bool("sandbox", !prefs.node_integration, context)?;
    prefs.partition = fields.string("partition", context)?;
    prefs.zoom_factor = fields.f64("zoomFactor", prefs.zoom_factor, context)?;
    prefs.javascript = fields.bool("javascript", prefs.javascript, context)?;
    prefs.web_security = fields.bool("webSecurity", prefs.web_security, context)?;
    prefs.allow_running_insecure_content =
        fields.bool("allowRunningInsecureContent", !prefs.web_security, context)?;
    prefs.images = fields.bool("images", prefs.images, context)?;
    prefs.context_isolation = fields.bool("contextIsolation", prefs.context_isolation, context)?;
    Ok(prefs)
}

/// `loadURL(url[, options])`, answering a promise. Electron's settles when the page has loaded;
/// P02's page is parsed on its own thread and nothing tells the main process, so it settles at
/// once — P13 and P20 make it wait. A `url` that is not a string throws, as Electron's gin
/// conversion does: a method written in JavaScript there, it reads a missing one as `undefined`.
fn load_url(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = Args::new(&[native::arg(args, 0)]).string(0, "url", context)?;
    let (_, page) = page_of(this, context)?;
    page.to_renderer(ToRenderer::Load {
        url,
        options: LoadOptions::default(),
    });
    native::settled(Ok(JsValue::undefined()), context)
}

fn close(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (window, _) = page_of(this, context)?;
    shared(context).hub.to_shell(ToShell::Window {
        window,
        op: WindowOp::Close,
    });
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
    // P13 makes this the `WebContents` class; until then it is the object `contents_ipc` keeps.
    let (_, page) = page_of(this, context)?;
    Ok(contents_ipc::contents_object(context, page.contents)?.into())
}

/// The window `this` stands for and the page it shows: `Illegal invocation` for an instance of
/// another class, `Object has been destroyed` once its window is gone.
fn page_of(this: &JsValue, context: &mut Context) -> JsResult<(WindowId, RendererHandle)> {
    let state = shared(context);
    define::live(this, WINDOW, context, |window| state.page(window))
}
