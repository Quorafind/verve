//! The web platform's small globals that plugin bundles take for granted: `crypto`,
//! `structuredClone`, `performance`, `Blob` and `File`.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{
            JsArray, JsArrayBuffer, JsDate, JsMap, JsPromise, JsRegExp, JsSet, JsUint8Array,
        },
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

/// `structuredClone(value, { transfer })`, and what `postMessage` copies a message with.
/// `transfer` is the transfer list, or `undefined`: its `ArrayBuffer`s are copied and then
/// detached, as moved; a `MessagePort` in it crosses as itself.
pub(crate) fn structured_clone(
    value: &JsValue,
    transfer: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let list = if transfer.is_null_or_undefined() {
        Vec::new()
    } else {
        crate::dom_events::iterate(transfer, context)?
    };
    let mut seen: Vec<(JsObject, JsObject)> = Vec::new();
    let mut moved: Vec<JsObject> = Vec::new();
    for (at, item) in list.iter().enumerate() {
        let object = item.as_object();
        if let Some(buffer) = object
            .as_ref()
            .filter(|o| JsArrayBuffer::from_object((*o).clone()).is_ok())
        {
            if moved.iter().any(|b| JsObject::equals(b, buffer)) {
                return Err(clone_error_at(
                    context,
                    &format!("ArrayBuffer at index {at} is a duplicate of an earlier ArrayBuffer."),
                ));
            }
            moved.push(buffer.clone());
        } else if let Some(port) = object.filter(crate::dom_events::is_port) {
            seen.push((port.clone(), port));
        } else {
            return Err(clone_error_at(
                context,
                &format!("Value at index {at} does not have a transferable type."),
            ));
        }
    }
    let copy = clone_value(value, &mut seen, context)?;
    for buffer in moved {
        if let Ok(buffer) = JsArrayBuffer::from_object(buffer) {
            let _ = buffer.detach(&JsValue::undefined());
        }
    }
    Ok(copy)
}

fn clone_error_at(context: &mut Context, message: &str) -> JsError {
    crate::dom_events::dom_exception_error(
        context,
        &format!("Failed to execute 'structuredClone' on 'Window': {message}"),
        "DataCloneError",
    )
}

/// What could not be cloned, as the error names it: `function f() {} could not be cloned.`
fn clone_error(context: &mut Context, value: &JsValue) -> JsError {
    let mut shown = crate::print(value);
    if shown.chars().count() > 60 {
        shown = shown.chars().take(57).collect::<String>() + "...";
    }
    clone_error_at(context, &format!("{shown} could not be cloned."))
}

/// `Object.prototype.toString.call(value)`: `[object Uint8Array]`, `[object Number]`.
fn tag_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let to_string = context
        .intrinsics()
        .constructors()
        .object()
        .prototype()
        .get(js_string!("toString"), context)?;
    text_of(&crate::call(context, &to_string, value, &[])?, context)
}

/// The typed array class named `name`.
fn typed_array_class(name: &str, context: &Context) -> Option<JsObject> {
    let constructors = context.intrinsics().constructors();
    let class = match name {
        "Int8Array" => constructors.typed_int8_array(),
        "Uint8Array" => constructors.typed_uint8_array(),
        "Uint8ClampedArray" => constructors.typed_uint8clamped_array(),
        "Int16Array" => constructors.typed_int16_array(),
        "Uint16Array" => constructors.typed_uint16_array(),
        "Int32Array" => constructors.typed_int32_array(),
        "Uint32Array" => constructors.typed_uint32_array(),
        "Float32Array" => constructors.typed_float32_array(),
        "Float64Array" => constructors.typed_float64_array(),
        "BigInt64Array" => constructors.typed_bigint64_array(),
        "BigUint64Array" => constructors.typed_biguint64_array(),
        _ => return None,
    };
    Some(class.constructor())
}

/// A deep copy, as `structuredClone` makes it: plain objects and arrays (sparse ones too),
/// dates, regular expressions, maps, sets, the wrappers of booleans, numbers, strings and
/// bigints, `ArrayBuffer`s, typed arrays of every kind and `DataView`s (views of one buffer
/// staying views of one copy), errors of each kind with their message, stack and cause,
/// `DOMException`s, blobs and files — cycles kept as cycles. Functions, symbols, promises, weak
/// collections and the runtime's event targets and events cannot be cloned: a `DataCloneError`.
fn clone_value(
    value: &JsValue,
    seen: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(source) = value.as_object() else {
        if value.is_symbol() {
            return Err(clone_error(context, value));
        }
        return Ok(value.clone());
    };
    if let Some((_, copy)) = seen
        .iter()
        .find(|(original, _)| JsObject::equals(original, &source))
    {
        return Ok(copy.clone().into());
    }
    if source.is_callable() || crate::dom_events::is_platform_object(&source) {
        return Err(clone_error(context, value));
    }
    if let Ok(date) = JsDate::from_object(source.clone()) {
        let copy = JsDate::new(context);
        copy.set_time(date.get_time(context)?, context)?;
        seen.push((source.clone(), copy.clone().into()));
        return Ok(copy.into());
    }
    if let Ok(regexp) = JsRegExp::from_object(source.clone()) {
        let pattern = JsString::from(regexp.source(context)?.as_str());
        let flags = JsString::from(regexp.flags(context)?.as_str());
        let copy = JsRegExp::new(pattern, flags, context)?;
        seen.push((source.clone(), copy.clone().into()));
        return Ok(copy.into());
    }
    if let Ok(buffer) = JsArrayBuffer::from_object(source.clone()) {
        let bytes = buffer.to_vec().unwrap_or_default();
        let copy = JsUint8Array::from_iter(bytes, context)?.buffer(context)?;
        let copy = copy.as_object().ok_or_else(|| type_error("not a buffer"))?;
        seen.push((source.clone(), copy.clone()));
        return Ok(copy.into());
    }
    if JsPromise::from_object(source.clone()).is_ok() {
        return Err(clone_error(context, value));
    }
    let exception = source
        .downcast_ref::<crate::dom_events::DomExceptionData>()
        .map(|e| (e.message.clone(), e.name.clone()));
    if let Some((message, name)) = exception {
        let copy = crate::dom_events::dom_exception(context, &message, &name);
        if let Some(copy) = copy.as_object() {
            seen.push((source.clone(), copy.clone()));
        }
        return Ok(copy);
    }
    let tag = tag_of(value, context)?;
    let kind = tag
        .trim_start_matches("[object ")
        .trim_end_matches(']')
        .to_owned();
    // A view: a typed array or a `DataView`, on a copy of its buffer.
    let is_view = typed_array_class(&kind, context).is_some() || kind == "DataView";
    if is_view && source.has_property(js_string!("buffer"), context)? {
        let buffer = clone_value(&source.get(js_string!("buffer"), context)?, seen, context)?;
        let offset = source.get(js_string!("byteOffset"), context)?;
        let (class, length) = match typed_array_class(&kind, context) {
            Some(class) => (class, source.get(js_string!("length"), context)?),
            None => (
                context
                    .intrinsics()
                    .constructors()
                    .data_view()
                    .constructor(),
                source.get(js_string!("byteLength"), context)?,
            ),
        };
        let copy = class.construct(&[buffer, offset, length], None, context)?;
        seen.push((source.clone(), copy.clone()));
        return Ok(copy.into());
    }
    match kind.as_str() {
        "WeakMap"
        | "WeakSet"
        | "WeakRef"
        | "FinalizationRegistry"
        | "Generator"
        | "AsyncGenerator"
        | "Symbol" => {
            return Err(clone_error(context, value));
        }
        // A wrapper: its primitive, wrapped again — if it really is one (a `toStringTag` can
        // claim anything).
        "Boolean" | "Number" | "String" | "BigInt" => {
            let constructors = context.intrinsics().constructors();
            let prototype = match kind.as_str() {
                "Boolean" => constructors.boolean().prototype(),
                "Number" => constructors.number().prototype(),
                "String" => constructors.string().prototype(),
                _ => constructors.bigint().prototype(),
            };
            let value_of = prototype.get(js_string!("valueOf"), context)?;
            if let Ok(primitive) = crate::call(context, &value_of, value, &[]) {
                let copy = primitive.to_object(context)?;
                seen.push((source.clone(), copy.clone()));
                return Ok(copy.into());
            }
        }
        _ => {}
    }
    if let Ok(map) = JsMap::from_object(source.clone()) {
        let copy = JsMap::new(context);
        seen.push((source.clone(), copy.clone().into()));
        let entries = crate::dom_events::iterate(&map.entries(context)?.into(), context)?;
        for pair in entries {
            let pair = pair.as_object().ok_or_else(|| type_error("a map entry"))?;
            let key = clone_value(&pair.get(0, context)?, seen, context)?;
            let item = clone_value(&pair.get(1, context)?, seen, context)?;
            copy.set(key, item, context)?;
        }
        return Ok(copy.into());
    }
    if let Ok(set) = JsSet::from_object(source.clone()) {
        let copy = JsSet::new(context);
        seen.push((source.clone(), copy.clone().into()));
        let values = crate::dom_events::iterate(&set.values(context)?.into(), context)?;
        for item in values {
            let item = clone_value(&item, seen, context)?;
            copy.add(item, context)?;
        }
        return Ok(copy.into());
    }
    if source.is::<boa_engine::builtins::error::Error>() {
        return clone_error_object(&source, seen, context);
    }
    // A blob, or a file: its bytes, type, and a file's name and date.
    if source.has_own_property(js_string!("__bytes"), context)? {
        let bytes = blob_bytes(value, context)?;
        let kind = text_of(&source.get(js_string!("type"), context)?, context)?;
        let is_file = source.has_own_property(js_string!("lastModified"), context)?;
        let class = context.global_object().get(
            JsString::from(if is_file { "File" } else { "Blob" }),
            context,
        )?;
        let copy = made_by(&class, context)?;
        dress_blob(&copy, bytes, &kind, context)?;
        if is_file {
            for key in ["name", "lastModified"] {
                let item = source.get(JsString::from(key), context)?;
                copy.set(JsString::from(key), item, false, context)?;
            }
        }
        seen.push((source.clone(), copy.clone()));
        return Ok(copy.into());
    }
    let copy = if source.is_array() {
        JsArray::new(context)?.into()
    } else {
        object(context)
    };
    seen.push((source.clone(), copy.clone()));
    if source.is_array() {
        // Its length, so a sparse array stays as long, holes and all.
        let length = source.get(js_string!("length"), context)?;
        copy.set(js_string!("length"), length, false, context)?;
    }
    copy_own(&source, &copy, seen, context)?;
    Ok(copy.into())
}

/// Copies `source`'s own enumerable string keys, as `Object.keys` lists them, onto `copy`.
fn copy_own(
    source: &JsObject,
    copy: &JsObject,
    seen: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<()> {
    let keys_fn = context
        .intrinsics()
        .constructors()
        .object()
        .constructor()
        .get(js_string!("keys"), context)?;
    let keys = crate::call(
        context,
        &keys_fn,
        &JsValue::undefined(),
        &[source.clone().into()],
    )?;
    for key in crate::native::list_of(&keys, context)? {
        let key = key.to_property_key(context)?;
        let item = source.get(key.clone(), context)?;
        let item = clone_value(&item, seen, context)?;
        copy.set(key, item, false, context)?;
    }
    Ok(())
}

/// An error, of its own kind when that is one of JavaScript's: its message, its stack, its
/// cause.
fn clone_error_object(
    source: &JsObject,
    seen: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = source.get(js_string!("name"), context)?;
    let name = if name.is_string() {
        text_of(&name, context)?
    } else {
        String::new()
    };
    let constructors = context.intrinsics().constructors();
    let class = match name.as_str() {
        "EvalError" => constructors.eval_error(),
        "RangeError" => constructors.range_error(),
        "ReferenceError" => constructors.reference_error(),
        "SyntaxError" => constructors.syntax_error(),
        "TypeError" => constructors.type_error(),
        "URIError" => constructors.uri_error(),
        _ => constructors.error(),
    }
    .constructor();
    let message = source.get(js_string!("message"), context)?;
    let args = if message.is_undefined() {
        Vec::new()
    } else {
        vec![js_text(&text_of(&message, context)?)]
    };
    let copy = class.construct(&args, None, context)?;
    seen.push((source.clone(), copy.clone()));
    let stack = source.get(js_string!("stack"), context)?;
    if stack.is_string() {
        copy.define_property_or_throw(
            js_string!("stack"),
            PropertyDescriptor::builder()
                .value(stack)
                .writable(true)
                .enumerable(false)
                .configurable(true),
            context,
        )?;
    }
    if source.has_own_property(js_string!("cause"), context)? {
        let cause = clone_value(&source.get(js_string!("cause"), context)?, seen, context)?;
        copy.define_property_or_throw(
            js_string!("cause"),
            PropertyDescriptor::builder()
                .value(cause)
                .writable(true)
                .enumerable(false)
                .configurable(true),
            context,
        )?;
    }
    Ok(copy.into())
}

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
        if args.is_empty() {
            return Err(type_error(
                "Failed to execute 'structuredClone' on 'Window': 1 argument required, but only 0 present.",
            ));
        }
        let transfer = match arg(args, 1).as_object() {
            Some(options) => options.get(js_string!("transfer"), cx)?,
            None => JsValue::undefined(),
        };
        structured_clone(&arg(args, 0), &transfer, cx)
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

    let (_, blob_proto) = class(context, "Blob", construct_blob, BLOB_METHODS, &[])?;
    let (_, file_proto) = class(context, "File", construct_file, &[], &[])?;
    file_proto.set_prototype(Some(blob_proto));
    Ok(())
}
