//! The web platform's small globals that plugin bundles take for granted:
//! `structuredClone`, `performance`, `Blob` and `File`.

use boa_engine::{
    Context, JsError, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{
        JsArray, JsArrayBuffer, JsDate, JsMap, JsPromise, JsRegExp, JsSet, JsUint8Array,
    },
    property::{Attribute, PropertyDescriptor},
};

use crate::native::{
    PlainClass, arg, arg_text, define_global, function, js_text, object, plain_class, text_of,
    type_error,
};

/// A class of this module: no getters, a constructor of no parameters.
fn global_class<'a>(
    name: &'a str,
    construct: NativeFunctionPointer,
    methods: &'a [(&'a str, usize, NativeFunctionPointer)],
) -> PlainClass<'a> {
    PlainClass {
        name,
        construct,
        length: 0,
        parent: None,
        methods,
        getters: &[],
    }
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

pub(crate) fn clone_error_at(context: &mut Context, message: &str) -> JsError {
    crate::dom_events::dom_exception_error(
        context,
        &format!("Failed to execute 'structuredClone' on 'Window': {message}"),
        "DataCloneError",
    )
}

/// What could not be cloned, as the error names it: `function f() {} could not be cloned.`
pub(crate) fn clone_error(context: &mut Context, value: &JsValue) -> JsError {
    let text = clone_text(context, value);
    clone_error_at(context, &text)
}

/// What is said of `value` when it cannot be cloned: `() => 1 could not be cloned.`, as Node
/// words it (the text of a function, `#<Promise>` for an object).
pub(crate) fn clone_text(context: &mut Context, value: &JsValue) -> String {
    let mut shown = match value.as_object() {
        Some(object) if object.is_callable() => {
            crate::native::text_of(value, context).unwrap_or_else(|_| crate::print(value))
        }
        Some(_) => {
            let class = crate::native::get(value, "constructor", context)
                .and_then(|c| crate::native::get(&c, "name", context))
                .map(|name| crate::print(&name))
                .unwrap_or_default();
            format!("#<{}>", if class.is_empty() { "Object" } else { &class })
        }
        None => crate::print(value),
    };
    if shown.chars().count() > 60 {
        shown = shown.chars().take(57).collect::<String>() + "...";
    }
    format!("{shown} could not be cloned.")
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
    if source.is_callable()
        || crate::clone::refused(&source, context)
        || crate::dom_events::is_platform_object(&source)
    {
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
        "Symbol" => return Err(clone_error(context, value)),
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
pub fn blob_bytes(blob: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let data = match blob.as_object() {
        Some(b) => b.get(js_string!("__bytes"), context)?,
        None => JsValue::undefined(),
    };
    Ok(crate::encoding::bytes_of(&data, context)?.unwrap_or_default())
}

/// Whether `value` is a `Blob` (or a `File`) this runtime made.
pub fn is_blob(value: &JsValue, context: &mut Context) -> bool {
    value.as_object().is_some_and(|o| {
        o.has_own_property(js_string!("__bytes"), context)
            .unwrap_or(false)
    })
}

/// A new `Blob` of `bytes` and MIME type `kind`, as `new Blob([bytes], { type })` makes it.
pub fn new_blob(bytes: Vec<u8>, kind: &str, context: &mut Context) -> JsResult<JsValue> {
    let class = crate::native::global(context, "Blob")?;
    let blob = made_by(&class, context)?;
    dress_blob(&blob, bytes, kind, context)?;
    Ok(blob.into())
}

/// Which file on the host's disk each `File` made by [`file_with_path`] is, kept where script
/// cannot see it: a weak map that only Rust reads.
struct FilePaths(boa_engine::object::builtins::JsWeakMap);

/// A new `File` of `bytes` named `name`, of MIME type `kind`, that the host found at `path`
/// (a file the user picked or dropped). Script cannot read the path; the host asks
/// [`path_of_file`], which is how Electron's `webUtils.getPathForFile` answers. A `File` the
/// page makes with `new File()` has no path.
///
/// # Errors
///
/// The runtime has no `File` class to make one with.
pub fn file_with_path(
    context: &mut Context,
    bytes: Vec<u8>,
    name: &str,
    kind: &str,
    path: &std::path::Path,
) -> JsResult<JsValue> {
    let class = crate::native::global(context, "File")?;
    let file = made_by(&class, context)?;
    dress_blob(&file, bytes, kind, context)?;
    file.set(js_string!("name"), js_text(name), false, context)?;
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(web_time::UNIX_EPOCH).ok())
        .map_or(0., |since| since.as_millis() as f64);
    file.set(js_string!("lastModified"), modified, false, context)?;
    let map = match context.get_data::<FilePaths>() {
        Some(FilePaths(map)) => map.clone(),
        None => {
            let map = boa_engine::object::builtins::JsWeakMap::new(context);
            context.insert_data(FilePaths(map.clone()));
            map
        }
    };
    let shown = js_text(&path.to_string_lossy());
    map.set(&file, shown, context)?;
    Ok(file.into())
}

/// Where the host found `file`, when it was made by [`file_with_path`] (in any world).
pub fn path_of_file(file: &JsValue, context: &mut Context) -> Option<std::path::PathBuf> {
    let object = file.as_object()?;
    let FilePaths(map) = context.get_data::<FilePaths>()?;
    let map = map.clone();
    let path = map.get(&object, context).ok()?;
    let path = path.as_string()?.to_std_string_escaped();
    Some(path.into())
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
        js_text(&arg_text(args, 1, context)?),
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
    // A `ReadableStream` of its bytes, in chunks of 64 KiB.
    ("stream", 0, |this, _, cx| {
        let bytes = blob_bytes(this, cx)?;
        let chunks = bytes.chunks(65536).map(<[u8]>::to_vec).collect();
        Ok(crate::streams::readable_of_chunks(cx, chunks)?.into())
    }),
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
    // No timeline is kept, so recording an entry records nothing. That
    // includes Node's `markResourceTiming`, which undici's `fetch` calls as
    // every response body ends.
    for name in [
        "mark",
        "measure",
        "clearMarks",
        "clearMeasures",
        "clearResourceTimings",
        "markResourceTiming",
    ] {
        let f = function(context, name, 1, |_, _, _| Ok(JsValue::undefined()));
        performance.set(JsString::from(name), f, false, context)?;
    }
    for name in ["getEntries", "getEntriesByName", "getEntriesByType"] {
        let entries = function(context, name, 1, |_, _, cx| Ok(JsArray::new(cx)?.into()));
        performance.set(JsString::from(name), entries, false, context)?;
    }
    context.register_global_property(
        js_string!("performance"),
        performance,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;

    let (blob, blob_proto) =
        plain_class(context, global_class("Blob", construct_blob, BLOB_METHODS))?;
    define_global(context, "Blob", blob)?;
    let (file, file_proto) = plain_class(context, global_class("File", construct_file, &[]))?;
    define_global(context, "File", file)?;
    file_proto.set_prototype(Some(blob_proto));
    Ok(())
}
