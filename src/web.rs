//! The web platform's small globals that plugin bundles take for granted: `crypto`,
//! `structuredClone`, `performance`, `AbortController` and `AbortSignal`, `Blob` and `File`.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArray, JsDate, JsMap, JsPromise, JsSet, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
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

fn constructor(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .constructor(true)
        .build()
        .into()
}

/// The object `new` makes for a native constructor: `this` is the constructor `new` was
/// given — a subclass's, when there is one — and the object is made on its prototype.
fn made_by(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let made = object(context);
    if let Some(constructor) = this.as_object() {
        let prototype = constructor.get(js_string!("prototype"), context)?;
        made.set_prototype(prototype.as_object());
    }
    Ok(made)
}

/// A class: its constructor, its prototype with `methods` and `getters`, as a global.
fn class(
    context: &mut Context,
    name: &str,
    construct: NativeFunctionPointer,
    methods: &[(&str, usize, NativeFunctionPointer)],
    getters: &[(&str, NativeFunctionPointer)],
) -> JsResult<(JsObject, JsObject)> {
    let class = constructor(context, name, 0, construct);
    let prototype = object(context);
    for (method, length, f) in methods {
        let method_fn = function(context, method, *length, *f);
        prototype.define_property_or_throw(
            JsString::from(*method),
            PropertyDescriptor::builder()
                .value(method_fn)
                .writable(true)
                .enumerable(false)
                .configurable(true),
            context,
        )?;
    }
    for (getter, f) in getters {
        let get = function(context, &format!("get {getter}"), 0, *f);
        prototype.define_property_or_throw(
            JsString::from(*getter),
            PropertyDescriptor::builder()
                .get(get)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    prototype.define_property_or_throw(
        js_string!("constructor"),
        PropertyDescriptor::builder()
            .value(class.clone())
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    class.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    context.register_global_property(
        JsString::from(name),
        class.clone(),
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok((class, prototype))
}

// ─── crypto ─────────────────────────────────────────────────────────────────────────────────

fn random_bytes(count: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; count];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

fn random_uuid() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn crypto(context: &mut Context) -> JsResult<JsObject> {
    let crypto = object(context);
    let get_random_values = function(context, "getRandomValues", 1, |_, args, cx| {
        // Fills the typed array it is given, byte by byte, and answers it.
        let array = arg(args, 0);
        let Some(object) = array.as_object() else {
            return Err(type_error("getRandomValues: not a typed array"));
        };
        let length = object.get(js_string!("byteLength"), cx)?.to_number(cx)? as usize;
        if length > 65536 {
            return Err(JsNativeError::range()
                .with_message("getRandomValues: more than 65536 bytes asked for")
                .into());
        }
        let buffer = object.get(js_string!("buffer"), cx)?;
        let offset = object.get(js_string!("byteOffset"), cx)?.to_number(cx)? as usize;
        let view = JsUint8Array::from_object(
            cx.intrinsics()
                .constructors()
                .typed_uint8_array()
                .constructor()
                .construct(&[buffer, offset.into(), length.into()], None, cx)?,
        )?;
        for (at, byte) in random_bytes(length).into_iter().enumerate() {
            view.set(at, byte, true, cx)?;
        }
        Ok(array)
    });
    crypto.set(
        js_string!("getRandomValues"),
        get_random_values,
        false,
        context,
    )?;
    let uuid = function(context, "randomUUID", 0, |_, _, _| {
        Ok(js_text(&random_uuid()))
    });
    crypto.set(js_string!("randomUUID"), uuid, false, context)?;
    let subtle = object(context);
    let digest = function(context, "digest", 2, |_, args, cx| {
        use sha2::Digest;
        let algorithm = arg(args, 0);
        let name = match algorithm.as_object() {
            Some(o) => text_of(&o.get(js_string!("name"), cx)?, cx)?,
            None => text_of(&algorithm, cx)?,
        }
        .to_ascii_uppercase();
        let data = crate::encoding::bytes_of(&arg(args, 1), cx)?
            .ok_or_else(|| type_error("digest: the data is not an ArrayBuffer or a view of one"))?;
        let hash: Vec<u8> = match name.as_str() {
            "SHA-1" => sha1::Sha1::digest(&data).to_vec(),
            "SHA-256" => sha2::Sha256::digest(&data).to_vec(),
            "SHA-384" => sha2::Sha384::digest(&data).to_vec(),
            "SHA-512" => sha2::Sha512::digest(&data).to_vec(),
            other => {
                let error = JsNativeError::error()
                    .with_message(format!("digest: {other} is not supported"));
                return Ok(JsPromise::reject(JsError::from(error), cx)?.into());
            }
        };
        let buffer = JsUint8Array::from_iter(hash, cx)?.buffer(cx)?;
        Ok(JsPromise::resolve(buffer, cx)?.into())
    });
    subtle.set(js_string!("digest"), digest, false, context)?;
    crypto.set(js_string!("subtle"), subtle, false, context)?;
    Ok(crypto)
}

// ─── structuredClone ────────────────────────────────────────────────────────────────────────

/// A deep copy, as `structuredClone` makes it: plain objects, arrays, dates, maps, sets,
/// typed arrays and errors, cycles kept as cycles; functions and symbols refused.
fn clone_value(
    value: &JsValue,
    seen: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(source) = value.as_object() else {
        if value.is_symbol() {
            return Err(clone_error("a Symbol"));
        }
        return Ok(value.clone());
    };
    if let Some((_, copy)) = seen
        .iter()
        .find(|(original, _)| JsObject::equals(original, &source))
    {
        return Ok(copy.clone().into());
    }
    if source.is_callable() {
        return Err(clone_error("a function"));
    }
    if let Ok(date) = JsDate::from_object(source.clone()) {
        let copy = JsDate::new(context);
        copy.set_time(date.get_time(context)?, context)?;
        return Ok(copy.into());
    }
    if let Some(bytes) = crate::encoding::bytes_of(value, context)? {
        let is_buffer =
            boa_engine::object::builtins::JsArrayBuffer::from_object(source.clone()).is_ok();
        let copy = JsUint8Array::from_iter(bytes, context)?;
        return if is_buffer {
            copy.buffer(context)
        } else {
            Ok(copy.into())
        };
    }
    if let Ok(map) = JsMap::from_object(source.clone()) {
        let copy = JsMap::new(context);
        seen.push((source.clone(), copy.clone().into()));
        let entries = map.entries(context)?;
        let list = JsArray::from_iter(std::iter::empty(), context);
        let from = context
            .intrinsics()
            .constructors()
            .array()
            .constructor()
            .get(js_string!("from"), context)?;
        let all = crate::call(context, &from, &JsValue::undefined(), &[entries.into()])?;
        let _ = list;
        if let Some(all) = all.as_object() {
            let all = JsArray::from_object(all.clone())?;
            for at in 0..all.length(context)? {
                let pair = all.get(at, context)?;
                let pair = JsArray::from_object(
                    pair.as_object().ok_or_else(|| clone_error("a map entry"))?,
                )?;
                let key = clone_value(&pair.get(0, context)?, seen, context)?;
                let item = clone_value(&pair.get(1, context)?, seen, context)?;
                copy.set(key, item, context)?;
            }
        }
        return Ok(copy.into());
    }
    if let Ok(set) = JsSet::from_object(source.clone()) {
        let copy = JsSet::new(context);
        seen.push((source.clone(), copy.clone().into()));
        let values = set.values(context)?;
        let from = context
            .intrinsics()
            .constructors()
            .array()
            .constructor()
            .get(js_string!("from"), context)?;
        let all = crate::call(context, &from, &JsValue::undefined(), &[values.into()])?;
        if let Some(all) = all.as_object() {
            let all = JsArray::from_object(all.clone())?;
            for at in 0..all.length(context)? {
                let item = clone_value(&all.get(at, context)?, seen, context)?;
                copy.add(item, context)?;
            }
        }
        return Ok(copy.into());
    }
    let copy = if source.is_array() {
        JsArray::new(context)?.into()
    } else {
        object(context)
    };
    seen.push((source.clone(), copy.clone()));
    // Its own enumerable string keys, as `Object.keys` lists them.
    let keys_fn = context
        .intrinsics()
        .constructors()
        .object()
        .constructor()
        .get(js_string!("keys"), context)?;
    let keys = crate::call(context, &keys_fn, &JsValue::undefined(), &[value.clone()])?;
    if let Some(keys) = keys.as_object() {
        let keys = JsArray::from_object(keys)?;
        for at in 0..keys.length(context)? {
            let key = keys.get(at, context)?.to_property_key(context)?;
            let item = source.get(key.clone(), context)?;
            let item = clone_value(&item, seen, context)?;
            copy.set(key, item, false, context)?;
        }
    }
    // An error keeps its name and message.
    if source.is::<boa_engine::builtins::error::Error>() {
        for key in ["name", "message"] {
            let item = source.get(JsString::from(key), context)?;
            copy.set(JsString::from(key), item, false, context)?;
        }
    }
    Ok(copy.into())
}

fn clone_error(what: &str) -> JsError {
    JsNativeError::typ()
        .with_message(format!("DataCloneError: {what} could not be cloned"))
        .into()
}

// ─── AbortController and AbortSignal ────────────────────────────────────────────────────────

fn new_signal(context: &mut Context) -> JsResult<JsObject> {
    let class = context
        .global_object()
        .get(js_string!("AbortSignal"), context)?;
    let prototype = class
        .as_object()
        .map(|c| c.get(js_string!("prototype"), context))
        .transpose()?;
    let signal = object(context);
    if let Some(prototype) = prototype.and_then(|p| p.as_object()) {
        signal.set_prototype(Some(prototype));
    }
    signal.set(js_string!("aborted"), false, false, context)?;
    signal.set(js_string!("reason"), JsValue::undefined(), false, context)?;
    signal.set(js_string!("onabort"), JsValue::null(), false, context)?;
    let listeners = JsArray::new(context)?;
    signal.set(js_string!("__listeners"), listeners, false, context)?;
    Ok(signal)
}

/// Aborts `signal` with `reason` (an `AbortError` when none is given): `aborted`, `reason`,
/// its `onabort` and its `abort` listeners.
fn abort(signal: &JsObject, reason: JsValue, context: &mut Context) -> JsResult<()> {
    if signal.get(js_string!("aborted"), context)?.to_boolean() {
        return Ok(());
    }
    let reason = if reason.is_undefined() {
        let error = JsNativeError::error()
            .with_message("This operation was aborted")
            .into_opaque(context);
        error.set(js_string!("name"), js_string!("AbortError"), false, context)?;
        error.into()
    } else {
        reason
    };
    signal.set(js_string!("aborted"), true, false, context)?;
    signal.set(js_string!("reason"), reason, false, context)?;
    let event = object(context);
    event.set(js_string!("type"), js_string!("abort"), false, context)?;
    event.set(js_string!("target"), signal.clone(), false, context)?;
    let handler = signal.get(js_string!("onabort"), context)?;
    if handler.is_callable() {
        crate::call(
            context,
            &handler,
            &signal.clone().into(),
            &[event.clone().into()],
        )?;
    }
    let listeners = signal.get(js_string!("__listeners"), context)?;
    if let Some(listeners) = listeners.as_object() {
        let listeners = JsArray::from_object(listeners.clone())?;
        for at in 0..listeners.length(context)? {
            let listener = listeners.get(at, context)?;
            crate::call(
                context,
                &listener,
                &signal.clone().into(),
                &[event.clone().into()],
            )?;
        }
    }
    Ok(())
}

const SIGNAL_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("addEventListener", 2, |this, args, cx| {
        let kind = text_of(&arg(args, 0), cx)?;
        let listener = arg(args, 1);
        if kind == "abort"
            && listener.is_callable()
            && let Some(signal) = this.as_object()
            && let Some(list) = signal.get(js_string!("__listeners"), cx)?.as_object()
        {
            JsArray::from_object(list.clone())?.push(listener, cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("removeEventListener", 2, |this, args, cx| {
        let listener = arg(args, 1);
        if let Some(signal) = this.as_object()
            && let Some(list) = signal.get(js_string!("__listeners"), cx)?.as_object()
        {
            let list = JsArray::from_object(list.clone())?;
            let mut kept = Vec::new();
            for at in 0..list.length(cx)? {
                let item = list.get(at, cx)?;
                if item != listener {
                    kept.push(item);
                }
            }
            let fresh = JsArray::from_iter(kept, cx);
            signal.set(js_string!("__listeners"), fresh, false, cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("throwIfAborted", 0, |this, _, cx| {
        let Some(signal) = this.as_object() else {
            return Ok(JsValue::undefined());
        };
        if signal.get(js_string!("aborted"), cx)?.to_boolean() {
            return Err(JsError::from_opaque(signal.get(js_string!("reason"), cx)?));
        }
        Ok(JsValue::undefined())
    }),
];

// ─── Blob and File ──────────────────────────────────────────────────────────────────────────

/// A blob's bytes are kept as a `Uint8Array` in a property of its own.
fn blob_bytes(blob: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let data = match blob.as_object() {
        Some(b) => b.get(js_string!("__bytes"), context)?,
        None => JsValue::undefined(),
    };
    Ok(crate::encoding::bytes_of(&data, context)?.unwrap_or_default())
}

fn dress_blob(blob: &JsObject, bytes: Vec<u8>, kind: &str, context: &mut Context) -> JsResult<()> {
    let size = bytes.len();
    let data = JsUint8Array::from_iter(bytes, context)?;
    blob.define_property_or_throw(
        js_string!("__bytes"),
        PropertyDescriptor::builder()
            .value(data)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    blob.set(js_string!("size"), size, false, context)?;
    blob.set(
        js_string!("type"),
        js_text(&kind.to_ascii_lowercase()),
        false,
        context,
    )?;
    Ok(())
}

/// The bytes of a blob's parts: strings as UTF-8, buffers and views as they are, blobs as
/// theirs.
fn parts_bytes(parts: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let mut out = Vec::new();
    let Some(list) = parts.as_object() else {
        return Ok(out);
    };
    let list = JsArray::from_object(list.clone())?;
    for at in 0..list.length(context)? {
        let part = list.get(at, context)?;
        if part.is_string() {
            out.extend(text_of(&part, context)?.into_bytes());
        } else if let Some(bytes) = crate::encoding::bytes_of(&part, context)? {
            out.extend(bytes);
        } else if part.as_object().is_some_and(|o| {
            o.has_own_property(js_string!("__bytes"), context)
                .unwrap_or(false)
        }) {
            out.extend(blob_bytes(&part, context)?);
        } else {
            out.extend(text_of(&part, context)?.into_bytes());
        }
    }
    Ok(out)
}

fn kind_of(options: &JsValue, context: &mut Context) -> JsResult<String> {
    match options.as_object() {
        Some(o) => {
            let kind = o.get(js_string!("type"), context)?;
            if kind.is_undefined() {
                Ok(String::new())
            } else {
                text_of(&kind, context)
            }
        }
        None => Ok(String::new()),
    }
}

fn construct_blob(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let blob = made_by(this, context)?;
    let bytes = parts_bytes(&arg(args, 0), context)?;
    let kind = kind_of(&arg(args, 1), context)?;
    dress_blob(&blob, bytes, &kind, context)?;
    Ok(blob.into())
}

fn construct_file(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let file = made_by(this, context)?;
    let bytes = parts_bytes(&arg(args, 0), context)?;
    let options = arg(args, 2);
    let kind = kind_of(&options, context)?;
    dress_blob(&file, bytes, &kind, context)?;
    file.set(
        js_string!("name"),
        js_text(&text_of(&arg(args, 1), context)?),
        false,
        context,
    )?;
    let modified = match options.as_object() {
        Some(o) => o.get(js_string!("lastModified"), context)?,
        None => JsValue::undefined(),
    };
    let modified = if modified.is_undefined() {
        web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map_or(0., |d| d.as_millis() as f64)
            .into()
    } else {
        modified
    };
    file.set(js_string!("lastModified"), modified, false, context)?;
    Ok(file.into())
}

const BLOB_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("text", 0, |this, _, cx| {
        let bytes = blob_bytes(this, cx)?;
        Ok(JsPromise::resolve(js_text(&String::from_utf8_lossy(&bytes)), cx)?.into())
    }),
    ("arrayBuffer", 0, |this, _, cx| {
        let bytes = blob_bytes(this, cx)?;
        let buffer = JsUint8Array::from_iter(bytes, cx)?.buffer(cx)?;
        Ok(JsPromise::resolve(buffer, cx)?.into())
    }),
    ("bytes", 0, |this, _, cx| {
        let bytes = blob_bytes(this, cx)?;
        let array = JsUint8Array::from_iter(bytes, cx)?;
        Ok(JsPromise::resolve(array, cx)?.into())
    }),
    ("slice", 3, |this, args, cx| {
        let bytes = blob_bytes(this, cx)?;
        let len = bytes.len() as i64;
        let at = |value: JsValue, default: i64, cx: &mut Context| -> JsResult<usize> {
            if value.is_undefined() {
                return Ok(default as usize);
            }
            let n = value.to_number(cx)? as i64;
            Ok((if n < 0 { len + n } else { n }).clamp(0, len) as usize)
        };
        let start = at(arg(args, 0), 0, cx)?;
        let end = at(arg(args, 1), len, cx)?.max(start);
        let kind = match arg(args, 2) {
            k if k.is_undefined() => String::new(),
            k => text_of(&k, cx)?,
        };
        let class = cx.global_object().get(js_string!("Blob"), cx)?;
        let blob = made_by(&class, cx)?;
        dress_blob(&blob, bytes[start..end].to_vec(), &kind, cx)?;
        Ok(blob.into())
    }),
];

// ─── Installing them ────────────────────────────────────────────────────────────────────────

pub fn install(context: &mut Context) -> JsResult<()> {
    let crypto = crypto(context)?;
    context.register_global_property(
        js_string!("crypto"),
        crypto,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;

    let clone = function(context, "structuredClone", 1, |_, args, cx| {
        let mut seen = Vec::new();
        clone_value(&arg(args, 0), &mut seen, cx)
    });
    context.register_global_property(
        js_string!("structuredClone"),
        clone,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;

    // `performance.now()`: milliseconds on the host's clock, the timers' own.
    let performance = object(context);
    let now = function(context, "now", 0, |_, _, cx| Ok(crate::now(cx).into()));
    performance.set(js_string!("now"), now, false, context)?;
    let origin = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0., |d| d.as_millis() as f64);
    performance.set(js_string!("timeOrigin"), origin, false, context)?;
    for name in ["mark", "measure", "clearMarks", "clearMeasures"] {
        let f = function(context, name, 1, |_, _, _| Ok(JsValue::undefined()));
        performance.set(JsString::from(name), f, false, context)?;
    }
    let entries = function(context, "getEntriesByName", 1, |_, _, cx| {
        Ok(JsArray::new(cx)?.into())
    });
    performance.set(js_string!("getEntriesByName"), entries, false, context)?;
    context.register_global_property(
        js_string!("performance"),
        performance,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;

    // AbortSignal first: a controller makes one.
    let (signal_class, _) = class(
        context,
        "AbortSignal",
        |_, _, _| Err(type_error("Illegal constructor")),
        SIGNAL_METHODS,
        &[],
    )?;
    let abort_now = function(context, "abort", 1, |_, args, cx| {
        let signal = new_signal(cx)?;
        abort(&signal, arg(args, 0), cx)?;
        Ok(signal.into())
    });
    signal_class.set(js_string!("abort"), abort_now, false, context)?;
    let timeout = function(context, "timeout", 1, |_, args, cx| {
        let signal = new_signal(cx)?;
        let ms = arg(args, 0).to_number(cx)?;
        let fire = FunctionObjectBuilder::new(
            cx.realm(),
            NativeFunction::from_copy_closure_with_captures(
                |_, _, signal: &JsObject, cx| {
                    let error = JsNativeError::error()
                        .with_message("The operation timed out")
                        .into_opaque(cx);
                    error.set(js_string!("name"), js_string!("TimeoutError"), false, cx)?;
                    abort(signal, error.into(), cx)?;
                    Ok(JsValue::undefined())
                },
                signal.clone(),
            ),
        )
        .build();
        let set_timeout = cx.global_object().get(js_string!("setTimeout"), cx)?;
        crate::call(
            cx,
            &set_timeout,
            &JsValue::undefined(),
            &[fire.into(), ms.into()],
        )?;
        Ok(signal.into())
    });
    signal_class.set(js_string!("timeout"), timeout, false, context)?;
    class(
        context,
        "AbortController",
        |this, _, cx| {
            let controller = made_by(this, cx)?;
            let signal = new_signal(cx)?;
            controller.set(js_string!("signal"), signal, false, cx)?;
            Ok(controller.into())
        },
        &[("abort", 1, |this, args, cx| {
            let signal = match this.as_object() {
                Some(c) => c.get(js_string!("signal"), cx)?,
                None => JsValue::undefined(),
            };
            if let Some(signal) = signal.as_object() {
                abort(&signal, arg(args, 0), cx)?;
            }
            Ok(JsValue::undefined())
        })],
        &[],
    )?;

    let (_, blob_proto) = class(context, "Blob", construct_blob, BLOB_METHODS, &[])?;
    let (_, file_proto) = class(context, "File", construct_file, &[], &[])?;
    file_proto.set_prototype(Some(blob_proto));
    Ok(())
}
