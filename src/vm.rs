//! `vm`, `v8`, `worker_threads`, `wasi` and `node:sqlite`: Node's modules about the engine, its
//! threads, and what it embeds.
//!
//! A `vm` context is a realm of its own, as Node's is a V8 context of its own: its own
//! intrinsics (`Object`, `Function`, `Promise`… and `Intl`, `WebAssembly`) and its own global
//! object, with nothing of this runtime's on it — no `process`, `require`, `console` or timers
//! but what the host puts on the contextified object. Rebon's Code Mode runs model-written
//! programs in one and relies on it: a program that reached `process` could forge protocol
//! frames on stdout, or exit. As on Node, a context is no boundary against what the host hands
//! in (a host function's `.constructor` is the host's `Function`), only against the global
//! scope.
//!
//! The context's global object is a proxy over the realm's ordinary global, made in the
//! engine's `create_global_object` hook ([`global_object`]) and given the traps of Node's
//! interceptors ([`TRAPS`]): a name the contextified object has, own or inherited, is read from
//! it, assignments and definitions land on it, and what it lacks is the realm's builtin. What is
//! not configurable on the realm's global — `undefined`, `NaN`, `Infinity`, and what a script
//! declares with `var` or `function` — stays there too, as a proxy's invariants require, its
//! value mirrored onto the object.
//!
//! `codeGeneration: { strings: false }` makes `eval` and `Function` (and its async and
//! generator kin) in the context throw Node's `EvalError`, from the engine's
//! `HostEnsureCanCompileStrings` ([`ensure_can_compile_strings`]); `wasm: false` makes the
//! context's `WebAssembly` refuse to compile or instantiate. `compileFunction` is the host's,
//! and compiles regardless. `timeout` interrupts a script still running after it with
//! `ERR_SCRIPT_EXECUTION_TIMEOUT`. An `import()` in a context goes to the
//! `importModuleDynamically` given to the context or to the last script run in it — its throw
//! is the import's rejection — and is refused without one ([`guard_import`]): modules are not
//! loaded into contexts, and there is no `vm.SourceTextModule`. Promises made in a context
//! settle on the runtime's one microtask queue, so `microtaskMode: 'afterEvaluate'` is
//! accepted and changes nothing; `breakOnSigint` is ignored.
//!
//! `v8` answers plausible heap figures, and `serialize`/`deserialize` as JSON in a `Buffer`.
//! `worker_threads` says this is the main thread; its `Worker` cannot start (it emits `error`
//! and `exit` to whoever listens) — the web's `Worker` global is the one that works here.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use boa_engine::{
    Context, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Script,
    Source,
    context::intrinsics::Intrinsics,
    error::{EngineError, RuntimeLimitError},
    js_string,
    module::{ModuleRequest, Referrer},
    native_function::NativeFunctionPointer,
    object::builtins::{JsProxy, JsWeakMap},
    realm::Realm,
};
use boa_gc::{Finalize, GcRefCell, Trace};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error, coded_error_value, exports, get, hidden,
    invoke, js_text, list_of, object, settled, text_of,
};

// ─── vm ─────────────────────────────────────────────────────────────────────────────────────

thread_local! {
    /// The handler of the global object of the realm being made for a context: set by
    /// [`create_context`] around making the realm, taken by [`global_object`].
    static NEXT_GLOBAL: RefCell<Option<JsObject>> = const { RefCell::new(None) };
}

/// The engine's `create_global_object` hook: an ordinary object — or, for a context's realm, a
/// proxy over one, whose handler is empty while the realm installs its builtins.
pub(crate) fn global_object(intrinsics: &Intrinsics) -> JsObject {
    let global = JsObject::with_object_proto(intrinsics);
    match NEXT_GLOBAL.with(|next| next.borrow_mut().take()) {
        Some(handler) => JsProxy::from_target_and_handler(global, handler).into(),
        None => global,
    }
}

/// What a context's realm keeps of its options, in its `[[HostDefined]]`.
#[derive(Trace, Finalize, JsData)]
struct Contextified {
    #[unsafe_ignore_trace]
    strings: bool,
    #[unsafe_ignore_trace]
    wasm: bool,
    /// Set while `compileFunction` compiles in the realm: the ban is on the context's code.
    #[unsafe_ignore_trace]
    compiling: Cell<bool>,
    /// `importModuleDynamically`: the context's, or the last script's run in it.
    import: GcRefCell<JsValue>,
}

fn contextified<R>(realm: &Realm, f: impl FnOnce(&Contextified) -> R) -> Option<R> {
    realm.host_defined().get::<Contextified>().map(f)
}

/// The engine's `HostEnsureCanCompileStrings`, for `eval` and `Function` in `realm`.
pub(crate) fn ensure_can_compile_strings(realm: &Realm) -> JsResult<()> {
    if contextified(realm, |c| !c.strings && !c.compiling.get()).unwrap_or(false) {
        return Err(JsNativeError::eval()
            .with_message("Code generation from strings disallowed for this context")
            .into());
    }
    Ok(())
}

/// Whether `WebAssembly` may compile in the running realm (`codeGeneration.wasm`).
pub(crate) fn wasm_allowed(context: &Context) -> bool {
    contextified(context.realm(), |c| c.wasm).unwrap_or(true)
}

/// An `import()` from code in a context: refused, by the context's `importModuleDynamically`
/// when it has one (whatever it throws), with Node's `ERR_VM_DYNAMIC_IMPORT_CALLBACK_MISSING`
/// when not. The loader is this realm's; a module it loads would hand the context this realm's
/// objects. `USE_MAIN_CONTEXT_DEFAULT_LOADER` (any symbol) lets it load them, as on Node.
pub(crate) fn guard_import(
    referrer: &Referrer,
    request: &ModuleRequest,
    context: &mut Context,
) -> JsResult<()> {
    let realm = match referrer {
        Referrer::Module(module) => module.realm().clone(),
        Referrer::Realm(realm) => realm.clone(),
        Referrer::Script(script) => script.realm().clone(),
    };
    let callback = contextified(&realm, |c| c.import.borrow().clone())
        .or_else(|| contextified(context.realm(), |c| c.import.borrow().clone()));
    let Some(callback) = callback else {
        return Ok(());
    };
    if callback.is_symbol() {
        return Ok(());
    }
    if callback.is_callable() {
        let specifier = JsValue::from(request.specifier().clone());
        crate::call(context, &callback, &JsValue::undefined(), &[specifier])?;
        return Err(coded_error(
            context,
            "vm: a context cannot load modules here; importModuleDynamically must throw",
            "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ));
    }
    Err(coded_error(
        context,
        "A dynamic import callback was not specified.",
        "ERR_VM_DYNAMIC_IMPORT_CALLBACK_MISSING",
    ))
}

/// The traps of a context's global object (see the module's comment): `(handler, sandbox)`
/// puts them on `handler`. Made once, in the runtime's realm, with its `Reflect` as it is then.
/// A property is *locked* on the realm's global when it is not configurable and not a writable
/// value: reads and writes of it must be the global's own.
const TRAPS: &str = r#"(() => {
  "use strict";
  const { apply, defineProperty, deleteProperty, get, getOwnPropertyDescriptor, has, ownKeys, set } = Reflect;
  const includes = Array.prototype.includes;
  const fixed = (target, key) => {
    const own = getOwnPropertyDescriptor(target, key);
    return own !== undefined && !own.configurable ? own : undefined;
  };
  const locked = (target, key) => {
    const own = fixed(target, key);
    return own !== undefined && !("value" in own && own.writable);
  };
  return (handler, sandbox) => {
    handler.get = (target, key, receiver) =>
      has(sandbox, key) && !locked(target, key) ? get(sandbox, key) : get(target, key, receiver);
    handler.set = (target, key, value, receiver) =>
      locked(target, key) ? set(target, key, value, receiver) : set(sandbox, key, value);
    handler.has = (target, key) => has(sandbox, key) || has(target, key);
    handler.deleteProperty = (target, key) =>
      fixed(target, key) === undefined && deleteProperty(sandbox, key) && deleteProperty(target, key);
    handler.getOwnPropertyDescriptor = (target, key) => {
      const own = getOwnPropertyDescriptor(target, key);
      const mine = getOwnPropertyDescriptor(sandbox, key);
      if (own !== undefined && !own.configurable) {
        if (mine !== undefined && "value" in mine && "value" in own && own.writable) own.value = mine.value;
        return own;
      }
      if (mine === undefined) return own;
      mine.configurable = true;
      return mine;
    };
    handler.defineProperty = (target, key, desc) => {
      if (fixed(target, key) === undefined && desc.configurable !== false) {
        return defineProperty(sandbox, key, desc);
      }
      if (!defineProperty(target, key, desc)) return false;
      if ("value" in desc) {
        defineProperty(sandbox, key, { value: desc.value, writable: true, enumerable: true, configurable: true });
      }
      return true;
    };
    handler.ownKeys = (target) => {
      const keys = ownKeys(sandbox);
      const theirs = ownKeys(target);
      for (let i = 0; i < theirs.length; i++) {
        if (!apply(includes, keys, [theirs[i]])) keys[keys.length] = theirs[i];
      }
      return keys;
    };
    handler.preventExtensions = () => false;
  };
})()"#;

/// The function [`TRAPS`] makes, kept for the runtime.
struct Traps(JsValue);

fn traps(context: &mut Context) -> JsResult<JsValue> {
    if let Some(Traps(traps)) = context.get_data::<Traps>() {
        return Ok(traps.clone());
    }
    let traps = context.eval(Source::from_bytes(TRAPS))?;
    context.insert_data(Traps(traps.clone()));
    Ok(traps)
}

/// `--disable-proto`'s script, run in every context's realm as it was in the runtime's.
#[derive(Clone, Copy)]
pub(crate) struct DisabledProto(pub(crate) &'static str);

/// Each context's realm, by its contextified object.
struct Contexts(JsWeakMap);

/// The record a context's object is mapped to.
#[derive(Trace, Finalize, JsData)]
struct ContextRealm(Realm);

fn contexts(context: &mut Context) -> JsWeakMap {
    if let Some(Contexts(map)) = context.get_data::<Contexts>() {
        return map.clone();
    }
    let map = JsWeakMap::new(context);
    context.insert_data(Contexts(map.clone()));
    map
}

/// The realm of the context `value` is, if it is one.
fn realm_of(value: &JsValue, context: &mut Context) -> JsResult<Option<Realm>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    let Some(record) = contexts(context).get(&object, context)?.as_object() else {
        return Ok(None);
    };
    let realm = record.downcast_ref::<ContextRealm>().map(|r| r.0.clone());
    Ok(realm)
}

fn require_context(value: &JsValue, context: &mut Context) -> JsResult<Realm> {
    realm_of(value, context)?.ok_or_else(|| {
        coded_error(
            context,
            "The \"contextifiedObject\" argument must be an vm.Context",
            "ERR_INVALID_ARG_TYPE",
        )
    })
}

/// Runs `f` with `realm` as the running realm.
fn in_realm<R>(context: &mut Context, realm: &Realm, f: impl FnOnce(&mut Context) -> R) -> R {
    let outer = context.enter_realm(realm.clone());
    let out = f(context);
    context.enter_realm(outer);
    out
}

/// A context's options: `codeGeneration` (or `contextCodeGeneration`, for `runInNewContext`)
/// and `importModuleDynamically`.
struct ContextOptions {
    strings: bool,
    wasm: bool,
    import: JsValue,
}

fn context_options(
    options: &JsValue,
    generation: &str,
    context: &mut Context,
) -> JsResult<ContextOptions> {
    let allowed = |value: JsValue, name: &str, context: &mut Context| -> JsResult<bool> {
        let flag = get(&value, name, context)?;
        Ok(flag.is_undefined() || flag.to_boolean())
    };
    let generation = if options.is_object() {
        get(options, generation, context)?
    } else {
        JsValue::undefined()
    };
    Ok(ContextOptions {
        strings: allowed(generation.clone(), "strings", context)?,
        wasm: allowed(generation, "wasm", context)?,
        import: if options.is_object() {
            get(options, "importModuleDynamically", context)?
        } else {
            JsValue::undefined()
        },
    })
}

/// Makes `sandbox` a context (once): a new realm whose global object answers with its
/// properties.
fn create_context(
    sandbox: &JsObject,
    options: ContextOptions,
    context: &mut Context,
) -> JsResult<Realm> {
    if let Some(realm) = realm_of(&sandbox.clone().into(), context)? {
        return Ok(realm);
    }
    let handler = JsObject::with_null_proto();
    NEXT_GLOBAL.with(|next| next.replace(Some(handler.clone())));
    let made = context.create_realm();
    NEXT_GLOBAL.with(|next| next.take());
    let realm = made?;
    in_realm(context, &realm, |cx| -> JsResult<()> {
        crate::intl::install(cx)?;
        crate::wasm::install(cx)?;
        if let Some(DisabledProto(script)) = cx.get_data::<DisabledProto>().copied() {
            cx.eval(Source::from_bytes(script))?;
        }
        Ok(())
    })?;
    let traps = traps(context)?;
    crate::call(
        context,
        &traps,
        &JsValue::undefined(),
        &[handler.into(), sandbox.clone().into()],
    )?;
    realm.host_defined_mut().insert(Contextified {
        strings: options.strings,
        wasm: options.wasm,
        compiling: Cell::new(false),
        import: GcRefCell::new(options.import),
    });
    let record = JsObject::from_proto_and_data(None, ContextRealm(realm.clone()));
    contexts(context).set(sandbox, record.into(), context)?;
    Ok(realm)
}

/// The object `runInNewContext` contextifies: the one given, or a fresh one.
fn new_context(
    value: &JsValue,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<Realm> {
    let sandbox = match value.as_object() {
        Some(sandbox) => sandbox,
        None if value.is_undefined() => object(context),
        None => {
            return Err(coded_error(
                context,
                "The \"contextObject\" argument must be of type object",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
    };
    let options = context_options(options, "contextCodeGeneration", context)?;
    create_context(&sandbox, options, context)
}

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

/// The `timeout` option, in milliseconds.
fn timeout_of(options: &JsValue, context: &mut Context) -> JsResult<Option<u64>> {
    if !options.is_object() {
        return Ok(None);
    }
    let timeout = get(options, "timeout", context)?;
    if timeout.is_undefined() {
        return Ok(None);
    }
    let ms = timeout.to_number(context)?;
    if !(ms >= 1. && ms <= u32::MAX as f64) {
        return Err(coded_error(
            context,
            &format!(
                "The value of \"options.timeout\" is out of range. It must be >= 1 && <= {}. Received {ms}",
                u32::MAX
            ),
            "ERR_OUT_OF_RANGE",
        ));
    }
    Ok(Some(ms as u64))
}

fn interrupted(error: &JsError) -> bool {
    matches!(
        error.as_engine(),
        Some(EngineError::RuntimeLimit(RuntimeLimitError::Interrupted))
    )
}

/// Runs `f`, interrupted after `timeout` ms by a watchdog thread, as V8's `TerminateExecution`
/// stops a script past Node's `timeout`.
fn with_timeout<R>(
    timeout: Option<u64>,
    context: &mut Context,
    f: impl FnOnce(&mut Context) -> JsResult<R>,
) -> JsResult<R> {
    let Some(ms) = timeout else {
        return f(context);
    };
    let handle = context.interrupt_handle();
    let (done, wait) = std::sync::mpsc::channel::<()>();
    let watchdog = {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let late = matches!(
                wait.recv_timeout(Duration::from_millis(ms)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            );
            if late {
                handle.interrupt();
            }
            late
        })
    };
    let out = f(context);
    drop(done);
    if !watchdog.join().unwrap_or(false) {
        return out;
    }
    // Asked after the script was done, the request would stop whatever ran next.
    handle.clear();
    match out {
        Err(error) if interrupted(&error) => Err(coded_error(
            context,
            &format!("Script execution timed out after {ms}ms"),
            "ERR_SCRIPT_EXECUTION_TIMEOUT",
        )),
        out => out,
    }
}

/// Runs `code` as a script of `realm`'s.
fn run_in_realm(
    code: &str,
    filename: &str,
    realm: &Realm,
    timeout: Option<u64>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let source = Source::from_bytes(code).with_path(Path::new(filename));
    let script = in_realm(context, realm, |cx| Script::parse(source, None, cx))?;
    with_timeout(timeout, context, |cx| script.evaluate(cx))
}

fn run_in_this_context(
    code: &str,
    filename: &str,
    timeout: Option<u64>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let realm = context.realm().clone();
    run_in_realm(code, filename, &realm, timeout, context)
}

/// `compileFunction`: `new Function(...params, code)` of the parsing context's realm, ban or
/// not; with `contextExtensions`, made inside `with` blocks over them. The body is compiled
/// alone first, so the one wrapped is one that parses as a body.
fn compile_function(
    code: &str,
    params: Vec<JsValue>,
    extensions: Vec<JsValue>,
    realm: &Realm,
    context: &mut Context,
) -> JsResult<JsValue> {
    let function = realm.intrinsics().constructors().function().constructor();
    let lifted = contextified(realm, |c| c.compiling.replace(true));
    let made = (|| {
        let mut parts = params.clone();
        parts.push(js_text(code));
        let plain = function.construct(&parts, None, context)?;
        if extensions.is_empty() {
            return Ok(plain.into());
        }
        let names = params
            .iter()
            .map(|p| text_of(p, context))
            .collect::<JsResult<Vec<_>>>()?;
        let scopes: Vec<String> = (0..extensions.len())
            .map(|i| format!("__vmExtension{i}"))
            .collect();
        let mut wrapper: Vec<JsValue> = scopes.iter().map(|s| js_text(s)).collect();
        let withs: String = scopes.iter().map(|s| format!("with ({s}) ")).collect();
        wrapper.push(js_text(&format!(
            "{withs}return function ({}) {{\n{code}\n}};",
            names.join(", ")
        )));
        let outer = function.construct(&wrapper, None, context)?;
        crate::call(context, &outer.into(), &JsValue::undefined(), &extensions)
    })();
    if let Some(before) = lifted {
        contextified(realm, |c| c.compiling.set(before));
    }
    made
}

/// Checks code parses — what `new vm.Script(code)` does before it is run.
fn check_syntax(code: &str, filename: &str, context: &mut Context) -> JsResult<()> {
    Script::parse(
        Source::from_bytes(code).with_path(Path::new(filename)),
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

/// Runs a `vm.Script` in a context's realm, its `importModuleDynamically` now the context's.
fn run_script_in(
    this: &JsValue,
    realm: &Realm,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (code, filename) = script_code(this, context)?;
    let import = get(this, "__import", context)?;
    if !import.is_undefined() {
        contextified(realm, |c| *c.import.borrow_mut() = import);
    }
    let timeout = timeout_of(options, context)?;
    run_in_realm(&code, &filename, realm, timeout, context)
}

const SCRIPT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("runInThisContext", 1, |this, args, cx| {
        let (code, filename) = script_code(this, cx)?;
        let timeout = timeout_of(&arg(args, 0), cx)?;
        run_in_this_context(&code, &filename, timeout, cx)
    }),
    ("runInContext", 2, |this, args, cx| {
        let realm = require_context(&arg(args, 0), cx)?;
        run_script_in(this, &realm, &arg(args, 1), cx)
    }),
    ("runInNewContext", 2, |this, args, cx| {
        let realm = new_context(&arg(args, 0), &arg(args, 1), cx)?;
        run_script_in(this, &realm, &arg(args, 1), cx)
    }),
    ("createCachedData", 0, |_, _, cx| {
        crate::encoding::make_buffer(cx, Vec::new())
    }),
];

const VM_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("runInThisContext", 2, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let filename = filename_of(&arg(args, 1), cx)?;
        let timeout = timeout_of(&arg(args, 1), cx)?;
        run_in_this_context(&code, &filename, timeout, cx)
    }),
    ("runInContext", 3, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let realm = require_context(&arg(args, 1), cx)?;
        let filename = filename_of(&arg(args, 2), cx)?;
        let timeout = timeout_of(&arg(args, 2), cx)?;
        run_in_realm(&code, &filename, &realm, timeout, cx)
    }),
    ("runInNewContext", 3, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let realm = new_context(&arg(args, 1), &arg(args, 2), cx)?;
        let filename = filename_of(&arg(args, 2), cx)?;
        let timeout = timeout_of(&arg(args, 2), cx)?;
        run_in_realm(&code, &filename, &realm, timeout, cx)
    }),
    ("createContext", 2, |_, args, cx| {
        let sandbox = match arg(args, 0).as_object() {
            Some(sandbox) => sandbox,
            None if arg(args, 0).is_undefined() => object(cx),
            None => {
                return Err(coded_error(
                    cx,
                    "The \"contextObject\" argument must be of type object",
                    "ERR_INVALID_ARG_TYPE",
                ));
            }
        };
        let options = context_options(&arg(args, 1), "codeGeneration", cx)?;
        create_context(&sandbox, options, cx)?;
        Ok(sandbox.into())
    }),
    ("isContext", 1, |_, args, cx| {
        Ok(realm_of(&arg(args, 0), cx)?.is_some().into())
    }),
    ("compileFunction", 3, |_, args, cx| {
        let code = text_of(&arg(args, 0), cx)?;
        let params = match arg(args, 1) {
            p if p.is_undefined() => Vec::new(),
            p => list_of(&p, cx)?,
        };
        let options = arg(args, 2);
        let parsing = get(&options, "parsingContext", cx)?;
        let realm = if parsing.is_undefined() {
            cx.realm().clone()
        } else {
            require_context(&parsing, cx)?
        };
        let extensions = match get(&options, "contextExtensions", cx)? {
            e if e.is_undefined() => Vec::new(),
            e => list_of(&e, cx)?,
        };
        compile_function(&code, params, extensions, &realm, cx)
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
                let options = arg(args, 1);
                let filename = filename_of(&options, cx)?;
                check_syntax(&code, &filename, cx)?;
                hidden(this, js_string!("__code"), js_text(&code), cx)?;
                hidden(this, js_string!("__filename"), js_text(&filename), cx)?;
                if options.is_object() {
                    let import = get(&options, "importModuleDynamically", cx)?;
                    hidden(this, js_string!("__import"), import, cx)?;
                }
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
    // Refused as Node refuses it without `--allow-worker`, before anything is started.
    crate::permission::worker(context)?;
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

/// `node:sqlite`: there to be required — bundles that can use it require it at load and choose
/// later — with `DatabaseSync` and `StatementSync` refusing to be made, since this runtime keeps
/// no SQLite of its own. Plugins keep their data in the vault, or in IndexedDB.
pub(crate) fn sqlite_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    for name in ["DatabaseSync", "StatementSync", "Session"] {
        let (class, _) = class(
            context,
            ClassSpec {
                name,
                init: |_, _, cx| {
                    Err(coded_error(
                        cx,
                        "node:sqlite is not available here: plugins keep their data in the vault or in IndexedDB",
                        "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
                    ))
                },
                ..ClassSpec::default()
            },
        )?;
        module.set(JsString::from(name), class, false, context)?;
    }
    let backup = crate::native::function(context, "backup", 2, |_, _, cx| {
        Err(coded_error(
            cx,
            "node:sqlite is not available here",
            "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ))
    });
    module.set(js_string!("backup"), backup, false, context)?;
    let constants = object(context);
    for (name, value) in [
        ("SQLITE_CHANGESET_OMIT", 0),
        ("SQLITE_CHANGESET_REPLACE", 1),
        ("SQLITE_CHANGESET_ABORT", 2),
    ] {
        constants.set(JsString::from(name), value, false, context)?;
    }
    module.set(js_string!("constants"), constants, false, context)?;
    Ok(module.into())
}
