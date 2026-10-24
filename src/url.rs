//! `URL` and `URLSearchParams`, as the WHATWG URL standard has them — parsed by the `url`
//! crate, which implements that standard.
//!
//! A URL's `searchParams` is its own `URLSearchParams`, kept in step both ways: setting
//! `url.search` changes the params, and changing the params changes `url.search`.

use std::cell::RefCell;

use boa_engine::{
    Context, Finalize, JsData, JsNativeError, JsObject, JsResult, JsString, JsValue,
    NativeFunction, Trace, js_string,
    native_function::NativeFunctionPointer,
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor, PropertyKey},
};

#[derive(Trace, Finalize, JsData)]
struct UrlData {
    #[unsafe_ignore_trace]
    url: RefCell<url::Url>,
    /// Its `searchParams`, made once.
    params: boa_gc::GcRefCell<Option<JsObject>>,
}

#[derive(Trace, Finalize, JsData)]
struct ParamsData {
    #[unsafe_ignore_trace]
    pairs: RefCell<Vec<(String, String)>>,
    /// The URL these are the query of, if they are one's.
    owner: boa_gc::GcRefCell<Option<JsObject>>,
}

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn text_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

fn type_error(message: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

fn js_text(text: &str) -> JsValue {
    JsString::from(text).into()
}

fn function(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
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

/// The prototype `new_target` names, or `fallback`'s.
fn prototype_of(new_target: &JsValue, fallback: &str, context: &mut Context) -> JsResult<JsObject> {
    let from = match new_target.as_object() {
        Some(target) => target.get(js_string!("prototype"), context)?,
        None => context
            .global_object()
            .get(JsString::from(fallback), context)?
            .as_object()
            .map(|c| c.get(js_string!("prototype"), context))
            .transpose()?
            .unwrap_or_default(),
    };
    Ok(from
        .as_object()
        .unwrap_or_else(|| context.intrinsics().constructors().object().prototype()))
}

// ─── URL ────────────────────────────────────────────────────────────────────────────────────

fn parse(input: &str, base: Option<&str>) -> Result<url::Url, String> {
    let parsed = match base {
        Some(base) => url::Url::parse(base).and_then(|base| base.join(input)),
        None => url::Url::parse(input),
    };
    parsed.map_err(|_| format!("Invalid URL: {input}"))
}

fn construct_url(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if new_target.is_undefined() {
        return Err(type_error(
            "Class constructor URL cannot be invoked without 'new'",
        ));
    }
    let input = text_of(&arg(args, 0), context)?;
    let base = match arg(args, 1) {
        b if b.is_undefined() => None,
        b => Some(text_of(&b, context)?),
    };
    let url = parse(&input, base.as_deref()).map_err(type_error)?;
    let prototype = prototype_of(new_target, "URL", context)?;
    let object = JsObject::from_proto_and_data(
        prototype,
        UrlData {
            url: RefCell::new(url),
            params: boa_gc::GcRefCell::new(None),
        },
    );
    Ok(object.into())
}

fn with_url<R>(this: &JsValue, f: impl FnOnce(&url::Url) -> R) -> JsResult<R> {
    let object = this.as_object().ok_or_else(|| type_error("not a URL"))?;
    let data = object
        .downcast_ref::<UrlData>()
        .ok_or_else(|| type_error("not a URL"))?;
    Ok(f(&data.url.borrow()))
}

/// Changes the URL, and — when its query may have changed — the pairs of its `searchParams`.
fn change_url(this: &JsValue, f: impl FnOnce(&mut url::Url)) -> JsResult<()> {
    let object = this.as_object().ok_or_else(|| type_error("not a URL"))?;
    let params = {
        let data = object
            .downcast_ref::<UrlData>()
            .ok_or_else(|| type_error("not a URL"))?;
        f(&mut data.url.borrow_mut());
        data.params.borrow().clone()
    };
    if let Some(params) = params
        && let Some(params) = params.downcast_ref::<ParamsData>()
    {
        let pairs = with_url(this, |u| u.query_pairs().into_owned().collect())?;
        *params.pairs.borrow_mut() = pairs;
    }
    Ok(())
}

type Getter = fn(&url::Url) -> String;
type Setter = fn(&mut url::Url, &str);

/// What a URL part reads as, by the standard's rules (`search` is `?…` or nothing, `port` is
/// empty when it is the scheme's own).
const PARTS: &[(&str, Getter, Option<Setter>)] = &[
    ("href", |u| u.as_str().to_owned(), None),
    ("origin", |u| u.origin().ascii_serialization(), None),
    (
        "protocol",
        |u| format!("{}:", u.scheme()),
        Some(|u, v| {
            let _ = u.set_scheme(v.trim_end_matches(':'));
        }),
    ),
    (
        "username",
        |u| u.username().to_owned(),
        Some(|u, v| {
            let _ = u.set_username(v);
        }),
    ),
    (
        "password",
        |u| u.password().unwrap_or("").to_owned(),
        Some(|u, v| {
            let _ = u.set_password(Some(v).filter(|v| !v.is_empty()));
        }),
    ),
    (
        "host",
        |u| match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => String::new(),
        },
        Some(|u, v| {
            let (host, port) = match v.rsplit_once(':') {
                Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse().ok()),
                _ => (v, None),
            };
            if u.set_host(Some(host)).is_ok() {
                let _ = u.set_port(port);
            }
        }),
    ),
    (
        "hostname",
        |u| u.host_str().unwrap_or("").to_owned(),
        Some(|u, v| {
            let _ = u.set_host(Some(v));
        }),
    ),
    (
        "port",
        |u| u.port().map(|p| p.to_string()).unwrap_or_default(),
        Some(|u, v| {
            let _ = u.set_port(if v.is_empty() { None } else { v.parse().ok() });
        }),
    ),
    (
        "pathname",
        |u| u.path().to_owned(),
        Some(|u, v| u.set_path(v)),
    ),
    (
        "search",
        |u| match u.query() {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        },
        Some(|u, v| {
            let v = v.strip_prefix('?').unwrap_or(v);
            u.set_query(if v.is_empty() { None } else { Some(v) });
        }),
    ),
    (
        "hash",
        |u| match u.fragment() {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        },
        Some(|u, v| {
            let v = v.strip_prefix('#').unwrap_or(v);
            u.set_fragment(if v.is_empty() { None } else { Some(v) });
        }),
    ),
];

fn url_part_get(this: &JsValue, name: &str) -> JsResult<JsValue> {
    let (_, get, _) = PARTS.iter().find(|(n, ..)| *n == name).expect("a URL part");
    with_url(this, |u| js_text(&get(u)))
}

fn url_part_set(this: &JsValue, name: &str, value: &str) -> JsResult<()> {
    if name == "href" {
        let url = parse(value, None).map_err(type_error)?;
        return change_url(this, |u| *u = url);
    }
    let (_, _, set) = PARTS.iter().find(|(n, ..)| *n == name).expect("a URL part");
    if let Some(set) = set {
        change_url(this, |u| set(u, value))?;
    }
    Ok(())
}

macro_rules! part_accessors {
    ($($name:literal),*) => {
        &[$(($name,
            (|this, _, _| url_part_get(this, $name)) as NativeFunctionPointer,
            (|this, args, cx| {
                let value = text_of(&arg(args, 0), cx)?;
                url_part_set(this, $name, &value)?;
                Ok(JsValue::undefined())
            }) as NativeFunctionPointer,
        )),*]
    };
}

const URL_ACCESSORS: &[(&str, NativeFunctionPointer, NativeFunctionPointer)] = part_accessors!(
    "href", "origin", "protocol", "username", "password", "host", "hostname", "port", "pathname",
    "search", "hash"
);

fn search_params(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = this.as_object().ok_or_else(|| type_error("not a URL"))?;
    let existing = object
        .downcast_ref::<UrlData>()
        .ok_or_else(|| type_error("not a URL"))?
        .params
        .borrow()
        .clone();
    if let Some(params) = existing {
        return Ok(params.into());
    }
    let pairs = with_url(this, |u| u.query_pairs().into_owned().collect())?;
    let prototype = prototype_of(&JsValue::undefined(), "URLSearchParams", context)?;
    let params = JsObject::from_proto_and_data(
        prototype,
        ParamsData {
            pairs: RefCell::new(pairs),
            owner: boa_gc::GcRefCell::new(Some(object.clone())),
        },
    );
    if let Some(data) = object.downcast_ref::<UrlData>() {
        *data.params.borrow_mut() = Some(params.clone());
    }
    Ok(params.into())
}

const URL_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("toString", 0, |this, _, _| url_part_get(this, "href")),
    ("toJSON", 0, |this, _, _| url_part_get(this, "href")),
];

const URL_STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("canParse", 2, |_, args, cx| {
        let input = text_of(&arg(args, 0), cx)?;
        let base = match arg(args, 1) {
            b if b.is_undefined() => None,
            b => Some(text_of(&b, cx)?),
        };
        Ok(parse(&input, base.as_deref()).is_ok().into())
    }),
    // A Blob's URL: a name for it, which nothing here fetches.
    // A blob's address: what it stands for is kept, for a worker made from it.
    ("createObjectURL", 1, |_, args, cx| {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let url = format!("blob:app://su/{n:016x}");
        let blob = arg(args, 0);
        let bytes = match blob.as_object() {
            Some(b) => crate::encoding::bytes_of(&b.get(js_string!("__bytes"), cx)?, cx)?,
            None => None,
        };
        if let Some(bytes) = bytes {
            crate::shared(cx)
                .blobs
                .borrow_mut()
                .insert(url.clone(), bytes);
        }
        Ok(js_text(&url))
    }),
    ("revokeObjectURL", 1, |_, args, cx| {
        let url = text_of(&arg(args, 0), cx)?;
        crate::shared(cx).blobs.borrow_mut().remove(&url);
        Ok(JsValue::undefined())
    }),
];

// ─── URLSearchParams ────────────────────────────────────────────────────────────────────────

fn construct_params(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if new_target.is_undefined() {
        return Err(type_error(
            "Class constructor URLSearchParams cannot be invoked without 'new'",
        ));
    }
    let init = arg(args, 0);
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(object) = init.as_object().filter(|_| !init.is_string()) {
        if let Some(other) = object.downcast_ref::<ParamsData>() {
            pairs = other.pairs.borrow().clone();
        } else if let Ok(list) = JsArray::from_object(object.clone()) {
            for at in 0..list.length(context)? {
                let pair = list.get(at, context)?;
                let pair = pair
                    .as_object()
                    .and_then(|p| JsArray::from_object(p.clone()).ok())
                    .ok_or_else(|| {
                        type_error("URLSearchParams: each pair must be [name, value]")
                    })?;
                let name = text_of(&pair.get(0, context)?, context)?;
                let value = text_of(&pair.get(1, context)?, context)?;
                pairs.push((name, value));
            }
        } else {
            for key in object.own_property_keys(context)? {
                let PropertyKey::String(name) = &key else {
                    continue;
                };
                let value = object.get(key.clone(), context)?;
                pairs.push((name.to_std_string_escaped(), text_of(&value, context)?));
            }
        }
    } else if !init.is_undefined() {
        let text = text_of(&init, context)?;
        let text = text.strip_prefix('?').unwrap_or(&text);
        pairs = form_urlencoded::parse(text.as_bytes())
            .into_owned()
            .collect();
    }
    let prototype = prototype_of(new_target, "URLSearchParams", context)?;
    Ok(JsObject::from_proto_and_data(
        prototype,
        ParamsData {
            pairs: RefCell::new(pairs),
            owner: boa_gc::GcRefCell::new(None),
        },
    )
    .into())
}

fn serialize(pairs: &[(String, String)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .finish()
}

fn params(this: &JsValue) -> JsResult<JsObject> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("not a URLSearchParams"))?;
    if object.downcast_ref::<ParamsData>().is_none() {
        return Err(type_error("not a URLSearchParams"));
    }
    Ok(object.clone())
}

fn read_pairs(this: &JsValue) -> JsResult<Vec<(String, String)>> {
    let object = params(this)?;
    let data = object.downcast_ref::<ParamsData>().expect("checked");
    Ok(data.pairs.borrow().clone())
}

/// Changes the pairs, and the query of the URL they belong to.
fn change_pairs(this: &JsValue, f: impl FnOnce(&mut Vec<(String, String)>)) -> JsResult<()> {
    let object = params(this)?;
    let (query, owner) = {
        let data = object.downcast_ref::<ParamsData>().expect("checked");
        f(&mut data.pairs.borrow_mut());
        (serialize(&data.pairs.borrow()), data.owner.borrow().clone())
    };
    if let Some(owner) = owner
        && let Some(url) = owner.downcast_ref::<UrlData>()
    {
        url.url
            .borrow_mut()
            .set_query(if query.is_empty() { None } else { Some(&query) });
    }
    Ok(())
}

fn pair_list(context: &mut Context, pairs: Vec<(String, String)>, which: u8) -> JsValue {
    let values: Vec<JsValue> = pairs
        .into_iter()
        .map(|(k, v)| match which {
            0 => js_text(&k),
            1 => js_text(&v),
            _ => JsArray::from_iter([js_text(&k), js_text(&v)], context).into(),
        })
        .collect();
    let list = JsArray::from_iter(values, context);
    // An iterator over them, as `entries()` and the rest answer.
    let values = list.values(context);
    values.unwrap_or_else(|_| list.into())
}

const PARAMS_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("append", 2, |this, args, cx| {
        let (name, value) = (text_of(&arg(args, 0), cx)?, text_of(&arg(args, 1), cx)?);
        change_pairs(this, |p| p.push((name, value)))?;
        Ok(JsValue::undefined())
    }),
    ("set", 2, |this, args, cx| {
        let (name, value) = (text_of(&arg(args, 0), cx)?, text_of(&arg(args, 1), cx)?);
        change_pairs(this, |p| match p.iter().position(|(k, _)| *k == name) {
            Some(first) => {
                p[first].1 = value;
                let mut at = 0;
                p.retain(|(k, _)| {
                    at += 1;
                    at - 1 == first || *k != name
                });
            }
            None => p.push((name, value)),
        })?;
        Ok(JsValue::undefined())
    }),
    ("delete", 2, |this, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        let value = match arg(args, 1) {
            v if v.is_undefined() => None,
            v => Some(text_of(&v, cx)?),
        };
        change_pairs(this, |p| {
            p.retain(|(k, v)| !(*k == name && value.as_ref().is_none_or(|want| want == v)))
        })?;
        Ok(JsValue::undefined())
    }),
    ("get", 1, |this, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        Ok(read_pairs(this)?
            .into_iter()
            .find(|(k, _)| *k == name)
            .map_or(JsValue::null(), |(_, v)| js_text(&v)))
    }),
    ("getAll", 1, |this, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        let values: Vec<JsValue> = read_pairs(this)?
            .into_iter()
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| js_text(&v))
            .collect();
        Ok(JsArray::from_iter(values, cx).into())
    }),
    ("has", 2, |this, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        let value = match arg(args, 1) {
            v if v.is_undefined() => None,
            v => Some(text_of(&v, cx)?),
        };
        Ok(read_pairs(this)?
            .iter()
            .any(|(k, v)| *k == name && value.as_ref().is_none_or(|want| want == v))
            .into())
    }),
    ("sort", 0, |this, _, _| {
        change_pairs(this, |p| {
            p.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()))
        })?;
        Ok(JsValue::undefined())
    }),
    ("toString", 0, |this, _, _| {
        Ok(js_text(&serialize(&read_pairs(this)?)))
    }),
    ("keys", 0, |this, _, cx| {
        Ok(pair_list(cx, read_pairs(this)?, 0))
    }),
    ("values", 0, |this, _, cx| {
        Ok(pair_list(cx, read_pairs(this)?, 1))
    }),
    ("entries", 0, |this, _, cx| {
        Ok(pair_list(cx, read_pairs(this)?, 2))
    }),
    ("forEach", 2, |this, args, cx| {
        let callback = arg(args, 0);
        let callable = callback
            .as_callable()
            .ok_or_else(|| type_error("URLSearchParams.forEach: callback is not a function"))?;
        for (k, v) in read_pairs(this)? {
            callable.call(&arg(args, 1), &[js_text(&v), js_text(&k), this.clone()], cx)?;
        }
        Ok(JsValue::undefined())
    }),
];

// ─── Installing them ────────────────────────────────────────────────────────────────────────

fn define_method(
    object: &JsObject,
    name: &str,
    method: JsObject,
    context: &mut Context,
) -> JsResult<()> {
    object.define_property_or_throw(
        JsString::from(name),
        PropertyDescriptor::builder()
            .value(method)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    Ok(())
}

fn class(
    context: &mut Context,
    name: &str,
    construct: NativeFunctionPointer,
    methods: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<(JsObject, JsObject)> {
    let class = constructor(context, name, 1, construct);
    let prototype = JsObject::with_object_proto(context.intrinsics());
    for (method_name, length, f) in methods {
        let method = function(context, method_name, *length, *f);
        define_method(&prototype, method_name, method, context)?;
    }
    define_method(&prototype, "constructor", class.clone(), context)?;
    class.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    context.register_global_property(
        JsString::from(name),
        class.clone(),
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok((class, prototype))
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let (url_class, url_proto) = class(context, "URL", construct_url, URL_METHODS)?;
    for (name, get, set) in URL_ACCESSORS {
        let getter = function(context, &format!("get {name}"), 0, *get);
        let setter = (!matches!(*name, "origin"))
            .then(|| function(context, &format!("set {name}"), 1, *set));
        let mut descriptor = PropertyDescriptor::builder()
            .get(getter)
            .enumerable(true)
            .configurable(true);
        if let Some(setter) = setter {
            descriptor = descriptor.set(setter);
        }
        url_proto.define_property_or_throw(JsString::from(*name), descriptor, context)?;
    }
    let getter = function(context, "get searchParams", 0, search_params);
    url_proto.define_property_or_throw(
        js_string!("searchParams"),
        PropertyDescriptor::builder()
            .get(getter)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    for (name, length, f) in URL_STATICS {
        let method = function(context, name, *length, *f);
        url_class.set(JsString::from(*name), method, false, context)?;
    }
    let (_, params_proto) = class(context, "URLSearchParams", construct_params, PARAMS_METHODS)?;
    // `for (const [k, v] of params)`, and `params.size`.
    let entries = params_proto.get(js_string!("entries"), context)?;
    params_proto.define_property_or_throw(
        boa_engine::JsSymbol::iterator(),
        PropertyDescriptor::builder()
            .value(entries)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    let size = function(context, "get size", 0, |this, _, _| {
        Ok(read_pairs(this)?.len().into())
    });
    params_proto.define_property_or_throw(
        js_string!("size"),
        PropertyDescriptor::builder()
            .get(size)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    Ok(())
}
