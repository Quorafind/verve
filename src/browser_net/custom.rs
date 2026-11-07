//! A request the host answers itself: the response is built from what its [`Responder`] says,
//! and its body is a stream the reader's demand drives.
//!
//! [`Responder`]: super::Responder

use std::rc::Rc;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};

use super::{BrowserNet, Inbound, NetError, RequestHead, Responder, ResponseHead};
use crate::fetch::{Fetched, FetchedBody, OutgoingBody};
use crate::native::{closure, get, invoke, js_text, object};
use crate::network::fetch::{plain_error, type_error_with_cause};
use crate::streams::{error_value, reject, resolve, source_close, source_enqueue, source_error};

/// One request the host is answering.
pub(crate) struct Custom {
    net: Rc<dyn BrowserNet>,
    /// Until the host has been asked.
    head: Option<RequestHead>,
    method: String,
    url: String,
    /// The response promise's, until the head arrives.
    resolvers: Option<ResolvingFunctions>,
    signal: JsValue,
    on_abort: JsValue,
    /// The body's stream, once the head said there is one.
    stream: Option<JsObject>,
    /// The promise of the pull the host has not answered yet: the stream asks again only after the
    /// host sent a chunk or an end, so it pulls once for each chunk.
    pulling: Option<ResolvingFunctions>,
}

/// Starts the host answering `head`; `body` is read whole first when it is a stream.
pub(crate) fn begin(
    net: Rc<dyn BrowserNet>,
    head: RequestHead,
    body: OutgoingBody,
    signal: JsValue,
    resolvers: ResolvingFunctions,
    cx: &mut Context,
) -> JsResult<()> {
    let id = head.id;
    let custom = Custom {
        net,
        method: head.method.clone(),
        url: head.url.clone(),
        head: Some(head),
        resolvers: Some(resolvers),
        signal: signal.clone(),
        on_abort: JsValue::undefined(),
        stream: None,
        pulling: None,
    };
    crate::shared(cx)
        .browser
        .borrow_mut()
        .custom
        .insert(id, custom);
    if signal.is_object() {
        let listener: JsValue = closure(cx, "", 1, on_abort, id).into();
        invoke(
            &signal,
            "addEventListener",
            &[js_text("abort"), listener.clone()],
            cx,
        )?;
        if let Some(custom) = crate::shared(cx).browser.borrow_mut().custom.get_mut(&id) {
            custom.on_abort = listener;
        }
    }
    match body {
        OutgoingBody::None => ask(id, None, cx),
        OutgoingBody::Bytes(bytes) => ask(id, Some(bytes), cx),
        OutgoingBody::Stream(stream) => {
            let bytes = crate::fetch::read_stream(&stream, cx)?;
            crate::streams::when(
                cx,
                bytes,
                id,
                |id, bytes, cx| {
                    let bytes = crate::encoding::bytes_of(&bytes, cx)?.unwrap_or_default();
                    ask(*id, Some(bytes), cx);
                    Ok(JsValue::undefined())
                },
                |id, reason, cx| {
                    abort(*id, reason, cx);
                    Ok(JsValue::undefined())
                },
            )?;
        }
    }
    Ok(())
}

/// Hands the request to the host, unless it was given up on meanwhile.
fn ask(id: u64, body: Option<Vec<u8>>, cx: &mut Context) {
    let Some((net, head)) = crate::shared(cx)
        .browser
        .borrow_mut()
        .custom
        .get_mut(&id)
        .and_then(|custom| Some((Rc::clone(&custom.net), custom.head.take()?)))
    else {
        return;
    };
    let respond = Responder {
        ticket: id,
        inbox: super::inbox(cx),
        finished: false,
    };
    net.handle(head, body, respond, cx);
}

/// What the host said about request `id`.
pub(crate) fn heard(id: u64, what: Inbound, cx: &mut Context) {
    let result = match what {
        Inbound::Head(head) => respond(id, head, cx),
        Inbound::Chunk(bytes) => chunk(id, bytes, cx),
        Inbound::End => end(id, cx),
        Inbound::Fail(error) => {
            fail(id, &error, cx);
            Ok(())
        }
    };
    if let Err(error) = result {
        let reason = error_value(error, cx);
        abort(id, reason, cx);
    }
}

/// The head is in: the response resolves, its body a stream the host's chunks feed.
fn respond(id: u64, head: ResponseHead, cx: &mut Context) -> JsResult<()> {
    let (resolvers, method, url) = {
        let shared = crate::shared(cx);
        let mut state = shared.browser.borrow_mut();
        let Some(custom) = state.custom.get_mut(&id) else {
            return Ok(());
        };
        match custom.resolvers.take() {
            Some(resolvers) => (resolvers, custom.method.clone(), custom.url.clone()),
            None => return Ok(()),
        }
    };
    let null_body = method == "HEAD" || matches!(head.status, 101 | 103 | 204 | 205 | 304);
    let body = if null_body {
        // Nobody will read a body, so the host is told to let go of the answer.
        if let Some(custom) = remove(id, cx)? {
            custom.net.cancel(id, cx);
        }
        None
    } else {
        let source = object(cx);
        let pull = closure(cx, "pull", 1, pull_body, id);
        let cancel = closure(cx, "cancel", 1, cancel_body, id);
        source.set(js_string!("pull"), pull, false, cx)?;
        source.set(js_string!("cancel"), cancel, false, cx)?;
        let stream = crate::streams::new_readable(cx, &source.into(), &JsValue::undefined())?;
        if let Some(custom) = crate::shared(cx).browser.borrow_mut().custom.get_mut(&id) {
            custom.stream = Some(stream.clone());
        }
        Some(FetchedBody::Stream(stream))
    };
    let response = crate::fetch::fetched_response(
        Fetched {
            status: head.status,
            status_text: head.status_text,
            headers: head.headers,
            url,
            redirected: false,
            body,
        },
        cx,
    )?;
    resolve(&resolvers, response.into(), cx);
    Ok(())
}

fn chunk(id: u64, bytes: Vec<u8>, cx: &mut Context) -> JsResult<()> {
    let stream = crate::shared(cx)
        .browser
        .borrow()
        .custom
        .get(&id)
        .and_then(|custom| custom.stream.clone());
    if let Some(stream) = stream.filter(|_| !bytes.is_empty()) {
        let chunk = crate::streams::u8array(bytes, cx)?;
        source_enqueue(cx, &stream, chunk)?;
    }
    let pulling = crate::shared(cx)
        .browser
        .borrow_mut()
        .custom
        .get_mut(&id)
        .and_then(|custom| custom.pulling.take());
    if let Some(pulling) = pulling {
        resolve(&pulling, JsValue::undefined(), cx);
    }
    Ok(())
}

fn end(id: u64, cx: &mut Context) -> JsResult<()> {
    if let Some(custom) = remove(id, cx)? {
        match (custom.stream, custom.resolvers) {
            (Some(stream), _) => source_close(cx, &stream)?,
            // The host ended without a head: an empty answer is no answer.
            (None, Some(resolvers)) => {
                let error = NetError::from_code(-324);
                let cause = plain_error(cx, &error.name, Some(error.short_name()));
                let reason = super::gate::failure(cx, custom.net.failure_style(), cause, &error);
                reject(&resolvers, reason, cx);
            }
            (None, None) => {}
        }
    }
    Ok(())
}

/// The host could not answer: the host's [`FailureStyle`](super::FailureStyle) fails the response
/// before its head (`fetch failed`, `Failed to fetch`, …), and `terminated` after it.
fn fail(id: u64, error: &NetError, cx: &mut Context) {
    let Ok(Some(custom)) = remove(id, cx) else {
        return;
    };
    let cause = plain_error(cx, &error.name, Some(error.short_name()));
    if let Some(resolvers) = custom.resolvers {
        let reason = super::gate::failure(cx, custom.net.failure_style(), cause, error);
        reject(&resolvers, reason, cx);
    } else if let Some(stream) = custom.stream {
        let reason = type_error_with_cause(cx, "terminated", cause);
        source_error(cx, &stream, reason);
    }
}

/// Gives the request up: the response rejects, or its body errors, with `reason`, and the host is
/// told to stop.
fn abort(id: u64, reason: JsValue, cx: &mut Context) {
    let Ok(Some(custom)) = remove(id, cx) else {
        return;
    };
    if let Some(resolvers) = &custom.resolvers {
        reject(resolvers, reason.clone(), cx);
    }
    if let Some(stream) = &custom.stream {
        source_error(cx, stream, reason);
    }
    custom.net.cancel(id, cx);
}

/// Forgets request `id` and its signal's listener.
fn remove(id: u64, cx: &mut Context) -> JsResult<Option<Custom>> {
    let mut removed = crate::shared(cx).browser.borrow_mut().custom.remove(&id);
    if let Some(pulling) = removed.as_mut().and_then(|custom| custom.pulling.take()) {
        resolve(&pulling, JsValue::undefined(), cx);
    }
    if let Some(custom) = &removed
        && custom.on_abort.is_callable()
    {
        invoke(
            &custom.signal,
            "removeEventListener",
            &[js_text("abort"), custom.on_abort.clone()],
            cx,
        )?;
    }
    Ok(removed)
}

fn on_abort(_: &JsValue, _: &[JsValue], id: &u64, cx: &mut Context) -> JsResult<JsValue> {
    let signal = crate::shared(cx)
        .browser
        .borrow()
        .custom
        .get(id)
        .map(|custom| custom.signal.clone());
    if let Some(signal) = signal {
        let reason = get(&signal, "reason", cx)?;
        abort(*id, reason, cx);
    }
    Ok(JsValue::undefined())
}

/// The body's `pull`: the reader wants a chunk, which the host is asked for.
fn pull_body(_: &JsValue, _: &[JsValue], id: &u64, cx: &mut Context) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(cx);
    let net = {
        let shared = crate::shared(cx);
        let mut state = shared.browser.borrow_mut();
        state.custom.get_mut(id).map(|custom| {
            custom.pulling = Some(resolvers);
            Rc::clone(&custom.net)
        })
    };
    match net {
        Some(net) => net.pull(*id, cx),
        None => return Ok(JsValue::undefined()),
    }
    Ok(promise.into())
}

/// The body's `cancel`: the reader is done with it, so is the host.
fn cancel_body(_: &JsValue, _: &[JsValue], id: &u64, cx: &mut Context) -> JsResult<JsValue> {
    if let Some(custom) = remove(*id, cx)? {
        custom.net.cancel(*id, cx);
    }
    Ok(JsValue::undefined())
}
