//! Copying a value out of a realm into a [`Tree`], and a [`Tree`] into a realm again: the
//! structured clone algorithm's two halves.

use std::collections::HashMap;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string,
    object::builtins::{
        JsArray, JsArrayBuffer, JsDataView, JsDate, JsMap, JsRegExp, JsSet, JsTypedArray,
        JsUint8Array, js_typed_array_from_kind,
    },
};

use super::tree::{Tree, ViewKind};
use crate::native::type_error;

/// What has been copied so far, numbered in the order it was met.
type Seen = HashMap<JsObject, usize>;

fn data_clone_error(what: &str) -> JsError {
    type_error(format!("DataCloneError: {what} could not be cloned"))
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
    if source.is_callable() {
        return Err(data_clone_error("a function"));
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

fn primitive(value: &JsValue, from: &mut Context) -> JsResult<Tree> {
    if let Some(text) = value.as_string() {
        return Ok(Tree::String(text.iter().collect()));
    }
    if value.is_symbol() {
        return Err(data_clone_error("a Symbol"));
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
        let kind = array
            .kind()
            .ok_or_else(|| data_clone_error("a detached typed array"))?;
        ViewKind::of(kind).ok_or_else(|| data_clone_error("a typed array of this kind"))?
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
    let error = source.is::<boa_engine::builtins::error::Error>();
    let mut names: Vec<JsString> = Vec::new();
    if let Some(keys) = keys.as_object() {
        let keys = JsArray::from_object(keys.clone())?;
        for at in 0..keys.length(from)? {
            names.push(keys.get(at, from)?.to_string(from)?);
        }
    }
    // An error keeps its name and message, which are not its own enumerable properties.
    if error {
        for key in [js_string!("name"), js_string!("message")] {
            if !names.contains(&key) {
                names.push(key);
            }
        }
    }
    let mut fields = Vec::with_capacity(names.len());
    for name in names {
        let item = source.get(name.clone(), from)?;
        fields.push((name.iter().collect(), copy(&item, from, seen)?));
    }
    Ok(if error {
        Tree::Error(fields)
    } else {
        Tree::Object(fields)
    })
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
        Tree::Object(fields) | Tree::Error(fields) => {
            let object = match tree {
                Tree::Error(_) => JsNativeError::error().into_opaque(to),
                _ => JsObject::with_object_proto(to.intrinsics()),
            };
            made.push(object.clone());
            for (name, item) in fields {
                let item = make(item, to, made)?;
                object.set(JsString::from(name.as_slice()), item, false, to)?;
            }
            object.into()
        }
    })
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
