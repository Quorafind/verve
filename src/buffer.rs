//! Node's `Buffer`, and `require('buffer')`.
//!
//! A `Buffer` is a `Uint8Array`, as Node's is: its prototype is `Buffer.prototype`, which
//! inherits `Uint8Array.prototype`, so indexing, `length`, `map` and the rest are the engine's
//! own, and only what Node adds is here — the encodings, reading and writing numbers at an
//! offset (`readUInt32BE`, `writeDoubleLE`, `readBigUInt64LE`…), `copy`, `fill`, `indexOf`,
//! `swap16`, `compare`, and the rest. Bytes are read and written where they lie, in the
//! `ArrayBuffer` under the view, not copied out first: a parser reading a large file a number at
//! a time costs what the number does.
//!
//! Errors are Node's: an offset out of range is a `RangeError` with `code: 'ERR_OUT_OF_RANGE'`
//! and Node's message, and so on.

use boa_engine::{
    Context, JsBigInt, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsArray, JsArrayBuffer, JsTypedArray, JsUint8Array},
    property::PropertyDescriptor,
};

use crate::encoding::{base64, bytes_of};
use crate::native::{arg, function, get, hidden, js_text, object, text_of};

// ─── Errors as Node words them ───────────────────────────────────────────────────────────────

fn coded(kind: JsNativeError, message: String, code: &str, context: &mut Context) -> JsError {
    let error = kind.with_message(message).into_opaque(context);
    let _ = error.set(js_string!("code"), JsString::from(code), false, context);
    JsError::from_opaque(error.into())
}

fn out_of_range(context: &mut Context, what: &str, range: &str, received: &JsValue) -> JsError {
    let received = describe_received(received);
    coded(
        JsNativeError::range(),
        format!("The value of \"{what}\" is out of range. It must be {range}. Received {received}"),
        "ERR_OUT_OF_RANGE",
        context,
    )
}

fn invalid_type(context: &mut Context, message: String) -> JsError {
    coded(
        JsNativeError::typ(),
        message,
        "ERR_INVALID_ARG_TYPE",
        context,
    )
}

fn bounds(context: &mut Context) -> JsError {
    coded(
        JsNativeError::range(),
        "Attempt to access memory outside buffer bounds".into(),
        "ERR_BUFFER_OUT_OF_BOUNDS",
        context,
    )
}

/// `1_000_000`: digits in threes, as Node writes a large number it was given.
fn grouped(digits: &str) -> String {
    let (sign, digits) = digits.strip_prefix('-').map_or(("", digits), |d| ("-", d));
    let mut out = String::new();
    for (at, c) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push('_');
        }
        out.push(c);
    }
    format!("{sign}{out}")
}

/// What a value was, as Node's errors say it: integers past 2³² grouped in threes.
fn describe_received(value: &JsValue) -> String {
    if let Some(n) = value.as_number() {
        if n.fract() == 0. && n.abs() > 4_294_967_296. && n.is_finite() {
            return grouped(&format!("{n:.0}"));
        }
        return crate::print(value);
    }
    if let Some(n) = value.as_bigint() {
        let digits = n.to_string_radix(10);
        let large = n.to_i128().unsigned_abs() > 1u128 << 32;
        return format!("{}n", if large { grouped(&digits) } else { digits });
    }
    if value.is_string() {
        return format!("type string ('{}')", crate::print(value));
    }
    format!("type {}", value.type_of())
}

// ─── Encodings ───────────────────────────────────────────────────────────────────────────────

/// An encoding's one name: `utf8`, `ucs2`, `latin1`, `ascii`, `hex`, `base64`, `base64url`.
fn encoding_name(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "utf8" | "utf-8" => "utf8",
        "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => "ucs2",
        "latin1" | "binary" => "latin1",
        "ascii" => "ascii",
        "hex" => "hex",
        "base64" => "base64",
        "base64url" => "base64url",
        _ => return None,
    })
}

/// The encoding an argument names: `utf8` when it is left out.
fn encoding_arg(value: &JsValue, context: &mut Context) -> JsResult<&'static str> {
    if value.is_null_or_undefined() {
        return Ok("utf8");
    }
    let name = text_of(value, context)?;
    encoding_name(&name).ok_or_else(|| {
        coded(
            JsNativeError::typ(),
            format!("Unknown encoding: {name}"),
            "ERR_UNKNOWN_ENCODING",
            context,
        )
    })
}

/// Base64 as Node reads it: either alphabet, padding or none, anything else skipped.
fn unbase64_lenient(text: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for &unit in text {
        let value = match unit {
            0x41..=0x5a => unit - 0x41,
            0x61..=0x7a => unit - 0x61 + 26,
            0x30..=0x39 => unit - 0x30 + 52,
            0x2b | 0x2d => 62,
            0x2f | 0x5f => 63,
            0x3d => break,
            _ => continue,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

/// Hex as Node reads it: pairs of digits, up to the first pair that is not one.
fn unhex(text: &[u16]) -> Vec<u8> {
    let digit = |u: u16| char::from_u32(u32::from(u)).and_then(|c| c.to_digit(16));
    text.chunks_exact(2)
        .map_while(|pair| Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8))
        .collect()
}

/// What a string is in `encoding`. UTF-8 writes a lone surrogate as U+FFFD, as Node does;
/// `latin1` and `ascii` keep each UTF-16 unit's low byte.
pub(crate) fn string_bytes(text: &JsString, encoding: &str) -> Vec<u8> {
    match encoding {
        "ucs2" => text.iter().flat_map(u16::to_le_bytes).collect(),
        "latin1" | "ascii" => text.iter().map(|u| u as u8).collect(),
        "hex" => unhex(&text.to_vec()),
        "base64" | "base64url" => unbase64_lenient(&text.to_vec()),
        _ => text.to_std_string_lossy().into_bytes(),
    }
}

/// The bytes read as text in `encoding`.
pub(crate) fn bytes_string(data: &[u8], encoding: &str) -> JsString {
    match encoding {
        "ucs2" => {
            let units: Vec<u16> = data
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            JsString::from(units.as_slice())
        }
        "latin1" => {
            let units: Vec<u16> = data.iter().map(|b| u16::from(*b)).collect();
            JsString::from(units.as_slice())
        }
        "ascii" => {
            let units: Vec<u16> = data.iter().map(|b| u16::from(b & 0x7f)).collect();
            JsString::from(units.as_slice())
        }
        "hex" => JsString::from(
            data.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
                .as_str(),
        ),
        "base64" => JsString::from(base64(data, false).as_str()),
        "base64url" => JsString::from(base64(data, true).as_str()),
        _ => JsString::from(String::from_utf8_lossy(data).as_ref()),
    }
}

fn js_string_arg(value: &JsValue, context: &mut Context) -> JsResult<JsString> {
    value.to_string(context)
}

// ─── The bytes under a view ──────────────────────────────────────────────────────────────────

/// Where a view's bytes are: its `ArrayBuffer`, and the range of it the view sees.
struct View {
    buffer: JsArrayBuffer,
    offset: usize,
    length: usize,
}

fn view_of(value: &JsValue, context: &mut Context) -> JsResult<Option<View>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        let length = buffer.byte_length();
        return Ok(Some(View {
            buffer,
            offset: 0,
            length,
        }));
    }
    let Ok(typed) = JsTypedArray::from_object(object.clone()) else {
        return Ok(None);
    };
    let Some(buffer) = typed.buffer(context)?.as_object() else {
        return Ok(None);
    };
    let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) else {
        return Ok(None);
    };
    let offset = typed.byte_offset(context)?;
    let length = typed.byte_length(context)?;
    Ok(Some(View {
        buffer,
        offset,
        length,
    }))
}

impl View {
    fn read<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        match self.buffer.data() {
            Some(data) => {
                let end = (self.offset + self.length).min(data.len());
                f(&data[self.offset.min(end)..end])
            }
            None => f(&[]),
        }
    }

    fn write<R>(&self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        match self.buffer.data_mut() {
            Some(mut data) => {
                let end = (self.offset + self.length).min(data.len());
                f(&mut data[self.offset.min(end)..end])
            }
            None => f(&mut []),
        }
    }

    fn to_vec(&self) -> Vec<u8> {
        self.read(<[u8]>::to_vec)
    }
}

/// The view `this` is: a `Buffer` or any `Uint8Array`.
fn this_view(this: &JsValue, context: &mut Context) -> JsResult<View> {
    match view_of(this, context)? {
        Some(view)
            if this
                .as_object()
                .is_none_or(|o| JsArrayBuffer::from_object(o).is_err()) =>
        {
            Ok(view)
        }
        _ => Err(invalid_type(context, "argument must be a buffer".into())),
    }
}

/// The bytes of a `Buffer`, a typed array, a `DataView` or an `ArrayBuffer`, read where they lie.
pub(crate) fn fast_bytes(value: &JsValue, context: &mut Context) -> JsResult<Option<Vec<u8>>> {
    match view_of(value, context)? {
        Some(view) => Ok(Some(view.to_vec())),
        None => bytes_of(value, context),
    }
}

// ─── Making Buffers ──────────────────────────────────────────────────────────────────────────

fn buffer_class(context: &mut Context) -> JsResult<JsObject> {
    crate::native::global(context, "Buffer")?
        .as_object()
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Buffer is not installed")
                .into()
        })
}

fn buffer_prototype(context: &mut Context) -> JsResult<JsObject> {
    buffer_class(context)?
        .get(js_string!("prototype"), context)?
        .as_object()
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("Buffer is not installed")
                .into()
        })
}

/// A `Uint8Array` of `bytes`, made a `Buffer`.
pub(crate) fn make(context: &mut Context, bytes: Vec<u8>) -> JsResult<JsValue> {
    let array: JsObject = JsUint8Array::from_iter(bytes, context)?.into();
    as_buffer(context, &array)?;
    Ok(array.into())
}

fn as_buffer(context: &mut Context, array: &JsObject) -> JsResult<()> {
    let prototype = buffer_prototype(context)?;
    array.set_prototype(Some(prototype));
    Ok(())
}

fn is_uint8(value: &JsValue) -> bool {
    value
        .as_object()
        .and_then(|o| JsTypedArray::from_object(o).ok())
        .and_then(|t| t.kind())
        .is_some_and(|k| matches!(format!("{k:?}").as_str(), "Uint8" | "Uint8Clamped"))
}

fn is_buffer(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    let Some(object) = value.as_object() else {
        return Ok(false);
    };
    let prototype = buffer_prototype(context)?;
    Ok(object
        .prototype()
        .is_some_and(|p| crate::native::inherits(&p, &prototype)))
}

/// A size given to `alloc`: a number from 0 to `kMaxLength`.
fn size_arg(value: &JsValue, context: &mut Context) -> JsResult<usize> {
    let Some(n) = value.as_number() else {
        let received = describe_received(value);
        return Err(invalid_type(
            context,
            format!("The \"size\" argument must be of type number. Received {received}"),
        ));
    };
    if !(0. ..=K_MAX_LENGTH).contains(&n) || n.is_nan() {
        return Err(out_of_range(
            context,
            "size",
            &format!(">= 0 && <= {}", K_MAX_LENGTH as u64),
            value,
        ));
    }
    Ok(n as usize)
}

const K_MAX_LENGTH: f64 = 9_007_199_254_740_991.;
const K_STRING_MAX_LENGTH: f64 = 536_870_888.;

fn construct_buffer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // `new Buffer(…)` and `Buffer(…)`, long deprecated in Node, are `Buffer.alloc` for a
    // number and `Buffer.from` for the rest.
    let first = arg(args, 0);
    if first.is_number() {
        let size = size_arg(&first, context)?;
        return make(context, vec![0; size]);
    }
    buffer_from(&JsValue::undefined(), args, context)
}

fn buffer_from(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = arg(args, 0);
    if let Some(text) = value.as_string() {
        let encoding = encoding_arg(&arg(args, 1), context)?;
        return make(context, string_bytes(&text, encoding));
    }
    if value.is_number() {
        return Err(invalid_type(
            context,
            format!(
                "The \"value\" argument must not be of type number. Received type number ({})",
                crate::print(&value)
            ),
        ));
    }
    if let Some(object) = value.as_object() {
        // An `ArrayBuffer`: a view of it, sharing its memory, as Node's is.
        if JsArrayBuffer::from_object(object.clone()).is_ok()
            || boa_engine::object::builtins::JsSharedArrayBuffer::from_object(object.clone())
                .is_ok()
        {
            let constructor = context
                .intrinsics()
                .constructors()
                .typed_uint8_array()
                .constructor();
            let mut view_args = vec![value.clone()];
            for at in 1..3 {
                let given = arg(args, at);
                if !given.is_undefined() {
                    view_args.push(given.to_number(context)?.into());
                }
            }
            let view = constructor.construct(&view_args, None, context)?;
            as_buffer(context, &view)?;
            return Ok(view.into());
        }
        // A typed array: its items, each made a byte — a copy, not a view.
        if let Ok(typed) = JsTypedArray::from_object(object.clone()) {
            if is_uint8(&value) || typed.kind().is_some_and(|k| format!("{k:?}") == "Int8") {
                let bytes = fast_bytes(&value, context)?.unwrap_or_default();
                return make(context, bytes);
            }
            let length = typed.length(context)?;
            let mut bytes = Vec::with_capacity(length);
            for at in 0..length {
                let item = object.get(at, context)?;
                bytes.push(to_byte(&item, context)?);
            }
            return make(context, bytes);
        }
        if let Some(bytes) = bytes_of(&value, context)? {
            return make(context, bytes);
        }
        // `valueOf` or `Symbol.toPrimitive` answering something else: that, made a Buffer.
        let primitive_key = boa_engine::JsSymbol::to_primitive();
        let to_primitive = object.get(primitive_key, context)?;
        if to_primitive.is_callable() {
            let primitive = crate::call(context, &to_primitive, &value, &[js_text("string")])?;
            if primitive.is_string() {
                let mut rest = vec![primitive];
                rest.extend(args.iter().skip(1).cloned());
                return buffer_from(&JsValue::undefined(), &rest, context);
            }
        }
        let value_of = object.get(js_string!("valueOf"), context)?;
        if value_of.is_callable() {
            let primitive = crate::call(context, &value_of, &value, &[])?;
            if primitive != value && (primitive.is_string() || primitive.is_object()) {
                let mut rest = vec![primitive];
                rest.extend(args.iter().skip(1).cloned());
                return buffer_from(&JsValue::undefined(), &rest, context);
            }
        }
        // An array or array-like of numbers, or `{ type: 'Buffer', data: [...] }`.
        let list = if object.get(js_string!("length"), context)?.is_number() {
            Some(value.clone())
        } else {
            let kind = object.get(js_string!("type"), context)?;
            let data = object.get(js_string!("data"), context)?;
            (crate::print(&kind) == "Buffer" && data.as_object().is_some_and(|d| d.is_array()))
                .then_some(data)
        };
        if let Some(list) = list {
            let items = crate::native::list_of(&list, context)?;
            let mut bytes = Vec::with_capacity(items.len());
            for item in items {
                bytes.push(to_byte(&item, context)?);
            }
            return make(context, bytes);
        }
    }
    let received = describe_received(&value);
    Err(invalid_type(
        context,
        format!(
            "The first argument must be of type string or an instance of Buffer, ArrayBuffer, or Array or an Array-like Object. Received {received}"
        ),
    ))
}

/// A number made a byte as a `Uint8Array` makes it: modulo 256, `NaN` as 0.
fn to_byte(value: &JsValue, context: &mut Context) -> JsResult<u8> {
    let n = value.to_number(context)?;
    if !n.is_finite() {
        return Ok(0);
    }
    Ok((n.trunc() as i64).rem_euclid(256) as u8)
}

/// What `fill` and `alloc` repeat: a string in an encoding, a number as a byte, or bytes.
fn fill_pattern(value: &JsValue, encoding: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    if let Some(text) = value.as_string() {
        if text.is_empty() {
            return Ok(vec![0]);
        }
        let encoding = encoding_arg(encoding, context)?;
        let bytes = string_bytes(&text, encoding);
        if bytes.is_empty() {
            return Err(coded(
                JsNativeError::typ(),
                format!(
                    "The argument 'value' is invalid. Received '{}'",
                    text.to_std_string_lossy()
                ),
                "ERR_INVALID_ARG_VALUE",
                context,
            ));
        }
        return Ok(bytes);
    }
    if let Some(bytes) = fast_bytes(value, context)? {
        if bytes.is_empty() {
            return Err(coded(
                JsNativeError::typ(),
                "The argument 'value' is invalid.".into(),
                "ERR_INVALID_ARG_VALUE",
                context,
            ));
        }
        return Ok(bytes);
    }
    if let Some(b) = value.as_boolean() {
        return Ok(vec![u8::from(b)]);
    }
    Ok(vec![(value.to_number(context)? as i64 & 255) as u8])
}

// ─── Statics ─────────────────────────────────────────────────────────────────────────────────

const STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("from", 3, buffer_from),
    ("alloc", 3, |_, args, cx| {
        let size = size_arg(&arg(args, 0), cx)?;
        let fill = arg(args, 1);
        let mut bytes = vec![0u8; size];
        if !fill.is_undefined() && fill.as_number() != Some(0.) && size > 0 {
            let pattern = fill_pattern(&fill, &arg(args, 2), cx)?;
            for (at, byte) in bytes.iter_mut().enumerate() {
                *byte = pattern[at % pattern.len()];
            }
        }
        make(cx, bytes)
    }),
    ("allocUnsafe", 1, |_, args, cx| {
        let size = size_arg(&arg(args, 0), cx)?;
        make(cx, vec![0; size])
    }),
    ("allocUnsafeSlow", 1, |_, args, cx| {
        let size = size_arg(&arg(args, 0), cx)?;
        make(cx, vec![0; size])
    }),
    ("isBuffer", 1, |_, args, cx| {
        Ok(is_buffer(&arg(args, 0), cx)?.into())
    }),
    ("isEncoding", 1, |_, args, _| {
        let name = arg(args, 0);
        Ok((name.is_string() && encoding_name(&crate::print(&name)).is_some()).into())
    }),
    ("byteLength", 2, |_, args, cx| {
        let value = arg(args, 0);
        if let Some(text) = value.as_string() {
            let encoding = match arg(args, 1) {
                e if e.is_undefined() => "utf8",
                e => encoding_name(&text_of(&e, cx)?).unwrap_or("utf8"),
            };
            return Ok(string_bytes(&text, encoding).len().into());
        }
        if let Some(view) = view_of(&value, cx)? {
            return Ok(view.length.into());
        }
        if let Some(bytes) = bytes_of(&value, cx)? {
            return Ok(bytes.len().into());
        }
        let received = describe_received(&value);
        Err(invalid_type(
            cx,
            format!(
                "The \"string\" argument must be of type string or an instance of Buffer or ArrayBuffer. Received {received}"
            ),
        ))
    }),
    ("concat", 2, |_, args, cx| {
        let list = arg(args, 0);
        if !list.as_object().is_some_and(|l| l.is_array()) {
            let received = describe_received(&list);
            return Err(invalid_type(
                cx,
                format!("The \"list\" argument must be an instance of Array. Received {received}"),
            ));
        }
        let items = crate::native::list_of(&list, cx)?;
        let mut bytes = Vec::new();
        for (at, item) in items.iter().enumerate() {
            if !is_uint8(item) {
                let received = describe_received(item);
                return Err(invalid_type(
                    cx,
                    format!(
                        "The \"list[{at}]\" argument must be an instance of Buffer or Uint8Array. Received {received}"
                    ),
                ));
            }
            bytes.extend(fast_bytes(item, cx)?.unwrap_or_default());
        }
        let total = arg(args, 1);
        if !total.is_undefined() {
            bytes.resize(total.to_number(cx)?.max(0.) as usize, 0);
        }
        make(cx, bytes)
    }),
    ("compare", 2, |_, args, cx| {
        let (a, b) = (arg(args, 0), arg(args, 1));
        for (name, value) in [("buf1", &a), ("buf2", &b)] {
            if !is_uint8(value) {
                let received = describe_received(value);
                return Err(invalid_type(
                    cx,
                    format!(
                        "The \"{name}\" argument must be an instance of Buffer or Uint8Array. Received {received}"
                    ),
                ));
            }
        }
        let a = fast_bytes(&a, cx)?.unwrap_or_default();
        let b = fast_bytes(&b, cx)?.unwrap_or_default();
        Ok((a.cmp(&b) as i32).into())
    }),
    ("of", 0, |_, args, cx| {
        let mut bytes = Vec::with_capacity(args.len());
        for item in args {
            bytes.push(to_byte(item, cx)?);
        }
        make(cx, bytes)
    }),
    ("copyBytesFrom", 3, |_, args, cx| {
        let view = arg(args, 0);
        let typed = view
            .as_object()
            .and_then(|o| JsTypedArray::from_object(o).ok())
            .ok_or_else(|| {
                invalid_type(
                    cx,
                    "The \"view\" argument must be an instance of TypedArray.".into(),
                )
            })?;
        let length = typed.length(cx)?;
        let bytes = fast_bytes(&view, cx)?.unwrap_or_default();
        let per = if length == 0 {
            1
        } else {
            bytes.len() / length.max(1)
        };
        let offset = match arg(args, 1) {
            o if o.is_undefined() => 0,
            o => o.to_number(cx)?.max(0.) as usize,
        };
        let count = match arg(args, 2) {
            c if c.is_undefined() => length.saturating_sub(offset),
            c => c.to_number(cx)?.max(0.) as usize,
        };
        let start = (offset * per).min(bytes.len());
        let end = ((offset + count) * per).min(bytes.len());
        make(cx, bytes[start..end].to_vec())
    }),
];

// ─── Methods ─────────────────────────────────────────────────────────────────────────────────

/// A position argument: a whole number, `default` when left out, clamped to `0..=length`.
fn position(
    value: &JsValue,
    default: usize,
    length: usize,
    context: &mut Context,
) -> JsResult<usize> {
    if value.is_undefined() {
        return Ok(default.min(length));
    }
    let n = value.to_number(context)?;
    if n.is_nan() {
        return Ok(0);
    }
    Ok(n.clamp(0., length as f64) as usize)
}

fn to_string(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let encoding = encoding_arg(&arg(args, 0), context)?;
    let start = position(&arg(args, 1), 0, view.length, context)?;
    let end = position(&arg(args, 2), view.length, view.length, context)?;
    if end <= start {
        return Ok(js_string!("").into());
    }
    Ok(view
        .read(|bytes| bytes_string(&bytes[start..end], encoding))
        .into())
}

/// `{encoding}Slice(start, end)` — `utf8Slice`, `latin1Slice` … — which some libraries call
/// directly, as Node's own code does.
fn slice_as(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    encoding: &str,
) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let start = position(&arg(args, 0), 0, view.length, context)?;
    let end = position(&arg(args, 1), view.length, view.length, context)?;
    if end <= start {
        return Ok(js_string!("").into());
    }
    Ok(view
        .read(|bytes| bytes_string(&bytes[start..end], encoding))
        .into())
}

/// Writes `bytes` at `offset`, at most `limit` of them; a UTF-8 character that does not fit
/// whole is not begun. Answers how many were written.
fn write_bytes(view: &View, mut bytes: Vec<u8>, offset: usize, limit: usize, utf8: bool) -> usize {
    let room = view.length.saturating_sub(offset).min(limit);
    if bytes.len() > room {
        let mut keep = room;
        if utf8 {
            while keep > 0 && keep < bytes.len() && (bytes[keep] & 0xc0) == 0x80 {
                keep -= 1;
            }
        }
        bytes.truncate(keep);
    }
    let count = bytes.len();
    view.write(|data| data[offset..offset + count].copy_from_slice(&bytes));
    count
}

fn write(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let text = arg(args, 0);
    let Some(text) = text.as_string() else {
        let received = describe_received(&text);
        return Err(invalid_type(
            context,
            format!("The \"string\" argument must be of type string. Received {received}"),
        ));
    };
    // `write(string, encoding)`, `write(string, offset, encoding)`, `write(string, offset,
    // length, encoding)`.
    let (offset, length, encoding) = match (arg(args, 1), arg(args, 2), arg(args, 3)) {
        (e, _, _) if e.is_string() => (JsValue::undefined(), JsValue::undefined(), e),
        (o, e, _) if e.is_string() => (o, JsValue::undefined(), e),
        (o, l, e) => (o, l, e),
    };
    let offset_n = if offset.is_undefined() {
        0.
    } else {
        offset.to_number(context)?
    };
    if offset_n < 0. || offset_n > view.length as f64 || offset_n.is_nan() {
        return Err(out_of_range(
            context,
            "offset",
            &format!(">= 0 && <= {}", view.length),
            &offset,
        ));
    }
    let offset = offset_n as usize;
    let limit = if length.is_undefined() {
        usize::MAX
    } else {
        let n = length.to_number(context)?;
        if n < 0. || n.is_nan() {
            return Err(out_of_range(
                context,
                "length",
                &format!(">= 0 && <= {}", view.length - offset),
                &length,
            ));
        }
        n as usize
    };
    let encoding = encoding_arg(&encoding, context)?;
    let bytes = string_bytes(&text, encoding);
    Ok(write_bytes(&view, bytes, offset, limit, encoding == "utf8").into())
}

/// `{encoding}Write(string, offset, length)`.
fn write_as(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    encoding: &str,
) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let text = js_string_arg(&arg(args, 0), context)?;
    let offset = position(&arg(args, 1), 0, view.length, context)?;
    let limit = position(&arg(args, 2), usize::MAX, usize::MAX, context)?;
    let bytes = string_bytes(&text, encoding);
    Ok(write_bytes(&view, bytes, offset, limit, encoding == "utf8").into())
}

fn fill(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let value = arg(args, 0);
    // `fill(value, encoding)` and `fill(value, offset, encoding)`.
    let (offset, end, encoding) = match (arg(args, 1), arg(args, 2), arg(args, 3)) {
        (e, _, _) if e.is_string() => (JsValue::undefined(), JsValue::undefined(), e),
        (o, e, _) if e.is_string() => (o, JsValue::undefined(), e),
        (o, n, e) => (o, n, e),
    };
    let start = if offset.is_undefined() {
        0
    } else {
        let n = offset.to_number(context)?;
        if n < 0. || n > view.length as f64 {
            return Err(out_of_range(
                context,
                "offset",
                &format!(">= 0 && <= {}", view.length),
                &offset,
            ));
        }
        n as usize
    };
    let stop = if end.is_undefined() {
        view.length
    } else {
        let n = end.to_number(context)?;
        if n < 0. || n > view.length as f64 {
            return Err(out_of_range(
                context,
                "end",
                &format!(">= 0 && <= {}", view.length),
                &end,
            ));
        }
        n as usize
    };
    if stop <= start {
        return Ok(this.clone());
    }
    let pattern = fill_pattern(&value, &encoding, context)?;
    view.write(|data| {
        for (at, byte) in data[start..stop].iter_mut().enumerate() {
            *byte = pattern[at % pattern.len()];
        }
    });
    Ok(this.clone())
}

/// The bytes `indexOf` looks for: a string in an encoding, a number as a byte, or bytes.
fn needle(value: &JsValue, encoding: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    if let Some(text) = value.as_string() {
        let encoding = encoding_arg(encoding, context)?;
        return Ok(string_bytes(&text, encoding));
    }
    if let Some(n) = value.as_number() {
        return Ok(vec![(n as i64).rem_euclid(256) as u8]);
    }
    if is_uint8(value) {
        return Ok(fast_bytes(value, context)?.unwrap_or_default());
    }
    let received = describe_received(value);
    Err(invalid_type(
        context,
        format!(
            "The \"value\" argument must be one of type number or string or an instance of Buffer or Uint8Array. Received {received}"
        ),
    ))
}

fn index_of(this: &JsValue, args: &[JsValue], context: &mut Context, last: bool) -> JsResult<i64> {
    let view = this_view(this, context)?;
    let (offset, encoding) = match (arg(args, 1), arg(args, 2)) {
        (e, _) if e.is_string() => (JsValue::undefined(), e),
        (o, e) => (o, e),
    };
    let wanted = needle(&arg(args, 0), &encoding, context)?;
    let length = view.length as i64;
    let from = if offset.is_undefined() || offset.is_null() {
        if last { length } else { 0 }
    } else {
        let n = offset.to_number(context)?;
        if n.is_nan() {
            if last { length } else { 0 }
        } else if n < 0. {
            length + n as i64
        } else {
            n as i64
        }
    };
    Ok(view.read(|bytes| {
        let n = wanted.len() as i64;
        if last {
            let start = from.min(length - n);
            if start < 0 {
                return -1;
            }
            if n == 0 {
                return start;
            }
            (0..=start)
                .rev()
                .find(|&at| bytes[at as usize..(at + n) as usize] == wanted[..])
                .unwrap_or(-1)
        } else {
            let start = from.max(0);
            if n == 0 {
                return start.min(length);
            }
            if start + n > length {
                return -1;
            }
            (start..=length - n)
                .find(|&at| bytes[at as usize..(at + n) as usize] == wanted[..])
                .unwrap_or(-1)
        }
    }))
}

fn copy(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let source = this_view(this, context)?;
    let target = arg(args, 0);
    if !is_uint8(&target) && view_of(&target, context)?.is_none() {
        let received = describe_received(&target);
        return Err(invalid_type(
            context,
            format!(
                "The \"target\" argument must be an instance of Buffer or Uint8Array. Received {received}"
            ),
        ));
    }
    let target_view = view_of(&target, context)?.expect("a view");
    let number =
        |value: &JsValue, name: &str, default: usize, context: &mut Context| -> JsResult<usize> {
            if value.is_undefined() {
                return Ok(default);
            }
            let n = value.to_number(context)?;
            if n < 0. {
                return Err(out_of_range(context, name, ">= 0", value));
            }
            Ok(if n.is_nan() { 0 } else { n as usize })
        };
    let target_start = number(&arg(args, 1), "targetStart", 0, context)?;
    let source_start = number(&arg(args, 2), "sourceStart", 0, context)?;
    let source_end = number(&arg(args, 3), "sourceEnd", source.length, context)?.min(source.length);
    if source_start > source.length {
        return Err(out_of_range(
            context,
            "sourceStart",
            &format!(">= 0 && <= {}", source.length),
            &arg(args, 2),
        ));
    }
    if target_start >= target_view.length || source_start >= source_end {
        return Ok(0.into());
    }
    let count = (source_end - source_start).min(target_view.length - target_start);
    let bytes = source.read(|data| data[source_start..source_start + count].to_vec());
    target_view.write(|data| data[target_start..target_start + count].copy_from_slice(&bytes));
    Ok(count.into())
}

fn compare(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let source = this_view(this, context)?;
    let target = arg(args, 0);
    if !is_uint8(&target) {
        let received = describe_received(&target);
        return Err(invalid_type(
            context,
            format!(
                "The \"target\" argument must be an instance of Buffer or Uint8Array. Received {received}"
            ),
        ));
    }
    let target = view_of(&target, context)?.expect("a view");
    let range = |value: &JsValue,
                 name: &str,
                 default: usize,
                 length: usize,
                 context: &mut Context|
     -> JsResult<usize> {
        if value.is_undefined() {
            return Ok(default);
        }
        let n = value.to_number(context)?;
        if n < 0. || n > length as f64 {
            return Err(out_of_range(
                context,
                name,
                &format!(">= 0 && <= {length}"),
                value,
            ));
        }
        Ok(n as usize)
    };
    let target_start = range(&arg(args, 1), "targetStart", 0, target.length, context)?;
    let target_end = range(
        &arg(args, 2),
        "targetEnd",
        target.length,
        target.length,
        context,
    )?;
    let source_start = range(&arg(args, 3), "sourceStart", 0, source.length, context)?;
    let source_end = range(
        &arg(args, 4),
        "sourceEnd",
        source.length,
        source.length,
        context,
    )?;
    let a = source.read(|d| d[source_start..source_end.max(source_start)].to_vec());
    let b = target.read(|d| d[target_start..target_end.max(target_start)].to_vec());
    Ok((a.cmp(&b) as i32).into())
}

fn swap(this: &JsValue, context: &mut Context, size: usize) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    if view.length % size != 0 {
        return Err(coded(
            JsNativeError::range(),
            format!("Buffer size must be a multiple of {}-bits", size * 8),
            "ERR_INVALID_BUFFER_SIZE",
            context,
        ));
    }
    view.write(|data| data.chunks_exact_mut(size).for_each(<[u8]>::reverse));
    Ok(this.clone())
}

/// `slice` and `subarray`: a `Buffer` over the same memory.
fn subarray(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let typed = this
        .as_object()
        .and_then(|o| JsTypedArray::from_object(o).ok())
        .ok_or_else(|| JsNativeError::typ().with_message("argument must be a buffer"))?;
    let length = typed.length(context)? as f64;
    let at = |value: &JsValue, default: f64, context: &mut Context| -> JsResult<f64> {
        if value.is_undefined() {
            return Ok(default);
        }
        let n = value.to_number(context)?;
        let n = if n.is_nan() { 0. } else { n.trunc() };
        Ok(if n < 0. {
            (length + n).max(0.)
        } else {
            n.min(length)
        })
    };
    let start = at(&arg(args, 0), 0., context)?;
    let end = at(&arg(args, 1), length, context)?.max(start);
    let buffer = typed.buffer(context)?;
    let offset = typed.byte_offset(context)? as f64;
    let constructor = context
        .intrinsics()
        .constructors()
        .typed_uint8_array()
        .constructor();
    let view = constructor.construct(
        &[buffer, (offset + start).into(), (end - start).into()],
        None,
        context,
    )?;
    as_buffer(context, &view)?;
    Ok(view.into())
}

/// `<Buffer 68 69>`: what `util.inspect` and the console write for a `Buffer`, at most 50
/// bytes (`buffer.INSPECT_MAX_BYTES`) of it.
fn inspect(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(view) = view_of(this, context)? else {
        return Ok(js_text("Buffer {}"));
    };
    let max = 50;
    let mut text = view.read(|bytes| {
        bytes
            .iter()
            .take(max)
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    });
    if view.length > max {
        text.push_str(&format!(
            " ... {} more byte{}",
            view.length - max,
            if view.length - max > 1 { "s" } else { "" }
        ));
    }
    let name = match this.as_object() {
        Some(o) => {
            let constructor = o.get(js_string!("constructor"), context)?;
            let name = get(&constructor, "name", context)?;
            if name.is_string() {
                crate::print(&name)
            } else {
                "Buffer".into()
            }
        }
        None => "Buffer".into(),
    };
    Ok(js_text(&if text.is_empty() {
        format!("<{name} >")
    } else {
        format!("<{name} {text}>")
    }))
}

// ─── Numbers at an offset ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Num {
    Unsigned,
    Signed,
    Float,
    BigUnsigned,
    BigSigned,
}

/// The offset a read or write starts at: a whole number from 0 to `length - size`.
fn offset_arg(
    value: &JsValue,
    size: usize,
    length: usize,
    context: &mut Context,
) -> JsResult<usize> {
    if value.is_undefined() {
        if length < size {
            return Err(bounds(context));
        }
        return Ok(0);
    }
    let Some(n) = value.as_number() else {
        let received = describe_received(value);
        return Err(invalid_type(
            context,
            format!("The \"offset\" argument must be of type number. Received {received}"),
        ));
    };
    if n.fract() != 0. || n.is_nan() {
        return Err(out_of_range(context, "offset", "an integer", value));
    }
    if length < size {
        return Err(bounds(context));
    }
    let last = length - size;
    if n < 0. || n > last as f64 {
        return Err(out_of_range(
            context,
            "offset",
            &format!(">= 0 and <= {last}"),
            value,
        ));
    }
    Ok(n as usize)
}

fn byte_length_arg(value: &JsValue, context: &mut Context) -> JsResult<usize> {
    let Some(n) = value.as_number() else {
        let received = describe_received(value);
        return Err(invalid_type(
            context,
            format!("The \"byteLength\" argument must be of type number. Received {received}"),
        ));
    };
    if !(1. ..=6.).contains(&n) || n.fract() != 0. {
        return Err(out_of_range(context, "byteLength", ">= 1 and <= 6", value));
    }
    Ok(n as usize)
}

fn read_number(
    this: &JsValue,
    offset: &JsValue,
    size: usize,
    little: bool,
    kind: Num,
    context: &mut Context,
) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let at = offset_arg(offset, size, view.length, context)?;
    let mut bytes = view.read(|data| data[at..at + size].to_vec());
    if little {
        bytes.reverse();
    }
    // `bytes` is big-endian now.
    Ok(match kind {
        Num::Float if size == 4 => {
            f64::from(f32::from_be_bytes(bytes.try_into().expect("4 bytes"))).into()
        }
        Num::Float => f64::from_be_bytes(bytes.try_into().expect("8 bytes")).into(),
        Num::BigUnsigned => {
            JsBigInt::from(u64::from_be_bytes(bytes.try_into().expect("8 bytes"))).into()
        }
        Num::BigSigned => {
            JsBigInt::from(i64::from_be_bytes(bytes.try_into().expect("8 bytes"))).into()
        }
        Num::Unsigned | Num::Signed => {
            let unsigned = bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
            if kind == Num::Signed && size < 8 {
                let shift = 64 - size * 8;
                (((unsigned << shift) as i64) >> shift).into()
            } else {
                (unsigned as f64).into()
            }
        }
    })
}

fn write_number(
    this: &JsValue,
    value: &JsValue,
    offset: &JsValue,
    size: usize,
    little: bool,
    kind: Num,
    context: &mut Context,
) -> JsResult<JsValue> {
    let view = this_view(this, context)?;
    let mut bytes: Vec<u8> = match kind {
        Num::BigUnsigned | Num::BigSigned => {
            let Some(n) = value.as_bigint() else {
                let received = describe_received(value);
                return Err(invalid_type(
                    context,
                    format!("The \"value\" argument must be of type bigint. Received {received}"),
                ));
            };
            let n = n.to_i128();
            let (min, max, range) = if kind == Num::BigUnsigned {
                (
                    0i128,
                    u64::MAX as i128,
                    "\">= 0n and < 2n ** 64n\"".to_owned(),
                )
            } else {
                (
                    i64::MIN as i128,
                    i64::MAX as i128,
                    "\">= -(2n ** 63n) and < 2n ** 63n\"".to_owned(),
                )
            };
            if n < min || n > max {
                return Err(out_of_range(
                    context,
                    "value",
                    range.trim_matches('"'),
                    value,
                ));
            }
            (n as u64).to_be_bytes().to_vec()
        }
        Num::Float => {
            let n = value.to_number(context)?;
            if size == 4 {
                (n as f32).to_be_bytes().to_vec()
            } else {
                n.to_be_bytes().to_vec()
            }
        }
        Num::Unsigned | Num::Signed => {
            let n = value.to_number(context)?;
            let bits = size as u32 * 8;
            let (min, max) = if kind == Num::Unsigned {
                (0., 2f64.powi(bits as i32) - 1.)
            } else {
                (
                    -(2f64.powi(bits as i32 - 1)),
                    2f64.powi(bits as i32 - 1) - 1.,
                )
            };
            if n > max || n < min {
                let range = if size > 4 {
                    if kind == Num::Unsigned {
                        format!(">= 0 and < 2 ** {bits}")
                    } else {
                        format!(">= -(2 ** {}) and < 2 ** {}", bits - 1, bits - 1)
                    }
                } else {
                    format!(">= {min} and <= {max}")
                };
                return Err(out_of_range(context, "value", &range, value));
            }
            let whole = if n.is_nan() { 0 } else { n.trunc() as i64 };
            whole.to_be_bytes()[8 - size..].to_vec()
        }
    };
    let at = offset_arg(offset, size, view.length, context)?;
    if little {
        bytes.reverse();
    }
    view.write(|data| data[at..at + size].copy_from_slice(&bytes));
    Ok((at + size).into())
}

macro_rules! reader {
    ($size:literal, $little:literal, $kind:expr) => {
        |this, args, cx| read_number(this, &arg(args, 0), $size, $little, $kind, cx)
    };
}

macro_rules! writer {
    ($size:literal, $little:literal, $kind:expr) => {
        |this, args, cx| {
            write_number(
                this,
                &arg(args, 0),
                &arg(args, 1),
                $size,
                $little,
                $kind,
                cx,
            )
        }
    };
}

/// Every fixed-size reader and writer, by Node's name; each also answers to its lowercase
/// `Uint` spelling (`readUint16LE`).
const NUMBERS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("readUInt8", 1, reader!(1, false, Num::Unsigned)),
    ("readUInt16LE", 1, reader!(2, true, Num::Unsigned)),
    ("readUInt16BE", 1, reader!(2, false, Num::Unsigned)),
    ("readUInt32LE", 1, reader!(4, true, Num::Unsigned)),
    ("readUInt32BE", 1, reader!(4, false, Num::Unsigned)),
    ("readInt8", 1, reader!(1, false, Num::Signed)),
    ("readInt16LE", 1, reader!(2, true, Num::Signed)),
    ("readInt16BE", 1, reader!(2, false, Num::Signed)),
    ("readInt32LE", 1, reader!(4, true, Num::Signed)),
    ("readInt32BE", 1, reader!(4, false, Num::Signed)),
    ("readFloatLE", 1, reader!(4, true, Num::Float)),
    ("readFloatBE", 1, reader!(4, false, Num::Float)),
    ("readDoubleLE", 1, reader!(8, true, Num::Float)),
    ("readDoubleBE", 1, reader!(8, false, Num::Float)),
    ("readBigUInt64LE", 1, reader!(8, true, Num::BigUnsigned)),
    ("readBigUInt64BE", 1, reader!(8, false, Num::BigUnsigned)),
    ("readBigInt64LE", 1, reader!(8, true, Num::BigSigned)),
    ("readBigInt64BE", 1, reader!(8, false, Num::BigSigned)),
    ("writeUInt8", 2, writer!(1, false, Num::Unsigned)),
    ("writeUInt16LE", 2, writer!(2, true, Num::Unsigned)),
    ("writeUInt16BE", 2, writer!(2, false, Num::Unsigned)),
    ("writeUInt32LE", 2, writer!(4, true, Num::Unsigned)),
    ("writeUInt32BE", 2, writer!(4, false, Num::Unsigned)),
    ("writeInt8", 2, writer!(1, false, Num::Signed)),
    ("writeInt16LE", 2, writer!(2, true, Num::Signed)),
    ("writeInt16BE", 2, writer!(2, false, Num::Signed)),
    ("writeInt32LE", 2, writer!(4, true, Num::Signed)),
    ("writeInt32BE", 2, writer!(4, false, Num::Signed)),
    ("writeFloatLE", 2, writer!(4, true, Num::Float)),
    ("writeFloatBE", 2, writer!(4, false, Num::Float)),
    ("writeDoubleLE", 2, writer!(8, true, Num::Float)),
    ("writeDoubleBE", 2, writer!(8, false, Num::Float)),
    ("writeBigUInt64LE", 2, writer!(8, true, Num::BigUnsigned)),
    ("writeBigUInt64BE", 2, writer!(8, false, Num::BigUnsigned)),
    ("writeBigInt64LE", 2, writer!(8, true, Num::BigSigned)),
    ("writeBigInt64BE", 2, writer!(8, false, Num::BigSigned)),
    // The variable-width ones: `readUIntLE(offset, byteLength)`, `writeIntBE(value, offset,
    // byteLength)`.
    ("readUIntLE", 2, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 1), cx)?;
        read_number(this, &arg(args, 0), size, true, Num::Unsigned, cx)
    }),
    ("readUIntBE", 2, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 1), cx)?;
        read_number(this, &arg(args, 0), size, false, Num::Unsigned, cx)
    }),
    ("readIntLE", 2, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 1), cx)?;
        read_number(this, &arg(args, 0), size, true, Num::Signed, cx)
    }),
    ("readIntBE", 2, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 1), cx)?;
        read_number(this, &arg(args, 0), size, false, Num::Signed, cx)
    }),
    ("writeUIntLE", 3, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 2), cx)?;
        write_number(
            this,
            &arg(args, 0),
            &arg(args, 1),
            size,
            true,
            Num::Unsigned,
            cx,
        )
    }),
    ("writeUIntBE", 3, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 2), cx)?;
        write_number(
            this,
            &arg(args, 0),
            &arg(args, 1),
            size,
            false,
            Num::Unsigned,
            cx,
        )
    }),
    ("writeIntLE", 3, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 2), cx)?;
        write_number(
            this,
            &arg(args, 0),
            &arg(args, 1),
            size,
            true,
            Num::Signed,
            cx,
        )
    }),
    ("writeIntBE", 3, |this, args, cx| {
        let size = byte_length_arg(&arg(args, 2), cx)?;
        write_number(
            this,
            &arg(args, 0),
            &arg(args, 1),
            size,
            false,
            Num::Signed,
            cx,
        )
    }),
];

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("toString", 3, to_string),
    ("toLocaleString", 3, to_string),
    ("toJSON", 0, |this, _, cx| {
        let view = this_view(this, cx)?;
        let bytes = view.to_vec();
        let data = JsArray::from_iter(bytes.into_iter().map(JsValue::from), cx);
        let out = object(cx);
        out.set(js_string!("type"), js_string!("Buffer"), false, cx)?;
        out.set(js_string!("data"), data, false, cx)?;
        Ok(out.into())
    }),
    ("equals", 1, |this, args, cx| {
        let other = arg(args, 0);
        if !is_uint8(&other) {
            let received = describe_received(&other);
            return Err(invalid_type(
                cx,
                format!(
                    "The \"otherBuffer\" argument must be an instance of Buffer or Uint8Array. Received {received}"
                ),
            ));
        }
        let a = this_view(this, cx)?.to_vec();
        let b = fast_bytes(&other, cx)?.unwrap_or_default();
        Ok((a == b).into())
    }),
    ("compare", 5, compare),
    ("copy", 4, copy),
    ("fill", 4, fill),
    ("indexOf", 3, |this, args, cx| {
        Ok(index_of(this, args, cx, false)?.into())
    }),
    ("lastIndexOf", 3, |this, args, cx| {
        Ok(index_of(this, args, cx, true)?.into())
    }),
    ("includes", 3, |this, args, cx| {
        Ok((index_of(this, args, cx, false)? != -1).into())
    }),
    ("swap16", 0, |this, _, cx| swap(this, cx, 2)),
    ("swap32", 0, |this, _, cx| swap(this, cx, 4)),
    ("swap64", 0, |this, _, cx| swap(this, cx, 8)),
    ("write", 4, write),
    ("slice", 2, subarray),
    ("subarray", 2, subarray),
    ("utf8Slice", 2, |this, args, cx| {
        slice_as(this, args, cx, "utf8")
    }),
    ("ucs2Slice", 2, |this, args, cx| {
        slice_as(this, args, cx, "ucs2")
    }),
    ("latin1Slice", 2, |this, args, cx| {
        slice_as(this, args, cx, "latin1")
    }),
    ("asciiSlice", 2, |this, args, cx| {
        slice_as(this, args, cx, "ascii")
    }),
    ("hexSlice", 2, |this, args, cx| {
        slice_as(this, args, cx, "hex")
    }),
    ("base64Slice", 2, |this, args, cx| {
        slice_as(this, args, cx, "base64")
    }),
    ("base64urlSlice", 2, |this, args, cx| {
        slice_as(this, args, cx, "base64url")
    }),
    ("utf8Write", 3, |this, args, cx| {
        write_as(this, args, cx, "utf8")
    }),
    ("ucs2Write", 3, |this, args, cx| {
        write_as(this, args, cx, "ucs2")
    }),
    ("latin1Write", 3, |this, args, cx| {
        write_as(this, args, cx, "latin1")
    }),
    ("asciiWrite", 3, |this, args, cx| {
        write_as(this, args, cx, "ascii")
    }),
    ("hexWrite", 3, |this, args, cx| {
        write_as(this, args, cx, "hex")
    }),
    ("base64Write", 3, |this, args, cx| {
        write_as(this, args, cx, "base64")
    }),
    ("base64urlWrite", 3, |this, args, cx| {
        write_as(this, args, cx, "base64url")
    }),
    ("inspect", 0, inspect),
];

// ─── Installing it ───────────────────────────────────────────────────────────────────────────

fn constructor(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
    boa_engine::object::FunctionObjectBuilder::new(
        context.realm(),
        boa_engine::NativeFunction::from_fn_ptr(f),
    )
    .name(JsString::from(name))
    .length(length)
    .constructor(true)
    .build()
    .into()
}

/// Makes `Buffer` a global: a constructor whose prototype inherits `Uint8Array`'s.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let buffer = constructor(context, "Buffer", 2, construct_buffer);
    let (uint8_prototype, uint8_constructor) = {
        let uint8 = context.intrinsics().constructors().typed_uint8_array();
        (uint8.prototype(), uint8.constructor())
    };
    let prototype = JsObject::with_null_proto();
    prototype.set_prototype(Some(uint8_prototype));
    crate::native::methods(context, &prototype, METHODS)?;
    crate::native::methods(context, &prototype, NUMBERS)?;
    // `readUint16LE` and the rest, Node's other spelling.
    for (name, length, f) in NUMBERS {
        if name.contains("UInt") {
            let alias = name.replace("UInt", "Uint");
            let method = function(context, &alias, *length, *f);
            hidden(&prototype, JsString::from(alias.as_str()), method, context)?;
        }
    }
    let inspect_fn = prototype.get(js_string!("inspect"), context)?;
    let custom = crate::util::symbol_for(context, "nodejs.util.inspect.custom")?;
    hidden(
        &prototype,
        custom.to_property_key(context)?,
        inspect_fn,
        context,
    )?;
    crate::native::accessors(
        context,
        &prototype,
        &[
            (
                "parent",
                |this, _, cx| match this
                    .as_object()
                    .and_then(|o| JsTypedArray::from_object(o).ok())
                {
                    Some(typed) => typed.buffer(cx),
                    None => Ok(JsValue::undefined()),
                },
                None,
            ),
            (
                "offset",
                |this, _, cx| match this
                    .as_object()
                    .and_then(|o| JsTypedArray::from_object(o).ok())
                {
                    Some(typed) => Ok(typed.byte_offset(cx)?.into()),
                    None => Ok(JsValue::undefined()),
                },
                None,
            ),
        ],
    )?;
    hidden(
        &prototype,
        js_string!("constructor"),
        buffer.clone(),
        context,
    )?;
    buffer.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    buffer.set_prototype(Some(uint8_constructor));
    crate::native::methods(context, &buffer, STATICS)?;
    buffer.set(js_string!("poolSize"), 8192, false, context)?;
    crate::native::define_global(context, "Buffer", buffer)?;
    Ok(())
}

// ─── require('buffer') ───────────────────────────────────────────────────────────────────────

/// `require('buffer')`: `Buffer`, `SlowBuffer`, `Blob`, `File`, `atob`, `btoa`, `constants`,
/// `kMaxLength`, `kStringMaxLength`, `INSPECT_MAX_BYTES`, `isUtf8`, `isAscii`, `transcode`,
/// `resolveObjectURL`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    for name in ["Buffer", "Blob", "File", "atob", "btoa"] {
        let value = crate::native::global(context, name)?;
        module.set(JsString::from(name), value, false, context)?;
    }
    // `SlowBuffer(size)`: a Buffer of that size, as Node's deprecated one is.
    let slow = constructor(context, "SlowBuffer", 1, |_, args, cx| {
        let size = size_arg(&arg(args, 0), cx)?;
        make(cx, vec![0; size])
    });
    let prototype = buffer_prototype(context)?;
    slow.set(js_string!("prototype"), prototype, false, context)?;
    module.set(js_string!("SlowBuffer"), slow, false, context)?;
    let constants = object(context);
    constants.set(js_string!("MAX_LENGTH"), K_MAX_LENGTH, false, context)?;
    constants.set(
        js_string!("MAX_STRING_LENGTH"),
        K_STRING_MAX_LENGTH,
        false,
        context,
    )?;
    module.set(js_string!("constants"), constants, false, context)?;
    module.set(js_string!("kMaxLength"), K_MAX_LENGTH, false, context)?;
    module.set(
        js_string!("kStringMaxLength"),
        K_STRING_MAX_LENGTH,
        false,
        context,
    )?;
    module.set(js_string!("INSPECT_MAX_BYTES"), 50, false, context)?;
    crate::native::exports(
        context,
        &module,
        &[
            ("isUtf8", 1, |_, args, cx| {
                let bytes = view_bytes_arg(&arg(args, 0), cx)?;
                Ok(std::str::from_utf8(&bytes).is_ok().into())
            }),
            ("isAscii", 1, |_, args, cx| {
                let bytes = view_bytes_arg(&arg(args, 0), cx)?;
                Ok(bytes.is_ascii().into())
            }),
            ("transcode", 3, |_, args, cx| {
                let source = arg(args, 0);
                if !is_uint8(&source) {
                    let received = describe_received(&source);
                    return Err(invalid_type(
                        cx,
                        format!(
                            "The \"source\" argument must be an instance of Buffer or Uint8Array. Received {received}"
                        ),
                    ));
                }
                let from = encoding_arg(&arg(args, 1), cx)?;
                let to = encoding_arg(&arg(args, 2), cx)?;
                for encoding in [from, to] {
                    if !matches!(encoding, "utf8" | "ucs2" | "latin1" | "ascii") {
                        return Err(coded(
                            JsNativeError::error(),
                            "Unable to transcode Buffer [U_ILLEGAL_ARGUMENT_ERROR]".into(),
                            "U_ILLEGAL_ARGUMENT_ERROR",
                            cx,
                        ));
                    }
                }
                let bytes = fast_bytes(&source, cx)?.unwrap_or_default();
                let text = bytes_string(&bytes, from);
                // Characters the target cannot hold become `?`, as ICU's transcoder makes them.
                let out: Vec<u8> = match to {
                    "latin1" => text
                        .iter()
                        .map(|u| if u > 0xff { b'?' } else { u as u8 })
                        .collect(),
                    "ascii" => text
                        .iter()
                        .map(|u| if u > 0x7f { b'?' } else { u as u8 })
                        .collect(),
                    other => string_bytes(&text, other),
                };
                make(cx, out)
            }),
            ("resolveObjectURL", 1, |_, args, cx| {
                let url = text_of(&arg(args, 0), cx)?;
                let bytes = crate::shared(cx).blobs.borrow().get(&url).cloned();
                match bytes {
                    Some(bytes) => {
                        let data = JsUint8Array::from_iter(bytes, cx)?;
                        let list = JsArray::from_iter([data.into()], cx);
                        Ok(crate::native::construct_global(cx, "Blob", &[list.into()])?.into())
                    }
                    None => Ok(JsValue::undefined()),
                }
            }),
        ],
    )?;
    Ok(module.into())
}

fn view_bytes_arg(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    match fast_bytes(value, context)? {
        Some(bytes) => Ok(bytes),
        None => {
            let received = describe_received(value);
            Err(invalid_type(
                context,
                format!(
                    "The \"input\" argument must be an instance of ArrayBuffer, Buffer, or TypedArray. Received {received}"
                ),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_base64_read_as_node_reads_them() {
        let units = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(unhex(&units("abc")), [0xab]);
        assert_eq!(unhex(&units("abzz12")), [0xab]);
        assert_eq!(unbase64_lenient(&units("aGVs bG8=\n")), b"hello");
        assert_eq!(unbase64_lenient(&units("aGVsbG8")), b"hello");
        assert_eq!(unbase64_lenient(&units("-_8")), [0xfb, 0xff]);
    }
}
