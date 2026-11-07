//! `net.request`: Electron's `ClientRequest` (a `Writable`) and the `IncomingMessage` (a
//! `Readable`) of its `'response'` (`lib/common/api/net-client-request.ts` of Electron 39.8.10).
//!
//! The request is collected as it is written and sent by `end()` — chunked or not, the server
//! gets the whole body — through the kernel's `fetch` asked of the session's
//! [`MainNet`](super::main_net), with redirects left to this file, because Electron tells the
//! script of every one: `'redirect'` with the status, the new method and address, and the
//! response's headers, whatever the policy (`follow` goes on by itself, `manual` goes on if the
//! listener called `followRedirect()` before returning, `error` fails the request).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::object::builtins::JsUint8Array;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::browser_net::{BrowserNet, InternalFetch, ResourceType, fetch_internal};
use verve::native::{self, ClassSpec, NewOnly, arg, get, invoke, js_text, object};

use super::define::{self, Fields, Kind};
use super::main_net::MainNet;
use crate::hub::Session;

const REQUEST: Kind = Kind("ClientRequest");
const MESSAGE: Kind = Kind("IncomingMessage");

/// The most redirects one request follows, as Chromium does.
const MAX_REDIRECTS: u32 = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    Follow,
    Error,
    Manual,
}

/// One request's state.
struct Req {
    session: Arc<Session>,
    method: String,
    url: String,
    policy: Policy,
    /// Lowercased name, and the name and value as the script gave them.
    headers: Vec<(String, String, String)>,
    bypass: bool,
    /// Whether the session's cookies go with the request: `credentials: 'include'`, or
    /// `useSessionCookies` when `credentials` is not given.
    cookies: bool,
    started: bool,
    first_write: bool,
    aborted: bool,
    chunked: Option<bool>,
    body: Vec<u8>,
    redirects: u32,
    /// Whether the listener of `'redirect'` has called `followRedirect()`, while it listens.
    waiting_for_follow: Option<bool>,
    controller: Option<JsObject>,
}

/// The requests the script made, by the id their object is bound to.
#[derive(Default)]
pub(crate) struct Requests {
    next: Cell<u32>,
    table: RefCell<HashMap<u32, Req>>,
    class: RefCell<Option<JsObject>>,
    message_class: RefCell<Option<JsObject>>,
    /// The bodies the `IncomingMessage`s read, by the id their object is bound to.
    readers: RefCell<HashMap<u32, BodyReader>>,
}

/// The reader of a response's body, and whether a read of it is under way.
struct BodyReader {
    reader: JsValue,
    reading: bool,
}

/// `new ClientRequest(options, callback)`.
pub(crate) fn new_request(
    context: &mut Context,
    options: &JsValue,
    callback: &JsValue,
) -> JsResult<JsObject> {
    let class = request_class(context)?;
    class.construct(&[options.clone(), callback.clone()], None, context)
}

fn request_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.requests.class.borrow().clone() {
        return Ok(class);
    }
    let writable = native::get(&verve::require(context, "stream")?, "Writable", context)?
        .as_object()
        .ok_or_else(|| native::type_error("stream.Writable"))?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "ClientRequest",
            length: 2,
            init: init_request,
            parent: Some(&writable),
            methods: &[
                ("setHeader", 2, set_header),
                ("getHeader", 1, get_header),
                ("removeHeader", 1, remove_header),
                ("_write", 3, write_chunk),
                ("_final", 1, final_chunk),
                ("abort", 0, abort),
                ("followRedirect", 0, follow_redirect),
                ("getUploadProgress", 0, upload_progress),
            ],
            accessors: &[("chunkedEncoding", get_chunked, Some(set_chunked))],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    *state.net.requests.class.borrow_mut() = Some(class.clone());
    Ok(class)
}

fn init_request(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let writable = native::get(&verve::require(context, "stream")?, "Writable", context)?;
    let options = object(context);
    options.set(js_string!("autoDestroy"), true, false, context)?;
    verve::call(context, &writable, &this.clone().into(), &[options.into()])?;
    if arg(args, 1).is_callable() {
        invoke(
            &this.clone().into(),
            "once",
            &[js_text("response"), arg(args, 1)],
            context,
        )?;
    }
    let req = parse_options(&arg(args, 0), context)?;
    let state = super::shared(context);
    let id = state.net.requests.next.get() + 1;
    state.net.requests.next.set(id);
    state.net.requests.table.borrow_mut().insert(id, req);
    define::bind(this, REQUEST, id, context);
    Ok(())
}

/// Whether the request sends the session's cookies. `credentials` decides when it is given
/// (`same-origin` needs the request's `origin` to be the address's own); without it, only
/// `useSessionCookies` does (observed in Electron 39.8.10: a plain `net.request` sends none).
fn sends_cookies(fields: &Fields, url: &str, context: &mut Context) -> JsResult<bool> {
    let origin_of = |text: &str| url::Url::parse(text).map(|u| u.origin().ascii_serialization());
    Ok(match fields.string("credentials", context)?.as_deref() {
        Some("include") => true,
        Some("same-origin") => match fields.string("origin", context)? {
            Some(origin) => origin_of(&origin).ok() == origin_of(url).ok(),
            None => false,
        },
        Some(_) => false,
        None => fields.bool("useSessionCookies", false, context)?,
    })
}

/// What `parseOptions` of `net-client-request.ts` makes of the constructor's argument.
fn parse_options(given: &JsValue, context: &mut Context) -> JsResult<Req> {
    let object_given = given.as_object();
    let fields = Fields::strict(object_given.as_ref(), "options");
    let mut url = match given.as_string() {
        Some(text) => Some(text.to_std_string_escaped()),
        None => fields.string("url", context)?,
    };
    if url.is_none() {
        url = Some(compose_url(&fields, context)?);
    }
    let url = url.unwrap_or_default();
    let policy = match fields.string("redirect", context)?.as_deref() {
        None | Some("follow") => Policy::Follow,
        Some("error") => Policy::Error,
        Some("manual") => Policy::Manual,
        Some(_) => {
            return Err(define::error(
                "redirect mode should be one of follow, error or manual",
            ));
        }
    };
    let mut headers = Vec::new();
    if let Some(given) = fields.object("headers", context)? {
        for (name, value) in define::entries_of(&given, context)? {
            validate_header(&js_text(&name), &value, context)?;
            let text = native::text_of(&value, context)?;
            headers.push((name.to_ascii_lowercase(), name, text));
        }
    }
    let parsed = native::construct_global(context, "URL", &[js_text(&url)])
        .map_err(|_| native::coded_type_error(context, "Invalid URL", "ERR_INVALID_URL"))?;
    let protocol = native::text_of(&get(&parsed.into(), "protocol", context)?, context)?;
    if protocol != "http:" && protocol != "https:" {
        return Err(define::error(
            "ClientRequest only supports http: and https: protocols",
        ));
    }
    let hub = Arc::clone(&super::shared(context).hub);
    let session = match fields.object("session", context)? {
        Some(object) => {
            let id = define::id_of(&object.into(), super::session::SESSION, context).ok_or_else(
                || native::type_error("`session` should be an instance of the Session class"),
            )?;
            hub.session_by_id(id).ok_or_else(define::destroyed)?
        }
        None => match fields.string("partition", context)? {
            Some(partition) => hub.session_for(Some(&partition)),
            None => hub.session_for(None),
        },
    };
    let method = fields
        .string("method", context)?
        .unwrap_or_else(|| "GET".into());
    let cookies = sends_cookies(&fields, &url, context)?;
    Ok(Req {
        session,
        method: method.to_ascii_uppercase(),
        url,
        policy,
        headers,
        bypass: fields.bool("bypassCustomProtocolHandlers", false, context)?,
        cookies,
        started: false,
        first_write: false,
        aborted: false,
        chunked: None,
        body: Vec::new(),
        redirects: 0,
        waiting_for_follow: None,
        controller: None,
    })
}

/// `{ protocol, host | hostname, port, path }` as an address.
fn compose_url(fields: &Fields, context: &mut Context) -> JsResult<String> {
    let protocol = fields
        .string("protocol", context)?
        .unwrap_or_else(|| "http:".into());
    let protocol = if protocol.ends_with(':') {
        protocol
    } else {
        format!("{protocol}:")
    };
    let host = match fields.string("host", context)? {
        Some(host) => host,
        None => {
            let hostname = fields
                .string("hostname", context)?
                .unwrap_or_else(|| "localhost".into());
            match fields.f64_opt("port", context)? {
                Some(port) => format!("{hostname}:{port}"),
                None => hostname,
            }
        }
    };
    let path = fields
        .string("path", context)?
        .unwrap_or_else(|| "/".into());
    if path.contains(' ') {
        return Err(native::type_error(
            "Request path contains unescaped characters",
        ));
    }
    Ok(format!("{protocol}//{host}{path}"))
}

/// `validateHeader` of the same file.
fn validate_header(name: &JsValue, value: &JsValue, context: &mut Context) -> JsResult<()> {
    let Some(text) = name.as_string().map(|n| n.to_std_string_escaped()) else {
        return Err(native::type_error(
            "`name` should be a string in setHeader(name, value)",
        ));
    };
    if value.is_null_or_undefined() {
        return Err(define::error(&format!(
            "`value` required in setHeader(\"{text}\", value)"
        )));
    }
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if text.is_empty() || !text.chars().all(token) {
        return Err(define::error(&format!("Invalid header name: '{text}'")));
    }
    let shown = native::text_of(value, context)?;
    if shown.contains(['\r', '\n', '\0']) {
        return Err(define::error(&format!(
            "Invalid value for header '{text}': '{shown}'"
        )));
    }
    Ok(())
}

/// Runs `with` on the request `this` stands for.
fn with_req<R>(
    this: &JsValue,
    context: &mut Context,
    with: impl FnOnce(&mut Req) -> R,
) -> JsResult<R> {
    let state = super::shared(context);
    let (_, ()) = define::live(this, REQUEST, context, |_| Some(()))?;
    let id = define::id_of(this, REQUEST, context).ok_or_else(define::destroyed)?;
    let mut table = state.net.requests.table.borrow_mut();
    Ok(with(table.get_mut(&id).ok_or_else(define::destroyed)?))
}

fn set_header(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let late = with_req(this, context, |req| req.started || req.first_write)?;
    if late {
        return Err(define::error("Can't set headers after they are sent"));
    }
    let (name, value) = (arg(args, 0), arg(args, 1));
    validate_header(&name, &value, context)?;
    let (name, value) = (
        native::text_of(&name, context)?,
        native::text_of(&value, context)?,
    );
    with_req(this, context, |req| {
        let lower = name.to_ascii_lowercase();
        req.headers.retain(|(known, _, _)| *known != lower);
        req.headers.push((lower, name, value));
    })?;
    Ok(JsValue::undefined())
}

fn get_header(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if arg(args, 0).is_null_or_undefined() {
        return Err(define::error("`name` is required for getHeader(name)"));
    }
    let name = native::text_of(&arg(args, 0), context)?.to_ascii_lowercase();
    let found = with_req(this, context, |req| {
        req.headers
            .iter()
            .find(|(lower, _, _)| *lower == name)
            .map(|(_, _, value)| value.clone())
    })?;
    Ok(found.map_or(JsValue::undefined(), |value| js_text(&value)))
}

fn remove_header(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if arg(args, 0).is_null_or_undefined() {
        return Err(define::error("`name` is required for removeHeader(name)"));
    }
    let name = native::text_of(&arg(args, 0), context)?.to_ascii_lowercase();
    let late = with_req(this, context, |req| {
        let late = req.started || req.first_write;
        if !late {
            req.headers.retain(|(lower, _, _)| *lower != name);
        }
        late
    })?;
    if late {
        return Err(define::error("Can't remove headers after they are sent"));
    }
    Ok(JsValue::undefined())
}

fn get_chunked(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(with_req(this, context, |req| req.chunked.unwrap_or(false))?.into())
}

fn set_chunked(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let on = arg(args, 0).to_boolean();
    let outcome = with_req(this, context, |req| {
        if req.started {
            return Err("chunkedEncoding can only be set before the request is started");
        }
        if req.chunked.is_some() {
            return Err("chunkedEncoding can only be set once");
        }
        req.chunked = Some(on);
        Ok(())
    })?;
    outcome.map_err(define::error)?;
    Ok(JsValue::undefined())
}

fn upload_progress(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = [
        ("active", false.into()),
        ("started", false.into()),
        ("current", 0.into()),
        ("total", 0.into()),
    ];
    Ok(define::record(context, &record)?.into())
}

/// `_write`: the chunk is kept until `end()`.
fn write_chunk(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let chunk = arg(args, 0);
    let bytes = match chunk.as_string() {
        Some(text) => text.to_std_string_escaped().into_bytes(),
        None => verve::encoding::bytes_of(&chunk, context)?.unwrap_or_default(),
    };
    with_req(this, context, |req| {
        req.first_write = true;
        req.body.extend(bytes);
    })?;
    verve::call(context, &arg(args, 2), &JsValue::undefined(), &[])
}

/// `_final`: the request is sent now, whether anything was written or not.
fn final_chunk(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    start(context, this)?;
    verve::call(context, &arg(args, 0), &JsValue::undefined(), &[])
}

fn abort(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let first = with_req(this, context, |req| {
        !std::mem::replace(&mut req.aborted, true)
    })?;
    if first {
        let announce = native::closure(
            context,
            "",
            0,
            |_, _, this: &JsValue, cx| invoke(this, "emit", &[js_text("abort")], cx),
            this.clone(),
        );
        let process = native::global(context, "process")?;
        invoke(&process, "nextTick", &[announce.into()], context)?;
    }
    die(context, this, None)?;
    Ok(JsValue::undefined())
}

fn follow_redirect(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let waiting = with_req(this, context, |req| match req.waiting_for_follow.as_mut() {
        Some(followed) => {
            *followed = true;
            true
        }
        None => false,
    })?;
    if waiting {
        return Ok(JsValue::undefined());
    }
    Err(define::error(
        "followRedirect() called, but was not waiting for a redirect",
    ))
}

/// Ends the request: the error is emitted when the request is destroyed already, the request
/// is destroyed, and what is in flight is cancelled.
fn die(context: &mut Context, this: &JsValue, error: Option<JsValue>) -> JsResult<()> {
    let destroyed = get(this, "destroyed", context)?.to_boolean();
    if destroyed && let Some(error) = &error {
        invoke(this, "emit", &[js_text("error"), error.clone()], context)?;
    }
    invoke(
        this,
        "destroy",
        &error.iter().cloned().collect::<Vec<_>>(),
        context,
    )?;
    let controller = with_req(this, context, |req| {
        drop(std::mem::take(&mut req.body));
        req.controller.take()
    })?;
    if let Some(controller) = controller {
        invoke(&controller.into(), "abort", &[], context)?;
    }
    Ok(())
}

// ─── Sending ────────────────────────────────────────────────────────────────────────────────

/// Sends the request, or its next hop.
fn start(context: &mut Context, this: &JsValue) -> JsResult<()> {
    let (session, url, method, headers, body, flags) = with_req(this, context, |req| {
        req.started = true;
        let body = (!req.body.is_empty() && !matches!(req.method.as_str(), "GET" | "HEAD"))
            .then(|| req.body.clone());
        (
            Arc::clone(&req.session),
            req.url.clone(),
            req.method.clone(),
            req.headers.clone(),
            body,
            (req.bypass, req.cookies),
        )
    })?;
    let init = object(context);
    init.set(js_string!("method"), js_text(&method), false, context)?;
    init.set(js_string!("redirect"), js_text("manual"), false, context)?;
    let list = object(context);
    for (_, name, value) in &headers {
        list.set(
            JsString::from(name.as_str()),
            js_text(value),
            false,
            context,
        )?;
    }
    init.set(js_string!("headers"), list, false, context)?;
    if let Some(bytes) = body {
        init.set(
            js_string!("body"),
            JsUint8Array::from_iter(bytes, context)?,
            false,
            context,
        )?;
    }
    let controller = native::construct_global(context, "AbortController", &[])?;
    init.set(
        js_string!("signal"),
        get(&controller.clone().into(), "signal", context)?,
        false,
        context,
    )?;
    with_req(this, context, |req| req.controller = Some(controller))?;
    let how = InternalFetch {
        net: Some(Rc::new(MainNet::for_request(context, session, flags)) as Rc<dyn BrowserNet>),
        resource_type: ResourceType::Other,
        initiator: None,
    };
    let promise = fetch_internal(context, &js_text(&url), &init.into(), how);
    native::then(
        context,
        &promise.into(),
        |_, args, this: &JsValue, cx| {
            answered(cx, this, &arg(args, 0))?;
            Ok(JsValue::undefined())
        },
        |_, args, this: &JsValue, cx| {
            let aborted = with_req(this, cx, |req| req.aborted)?;
            if !aborted {
                die(cx, this, Some(arg(args, 0)))?;
            }
            Ok(JsValue::undefined())
        },
        this.clone(),
    )?;
    Ok(())
}

/// A response head arrived: a redirect is told and followed, any other is the `'response'`.
fn answered(context: &mut Context, this: &JsValue, response: &JsValue) -> JsResult<()> {
    if with_req(this, context, |req| req.aborted)? {
        return Ok(());
    }
    let status = get(response, "status", context)?.as_number().unwrap_or(0.) as u16;
    let headers = get(response, "headers", context)?;
    let location = invoke(&headers, "get", &[js_text("location")], context)?;
    if crate::msg::is_redirect(status) && location.is_string() {
        let target = native::text_of(&location, context)?;
        return redirected(context, this, response, status, &target);
    }
    // No other hop follows a response that is not a redirect: the body is of no use now.
    with_req(this, context, |req| drop(std::mem::take(&mut req.body)))?;
    let message = incoming_message(context, response)?;
    invoke(
        this,
        "emit",
        &[js_text("response"), message.into()],
        context,
    )?;
    Ok(())
}

/// The response is a redirect to `location`.
fn redirected(
    context: &mut Context,
    this: &JsValue,
    response: &JsValue,
    status: u16,
    location: &str,
) -> JsResult<()> {
    let (policy, current, method) = with_req(this, context, |req| {
        (req.policy, req.url.clone(), req.method.clone())
    })?;
    // The body of the redirect is of no use.
    if let Ok(body) = get(response, "body", context)
        && !body.is_null_or_undefined()
    {
        invoke(&body, "cancel", &[], context)?;
    }
    if policy == Policy::Error {
        let error = define::error("Attempted to redirect, but redirect policy was 'error'");
        let error = error.into_opaque(context)?;
        return die(context, this, Some(error));
    }
    let next = native::construct_global(context, "URL", &[js_text(location), js_text(&current)])?;
    let next = native::text_of(&get(&next.into(), "href", context)?, context)?;
    let to_get = (matches!(status, 301 | 302) && method == "POST")
        || (status == 303 && method != "GET" && method != "HEAD");
    let new_method = if to_get { "GET".to_owned() } else { method };
    let sent = get(response, "headers", context)?;
    let shown = headers_record(context, &sent)?;
    with_req(this, context, |req| {
        req.waiting_for_follow = Some(policy == Policy::Follow);
    })?;
    let told = invoke(
        this,
        "emit",
        &[
            js_text("redirect"),
            status.into(),
            js_text(&new_method),
            js_text(&next),
            shown,
        ],
        context,
    );
    let followed = with_req(this, context, |req| {
        req.waiting_for_follow.take().unwrap_or(false)
    })?;
    told?;
    let aborted = with_req(this, context, |req| req.aborted)?;
    if aborted {
        return Ok(());
    }
    if !followed {
        let error = define::error("Redirect was cancelled").into_opaque(context)?;
        return die(context, this, Some(error));
    }
    let too_many = with_req(this, context, |req| {
        req.redirects += 1;
        req.url = next;
        if to_get {
            req.body.clear();
            req.headers
                .retain(|(lower, _, _)| !lower.starts_with("content-"));
        }
        req.method = new_method;
        req.redirects > MAX_REDIRECTS
    })?;
    if too_many {
        let error = define::error("net::ERR_TOO_MANY_REDIRECTS").into_opaque(context)?;
        return die(context, this, Some(error));
    }
    start(context, this)
}

/// A response's headers as `{ name: [values] }`, the shape `'redirect'` hands over.
fn headers_record(context: &mut Context, headers: &JsValue) -> JsResult<JsValue> {
    let record = object(context);
    for (name, value) in define::header_pairs(headers, context)? {
        let key = JsString::from(name.as_str());
        let list = match record.get(key.clone(), context)?.as_object() {
            Some(list) => list,
            None => {
                let made = native::array(context, Vec::new())
                    .as_object()
                    .expect("an array");
                record.set(key, made.clone(), false, context)?;
                made
            }
        };
        invoke(&list.into(), "push", &[js_text(&value)], context)?;
    }
    Ok(record.into())
}

// ─── The response ───────────────────────────────────────────────────────────────────────────

fn message_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.requests.message_class.borrow().clone() {
        return Ok(class);
    }
    let readable = native::get(&verve::require(context, "stream")?, "Readable", context)?
        .as_object()
        .ok_or_else(|| native::type_error("stream.Readable"))?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "IncomingMessage",
            init: |this, _, cx| {
                let readable = native::get(&verve::require(cx, "stream")?, "Readable", cx)?;
                verve::call(cx, &readable, &this.clone().into(), &[])?;
                let requests = &super::shared(cx).net.requests;
                let id = requests.next.get() + 1;
                requests.next.set(id);
                define::bind(this, MESSAGE, id, cx);
                Ok(())
            },
            parent: Some(&readable),
            methods: &[("_read", 0, read_more)],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    *state.net.requests.message_class.borrow_mut() = Some(class.clone());
    Ok(class)
}

/// The `IncomingMessage` of `response`: its head as properties, its body read as it is wanted.
fn incoming_message(context: &mut Context, response: &JsValue) -> JsResult<JsObject> {
    let message = message_class(context)?.construct(&[], None, context)?;
    let headers = get(response, "headers", context)?;
    let joined = define::header_pairs(&headers, context)?;
    let raw: Vec<JsValue> = joined
        .iter()
        .flat_map(|(name, value)| [js_text(name), js_text(value)])
        .collect();
    let record = object(context);
    for (name, value) in joined.iter().filter(|(name, _)| name != "set-cookie") {
        let known = record.get(JsString::from(name.as_str()), context)?;
        let text = match known.as_string() {
            Some(known) => format!("{}, {value}", known.to_std_string_escaped()),
            None => value.clone(),
        };
        record.set(
            JsString::from(name.as_str()),
            js_text(&text),
            false,
            context,
        )?;
    }
    let cookies: Vec<JsValue> = joined
        .iter()
        .filter(|(name, _)| name == "set-cookie")
        .map(|(_, value)| js_text(value))
        .collect();
    if !cookies.is_empty() {
        let cookies = native::array(context, cookies);
        record.set(js_string!("set-cookie"), cookies, false, context)?;
    }
    let fields = [
        ("statusCode", get(response, "status", context)?),
        ("statusMessage", get(response, "statusText", context)?),
        ("headers", record.into()),
        ("rawHeaders", native::array(context, raw)),
        ("httpVersion", js_text("1.1")),
        ("httpVersionMajor", 1.into()),
        ("httpVersionMinor", 1.into()),
    ];
    for (name, value) in fields {
        message.set(JsString::from(name), value, false, context)?;
    }
    let body = get(response, "body", context)?;
    if !body.is_null_or_undefined() {
        let reader = invoke(&body, "getReader", &[], context)?;
        if let Some(id) = define::id_of(&message.clone().into(), MESSAGE, context) {
            let readers = &super::shared(context).net.requests.readers;
            readers.borrow_mut().insert(
                id,
                BodyReader {
                    reader,
                    reading: false,
                },
            );
        }
    }
    Ok(message)
}

/// `_read`: one more chunk of the body, or its end.
fn read_more(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = define::id_of(this, MESSAGE, context);
    let readers = &super::shared(context).net.requests.readers;
    let reader = id.and_then(|id| {
        let mut table = readers.borrow_mut();
        let entry = table.get_mut(&id)?;
        // A read is already under way: its answer will push.
        (!std::mem::replace(&mut entry.reading, true)).then(|| entry.reader.clone())
    });
    let Some(reader) = reader else {
        if id.is_none_or(|id| !readers.borrow().contains_key(&id)) {
            invoke(this, "push", &[JsValue::null()], context)?;
        }
        return Ok(JsValue::undefined());
    };
    let promise = invoke(&reader, "read", &[], context)?;
    native::then(
        context,
        &promise,
        |_, args, this: &JsValue, cx| {
            let result = arg(args, 0);
            let id = define::id_of(this, MESSAGE, cx);
            let readers = &super::shared(cx).net.requests.readers;
            let done = get(&result, "done", cx)?.to_boolean();
            if let Some(id) = id {
                if done {
                    readers.borrow_mut().remove(&id);
                } else if let Some(entry) = readers.borrow_mut().get_mut(&id) {
                    entry.reading = false;
                }
            }
            if done {
                invoke(this, "push", &[JsValue::null()], cx)?;
            } else {
                let bytes =
                    verve::encoding::bytes_of(&get(&result, "value", cx)?, cx)?.unwrap_or_default();
                let buffer = define::buffer_of(cx, bytes)?;
                invoke(this, "push", &[buffer], cx)?;
            }
            Ok(JsValue::undefined())
        },
        |_, args, this: &JsValue, cx| {
            if let Some(id) = define::id_of(this, MESSAGE, cx) {
                super::shared(cx)
                    .net
                    .requests
                    .readers
                    .borrow_mut()
                    .remove(&id);
            }
            invoke(this, "destroy", &[arg(args, 0)], cx)
        },
        this.clone(),
    )?;
    Ok(JsValue::undefined())
}
