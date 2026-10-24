//! Bytes and text: `atob` and `btoa`, `TextEncoder` and `TextDecoder`, and Node's `Buffer` —
//! what a plugin bundles expect from a browser and from Electron's Node.
//!
//! A `Buffer` is a `Uint8Array`, as Node's is: its prototype is `Buffer.prototype`, which
//! inherits `Uint8Array.prototype`, so indexing, `length`, `subarray` and the rest are the
//! engine's own, and only what Node adds is here.

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArrayBuffer, JsUint8Array},
    },
    property::Attribute,
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

/// The bytes of an `ArrayBuffer`, a typed array or a `DataView`.
pub fn bytes_of(value: &JsValue, context: &mut Context) -> JsResult<Option<Vec<u8>>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if let Ok(buffer) = JsArrayBuffer::from_object(object.clone()) {
        return Ok(Some(buffer.data().map(|d| d.to_vec()).unwrap_or_default()));
    }
    let buffer = object.get(js_string!("buffer"), context)?;
    let Some(buffer) = buffer.as_object() else {
        return Ok(None);
    };
    let Ok(buffer) = JsArrayBuffer::from_object(buffer.clone()) else {
        return Ok(None);
    };
    let offset = object
        .get(js_string!("byteOffset"), context)?
        .to_number(context)? as usize;
    let length = object
        .get(js_string!("byteLength"), context)?
        .to_number(context)? as usize;
    let Some(all) = buffer.data() else {
        return Ok(Some(Vec::new()));
    };
    let end = (offset + length).min(all.len());
    Ok(Some(all[offset.min(end)..end].to_vec()))
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
        let text = if input.is_undefined() {
            String::new()
        } else {
            input.to_string(cx)?.to_std_string_lossy()
        };
        uint8(cx, text.into_bytes())
    });
    encoder.set(js_string!("encode"), encode, false, context)?;
    let encode_into = function(context, "encodeInto", 2, |_, args, cx| {
        let text = arg(args, 0).to_string(cx)?.to_std_string_lossy();
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
                .with_message(format!(
                    "TextDecoder: the encoding '{other}' is not supported"
                ))
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
    decoder.set(
        js_string!("encoding"),
        JsString::from(encoding),
        false,
        context,
    )?;
    decoder.set(js_string!("fatal"), fatal, false, context)?;
    decoder.set(js_string!("ignoreBOM"), ignore_bom, false, context)?;
    let decode_fn = function(context, "decode", 1, |this, args, cx| {
        let input = arg(args, 0);
        let bytes = if input.is_undefined() {
            Vec::new()
        } else {
            bytes_of(&input, cx)?.ok_or_else(|| {
                type_error("TextDecoder.decode: not an ArrayBuffer or a view of one")
            })?
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

/// A `Uint8Array` of `bytes`, made a `Buffer` (see `crate::buffer`).
pub fn make_buffer(context: &mut Context, bytes: Vec<u8>) -> JsResult<JsValue> {
    crate::buffer::make(context, bytes)
}

fn function(context: &mut Context, name: &str, length: usize, f: NativeFunctionPointer) -> JsValue {
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

pub fn install(context: &mut Context) -> JsResult<()> {
    for (name, f) in [("atob", atob as NativeFunctionPointer), ("btoa", btoa)] {
        context.register_global_callable(
            JsString::from(name),
            1,
            NativeFunction::from_fn_ptr(f),
        )?;
    }
    let encoder = constructor(context, "TextEncoder", 0, construct_encoder);
    context.register_global_property(
        js_string!("TextEncoder"),
        encoder,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    let decoder = constructor(context, "TextDecoder", 0, construct_decoder);
    context.register_global_property(
        js_string!("TextDecoder"),
        decoder,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;

    crate::buffer::install(context)?;
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
    }
}
