//! Copying a value out of a realm into a [`Tree`], and a [`Tree`] into a realm again: the
//! structured clone algorithm's two halves.

use std::collections::HashMap;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string,
    object::builtins::{
        JsArray, JsArrayBuffer, JsAsyncGenerator, JsDataView, JsDate, JsGenerator, JsMap,
        JsPromise, JsProxy, JsRegExp, JsSet, JsTypedArray, JsUint8Array, JsWeakMap, JsWeakSet,
        js_typed_array_from_kind,
    },
};

use super::tree::{Tree, ViewKind, utf16};
use crate::native::type_error;
use crate::web::clone_text;

/// What has been copied so far, numbered in the order it was met.
type Seen = HashMap<JsObject, usize>;

/// The error a value that cannot be cloned is refused with, in Node's words: a `DOMException`
/// named `DataCloneError`, without the page-API prefix `structuredClone` puts on its own.
fn clone_error(context: &mut Context, value: &JsValue) -> JsError {
    let message = clone_text(context, value);
    crate::dom_events::dom_exception_error(context, &message, "DataCloneError")
}

/// `value`, out of `context`'s realm, as `structuredClone` copies it.
pub(super) fn out(value: &JsValue, context: &mut Context) -> JsResult<Tree> {
    copy(value, context, &mut Seen::new())
}

fn copy(value: &JsValue, from: &mut Context, seen: &mut Seen) -> JsResult<Tree> {
    let Some(source) = value.as_object() else {
        return primitive(value, from);
    };
    if let Some(at) = seen.get(&source) {
        return Ok(Tree::Seen(*at));
    }
    if source.is_callable() || refused(&source, from) {
        return Err(clone_error(from, value));
    }
    // Numbered before what is inside it, so what refers back to it finds it.
    let number = seen.len();
    seen.insert(source.clone(), number);
    if let Some(leaf) = leaf(value, &source, from)? {
        return Ok(leaf);
    }
    if let Some(collection) = collection(&source, from, seen)? {
        return Ok(collection);
    }
    fields(value, &source, from, seen)
}

/// Whether `source` stands for something alive — a promise, a generator, a proxy, a weak
/// collection or reference, a finalization registry — which no structured clone can copy,
/// whichever way it is asked for (`structuredClone`, a message to a worker, a message to
/// another process).
pub(crate) fn refused(source: &JsObject, context: &mut Context) -> bool {
    if JsProxy::from_object(source.clone()).is_ok() {
        return !is_env(source, context);
    }
    let constructors = context.intrinsics().constructors();
    let weak = [
        constructors.weak_ref().prototype(),
        constructors.finalization_registry().prototype(),
    ];
    JsPromise::from_object(source.clone()).is_ok()
        || JsWeakMap::from_object(source.clone()).is_ok()
        || JsWeakSet::from_object(source.clone()).is_ok()
        || JsGenerator::from_object(source.clone()).is_ok()
        || JsAsyncGenerator::from_object(source.clone()).is_ok()
        || source.prototype().is_some_and(|proto| {
            weak.iter()
                .any(|class| crate::native::inherits(&proto, class))
        })
}

/// Whether `source` is `process.env`, which is a proxy on Windows (names found whatever their
/// case) and which Node clones as the plain object of strings it stands for.
fn is_env(source: &JsObject, context: &mut Context) -> bool {
    let env = crate::native::global(context, "process")
        .and_then(|process| crate::native::get(&process, "env", context));
    env.ok()
        .and_then(|env| env.as_object())
        .is_some_and(|env| JsObject::equals(&env, source))
}

fn primitive(value: &JsValue, from: &mut Context) -> JsResult<Tree> {
    if let Some(text) = value.as_string() {
        return Ok(Tree::String(text.iter().collect()));
    }
    if value.is_symbol() {
        return Err(clone_error(from, value));
    }
    if value.is_bigint() {
        return Ok(Tree::BigInt(value.to_string(from)?.to_std_string_escaped()));
    }
    if let Some(n) = value.as_number() {
        return Ok(Tree::Number(n));
    }
    if let Some(b) = value.as_boolean() {
        return Ok(Tree::Bool(b));
    }
    Ok(if value.is_null() {
        Tree::Null
    } else {
        Tree::Undefined
    })
}

/// An object with no other values inside it: a date, a pattern, bytes, a blob.
fn leaf(value: &JsValue, source: &JsObject, from: &mut Context) -> JsResult<Option<Tree>> {
    if let Ok(date) = JsDate::from_object(source.clone()) {
        let time = date.get_time(from)?.as_number().unwrap_or(f64::NAN);
        return Ok(Some(Tree::Date(time)));
    }
    if let Ok(regexp) = JsRegExp::from_object(source.clone()) {
        return Ok(Some(Tree::RegExp {
            source: regexp.source(from)?.encode_utf16().collect(),
            flags: regexp.flags(from)?.encode_utf16().collect(),
        }));
    }
    if crate::blob::is_blob(value, from) {
        return copy_blob(value, source, from).map(Some);
    }
    if let Ok(buffer) = JsArrayBuffer::from_object(source.clone()) {
        return Ok(Some(Tree::ArrayBuffer(buffer.to_vec().unwrap_or_default())));
    }
    copy_view(value, source, from)
}

fn copy_blob(value: &JsValue, source: &JsObject, from: &mut Context) -> JsResult<Tree> {
    let bytes = crate::blob::blob_bytes(value, from)?;
    let media_type = source
        .get(js_string!("type"), from)?
        .to_string(from)?
        .to_std_string_escaped();
    let name = source
        .get(js_string!("name"), from)?
        .as_string()
        .map(|name| name.to_std_string_escaped());
    let modified = source
        .get(js_string!("lastModified"), from)?
        .as_number()
        .filter(|_| name.is_some());
    Ok(Tree::Blob {
        bytes,
        media_type,
        name,
        modified,
    })
}

/// A typed array or a `DataView`, as the bytes it shows.
fn copy_view(value: &JsValue, source: &JsObject, from: &mut Context) -> JsResult<Option<Tree>> {
    let kind = if let Ok(array) = JsTypedArray::from_object(source.clone()) {
        // A detached array has no kind to cross as.
        let kind = array.kind().ok_or_else(|| clone_error(from, value))?;
        ViewKind::of(kind).ok_or_else(|| clone_error(from, value))?
    } else if JsDataView::from_object(source.clone()).is_ok() {
        ViewKind::DataView
    } else {
        return Ok(None);
    };
    let bytes = crate::encoding::bytes_of(value, from)?.unwrap_or_default();
    Ok(Some(Tree::View { kind, bytes }))
}

/// A map, a set or an array: an object whose items are other values.
fn collection(source: &JsObject, from: &mut Context, seen: &mut Seen) -> JsResult<Option<Tree>> {
    if let Ok(map) = JsMap::from_object(source.clone()) {
        let mut pairs = Vec::new();
        for pair in entries(&map.entries(from)?.into(), from)? {
            let pair = entries(&pair, from)?;
            let key = copy(pair.first().unwrap_or(&JsValue::undefined()), from, seen)?;
            let item = copy(pair.get(1).unwrap_or(&JsValue::undefined()), from, seen)?;
            pairs.push((key, item));
        }
        return Ok(Some(Tree::Map(pairs)));
    }
    if let Ok(set) = JsSet::from_object(source.clone()) {
        let mut items = Vec::new();
        for item in entries(&set.values(from)?.into(), from)? {
            items.push(copy(&item, from, seen)?);
        }
        return Ok(Some(Tree::Set(items)));
    }
    if source.is_array() {
        let array = JsArray::from_object(source.clone())?;
        let length = array.length(from)?;
        let mut items = Vec::with_capacity(length as usize);
        for at in 0..length {
            items.push(copy(&array.get(at, from)?, from, seen)?);
        }
        return Ok(Some(Tree::Array(items)));
    }
    Ok(None)
}

/// A plain object or an error: its own enumerable properties, by name.
fn fields(
    value: &JsValue,
    source: &JsObject,
    from: &mut Context,
    seen: &mut Seen,
) -> JsResult<Tree> {
    let keys_fn = from
        .intrinsics()
        .constructors()
        .object()
        .constructor()
        .get(js_string!("keys"), from)?;
    let keys = crate::call(
        from,
        &keys_fn,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    if source.is::<boa_engine::builtins::error::Error>() {
        return copy_error(source, from, seen);
    }
    let mut names: Vec<JsString> = Vec::new();
    if let Some(keys) = keys.as_object() {
        let keys = JsArray::from_object(keys.clone())?;
        for at in 0..keys.length(from)? {
            names.push(keys.get(at, from)?.to_string(from)?);
        }
    }
    let mut fields = Vec::with_capacity(names.len());
    for name in names {
        let item = source.get(name.clone(), from)?;
        fields.push((name.iter().collect(), copy(&item, from, seen)?));
    }
    Ok(Tree::Object(fields))
}

/// The names of the errors the structured clone keeps; any other becomes `Error`.
const ERROR_NAMES: [&str; 7] = [
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
];

/// An error as the HTML structured clone carries it: its name when it is one of the standard
/// ones, its `stack` text, and its own `message` and `cause`. Anything else it has — `code`, a custom name —
/// stays behind.
fn copy_error(source: &JsObject, from: &mut Context, seen: &mut Seen) -> JsResult<Tree> {
    let name = source.get(js_string!("name"), from)?.to_string(from)?;
    let name = name.to_std_string_escaped();
    let name = if ERROR_NAMES.contains(&name.as_str()) {
        name
    } else {
        "Error".to_owned()
    };
    let mut fields = vec![(utf16("name"), Tree::String(utf16(&name)))];
    // The stack is text made when the error was, wherever the engine keeps it.
    let stack = source.get(js_string!("stack"), from)?;
    if let Some(stack) = stack.as_string() {
        fields.push((utf16("stack"), Tree::String(stack.iter().collect())));
    }
    for key in ["message", "cause"] {
        if source.has_own_property(JsString::from(key), from)? {
            let item = source.get(JsString::from(key), from)?;
            fields.push((utf16(key), copy(&item, from, seen)?));
        }
    }
    Ok(Tree::Error(fields))
}

/// The items of an array or an iterator, as a list.
pub(crate) fn entries(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let list = crate::call(
        context,
        &from,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    let Some(list) = list.as_object() else {
        return Ok(Vec::new());
    };
    let list = JsArray::from_object(list)?;
    (0..list.length(context)?)
        .map(|at| list.get(at, context))
        .collect()
}

/// `tree`, made again in `context`'s realm.
pub(super) fn into(tree: &Tree, context: &mut Context) -> JsResult<JsValue> {
    make(tree, context, &mut Vec::new())
}

/// What has been made so far, in the order it was met, for [`Tree::Seen`] to point back at.
type Made = Vec<JsObject>;

fn make(tree: &Tree, to: &mut Context, made: &mut Made) -> JsResult<JsValue> {
    Ok(match tree {
        Tree::Undefined => JsValue::undefined(),
        Tree::Null => JsValue::null(),
        Tree::Bool(b) => (*b).into(),
        Tree::Number(n) => (*n).into(),
        Tree::String(units) => JsString::from(units.as_slice()).into(),
        Tree::BigInt(text) => {
            boa_engine::JsBigInt::from_string(text).map_or(JsValue::undefined(), JsValue::from)
        }
        Tree::Seen(at) => made
            .get(*at)
            .cloned()
            .map_or(JsValue::undefined(), JsValue::from),
        Tree::Date(time) => {
            let date = JsDate::new(to);
            date.set_time(*time, to)?;
            keep(made, date.into())
        }
        Tree::RegExp { source, flags } => {
            let regexp = JsRegExp::new(
                JsString::from(source.as_slice()),
                JsString::from(flags.as_slice()),
                to,
            )?;
            keep(made, regexp.into())
        }
        Tree::Blob { .. } => make_blob(tree, to, made)?,
        Tree::ArrayBuffer(bytes) => {
            let buffer = JsUint8Array::from_iter(bytes.iter().copied(), to)?.buffer(to)?;
            if let Some(object) = buffer.as_object() {
                made.push(object.clone());
            }
            buffer
        }
        Tree::View { kind, bytes } => make_view(*kind, bytes, to, made)?,
        Tree::Map(_) | Tree::Set(_) | Tree::Array(_) => make_collection(tree, to, made)?,
        Tree::Object(fields) => {
            let object = JsObject::with_object_proto(to.intrinsics());
            made.push(object.clone());
            for (name, item) in fields {
                let item = make(item, to, made)?;
                object.set(JsString::from(name.as_slice()), item, false, to)?;
            }
            object.into()
        }
        Tree::Error(fields) => make_error(fields, to, made)?,
    })
}

/// An error of the class its name says, with the fields it was cloned with as the properties an
/// error has: not listed. A name that is not a standard one — only a clone made by hand has such a
/// name — stays as the error's own.
fn make_error(fields: &[(Vec<u16>, Tree)], to: &mut Context, made: &mut Made) -> JsResult<JsValue> {
    let name = fields.iter().find_map(|(key, item)| match item {
        Tree::String(text) if *key == utf16("name") => Some(String::from_utf16_lossy(text)),
        _ => None,
    });
    let native = match name.as_deref() {
        Some("EvalError") => JsNativeError::eval(),
        Some("RangeError") => JsNativeError::range(),
        Some("ReferenceError") => JsNativeError::reference(),
        Some("SyntaxError") => JsNativeError::syntax(),
        Some("TypeError") => JsNativeError::typ(),
        Some("URIError") => JsNativeError::uri(),
        _ => JsNativeError::error(),
    };
    let object = native.into_opaque(to);
    made.push(object.clone());
    let standard = name
        .as_deref()
        .is_none_or(|name| ERROR_NAMES.contains(&name));
    for (key, item) in fields {
        if standard && *key == utf16("name") {
            continue;
        }
        let item = make(item, to, made)?;
        crate::native::hidden(&object, JsString::from(key.as_slice()), item, to)?;
    }
    Ok(object.into())
}

/// Records `object` as made, and answers it as a value.
fn keep(made: &mut Made, object: JsObject) -> JsValue {
    made.push(object.clone());
    object.into()
}

fn make_blob(tree: &Tree, to: &mut Context, made: &mut Made) -> JsResult<JsValue> {
    let Tree::Blob {
        bytes,
        media_type,
        name,
        modified,
    } = tree
    else {
        return Ok(JsValue::undefined());
    };
    let data = JsUint8Array::from_iter(bytes.iter().copied(), to)?;
    let parts: JsValue = JsArray::from_iter([JsValue::from(data)], to).into();
    let options = crate::native::object(to);
    options.set(
        js_string!("type"),
        JsString::from(media_type.as_str()),
        false,
        to,
    )?;
    let class = crate::native::global(to, if name.is_some() { "File" } else { "Blob" })?;
    let class = class
        .as_object()
        .ok_or_else(|| type_error("Blob is not a constructor"))?;
    let blob = match name {
        Some(name) => {
            let modified = modified.unwrap_or_default();
            options.set(js_string!("lastModified"), modified, false, to)?;
            let args = [parts, JsString::from(name.as_str()).into(), options.into()];
            class.construct(&args, None, to)?
        }
        None => class.construct(&[parts, options.into()], None, to)?,
    };
    Ok(keep(made, blob))
}

fn make_view(kind: ViewKind, bytes: &[u8], to: &mut Context, made: &mut Made) -> JsResult<JsValue> {
    let buffer = JsUint8Array::from_iter(bytes.iter().copied(), to)?.buffer(to)?;
    let object = buffer
        .as_object()
        .ok_or_else(|| type_error("not a buffer"))?;
    let buffer = JsArrayBuffer::from_object(object)?;
    let view = match kind.boa() {
        Some(kind) => js_typed_array_from_kind(kind, buffer, to)?,
        None => JsDataView::from_js_array_buffer(buffer, None, None, to)?.into(),
    };
    if let Some(object) = view.as_object() {
        made.push(object.clone());
    }
    Ok(view)
}

fn make_collection(tree: &Tree, to: &mut Context, made: &mut Made) -> JsResult<JsValue> {
    Ok(match tree {
        Tree::Map(pairs) => {
            let map = JsMap::new(to);
            made.push(map.clone().into());
            for (key, item) in pairs {
                let key = make(key, to, made)?;
                let item = make(item, to, made)?;
                map.set(key, item, to)?;
            }
            map.into()
        }
        Tree::Set(items) => {
            let set = JsSet::new(to);
            made.push(set.clone().into());
            for item in items {
                let item = make(item, to, made)?;
                set.add(item, to)?;
            }
            set.into()
        }
        Tree::Array(items) => {
            let array = JsArray::new(to)?;
            made.push(array.clone().into());
            for item in items {
                let item = make(item, to, made)?;
                array.push(item, to)?;
            }
            array.into()
        }
        _ => JsValue::undefined(),
    })
}
