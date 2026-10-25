//! `fetch`, on the realm's own HTTP client. A request goes out through `http.request` or
//! `https.request` — the same reactor, TLS, keepalive pooling and outbound
//! [`Permissions`](crate::Permissions) as Node's modules — so an embedder grants a host once and
//! both paths obey it. Nothing waits: the reactor's events reach the realm on the host's beat
//! ([`crate::Runtime::run_due`] and friends), as the `http` module's do, and `fetch` listens to
//! the `ClientRequest` and `IncomingMessage` they drive.
//!
//! The response's body is a `ReadableStream` fed as the bytes arrive, not when they have all
//! come: an SSE reply is read event by event (`body.pipeThrough(new TextDecoderStream())`). Its
//! queue holds one chunk; while it is full the message is paused, which pauses the socket, so a
//! reader that stops reading stops the network too. `content-encoding` (`gzip`, `deflate`,
//! `br`) is undone on the way, chunk by chunk.
//!
//! What undici does, this does: a host the permissions refuse, a name that does not resolve or a
//! connection refused rejects with `TypeError: fetch failed`, its `cause` the error underneath;
//! redirects are followed (up to 20), returned (`manual`) or refused (`error`), with the method,
//! body and credentials changed as the standard says; an `AbortSignal` rejects the response or
//! errors its body with the signal's `reason`, and closes the connection. A request body that is
//! a stream is read whole before sending. `data:` and `blob:` addresses are answered without any
//! network.

use boa_engine::{
    Context, JsData, JsError, JsObject, JsResult, JsString, JsValue,
    builtins::promise::ResolvingFunctions, js_string,
};
use boa_gc::{Finalize, Trace};
use std::io::Write;

use crate::fetch::{Fetched, FetchedBody, OutgoingBody};
use crate::native::{arg, closure, get, hidden, invoke, js_text, list_of, object, text_of};
use crate::streams::{error_value, pending, reject, resolve, when};

/// How long a kept-alive connection may sit unused before it is closed, as undici's default.
const IDLE_MS: u32 = 4_000;

/// The most redirects one fetch follows, as the standard says.
const MAX_REDIRECTS: u32 = 20;

/// The global `fetch`.
pub(crate) fn function(context: &mut Context) -> JsObject {
    crate::native::function(context, "fetch", 1, |_, args, cx| {
        let (promise, resolvers) = pending(cx);
        // Nothing is thrown: every failure, even a bad argument, is the promise's.
        if let Err(error) = begin(&arg(args, 0), &arg(args, 1), &resolvers, cx) {
            let reason = error_value(error, cx);
            reject(&resolvers, reason, cx);
        }
        Ok(promise.into())
    })
}

// ─── One fetch ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Redirect {
    Follow,
    Manual,
    Error,
}

/// What the request is now: the redirects followed may have changed every part of it.
struct Hop {
    url: url::Url,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    redirect: Redirect,
    /// Redirects followed so far; also which request's events are this hop's.
    redirects: u32,
    /// Answered and read to the end, failed, cancelled or aborted: nothing more to do.
    done: bool,
    /// The message is paused because the body's queue is full.
    paused: bool,
    decoders: Vec<Decoder>,
}

#[derive(Trace, Finalize, JsData)]
struct Fetch {
    /// The response promise's, until it settles.
    resolvers: Option<ResolvingFunctions>,
    signal: JsValue,
    on_abort: JsValue,
    /// This hop's `ClientRequest` and, once it answered, its `IncomingMessage`.
    request: JsValue,
    message: JsValue,
    /// The response's body, once there is a response with one.
    stream: Option<JsObject>,
    #[unsafe_ignore_trace]
    hop: Hop,
}

fn with<R>(fetch: &JsObject, f: impl FnOnce(&mut Fetch) -> R) -> R {
    f(&mut fetch.downcast_mut::<Fetch>().expect("a fetch"))
}

fn begin(
    input: &JsValue,
    init: &JsValue,
    resolvers: &ResolvingFunctions,
    cx: &mut Context,
) -> JsResult<()> {
    let outgoing = crate::fetch::outgoing(input, init, cx)?;
    let signal = outgoing.signal.clone();
    if get(&signal, "aborted", cx)?.to_boolean() {
        let reason = get(&signal, "reason", cx)?;
        reject(resolvers, reason, cx);
        return Ok(());
    }
    let url = url::Url::parse(&outgoing.url)
        .map_err(|_| crate::native::type_error(format!("Invalid URL: {}", outgoing.url)))?;
    match url.scheme() {
        "http" | "https" => {}
        "data" => return answer(local_data(&url), &url, resolvers, cx),
        "blob" => {
            let found = (outgoing.method == "GET")
                .then(|| crate::shared(cx).blobs.borrow().get(url.as_str()).cloned())
                .flatten();
            let local = found
                .map(|bytes| (String::new(), bytes))
                .ok_or_else(|| "invalid method or blob URL not found".to_owned());
            return answer(local, &url, resolvers, cx);
        }
        scheme => {
            let cause = plain_error(cx, &format!("unknown scheme '{scheme}:'"), None);
            let reason = fetch_failed(cx, cause);
            reject(resolvers, reason, cx);
            return Ok(());
        }
    }
    let redirect = match outgoing.redirect.as_str() {
        "manual" => Redirect::Manual,
        "error" => Redirect::Error,
        _ => Redirect::Follow,
    };
    let fetch = JsObject::from_proto_and_data(
        None,
        Fetch {
            resolvers: Some(resolvers.clone()),
            signal: signal.clone(),
            on_abort: JsValue::undefined(),
            request: JsValue::undefined(),
            message: JsValue::undefined(),
            stream: None,
            hop: Hop {
                url,
                method: outgoing.method,
                headers: outgoing.headers,
                body: None,
                redirect,
                redirects: 0,
                done: false,
                paused: false,
                decoders: Vec::new(),
            },
        },
    );
    if signal.is_object() {
        let listener: JsValue = closure(cx, "", 1, on_abort, fetch.clone()).into();
        invoke(
            &signal,
            "addEventListener",
            &[js_text("abort"), listener.clone()],
            cx,
        )?;
        with(&fetch, |f| f.on_abort = listener);
    }
    match outgoing.body {
        OutgoingBody::None => send(&fetch, cx),
        OutgoingBody::Bytes(bytes) => {
            with(&fetch, |f| f.hop.body = Some(bytes));
            send(&fetch, cx);
        }
        OutgoingBody::Stream(stream) => {
            let bytes = crate::fetch::read_stream(&stream, cx)?;
            when(
                cx,
                bytes,
                fetch,
                |fetch, bytes, cx| {
                    let bytes = crate::encoding::bytes_of(&bytes, cx)?.unwrap_or_default();
                    with(fetch, |f| f.hop.body = Some(bytes));
                    if !with(fetch, |f| f.hop.done) {
                        send(fetch, cx);
                    }
                    Ok(JsValue::undefined())
                },
                |fetch, reason, cx| {
                    fail(fetch, reason, cx);
                    Ok(JsValue::undefined())
                },
            )?;
        }
    }
    Ok(())
}

/// A `data:` or `blob:` address answered: its type and bytes, or why it cannot be.
fn answer(
    local: Result<(String, Vec<u8>), String>,
    url: &url::Url,
    resolvers: &ResolvingFunctions,
    cx: &mut Context,
) -> JsResult<()> {
    match local {
        Ok((kind, bytes)) => {
            let mut headers = vec![("content-length".to_owned(), bytes.len().to_string())];
            if !kind.is_empty() {
                headers.insert(0, ("content-type".to_owned(), kind));
            }
            let response = crate::fetch::fetched_response(
                Fetched {
                    status: 200,
                    status_text: "OK".into(),
                    headers,
                    url: url.as_str().to_owned(),
                    redirected: false,
                    body: Some(FetchedBody::Bytes(bytes)),
                },
                cx,
            )?;
            resolve(resolvers, response.into(), cx);
        }
        Err(message) => {
            let cause = plain_error(cx, &message, None);
            let reason = fetch_failed(cx, cause);
            reject(resolvers, reason, cx);
        }
    }
    Ok(())
}

/// Sends this hop's request; whatever stops it from going out fails the fetch.
fn send(fetch: &JsObject, cx: &mut Context) {
    if let Err(error) = start(fetch, cx) {
        let cause = error_value(error, cx);
        fail(fetch, cause, cx);
    }
}

fn start(fetch: &JsObject, cx: &mut Context) -> JsResult<()> {
    let (url, method, headers, body, hop) = with(fetch, |f| {
        (
            f.hop.url.clone(),
            f.hop.method.clone(),
            f.hop.headers.clone(),
            f.hop.body.clone(),
            f.hop.redirects,
        )
    });
    let https = url.scheme() == "https";
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url
        .port_or_known_default()
        .unwrap_or(if https { 443 } else { 80 });
    if !crate::shared(cx)
        .network
        .borrow()
        .permissions
        .allows_outbound(&host, port)
    {
        // Shaped as Node's permission model shapes its refusals.
        let resource = format!("{host}:{port}");
        let message = format!("Verve's permissions do not allow connecting to {resource}");
        let refusal = plain_error(cx, &message, Some("ERR_ACCESS_DENIED"));
        super::tcp::put(&refusal, "permission", js_text("net"), cx)?;
        super::tcp::put(&refusal, "resource", js_text(&resource), cx)?;
        return Err(JsError::from_opaque(refusal));
    }
    let module = crate::modules::node_module(cx, if https { "https" } else { "http" })?;
    let agent = agent(&module, cx)?;
    let options = object(cx);
    let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let headers = wire_headers(&headers, &method, body.as_deref(), cx)?;
    for (key, value) in [
        ("protocol", js_text(&format!("{}:", url.scheme()))),
        ("hostname", js_text(&host)),
        ("port", port.into()),
        ("path", js_text(if path.is_empty() { "/" } else { path })),
        ("method", js_text(&method)),
        ("agent", agent),
        ("headers", headers.into()),
    ] {
        options.set(JsString::from(key), value, false, cx)?;
    }
    let request = invoke(&module, "request", &[options.into()], cx)?;
    with(fetch, |f| {
        f.request = request.clone();
        f.message = JsValue::undefined();
    });
    for (event, listener) in [
        ("response", on_response as Listener),
        ("error", on_request_error),
    ] {
        let listener = closure(cx, "", 1, listener, (fetch.clone(), hop));
        invoke(&request, "on", &[js_text(event), listener.into()], cx)?;
    }
    // A connection from the pool is in use again: its idle timer stops.
    if get(&request, "reusedSocket", cx)?.to_boolean() {
        let socket = get(&request, "socket", cx)?;
        invoke(&socket, "setTimeout", &[0.into()], cx)?;
    }
    let end = match body {
        Some(bytes) => vec![crate::encoding::make_buffer(cx, bytes)?],
        None => Vec::new(),
    };
    invoke(&request, "end", &end, cx)?;
    Ok(())
}

type Listener = fn(&JsValue, &[JsValue], &(JsObject, u32), &mut Context) -> JsResult<JsValue>;
type BodyListener = fn(&JsValue, &[JsValue], &JsObject, &mut Context) -> JsResult<JsValue>;

/// The request's headers as `http.request` takes them, with what undici adds when they are not
/// there: `accept`, `accept-language`, `accept-encoding` (what this module decodes),
/// `user-agent`, and the body's `content-length`.
fn wire_headers(
    list: &[(String, String)],
    method: &str,
    body: Option<&[u8]>,
    cx: &mut Context,
) -> JsResult<JsObject> {
    let mut joined: Vec<(String, String)> = Vec::new();
    for (name, value) in list {
        match joined.iter_mut().find(|(n, _)| n == name) {
            Some((_, old)) => {
                old.push_str(", ");
                old.push_str(value);
            }
            None => joined.push((name.clone(), value.clone())),
        }
    }
    let length = match body {
        Some(bytes) => Some(bytes.len()),
        None if matches!(method, "POST" | "PUT" | "PATCH") => Some(0),
        None => None,
    };
    let defaults = [
        ("accept", Some("*/*".to_owned())),
        ("accept-language", Some("*".to_owned())),
        ("sec-fetch-mode", Some("cors".to_owned())),
        ("user-agent", Some("Verve".to_owned())),
        ("accept-encoding", Some("gzip, deflate, br".to_owned())),
        ("content-length", length.map(|n| n.to_string())),
    ];
    for (name, value) in defaults {
        if let Some(value) = value
            && !joined.iter().any(|(n, _)| n == name)
        {
            joined.push((name.to_owned(), value));
        }
    }
    let headers = object(cx);
    for (name, value) in joined {
        headers.set(JsString::from(name), js_text(&value), false, cx)?;
    }
    Ok(headers)
}

/// The agent `fetch` keeps per module: kept-alive connections, closed after [`IDLE_MS`] unused.
fn agent(module: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let kept = get(module, "__fetchAgent", cx)?;
    if kept.is_object() {
        return Ok(kept);
    }
    let class = get(module, "Agent", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("Agent"))?;
    let options = object(cx);
    options.set(js_string!("keepAlive"), true, false, cx)?;
    let agent = class.construct(&[options.into()], None, cx)?;
    // Undici's proxy rules: the realm's proxy, `HTTP_PROXY` standing in for `HTTPS_PROXY`.
    hidden(&agent, js_string!("__proxyRealm"), js_text("fetch"), cx)?;
    let agent: JsValue = agent.into();
    let free = crate::native::function(cx, "", 1, |_, args, cx| {
        let socket = arg(args, 0);
        if !get(&socket, "__fetchIdle", cx)?.to_boolean() {
            if let Some(target) = socket.as_object() {
                hidden(&target, js_string!("__fetchIdle"), true, cx)?;
            }
            let idle = crate::native::function(cx, "", 0, |this, _, cx| {
                // Still in the pool, unused: closed. One back at work keeps going.
                if get(this, "__httpRequest", cx)?.is_undefined() {
                    invoke(this, "destroy", &[], cx)?;
                }
                Ok(JsValue::undefined())
            });
            invoke(&socket, "on", &[js_text("timeout"), idle.into()], cx)?;
        }
        invoke(&socket, "setTimeout", &[IDLE_MS.into()], cx)?;
        Ok(JsValue::undefined())
    });
    invoke(&agent, "on", &[js_text("free"), free.into()], cx)?;
    if let Some(target) = module.as_object() {
        hidden(&target, js_string!("__fetchAgent"), agent.clone(), cx)?;
    }
    Ok(agent)
}

/// Whether a listener belongs to a hop the fetch has moved past, or to a fetch already over.
fn stale(fetch: &JsObject, hop: u32) -> bool {
    with(fetch, |f| f.hop.done || f.hop.redirects != hop)
}

fn on_request_error(
    _: &JsValue,
    args: &[JsValue],
    (fetch, hop): &(JsObject, u32),
    cx: &mut Context,
) -> JsResult<JsValue> {
    if !stale(fetch, *hop) {
        let cause = undici_cause(arg(args, 0), cx)?;
        fail(fetch, cause, cx);
    }
    Ok(JsValue::undefined())
}

fn on_response(
    _: &JsValue,
    args: &[JsValue],
    (fetch, hop): &(JsObject, u32),
    cx: &mut Context,
) -> JsResult<JsValue> {
    let message = arg(args, 0);
    if stale(fetch, *hop) {
        drain(&message, cx)?;
        return Ok(JsValue::undefined());
    }
    let status = get(&message, "statusCode", cx)?.as_number().unwrap_or(0.) as u16;
    let status_text = get(&message, "statusMessage", cx)?;
    let status_text = if status_text.is_undefined() {
        String::new()
    } else {
        text_of(&status_text, cx)?
    };
    let mut headers = Vec::new();
    for pair in list_of(&get(&message, "rawHeaders", cx)?, cx)?.chunks_exact(2) {
        headers.push((
            text_of(&pair[0], cx)?.to_ascii_lowercase(),
            text_of(&pair[1], cx)?.trim().to_owned(),
        ));
    }
    let location = headers
        .iter()
        .find(|(n, _)| n == "location")
        .map(|(_, v)| v.clone());
    let mode = with(fetch, |f| f.hop.redirect);
    if let Some(location) = location.filter(|_| matches!(status, 301 | 302 | 303 | 307 | 308)) {
        match mode {
            Redirect::Manual => {}
            Redirect::Error => {
                drain(&message, cx)?;
                let cause = plain_error(cx, "unexpected redirect", None);
                fail(fetch, cause, cx);
                return Ok(JsValue::undefined());
            }
            Redirect::Follow => {
                drain(&message, cx)?;
                match follow(fetch, status, &location) {
                    Ok(()) => send(fetch, cx),
                    Err(message) => {
                        let cause = plain_error(cx, &message, None);
                        fail(fetch, cause, cx);
                    }
                }
                return Ok(JsValue::undefined());
            }
        }
    }
    respond(fetch, &message, status, status_text, headers, cx)?;
    Ok(JsValue::undefined())
}

/// Reads and drops a message the fetch has no use for, so its connection can go back to the
/// pool.
fn drain(message: &JsValue, cx: &mut Context) -> JsResult<()> {
    let ignore = crate::native::function(cx, "", 1, |_, _, _| Ok(JsValue::undefined()));
    invoke(message, "on", &[js_text("error"), ignore.into()], cx)?;
    invoke(message, "resume", &[], cx)?;
    Ok(())
}

/// The next hop of a redirect, as the standard's HTTP-redirect fetch makes it.
fn follow(fetch: &JsObject, status: u16, location: &str) -> Result<(), String> {
    with(fetch, |f| {
        let hop = &mut f.hop;
        let next = hop
            .url
            .join(location)
            .map_err(|_| format!("invalid redirect location '{location}'"))?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err("URL scheme must be a HTTP(S) scheme".into());
        }
        if hop.redirects >= MAX_REDIRECTS {
            return Err("redirect count exceeded".into());
        }
        hop.redirects += 1;
        if ((status == 301 || status == 302) && hop.method == "POST")
            || (status == 303 && hop.method != "GET" && hop.method != "HEAD")
        {
            hop.method = "GET".into();
            hop.body = None;
            hop.headers.retain(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "content-encoding"
                        | "content-language"
                        | "content-location"
                        | "content-type"
                        | "content-length"
                )
            });
        }
        if next.origin() != hop.url.origin() {
            hop.headers.retain(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "authorization" | "proxy-authorization" | "cookie" | "host"
                )
            });
        }
        hop.url = next;
        Ok(())
    })
}

/// The response, resolved: its body a stream the message feeds, or none.
fn respond(
    fetch: &JsObject,
    message: &JsValue,
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    cx: &mut Context,
) -> JsResult<()> {
    let (method, mut url, redirects) = with(fetch, |f| {
        (f.hop.method.clone(), f.hop.url.clone(), f.hop.redirects)
    });
    url.set_fragment(None);
    let null_body = method == "HEAD" || matches!(status, 101 | 103 | 204 | 205 | 304);
    let body = if null_body {
        drain(message, cx)?;
        finish(fetch, cx)?;
        None
    } else {
        let encodings = headers
            .iter()
            .filter(|(n, _)| n == "content-encoding")
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let decoders = Decoder::chain(&encodings);
        let source = object(cx);
        let pull = closure(cx, "pull", 1, pull_body, fetch.clone());
        let cancel = closure(cx, "cancel", 1, cancel_body, fetch.clone());
        source.set(js_string!("pull"), pull, false, cx)?;
        source.set(js_string!("cancel"), cancel, false, cx)?;
        let stream = crate::streams::new_readable(cx, &source.into(), &JsValue::undefined())?;
        with(fetch, |f| {
            f.message = message.clone();
            f.stream = Some(stream.clone());
            f.hop.decoders = decoders;
        });
        for (event, listener) in [
            ("data", on_body_data as BodyListener),
            ("end", on_body_end),
            ("error", on_body_error),
        ] {
            let listener = closure(cx, "", 1, listener, fetch.clone());
            invoke(message, "on", &[js_text(event), listener.into()], cx)?;
        }
        Some(FetchedBody::Stream(stream))
    };
    let response = crate::fetch::fetched_response(
        Fetched {
            status,
            status_text,
            headers,
            url: url.to_string(),
            redirected: redirects > 0,
            body,
        },
        cx,
    )?;
    if let Some(resolvers) = with(fetch, |f| f.resolvers.take()) {
        resolve(&resolvers, response.into(), cx);
    }
    Ok(())
}

// ─── The body, as it arrives ────────────────────────────────────────────────────────────────

/// The body's stream, while the fetch is still going.
fn live_stream(fetch: &JsObject) -> Option<JsObject> {
    with(fetch, |f| if f.hop.done { None } else { f.stream.clone() })
}

fn on_body_data(
    _: &JsValue,
    args: &[JsValue],
    fetch: &JsObject,
    cx: &mut Context,
) -> JsResult<JsValue> {
    let Some(stream) = live_stream(fetch) else {
        return Ok(JsValue::undefined());
    };
    let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
    let decoded = with(fetch, |f| Decoder::run(&mut f.hop.decoders, &bytes, false));
    let bytes = match decoded {
        Ok(bytes) => bytes,
        Err(error) => {
            let cause = plain_error(cx, &error.to_string(), Some("Z_DATA_ERROR"));
            fail(fetch, cause, cx);
            return Ok(JsValue::undefined());
        }
    };
    if !bytes.is_empty() {
        let chunk = crate::streams::u8array(bytes, cx)?;
        if !crate::streams::source_enqueue(cx, &stream, chunk)? {
            return Ok(JsValue::undefined());
        }
    }
    // A full queue pauses the message — and with it the socket — until the reader pulls.
    if crate::streams::source_desired_size(&stream).is_some_and(|size| size <= 0.) {
        let message = with(fetch, |f| {
            f.hop.paused = true;
            f.message.clone()
        });
        invoke(&message, "pause", &[], cx)?;
    }
    Ok(JsValue::undefined())
}

fn on_body_end(
    _: &JsValue,
    _: &[JsValue],
    fetch: &JsObject,
    cx: &mut Context,
) -> JsResult<JsValue> {
    let Some(stream) = live_stream(fetch) else {
        return Ok(JsValue::undefined());
    };
    match with(fetch, |f| Decoder::run(&mut f.hop.decoders, &[], true)) {
        Ok(rest) => {
            if !rest.is_empty() {
                let chunk = crate::streams::u8array(rest, cx)?;
                crate::streams::source_enqueue(cx, &stream, chunk)?;
            }
            finish(fetch, cx)?;
            crate::streams::source_close(cx, &stream)?;
        }
        Err(error) => {
            let cause = plain_error(cx, &error.to_string(), Some("Z_DATA_ERROR"));
            fail(fetch, cause, cx);
        }
    }
    Ok(JsValue::undefined())
}

fn on_body_error(
    _: &JsValue,
    args: &[JsValue],
    fetch: &JsObject,
    cx: &mut Context,
) -> JsResult<JsValue> {
    if !with(fetch, |f| f.hop.done) {
        fail(fetch, arg(args, 0), cx);
    }
    Ok(JsValue::undefined())
}

/// The source's `pull`: the reader wants more, so a paused message flows again.
fn pull_body(_: &JsValue, _: &[JsValue], fetch: &JsObject, cx: &mut Context) -> JsResult<JsValue> {
    let message = with(fetch, |f| {
        std::mem::take(&mut f.hop.paused).then(|| f.message.clone())
    });
    if let Some(message) = message {
        invoke(&message, "resume", &[], cx)?;
    }
    Ok(JsValue::undefined())
}

/// The source's `cancel`: the reader is done with the body, so is the connection.
fn cancel_body(
    _: &JsValue,
    _: &[JsValue],
    fetch: &JsObject,
    cx: &mut Context,
) -> JsResult<JsValue> {
    if !with(fetch, |f| f.hop.done) {
        finish(fetch, cx)?;
        teardown(fetch, cx)?;
    }
    Ok(JsValue::undefined())
}

// ─── Endings ────────────────────────────────────────────────────────────────────────────────

/// The signal's `abort`: the response rejects, or its body errors, with the signal's reason.
fn on_abort(_: &JsValue, _: &[JsValue], fetch: &JsObject, cx: &mut Context) -> JsResult<JsValue> {
    if with(fetch, |f| f.hop.done) {
        return Ok(JsValue::undefined());
    }
    let signal = with(fetch, |f| f.signal.clone());
    let reason = get(&signal, "reason", cx)?;
    end_with(fetch, reason, cx);
    Ok(JsValue::undefined())
}

/// The fetch failed with `cause` underneath: `TypeError: fetch failed` before the response,
/// `TypeError: terminated` once its body was on its way — undici's two.
fn fail(fetch: &JsObject, cause: JsValue, cx: &mut Context) {
    if with(fetch, |f| f.hop.done) {
        return;
    }
    let answered = with(fetch, |f| f.resolvers.is_none());
    let reason = if answered {
        type_error_with_cause(cx, "terminated", cause)
    } else {
        fetch_failed(cx, cause)
    };
    end_with(fetch, reason, cx);
}

/// Rejects the response or errors the body with `reason`, and closes the connection.
fn end_with(fetch: &JsObject, reason: JsValue, cx: &mut Context) {
    let (resolvers, stream) = with(fetch, |f| (f.resolvers.take(), f.stream.clone()));
    if let Some(resolvers) = resolvers {
        reject(&resolvers, reason.clone(), cx);
    }
    if let Some(stream) = stream {
        crate::streams::source_error(cx, &stream, reason);
    }
    let _ = finish(fetch, cx);
    let _ = teardown(fetch, cx);
}

/// Nothing more will happen: the signal no longer matters.
fn finish(fetch: &JsObject, cx: &mut Context) -> JsResult<()> {
    let (signal, listener) = with(fetch, |f| {
        f.hop.done = true;
        (f.signal.clone(), std::mem::take(&mut f.on_abort))
    });
    if listener.is_callable() {
        invoke(
            &signal,
            "removeEventListener",
            &[js_text("abort"), listener],
            cx,
        )?;
    }
    Ok(())
}

/// Closes this hop's connection: it is not handed back to the pool half read.
fn teardown(fetch: &JsObject, cx: &mut Context) -> JsResult<()> {
    let request = with(fetch, |f| f.request.clone());
    if request.is_object() {
        let socket = get(&request, "socket", cx)?;
        if socket.is_object() {
            invoke(&socket, "destroy", &[], cx)?;
        }
    }
    Ok(())
}

// ─── Errors ─────────────────────────────────────────────────────────────────────────────────

/// An `Error` with `message`, and Node's `code` when there is one.
fn plain_error(cx: &mut Context, message: &str, code: Option<&str>) -> JsValue {
    match code {
        Some(code) => crate::native::coded_error_value(cx, message, code),
        None => crate::native::construct_global(cx, "Error", &[js_text(message)])
            .map(JsValue::from)
            .unwrap_or_default(),
    }
}

fn type_error_with_cause(cx: &mut Context, message: &str, cause: JsValue) -> JsValue {
    let options = object(cx);
    let _ = options.set(js_string!("cause"), cause, false, cx);
    crate::native::construct_global(cx, "TypeError", &[js_text(message), options.into()])
        .map(JsValue::from)
        .unwrap_or_default()
}

/// A proxy's refusal to tunnel, as undici's `ProxyAgent` reports it; any other error as it is.
fn undici_cause(error: JsValue, cx: &mut Context) -> JsResult<JsValue> {
    if crate::print(&get(&error, "code", cx)?) != "ERR_PROXY_TUNNEL" {
        return Ok(error);
    }
    let status = crate::print(&get(&error, "statusCode", cx)?);
    let message = format!("Proxy response ({status}) !== 200 when HTTP Tunneling");
    let cause = plain_error(cx, &message, Some("UND_ERR_ABORTED"));
    super::tcp::put(&cause, "name", js_text("RequestAbortedError"), cx)?;
    super::tcp::put(&cause, "cause", error, cx)?;
    Ok(cause)
}

/// `TypeError: fetch failed`, as undici rejects with for any network error.
fn fetch_failed(cx: &mut Context, cause: JsValue) -> JsValue {
    type_error_with_cause(cx, "fetch failed", cause)
}

// ─── data: ──────────────────────────────────────────────────────────────────────────────────

/// A `data:` address's type and bytes, by the standard's data: URL processor.
fn local_data(url: &url::Url) -> Result<(String, Vec<u8>), String> {
    let mut url = url.clone();
    url.set_fragment(None);
    let rest = &url.as_str()["data:".len()..];
    let (kind, body) = rest
        .split_once(',')
        .ok_or_else(|| "invalid data: URL".to_owned())?;
    let mut kind = kind
        .trim_matches([' ', '\t', '\n', '\r', '\x0c'])
        .to_owned();
    let mut bytes = percent_decode(body);
    let lower = kind.to_ascii_lowercase();
    if let Some(head) = lower.strip_suffix("base64")
        && head.trim_end_matches(' ').ends_with(';')
    {
        let text: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        bytes = crate::encoding::unbase64(&text)
            .ok_or_else(|| "invalid base64 in data: URL".to_owned())?;
        let cut = head.trim_end_matches(' ').len() - 1;
        kind.truncate(cut);
    }
    if kind.starts_with(';') {
        kind.insert_str(0, "text/plain");
    }
    if kind.is_empty() || !kind.contains('/') {
        kind = "text/plain;charset=US-ASCII".into();
    }
    Ok((kind, bytes))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let hex = |b: Option<&u8>| b.and_then(|b| (*b as char).to_digit(16));
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(high), Some(low)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2)))
        {
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

// ─── content-encoding ───────────────────────────────────────────────────────────────────────

/// One coding undone, chunk by chunk.
enum Decoder {
    Gzip(flate2::write::MultiGzDecoder<Vec<u8>>),
    Zlib(flate2::write::ZlibDecoder<Vec<u8>>),
    Raw(flate2::write::DeflateDecoder<Vec<u8>>),
    /// `deflate` before its first two bytes say which: servers send both zlib-wrapped and raw
    /// deflate under that name, and undici reads both.
    Deflate(Vec<u8>),
    Brotli(Box<brotli::DecompressorWriter<Vec<u8>>>),
}

impl Decoder {
    /// The decoders for a `content-encoding` value, the last coding applied first undone. An
    /// unknown coding leaves the body as it came, as undici does.
    fn chain(value: &str) -> Vec<Decoder> {
        let mut decoders = Vec::new();
        for coding in value.split(',').rev() {
            match coding.trim().to_ascii_lowercase().as_str() {
                "gzip" | "x-gzip" => decoders.push(Decoder::Gzip(
                    flate2::write::MultiGzDecoder::new(Vec::new()),
                )),
                "deflate" => decoders.push(Decoder::Deflate(Vec::new())),
                "br" => decoders.push(Decoder::Brotli(Box::new(brotli::DecompressorWriter::new(
                    Vec::new(),
                    4096,
                )))),
                "" | "identity" => {}
                _ => return Vec::new(),
            }
        }
        decoders
    }

    /// `bytes` through every decoder in turn; `last` finishes them.
    fn run(chain: &mut [Decoder], bytes: &[u8], last: bool) -> std::io::Result<Vec<u8>> {
        let mut bytes = bytes.to_vec();
        for decoder in chain {
            bytes = decoder.write(&bytes)?;
            if last {
                bytes.extend(decoder.finish()?);
            }
        }
        Ok(bytes)
    }

    fn write(&mut self, bytes: &[u8]) -> std::io::Result<Vec<u8>> {
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        match self {
            Decoder::Deflate(seen) => {
                seen.extend_from_slice(bytes);
                if seen.len() < 2 {
                    return Ok(Vec::new());
                }
                let header = (u16::from(seen[0]) << 8) | u16::from(seen[1]);
                let zlib = seen[0] & 0x0f == 8 && header % 31 == 0;
                let seen = std::mem::take(seen);
                *self = if zlib {
                    Decoder::Zlib(flate2::write::ZlibDecoder::new(Vec::new()))
                } else {
                    Decoder::Raw(flate2::write::DeflateDecoder::new(Vec::new()))
                };
                self.write(&seen)
            }
            Decoder::Gzip(d) => {
                d.write_all(bytes)?;
                d.flush()?;
                Ok(std::mem::take(d.get_mut()))
            }
            Decoder::Zlib(d) => {
                d.write_all(bytes)?;
                d.flush()?;
                Ok(std::mem::take(d.get_mut()))
            }
            Decoder::Raw(d) => {
                d.write_all(bytes)?;
                d.flush()?;
                Ok(std::mem::take(d.get_mut()))
            }
            Decoder::Brotli(d) => {
                d.write_all(bytes)?;
                d.flush()?;
                Ok(std::mem::take(d.get_mut()))
            }
        }
    }

    fn finish(&mut self) -> std::io::Result<Vec<u8>> {
        match self {
            Decoder::Deflate(seen) if seen.is_empty() => Ok(Vec::new()),
            Decoder::Deflate(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "truncated deflate body",
            )),
            Decoder::Gzip(d) => {
                d.try_finish()?;
                Ok(std::mem::take(d.get_mut()))
            }
            Decoder::Zlib(d) => {
                d.try_finish()?;
                Ok(std::mem::take(d.get_mut()))
            }
            Decoder::Raw(d) => {
                d.try_finish()?;
                Ok(std::mem::take(d.get_mut()))
            }
            Decoder::Brotli(d) => {
                d.close()?;
                Ok(std::mem::take(d.get_mut()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoders_undo_each_coding_in_pieces() {
        let text = b"data: one\n\ndata: two\n\n".repeat(50);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&text).unwrap();
        let gzip = gzip.finish().unwrap();
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zlib.write_all(&text).unwrap();
        let zlib = zlib.finish().unwrap();
        let mut raw =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        raw.write_all(&text).unwrap();
        let raw = raw.finish().unwrap();
        let mut br = Vec::new();
        {
            let mut w = brotli::CompressorWriter::new(&mut br, 4096, 5, 22);
            w.write_all(&text).unwrap();
        }
        for (coding, body) in [
            ("gzip", gzip),
            ("deflate", zlib),
            ("deflate", raw),
            ("br", br),
        ] {
            let mut chain = Decoder::chain(coding);
            let mut out = Vec::new();
            for piece in body.chunks(7) {
                out.extend(Decoder::run(&mut chain, piece, false).unwrap());
            }
            out.extend(Decoder::run(&mut chain, &[], true).unwrap());
            assert_eq!(out, text, "{coding}");
        }
        assert!(Decoder::chain("compress").is_empty());
    }

    #[test]
    fn data_urls_decode_as_the_standard_says() {
        let parse = |s: &str| local_data(&url::Url::parse(s).unwrap()).unwrap();
        assert_eq!(
            parse("data:,Hello%2C%20World"),
            (
                "text/plain;charset=US-ASCII".into(),
                b"Hello, World".to_vec()
            )
        );
        assert_eq!(
            parse("data:text/plain;base64,SGVsbG8=#frag"),
            ("text/plain".into(), b"Hello".to_vec())
        );
        assert_eq!(
            parse("data:;charset=utf-8,x"),
            ("text/plain;charset=utf-8".into(), b"x".to_vec())
        );
        assert!(local_data(&url::Url::parse("data:;base64,%%%").unwrap()).is_err());
    }
}
