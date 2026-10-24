//! Bytes and text: `atob` and `btoa`, `TextEncoder` and `TextDecoder`, and Node's `Buffer` —
//! what a plugin bundles expect from a browser and from Electron's Node.
//!
//! A `Buffer` is a `Uint8Array`, as Node's is: its prototype is `Buffer.prototype`, which
//! inherits `Uint8Array.prototype`, so indexing, `length`, `subarray` and the rest are the
//! engine's own, and only what Node adds is here.

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArray, JsArrayBuffer, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn text_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

fn type_error(message: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

// ─── Base64 and the rest ────────────────────────────────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8], url: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - i * 8)));
        for i in 0..4 {
            if i <= chunk.len() {
                let c = B64[((n >> (18 - i * 6)) & 63) as usize];
                out.push(match (url, c) {
                    (true, b'+') => '-',
                    (true, b'/') => '_',
                    _ => c as char,
                });
            } else if !url {
                out.push('=');
            }
        }
    }
    out
}

/// Base64 read forgivingly, as `atob` and Node read it: either alphabet, padding or none,
/// white space ignored. `None` for a character that is not base64.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text.bytes().collect();
    digits
        .chunks(2)
        .map_while(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

/// The bytes of an `ArrayBuffer`, a typed array or a `DataView`.
pub fn bytes_of(value: &JsValue, context: &mut Context) -> JsResult<Option<Vec<u8>>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        return Ok(Some(buffer.to_vec().unwrap_or_default()));
    }
    let buffer = object.get(js_string!("buffer"), context)?;
    let Some(buffer) = buffer.as_object() else {
        return Ok(None);
    };
    let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) else {
        return Ok(None);
    };
    let offset = object.get(js_string!("byteOffset"), context)?.to_number(context)? as usize;
    let length = object.get(js_string!("byteLength"), context)?.to_number(context)? as usize;
    let all = buffer.to_vec().unwrap_or_default();
    let end = (offset + length).min(all.len());
    Ok(Some(all.get(offset.min(end)..end).unwrap_or_default().to_vec()))
}

/// What a string is in `encoding`, Node's names.
fn encode(text: &str, encoding: &str) -> JsResult<Vec<u8>> {
    Ok(match encoding.to_ascii_lowercase().as_str() {
        "" | "utf8" | "utf-8" => text.as_bytes().to_vec(),
        "base64" | "base64url" => unbase64(text).unwrap_or_default(),
        "hex" => unhex(text),
        "latin1" | "binary" | "ascii" => text.chars().map(|c| c as u32 as u8).collect(),
        "utf16le" | "utf-16le" | "ucs2" | "ucs-2" => {
            text.encode_utf16().flat_map(u16::to_le_bytes).collect()
        }
        other => return Err(type_error(format!("Unknown encoding: {other}"))),
    })
}

/// The bytes, read as text in `encoding`.
fn decode(data: &[u8], encoding: &str) -> JsResult<String> {
    Ok(match encoding.to_ascii_lowercase().as_str() {
        "" | "utf8" | "utf-8" => String::from_utf8_lossy(data).into_owned(),
        "base64" => base64(data, false),
        "base64url" => base64(data, true),
        "hex" => hex(data),
        "latin1" | "binary" => data.iter().map(|b| char::from(*b)).collect(),
        "ascii" => data.iter().map(|b| char::from(b & 0x7f)).collect(),
        "utf16le" | "utf-16le" | "ucs2" | "ucs-2" => {
            let units: Vec<u16> = data
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        other => return Err(type_error(format!("Unknown encoding: {other}"))),
    })
}

// ─── atob and btoa ──────────────────────────────────────────────────────────────────────────

fn atob(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = text_of(&arg(args, 0), context)?;
    let bytes = unbase64(&text).ok_or_else(|| {
        JsNativeError::error()
            .with_message("atob: the string to be decoded is not correctly encoded")
    })?;
    // A "binary string": one character per byte.
    let out: String = bytes.iter().map(|b| char::from(*b)).collect();
    Ok(JsString::from(out.as_str()).into())
}

fn btoa(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let text = text_of(&arg(args, 0), context)?;
    let mut bytes = Vec::with_capacity(text.len());
    for c in text.chars() {
        let code = c as u32;
        if code > 0xff {
            return Err(JsNativeError::error()
                .with_message("btoa: the string contains characters outside of the Latin1 range")
                .into());
        }
        bytes.push(code as u8);
    }
    Ok(JsString::from(base64(&bytes, false).as_str()).into())
}

// ─── TextEncoder and TextDecoder ────────────────────────────────────────────────────────────

fn uint8(context: &mut Context, bytes: Vec<u8>) -> JsResult<JsValue> {
    Ok(JsUint8Array::from_iter(bytes, context)?.into())
}

fn construct_encoder(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let encoder = JsObject::with_object_proto(context.intrinsics());
    encoder.set(js_string!("encoding"), js_string!("utf-8"), false, context)?;
    let encode = function(context, "encode", 1, |_, args, cx| {
        let input = arg(args, 0);
        let text = if input.is_undefined() { String::new() } else { text_of(&input, cx)? };
        uint8(cx, text.into_bytes())
    });
    encoder.set(js_string!("encode"), encode, false, context)?;
    let encode_into = function(context, "encodeInto", 2, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        let target = arg(args, 1);
        let target = target
            .as_object()
            .and_then(|o| JsUint8Array::from_object(o.clone()).ok())
            .ok_or_else(|| type_error("encodeInto: the destination is not a Uint8Array"))?;
        let room = target.length(cx)?;
        let mut written = 0usize;
        let mut read = 0usize;
        for c in text.chars() {
            let mut piece = [0u8; 4];
            let piece = c.encode_utf8(&mut piece).as_bytes();
            if written + piece.len() > room {
                break;
            }
            for byte in piece {
                target.set(written, *byte, true, cx)?;
                written += 1;
            }
            read += c.len_utf16();
        }
        let out = JsObject::with_object_proto(cx.intrinsics());
        out.set(js_string!("read"), read, false, cx)?;
        out.set(js_string!("written"), written, false, cx)?;
        Ok(out.into())
    });
    encoder.set(js_string!("encodeInto"), encode_into, false, context)?;
    Ok(encoder.into())
}

fn construct_decoder(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let label = match arg(args, 0) {
        value if value.is_undefined() => "utf-8".to_owned(),
        value => text_of(&value, context)?.trim().to_ascii_lowercase(),
    };
    let encoding = match label.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => "utf-8",
        "utf-16le" | "utf-16" => "utf-16le",
        "latin1" | "iso-8859-1" | "windows-1252" | "ascii" | "us-ascii" => "windows-1252",
        other => {
            return Err(JsNativeError::range()
                .with_message(format!("TextDecoder: the encoding '{other}' is not supported"))
                .into());
        }
    };
    let options = arg(args, 1);
    let fatal = match options.as_object() {
        Some(o) => o.get(js_string!("fatal"), context)?.to_boolean(),
        None => false,
    };
    let ignore_bom = match options.as_object() {
        Some(o) => o.get(js_string!("ignoreBOM"), context)?.to_boolean(),
        None => false,
    };
    let decoder = JsObject::with_object_proto(context.intrinsics());
    decoder.set(js_string!("encoding"), JsString::from(encoding), false, context)?;
    decoder.set(js_string!("fatal"), fatal, false, context)?;
    decoder.set(js_string!("ignoreBOM"), ignore_bom, false, context)?;
    let decode_fn = function(context, "decode", 1, |this, args, cx| {
        let input = arg(args, 0);
        let bytes = if input.is_undefined() {
            Vec::new()
        } else {
            bytes_of(&input, cx)?
                .ok_or_else(|| type_error("TextDecoder.decode: not an ArrayBuffer or a view of one"))?
        };
        let field = |name: &str, cx: &mut Context| -> JsResult<JsValue> {
            match this.as_object() {
                Some(o) => o.get(JsString::from(name), cx),
                None => Ok(JsValue::undefined()),
            }
        };
        let encoding = text_of(&field("encoding", cx)?, cx)?;
        let fatal = field("fatal", cx)?.to_boolean();
        let ignore_bom = field("ignoreBOM", cx)?.to_boolean();
        let text = match encoding.as_str() {
            "utf-16le" => {
                let mut units: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect();
                if !ignore_bom && units.first() == Some(&0xfeff) {
                    units.remove(0);
                }
                String::from_utf16_lossy(&units)
            }
            "windows-1252" => bytes.iter().map(|b| char::from(*b)).collect(),
            _ => {
                let body = if !ignore_bom && bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                    &bytes[3..]
                } else {
                    &bytes[..]
                };
                if fatal {
                    std::str::from_utf8(body)
                        .map_err(|_| type_error("TextDecoder: the data is not valid utf-8"))?
                        .to_owned()
                } else {
                    String::from_utf8_lossy(body).into_owned()
                }
            }
        };
        Ok(JsString::from(text.as_str()).into())
    });
    decoder.set(js_string!("decode"), decode_fn, false, context)?;
    Ok(decoder.into())
}

// ─── Buffer ─────────────────────────────────────────────────────────────────────────────────

fn buffer_class(context: &mut Context) -> JsResult<JsObject> {
    context
        .global_object()
        .get(js_string!("Buffer"), context)?
        .as_object()
        .ok_or_else(|| type_error("Buffer is not installed"))
}

/// A `Uint8Array` of `bytes`, made a `Buffer`.
pub fn make_buffer(context: &mut Context, bytes: Vec<u8>) -> JsResult<JsValue> {
    let array: JsObject = JsUint8Array::from_iter(bytes, context)?.into();
    as_buffer(context, &array)?;
    Ok(array.into())
}

fn as_buffer(context: &mut Context, array: &JsObject) -> JsResult<()> {
    let class = buffer_class(context)?;
    let prototype = class.get(js_string!("prototype"), context)?;
    array.set_prototype(prototype.as_object());
    Ok(())
}

fn is_buffer(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    let class = buffer_class(context)?;
    if !value.is_object() {
        return Ok(false);
    }
    value.instance_of(&class.into(), context)
}

fn construct_buffer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // `new Buffer(…)`, which Node has long deprecated, is `Buffer.from` / `Buffer.alloc`.
    let first = arg(args, 0);
    if first.is_number() {
        let size = first.to_number(context)? as usize;
        return make_buffer(context, vec![0; size]);
    }
    buffer_from(&JsValue::undefined(), args, context)
}

fn buffer_from(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = arg(args, 0);
    if value.is_string() {
        let text = text_of(&value, context)?;
        let encoding = match arg(args, 1) {
            e if e.is_undefined() => String::new(),
            e => text_of(&e, context)?,
        };
        return make_buffer(context, encode(&text, &encoding)?);
    }
    if let Some(object) = value.as_object() {
        // An `ArrayBuffer`: a view of it, sharing its memory, as Node's is.
        if JsArrayBuffer::from_object(object.clone()).is_ok() {
            let constructor = context
                .intrinsics()
                .constructors()
                .typed_uint8_array()
                .constructor();
            let mut view_args = vec![value.clone()];
            for at in 1..3 {
                let given = arg(args, at);
                if !given.is_undefined() {
                    view_args.push(given);
                }
            }
            let view = constructor.construct(&view_args, None, context)?;
            as_buffer(context, &view)?;
            return Ok(view.into());
        }
        if let Some(bytes) = bytes_of(&value, context)? {
            return make_buffer(context, bytes);
        }
        // An array of numbers, or `{ type: 'Buffer', data: [...] }` from `toJSON`.
        let list = match JsArray::from_object(object.clone()) {
            Ok(list) => Some(list),
            Err(_) => object
                .get(js_string!("data"), context)?
                .as_object()
                .and_then(|d| JsArray::from_object(d.clone()).ok()),
        };
        if let Some(list) = list {
            let mut bytes = Vec::new();
            for at in 0..list.length(context)? {
                bytes.push(list.get(at, context)?.to_number(context)? as i64 as u8);
            }
            return make_buffer(context, bytes);
        }
    }
    Err(type_error(
        "Buffer.from: the first argument must be a string, a Buffer, an ArrayBuffer, an Array or an array-like object",
    ))
}

fn this_bytes(this: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    bytes_of(this, context)?.ok_or_else(|| type_error("not a Buffer"))
}

const BUFFER_STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("from", 3, buffer_from),
    ("alloc", 3, |_, args, cx| {
        let size = arg(args, 0).to_number(cx)? as usize;
        let fill = arg(args, 1);
        let bytes = if fill.is_undefined() {
            vec![0; size]
        } else if fill.is_number() {
            vec![fill.to_number(cx)? as i64 as u8; size]
        } else {
            let pattern = encode(&text_of(&fill, cx)?, "utf8")?;
            if pattern.is_empty() {
                vec![0; size]
            } else {
                pattern.iter().copied().cycle().take(size).collect()
            }
        };
        make_buffer(cx, bytes)
    }),
    ("allocUnsafe", 1, |_, args, cx| {
        let size = arg(args, 0).to_number(cx)? as usize;
        make_buffer(cx, vec![0; size])
    }),
    ("isBuffer", 1, |_, args, cx| Ok(is_buffer(&arg(args, 0), cx)?.into())),
    ("isEncoding", 1, |_, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        Ok(encode("", &name).is_ok().into())
    }),
    ("byteLength", 2, |_, args, cx| {
        let value = arg(args, 0);
        if let Some(bytes) = bytes_of(&value, cx)? {
            return Ok(bytes.len().into());
        }
        let encoding = match arg(args, 1) {
            e if e.is_undefined() => String::new(),
            e => text_of(&e, cx)?,
        };
        Ok(encode(&text_of(&value, cx)?, &encoding)?.len().into())
    }),
    ("concat", 2, |_, args, cx| {
        let list = arg(args, 0);
        let list = list
            .as_object()
            .and_then(|l| JsArray::from_object(l.clone()).ok())
            .ok_or_else(|| type_error("Buffer.concat: the list must be an Array"))?;
        let mut bytes = Vec::new();
        for at in 0..list.length(cx)? {
            let item = list.get(at, cx)?;
            bytes.extend(bytes_of(&item, cx)?.unwrap_or_default());
        }
        let total = arg(args, 1);
        if !total.is_undefined() {
            bytes.resize(total.to_number(cx)? as usize, 0);
        }
        make_buffer(cx, bytes)
    }),
    ("compare", 2, |_, args, cx| {
        let a = bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
        let b = bytes_of(&arg(args, 1), cx)?.unwrap_or_default();
        Ok((a.cmp(&b) as i32).into())
    }),
];

const BUFFER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("toString", 3, |this, args, cx| {
        let bytes = this_bytes(this, cx)?;
        let encoding = match arg(args, 0) {
            e if e.is_undefined() => String::new(),
            e => text_of(&e, cx)?,
        };
        let start = match arg(args, 1) {
            s if s.is_undefined() => 0,
            s => (s.to_number(cx)? as usize).min(bytes.len()),
        };
        let end = match arg(args, 2) {
            e if e.is_undefined() => bytes.len(),
            e => (e.to_number(cx)? as usize).clamp(start, bytes.len()),
        };
        Ok(JsString::from(decode(&bytes[start..end], &encoding)?.as_str()).into())
    }),
    ("toJSON", 0, |this, _, cx| {
        let bytes = this_bytes(this, cx)?;
        let data = JsArray::from_iter(bytes.into_iter().map(JsValue::from), cx);
        let out = JsObject::with_object_proto(cx.intrinsics());
        out.set(js_string!("type"), js_string!("Buffer"), false, cx)?;
        out.set(js_string!("data"), data, false, cx)?;
        Ok(out.into())
    }),
    ("equals", 1, |this, args, cx| {
        let a = this_bytes(this, cx)?;
        let b = bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
        Ok((a == b).into())
    }),
    ("compare", 1, |this, args, cx| {
        let a = this_bytes(this, cx)?;
        let b = bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
        Ok((a.cmp(&b) as i32).into())
    }),
    // Node's `slice` shares memory, as `subarray` does — unlike `Uint8Array`'s.
    ("slice", 2, |this, args, cx| {
        let Some(object) = this.as_object() else {
            return Err(type_error("not a Buffer"));
        };
        let subarray = object.get(js_string!("subarray"), cx)?;
        let view = subarray
            .as_callable()
            .ok_or_else(|| type_error("not a Buffer"))?
            .call(this, args, cx)?;
        if let Some(view) = view.as_object() {
            as_buffer(cx, &view)?;
        }
        Ok(view)
    }),
    ("write", 4, |this, args, cx| {
        let Some(object) = this.as_object() else {
            return Err(type_error("not a Buffer"));
        };
        let target = JsUint8Array::from_object(object.clone())?;
        let text = text_of(&arg(args, 0), cx)?;
        let offset = match arg(args, 1) {
            o if o.is_number() => o.to_number(cx)? as usize,
            _ => 0,
        };
        let encoding = [arg(args, 1), arg(args, 2), arg(args, 3)]
            .into_iter()
            .find(JsValue::is_string)
            .map(|e| text_of(&e, cx))
            .transpose()?
            .unwrap_or_default();
        let bytes = encode(&text, &encoding)?;
        let room = target.length(cx)?.saturating_sub(offset);
        let count = bytes.len().min(room);
        for (at, byte) in bytes.into_iter().take(count).enumerate() {
            target.set(offset + at, byte, true, cx)?;
        }
        Ok(count.into())
    }),
    ("readUInt8", 1, |this, args, cx| {
        let bytes = this_bytes(this, cx)?;
        let at = arg(args, 0).to_number(cx).unwrap_or(0.) as usize;
        bytes
            .get(at)
            .map(|b| JsValue::from(*b))
            .ok_or_else(|| JsNativeError::range().with_message("offset out of range").into())
    }),
];

fn function(context: &mut Context, name: &str, length: usize, f: NativeFunctionPointer) -> JsValue {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

fn constructor(context: &mut Context, name: &str, length: usize, f: NativeFunctionPointer) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .constructor(true)
        .build()
        .into()
}

pub fn install(context: &mut Context) -> JsResult<()> {
    for (name, f) in [("atob", atob as NativeFunctionPointer), ("btoa", btoa)] {
        context.register_global_callable(JsString::from(name), 1, NativeFunction::from_fn_ptr(f))?;
    }
    let encoder = constructor(context, "TextEncoder", 0, construct_encoder);
    context.register_global_property(js_string!("TextEncoder"), encoder, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    let decoder = constructor(context, "TextDecoder", 0, construct_decoder);
    context.register_global_property(js_string!("TextDecoder"), decoder, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;

    // Buffer: a constructor whose prototype inherits Uint8Array's.
    let buffer = constructor(context, "Buffer", 2, construct_buffer);
    let (uint8_prototype, uint8_constructor) = {
        let uint8 = context.intrinsics().constructors().typed_uint8_array();
        (uint8.prototype(), uint8.constructor())
    };
    let prototype = JsObject::with_null_proto();
    prototype.set_prototype(Some(uint8_prototype));
    for (name, length, f) in BUFFER_METHODS {
        let method = function(context, name, *length, *f);
        prototype.define_property_or_throw(
            JsString::from(*name),
            PropertyDescriptor::builder()
                .value(method)
                .writable(true)
                .enumerable(false)
                .configurable(true),
            context,
        )?;
    }
    prototype.define_property_or_throw(
        js_string!("constructor"),
        PropertyDescriptor::builder()
            .value(buffer.clone())
            .writable(true)
            .enumerable(false)
            .configurable(true),
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
    for (name, length, f) in BUFFER_STATICS {
        let method = function(context, name, *length, *f);
        buffer.set(JsString::from(*name), method, false, context)?;
    }
    context.register_global_property(js_string!("Buffer"), buffer, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_reads_both_alphabets_and_round_trips() {
        assert_eq!(base64(b"hello, world", false), "aGVsbG8sIHdvcmxk");
        assert_eq!(base64(&[0xfb, 0xff], false), "+/8=");
        assert_eq!(base64(&[0xfb, 0xff], true), "-_8");
        assert_eq!(unbase64("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(unbase64("aGVs bG8=\n").unwrap(), b"hello");
        assert!(unbase64("not*base64").is_none());
        assert_eq!(unhex("cafe"), [0xca, 0xfe]);
    }
}
