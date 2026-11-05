//! Asking for things: through the embedder's [`Loader`] when it answers the scheme, through the
//! realm's own `fetch` for the network's (`http`, `https`, `data`, `blob`), and the page's own
//! `fetch` the same way round.

use boa_engine::object::builtins::JsArrayBuffer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use boa_gc::{Finalize, Trace};
use url::Url;
use verve::native::{closure, get, invoke, js_text};

use super::{Destination, Inbox, LoadError, LoadRequest, LoadResult, Wait};
use crate::state::shared;

/// The schemes Verve's own `fetch` answers.
fn is_network(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "data" | "blob")
}

/// The text of a document or script body: UTF-8, a byte order mark dropped. (Other encodings
/// are P41's.)
pub(crate) fn text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Asks for `url`; the answer is taken by [`crate::Document::pump_loads`] and handed to `wait`.
pub(crate) fn start(context: &mut Context, url: Url, destination: Destination, wait: Wait) {
    let (token, inbox, loader) = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let load = &mut state.load;
        let token = load.next;
        load.next += 1;
        load.waiting.insert(token, wait);
        (token, load.inbox.clone(), load.loader.clone())
    };
    let initiator = match destination {
        Destination::Document => None,
        _ => Some(shared(context).borrow().url.clone()),
    };
    let request = LoadRequest {
        url,
        destination,
        initiator,
        method: "GET".into(),
        headers: Vec::new(),
        body: None,
    };
    send(context, request, inbox, token, loader);
}

fn send(
    context: &mut Context,
    request: LoadRequest,
    inbox: Inbox,
    token: u64,
    loader: Option<std::rc::Rc<dyn super::Loader>>,
) {
    let scheme = request.url.scheme().to_owned();
    let claimed = loader
        .as_ref()
        .is_some_and(|l| l.handles(&scheme) || !is_network(&scheme));
    match loader.filter(|_| claimed) {
        Some(loader) => loader.fetch(
            request,
            Box::new(move |result| inbox.borrow_mut().push((token, result))),
        ),
        None if is_network(&scheme) => through_fetch(context, request.url, inbox, token),
        None => inbox
            .borrow_mut()
            .push((token, LoadResult::failed(request.url, -2, "ERR_FAILED"))),
    }
}

/// Where a result goes when the realm's `fetch` has it.
#[derive(Trace, Finalize)]
struct Reply {
    #[unsafe_ignore_trace]
    inbox: Inbox,
    token: u64,
    #[unsafe_ignore_trace]
    url: Url,
    response: Option<JsObject>,
}

impl Reply {
    fn push(
        &self,
        status: u16,
        url: Option<Url>,
        mime: Option<String>,
        body: Result<Vec<u8>, LoadError>,
    ) {
        let result = LoadResult {
            status,
            url: url.unwrap_or_else(|| self.url.clone()),
            mime,
            headers: Vec::new(),
            body,
        };
        self.inbox.borrow_mut().push((self.token, result));
    }

    fn fail(&self) {
        let error = LoadError {
            code: -2,
            description: "ERR_FAILED".into(),
        };
        self.push(0, None, None, Err(error));
    }
}

fn through_fetch(context: &mut Context, url: Url, inbox: Inbox, token: u64) {
    let reply = Reply {
        inbox,
        token,
        url,
        response: None,
    };
    let started = (|| -> JsResult<()> {
        // The runtime's own `fetch`, not the page's: a page that wraps `window.fetch` does not
        // see its own scripts and sheets go by.
        let fetch = shared(context)
            .borrow()
            .load
            .original_fetch
            .clone()
            .unwrap_or_default();
        let promise = verve::call(
            context,
            &fetch,
            &JsValue::undefined(),
            &[js_text(reply.url.as_str())],
        )?;
        let on_response = closure(context, "response", 0, got_response, copy_of(&reply));
        let on_error = closure(context, "failed", 0, failed, copy_of(&reply));
        invoke(
            &promise,
            "then",
            &[on_response.into(), on_error.into()],
            context,
        )?;
        Ok(())
    })();
    if started.is_err() {
        reply.fail();
    }
}

fn copy_of(reply: &Reply) -> Reply {
    Reply {
        inbox: reply.inbox.clone(),
        token: reply.token,
        url: reply.url.clone(),
        response: reply.response.clone(),
    }
}

fn failed(_: &JsValue, _: &[JsValue], reply: &Reply, _: &mut Context) -> JsResult<JsValue> {
    reply.fail();
    Ok(JsValue::undefined())
}

/// The runtime's `fetch` answered. The page can replace what the chain calls on the response
/// (`Response.prototype.arrayBuffer`): a step that throws fails the load, which would else wait
/// for an answer that never comes.
fn got_response(
    _: &JsValue,
    args: &[JsValue],
    reply: &Reply,
    context: &mut Context,
) -> JsResult<JsValue> {
    if read_response(args, reply, context).is_err() {
        reply.fail();
    }
    Ok(JsValue::undefined())
}

fn read_response(args: &[JsValue], reply: &Reply, context: &mut Context) -> JsResult<()> {
    let response = args.first().and_then(JsValue::as_object);
    let Some(response) = response else {
        reply.fail();
        return Ok(());
    };
    let body = invoke(&response.clone().into(), "arrayBuffer", &[], context)?;
    let mut next = copy_of(reply);
    next.response = Some(response);
    let on_body = closure(context, "body", 0, got_body, copy_of(&next));
    let on_error = closure(context, "failed", 0, failed, next);
    invoke(&body, "then", &[on_body.into(), on_error.into()], context)?;
    Ok(())
}

fn got_body(
    _: &JsValue,
    args: &[JsValue],
    reply: &Reply,
    context: &mut Context,
) -> JsResult<JsValue> {
    if read_body(args, reply, context).is_err() {
        reply.fail();
    }
    Ok(JsValue::undefined())
}

fn read_body(args: &[JsValue], reply: &Reply, context: &mut Context) -> JsResult<()> {
    let response: JsValue = reply.response.clone().map(Into::into).unwrap_or_default();
    let status = get(&response, "status", context)?
        .to_u32(context)
        .unwrap_or(0) as u16;
    let headers = get(&response, "headers", context)?;
    let mime = invoke(&headers, "get", &[js_text("content-type")], context)?;
    let mime = mime.as_string().map(|m| m.to_std_string_escaped());
    // Where it came from after redirects: relative addresses in it resolve against that.
    // The runtime's own getter, not the page's: `Response.prototype.url` may be replaced.
    let getter = shared(context).borrow().load.original_response_url.clone();
    let finally = match getter.as_ref().and_then(JsValue::as_callable) {
        Some(getter) => getter.call(&response, &[], context)?,
        None => JsValue::undefined(),
    };
    let finally = finally
        .as_string()
        .and_then(|u| Url::parse(&u.to_std_string_escaped()).ok());
    let bytes = args
        .first()
        .and_then(JsValue::as_object)
        .and_then(|o| JsArrayBuffer::from_object(o.clone()).ok())
        .and_then(|buffer| buffer.data().map(|data| data.to_vec()));
    match bytes {
        Some(bytes) => reply.push(status, finally, mime, Ok(bytes)),
        None => reply.fail(),
    }
    Ok(())
}
