//! `process`: the global, and `require('process')`, which is the same object.
//!
//! It is shaped as Electron's renderer has it, since that is where the plugins this runtime
//! runs were written to run: `process.versions` has `node` and `electron` (and `chrome`), so a
//! library choosing between Node and the browser finds both, as it does in Obsidian; `type` is
//! `"renderer"`, and there is no `process.browser`. It is an `EventEmitter`. `env` is the
//! environment the host was started with, as Node's is; the host may change it, or empty it.
//!
//! What would reach outside the runtime is answered without doing so: `exit` and `abort` are
//! noted (`exitCode`, a warning on the console) and do not end anything; `kill` refuses;
//! `binding` finds no native module, as Node's does for one it lacks. `stdout` and `stderr`
//! write to the console the host reads.
//!
//! A realm `verve run` starts is not a guest: it is the process. [`become_process`] gives it
//! Node's `process` instead — its own argv and environment, real standard streams, a real
//! `exit` — see [`Profile`].

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{
    Context, JsBigInt, JsError, JsObject, JsResult, JsString, JsSymbol, JsValue, js_string,
    object::builtins::JsArray, property::PropertyDescriptor,
};

use crate::native::{arg, array, coded_error, exports, function, invoke, js_text, object, text_of};
use crate::{Level, console_line};

/// The Node release `process.version` claims: the one Electron 39, and with it Obsidian's
/// current desktop app (1.14), is built on. Plugins check these — Media Extended refuses an
/// Electron older than 39 — so they match the user agent the embedder gives its `navigator`.
pub const NODE_VERSION: &str = "22.20.0";
pub const ELECTRON_VERSION: &str = "39.2.0";
pub const CHROME_VERSION: &str = "142.0.7444.59";
const V8_VERSION: &str = "14.2.231.17-electron.0";

/// What a realm's `process` is: the process an embedder is building — Electron's main process,
/// its renderers, a utility — rather than the plugin host this runtime was first made for.
///
/// [`ProcessInfo::default`] is the plugin host's shape, exactly: `type` is `"renderer"`, `title`
/// is `"obsidian"`, `versions` has Electron's and Chromium's numbers in it. An embedder replaces
/// it with [`crate::Runtime::set_process_info`]. `verve run` makes one of its own, so a script
/// that is the whole program has Node's `process`.
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    /// `process.title`.
    pub title: String,
    /// `process.type`: `"browser"`, `"renderer"`, `"utility"`; `None` deletes it, as Node's
    /// `process` has no such property.
    pub process_type: Option<String>,
    /// `process.argv`, whole: the executable, the entry, its arguments.
    pub argv: Vec<String>,
    /// `process.argv0`.
    pub argv0: String,
    /// `process.execPath`.
    pub exec_path: String,
    /// `process.execArgv`.
    pub exec_argv: Vec<String>,
    /// `process.resourcesPath`; `None` leaves it undefined, as Node's is.
    pub resources_path: Option<String>,
    /// `process.defaultApp`, which Electron defines only for an app run from a folder.
    pub default_app: bool,
    /// Whether the Electron-only fields appear: `versions.electron`, `versions.chrome`.
    pub electron_fields: bool,
    /// `process.sandboxed`, which Electron's renderers have; `None` leaves it undefined, as
    /// Node's `process` has no such property.
    pub sandboxed: Option<bool>,
    /// `process.contextIsolated`, which Electron has in a renderer when it says so.
    pub context_isolated: Option<bool>,
    /// What `process.exit` does.
    pub exit: ExitBehavior,
    /// What an error nobody caught does here.
    pub on_uncaught: UncaughtBehavior,
}

impl Default for ProcessInfo {
    fn default() -> Self {
        let exec_path = std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "electron".into());
        Self {
            title: "obsidian".into(),
            process_type: Some("renderer".into()),
            argv: vec![exec_path.clone()],
            argv0: "electron".into(),
            exec_path,
            exec_argv: Vec::new(),
            resources_path: None,
            default_app: false,
            electron_fields: true,
            sandboxed: None,
            context_isolated: None,
            exit: ExitBehavior::Note,
            on_uncaught: UncaughtBehavior::Log,
        }
    }
}

/// What `process.exit` does in a realm.
#[derive(Clone)]
pub enum ExitBehavior {
    /// Noted — `exitCode` set, the `exit` event heard, a warning on the console — and nothing
    /// ends: how this runtime has always served a plugin's `process.exit`.
    Note,
    /// The process ends here, as Node's does: `verve run`.
    Process,
    /// Handed to the host — an Electron main process turns it into `app.exit(code)` — and the
    /// script that called it ends with an interrupt it cannot catch. The interrupt stays asked
    /// for until something runs and meets it, so a host that runs nothing after the call clears
    /// it with `Runtime::interrupt_handle().clear()` before the next job.
    Host(Arc<dyn Fn(i32) + Send + Sync>),
}

/// What an uncaught error does in a realm.
#[derive(Clone)]
pub enum UncaughtBehavior {
    /// Written to the console, and the realm goes on: the plugin host's, as it always was.
    Log,
    /// The process ends with 1, as Node's does.
    Exit,
    /// Told to the host, which decides: an Electron main process keeps running (and may show a
    /// dialog) until it quits.
    Report(Arc<dyn Fn(&str) + Send + Sync>),
}

impl std::fmt::Debug for ExitBehavior {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Note => out.write_str("Note"),
            Self::Process => out.write_str("Process"),
            Self::Host(_) => out.write_str("Host(..)"),
        }
    }
}

impl std::fmt::Debug for UncaughtBehavior {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Log => out.write_str("Log"),
            Self::Exit => out.write_str("Exit"),
            Self::Report(_) => out.write_str("Report(..)"),
        }
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// Seconds and nanoseconds on the host's clock: `process.hrtime`'s numbers.
fn hrtime_ns(context: &Context) -> i128 {
    (crate::now(context) * 1_000_000.) as i128
}

const PROCESS_FUNCTIONS: &[(
    &str,
    usize,
    boa_engine::native_function::NativeFunctionPointer,
)] = &[
    ("nextTick", 1, |_, args, cx| {
        let callback = arg(args, 0);
        if !callback.is_callable() {
            return Err(coded_error(
                cx,
                "The \"callback\" argument must be of type function",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        crate::microtask(cx, callback, args.iter().skip(1).cloned().collect());
        Ok(JsValue::undefined())
    }),
    ("cwd", 0, |_, _, cx| {
        Ok(js_text(&crate::shared(cx).cwd.borrow()))
    }),
    ("chdir", 1, |_, args, cx| {
        let to = text_of(&arg(args, 0), cx)?;
        let current = std::path::PathBuf::from(crate::shared(cx).cwd.borrow().clone());
        let target = current.join(&to);
        if !target.is_dir() {
            return Err(coded_error(
                cx,
                &format!("ENOENT: no such file or directory, chdir '{to}'"),
                "ENOENT",
            ));
        }
        let target = std::fs::canonicalize(&target).unwrap_or(target);
        let text = target
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_owned();
        *crate::shared(cx).cwd.borrow_mut() = text;
        Ok(JsValue::undefined())
    }),
    ("hrtime", 1, |_, args, cx| {
        let mut ns = hrtime_ns(cx);
        // Given an earlier reading, the time since it.
        let previous = crate::native::list_of(&arg(args, 0), cx)?;
        if previous.len() == 2 {
            let seconds = previous[0].to_number(cx)? as i128;
            let nanos = previous[1].to_number(cx)? as i128;
            ns -= seconds * 1_000_000_000 + nanos;
        }
        let seconds = JsValue::from((ns / 1_000_000_000) as f64);
        let nanos = JsValue::from((ns % 1_000_000_000) as f64);
        Ok(array(cx, vec![seconds, nanos]))
    }),
    ("uptime", 0, |_, _, cx| Ok((crate::now(cx) / 1000.).into())),
    ("memoryUsage", 0, |_, _, cx| memory_usage(cx)),
    ("cpuUsage", 1, |_, _, cx| {
        let usage = object(cx);
        let micros = (crate::now(cx) * 1000.).floor();
        usage.set(js_string!("user"), micros, false, cx)?;
        usage.set(js_string!("system"), 0, false, cx)?;
        Ok(usage.into())
    }),
    ("resourceUsage", 0, |_, _, cx| {
        let usage = object(cx);
        let micros = (crate::now(cx) * 1000.).floor();
        for (key, value) in [
            ("userCPUTime", micros),
            ("systemCPUTime", 0.),
            ("maxRSS", 0.),
        ] {
            usage.set(JsString::from(key), value, false, cx)?;
        }
        Ok(usage.into())
    }),
    ("emitWarning", 3, |this, args, cx| {
        let warning = arg(args, 0);
        // `emitWarning(message, type)`, `emitWarning(message, { type, code })`, or an `Error`.
        let (name, message, code) =
            if let Some(error) = warning.as_object().filter(|_| !warning.is_string()) {
                let name = text_of(&error.get(js_string!("name"), cx)?, cx)?;
                let message = text_of(&error.get(js_string!("message"), cx)?, cx)?;
                (name, message, String::new())
            } else {
                let message = text_of(&warning, cx)?;
                let second = arg(args, 1);
                let (name, code) =
                    if let Some(options) = second.as_object().filter(|_| !second.is_string()) {
                        let kind = options.get(js_string!("type"), cx)?;
                        let code = options.get(js_string!("code"), cx)?;
                        (
                            if kind.is_undefined() {
                                "Warning".to_owned()
                            } else {
                                text_of(&kind, cx)?
                            },
                            if code.is_undefined() {
                                String::new()
                            } else {
                                text_of(&code, cx)?
                            },
                        )
                    } else if second.is_undefined() {
                        ("Warning".to_owned(), String::new())
                    } else {
                        let code = arg(args, 2);
                        (
                            text_of(&second, cx)?,
                            if code.is_string() {
                                text_of(&code, cx)?
                            } else {
                                String::new()
                            },
                        )
                    };
                (name, message, code)
            };
        let label = if code.is_empty() {
            name.clone()
        } else {
            format!("[{code}] {name}")
        };
        if !warnings_silenced(cx) {
            console_line(
                cx,
                Level::Warn,
                format!("(node:{}) {label}: {message}", std::process::id()),
            );
        }
        let error = boa_engine::JsNativeError::error()
            .with_message(message)
            .into_opaque(cx);
        error.set(js_string!("name"), js_text(&name), false, cx)?;
        if !code.is_empty() {
            error.set(js_string!("code"), js_text(&code), false, cx)?;
        }
        let emitted = invoke(this, "listenerCount", &[js_text("warning")], cx)?;
        if emitted.to_number(cx)? > 0. {
            invoke(this, "emit", &[js_text("warning"), error.into()], cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("exit", 1, exit),
    ("reallyExit", 1, |_, args, cx| match exit_behavior(cx) {
        ExitBehavior::Process => finish(exit_code_of(&arg(args, 0), cx)?.unwrap_or(0)),
        _ => Ok(JsValue::undefined()),
    }),
    ("abort", 0, |_, _, cx| match exit_behavior(cx) {
        ExitBehavior::Process => std::process::abort(),
        _ => {
            console_line(
                cx,
                Level::Warn,
                "process.abort() ignored: a plugin cannot end the app".to_owned(),
            );
            Ok(JsValue::undefined())
        }
    }),
    ("kill", 2, |_, args, cx| {
        let pid = crate::print(&arg(args, 0));
        Err(coded_error(
            cx,
            &format!("kill EPERM: a plugin cannot signal process {pid}"),
            "EPERM",
        ))
    }),
    ("binding", 1, |_, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        Err(boa_engine::JsNativeError::error()
            .with_message(format!("No such module: {name}"))
            .into())
    }),
    ("_linkedBinding", 1, |_, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        Err(boa_engine::JsNativeError::error()
            .with_message(format!("No such binding: {name}"))
            .into())
    }),
    ("dlopen", 2, |_, _, cx| {
        Err(coded_error(
            cx,
            "process.dlopen: native addons cannot be loaded",
            "ERR_DLOPEN_FAILED",
        ))
    }),
    ("umask", 1, |_, _, _| Ok(0o022.into())),
    ("getActiveResourcesInfo", 0, |_, _, cx| {
        Ok(JsArray::new(cx)?.into())
    }),
    ("setUncaughtExceptionCaptureCallback", 1, |_, _, _| {
        Ok(JsValue::undefined())
    }),
    ("hasUncaughtExceptionCaptureCallback", 0, |_, _, _| {
        Ok(false.into())
    }),
    (
        "setSourceMapsEnabled",
        1,
        |_, _, _| Ok(JsValue::undefined()),
    ),
    ("constrainedMemory", 0, |_, _, _| Ok(0.into())),
    ("availableMemory", 0, |_, _, _| Ok((4u64 << 30).into())),
    ("getBuiltinModule", 1, |_, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        Ok(crate::modules::builtin(cx, &name)?.unwrap_or_default())
    }),
];

/// What `process.exit` does in this realm, copied out so no borrow is held while a listener
/// runs: one may call `set_process_info` itself.
fn exit_behavior(context: &Context) -> ExitBehavior {
    crate::shared(context).process.borrow().exit.clone()
}

/// `process.exit(code)`: what the realm's [`ExitBehavior`] says happens.
fn exit(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    match exit_behavior(context) {
        ExitBehavior::Note => note_exit(this, args, context),
        ExitBehavior::Process => {
            let code = exit_code_of(&arg(args, 0), context)?;
            end_process(context, code)
        }
        ExitBehavior::Host(host) => {
            let code = exit_code_of(&arg(args, 0), context)?;
            let code = exiting(context, code);
            host(code);
            // The script that asked to quit ends here, and cannot catch it: what the host does
            // next is on its own thread's terms.
            context.interrupt_handle().interrupt();
            Ok(JsValue::undefined())
        }
    }
}

/// `process.exit` as a plugin host has always served it: noted, not acted on.
fn note_exit(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = arg(args, 0);
    if let Some(process) = this.as_object() {
        if !code.is_undefined() {
            process.set(js_string!("exitCode"), code.clone(), false, context)?;
        }
        let code = process.get(js_string!("exitCode"), context)?;
        console_line(
            context,
            Level::Warn,
            format!(
                "process.exit({}) ignored: a plugin cannot end the app",
                crate::print(&code)
            ),
        );
        let _ = invoke(this, "emit", &[js_text("exit"), code], context);
    }
    Ok(JsValue::undefined())
}

/// `process.memoryUsage()`: Boa does not say how big its heap is, so this is a shape with
/// plausible numbers for code that only logs them or compares them with a limit.
fn memory_usage(context: &mut Context) -> JsResult<JsValue> {
    let usage = object(context);
    for (key, value) in [
        ("rss", 200_000_000.),
        ("heapTotal", 100_000_000.),
        ("heapUsed", 50_000_000.),
        ("external", 1_000_000.),
        ("arrayBuffers", 500_000.),
    ] {
        usage.set(JsString::from(key), value, false, context)?;
    }
    Ok(usage.into())
}

/// A stream of the process's own — `stdout`, `stderr`, `stdin` — as an `EventEmitter` with
/// what code writing to it, or asking about it, reaches for. Writes go to the console.
fn std_stream(context: &mut Context, emitter: &JsObject, fd: u32) -> JsResult<JsObject> {
    let stream = emitter.construct(&[], None, context)?;
    stream.set(js_string!("fd"), fd, false, context)?;
    stream.set(js_string!("isTTY"), false, false, context)?;
    stream.set(js_string!("writable"), fd != 0, false, context)?;
    stream.set(js_string!("readable"), fd == 0, false, context)?;
    stream.set(js_string!("columns"), 80, false, context)?;
    stream.set(js_string!("rows"), 24, false, context)?;
    stream.set(js_string!("_isStdio"), true, false, context)?;
    let write: boa_engine::native_function::NativeFunctionPointer = if fd == 2 {
        |_, args, cx| std_write(Level::Error, args, cx)
    } else {
        |_, args, cx| std_write(Level::Log, args, cx)
    };
    let write = function(context, "write", 3, write);
    stream.set(js_string!("write"), write, false, context)?;
    exports(
        context,
        &stream,
        &[
            ("end", 3, |this, args, cx| {
                if !arg(args, 0).is_callable() && !arg(args, 0).is_undefined() {
                    invoke(this, "write", args, cx)?;
                }
                Ok(this.clone())
            }),
            ("cork", 0, |_, _, _| Ok(JsValue::undefined())),
            ("uncork", 0, |_, _, _| Ok(JsValue::undefined())),
            ("setEncoding", 1, |this, _, _| Ok(this.clone())),
            ("setDefaultEncoding", 1, |this, _, _| Ok(this.clone())),
            ("pause", 0, |this, _, _| Ok(this.clone())),
            ("resume", 0, |this, _, _| Ok(this.clone())),
            ("read", 1, |_, _, _| Ok(JsValue::null())),
            ("unref", 0, |this, _, _| Ok(this.clone())),
            ("ref", 0, |this, _, _| Ok(this.clone())),
            ("setRawMode", 1, |this, _, _| Ok(this.clone())),
            ("getColorDepth", 0, |_, _, _| Ok(1.into())),
            ("hasColors", 0, |_, _, _| Ok(false.into())),
            ("getWindowSize", 0, |_, _, cx| {
                Ok(array(cx, vec![80.into(), 24.into()]))
            }),
            ("destroy", 0, |this, _, _| Ok(this.clone())),
        ],
    )?;
    Ok(stream)
}

/// `process.stdout.write(chunk, encoding, callback)`: the chunk on the console, a line at a time.
fn std_write(level: Level, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let chunk = arg(args, 0);
    let text = match crate::encoding::bytes_of(&chunk, context)? {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => text_of(&chunk, context)?,
    };
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    console_line(context, level, text.to_owned());
    if let Some(callback) = args.iter().skip(1).find(|a| a.is_callable()) {
        crate::microtask(context, callback.clone(), Vec::new());
    }
    Ok(true.into())
}

/// Makes `process` and puts it on the global object.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let emitter = crate::modules::node_module(context, "events")?;
    let emitter = emitter.as_object().expect("EventEmitter is a class");
    let process = emitter.construct(&[], None, context)?;
    process.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(js_text("process"))
            .writable(true)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    exports(context, &process, PROCESS_FUNCTIONS)?;
    let hrtime = process.get(js_string!("hrtime"), context)?;
    let bigint = function(context, "bigint", 0, |_, _, cx| {
        Ok(JsBigInt::new(hrtime_ns(cx) as i64).into())
    });
    hrtime
        .as_object()
        .expect("a function")
        .set(js_string!("bigint"), bigint, false, context)?;
    let memory = process.get(js_string!("memoryUsage"), context)?;
    let rss = function(context, "rss", 0, |_, _, _| Ok(200_000_000.into()));
    memory
        .as_object()
        .expect("a function")
        .set(js_string!("rss"), rss, false, context)?;
    if !cfg!(windows) {
        for name in ["getuid", "geteuid", "getgid", "getegid"] {
            let f = function(context, name, 0, |_, _, _| Ok(1000.into()));
            process.set(JsString::from(name), f, false, context)?;
        }
        let groups = function(context, "getgroups", 0, |_, _, cx| {
            Ok(array(cx, vec![1000.into()]))
        });
        process.set(js_string!("getgroups"), groups, false, context)?;
    }

    // Set in the order a plugin host's `process` has always had its keys in, so `Object.keys`
    // answers the same; `apply` sets them again, in place, when an embedder replaces the info.
    let info = crate::shared(context).process.borrow().clone();
    let platform = crate::node::platform();
    let head: [(&str, JsValue); 6] = [
        ("title", js_text(&info.title)),
        ("version", js_text(&format!("v{NODE_VERSION}"))),
        ("platform", js_text(platform)),
        ("arch", js_text(arch())),
        ("pid", std::process::id().into()),
        ("ppid", 0.into()),
    ];
    for (key, value) in head {
        process.set(JsString::from(key), value, false, context)?;
    }
    process.set(
        js_string!("execPath"),
        js_text(&info.exec_path),
        false,
        context,
    )?;
    apply_kind(context, &process, &info)?;
    process.set(js_string!("exitCode"), JsValue::undefined(), false, context)?;
    process.set(js_string!("debugPort"), 9229, false, context)?;
    process.set(js_string!("noDeprecation"), false, false, context)?;
    apply_arguments(context, &process, &info)?;

    let vars: Vec<(String, String)> = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let env = env_object(context, &vars)?;
    process.set(js_string!("env"), env, false, context)?;
    apply_versions(context, &process, &info)?;

    let release = object(context);
    release.set(js_string!("name"), js_text("node"), false, context)?;
    release.set(js_string!("lts"), js_text("Jod"), false, context)?;
    process.set(js_string!("release"), release, false, context)?;
    let features = object(context);
    for key in ["inspector", "ipv6", "tls", "cached_builtins"] {
        features.set(JsString::from(key), true, false, context)?;
    }
    features.set(js_string!("typescript"), false, false, context)?;
    process.set(js_string!("features"), features, false, context)?;
    let config = object(context);
    let variables = object(context);
    config.set(js_string!("variables"), variables, false, context)?;
    process.set(js_string!("config"), config, false, context)?;
    let flags = crate::native::construct_global(context, "Set", &[])?;
    process.set(
        js_string!("allowedNodeEnvironmentFlags"),
        flags,
        false,
        context,
    )?;

    for (name, fd) in [("stdin", 0), ("stdout", 1), ("stderr", 2)] {
        let stream = std_stream(context, &emitter, fd)?;
        process.set(JsString::from(name), stream, false, context)?;
    }
    crate::permission::install(context, &process)?;
    crate::native::define_global(context, "process", process)?;
    Ok(())
}

/// Puts what [`ProcessInfo`] says on `process`: its title, type, arguments, Electron's fields
/// and versions. Called whenever an embedder replaces the [`ProcessInfo`]; `install` makes the
/// same properties piece by piece, in the order they have always had.
pub(crate) fn apply(context: &mut Context, process: &JsObject, info: &ProcessInfo) -> JsResult<()> {
    process.set(js_string!("title"), js_text(&info.title), false, context)?;
    process.set(
        js_string!("execPath"),
        js_text(&info.exec_path),
        false,
        context,
    )?;
    apply_kind(context, process, info)?;
    apply_arguments(context, process, info)?;
    apply_versions(context, process, info)
}

/// `type` and the fields Electron's processes have: set when the info says so, and taken off
/// again when it does not, so replacing a renderer's info with a utility's leaves no trace.
fn apply_kind(context: &mut Context, process: &JsObject, info: &ProcessInfo) -> JsResult<()> {
    let renderer = info.process_type.as_deref() == Some("renderer");
    for (key, value) in [
        ("type", info.process_type.as_deref().map(js_text)),
        ("resourcesPath", info.resources_path.as_deref().map(js_text)),
        ("contextIsolated", info.context_isolated.map(JsValue::from)),
        (
            "defaultApp",
            info.default_app.then_some(JsValue::from(true)),
        ),
        (
            "sandboxed",
            info.sandboxed.filter(|_| renderer).map(JsValue::from),
        ),
    ] {
        match value {
            Some(value) => {
                process.set(JsString::from(key), value, false, context)?;
            }
            None => {
                // Deleting what is not there succeeds; only a property that cannot be removed
                // fails, and all of these are ordinary ones.
                process.delete_property_or_throw(JsString::from(key), context)?;
            }
        }
    }
    Ok(())
}

fn apply_arguments(context: &mut Context, process: &JsObject, info: &ProcessInfo) -> JsResult<()> {
    let argv: Vec<JsValue> = info.argv.iter().map(|a| js_text(a)).collect();
    let argv = array(context, argv);
    process.set(js_string!("argv"), argv, false, context)?;
    process.set(js_string!("argv0"), js_text(&info.argv0), false, context)?;
    let exec_argv: Vec<JsValue> = info.exec_argv.iter().map(|a| js_text(a)).collect();
    let exec_argv = array(context, exec_argv);
    process.set(js_string!("execArgv"), exec_argv, false, context)?;
    Ok(())
}

fn apply_versions(context: &mut Context, process: &JsObject, info: &ProcessInfo) -> JsResult<()> {
    let versions = object(context);
    versions.set(js_string!("node"), js_text(NODE_VERSION), false, context)?;
    if info.electron_fields {
        for (key, value) in [("electron", ELECTRON_VERSION), ("chrome", CHROME_VERSION)] {
            versions.set(JsString::from(key), js_text(value), false, context)?;
        }
    }
    for (key, value) in [
        ("v8", V8_VERSION),
        ("uv", "1.51.0"),
        ("zlib", "1.3.0.1-motley"),
        ("modules", "136"),
        ("napi", "10"),
        ("openssl", "0.0.0"),
        ("unicode", "16.0"),
    ] {
        versions.set(JsString::from(key), js_text(value), false, context)?;
    }
    process.set(js_string!("versions"), versions, false, context)?;
    Ok(())
}

/// Takes `process.noAsar` off again, for a realm whose overlay was removed: there is nothing
/// left for it to put aside.
pub(crate) fn hide_no_asar(context: &mut Context) -> JsResult<()> {
    let Some(process) = crate::native::global(context, "process")?.as_object() else {
        return Ok(());
    };
    process.delete_property_or_throw(js_string!("noAsar"), context)?;
    Ok(())
}

/// `process.noAsar`, as Electron has it: reading it says whether the overlay is put aside, and
/// setting it puts the overlay aside or back. An accessor over the realm's own layers, not a
/// flag of its own, so [`crate::Runtime::set_no_asar`] and JavaScript always agree.
pub(crate) fn expose_no_asar(context: &mut Context) -> JsResult<()> {
    let Some(process) = crate::native::global(context, "process")?.as_object() else {
        return Ok(());
    };
    crate::native::accessors(
        context,
        &process,
        &[(
            "noAsar",
            |_, _, cx| Ok(crate::shared(cx).vfs.borrow().no_asar.into()),
            Some(|_, args, cx| {
                crate::shared(cx).vfs.borrow_mut().no_asar = arg(args, 0).to_boolean();
                Ok(JsValue::undefined())
            }),
        )],
    )
}

/// `require('process')`: the global `process`, whatever it is now.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    crate::native::global(context, "process")
}

// ─── A process of its own ───────────────────────────────────────────────────────────────────

/// What `verve run` makes of a realm: `process` as Node's own is, for a script that is the
/// whole program rather than a plugin in someone else's window. Its console and standard
/// streams are the process's file descriptors, `process.exit` ends the process, an uncaught
/// error or unhandled rejection ends it with 1, and `process.stdin` reads the real input.
pub(crate) struct Profile {
    no_warnings: bool,
    /// Set once `exit` listeners have been told; a second exit does not tell them again.
    exiting: Cell<bool>,
    stdin: RefCell<Stdin>,
    /// Who to wake when input arrives.
    waker: crate::Waker,
    /// The entry file: `require.main` is the module run from it.
    main: RefCell<Option<PathBuf>>,
}

#[derive(Default)]
struct Stdin {
    stream: Option<JsObject>,
    /// What the reader thread read, `None` at the end; there once the stream is first read.
    chunks: Option<std::sync::mpsc::Receiver<Option<Vec<u8>>>>,
    ended: bool,
}

/// How the process the realm becomes was started.
pub(crate) struct Setup {
    /// `process.argv`, whole: the executable, the entry (none for `-e`), its arguments.
    pub argv: Vec<String>,
    /// `process.argv0`.
    pub argv0: String,
    /// `process.execPath`.
    pub exec_path: String,
    /// `process.execArgv`.
    pub exec_argv: Vec<String>,
    /// The environment `process.env` starts as.
    pub env: Vec<(String, String)>,
    /// Whether `process.emitWarning` stays quiet (`--no-warnings`).
    pub no_warnings: bool,
    /// How the process wakes its host.
    pub waker: crate::Waker,
}

impl Profile {
    /// A console line, to standard output or standard error as Node's console writes it.
    pub(crate) fn console(&self, level: Level, text: &str) {
        let mut line = String::with_capacity(text.len() + 1);
        line.push_str(text);
        line.push('\n');
        let fd = match level {
            Level::Warn | Level::Error => 2,
            _ => 1,
        };
        let _ = write_fd(fd, line.as_bytes());
    }
}

/// Bytes written to standard output (1) or standard error (2), in order, and flushed: nothing
/// a script wrote waits in a buffer for a process that is about to end.
pub(crate) fn write_fd(fd: i32, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if fd == 2 {
        let mut out = std::io::stderr().lock();
        out.write_all(bytes)?;
        out.flush()
    } else {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()
    }
}

pub(crate) fn profile(context: &Context) -> Option<Rc<Profile>> {
    crate::shared(context).profile.borrow().clone()
}

fn class_of(context: &mut Context, module: &str, name: &str) -> JsResult<JsObject> {
    let module = crate::modules::node_module(context, module)?;
    crate::native::get(&module, name, context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("{name} is not a class")))
}

/// The realm made a process: see [`Profile`]. Its `process` is Node's, as Node's is.
pub(crate) fn become_process(context: &mut Context, setup: Setup) -> JsResult<()> {
    let profile = Rc::new(Profile {
        no_warnings: setup.no_warnings,
        exiting: Cell::new(false),
        stdin: RefCell::new(Stdin::default()),
        waker: setup.waker,
        main: RefCell::new(None),
    });
    let process = crate::native::global(context, "process")?
        .as_object()
        .ok_or_else(|| crate::native::type_error("process is not an object"))?;
    let info = ProcessInfo {
        title: "verve".into(),
        process_type: None,
        argv: setup.argv.clone(),
        argv0: setup.argv0.clone(),
        exec_path: setup.exec_path.clone(),
        exec_argv: setup.exec_argv.clone(),
        resources_path: None,
        default_app: false,
        electron_fields: false,
        sandboxed: None,
        context_isolated: None,
        exit: ExitBehavior::Process,
        on_uncaught: UncaughtBehavior::Exit,
    };
    *crate::shared(context).process.borrow_mut() = info.clone();
    apply(context, &process, &info)?;
    let env = env_object(context, &setup.env)?;
    process.set(js_string!("env"), env, false, context)?;
    if let Some(versions) = process.get(js_string!("versions"), context)?.as_object() {
        let verve = js_text(env!("CARGO_PKG_VERSION"));
        versions.set(js_string!("verve"), verve, false, context)?;
    }

    let readable = class_of(context, "stream", "Readable")?;
    let options = object(context);
    options.set(js_string!("highWaterMark"), 65536, false, context)?;
    let stdin = readable.construct(&[options.into()], None, context)?;
    std_properties(context, &stdin, 0)?;
    let read = function(context, "_read", 1, |_, _, cx| {
        start_stdin(cx);
        Ok(JsValue::undefined())
    });
    stdin.set(js_string!("_read"), read, false, context)?;
    exports(
        context,
        &stdin,
        &[
            ("setRawMode", 1, |this, _, _| Ok(this.clone())),
            ("ref", 0, |this, _, cx| {
                if let Some(stream) = this.as_object() {
                    crate::native::hidden(&stream, js_string!("__unref"), false, cx)?;
                }
                Ok(this.clone())
            }),
            ("unref", 0, |this, _, cx| {
                if let Some(stream) = this.as_object() {
                    crate::native::hidden(&stream, js_string!("__unref"), true, cx)?;
                }
                Ok(this.clone())
            }),
        ],
    )?;
    process.set(js_string!("stdin"), stdin.clone(), false, context)?;
    profile.stdin.borrow_mut().stream = Some(stdin);

    let writable = class_of(context, "stream", "Writable")?;
    for (name, fd) in [("stdout", 1), ("stderr", 2)] {
        let stream = writable.construct(&[], None, context)?;
        std_properties(context, &stream, fd)?;
        let write = function(context, "_write", 3, |this, args, cx| {
            let fd = crate::native::get(this, "fd", cx)?.as_number() == Some(2.);
            let chunk = arg(args, 0);
            let bytes = match crate::encoding::bytes_of(&chunk, cx)? {
                Some(bytes) => bytes,
                None => text_of(&chunk, cx)?.into_bytes(),
            };
            let outcome = match write_fd(if fd { 2 } else { 1 }, &bytes) {
                Ok(()) => Vec::new(),
                Err(error) => {
                    let code = if error.kind() == std::io::ErrorKind::BrokenPipe {
                        "EPIPE"
                    } else {
                        "EIO"
                    };
                    vec![coded_error(cx, &format!("write {code}"), code).into_opaque(cx)?]
                }
            };
            crate::call(cx, &arg(args, 2), &JsValue::undefined(), &outcome)
        });
        stream.set(js_string!("_write"), write, false, context)?;
        exports(
            context,
            &stream,
            &[
                ("getColorDepth", 0, |_, _, _| Ok(1.into())),
                ("hasColors", 0, |_, _, _| Ok(false.into())),
                ("getWindowSize", 0, |_, _, cx| {
                    Ok(array(cx, vec![80.into(), 24.into()]))
                }),
            ],
        )?;
        process.set(JsString::from(name), stream, false, context)?;
    }

    // Node's own timers: the global `setTimeout` answers a `Timeout`, which `unref()` lets a
    // process end without.
    let timers = crate::modules::node_module(context, "timers")?;
    for name in [
        "setTimeout",
        "setInterval",
        "setImmediate",
        "clearTimeout",
        "clearInterval",
        "clearImmediate",
    ] {
        let f = crate::native::get(&timers, name, context)?;
        crate::native::define_global(context, name, f)?;
    }
    *crate::shared(context).profile.borrow_mut() = Some(profile);
    Ok(())
}

/// What a standard stream says about itself.
fn std_properties(context: &mut Context, stream: &JsObject, fd: i32) -> JsResult<()> {
    use std::io::IsTerminal;
    let tty = match fd {
        0 => std::io::stdin().is_terminal(),
        1 => std::io::stdout().is_terminal(),
        _ => std::io::stderr().is_terminal(),
    };
    crate::native::hidden(stream, js_string!("fd"), fd, context)?;
    crate::native::hidden(stream, js_string!("_isStdio"), true, context)?;
    stream.set(js_string!("isTTY"), tty, false, context)?;
    if tty && fd != 0 {
        stream.set(js_string!("columns"), 80, false, context)?;
        stream.set(js_string!("rows"), 24, false, context)?;
    }
    Ok(())
}

/// Starts reading the process's input on a thread of its own, the first time it is asked for.
fn start_stdin(context: &mut Context) {
    let Some(profile) = profile(context) else {
        return;
    };
    let mut stdin = profile.stdin.borrow_mut();
    if stdin.chunks.is_some() || stdin.ended {
        return;
    }
    let (sender, chunks) = std::sync::mpsc::channel();
    let waker = profile.waker.clone();
    let started = std::thread::Builder::new()
        .name("verve-stdin".into())
        .spawn(move || {
            use std::io::Read;
            let mut input = std::io::stdin().lock();
            let mut buffer = vec![0u8; 65536];
            loop {
                let read = match input.read(&mut buffer) {
                    Ok(0) => None,
                    Ok(n) => Some(buffer[..n].to_vec()),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => None,
                };
                let done = read.is_none();
                if sender.send(read).is_err() {
                    break;
                }
                waker();
                if done {
                    break;
                }
            }
        });
    match started {
        Ok(_) => stdin.chunks = Some(chunks),
        Err(_) => stdin.ended = true,
    }
}

/// What the process's input brought since the last look, pushed into `process.stdin`.
/// Answers whether there was anything.
pub(crate) fn deliver_stdin(context: &mut Context) -> bool {
    let Some(profile) = profile(context) else {
        return false;
    };
    let mut delivered = false;
    loop {
        let (next, stream) = {
            let stdin = profile.stdin.borrow();
            let next = stdin.chunks.as_ref().and_then(|c| c.try_recv().ok());
            (next, stdin.stream.clone())
        };
        let (Some(next), Some(stream)) = (next, stream) else {
            break;
        };
        delivered = true;
        let chunk = match next {
            Some(bytes) => crate::encoding::make_buffer(context, bytes),
            None => {
                let mut stdin = profile.stdin.borrow_mut();
                stdin.ended = true;
                stdin.chunks = None;
                Ok(JsValue::null())
            }
        };
        let pushed = chunk.and_then(|chunk| invoke(&stream.into(), "push", &[chunk], context));
        if let Err(error) = pushed {
            crate::report(context, &error);
        }
        let _ = context.run_jobs();
    }
    delivered
}

/// Whether the process's input keeps it running: it is being read — flowing, or listened to
/// for `readable` — has not ended, and was not `unref()`ed.
pub(crate) fn stdin_keeps_alive(context: &mut Context) -> bool {
    let Some(profile) = profile(context) else {
        return false;
    };
    let stream = {
        let stdin = profile.stdin.borrow();
        if stdin.ended || stdin.chunks.is_none() {
            return false;
        }
        stdin.stream.clone()
    };
    let Some(stream) = stream else {
        return false;
    };
    let stream: JsValue = stream.into();
    let check = |context: &mut Context| -> JsResult<bool> {
        if crate::native::get(&stream, "__unref", context)?.to_boolean()
            || crate::native::get(&stream, "destroyed", context)?.to_boolean()
        {
            return Ok(false);
        }
        let state = crate::native::get(&stream, "_readableState", context)?;
        let flowing = crate::native::get(&state, "flowing", context)?;
        if flowing.as_boolean() == Some(true) {
            return Ok(true);
        }
        let readable = invoke(&stream, "listenerCount", &[js_text("readable")], context)?;
        Ok(readable.to_number(context)? > 0.)
    };
    check(context).unwrap_or(false)
}

/// `require.main` is the module run from `path`.
pub(crate) fn set_main(context: &Context, path: PathBuf) {
    if let Some(profile) = profile(context) {
        *profile.main.borrow_mut() = Some(path);
    }
}

pub(crate) fn main_path(context: &Context) -> Option<PathBuf> {
    profile(context).and_then(|p| p.main.borrow().clone())
}

/// Whether warnings are kept off standard error: `--no-warnings`.
pub(crate) fn warnings_silenced(context: &Context) -> bool {
    profile(context).is_some_and(|p| p.no_warnings)
}

/// `process.exit(code)`'s and `process.exitCode`'s number; `None` for none.
fn exit_code_of(value: &JsValue, context: &mut Context) -> JsResult<Option<i32>> {
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let number = value.to_number(context)?;
    if !number.is_finite() || number.fract() != 0. {
        return Err(coded_error(
            context,
            &format!(
                "The \"code\" argument must be an integer. Received {}",
                crate::print(value)
            ),
            "ERR_OUT_OF_RANGE",
        ));
    }
    Ok(Some(number as i32))
}

/// `process.exitCode`, as the process would end with it now.
pub(crate) fn exit_code(context: &mut Context) -> i32 {
    let code = crate::native::global(context, "process")
        .and_then(|p| crate::native::get(&p, "exitCode", context));
    match code {
        Ok(code) => exit_code_of(&code, context).ok().flatten().unwrap_or(0),
        Err(_) => 0,
    }
}

/// The end, as Node ends: `exitCode` set, `exit` listeners told — once — and whatever they
/// left as `exitCode` is the code. Answers that code; the caller ends the process with it.
pub(crate) fn exiting(context: &mut Context, code: Option<i32>) -> i32 {
    // What the code cache compiled is written before the process can end.
    crate::code_cache::flush(context, true);
    // `process.exit()` with nothing said takes the `exitCode` a script already set, as Node's.
    let code = code.unwrap_or_else(|| exit_code(context));
    // `verve run`'s `exit` listeners are told once; a host's realm has no such latch, and the
    // host decides what happens next anyway.
    if let Some(profile) = profile(context)
        && profile.exiting.replace(true)
    {
        return code;
    }
    let process = crate::native::global(context, "process").unwrap_or_default();
    if let Some(process) = process.as_object() {
        let _ = process.set(js_string!("exitCode"), code, false, context);
        let _ = process.set(js_string!("_exiting"), true, false, context);
    }
    if let Err(error) = invoke(&process, "emit", &[js_text("exit"), code.into()], context) {
        print_uncaught(context, &error);
        return 7;
    }
    exit_code(context)
}

/// `process.exit`: listeners told, output flushed, and the process ended right here.
fn end_process(context: &mut Context, code: Option<i32>) -> ! {
    let code = exiting(context, code);
    finish(code)
}

fn finish(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(code)
}

/// An error on standard error, as Node shows one that ended its process: the stack of an
/// `Error`, with what else it carries; anything else as `console.error` would show it.
pub(crate) fn print_uncaught(context: &mut Context, error: &JsError) {
    let value = error.clone().into_opaque(context).unwrap_or_default();
    let text = crate::util::console_text(std::slice::from_ref(&value), context)
        .unwrap_or_else(|_| crate::describe(context, error));
    let _ = write_fd(2, format!("{text}\n").as_bytes());
}

/// An error nobody caught: to the `uncaughtException` listeners if there are any, or the end
/// of the process, with 1, as Node's.
pub(crate) fn uncaught(context: &mut Context, error: &JsError) {
    let Some(profile) = profile(context) else {
        return;
    };
    if profile.exiting.get() {
        print_uncaught(context, error);
        finish(7);
    }
    let process = crate::native::global(context, "process").unwrap_or_default();
    let listening = invoke(
        &process,
        "listenerCount",
        &[js_text("uncaughtException")],
        context,
    )
    .ok()
    .and_then(|n| n.as_number())
    .unwrap_or(0.);
    if listening > 0. {
        let value = error.clone().into_opaque(context).unwrap_or_default();
        let heard = invoke(
            &process,
            "emit",
            &[
                js_text("uncaughtException"),
                value,
                js_text("uncaughtException"),
            ],
            context,
        );
        match heard {
            Ok(_) => return,
            Err(error) => {
                print_uncaught(context, &error);
                end_process(context, Some(7));
            }
        }
    }
    print_uncaught(context, error);
    // Only a host that is told reads the error's `name` and `message`, which may be getters.
    after_uncaught(context, |context| crate::describe(context, error));
}

/// What an uncaught error does, in both paths that report one: the process ends with 1, the
/// host hears it, or the console's line is all there is. `line` makes the text the host is told,
/// and runs only for a host that is: describing an error can run its getters.
pub(crate) fn after_uncaught(context: &mut Context, line: impl FnOnce(&mut Context) -> String) {
    let behavior = crate::shared(context).process.borrow().on_uncaught.clone();
    match behavior {
        UncaughtBehavior::Log => {}
        UncaughtBehavior::Exit => end_process(context, Some(1)),
        UncaughtBehavior::Report(report) => report(&line(context)),
    }
}

/// A rejection nobody handled, with no `unhandledRejection` listener: Node's default
/// (`--unhandled-rejections=throw`) makes it an uncaught exception — the reason itself when
/// it is an `Error`, else an `ERR_UNHANDLED_REJECTION` that names it.
pub(crate) fn unhandled_rejection(context: &mut Context, reason: JsValue) {
    let is_error = reason
        .as_object()
        .is_some_and(|o| o.is::<boa_engine::builtins::error::Error>());
    let error = if is_error {
        JsError::from_opaque(reason)
    } else {
        let shown = crate::util::console_text(std::slice::from_ref(&reason), context)
            .unwrap_or_else(|_| crate::print(&reason));
        let error = coded_error(
            context,
            &format!(
                "This error originated either by throwing inside of an async function without a \
                 catch block, or by rejecting a promise which was not handled with .catch(). The \
                 promise rejected with the reason \"{shown}\"."
            ),
            "ERR_UNHANDLED_REJECTION",
        );
        if let Ok(value) = error.clone().into_opaque(context)
            && let Some(object) = value.as_object()
        {
            let name = js_text("UnhandledPromiseRejection");
            let _ = object.set(js_string!("name"), name, false, context);
        }
        error
    };
    uncaught(context, &error);
}

/// `process.env` holding `vars`. On Windows a name is found whatever its case,
/// as Windows and Node's `process.env` find it — `Path` is `PATH`, and a proxy
/// variable set as `https_proxy` answers `HTTPS_PROXY` — while the names list
/// as they were given. A value set is kept as a string, as Node keeps it.
pub(crate) fn env_object(context: &mut Context, vars: &[(String, String)]) -> JsResult<JsObject> {
    let target = object(context);
    for (key, value) in vars {
        target.set(JsString::from(key.as_str()), js_text(value), false, context)?;
    }
    if !cfg!(windows) {
        return Ok(target);
    }
    let wrap = context.eval(boa_engine::Source::from_bytes(ENV_PROXY))?;
    let env = crate::call(context, &wrap, &JsValue::undefined(), &[target.into()])?;
    env.as_object()
        .ok_or_else(|| crate::native::type_error("process.env is not an object"))
}

/// What makes `process.env` case-insensitive: a proxy over the object holding the variables.
const ENV_PROXY: &str = r#"(target) => {
  const find = (key) => {
    const upper = key.toUpperCase();
    return Object.keys(target).find((name) => name.toUpperCase() === upper);
  };
  const real = (key) => (typeof key === 'string' ? find(key) : key);
  return new Proxy(target, {
    get(t, key, receiver) {
      const name = real(key);
      return name === undefined ? undefined : Reflect.get(t, name, receiver);
    },
    set(t, key, value) {
      if (typeof key !== 'string') return Reflect.set(t, key, value);
      t[find(key) ?? key] = String(value);
      return true;
    },
    has(t, key) {
      return real(key) !== undefined;
    },
    deleteProperty(t, key) {
      const name = real(key);
      if (name !== undefined) delete t[name];
      return true;
    },
    getOwnPropertyDescriptor(t, key) {
      const name = real(key);
      return name === undefined ? undefined : Reflect.getOwnPropertyDescriptor(t, name);
    },
    defineProperty(t, key, descriptor) {
      const name = typeof key === 'string' ? (find(key) ?? key) : key;
      return Reflect.defineProperty(t, name, descriptor);
    },
  });
}"#;
