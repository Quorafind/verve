//! `string_decoder`: bytes that arrive in pieces, read as text without breaking a character
//! that a piece cut in two.
//!
//! A `StringDecoder` keeps the bytes of a character not yet complete — the start of a UTF-8
//! sequence, half a UTF-16 unit or a high surrogate, a base64 group short of three — and puts
//! them before the next piece. `end()` writes out what is left: a replacement character for an
//! unfinished UTF-8 sequence, as Node's does, or the padded base64 of the last bytes.
//!
//! The bytes held back are kept in the decoder's `lastChar`, with `lastNeed` and `lastTotal`,
//! the fields Node's own decoder has.

use boa_engine::{
    Context, JsResult, JsString, JsValue, js_string, native_function::NativeFunctionPointer,
};

use crate::native::{ClassSpec, arg, class, get, js_text, text_of};

/// Node's name for an encoding a decoder reads, or `None` for one it does not know.
pub(crate) fn normalize_encoding(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "" | "utf8" | "utf-8" => "utf8",
        "ucs2" | "ucs-2" | "utf16le" | "utf-16le" => "utf16le",
        "latin1" | "binary" => "latin1",
        "base64" => "base64",
        "base64url" => "base64url",
        "hex" => "hex",
        "ascii" => "ascii",
        _ => return None,
    })
}

/// `bytes` as text in `encoding`, the whole of them.
pub(crate) fn decode_all(bytes: &[u8], encoding: &str) -> String {
    match encoding {
        "utf16le" => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        "latin1" => bytes.iter().map(|b| char::from(*b)).collect(),
        "ascii" => bytes.iter().map(|b| char::from(b & 0x7f)).collect(),
        "base64" => crate::encoding::base64(bytes, false),
        "base64url" => crate::encoding::base64(bytes, true),
        "hex" => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// How many bytes at the end of `bytes` begin a UTF-8 character they do not finish.
fn utf8_incomplete(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let byte = bytes[bytes.len() - back];
        if byte & 0xc0 == 0x80 {
            // A continuation byte: look further back for its lead.
            continue;
        }
        let needs = if byte & 0xe0 == 0xc0 {
            2
        } else if byte & 0xf0 == 0xe0 {
            3
        } else if byte & 0xf8 == 0xf0 {
            4
        } else {
            return 0;
        };
        return if needs > back { back } else { 0 };
    }
    0
}

/// Splits `bytes` into what can be decoded now and what must wait for the next piece.
fn split_complete(bytes: &[u8], encoding: &str) -> usize {
    match encoding {
        "utf8" => bytes.len() - utf8_incomplete(bytes),
        "utf16le" => {
            let mut end = bytes.len() - bytes.len() % 2;
            // A high surrogate waits for its low half.
            if end >= 2 {
                let unit = u16::from_le_bytes([bytes[end - 2], bytes[end - 1]]);
                if (0xd800..0xdc00).contains(&unit) {
                    end -= 2;
                }
            }
            end
        }
        "base64" | "base64url" => bytes.len() - bytes.len() % 3,
        _ => bytes.len(),
    }
}

fn pending_of(this: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    let held = get(this, "lastChar", context)?;
    let need = get(this, "lastNeed", context)?.to_number(context)?;
    let total = get(this, "lastTotal", context)?.to_number(context)?;
    let all = crate::encoding::bytes_of(&held, context)?.unwrap_or_default();
    let count = ((total - need).max(0.) as usize).min(all.len());
    Ok(all[..count].to_vec())
}

/// Keeps `bytes` as the character still to finish: in `lastChar` (four bytes, as Node's), with
/// `lastTotal` how many are held plus how many more it needs, and `lastNeed` how many more.
fn set_pending(
    this: &JsValue,
    bytes: &[u8],
    encoding: &str,
    context: &mut Context,
) -> JsResult<()> {
    let Some(target) = this.as_object() else {
        return Ok(());
    };
    let need = match encoding {
        "utf8" if !bytes.is_empty() => {
            let lead = bytes[0];
            let total = if lead & 0xe0 == 0xc0 {
                2
            } else if lead & 0xf0 == 0xe0 {
                3
            } else {
                4
            };
            total - bytes.len()
        }
        "utf16le" if !bytes.is_empty() => 4 - bytes.len().min(4),
        "base64" | "base64url" if !bytes.is_empty() => 3 - bytes.len(),
        _ => 0,
    };
    let mut held = vec![0u8; 4];
    held[..bytes.len().min(4)].copy_from_slice(&bytes[..bytes.len().min(4)]);
    let held = crate::encoding::make_buffer(context, held)?;
    target.set(js_string!("lastChar"), held, false, context)?;
    target.set(js_string!("lastNeed"), need, false, context)?;
    target.set(js_string!("lastTotal"), bytes.len() + need, false, context)?;
    Ok(())
}

fn encoding_of(this: &JsValue, context: &mut Context) -> JsResult<String> {
    text_of(&get(this, "encoding", context)?, context)
}

/// `write(buffer)`: the text of the characters this piece completes.
pub(crate) fn decoder_write(
    this: &JsValue,
    chunk: &JsValue,
    context: &mut Context,
) -> JsResult<String> {
    if chunk.is_string() {
        return text_of(chunk, context);
    }
    let encoding = encoding_of(this, context)?;
    let mut bytes = pending_of(this, context)?;
    bytes.extend(crate::encoding::bytes_of(chunk, context)?.unwrap_or_default());
    let end = split_complete(&bytes, &encoding);
    set_pending(this, &bytes[end..], &encoding, context)?;
    Ok(decode_all(&bytes[..end], &encoding))
}

/// `end(buffer?)`: the rest, and what was held back.
pub(crate) fn decoder_end(
    this: &JsValue,
    chunk: &JsValue,
    context: &mut Context,
) -> JsResult<String> {
    let mut out = if chunk.is_null_or_undefined() {
        String::new()
    } else {
        decoder_write(this, chunk, context)?
    };
    let encoding = encoding_of(this, context)?;
    let held = pending_of(this, context)?;
    if !held.is_empty() {
        out.push_str(&match encoding.as_str() {
            "utf8" => "\u{fffd}".to_owned(),
            _ => decode_all(&held, &encoding),
        });
    }
    set_pending(this, &[], &encoding, context)?;
    Ok(out)
}

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("write", 1, |this, args, cx| {
        let chunk = arg(args, 0);
        if !chunk.is_string() && crate::encoding::bytes_of(&chunk, cx)?.is_none() {
            return Err(crate::events::coded_type_error(
                cx,
                "The \"buf\" argument must be an instance of Buffer, TypedArray, or DataView.",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        Ok(js_text(&decoder_write(this, &chunk, cx)?))
    }),
    ("end", 1, |this, args, cx| {
        Ok(js_text(&decoder_end(this, &arg(args, 0), cx)?))
    }),
    ("text", 2, |this, args, cx| {
        let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
        let from = (arg(args, 1).to_number(cx)?.max(0.) as usize).min(bytes.len());
        let rest = crate::encoding::make_buffer(cx, bytes[from..].to_vec())?;
        Ok(js_text(&decoder_write(this, &rest, cx)?))
    }),
];

fn init(target: &boa_engine::JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let given = arg(args, 0);
    let name = if given.is_null_or_undefined() {
        String::new()
    } else {
        text_of(&given, context)?
    };
    let Some(encoding) = normalize_encoding(&name) else {
        return Err(crate::events::coded_type_error(
            context,
            &format!("Unknown encoding: {name}"),
            "ERR_UNKNOWN_ENCODING",
        ));
    };
    target.set(
        js_string!("encoding"),
        JsString::from(encoding),
        false,
        context,
    )?;
    set_pending(&target.clone().into(), &[], encoding, context)
}

/// `require('string_decoder')`: `{ StringDecoder }`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let (decoder, _) = class(
        context,
        ClassSpec {
            name: "StringDecoder",
            length: 1,
            init,
            methods: METHODS,
            ..Default::default()
        },
    )?;
    let module = crate::native::object(context);
    module.set(js_string!("StringDecoder"), decoder, false, context)?;
    Ok(module.into())
}

/// A new `StringDecoder` for `encoding`, from Rust: a readable stream's `setEncoding`.
pub(crate) fn new_decoder(context: &mut Context, encoding: &JsValue) -> JsResult<JsValue> {
    let module = crate::modules::node_module(context, "string_decoder")?;
    let class = get(&module, "StringDecoder", context)?;
    let class = class
        .as_object()
        .ok_or_else(|| crate::native::type_error("StringDecoder is not a class"))?;
    Ok(class
        .construct(std::slice::from_ref(encoding), None, context)?
        .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_cut_in_two_waits_for_its_other_half() {
        let euro = "€".as_bytes();
        assert_eq!(utf8_incomplete(&euro[..1]), 1);
        assert_eq!(utf8_incomplete(&euro[..2]), 2);
        assert_eq!(utf8_incomplete(euro), 0);
        assert_eq!(utf8_incomplete(b"abc"), 0);
        assert_eq!(split_complete(&[1, 2, 3, 4, 5], "base64"), 3);
        assert_eq!(split_complete(&[0x3d, 0xd8], "utf16le"), 0);
    }
}
