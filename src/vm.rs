//! `vm`, `v8`, `worker_threads` and `wasi`: Node's modules about the engine and its threads.
//!
//! `vm.runInThisContext` evaluates code in the realm's global scope, as Node's does. Node runs
//! `runInNewContext` and `runInContext` in a realm of their own; here they run in this one,
//! with the context object in front of the global scope: a name the code reads is the context
//! object's when it has it, and the realm's global otherwise; a name it assigns, and a `var` it
//! declares at the top, land on the context object; `this` is the context object. What the
//! code reaches through the realm's globals is the realm's own — this is scoping, not a sandbox
//! — and a function it declares at the top is not put on the context object.
//!
//! `v8` answers plausible heap figures, and `serialize`/`deserialize` as JSON in a `Buffer`.
//! `worker_threads` says this is the main thread; its `Worker` cannot start (it emits `error`
//! and `exit` to whoever listens) — the web's `Worker` global is the one that works here.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, Source, js_string,
    native_function::NativeFunctionPointer,
};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error, coded_error_value, exports, get, hidden,
    invoke, js_text, object, settled, text_of,
};

// ─── vm ─────────────────────────────────────────────────────────────────────────────────────

/// The file name evaluated code is said to come from: the `filename` option, or the string
/// itself when the options are one.
fn filename_of(options: &JsValue, context: &mut Context) -> JsResult<String> {
    if options.is_string() {
        return text_of(options, context);
    }
    let name = get(options, "filename", context)?;
    Ok(if name.is_undefined() {
        "evalmachine.<anonymous>".into()
    } else {
        text_of(&name, context)?
    })
}

fn run_in_this_context(code: &str, filename: &str, context: &mut Context) -> JsResult<JsValue> {
    context.eval(Source::from_bytes(code).with_path(std::path::Path::new(filename)))
}

/// The names the wrapper below uses for itself, which the context object never answers for.
const OWN_NAMES: [&str; 2] = ["__vm_scope", "__vm_code"];

/// The context object as a scope: it has every name, gives the realm's global for one it lacks,
/// and takes every assignment.
fn scope_has(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let key = arg(args, 1);
    let own = key
        .as_string()
        .is_some_and(|k| OWN_NAMES.contains(&k.to_std_string_escaped().as_str()));
    Ok((!own).into())
}

fn scope_get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let key = arg(args, 1).to_property_key(context)?;
    let Some(target) = target.as_object() else {
        return Ok(JsValue::undefined());
    };
    if target.has_property(key.clone(), context)? {
        return target.get(key, context);
    }
    if let boa_engine::property::PropertyKey::Symbol(symbol) = &key
        && *symbol == boa_engine::JsSymbol::unscopables()
    {
        return Ok(JsValue::undefined());
    }
    context.global_object().get(key, context)
}

fn scope_set(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let key = arg(args, 1).to_property_key(context)?;
    if let Some(target) = target.as_object() {
        target.set(key, arg(args, 2), false, context)?;
    }
    Ok(true.into())
}

/// Runs `code` with `sandbox` in front of the global scope (see the module's comment). The
/// scope is a `with` over a proxy of the sandbox, in a function made once for the realm.
fn run_in_sandbox(
    code: &str,
    sandbox: &JsObject,
    _filename: &str,
    context: &mut Context,
) -> JsResult<JsValue> {
    let scope: JsObject = boa_engine::object::builtins::JsProxy::builder(sandbox.clone())
        .has(scope_has)
        .get(scope_get)
        .set(scope_set)
        .build(context)?
        .into();
    let runner = match context.get_data::<SandboxRunner>() {
        Some(runner) => runner.0.clone(),
        None => {
            let runner = context.eval(Source::from_bytes(
                "(function (__vm_scope, __vm_code) { with (__vm_scope) { return eval(__vm_code); } })",
            ))?;
            context.insert_data(SandboxRunner(runner.clone()));
            runner
        }
    };
    crate::call(
        context,
        &runner,
        &sandbox.clone().into(),
        &[scope.into(), js_text(code)],
    )
}

/// The function `run_in_sandbox` evaluates in, kept for the realm.
struct SandboxRunner(JsValue);

/// The context object for `runInContext`/`runInNewContext`: the one given, or a fresh one.
fn contextify(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let target = match value.as_object() {
        Some(target) => target,
        None => object(context),
    };
    hidden(&target, js_string!("__vmContext"), true, context)?;
    Ok(target)
}

fn require_context(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    match value.as_object() {
        Some(target) if target.get(js_string!("__vmContext"), context)?.to_boolean() => Ok(target),
        _ => Err(coded_error(
            context,
            "The \"contextifiedObject\" argument must be an vm.Context",
            "ERR_INVALID_ARG_TYPE",
        )),
    }
}

/// Checks code parses — what `new vm.Script(code)` does before it is run.
fn check_syntax(code: &str, filename: &str, context: &mut Context) -> JsResult<()> {
    boa_engine::Script::parse(
        Source::from_bytes(code).with_path(std::path::Path::new(filename)),
        None,
        context,
    )?;
    Ok(())
}

fn script_code(this: &JsValue, context: &mut Context) -> JsResult<(String, String)> {
    let code = text_of(&get(this, "__code", context)?, context)?;
    let filename = text_of(&get(this, "__filename", context)?, context)?;
    Ok((code, filename))
}

const SCRIPT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("runInThisContext", 1, |this, _, cx| {
        let (code, filename) = script_code(this, cx)?;
        run_in_this_context(&code, &filename, cx)
    }),
    ("runInContext", 2, |this, args, cx| {
        let (code, filename) = script_code(this, cx)?;
        let sandbox = require_context(&arg(args, 0), cx)?;
        run_in_sandbox(&code, &sandbox, &filename, cx)
    }),
    ("runInNewContext", 2, |this, args, cx| {
        let (code, filename) = script_code(this, cx)?;
        let sandbox = contextify(&arg(args, 0), cx)?;
        run_in_sandbox(&code, &sandbox, &filename, cx)
    }),
    ("createCachedData", 0, |_, _, cx| {
        crate::encoding::make_buffer(cx, Vec::new())
    }),
];

const VM_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("runInThisContext", 2, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let filename = filename_of(&arg(args, 1), cx)?;
        run_in_this_context(&code, &filename, cx)
    }),
    ("runInContext", 3, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let sandbox = require_context(&arg(args, 1), cx)?;
        let filename = filename_of(&arg(args, 2), cx)?;
        run_in_sandbox(&code, &sandbox, &filename, cx)
    }),
    ("runInNewContext", 3, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let sandbox = contextify(&arg(args, 1), cx)?;
        let filename = filename_of(&arg(args, 2), cx)?;
        run_in_sandbox(&code, &sandbox, &filename, cx)
    }),
    ("createContext", 2, |_, args, cx| {
        Ok(contextify(&arg(args, 0), cx)?.into())
    }),
    ("isContext", 1, |_, args, cx| {
        Ok(match arg(args, 0).as_object() {
            Some(target) => target.get(js_string!("__vmContext"), cx)?.to_boolean(),
            None => false,
        }
        .into())
    }),
    ("compileFunction", 3, |_, args, cx| {
        // `new Function(...params, code)`.
        let mut parts: Vec<JsValue> = crate::native::list_of(&arg(args, 1), cx)?;
        parts.push(arg(args, 0));
        let function = cx.intrinsics().constructors().function().constructor();
        Ok(function.construct(&parts, None, cx)?.into())
    }),
    ("measureMemory", 1, |_, _, cx| {
        let result = object(cx);
        let total = object(cx);
        total.set(js_string!("jsMemoryEstimate"), 50_000_000, false, cx)?;
        let range = array(cx, vec![40_000_000.into(), 60_000_000.into()]);
        total.set(js_string!("jsMemoryRange"), range, false, cx)?;
        result.set(js_string!("total"), total, false, cx)?;
        settled(Ok(result.into()), cx)
    }),
];

pub(crate) fn vm_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, VM_FUNCTIONS)?;
    let (script, _) = class(
        context,
        ClassSpec {
            name: "Script",
            length: 2,
            init: |this, args, cx| {
                let code = text_of(&arg(args, 0), cx)?;
                let filename = filename_of(&arg(args, 1), cx)?;
                check_syntax(&code, &filename, cx)?;
                hidden(this, js_string!("__code"), js_text(&code), cx)?;
                hidden(this, js_string!("__filename"), js_text(&filename), cx)?;
                this.set(js_string!("cachedDataRejected"), false, false, cx)?;
                this.set(js_string!("sourceMapURL"), JsValue::undefined(), false, cx)?;
                Ok(())
            },
            methods: SCRIPT_METHODS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Script"), script, false, context)?;
    let constants = object(context);
    for name in ["USE_MAIN_CONTEXT_DEFAULT_LOADER", "DONT_CONTEXTIFY"] {
        let symbol = boa_engine::JsSymbol::new(Some(JsString::from(name)))
            .map_or(JsValue::undefined(), JsValue::from);
        constants.set(JsString::from(name), symbol, false, context)?;
    }
    module.set(js_string!("constants"), constants, false, context)?;
    Ok(module.into())
}

// ─── v8 ─────────────────────────────────────────────────────────────────────────────────────

const V8_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("getHeapStatistics", 0, |_, _, cx| {
        let stats = object(cx);
        for (key, value) in [
            ("total_heap_size", 100_000_000.),
            ("total_heap_size_executable", 1_000_000.),
            ("total_physical_size", 100_000_000.),
            ("total_available_size", 4_000_000_000.),
            ("used_heap_size", 50_000_000.),
            ("heap_size_limit", 4_345_298_944.),
            ("malloced_memory", 1_000_000.),
            ("peak_malloced_memory", 2_000_000.),
            ("does_zap_garbage", 0.),
            ("number_of_native_contexts", 1.),
            ("number_of_detached_contexts", 0.),
            ("total_global_handles_size", 8192.),
            ("used_global_handles_size", 4096.),
            ("external_memory", 1_000_000.),
        ] {
            stats.set(JsString::from(key), value, false, cx)?;
        }
        Ok(stats.into())
    }),
    ("getHeapSpaceStatistics", 0, |_, _, cx| {
        Ok(array(cx, Vec::new()))
    }),
    ("getHeapCodeStatistics", 0, |_, _, cx| {
        let stats = object(cx);
        for key in [
            "code_and_metadata_size",
            "bytecode_and_metadata_size",
            "external_script_source_size",
            "cpu_profiler_metadata_size",
        ] {
            stats.set(JsString::from(key), 0, false, cx)?;
        }
        Ok(stats.into())
    }),
    ("cachedDataVersionTag", 0, |_, _, _| {
        Ok(3_725_769_092u32.into())
    }),
    ("setFlagsFromString", 1, |_, _, _| Ok(JsValue::undefined())),
    ("setHeapSnapshotNearHeapLimit", 1, |_, _, _| {
        Ok(JsValue::undefined())
    }),
    ("writeHeapSnapshot", 1, |_, _, cx| {
        Err(coded_error(
            cx,
            "v8.writeHeapSnapshot: heap snapshots are not available",
            "ERR_METHOD_NOT_IMPLEMENTED",
        ))
    }),
    ("getHeapSnapshot", 0, |_, _, cx| {
        Err(coded_error(
            cx,
            "v8.getHeapSnapshot: heap snapshots are not available",
            "ERR_METHOD_NOT_IMPLEMENTED",
        ))
    }),
    ("takeCoverage", 0, |_, _, _| Ok(JsValue::undefined())),
    ("stopCoverage", 0, |_, _, _| Ok(JsValue::undefined())),
    ("isStringOneByteRepresentation", 1, |_, args, cx| {
        let units = arg(args, 0).to_string(cx)?.to_vec();
        Ok(units.iter().all(|u| *u < 256).into())
    }),
    ("serialize", 1, |_, args, cx| {
        let json = cx.intrinsics().objects().json();
        let text = invoke(&json.into(), "stringify", &[arg(args, 0)], cx)?;
        let text = if text.is_undefined() {
            "null".to_owned()
        } else {
            text_of(&text, cx)?
        };
        crate::encoding::make_buffer(cx, text.into_bytes())
    }),
    ("deserialize", 1, |_, args, cx| {
        let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
        let json = cx.intrinsics().objects().json();
        invoke(
            &json.into(),
            "parse",
            &[js_text(&String::from_utf8_lossy(&bytes))],
            cx,
        )
    }),
];

pub(crate) fn v8_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, V8_FUNCTIONS)?;
    for name in [
        "Serializer",
        "Deserializer",
        "DefaultSerializer",
        "DefaultDeserializer",
        "GCProfiler",
    ] {
        let (class, _) = class(
            context,
            ClassSpec {
                name,
                ..ClassSpec::default()
            },
        )?;
        module.set(JsString::from(name), class, false, context)?;
    }
    let startup = object(context);
    exports(
        context,
        &startup,
        &[
            ("isBuildingSnapshot", 0, |_, _, _| Ok(false.into())),
            (
                "addSerializeCallback",
                2,
                |_, _, _| Ok(JsValue::undefined()),
            ),
            ("addDeserializeCallback", 2, |_, _, _| {
                Ok(JsValue::undefined())
            }),
            ("setDeserializeMainFunction", 2, |_, _, _| {
                Ok(JsValue::undefined())
            }),
        ],
    )?;
    module.set(js_string!("startupSnapshot"), startup, false, context)?;
    Ok(module.into())
}

// ─── worker_threads ─────────────────────────────────────────────────────────────────────────

/// `setEnvironmentData`'s values, for the realm.
struct EnvironmentData(JsObject);

fn environment_data(context: &mut Context) -> JsResult<JsObject> {
    if let Some(data) = context.get_data::<EnvironmentData>() {
        return Ok(data.0.clone());
    }
    let map = crate::native::construct_global(context, "Map", &[])?;
    context.insert_data(EnvironmentData(map.clone()));
    Ok(map)
}

fn worker_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    let emitter = crate::modules::node_module(context, "events")?;
    crate::call(context, &emitter, &this.clone().into(), &[])?;
    this.set(js_string!("threadId"), -1, false, context)?;
    let error = coded_error_value(
        context,
        "worker_threads.Worker cannot start a thread here: use the web's Worker",
        "ERR_WORKER_INIT_FAILED",
    );
    let fire = closure(
        context,
        "",
        0,
        |_, _, (worker, error): &(JsObject, JsValue), cx| {
            let worker: JsValue = worker.clone().into();
            if invoke(&worker, "listenerCount", &[js_text("error")], cx)?.to_number(cx)? > 0. {
                invoke(&worker, "emit", &[js_text("error"), error.clone()], cx)?;
            }
            invoke(&worker, "emit", &[js_text("exit"), 1.into()], cx)?;
            Ok(JsValue::undefined())
        },
        (this.clone(), error),
    );
    crate::microtask(context, fire.into(), Vec::new());
    Ok(())
}

pub(crate) fn worker_threads_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    module.set(js_string!("isMainThread"), true, false, context)?;
    module.set(js_string!("parentPort"), JsValue::null(), false, context)?;
    module.set(js_string!("workerData"), JsValue::null(), false, context)?;
    module.set(js_string!("threadId"), 0, false, context)?;
    module.set(js_string!("isInternalThread"), false, false, context)?;
    let limits = object(context);
    module.set(js_string!("resourceLimits"), limits, false, context)?;
    let share = boa_engine::JsSymbol::new(Some(js_string!("nodejs.worker_threads.SHARE_ENV")))
        .map_or(JsValue::undefined(), JsValue::from);
    module.set(js_string!("SHARE_ENV"), share, false, context)?;
    for name in ["MessageChannel", "MessagePort", "BroadcastChannel"] {
        let global = crate::native::global(context, name)?;
        module.set(JsString::from(name), global, false, context)?;
    }
    let emitter = crate::modules::node_module(context, "events")?.as_object();
    let (worker, _) = class(
        context,
        ClassSpec {
            name: "Worker",
            length: 2,
            parent: emitter.as_ref(),
            init: worker_init,
            methods: &[
                ("postMessage", 2, |_, _, _| Ok(JsValue::undefined())),
                ("terminate", 0, |_, _, cx| settled(Ok(1.into()), cx)),
                ("ref", 0, |this, _, _| Ok(this.clone())),
                ("unref", 0, |this, _, _| Ok(this.clone())),
                ("getHeapSnapshot", 0, |_, _, cx| {
                    settled(
                        Err(coded_error(
                            cx,
                            "The worker is not running",
                            "ERR_WORKER_NOT_RUNNING",
                        )),
                        cx,
                    )
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Worker"), worker, false, context)?;
    exports(
        context,
        &module,
        &[
            (
                "markAsUntransferable",
                1,
                |_, _, _| Ok(JsValue::undefined()),
            ),
            ("isMarkedAsUntransferable", 1, |_, _, _| Ok(false.into())),
            ("markAsUncloneable", 1, |_, _, _| Ok(JsValue::undefined())),
            ("moveMessagePortToContext", 2, |_, args, _| Ok(arg(args, 0))),
            (
                "receiveMessageOnPort",
                1,
                |_, _, _| Ok(JsValue::undefined()),
            ),
            ("getEnvironmentData", 1, |_, args, cx| {
                let map = environment_data(cx)?;
                invoke(&map.into(), "get", &[arg(args, 0)], cx)
            }),
            ("setEnvironmentData", 2, |_, args, cx| {
                let map: JsValue = environment_data(cx)?.into();
                if arg(args, 1).is_undefined() {
                    invoke(&map, "delete", &[arg(args, 0)], cx)?;
                } else {
                    invoke(&map, "set", &[arg(args, 0), arg(args, 1)], cx)?;
                }
                Ok(JsValue::undefined())
            }),
        ],
    )?;
    Ok(module.into())
}

// ─── wasi ───────────────────────────────────────────────────────────────────────────────────

pub(crate) fn wasi_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let (wasi, _) = class(
        context,
        ClassSpec {
            name: "WASI",
            init: |_, _, cx| {
                Err(coded_error(
                    cx,
                    "WASI is not available here: WebAssembly runs without a system interface",
                    "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
                ))
            },
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("WASI"), wasi, false, context)?;
    Ok(module.into())
}
