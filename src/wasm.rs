//! `WebAssembly`, native: modules compiled and run by [wasmi], an interpreter written in
//! Rust — what wasm-bindgen's glue (Templater's parser) and Emscripten's ask for.
//!
//! A module's linear memory lives in wasmi; what JavaScript sees of it is an `ArrayBuffer`
//! kept the same as it at every crossing: copied in as JavaScript calls into the module (or a
//! function it imported returns to it), copied out as the module returns (or calls out). When
//! the memory grows, the old buffer is detached and a new one made, as a browser does — so
//! glue that keeps a `Uint8Array` over `memory.buffer` and makes it again once its
//! `byteLength` is 0 finds what it expects.
//!
//! A function the module imports is called with the realm it was called from; one that throws
//! unwinds the module and is thrown on, the same error, out of the export that was called.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArray, JsArrayBuffer, JsPromise},
    },
    property::{Attribute, PropertyDescriptor},
};
use wasmi::{
    AsContextMut, Caller, Engine, Extern, ExternRef, ExternType, Func, FuncRef,
    FuncType, Global, Memory, MemoryType, Module, Mutability, Store, StoreContextMut, Table,
    TableType, Val,
    core::{F32, F64, ValType},
};

use crate::shared;

/// What a store keeps beside the module's own state: how to reach the realm, and the
/// JavaScript values the module holds as `externref`s.
pub(crate) struct HostData {
    id: u32,
    context: *mut Context,
    imports: Vec<JsValue>,
    externs: Vec<JsValue>,
}

/// One instance (or one memory, table or global made by itself): its store, and what
/// JavaScript holds of it.
pub(crate) struct Wasm {
    store: RefCell<Store<HostData>>,
    /// The `Caller` of the host function running now, while one is: a call into the module
    /// from inside it goes through it, the store being lent to the module.
    caller: Cell<*mut ()>,
    memories: RefCell<Vec<Memory>>,
    views: RefCell<Vec<Option<JsArrayBuffer>>>,
    funcs: RefCell<Vec<Func>>,
    tables: RefCell<Vec<Table>>,
    globals: RefCell<Vec<Global>>,
}

/// Everything WebAssembly keeps for a realm.
#[derive(Default)]
pub(crate) struct WasmState {
    engine: Option<Engine>,
    modules: Vec<Rc<Module>>,
    instances: Vec<Rc<Wasm>>,
    /// The error a function the module imported threw, on its way out of the module.
    thrown: Option<JsError>,
}

#[derive(Debug)]
struct Thrown;

impl std::fmt::Display for Thrown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JavaScript function the module imported threw")
    }
}

impl wasmi::core::HostError for Thrown {}

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn js_text(text: &str) -> JsValue {
    JsString::from(text).into()
}

fn object(context: &mut Context) -> JsObject {
    JsObject::with_object_proto(context.intrinsics())
}

fn list_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context.intrinsics().constructors().array().constructor().get(js_string!("from"), context)?;
    let list = crate::call(context, &from, &JsValue::undefined(), std::slice::from_ref(value))?;
    let Some(list) = list.as_object() else { return Ok(Vec::new()) };
    let list = JsArray::from_object(list)?;
    (0..list.length(context)?).map(|at| list.get(at, context)).collect()
}

fn engine(context: &Context) -> Engine {
    let state = shared(context);
    let mut wasm = state.wasm.borrow_mut();
    wasm.engine.get_or_insert_with(Engine::default).clone()
}

fn instance(context: &Context, id: u32) -> JsResult<Rc<Wasm>> {
    shared(context)
        .wasm
        .borrow()
        .instances
        .get(id as usize)
        .cloned()
        .ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly object").into())
}

/// One of `WebAssembly`'s errors — `CompileError`, `LinkError`, `RuntimeError` — made by its
/// class, so `instanceof` knows it.
fn wasm_error(context: &mut Context, class: &str, message: &str) -> JsError {
    let made = (|| -> JsResult<JsValue> {
        let namespace = context.global_object().get(js_string!("WebAssembly"), context)?;
        let class = namespace.as_object().map(|n| n.get(JsString::from(class), context)).transpose()?;
        match class.and_then(|c| c.as_object()) {
            Some(class) => Ok(class.construct(&[js_text(message)], None, context)?.into()),
            None => Ok(JsValue::undefined()),
        }
    })();
    match made {
        Ok(value) if value.is_object() => JsError::from_opaque(value),
        _ => JsNativeError::error().with_message(format!("{class}: {message}")).into(),
    }
}

fn link_error(context: &mut Context, message: &str) -> JsError {
    wasm_error(context, "LinkError", message)
}

fn hidden(target: &JsObject, name: &str, value: impl Into<JsValue>, context: &mut Context) -> JsResult<()> {
    target.define_property_or_throw(
        JsString::from(name),
        PropertyDescriptor::builder().value(value.into()).writable(true).enumerable(false).configurable(true),
        context,
    )?;
    Ok(())
}

fn hidden_number(target: &JsValue, name: &str, context: &mut Context) -> Option<u32> {
    let value = target.as_object()?.get(JsString::from(name), context).ok()?;
    value.as_number().map(|n| n as u32)
}

fn with_proto(context: &mut Context, class: &str) -> JsResult<JsObject> {
    let made = object(context);
    let namespace = context.global_object().get(js_string!("WebAssembly"), context)?;
    if let Some(namespace) = namespace.as_object()
        && let Some(class) = namespace.get(JsString::from(class), context)?.as_object()
    {
        made.set_prototype(class.get(js_string!("prototype"), context)?.as_object());
    }
    Ok(made)
}

// ─── The store, and keeping memory and buffers the same ─────────────────────────────────────

/// Runs `f` with the instance's store: the one lent to the module through the host function
/// running now, if one is, else its own.
fn with_store<R>(wasm: &Wasm, f: impl FnOnce(StoreContextMut<'_, HostData>) -> R) -> JsResult<R> {
    let caller = wasm.caller.get();
    if !caller.is_null() {
        // SAFETY: set only while that host function runs, and taken back before it returns;
        // the `Caller` is the store's one live borrow then, and nothing else uses it here.
        let caller = unsafe { &mut *(caller as *mut Caller<'static, HostData>) };
        return Ok(f(caller.as_context_mut()));
    }
    match wasm.store.try_borrow_mut() {
        Ok(mut store) => Ok(f(store.as_context_mut())),
        Err(_) => Err(JsNativeError::error().with_message("the WebAssembly instance is busy").into()),
    }
}

/// JavaScript's buffers into the module's memories: done as the module is entered.
fn sync_in(wasm: &Wasm, store: &mut StoreContextMut<'_, HostData>) {
    let memories = wasm.memories.borrow();
    let views = wasm.views.borrow();
    for (memory, view) in memories.iter().zip(views.iter()) {
        let Some(view) = view else { continue };
        let Some(from) = view.data() else { continue };
        let to = memory.data_mut(&mut *store);
        if from.len() == to.len() {
            to.copy_from_slice(&from);
        }
    }
}

/// The module's memories out to JavaScript's buffers: done as the module is left. A memory
/// that has grown gets a buffer of its own, the old one detached.
fn sync_out(wasm: &Wasm, store: &StoreContextMut<'_, HostData>, context: &mut Context) -> JsResult<()> {
    let memories = wasm.memories.borrow();
    let mut views = wasm.views.borrow_mut();
    for (at, memory) in memories.iter().enumerate() {
        let data = memory.data(store);
        if let Some(view) = &views[at]
            && let Some(mut into) = view.data_mut()
            && into.len() == data.len()
        {
            into.copy_from_slice(data);
            continue;
        }
        if let Some(old) = views[at].take() {
            let _ = old.detach(&JsValue::undefined());
        }
        let block = boa_engine::object::builtins::AlignedVec::from_iter(64, data.iter().copied());
        views[at] = Some(JsArrayBuffer::from_byte_block(block, context)?);
    }
    Ok(())
}

// ─── Values across ──────────────────────────────────────────────────────────────────────────

/// A JavaScript value on its way into the module: numbers made now; references, which need
/// the store, made once it is had.
enum Arg {
    Val(Val),
    Extern(JsValue),
    /// A function of an instance's: which instance, and which of its functions.
    Func(Option<(u32, usize)>),
}

fn arg_for(value: &JsValue, ty: ValType, context: &mut Context) -> JsResult<Arg> {
    Ok(match ty {
        ValType::I32 => Arg::Val(Val::I32(value.to_i32(context)?)),
        ValType::I64 => Arg::Val(Val::I64(value.to_big_int64(context)?)),
        ValType::F32 => Arg::Val(Val::F32(F32::from_float(value.to_number(context)? as f32))),
        ValType::F64 => Arg::Val(Val::F64(F64::from_float(value.to_number(context)?))),
        ValType::ExternRef => Arg::Extern(value.clone()),
        ValType::FuncRef => Arg::Func(match value.as_object() {
            Some(f) => {
                let owner = f.get(js_string!("__wasm"), context)?.as_number();
                let index = f.get(js_string!("__func"), context)?.as_number();
                owner.zip(index).map(|(o, i)| (o as u32, i as usize))
            }
            None => None,
        }),
    })
}

fn val_of(arg: Arg, store: &mut StoreContextMut<'_, HostData>, wasm: &Wasm) -> Val {
    match arg {
        Arg::Val(v) => v,
        Arg::Extern(v) if v.is_null() => Val::ExternRef(ExternRef::null()),
        Arg::Extern(v) => {
            let data = store.data_mut();
            data.externs.push(v);
            let index = (data.externs.len() - 1) as u32;
            Val::ExternRef(ExternRef::new::<u32>(&mut *store, index))
        }
        Arg::Func(which) => {
            // Only a function of this instance's own goes into its tables.
            let found = which
                .filter(|(owner, _)| *owner == store.data().id)
                .and_then(|(_, index)| wasm.funcs.borrow().get(index).copied());
            Val::FuncRef(FuncRef::new(found))
        }
    }
}

fn js_of(value: &Val, store: &StoreContextMut<'_, HostData>, wasm_id: u32, context: &mut Context) -> JsResult<JsValue> {
    Ok(match value {
        Val::I32(n) => (*n).into(),
        Val::I64(n) => boa_engine::JsBigInt::from(*n).into(),
        Val::F32(n) => f64::from(n.to_float()).into(),
        Val::F64(n) => n.to_float().into(),
        Val::ExternRef(r) => match r.data(store).and_then(|d| d.downcast_ref::<u32>()) {
            Some(index) => store.data().externs.get(*index as usize).cloned().unwrap_or_default(),
            None => JsValue::null(),
        },
        Val::FuncRef(r) => match r.func() {
            Some(func) => export_function(context, wasm_id, *func, store)?.into(),
            None => JsValue::null(),
        },
    })
}

// ─── Calls ──────────────────────────────────────────────────────────────────────────────────

/// A function of the module as JavaScript calls it.
fn export_function(context: &mut Context, wasm_id: u32, func: Func, store: &StoreContextMut<'_, HostData>) -> JsResult<JsObject> {
    let wasm = instance(context, wasm_id)?;
    let index = {
        let mut funcs = wasm.funcs.borrow_mut();
        funcs.push(func);
        funcs.len() - 1
    } as u32;
    let length = func.ty(store).params().len();
    let function: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(move |_, args, cx| call_export(wasm_id, index, args, cx)),
    )
    .name(JsString::from(index.to_string()))
    .length(length)
    .build()
    .into();
    hidden(&function, "__wasm", wasm_id, context)?;
    hidden(&function, "__func", index, context)?;
    Ok(function)
}

fn call_export(wasm_id: u32, index: u32, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let wasm = instance(context, wasm_id)?;
    let func = wasm.funcs.borrow()[index as usize];
    let ty = with_store(&wasm, |store| func.ty(&store))?;
    let mut prepared = Vec::new();
    for (at, param) in ty.params().iter().enumerate() {
        prepared.push(arg_for(&arg(args, at), *param, context)?);
    }
    let context_ptr: *mut Context = context;
    let outcome = with_store(&wasm, |mut store| {
        let previous = store.data().context;
        store.data_mut().context = context_ptr;
        sync_in(&wasm, &mut store);
        let params: Vec<Val> = prepared.into_iter().map(|a| val_of(a, &mut store, &wasm)).collect();
        let mut results: Vec<Val> = ty.results().iter().map(|t| Val::default(*t)).collect();
        let called = func.call(&mut store, &params, &mut results);
        // SAFETY: the pointer is the `context` this call was made with.
        let context = unsafe { &mut *context_ptr };
        let synced = sync_out(&wasm, &store, context);
        let out: Result<JsResult<JsValue>, wasmi::Error> = match called {
            Ok(()) => Ok(synced.and_then(|()| {
                let values = results
                    .iter()
                    .map(|v| js_of(v, &store, wasm_id, context))
                    .collect::<JsResult<Vec<_>>>()?;
                Ok(match values.len() {
                    0 => JsValue::undefined(),
                    1 => values.into_iter().next().expect("one"),
                    _ => JsArray::from_iter(values, context).into(),
                })
            })),
            Err(error) => Err(error),
        };
        store.data_mut().context = previous;
        out
    })?;
    match outcome {
        Ok(value) => value,
        Err(error) => {
            if error.downcast_ref::<Thrown>().is_some()
                && let Some(thrown) = shared(context).wasm.borrow_mut().thrown.take()
            {
                return Err(thrown);
            }
            Err(wasm_error(context, "RuntimeError", &error_text(&error)))
        }
    }
}

fn error_text(error: &wasmi::Error) -> String {
    match error.as_trap_code() {
        Some(code) => code.to_string(),
        None => error.to_string(),
    }
}

/// A function JavaScript gave the module to import, as the module calls it.
fn host_function(store: &mut Store<HostData>, ty: FuncType, import: usize) -> Func {
    let results: Vec<ValType> = ty.results().to_vec();
    Func::new(&mut *store, ty, move |mut caller: Caller<'_, HostData>, params: &[Val], out: &mut [Val]| {
        let context_ptr = caller.data().context;
        // SAFETY: the realm the call into the module was made from, waiting on it.
        let context = unsafe { &mut *context_ptr };
        let wasm_id = caller.data().id;
        let Ok(wasm) = instance(context, wasm_id) else {
            return Err(wasmi::Error::new("the instance is gone"));
        };
        let function = caller.data().imports[import].clone();
        let ctx = caller.as_context_mut();
        let mut args = Vec::new();
        for param in params {
            match js_of(param, &ctx, wasm_id, context) {
                Ok(value) => args.push(value),
                Err(error) => return Err(throw(context, error)),
            }
        }
        if let Err(error) = sync_out(&wasm, &ctx, context) {
            return Err(throw(context, error));
        }
        drop(ctx);
        // While JavaScript runs, a call back into the module goes through this caller.
        let previous = wasm.caller.replace(&mut caller as *mut Caller<'_, HostData> as *mut ());
        let returned = crate::call(context, &function, &JsValue::undefined(), &args);
        wasm.caller.set(previous);
        let mut ctx = caller.as_context_mut();
        sync_in(&wasm, &mut ctx);
        let returned = match returned {
            Ok(value) => value,
            Err(error) => return Err(throw(context, error)),
        };
        // What it answered, as the results the module expects.
        let values = match results.len() {
            0 => Vec::new(),
            1 => vec![returned],
            _ => match list_of(&returned, context) {
                Ok(list) => list,
                Err(error) => return Err(throw(context, error)),
            },
        };
        for (at, ty) in results.iter().enumerate() {
            let value = values.get(at).cloned().unwrap_or_default();
            match arg_for(&value, *ty, context) {
                Ok(prepared) => out[at] = val_of(prepared, &mut ctx, &wasm),
                Err(error) => return Err(throw(context, error)),
            }
        }
        Ok(())
    })
}

/// A JavaScript error, kept to be thrown again where the module was called from.
fn throw(context: &Context, error: JsError) -> wasmi::Error {
    shared(context).wasm.borrow_mut().thrown = Some(error);
    wasmi::Error::host(Thrown)
}

// ─── Compiling and instantiating ────────────────────────────────────────────────────────────

fn bytes_from(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    crate::encoding::bytes_of(value, context)?.ok_or_else(|| {
        JsNativeError::typ()
            .with_message("WebAssembly needs the module's bytes: an ArrayBuffer or a typed array")
            .into()
    })
}

fn compile(bytes: &[u8], context: &mut Context) -> JsResult<JsObject> {
    let engine = engine(context);
    let module = Module::new(&engine, bytes).map_err(|e| wasm_error(context, "CompileError", &e.to_string()))?;
    let id = {
        let state = shared(context);
        let mut wasm = state.wasm.borrow_mut();
        wasm.modules.push(Rc::new(module));
        wasm.modules.len() - 1
    };
    let object = with_proto(context, "Module")?;
    hidden(&object, "__module", id as u32, context)?;
    Ok(object)
}

fn module_of(value: &JsValue, context: &mut Context) -> JsResult<Rc<Module>> {
    let id = hidden_number(value, "__module", context)
        .ok_or_else(|| JsError::from(JsNativeError::typ().with_message("not a WebAssembly.Module")))?;
    Ok(shared(context).wasm.borrow().modules[id as usize].clone())
}

fn new_wasm(context: &Context) -> (u32, Rc<Wasm>) {
    let engine = engine(context);
    let state = shared(context);
    let mut wasm = state.wasm.borrow_mut();
    let id = wasm.instances.len() as u32;
    let made = Rc::new(Wasm {
        store: RefCell::new(Store::new(
            &engine,
            HostData { id, context: std::ptr::null_mut(), imports: Vec::new(), externs: Vec::new() },
        )),
        caller: Cell::new(std::ptr::null_mut()),
        memories: RefCell::new(Vec::new()),
        views: RefCell::new(Vec::new()),
        funcs: RefCell::new(Vec::new()),
        tables: RefCell::new(Vec::new()),
        globals: RefCell::new(Vec::new()),
    });
    wasm.instances.push(made.clone());
    (id, made)
}

/// A memory, table or global JavaScript made or was handed, by the object it is.
fn owned(value: &JsValue, kind: &str, context: &mut Context) -> Option<(Rc<Wasm>, usize)> {
    let id = hidden_number(value, "__wasm", context)?;
    let index = hidden_number(value, kind, context)? as usize;
    Some((instance(context, id).ok()?, index))
}

fn instantiate(module: &Module, imports: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let (id, wasm) = new_wasm(context);
    let mut linker = wasmi::Linker::<HostData>::new(module.engine());
    // What each import is, from the object the imports came in.
    let mut rebind: Vec<(JsObject, &'static str, u32)> = Vec::new();
    for import in module.imports() {
        let (module_name, name) = (import.module().to_owned(), import.name().to_owned());
        let namespace = match imports.as_object() {
            Some(imports) => imports.get(JsString::from(module_name.as_str()), context)?,
            None => JsValue::undefined(),
        };
        let Some(namespace) = namespace.as_object() else {
            return Err(link_error(context, &format!("import object field '{module_name}' is not an Object")));
        };
        let value = namespace.get(JsString::from(name.as_str()), context)?;
        let item: Extern = match import.ty() {
            ExternType::Func(ty) => {
                if !value.is_callable() {
                    return Err(link_error(context, &format!("{module_name}.{name}: function import requires a callable")));
                }
                let mut store = wasm.store.borrow_mut();
                store.data_mut().imports.push(value);
                let at = store.data().imports.len() - 1;
                Extern::Func(host_function(&mut store, ty.clone(), at))
            }
            ExternType::Memory(ty) => {
                // A memory made by JavaScript: made again in this store, with what it held,
                // and the object bound to this one from now on.
                let held = match owned(&value, "__memory", context) {
                    Some((from, at)) => {
                        let memory = from.memories.borrow()[at];
                        let wanted = memory.ty(&*from.store.borrow());
                        // What JavaScript wrote in it, with the buffer it wrote through: kept, so
                        // views made over it before now still see the memory.
                        let data = with_store(&from, |mut s| {
                            sync_in(&from, &mut s);
                            memory.data(&s).to_vec()
                        })?;
                        let view = from.views.borrow_mut()[at].take();
                        Some((wanted, data, view))
                    }
                    None => None,
                };
                let Some((wanted, data, view)) = held else {
                    return Err(link_error(context, &format!("{module_name}.{name}: memory import must be a WebAssembly.Memory")));
                };
                let _ = ty;
                let ty = wanted;
                let mut store = wasm.store.borrow_mut();
                let memory = Memory::new(&mut *store, ty).map_err(|e| link_error(context, &e.to_string()))?;
                let into = memory.data_mut(&mut *store);
                let n = into.len().min(data.len());
                into[..n].copy_from_slice(&data[..n]);
                wasm.memories.borrow_mut().push(memory);
                wasm.views.borrow_mut().push(view);
                if let Some(object) = value.as_object() {
                    rebind.push((object.clone(), "__memory", (wasm.memories.borrow().len() - 1) as u32));
                }
                Extern::Memory(memory)
            }
            ExternType::Global(ty) => {
                let given = match owned(&value, "__global", context) {
                    Some((from, at)) => {
                        let global = from.globals.borrow()[at];
                        with_store(&from, |s| global.get(&s))?
                    }
                    None => match arg_for(&value, ty.content(), context)? {
                        Arg::Val(v) => v,
                        _ => Val::default(ty.content()),
                    },
                };
                let mut store = wasm.store.borrow_mut();
                let global = Global::new(&mut *store, given, ty.mutability());
                wasm.globals.borrow_mut().push(global);
                Extern::Global(global)
            }
            ExternType::Table(ty) => {
                let mut store = wasm.store.borrow_mut();
                let table = Table::new(&mut *store, *ty, Val::default(ty.element()))
                    .map_err(|e| link_error(context, &e.to_string()))?;
                wasm.tables.borrow_mut().push(table);
                if let Some(object) = value.as_object() {
                    rebind.push((object.clone(), "__table", (wasm.tables.borrow().len() - 1) as u32));
                }
                Extern::Table(table)
            }
        };
        linker
            .define(&module_name, &name, item)
            .map_err(|e| link_error(context, &e.to_string()))?;
    }
    for (object, kind, index) in rebind {
        hidden(&object, "__wasm", id, context)?;
        hidden(&object, kind, index, context)?;
    }
    // Made, and its start function run.
    let context_ptr: *mut Context = context;
    let made = {
        let mut store = wasm.store.borrow_mut();
        store.data_mut().context = context_ptr;
        let pre = linker.instantiate(&mut *store, module).map_err(|e| e.to_string());
        let started = pre.and_then(|pre| pre.start(&mut *store).map_err(|e| {
            if e.downcast_ref::<Thrown>().is_some() { "thrown".to_owned() } else { error_text(&e) }
        }));
        started
    };
    let made = match made {
        Ok(made) => made,
        Err(message) if message == "thrown" => {
            let thrown = shared(context).wasm.borrow_mut().thrown.take();
            return Err(thrown.unwrap_or_else(|| JsNativeError::error().with_message("start failed").into()));
        }
        Err(message) => return Err(link_error(context, &message)),
    };
    // Its exports, as JavaScript sees them.
    let exports = object(context);
    let listed: Vec<(String, Extern)> = {
        let store = wasm.store.borrow();
        made.exports(&*store).map(|e| (e.name().to_owned(), e.into_extern())).collect()
    };
    for (name, item) in listed {
        let value: JsValue = match item {
            Extern::Func(func) => {
                let mut store = wasm.store.borrow_mut();
                let ctx = store.as_context_mut();
                export_function(context, id, func, &ctx)?.into()
            }
            Extern::Memory(memory) => {
                // An imported memory exported again is the one already kept.
                let at = {
                    let store = wasm.store.borrow();
                    let mut memories = wasm.memories.borrow_mut();
                    let pointer = memory.data_ptr(&*store);
                    match memories.iter().position(|m| m.data_ptr(&*store) == pointer) {
                        Some(at) => at,
                        None => {
                            memories.push(memory);
                            wasm.views.borrow_mut().push(None);
                            memories.len() - 1
                        }
                    }
                };
                handle(context, "Memory", id, "__memory", at as u32)?.into()
            }
            Extern::Table(table) => {
                let at = {
                    let mut tables = wasm.tables.borrow_mut();
                    tables.push(table);
                    tables.len() - 1
                };
                handle(context, "Table", id, "__table", at as u32)?.into()
            }
            Extern::Global(global) => {
                let at = {
                    let mut globals = wasm.globals.borrow_mut();
                    globals.push(global);
                    globals.len() - 1
                };
                handle(context, "Global", id, "__global", at as u32)?.into()
            }
        };
        exports.set(JsString::from(name.as_str()), value, false, context)?;
    }
    // Its memories' buffers, as they are once made.
    {
        let mut store = wasm.store.borrow_mut();
        let ctx = store.as_context_mut();
        sync_out(&wasm, &ctx, context)?;
    }
    let _ = exports.set_integrity_level(boa_engine::object::IntegrityLevel::Frozen, context);
    let object = with_proto(context, "Instance")?;
    object.set(js_string!("exports"), exports, false, context)?;
    Ok(object)
}

fn handle(context: &mut Context, class: &str, id: u32, kind: &str, index: u32) -> JsResult<JsObject> {
    let object = with_proto(context, class)?;
    hidden(&object, "__wasm", id, context)?;
    hidden(&object, kind, index, context)?;
    Ok(object)
}

// ─── WebAssembly's functions ────────────────────────────────────────────────────────────────

fn settled(context: &mut Context, outcome: JsResult<JsValue>) -> JsResult<JsValue> {
    Ok(match outcome {
        Ok(value) => JsPromise::resolve(value, context)?.into(),
        Err(error) => JsPromise::reject(error, context)?.into(),
    })
}

fn is_module(value: &JsValue, context: &mut Context) -> bool {
    hidden_number(value, "__module", context).is_some()
}

const NAMESPACE: &[(&str, usize, NativeFunctionPointer)] = &[
    ("validate", 1, |_, args, cx| {
        let bytes = bytes_from(&arg(args, 0), cx)?;
        Ok(Module::validate(&engine(cx), &bytes).is_ok().into())
    }),
    ("compile", 1, |_, args, cx| {
        let outcome = bytes_from(&arg(args, 0), cx).and_then(|b| compile(&b, cx)).map(JsValue::from);
        settled(cx, outcome)
    }),
    ("instantiate", 2, |_, args, cx| {
        let source = arg(args, 0);
        let imports = arg(args, 1);
        let outcome = if is_module(&source, cx) {
            // A module: the instance alone.
            module_of(&source, cx).and_then(|m| instantiate(&m, &imports, cx)).map(JsValue::from)
        } else {
            // Bytes: the module and its instance.
            (|| {
                let bytes = bytes_from(&source, cx)?;
                let module_object = compile(&bytes, cx)?;
                let module = module_of(&module_object.clone().into(), cx)?;
                let instance = instantiate(&module, &imports, cx)?;
                let result = object(cx);
                result.set(js_string!("module"), module_object, false, cx)?;
                result.set(js_string!("instance"), instance, false, cx)?;
                Ok(result.into())
            })()
        };
        settled(cx, outcome)
    }),
    ("compileStreaming", 1, |_, args, cx| streaming(args, cx, false)),
    ("instantiateStreaming", 2, |_, args, cx| streaming(args, cx, true)),
];

/// `compileStreaming` and `instantiateStreaming`: the response's bytes, once it has them.
fn streaming(args: &[JsValue], context: &mut Context, instantiating: bool) -> JsResult<JsValue> {
    let source = JsPromise::resolve(arg(args, 0), context)?;
    let read = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|_, args, cx| {
            let response = arg(args, 0);
            let read = response.as_object().map(|r| r.get(js_string!("arrayBuffer"), cx)).transpose()?;
            match read {
                Some(read) if read.is_callable() => crate::call(cx, &read, &response, &[]),
                _ => Err(JsNativeError::typ().with_message("WebAssembly streaming needs a Response").into()),
            }
        }),
    )
    .build();
    let bytes = source.then(Some(read), None, context)?;
    let next = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, (imports, instantiating): &(JsValue, bool), cx| {
                let namespace = cx.global_object().get(js_string!("WebAssembly"), cx)?;
                let name = if *instantiating { js_string!("instantiate") } else { js_string!("compile") };
                let f = namespace.as_object().expect("WebAssembly").get(name, cx)?;
                crate::call(cx, &f, &namespace, &[arg(args, 0), imports.clone()])
            },
            (arg(args, 1), instantiating),
        ),
    )
    .build();
    Ok(bytes.then(Some(next), None, context)?.into())
}

/// `new WebAssembly.Module(bytes)`, compiled now.
fn construct_module(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let bytes = bytes_from(&arg(args, 0), context)?;
    Ok(compile(&bytes, context)?.into())
}

/// `new WebAssembly.Instance(module, imports)`, made now.
fn construct_instance(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let module = module_of(&arg(args, 0), context)?;
    Ok(instantiate(&module, &arg(args, 1), context)?.into())
}

fn descriptor_number(descriptor: &JsValue, key: &str, context: &mut Context) -> JsResult<Option<u32>> {
    let Some(descriptor) = descriptor.as_object() else { return Ok(None) };
    let value = descriptor.get(JsString::from(key), context)?;
    if value.is_undefined() { Ok(None) } else { Ok(Some(value.to_u32(context)?)) }
}

/// `new WebAssembly.Memory({ initial, maximum })`: a memory of its own, until a module
/// imports it.
fn construct_memory(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let descriptor = arg(args, 0);
    let initial = descriptor_number(&descriptor, "initial", context)?.unwrap_or(0);
    let maximum = descriptor_number(&descriptor, "maximum", context)?;
    let ty = MemoryType::new(initial, maximum).map_err(|e| JsError::from(JsNativeError::range().with_message(e.to_string())))?;
    let (id, wasm) = new_wasm(context);
    let memory = Memory::new(&mut *wasm.store.borrow_mut(), ty)
        .map_err(|e| JsError::from(JsNativeError::range().with_message(e.to_string())))?;
    wasm.memories.borrow_mut().push(memory);
    wasm.views.borrow_mut().push(None);
    {
        let mut store = wasm.store.borrow_mut();
        let ctx = store.as_context_mut();
        sync_out(&wasm, &ctx, context)?;
    }
    Ok(handle(context, "Memory", id, "__memory", 0)?.into())
}

fn value_type(name: &str) -> Option<ValType> {
    Some(match name {
        "i32" => ValType::I32,
        "i64" => ValType::I64,
        "f32" => ValType::F32,
        "f64" => ValType::F64,
        "anyfunc" | "funcref" => ValType::FuncRef,
        "externref" => ValType::ExternRef,
        _ => return None,
    })
}

/// `new WebAssembly.Global({ value, mutable }, initial)`.
fn construct_global(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let descriptor = arg(args, 0);
    let (kind, mutable) = match descriptor.as_object() {
        Some(d) => (
            d.get(js_string!("value"), context)?.to_string(context)?.to_std_string_escaped(),
            d.get(js_string!("mutable"), context)?.to_boolean(),
        ),
        None => ("i32".to_owned(), false),
    };
    let ty = value_type(&kind).ok_or_else(|| JsError::from(JsNativeError::typ().with_message(format!("bad global type {kind}"))))?;
    let initial = arg(args, 1);
    let prepared = if initial.is_undefined() { Arg::Val(Val::default(ty)) } else { arg_for(&initial, ty, context)? };
    let (id, wasm) = new_wasm(context);
    let global = {
        let mut store = wasm.store.borrow_mut();
        let mut ctx = store.as_context_mut();
        let value = val_of(prepared, &mut ctx, &wasm);
        Global::new(&mut *store, value, if mutable { Mutability::Var } else { Mutability::Const })
    };
    wasm.globals.borrow_mut().push(global);
    Ok(handle(context, "Global", id, "__global", 0)?.into())
}

/// `new WebAssembly.Table({ element, initial, maximum })`.
fn construct_table(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let descriptor = arg(args, 0);
    let element = match descriptor.as_object() {
        Some(d) => d.get(js_string!("element"), context)?.to_string(context)?.to_std_string_escaped(),
        None => "anyfunc".to_owned(),
    };
    let element = value_type(&element).unwrap_or(ValType::FuncRef);
    let initial = descriptor_number(&descriptor, "initial", context)?.unwrap_or(0);
    let maximum = descriptor_number(&descriptor, "maximum", context)?;
    let (id, wasm) = new_wasm(context);
    let table = Table::new(&mut *wasm.store.borrow_mut(), TableType::new(element, initial, maximum), Val::default(element))
        .map_err(|e| JsError::from(JsNativeError::range().with_message(e.to_string())))?;
    wasm.tables.borrow_mut().push(table);
    Ok(handle(context, "Table", id, "__table", 0)?.into())
}

fn memory_of(this: &JsValue, context: &mut Context) -> JsResult<(Rc<Wasm>, usize)> {
    owned(this, "__memory", context).ok_or_else(|| JsNativeError::typ().with_message("not a WebAssembly.Memory").into())
}

const MEMORY_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[("grow", 1, |this, args, cx| {
    let (wasm, at) = memory_of(this, cx)?;
    let delta = arg(args, 0).to_u32(cx)?;
    let memory = wasm.memories.borrow()[at];
    let context_ptr: *mut Context = cx;
    let grown = with_store(&wasm, |mut store| {
        sync_in(&wasm, &mut store);
        let grown = memory.grow(&mut store, delta);
        // SAFETY: the pointer is `cx`, not used otherwise inside this closure.
        let context = unsafe { &mut *context_ptr };
        sync_out(&wasm, &store, context).map(|()| grown)
    })??;
    match grown {
        Ok(old) => Ok(old.into()),
        Err(error) => Err(JsNativeError::range().with_message(error.to_string()).into()),
    }
})];

const MEMORY_GETTERS: &[(&str, NativeFunctionPointer)] = &[("buffer", |this, _, cx| {
    let (wasm, at) = memory_of(this, cx)?;
    let view = wasm.views.borrow()[at].clone();
    Ok(view.map_or(JsValue::undefined(), JsValue::from))
})];

fn table_of(this: &JsValue, context: &mut Context) -> JsResult<(Rc<Wasm>, u32, Table)> {
    let (wasm, at) = owned(this, "__table", context).ok_or_else(|| JsError::from(JsNativeError::typ().with_message("not a WebAssembly.Table")))?;
    let table = wasm.tables.borrow()[at];
    let id = hidden_number(this, "__wasm", context).unwrap_or(0);
    Ok((wasm, id, table))
}

const TABLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("get", 1, |this, args, cx| {
        let (wasm, id, table) = table_of(this, cx)?;
        let index = arg(args, 0).to_u32(cx)?;
        let context_ptr: *mut Context = cx;
        with_store(&wasm, |store| {
            // SAFETY: as in `grow`.
            let context = unsafe { &mut *context_ptr };
            match table.get(&store, index) {
                Some(value) => js_of(&value, &store, id, context),
                None => Err(JsNativeError::range().with_message("table index out of bounds").into()),
            }
        })?
    }),
    ("set", 2, |this, args, cx| {
        let (wasm, _, table) = table_of(this, cx)?;
        let index = arg(args, 0).to_u32(cx)?;
        let element = with_store(&wasm, |store| table.ty(&store).element())?;
        let prepared = arg_for(&arg(args, 1), element, cx)?;
        with_store(&wasm, |mut store| {
            let value = val_of(prepared, &mut store, &wasm);
            table.set(&mut store, index, value).map_err(|e| JsError::from(JsNativeError::range().with_message(e.to_string())))
        })??;
        Ok(JsValue::undefined())
    }),
    ("grow", 2, |this, args, cx| {
        let (wasm, _, table) = table_of(this, cx)?;
        let delta = arg(args, 0).to_u32(cx)?;
        let element = with_store(&wasm, |store| table.ty(&store).element())?;
        let init = match arg(args, 1) {
            v if v.is_undefined() => Arg::Val(Val::default(element)),
            v => arg_for(&v, element, cx)?,
        };
        let grown = with_store(&wasm, |mut store| {
            let init = val_of(init, &mut store, &wasm);
            table.grow(&mut store, delta, init)
        })?;
        grown.map(JsValue::from).map_err(|e| JsNativeError::range().with_message(e.to_string()).into())
    }),
];

const TABLE_GETTERS: &[(&str, NativeFunctionPointer)] = &[("length", |this, _, cx| {
    let (wasm, _, table) = table_of(this, cx)?;
    Ok(with_store(&wasm, |store| table.size(&store))?.into())
})];

fn global_of(this: &JsValue, context: &mut Context) -> JsResult<(Rc<Wasm>, u32, Global)> {
    let (wasm, at) = owned(this, "__global", context).ok_or_else(|| JsError::from(JsNativeError::typ().with_message("not a WebAssembly.Global")))?;
    let global = wasm.globals.borrow()[at];
    let id = hidden_number(this, "__wasm", context).unwrap_or(0);
    Ok((wasm, id, global))
}

fn global_get(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (wasm, id, global) = global_of(this, context)?;
    let context_ptr: *mut Context = context;
    with_store(&wasm, |store| {
        // SAFETY: as in `grow`.
        let context = unsafe { &mut *context_ptr };
        js_of(&global.get(&store), &store, id, context)
    })?
}

fn global_set(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (wasm, _, global) = global_of(this, context)?;
    let ty = with_store(&wasm, |store| global.ty(&store))?;
    let prepared = arg_for(&arg(args, 0), ty.content(), context)?;
    with_store(&wasm, |mut store| {
        let value = val_of(prepared, &mut store, &wasm);
        global.set(&mut store, value).map_err(|e| JsError::from(JsNativeError::typ().with_message(e.to_string())))
    })??;
    Ok(JsValue::undefined())
}

const GLOBAL_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[("valueOf", 0, global_get)];

/// `WebAssembly.Module.exports(m)` and `.imports(m)`: what it offers and asks for.
fn describe(module: &Module, exports: bool, context: &mut Context) -> JsResult<JsValue> {
    fn kind(ty: &ExternType) -> &'static str {
        match ty {
            ExternType::Func(_) => "function",
            ExternType::Memory(_) => "memory",
            ExternType::Table(_) => "table",
            ExternType::Global(_) => "global",
        }
    }
    let mut out = Vec::new();
    if exports {
        for export in module.exports() {
            let entry = object(context);
            entry.set(js_string!("name"), js_text(export.name()), false, context)?;
            entry.set(js_string!("kind"), js_text(kind(export.ty())), false, context)?;
            out.push(entry.into());
        }
    } else {
        for import in module.imports() {
            let entry = object(context);
            entry.set(js_string!("module"), js_text(import.module()), false, context)?;
            entry.set(js_string!("name"), js_text(import.name()), false, context)?;
            entry.set(js_string!("kind"), js_text(kind(import.ty())), false, context)?;
            out.push(entry.into());
        }
    }
    Ok(JsArray::from_iter(out, context).into())
}

// ─── Installing it ──────────────────────────────────────────────────────────────────────────

fn function(context: &mut Context, name: &str, length: usize, f: NativeFunctionPointer) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

struct ClassSpec<'a> {
    name: &'a str,
    construct: NativeFunctionPointer,
    methods: &'a [(&'a str, usize, NativeFunctionPointer)],
    getters: &'a [(&'a str, NativeFunctionPointer)],
    proto_parent: Option<JsObject>,
}

fn class(context: &mut Context, namespace: &JsObject, spec: ClassSpec<'_>) -> JsResult<JsObject> {
    let constructor: JsObject = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(spec.construct))
        .name(JsString::from(spec.name))
        .length(1)
        .constructor(true)
        .build()
        .into();
    let prototype = object(context);
    if let Some(parent) = spec.proto_parent {
        prototype.set_prototype(Some(parent));
    }
    for (name, length, f) in spec.methods {
        let f = function(context, name, *length, *f);
        prototype.define_property_or_throw(
            JsString::from(*name),
            PropertyDescriptor::builder().value(f).writable(true).enumerable(false).configurable(true),
            context,
        )?;
    }
    for (name, f) in spec.getters {
        let f = function(context, &format!("get {name}"), 0, *f);
        prototype.define_property_or_throw(
            JsString::from(*name),
            PropertyDescriptor::builder().get(f).enumerable(true).configurable(true),
            context,
        )?;
    }
    prototype.define_property_or_throw(
        js_string!("constructor"),
        PropertyDescriptor::builder().value(constructor.clone()).writable(true).enumerable(false).configurable(true),
        context,
    )?;
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder().value(prototype).writable(false).enumerable(false).configurable(false),
        context,
    )?;
    namespace.set(JsString::from(spec.name), constructor.clone(), false, context)?;
    Ok(constructor)
}

/// The three errors: each an `Error` with its own name and prototype.
fn error_class(context: &mut Context, namespace: &JsObject, name: &'static str) -> JsResult<()> {
    let error_proto = context.intrinsics().constructors().error().prototype();
    let construct: NativeFunctionPointer = match name {
        "CompileError" => |this, args, cx| make_error(this, args, cx, "CompileError"),
        "LinkError" => |this, args, cx| make_error(this, args, cx, "LinkError"),
        _ => |this, args, cx| make_error(this, args, cx, "RuntimeError"),
    };
    let constructor = class(
        context,
        namespace,
        ClassSpec { name, construct, methods: &[], getters: &[], proto_parent: Some(error_proto) },
    )?;
    let prototype = constructor.get(js_string!("prototype"), context)?;
    if let Some(prototype) = prototype.as_object() {
        prototype.set(js_string!("name"), js_text(name), false, context)?;
        prototype.set(js_string!("message"), js_text(""), false, context)?;
    }
    Ok(())
}

fn make_error(_: &JsValue, args: &[JsValue], context: &mut Context, name: &str) -> JsResult<JsValue> {
    let message = match arg(args, 0) {
        m if m.is_undefined() => String::new(),
        m => m.to_string(context)?.to_std_string_escaped(),
    };
    let error = JsNativeError::error().with_message(message).into_opaque(context);
    let namespace = context.global_object().get(js_string!("WebAssembly"), context)?;
    if let Some(class) = namespace.as_object().map(|n| n.get(JsString::from(name), context)).transpose()?
        && let Some(class) = class.as_object()
    {
        error.set_prototype(class.get(js_string!("prototype"), context)?.as_object());
    }
    Ok(error.into())
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let namespace = object(context);
    context.register_global_property(js_string!("WebAssembly"), namespace.clone(), Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    for (name, length, f) in NAMESPACE {
        let f = function(context, name, *length, *f);
        namespace.set(JsString::from(*name), f, false, context)?;
    }
    let module = class(context, &namespace, ClassSpec { name: "Module", construct: construct_module, methods: &[], getters: &[], proto_parent: None })?;
    for (name, f) in [
        ("exports", (|_, args, cx| { let m = module_of(&arg(args, 0), cx)?; describe(&m, true, cx) }) as NativeFunctionPointer),
        ("imports", |_, args, cx| { let m = module_of(&arg(args, 0), cx)?; describe(&m, false, cx) }),
        ("customSections", |_, _, cx| Ok(JsArray::new(cx)?.into())),
    ] {
        let f = function(context, name, 1, f);
        module.set(JsString::from(name), f, false, context)?;
    }
    class(context, &namespace, ClassSpec { name: "Instance", construct: construct_instance, methods: &[], getters: &[], proto_parent: None })?;
    class(context, &namespace, ClassSpec { name: "Memory", construct: construct_memory, methods: MEMORY_METHODS, getters: MEMORY_GETTERS, proto_parent: None })?;
    class(context, &namespace, ClassSpec { name: "Table", construct: construct_table, methods: TABLE_METHODS, getters: TABLE_GETTERS, proto_parent: None })?;
    let global = class(context, &namespace, ClassSpec { name: "Global", construct: construct_global, methods: GLOBAL_METHODS, getters: &[], proto_parent: None })?;
    if let Some(prototype) = global.get(js_string!("prototype"), context)?.as_object() {
        let get = function(context, "get value", 0, global_get);
        let set = function(context, "set value", 1, global_set);
        prototype.define_property_or_throw(
            js_string!("value"),
            PropertyDescriptor::builder().get(get).set(set).enumerable(true).configurable(true),
            context,
        )?;
    }
    for name in ["CompileError", "LinkError", "RuntimeError"] {
        error_class(context, &namespace, name)?;
    }
    Ok(())
}
