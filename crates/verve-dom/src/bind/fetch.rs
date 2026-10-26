//! DOM 地址解析与传输分离；真正的请求仍经过安装前的 Verve fetch 和权限链。

use boa_engine::{Context, JsResult, js_string};

use super::{arg, closure, get, rejected, string, text, type_error};
use crate::state::shared;

pub fn install(context: &mut Context) -> JsResult<()> {
    let global = context.global_object();
    let original = global.get(js_string!("fetch"), context)?;
    let original = original
        .as_callable()
        .ok_or_else(|| type_error("DOM requires the runtime's fetch"))?;
    let request = global.get(js_string!("Request"), context)?;
    let fetch = closure(
        context,
        "fetch",
        |this, args, (original, request), cx| {
            let result = (|| {
                let input = arg(args, 0);
                let is_request = input.is_object() && input.instance_of(request, cx)?;
                let address = if is_request {
                    string(&get(&input, "url", cx)?, cx)?
                } else {
                    string(&input, cx)?
                };
                let url = shared(cx)
                    .borrow()
                    .url
                    .join(&address)
                    .map_err(|_| type_error(format!("Failed to parse URL from {address}")))?;
                if !matches!(url.scheme(), "http" | "https" | "data" | "blob") {
                    return Err(type_error("fetch 不支持此地址协议"));
                }
                // Request 必须原样转交，否则会丢失 body、signal 和原实现的校验。
                let mut forwarded = args.to_vec();
                if !is_request {
                    if forwarded.is_empty() {
                        forwarded.push(text(url.as_str()));
                    } else {
                        forwarded[0] = text(url.as_str());
                    }
                }
                original.call(this, &forwarded, cx)
            })();
            match result {
                Ok(value) => Ok(value),
                Err(error) => rejected(error, cx),
            }
        },
        (original, request),
    );
    global.set(js_string!("fetch"), fetch, true, context)?;
    Ok(())
}
