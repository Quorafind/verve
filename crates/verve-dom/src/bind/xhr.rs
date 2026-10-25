//! `XMLHttpRequest`: the old way to the network, which bundled libraries still take — sent
//! through the realm's own `fetch` (and so under its permissions), answered on the host's
//! beat, with the events a page hears: `readystatechange` through each state, then `load`,
//! `error` or `abort`, then `loadend`.
//!
//! Synchronous requests (`open(…, false)`) are sent as asynchronous ones: nothing here may wait
//! for the network.

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};

use super::{
    Accessor, ClassSpec, Method, arg, arg_string, call_method, class, closure, get, list,
    plain_object, set, string, text, type_error,
};
use crate::state::shared;

const EVENTS: &[&str] = &["abort", "error", "load", "loadend", "loadstart", "progress", "readystatechange", "timeout"];

/// What a request keeps of itself, on a hidden object.
fn record(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    get(this, "__xhr", context)?
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation: not an XMLHttpRequest"))
}

fn construct(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = shared(context).borrow().class("EventTarget").expect("made").0;
    let object = target.construct(&[], new_target.as_object().as_ref(), context)?;
    let hidden = plain_object(context);
    let headers = super::array(Vec::new(), context);
    set(&hidden, "headers", headers, context)?;
    set(&hidden, "readyState", 0, context)?;
    set(&hidden, "status", 0, context)?;
    set(&hidden, "statusText", text(""), context)?;
    set(&hidden, "responseText", text(""), context)?;
    set(&hidden, "response", text(""), context)?;
    set(&hidden, "responseURL", text(""), context)?;
    set(&hidden, "responseHeaders", text(""), context)?;
    set(&hidden, "aborted", false, context)?;
    super::hidden(&object, js_string!("__xhr"), hidden, context)?;
    for (key, value) in [
        ("responseType", text("")),
        ("timeout", 0.into()),
        ("withCredentials", false.into()),
    ] {
        set(&object, key, value, context)?;
    }
    let upload = target.construct(&[], None, context)?;
    super::hidden(&object, js_string!("upload"), upload, context)?;
    Ok(object.into())
}

fn fire(this: &JsValue, kind: &str, context: &mut Context) -> JsResult<()> {
    super::events::fire(context, this, "ProgressEvent", kind, &[("bubbles", false.into()), ("cancelable", false.into())])
        .or_else(|_| super::events::fire(context, this, "Event", kind, &[("bubbles", false.into()), ("cancelable", false.into())]))?;
    Ok(())
}

fn ready(this: &JsValue, state: u32, context: &mut Context) -> JsResult<()> {
    let hidden = record(this, context)?;
    set(&hidden, "readyState", state, context)?;
    fire(this, "readystatechange", context)
}

fn read(this: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    let hidden = record(this, context)?;
    hidden.get(boa_engine::JsString::from(key), context)
}

const XHR_ACCESSORS: &[Accessor] = &[
    ("readyState", Some(|this, _, cx| read(this, "readyState", cx)), None),
    ("status", Some(|this, _, cx| read(this, "status", cx)), None),
    ("statusText", Some(|this, _, cx| read(this, "statusText", cx)), None),
    ("responseText", Some(|this, _, cx| read(this, "responseText", cx)), None),
    ("response", Some(|this, _, cx| read(this, "response", cx)), None),
    ("responseURL", Some(|this, _, cx| read(this, "responseURL", cx)), None),
    (
        "responseXML",
        Some(|this, _, cx| {
            let body = read(this, "responseText", cx)?;
            if string(&body, cx)?.is_empty() {
                return Ok(JsValue::null());
            }
            let parser = cx.global_object().get(js_string!("DOMParser"), cx)?;
            let Some(parser) = parser.as_object() else { return Ok(JsValue::null()) };
            let parser = parser.construct(&[], None, cx)?;
            call_method(&parser.into(), "parseFromString", &[body, text("text/html")], cx)
        }),
        None,
    ),
];

const XHR_METHODS: &[Method] = &[
    ("open", 2, |this, args, cx| {
        let hidden = record(this, cx)?;
        let method = arg_string(args, 0, cx)?.to_ascii_uppercase();
        let url = arg_string(args, 1, cx)?;
        let resolved = shared(cx).borrow().url.join(&url).map(|u| u.to_string()).unwrap_or(url);
        set(&hidden, "method", text(&method), cx)?;
        set(&hidden, "url", text(&resolved), cx)?;
        set(&hidden, "aborted", false, cx)?;
        let headers = super::array(Vec::new(), cx);
        set(&hidden, "headers", headers, cx)?;
        ready(this, 1, cx)?;
        Ok(JsValue::undefined())
    }),
    ("setRequestHeader", 2, |this, args, cx| {
        let hidden = record(this, cx)?;
        let headers = hidden.get(js_string!("headers"), cx)?;
        let pair = super::array(vec![text(&arg_string(args, 0, cx)?), text(&arg_string(args, 1, cx)?)], cx);
        call_method(&headers, "push", &[pair], cx)?;
        Ok(JsValue::undefined())
    }),
    ("send", 1, |this, args, cx| {
        let hidden = record(this, cx)?;
        let method = hidden.get(js_string!("method"), cx)?;
        let url = hidden.get(js_string!("url"), cx)?;
        if url.is_undefined() {
            return Err(super::dom_error(cx, "Failed to execute 'send' on 'XMLHttpRequest': The object's state must be OPENED.", "InvalidStateError"));
        }
        let init = plain_object(cx);
        set(&init, "method", method.clone(), cx)?;
        let headers = hidden.get(js_string!("headers"), cx)?;
        set(&init, "headers", headers, cx)?;
        let body = arg(args, 0);
        let method_text = string(&method, cx)?;
        if !body.is_null_or_undefined() && method_text != "GET" && method_text != "HEAD" {
            set(&init, "body", body, cx)?;
        }
        fire(this, "loadstart", cx)?;
        let fetch = cx.global_object().get(js_string!("fetch"), cx)?;
        let promise = verve::call(cx, &fetch, &JsValue::undefined(), &[url, init.into()])?;
        let on_response = closure(cx, "response", |_, args, xhr: &JsValue, cx| answered(xhr, &arg(args, 0), cx), this.clone());
        let on_error = closure(cx, "error", |_, _, xhr: &JsValue, cx| failed(xhr, "error", cx), this.clone());
        call_method(&promise, "then", &[on_response.into(), on_error.into()], cx)?;
        Ok(JsValue::undefined())
    }),
    ("abort", 0, |this, _, cx| {
        let hidden = record(this, cx)?;
        set(&hidden, "aborted", true, cx)?;
        failed(this, "abort", cx)?;
        Ok(JsValue::undefined())
    }),
    ("getResponseHeader", 1, |this, args, cx| {
        let name = arg_string(args, 0, cx)?.to_ascii_lowercase();
        let headers = string(&read(this, "responseHeaders", cx)?, cx)?;
        let values: Vec<&str> = headers
            .lines()
            .filter_map(|line| line.split_once(": "))
            .filter(|(k, _)| k.eq_ignore_ascii_case(&name))
            .map(|(_, v)| v)
            .collect();
        Ok(if values.is_empty() { JsValue::null() } else { text(&values.join(", ")) })
    }),
    ("getAllResponseHeaders", 0, |this, _, cx| read(this, "responseHeaders", cx)),
    ("overrideMimeType", 1, |_, _, _| Ok(JsValue::undefined())),
];

/// The response came: its status and headers, then its body as `responseType` asks.
fn answered(xhr: &JsValue, response: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let hidden = record(xhr, context)?;
    if hidden.get(js_string!("aborted"), context)?.to_boolean() {
        return Ok(JsValue::undefined());
    }
    let status = get(response, "status", context)?;
    set(&hidden, "status", status, context)?;
    let status_text = get(response, "statusText", context)?;
    set(&hidden, "statusText", status_text, context)?;
    let url = get(response, "url", context)?;
    set(&hidden, "responseURL", url, context)?;
    // `name: value` lines, as `getAllResponseHeaders` answers.
    let headers = get(response, "headers", context)?;
    let entries = call_method(&headers, "entries", &[], context)?;
    let array_from = context.intrinsics().constructors().array().constructor().get(js_string!("from"), context)?;
    let pairs = verve::call(context, &array_from, &JsValue::undefined(), &[entries])?;
    let mut lines = String::new();
    for pair in list(&pairs, context)? {
        let pair = list(&pair, context)?;
        if let [name, value] = pair.as_slice() {
            lines.push_str(&format!("{}: {}\r\n", string(name, context)?, string(value, context)?));
        }
    }
    set(&hidden, "responseHeaders", text(&lines), context)?;
    ready(xhr, 2, context)?;
    let kind = string(&get(xhr, "responseType", context)?, context)?;
    let reader = match kind.as_str() {
        "arraybuffer" => "arrayBuffer",
        "blob" => "blob",
        _ => "text",
    };
    let body = call_method(response, reader, &[], context)?;
    let on_body = closure(context, "body", |_, args, xhr: &JsValue, cx| loaded(xhr, &arg(args, 0), cx), xhr.clone());
    let on_error = closure(context, "error", |_, _, xhr: &JsValue, cx| failed(xhr, "error", cx), xhr.clone());
    call_method(&body, "then", &[on_body.into(), on_error.into()], context)?;
    Ok(JsValue::undefined())
}

fn loaded(xhr: &JsValue, body: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let hidden = record(xhr, context)?;
    if hidden.get(js_string!("aborted"), context)?.to_boolean() {
        return Ok(JsValue::undefined());
    }
    ready(xhr, 3, context)?;
    let kind = string(&get(xhr, "responseType", context)?, context)?;
    let response = match kind.as_str() {
        "json" => {
            let json = context.intrinsics().objects().json();
            let parse = json.get(js_string!("parse"), context)?;
            verve::call(context, &parse, &json.into(), std::slice::from_ref(body)).unwrap_or(JsValue::null())
        }
        "document" => {
            let parser = context.global_object().get(js_string!("DOMParser"), context)?;
            match parser.as_object() {
                Some(parser) => {
                    let parser = parser.construct(&[], None, context)?;
                    call_method(&parser.into(), "parseFromString", &[body.clone(), text("text/html")], context)?
                }
                None => JsValue::null(),
            }
        }
        _ => body.clone(),
    };
    if body.is_string() {
        set(&hidden, "responseText", body.clone(), context)?;
    }
    set(&hidden, "response", response, context)?;
    ready(xhr, 4, context)?;
    fire(xhr, "progress", context)?;
    fire(xhr, "load", context)?;
    fire(xhr, "loadend", context)?;
    Ok(JsValue::undefined())
}

fn failed(xhr: &JsValue, kind: &str, context: &mut Context) -> JsResult<JsValue> {
    let hidden = record(xhr, context)?;
    set(&hidden, "status", 0, context)?;
    set(&hidden, "readyState", 4, context)?;
    fire(xhr, "readystatechange", context)?;
    fire(xhr, kind, context)?;
    fire(xhr, "loadend", context)?;
    if kind == "abort" {
        set(&hidden, "readyState", 0, context)?;
    }
    Ok(JsValue::undefined())
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let (constructor, prototype) = class(
        context,
        ClassSpec {
            name: "XMLHttpRequest",
            length: 0,
            construct,
            parent: Some("EventTarget"),
            methods: XHR_METHODS,
            accessors: XHR_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    verve::dom_events::handler_accessors(context, &prototype, EVENTS)?;
    for (name, value) in [("UNSENT", 0), ("OPENED", 1), ("HEADERS_RECEIVED", 2), ("LOADING", 3), ("DONE", 4)] {
        constructor.set(boa_engine::JsString::from(name), value, false, context)?;
        prototype.set(boa_engine::JsString::from(name), value, false, context)?;
    }
    class(
        context,
        ClassSpec {
            name: "XMLHttpRequestEventTarget",
            length: 0,
            construct: super::illegal_constructor,
            parent: Some("EventTarget"),
            methods: &[],
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    Ok(())
}
