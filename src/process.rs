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

use boa_engine::{
    Context, JsBigInt, JsObject, JsResult, JsString, JsValue, js_string, object::builtins::JsArray,
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
        console_line(
            cx,
            Level::Warn,
            format!("(node:{}) {label}: {message}", std::process::id()),
        );
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
    ("exit", 1, |this, args, cx| {
        let code = arg(args, 0);
        if let Some(process) = this.as_object() {
            if !code.is_undefined() {
                process.set(js_string!("exitCode"), code.clone(), false, cx)?;
            }
            let code = process.get(js_string!("exitCode"), cx)?;
            console_line(
                cx,
                Level::Warn,
                format!(
                    "process.exit({}) ignored: a plugin cannot end the app",
                    crate::print(&code)
                ),
            );
            let _ = invoke(this, "emit", &[js_text("exit"), code], cx);
        }
        Ok(JsValue::undefined())
    }),
    ("reallyExit", 1, |_, _, _| Ok(JsValue::undefined())),
    ("abort", 0, |_, _, cx| {
        console_line(
            cx,
            Level::Warn,
            "process.abort() ignored: a plugin cannot end the app".to_owned(),
        );
        Ok(JsValue::undefined())
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

    let platform = crate::node::platform();
    let exec_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "electron".into());
    let values: [(&str, JsValue); 11] = [
        ("title", js_text("obsidian")),
        ("version", js_text(&format!("v{NODE_VERSION}"))),
        ("platform", js_text(platform)),
        ("arch", js_text(arch())),
        ("pid", std::process::id().into()),
        ("ppid", 0.into()),
        ("execPath", js_text(&exec_path)),
        ("type", js_text("renderer")),
        ("exitCode", JsValue::undefined()),
        ("debugPort", 9229.into()),
        ("noDeprecation", false.into()),
    ];
    for (key, value) in values {
        process.set(JsString::from(key), value, false, context)?;
    }
    let argv = array(context, vec![js_text(&exec_path)]);
    process.set(js_string!("argv"), argv, false, context)?;
    let argv0 = js_text("electron");
    process.set(js_string!("argv0"), argv0, false, context)?;
    let exec_argv = JsArray::new(context)?;
    process.set(js_string!("execArgv"), exec_argv, false, context)?;

    let env = object(context);
    for (key, value) in std::env::vars_os() {
        env.set(
            JsString::from(key.to_string_lossy().as_ref()),
            js_text(&value.to_string_lossy()),
            false,
            context,
        )?;
    }
    process.set(js_string!("env"), env, false, context)?;

    let versions = object(context);
    for (key, value) in [
        ("node", NODE_VERSION),
        ("electron", ELECTRON_VERSION),
        ("chrome", CHROME_VERSION),
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
    crate::native::define_global(context, "process", process)?;
    Ok(())
}

/// `require('process')`: the global `process`, whatever it is now.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    crate::native::global(context, "process")
}
