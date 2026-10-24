//! The classes `fetch` speaks in — `Headers`, `Request`, `Response` and `FormData` — made
//! without any network: the host owns `fetch` itself (Obsidian's `requestUrl`, the window's own),
//! and bundled libraries build these objects whether or not they send them. An SDK makes a
//! `Request` to sign it, wraps bytes it already has in a `Response` to read them as JSON, posts a
//! `FormData`; a polyfill checks that `Response` exists before choosing its path.
//!
//! A body is kept as bytes until something asks for it as a stream (`response.body`), and from
//! then on is that `ReadableStream`; `text()`, `json()`, `arrayBuffer()`, `bytes()`, `blob()` and
//! `formData()` read either, once. `FormData` is sent as `multipart/form-data` and read back from
//! that or from a urlencoded body.

use std::rc::Rc;

use boa_engine::{
    Context, JsData, JsObject, JsResult, JsSymbol, JsValue, builtins::promise::ResolvingFunctions,
    js_string, native_function::NativeFunctionPointer,
};
use boa_gc::{Finalize, Trace};

use crate::native::{arg, array, get, invoke, js_text, list_of, object, text_of, type_error};
use crate::streams::{
    error_value, pending, prototype_for, reject, rejected, resolve, rethrow, symbol_method,
    type_error_value, u8array, web_class, when,
};

// ─── The classes, made once ─────────────────────────────────────────────────────────────────

pub(crate) struct Classes {
    headers: JsObject,
    headers_proto: JsObject,
    request: JsObject,
    request_proto: JsObject,
    response: JsObject,
    response_proto: JsObject,
    form_data: JsObject,
    form_data_proto: JsObject,
}

fn classes(context: &mut Context) -> JsResult<Rc<Classes>> {
    if let Some(classes) = context.get_data::<Rc<Classes>>() {
        return Ok(classes.clone());
    }
    let classes = Rc::new(make_classes(context)?);
    context.insert_data(classes.clone());
    Ok(classes)
}

fn make_classes(context: &mut Context) -> JsResult<Classes> {
    let (headers, headers_proto) = web_class(
        context,
        "Headers",
        0,
        construct_headers,
        HEADERS_METHODS,
        &[],
        &[],
    )?;
    symbol_method(
        context,
        &headers_proto,
        JsSymbol::iterator(),
        "entries",
        |this, _, cx| headers_iterator(this, cx, 2),
    )?;
    let (request, request_proto) = web_class(
        context,
        "Request",
        1,
        construct_request,
        REQUEST_METHODS,
        REQUEST_ACCESSORS,
        &[],
    )?;
    let (response, response_proto) = web_class(
        context,
        "Response",
        0,
        construct_response,
        RESPONSE_METHODS,
        RESPONSE_ACCESSORS,
        RESPONSE_STATICS,
    )?;
    let (form_data, form_data_proto) = web_class(
        context,
        "FormData",
        0,
        construct_form_data,
        FORM_DATA_METHODS,
        &[],
        &[],
    )?;
    symbol_method(
        context,
        &form_data_proto,
        JsSymbol::iterator(),
        "entries",
        |this, _, cx| form_iterator(this, cx, 2),
    )?;
    Ok(Classes {
        headers,
        headers_proto,
        request,
        request_proto,
        response,
        response_proto,
        form_data,
        form_data_proto,
    })
}

// ─── Headers ────────────────────────────────────────────────────────────────────────────────

/// A header list: names in lower case, in the order they were added; `immutable` for a
/// `Response.error()`'s or `Response.redirect()`'s.
#[derive(Trace, Finalize, JsData)]
struct Headers {
    #[unsafe_ignore_trace]
    list: Vec<(String, String)>,
    #[unsafe_ignore_trace]
    immutable: bool,
}

fn headers_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Headers>())
        .ok_or_else(|| type_error("not a Headers"))
}

fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// A header's name, checked and lower-cased.
fn header_name(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let name = text_of(value, context)?;
    if !is_token(&name) {
        return Err(type_error(format!(
            "'{name}' is not a valid HTTP header name"
        )));
    }
    Ok(name.to_ascii_lowercase())
}

/// A header's value, trimmed of white space and checked.
fn header_value(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let text = text_of(value, context)?;
    let text = text.trim_matches([' ', '\t', '\r', '\n']).to_owned();
    if text.contains(['\0', '\r', '\n']) {
        return Err(type_error(format!(
            "'{text}' is not a valid HTTP header value"
        )));
    }
    Ok(text)
}

fn with_headers<R>(headers: &JsObject, f: impl FnOnce(&mut Headers) -> R) -> R {
    f(&mut headers.downcast_mut::<Headers>().expect("a Headers"))
}

fn check_mutable(headers: &JsObject) -> JsResult<()> {
    if with_headers(headers, |h| h.immutable) {
        return Err(type_error("These headers are immutable"));
    }
    Ok(())
}

/// The value of `name`: its values joined by `, `, or `None`.
fn header_get(list: &[(String, String)], name: &str) -> Option<String> {
    let values: Vec<&str> = list
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// The list as iteration sees it: sorted by name, the values of a name joined, `set-cookie`'s
/// kept apart.
fn sorted(list: &[(String, String)]) -> Vec<(String, String)> {
    let mut names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let mut out = Vec::new();
    for name in names {
        if name == "set-cookie" {
            out.extend(list.iter().filter(|(n, _)| n == name).cloned());
        } else if let Some(value) = header_get(list, name) {
            out.push((name.to_owned(), value));
        }
    }
    out
}

/// Fills a header list from what `new Headers(init)` takes: another `Headers`, a list of
/// pairs, or a record.
fn fill_headers(init: &JsValue, context: &mut Context) -> JsResult<Vec<(String, String)>> {
    let mut list = Vec::new();
    if init.is_null_or_undefined() {
        return Ok(list);
    }
    if let Ok(other) = headers_of(init) {
        return Ok(with_headers(&other, |h| h.list.clone()));
    }
    let Some(object) = init.as_object() else {
        return Err(type_error("Headers: the init must be an object"));
    };
    let iterator = object.get(JsSymbol::iterator(), context)?;
    if iterator.is_callable() {
        // Pairs, from an array or any iterable — `new Headers(map)`, `new Headers(entries)`.
        let from = context
            .intrinsics()
            .constructors()
            .array()
            .constructor()
            .get(js_string!("from"), context)?;
        let pairs = crate::call(
            context,
            &from,
            &JsValue::undefined(),
            std::slice::from_ref(init),
        )?;
        for pair in list_of(&pairs, context)? {
            let pair = list_of(&pair, context)?;
            if pair.len() != 2 {
                return Err(type_error(
                    "Headers: each header must be a [name, value] pair",
                ));
            }
            list.push((
                header_name(&pair[0], context)?,
                header_value(&pair[1], context)?,
            ));
        }
        return Ok(list);
    }
    let keys = context
        .intrinsics()
        .constructors()
        .object()
        .constructor()
        .get(js_string!("keys"), context)?;
    let keys = crate::call(
        context,
        &keys,
        &JsValue::undefined(),
        std::slice::from_ref(init),
    )?;
    for key in list_of(&keys, context)? {
        let value = object.get(key.to_property_key(context)?, context)?;
        list.push((header_name(&key, context)?, header_value(&value, context)?));
    }
    Ok(list)
}

/// A `Headers` holding `list`, made from Rust.
fn new_headers(
    context: &mut Context,
    list: Vec<(String, String)>,
    immutable: bool,
) -> JsResult<JsObject> {
    let proto = classes(context)?.headers_proto.clone();
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Headers { list, immutable },
    ))
}

fn construct_headers(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let proto = classes(context)?.headers_proto.clone();
    let proto = prototype_for(this, "Headers", proto, context)?;
    let list = fill_headers(&arg(args, 0), context)?;
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Headers {
            list,
            immutable: false,
        },
    )
    .into())
}

/// An iterator over the headers as iteration sees them: names (`0`), values (`1`) or pairs (`2`).
fn headers_iterator(this: &JsValue, context: &mut Context, which: u8) -> JsResult<JsValue> {
    let headers = headers_of(this)?;
    let list = sorted(&with_headers(&headers, |h| h.list.clone()));
    let items: Vec<JsValue> = list
        .into_iter()
        .map(|(name, value)| match which {
            0 => js_text(&name),
            1 => js_text(&value),
            _ => array(context, vec![js_text(&name), js_text(&value)]),
        })
        .collect();
    let items = array(context, items);
    invoke(&items, "values", &[], context)
}

const HEADERS_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("append", 2, |this, args, cx| {
        let headers = headers_of(this)?;
        let (name, value) = (
            header_name(&arg(args, 0), cx)?,
            header_value(&arg(args, 1), cx)?,
        );
        check_mutable(&headers)?;
        with_headers(&headers, |h| h.list.push((name, value)));
        Ok(JsValue::undefined())
    }),
    ("set", 2, |this, args, cx| {
        let headers = headers_of(this)?;
        let (name, value) = (
            header_name(&arg(args, 0), cx)?,
            header_value(&arg(args, 1), cx)?,
        );
        check_mutable(&headers)?;
        with_headers(&headers, |h| {
            match h.list.iter().position(|(n, _)| *n == name) {
                Some(at) => {
                    h.list[at].1 = value;
                    let mut index = 0;
                    h.list.retain(|(n, _)| {
                        let keep = index <= at || *n != name;
                        index += 1;
                        keep
                    });
                }
                None => h.list.push((name, value)),
            }
        });
        Ok(JsValue::undefined())
    }),
    ("delete", 1, |this, args, cx| {
        let headers = headers_of(this)?;
        let name = header_name(&arg(args, 0), cx)?;
        check_mutable(&headers)?;
        with_headers(&headers, |h| h.list.retain(|(n, _)| *n != name));
        Ok(JsValue::undefined())
    }),
    ("get", 1, |this, args, cx| {
        let headers = headers_of(this)?;
        let name = header_name(&arg(args, 0), cx)?;
        Ok(with_headers(&headers, |h| header_get(&h.list, &name))
            .map_or(JsValue::null(), |v| js_text(&v)))
    }),
    ("getSetCookie", 0, |this, _, cx| {
        let headers = headers_of(this)?;
        let cookies: Vec<JsValue> = with_headers(&headers, |h| {
            h.list
                .iter()
                .filter(|(n, _)| n == "set-cookie")
                .map(|(_, v)| js_text(v))
                .collect()
        });
        Ok(array(cx, cookies))
    }),
    ("has", 1, |this, args, cx| {
        let headers = headers_of(this)?;
        let name = header_name(&arg(args, 0), cx)?;
        Ok(with_headers(&headers, |h| h.list.iter().any(|(n, _)| *n == name)).into())
    }),
    ("forEach", 2, |this, args, cx| {
        let headers = headers_of(this)?;
        let (callback, this_arg) = (arg(args, 0), arg(args, 1));
        let list = sorted(&with_headers(&headers, |h| h.list.clone()));
        for (name, value) in list {
            crate::call(
                cx,
                &callback,
                &this_arg,
                &[js_text(&value), js_text(&name), headers.clone().into()],
            )?;
        }
        Ok(JsValue::undefined())
    }),
    ("keys", 0, |this, _, cx| headers_iterator(this, cx, 0)),
    ("values", 0, |this, _, cx| headers_iterator(this, cx, 1)),
    ("entries", 0, |this, _, cx| headers_iterator(this, cx, 2)),
];

fn headers_list(headers: &JsObject) -> Vec<(String, String)> {
    with_headers(headers, |h| h.list.clone())
}

fn content_type(headers: &JsObject) -> String {
    with_headers(headers, |h| header_get(&h.list, "content-type")).unwrap_or_default()
}

// ─── Bodies ─────────────────────────────────────────────────────────────────────────────────

/// A body: none, bytes not yet asked for as a stream, or a stream.
#[derive(Trace, Finalize, Clone)]
enum Body {
    Empty,
    Bytes(Vec<u8>),
    Stream(JsObject),
}

/// Whether `value` is a `Blob` (or `File`) of this runtime's: its bytes kept with it.
fn is_blob(value: &JsValue, context: &mut Context) -> bool {
    value.as_object().is_some_and(|o| {
        o.has_own_property(js_string!("__bytes"), context)
            .unwrap_or(false)
    })
}

fn is_instance_of_global(value: &JsValue, name: &str, context: &mut Context) -> JsResult<bool> {
    let class = crate::native::global(context, name)?;
    if !value.is_object() || !class.is_callable() {
        return Ok(false);
    }
    value.instance_of(&class, context)
}

/// What a body given to `new Response(body)` or `new Request(url, { body })` is, and the
/// `Content-Type` it implies.
fn extract_body(value: &JsValue, context: &mut Context) -> JsResult<(Body, Option<String>)> {
    if value.is_null_or_undefined() {
        return Ok((Body::Empty, None));
    }
    if value.is_string() {
        return Ok((
            Body::Bytes(text_of(value, context)?.into_bytes()),
            Some("text/plain;charset=UTF-8".into()),
        ));
    }
    if crate::streams::is_readable(value) {
        let stream = value.as_object().expect("a stream");
        if crate::streams::is_locked(&stream) || crate::streams::is_disturbed(&stream) {
            return Err(type_error("The body stream is locked or has been read"));
        }
        return Ok((Body::Stream(stream), None));
    }
    if is_blob(value, context) {
        let bytes = crate::web::blob_bytes(value, context)?;
        let kind = text_of(&get(value, "type", context)?, context)?;
        return Ok((Body::Bytes(bytes), (!kind.is_empty()).then_some(kind)));
    }
    if let Some(form) = value.as_object().filter(|o| o.is::<FormData>()) {
        let (bytes, boundary) = multipart(&form, context)?;
        return Ok((
            Body::Bytes(bytes),
            Some(format!("multipart/form-data; boundary={boundary}")),
        ));
    }
    if is_instance_of_global(value, "URLSearchParams", context)? {
        let text = text_of(value, context)?;
        return Ok((
            Body::Bytes(text.into_bytes()),
            Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
        ));
    }
    if let Some(bytes) = crate::encoding::bytes_of(value, context)? {
        return Ok((Body::Bytes(bytes), None));
    }
    Ok((
        Body::Bytes(text_of(value, context)?.into_bytes()),
        Some("text/plain;charset=UTF-8".into()),
    ))
}

/// The body, whether it was used, and the headers, of a `Request` or a `Response`.
fn with_body<R>(target: &JsObject, f: impl FnOnce(&mut Body, &mut bool) -> R) -> Option<R> {
    if let Some(mut response) = target.downcast_mut::<Response>() {
        let response = &mut *response;
        return Some(f(&mut response.body, &mut response.used));
    }
    if let Some(mut request) = target.downcast_mut::<Request>() {
        let request = &mut *request;
        return Some(f(&mut request.body, &mut request.used));
    }
    None
}

fn bodied(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Response>() || o.is::<Request>())
        .ok_or_else(|| type_error("not a Request or a Response"))
}

fn headers_object(target: &JsObject) -> JsObject {
    if let Some(response) = target.downcast_ref::<Response>() {
        return response.headers.clone();
    }
    target
        .downcast_ref::<Request>()
        .map(|r| r.headers.clone())
        .expect("a Request or a Response")
}

/// `body`: the stream of the body, made now from its bytes if it is still bytes; `null` for
/// none.
fn body_stream(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = bodied(this)?;
    let body = with_body(&target, |body, _| body.clone()).expect("a body");
    match &body {
        Body::Empty => Ok(JsValue::null()),
        Body::Stream(stream) => Ok(stream.clone().into()),
        Body::Bytes(bytes) => {
            let chunks = if bytes.is_empty() {
                Vec::new()
            } else {
                vec![bytes.clone()]
            };
            let stream = crate::streams::readable_of_chunks(context, chunks)?;
            with_body(&target, |body, _| *body = Body::Stream(stream.clone()));
            Ok(stream.into())
        }
    }
}

fn body_used(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let target = bodied(this)?;
    Ok(with_body(&target, |body, used| {
        *used || matches!(body, Body::Stream(stream) if crate::streams::is_disturbed(stream))
    })
    .unwrap_or(false)
    .into())
}

/// Reads the whole body, once: a promise of its bytes as a `Uint8Array`.
fn consume(context: &mut Context, target: &JsObject) -> JsResult<JsValue> {
    let taken = with_body(target, |body, used| {
        let disturbed = matches!(body, Body::Stream(stream) if crate::streams::is_disturbed(stream) || crate::streams::is_locked(stream));
        if *used || disturbed {
            return Err(());
        }
        *used = true;
        Ok(body.clone())
    })
    .expect("a body");
    match &taken {
        Err(()) => {
            let reason = type_error_value("Body is unusable: Body has already been read", context);
            rejected(reason, context)
        }
        Ok(Body::Empty) => {
            let bytes = u8array(Vec::new(), context)?;
            crate::streams::resolved(bytes, context)
        }
        Ok(Body::Bytes(bytes)) => {
            let bytes = u8array(bytes.clone(), context)?;
            crate::streams::resolved(bytes, context)
        }
        Ok(Body::Stream(stream)) => read_all(context, stream),
    }
}

/// Every chunk of a stream, read to its end and joined.
#[derive(Trace, Finalize, JsData)]
struct Collect {
    reader: JsObject,
    #[unsafe_ignore_trace]
    bytes: Vec<u8>,
    resolvers: ResolvingFunctions,
}

fn read_all(context: &mut Context, stream: &JsObject) -> JsResult<JsValue> {
    let reader = crate::streams::acquire_reader(context, stream, false)?;
    let (promise, resolvers) = pending(context);
    let collect = JsObject::from_proto_and_data(
        None,
        Collect {
            reader,
            bytes: Vec::new(),
            resolvers,
        },
    );
    collect_step(context, &collect)?;
    Ok(promise.into())
}

fn collect_step(context: &mut Context, collect: &JsObject) -> JsResult<()> {
    let reader = collect
        .downcast_ref::<Collect>()
        .map(|c| c.reader.clone())
        .expect("a collect");
    let read = crate::streams::read(context, &reader)?;
    when(
        context,
        read.into(),
        collect.clone(),
        |collect, result, cx| {
            let (reader, resolvers) = {
                let c = collect.downcast_ref::<Collect>().expect("a collect");
                (c.reader.clone(), c.resolvers.clone())
            };
            if get(&result, "done", cx)?.to_boolean() {
                let bytes = std::mem::take(
                    &mut collect.downcast_mut::<Collect>().expect("a collect").bytes,
                );
                crate::streams::release_reader(cx, &reader);
                let bytes = u8array(bytes, cx)?;
                resolve(&resolvers, bytes, cx);
                return Ok(JsValue::undefined());
            }
            let chunk = get(&result, "value", cx)?;
            match crate::encoding::bytes_of(&chunk, cx)? {
                Some(bytes) => {
                    collect
                        .downcast_mut::<Collect>()
                        .expect("a collect")
                        .bytes
                        .extend(bytes);
                    collect_step(cx, collect)?;
                }
                None => {
                    let reason = type_error_value("A body stream's chunk is not a Uint8Array", cx);
                    crate::streams::release_reader(cx, &reader);
                    reject(&resolvers, reason, cx);
                }
            }
            Ok(JsValue::undefined())
        },
        |collect, reason, cx| {
            let resolvers = collect
                .downcast_ref::<Collect>()
                .map(|c| c.resolvers.clone())
                .expect("a collect");
            reject(&resolvers, reason, cx);
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

/// Text as `text()` reads a body: UTF-8, a byte-order mark dropped.
fn utf8_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

fn body_bytes_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    Ok(crate::encoding::bytes_of(value, context)?.unwrap_or_default())
}

/// The body read, then made what `then` makes of its bytes; a rejected promise when `this` is
/// not a body at all.
fn consume_then(
    this: &JsValue,
    context: &mut Context,
    then: fn(&JsValue, JsValue, &mut Context) -> JsResult<JsValue>,
) -> JsResult<JsValue> {
    let target = match bodied(this) {
        Ok(target) => target,
        Err(error) => {
            let reason = error_value(error, context);
            return rejected(reason, context);
        }
    };
    let kind = js_text(&content_type(&headers_object(&target)));
    let bytes = consume(context, &target)?;
    Ok(when(context, bytes, kind, then, rethrow)?.into())
}

const BODY_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("text", 0, |this, _, cx| {
        consume_then(this, cx, |_, bytes, cx| {
            Ok(js_text(&utf8_text(&body_bytes_of(&bytes, cx)?)))
        })
    }),
    ("json", 0, |this, _, cx| {
        consume_then(this, cx, |_, bytes, cx| {
            let text = utf8_text(&body_bytes_of(&bytes, cx)?);
            let parse = cx
                .intrinsics()
                .objects()
                .json()
                .get(js_string!("parse"), cx)?;
            crate::call(cx, &parse, &JsValue::undefined(), &[js_text(&text)])
        })
    }),
    ("arrayBuffer", 0, |this, _, cx| {
        consume_then(this, cx, |_, bytes, cx| get(&bytes, "buffer", cx))
    }),
    ("bytes", 0, |this, _, cx| {
        consume_then(this, cx, |_, bytes, _| Ok(bytes))
    }),
    ("blob", 0, |this, _, cx| {
        consume_then(this, cx, |kind, bytes, cx| {
            let options = object(cx);
            let kind = text_of(kind, cx)?.to_ascii_lowercase();
            options.set(js_string!("type"), js_text(&kind), false, cx)?;
            let parts = array(cx, vec![bytes]);
            Ok(crate::native::construct_global(cx, "Blob", &[parts, options.into()])?.into())
        })
    }),
    ("formData", 0, |this, _, cx| {
        consume_then(this, cx, |kind, bytes, cx| {
            let kind = text_of(kind, cx)?;
            let bytes = body_bytes_of(&bytes, cx)?;
            Ok(parse_form(&kind, &bytes, cx)?.into())
        })
    }),
];

// ─── Request ────────────────────────────────────────────────────────────────────────────────

/// What a request says besides its body and headers, kept as text.
#[derive(Clone, Default)]
struct RequestFields {
    url: String,
    method: String,
    mode: String,
    credentials: String,
    cache: String,
    redirect: String,
    referrer: String,
    referrer_policy: String,
    integrity: String,
    keepalive: bool,
    duplex: String,
    destination: String,
}

#[derive(Trace, Finalize, JsData)]
struct Request {
    body: Body,
    #[unsafe_ignore_trace]
    used: bool,
    headers: JsObject,
    signal: JsValue,
    #[unsafe_ignore_trace]
    fields: RequestFields,
}

fn request_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Request>())
        .ok_or_else(|| type_error("not a Request"))
}

/// A method as `Request` takes it: a token; the six the standard names upper-cased; the three
/// it forbids refused.
fn normalize_method(method: &str) -> JsResult<String> {
    if !is_token(method) {
        return Err(type_error(format!("'{method}' is not a valid HTTP method")));
    }
    let upper = method.to_ascii_uppercase();
    if ["CONNECT", "TRACE", "TRACK"].contains(&upper.as_str()) {
        return Err(type_error(format!("'{method}' HTTP method is unsupported")));
    }
    if ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"].contains(&upper.as_str()) {
        return Ok(upper);
    }
    Ok(method.to_owned())
}

fn construct_request(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let proto = classes(context)?.request_proto.clone();
    let proto = prototype_for(this, "Request", proto, context)?;
    let (input, init) = (arg(args, 0), arg(args, 1));
    let field = |name: &str, cx: &mut Context| -> JsResult<Option<String>> {
        let value = get(&init, name, cx)?;
        if value.is_undefined() {
            Ok(None)
        } else {
            Ok(Some(text_of(&value, cx)?))
        }
    };
    // From another request, or from a URL.
    let from = request_of(&input).ok();
    let (mut fields, mut headers, mut signal, mut body) = match &from {
        Some(other) => {
            let r = other.downcast_ref::<Request>().expect("a Request");
            (
                r.fields.clone(),
                headers_list(&r.headers),
                r.signal.clone(),
                Some((r.body.clone(), r.used)),
            )
        }
        None => {
            let text = text_of(&input, context)?;
            let url = crate::native::construct_global(context, "URL", &[js_text(&text)])
                .map_err(|_| type_error(format!("Failed to parse URL from {text}")))?;
            let href = text_of(&url.get(js_string!("href"), context)?, context)?;
            let fields = RequestFields {
                url: href,
                method: "GET".into(),
                mode: "cors".into(),
                credentials: "same-origin".into(),
                cache: "default".into(),
                redirect: "follow".into(),
                referrer: "about:client".into(),
                duplex: "half".into(),
                ..RequestFields::default()
            };
            (fields, Vec::new(), JsValue::undefined(), None)
        }
    };
    if let Some(method) = field("method", context)? {
        fields.method = normalize_method(&method)?;
    }
    for (name, slot) in [
        ("mode", &mut fields.mode),
        ("credentials", &mut fields.credentials),
        ("cache", &mut fields.cache),
        ("redirect", &mut fields.redirect),
        ("referrer", &mut fields.referrer),
        ("referrerPolicy", &mut fields.referrer_policy),
        ("integrity", &mut fields.integrity),
        ("duplex", &mut fields.duplex),
    ] {
        let value = get(&init, name, context)?;
        if !value.is_undefined() {
            *slot = text_of(&value, context)?;
        }
    }
    let keepalive = get(&init, "keepalive", context)?;
    if !keepalive.is_undefined() {
        fields.keepalive = keepalive.to_boolean();
    }
    let given_headers = get(&init, "headers", context)?;
    if !given_headers.is_undefined() {
        headers = fill_headers(&given_headers, context)?;
    }
    let given_signal = get(&init, "signal", context)?;
    if !given_signal.is_null_or_undefined() {
        signal = given_signal;
    }
    if signal.is_undefined() {
        // A signal that never aborts, as a request made without one has.
        signal = crate::native::construct_global(context, "AbortController", &[])
            .and_then(|c| c.get(js_string!("signal"), context))
            .unwrap_or_default();
    }
    let given_body = get(&init, "body", context)?;
    let (body, implied) = if !given_body.is_undefined() {
        extract_body(&given_body, context)?
    } else {
        match body.take() {
            Some((_, true)) => {
                return Err(type_error(
                    "Cannot construct a Request with a Request whose body was already used",
                ));
            }
            Some((body, false)) => {
                // The body moves to the new request: the old one is used up.
                if let Some(other) = &from
                    && !matches!(body, Body::Empty)
                {
                    other.downcast_mut::<Request>().expect("a Request").used = true;
                }
                (body, None)
            }
            None => (Body::Empty, None),
        }
    };
    if !matches!(body, Body::Empty) && (fields.method == "GET" || fields.method == "HEAD") {
        return Err(type_error("Request with GET/HEAD method cannot have body."));
    }
    if let Some(kind) = implied
        && header_get(&headers, "content-type").is_none()
    {
        headers.push(("content-type".into(), kind));
    }
    let headers = new_headers(context, headers, false)?;
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Request {
            body,
            used: false,
            headers,
            signal,
            fields,
        },
    )
    .into())
}

macro_rules! request_text {
    ($name:literal, $field:ident) => {
        (
            $name,
            |this, _, _| {
                Ok(js_text(
                    &request_of(this)?
                        .downcast_ref::<Request>()
                        .expect("a Request")
                        .fields
                        .$field,
                ))
            },
            None,
        )
    };
}

const REQUEST_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    request_text!("url", url),
    request_text!("method", method),
    request_text!("mode", mode),
    request_text!("credentials", credentials),
    request_text!("cache", cache),
    request_text!("redirect", redirect),
    request_text!("referrer", referrer),
    request_text!("referrerPolicy", referrer_policy),
    request_text!("integrity", integrity),
    request_text!("duplex", duplex),
    request_text!("destination", destination),
    (
        "keepalive",
        |this, _, _| {
            Ok(request_of(this)?
                .downcast_ref::<Request>()
                .expect("a Request")
                .fields
                .keepalive
                .into())
        },
        None,
    ),
    (
        "headers",
        |this, _, _| {
            Ok(request_of(this)?
                .downcast_ref::<Request>()
                .expect("a Request")
                .headers
                .clone()
                .into())
        },
        None,
    ),
    (
        "signal",
        |this, _, _| {
            Ok(request_of(this)?
                .downcast_ref::<Request>()
                .expect("a Request")
                .signal
                .clone())
        },
        None,
    ),
    ("body", body_stream, None),
    ("bodyUsed", body_used, None),
];

/// A body copied for `clone()`: a stream is teed, one branch kept, the other given.
fn clone_body(context: &mut Context, target: &JsObject) -> JsResult<Body> {
    let (body, used) = with_body(target, |body, used| (body.clone(), *used)).expect("a body");
    if used
        || matches!(&body, Body::Stream(s) if crate::streams::is_disturbed(s) || crate::streams::is_locked(s))
    {
        return Err(type_error("Cannot clone a body that was already read"));
    }
    Ok(match &body {
        Body::Stream(stream) => {
            let (mine, theirs) = crate::streams::tee(context, stream)?;
            with_body(target, |body, _| *body = Body::Stream(mine));
            Body::Stream(theirs)
        }
        _ => body.clone(),
    })
}

const REQUEST_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("clone", 0, |this, _, cx| {
        let request = request_of(this)?;
        let body = clone_body(cx, &request)?;
        let (headers, signal, fields) = {
            let r = request.downcast_ref::<Request>().expect("a Request");
            (headers_list(&r.headers), r.signal.clone(), r.fields.clone())
        };
        let headers = new_headers(cx, headers, false)?;
        let proto = request
            .prototype()
            .unwrap_or(classes(cx)?.request_proto.clone());
        Ok(JsObject::from_proto_and_data(
            Some(proto),
            Request {
                body,
                used: false,
                headers,
                signal,
                fields,
            },
        )
        .into())
    }),
    BODY_METHODS[0],
    BODY_METHODS[1],
    BODY_METHODS[2],
    BODY_METHODS[3],
    BODY_METHODS[4],
    BODY_METHODS[5],
];

// ─── Response ───────────────────────────────────────────────────────────────────────────────

#[derive(Trace, Finalize, JsData)]
struct Response {
    body: Body,
    #[unsafe_ignore_trace]
    used: bool,
    headers: JsObject,
    #[unsafe_ignore_trace]
    status: u16,
    #[unsafe_ignore_trace]
    status_text: String,
    #[unsafe_ignore_trace]
    kind: &'static str,
    #[unsafe_ignore_trace]
    url: String,
    #[unsafe_ignore_trace]
    redirected: bool,
}

fn response_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Response>())
        .ok_or_else(|| type_error("not a Response"))
}

/// A status that cannot have a body.
fn null_body_status(status: u16) -> bool {
    matches!(status, 101 | 103 | 204 | 205 | 304)
}

/// A response's status, text and headers from `init`, its body from `body`.
fn make_response(
    context: &mut Context,
    proto: JsObject,
    body: &JsValue,
    init: &JsValue,
) -> JsResult<JsObject> {
    let status = get(init, "status", context)?;
    let status = if status.is_undefined() {
        200.
    } else {
        status.to_number(context)?
    };
    if status.fract() != 0. || !(200. ..=599.).contains(&status) {
        return Err(crate::native::range_error(format!(
            "init[\"status\"] must be in the range of 200 to 599, inclusive. Received {status}"
        )));
    }
    let status = status as u16;
    let text = get(init, "statusText", context)?;
    let status_text = if text.is_undefined() {
        String::new()
    } else {
        text_of(&text, context)?
    };
    if status_text.contains(['\r', '\n']) {
        return Err(type_error("Invalid statusText"));
    }
    let mut headers = fill_headers(&get(init, "headers", context)?, context)?;
    let (body, implied) = extract_body(body, context)?;
    if !matches!(body, Body::Empty) && null_body_status(status) {
        return Err(type_error(format!(
            "Response with null body status {status} cannot have body"
        )));
    }
    if let Some(kind) = implied
        && header_get(&headers, "content-type").is_none()
    {
        headers.push(("content-type".into(), kind));
    }
    let headers = new_headers(context, headers, false)?;
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Response {
            body,
            used: false,
            headers,
            status,
            status_text,
            kind: "default",
            url: String::new(),
            redirected: false,
        },
    ))
}

fn construct_response(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let proto = classes(context)?.response_proto.clone();
    let proto = prototype_for(this, "Response", proto, context)?;
    Ok(make_response(context, proto, &arg(args, 0), &arg(args, 1))?.into())
}

fn response_field<R>(this: &JsValue, f: impl FnOnce(&Response) -> R) -> JsResult<R> {
    let response = response_of(this)?;
    let r = response.downcast_ref::<Response>().expect("a Response");
    Ok(f(&r))
}

const RESPONSE_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    (
        "type",
        |this, _, _| response_field(this, |r| js_text(r.kind)),
        None,
    ),
    (
        "url",
        |this, _, _| response_field(this, |r| js_text(&r.url)),
        None,
    ),
    (
        "redirected",
        |this, _, _| response_field(this, |r| r.redirected.into()),
        None,
    ),
    (
        "status",
        |this, _, _| response_field(this, |r| r.status.into()),
        None,
    ),
    (
        "ok",
        |this, _, _| response_field(this, |r| (200..300).contains(&r.status).into()),
        None,
    ),
    (
        "statusText",
        |this, _, _| response_field(this, |r| js_text(&r.status_text)),
        None,
    ),
    (
        "headers",
        |this, _, _| response_field(this, |r| r.headers.clone().into()),
        None,
    ),
    ("body", body_stream, None),
    ("bodyUsed", body_used, None),
];

const RESPONSE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("clone", 0, |this, _, cx| {
        let response = response_of(this)?;
        let body = clone_body(cx, &response)?;
        let (headers, status, status_text, kind, url, redirected, immutable) = {
            let r = response.downcast_ref::<Response>().expect("a Response");
            let immutable = with_headers(&r.headers, |h| h.immutable);
            (
                headers_list(&r.headers),
                r.status,
                r.status_text.clone(),
                r.kind,
                r.url.clone(),
                r.redirected,
                immutable,
            )
        };
        let headers = new_headers(cx, headers, immutable)?;
        let proto = response
            .prototype()
            .unwrap_or(classes(cx)?.response_proto.clone());
        Ok(JsObject::from_proto_and_data(
            Some(proto),
            Response {
                body,
                used: false,
                headers,
                status,
                status_text,
                kind,
                url,
                redirected,
            },
        )
        .into())
    }),
    BODY_METHODS[0],
    BODY_METHODS[1],
    BODY_METHODS[2],
    BODY_METHODS[3],
    BODY_METHODS[4],
    BODY_METHODS[5],
];

const RESPONSE_STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("error", 0, |_, _, cx| {
        let proto = classes(cx)?.response_proto.clone();
        let headers = new_headers(cx, Vec::new(), true)?;
        Ok(JsObject::from_proto_and_data(
            Some(proto),
            Response {
                body: Body::Empty,
                used: false,
                headers,
                status: 0,
                status_text: String::new(),
                kind: "error",
                url: String::new(),
                redirected: false,
            },
        )
        .into())
    }),
    ("redirect", 2, |_, args, cx| {
        let text = text_of(&arg(args, 0), cx)?;
        let url = crate::native::construct_global(cx, "URL", &[js_text(&text)])
            .map_err(|_| type_error(format!("Failed to parse URL from {text}")))?;
        let href = text_of(&url.get(js_string!("href"), cx)?, cx)?;
        let status = match arg(args, 1) {
            s if s.is_undefined() => 302,
            s => s.to_number(cx)? as u16,
        };
        if ![301, 302, 303, 307, 308].contains(&status) {
            return Err(crate::native::range_error(format!(
                "Invalid status code {status}"
            )));
        }
        let proto = classes(cx)?.response_proto.clone();
        let headers = new_headers(cx, vec![("location".into(), href)], true)?;
        Ok(JsObject::from_proto_and_data(
            Some(proto),
            Response {
                body: Body::Empty,
                used: false,
                headers,
                status,
                status_text: String::new(),
                kind: "default",
                url: String::new(),
                redirected: false,
            },
        )
        .into())
    }),
    ("json", 2, |_, args, cx| {
        let json = cx.intrinsics().objects().json();
        let stringify = json.get(js_string!("stringify"), cx)?;
        let text = crate::call(cx, &stringify, &json.into(), &[arg(args, 0)])?;
        if text.is_undefined() {
            return Err(type_error("Value is not JSON serializable"));
        }
        let proto = classes(cx)?.response_proto.clone();
        let response = make_response(cx, proto, &text, &arg(args, 1))?;
        let headers = headers_object(&response);
        with_headers(&headers, |h| {
            h.list.retain(|(n, _)| n != "content-type");
            h.list
                .push(("content-type".into(), "application/json".into()));
        });
        Ok(response.into())
    }),
];

// ─── FormData ───────────────────────────────────────────────────────────────────────────────

#[derive(Trace, Finalize)]
struct Entry {
    #[unsafe_ignore_trace]
    name: String,
    value: JsValue,
}

#[derive(Trace, Finalize, JsData)]
struct FormData {
    entries: Vec<Entry>,
}

fn form_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<FormData>())
        .ok_or_else(|| type_error("not a FormData"))
}

fn construct_form_data(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // A form element to read the fields of is the DOM's, which the host owns: one given is
    // not read here.
    let proto = classes(context)?.form_data_proto.clone();
    let proto = prototype_for(this, "FormData", proto, context)?;
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        FormData {
            entries: Vec::new(),
        },
    )
    .into())
}

/// What an entry holds: text, or a `File` — a blob given is made a `File`, named by
/// `filename`, its own name, or `blob`.
fn entry_value(value: &JsValue, filename: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    if !is_blob(value, context) {
        return Ok(js_text(&text_of(value, context)?));
    }
    let is_file = is_instance_of_global(value, "File", context)?;
    if is_file && filename.is_undefined() {
        return Ok(value.clone());
    }
    let name = if !filename.is_undefined() {
        text_of(filename, context)?
    } else if is_file {
        text_of(&get(value, "name", context)?, context)?
    } else {
        "blob".to_owned()
    };
    let options = object(context);
    options.set(
        js_string!("type"),
        get(value, "type", context)?,
        false,
        context,
    )?;
    if is_file {
        options.set(
            js_string!("lastModified"),
            get(value, "lastModified", context)?,
            false,
            context,
        )?;
    }
    let parts = array(context, vec![value.clone()]);
    Ok(
        crate::native::construct_global(context, "File", &[parts, js_text(&name), options.into()])?
            .into(),
    )
}

fn form_entries(form: &JsObject) -> Vec<(String, JsValue)> {
    form.downcast_ref::<FormData>()
        .map(|f| {
            f.entries
                .iter()
                .map(|e| (e.name.clone(), e.value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn form_iterator(this: &JsValue, context: &mut Context, which: u8) -> JsResult<JsValue> {
    let form = form_of(this)?;
    let items: Vec<JsValue> = form_entries(&form)
        .into_iter()
        .map(|(name, value)| match which {
            0 => js_text(&name),
            1 => value,
            _ => array(context, vec![js_text(&name), value]),
        })
        .collect();
    let items = array(context, items);
    invoke(&items, "values", &[], context)
}

const FORM_DATA_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("append", 3, |this, args, cx| {
        let form = form_of(this)?;
        let name = text_of(&arg(args, 0), cx)?;
        let value = entry_value(&arg(args, 1), &arg(args, 2), cx)?;
        form.downcast_mut::<FormData>()
            .expect("a FormData")
            .entries
            .push(Entry { name, value });
        Ok(JsValue::undefined())
    }),
    ("set", 3, |this, args, cx| {
        let form = form_of(this)?;
        let name = text_of(&arg(args, 0), cx)?;
        let value = entry_value(&arg(args, 1), &arg(args, 2), cx)?;
        let mut f = form.downcast_mut::<FormData>().expect("a FormData");
        match f.entries.iter().position(|e| e.name == name) {
            Some(at) => {
                f.entries[at].value = value;
                let mut index = 0;
                f.entries.retain(|e| {
                    let keep = index <= at || e.name != name;
                    index += 1;
                    keep
                });
            }
            None => f.entries.push(Entry { name, value }),
        }
        Ok(JsValue::undefined())
    }),
    ("get", 1, |this, args, cx| {
        let form = form_of(this)?;
        let name = text_of(&arg(args, 0), cx)?;
        Ok(form_entries(&form)
            .into_iter()
            .find(|(n, _)| *n == name)
            .map_or(JsValue::null(), |(_, v)| v))
    }),
    ("getAll", 1, |this, args, cx| {
        let form = form_of(this)?;
        let name = text_of(&arg(args, 0), cx)?;
        let all: Vec<JsValue> = form_entries(&form)
            .into_iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, v)| v)
            .collect();
        Ok(array(cx, all))
    }),
    ("has", 1, |this, args, cx| {
        let form = form_of(this)?;
        let name = text_of(&arg(args, 0), cx)?;
        Ok(form_entries(&form).iter().any(|(n, _)| *n == name).into())
    }),
    ("delete", 1, |this, args, cx| {
        let form = form_of(this)?;
        let name = text_of(&arg(args, 0), cx)?;
        form.downcast_mut::<FormData>()
            .expect("a FormData")
            .entries
            .retain(|e| e.name != name);
        Ok(JsValue::undefined())
    }),
    ("forEach", 2, |this, args, cx| {
        let form = form_of(this)?;
        let (callback, this_arg) = (arg(args, 0), arg(args, 1));
        for (name, value) in form_entries(&form) {
            crate::call(
                cx,
                &callback,
                &this_arg,
                &[value, js_text(&name), form.clone().into()],
            )?;
        }
        Ok(JsValue::undefined())
    }),
    ("keys", 0, |this, _, cx| form_iterator(this, cx, 0)),
    ("values", 0, |this, _, cx| form_iterator(this, cx, 1)),
    ("entries", 0, |this, _, cx| form_iterator(this, cx, 2)),
];

/// A name or file name in a part's `Content-Disposition`, escaped as browsers escape it.
fn escape_disposition(text: &str) -> String {
    text.replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// The form as `multipart/form-data`: its bytes, and the boundary between its parts.
fn multipart(form: &JsObject, context: &mut Context) -> JsResult<(Vec<u8>, String)> {
    let mut random = [0u8; 12];
    let _ = getrandom::fill(&mut random);
    let boundary = format!(
        "----RuntimeFormBoundary{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let mut out = Vec::new();
    for (name, value) in form_entries(form) {
        out.extend(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
                escape_disposition(&name)
            )
            .into_bytes(),
        );
        if is_blob(&value, context) {
            let filename = text_of(&get(&value, "name", context)?, context)?;
            let kind = text_of(&get(&value, "type", context)?, context)?;
            let kind = if kind.is_empty() {
                "application/octet-stream".to_owned()
            } else {
                kind
            };
            out.extend(
                format!(
                    "; filename=\"{}\"\r\nContent-Type: {kind}\r\n\r\n",
                    escape_disposition(&filename)
                )
                .into_bytes(),
            );
            out.extend(crate::web::blob_bytes(&value, context)?);
        } else {
            out.extend(b"\r\n\r\n");
            let text = text_of(&value, context)?;
            out.extend(
                text.replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\n', "\r\n")
                    .into_bytes(),
            );
        }
        out.extend(b"\r\n");
    }
    out.extend(format!("--{boundary}--\r\n").into_bytes());
    Ok((out, boundary))
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&at| &haystack[at..at + needle.len()] == needle)
}

/// A parameter of a header value: `boundary` of `multipart/form-data; boundary=x`.
fn parameter(header: &str, key: &str) -> Option<String> {
    header.split(';').skip(1).find_map(|part| {
        let (k, v) = part.split_once('=')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().trim_matches('"').to_owned())
    })
}

/// `formData()`: a body read back as a `FormData`, from `multipart/form-data` or a urlencoded
/// body.
fn parse_form(kind: &str, bytes: &[u8], context: &mut Context) -> JsResult<JsObject> {
    let proto = classes(context)?.form_data_proto.clone();
    let mut entries = Vec::new();
    let essence = kind
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if essence == "application/x-www-form-urlencoded" {
        for (name, value) in form_urlencoded::parse(bytes) {
            entries.push(Entry {
                name: name.into_owned(),
                value: js_text(&value),
            });
        }
    } else if essence == "multipart/form-data" {
        let boundary = parameter(kind, "boundary")
            .ok_or_else(|| type_error("formData: the multipart body names no boundary"))?;
        let delimiter = format!("--{boundary}").into_bytes();
        let mut at = find(bytes, &delimiter, 0)
            .ok_or_else(|| type_error("formData: the multipart body is malformed"))?;
        loop {
            at += delimiter.len();
            if bytes[at..].starts_with(b"--") {
                break;
            }
            let start = at
                + if bytes[at..].starts_with(b"\r\n") {
                    2
                } else {
                    0
                };
            let Some(end) = find(bytes, &delimiter, start) else {
                break;
            };
            let part = &bytes[start..end];
            let part = part.strip_suffix(b"\r\n").unwrap_or(part);
            let split = find(part, b"\r\n\r\n", 0)
                .ok_or_else(|| type_error("formData: a multipart part is malformed"))?;
            let head = String::from_utf8_lossy(&part[..split]).into_owned();
            let body = &part[split + 4..];
            let mut name = None;
            let mut filename = None;
            let mut content_type = String::new();
            for line in head.split("\r\n") {
                let Some((key, value)) = line.split_once(':') else {
                    continue;
                };
                if key.trim().eq_ignore_ascii_case("content-disposition") {
                    let value = format!("x;{}", value.split_once(';').map_or("", |(_, rest)| rest));
                    name = parameter(&value, "name");
                    filename = parameter(&value, "filename");
                } else if key.trim().eq_ignore_ascii_case("content-type") {
                    content_type = value.trim().to_owned();
                }
            }
            let Some(name) = name else {
                at = end;
                continue;
            };
            let value = match filename {
                Some(filename) => {
                    let options = object(context);
                    options.set(js_string!("type"), js_text(&content_type), false, context)?;
                    let data = u8array(body.to_vec(), context)?;
                    let parts = array(context, vec![data]);
                    crate::native::construct_global(
                        context,
                        "File",
                        &[parts, js_text(&filename), options.into()],
                    )?
                    .into()
                }
                None => js_text(&String::from_utf8_lossy(body)),
            };
            entries.push(Entry { name, value });
            at = end;
        }
    } else {
        return Err(type_error(format!(
            "formData: cannot read a body of type '{kind}'"
        )));
    }
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        FormData { entries },
    ))
}

// ─── Installing them ────────────────────────────────────────────────────────────────────────

const GLOBALS: &[(&str, crate::native::GlobalMaker)] = &[
    ("Headers", |cx| Ok(classes(cx)?.headers.clone().into())),
    ("Request", |cx| Ok(classes(cx)?.request.clone().into())),
    ("Response", |cx| Ok(classes(cx)?.response.clone().into())),
    ("FormData", |cx| Ok(classes(cx)?.form_data.clone().into())),
];

/// The four globals, each made (with the rest) the first time one is read. A host with
/// classes of its own — a DOM's `FormData` — simply puts them in their place.
pub fn install(context: &mut Context) -> JsResult<()> {
    for (name, make) in GLOBALS {
        crate::native::lazy_global(context, name, *make)?;
    }
    Ok(())
}
