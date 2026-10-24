//! `Intl`, natively, without ICU's data: what plugins use it for — sorting text people read
//! (`Collator`), formatting numbers and dates for display (`NumberFormat`, `DateTimeFormat`),
//! choosing a plural (`PluralRules`), saying how long ago (`RelativeTimeFormat`), joining a list
//! (`ListFormat`), naming languages and regions (`DisplayNames`), reading a locale's parts
//! (`Locale`), and cutting text into what a reader sees as characters, words and sentences
//! (`Segmenter`, by Unicode's own rules).
//!
//! Every format is English-shaped (`en-US`) whatever locale is asked for, and the formatters
//! say so: `supportedLocalesOf` answers only English locales, and `resolvedOptions().locale`
//! is `en-US` for any other — so a library that has its own tables for other languages, as
//! date libraries do, uses them rather than English it did not ask for. Comparison is by Unicode
//! code point after folding accents and case, not by a language's own collation.
//!
//! Boa's own `Intl` is not built in (its ICU data is large), so `Date.prototype.toLocaleString`
//! and its kin, `Number.prototype.toLocaleString` and `String.prototype.localeCompare` are
//! routed here too.

mod date;
mod number;

use std::cmp::Ordering;

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction,
    Trace, js_string,
    native_function::NativeFunctionPointer,
    object::FunctionObjectBuilder,
    property::{Attribute, PropertyDescriptor},
};

use crate::native::{
    arg, array, closure, get, hidden, js_text, list_of, object, range_error, text_of, type_error,
};

// ─── Options ────────────────────────────────────────────────────────────────────────────────

pub(crate) fn option(options: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    get(options, key, context)
}

/// A string option, one of `allowed` when that is given: a `RangeError` otherwise, as `Intl`
/// answers a value it does not know.
pub(crate) fn option_str(
    options: &JsValue,
    key: &str,
    allowed: &[&str],
    context: &mut Context,
) -> JsResult<Option<String>> {
    let value = option(options, key, context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    let text = text_of(&value, context)?;
    if !allowed.is_empty() && !allowed.contains(&text.as_str()) {
        return Err(range_error(format!(
            "Value {text} out of range for Intl options property {key}"
        )));
    }
    Ok(Some(text))
}

pub(crate) fn option_bool(
    options: &JsValue,
    key: &str,
    context: &mut Context,
) -> JsResult<Option<bool>> {
    let value = option(options, key, context)?;
    Ok(if value.is_undefined() {
        None
    } else {
        Some(value.to_boolean())
    })
}

/// A whole-number option between `min` and `max`.
pub(crate) fn option_number(
    options: &JsValue,
    key: &str,
    min: usize,
    max: usize,
    context: &mut Context,
) -> JsResult<Option<usize>> {
    let value = option(options, key, context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    let n = value.to_number(context)?;
    if !n.is_finite() || n < min as f64 || n > max as f64 {
        return Err(range_error(format!("{key} value is out of range.")));
    }
    Ok(Some(n.floor() as usize))
}

/// The options object a constructor is given: `undefined` stays so, anything else is made an
/// object.
pub(crate) fn options_of(value: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    if value.is_undefined() {
        return Ok(JsValue::undefined());
    }
    Ok(value.to_object(context)?.into())
}

// ─── Locales ────────────────────────────────────────────────────────────────────────────────

/// The locale every format here is in.
pub(crate) const DEFAULT_LOCALE: &str = "en-US";

/// A BCP 47 language tag, in its parts.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Tag {
    pub(crate) language: String,
    pub(crate) script: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) variants: Vec<String>,
    /// The `-u-` extension's keywords: `hc` → `h23`, `nu` → `latn`.
    pub(crate) unicode: Vec<(String, String)>,
    /// Other extensions and private use, as written.
    pub(crate) other: Vec<String>,
}

impl Tag {
    /// `en-Latn-US`: language, script, region and variants.
    pub(crate) fn base_name(&self) -> String {
        let mut out = self.language.clone();
        for part in self
            .script
            .iter()
            .chain(self.region.iter())
            .chain(self.variants.iter())
        {
            out.push('-');
            out.push_str(part);
        }
        out
    }

    pub(crate) fn keyword(&self, key: &str) -> Option<&str> {
        self.unicode
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.base_name())?;
        if !self.unicode.is_empty() {
            f.write_str("-u")?;
            for (key, value) in &self.unicode {
                write!(f, "-{key}")?;
                if !value.is_empty() && value != "true" {
                    write!(f, "-{value}")?;
                }
            }
        }
        for other in &self.other {
            write!(f, "-{other}")?;
        }
        Ok(())
    }
}

fn is_alpha(s: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Reads a language tag, canonicalising its case (`EN-us` is `en-US`); `None` when it is not
/// one.
pub(crate) fn parse_tag(text: &str) -> Option<Tag> {
    let lower = text.to_ascii_lowercase();
    let mut parts = lower.split(['-', '_']).peekable();
    let language = parts.next()?;
    if !(is_alpha(language, 2..=3) || is_alpha(language, 5..=8)) {
        return None;
    }
    let language = match language {
        "iw" => "he",
        "in" => "id",
        "ji" => "yi",
        "mo" => "ro",
        other => other,
    };
    let mut tag = Tag {
        language: language.to_owned(),
        ..Tag::default()
    };
    if let Some(script) = parts.peek().filter(|p| is_alpha(p, 4..=4)) {
        let mut chars = script.chars();
        let first = chars.next()?.to_ascii_uppercase();
        tag.script = Some(std::iter::once(first).chain(chars).collect());
        parts.next();
    }
    if let Some(region) = parts
        .peek()
        .filter(|p| is_alpha(p, 2..=2) || (p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit())))
    {
        tag.region = Some(region.to_ascii_uppercase());
        parts.next();
    }
    while let Some(variant) = parts.peek().filter(|p| {
        let alnum = p.bytes().all(|b| b.is_ascii_alphanumeric());
        alnum && ((5..=8).contains(&p.len()) || (p.len() == 4 && p.as_bytes()[0].is_ascii_digit()))
    }) {
        tag.variants.push((*variant).to_owned());
        parts.next();
    }
    while let Some(singleton) = parts.next() {
        if singleton.len() != 1 || !singleton.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        if singleton == "u" {
            let mut key: Option<String> = None;
            let mut value: Vec<&str> = Vec::new();
            while let Some(part) = parts.peek().filter(|p| p.len() > 1) {
                if part.len() == 2 {
                    if let Some(key) = key.take() {
                        tag.unicode.push((key, value.join("-")));
                    }
                    key = Some((*part).to_owned());
                    value.clear();
                } else if key.is_some() {
                    value.push(part);
                }
                parts.next();
            }
            if let Some(key) = key {
                tag.unicode.push((key, value.join("-")));
            }
        } else {
            let rest: Vec<&str> = if singleton == "x" {
                parts.by_ref().collect()
            } else {
                let mut taken = Vec::new();
                while let Some(part) = parts.peek().filter(|p| p.len() > 1) {
                    taken.push(*part);
                    parts.next();
                }
                taken
            };
            if rest.is_empty() {
                return None;
            }
            tag.other.push(format!("{singleton}-{}", rest.join("-")));
        }
    }
    Some(tag)
}

/// The locales a constructor or `supportedLocalesOf` is given — a tag, a list of them, or
/// `Intl.Locale`s — canonical and without repeats. A `RangeError` for one that is not a tag.
pub(crate) fn requested_locales(locales: &JsValue, context: &mut Context) -> JsResult<Vec<Tag>> {
    if locales.is_undefined() {
        return Ok(Vec::new());
    }
    let is_locale = locales
        .as_object()
        .is_some_and(|o| o.downcast_ref::<LocaleData>().is_some());
    let items = if locales.is_string() || is_locale {
        vec![locales.clone()]
    } else {
        list_of(&locales.to_object(context)?.into(), context)?
    };
    let mut out: Vec<Tag> = Vec::new();
    for item in items {
        if !item.is_string() && !item.is_object() {
            return Err(type_error("Language ID should be string or object."));
        }
        let text = text_of(&item, context)?;
        let tag = parse_tag(&text)
            .ok_or_else(|| range_error(format!("Incorrect locale information provided: {text}")))?;
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    Ok(out)
}

/// Whether a format here is in `tag`'s language: English, any region.
fn supported(tag: &Tag) -> bool {
    tag.language == "en"
}

/// The locale a formatter resolves to: the first asked for when it is English, `en-US`
/// otherwise — with the asked-for `-u-` keywords, which a formatter may honour.
pub(crate) fn resolve_locale(locales: &JsValue, context: &mut Context) -> JsResult<Tag> {
    let requested = requested_locales(locales, context)?;
    let keywords = requested
        .first()
        .map(|t| t.unicode.clone())
        .unwrap_or_default();
    let mut tag = requested
        .into_iter()
        .find(supported)
        .unwrap_or_else(|| parse_tag(DEFAULT_LOCALE).expect("the default locale"));
    tag.unicode = keywords;
    tag.other.clear();
    Ok(tag)
}

fn supported_locales_of(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let requested = requested_locales(&arg(args, 0), context)?;
    let items: Vec<JsValue> = requested
        .iter()
        .filter(|t| supported(t))
        .map(|t| js_text(&t.to_string()))
        .collect();
    Ok(array(context, items))
}

// ─── Classes ────────────────────────────────────────────────────────────────────────────────

/// What an `Intl` constructor does with the prototype the object is to have and its
/// arguments: makes the formatter.
pub(crate) type Make = fn(&JsObject, &[JsValue], &mut Context) -> JsResult<JsObject>;

/// A class's methods and accessors, as [`crate::native::methods`] and
/// [`crate::native::accessors`] take them.
pub(crate) type Methods = &'static [(&'static str, usize, NativeFunctionPointer)];
pub(crate) type Accessors = &'static [(
    &'static str,
    NativeFunctionPointer,
    Option<NativeFunctionPointer>,
)];

#[derive(Clone, Copy)]
struct MakeBox(Make);

impl Finalize for MakeBox {}
// SAFETY: a function pointer holds no garbage-collected value.
unsafe impl Trace for MakeBox {
    boa_gc::empty_trace!();
}

/// An `Intl` class: callable with or without `new` (as `Intl.DateTimeFormat()` is), with
/// `supportedLocalesOf`, and `Intl.Name` as its `toStringTag`.
fn intl_class(
    context: &mut Context,
    name: &str,
    length: usize,
    make: Make,
    methods: Methods,
    accessors: Accessors,
) -> JsResult<JsObject> {
    let prototype = object(context);
    let constructor: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, args, (prototype, make): &(JsObject, MakeBox), context| {
                let own = match this.as_object() {
                    Some(new_target) => new_target
                        .get(js_string!("prototype"), context)?
                        .as_object(),
                    None => None,
                };
                let own = own.unwrap_or_else(|| prototype.clone());
                Ok((make.0)(&own, args, context)?.into())
            },
            (prototype.clone(), MakeBox(make)),
        ),
    )
    .name(JsString::from(name))
    .length(length)
    .constructor(true)
    .build()
    .into();
    crate::native::methods(context, &prototype, methods)?;
    crate::native::accessors(context, &prototype, accessors)?;
    hidden(
        &prototype,
        js_string!("constructor"),
        constructor.clone(),
        context,
    )?;
    prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(js_text(&format!("Intl.{name}")))
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    crate::native::methods(
        context,
        &constructor,
        &[("supportedLocalesOf", 1, supported_locales_of)],
    )?;
    Ok(constructor)
}

/// The data a formatter keeps, cloned out of `this`.
pub(crate) fn data_of<T: boa_engine::object::NativeObject + Clone>(
    this: &JsValue,
    what: &str,
) -> JsResult<T> {
    let incompatible = || {
        type_error(format!(
            "Method Intl.{what} called on incompatible receiver"
        ))
    };
    let object = this.as_object().ok_or_else(incompatible)?;
    let data = object.downcast_ref::<T>().ok_or_else(incompatible)?;
    Ok(data.clone())
}

/// A plain object of `fields`, those `undefined` left out: what `resolvedOptions` answers.
pub(crate) fn record(context: &mut Context, fields: Vec<(&str, JsValue)>) -> JsResult<JsValue> {
    let out = object(context);
    for (key, value) in fields {
        if !value.is_undefined() {
            out.set(JsString::from(key), value, false, context)?;
        }
    }
    Ok(out.into())
}

/// `[{ type, value }, …]`: what `formatToParts` answers.
pub(crate) fn parts_array(context: &mut Context, parts: &[(&str, String)]) -> JsResult<JsValue> {
    let mut items = Vec::with_capacity(parts.len());
    for (kind, value) in parts {
        let part = object(context);
        part.set(js_string!("type"), js_text(kind), false, context)?;
        part.set(js_string!("value"), js_text(value), false, context)?;
        items.push(part.into());
    }
    Ok(array(context, items))
}

/// The parts, joined: what `format` answers.
pub(crate) fn joined(parts: &[(&str, String)]) -> String {
    parts.iter().map(|(_, v)| v.as_str()).collect()
}

/// A function a `format`-like getter answers: made once for the formatter and kept on it, so
/// `arr.map(fmt.format)` works and `fmt.format === fmt.format` holds.
pub(crate) fn bound(
    this: &JsValue,
    key: &str,
    f: fn(&JsValue, &[JsValue], &JsObject, &mut Context) -> JsResult<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("incompatible receiver"))?;
    let slot = JsString::from(format!("__bound_{key}"));
    if object.has_own_property(slot.clone(), context)? {
        return object.get(slot, context);
    }
    let function = closure(context, "", 1, f, object.clone());
    hidden(&object, slot, function.clone(), context)?;
    Ok(function.into())
}

// ─── Collator ───────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct CollatorData {
    locale: String,
    usage: String,
    sensitivity: String,
    ignore_punctuation: bool,
    numeric: bool,
    case_first: String,
}

/// A letter without its accent: `é` is `e`, `Å` is `A`, `ß` is `ss` — for comparing as a reader
/// does, accents mattering only after the letters themselves.
fn strip_accent(c: char, out: &mut String) {
    let base = match c {
        'À'..='Å' => 'A',
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'Ç' | 'Ć' | 'Č' => 'C',
        'ç' | 'ć' | 'č' => 'c',
        'È'..='Ë' | 'Ē' | 'Ę' | 'Ě' => 'E',
        'è'..='ë' | 'ē' | 'ę' | 'ě' => 'e',
        'Ì'..='Ï' | 'Ī' => 'I',
        'ì'..='ï' | 'ī' | 'ı' => 'i',
        'Ñ' | 'Ń' | 'Ň' => 'N',
        'ñ' | 'ń' | 'ň' => 'n',
        'Ò'..='Ö' | 'Ø' | 'Ō' | 'Ő' => 'O',
        'ò'..='ö' | 'ø' | 'ō' | 'ő' => 'o',
        'Ù'..='Ü' | 'Ū' | 'Ů' | 'Ű' => 'U',
        'ù'..='ü' | 'ū' | 'ů' | 'ű' => 'u',
        'Ý' | 'Ÿ' => 'Y',
        'ý' | 'ÿ' => 'y',
        'Ś' | 'Š' => 'S',
        'ś' | 'š' => 's',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ź' | 'ż' | 'ž' => 'z',
        'Ł' => 'L',
        'ł' => 'l',
        'Ř' => 'R',
        'ř' => 'r',
        'Ď' => 'D',
        'ď' | 'đ' => 'd',
        'Ť' => 'T',
        'ť' => 't',
        'ß' => {
            out.push_str("ss");
            return;
        }
        'Æ' => {
            out.push_str("AE");
            return;
        }
        'æ' => {
            out.push_str("ae");
            return;
        }
        // Combining marks, as decomposed text has them.
        '\u{300}'..='\u{36f}' => return,
        other => other,
    };
    out.push(base);
}

fn without_accents(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        strip_accent(c, &mut out);
    }
    out
}

/// Text split into runs of digits and runs of everything else, for numeric comparison.
fn runs(text: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut digits = None;
    for (i, c) in text.char_indices() {
        let d = c.is_ascii_digit();
        match digits {
            Some(previous) if previous != d => {
                out.push((previous, &text[start..i]));
                start = i;
            }
            _ => {}
        }
        digits = Some(d);
    }
    if let Some(d) = digits {
        out.push((d, &text[start..]));
    }
    out
}

fn compare_text(a: &str, b: &str, numeric: bool) -> Ordering {
    if !numeric {
        return a.cmp(b);
    }
    let (ra, rb) = (runs(a), runs(b));
    for (x, y) in ra.iter().zip(rb.iter()) {
        let order = match (x.0, y.0) {
            (true, true) => {
                let (nx, ny) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny))
            }
            _ => x.1.cmp(y.1),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    ra.len().cmp(&rb.len())
}

/// Compares as a reader sorts: letters first (accents and case aside, digits by their value
/// when `numeric`), then accents (`accent`, `variant`), then case (`case`, `variant`) — lower
/// case first, as ICU's default, or upper first.
fn collate_with(
    a: &str,
    b: &str,
    numeric: bool,
    sensitivity: &str,
    ignore_punctuation: bool,
    case_first: &str,
) -> Ordering {
    let keep = |text: &str| -> String {
        if ignore_punctuation {
            text.chars()
                .filter(|c| !c.is_ascii_punctuation() && !c.is_whitespace())
                .collect()
        } else {
            text.to_owned()
        }
    };
    let (a, b) = (keep(a), keep(b));
    let (base_a, base_b) = (
        without_accents(&a).to_lowercase(),
        without_accents(&b).to_lowercase(),
    );
    let order = compare_text(&base_a, &base_b, numeric);
    if order != Ordering::Equal {
        return order;
    }
    if matches!(sensitivity, "accent" | "variant") {
        let order = compare_text(&a.to_lowercase(), &b.to_lowercase(), numeric);
        if order != Ordering::Equal {
            return order;
        }
    }
    if matches!(sensitivity, "case" | "variant") {
        // Same letters: lower case before upper (or after, with `caseFirst: "upper"`).
        let order = compare_text(&without_accents(&b), &without_accents(&a), numeric);
        let order = if case_first == "upper" {
            order.reverse()
        } else {
            order
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn make_collator(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let tag = resolve_locale(&arg(args, 0), context)?;
    let options = options_of(&arg(args, 1), context)?;
    let usage = option_str(&options, "usage", &["sort", "search"], context)?
        .unwrap_or_else(|| "sort".into());
    let numeric = option_bool(&options, "numeric", context)?
        .unwrap_or_else(|| tag.keyword("kn").is_some_and(|v| v != "false"));
    let case_first = option_str(&options, "caseFirst", &["upper", "lower", "false"], context)?
        .or_else(|| tag.keyword("kf").map(str::to_owned))
        .unwrap_or_else(|| "false".into());
    let sensitivity = option_str(
        &options,
        "sensitivity",
        &["base", "accent", "case", "variant"],
        context,
    )?
    .unwrap_or_else(|| "variant".into());
    let ignore_punctuation = option_bool(&options, "ignorePunctuation", context)?.unwrap_or(false);
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        CollatorData {
            locale: tag.base_name(),
            usage,
            sensitivity,
            ignore_punctuation,
            numeric,
            case_first,
        },
    ))
}

fn collator_compare(
    _: &JsValue,
    args: &[JsValue],
    collator: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let data: CollatorData = data_of(&collator.clone().into(), "Collator.prototype.compare")?;
    let a = text_of(&arg(args, 0), context)?;
    let b = text_of(&arg(args, 1), context)?;
    let order = collate_with(
        &a,
        &b,
        data.numeric,
        &data.sensitivity,
        data.ignore_punctuation,
        &data.case_first,
    );
    Ok((order as i32).into())
}

const COLLATOR_METHODS: Methods = &[("resolvedOptions", 0, |this, _, cx| {
    let data: CollatorData = data_of(this, "Collator.prototype.resolvedOptions")?;
    record(
        cx,
        vec![
            ("locale", js_text(&data.locale)),
            ("usage", js_text(&data.usage)),
            ("sensitivity", js_text(&data.sensitivity)),
            ("ignorePunctuation", data.ignore_punctuation.into()),
            ("collation", js_text("default")),
            ("numeric", data.numeric.into()),
            ("caseFirst", js_text(&data.case_first)),
        ],
    )
})];

const COLLATOR_ACCESSORS: Accessors = &[(
    "compare",
    |this, _, cx| bound(this, "compare", collator_compare, cx),
    None,
)];

// ─── PluralRules ────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct PluralData {
    locale: String,
    ordinal: bool,
    #[unsafe_ignore_trace]
    digits: number::Digits,
}

/// English's plural category for `n`, as formatted with `digits`.
fn plural_category(n: f64, ordinal: bool, digits: &number::Digits) -> &'static str {
    if !n.is_finite() {
        return "other";
    }
    if ordinal {
        let whole = n.abs().trunc();
        if whole != n.abs() {
            return "other";
        }
        let (ten, hundred) = ((whole % 10.) as u32, (whole % 100.) as u32);
        return match (ten, hundred) {
            (1, h) if h != 11 => "one",
            (2, h) if h != 12 => "two",
            (3, h) if h != 13 => "few",
            _ => "other",
        };
    }
    // `one` only for exactly 1 with no visible fraction: `1` but not `1.0`.
    let shown = number::round(n.abs(), digits);
    if shown.integer == "1" && shown.fraction.is_empty() {
        "one"
    } else {
        "other"
    }
}

fn make_plural_rules(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let tag = resolve_locale(&arg(args, 0), context)?;
    let options = options_of(&arg(args, 1), context)?;
    let kind = option_str(&options, "type", &["cardinal", "ordinal"], context)?
        .unwrap_or_else(|| "cardinal".into());
    let digits = number::digit_options(&options, 0, 3, context)?;
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        PluralData {
            locale: tag.base_name(),
            ordinal: kind == "ordinal",
            digits,
        },
    ))
}

const PLURAL_METHODS: Methods = &[
    ("select", 1, |this, args, cx| {
        let data: PluralData = data_of(this, "PluralRules.prototype.select")?;
        let n = arg(args, 0).to_number(cx)?;
        Ok(js_text(plural_category(n, data.ordinal, &data.digits)))
    }),
    ("selectRange", 2, |this, args, cx| {
        let data: PluralData = data_of(this, "PluralRules.prototype.selectRange")?;
        let n = arg(args, 1).to_number(cx)?;
        Ok(js_text(plural_category(n, data.ordinal, &data.digits)))
    }),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: PluralData = data_of(this, "PluralRules.prototype.resolvedOptions")?;
        let categories: Vec<JsValue> = if data.ordinal {
            ["few", "one", "two", "other"]
                .iter()
                .map(|c| js_text(c))
                .collect()
        } else {
            vec![js_text("one"), js_text("other")]
        };
        let categories = array(cx, categories);
        let mut fields = vec![
            ("locale", js_text(&data.locale)),
            (
                "type",
                js_text(if data.ordinal { "ordinal" } else { "cardinal" }),
            ),
        ];
        fields.extend(data.digits.resolved());
        fields.push(("pluralCategories", categories));
        record(cx, fields)
    }),
];

// ─── RelativeTimeFormat ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct RelativeData {
    locale: String,
    style: String,
    numeric: String,
}

/// The unit a `RelativeTimeFormat` is given, singular: `days` is `day`.
fn relative_unit(text: &str) -> JsResult<&'static str> {
    Ok(match text.strip_suffix('s').unwrap_or(text) {
        "second" => "second",
        "minute" => "minute",
        "hour" => "hour",
        "day" => "day",
        "week" => "week",
        "month" => "month",
        "quarter" => "quarter",
        "year" => "year",
        _ => {
            return Err(range_error(format!(
                "Invalid unit argument for format() '{text}'"
            )));
        }
    })
}

/// The unit's word in `style`, for `count`.
fn unit_word(unit: &str, style: &str, count: f64) -> String {
    let one = count == 1.;
    if style == "long" {
        return if one {
            unit.to_owned()
        } else {
            format!("{unit}s")
        };
    }
    match unit {
        "second" => "sec.",
        "minute" => "min.",
        "hour" => "hr.",
        "day" if one => "day",
        "day" => "days",
        "week" => "wk.",
        "month" => "mo.",
        "quarter" if one => "qtr.",
        "quarter" => "qtrs.",
        _ => "yr.",
    }
    .to_owned()
}

/// `numeric: "auto"`'s words: `yesterday`, `next month`, `now`.
fn relative_word(unit: &str, value: f64) -> Option<String> {
    if value.fract() != 0. || value.abs() > 1. {
        return None;
    }
    let word = match (unit, value as i32) {
        ("second", 0) => "now".to_owned(),
        ("minute", 0) => "this minute".to_owned(),
        ("hour", 0) => "this hour".to_owned(),
        ("day", 0) => "today".to_owned(),
        ("day", 1) => "tomorrow".to_owned(),
        ("day", -1) => "yesterday".to_owned(),
        ("week" | "month" | "quarter" | "year", 0) => format!("this {unit}"),
        ("week" | "month" | "quarter" | "year", 1) => format!("next {unit}"),
        ("week" | "month" | "quarter" | "year", -1) => format!("last {unit}"),
        _ => return None,
    };
    Some(word)
}

fn relative_parts(
    data: &RelativeData,
    value: f64,
    unit: &str,
) -> JsResult<Vec<(&'static str, String)>> {
    if !value.is_finite() {
        return Err(range_error("Invalid number value"));
    }
    if data.numeric == "auto"
        && let Some(word) = relative_word(unit, value)
    {
        return Ok(vec![("literal", word)]);
    }
    let count = value.abs();
    let digits = number::Digits::default();
    let formatted = number::group_integer(&number::round(count, &digits).joined(), "auto");
    let word = unit_word(unit, &data.style, count);
    let past = value < 0. || (value == 0. && value.is_sign_negative());
    let mut parts = Vec::new();
    if !past {
        parts.push(("literal", "in ".to_owned()));
    }
    parts.push(("integer", formatted));
    parts.push((
        "literal",
        if past {
            format!(" {word} ago")
        } else {
            format!(" {word}")
        },
    ));
    Ok(parts)
}

fn make_relative(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let tag = resolve_locale(&arg(args, 0), context)?;
    let options = options_of(&arg(args, 1), context)?;
    let style = option_str(&options, "style", &["long", "short", "narrow"], context)?
        .unwrap_or_else(|| "long".into());
    let numeric = option_str(&options, "numeric", &["always", "auto"], context)?
        .unwrap_or_else(|| "always".into());
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        RelativeData {
            locale: tag.base_name(),
            style,
            numeric,
        },
    ))
}

fn relative_args(
    this: &JsValue,
    args: &[JsValue],
    what: &str,
    context: &mut Context,
) -> JsResult<(Vec<(&'static str, String)>, &'static str)> {
    let data: RelativeData = data_of(this, what)?;
    let value = arg(args, 0).to_number(context)?;
    let unit = relative_unit(&text_of(&arg(args, 1), context)?)?;
    Ok((relative_parts(&data, value, unit)?, unit))
}

const RELATIVE_METHODS: Methods = &[
    ("format", 2, |this, args, cx| {
        let (parts, _) = relative_args(this, args, "RelativeTimeFormat.prototype.format", cx)?;
        Ok(js_text(&joined(&parts)))
    }),
    ("formatToParts", 2, |this, args, cx| {
        let (parts, unit) =
            relative_args(this, args, "RelativeTimeFormat.prototype.formatToParts", cx)?;
        let list = parts_array(cx, &parts)?;
        // The number's part says which unit it counts.
        for (item, (kind, _)) in list_of(&list, cx)?.iter().zip(&parts) {
            if *kind == "integer"
                && let Some(item) = item.as_object()
            {
                item.set(js_string!("unit"), js_text(unit), false, cx)?;
            }
        }
        Ok(list)
    }),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: RelativeData = data_of(this, "RelativeTimeFormat.prototype.resolvedOptions")?;
        record(
            cx,
            vec![
                ("locale", js_text(&data.locale)),
                ("style", js_text(&data.style)),
                ("numeric", js_text(&data.numeric)),
                ("numberingSystem", js_text("latn")),
            ],
        )
    }),
];

// ─── ListFormat ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct ListData {
    locale: String,
    kind: String,
    style: String,
}

fn list_parts(data: &ListData, items: &[String]) -> Vec<(&'static str, String)> {
    let word = match (data.kind.as_str(), data.style.as_str()) {
        ("conjunction", "long") => Some(" and "),
        ("conjunction", "short") => Some(" & "),
        ("disjunction", _) => Some(" or "),
        _ => None,
    };
    let separator = if data.kind == "unit" && data.style == "narrow" {
        " "
    } else {
        ", "
    };
    let mut parts = Vec::new();
    let count = items.len();
    for (at, item) in items.iter().enumerate() {
        if at > 0 {
            let literal = match word {
                Some(word) if at == count - 1 && count == 2 => word.to_owned(),
                // The serial comma: `a, b, and c`.
                Some(word) if at == count - 1 => format!(",{word}"),
                _ => separator.to_owned(),
            };
            parts.push(("literal", literal));
        }
        parts.push(("element", item.clone()));
    }
    parts
}

fn list_items(value: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    let items = crate::dom_events::iterate(value, context)?;
    items
        .iter()
        .map(|item| {
            if item.is_string() {
                text_of(item, context)
            } else {
                Err(type_error("Iterable yielded a value which is not a string"))
            }
        })
        .collect()
}

fn make_list_format(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let tag = resolve_locale(&arg(args, 0), context)?;
    let options = options_of(&arg(args, 1), context)?;
    let kind = option_str(
        &options,
        "type",
        &["conjunction", "disjunction", "unit"],
        context,
    )?
    .unwrap_or_else(|| "conjunction".into());
    let style = option_str(&options, "style", &["long", "short", "narrow"], context)?
        .unwrap_or_else(|| "long".into());
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        ListData {
            locale: tag.base_name(),
            kind,
            style,
        },
    ))
}

const LIST_METHODS: Methods = &[
    ("format", 1, |this, args, cx| {
        let data: ListData = data_of(this, "ListFormat.prototype.format")?;
        let items = list_items(&arg(args, 0), cx)?;
        Ok(js_text(&joined(&list_parts(&data, &items))))
    }),
    ("formatToParts", 1, |this, args, cx| {
        let data: ListData = data_of(this, "ListFormat.prototype.formatToParts")?;
        let items = list_items(&arg(args, 0), cx)?;
        parts_array(cx, &list_parts(&data, &items))
    }),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: ListData = data_of(this, "ListFormat.prototype.resolvedOptions")?;
        record(
            cx,
            vec![
                ("locale", js_text(&data.locale)),
                ("type", js_text(&data.kind)),
                ("style", js_text(&data.style)),
            ],
        )
    }),
];

// ─── DisplayNames ───────────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct DisplayData {
    locale: String,
    style: String,
    kind: String,
    fallback: String,
}

const LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arabic"),
    ("bg", "Bulgarian"),
    ("bn", "Bangla"),
    ("ca", "Catalan"),
    ("cs", "Czech"),
    ("da", "Danish"),
    ("de", "German"),
    ("el", "Greek"),
    ("en", "English"),
    ("eo", "Esperanto"),
    ("es", "Spanish"),
    ("et", "Estonian"),
    ("eu", "Basque"),
    ("fa", "Persian"),
    ("fi", "Finnish"),
    ("fil", "Filipino"),
    ("fr", "French"),
    ("ga", "Irish"),
    ("gl", "Galician"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("hr", "Croatian"),
    ("hu", "Hungarian"),
    ("hy", "Armenian"),
    ("id", "Indonesian"),
    ("is", "Icelandic"),
    ("it", "Italian"),
    ("ja", "Japanese"),
    ("ka", "Georgian"),
    ("kk", "Kazakh"),
    ("km", "Khmer"),
    ("ko", "Korean"),
    ("la", "Latin"),
    ("lt", "Lithuanian"),
    ("lv", "Latvian"),
    ("mk", "Macedonian"),
    ("ml", "Malayalam"),
    ("mn", "Mongolian"),
    ("mr", "Marathi"),
    ("ms", "Malay"),
    ("my", "Burmese"),
    ("nb", "Norwegian Bokmål"),
    ("ne", "Nepali"),
    ("nl", "Dutch"),
    ("nn", "Norwegian Nynorsk"),
    ("no", "Norwegian"),
    ("pa", "Punjabi"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("sq", "Albanian"),
    ("sr", "Serbian"),
    ("sv", "Swedish"),
    ("sw", "Swahili"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("th", "Thai"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("uz", "Uzbek"),
    ("vi", "Vietnamese"),
    ("zh", "Chinese"),
    ("zh-Hans", "Simplified Chinese"),
    ("zh-Hant", "Traditional Chinese"),
    ("en-US", "American English"),
    ("en-GB", "British English"),
    ("pt-BR", "Brazilian Portuguese"),
    ("es-MX", "Mexican Spanish"),
];

const REGIONS: &[(&str, &str)] = &[
    ("AR", "Argentina"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("BE", "Belgium"),
    ("BR", "Brazil"),
    ("CA", "Canada"),
    ("CH", "Switzerland"),
    ("CL", "Chile"),
    ("CN", "China"),
    ("CO", "Colombia"),
    ("CZ", "Czechia"),
    ("DE", "Germany"),
    ("DK", "Denmark"),
    ("EG", "Egypt"),
    ("ES", "Spain"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("GB", "United Kingdom"),
    ("GR", "Greece"),
    ("HK", "Hong Kong"),
    ("HU", "Hungary"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IN", "India"),
    ("IR", "Iran"),
    ("IT", "Italy"),
    ("JP", "Japan"),
    ("KR", "South Korea"),
    ("MX", "Mexico"),
    ("MY", "Malaysia"),
    ("NG", "Nigeria"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NZ", "New Zealand"),
    ("PH", "Philippines"),
    ("PK", "Pakistan"),
    ("PL", "Poland"),
    ("PT", "Portugal"),
    ("RO", "Romania"),
    ("RU", "Russia"),
    ("SA", "Saudi Arabia"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("TH", "Thailand"),
    ("TR", "Türkiye"),
    ("TW", "Taiwan"),
    ("UA", "Ukraine"),
    ("US", "United States"),
    ("VN", "Vietnam"),
    ("ZA", "South Africa"),
    ("001", "world"),
    ("150", "Europe"),
    ("419", "Latin America"),
];

const SCRIPTS: &[(&str, &str)] = &[
    ("Arab", "Arabic"),
    ("Cyrl", "Cyrillic"),
    ("Deva", "Devanagari"),
    ("Grek", "Greek"),
    ("Hans", "Simplified Han"),
    ("Hant", "Traditional Han"),
    ("Hebr", "Hebrew"),
    ("Hira", "Hiragana"),
    ("Jpan", "Japanese"),
    ("Kana", "Katakana"),
    ("Kore", "Korean"),
    ("Latn", "Latin"),
    ("Thai", "Thai"),
];

/// Currencies by code: their names, singular and plural.
pub(crate) const CURRENCIES: &[(&str, &str, &str)] = &[
    ("USD", "US dollar", "US dollars"),
    ("EUR", "euro", "euros"),
    ("GBP", "British pound", "British pounds"),
    ("JPY", "Japanese yen", "Japanese yen"),
    ("CNY", "Chinese yuan", "Chinese yuan"),
    ("INR", "Indian rupee", "Indian rupees"),
    ("CAD", "Canadian dollar", "Canadian dollars"),
    ("AUD", "Australian dollar", "Australian dollars"),
    ("CHF", "Swiss franc", "Swiss francs"),
    ("KRW", "South Korean won", "South Korean won"),
    ("BRL", "Brazilian real", "Brazilian reals"),
    ("RUB", "Russian ruble", "Russian rubles"),
    ("MXN", "Mexican peso", "Mexican pesos"),
    ("HKD", "Hong Kong dollar", "Hong Kong dollars"),
    ("NZD", "New Zealand dollar", "New Zealand dollars"),
    ("SEK", "Swedish krona", "Swedish kronor"),
    ("NOK", "Norwegian krone", "Norwegian kroner"),
    ("DKK", "Danish krone", "Danish kroner"),
    ("PLN", "Polish zloty", "Polish zlotys"),
    ("TWD", "New Taiwan dollar", "New Taiwan dollars"),
    ("SGD", "Singapore dollar", "Singapore dollars"),
    ("TRY", "Turkish lira", "Turkish Lira"),
    ("ZAR", "South African rand", "South African rand"),
    ("ILS", "Israeli new shekel", "Israeli new shekels"),
    ("VND", "Vietnamese dong", "Vietnamese dong"),
];

fn display_name(data: &DisplayData, code: &str) -> JsResult<Option<String>> {
    let found = match data.kind.as_str() {
        "language" => {
            let tag = parse_tag(code).ok_or_else(|| range_error("invalid_argument"))?;
            let base = tag.base_name();
            let exact = LANGUAGES
                .iter()
                .find(|(c, _)| *c == base)
                .map(|(_, n)| (*n).to_owned());
            exact.or_else(|| {
                let language = LANGUAGES.iter().find(|(c, _)| *c == tag.language)?.1;
                let region = tag
                    .region
                    .as_deref()
                    .and_then(|r| REGIONS.iter().find(|(c, _)| *c == r))
                    .map(|(_, n)| *n);
                Some(match region {
                    Some(region) => format!("{language} ({region})"),
                    None => language.to_owned(),
                })
            })
        }
        "region" => REGIONS
            .iter()
            .find(|(c, _)| c.eq_ignore_ascii_case(code))
            .map(|(_, n)| (*n).to_owned()),
        "script" => SCRIPTS
            .iter()
            .find(|(c, _)| c.eq_ignore_ascii_case(code))
            .map(|(_, n)| (*n).to_owned()),
        "currency" => CURRENCIES
            .iter()
            .find(|(c, ..)| c.eq_ignore_ascii_case(code))
            .map(|(_, one, _)| {
                let mut name = (*one).to_owned();
                if let Some(first) = name.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                name
            }),
        "calendar" => match code {
            "gregory" => Some("Gregorian Calendar".to_owned()),
            "iso8601" => Some("ISO-8601 Calendar".to_owned()),
            "buddhist" => Some("Buddhist Calendar".to_owned()),
            "chinese" => Some("Chinese Calendar".to_owned()),
            "islamic" => Some("Hijri Calendar".to_owned()),
            "japanese" => Some("Japanese Calendar".to_owned()),
            _ => None,
        },
        "dateTimeField" => match code {
            "era" | "year" | "quarter" | "month" | "day" | "hour" | "minute" | "second" => {
                Some(code.to_owned())
            }
            "weekOfYear" => Some("week".to_owned()),
            "weekday" => Some("day of the week".to_owned()),
            "dayPeriod" => Some("AM/PM".to_owned()),
            "timeZoneName" => Some("time zone".to_owned()),
            _ => return Err(range_error("invalid_argument")),
        },
        _ => None,
    };
    Ok(found.or_else(|| (data.fallback == "code").then(|| code.to_owned())))
}

fn make_display_names(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let tag = resolve_locale(&arg(args, 0), context)?;
    let options = arg(args, 1);
    if !options.is_object() {
        return Err(type_error(
            "Intl.DisplayNames: the options must be an object with a type",
        ));
    }
    let style = option_str(&options, "style", &["long", "short", "narrow"], context)?
        .unwrap_or_else(|| "long".into());
    let kinds = [
        "language",
        "region",
        "script",
        "currency",
        "calendar",
        "dateTimeField",
    ];
    let kind = option_str(&options, "type", &kinds, context)?
        .ok_or_else(|| type_error("Intl.DisplayNames: the type option is required"))?;
    let fallback = option_str(&options, "fallback", &["code", "none"], context)?
        .unwrap_or_else(|| "code".into());
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        DisplayData {
            locale: tag.base_name(),
            style,
            kind,
            fallback,
        },
    ))
}

const DISPLAY_METHODS: Methods = &[
    ("of", 1, |this, args, cx| {
        let data: DisplayData = data_of(this, "DisplayNames.prototype.of")?;
        let code = text_of(&arg(args, 0), cx)?;
        Ok(display_name(&data, &code)?.map_or(JsValue::undefined(), |n| js_text(&n)))
    }),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: DisplayData = data_of(this, "DisplayNames.prototype.resolvedOptions")?;
        record(
            cx,
            vec![
                ("locale", js_text(&data.locale)),
                ("style", js_text(&data.style)),
                ("type", js_text(&data.kind)),
                ("fallback", js_text(&data.fallback)),
            ],
        )
    }),
];

// ─── Locale ─────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct LocaleData {
    #[unsafe_ignore_trace]
    tag: Tag,
}

/// The script and region a language is most likely written in and spoken: what `maximize`
/// adds.
fn likely(language: &str) -> (&'static str, &'static str) {
    match language {
        "zh" => ("Hans", "CN"),
        "ja" => ("Jpan", "JP"),
        "ko" => ("Kore", "KR"),
        "ru" => ("Cyrl", "RU"),
        "uk" => ("Cyrl", "UA"),
        "bg" => ("Cyrl", "BG"),
        "sr" => ("Cyrl", "RS"),
        "mk" => ("Cyrl", "MK"),
        "kk" => ("Cyrl", "KZ"),
        "ar" => ("Arab", "EG"),
        "fa" => ("Arab", "IR"),
        "ur" => ("Arab", "PK"),
        "he" => ("Hebr", "IL"),
        "hi" | "mr" => ("Deva", "IN"),
        "ne" => ("Deva", "NP"),
        "el" => ("Grek", "GR"),
        "th" => ("Thai", "TH"),
        "de" => ("Latn", "DE"),
        "fr" => ("Latn", "FR"),
        "es" => ("Latn", "ES"),
        "it" => ("Latn", "IT"),
        "pt" => ("Latn", "BR"),
        "nl" => ("Latn", "NL"),
        "sv" => ("Latn", "SE"),
        "da" => ("Latn", "DK"),
        "nb" | "no" | "nn" => ("Latn", "NO"),
        "fi" => ("Latn", "FI"),
        "pl" => ("Latn", "PL"),
        "cs" => ("Latn", "CZ"),
        "tr" => ("Latn", "TR"),
        "vi" => ("Latn", "VN"),
        "id" => ("Latn", "ID"),
        _ => ("Latn", "US"),
    }
}

fn is_rtl(tag: &Tag) -> bool {
    ["ar", "he", "fa", "ur", "yi", "ps", "sd", "ug", "dv"].contains(&tag.language.as_str())
}

fn make_locale(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let given = arg(args, 0);
    if !given.is_string() && !given.is_object() {
        return Err(type_error(
            "First argument to Intl.Locale constructor can't be empty or missing",
        ));
    }
    let text = text_of(&given, context)?;
    let mut tag = parse_tag(&text)
        .ok_or_else(|| range_error(format!("Incorrect locale information provided: {text}")))?;
    let options = options_of(&arg(args, 1), context)?;
    if let Some(language) = option_str(&options, "language", &[], context)? {
        tag.language = language.to_ascii_lowercase();
    }
    if let Some(script) = option_str(&options, "script", &[], context)? {
        tag.script = parse_tag(&format!("und-{script}")).and_then(|t| t.script);
    }
    if let Some(region) = option_str(&options, "region", &[], context)? {
        tag.region = Some(region.to_ascii_uppercase());
    }
    for (option_name, key) in [
        ("calendar", "ca"),
        ("collation", "co"),
        ("hourCycle", "hc"),
        ("caseFirst", "kf"),
        ("numberingSystem", "nu"),
        ("numeric", "kn"),
        ("firstDayOfWeek", "fw"),
    ] {
        let value = option(&options, option_name, context)?;
        if value.is_undefined() {
            continue;
        }
        let value = if value.is_boolean() {
            value.to_boolean().to_string()
        } else {
            text_of(&value, context)?
        };
        tag.unicode.retain(|(k, _)| k != key);
        tag.unicode.push((key.to_owned(), value));
    }
    tag.unicode.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        LocaleData { tag },
    ))
}

fn locale_tag(this: &JsValue) -> JsResult<Tag> {
    Ok(data_of::<LocaleData>(this, "Locale")?.tag.clone())
}

fn new_locale(this: &JsValue, tag: Tag) -> JsResult<JsValue> {
    let prototype = this
        .as_object()
        .and_then(|o| o.prototype())
        .ok_or_else(|| type_error("not an Intl.Locale"))?;
    Ok(JsObject::from_proto_and_data(prototype, LocaleData { tag }).into())
}

/// `weekInfo`: the week as the region counts it — Monday first, except where Sunday is.
fn week_info(tag: &Tag, context: &mut Context) -> JsResult<JsValue> {
    let sunday_first = [
        "US", "CA", "JP", "BR", "MX", "IL", "IN", "PH", "KR", "TW", "HK", "ZA", "SA", "AU",
    ];
    let region = tag
        .region
        .clone()
        .unwrap_or_else(|| likely(&tag.language).1.to_owned());
    let first = if sunday_first.contains(&region.as_str()) {
        7
    } else {
        1
    };
    let weekend = array(context, vec![6.into(), 7.into()]);
    record(
        context,
        vec![
            ("firstDay", first.into()),
            ("weekend", weekend),
            ("minimalDays", 1.into()),
        ],
    )
}

fn text_info(tag: &Tag, context: &mut Context) -> JsResult<JsValue> {
    record(
        context,
        vec![(
            "direction",
            js_text(if is_rtl(tag) { "rtl" } else { "ltr" }),
        )],
    )
}

const LOCALE_METHODS: Methods = &[
    ("toString", 0, |this, _, _| {
        Ok(js_text(&locale_tag(this)?.to_string()))
    }),
    ("toJSON", 0, |this, _, _| {
        Ok(js_text(&locale_tag(this)?.to_string()))
    }),
    ("maximize", 0, |this, _, _| {
        let mut tag = locale_tag(this)?;
        let (script, region) = likely(&tag.language);
        tag.script.get_or_insert_with(|| script.to_owned());
        tag.region.get_or_insert_with(|| region.to_owned());
        new_locale(this, tag)
    }),
    ("minimize", 0, |this, _, _| {
        let mut tag = locale_tag(this)?;
        let (script, region) = likely(&tag.language);
        if tag.script.as_deref() == Some(script) {
            tag.script = None;
        }
        if tag.region.as_deref() == Some(region) {
            tag.region = None;
        }
        new_locale(this, tag)
    }),
    ("getWeekInfo", 0, |this, _, cx| {
        week_info(&locale_tag(this)?, cx)
    }),
    ("getTextInfo", 0, |this, _, cx| {
        text_info(&locale_tag(this)?, cx)
    }),
    ("getCalendars", 0, |_, _, cx| {
        Ok(array(cx, vec![js_text("gregory")]))
    }),
    ("getCollations", 0, |_, _, cx| {
        Ok(array(cx, vec![js_text("default")]))
    }),
    ("getNumberingSystems", 0, |_, _, cx| {
        Ok(array(cx, vec![js_text("latn")]))
    }),
    ("getHourCycles", 0, |this, _, cx| {
        let tag = locale_tag(this)?;
        let twelve = ["US", "CA", "AU", "IN", "PH", "EG", "NZ"]
            .contains(&tag.region.as_deref().unwrap_or(likely(&tag.language).1));
        Ok(array(cx, vec![js_text(if twelve { "h12" } else { "h23" })]))
    }),
];

fn locale_keyword(this: &JsValue, key: &str) -> JsResult<JsValue> {
    let tag = locale_tag(this)?;
    Ok(tag.keyword(key).map_or(JsValue::undefined(), js_text))
}

fn some_text(text: Option<String>) -> JsValue {
    text.map_or(JsValue::undefined(), |s| js_text(&s))
}

const LOCALE_ACCESSORS: Accessors = &[
    (
        "baseName",
        |this, _, _| Ok(js_text(&locale_tag(this)?.base_name())),
        None,
    ),
    (
        "language",
        |this, _, _| Ok(js_text(&locale_tag(this)?.language)),
        None,
    ),
    (
        "script",
        |this, _, _| Ok(some_text(locale_tag(this)?.script)),
        None,
    ),
    (
        "region",
        |this, _, _| Ok(some_text(locale_tag(this)?.region)),
        None,
    ),
    (
        "variants",
        |this, _, _| {
            let tag = locale_tag(this)?;
            Ok(some_text(
                (!tag.variants.is_empty()).then(|| tag.variants.join("-")),
            ))
        },
        None,
    ),
    ("calendar", |this, _, _| locale_keyword(this, "ca"), None),
    ("collation", |this, _, _| locale_keyword(this, "co"), None),
    ("hourCycle", |this, _, _| locale_keyword(this, "hc"), None),
    ("caseFirst", |this, _, _| locale_keyword(this, "kf"), None),
    (
        "numberingSystem",
        |this, _, _| locale_keyword(this, "nu"),
        None,
    ),
    (
        "firstDayOfWeek",
        |this, _, _| locale_keyword(this, "fw"),
        None,
    ),
    (
        "numeric",
        |this, _, _| {
            Ok(locale_tag(this)?
                .keyword("kn")
                .is_some_and(|v| v.is_empty() || v == "true")
                .into())
        },
        None,
    ),
    (
        "weekInfo",
        |this, _, cx| week_info(&locale_tag(this)?, cx),
        None,
    ),
    (
        "textInfo",
        |this, _, cx| text_info(&locale_tag(this)?, cx),
        None,
    ),
];

// ─── Segmenter ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Trace, Finalize, JsData)]
struct SegmenterData {
    locale: String,
    granularity: String,
}

/// One segment: where it starts and ends in UTF-16 units — JavaScript's indices — its text,
/// and, cut by word, whether it is a word rather than space or punctuation.
struct Piece {
    start: usize,
    end: usize,
    text: String,
    word_like: bool,
}

fn segment_text(text: &str, granularity: &str) -> Vec<Piece> {
    use unicode_segmentation::UnicodeSegmentation;
    let pieces: Vec<&str> = match granularity {
        "word" => text.split_word_bounds().collect(),
        "sentence" => text.split_sentence_bounds().collect(),
        _ => text.graphemes(true).collect(),
    };
    let mut out = Vec::with_capacity(pieces.len());
    let mut units = 0;
    for piece in pieces {
        let length: usize = piece.chars().map(char::len_utf16).sum();
        out.push(Piece {
            start: units,
            end: units + length,
            text: piece.to_owned(),
            word_like: piece.chars().any(char::is_alphanumeric),
        });
        units += length;
    }
    out
}

fn segment_object(
    piece: &Piece,
    input: &JsValue,
    word: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let out = object(context);
    out.set(js_string!("segment"), js_text(&piece.text), false, context)?;
    out.set(js_string!("index"), piece.start, false, context)?;
    out.set(js_string!("input"), input.clone(), false, context)?;
    if word {
        out.set(js_string!("isWordLike"), piece.word_like, false, context)?;
    }
    Ok(out.into())
}

fn make_segmenter(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let tag = resolve_locale(&arg(args, 0), context)?;
    let options = options_of(&arg(args, 1), context)?;
    let granularity = option_str(
        &options,
        "granularity",
        &["grapheme", "word", "sentence"],
        context,
    )?
    .unwrap_or_else(|| "grapheme".into());
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        SegmenterData {
            locale: tag.base_name(),
            granularity,
        },
    ))
}

/// `segmenter.segment(text)`: the segments, iterable, with `containing(index)`.
fn segment(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let data: SegmenterData = data_of(this, "Segmenter.prototype.segment")?;
    let input = js_text(&text_of(&arg(args, 0), context)?);
    let text = input.as_string().expect("a string").to_std_string_lossy();
    let word = data.granularity == "word";
    let pieces = segment_text(&text, &data.granularity);
    let mut items = Vec::with_capacity(pieces.len());
    let mut ends = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        items.push(segment_object(piece, &input, word, context)?);
        ends.push(JsValue::from(piece.end));
    }
    let list = array(context, items);
    let ends = array(context, ends);
    let segments = object(context);
    hidden(&segments, js_string!("__segments"), list, context)?;
    hidden(&segments, js_string!("__ends"), ends, context)?;
    crate::native::methods(
        context,
        &segments,
        &[("containing", 1, |this, args, cx| {
            let index = arg(args, 0);
            let index = if index.is_undefined() {
                0.
            } else {
                index.to_number(cx)?
            };
            if index < 0. || index.is_nan() {
                return Ok(JsValue::undefined());
            }
            let list = list_of(&get(this, "__segments", cx)?, cx)?;
            let ends = list_of(&get(this, "__ends", cx)?, cx)?;
            for (item, end) in list.iter().zip(ends) {
                if index < end.to_number(cx)? {
                    return Ok(item.clone());
                }
            }
            Ok(JsValue::undefined())
        })],
    )?;
    let iterate = crate::native::function(context, "[Symbol.iterator]", 0, |this, _, cx| {
        let list = get(this, "__segments", cx)?;
        crate::native::invoke(&list, "values", &[], cx)
    });
    segments.define_property_or_throw(
        JsSymbol::iterator(),
        PropertyDescriptor::builder()
            .value(iterate)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    Ok(segments.into())
}

const SEGMENTER_METHODS: Methods = &[
    ("segment", 1, segment),
    ("resolvedOptions", 0, |this, _, cx| {
        let data: SegmenterData = data_of(this, "Segmenter.prototype.resolvedOptions")?;
        record(
            cx,
            vec![
                ("locale", js_text(&data.locale)),
                ("granularity", js_text(&data.granularity)),
            ],
        )
    }),
];

// ─── The routes from Date, Number, BigInt and String ────────────────────────────────────────

/// `Date.prototype.toLocaleString` and its kin, `Number.prototype.toLocaleString` (and
/// `BigInt`'s) and `String.prototype.localeCompare`, made what the formatters here answer.
fn route_prototypes(context: &mut Context) -> JsResult<()> {
    let date_proto = context.intrinsics().constructors().date().prototype();
    let date_methods: Methods = &[
        ("toLocaleString", 0, |this, args, cx| {
            date::to_locale(this, args, date::Defaults::All, cx)
        }),
        ("toLocaleDateString", 0, |this, args, cx| {
            date::to_locale(this, args, date::Defaults::Date, cx)
        }),
        ("toLocaleTimeString", 0, |this, args, cx| {
            date::to_locale(this, args, date::Defaults::Time, cx)
        }),
    ];
    crate::native::methods(context, &date_proto, date_methods)?;
    let number_proto = context.intrinsics().constructors().number().prototype();
    crate::native::methods(
        context,
        &number_proto,
        &[("toLocaleString", 0, number::to_locale)],
    )?;
    let bigint_proto = context.intrinsics().constructors().bigint().prototype();
    crate::native::methods(
        context,
        &bigint_proto,
        &[("toLocaleString", 0, number::to_locale)],
    )?;
    let string_proto = context.intrinsics().constructors().string().prototype();
    crate::native::methods(
        context,
        &string_proto,
        &[("localeCompare", 1, |this, args, cx| {
            if this.is_null_or_undefined() {
                return Err(type_error(
                    "String.prototype.localeCompare called on null or undefined",
                ));
            }
            let a = text_of(this, cx)?;
            let b = text_of(&arg(args, 0), cx)?;
            let collator = make_collator(&object(cx), &[arg(args, 1), arg(args, 2)], cx)?;
            collator_compare(
                &JsValue::undefined(),
                &[js_text(&a), js_text(&b)],
                &collator,
                cx,
            )
        })],
    )?;
    Ok(())
}

// ─── Installing it ──────────────────────────────────────────────────────────────────────────

pub fn install(context: &mut Context) -> JsResult<()> {
    let intl = object(context);
    let classes: [(&str, usize, Make, Methods, Accessors); 9] = [
        (
            "Collator",
            0,
            make_collator,
            COLLATOR_METHODS,
            COLLATOR_ACCESSORS,
        ),
        (
            "NumberFormat",
            0,
            number::make,
            number::METHODS,
            number::ACCESSORS,
        ),
        (
            "DateTimeFormat",
            0,
            date::make,
            date::METHODS,
            date::ACCESSORS,
        ),
        ("PluralRules", 0, make_plural_rules, PLURAL_METHODS, &[]),
        (
            "RelativeTimeFormat",
            0,
            make_relative,
            RELATIVE_METHODS,
            &[],
        ),
        ("ListFormat", 0, make_list_format, LIST_METHODS, &[]),
        ("DisplayNames", 2, make_display_names, DISPLAY_METHODS, &[]),
        ("Locale", 1, make_locale, LOCALE_METHODS, LOCALE_ACCESSORS),
        ("Segmenter", 0, make_segmenter, SEGMENTER_METHODS, &[]),
    ];
    for (name, length, make, methods, accessors) in classes {
        let class = intl_class(context, name, length, make, methods, accessors)?;
        hidden(&intl, JsString::from(name), class, context)?;
    }
    crate::native::methods(
        context,
        &intl,
        &[
            ("getCanonicalLocales", 1, |_, args, cx| {
                let tags = requested_locales(&arg(args, 0), cx)?;
                let items: Vec<JsValue> = tags.iter().map(|t| js_text(&t.to_string())).collect();
                Ok(array(cx, items))
            }),
            ("supportedValuesOf", 1, |_, args, cx| {
                let key = text_of(&arg(args, 0), cx)?;
                let values: Vec<&str> = match key.as_str() {
                    "calendar" => vec!["gregory", "iso8601"],
                    "collation" => vec!["default"],
                    "currency" => {
                        let mut codes: Vec<&str> = CURRENCIES.iter().map(|(c, ..)| *c).collect();
                        codes.sort_unstable();
                        codes
                    }
                    "numberingSystem" => vec!["latn"],
                    "timeZone" => date::zone_names(),
                    "unit" => number::unit_names(),
                    _ => return Err(range_error(format!("Invalid key : {key}"))),
                };
                let items: Vec<JsValue> = values.iter().map(|v| js_text(v)).collect();
                Ok(array(cx, items))
            }),
        ],
    )?;
    intl.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(js_text("Intl"))
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    route_prototypes(context)?;
    context.register_global_property(
        js_string!("Intl"),
        intl,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collate(a: &str, b: &str, numeric: bool, sensitivity: &str) -> Ordering {
        collate_with(a, b, numeric, sensitivity, false, "false")
    }

    #[test]
    fn text_is_sorted_as_people_read_it() {
        assert_eq!(
            collate("item 10", "item 9", true, "base"),
            Ordering::Greater
        );
        assert_eq!(collate("item 10", "item 9", false, "base"), Ordering::Less);
        assert_eq!(collate("apple", "Banana", false, "variant"), Ordering::Less);
        assert_eq!(collate("a", "A", false, "base"), Ordering::Equal);
        assert_eq!(collate("a", "A", false, "variant"), Ordering::Less);
        assert_eq!(collate("résumé", "resume", false, "base"), Ordering::Equal);
        assert_eq!(
            collate("résumé", "resume", false, "accent"),
            Ordering::Greater
        );
        assert_eq!(collate("éclair", "fig", false, "variant"), Ordering::Less);
    }

    #[test]
    fn tags_are_read_and_made_canonical() {
        assert_eq!(parse_tag("EN_us").unwrap().to_string(), "en-US");
        assert_eq!(parse_tag("zh-hant-tw").unwrap().to_string(), "zh-Hant-TW");
        let tag = parse_tag("en-US-u-hc-h23-ca-gregory").unwrap();
        assert_eq!(tag.keyword("hc"), Some("h23"));
        assert_eq!(tag.base_name(), "en-US");
        assert!(parse_tag("1x").is_none());
        assert!(parse_tag("").is_none());
        assert_eq!(parse_tag("iw").unwrap().language, "he");
    }

    #[test]
    fn segments_count_in_utf16_units() {
        let pieces = segment_text("a👍🏽b", "grapheme");
        let spans: Vec<(usize, usize)> = pieces.iter().map(|p| (p.start, p.end)).collect();
        assert_eq!(spans, [(0, 1), (1, 5), (5, 6)]);
        let words = segment_text("Hi, you.", "word");
        let like: Vec<bool> = words.iter().map(|p| p.word_like).collect();
        assert_eq!(like, [true, false, false, true, false]);
    }
}
