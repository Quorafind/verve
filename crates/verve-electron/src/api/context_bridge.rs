//! `contextBridge`: what a preload hands the page, and the rules that make it safe. The rules are
//! Electron 39.8.10's, as `tests/electron/apps/bridge-matrix` records them: a value is copied into
//! the world it goes to — an object, an array, an error, a date — or stands for itself there, as a
//! function made in that world that calls the original in its own (arguments and results crossing
//! the other way), a promise that settles when the original does, or the node's object in that
//! world. Nothing the other world can reach is the source's own object.

use std::collections::HashMap;

use boa_engine::builtins::error::Error as ErrorData;
use boa_engine::builtins::object::OrdinaryObject;
use boa_engine::object::builtins::{
    JsArray, JsFunction, JsPromise, JsProxy, JsTypedArray, JsWeakMap, JsWeakSet,
};
use boa_engine::object::{FunctionObjectBuilder, IntegrityLevel};
use boa_engine::property::{PropertyDescriptor, PropertyKey};
use boa_engine::{
    Context, Finalize, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue,
    NativeFunction, Source, Trace, js_string,
};
use verve::clone::Cloned;
use verve::native::{self, arg};
use verve::{MAIN_WORLD, WorldId, current_world, in_realm, in_world, world_realm};

use crate::preload;

/// Electron's limit on how deep an object it copies may be.
const MAX_DEPTH: usize = 1000;

/// What a thrown value that is no error becomes, in Electron's words.
const UNKNOWN: &str = "An unknown exception occurred in the isolated context, an error occurred but a valid exception was not thrown.";

/// `require('electron').contextBridge`.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let bridge = native::object(context);
    native::methods(
        context,
        &bridge,
        &[
            ("exposeInMainWorld", 2, |_, args, cx| {
                expose(MAIN_WORLD, args, cx)
            }),
            ("exposeInIsolatedWorld", 3, expose_in_isolated),
            ("executeInMainWorld", 1, execute_in_main),
        ],
    )?;
    Ok(bridge)
}

/// `exposeInIsolatedWorld(worldId, key, api)`: the world Electron numbers `worldId` is made when
/// first named; 999 is the preload's own.
fn expose_in_isolated(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = arg(args, 0).to_i32(context)?;
    let world = preload::world_numbered(i64::from(id), context)?;
    expose(world, args.get(1..).unwrap_or_default(), context)
}

fn expose(target: WorldId, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let setup = preload::setup(context);
    if !setup.prefs.context_isolation {
        return Err(native::error(
            "contextBridge API can only be used when contextIsolation is enabled",
        ));
    }
    let Some(key) = arg(args, 0).as_string() else {
        return Err(native::type_error(
            "Error processing argument at index 1, conversion failure from ",
        ));
    };
    let Some(global) = verve::world_global(context, target) else {
        return Err(native::error("no such world"));
    };
    if global.has_own_property(key.clone(), context)? {
        return Err(native::error(
            "Cannot bind an API on top of an existing property on the window object",
        ));
    }
    let value = bridge_value(&arg(args, 1), current_world(context), target, context)?;
    if let Some(api) = value.as_object() {
        freeze(&api, &mut Vec::new(), context)?;
    }
    global.define_property_or_throw(
        key,
        PropertyDescriptor::builder()
            .value(value)
            .writable(false)
            .enumerable(true)
            .configurable(false),
        context,
    )?;
    Ok(JsValue::undefined())
}

/// The exposed API cannot be changed: it and every object in it is frozen.
fn freeze(object: &JsObject, seen: &mut Vec<JsObject>, context: &mut Context) -> JsResult<()> {
    // A view's elements cannot be frozen: Electron leaves it as it is.
    let view = JsTypedArray::from_object(object.clone()).is_ok();
    if object.is_callable() || view || seen.contains(object) {
        return Ok(());
    }
    seen.push(object.clone());
    for key in object.own_property_keys(context)? {
        if let Some(inner) = object.get(key, context)?.as_object() {
            freeze(&inner, seen, context)?;
        }
    }
    object.set_integrity_level(IntegrityLevel::Frozen, context)?;
    Ok(())
}

/// `executeInMainWorld({ func, args })`: `func` runs in the page's world, given `args`, and what
/// it answers crosses back.
fn execute_in_main(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(spec) = arg(args, 0).as_object() else {
        return Err(native::type_error("executeInMainWorld expects an object"));
    };
    let func = spec.get(js_string!("func"), context)?;
    let Some(source) = func.as_object().filter(JsObject::is_callable) else {
        return Err(native::type_error("func must be a function"));
    };
    let from = current_world(context);
    let text = JsValue::from(source)
        .to_string(context)?
        .to_std_string_escaped();
    let given = spec.get(js_string!("args"), context)?;
    let given = if given.is_undefined() {
        Vec::new()
    } else {
        native::list_of(&given, context)?
    };
    let mut crossed = Vec::with_capacity(given.len());
    for value in &given {
        crossed.push(bridge_value(value, from, MAIN_WORLD, context)?);
    }
    let result = in_world(context, MAIN_WORLD, |cx| {
        let function = cx.eval(Source::from_bytes(&format!("({text})")))?;
        verve::call(cx, &function, &JsValue::undefined(), &crossed)
    });
    match result {
        Ok(value) => bridge_value(&value, MAIN_WORLD, from, context),
        // What the page's function threw crosses as a thrown value does anywhere else.
        Err(error) => {
            let thrown = thrown_value(&error, context);
            Err(JsError::from_opaque(in_world(context, from, |cx| {
                thrown_across(&thrown, cx)
            })))
        }
    }
}

// ---------- values ----------

/// Which function stands for which, so that one that comes back is the function it was made from.
struct Proxies(JsWeakMap);

fn proxies(context: &mut Context) -> JsWeakMap {
    if let Some(Proxies(map)) = context.get_data::<Proxies>() {
        return map.clone();
    }
    let map = JsWeakMap::new(context);
    context.insert_data(Proxies(map.clone()));
    map
}

/// What a bridged function calls: the function, and the worlds the call goes between.
#[derive(Trace, Finalize)]
struct Crossing {
    original: JsObject,
    #[unsafe_ignore_trace]
    from: WorldId,
    #[unsafe_ignore_trace]
    to: WorldId,
}

/// `value`, as the world `to` has it, coming from the world `from`.
///
/// # Errors
///
/// What cannot cross: `An object could not be cloned.`, or an object nested deeper than
/// Electron copies.
pub(crate) fn bridge_value(
    value: &JsValue,
    from: WorldId,
    to: WorldId,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(realm) = world_realm(context, to) else {
        return Err(native::error("no such world"));
    };
    in_realm(context, &realm, |cx| {
        let mut walk = Walk {
            from,
            to,
            seen: HashMap::new(),
            depth: 0,
        };
        walk.value(value, cx)
    })
}

struct Walk {
    from: WorldId,
    to: WorldId,
    /// The copy made of each object met, so that a cycle or an object met twice is one copy.
    seen: HashMap<JsObject, JsObject>,
    depth: usize,
}

impl Walk {
    fn value(&mut self, value: &JsValue, context: &mut Context) -> JsResult<JsValue> {
        let Some(source) = value.as_object() else {
            return Ok(value.clone());
        };
        if let Some(copy) = self.seen.get(&source) {
            return Ok(copy.clone().into());
        }
        if self.depth >= MAX_DEPTH {
            return Err(JsNativeError::typ()
                .with_message("Electron contextBridge recursion depth exceeded.  Nested objects deeper than 1000 are not supported.")
                .into());
        }
        self.depth += 1;
        let copy = self.cross(&source, context);
        self.depth -= 1;
        copy
    }

    fn cross(&mut self, source: &JsObject, context: &mut Context) -> JsResult<JsValue> {
        if source.is_callable() {
            return proxy_function(source, self.from, self.to, context);
        }
        if let Ok(promise) = JsPromise::from_object(source.clone()) {
            return proxy_promise(&promise, self.from, self.to, context);
        }
        if let Some(node) = counterpart(source, context) {
            return Ok(node);
        }
        if JsArray::from_object(source.clone()).is_ok() {
            return self.array(source, context);
        }
        if source.is::<ErrorData>() {
            return copy_error(source, context);
        }
        if JsProxy::from_object(source.clone()).is_ok()
            || JsWeakMap::from_object(source.clone()).is_ok()
            || JsWeakSet::from_object(source.clone()).is_ok()
        {
            return Err(native::error("An object could not be cloned."));
        }
        let value: JsValue = source.clone().into();
        if verve::blob::is_blob(&value, context) {
            return self
                .read(context, |cx| blob_parts(&value, cx))
                .and_then(|parts| parts.make(context));
        }
        if let Some(boxed) = boxed_primitive(source) {
            return Ok(boxed.to_object(context)?.into());
        }
        if !source.is::<OrdinaryObject>() {
            // A date, an expression, a map, a buffer: copied as `structuredClone` does; what it
            // cannot copy (a platform object) is copied as a plain object.
            if let Ok(cloned) = self.read(context, |cx| Cloned::from_value(&value, cx)) {
                return cloned.to_value(context);
            }
        }
        self.plain(source, context)
    }

    /// Runs `f`, which reads the source, with the source's world running: nothing of the
    /// destination's — a built-in the page may have changed — is ever given the source's objects.
    fn read<R>(&self, context: &mut Context, f: impl FnOnce(&mut Context) -> R) -> R {
        in_world(context, self.from, f)
    }

    fn array(&mut self, source: &JsObject, context: &mut Context) -> JsResult<JsValue> {
        let copy = JsArray::new(context)?;
        self.seen.insert(source.clone(), copy.clone().into());
        let length = source
            .get(js_string!("length"), context)?
            .to_length(context)?;
        for at in 0..length {
            let item = source.get(at, context)?;
            let item = self.value(&item, context)?;
            copy.push(item, context)?;
        }
        Ok(copy.into())
    }

    /// An object, as a plain one of the world it goes to: its own enumerable properties, string
    /// and symbol keyed, getters read, the prototype left behind.
    fn plain(&mut self, source: &JsObject, context: &mut Context) -> JsResult<JsValue> {
        let copy = JsObject::with_object_proto(context.intrinsics());
        self.seen.insert(source.clone(), copy.clone());
        for key in source.own_property_keys(context)? {
            if !self.read(context, |cx| is_enumerable(source, &key, cx))? {
                continue;
            }
            let item = source.get(key.clone(), context)?;
            let item = self.value(&item, context)?;
            copy.create_data_property_or_throw(key, item, context)?;
        }
        Ok(copy.into())
    }
}

/// Whether `key` is an own enumerable property of `source`, asked with the running world's
/// `Object.prototype.propertyIsEnumerable`: an object with no prototype has no such method. The
/// running world is the source's.
fn is_enumerable(source: &JsObject, key: &PropertyKey, context: &mut Context) -> JsResult<bool> {
    let prototype = context.intrinsics().constructors().object().prototype();
    let ask = prototype.get(js_string!("propertyIsEnumerable"), context)?;
    let key = JsValue::from(key.clone());
    Ok(verve::call(context, &ask, &source.clone().into(), &[key])?.to_boolean())
}

/// The primitive inside a `new Number(1)`, `new String('a')` or `new Boolean(true)`.
fn boxed_primitive(object: &JsObject) -> Option<JsValue> {
    if let Some(number) = object.downcast_ref::<f64>() {
        return Some((*number).into());
    }
    if let Some(flag) = object.downcast_ref::<bool>() {
        return Some((*flag).into());
    }
    object
        .downcast_ref::<JsString>()
        .map(|text| text.clone().into())
}

/// A `Blob` or `File`, read in its own world.
enum BlobParts {
    /// A `File` found on disk: its bytes, name, type and the path it was found at.
    OnDisk(Vec<u8>, String, String, std::path::PathBuf),
    Plain(Cloned),
}

/// What a `Blob` or `File` is, read with the world it belongs to running.
fn blob_parts(blob: &JsValue, context: &mut Context) -> JsResult<BlobParts> {
    let name = blob.as_object().map(|o| o.get(js_string!("name"), context));
    let name = name.transpose()?.filter(|n| n.is_string());
    let path = verve::blob::path_of_file(blob, context);
    match (path, name) {
        (Some(path), Some(name)) => {
            let bytes = verve::blob::blob_bytes(blob, context)?;
            let kind = native::get(blob, "type", context)?;
            let kind = native::text_of(&kind, context)?;
            let name = native::text_of(&name, context)?;
            Ok(BlobParts::OnDisk(bytes, name, kind, path))
        }
        _ => Ok(BlobParts::Plain(Cloned::from_value(blob, context)?)),
    }
}

impl BlobParts {
    /// The blob as one of the running world's, a `File` found on disk still knowing where.
    fn make(self, context: &mut Context) -> JsResult<JsValue> {
        match self {
            Self::OnDisk(bytes, name, kind, path) => {
                verve::blob::file_with_path(context, bytes, &name, &kind, &path)
            }
            Self::Plain(cloned) => cloned.to_value(context),
        }
    }
}

/// An `Error` of the world that is running with the message and stack of `source`'s: what an
/// error is when it crosses, whatever it was.
fn copy_error(source: &JsObject, context: &mut Context) -> JsResult<JsValue> {
    let message = source
        .get(js_string!("message"), context)?
        .to_string(context)
        .map(|m| m.to_std_string_escaped())
        .unwrap_or_default();
    let error = JsNativeError::error()
        .with_message(message)
        .into_opaque(context);
    let stack = source.get(js_string!("stack"), context)?;
    if stack.is_string() {
        error.set(js_string!("stack"), stack, false, context)?;
    }
    Ok(error.into())
}

/// What something thrown is, as the world that is running has it: an `Error` of its own for an
/// error, the unknown one for anything else.
fn thrown_across(thrown: &JsValue, context: &mut Context) -> JsValue {
    match thrown.as_object() {
        Some(object) if object.is::<ErrorData>() => copy_error(&object, context),
        _ => Ok(JsNativeError::error()
            .with_message(UNKNOWN)
            .into_opaque(context)
            .into()),
    }
    .unwrap_or_else(|_| {
        JsNativeError::error()
            .with_message(UNKNOWN)
            .into_opaque(context)
            .into()
    })
}

/// The object a node (or the window) of the world the value comes from has in the world that
/// is running.
fn counterpart(object: &JsObject, context: &mut Context) -> Option<JsValue> {
    let value: JsValue = object.clone().into();
    if let Some(node) = verve_dom::host::node_of(&value) {
        return Some(verve_dom::host::wrap(context, node));
    }
    verve::world_of_global(context, object)?;
    Some(context.global_object().into())
}

// ---------- functions ----------

/// A function of the running world that calls `original`, of the world `from`: the arguments
/// cross to `from`, the result and what is thrown cross back. `this` is not handed on. A function
/// that is itself such a stand-in is not wrapped again, but replaced by its original.
fn proxy_function(
    original: &JsObject,
    from: WorldId,
    to: WorldId,
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some((source, world)) = original_of(original, context)? {
        if world == to {
            return Ok(source.into());
        }
        return proxy_function(&source, world, to, context);
    }
    let proxy = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, crossing: &Crossing, context| call_across(crossing, args, context),
            Crossing {
                original: original.clone(),
                from,
                to,
            },
        ),
    )
    .name(js_string!(""))
    .length(0)
    .build();
    let record = JsArray::from_iter([original.clone().into(), JsValue::from(from)], context);
    proxies(context).set(&proxy, record.into(), context)?;
    Ok(proxy.into())
}

/// The function a stand-in was made from, and the world it belongs to.
fn original_of(
    function: &JsObject,
    context: &mut Context,
) -> JsResult<Option<(JsObject, WorldId)>> {
    let record = proxies(context).get(function, context)?;
    let Some(record) = record.as_object() else {
        return Ok(None);
    };
    let original = record.get(0, context)?.as_object();
    let world = record.get(1, context)?.to_u32(context)?;
    Ok(original.map(|original| (original, world)))
}

fn call_across(crossing: &Crossing, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (from, to) = (crossing.from, crossing.to);
    let mut converted = Vec::with_capacity(args.len());
    for given in args {
        converted.push(bridge_value(given, to, from, context)?);
    }
    let called = crossing
        .original
        .call(&JsValue::undefined(), &converted, context);
    match called {
        Ok(result) => bridge_value(&result, from, to, context).map_err(|error| {
            let text = error_text(&error, context);
            native::error(format!("Uncaught {text}"))
        }),
        Err(error) => {
            let thrown = thrown_value(&error, context);
            Err(JsError::from_opaque(in_world(context, to, |cx| {
                thrown_across(&thrown, cx)
            })))
        }
    }
}

/// What was thrown, as the value `catch` would have.
fn thrown_value(error: &JsError, context: &mut Context) -> JsValue {
    error.clone().into_opaque(context).unwrap_or_default()
}

/// `name: message` of an error, as a console prints the start of it.
fn error_text(error: &JsError, context: &mut Context) -> String {
    let (name, message) = preload::error_parts(error, context);
    format!("{name}: {message}")
}

// ---------- promises ----------

/// A promise of the running world that settles with `source`'s, its value or reason crossing.
fn proxy_promise(
    source: &JsPromise,
    from: WorldId,
    to: WorldId,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let settlers: (JsObject, JsObject) = (
        resolvers.resolve.clone().into(),
        resolvers.reject.clone().into(),
    );
    let settle = |resolve: bool, context: &mut Context| -> JsResult<JsFunction> {
        let function = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure_with_captures(
                move |_, args, (on_resolve, on_reject): &(JsObject, JsObject), context| {
                    let value = arg(args, 0);
                    let crossed = if resolve {
                        bridge_value(&value, from, to, context)
                            .map_err(|e| thrown_value(&e, context))
                    } else {
                        Err(in_world(context, to, |cx| thrown_across(&value, cx)))
                    };
                    let (settler, outcome) = match crossed {
                        Ok(value) => (on_resolve, value),
                        Err(reason) => (on_reject, reason),
                    };
                    settler.call(&JsValue::undefined(), &[outcome], context)
                },
                settlers.clone(),
            ),
        )
        .build();
        JsFunction::from_object(function.into()).ok_or_else(|| native::error("not a function"))
    };
    let (on_value, on_reason) = (settle(true, context)?, settle(false, context)?);
    // The page's promise is not subscribed to directly: `then` asks the promise for the
    // constructor to derive with, which is the page's to choose, and boa takes the answer to be
    // one that works. A promise of the promise's own world is resolved with it instead (the page
    // sees only its own functions), and that one is subscribed to, told to derive with the
    // engine's own constructor.
    in_world(context, from, |cx| -> JsResult<()> {
        let (relay, resolvers) = JsPromise::new_pending(cx);
        let plain = JsObject::with_null_proto();
        JsObject::from(relay.clone()).create_data_property_or_throw(
            js_string!("constructor"),
            plain,
            cx,
        )?;
        resolvers
            .resolve
            .call(&JsValue::undefined(), &[source.clone().into()], cx)?;
        relay.then(Some(on_value), Some(on_reason), cx)?;
        Ok(())
    })?;
    Ok(promise.into())
}
