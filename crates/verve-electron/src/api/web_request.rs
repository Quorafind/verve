//! `session.webRequest`: listeners that see a session's requests and may change them
//! (`web_request.cc` of Electron 39.8.10; the shape of `details` and the errors are observed, see
//! P16's contract).
//!
//! The listeners are functions in the main process's realm. A request of a page that one of them
//! wants reaches here as [`FromRenderer::WebRequest`](crate::msg::FromRenderer); a request of the
//! main process's own is handed to [`run`] on the spot. The session records the filter of each
//! stage's listener ([`Session::web_request_wants`]), which is how a request that nobody wants
//! costs nothing.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use boa_engine::object::builtins::JsUint8Array;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, ClassSpec, NewOnly, arg, get, invoke, js_text, list_of, text_of};

use super::define::{self, Kind};
use crate::hub::Session;
use crate::ids::ContentsId;
use crate::msg::{
    CookieChange, SessionId, ToRenderer, WebRequestDetails, WebRequestReply, WebRequestStage,
};
use crate::net_watch::{Listeners, Then};

use super::match_pattern::Patterns;

/// The class of a session's `webRequest`.
const WEB_REQUEST: Kind = Kind("WebRequest");

/// The names `filter.types` may hold: Electron's `resourceType`s.
const TYPES: &[&str] = &[
    "mainFrame",
    "subFrame",
    "stylesheet",
    "script",
    "image",
    "font",
    "object",
    "xhr",
    "ping",
    "cspReport",
    "media",
    "webSocket",
    "other",
];

/// What the main process keeps of `webRequest`: the objects, the listeners and the answers they
/// owe.
#[derive(Default)]
pub(crate) struct Objects {
    class: RefCell<Option<JsObject>>,
    by_session: RefCell<HashMap<SessionId, JsObject>>,
    /// The listener of each stage of each session.
    listeners: RefCell<HashMap<(SessionId, usize), JsValue>>,
    /// The callbacks of the listeners that have been asked and not answered, by serial.
    waiting: RefCell<HashMap<u64, Then>>,
    serial: Cell<u64>,
}

/// The `webRequest` of `session`, made the first time.
pub(crate) fn object_of(context: &mut Context, session: &Arc<Session>) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(found) = state.net.web.by_session.borrow().get(&session.id()) {
        return Ok(found.clone());
    }
    let class = web_request_class(context)?;
    let made = class.construct(&[session.id().into()], None, context)?;
    state
        .net
        .web
        .by_session
        .borrow_mut()
        .insert(session.id(), made.clone());
    Ok(made)
}

fn web_request_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.web.class.borrow().clone() {
        return Ok(class);
    }
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "WebRequest",
            init: |this, args, cx| {
                let id = arg(args, 0).as_number().unwrap_or(0.) as SessionId;
                define::bind(this, WEB_REQUEST, id, cx);
                Ok(())
            },
            methods: &[
                ("onBeforeRequest", 2, listen::<0>),
                ("onBeforeSendHeaders", 2, listen::<1>),
                ("onSendHeaders", 2, listen::<2>),
                ("onHeadersReceived", 2, listen::<3>),
                ("onResponseStarted", 2, listen::<4>),
                ("onBeforeRedirect", 2, listen::<5>),
                ("onCompleted", 2, listen::<6>),
                ("onErrorOccurred", 2, listen::<7>),
            ],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    *state.net.web.class.borrow_mut() = Some(class.clone());
    Ok(class)
}

/// What `filter` says: the patterns of its `urls` and the `types` it is limited to.
fn read_filter(filter: &JsObject, context: &mut Context) -> JsResult<Patterns> {
    let value: JsValue = filter.clone().into();
    let urls = get(&value, "urls", context)?;
    if !urls.as_object().is_some_and(|o| o.is_array()) {
        return Err(native::type_error(
            "Parameter 'filter' must have property 'urls'.",
        ));
    }
    let mut patterns = Vec::new();
    for url in list_of(&urls, context)? {
        patterns.push(text_of(&url, context)?);
    }
    let mut types = Vec::new();
    let given = get(&value, "types", context)?;
    if given.as_object().is_some_and(|o| o.is_array()) {
        for kind in list_of(&given, context)? {
            let kind = text_of(&kind, context)?;
            if !TYPES.contains(&kind.as_str()) {
                return Err(define::error(&format!("Invalid type {kind}")));
            }
            types.push(kind);
        }
    }
    Patterns::parse(&patterns, types).map_err(native::type_error)
}

/// `onBeforeRequest([filter, ]listener)` and its sisters: the listener replaces the stage's
/// last, and `null` takes it away.
fn listen<const STAGE: usize>(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let stage = WebRequestStage::ALL[STAGE];
    let hub = Arc::clone(&super::shared(context).hub);
    let (id, session) = define::live(this, WEB_REQUEST, context, |id| hub.session_by_id(id))?;
    let first = arg(args, 0);
    let (filter, listener) = match first.as_object() {
        Some(object) if !first.is_callable() => {
            (Some(read_filter(&object, context)?), arg(args, 1))
        }
        _ => (None, first),
    };
    if !listener.is_null() && !listener.is_callable() {
        return Err(native::type_error("Must pass null or a Function"));
    }
    let state = super::shared(context);
    if listener.is_null() {
        session.set_web_request(stage, None);
        state.net.web.listeners.borrow_mut().remove(&(id, STAGE));
    } else {
        session.set_web_request(stage, Some(filter.unwrap_or_else(Patterns::everything)));
        state
            .net
            .web
            .listeners
            .borrow_mut()
            .insert((id, STAGE), listener);
    }
    Ok(JsValue::undefined())
}

// ─── Telling a listener ─────────────────────────────────────────────────────────────────────

/// Name → value, as the headers object of a request, a repeated name joined by `, `.
fn request_headers_object(
    context: &mut Context,
    headers: &[(String, String)],
) -> JsResult<JsValue> {
    let object = native::object(context);
    for (name, value) in headers {
        let key = boa_engine::JsString::from(name.as_str());
        let joined = match object.get(key.clone(), context)?.as_string() {
            Some(before) => format!("{}, {value}", before.to_std_string_escaped()),
            None => value.clone(),
        };
        object.set(key, js_text(&joined), false, context)?;
    }
    Ok(object.into())
}

/// Name → the values of that name, as `responseHeaders` shows them.
fn response_headers_object(
    context: &mut Context,
    headers: &[(String, String)],
) -> JsResult<JsValue> {
    let mut grouped: Vec<(&str, Vec<JsValue>)> = Vec::new();
    for (name, value) in headers {
        match grouped
            .iter_mut()
            .find(|(seen, _)| seen.eq_ignore_ascii_case(name))
        {
            Some((_, values)) => values.push(js_text(value)),
            None => grouped.push((name, vec![js_text(value)])),
        }
    }
    let object = native::object(context);
    for (name, values) in grouped {
        let list = native::array(context, values);
        object.set(boa_engine::JsString::from(name), list, false, context)?;
    }
    Ok(object.into())
}

/// `uploadData`: one `rawData` element with the body.
fn upload_data(context: &mut Context, body: &[u8]) -> JsResult<JsValue> {
    let bytes = JsUint8Array::from_iter(body.iter().copied(), context)?;
    let buffer = invoke(
        &native::global(context, "Buffer")?,
        "from",
        &[bytes.into()],
        context,
    )?;
    let part = define::record(context, &[("type", js_text("rawData")), ("bytes", buffer)])?;
    Ok(native::array(context, vec![part.into()]))
}

/// The `details` a listener of `stage` is given, with the fields Electron 39.8.10 gives it in
/// the order it gives them.
fn details_object(
    context: &mut Context,
    stage: WebRequestStage,
    d: &WebRequestDetails,
    contents: Option<ContentsId>,
) -> JsResult<JsObject> {
    use WebRequestStage as S;
    let mut fields: Vec<(&str, JsValue)> = vec![
        ("id", (d.id as f64).into()),
        ("url", js_text(&d.url)),
        ("method", js_text(&d.method)),
        ("timestamp", d.timestamp.into()),
        ("resourceType", js_text(d.resource_type.name())),
    ];
    let after_type = matches!(
        stage,
        S::HeadersReceived | S::ResponseStarted | S::BeforeRedirect | S::Completed
    );
    if after_type {
        if let Some(ip) = &d.ip {
            fields.push(("ip", js_text(ip)));
        }
        fields.push(("fromCache", false.into()));
        if let Some(response) = &d.response {
            fields.push(("statusLine", js_text(&response.status_line)));
            fields.push(("statusCode", response.status.into()));
            let headers = response_headers_object(context, &response.headers)?;
            fields.push(("responseHeaders", headers));
        }
    }
    if let Some(id) = contents {
        let page = super::contents_ipc::page(context, id)?;
        let frame = page
            .as_ref()
            .map_or(JsValue::null(), |p| p.frame.clone().into());
        let object = super::contents_ipc::contents_object(context, id)?;
        fields.push(("frame", frame));
        fields.push(("webContents", object.into()));
        fields.push(("webContentsId", id.into()));
    }
    fields.push(("referrer", js_text(&d.referrer)));
    if let Some(body) = &d.upload {
        fields.push(("uploadData", upload_data(context, body)?));
    }
    match stage {
        S::BeforeRequest => fields.push(("redirectURL", js_text(""))),
        S::BeforeSendHeaders | S::SendHeaders => {
            let headers =
                request_headers_object(context, d.request_headers.as_deref().unwrap_or(&[]))?;
            fields.push(("requestHeaders", headers));
        }
        S::BeforeRedirect => {
            fields.push((
                "redirectURL",
                js_text(d.redirect_url.as_deref().unwrap_or("")),
            ));
        }
        S::Completed | S::ErrorOccurred => {
            fields.push(("error", js_text(d.error.as_deref().unwrap_or(""))));
        }
        S::HeadersReceived | S::ResponseStarted => {}
    }
    define::record(context, &fields)
}

/// A header object a callback gave, as a list: an array makes one entry for each of its values.
fn header_list(object: &JsValue, context: &mut Context) -> JsResult<Vec<(String, String)>> {
    let Some(object) = object.as_object() else {
        return Ok(Vec::new());
    };
    let mut list = Vec::new();
    for (name, value) in define::entries_of(&object, context)? {
        let values = if value.as_object().is_some_and(|o| o.is_array()) {
            list_of(&value, context)?
        } else {
            vec![value]
        };
        for value in values {
            list.push((name.clone(), text_of(&value, context)?));
        }
    }
    Ok(list)
}

/// What a callback was called with.
fn reply_of(value: &JsValue, context: &mut Context) -> JsResult<WebRequestReply> {
    if !value.is_object() {
        return Ok(WebRequestReply::default());
    }
    let text = |name: &str, cx: &mut Context| -> JsResult<Option<String>> {
        let found = get(value, name, cx)?;
        found
            .as_string()
            .map(|s| Ok(s.to_std_string_escaped()))
            .transpose()
    };
    let headers = |name: &str, cx: &mut Context| -> JsResult<Option<Vec<(String, String)>>> {
        let found = get(value, name, cx)?;
        found
            .is_object()
            .then(|| header_list(&found, cx))
            .transpose()
    };
    Ok(WebRequestReply {
        cancel: get(value, "cancel", context)?.to_boolean(),
        redirect_url: text("redirectURL", context)?,
        request_headers: headers("requestHeaders", context)?,
        response_headers: headers("responseHeaders", context)?,
        status_line: text("statusLine", context)?,
    })
}

/// The callback of a listener that blocks: the first call answers `serial`'s `then`.
fn callback(context: &mut Context, serial: u64) -> JsObject {
    native::closure(
        context,
        "",
        1,
        |_, args, serial: &u64, cx| {
            let then = super::shared(cx)
                .net
                .web
                .waiting
                .borrow_mut()
                .remove(serial);
            if let Some(then) = then {
                let read = reply_of(&arg(args, 0), cx);
                let reply = define::report(cx, read).unwrap_or_default();
                then(reply, cx);
            }
            Ok(JsValue::undefined())
        },
        serial,
    )
}

/// Shows the listener of `stage` of session `session` a request. A stage that blocks goes on with
/// `then` when the listener calls its callback (at once, if nobody listens or the listener
/// throws); the others need no answer.
pub(crate) fn run(
    context: &mut Context,
    session: SessionId,
    stage: WebRequestStage,
    details: &WebRequestDetails,
    (contents, then): (Option<ContentsId>, Option<Then>),
) {
    let state = super::shared(context);
    let listener = state
        .net
        .web
        .listeners
        .borrow()
        .get(&(session, stage.index()))
        .cloned();
    let Some(listener) = listener else {
        if let Some(then) = then {
            then(WebRequestReply::default(), context);
        }
        return;
    };
    let serial = state.net.web.serial.get() + 1;
    state.net.web.serial.set(serial);
    let blocks = stage.blocks();
    if let (true, Some(then)) = (blocks, then) {
        state.net.web.waiting.borrow_mut().insert(serial, then);
    }
    let result = details_object(context, stage, details, contents).and_then(|object| {
        let mut args = vec![JsValue::from(object)];
        if blocks {
            args.push(callback(context, serial).into());
        }
        verve::call(context, &listener, &JsValue::undefined(), &args)
    });
    if let Err(error) = result {
        verve::report(context, &error);
        let then = state.net.web.waiting.borrow_mut().remove(&serial);
        if let Some(then) = then {
            then(WebRequestReply::default(), context);
        }
    }
}

/// A request of page `contents` that its session's listener wants: the answer goes back to the
/// page as [`ToRenderer::WebRequestReply`] when the stage blocks.
pub(crate) fn from_page(
    context: &mut Context,
    contents: ContentsId,
    (call, session): (u64, SessionId),
    stage: WebRequestStage,
    details: &WebRequestDetails,
) {
    let hub = Arc::clone(&super::shared(context).hub);
    let then: Option<Then> = (call != 0).then(|| {
        Box::new(move |reply: WebRequestReply, _: &mut Context| {
            hub.to_renderer(contents, ToRenderer::WebRequestReply { call, reply });
        }) as Then
    });
    run(context, session, stage, details, (Some(contents), then));
}

/// The listeners of a session, for the main process's own requests: they run where they stand.
pub(crate) struct MainListeners {
    pub session: SessionId,
}

impl Listeners for MainListeners {
    fn ask(
        &self,
        stage: WebRequestStage,
        details: WebRequestDetails,
        then: Then,
        context: &mut Context,
    ) {
        run(context, self.session, stage, &details, (None, Some(then)));
    }

    fn tell(&self, stage: WebRequestStage, details: WebRequestDetails, context: &mut Context) {
        run(context, self.session, stage, &details, (None, None));
    }

    fn changed(&self, changes: Vec<CookieChange>, context: &mut Context) {
        super::cookies::emit_changes(context, self.session, changes);
    }
}
