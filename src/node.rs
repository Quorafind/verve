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

/// `process.platform` and `os.platform()`: Node's name for the system.
pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

// ─── Installing them ────────────────────────────────────────────────────────────────────────

/// Node's modules, by the names `require` answers them to — each made the first time it is
/// required, and kept.
pub(crate) const MODULES: &[(&str, crate::ModuleMaker)] = &[
    ("path", crate::path::module),
    ("path/posix", crate::path::posix_module),
    ("path/win32", crate::path::win32_module),
    ("util", |cx, _| util_module(cx)),
    ("events", |cx, _| events_module(cx)),
    ("os", crate::os::module),
    ("constants", crate::os::constants_module),
    ("url", crate::node_url::module),
    ("querystring", crate::querystring::querystring_module),
    ("punycode", crate::querystring::punycode_module),
    ("fs", crate::fs::module),
    ("fs/promises", crate::fs::promises_module),
    ("child_process", crate::sockets::child_process_module),
    ("http", crate::sockets::http_module),
    ("https", crate::sockets::https_module),
    ("http2", crate::sockets::http2_module),
    ("net", crate::sockets::net_module),
    ("tls", crate::sockets::tls_module),
    ("dns", crate::sockets::dns_module),
    ("dns/promises", crate::sockets::dns_promises_module),
    ("dgram", crate::sockets::dgram_module),
    ("cluster", crate::sockets::cluster_module),
    ("module", crate::modules::module_module),
    ("timers", crate::timers::module),
    ("async_hooks", crate::hooks::async_hooks_module),
    (
        "diagnostics_channel",
        crate::hooks::diagnostics_channel_module,
    ),
    ("perf_hooks", crate::hooks::perf_hooks_module),
    ("inspector", crate::hooks::inspector_module),
    ("inspector/promises", crate::hooks::inspector_module),
    ("trace_events", crate::hooks::trace_events_module),
    ("timers/promises", crate::timers::promises_module),
    ("vm", crate::vm::vm_module),
    ("v8", crate::vm::v8_module),
    ("worker_threads", crate::vm::worker_threads_module),
    ("wasi", crate::vm::wasi_module),
    ("readline", crate::readline::readline_module),
    (
        "readline/promises",
        crate::readline::readline_promises_module,
    ),
    ("tty", crate::readline::tty_module),
    ("repl", crate::readline::repl_module),
    ("console", crate::readline::console_module),
    ("process", crate::process::module),
];
