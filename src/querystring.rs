//! `querystring` and `punycode`: two of Node's older modules, still bundled into plugins by the
//! npm packages that use them (`request`-era HTTP clients, URL parsers).
//!
//! `querystring` reads and writes `a=1&b=2&b=3` by Node's own rules: `+` is a space, a key given
//! twice becomes an array, a percent escape that is not valid UTF-8 is read byte by byte rather
//! than refused, and the answer of `parse` has no prototype. `punycode` is RFC 3492, with
//! `toASCII`/`toUnicode` for whole domains and e-mail addresses, as the `punycode.js` package
//! Node ships has it.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer, object::builtins::JsArray,
};

use crate::native::{arg, array, exports, get, js_text, list_of, object, text_of};

// ─── Escaping ───────────────────────────────────────────────────────────────────────────────

/// What `querystring.escape` and `encodeURIComponent` leave as they are.
fn unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&c)
}

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Percent escapes read as bytes — an escape that is not one is kept as it is — and the bytes
/// read as UTF-8, a bad sequence as U+FFFD: Node's `unescapeBuffer`, then `toString`.
pub(crate) fn unescape_bytes(text: &str, plus_is_space: bool) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(if plus_is_space && c == b'+' { b' ' } else { c });
        i += 1;
    }
    out
}

pub(crate) fn unescape(text: &str) -> String {
    String::from_utf8_lossy(&unescape_bytes(text, false)).into_owned()
}

// ─── querystring ────────────────────────────────────────────────────────────────────────────

/// A string option: `sep` or `eq`, its default when not given or empty.
fn separator(value: &JsValue, default: &str, context: &mut Context) -> JsResult<String> {
    if value.is_null_or_undefined() {
        return Ok(default.to_owned());
    }
    let text = text_of(value, context)?;
    Ok(if text.is_empty() {
        default.to_owned()
    } else {
        text
    })
}

/// Decodes one key or value: `+` as a space, then the caller's `decodeURIComponent`, or Node's
/// own forgiving unescape.
fn decode_part(text: &str, decoder: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let spaced = text.replace('+', " ");
    if decoder.is_callable()
        && let Ok(value) = crate::call(context, decoder, &JsValue::undefined(), &[js_text(&spaced)])
    {
        return Ok(value);
    }
    Ok(js_text(&unescape(&spaced)))
}

fn parse(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let out = JsObject::with_null_proto();
    let input = arg(args, 0);
    if !input.is_string() {
        return Ok(out.into());
    }
    let text = text_of(&input, context)?;
    if text.is_empty() {
        return Ok(out.into());
    }
    let sep = separator(&arg(args, 1), "&", context)?;
    let eq = separator(&arg(args, 2), "=", context)?;
    let options = arg(args, 3);
    let max_keys = match get(&options, "maxKeys", context)? {
        m if m.is_number() => m.to_number(context)?.max(0.) as usize,
        _ => 1000,
    };
    let decoder = get(&options, "decodeURIComponent", context)?;
    for (seen, pair) in text.split(sep.as_str()).enumerate() {
        if max_keys > 0 && seen >= max_keys {
            break;
        }
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.find(eq.as_str()) {
            Some(at) => (&pair[..at], &pair[at + eq.len()..]),
            None => (pair, ""),
        };
        let key = decode_part(key, &decoder, context)?;
        let value = decode_part(value, &decoder, context)?;
        let key = key.to_property_key(context)?;
        let existing = out.get(key.clone(), context)?;
        if existing.is_undefined() && !out.has_own_property(key.clone(), context)? {
            out.set(key, value, false, context)?;
        } else if let Some(list) = existing
            .as_object()
            .and_then(|o| JsArray::from_object(o.clone()).ok())
        {
            list.push(value, context)?;
        } else {
            let list = array(context, vec![existing, value]);
            out.set(key, list, false, context)?;
        }
    }
    Ok(out.into())
}

/// A value as `stringify` writes it: strings, finite numbers, bigints and booleans; anything
/// else is the empty string.
fn primitive(value: &JsValue, context: &mut Context) -> JsResult<String> {
    if value.is_string() || value.is_bigint() || value.is_boolean() {
        return text_of(value, context);
    }
    if let Some(n) = value.as_number() {
        return Ok(if n.is_finite() {
            crate::print(value)
        } else {
            String::new()
        });
    }
    Ok(String::new())
}

fn encode_part(text: &str, encoder: &JsValue, context: &mut Context) -> JsResult<String> {
    if encoder.is_callable() {
        let out = crate::call(context, encoder, &JsValue::undefined(), &[js_text(text)])?;
        return text_of(&out, context);
    }
    Ok(escape(text))
}

fn stringify(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = arg(args, 0);
    let Some(source) = input.as_object() else {
        return Ok(js_text(""));
    };
    let sep = separator(&arg(args, 1), "&", context)?;
    let eq = separator(&arg(args, 2), "=", context)?;
    let encoder = get(&arg(args, 3), "encodeURIComponent", context)?;
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
        std::slice::from_ref(&input),
    )?;
    let mut parts = Vec::new();
    for key in list_of(&keys, context)? {
        let name = text_of(&key, context)?;
        let value = source.get(key.to_property_key(context)?, context)?;
        let key = encode_part(&name, &encoder, context)?;
        if value.as_object().is_some_and(|o| o.is_array()) {
            for item in list_of(&value, context)? {
                let item = primitive(&item, context)?;
                parts.push(format!(
                    "{key}{eq}{}",
                    encode_part(&item, &encoder, context)?
                ));
            }
        } else {
            let value = primitive(&value, context)?;
            parts.push(format!(
                "{key}{eq}{}",
                encode_part(&value, &encoder, context)?
            ));
        }
    }
    Ok(js_text(&parts.join(&sep)))
}

const QUERYSTRING: &[(&str, usize, NativeFunctionPointer)] = &[
    ("parse", 4, parse),
    ("decode", 4, parse),
    ("stringify", 4, stringify),
    ("encode", 4, stringify),
    ("escape", 1, |_, args, cx| {
        Ok(js_text(&escape(&text_of(&arg(args, 0), cx)?)))
    }),
    ("unescape", 2, |_, args, cx| {
        Ok(js_text(&unescape(&text_of(&arg(args, 0), cx)?)))
    }),
    ("unescapeBuffer", 2, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        crate::encoding::make_buffer(cx, unescape_bytes(&text, arg(args, 1).to_boolean()))
    }),
];

pub(crate) fn querystring_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, QUERYSTRING)?;
    Ok(module.into())
}

// ─── punycode ───────────────────────────────────────────────────────────────────────────────

const BASE: u32 = 36;
const T_MIN: u32 = 1;
const T_MAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
const INITIAL_N: u32 = 128;

fn adapt(mut delta: u32, points: u32, first: bool) -> u32 {
    delta = if first { delta / DAMP } else { delta / 2 };
    delta += delta / points;
    let mut k = 0;
    while delta > ((BASE - T_MIN) * T_MAX) / 2 {
        delta /= BASE - T_MIN;
        k += BASE;
    }
    k + (BASE - T_MIN + 1) * delta / (delta + SKEW)
}

fn digit(d: u32) -> char {
    (if d < 26 {
        b'a' + d as u8
    } else {
        b'0' + (d - 26) as u8
    }) as char
}

fn basic_value(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32 + 26),
        'A'..='Z' => Some(c as u32 - 'A' as u32),
        'a'..='z' => Some(c as u32 - 'a' as u32),
        _ => None,
    }
}

/// RFC 3492: code points to the ASCII that stands for them.
pub(crate) fn punycode_encode(input: &[u32]) -> Option<String> {
    let mut out: String = input
        .iter()
        .filter(|&&c| c < 0x80)
        .map(|&c| char::from_u32(c).unwrap_or('?'))
        .collect();
    let basic = out.len() as u32;
    let mut handled = basic;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (INITIAL_N, 0u32, INITIAL_BIAS);
    while (handled as usize) < input.len() {
        let m = input.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(handled + 1)?)?;
        n = m;
        for &c in input {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        T_MIN
                    } else if k >= bias + T_MAX {
                        T_MAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, handled + 1, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(out)
}

/// RFC 3492: the ASCII back to code points.
pub(crate) fn punycode_decode(input: &str) -> Option<Vec<u32>> {
    let (basic, rest) = match input.rfind('-') {
        Some(at) => (&input[..at], &input[at + 1..]),
        None => ("", input),
    };
    let mut out: Vec<u32> = basic.chars().map(|c| c as u32).collect();
    if out.iter().any(|&c| c >= 0x80) {
        return None;
    }
    let (mut n, mut i, mut bias) = (INITIAL_N, 0u32, INITIAL_BIAS);
    let mut chars = rest.chars().peekable();
    while chars.peek().is_some() {
        let old_i = i;
        let mut w = 1u32;
        let mut k = BASE;
        loop {
            let d = basic_value(chars.next()?)?;
            i = i.checked_add(d.checked_mul(w)?)?;
            let t = if k <= bias {
                T_MIN
            } else if k >= bias + T_MAX {
                T_MAX
            } else {
                k - bias
            };
            if d < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = out.len() as u32 + 1;
        bias = adapt(i - old_i, len, old_i == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        out.insert(i as usize, n);
        i += 1;
    }
    Some(out)
}

fn code_points(text: &str) -> Vec<u32> {
    text.chars().map(|c| c as u32).collect()
}

fn from_points(points: &[u32]) -> String {
    points
        .iter()
        .map(|&c| char::from_u32(c).unwrap_or('\u{fffd}'))
        .collect()
}

/// Each label of a domain — or of the part of an e-mail address after its `@` — mapped by `f`,
/// the separators IDNA allows (`。`, `．`, `｡`) read as dots.
fn map_domain(text: &str, f: impl Fn(&str) -> Option<String>) -> Option<String> {
    let (local, domain) = match text.rfind('@') {
        Some(at) => (&text[..=at], &text[at + 1..]),
        None => ("", text),
    };
    let domain = domain.replace(['\u{3002}', '\u{ff0e}', '\u{ff61}'], ".");
    let labels: Option<Vec<String>> = domain.split('.').map(f).collect();
    Some(format!("{local}{}", labels?.join(".")))
}

pub(crate) fn to_ascii(domain: &str) -> Option<String> {
    map_domain(domain, |label| {
        if label.is_ascii() {
            Some(label.to_owned())
        } else {
            Some(format!("xn--{}", punycode_encode(&code_points(label))?))
        }
    })
}

pub(crate) fn to_unicode(domain: &str) -> Option<String> {
    map_domain(domain, |label| match label.get(..4) {
        Some(prefix) if prefix.eq_ignore_ascii_case("xn--") => Some(from_points(&punycode_decode(
            &label[4..].to_ascii_lowercase(),
        )?)),
        _ => Some(label.to_owned()),
    })
}

fn punycode_error(_: &mut Context, what: &str) -> boa_engine::JsError {
    boa_engine::JsNativeError::range()
        .with_message(what.to_owned())
        .into()
}

const PUNYCODE: &[(&str, usize, NativeFunctionPointer)] = &[
    ("encode", 1, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        punycode_encode(&code_points(&text))
            .map(|t| js_text(&t))
            .ok_or_else(|| punycode_error(cx, "Overflow: input needs wider integers to process"))
    }),
    ("decode", 1, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        punycode_decode(&text)
            .map(|p| js_text(&from_points(&p)))
            .ok_or_else(|| punycode_error(cx, "Invalid input"))
    }),
    ("toASCII", 1, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        to_ascii(&text)
            .map(|t| js_text(&t))
            .ok_or_else(|| punycode_error(cx, "Overflow: input needs wider integers to process"))
    }),
    ("toUnicode", 1, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        to_unicode(&text)
            .map(|t| js_text(&t))
            .ok_or_else(|| punycode_error(cx, "Invalid input"))
    }),
];

pub(crate) fn punycode_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, PUNYCODE)?;
    module.set(js_string!("version"), js_text("2.1.0"), false, context)?;
    let ucs2 = object(context);
    exports(
        context,
        &ucs2,
        &[
            ("decode", 1, |_, args, cx| {
                // UTF-16 code units paired into code points; a lone surrogate stays one.
                let units = arg(args, 0).to_string(cx)?.to_vec();
                let points: Vec<JsValue> = char::decode_utf16(units.iter().copied())
                    .map(|c| {
                        JsValue::from(
                            c.map_or_else(|e| u32::from(e.unpaired_surrogate()), |c| c as u32),
                        )
                    })
                    .collect();
                Ok(array(cx, points))
            }),
            ("encode", 1, |_, args, cx| {
                let mut units: Vec<u16> = Vec::new();
                for point in list_of(&arg(args, 0), cx)? {
                    let point = point.to_number(cx)? as u32;
                    match char::from_u32(point) {
                        Some(c) => units.extend(c.encode_utf16(&mut [0; 2]).iter()),
                        None => units.push(point as u16),
                    }
                }
                Ok(JsString::from(units.as_slice()).into())
            }),
        ],
    )?;
    module.set(js_string!("ucs2"), ucs2, false, context)?;
    Ok(module.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punycode_round_trips() {
        assert_eq!(
            punycode_encode(&code_points("mañana")).unwrap(),
            "maana-pta"
        );
        assert_eq!(
            from_points(&punycode_decode("maana-pta").unwrap()),
            "mañana"
        );
        assert_eq!(punycode_encode(&code_points("☃-⌘")).unwrap(), "--dqo34k");
        assert_eq!(to_ascii("mañana.com").unwrap(), "xn--maana-pta.com");
        assert_eq!(to_unicode("xn--maana-pta.com").unwrap(), "mañana.com");
        assert_eq!(
            to_ascii("джумла@джpумлатест.bрфa").unwrap(),
            "джумла@xn--p-8sbkgc5ag7bhce.xn--ba-lmcq"
        );
    }

    #[test]
    fn escapes_as_node() {
        assert_eq!(escape("a b&c=ü"), "a%20b%26c%3D%C3%BC");
        assert_eq!(unescape("a%20b%zz%C3%BC"), "a b%zzü");
    }
}
