//! Node's own modules, as Electron gives them to a desktop plugin: `path`, `util`, `events`,
//! `os`, `url`, `fs` (and `fs/promises`), `child_process`, `http` and `https` — each native here.
//!
//! `fs` reads and writes the disk as Node's does: a desktop Obsidian plugin may, and some do.
//! `child_process` is there to be required, but running programs is not something this
//! runtime does for a plugin: every call to it fails, as a command that could not start does.
//! So with `http`: a server's `listen` hears `error`, and so does a request — plugins ask the
//! network through `requestUrl`.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArray, JsPromise},
    },
};

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn text_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

fn js_text(text: &str) -> JsValue {
    JsString::from(text).into()
}

fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

fn object(context: &mut Context) -> JsObject {
    JsObject::with_object_proto(context.intrinsics())
}

fn function(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

fn with_functions(
    context: &mut Context,
    target: &JsObject,
    functions: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<()> {
    for (name, length, f) in functions {
        let function = function(context, name, *length, *f);
        target.set(JsString::from(*name), function, false, context)?;
    }
    Ok(())
}

// ─── path ───────────────────────────────────────────────────────────────────────────────────

/// A path's parts, either separator read as one.
fn split(path: &str) -> (bool, Vec<&str>) {
    let absolute = path.starts_with('/') || path.starts_with('\\') || path.get(1..2) == Some(":");
    (
        absolute,
        path.split(['/', '\\']).filter(|p| !p.is_empty()).collect(),
    )
}

fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let drive = path
        .get(..2)
        .filter(|d| d.ends_with(':'))
        .map(str::to_owned);
    let rest = if drive.is_some() { &path[2..] } else { path };
    let (absolute, parts) = split(rest);
    let absolute = absolute || drive.is_some();
    let mut out: Vec<&str> = Vec::new();
    for part in parts {
        match part {
            "." => {}
            ".." => {
                if out.last().is_some_and(|p| *p != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let trailing = path.ends_with('/') || path.ends_with('\\');
    let mut joined = out.join("/");
    if trailing && !joined.is_empty() {
        joined.push('/');
    }
    match (drive, absolute) {
        (Some(drive), _) => format!("{drive}/{joined}"),
        (None, true) => format!("/{joined}"),
        (None, false) if joined.is_empty() => ".".into(),
        (None, false) => joined,
    }
}

fn texts(args: &[JsValue], context: &mut Context) -> JsResult<Vec<String>> {
    args.iter().map(|a| text_of(a, context)).collect()
}

fn base_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
}

fn ext_name(path: &str) -> &str {
    let base = base_name(path);
    match base.rfind('.') {
        Some(0) | None => "",
        Some(at) => &base[at..],
    }
}

fn dir_name(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(0) => "/".into(),
        Some(at) => trimmed[..at].to_owned(),
        None if path.starts_with('/') => "/".into(),
        None => ".".into(),
    }
}

const PATH_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("join", 2, |_, args, cx| {
        let parts = texts(args, cx)?;
        let joined: Vec<&str> = parts
            .iter()
            .map(String::as_str)
            .filter(|p| !p.is_empty())
            .collect();
        Ok(js_text(&normalize(&joined.join("/"))))
    }),
    ("resolve", 2, |_, args, cx| {
        // From the last absolute part on, as Node resolves; the working folder is the root.
        let parts = texts(args, cx)?;
        let mut out = String::new();
        for part in parts.iter().rev().filter(|p| !p.is_empty()) {
            out = if out.is_empty() {
                part.clone()
            } else {
                format!("{part}/{out}")
            };
            if split(part).0 {
                break;
            }
        }
        if !split(&out).0 {
            out = format!("/{out}");
        }
        let normalized = normalize(&out);
        let trimmed = normalized.trim_end_matches('/');
        Ok(js_text(if trimmed.is_empty() || trimmed.ends_with(':') {
            &normalized
        } else {
            trimmed
        }))
    }),
    ("normalize", 1, |_, args, cx| {
        Ok(js_text(&normalize(&text_of(&arg(args, 0), cx)?)))
    }),
    ("dirname", 1, |_, args, cx| {
        Ok(js_text(&dir_name(&text_of(&arg(args, 0), cx)?)))
    }),
    ("basename", 2, |_, args, cx| {
        let path = text_of(&arg(args, 0), cx)?;
        let mut base = base_name(&path).to_owned();
        if let Some(ext) = args.get(1).filter(|e| !e.is_undefined()) {
            let ext = text_of(ext, cx)?;
            if base.ends_with(&ext) && base != ext {
                base.truncate(base.len() - ext.len());
            }
        }
        Ok(js_text(&base))
    }),
    ("extname", 1, |_, args, cx| {
        Ok(js_text(ext_name(&text_of(&arg(args, 0), cx)?)))
    }),
    ("isAbsolute", 1, |_, args, cx| {
        Ok(split(&text_of(&arg(args, 0), cx)?).0.into())
    }),
    ("relative", 2, |_, args, cx| {
        let from = normalize(&text_of(&arg(args, 0), cx)?);
        let to = normalize(&text_of(&arg(args, 1), cx)?);
        let (_, from) = split(&from);
        let (_, to) = split(&to);
        let common = from
            .iter()
            .zip(to.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let mut out: Vec<&str> = vec![".."; from.len() - common];
        out.extend(&to[common..]);
        Ok(js_text(&out.join("/")))
    }),
    ("parse", 1, |_, args, cx| {
        let path = text_of(&arg(args, 0), cx)?;
        let base = base_name(&path).to_owned();
        let ext = ext_name(&path).to_owned();
        let out = object(cx);
        out.set(
            js_string!("root"),
            js_text(if split(&path).0 { "/" } else { "" }),
            false,
            cx,
        )?;
        out.set(js_string!("dir"), js_text(&dir_name(&path)), false, cx)?;
        out.set(js_string!("base"), js_text(&base), false, cx)?;
        out.set(js_string!("ext"), js_text(&ext), false, cx)?;
        out.set(
            js_string!("name"),
            js_text(&base[..base.len() - ext.len()]),
            false,
            cx,
        )?;
        Ok(out.into())
    }),
    ("format", 1, |_, args, cx| {
        let parts = arg(args, 0);
        let field = |name: &str, cx: &mut Context| -> JsResult<String> {
            match parts.as_object() {
                Some(p) => {
                    let v = p.get(JsString::from(name), cx)?;
                    if v.is_null_or_undefined() {
                        Ok(String::new())
                    } else {
                        text_of(&v, cx)
                    }
                }
                None => Ok(String::new()),
            }
        };
        let dir = field("dir", cx)?;
        let root = field("root", cx)?;
        let base = match field("base", cx)? {
            b if !b.is_empty() => b,
            _ => format!("{}{}", field("name", cx)?, field("ext", cx)?),
        };
        let dir = if dir.is_empty() {
            root
        } else {
            format!("{dir}/")
        };
        Ok(js_text(&format!("{dir}{base}").replace("//", "/")))
    }),
    ("toNamespacedPath", 1, |_, args, _| Ok(arg(args, 0))),
];

fn path_module(context: &mut Context) -> JsResult<JsValue> {
    let path = object(context);
    with_functions(context, &path, PATH_FUNCTIONS)?;
    path.set(js_string!("sep"), js_string!("/"), false, context)?;
    path.set(
        js_string!("delimiter"),
        js_text(if cfg!(windows) { ";" } else { ":" }),
        false,
        context,
    )?;
    path.set(js_string!("posix"), path.clone(), false, context)?;
    path.set(js_string!("win32"), path.clone(), false, context)?;
    Ok(path.into())
}

// ─── util ───────────────────────────────────────────────────────────────────────────────────

/// `util.format`: `%s`, `%d`, `%i`, `%f`, `%j`, `%o`, `%O` and `%%`, the rest after a space.
fn format_args(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    let Some(first) = args.first() else {
        return Ok(String::new());
    };
    if !first.is_string() {
        let parts: Vec<String> = args.iter().map(crate::print).collect();
        return Ok(parts.join(" "));
    }
    let pattern = text_of(first, context)?;
    let mut rest = args[1..].iter();
    let mut out = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(spec @ ('s' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c')) => {
                chars.next();
                let Some(value) = rest.next() else {
                    out.push('%');
                    out.push(spec);
                    continue;
                };
                let piece = match spec {
                    's' => text_of(value, context)?,
                    'd' | 'i' => {
                        let n = value.to_number(context)?;
                        if spec == 'i' {
                            format!("{}", n.trunc())
                        } else {
                            crate::print(&JsValue::from(n))
                        }
                    }
                    'f' => crate::print(&JsValue::from(value.to_number(context)?)),
                    'j' => {
                        let json = context.intrinsics().objects().json();
                        let stringify = json.get(js_string!("stringify"), context)?;
                        let out = crate::call(
                            context,
                            &stringify,
                            &json.into(),
                            std::slice::from_ref(value),
                        )?;
                        crate::print(&out)
                    }
                    'c' => String::new(),
                    _ => crate::print(value),
                };
                out.push_str(&piece);
            }
            _ => out.push('%'),
        }
    }
    for value in rest {
        out.push(' ');
        out.push_str(&crate::print(value));
    }
    Ok(out)
}

/// `promisify(f)`: a function answering a promise, settled by `f`'s callback.
fn promisified(
    this: &JsValue,
    args: &[JsValue],
    f: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let settle = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, resolvers: &boa_engine::builtins::promise::ResolvingFunctions, cx| {
                let error = arg(args, 0);
                if error.is_null_or_undefined() {
                    resolvers
                        .resolve
                        .call(&JsValue::undefined(), &[arg(args, 1)], cx)?;
                } else {
                    resolvers.reject.call(&JsValue::undefined(), &[error], cx)?;
                }
                Ok(JsValue::undefined())
            },
            resolvers.clone(),
        ),
    )
    .build();
    let mut call_args = args.to_vec();
    call_args.push(settle.into());
    if let Err(error) = crate::call(context, f, this, &call_args) {
        let reason = error.into_opaque(context)?;
        resolvers
            .reject
            .call(&JsValue::undefined(), &[reason], context)?;
    }
    Ok(promise.into())
}

const UTIL_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("format", 1, |_, args, cx| {
        Ok(js_text(&format_args(args, cx)?))
    }),
    ("inspect", 1, |_, args, _| {
        Ok(js_text(&crate::print(&arg(args, 0))))
    }),
    ("promisify", 1, |_, args, cx| {
        let f = arg(args, 0);
        if !f.is_callable() {
            return Err(type_error("promisify: the argument must be a function"));
        }
        Ok(FunctionObjectBuilder::new(
            cx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                |this, args, f: &JsValue, cx| promisified(this, args, f, cx),
                f,
            ),
        )
        .build()
        .into())
    }),
    ("inherits", 2, |_, args, cx| {
        let (child, parent) = (arg(args, 0), arg(args, 1));
        if let (Some(child), Some(parent)) = (child.as_object(), parent.as_object()) {
            let parent_proto = parent.get(js_string!("prototype"), cx)?;
            let child_proto = child.get(js_string!("prototype"), cx)?;
            if let Some(child_proto) = child_proto.as_object() {
                child_proto.set_prototype(parent_proto.as_object());
            }
            child.set(js_string!("super_"), parent.clone(), false, cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("deprecate", 2, |_, args, _| Ok(arg(args, 0))),
    ("isDeepStrictEqual", 2, |_, args, cx| {
        let json = cx.intrinsics().objects().json();
        let stringify = json.get(js_string!("stringify"), cx)?;
        let a = crate::call(cx, &stringify, &json.clone().into(), &[arg(args, 0)])?;
        let b = crate::call(cx, &stringify, &json.into(), &[arg(args, 1)])?;
        Ok((crate::print(&a) == crate::print(&b)).into())
    }),
];

fn util_module(context: &mut Context) -> JsResult<JsValue> {
    let util = object(context);
    with_functions(context, &util, UTIL_FUNCTIONS)?;
    let types = object(context);
    with_functions(
        context,
        &types,
        &[
            ("isPromise", 1, |_, args, _| {
                Ok(arg(args, 0)
                    .as_object()
                    .is_some_and(|o| JsPromise::from_object(o.clone()).is_ok())
                    .into())
            }),
            ("isDate", 1, |_, args, _| {
                Ok(arg(args, 0)
                    .as_object()
                    .is_some_and(|o| {
                        boa_engine::object::builtins::JsDate::from_object(o.clone()).is_ok()
                    })
                    .into())
            }),
            ("isRegExp", 1, |_, args, _| {
                Ok(arg(args, 0)
                    .as_object()
                    .is_some_and(|o| {
                        boa_engine::object::builtins::JsRegExp::from_object(o.clone()).is_ok()
                    })
                    .into())
            }),
        ],
    )?;
    util.set(js_string!("types"), types, false, context)?;
    for name in ["TextEncoder", "TextDecoder"] {
        let class = context.global_object().get(JsString::from(name), context)?;
        util.set(JsString::from(name), class, false, context)?;
    }
    Ok(util.into())
}

// ─── events ─────────────────────────────────────────────────────────────────────────────────

/// The listeners of `name` on an emitter: its `_events[name]`, as Node keeps them.
fn listeners_of(
    emitter: &JsValue,
    name: &JsValue,
    context: &mut Context,
) -> JsResult<Vec<JsValue>> {
    let Some(emitter) = emitter.as_object() else {
        return Ok(Vec::new());
    };
    let events = emitter.get(js_string!("_events"), context)?;
    let Some(events) = events.as_object() else {
        return Ok(Vec::new());
    };
    let key = name.to_property_key(context)?;
    let list = events.get(key, context)?;
    let Some(list) = list.as_object() else {
        return Ok(Vec::new());
    };
    let list = JsArray::from_object(list.clone())?;
    (0..list.length(context)?)
        .map(|at| list.get(at, context))
        .collect()
}

fn set_listeners(
    emitter: &JsValue,
    name: &JsValue,
    list: Vec<JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    let Some(emitter) = emitter.as_object() else {
        return Ok(());
    };
    let mut events = emitter.get(js_string!("_events"), context)?;
    if !events.is_object() {
        events = object(context).into();
        emitter.set(js_string!("_events"), events.clone(), false, context)?;
    }
    let events = events.as_object().expect("an object");
    let key = name.to_property_key(context)?;
    if list.is_empty() {
        events.delete_property_or_throw(key, context)?;
    } else {
        let array = JsArray::from_iter(list, context);
        events.set(key, array, false, context)?;
    }
    Ok(())
}

fn add_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    first: bool,
) -> JsResult<JsValue> {
    let (name, listener) = (arg(args, 0), arg(args, 1));
    if !listener.is_callable() {
        return Err(type_error(
            "The \"listener\" argument must be of type Function",
        ));
    }
    let mut list = listeners_of(this, &name, context)?;
    if first {
        list.insert(0, listener)
    } else {
        list.push(listener)
    }
    set_listeners(this, &name, list, context)?;
    Ok(this.clone())
}

/// A listener that takes itself off before it runs: what `once` adds. It keeps the function it
/// stands for as `listener`, so `off(f)` finds it.
fn once_wrapper(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (name, listener) = (arg(args, 0), arg(args, 1));
    let wrapper: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, args, (name, listener): &(JsValue, JsValue), cx| {
                let list: Vec<JsValue> = listeners_of(this, name, cx)?
                    .into_iter()
                    .filter(|l| {
                        let own = l
                            .as_object()
                            .and_then(|o| o.get(js_string!("listener"), cx).ok());
                        !own.is_some_and(|own| own == *listener)
                    })
                    .collect();
                set_listeners(this, name, list, cx)?;
                crate::call(cx, listener, this, args)
            },
            (name.clone(), listener.clone()),
        ),
    )
    .build()
    .into();
    wrapper.set(js_string!("listener"), listener, false, context)?;
    let _ = this;
    Ok(wrapper.into())
}

const EMITTER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("on", 2, |this, args, cx| {
        add_listener(this, args, cx, false)
    }),
    ("addListener", 2, |this, args, cx| {
        add_listener(this, args, cx, false)
    }),
    ("prependListener", 2, |this, args, cx| {
        add_listener(this, args, cx, true)
    }),
    ("once", 2, |this, args, cx| {
        let wrapper = once_wrapper(this, args, cx)?;
        add_listener(this, &[arg(args, 0), wrapper], cx, false)
    }),
    ("prependOnceListener", 2, |this, args, cx| {
        let wrapper = once_wrapper(this, args, cx)?;
        add_listener(this, &[arg(args, 0), wrapper], cx, true)
    }),
    ("off", 2, remove_listener),
    ("removeListener", 2, remove_listener),
    ("removeAllListeners", 1, |this, args, cx| {
        let name = arg(args, 0);
        if name.is_undefined() {
            if let Some(emitter) = this.as_object() {
                let fresh = object(cx);
                emitter.set(js_string!("_events"), fresh, false, cx)?;
            }
        } else {
            set_listeners(this, &name, Vec::new(), cx)?;
        }
        Ok(this.clone())
    }),
    ("emit", 1, |this, args, cx| {
        let name = arg(args, 0);
        let list = listeners_of(this, &name, cx)?;
        if list.is_empty() {
            // An `error` nobody listens for is thrown, as Node throws it.
            if name
                .as_string()
                .is_some_and(|n| n.to_std_string_escaped() == "error")
            {
                return Err(JsError::from_opaque(arg(args, 1)));
            }
            return Ok(false.into());
        }
        for listener in list {
            crate::call(cx, &listener, this, &args[1.min(args.len())..])?;
        }
        Ok(true.into())
    }),
    ("listeners", 1, |this, args, cx| {
        let list = listeners_of(this, &arg(args, 0), cx)?;
        Ok(JsArray::from_iter(list, cx).into())
    }),
    ("rawListeners", 1, |this, args, cx| {
        let list = listeners_of(this, &arg(args, 0), cx)?;
        Ok(JsArray::from_iter(list, cx).into())
    }),
    ("listenerCount", 1, |this, args, cx| {
        Ok(listeners_of(this, &arg(args, 0), cx)?.len().into())
    }),
    ("eventNames", 0, |this, _, cx| {
        let Some(emitter) = this.as_object() else {
            return Ok(JsArray::new(cx)?.into());
        };
        let events = emitter.get(js_string!("_events"), cx)?;
        let names: Vec<JsValue> = match events.as_object() {
            Some(events) => events
                .own_property_keys(cx)?
                .into_iter()
                .map(|k| js_text(&k.to_string()))
                .collect(),
            None => Vec::new(),
        };
        Ok(JsArray::from_iter(names, cx).into())
    }),
    ("setMaxListeners", 1, |this, _, _| Ok(this.clone())),
    ("getMaxListeners", 0, |_, _, _| Ok(10.into())),
];

fn remove_listener(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (name, listener) = (arg(args, 0), arg(args, 1));
    let mut list = listeners_of(this, &name, context)?;
    let at = list.iter().rposition(|l| {
        *l == listener
            || l.as_object()
                .and_then(|o| o.get(js_string!("listener"), context).ok())
                .is_some_and(|own| own == listener)
    });
    if let Some(at) = at {
        list.remove(at);
        set_listeners(this, &name, list, context)?;
    }
    Ok(this.clone())
}

fn events_module(context: &mut Context) -> JsResult<JsValue> {
    // Constructed — `new EventEmitter()`, `super()` — or called on an object as
    // `EventEmitter.call(this)` by older code: see `crate::native`.
    let (emitter, _) = crate::native::class(
        context,
        crate::native::ClassSpec {
            name: "EventEmitter",
            init: |target, _, cx| {
                if !target.has_own_property(js_string!("_events"), cx)? {
                    let events = object(cx);
                    target.set(js_string!("_events"), events, false, cx)?;
                }
                Ok(())
            },
            methods: EMITTER_METHODS,
            ..Default::default()
        },
    )?;
    emitter.set(js_string!("EventEmitter"), emitter.clone(), false, context)?;
    emitter.set(js_string!("defaultMaxListeners"), 10, false, context)?;
    Ok(emitter.into())
}

// ─── os ─────────────────────────────────────────────────────────────────────────────────────

fn home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".into())
}

const OS_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("platform", 0, |_, _, _| Ok(js_text(platform()))),
    ("type", 0, |_, _, _| {
        Ok(js_text(match std::env::consts::OS {
            "windows" => "Windows_NT",
            "macos" => "Darwin",
            _ => "Linux",
        }))
    }),
    ("arch", 0, |_, _, _| {
        Ok(js_text(match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => other,
        }))
    }),
    ("homedir", 0, |_, _, _| Ok(js_text(&home()))),
    ("tmpdir", 0, |_, _, _| {
        Ok(js_text(&std::env::temp_dir().to_string_lossy()))
    }),
    ("hostname", 0, |_, _, _| {
        Ok(js_text(
            &std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "localhost".into()),
        ))
    }),
    ("release", 0, |_, _, _| Ok(js_string!("").into())),
    ("cpus", 0, |_, _, cx| Ok(JsArray::new(cx)?.into())),
    ("totalmem", 0, |_, _, _| Ok(0.into())),
    ("freemem", 0, |_, _, _| Ok(0.into())),
    ("uptime", 0, |_, _, _| Ok(0.into())),
    ("userInfo", 0, |_, _, cx| {
        let info = object(cx);
        info.set(js_string!("homedir"), js_text(&home()), false, cx)?;
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_default();
        info.set(js_string!("username"), js_text(&user), false, cx)?;
        Ok(info.into())
    }),
];

pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

fn os_module(context: &mut Context) -> JsResult<JsValue> {
    let os = object(context);
    with_functions(context, &os, OS_FUNCTIONS)?;
    os.set(
        js_string!("EOL"),
        js_text(if cfg!(windows) { "\r\n" } else { "\n" }),
        false,
        context,
    )?;
    Ok(os.into())
}

// ─── fs ─────────────────────────────────────────────────────────────────────────────────────

fn io_error(error: std::io::Error, path: &str, call: &str) -> JsError {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        _ => "EIO",
    };
    JsNativeError::error()
        .with_message(format!("{code}: {error}, {call} '{path}'"))
        .into()
}

fn with_code(error: JsError, context: &mut Context) -> JsValue {
    let message = error.to_string();
    let value = error.into_opaque(context).unwrap_or_default();
    if let Some(object) = value.as_object() {
        let code = message
            .split(':')
            .next()
            .unwrap_or("")
            .trim_start_matches("Error")
            .trim()
            .to_owned();
        let _ = object.set(js_string!("code"), js_text(&code), false, context);
    }
    value
}

fn path_arg(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    let value = arg(args, 0);
    // A `file:` URL, as Node takes one.
    let text = text_of(&value, context)?;
    Ok(text
        .strip_prefix("file:///")
        .map(str::to_owned)
        .unwrap_or(text))
}

fn encoding_of(value: &JsValue, context: &mut Context) -> JsResult<Option<String>> {
    if value.is_string() {
        return Ok(Some(text_of(value, context)?));
    }
    if let Some(options) = value.as_object() {
        let encoding = options.get(js_string!("encoding"), context)?;
        if !encoding.is_null_or_undefined() {
            return Ok(Some(text_of(&encoding, context)?));
        }
    }
    Ok(None)
}

fn read_file(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let bytes = std::fs::read(&path).map_err(|e| io_error(e, &path, "open"))?;
    match encoding_of(&arg(args, 1), context)? {
        Some(encoding) => {
            let buffer = crate::encoding::make_buffer(context, bytes)?;
            let to_string = buffer
                .as_object()
                .expect("a buffer")
                .get(js_string!("toString"), context)?;
            crate::call(context, &to_string, &buffer, &[js_text(&encoding)])
        }
        None => crate::encoding::make_buffer(context, bytes),
    }
}

fn write_file(args: &[JsValue], context: &mut Context, append: bool) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let data = arg(args, 1);
    let bytes = match crate::encoding::bytes_of(&data, context)? {
        Some(bytes) => bytes,
        None => text_of(&data, context)?.into_bytes(),
    };
    let result = if append {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(&bytes))
    } else {
        std::fs::write(&path, bytes)
    };
    result.map_err(|e| io_error(e, &path, "open"))?;
    Ok(JsValue::undefined())
}

fn stat(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let meta = std::fs::metadata(&path).map_err(|e| io_error(e, &path, "stat"))?;
    let ms = |t: std::io::Result<std::time::SystemTime>| {
        t.ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0., |d| d.as_secs_f64() * 1000.)
    };
    let out = object(context);
    out.set(js_string!("size"), meta.len() as f64, false, context)?;
    out.set(js_string!("mtimeMs"), ms(meta.modified()), false, context)?;
    out.set(js_string!("ctimeMs"), ms(meta.created()), false, context)?;
    out.set(
        js_string!("birthtimeMs"),
        ms(meta.created()),
        false,
        context,
    )?;
    for (key, value) in [
        ("mtime", ms(meta.modified())),
        ("ctime", ms(meta.created())),
        ("birthtime", ms(meta.created())),
    ] {
        let date = boa_engine::object::builtins::JsDate::new(context);
        date.set_time(value, context)?;
        out.set(JsString::from(key), date, false, context)?;
    }
    let is_file = meta.is_file();
    let is_dir = meta.is_dir();
    let file = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |_, _, _| Ok(is_file.into())),
    )
    .build();
    let dir = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |_, _, _| Ok(is_dir.into())),
    )
    .build();
    out.set(js_string!("isFile"), file, false, context)?;
    out.set(js_string!("isDirectory"), dir, false, context)?;
    let no = function(context, "isSymbolicLink", 0, |_, _, _| Ok(false.into()));
    out.set(js_string!("isSymbolicLink"), no, false, context)?;
    Ok(out.into())
}

fn read_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let mut names: Vec<String> = std::fs::read_dir(&path)
        .map_err(|e| io_error(e, &path, "scandir"))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(JsArray::from_iter(names.iter().map(|n| js_text(n)), context).into())
}

fn make_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let recursive = arg(args, 1)
        .as_object()
        .map(|o| o.get(js_string!("recursive"), context))
        .transpose()?
        .is_some_and(|r| r.to_boolean());
    let result = if recursive {
        std::fs::create_dir_all(&path)
    } else {
        std::fs::create_dir(&path)
    };
    result.map_err(|e| io_error(e, &path, "mkdir"))?;
    Ok(JsValue::undefined())
}

fn remove(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, context)?;
    let options = arg(args, 1);
    let flag = |name: &str, cx: &mut Context| -> JsResult<bool> {
        Ok(options
            .as_object()
            .map(|o| o.get(JsString::from(name), cx))
            .transpose()?
            .is_some_and(|v| v.to_boolean()))
    };
    let (recursive, force) = (flag("recursive", context)?, flag("force", context)?);
    let target = std::path::Path::new(&path);
    let result = if target.is_dir() {
        if recursive {
            std::fs::remove_dir_all(target)
        } else {
            std::fs::remove_dir(target)
        }
    } else {
        std::fs::remove_file(target)
    };
    match result {
        Err(e) if !(force && e.kind() == std::io::ErrorKind::NotFound) => {
            Err(io_error(e, &path, "rm"))
        }
        _ => Ok(JsValue::undefined()),
    }
}

/// The operations, each as `fs` has it synchronously; the promise and callback forms are made
/// from these.
const FS_OPERATIONS: &[(&str, NativeFunctionPointer)] = &[
    ("readFile", |_, args, cx| read_file(args, cx)),
    ("writeFile", |_, args, cx| write_file(args, cx, false)),
    ("appendFile", |_, args, cx| write_file(args, cx, true)),
    ("stat", |_, args, cx| stat(args, cx)),
    ("lstat", |_, args, cx| stat(args, cx)),
    ("readdir", |_, args, cx| read_dir(args, cx)),
    ("mkdir", |_, args, cx| make_dir(args, cx)),
    ("rm", |_, args, cx| remove(args, cx)),
    ("rmdir", |_, args, cx| remove(args, cx)),
    ("unlink", |_, args, cx| remove(args, cx)),
    ("rename", |_, args, cx| {
        let from = path_arg(args, cx)?;
        let to = path_arg(&args[1.min(args.len())..], cx)?;
        std::fs::rename(&from, &to).map_err(|e| io_error(e, &from, "rename"))?;
        Ok(JsValue::undefined())
    }),
    ("copyFile", |_, args, cx| {
        let from = path_arg(args, cx)?;
        let to = path_arg(&args[1.min(args.len())..], cx)?;
        std::fs::copy(&from, &to).map_err(|e| io_error(e, &from, "copyfile"))?;
        Ok(JsValue::undefined())
    }),
    ("access", |_, args, cx| {
        let path = path_arg(args, cx)?;
        std::fs::metadata(&path).map_err(|e| io_error(e, &path, "access"))?;
        Ok(JsValue::undefined())
    }),
    ("realpath", |_, args, cx| {
        let path = path_arg(args, cx)?;
        let real = std::fs::canonicalize(&path).map_err(|e| io_error(e, &path, "realpath"))?;
        Ok(js_text(&real.to_string_lossy()))
    }),
];

fn fs_module(context: &mut Context) -> JsResult<JsValue> {
    let fs = object(context);
    let promises = object(context);
    for (name, operation) in FS_OPERATIONS {
        let op: JsValue = function(context, name, 2, *operation).into();
        // `fs.readFileSync`.
        fs.set(
            JsString::from(format!("{name}Sync")),
            op.clone(),
            false,
            context,
        )?;
        // `fs.promises.readFile`, `require('fs/promises').readFile`.
        let promised = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure_with_captures(
                |this, args, op: &JsValue, cx| match crate::call(cx, op, this, args) {
                    Ok(value) => Ok(JsPromise::resolve(value, cx)?.into()),
                    Err(error) => {
                        let reason = with_code(error, cx);
                        Ok(JsPromise::reject(JsError::from_opaque(reason), cx)?.into())
                    }
                },
                op.clone(),
            ),
        )
        .name(JsString::from(*name))
        .build();
        promises.set(JsString::from(*name), promised, false, context)?;
        // `fs.readFile(path, …, callback)`: the callback called once this turn is over.
        let with_callback = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure_with_captures(
                |this, args, op: &JsValue, cx| {
                    let (callback, rest) = match args.last() {
                        Some(last) if last.is_callable() => (last.clone(), &args[..args.len() - 1]),
                        _ => return crate::call(cx, op, this, args),
                    };
                    let outcome = crate::call(cx, op, this, rest);
                    let callback_args = match outcome {
                        Ok(value) => vec![JsValue::null(), value],
                        Err(error) => vec![with_code(error, cx)],
                    };
                    crate::microtask(cx, callback, callback_args);
                    Ok(JsValue::undefined())
                },
                op,
            ),
        )
        .name(JsString::from(*name))
        .build();
        fs.set(JsString::from(*name), with_callback, false, context)?;
    }
    let exists = function(context, "existsSync", 1, |_, args, cx| {
        let path = path_arg(args, cx)?;
        Ok(std::path::Path::new(&path).exists().into())
    });
    fs.set(js_string!("existsSync"), exists, false, context)?;
    fs.set(js_string!("promises"), promises.clone(), false, context)?;
    let constants = object(context);
    for (name, value) in [("F_OK", 0), ("R_OK", 4), ("W_OK", 2), ("X_OK", 1)] {
        constants.set(JsString::from(name), value, false, context)?;
    }
    fs.set(js_string!("constants"), constants, false, context)?;
    Ok(fs.into())
}

// ─── child_process ──────────────────────────────────────────────────────────────────────────

/// Every way to start a program fails, as one that could not be found does: its callback hears
/// the error this turn, and what it answers emits `error`.
fn refused(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let error = JsNativeError::error()
        .with_message("child_process is not available in Su: plugins cannot start programs")
        .into_opaque(context);
    error.set(js_string!("code"), js_string!("ENOSYS"), false, context)?;
    if let Some(callback) = args.iter().rev().find(|a| a.is_callable()) {
        crate::microtask(context, callback.clone(), vec![error.into()]);
    }
    let events = emitter_class(context)?;
    let child = events.construct(&[], None, context)?;
    for stream in ["stdout", "stderr"] {
        let emitter = events.construct(&[], None, context)?;
        child.set(JsString::from(stream), emitter, false, context)?;
    }
    Ok(child.into())
}

fn child_process_module(context: &mut Context) -> JsResult<JsValue> {
    let module = object(context);
    for name in [
        "exec",
        "execFile",
        "spawn",
        "fork",
        "execSync",
        "execFileSync",
        "spawnSync",
    ] {
        let f = function(context, name, 3, refused);
        module.set(JsString::from(name), f, false, context)?;
    }
    Ok(module.into())
}

// ─── http, https ────────────────────────────────────────────────────────────────────────────

/// An `EventEmitter` that hears `error` a microtask from now: what a server that cannot
/// listen, or a request that cannot be sent, does. Plugins make their requests through
/// `requestUrl`; serving and Node's own client are not something this runtime does.
fn failing_emitter(context: &mut Context, message: &str, code: &str) -> JsResult<JsObject> {
    let emitter = emitter_class(context)?.construct(&[], None, context)?;
    let error = JsNativeError::error()
        .with_message(message.to_owned())
        .into_opaque(context);
    error.set(js_string!("code"), JsString::from(code), false, context)?;
    let fire = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, _, (emitter, error): &(JsObject, JsValue), cx| {
                // Heard only by one listening: an `error` nobody hears is not thrown here.
                let count = emitter.get(js_string!("listenerCount"), cx)?;
                let listening = crate::call(
                    cx,
                    &count,
                    &emitter.clone().into(),
                    &[js_string!("error").into()],
                )?;
                if listening.to_number(cx)? > 0. {
                    let emit = emitter.get(js_string!("emit"), cx)?;
                    crate::call(
                        cx,
                        &emit,
                        &emitter.clone().into(),
                        &[js_string!("error").into(), error.clone()],
                    )?;
                }
                Ok(JsValue::undefined())
            },
            (emitter.clone(), error.into()),
        ),
    )
    .build();
    crate::microtask(context, fire.into(), Vec::new());
    Ok(emitter)
}

fn http_module(context: &mut Context) -> JsResult<JsValue> {
    let module = object(context);
    let create_server = function(context, "createServer", 2, |_, _, cx| {
        let server = emitter_class(cx)?.construct(&[], None, cx)?;
        let listen = function(cx, "listen", 3, |this, _, cx| {
            if let Some(server) = this.as_object() {
                let failing =
                    failing_emitter(cx, "Su does not let plugins serve on a port", "EACCES")?;
                // The server's own listeners hear it.
                let fire = FunctionObjectBuilder::new(
                    cx.realm(),
                    NativeFunction::from_copy_closure_with_captures(
                        |_, args, (server,): &(JsObject,), cx| {
                            let emit = server.get(js_string!("emit"), cx)?;
                            let error = args.first().cloned().unwrap_or_default();
                            crate::call(
                                cx,
                                &emit,
                                &server.clone().into(),
                                &[js_string!("error").into(), error],
                            )?;
                            Ok(JsValue::undefined())
                        },
                        (server.clone(),),
                    ),
                )
                .build();
                let on = failing.get(js_string!("on"), cx)?;
                crate::call(
                    cx,
                    &on,
                    &failing.clone().into(),
                    &[js_string!("error").into(), fire.into()],
                )?;
            }
            Ok(this.clone())
        });
        server.set(js_string!("listen"), listen, false, cx)?;
        let close = function(cx, "close", 1, |this, args, cx| {
            if let Some(callback) = args.iter().find(|a| a.is_callable()) {
                crate::microtask(cx, callback.clone(), Vec::new());
            }
            Ok(this.clone())
        });
        server.set(js_string!("close"), close, false, cx)?;
        server.set(js_string!("listening"), false, false, cx)?;
        Ok(server.into())
    });
    module.set(js_string!("createServer"), create_server, false, context)?;
    for name in ["request", "get"] {
        let f = function(context, name, 3, |_, _, cx| {
            Ok(failing_emitter(
                cx,
                "Node's http client is not available in Su: use requestUrl",
                "ENOSYS",
            )?
            .into())
        });
        module.set(JsString::from(name), f, false, context)?;
    }
    let methods: Vec<JsValue> = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
        .iter()
        .map(|m| js_text(m))
        .collect();
    let methods = JsArray::from_iter(methods, context);
    module.set(js_string!("METHODS"), methods, false, context)?;
    let codes = object(context);
    for (code, text) in [
        (200, "OK"),
        (201, "Created"),
        (204, "No Content"),
        (301, "Moved Permanently"),
        (302, "Found"),
        (304, "Not Modified"),
        (400, "Bad Request"),
        (401, "Unauthorized"),
        (403, "Forbidden"),
        (404, "Not Found"),
        (405, "Method Not Allowed"),
        (500, "Internal Server Error"),
        (502, "Bad Gateway"),
        (503, "Service Unavailable"),
    ] {
        codes.set(
            JsString::from(code.to_string()),
            js_text(text),
            false,
            context,
        )?;
    }
    module.set(js_string!("STATUS_CODES"), codes, false, context)?;
    Ok(module.into())
}

// ─── url ────────────────────────────────────────────────────────────────────────────────────

fn url_module(context: &mut Context) -> JsResult<JsValue> {
    let module = object(context);
    for name in ["URL", "URLSearchParams"] {
        let class = context.global_object().get(JsString::from(name), context)?;
        module.set(JsString::from(name), class, false, context)?;
    }
    with_functions(
        context,
        &module,
        &[
            ("fileURLToPath", 1, |_, args, cx| {
                let text = text_of(&arg(args, 0), cx)?;
                let path = text.strip_prefix("file://").unwrap_or(&text);
                let path = if cfg!(windows) {
                    path.trim_start_matches('/')
                } else {
                    path
                };
                Ok(js_text(path))
            }),
            ("pathToFileURL", 1, |_, args, cx| {
                let path = text_of(&arg(args, 0), cx)?.replace('\\', "/");
                let href = if path.starts_with('/') {
                    format!("file://{path}")
                } else {
                    format!("file:///{path}")
                };
                let class = cx.global_object().get(js_string!("URL"), cx)?;
                let class = class
                    .as_object()
                    .ok_or_else(|| type_error("URL is not installed"))?;
                Ok(class.construct(&[js_text(&href)], None, cx)?.into())
            }),
        ],
    )?;
    Ok(module.into())
}

// ─── Installing them ────────────────────────────────────────────────────────────────────────

/// Node's modules, by the names `require` answers them to — each made the first time it is
/// required, and kept.
pub(crate) const MODULES: &[(&str, crate::ModuleMaker)] = &[
    ("path", |cx, _| path_module(cx)),
    ("util", |cx, _| util_module(cx)),
    ("events", |cx, _| events_module(cx)),
    ("os", |cx, _| os_module(cx)),
    ("url", |cx, _| url_module(cx)),
    ("fs", |cx, _| fs_module(cx)),
    ("fs/promises", |cx, _| {
        let fs = crate::modules::node_module(cx, "fs")?;
        crate::native::get(&fs, "promises", cx)
    }),
    ("child_process", |cx, _| child_process_module(cx)),
    ("http", |cx, _| http_module(cx)),
    ("https", |cx, _| http_module(cx)),
    ("module", crate::modules::module_module),
    ("process", crate::process::module),
];

/// The `EventEmitter` class, made once for the realm.
fn emitter_class(context: &mut Context) -> JsResult<JsObject> {
    crate::modules::node_module(context, "events")?
        .as_object()
        .ok_or_else(|| type_error("EventEmitter is not a class"))
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn paths_normalize_as_node_does() {
        assert_eq!(normalize("a/b/../c/./d"), "a/c/d");
        assert_eq!(normalize("/a//b/"), "/a/b/");
        assert_eq!(normalize("../x"), "../x");
        assert_eq!(normalize("C:\\Users\\me\\..\\vault"), "C:/Users/vault");
        assert_eq!(normalize(""), ".");
    }
}
