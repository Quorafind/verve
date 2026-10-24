//! `readline` (and `readline/promises`), `tty`, `repl`, and the `console` module: Node's
//! terminal modules, which libraries require for command-line paths a plugin never takes.
//!
//! A readline `Interface` reads lines from its `input` when that is an emitter of `data` —
//! each line is a `line` event, the answer to a pending `question`, and an item of its async
//! iterator. With no input that ever speaks (a plugin has no terminal), a `question` is simply
//! never answered. `tty.isatty` is `false` for every descriptor, as for a program whose output
//! is not a terminal. `require('console')` is the global `console`, with Node's `Console` class,
//! whose instances write to the streams they are given.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsSymbol, JsValue, js_string,
    native_function::NativeFunctionPointer,
};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error, exports, function, get, hidden, invoke,
    js_text, list_of, object, text_of,
};

fn emitter_class(context: &mut Context) -> JsResult<JsObject> {
    crate::modules::node_module(context, "events")?
        .as_object()
        .ok_or_else(|| crate::native::type_error("EventEmitter is not a class"))
}

fn emitter_init(this: &JsObject, context: &mut Context) -> JsResult<()> {
    let emitter = emitter_class(context)?;
    crate::call(context, &emitter.into(), &this.clone().into(), &[])?;
    Ok(())
}

fn returns_this(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(this.clone())
}

fn callback_later(args: &[JsValue], context: &mut Context) {
    if let Some(callback) = args.iter().rev().find(|a| a.is_callable()) {
        crate::microtask(context, callback.clone(), Vec::new());
    }
}

// ─── Interface ──────────────────────────────────────────────────────────────────────────────

fn emit(target: &JsObject, event: &str, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let mut all = vec![js_text(event)];
    all.extend_from_slice(args);
    invoke(&target.clone().into(), "emit", &all, context)?;
    Ok(())
}

/// A line read: to the oldest `question` waiting, else to the oldest async-iterator `next()`
/// waiting, else kept for the iterator — and a `line` event either way.
fn deliver(interface: &JsObject, line: &str, context: &mut Context) -> JsResult<()> {
    let questions = list_of(&interface.get(js_string!("__questions"), context)?, context)?;
    if let Some((first, rest)) = questions.split_first() {
        let rest = array(context, rest.to_vec());
        hidden(interface, js_string!("__questions"), rest, context)?;
        crate::call(context, first, &JsValue::undefined(), &[js_text(line)])?;
        return Ok(());
    }
    let waiting = list_of(&interface.get(js_string!("__waiting"), context)?, context)?;
    if let Some((first, rest)) = waiting.split_first() {
        let rest = array(context, rest.to_vec());
        hidden(interface, js_string!("__waiting"), rest, context)?;
        let step = step(js_text(line), false, context)?;
        crate::call(context, first, &JsValue::undefined(), &[step])?;
    } else {
        let lines = interface.get(js_string!("__lines"), context)?;
        invoke(&lines, "push", &[js_text(line)], context)?;
    }
    emit(interface, "line", &[js_text(line)], context)
}

fn step(value: JsValue, done: bool, context: &mut Context) -> JsResult<JsValue> {
    let result = object(context);
    result.set(js_string!("value"), value, false, context)?;
    result.set(js_string!("done"), done, false, context)?;
    Ok(result.into())
}

/// Text from the input: added to what is buffered, and every whole line in it delivered.
fn feed(interface: &JsObject, text: &str, context: &mut Context) -> JsResult<()> {
    if interface.get(js_string!("closed"), context)?.to_boolean() {
        return Ok(());
    }
    let buffered = text_of(&interface.get(js_string!("__partial"), context)?, context)?;
    let all = format!("{buffered}{text}");
    let mut parts: Vec<&str> = all.split('\n').collect();
    let rest = parts.pop().unwrap_or("").to_owned();
    hidden(interface, js_string!("__partial"), js_text(&rest), context)?;
    for line in parts {
        deliver(interface, line.strip_suffix('\r').unwrap_or(line), context)?;
    }
    Ok(())
}

fn on_data(
    _: &JsValue,
    args: &[JsValue],
    interface: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let chunk = arg(args, 0);
    let text = match crate::encoding::bytes_of(&chunk, context)? {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => text_of(&chunk, context)?,
    };
    feed(interface, &text, context)?;
    Ok(JsValue::undefined())
}

fn on_end(
    _: &JsValue,
    _: &[JsValue],
    interface: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let rest = text_of(&interface.get(js_string!("__partial"), context)?, context)?;
    if !rest.is_empty() {
        hidden(interface, js_string!("__partial"), js_text(""), context)?;
        deliver(interface, &rest, context)?;
    }
    close(interface, context)?;
    Ok(JsValue::undefined())
}

fn close(interface: &JsObject, context: &mut Context) -> JsResult<()> {
    if interface.get(js_string!("closed"), context)?.to_boolean() {
        return Ok(());
    }
    interface.set(js_string!("closed"), true, false, context)?;
    for waiting in list_of(&interface.get(js_string!("__waiting"), context)?, context)? {
        let done = step(JsValue::undefined(), true, context)?;
        crate::call(context, &waiting, &JsValue::undefined(), &[done])?;
    }
    let empty = array(context, Vec::new());
    hidden(interface, js_string!("__waiting"), empty, context)?;
    emit(interface, "close", &[], context)
}

fn write_out(interface: &JsValue, text: &str, context: &mut Context) -> JsResult<()> {
    let output = get(interface, "output", context)?;
    if get(&output, "write", context)?.is_callable() {
        invoke(&output, "write", &[js_text(text)], context)?;
    }
    Ok(())
}

fn interface_init(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, context)?;
    // `createInterface({ input, output, prompt })`, or `(input, output, completer, terminal)`.
    let first = arg(args, 0);
    let options_form = first.is_object() && !get(&first, "input", context)?.is_undefined();
    let (input, output, prompt) = if options_form {
        (
            get(&first, "input", context)?,
            get(&first, "output", context)?,
            get(&first, "prompt", context)?,
        )
    } else {
        (first, arg(args, 1), JsValue::undefined())
    };
    this.set(js_string!("input"), input.clone(), false, context)?;
    this.set(js_string!("output"), output, false, context)?;
    this.set(js_string!("terminal"), false, false, context)?;
    this.set(js_string!("line"), js_text(""), false, context)?;
    this.set(js_string!("cursor"), 0, false, context)?;
    this.set(js_string!("closed"), false, false, context)?;
    let prompt = if prompt.is_undefined() {
        js_text("> ")
    } else {
        prompt
    };
    hidden(this, js_string!("__prompt"), prompt, context)?;
    hidden(this, js_string!("__partial"), js_text(""), context)?;
    for key in ["__questions", "__waiting", "__lines"] {
        let list = array(context, Vec::new());
        hidden(this, JsString::from(key), list, context)?;
    }
    if get(&input, "on", context)?.is_callable() {
        let data = closure(context, "", 1, on_data, this.clone());
        invoke(&input, "on", &[js_text("data"), data.into()], context)?;
        let end = closure(context, "", 0, on_end, this.clone());
        invoke(&input, "on", &[js_text("end"), end.into()], context)?;
    }
    Ok(())
}

/// `question(query, [options], callback)`: the query written out, the callback given the next
/// line.
fn question(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let query = text_of(&arg(args, 0), context)?;
    write_out(this, &query, context)?;
    if let Some(callback) = args.iter().skip(1).find(|a| a.is_callable()) {
        let questions = get(this, "__questions", context)?;
        invoke(&questions, "push", std::slice::from_ref(callback), context)?;
    }
    Ok(JsValue::undefined())
}

const INTERFACE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("question", 3, question),
    ("close", 0, |this, _, cx| {
        if let Some(interface) = this.as_object() {
            close(&interface, cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("pause", 0, |this, _, cx| {
        if let Some(interface) = this.as_object() {
            emit(&interface, "pause", &[], cx)?;
        }
        Ok(this.clone())
    }),
    ("resume", 0, |this, _, cx| {
        if let Some(interface) = this.as_object() {
            emit(&interface, "resume", &[], cx)?;
        }
        Ok(this.clone())
    }),
    ("setPrompt", 1, |this, args, cx| {
        if let Some(interface) = this.as_object() {
            hidden(&interface, js_string!("__prompt"), arg(args, 0), cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("getPrompt", 0, |this, _, cx| get(this, "__prompt", cx)),
    ("prompt", 1, |this, _, cx| {
        let prompt = text_of(&get(this, "__prompt", cx)?, cx)?;
        write_out(this, &prompt, cx)?;
        Ok(JsValue::undefined())
    }),
    ("write", 2, |this, args, cx| {
        // As if typed at the input.
        let data = arg(args, 0);
        if let Some(interface) = this.as_object()
            && !data.is_null_or_undefined()
        {
            let text = text_of(&data, cx)?;
            feed(&interface, &text, cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("getCursorPos", 0, |_, _, cx| {
        let pos = object(cx);
        pos.set(js_string!("rows"), 0, false, cx)?;
        pos.set(js_string!("cols"), 0, false, cx)?;
        Ok(pos.into())
    }),
    ("next", 0, |this, _, cx| {
        // The async iterator over lines: `for await (const line of rl)`.
        let Some(interface) = this.as_object() else {
            return Ok(JsValue::undefined());
        };
        let lines = interface.get(js_string!("__lines"), cx)?;
        if list_of(&lines, cx)?.is_empty() {
            if interface.get(js_string!("closed"), cx)?.to_boolean() {
                let done = step(JsValue::undefined(), true, cx)?;
                return crate::native::settled(Ok(done), cx);
            }
            let (promise, resolvers) = boa_engine::object::builtins::JsPromise::new_pending(cx);
            let waiting = interface.get(js_string!("__waiting"), cx)?;
            invoke(&waiting, "push", &[resolvers.resolve.into()], cx)?;
            return Ok(promise.into());
        }
        let line = invoke(&lines, "shift", &[], cx)?;
        let result = step(line, false, cx)?;
        crate::native::settled(Ok(result), cx)
    }),
    ("return", 0, |this, _, cx| {
        if let Some(interface) = this.as_object() {
            close(&interface, cx)?;
        }
        let done = step(JsValue::undefined(), true, cx)?;
        crate::native::settled(Ok(done), cx)
    }),
];

/// `readline/promises`' `question(query)`: a promise of the next line.
fn question_promise(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (promise, resolvers) = boa_engine::object::builtins::JsPromise::new_pending(context);
    question(this, &[arg(args, 0), resolvers.resolve.into()], context)?;
    Ok(promise.into())
}

const CURSOR_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("clearLine", 3, |_, args, cx| {
        callback_later(args, cx);
        Ok(true.into())
    }),
    ("clearScreenDown", 2, |_, args, cx| {
        callback_later(args, cx);
        Ok(true.into())
    }),
    ("cursorTo", 4, |_, args, cx| {
        callback_later(args, cx);
        Ok(true.into())
    }),
    ("moveCursor", 4, |_, args, cx| {
        callback_later(args, cx);
        Ok(true.into())
    }),
    ("emitKeypressEvents", 2, |_, _, _| Ok(JsValue::undefined())),
];

fn interface_class(context: &mut Context, promises: bool) -> JsResult<JsObject> {
    let emitter = emitter_class(context)?;
    let (class, prototype) = class(
        context,
        ClassSpec {
            name: "Interface",
            parent: Some(&emitter),
            init: interface_init,
            methods: INTERFACE_METHODS,
            ..ClassSpec::default()
        },
    )?;
    if promises {
        let question = function(context, "question", 2, question_promise);
        prototype.set(js_string!("question"), question, false, context)?;
    }
    let itself = function(context, "[Symbol.asyncIterator]", 0, |this, _, _| {
        Ok(this.clone())
    });
    hidden(&prototype, JsSymbol::async_iterator(), itself, context)?;
    Ok(class)
}

fn create_interface(module: &str, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let module = crate::modules::node_module(context, module)?;
    let class = get(&module, "Interface", context)?;
    let class = class
        .as_object()
        .ok_or_else(|| crate::native::type_error("Interface is not a class"))?;
    Ok(class.construct(args, None, context)?.into())
}

pub(crate) fn readline_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, CURSOR_FUNCTIONS)?;
    let interface = interface_class(context, false)?;
    module.set(js_string!("Interface"), interface, false, context)?;
    let create = function(context, "createInterface", 4, |_, args, cx| {
        create_interface("readline", args, cx)
    });
    module.set(js_string!("createInterface"), create, false, context)?;
    let promises = crate::modules::node_module(context, "readline/promises")?;
    module.set(js_string!("promises"), promises, false, context)?;
    Ok(module.into())
}

pub(crate) fn readline_promises_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let interface = interface_class(context, true)?;
    module.set(js_string!("Interface"), interface, false, context)?;
    let create = function(context, "createInterface", 4, |_, args, cx| {
        create_interface("readline/promises", args, cx)
    });
    module.set(js_string!("createInterface"), create, false, context)?;
    let (readline, _) = class(
        context,
        ClassSpec {
            name: "Readline",
            length: 2,
            init: |this, args, cx| {
                this.set(js_string!("stream"), arg(args, 0), false, cx)?;
                Ok(())
            },
            methods: &[
                ("clearLine", 1, returns_this),
                ("clearScreenDown", 0, returns_this),
                ("cursorTo", 2, returns_this),
                ("moveCursor", 2, returns_this),
                ("commit", 0, |_, _, cx| {
                    crate::native::settled(Ok(JsValue::undefined()), cx)
                }),
                ("rollback", 0, returns_this),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Readline"), readline, false, context)?;
    Ok(module.into())
}

// ─── tty ────────────────────────────────────────────────────────────────────────────────────

pub(crate) fn tty_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let isatty = function(context, "isatty", 1, |_, _, _| Ok(false.into()));
    module.set(js_string!("isatty"), isatty, false, context)?;
    let net = crate::modules::node_module(context, "net")?;
    let socket = get(&net, "Socket", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("net.Socket"))?;
    let (read, _) = class(
        context,
        ClassSpec {
            name: "ReadStream",
            parent: Some(&socket),
            init: |this, args, cx| {
                emitter_init(this, cx)?;
                this.set(js_string!("fd"), arg(args, 0), false, cx)?;
                this.set(js_string!("isTTY"), true, false, cx)?;
                this.set(js_string!("isRaw"), false, false, cx)?;
                Ok(())
            },
            methods: &[("setRawMode", 1, returns_this)],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("ReadStream"), read, false, context)?;
    let (write, _) = class(
        context,
        ClassSpec {
            name: "WriteStream",
            parent: Some(&socket),
            init: |this, args, cx| {
                emitter_init(this, cx)?;
                this.set(js_string!("fd"), arg(args, 0), false, cx)?;
                this.set(js_string!("isTTY"), true, false, cx)?;
                this.set(js_string!("columns"), 80, false, cx)?;
                this.set(js_string!("rows"), 24, false, cx)?;
                Ok(())
            },
            methods: &[
                ("getColorDepth", 1, |_, _, _| Ok(1.into())),
                ("hasColors", 2, |_, _, _| Ok(false.into())),
                ("getWindowSize", 0, |_, _, cx| {
                    Ok(array(cx, vec![80.into(), 24.into()]))
                }),
                ("clearLine", 2, |_, args, cx| {
                    callback_later(args, cx);
                    Ok(true.into())
                }),
                ("clearScreenDown", 1, |_, args, cx| {
                    callback_later(args, cx);
                    Ok(true.into())
                }),
                ("cursorTo", 3, |_, args, cx| {
                    callback_later(args, cx);
                    Ok(true.into())
                }),
                ("moveCursor", 3, |_, args, cx| {
                    callback_later(args, cx);
                    Ok(true.into())
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("WriteStream"), write, false, context)?;
    Ok(module.into())
}

// ─── repl ───────────────────────────────────────────────────────────────────────────────────

pub(crate) fn repl_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let start = function(context, "start", 1, |_, _, cx| {
        Err(coded_error(
            cx,
            "repl.start: there is no terminal for a REPL here",
            "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ))
    });
    module.set(js_string!("start"), start, false, context)?;
    let (server, _) = class(
        context,
        ClassSpec {
            name: "REPLServer",
            init: |_, _, cx| {
                Err(coded_error(
                    cx,
                    "repl.REPLServer: there is no terminal for a REPL here",
                    "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
                ))
            },
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("REPLServer"), server, false, context)?;
    let names: Vec<JsValue> = crate::node::MODULES
        .iter()
        .map(|(n, _)| js_text(n))
        .collect();
    let names = array(context, names);
    module.set(js_string!("builtinModules"), names, false, context)?;
    for (key, value) in [
        ("REPL_MODE_SLOPPY", "sloppy"),
        ("REPL_MODE_STRICT", "strict"),
    ] {
        let symbol = boa_engine::JsSymbol::new(Some(JsString::from(value)))
            .map_or(JsValue::undefined(), JsValue::from);
        module.set(JsString::from(key), symbol, false, context)?;
    }
    Ok(module.into())
}

// ─── console ────────────────────────────────────────────────────────────────────────────────

/// A line written to a `Console`'s stream: `util.format` of the arguments, and a new line.
fn console_write(
    this: &JsValue,
    stream: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let util = crate::modules::node_module(context, "util")?;
    let text = invoke(&util, "format", args, context)?;
    let text = format!("{}\n", text_of(&text, context)?);
    let mut target = get(this, stream, context)?;
    if !get(&target, "write", context)?.is_callable() {
        target = get(this, "_stdout", context)?;
    }
    invoke(&target, "write", &[js_text(&text)], context)?;
    Ok(JsValue::undefined())
}

/// What a `Console` method does that is not writing: the global console's own.
fn console_delegate(name: &str, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let console = crate::native::global(context, "console")?;
    invoke(&console, name, args, context)
}

const CONSOLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("log", 0, |this, args, cx| {
        console_write(this, "_stdout", args, cx)
    }),
    ("info", 0, |this, args, cx| {
        console_write(this, "_stdout", args, cx)
    }),
    ("debug", 0, |this, args, cx| {
        console_write(this, "_stdout", args, cx)
    }),
    ("dir", 0, |this, args, cx| {
        console_write(this, "_stdout", &args[..args.len().min(1)], cx)
    }),
    ("table", 0, |this, args, cx| {
        console_write(this, "_stdout", args, cx)
    }),
    ("warn", 0, |this, args, cx| {
        console_write(this, "_stderr", args, cx)
    }),
    ("error", 0, |this, args, cx| {
        console_write(this, "_stderr", args, cx)
    }),
    ("trace", 0, |this, args, cx| {
        console_write(this, "_stderr", args, cx)
    }),
    ("assert", 0, |this, args, cx| {
        if arg(args, 0).to_boolean() {
            return Ok(JsValue::undefined());
        }
        let mut rest = vec![js_text("Assertion failed")];
        rest.extend(args.iter().skip(1).cloned());
        console_write(this, "_stderr", &rest, cx)
    }),
    ("time", 0, |_, args, cx| console_delegate("time", args, cx)),
    ("timeEnd", 0, |_, args, cx| {
        console_delegate("timeEnd", args, cx)
    }),
    ("timeLog", 0, |_, args, cx| {
        console_delegate("timeLog", args, cx)
    }),
    ("count", 0, |_, args, cx| {
        console_delegate("count", args, cx)
    }),
    ("countReset", 0, |_, args, cx| {
        console_delegate("countReset", args, cx)
    }),
    ("group", 0, |_, args, cx| {
        console_delegate("group", args, cx)
    }),
    ("groupCollapsed", 0, |_, args, cx| {
        console_delegate("groupCollapsed", args, cx)
    }),
    ("groupEnd", 0, |_, args, cx| {
        console_delegate("groupEnd", args, cx)
    }),
    ("clear", 0, |_, _, _| Ok(JsValue::undefined())),
];

pub(crate) fn console_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let console = crate::native::global(context, "console")?;
    let (class, _) = class(
        context,
        ClassSpec {
            name: "Console",
            length: 1,
            init: |this, args, cx| {
                // `new Console(stdout, stderr)` or `new Console({ stdout, stderr })`.
                let first = arg(args, 0);
                let options_form = first.is_object() && !get(&first, "stdout", cx)?.is_undefined();
                let (stdout, stderr) = if options_form {
                    (get(&first, "stdout", cx)?, get(&first, "stderr", cx)?)
                } else {
                    (first, arg(args, 1))
                };
                if !get(&stdout, "write", cx)?.is_callable() {
                    return Err(coded_error(
                        cx,
                        "The \"stdout\" argument must be a writable stream",
                        "ERR_CONSOLE_WRITABLE_STREAM",
                    ));
                }
                let stderr = if stderr.is_undefined() {
                    stdout.clone()
                } else {
                    stderr
                };
                hidden(this, js_string!("_stdout"), stdout, cx)?;
                hidden(this, js_string!("_stderr"), stderr, cx)?;
                Ok(())
            },
            methods: CONSOLE_METHODS,
            ..ClassSpec::default()
        },
    )?;
    if let Some(console) = console.as_object() {
        hidden(&console, js_string!("Console"), class, context)?;
    }
    Ok(console)
}
