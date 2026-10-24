//! Node's `util` (and `util/types`, and `sys`, its old name): `inspect` and `format` as Node
//! writes values, deep equality, `promisify` and `callbackify`, `inherits`, `deprecate`,
//! `debuglog`, and the type checks.
//!
//! `inspect` follows Node's own algorithm (`lib/internal/util/inspect.js`) closely, since what
//! it writes is read by people and, now and then, compared by code: strings in single quotes,
//! `{ a: 1 }`, `[ 1, 2, 3 ]`, `Map(1) { 'a' => 1 }`, `[Function: f]`, `<ref *1>` and
//! `[Circular *1]`, objects deeper than `depth` as `[Object]`, entries on one line while they fit
//! in `breakLength` and long arrays grouped in columns. A value's own
//! `[util.inspect.custom](depth, options, inspect)` is asked first, as Node asks it.
//!
//! The console formats what it is given the same way (`util.formatWithOptions`), so
//! `console.log('%s items', 3, { a: 1 })` reads as it does in Node and in Electron.

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue,
    builtins::promise::PromiseState,
    js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{
        JsArray, JsArrayBuffer, JsDataView, JsDate, JsGenerator, JsMap, JsMapIterator, JsPromise,
        JsProxy, JsRegExp, JsSet, JsSetIterator, JsSharedArrayBuffer, JsTypedArray, JsWeakMap,
        JsWeakSet,
    },
    property::PropertyKey,
};

use crate::native::{
    arg, array, closure, coded_error, exports, function, get, invoke, js_text, object, text_of,
    type_error,
};

// ─── Reaching the engine's own functions ─────────────────────────────────────────────────────

/// `Object[name]`: `keys`, `getOwnPropertyDescriptor`, `is`…
fn object_fn(context: &mut Context, name: &str) -> JsResult<JsValue> {
    context
        .intrinsics()
        .constructors()
        .object()
        .constructor()
        .get(JsString::from(name), context)
}

fn call_object_fn(context: &mut Context, name: &str, args: &[JsValue]) -> JsResult<JsValue> {
    let f = object_fn(context, name)?;
    crate::call(context, &f, &JsValue::undefined(), args)
}

/// `Object.prototype.toString.call(value)`: `[object Map]`.
fn object_tag(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let prototype = context.intrinsics().constructors().object().prototype();
    let to_string = prototype.get(js_string!("toString"), context)?;
    Ok(crate::print(&crate::call(context, &to_string, value, &[])?))
}

/// `Object.is(a, b)`: SameValue.
fn same_value(a: &JsValue, b: &JsValue) -> bool {
    JsValue::same_value(a, b)
}

/// The items `Array.from` makes of `value`: a map's entries, a set's values.
fn items(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let all = crate::call(
        context,
        &from,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    crate::native::list_of(&all, context)
}

/// A property as `Object.getOwnPropertyDescriptor` describes it.
struct Descriptor {
    value: Option<JsValue>,
    get: Option<JsValue>,
    set: Option<JsValue>,
    enumerable: bool,
}

fn descriptor(
    target: &JsObject,
    key: &PropertyKey,
    context: &mut Context,
) -> JsResult<Option<Descriptor>> {
    let described = call_object_fn(
        context,
        "getOwnPropertyDescriptor",
        &[target.clone().into(), key.into()],
    )?;
    let Some(described) = described.as_object() else {
        return Ok(None);
    };
    let field = |name: &str, context: &mut Context| -> JsResult<Option<JsValue>> {
        if described.has_own_property(JsString::from(name), context)? {
            Ok(Some(described.get(JsString::from(name), context)?))
        } else {
            Ok(None)
        }
    };
    let value = field("value", context)?;
    let get = field("get", context)?.filter(|g| !g.is_undefined());
    let set = field("set", context)?.filter(|s| !s.is_undefined());
    let enumerable = described
        .get(js_string!("enumerable"), context)?
        .to_boolean();
    Ok(Some(Descriptor {
        value,
        get,
        set,
        enumerable,
    }))
}

fn has_own(
    target: &JsObject,
    key: impl Into<PropertyKey>,
    context: &mut Context,
) -> JsResult<bool> {
    target.has_own_property(key, context)
}

/// Whether `key` is an array index: `"0"`, `"12"`, not `"01"` or `"-1"`.
fn is_index(key: &PropertyKey) -> bool {
    match key {
        PropertyKey::Index(_) => true,
        PropertyKey::String(text) => {
            let text = text.to_std_string_escaped();
            !text.is_empty()
                && (text == "0" || !text.starts_with('0'))
                && text.bytes().all(|b| b.is_ascii_digit())
                && text.parse::<u64>().is_ok_and(|n| n < u32::MAX as u64)
        }
        PropertyKey::Symbol(_) => false,
    }
}

/// An object's own keys, as `inspect` lists them: enumerable strings and symbols, or every one
/// with `showHidden`.
fn own_keys(target: &JsObject, hidden: bool, context: &mut Context) -> JsResult<Vec<PropertyKey>> {
    let names = call_object_fn(
        context,
        if hidden {
            "getOwnPropertyNames"
        } else {
            "keys"
        },
        &[target.clone().into()],
    )?;
    let mut keys = Vec::new();
    for name in crate::native::list_of(&names, context)? {
        keys.push(name.to_property_key(context)?);
    }
    let symbols = call_object_fn(context, "getOwnPropertySymbols", &[target.clone().into()])?;
    for symbol in crate::native::list_of(&symbols, context)? {
        let key = symbol.to_property_key(context)?;
        if hidden || descriptor(target, &key, context)?.is_some_and(|d| d.enumerable) {
            keys.push(key);
        }
    }
    Ok(keys)
}

// ─── What a value is ─────────────────────────────────────────────────────────────────────────

fn is_error(target: &JsObject, context: &mut Context) -> bool {
    if target.is::<boa_engine::builtins::error::Error>() {
        return true;
    }
    let prototype = context.intrinsics().constructors().error().prototype();
    target
        .prototype()
        .is_some_and(|p| crate::native::inherits(&p, &prototype))
}

/// A boxed primitive's kind and value: `new Number(3)` is `("Number", 3)`.
fn boxed(target: &JsObject) -> Option<(&'static str, JsValue)> {
    if let Some(n) = target.downcast_ref::<f64>() {
        return Some(("Number", JsValue::from(*n)));
    }
    if let Some(b) = target.downcast_ref::<bool>() {
        return Some(("Boolean", JsValue::from(*b)));
    }
    if let Some(s) = target.downcast_ref::<JsString>() {
        return Some(("String", JsValue::from(s.clone())));
    }
    if let Some(s) = target.downcast_ref::<JsSymbol>() {
        return Some(("Symbol", JsValue::from(s.clone())));
    }
    if let Some(n) = target.downcast_ref::<boa_engine::JsBigInt>() {
        return Some(("BigInt", JsValue::from(n.clone())));
    }
    None
}

/// The kind of function: `"AsyncGeneratorFunction"`, `"AsyncFunction"`, `"GeneratorFunction"`
/// or `"Function"`, read from the `Symbol.toStringTag` of what it inherits.
fn function_kind(target: &JsObject, context: &mut Context) -> JsResult<String> {
    let tag = match target.prototype() {
        Some(prototype) => prototype.get(JsSymbol::to_string_tag(), context)?,
        None => JsValue::undefined(),
    };
    Ok(
        match tag
            .as_string()
            .map(|t| t.to_std_string_escaped())
            .as_deref()
        {
            Some(kind @ ("AsyncFunction" | "GeneratorFunction" | "AsyncGeneratorFunction")) => {
                kind.to_owned()
            }
            _ => "Function".to_owned(),
        },
    )
}

/// Whether a function is a class: one written in JavaScript whose `prototype` cannot be
/// written, which only a `class` has.
fn is_class(target: &JsObject, context: &mut Context) -> JsResult<bool> {
    if !target.is::<boa_engine::builtins::function::OrdinaryFunction>() {
        return Ok(false);
    }
    let key: PropertyKey = js_string!("prototype").into();
    Ok(descriptor(target, &key, context)?.is_some_and(|d| {
        d.value.is_some() && d.get.is_none() && !writable(target, &key, context).unwrap_or(true)
    }))
}

fn writable(target: &JsObject, key: &PropertyKey, context: &mut Context) -> JsResult<bool> {
    let described = call_object_fn(
        context,
        "getOwnPropertyDescriptor",
        &[target.clone().into(), key.into()],
    )?;
    Ok(get(&described, "writable", context)?.to_boolean())
}

fn function_name(target: &JsObject, context: &mut Context) -> JsResult<String> {
    let name = target.get(js_string!("name"), context)?;
    Ok(if name.is_string() {
        crate::print(&name)
    } else {
        String::new()
    })
}

/// The name of the class `target` is an instance of: the first `constructor` up its prototype
/// chain that is a named function it is an instance of. `None` for a null-prototype object.
fn constructor_name(target: &JsObject, context: &mut Context) -> JsResult<Option<String>> {
    let mut at = Some(target.clone());
    let mut first = true;
    let mut depth = 0;
    let key: PropertyKey = js_string!("constructor").into();
    while let Some(object) = at {
        depth += 1;
        if depth > 1000 {
            break;
        }
        if let Some(found) = descriptor(&object, &key, context)?
            && let Some(constructor) = found.value.and_then(|v| v.as_object())
            && constructor.is_callable()
        {
            let name = function_name(&constructor, context)?;
            if !name.is_empty()
                && JsValue::from(target.clone())
                    .instance_of(&constructor.into(), context)
                    .unwrap_or(false)
            {
                return Ok(Some(name));
            }
        }
        at = object.prototype();
        if first && at.is_none() {
            return Ok(None);
        }
        first = false;
    }
    Ok(Some("Object".to_owned()))
}

// ─── inspect ─────────────────────────────────────────────────────────────────────────────────

/// How `compact` was given: `true`, or a number (`false` is 0).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Compact {
    True,
    Level(u32),
}

/// `inspect`'s options, as Node names them.
#[derive(Clone)]
pub(crate) struct Options {
    pub show_hidden: bool,
    /// How deep to go; infinite for `depth: null` or `Infinity`.
    pub depth: f64,
    pub colors: bool,
    pub custom_inspect: bool,
    pub max_array_length: f64,
    pub max_string_length: f64,
    pub break_length: f64,
    pub compact: Compact,
    pub sorted: bool,
    pub getters: bool,
    pub numeric_separator: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            show_hidden: false,
            depth: 2.,
            colors: false,
            custom_inspect: true,
            max_array_length: 100.,
            max_string_length: 10000.,
            break_length: 128.,
            compact: Compact::Level(3),
            sorted: false,
            getters: false,
            numeric_separator: false,
        }
    }
}

impl Options {
    /// The options `given` sets, over these.
    pub(crate) fn with(mut self, given: &JsValue, context: &mut Context) -> JsResult<Self> {
        let Some(given) = given.as_object() else {
            return Ok(self);
        };
        let read = |name: &str, context: &mut Context| -> JsResult<Option<JsValue>> {
            let value = given.get(JsString::from(name), context)?;
            Ok((!value.is_undefined()).then_some(value))
        };
        let length = |value: JsValue, context: &mut Context| -> JsResult<f64> {
            Ok(if value.is_null() {
                f64::INFINITY
            } else {
                value.to_number(context)?.max(0.)
            })
        };
        if let Some(v) = read("showHidden", context)? {
            self.show_hidden = v.to_boolean();
        }
        if let Some(v) = read("depth", context)? {
            self.depth = length(v, context)?;
        }
        if let Some(v) = read("colors", context)? {
            self.colors = v.to_boolean();
        }
        if let Some(v) = read("customInspect", context)? {
            self.custom_inspect = v.to_boolean();
        }
        if let Some(v) = read("maxArrayLength", context)? {
            self.max_array_length = length(v, context)?;
        }
        if let Some(v) = read("maxStringLength", context)? {
            self.max_string_length = length(v, context)?;
        }
        if let Some(v) = read("breakLength", context)? {
            self.break_length = v.to_number(context)?;
        }
        if let Some(v) = read("compact", context)? {
            self.compact = match v.as_boolean() {
                Some(true) => Compact::True,
                Some(false) => Compact::Level(0),
                None => Compact::Level(v.to_number(context)?.max(0.) as u32),
            };
        }
        if let Some(v) = read("sorted", context)? {
            self.sorted = v.to_boolean();
        }
        if let Some(v) = read("getters", context)? {
            self.getters = v.to_boolean();
        }
        if let Some(v) = read("numericSeparator", context)? {
            self.numeric_separator = v.to_boolean();
        }
        Ok(self)
    }

    /// These options as an object, as a custom inspect function is handed them.
    fn to_object(&self, context: &mut Context) -> JsResult<JsObject> {
        let out = object(context);
        let depth = if self.depth.is_infinite() {
            JsValue::null()
        } else {
            self.depth.into()
        };
        let compact = match self.compact {
            Compact::True => JsValue::from(true),
            Compact::Level(0) => false.into(),
            Compact::Level(n) => n.into(),
        };
        let infinite = |n: f64| {
            if n.is_infinite() {
                JsValue::from(f64::INFINITY)
            } else {
                n.into()
            }
        };
        let fields: [(&str, JsValue); 11] = [
            ("showHidden", self.show_hidden.into()),
            ("depth", depth),
            ("colors", self.colors.into()),
            ("customInspect", self.custom_inspect.into()),
            ("showProxy", false.into()),
            ("maxArrayLength", infinite(self.max_array_length)),
            ("maxStringLength", infinite(self.max_string_length)),
            ("breakLength", self.break_length.into()),
            ("compact", compact),
            ("sorted", self.sorted.into()),
            ("getters", self.getters.into()),
        ];
        for (key, value) in fields {
            out.set(JsString::from(key), value, false, context)?;
        }
        out.set(
            js_string!("numericSeparator"),
            self.numeric_separator,
            false,
            context,
        )?;
        let stylize: NativeFunctionPointer = if self.colors {
            |_, args, cx| {
                let text = text_of(&arg(args, 0), cx)?;
                let style = text_of(&arg(args, 1), cx)?;
                Ok(js_text(&paint(&text, &style)))
            }
        } else {
            |_, args, cx| Ok(js_text(&text_of(&arg(args, 0), cx)?))
        };
        let stylize = function(context, "stylize", 2, stylize);
        out.set(js_string!("stylize"), stylize, false, context)?;
        Ok(out)
    }
}

/// The ANSI colours Node gives each kind of value when `colors` is on.
fn style_colour(style: &str) -> Option<(u8, u8)> {
    Some(match style {
        "special" => (36, 39),
        "number" | "bigint" | "boolean" => (33, 39),
        "undefined" => (90, 39),
        "null" => (1, 22),
        "string" | "symbol" => (32, 39),
        "date" => (35, 39),
        "regexp" => (31, 39),
        "module" => (4, 24),
        _ => return None,
    })
}

fn paint(text: &str, style: &str) -> String {
    match style_colour(style) {
        Some((open, close)) => format!("\u{1b}[{open}m{text}\u{1b}[{close}m"),
        None => text.to_owned(),
    }
}

/// How long `text` is as JavaScript counts it, colours left out.
fn width(text: &str) -> usize {
    let mut length = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        length += c.len_utf16();
    }
    length
}

/// What `inspect` keeps while it walks a value.
struct Walk {
    options: Options,
    indentation: usize,
    current_depth: usize,
    seen: Vec<JsObject>,
    circular: Vec<JsObject>,
}

impl Walk {
    fn stylize(&self, text: &str, style: &str) -> String {
        if self.options.colors {
            paint(text, style)
        } else {
            text.to_owned()
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Extras {
    Object,
    Array,
}

/// `value` as `util.inspect` writes it with `options`.
pub(crate) fn inspect(
    value: &JsValue,
    options: &Options,
    context: &mut Context,
) -> JsResult<String> {
    let mut walk = Walk {
        options: options.clone(),
        indentation: 0,
        current_depth: 0,
        seen: Vec::new(),
        circular: Vec::new(),
    };
    format_value(&mut walk, value, 0, context)
}

/// A string in quotes, as Node quotes it: single quotes, unless the string has one and no
/// double quote (then double), or has both and no backtick (then backticks); control
/// characters escaped.
pub(crate) fn quote(text: &str) -> String {
    let quote = if !text.contains('\'') {
        '\''
    } else if !text.contains('"') {
        '"'
    } else if !text.contains('`') && !text.contains("${") {
        '`'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// A number as Node writes it: `-0` kept, and with `numericSeparator` `1_000_000`.
fn number_text(n: f64, separator: bool) -> String {
    if n == 0. && n.is_sign_negative() {
        return "-0".into();
    }
    let text = crate::print(&JsValue::from(n));
    if !separator || !n.is_finite() || text.contains('e') {
        return text;
    }
    let (int, frac) = match text.split_once('.') {
        Some((i, f)) => (i.to_owned(), Some(f.to_owned())),
        None => (text.clone(), None),
    };
    let (sign, digits) = int
        .strip_prefix('-')
        .map_or(("", int.as_str()), |d| ("-", d));
    let mut grouped = String::new();
    for (at, c) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            grouped.push('_');
        }
        grouped.push(c);
    }
    match frac {
        Some(frac) => {
            let mut f = String::new();
            for (at, c) in frac.chars().enumerate() {
                if at > 0 && at % 3 == 0 {
                    f.push('_');
                }
                f.push(c);
            }
            format!("{sign}{grouped}.{f}")
        }
        None => format!("{sign}{grouped}"),
    }
}

fn format_primitive(walk: &Walk, value: &JsValue, context: &mut Context) -> JsResult<String> {
    if let Some(text) = value.as_string() {
        let mut text = text.to_std_string_escaped();
        let mut trailer = String::new();
        let units = text.encode_utf16().count();
        if (units as f64) > walk.options.max_string_length {
            let keep = walk.options.max_string_length as usize;
            let remaining = units - keep;
            let kept: Vec<u16> = text.encode_utf16().take(keep).collect();
            text = String::from_utf16_lossy(&kept);
            trailer = format!(
                "... {remaining} more character{}",
                if remaining > 1 { "s" } else { "" }
            );
        }
        let units = text.encode_utf16().count() as f64;
        if walk.options.compact != Compact::True
            && units > 16.
            && units > walk.options.break_length - walk.indentation as f64 - 4.
        {
            // Long text, a line at a time, joined with `+`.
            let mut lines = Vec::new();
            let mut start = 0;
            for (at, c) in text.char_indices() {
                if c == '\n' {
                    lines.push(&text[start..=at]);
                    start = at + 1;
                }
            }
            if start < text.len() {
                lines.push(&text[start..]);
            }
            let joiner = format!(" +\n{}", " ".repeat(walk.indentation + 2));
            let parts: Vec<String> = lines
                .iter()
                .map(|l| walk.stylize(&quote(l), "string"))
                .collect();
            return Ok(parts.join(&joiner) + &trailer);
        }
        return Ok(walk.stylize(&quote(&text), "string") + &trailer);
    }
    if let Some(n) = value.as_number() {
        return Ok(walk.stylize(&number_text(n, walk.options.numeric_separator), "number"));
    }
    if let Some(n) = value.as_bigint() {
        return Ok(walk.stylize(&format!("{}n", n.to_string_radix(10)), "bigint"));
    }
    if let Some(b) = value.as_boolean() {
        return Ok(walk.stylize(if b { "true" } else { "false" }, "boolean"));
    }
    if value.is_undefined() {
        return Ok(walk.stylize("undefined", "undefined"));
    }
    if value.is_null() {
        return Ok(walk.stylize("null", "null"));
    }
    if let Some(symbol) = value.as_symbol() {
        return Ok(walk.stylize(
            &symbol.descriptive_string().to_std_string_escaped(),
            "symbol",
        ));
    }
    text_of(value, context)
}

/// The symbol a value's own inspect function is kept under: `util.inspect.custom`.
fn custom_symbol(context: &mut Context) -> JsResult<JsValue> {
    symbol_for(context, "nodejs.util.inspect.custom")
}

fn symbol_for(context: &mut Context, key: &str) -> JsResult<JsValue> {
    let symbol = context.intrinsics().constructors().symbol().constructor();
    let for_fn = symbol.get(js_string!("for"), context)?;
    crate::call(context, &for_fn, &JsValue::undefined(), &[js_text(key)])
}

fn format_value(
    walk: &mut Walk,
    value: &JsValue,
    recurse: usize,
    context: &mut Context,
) -> JsResult<String> {
    let Some(target) = value.as_object() else {
        return format_primitive(walk, value, context);
    };
    if walk.options.custom_inspect {
        let symbol = custom_symbol(context)?;
        let custom = target.get(symbol.to_property_key(context)?, context)?;
        let inspect_fn = util_inspect_function(context)?;
        if custom.is_callable() && custom != inspect_fn && !is_own_prototype(&target, context)? {
            let depth = walk.options.depth - recurse as f64;
            let options = walk.options.to_object(context)?;
            options.set(
                js_string!("depth"),
                if depth.is_infinite() {
                    JsValue::null()
                } else {
                    depth.into()
                },
                false,
                context,
            )?;
            let returned = crate::call(
                context,
                &custom,
                value,
                &[depth.into(), options.into(), inspect_fn],
            )?;
            if returned != *value {
                if !returned.is_string() {
                    return format_value(walk, &returned, recurse, context);
                }
                let text = crate::print(&returned);
                return Ok(text.replace('\n', &format!("\n{}", " ".repeat(walk.indentation))));
            }
        }
    }
    if walk.seen.iter().any(|seen| JsObject::equals(seen, &target)) {
        let index = match walk
            .circular
            .iter()
            .position(|c| JsObject::equals(c, &target))
        {
            Some(at) => at + 1,
            None => {
                walk.circular.push(target.clone());
                walk.circular.len()
            }
        };
        return Ok(walk.stylize(&format!("[Circular *{index}]"), "special"));
    }
    format_raw(walk, &target, recurse, context)
}

/// Whether `target` is its own constructor's `prototype` — `Foo.prototype` itself, whose
/// inspect function is not asked about itself.
fn is_own_prototype(target: &JsObject, context: &mut Context) -> JsResult<bool> {
    let constructor = target.get(js_string!("constructor"), context)?;
    let Some(constructor) = constructor.as_object() else {
        return Ok(false);
    };
    let prototype = constructor.get(js_string!("prototype"), context)?;
    Ok(prototype
        .as_object()
        .is_some_and(|p| JsObject::equals(&p, target)))
}

fn prefix(constructor: &Option<String>, tag: &str, fallback: &str, size: &str) -> String {
    match constructor {
        None if !tag.is_empty() && fallback != tag => {
            format!("[{fallback}{size}: null prototype] [{tag}] ")
        }
        None => format!("[{fallback}{size}: null prototype] "),
        Some(name) if !tag.is_empty() && name != tag => format!("{name}{size} [{tag}] "),
        Some(name) => format!("{name}{size} "),
    }
}

/// What makes the entries of a value: its items before its keys.
enum Entries {
    None,
    Array(JsObject),
    TypedArray(JsObject),
    Set,
    Map,
    ArrayBuffer(Vec<u8>),
    Promise(JsPromise),
    Weak,
}

fn format_raw(
    walk: &mut Walk,
    target: &JsObject,
    recurse: usize,
    context: &mut Context,
) -> JsResult<String> {
    let value: JsValue = target.clone().into();
    let constructor = constructor_name(target, context)?;
    let mut tag = match target.get(JsSymbol::to_string_tag(), context)? {
        t if t.is_string() => crate::print(&t),
        _ => String::new(),
    };
    if !tag.is_empty() {
        let key: PropertyKey = JsSymbol::to_string_tag().into();
        let listed = match descriptor(target, &key, context)? {
            Some(d) => walk.options.show_hidden || d.enumerable,
            None => false,
        };
        if listed {
            tag.clear();
        }
    }
    let hidden = walk.options.show_hidden;
    let mut keys: Vec<PropertyKey>;
    let mut base = String::new();
    let mut braces = (String::from("{"), String::from("}"));
    let mut extras = Extras::Object;
    let mut entries = Entries::None;

    if target.is_array() {
        let length = target
            .get(js_string!("length"), context)?
            .to_length(context)?;
        let shown_prefix = if constructor.as_deref() != Some("Array") || !tag.is_empty() {
            prefix(&constructor, &tag, "Array", &format!("({length})"))
        } else {
            String::new()
        };
        keys = own_keys(target, hidden, context)?
            .into_iter()
            .filter(|k| !is_index(k))
            .collect();
        braces = (format!("{shown_prefix}["), "]".into());
        if length == 0 && keys.is_empty() {
            return Ok(format!("{}]", braces.0));
        }
        extras = Extras::Array;
        entries = Entries::Array(target.clone());
    } else if let Ok(set) = JsSet::from_object(target.clone()) {
        let size = set.size();
        let shown_prefix = prefix(&constructor, &tag, "Set", &format!("({size})"));
        keys = own_keys(target, hidden, context)?;
        if size == 0 && keys.is_empty() {
            return Ok(format!("{shown_prefix}{{}}"));
        }
        braces = (format!("{shown_prefix}{{"), "}".into());
        entries = Entries::Set;
    } else if let Ok(map) = JsMap::from_object(target.clone()) {
        let size = map.get_size(context)?.to_length(context)?;
        let shown_prefix = prefix(&constructor, &tag, "Map", &format!("({size})"));
        keys = own_keys(target, hidden, context)?;
        if size == 0 && keys.is_empty() {
            return Ok(format!("{shown_prefix}{{}}"));
        }
        braces = (format!("{shown_prefix}{{"), "}".into());
        entries = Entries::Map;
    } else if let Ok(typed) = JsTypedArray::from_object(target.clone()) {
        keys = own_keys(target, hidden, context)?
            .into_iter()
            .filter(|k| !is_index(k))
            .collect();
        let size = typed.length(context)?;
        let fallback = match constructor {
            None => typed
                .kind()
                .map(|k| format!("{k:?}Array"))
                .unwrap_or_default(),
            Some(_) => String::new(),
        };
        let shown_prefix = prefix(&constructor, &tag, &fallback, &format!("({size})"));
        braces = (format!("{shown_prefix}["), "]".into());
        if size == 0 && keys.is_empty() && !hidden {
            return Ok(format!("{}]", braces.0));
        }
        extras = Extras::Array;
        entries = Entries::TypedArray(target.clone());
    } else if JsMapIterator::from_object(target.clone()).is_ok()
        || JsSetIterator::from_object(target.clone()).is_ok()
    {
        // Its items cannot be read without taking them from whoever iterates it.
        keys = own_keys(target, hidden, context)?;
        braces = (
            format!("{}{{", prefix(&constructor, &tag, "Object", "")),
            "}".into(),
        );
        if keys.is_empty() {
            return Ok(format!("{}}}", braces.0));
        }
    } else {
        keys = own_keys(target, hidden, context)?;
        if target.is_callable() {
            base = function_base(target, &constructor, &tag, context)?;
            if keys.is_empty() {
                return Ok(walk.stylize(&base, "special"));
            }
        } else if constructor.as_deref() == Some("Object")
            && boxed(target).is_none()
            && !is_error(target, context)
            && JsPromise::from_object(target.clone()).is_err()
            && JsArrayBuffer::from_object(target.clone()).is_err()
        {
            if !tag.is_empty() {
                braces.0 = format!("{}{{", prefix(&constructor, &tag, "Object", ""));
            }
            if keys.is_empty() {
                return Ok(format!("{}}}", braces.0));
            }
        } else if let Ok(regexp) = JsRegExp::from_object(target.clone()) {
            base = regexp.to_string(context)?;
            let shown_prefix = prefix(&constructor, &tag, "RegExp", "");
            if shown_prefix != "RegExp " {
                base = format!("{shown_prefix}{base}");
            }
            if keys.is_empty() || walk.options.depth < recurse as f64 {
                return Ok(walk.stylize(&base, "regexp"));
            }
        } else if let Ok(date) = JsDate::from_object(target.clone()) {
            let time = date.get_time(context)?.to_number(context)?;
            base = if time.is_nan() {
                "Invalid Date".into()
            } else {
                crate::print(&date.to_iso_string(context)?)
            };
            let shown_prefix = prefix(&constructor, &tag, "Date", "");
            if shown_prefix != "Date " {
                base = format!("{shown_prefix}{base}");
            }
            if keys.is_empty() {
                return Ok(walk.stylize(&base, "date"));
            }
        } else if is_error(target, context) {
            base = format_error(walk, target, &mut keys, context)?;
            if keys.is_empty() {
                return Ok(base);
            }
        } else if let Ok(buffer) = JsArrayBuffer::from_object(target.clone()) {
            let shown_prefix = prefix(&constructor, &tag, "ArrayBuffer", "");
            braces.0 = format!("{shown_prefix}{{");
            keys.insert(0, js_string!("byteLength").into());
            entries = Entries::ArrayBuffer(buffer.to_vec().unwrap_or_default());
        } else if JsSharedArrayBuffer::from_object(target.clone()).is_ok() {
            let shown_prefix = prefix(&constructor, &tag, "SharedArrayBuffer", "");
            braces.0 = format!("{shown_prefix}{{");
            keys.insert(0, js_string!("byteLength").into());
        } else if JsDataView::from_object(target.clone()).is_ok() {
            braces.0 = format!("{}{{", prefix(&constructor, &tag, "DataView", ""));
            for (at, key) in ["byteLength", "byteOffset", "buffer"]
                .into_iter()
                .enumerate()
            {
                keys.insert(at, JsString::from(key).into());
            }
        } else if let Ok(promise) = JsPromise::from_object(target.clone()) {
            braces.0 = format!("{}{{", prefix(&constructor, &tag, "Promise", ""));
            entries = Entries::Promise(promise);
        } else if JsWeakSet::from_object(target.clone()).is_ok() {
            braces.0 = format!("{}{{", prefix(&constructor, &tag, "WeakSet", ""));
            entries = Entries::Weak;
        } else if JsWeakMap::from_object(target.clone()).is_ok() {
            braces.0 = format!("{}{{", prefix(&constructor, &tag, "WeakMap", ""));
            entries = Entries::Weak;
        } else if let Some((kind, primitive)) = boxed(target) {
            if kind == "String" {
                keys.retain(|k| !is_index(k));
            }
            let plain = walk.clone_plain();
            let shown = format_primitive(&plain, &primitive, context)?;
            let mut text = format!("[{kind}");
            if constructor.as_deref() != Some(kind) {
                match &constructor {
                    None => text.push_str(" (null prototype)"),
                    Some(name) => text.push_str(&format!(" ({name})")),
                }
            }
            text.push_str(&format!(": {shown}]"));
            if !tag.is_empty() && constructor.as_deref() != Some(tag.as_str()) {
                text.push_str(&format!(" [{tag}]"));
            }
            base = walk.stylize(
                &text,
                if kind == "Symbol" {
                    "symbol"
                } else {
                    "special"
                },
            );
            if keys.is_empty() {
                return Ok(base);
            }
        } else {
            let shown_prefix = prefix(&constructor, &tag, "Object", "");
            if keys.is_empty() {
                return Ok(format!("{shown_prefix}{{}}"));
            }
            braces.0 = format!("{shown_prefix}{{");
        }
    }

    if walk.options.depth < recurse as f64 {
        let fallback = if target.is_array() { "Array" } else { "Object" };
        let name = prefix(&constructor, &tag, fallback, "");
        let name = name.trim_end();
        let shown = if constructor.is_some() {
            format!("[{name}]")
        } else {
            name.to_owned()
        };
        return Ok(walk.stylize(&shown, "special"));
    }

    let recurse = recurse + 1;
    walk.seen.push(target.clone());
    walk.current_depth = recurse;
    let mut output = format_entries(walk, &value, &entries, recurse, context)?;
    for key in &keys {
        output.push(format_property(
            walk, target, key, recurse, extras, context,
        )?);
    }
    walk.seen.pop();

    if let Some(index) = walk
        .circular
        .iter()
        .position(|c| JsObject::equals(c, target))
    {
        let reference = walk.stylize(&format!("<ref *{}>", index + 1), "special");
        if walk.options.compact != Compact::True {
            base = if base.is_empty() {
                reference
            } else {
                format!("{reference} {base}")
            };
        } else {
            braces.0 = format!("{reference} {}", braces.0);
        }
    }

    if walk.options.sorted {
        if extras == Extras::Object {
            output.sort();
        } else if keys.len() > 1 {
            let from = output.len() - keys.len();
            output[from..].sort();
        }
    }
    Ok(reduce(
        walk,
        output,
        &base,
        &braces,
        extras,
        recurse,
        Some(&value),
        context,
    ))
}

impl Walk {
    /// A walk with no colours, for a boxed primitive's value.
    fn clone_plain(&self) -> Walk {
        let mut options = self.options.clone();
        options.colors = false;
        Walk {
            options,
            indentation: self.indentation,
            current_depth: self.current_depth,
            seen: Vec::new(),
            circular: Vec::new(),
        }
    }
}

fn function_base(
    target: &JsObject,
    constructor: &Option<String>,
    tag: &str,
    context: &mut Context,
) -> JsResult<String> {
    let name = function_name(target, context)?;
    if is_class(target, context)? {
        let mut base = if name.is_empty() {
            "[class (anonymous)".to_owned()
        } else {
            format!("[class {name}")
        };
        if let Some(parent) = target.prototype() {
            let function_prototype = context.intrinsics().constructors().function().prototype();
            if !JsObject::equals(&parent, &function_prototype) && parent.is_callable() {
                let parent_name = function_name(&parent, context)?;
                if !parent_name.is_empty() {
                    base.push_str(&format!(" extends {parent_name}"));
                }
            }
        }
        base.push(']');
        if let Some(c) = constructor
            && c != "Function"
        {
            base.push_str(&format!(" [{c}]"));
        }
        return Ok(base);
    }
    let kind = function_kind(target, context)?;
    let mut base = format!("[{kind}");
    if constructor.is_none() {
        base.push_str(" (null prototype)");
    }
    if name.is_empty() {
        base.push_str(" (anonymous)");
    } else {
        base.push_str(&format!(": {name}"));
    }
    base.push(']');
    if let Some(c) = constructor
        && *c != kind
        && c != "Function"
    {
        base.push_str(&format!(" {c}"));
    }
    if !tag.is_empty() && constructor.as_deref() != Some(tag) {
        base.push_str(&format!(" [{tag}]"));
    }
    Ok(base)
}

/// An error as Node writes one: its stack, headed `Name: message`, or `[Name: message]` when
/// it has no stack; the keys its stack already says are left out.
fn format_error(
    walk: &Walk,
    target: &JsObject,
    keys: &mut Vec<PropertyKey>,
    context: &mut Context,
) -> JsResult<String> {
    let name = match target.get(js_string!("name"), context)? {
        n if n.is_null_or_undefined() => "Error".to_owned(),
        n => text_of(&n, context)?,
    };
    let message = match target.get(js_string!("message"), context)? {
        m if m.is_undefined() => String::new(),
        m => text_of(&m, context)?,
    };
    let header = if message.is_empty() {
        name.clone()
    } else {
        format!("{name}: {message}")
    };
    let stack = match target.get(js_string!("stack"), context)? {
        s if s.is_string() => crate::print(&s),
        _ => String::new(),
    };
    // Boa's stack is the frames alone; another engine's starts with the header.
    let frames_only = stack.trim_start().starts_with("at ");
    let mut text = if stack.trim().is_empty() {
        header.clone()
    } else if frames_only {
        format!("{header}\n{}", stack.trim_end())
    } else {
        stack.trim_end().to_owned()
    };
    keys.retain(|key| {
        let PropertyKey::String(k) = key else {
            return true;
        };
        let k = k.to_std_string_escaped();
        !(k == "stack"
            || (k == "message" && text.contains(&message))
            || (k == "name" && text.contains(&name)))
    });
    if has_own(target, js_string!("cause"), context)?
        && !keys
            .iter()
            .any(|k| matches!(k, PropertyKey::String(s) if s.to_std_string_escaped() == "cause"))
    {
        keys.push(js_string!("cause").into());
    }
    if get(&target.clone().into(), "errors", context)?.is_object()
        && target
            .get(js_string!("errors"), context)?
            .as_object()
            .is_some_and(|e| e.is_array())
        && !keys
            .iter()
            .any(|k| matches!(k, PropertyKey::String(s) if s.to_std_string_escaped() == "errors"))
    {
        keys.push(js_string!("errors").into());
    }
    let from = text.find(&message).map_or(0, |at| at + message.len());
    if !text[from.min(text.len())..].contains("\n    at") {
        text = format!("[{text}]");
    }
    if walk.indentation != 0 {
        text = text.replace('\n', &format!("\n{}", " ".repeat(walk.indentation)));
    }
    Ok(text)
}

fn format_entries(
    walk: &mut Walk,
    value: &JsValue,
    entries: &Entries,
    recurse: usize,
    context: &mut Context,
) -> JsResult<Vec<String>> {
    let mut output = Vec::new();
    match entries {
        Entries::None => {}
        Entries::Array(target) => {
            let length = target
                .get(js_string!("length"), context)?
                .to_length(context)? as usize;
            let max = walk.options.max_array_length.min(length as f64) as usize;
            let mut at = 0;
            let mut shown = 0;
            while at < length && shown < max {
                if !has_own(target, at as u32, context)? {
                    let mut holes = 0;
                    while at < length && !has_own(target, at as u32, context)? {
                        holes += 1;
                        at += 1;
                    }
                    output.push(walk.stylize(
                        &format!("<{holes} empty item{}>", if holes > 1 { "s" } else { "" }),
                        "undefined",
                    ));
                } else {
                    let key: PropertyKey = (at as u32).into();
                    output.push(format_property(
                        walk,
                        target,
                        &key,
                        recurse,
                        Extras::Array,
                        context,
                    )?);
                    at += 1;
                }
                shown += 1;
            }
            let remaining = length - at;
            if remaining > 0 {
                output.push(format!(
                    "... {remaining} more item{}",
                    if remaining > 1 { "s" } else { "" }
                ));
            }
        }
        Entries::TypedArray(target) => {
            let length = target
                .get(js_string!("length"), context)?
                .to_length(context)? as usize;
            let max = walk.options.max_array_length.min(length as f64) as usize;
            for at in 0..max {
                let item = target.get(at as u32, context)?;
                output.push(format_primitive(walk, &item, context)?);
            }
            if length > max {
                let remaining = length - max;
                output.push(format!(
                    "... {remaining} more item{}",
                    if remaining > 1 { "s" } else { "" }
                ));
            }
        }
        Entries::Set => {
            walk.indentation += 2;
            let all = items(value, context)?;
            let max = walk.options.max_array_length.min(all.len() as f64) as usize;
            for item in &all[..max] {
                output.push(format_value(walk, item, recurse, context)?);
            }
            if all.len() > max {
                let remaining = all.len() - max;
                output.push(format!(
                    "... {remaining} more item{}",
                    if remaining > 1 { "s" } else { "" }
                ));
            }
            walk.indentation -= 2;
        }
        Entries::Map => {
            walk.indentation += 2;
            let all = items(value, context)?;
            let max = walk.options.max_array_length.min(all.len() as f64) as usize;
            for pair in &all[..max] {
                let pair = crate::native::list_of(pair, context)?;
                let key = format_value(
                    walk,
                    &pair.first().cloned().unwrap_or_default(),
                    recurse,
                    context,
                )?;
                let item = format_value(
                    walk,
                    &pair.get(1).cloned().unwrap_or_default(),
                    recurse,
                    context,
                )?;
                output.push(format!("{key} => {item}"));
            }
            if all.len() > max {
                let remaining = all.len() - max;
                output.push(format!(
                    "... {remaining} more item{}",
                    if remaining > 1 { "s" } else { "" }
                ));
            }
            walk.indentation -= 2;
        }
        Entries::ArrayBuffer(bytes) => {
            let max = (walk.options.max_array_length as usize).min(bytes.len());
            let mut hex: Vec<String> = bytes[..max].iter().map(|b| format!("{b:02x}")).collect();
            if bytes.len() > max {
                hex.push(format!(
                    "... {} more byte{}",
                    bytes.len() - max,
                    if bytes.len() - max > 1 { "s" } else { "" }
                ));
            }
            output.push(format!(
                "{}: <{}>",
                walk.stylize("[Uint8Contents]", "special"),
                hex.join(" ")
            ));
        }
        Entries::Promise(promise) => match promise.state() {
            PromiseState::Pending => output.push(walk.stylize("<pending>", "special")),
            PromiseState::Fulfilled(result) => {
                walk.indentation += 2;
                let shown = format_value(walk, &result, recurse, context)?;
                walk.indentation -= 2;
                output.push(shown);
            }
            PromiseState::Rejected(reason) => {
                walk.indentation += 2;
                let shown = format_value(walk, &reason, recurse, context)?;
                walk.indentation -= 2;
                output.push(format!("{} {shown}", walk.stylize("<rejected>", "special")));
            }
        },
        Entries::Weak => output.push(walk.stylize("<items unknown>", "special")),
    }
    Ok(output)
}

/// A key as Node writes it: bare when it is an identifier, quoted otherwise, in brackets when
/// it is a symbol or not enumerable.
fn key_text(walk: &Walk, key: &PropertyKey, enumerable: bool) -> String {
    match key {
        PropertyKey::Symbol(symbol) => {
            let text = symbol.descriptive_string().to_std_string_escaped();
            format!("[{}]", walk.stylize(&text, "symbol"))
        }
        _ => {
            let text = match key {
                PropertyKey::String(s) => s.to_std_string_escaped(),
                PropertyKey::Index(i) => i.get().to_string(),
                PropertyKey::Symbol(_) => unreachable!(),
            };
            if text == "__proto__" {
                "['__proto__']".into()
            } else if !enumerable {
                format!("[{text}]")
            } else if is_identifier(&text) {
                walk.stylize(&text, "name")
            } else {
                walk.stylize(&quote(&text), "string")
            }
        }
    }
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn format_property(
    walk: &mut Walk,
    target: &JsObject,
    key: &PropertyKey,
    recurse: usize,
    extras: Extras,
    context: &mut Context,
) -> JsResult<String> {
    let found = descriptor(target, key, context)?;
    let found = match found {
        Some(found) => found,
        // A key listed but not own — `byteLength` of an `ArrayBuffer` — read as a value.
        None => Descriptor {
            value: Some(target.get(key.clone(), context)?),
            get: None,
            set: None,
            enumerable: true,
        },
    };
    let shown = if let Some(value) = &found.value {
        let diff = if walk.options.compact != Compact::True || extras != Extras::Object {
            2
        } else {
            3
        };
        walk.indentation += diff;
        let shown = format_value(walk, value, recurse, context);
        walk.indentation -= diff;
        shown?
    } else if let Some(getter) = &found.get {
        let label = if found.set.is_some() {
            "Getter/Setter"
        } else {
            "Getter"
        };
        if walk.options.getters {
            match crate::call(context, getter, &target.clone().into(), &[]) {
                Ok(value) => {
                    walk.indentation += 2;
                    let shown = format_value(walk, &value, recurse, context);
                    walk.indentation -= 2;
                    format!(
                        "{} {}",
                        walk.stylize(&format!("[{label}:"), "special"),
                        shown?
                    ) + &walk.stylize("]", "special")
                }
                Err(error) => {
                    let message = crate::describe(context, &error);
                    walk.stylize(
                        &format!("[{label}: <Inspection threw ({message})>]"),
                        "special",
                    )
                }
            }
        } else {
            walk.stylize(&format!("[{label}]"), "special")
        }
    } else if found.set.is_some() {
        walk.stylize("[Setter]", "special")
    } else {
        walk.stylize("undefined", "undefined")
    };
    if extras == Extras::Array && is_index(key) {
        return Ok(shown);
    }
    Ok(format!(
        "{}: {shown}",
        key_text(walk, key, found.enumerable)
    ))
}

/// Long arrays of short entries in columns, as Node lays them out.
fn group_array_elements(
    walk: &Walk,
    output: Vec<String>,
    value: Option<&JsValue>,
    context: &mut Context,
) -> JsResult<Vec<String>> {
    let mut total = 0usize;
    let mut max_length = 0usize;
    let mut output_length = output.len();
    let more_items = (walk.options.max_array_length as usize) < output.len()
        || output.last().is_some_and(|l| {
            l.starts_with("... ") && l.ends_with("more items") || l.ends_with("more item")
        });
    if more_items && output_length > 0 {
        output_length -= 1;
    }
    let separator_space = 2;
    let mut lengths = Vec::with_capacity(output_length);
    for entry in &output[..output_length] {
        let len = width(entry);
        lengths.push(len);
        total += len + separator_space;
        max_length = max_length.max(len);
    }
    let actual_max = max_length + separator_space;
    let break_length = walk.options.break_length;
    if (actual_max * 3 + walk.indentation) as f64 >= break_length
        || !((total as f64 / actual_max as f64) > 5. || max_length <= 6)
    {
        return Ok(output);
    }
    let average_bias = (actual_max as f64 - total as f64 / output.len() as f64).sqrt();
    let biased_max = (actual_max as f64 - 3. - average_bias).max(1.);
    let compact = match walk.options.compact {
        Compact::Level(n) => n as f64,
        Compact::True => 3.,
    };
    let columns = ((2.5 * biased_max * output_length as f64).sqrt() / biased_max)
        .round()
        .min(((break_length - walk.indentation as f64) / actual_max as f64).floor())
        .min(compact * 4.)
        .min(15.);
    if columns <= 1. {
        return Ok(output);
    }
    let columns = columns as usize;
    let mut max_line_length = Vec::with_capacity(columns);
    for i in 0..columns {
        let mut line_length = 0;
        let mut j = i;
        while j < output_length {
            line_length = line_length.max(lengths[j]);
            j += columns;
        }
        max_line_length.push(line_length + separator_space);
    }
    // Numbers line up on the right, everything else on the left.
    let mut pad_start = value.is_some();
    if let Some(value) = value.and_then(|v| v.as_object()) {
        for at in 0..output_length {
            let item = value.get(at as u32, context)?;
            if !item.is_number() && !item.is_bigint() {
                pad_start = false;
                break;
            }
        }
    }
    let pad = |text: &str, to: usize, start: bool| -> String {
        let len = width(text);
        if len >= to {
            return text.to_owned();
        }
        let fill = " ".repeat(to - len);
        if start {
            format!("{fill}{text}")
        } else {
            format!("{text}{fill}")
        }
    };
    let mut grouped = Vec::new();
    let mut i = 0;
    while i < output_length {
        let max = (i + columns).min(output_length);
        let mut line = String::new();
        let mut j = i;
        while j < max - 1 {
            line.push_str(&pad(
                &format!("{}, ", output[j]),
                max_line_length[j - i],
                pad_start,
            ));
            j += 1;
        }
        if pad_start {
            line.push_str(&pad(
                &output[j],
                max_line_length[j - i] - separator_space,
                true,
            ));
        } else {
            line.push_str(&output[j]);
        }
        grouped.push(line);
        i += columns;
    }
    if more_items {
        grouped.push(output[output_length].clone());
    }
    Ok(grouped)
}

fn below_break_length(walk: &Walk, output: &[String], start: usize, base: &str) -> bool {
    let mut total = output.len() + start;
    if (total + output.len()) as f64 > walk.options.break_length {
        return false;
    }
    for entry in output {
        total += width(entry);
        if total as f64 > walk.options.break_length {
            return false;
        }
    }
    base.is_empty() || !base.contains('\n')
}

#[allow(clippy::too_many_arguments)]
fn reduce(
    walk: &mut Walk,
    mut output: Vec<String>,
    base: &str,
    braces: &(String, String),
    extras: Extras,
    recurse: usize,
    value: Option<&JsValue>,
    context: &mut Context,
) -> String {
    let base_space = if base.is_empty() {
        String::new()
    } else {
        format!("{base} ")
    };
    match walk.options.compact {
        Compact::Level(level) => {
            if level >= 1 {
                let entries = output.len();
                if extras == Extras::Array && entries > 6 {
                    output = group_array_elements(walk, output, value, context).unwrap_or_default();
                }
                if (walk.current_depth - recurse) < level as usize && entries == output.len() {
                    let start = output.len() + walk.indentation + braces.0.len() + base.len() + 10;
                    if below_break_length(walk, &output, start, base) {
                        let joined = output.join(", ");
                        if !joined.contains('\n') {
                            return format!("{base_space}{} {joined} {}", braces.0, braces.1);
                        }
                    }
                }
            }
            let indentation = format!("\n{}", " ".repeat(walk.indentation));
            format!(
                "{base_space}{}{indentation}  {}{indentation}{}",
                braces.0,
                output.join(&format!(",{indentation}  ")),
                braces.1
            )
        }
        Compact::True => {
            if below_break_length(walk, &output, 0, base) {
                let base = if base.is_empty() {
                    String::new()
                } else {
                    format!(" {base}")
                };
                return format!("{}{base} {} {}", braces.0, output.join(", "), braces.1);
            }
            let indentation = " ".repeat(walk.indentation);
            let line = if base.is_empty() && braces.0.len() == 1 {
                " ".to_owned()
            } else {
                let base = if base.is_empty() {
                    String::new()
                } else {
                    format!(" {base}")
                };
                format!("{base}\n{indentation}  ")
            };
            format!(
                "{}{line}{} {}",
                braces.0,
                output.join(&format!(",\n{indentation}  ")),
                braces.1
            )
        }
    }
}

// ─── format ──────────────────────────────────────────────────────────────────────────────────

/// The names of the engine's own classes: an object whose `toString` is one of theirs is
/// written by `%s` as `inspect` would write it.
const BUILT_IN_OBJECTS: &[&str] = &[
    "Object",
    "Function",
    "Array",
    "Number",
    "Boolean",
    "String",
    "Symbol",
    "Date",
    "Promise",
    "RegExp",
    "Error",
    "AggregateError",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "ArrayBuffer",
    "Uint8Array",
    "Int8Array",
    "Uint16Array",
    "Int16Array",
    "Uint32Array",
    "Int32Array",
    "Float32Array",
    "Float64Array",
    "Uint8ClampedArray",
    "BigUint64Array",
    "BigInt64Array",
    "DataView",
    "Map",
    "BigInt",
    "Set",
    "WeakMap",
    "WeakSet",
    "Proxy",
    "Reflect",
    "SharedArrayBuffer",
    "WeakRef",
    "FinalizationRegistry",
];

fn has_built_in_to_string(target: &JsObject, context: &mut Context) -> JsResult<bool> {
    if JsProxy::from_object(target.clone()).is_ok() {
        return Ok(true);
    }
    if !target.get(js_string!("toString"), context)?.is_callable() {
        return Ok(true);
    }
    if has_own(target, js_string!("toString"), context)? {
        return Ok(false);
    }
    let mut pointer = target.clone();
    loop {
        match pointer.prototype() {
            None => return Ok(true),
            Some(next) => {
                pointer = next;
                if has_own(&pointer, js_string!("toString"), context)? {
                    break;
                }
            }
        }
    }
    let key: PropertyKey = js_string!("constructor").into();
    let Some(found) = descriptor(&pointer, &key, context)? else {
        return Ok(false);
    };
    let Some(constructor) = found.value.and_then(|v| v.as_object()) else {
        return Ok(false);
    };
    if !constructor.is_callable() {
        return Ok(false);
    }
    let name = function_name(&constructor, context)?;
    Ok(BUILT_IN_OBJECTS.contains(&name.as_str()))
}

/// `JSON.stringify`, with a cycle written `[Circular]` as `%j` writes it.
fn json_text(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let json = context.intrinsics().objects().json();
    let stringify = json.get(js_string!("stringify"), context)?;
    match crate::call(
        context,
        &stringify,
        &json.into(),
        std::slice::from_ref(value),
    ) {
        Ok(out) => Ok(if out.is_undefined() {
            "undefined".into()
        } else {
            crate::print(&out)
        }),
        Err(error) => {
            let text = crate::describe(context, &error);
            if text.contains("ircular") || text.contains("cyclic") {
                Ok("[Circular]".into())
            } else {
                Err(error)
            }
        }
    }
}

/// `util.formatWithOptions(options, ...args)`: the first argument as a pattern when it is a
/// string, the rest after it — strings as they are, everything else inspected.
pub(crate) fn format(
    args: &[JsValue],
    options: &Options,
    context: &mut Context,
) -> JsResult<String> {
    let Some(first) = args.first() else {
        return Ok(String::new());
    };
    let mut out = String::new();
    let mut next = 0usize;
    if let Some(pattern) = first.as_string() {
        next = 1;
        let pattern = pattern.to_std_string_escaped();
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            let Some(&spec) = chars.peek() else {
                out.push('%');
                continue;
            };
            if spec == '%' {
                chars.next();
                out.push('%');
                continue;
            }
            if !matches!(spec, 's' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c') {
                out.push('%');
                continue;
            }
            if next >= args.len() {
                out.push('%');
                continue;
            }
            chars.next();
            let value = &args[next];
            next += 1;
            let piece = match spec {
                's' => {
                    if let Some(n) = value.as_number() {
                        number_text(n, false)
                    } else if let Some(n) = value.as_bigint() {
                        format!("{}n", n.to_string_radix(10))
                    } else if let Some(target) = value.as_object().filter(|o| !o.is_callable()) {
                        if has_built_in_to_string(&target, context)? {
                            let mut shallow = options.clone();
                            shallow.depth = 0.;
                            shallow.colors = false;
                            shallow.compact = Compact::Level(3);
                            inspect(value, &shallow, context)?
                        } else {
                            text_of(value, context)?
                        }
                    } else if value.is_symbol() {
                        crate::print(value)
                    } else {
                        text_of(value, context)?
                    }
                }
                'd' => {
                    if let Some(n) = value.as_bigint() {
                        format!("{}n", n.to_string_radix(10))
                    } else if value.is_symbol() {
                        "NaN".into()
                    } else {
                        number_text(value.to_number(context).unwrap_or(f64::NAN), false)
                    }
                }
                'i' => {
                    if let Some(n) = value.as_bigint() {
                        format!("{}n", n.to_string_radix(10))
                    } else if value.is_symbol() {
                        "NaN".into()
                    } else {
                        let parse = crate::native::global(context, "parseInt")?;
                        let text = text_of(value, context)?;
                        let n =
                            crate::call(context, &parse, &JsValue::undefined(), &[js_text(&text)])?;
                        number_text(n.to_number(context)?, false)
                    }
                }
                'f' => {
                    if value.is_symbol() {
                        "NaN".into()
                    } else {
                        let parse = crate::native::global(context, "parseFloat")?;
                        let text = text_of(value, context)?;
                        let n =
                            crate::call(context, &parse, &JsValue::undefined(), &[js_text(&text)])?;
                        number_text(n.to_number(context)?, false)
                    }
                }
                'j' => json_text(value, context)?,
                'o' => {
                    let mut all = options.clone();
                    all.show_hidden = true;
                    all.depth = 4.;
                    inspect(value, &all, context)?
                }
                'O' => inspect(value, options, context)?,
                _ => String::new(),
            };
            out.push_str(&piece);
        }
    }
    for (at, value) in args.iter().enumerate().skip(next) {
        if at > 0 {
            out.push(' ');
        }
        if value.is_string() {
            out.push_str(&crate::print(value));
        } else {
            out.push_str(&inspect(value, options, context)?);
        }
    }
    Ok(out)
}

/// The options `util.inspect.defaultOptions` holds now, when `util` has been made; Node's own
/// defaults otherwise.
pub(crate) fn default_options(context: &mut Context) -> JsResult<Options> {
    let made = crate::shared(context)
        .builtins
        .borrow()
        .get("util")
        .cloned();
    let Some(util) = made else {
        return Ok(Options::default());
    };
    let inspect_fn = get(&util, "inspect", context)?;
    let defaults = get(&inspect_fn, "defaultOptions", context)?;
    Options::default().with(&defaults, context)
}

/// What `console.log(...args)` writes: `util.format`'s line.
pub(crate) fn console_text(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    let options = default_options(context)?;
    format(args, &options, context)
}

/// The `util.inspect` function of this realm.
fn util_inspect_function(context: &mut Context) -> JsResult<JsValue> {
    let util = crate::modules::node_module(context, "util")?;
    get(&util, "inspect", context)
}

// ─── Deep equality ───────────────────────────────────────────────────────────────────────────

/// Whether `a` and `b` are deeply equal, as `assert.deepStrictEqual` (with `strict`) or
/// `assert.deepEqual` decides.
pub(crate) fn deep_equal(
    a: &JsValue,
    b: &JsValue,
    strict: bool,
    context: &mut Context,
) -> JsResult<bool> {
    let mut memo = Vec::new();
    inner_equal(a, b, strict, &mut memo, context)
}

fn loose_equals(a: &JsValue, b: &JsValue, context: &mut Context) -> JsResult<bool> {
    a.equals(b, context)
}

fn inner_equal(
    a: &JsValue,
    b: &JsValue,
    strict: bool,
    memo: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<bool> {
    if a.strict_equals(b) {
        return Ok(!strict || same_value(a, b) || !a.is_number());
    }
    let (Some(x), Some(y)) = (a.as_object(), b.as_object()) else {
        if strict {
            return Ok(
                a.as_number().is_some_and(f64::is_nan) && b.as_number().is_some_and(f64::is_nan)
            );
        }
        if a.is_object() || b.is_object() {
            return Ok(false);
        }
        let nan = a.as_number().is_some_and(f64::is_nan) && b.as_number().is_some_and(f64::is_nan);
        return Ok(nan || loose_equals(a, b, context)?);
    };
    if strict {
        let same = match (x.prototype(), y.prototype()) {
            (None, None) => true,
            (Some(p), Some(q)) => JsObject::equals(&p, &q),
            _ => false,
        };
        if !same {
            return Ok(false);
        }
    }
    if object_tag(a, context)? != object_tag(b, context)? {
        return Ok(false);
    }
    if memo
        .iter()
        .any(|(p, q)| JsObject::equals(p, &x) && JsObject::equals(q, &y))
    {
        return Ok(true);
    }
    memo.push((x.clone(), y.clone()));
    let outcome = compare_objects(&x, &y, strict, memo, context);
    memo.pop();
    outcome
}

fn compare_objects(
    x: &JsObject,
    y: &JsObject,
    strict: bool,
    memo: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<bool> {
    let (a, b): (JsValue, JsValue) = (x.clone().into(), y.clone().into());
    if x.is_array() {
        if !y.is_array() {
            return Ok(false);
        }
        let la = x.get(js_string!("length"), context)?.to_length(context)?;
        let lb = y.get(js_string!("length"), context)?.to_length(context)?;
        if la != lb {
            return Ok(false);
        }
        for at in 0..la as u32 {
            let (ha, hb) = (has_own(x, at, context)?, has_own(y, at, context)?);
            if strict && ha != hb {
                return Ok(false);
            }
            let (va, vb) = (x.get(at, context)?, y.get(at, context)?);
            if !inner_equal(&va, &vb, strict, memo, context)? {
                return Ok(false);
            }
        }
        return keys_equal(x, y, strict, true, memo, context);
    }
    if let Ok(date) = JsDate::from_object(x.clone()) {
        let Ok(other) = JsDate::from_object(y.clone()) else {
            return Ok(false);
        };
        let (ta, tb) = (
            date.get_time(context)?.to_number(context)?,
            other.get_time(context)?.to_number(context)?,
        );
        if !(ta == tb || (ta.is_nan() && tb.is_nan())) {
            return Ok(false);
        }
    } else if JsRegExp::from_object(x.clone()).is_ok() {
        if JsRegExp::from_object(y.clone()).is_err() {
            return Ok(false);
        }
        for key in ["source", "flags", "lastIndex"] {
            let (va, vb) = (
                x.get(JsString::from(key), context)?,
                y.get(JsString::from(key), context)?,
            );
            if !va.strict_equals(&vb) {
                return Ok(false);
            }
        }
    } else if is_error(x, context) {
        if !is_error(y, context) {
            return Ok(false);
        }
        for key in ["message", "name"] {
            let (va, vb) = (
                x.get(JsString::from(key), context)?,
                y.get(JsString::from(key), context)?,
            );
            if !va.strict_equals(&vb) {
                return Ok(false);
            }
        }
    } else if JsTypedArray::from_object(x.clone()).is_ok()
        || JsDataView::from_object(x.clone()).is_ok()
    {
        let (Some(ba), Some(bb)) = (
            crate::encoding::bytes_of(&a, context)?,
            crate::encoding::bytes_of(&b, context)?,
        ) else {
            return Ok(false);
        };
        if ba != bb {
            return Ok(false);
        }
        // A float array's `NaN`s and `-0`s compare as numbers would in loose mode — rare
        // enough that the bytes decide.
        return keys_equal_filtered(x, y, strict, memo, context, true);
    } else if let Ok(set) = JsSet::from_object(x.clone()) {
        let Ok(other) = JsSet::from_object(y.clone()) else {
            return Ok(false);
        };
        if set.size() != other.size() {
            return Ok(false);
        }
        if !set_equal(&a, &b, &other, strict, memo, context)? {
            return Ok(false);
        }
    } else if let Ok(map) = JsMap::from_object(x.clone()) {
        let Ok(other) = JsMap::from_object(y.clone()) else {
            return Ok(false);
        };
        if map.get_size(context)?.to_length(context)?
            != other.get_size(context)?.to_length(context)?
        {
            return Ok(false);
        }
        if !map_equal(&a, &b, &other, strict, memo, context)? {
            return Ok(false);
        }
    } else if JsArrayBuffer::from_object(x.clone()).is_ok() {
        let (Some(ba), Some(bb)) = (
            crate::encoding::bytes_of(&a, context)?,
            crate::encoding::bytes_of(&b, context)?,
        ) else {
            return Ok(false);
        };
        if ba != bb {
            return Ok(false);
        }
    } else if let Some((kind, va)) = boxed(x) {
        let Some((other_kind, vb)) = boxed(y) else {
            return Ok(false);
        };
        if kind != other_kind || !same_value(&va, &vb) {
            return Ok(false);
        }
    } else if y.is_array()
        || JsDate::from_object(y.clone()).is_ok()
        || JsRegExp::from_object(y.clone()).is_ok()
        || JsTypedArray::from_object(y.clone()).is_ok()
        || JsSet::from_object(y.clone()).is_ok()
        || JsMap::from_object(y.clone()).is_ok()
        || boxed(y).is_some()
        || is_error(y, context)
    {
        return Ok(false);
    }
    keys_equal(x, y, strict, false, memo, context)
}

fn keys_equal(
    x: &JsObject,
    y: &JsObject,
    strict: bool,
    array: bool,
    memo: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<bool> {
    keys_equal_filtered(x, y, strict, memo, context, array)
}

/// Both have the same own enumerable keys (and, strictly, symbols), each with equal values.
fn keys_equal_filtered(
    x: &JsObject,
    y: &JsObject,
    strict: bool,
    memo: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
    skip_indices: bool,
) -> JsResult<bool> {
    let mut ka = own_keys(x, false, context)?;
    let mut kb = own_keys(y, false, context)?;
    if skip_indices {
        ka.retain(|k| !is_index(k));
        kb.retain(|k| !is_index(k));
    }
    if !strict {
        ka.retain(|k| !matches!(k, PropertyKey::Symbol(_)));
        kb.retain(|k| !matches!(k, PropertyKey::Symbol(_)));
    }
    if ka.len() != kb.len() {
        return Ok(false);
    }
    for key in &ka {
        let listed = match descriptor(y, key, context)? {
            Some(d) => d.enumerable,
            None => false,
        };
        if !listed {
            return Ok(false);
        }
    }
    for key in ka {
        let (va, vb) = (x.get(key.clone(), context)?, y.get(key, context)?);
        if !inner_equal(&va, &vb, strict, memo, context)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn set_equal(
    a: &JsValue,
    b: &JsValue,
    other: &JsSet,
    strict: bool,
    memo: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<bool> {
    let mine = items(a, context)?;
    let theirs = items(b, context)?;
    let mut matched = vec![false; theirs.len()];
    for item in mine {
        if !item.is_object() && other.has(item.clone()) {
            if let Some(at) = theirs.iter().position(|t| same_value(t, &item)) {
                matched[at] = true;
            }
            continue;
        }
        let mut found = false;
        for (at, candidate) in theirs.iter().enumerate() {
            if matched[at] {
                continue;
            }
            if inner_equal(&item, candidate, strict, memo, context)? {
                matched[at] = true;
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}

fn map_equal(
    a: &JsValue,
    b: &JsValue,
    other: &JsMap,
    strict: bool,
    memo: &mut Vec<(JsObject, JsObject)>,
    context: &mut Context,
) -> JsResult<bool> {
    let mine = items(a, context)?;
    let theirs = items(b, context)?;
    let mut matched = vec![false; theirs.len()];
    for pair in mine {
        let pair = crate::native::list_of(&pair, context)?;
        let (key, value) = (pair[0].clone(), pair[1].clone());
        if !key.is_object() {
            if other.has(key.clone(), context)?.to_boolean() {
                let theirs_value = other.get(key.clone(), context)?;
                if !inner_equal(&value, &theirs_value, strict, memo, context)? {
                    return Ok(false);
                }
                if let Some(at) = theirs.iter().position(|t| {
                    crate::native::list_of(t, context)
                        .ok()
                        .is_some_and(|p| same_value(&p[0], &key))
                }) {
                    matched[at] = true;
                }
                continue;
            }
            if strict {
                return Ok(false);
            }
        }
        let mut found = false;
        for (at, candidate) in theirs.iter().enumerate() {
            if matched[at] {
                continue;
            }
            let candidate = crate::native::list_of(candidate, context)?;
            if inner_equal(&key, &candidate[0], strict, memo, context)?
                && inner_equal(&value, &candidate[1], strict, memo, context)?
            {
                matched[at] = true;
                found = true;
                break;
            }
        }
        if !found {
            return Ok(false);
        }
    }
    Ok(true)
}

// ─── promisify, callbackify, deprecate, debuglog ─────────────────────────────────────────────

/// `promisify(f)`'s function: a promise settled by `f`'s callback.
fn promisified(
    this: &JsValue,
    args: &[JsValue],
    f: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let settle = closure(
        context,
        "",
        2,
        |_, args, resolvers: &boa_engine::builtins::promise::ResolvingFunctions, cx| {
            let error = arg(args, 0);
            if error.is_null_or_undefined() {
                resolvers
                    .resolve
                    .call(&JsValue::undefined(), &[arg(args, 1)], cx)?;
            } else {
                resolvers.reject.call(&JsValue::undefined(), &[error], cx)?;
            }
            Ok(JsValue::undefined())
        },
        resolvers.clone(),
    );
    let mut call_args = args.to_vec();
    call_args.push(settle.into());
    if let Err(error) = crate::call(context, f, this, &call_args) {
        let reason = error.into_opaque(context)?;
        resolvers
            .reject
            .call(&JsValue::undefined(), &[reason], context)?;
    }
    Ok(promise.into())
}

fn promisify(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let f = arg(args, 0);
    if !f.is_callable() {
        return Err(coded_error(
            context,
            "The \"original\" argument must be of type function",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let custom_key =
        symbol_for(context, "nodejs.util.promisify.custom")?.to_property_key(context)?;
    let custom = get(&f, "", context)
        .ok()
        .and(
            f.as_object()
                .map(|o| o.get(custom_key.clone(), context))
                .transpose()?,
        )
        .unwrap_or_default();
    if !custom.is_undefined() {
        if !custom.is_callable() {
            return Err(coded_error(
                context,
                "The \"util.promisify.custom\" argument must be of type function",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        return Ok(custom);
    }
    let name = get(&f, "name", context)?;
    let wrapped = closure(
        context,
        &crate::print(&name),
        0,
        |this, args, f: &JsValue, cx| promisified(this, args, f, cx),
        f.clone(),
    );
    // What it inherits and owns, as Node copies them over.
    if let Some(original) = f.as_object() {
        wrapped.set_prototype(original.prototype());
    }
    crate::native::hidden(&wrapped, custom_key, wrapped.clone(), context)?;
    Ok(wrapped.into())
}

fn callbackify(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let f = arg(args, 0);
    if !f.is_callable() {
        return Err(coded_error(
            context,
            "The \"original\" argument must be of type function",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let name = get(&f, "name", context)?;
    Ok(closure(
        context,
        &format!("{}Callbackified", crate::print(&name)),
        0,
        |this, args, f: &JsValue, cx| {
            let Some((callback, rest)) = args.split_last() else {
                return Err(coded_error(
                    cx,
                    "The last argument must be of type function",
                    "ERR_INVALID_ARG_TYPE",
                ));
            };
            if !callback.is_callable() {
                return Err(coded_error(
                    cx,
                    "The last argument must be of type function",
                    "ERR_INVALID_ARG_TYPE",
                ));
            }
            let outcome = crate::call(cx, f, this, rest);
            let promise = match outcome {
                Ok(value) => JsPromise::resolve(value, cx)?,
                Err(error) => JsPromise::reject(error, cx)?,
            };
            let on_value = closure(
                cx,
                "",
                1,
                |_, args, callback: &JsValue, cx| {
                    crate::microtask(cx, callback.clone(), vec![JsValue::null(), arg(args, 0)]);
                    Ok(JsValue::undefined())
                },
                callback.clone(),
            );
            let on_error = closure(
                cx,
                "",
                1,
                |_, args, callback: &JsValue, cx| {
                    // A falsy reason is wrapped, so the callback can tell it was an error.
                    let mut reason = arg(args, 0);
                    if !reason.to_boolean() {
                        let wrapped = JsNativeError::error()
                            .with_message("Promise was rejected with a falsy value")
                            .into_opaque(cx);
                        wrapped.set(
                            js_string!("code"),
                            js_string!("ERR_FALSY_VALUE_REJECTION"),
                            false,
                            cx,
                        )?;
                        wrapped.set(js_string!("reason"), reason, false, cx)?;
                        reason = wrapped.into();
                    }
                    crate::microtask(cx, callback.clone(), vec![reason]);
                    Ok(JsValue::undefined())
                },
                callback.clone(),
            );
            promise.then(
                Some(
                    boa_engine::object::builtins::JsFunction::from_object(on_value)
                        .expect("a function"),
                ),
                Some(
                    boa_engine::object::builtins::JsFunction::from_object(on_error)
                        .expect("a function"),
                ),
                cx,
            )?;
            Ok(JsValue::undefined())
        },
        f,
    )
    .into())
}

/// `deprecate(f, message, code)`: `f`, warning once — on its first call — that it is
/// deprecated, unless `process.noDeprecation`.
fn deprecate(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let f = arg(args, 0);
    if !f.is_callable() {
        return Err(coded_error(
            context,
            "The \"fn\" argument must be of type function",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let state = object(context);
    state.set(js_string!("warned"), false, false, context)?;
    let name = get(&f, "name", context)?;
    let wrapped = closure(
        context,
        &crate::print(&name),
        0,
        |this, args, (f, message, code, state): &(JsValue, JsValue, JsValue, JsObject), cx| {
            if !state.get(js_string!("warned"), cx)?.to_boolean() {
                state.set(js_string!("warned"), true, false, cx)?;
                let process = crate::native::global(cx, "process")?;
                let silent = get(&process, "noDeprecation", cx)?.to_boolean();
                if !silent && process.is_object() {
                    let mut warn_args = vec![message.clone(), js_text("DeprecationWarning")];
                    if !code.is_undefined() {
                        warn_args.push(code.clone());
                    }
                    invoke(&process, "emitWarning", &warn_args, cx)?;
                }
            }
            crate::call(cx, f, this, args)
        },
        (f.clone(), arg(args, 1), arg(args, 2), state),
    );
    if let Some(original) = f.as_object() {
        let prototype = original.get(js_string!("prototype"), context)?;
        if prototype.is_object() {
            wrapped.set(js_string!("prototype"), prototype, false, context)?;
        }
    }
    Ok(wrapped.into())
}

/// Whether `NODE_DEBUG` names `section`: a comma-separated list of names, `*` matching any run.
fn debug_enabled(section: &str, context: &mut Context) -> JsResult<bool> {
    let process = crate::native::global(context, "process")?;
    let env = get(&process, "env", context)?;
    let wanted = get(&env, "NODE_DEBUG", context)?;
    if !wanted.is_string() {
        return Ok(false);
    }
    let wanted = crate::print(&wanted).to_ascii_uppercase();
    let section = section.to_ascii_uppercase();
    Ok(wanted
        .split([',', ' '])
        .filter(|w| !w.is_empty())
        .any(|pattern| glob(pattern, &section)))
}

fn glob(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => {
            text.starts_with(head)
                && (0..=text.len() - head.len()).any(|at| {
                    text.is_char_boundary(head.len() + at) && glob(tail, &text[head.len() + at..])
                })
        }
    }
}

fn debuglog(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let section = text_of(&arg(args, 0), context)?;
    let enabled = debug_enabled(&section, context)?;
    let logger = closure(
        context,
        "debug",
        0,
        |_, args, (section, enabled): &(JsValue, bool), cx| {
            if *enabled {
                let text = format(args, &Options::default(), cx)?;
                let line = format!(
                    "{} {}: {text}",
                    crate::print(section).to_ascii_uppercase(),
                    std::process::id()
                );
                crate::console_line(cx, crate::Level::Error, line);
            }
            Ok(JsValue::undefined())
        },
        (js_text(&section), enabled),
    );
    logger.set(js_string!("enabled"), enabled, false, context)?;
    Ok(logger.into())
}

// ─── The module ──────────────────────────────────────────────────────────────────────────────

/// `inspect(value, options)`, or the old `inspect(value, showHidden, depth, colors)`.
fn inspect_native(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut options = default_options_of_this(context)?;
    let second = arg(args, 1);
    if second.is_object() {
        options = options.with(&second, context)?;
    } else if args.len() > 1 {
        if !second.is_undefined() {
            options.show_hidden = second.to_boolean();
        }
        let depth = arg(args, 2);
        if args.len() > 2 && !depth.is_undefined() {
            options.depth = if depth.is_null() {
                f64::INFINITY
            } else {
                depth.to_number(context)?
            };
        }
        let colors = arg(args, 3);
        if !colors.is_undefined() {
            options.colors = colors.to_boolean();
        }
    }
    Ok(js_text(&inspect(&arg(args, 0), &options, context)?))
}

/// The defaults, read from `util.inspect.defaultOptions` while `util` is being made too.
fn default_options_of_this(context: &mut Context) -> JsResult<Options> {
    default_options(context)
}

const UTIL_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("format", 1, |_, args, cx| {
        let options = default_options(cx)?;
        Ok(js_text(&format(args, &options, cx)?))
    }),
    ("formatWithOptions", 2, |_, args, cx| {
        let given = arg(args, 0);
        if !given.is_object() {
            return Err(coded_error(
                cx,
                "The \"inspectOptions\" argument must be of type object",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        let options = default_options(cx)?.with(&given, cx)?;
        Ok(js_text(&format(&args[1..], &options, cx)?))
    }),
    ("inspect", 2, inspect_native),
    ("promisify", 1, promisify),
    ("callbackify", 1, callbackify),
    ("inherits", 2, |_, args, cx| {
        let (child, parent) = (arg(args, 0), arg(args, 1));
        let Some(child) = child.as_object().filter(|c| c.is_callable()) else {
            return Err(coded_error(
                cx,
                "The \"ctor\" argument must be of type function",
                "ERR_INVALID_ARG_TYPE",
            ));
        };
        let Some(parent) = parent.as_object().filter(|p| p.is_callable()) else {
            return Err(coded_error(
                cx,
                "The \"superCtor\" argument must be of type function",
                "ERR_INVALID_ARG_TYPE",
            ));
        };
        let parent_proto = parent.get(js_string!("prototype"), cx)?;
        if parent_proto.is_undefined() {
            return Err(coded_error(
                cx,
                "The \"superCtor.prototype\" property must be of type object",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        child.set(js_string!("super_"), parent.clone(), true, cx)?;
        let child_proto = child.get(js_string!("prototype"), cx)?;
        call_object_fn(cx, "setPrototypeOf", &[child_proto, parent_proto])?;
        Ok(JsValue::undefined())
    }),
    ("deprecate", 3, deprecate),
    ("debuglog", 2, debuglog),
    ("isDeepStrictEqual", 2, |_, args, cx| {
        Ok(deep_equal(&arg(args, 0), &arg(args, 1), true, cx)?.into())
    }),
    ("toUSVString", 1, |_, args, cx| {
        let text = arg(args, 0).to_string(cx)?;
        let units: Vec<u16> = text.iter().collect();
        Ok(js_text(&String::from_utf16_lossy(&units)))
    }),
    ("stripVTControlCharacters", 1, |_, args, cx| {
        let text = arg(args, 0);
        if !text.is_string() {
            return Err(coded_error(
                cx,
                "The \"str\" argument must be of type string",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        Ok(js_text(&strip_ansi(&crate::print(&text))))
    }),
    ("styleText", 2, |_, args, cx| {
        let formats = arg(args, 0);
        let text = text_of(&arg(args, 1), cx)?;
        let names: Vec<String> = if formats.is_object() {
            crate::native::list_of(&formats, cx)?
                .iter()
                .map(crate::print)
                .collect()
        } else {
            vec![text_of(&formats, cx)?]
        };
        let mut out = text;
        for name in names.iter().rev() {
            let Some((open, close)) = ansi_code(name) else {
                return Err(coded_error(
                    cx,
                    &format!("The argument 'format' must be a valid color. Received '{name}'"),
                    "ERR_INVALID_ARG_VALUE",
                ));
            };
            out = format!("\u{1b}[{open}m{out}\u{1b}[{close}m");
        }
        Ok(js_text(&out))
    }),
    ("getSystemErrorName", 1, |_, args, cx| {
        let code = arg(args, 0).to_number(cx)? as i32;
        Ok(js_text(
            system_errors()
                .iter()
                .find(|(n, _, _)| *n == code)
                .map_or("Unknown system error", |(_, name, _)| name),
        ))
    }),
    ("getSystemErrorMap", 0, |_, _, cx| {
        let map = boa_engine::object::builtins::JsMap::new(cx);
        for (code, name, message) in system_errors() {
            let entry = array(cx, vec![js_text(name), js_text(message)]);
            map.set(*code, entry, cx)?;
        }
        Ok(map.into())
    }),
    ("getSystemErrorMessage", 1, |_, args, cx| {
        let code = arg(args, 0).to_number(cx)? as i32;
        Ok(js_text(
            system_errors()
                .iter()
                .find(|(n, _, _)| *n == code)
                .map_or("Unknown system error", |(_, _, m)| m),
        ))
    }),
    ("aborted", 2, |_, args, cx| {
        // Settles once the signal aborts: at once when it has.
        let signal = arg(args, 0);
        let (promise, resolvers) = JsPromise::new_pending(cx);
        if get(&signal, "aborted", cx)?.to_boolean() {
            resolvers.resolve.call(&JsValue::undefined(), &[], cx)?;
        } else if signal.is_object() {
            let options = object(cx);
            options.set(js_string!("once"), true, false, cx)?;
            invoke(
                &signal,
                "addEventListener",
                &[
                    js_text("abort"),
                    resolvers.resolve.clone().into(),
                    options.into(),
                ],
                cx,
            )?;
        }
        Ok(promise.into())
    }),
    ("transferableAbortSignal", 1, |_, args, _| Ok(arg(args, 0))),
    ("transferableAbortController", 0, |_, _, cx| {
        Ok(crate::native::construct_global(cx, "AbortController", &[])?.into())
    }),
    ("parseArgs", 1, parse_args),
    ("getCallSite", 1, |_, _, cx| Ok(JsArray::new(cx)?.into())),
    ("getCallSites", 1, |_, _, cx| Ok(JsArray::new(cx)?.into())),
    // The old type checks Node keeps for compatibility.
    ("isArray", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.is_array())
            .into())
    }),
    ("isBoolean", 1, |_, args, _| {
        Ok(arg(args, 0).is_boolean().into())
    }),
    ("isNull", 1, |_, args, _| Ok(arg(args, 0).is_null().into())),
    ("isNullOrUndefined", 1, |_, args, _| {
        Ok(arg(args, 0).is_null_or_undefined().into())
    }),
    ("isNumber", 1, |_, args, _| {
        Ok(arg(args, 0).is_number().into())
    }),
    ("isString", 1, |_, args, _| {
        Ok(arg(args, 0).is_string().into())
    }),
    ("isSymbol", 1, |_, args, _| {
        Ok(arg(args, 0).is_symbol().into())
    }),
    ("isUndefined", 1, |_, args, _| {
        Ok(arg(args, 0).is_undefined().into())
    }),
    ("isRegExp", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| JsRegExp::from_object(o).is_ok())
            .into())
    }),
    ("isObject", 1, |_, args, _| {
        let v = arg(args, 0);
        Ok((v.is_object() && !v.is_callable()).into())
    }),
    ("isDate", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| JsDate::from_object(o).is_ok())
            .into())
    }),
    ("isError", 1, |_, args, cx| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| is_error(&o, cx))
            .into())
    }),
    ("isFunction", 1, |_, args, _| {
        Ok(arg(args, 0).is_callable().into())
    }),
    ("isPrimitive", 1, |_, args, _| {
        Ok((!arg(args, 0).is_object()).into())
    }),
    ("isBuffer", 1, |_, args, cx| {
        let buffer = crate::native::global(cx, "Buffer")?;
        let value = arg(args, 0);
        Ok((value.is_object() && value.instance_of(&buffer, cx).unwrap_or(false)).into())
    }),
    ("log", 0, |_, args, cx| {
        let text = format(args, &Options::default(), cx)?;
        crate::console_line(cx, crate::Level::Log, text);
        Ok(JsValue::undefined())
    }),
];

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{9b}' || (c == '\u{1b}' && chars.peek() == Some(&'[')) {
            if c == '\u{1b}' {
                chars.next();
            }
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `inspect.colors`: each style's ANSI codes, as `[open, close]`.
const ANSI: &[(&str, u8, u8)] = &[
    ("reset", 0, 0),
    ("bold", 1, 22),
    ("dim", 2, 22),
    ("italic", 3, 23),
    ("underline", 4, 24),
    ("blink", 5, 25),
    ("inverse", 7, 27),
    ("hidden", 8, 28),
    ("strikethrough", 9, 29),
    ("doubleunderline", 21, 24),
    ("black", 30, 39),
    ("red", 31, 39),
    ("green", 32, 39),
    ("yellow", 33, 39),
    ("blue", 34, 39),
    ("magenta", 35, 39),
    ("cyan", 36, 39),
    ("white", 37, 39),
    ("bgBlack", 40, 49),
    ("bgRed", 41, 49),
    ("bgGreen", 42, 49),
    ("bgYellow", 43, 49),
    ("bgBlue", 44, 49),
    ("bgMagenta", 45, 49),
    ("bgCyan", 46, 49),
    ("bgWhite", 47, 49),
    ("framed", 51, 54),
    ("overlined", 53, 55),
    ("gray", 90, 39),
    ("grey", 90, 39),
    ("redBright", 91, 39),
    ("greenBright", 92, 39),
    ("yellowBright", 93, 39),
    ("blueBright", 94, 39),
    ("magentaBright", 95, 39),
    ("cyanBright", 96, 39),
    ("whiteBright", 97, 39),
    ("bgGray", 100, 49),
    ("bgRedBright", 101, 49),
    ("bgGreenBright", 102, 49),
    ("bgYellowBright", 103, 49),
    ("bgBlueBright", 104, 49),
    ("bgMagentaBright", 105, 49),
    ("bgCyanBright", 106, 49),
    ("bgWhiteBright", 107, 49),
];

fn ansi_code(name: &str) -> Option<(u8, u8)> {
    ANSI.iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, open, close)| (*open, *close))
}

/// A few of libuv's error numbers, as `getSystemErrorName` names them.
fn system_errors() -> &'static [(i32, &'static str, &'static str)] {
    &[
        (-1, "EPERM", "operation not permitted"),
        (-2, "ENOENT", "no such file or directory"),
        (-3, "ESRCH", "no such process"),
        (-4, "EINTR", "interrupted system call"),
        (-5, "EIO", "i/o error"),
        (-9, "EBADF", "bad file descriptor"),
        (-11, "EAGAIN", "resource temporarily unavailable"),
        (-12, "ENOMEM", "not enough memory"),
        (-13, "EACCES", "permission denied"),
        (-17, "EEXIST", "file already exists"),
        (-20, "ENOTDIR", "not a directory"),
        (-21, "EISDIR", "illegal operation on a directory"),
        (-22, "EINVAL", "invalid argument"),
        (-24, "EMFILE", "too many open files"),
        (-28, "ENOSPC", "no space left on device"),
        (-32, "EPIPE", "broken pipe"),
        (-38, "ENOSYS", "function not implemented"),
        (-39, "ENOTEMPTY", "directory not empty"),
        (-104, "ECONNRESET", "connection reset by peer"),
        (-110, "ETIMEDOUT", "connection timed out"),
        (-111, "ECONNREFUSED", "connection refused"),
    ]
}

/// `util.parseArgs({ args, options, strict, allowPositionals })`, for `--long`, `--long=value`,
/// `-s` and positionals.
fn parse_args(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let config = arg(args, 0);
    let given = get(&config, "args", context)?;
    let list: Vec<String> = if given.is_object() {
        crate::native::list_of(&given, context)?
            .iter()
            .map(crate::print)
            .collect()
    } else {
        let process = crate::native::global(context, "process")?;
        let argv = get(&process, "argv", context)?;
        crate::native::list_of(&argv, context)?
            .iter()
            .skip(2)
            .map(crate::print)
            .collect()
    };
    let options = get(&config, "options", context)?;
    let strict = match get(&config, "strict", context)? {
        v if v.is_undefined() => true,
        v => v.to_boolean(),
    };
    let allow_positionals = match get(&config, "allowPositionals", context)? {
        v if v.is_undefined() => !strict,
        v => v.to_boolean(),
    };
    // Each option's name, kind, short letter and whether it may repeat.
    let mut known: Vec<(String, bool, Option<String>, bool)> = Vec::new();
    if let Some(options) = options.as_object() {
        for key in options.own_property_keys(context)? {
            let name = key.to_string();
            let spec = options.get(key, context)?;
            let is_string = crate::print(&get(&spec, "type", context)?) == "string";
            let short = get(&spec, "short", context)?;
            let short = short.is_string().then(|| crate::print(&short));
            let multiple = get(&spec, "multiple", context)?.to_boolean();
            known.push((name, is_string, short, multiple));
        }
    }
    let values = JsObject::with_null_proto();
    let positionals = JsArray::new(context)?;
    let mut at = 0;
    let store =
        |name: &str, value: JsValue, multiple: bool, context: &mut Context| -> JsResult<()> {
            if multiple {
                let existing = values.get(JsString::from(name), context)?;
                let list = match existing
                    .as_object()
                    .and_then(|e| JsArray::from_object(e.clone()).ok())
                {
                    Some(list) => list,
                    None => JsArray::new(context)?,
                };
                list.push(value, context)?;
                values.set(JsString::from(name), list, false, context)?;
            } else {
                values.set(JsString::from(name), value, false, context)?;
            }
            Ok(())
        };
    while at < list.len() {
        let item = list[at].clone();
        at += 1;
        if item == "--" {
            for rest in &list[at..] {
                positionals.push(js_text(rest), context)?;
            }
            break;
        }
        let (name, inline) = if let Some(long) = item.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                None => (long.to_owned(), None),
            }
        } else if item.len() > 1 && item.starts_with('-') {
            let letter = item[1..2].to_owned();
            let name = known
                .iter()
                .find(|k| k.2.as_deref() == Some(&letter))
                .map_or(letter, |k| k.0.clone());
            let rest = &item[2..];
            (name, (!rest.is_empty()).then(|| rest.to_owned()))
        } else {
            if strict && !allow_positionals {
                return Err(coded_error(
                    context,
                    &format!(
                        "Unexpected argument '{item}'. This command does not take positional arguments"
                    ),
                    "ERR_PARSE_ARGS_UNEXPECTED_POSITIONAL",
                ));
            }
            positionals.push(js_text(&item), context)?;
            continue;
        };
        match known.iter().find(|k| k.0 == name).cloned() {
            Some((_, true, _, multiple)) => {
                let value = match inline {
                    Some(v) => v,
                    None if at < list.len() => {
                        at += 1;
                        list[at - 1].clone()
                    }
                    None => {
                        return Err(coded_error(
                            context,
                            &format!("Option '--{name} <value>' argument missing"),
                            "ERR_PARSE_ARGS_INVALID_OPTION_VALUE",
                        ));
                    }
                };
                store(&name, js_text(&value), multiple, context)?;
            }
            Some((_, false, _, multiple)) => store(&name, true.into(), multiple, context)?,
            None if strict => {
                return Err(coded_error(
                    context,
                    &format!("Unknown option '--{name}'"),
                    "ERR_PARSE_ARGS_UNKNOWN_OPTION",
                ));
            }
            None => store(
                &name,
                inline.map_or(JsValue::from(true), |v| js_text(&v)),
                false,
                context,
            )?,
        }
    }
    let out = object(context);
    out.set(js_string!("values"), values, false, context)?;
    out.set(js_string!("positionals"), positionals, false, context)?;
    Ok(out.into())
}

// ─── util.types ──────────────────────────────────────────────────────────────────────────────

fn typed_kind(value: &JsValue) -> Option<String> {
    let typed = JsTypedArray::from_object(value.as_object()?).ok()?;
    typed.kind().map(|k| format!("{k:?}Array"))
}

macro_rules! typed_check {
    ($name:literal, $kind:literal) => {
        ($name, 1, |_, args, _| {
            Ok(typed_kind(&arg(args, 0)).is_some_and(|k| k == $kind).into())
        })
    };
}

fn is_object_of<T>(value: &JsValue, check: fn(JsObject) -> Option<T>) -> bool {
    value.as_object().and_then(check).is_some()
}

const TYPES: &[(&str, usize, NativeFunctionPointer)] = &[
    ("isAnyArrayBuffer", 1, |_, args, _| {
        let v = arg(args, 0);
        Ok((is_object_of(&v, |o| JsArrayBuffer::from_object(o).ok())
            || is_object_of(&v, |o| JsSharedArrayBuffer::from_object(o).ok()))
        .into())
    }),
    ("isArrayBuffer", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsArrayBuffer::from_object(o).ok()).into())
    }),
    ("isSharedArrayBuffer", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsSharedArrayBuffer::from_object(o).ok()).into())
    }),
    ("isArrayBufferView", 1, |_, args, _| {
        let v = arg(args, 0);
        Ok((is_object_of(&v, |o| JsTypedArray::from_object(o).ok())
            || is_object_of(&v, |o| JsDataView::from_object(o).ok()))
        .into())
    }),
    ("isTypedArray", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsTypedArray::from_object(o).ok()).into())
    }),
    ("isDataView", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsDataView::from_object(o).ok()).into())
    }),
    typed_check!("isUint8Array", "Uint8Array"),
    typed_check!("isUint8ClampedArray", "Uint8ClampedArray"),
    typed_check!("isUint16Array", "Uint16Array"),
    typed_check!("isUint32Array", "Uint32Array"),
    typed_check!("isInt8Array", "Int8Array"),
    typed_check!("isInt16Array", "Int16Array"),
    typed_check!("isInt32Array", "Int32Array"),
    typed_check!("isFloat16Array", "Float16Array"),
    typed_check!("isFloat32Array", "Float32Array"),
    typed_check!("isFloat64Array", "Float64Array"),
    typed_check!("isBigInt64Array", "BigInt64Array"),
    typed_check!("isBigUint64Array", "BigUint64Array"),
    ("isAsyncFunction", 1, |_, args, cx| {
        let Some(f) = arg(args, 0).as_object().filter(|o| o.is_callable()) else {
            return Ok(false.into());
        };
        Ok(function_kind(&f, cx)?.starts_with("Async").into())
    }),
    ("isGeneratorFunction", 1, |_, args, cx| {
        let Some(f) = arg(args, 0).as_object().filter(|o| o.is_callable()) else {
            return Ok(false.into());
        };
        Ok(function_kind(&f, cx)?.ends_with("GeneratorFunction").into())
    }),
    ("isGeneratorObject", 1, |_, args, cx| {
        let v = arg(args, 0);
        if is_object_of(&v, |o| JsGenerator::from_object(o).ok()) {
            return Ok(true.into());
        }
        Ok((v.is_object() && object_tag(&v, cx)? == "[object AsyncGenerator]").into())
    }),
    ("isMap", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsMap::from_object(o).ok()).into())
    }),
    ("isSet", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsSet::from_object(o).ok()).into())
    }),
    ("isWeakMap", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsWeakMap::from_object(o).ok()).into())
    }),
    ("isWeakSet", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsWeakSet::from_object(o).ok()).into())
    }),
    ("isMapIterator", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsMapIterator::from_object(o).ok()).into())
    }),
    ("isSetIterator", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsSetIterator::from_object(o).ok()).into())
    }),
    ("isDate", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsDate::from_object(o).ok()).into())
    }),
    ("isRegExp", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsRegExp::from_object(o).ok()).into())
    }),
    ("isPromise", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsPromise::from_object(o).ok()).into())
    }),
    ("isProxy", 1, |_, args, _| {
        Ok(is_object_of(&arg(args, 0), |o| JsProxy::from_object(o).ok()).into())
    }),
    ("isNativeError", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.is::<boa_engine::builtins::error::Error>())
            .into())
    }),
    ("isBoxedPrimitive", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| boxed(&o).is_some())
            .into())
    }),
    ("isNumberObject", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.downcast_ref::<f64>().is_some())
            .into())
    }),
    ("isStringObject", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.downcast_ref::<JsString>().is_some())
            .into())
    }),
    ("isBooleanObject", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.downcast_ref::<bool>().is_some())
            .into())
    }),
    ("isSymbolObject", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.downcast_ref::<JsSymbol>().is_some())
            .into())
    }),
    ("isBigIntObject", 1, |_, args, _| {
        Ok(arg(args, 0)
            .as_object()
            .is_some_and(|o| o.downcast_ref::<boa_engine::JsBigInt>().is_some())
            .into())
    }),
    ("isArgumentsObject", 1, |_, args, cx| {
        let v = arg(args, 0);
        Ok((v.is_object() && object_tag(&v, cx)? == "[object Arguments]").into())
    }),
    ("isModuleNamespaceObject", 1, |_, args, cx| {
        let v = arg(args, 0);
        Ok((v.is_object() && object_tag(&v, cx)? == "[object Module]").into())
    }),
    ("isExternal", 1, |_, _, _| Ok(false.into())),
    ("isKeyObject", 1, |_, _, _| Ok(false.into())),
    ("isCryptoKey", 1, |_, args, cx| {
        let v = arg(args, 0);
        Ok((v.is_object() && object_tag(&v, cx)? == "[object CryptoKey]").into())
    }),
    ("isWeakRef", 1, |_, args, cx| {
        let v = arg(args, 0);
        Ok((v.is_object() && object_tag(&v, cx)? == "[object WeakRef]").into())
    }),
];

/// `require('util/types')`.
pub(crate) fn types_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let util = crate::modules::node_module(context, "util")?;
    get(&util, "types", context)
}

fn make_types(context: &mut Context) -> JsResult<JsObject> {
    let types = object(context);
    exports(context, &types, TYPES)?;
    Ok(types)
}

/// `require('util')`.
pub(crate) fn util_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let util = object(context);
    exports(context, &util, UTIL_FUNCTIONS)?;
    let types = make_types(context)?;
    util.set(js_string!("types"), types, false, context)?;
    for name in ["TextEncoder", "TextDecoder"] {
        let class = crate::native::global(context, name)?;
        util.set(JsString::from(name), class, false, context)?;
    }
    // `inspect.custom`, `inspect.defaultOptions`, `inspect.colors`, `inspect.styles`.
    let inspect_fn = util.get(js_string!("inspect"), context)?;
    let inspect_obj = inspect_fn.as_object().expect("a function");
    let custom = custom_symbol(context)?;
    inspect_obj.set(js_string!("custom"), custom, false, context)?;
    let defaults = object(context);
    let shown = Options::default().to_object(context)?;
    for key in [
        "showHidden",
        "depth",
        "colors",
        "customInspect",
        "showProxy",
        "maxArrayLength",
        "maxStringLength",
        "breakLength",
        "compact",
        "sorted",
        "getters",
        "numericSeparator",
    ] {
        let value = shown.get(JsString::from(key), context)?;
        defaults.set(JsString::from(key), value, false, context)?;
    }
    inspect_obj.set(js_string!("defaultOptions"), defaults, false, context)?;
    let colors = object(context);
    for (name, open, close) in ANSI {
        let pair = array(context, vec![(*open).into(), (*close).into()]);
        colors.set(JsString::from(*name), pair, false, context)?;
    }
    inspect_obj.set(js_string!("colors"), colors, false, context)?;
    let styles = object(context);
    for (style, colour) in [
        ("special", "cyan"),
        ("number", "yellow"),
        ("bigint", "yellow"),
        ("boolean", "yellow"),
        ("undefined", "grey"),
        ("null", "bold"),
        ("string", "green"),
        ("symbol", "green"),
        ("date", "magenta"),
        ("regexp", "red"),
        ("module", "underline"),
    ] {
        styles.set(JsString::from(style), js_text(colour), false, context)?;
    }
    inspect_obj.set(js_string!("styles"), styles, false, context)?;
    // `promisify.custom`.
    let promisify_fn = util.get(js_string!("promisify"), context)?;
    let promisify_custom = symbol_for(context, "nodejs.util.promisify.custom")?;
    promisify_fn.as_object().expect("a function").set(
        js_string!("custom"),
        promisify_custom,
        false,
        context,
    )?;
    // `debug` is `debuglog`'s other name.
    let debuglog_fn = util.get(js_string!("debuglog"), context)?;
    util.set(js_string!("debug"), debuglog_fn, false, context)?;
    mime(context, &util)?;
    Ok(util.into())
}

// ─── MIMEType ────────────────────────────────────────────────────────────────────────────────

/// `MIMEType` and `MIMEParams`: a type, its subtype and its parameters, read from text and
/// written back.
fn mime(context: &mut Context, util: &JsObject) -> JsResult<()> {
    let (params_class, _) = crate::native::class(
        context,
        crate::native::ClassSpec {
            name: "MIMEParams",
            init: |this, _, cx| {
                let pairs = JsArray::new(cx)?;
                crate::native::hidden(this, js_string!("__pairs"), pairs, cx)
            },
            methods: &[
                ("get", 1, |this, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
                    for pair in mime_pairs(this, cx)? {
                        if pair.0 == name {
                            return Ok(js_text(&pair.1));
                        }
                    }
                    Ok(JsValue::null())
                }),
                ("has", 1, |this, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
                    Ok(mime_pairs(this, cx)?.iter().any(|p| p.0 == name).into())
                }),
                ("set", 2, |this, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
                    let value = text_of(&arg(args, 1), cx)?;
                    let mut pairs = mime_pairs(this, cx)?;
                    match pairs.iter_mut().find(|p| p.0 == name) {
                        Some(pair) => pair.1 = value,
                        None => pairs.push((name, value)),
                    }
                    set_mime_pairs(this, pairs, cx)?;
                    Ok(JsValue::undefined())
                }),
                ("delete", 1, |this, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
                    let pairs = mime_pairs(this, cx)?
                        .into_iter()
                        .filter(|p| p.0 != name)
                        .collect();
                    set_mime_pairs(this, pairs, cx)?;
                    Ok(JsValue::undefined())
                }),
                ("toString", 0, |this, _, cx| {
                    Ok(js_text(&mime_params_text(&mime_pairs(this, cx)?)))
                }),
                ("toJSON", 0, |this, _, cx| {
                    Ok(js_text(&mime_params_text(&mime_pairs(this, cx)?)))
                }),
                ("keys", 0, |this, _, cx| {
                    let keys: Vec<JsValue> = mime_pairs(this, cx)?
                        .iter()
                        .map(|p| js_text(&p.0))
                        .collect();
                    let list = array(cx, keys);
                    invoke(&list, "values", &[], cx)
                }),
                ("values", 0, |this, _, cx| {
                    let values: Vec<JsValue> = mime_pairs(this, cx)?
                        .iter()
                        .map(|p| js_text(&p.1))
                        .collect();
                    let list = array(cx, values);
                    invoke(&list, "values", &[], cx)
                }),
                ("entries", 0, |this, _, cx| {
                    let mut entries = Vec::new();
                    for (k, v) in mime_pairs(this, cx)? {
                        entries.push(array(cx, vec![js_text(&k), js_text(&v)]));
                    }
                    let list = array(cx, entries);
                    invoke(&list, "values", &[], cx)
                }),
            ],
            ..Default::default()
        },
    )?;
    let iterator_key = JsSymbol::iterator();
    let prototype = params_class.get(js_string!("prototype"), context)?;
    let entries = get(&prototype, "entries", context)?;
    if let Some(prototype) = prototype.as_object() {
        crate::native::hidden(&prototype, iterator_key, entries, context)?;
    }
    util.set(
        js_string!("MIMEParams"),
        params_class.clone(),
        false,
        context,
    )?;
    let (mime_class, _) = crate::native::class(
        context,
        crate::native::ClassSpec {
            name: "MIMEType",
            length: 1,
            init: |this, args, cx| {
                let text = text_of(&arg(args, 0), cx)?;
                let (essence, rest) = text.split_once(';').unwrap_or((text.as_str(), ""));
                let Some((kind, subtype)) = essence.trim().split_once('/') else {
                    return Err(coded_error(
                        cx,
                        &format!("The MIME syntax for a type in \"{text}\" is invalid"),
                        "ERR_INVALID_MIME_SYNTAX",
                    ));
                };
                if kind.is_empty() || subtype.is_empty() {
                    return Err(coded_error(
                        cx,
                        &format!("The MIME syntax for a subtype in \"{text}\" is invalid"),
                        "ERR_INVALID_MIME_SYNTAX",
                    ));
                }
                crate::native::hidden(
                    this,
                    js_string!("__type"),
                    js_text(&kind.trim().to_ascii_lowercase()),
                    cx,
                )?;
                crate::native::hidden(
                    this,
                    js_string!("__subtype"),
                    js_text(&subtype.trim().to_ascii_lowercase()),
                    cx,
                )?;
                let util = crate::modules::node_module(cx, "util")?;
                let params_class = get(&util, "MIMEParams", cx)?;
                let params = params_class
                    .as_object()
                    .expect("a class")
                    .construct(&[], None, cx)?;
                let mut pairs = Vec::new();
                for part in rest.split(';') {
                    if let Some((k, v)) = part.split_once('=') {
                        let v = v.trim();
                        let v = v
                            .strip_prefix('"')
                            .and_then(|v| v.strip_suffix('"'))
                            .unwrap_or(v);
                        pairs.push((k.trim().to_ascii_lowercase(), v.to_owned()));
                    }
                }
                set_mime_pairs(&params.clone().into(), pairs, cx)?;
                crate::native::hidden(this, js_string!("params"), params, cx)?;
                Ok(())
            },
            accessors: &[
                (
                    "type",
                    |this, _, cx| get(this, "__type", cx),
                    Some(|this, args, cx| {
                        if let Some(o) = this.as_object() {
                            let v = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
                            crate::native::hidden(&o, js_string!("__type"), js_text(&v), cx)?;
                        }
                        Ok(JsValue::undefined())
                    }),
                ),
                (
                    "subtype",
                    |this, _, cx| get(this, "__subtype", cx),
                    Some(|this, args, cx| {
                        if let Some(o) = this.as_object() {
                            let v = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
                            crate::native::hidden(&o, js_string!("__subtype"), js_text(&v), cx)?;
                        }
                        Ok(JsValue::undefined())
                    }),
                ),
                (
                    "essence",
                    |this, _, cx| {
                        let kind = crate::print(&get(this, "__type", cx)?);
                        let subtype = crate::print(&get(this, "__subtype", cx)?);
                        Ok(js_text(&format!("{kind}/{subtype}")))
                    },
                    None,
                ),
            ],
            methods: &[("toString", 0, mime_text), ("toJSON", 0, mime_text)],
            ..Default::default()
        },
    )?;
    util.set(js_string!("MIMEType"), mime_class, false, context)?;
    Ok(())
}

fn mime_text(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = crate::print(&get(this, "__type", context)?);
    let subtype = crate::print(&get(this, "__subtype", context)?);
    let params = get(this, "params", context)?;
    let pairs = match params.as_object() {
        Some(p) => mime_pairs(&p.into(), context)?,
        None => Vec::new(),
    };
    let rest = mime_params_text(&pairs);
    Ok(js_text(&if rest.is_empty() {
        format!("{kind}/{subtype}")
    } else {
        format!("{kind}/{subtype};{rest}")
    }))
}

fn mime_pairs(this: &JsValue, context: &mut Context) -> JsResult<Vec<(String, String)>> {
    let pairs = get(this, "__pairs", context)?;
    let mut out = Vec::new();
    for pair in crate::native::list_of(&pairs, context)? {
        let pair = crate::native::list_of(&pair, context)?;
        out.push((crate::print(&pair[0]), crate::print(&pair[1])));
    }
    Ok(out)
}

fn set_mime_pairs(
    this: &JsValue,
    pairs: Vec<(String, String)>,
    context: &mut Context,
) -> JsResult<()> {
    let Some(this) = this.as_object() else {
        return Err(type_error("not MIMEParams"));
    };
    let mut list = Vec::new();
    for (k, v) in pairs {
        list.push(array(context, vec![js_text(&k), js_text(&v)]));
    }
    let list = array(context, list);
    crate::native::hidden(&this, js_string!("__pairs"), list, context)
}

fn mime_params_text(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            let plain = !v.is_empty()
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c));
            if plain {
                format!("{k}={v}")
            } else {
                format!("{k}=\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}
