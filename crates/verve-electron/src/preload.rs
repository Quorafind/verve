//! A page's world setup and its preload script: Electron 39's `webPreferences` rules for what a
//! page and its preload see.
//!
//! The page's scripts run in the runtime's first world. With `contextIsolation` (the default) the
//! preload runs in a world of its own over the same document, so that the page cannot reach what
//! it sets up; without, in the page's. A `sandbox`ed preload (the default) has a `require` that
//! answers `electron`, `events`, `timers` and `url` only and a trimmed `process`; an unsandboxed
//! one has Node's. A page has Node's globals only with `nodeIntegration` and no isolation.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, Source, js_string};
use verve::native::{self, arg};
use verve::vfs::Overlay;
use verve::{MAIN_WORLD, Runtime, WorldId, current_world};
use verve_dom::Document;

use crate::api::ipc_renderer::Outbox;
use crate::api::{context_bridge, ipc_renderer, web_frame, web_utils};
use crate::msg::{FromRenderer, WebPreferences};

/// The number Electron gives the world a preload runs in.
pub(crate) const PRELOAD_WORLD: i64 = 999;

/// Node's globals, which a page without `nodeIntegration` does not have.
const NODE_GLOBALS: [&str; 5] = [
    "process",
    "Buffer",
    "global",
    "setImmediate",
    "clearImmediate",
];

/// The modules a sandboxed preload may `require` besides `electron`.
const SANDBOX_MODULES: [&str; 6] = [
    "events",
    "node:events",
    "timers",
    "node:timers",
    "url",
    "node:url",
];

/// What a page was made as, and the worlds it has.
pub(crate) struct Setup {
    pub(crate) prefs: WebPreferences,
    /// The world the preload runs in: the page's own without isolation.
    preload_world: Cell<WorldId>,
    /// The worlds `exposeInIsolatedWorld` and `webFrame` were asked for, by Electron's number.
    numbered: RefCell<HashMap<i64, WorldId>>,
    /// `require('electron')` in each world.
    exports: RefCell<HashMap<WorldId, JsObject>>,
    /// The page's Node globals, kept for a sandboxed preload that runs in the page's world.
    node_globals: RefCell<Vec<(&'static str, JsValue)>>,
    /// The zoom factor `webFrame` set; Electron's is 1.
    pub(crate) zoom: Cell<f64>,
    /// How many stylesheets `webFrame.insertCSS` has added: the number its next key has.
    pub(crate) sheets: Cell<u32>,
    /// Where the preload is read from when the app is in an archive, and where its failure goes.
    overlay: Option<Arc<dyn Overlay>>,
    outbox: Outbox,
    /// Whether the preload has run: it runs once, as the document starts to load.
    ran: Cell<bool>,
}

/// The page's setup.
///
/// # Panics
///
/// The page has not been given one by [`prepare`].
pub(crate) fn setup(context: &Context) -> Rc<Setup> {
    context
        .get_data::<Rc<Setup>>()
        .cloned()
        .expect("a page is set up before its scripts run")
}

/// Whether the page's scripts see Node: `nodeIntegration`, and no isolation to keep it in the
/// preload's world.
pub(crate) fn page_has_node(prefs: &WebPreferences) -> bool {
    prefs.node_integration && !prefs.context_isolation
}

/// What a page's realm may do beyond the page itself: every network, as Chromium's renderer
/// reaches any, and more only where Node runs in it: with `nodeIntegration` or a preload, and no
/// sandbox (which `page_has_node` does not look at: it only decides what the page's own scripts
/// see). A page without Node gets no listening sockets and no raw DNS.
pub(crate) fn permissions_for(prefs: &WebPreferences) -> verve::Permissions {
    if !prefs.sandbox && (prefs.node_integration || prefs.preload.is_some()) {
        return verve::Permissions::all();
    }
    verve::Permissions {
        outbound: vec![verve::NetworkRule {
            host: None,
            port: None,
        }],
        ..verve::Permissions::default()
    }
}

/// The world Electron's `number` stands for, made when first asked for.
///
/// # Errors
///
/// The world could not be made.
pub(crate) fn world_numbered(number: i64, context: &mut Context) -> JsResult<WorldId> {
    let setup = setup(context);
    if number == 0 {
        return Ok(MAIN_WORLD);
    }
    if number == PRELOAD_WORLD {
        return Ok(setup.preload_world.get());
    }
    if let Some(world) = setup.numbered.borrow().get(&number) {
        return Ok(*world);
    }
    let world = verve_dom::create_world(context).map_err(|error| {
        native::type_error(format!("world {number} could not be made: {error}"))
    })?;
    setup.numbered.borrow_mut().insert(number, world);
    Ok(world)
}

/// `require('electron')` for the world that is running: made once for each.
///
/// # Errors
///
/// A member that could not be made.
pub(crate) fn electron_exports(context: &mut Context) -> JsResult<JsObject> {
    let world = current_world(context);
    let setup = setup(context);
    if let Some(exports) = setup.exports.borrow().get(&world) {
        return Ok(exports.clone());
    }
    let exports = native::object(context);
    exports.set(
        js_string!("ipcRenderer"),
        ipc_renderer::ipc_renderer(context)?,
        false,
        context,
    )?;
    exports.set(
        js_string!("contextBridge"),
        context_bridge::make(context)?,
        false,
        context,
    )?;
    exports.set(
        js_string!("webFrame"),
        web_frame::make(context)?,
        false,
        context,
    )?;
    exports.set(
        js_string!("crashReporter"),
        crash_reporter(context)?,
        false,
        context,
    )?;
    exports.set(
        js_string!("webUtils"),
        web_utils::make(context)?,
        false,
        context,
    )?;
    exports.set(
        js_string!("nativeImage"),
        crate::api::native_image::make(context)?,
        false,
        context,
    )?;
    setup.exports.borrow_mut().insert(world, exports.clone());
    Ok(exports)
}

/// `crashReporter` as a renderer has it: the parameters it adds to a report, which this runtime
/// keeps nowhere, since it makes none.
fn crash_reporter(context: &mut Context) -> JsResult<JsObject> {
    let reporter = native::object(context);
    native::exports(
        context,
        &reporter,
        &[
            ("addExtraParameter", 2, |_, _, _| Ok(JsValue::undefined())),
            (
                "removeExtraParameter",
                1,
                |_, _, _| Ok(JsValue::undefined()),
            ),
            ("getParameters", 0, |_, _, cx| Ok(native::object(cx).into())),
        ],
    )?;
    Ok(reporter)
}

/// What `process` says of a renderer.
pub(crate) fn renderer_info() -> verve::ProcessInfo {
    verve::ProcessInfo {
        title: "electron".into(),
        process_type: Some("renderer".into()),
        ..verve::ProcessInfo::default()
    }
}

/// Gives the page its setup, takes Node's globals off the page's world when it has no Node, and
/// makes the world its preload will run in. Called once, with the page's document just made;
/// [`run`] then runs the preload when the document starts to load.
pub(crate) fn prepare(
    runtime: &mut Runtime,
    document: &Document,
    prefs: &WebPreferences,
    outbox: &Outbox,
    overlay: Option<Arc<dyn Overlay>>,
) {
    let setup = Rc::new(Setup {
        prefs: prefs.clone(),
        preload_world: Cell::new(MAIN_WORLD),
        numbered: RefCell::new(HashMap::new()),
        exports: RefCell::new(HashMap::new()),
        node_globals: RefCell::new(Vec::new()),
        zoom: Cell::new(1.),
        sheets: Cell::new(0),
        overlay,
        outbox: Rc::clone(outbox),
        ran: Cell::new(false),
    });
    runtime.context().insert_data(Rc::clone(&setup));
    let described = describe_process(runtime.context(), prefs);
    crate::api::define::report(runtime.context(), described);
    if !page_has_node(prefs) {
        let kept = take_node_globals(runtime.context());
        *setup.node_globals.borrow_mut() = kept;
    }
    if prefs
        .preload
        .as_ref()
        .is_some_and(|path| path.is_absolute())
        && prefs.context_isolation
    {
        match document.create_world(runtime) {
            Ok(world) => setup.preload_world.set(world),
            Err(error) => eprintln!("electron: the preload's world could not be made: {error}"),
        }
    }
}

/// Runs the page's preload in its world, once; a failure goes to the main process as
/// `'preload-error'`. The page's scripts come after.
pub(crate) fn run(runtime: &mut Runtime) {
    let setup = setup(runtime.context());
    let Some(path) = setup
        .prefs
        .preload
        .as_ref()
        .filter(|path| path.is_absolute())
    else {
        return;
    };
    if setup.ran.replace(true) {
        return;
    }
    let previous = runtime.enter_world(setup.preload_world.get());
    let failure = run_preload(runtime, &setup, path, setup.overlay.as_deref());
    runtime.enter_world(previous);
    runtime.run_microtasks();
    if let Err((name, message)) = failure {
        setup.outbox.borrow_mut().push(FromRenderer::PreloadError {
            path: path.to_string_lossy().into_owned(),
            name,
            message,
        });
    }
}

/// Says in the running world's `process` what `webPreferences` made of it: a renderer's, with
/// the arguments the page was given and whether its scripts are isolated.
fn describe_process(context: &mut Context, prefs: &WebPreferences) -> JsResult<()> {
    let process = native::global(context, "process")?;
    let Some(process) = process.as_object() else {
        return Ok(());
    };
    add_renderer_fields(context, &process, prefs.context_isolation)?;
    set_arguments(context, &process, &prefs.additional_arguments)
}

/// `contextIsolated`, `isMainFrame` and `contextId`: what a renderer's `process` adds.
fn add_renderer_fields(context: &mut Context, process: &JsObject, isolated: bool) -> JsResult<()> {
    process.set(js_string!("contextIsolated"), isolated, false, context)?;
    process.set(js_string!("isMainFrame"), true, false, context)?;
    let id = format!("{:x}", std::process::id());
    process.set(js_string!("contextId"), JsString::from(id), false, context)?;
    Ok(())
}

/// The arguments the page was given with `additionalArguments`, after the program's name.
fn set_arguments(context: &mut Context, process: &JsObject, extra: &[String]) -> JsResult<()> {
    if extra.is_empty() {
        return Ok(());
    }
    let argv = process.get(js_string!("argv"), context)?;
    let mut all = native::list_of(&argv, context).unwrap_or_default();
    all.extend(
        extra
            .iter()
            .map(|a| JsValue::from(JsString::from(a.as_str()))),
    );
    let list = native::array(context, all);
    process.set(js_string!("argv"), list, false, context)?;
    Ok(())
}

/// Takes Node's globals off the running world's global object, answering what they were.
fn take_node_globals(context: &mut Context) -> Vec<(&'static str, JsValue)> {
    let global = context.global_object();
    let mut kept = Vec::new();
    for name in NODE_GLOBALS {
        let key = JsString::from(name);
        if let Ok(value) = global.get(key.clone(), context) {
            kept.push((name, value));
        }
        let deleted = global.delete_property_or_throw(key, context);
        crate::api::define::report(context, deleted);
    }
    kept
}

/// Reads and runs the preload `path` in the world that is running. A preload that cannot be
/// read or throws fails with the error's name and message; the console says so first, as
/// Electron's does.
fn run_preload(
    runtime: &mut Runtime,
    setup: &Setup,
    path: &Path,
    overlay: Option<&dyn Overlay>,
) -> Result<(), (String, String)> {
    let outcome = read_source(path, overlay)
        .map_err(|error| ("Error".to_owned(), error))
        .and_then(|source| execute(runtime, setup, path, &source));
    if let Err((name, message)) = &outcome {
        let context = runtime.context();
        let shown = format!("Unable to load preload script: {}", path.display());
        for line in [shown, format!("{name}: {message}")] {
            let text = JsValue::from(JsString::from(line));
            let console = native::global(context, "console").unwrap_or_default();
            let _ = native::invoke(&console, "error", &[text], context);
        }
    }
    outcome
}

/// The text of the preload, from the app's archive when it is in one.
fn read_source(path: &Path, overlay: Option<&dyn Overlay>) -> Result<String, String> {
    let bytes = match overlay.and_then(|overlay| overlay.read(path)) {
        Some(read) => read.map(|bytes| bytes.into_owned()),
        None => std::fs::read(path),
    };
    let bytes = bytes.map_err(|error| {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => "ENOENT: no such file or directory",
            _ => "EACCES: permission denied",
        };
        format!("{code}, open '{}'", path.display())
    })?;
    String::from_utf8(bytes).map_err(|_| "the preload is not UTF-8 text".to_owned())
}

fn execute(
    runtime: &mut Runtime,
    setup: &Setup,
    path: &Path,
    source: &str,
) -> Result<(), (String, String)> {
    let sandboxed = setup.prefs.sandbox;
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    // The page's own `process` was described with the page; an isolated world's is new.
    if setup.preload_world.get() != MAIN_WORLD {
        runtime
            .set_process_info(renderer_info())
            .map_err(|e| error_parts(&e, runtime.context()))?;
        let context = runtime.context();
        describe_process(context, &setup.prefs).map_err(|e| error_parts(&e, context))?;
    }
    let context = runtime.context();
    let fail = |error: boa_engine::JsError, cx: &mut Context| error_parts(&error, cx);
    let exports = native::object(context);
    let module = native::object(context);
    module
        .set(js_string!("exports"), exports.clone(), false, context)
        .map_err(|e| fail(e, context))?;
    let require = if sandboxed {
        sandbox_require(runtime)
    } else {
        full_require(runtime, &dir)
    }
    .map_err(|e| fail(e, runtime.context()))?;
    let context = runtime.context();
    let name = JsString::from(path.to_string_lossy().as_ref());
    let (wrapper, args, this): (&str, Vec<JsValue>, JsValue) = if sandboxed {
        let globals = world_globals(setup, context).map_err(|e| fail(e, context))?;
        let args = [exports.clone().into(), require, module.into()]
            .into_iter()
            .chain(globals)
            .collect();
        (
            "(function (exports, require, module, process, Buffer, global, setImmediate, clearImmediate) {",
            args,
            context.global_object().into(),
        )
    } else {
        let _ = module.set(js_string!("id"), name.clone(), false, context);
        let _ = module.set(js_string!("filename"), name.clone(), false, context);
        (
            "(function (exports, require, module, __filename, __dirname) {",
            vec![
                exports.clone().into(),
                require,
                module.into(),
                name.clone().into(),
                JsString::from(dir.to_string_lossy().as_ref()).into(),
            ],
            exports.clone().into(),
        )
    };
    let text = format!("{wrapper}{source}\n}})");
    let function = context
        .eval(Source::from_reader(text.as_bytes(), Some(path)))
        .map_err(|e| fail(e, context))?;
    verve::call(context, &function, &this, &args).map_err(|e| fail(e, context))?;
    Ok(())
}

/// The `process`, `Buffer`, `global`, `setImmediate` and `clearImmediate` a sandboxed preload is
/// given: the world's own, with a `process` trimmed to what a sandbox has.
fn world_globals(setup: &Setup, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let global = context.global_object();
    let own = if setup.preload_world.get() == MAIN_WORLD {
        setup.node_globals.borrow().clone()
    } else {
        take_node_globals(context)
    };
    let find = |name: &str| {
        own.iter()
            .find(|(n, _)| *n == name)
            .map_or_else(JsValue::undefined, |(_, v)| v.clone())
    };
    let full = find("process");
    let process = match full.as_object() {
        Some(full) => sandbox_process(context, &full)?.into(),
        None => JsValue::undefined(),
    };
    Ok(vec![
        process,
        find("Buffer"),
        global.into(),
        find("setImmediate"),
        find("clearImmediate"),
    ])
}

/// `process` as a sandboxed preload has it: what the platform and the page are, and a few
/// functions about the process.
fn sandbox_process(context: &mut Context, full: &JsObject) -> JsResult<JsObject> {
    let limited = JsObject::with_object_proto(context.intrinsics());
    // The running world's own `EventEmitter` is what it extends: the page's, if `full` were asked.
    let emitter = verve::events::emitter_class(context)?;
    limited.set_prototype(emitter.get(js_string!("prototype"), context)?.as_object());
    for name in [
        "type",
        "platform",
        "arch",
        "version",
        "versions",
        "env",
        "argv",
        "execPath",
        "pid",
        "contextId",
        "contextIsolated",
        "isMainFrame",
    ] {
        let value = full.get(JsString::from(name), context)?;
        limited.set(JsString::from(name), value, false, context)?;
    }
    limited.set(js_string!("sandboxed"), true, false, context)?;
    let none = |_: &JsValue, _: &[JsValue], _: &mut Context| Ok(JsValue::undefined());
    let numbers = |_: &JsValue, _: &[JsValue], cx: &mut Context| {
        let record = native::object(cx);
        for key in [
            "total",
            "free",
            "private",
            "shared",
            "residentSet",
            "percentCPUUsage",
        ] {
            record.set(JsString::from(key), 0, false, cx)?;
        }
        Ok(record.into())
    };
    // Own and listed, as a sandboxed `process` has them: an emitter's state, and the methods.
    limited.set(
        js_string!("_events"),
        native::object(context),
        false,
        context,
    )?;
    limited.set(js_string!("_eventsCount"), 0, false, context)?;
    limited.set(
        js_string!("_maxListeners"),
        JsValue::undefined(),
        false,
        context,
    )?;
    native::exports(
        context,
        &limited,
        &[
            ("crash", 0, none),
            ("hang", 0, none),
            ("getHeapStatistics", 0, numbers),
            ("getCPUUsage", 0, numbers),
            ("getBlinkMemoryInfo", 0, numbers),
            ("getSystemMemoryInfo", 0, numbers),
            ("getProcessMemoryInfo", 0, |_, _, cx| {
                let record = native::object(cx);
                for key in ["private", "shared", "residentSet"] {
                    record.set(JsString::from(key), 0, false, cx)?;
                }
                native::settled(Ok(record.into()), cx)
            }),
            ("getCreationTime", 0, |_, _, _| Ok(JsValue::null())),
            ("getSystemVersion", 0, |_, _, _| {
                Ok(JsString::from("10.0.0").into())
            }),
            ("uptime", 0, |_, _, _| Ok(JsValue::from(0))),
        ],
    )?;
    Ok(limited)
}

/// The sandboxed preload's `require`: `electron` and a few Node modules, and an error for the rest.
fn sandbox_require(runtime: &mut Runtime) -> JsResult<JsValue> {
    let node = runtime.global_require();
    require_through(runtime.context(), node, true)
}

/// The unsandboxed preload's `require`: Node's, resolving against the preload's folder.
fn full_require(runtime: &mut Runtime, dir: &Path) -> JsResult<JsValue> {
    let node = runtime.global_require_in(dir);
    require_through(runtime.context(), node, false)
}

/// A `require` that answers `electron` with the running world's own, and the rest through `node`
/// (all of them, or only the sandbox's).
fn require_through(context: &mut Context, node: JsValue, sandboxed: bool) -> JsResult<JsValue> {
    let require = native::closure(
        context,
        "require",
        1,
        |_, args, (node, sandboxed): &(JsValue, bool), cx| {
            let name = native::text_of(&arg(args, 0), cx)?;
            if matches!(
                name.as_str(),
                "electron" | "electron/renderer" | "electron/common"
            ) {
                return Ok(electron_exports(cx)?.into());
            }
            // The world's own `EventEmitter`, not the page's that the runtime's `require` has.
            if matches!(name.as_str(), "events" | "node:events") {
                return Ok(verve::events::emitter_class(cx)?.into());
            }
            if *sandboxed && !SANDBOX_MODULES.contains(&name.as_str()) {
                return Err(native::error(format!("module not found: {name}")));
            }
            verve::call(cx, node, &JsValue::undefined(), args)
        },
        (node.clone(), sandboxed),
    );
    if let (Some(inner), false) = (node.as_object(), sandboxed) {
        for key in ["resolve", "cache"] {
            let value = inner.get(JsString::from(key), context)?;
            require.set(JsString::from(key), value, false, context)?;
        }
    }
    Ok(require.into())
}

/// The name and message of a thrown value, as a preload failure carries them.
pub(crate) fn error_parts(error: &boa_engine::JsError, context: &mut Context) -> (String, String) {
    let thrown = error.clone().into_opaque(context).unwrap_or_default();
    let read = |object: &JsObject, name: &str, cx: &mut Context| {
        object
            .get(JsString::from(name), cx)
            .and_then(|v| v.to_string(cx))
            .map(|s| s.to_std_string_escaped())
    };
    match thrown.as_object() {
        Some(object) => (
            read(&object, "name", context).unwrap_or_else(|_| "Error".to_owned()),
            read(&object, "message", context).unwrap_or_default(),
        ),
        None => ("Error".to_owned(), verve::print(&thrown)),
    }
}

/// The folder a `file:` page is in, for the `require` of a page with Node.
pub(crate) fn page_folder(url: &str) -> Option<PathBuf> {
    url::Url::parse(url)
        .ok()
        .filter(|url| url.scheme() == "file")
        .and_then(|url| url.to_file_path().ok())
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

/// The globals a page with Node has besides `process`: `require` for the page's folder,
/// `module`, `__filename` and `__dirname`.
///
/// # Errors
///
/// A global that could not be defined.
pub(crate) fn define_page_node(
    context: &mut Context,
    require: JsValue,
    url: &str,
    folder: &Path,
) -> JsResult<()> {
    let module = native::object(context);
    let exports = native::object(context);
    module.set(js_string!("exports"), exports, false, context)?;
    module.set(
        js_string!("id"),
        JsString::from("electron/js2c/renderer_init"),
        false,
        context,
    )?;
    let file = url::Url::parse(url)
        .ok()
        .and_then(|url| url.to_file_path().ok())
        .unwrap_or_default();
    let text = |path: &Path| JsString::from(path.to_string_lossy().as_ref());
    native::define_global(context, "require", require)?;
    native::define_global(context, "module", module)?;
    native::define_global(context, "__filename", text(&file))?;
    native::define_global(context, "__dirname", text(folder))
}
