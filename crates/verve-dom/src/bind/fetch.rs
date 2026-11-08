//! DOM 地址解析与传输分离；真正的请求仍经过安装前的 Verve fetch 和权限链。

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use boa_gc::{Finalize, Trace};
use verve::native::{self, arg, closure, get, js_text, text_of, type_error};

use crate::state::shared;

/// The getter of `Response.prototype.url`, as the runtime made it.
fn response_url_getter(context: &mut Context) -> Option<boa_engine::JsValue> {
    let global = context.global_object();
    let response = global.get(js_string!("Response"), context).ok()?;
    let proto = get(&response, "prototype", context).ok()?;
    let object = global.get(js_string!("Object"), context).ok()?;
    let describe = get(&object, "getOwnPropertyDescriptor", context).ok()?;
    let descriptor = describe
        .as_callable()?
        .call(&object, &[proto, js_text("url")], context)
        .ok()?;
    get(&descriptor, "get", context).ok()
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let global = context.global_object();
    let original = global.get(js_string!("fetch"), context)?;
    let original = original
        .as_callable()
        .ok_or_else(|| type_error("DOM requires the runtime's fetch"))?;
    let request = global.get(js_string!("Request"), context)?;
    // The page loads its own subresources through this one, whatever it makes of `window.fetch`.
    shared(context).borrow_mut().load.original_fetch = Some(original.clone().into());
    shared(context).borrow_mut().load.original_response_url = response_url_getter(context);
    let captures = Captures {
        original,
        request,
        xhr: false,
    };
    let fetch = closure(context, "fetch", 0, page_fetch, captures.clone());
    shared(context).borrow_mut().load.dom_fetch = Some(fetch.clone().into());
    global.set(js_string!("fetch"), fetch, true, context)?;
    let xhr = Captures {
        original: captures.original.clone(),
        request: captures.request.clone(),
        xhr: true,
    };
    let xhr = closure(context, "fetch", 0, page_fetch, xhr);
    shared(context).borrow_mut().load.dom_xhr_fetch = Some(xhr.into());
    Ok(())
}

/// What the page's `fetch` (and the `XMLHttpRequest` that asks through its twin) keeps.
#[derive(Clone, Trace, Finalize)]
struct Captures {
    original: JsObject,
    request: JsValue,
    /// Asked by `XMLHttpRequest`, which `supportFetchAPI` does not limit.
    xhr: bool,
}

fn page_fetch(
    this: &JsValue,
    args: &[JsValue],
    captures: &Captures,
    context: &mut Context,
) -> JsResult<JsValue> {
    match fetch_of(this, args, captures, context) {
        Ok(value) => Ok(value),
        Err(error) => native::settled(Err(error), context),
    }
}

fn fetch_of(
    this: &JsValue,
    args: &[JsValue],
    captures: &Captures,
    context: &mut Context,
) -> JsResult<JsValue> {
    let input = arg(args, 0);
    let is_request = input.is_object() && input.instance_of(&captures.request, context)?;
    let address = if is_request {
        text_of(&get(&input, "url", context)?, context)?
    } else {
        text_of(&input, context)?
    };
    let url = shared(context)
        .borrow()
        .base_url()
        .join(&address)
        .map_err(|_| type_error(format!("Failed to parse URL from {address}")))?;
    if crate::load::claims(context, &url) {
        let method = request_method(args, &input, is_request, context)?;
        return crate::load::fetch_response(context, url, &method);
    }
    let forwarded = crate::load::forwards(context, &url);
    if forwarded && !captures.xhr && !crate::load::allows_fetch(context, &url) {
        return Err(type_error("Failed to fetch"));
    }
    // A page of a scheme with no origin is cross-origin to everything, itself included: its
    // `XMLHttpRequest` to a custom scheme fails (Electron 39.8.10, probe3 `bare`), where a
    // standard scheme's page reads its own.
    if forwarded && captures.xhr && opaque_page_asks_custom(context, &url) {
        return Err(type_error("Failed to fetch"));
    }
    if !forwarded && !crate::load::is_network(url.scheme()) {
        return Err(type_error("fetch does not support this URL scheme"));
    }
    // A `Request` is handed on as it is, or its body, signal and the original's checks are lost.
    let mut sent = args.to_vec();
    if !is_request {
        if sent.is_empty() {
            sent.push(js_text(url.as_str()));
        } else {
            sent[0] = js_text(url.as_str());
        }
    }
    captures.original.call(this, &sent, context)
}

/// Whether a page with no origin (a document of a non-standard custom scheme) asks `url`, a
/// custom scheme's address.
fn opaque_page_asks_custom(context: &Context, url: &url::Url) -> bool {
    let state = shared(context);
    let state = state.borrow();
    let page = state.base_url();
    !matches!(url.scheme(), "http" | "https" | "file" | "data" | "blob")
        && state.schemes.origin_of(&page) == "null"
}

/// The method a `fetch` asks for: the `Request`'s, else the init's, else `GET`.
fn request_method(
    args: &[boa_engine::JsValue],
    input: &boa_engine::JsValue,
    is_request: bool,
    context: &mut Context,
) -> JsResult<String> {
    let source = if is_request {
        input.clone()
    } else {
        arg(args, 1)
    };
    let method = get(&source, "method", context)?;
    if method.is_undefined() {
        return Ok("GET".to_owned());
    }
    Ok(text_of(&method, context)?.to_ascii_uppercase())
}
