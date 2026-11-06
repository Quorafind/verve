//! `require('electron').app`, as Electron 39's browser process has it: an `EventEmitter` whose
//! methods are its own properties.
//!
//! The lifecycle — `ready`, `quit`, `exit`, the windows closing — is a state machine in
//! [`quit`]; the places an app has are [`paths`]; what the system answers is [`platform`] and
//! [`system`]. This file is the object itself: the state it keeps, the table of its methods, and
//! the events the main process emits on it.

mod paths;
pub(crate) mod platform;
pub(crate) mod quit;
mod system;
pub(crate) use system::user_agent;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, arg, js_text};

use super::command_line;
use super::define::{self, Args};
use super::shared;
use crate::launcher::{Start, Switch, Target};
use crate::msg::AppEvent;
use crate::single_instance::SingleInstance;
use paths::Paths;
use quit::Ending;

/// What the app object keeps between calls.
pub(crate) struct AppState {
    object: RefCell<Option<JsObject>>,
    ready: Cell<bool>,
    /// The `resolve` of each promise `whenReady` gave out before `ready`.
    waiters: RefCell<Vec<JsFunction>>,
    pub(crate) quit: Cell<Ending>,
    /// The code the app ends with once it has said `ready`: a quit made in the entry.
    pending_exit: Cell<Option<i32>>,
    name: RefCell<String>,
    version: RefCell<String>,
    pub(crate) paths: RefCell<Paths>,
    pub(crate) switches: RefCell<Vec<Switch>>,
    pub(crate) relaunch: RefCell<Option<Relaunch>>,
    single: RefCell<Option<SingleInstance>>,
    pub(crate) settings: RefCell<Settings>,
    packaged: bool,
    app_path: PathBuf,
}

/// What `app.relaunch` was asked for.
pub(crate) struct Relaunch {
    pub(crate) args: Option<Vec<String>>,
    pub(crate) exec_path: Option<PathBuf>,
}

/// What the app set and Electron only remembers: the badge, the recent documents, the About panel
/// and the switches that must be thrown before `ready`.
#[derive(Default)]
pub(crate) struct Settings {
    badge: i32,
    recent: Vec<String>,
    user_agent: Option<String>,
    accessibility: bool,
    hardware_acceleration_off: bool,
    sandbox: bool,
    /// What `setAppUserModelId` set; `electron.app.<name>` until then.
    user_model_id: Option<String>,
}

impl AppState {
    /// The app that `launch` started, named and versioned by its manifest.
    pub(crate) fn new(launch: &Start) -> Self {
        let manifest = &launch.manifest;
        let name = manifest
            .product_name
            .clone()
            .or_else(|| manifest.name.clone())
            .unwrap_or_else(|| "Electron".into());
        let user_data = launch.user_data_dir();
        let paths = Paths::new(
            platform::folders(),
            crate::launcher::exe_path(),
            &name,
            user_data,
        );
        Self {
            object: RefCell::new(None),
            ready: Cell::new(false),
            waiters: RefCell::new(Vec::new()),
            quit: Cell::new(Ending::Running),
            pending_exit: Cell::new(None),
            version: RefCell::new(
                manifest
                    .version
                    .clone()
                    .unwrap_or_else(|| verve::ELECTRON_VERSION.into()),
            ),
            name: RefCell::new(name),
            paths: RefCell::new(paths),
            switches: RefCell::new(launch.switches.clone()),
            relaunch: RefCell::new(None),
            single: RefCell::new(None),
            settings: RefCell::new(Settings::default()),
            packaged: matches!(launch.target, Target::Packaged { .. }),
            app_path: match &launch.target {
                Target::App { app_path, .. } | Target::Packaged { app_path } => app_path.clone(),
                _ => launch.resources_path.join("default_app.asar"),
            },
        }
    }

    /// Whether the app runs from its packaged `app.asar`.
    pub(crate) fn is_packaged(&self) -> bool {
        self.packaged
    }

    /// The app's name, which `userAgentFallback` and `getName` say.
    pub(crate) fn name(&self) -> String {
        self.name.borrow().clone()
    }

    /// Whether `app.enableSandbox()` was called: every window's page is sandboxed then.
    pub(crate) fn sandbox_forced(&self) -> bool {
        self.settings.borrow().sandbox
    }

    /// Whether `ready` has been emitted.
    pub(crate) fn is_ready(&self) -> bool {
        self.ready.get()
    }

    /// The AppUserModelID Windows groups the app's windows under, and names its login item by.
    pub(crate) fn user_model_id(&self) -> String {
        let set = self.settings.borrow().user_model_id.clone();
        set.unwrap_or_else(|| format!("electron.app.{}", self.name()))
    }

    /// The profile directory, which exists once the app is ready.
    pub(crate) fn user_data(&self) -> PathBuf {
        self.paths.borrow().user_data().to_path_buf()
    }
}

/// The app object, made the first time something asks for it.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let state = shared(context);
    if let Some(app) = state.app.object.borrow().clone() {
        return Ok(app);
    }
    let emitter = define::emitter(context)?;
    let app = emitter.construct(&[], None, context)?;
    native::methods(context, &app, METHODS)?;
    native::methods(context, &app, system::METHODS)?;
    native::accessors(context, &app, ACCESSORS)?;
    native::accessors(context, &app, system::ACCESSORS)?;
    native::hidden(
        &app,
        js_string!("commandLine"),
        command_line::make(context)?,
        context,
    )?;
    *state.app.object.borrow_mut() = Some(app.clone());
    Ok(app)
}

/// Emits `name` on the app with an `Event` before `args`, when anything made the app object;
/// answers whether a listener prevented the default.
pub(crate) fn emit(context: &mut Context, name: &str, args: &[JsValue]) -> JsResult<bool> {
    let app = shared(context).app.object.borrow().clone();
    match app {
        Some(app) => define::emit_cancelable(context, &app, name, args),
        None => Ok(false),
    }
}

/// [`emit`] for an event nobody can answer, with a listener's throw reported like any uncaught
/// error of the main process.
pub(crate) fn emit_quietly(context: &mut Context, name: &str, args: &[JsValue]) -> bool {
    let app = shared(context).app.object.borrow().clone();
    app.is_some_and(|app| define::emit_reporting(context, &app, name, args))
}

/// Whether the app has a listener for `name`.
pub(crate) fn is_heard(context: &mut Context, name: &str) -> bool {
    let app = shared(context).app.object.borrow().clone();
    app.is_some_and(|app| {
        native::invoke(&app.into(), "listenerCount", &[js_text(name)], context)
            .ok()
            .and_then(|count| count.as_number())
            .is_some_and(|count| count > 0.)
    })
}

/// What the system says to the app: the Dock icon was clicked, a file or a URL was opened with it.
/// `ThemeChanged` is `nativeTheme`'s (P14).
pub(crate) fn platform_event(context: &mut Context, event: AppEvent) {
    let (name, args) = match event {
        AppEvent::Activate {
            has_visible_windows,
        } => ("activate", vec![has_visible_windows.into()]),
        AppEvent::OpenFile(path) => ("open-file", vec![path_text(&path)]),
        AppEvent::OpenUrl(url) => ("open-url", vec![js_text(&url)]),
        AppEvent::QuitRequested => return quit::quit(context),
        _ => return,
    };
    emit_quietly(context, name, &args);
}

/// A second instance started: `second-instance` with its command line, working directory and
/// `additionalData` (the JSON it crossed in, parsed again).
pub(crate) fn second_instance(context: &mut Context, argv: &[String], cwd: &Path, data: &str) {
    let list = argv.iter().map(|a| js_text(a)).collect();
    let argv = native::array(context, list);
    let parsed = native::global(context, "JSON")
        .and_then(|json| native::invoke(&json, "parse", &[js_text(data)], context))
        .unwrap_or_default();
    let args = [argv, path_text(cwd), parsed];
    emit_quietly(context, "second-instance", &args);
}

/// The entry has run: `will-finish-launching`.
pub(crate) fn will_finish_launching(context: &mut Context) {
    // Electron's own listener, the first there is, sets the default application menu.
    let menu = super::menu::set_default_application_menu(context);
    define::report(context, menu);
    emit_quietly(context, "will-finish-launching", &[]);
}

/// The shell is up: the profile directory is made, `ready` is emitted, and the promises
/// `whenReady` gave out are kept. Once.
pub(crate) fn ready(context: &mut Context) {
    let state = shared(context);
    if state.app.ready.replace(true) {
        return;
    }
    for name in ["userData", "sessionData", "logs", "crashDumps"] {
        if let Some(dir) = state.app.paths.borrow().get(name) {
            platform::make_dir(&dir);
        }
    }
    // Electron gives `launchInfo` on macOS only; elsewhere the listener gets an empty object.
    let launch_info = native::object(context);
    emit_quietly(context, "ready", &[launch_info.into()]);
    let waiters = std::mem::take(&mut *state.app.waiters.borrow_mut());
    for resolve in waiters {
        let settled = resolve.call(&JsValue::undefined(), &[], context);
        define::report(context, settled);
    }
    // A quit in the entry was waiting for this.
    if let Some(code) = state.app.pending_exit.take() {
        let hub = Arc::clone(&state.hub);
        state.hub.to_main(move |_| hub.shutdown(code));
    }
}

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("quit", 0, |_, _, cx| {
        quit::quit(cx);
        Ok(JsValue::undefined())
    }),
    ("exit", 0, exit),
    ("relaunch", 0, relaunch),
    ("isReady", 0, |_, _, cx| {
        Ok(shared(cx).app.is_ready().into())
    }),
    ("whenReady", 0, when_ready),
    ("focus", 0, focus),
    ("getAppPath", 0, |_, _, cx| {
        Ok(path_text(&shared(cx).app.app_path))
    }),
    ("getPath", 1, get_path),
    ("setPath", 2, set_path),
    ("getVersion", 0, |_, _, cx| {
        Ok(js_text(&shared(cx).app.version.borrow()))
    }),
    ("getName", 0, |_, _, cx| Ok(js_text(&shared(cx).app.name()))),
    ("setName", 1, set_name),
    ("requestSingleInstanceLock", 0, request_single_instance_lock),
    ("hasSingleInstanceLock", 0, |_, _, cx| {
        Ok(shared(cx).app.single.borrow().is_some().into())
    }),
    ("releaseSingleInstanceLock", 0, |_, _, cx| {
        shared(cx).app.single.borrow_mut().take();
        Ok(JsValue::undefined())
    }),
];

const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    (
        "isPackaged",
        |_, _, cx| Ok(shared(cx).app.packaged.into()),
        None,
    ),
    (
        "name",
        |_, _, cx| Ok(js_text(&shared(cx).app.name())),
        Some(set_name),
    ),
    (
        "applicationMenu",
        super::menu::application_menu_getter,
        Some(super::menu::application_menu_setter),
    ),
];

fn path_text(path: &std::path::Path) -> JsValue {
    js_text(&path.to_string_lossy())
}

/// `app.exit([code])`: ends the app without asking anything of the windows. As in Electron the
/// events of the end run inside the call — but before `ready` the process just ends, with no
/// event and not another line of the script (observed on 39.8.10).
pub(crate) fn exit(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = Args::new(args).f64_or(0, "exitCode", 0., context)? as i32;
    let state = shared(context);
    if state.app.is_ready() {
        quit::finish(context, code, true);
    } else {
        state.app.quit.set(Ending::Exiting(code));
        state.hub.shutdown(code);
    }
    Ok(JsValue::undefined())
}

/// `app.relaunch([{ args, execPath }])`: remembers to start the app again as it ends.
fn relaunch(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let options = Args::new(args).object_opt(0, "options", context)?;
    let fields = define::Fields::new(options.as_ref());
    let list = match &options {
        Some(options) => options.get(js_string!("args"), context)?,
        None => JsValue::undefined(),
    };
    let args = if list.is_undefined() {
        None
    } else {
        let items = native::list_of(&list, context)?;
        let texts: JsResult<Vec<String>> =
            items.iter().map(|i| native::text_of(i, context)).collect();
        Some(texts?)
    };
    let exec_path = fields.string("execPath", context)?.map(PathBuf::from);
    *shared(context).app.relaunch.borrow_mut() = Some(Relaunch { args, exec_path });
    Ok(JsValue::undefined())
}

/// `app.whenReady()`: a promise for `ready`, kept at once when it has been.
fn when_ready(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state = shared(context);
    if state.app.is_ready() {
        return native::settled(Ok(JsValue::undefined()), context);
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    state.app.waiters.borrow_mut().push(resolvers.resolve);
    Ok(promise.into())
}

/// `app.focus()`: the most recent window takes focus.
fn focus(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let hub = Arc::clone(&shared(context).hub);
    if let Some(window) = hub.windows().last() {
        hub.to_window(*window, crate::msg::WindowOp::Focus);
    }
    Ok(JsValue::undefined())
}

/// `app.getPath(name)`.
fn get_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = Args::new(args).string(0, "name", context)?;
    match shared(context).app.paths.borrow().get(&name) {
        Some(path) => Ok(path_text(&path)),
        None => Err(define::error(&format!("Failed to get '{name}' path"))),
    }
}

/// `app.setPath(name, path)`: a place the system names, moved. The directory is not made (the
/// profile's is, at `ready`), and a relative path is no path.
fn set_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (name, path) = (
        Args::new(args).string(0, "name", context)?,
        Args::new(args).string(1, "path", context)?,
    );
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(define::error("Path must be absolute"));
    }
    let state = shared(context);
    if state.app.paths.borrow().get(&name).is_none() {
        return Err(define::error("Failed to set path"));
    }
    state.app.paths.borrow_mut().set(&name, path);
    Ok(JsValue::undefined())
}

/// `app.setName(name)`, and the `name` property's setter.
fn set_name(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = native::text_of(&arg(args, 0), context)?;
    *shared(context).app.name.borrow_mut() = name;
    Ok(JsValue::undefined())
}

/// `app.requestSingleInstanceLock([additionalData])`: whether this process is the only one for
/// its profile directory; a second one tells the first and gets `false`.
fn request_single_instance_lock(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let state = shared(context);
    if state.app.single.borrow().is_some() {
        return Ok(true.into());
    }
    let data = json_of(&native::arg(args, 0), context)?;
    let argv = crate::single_instance::second_instance_argv(&state.hub.launch());
    let lock = SingleInstance::request(&state.app.user_data(), &state.hub, argv, data);
    let got = lock.is_some();
    *state.app.single.borrow_mut() = lock;
    Ok(got.into())
}

/// `JSON.stringify(value)`, `null` for what has no JSON: how `additionalData` crosses to the
/// first instance.
fn json_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let json = native::global(context, "JSON")?;
    let text = native::invoke(&json, "stringify", std::slice::from_ref(value), context)?;
    Ok(text
        .as_string()
        .map_or_else(|| "null".into(), |t| t.to_std_string_escaped()))
}
