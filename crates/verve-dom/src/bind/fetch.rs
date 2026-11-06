//! DOM 地址解析与传输分离；真正的请求仍经过安装前的 Verve fetch 和权限链。

use boa_engine::{Context, JsResult, js_string};
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
    let fetch = closure(
        context,
        "fetch",
        0,
        |this, args, (original, request), cx| {
            let result = (|| {
                let input = arg(args, 0);
                let is_request = input.is_object() && input.instance_of(request, cx)?;
                let address = if is_request {
                    text_of(&get(&input, "url", cx)?, cx)?
                } else {
                    text_of(&input, cx)?
                };
                let url = shared(cx)
                    .borrow()
                    .base_url()
                    .join(&address)
                    .map_err(|_| type_error(format!("Failed to parse URL from {address}")))?;
                if crate::load::claims(cx, &url) {
                    let method = request_method(args, &input, is_request, cx)?;
                    return crate::load::fetch_response(cx, url, &method);
                }
                if !crate::load::is_network(url.scheme()) {
                    return Err(type_error("fetch 不支持此地址协议"));
                }
                // Request 必须原样转交，否则会丢失 body、signal 和原实现的校验。
                let mut forwarded = args.to_vec();
                if !is_request {
                    if forwarded.is_empty() {
                        forwarded.push(js_text(url.as_str()));
                    } else {
                        forwarded[0] = js_text(url.as_str());
                    }
                }
                original.call(this, &forwarded, cx)
            })();
            match result {
                Ok(value) => Ok(value),
                Err(error) => native::settled(Err(error), cx),
            }
        },
        (original, request),
    );
    shared(context).borrow_mut().load.dom_fetch = Some(fetch.clone().into());
    global.set(js_string!("fetch"), fetch, true, context)?;
    Ok(())
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
