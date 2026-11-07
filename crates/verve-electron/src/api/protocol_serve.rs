//! Running a scheme's handler for a request and handing its answer on, a chunk at a time.
//!
//! The engine answers the main process's `net.fetch` (the answer goes through a kernel
//! `Responder`). What an asker brings is the [`Sink`] the answer is written to and whether the
//! engine reads on its own or on demand.
//!
//! A handler answers with a web `Response`, whose body is a `ReadableStream` read here with
//! `reader.read()` — one read for each pull, which is how a reader that stops reading stops the
//! handler's own producer. An answer that is not a `Response`, a throw and a rejection fail the
//! request with `net::ERR_UNEXPECTED`, `Response.error()` with `net::ERR_FAILED`
//! (`lib/browser/api/protocol.ts` of Electron 39.8.10).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::Level;
use verve::browser_net::{NetError, Responder};
use verve::native::{self, arg, get, invoke, js_text};

use super::protocol::{Entry, HandlerKind};
use crate::msg::{ProtoRequest, SessionId};

/// Where an answer is written.
pub(crate) trait Sink {
    fn head(&mut self, status: u16, text: String, headers: Vec<(String, String)>);
    fn chunk(&mut self, bytes: Vec<u8>);
    fn end(&mut self);
    fn fail(&mut self, error: NetError);
}

/// Who asked, as the engine finds the asker again when it pulls or gives up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Asker {
    /// The main process's own `net.fetch`.
    Main,
}

/// One request being answered.
struct Served {
    sink: Box<dyn Sink>,
    /// The handler's answer body, once there is one.
    reader: Option<JsObject>,
    /// Chunks the asker has pulled and the engine has not read yet.
    wants: u32,
    reading: bool,
    /// Reads without being pulled, until the body ends.
    auto: bool,
}

/// The requests being answered, by serial; and the askers' own numbers for them.
#[derive(Default)]
pub(crate) struct Serving {
    next: Cell<u64>,
    served: RefCell<HashMap<u64, Served>>,
    calls: RefCell<HashMap<(Asker, u64), u64>>,
}

impl Serving {
    fn serial_of(&self, who: Asker, call: u64) -> Option<u64> {
        self.calls.borrow().get(&(who, call)).copied()
    }
}

/// A request to answer.
pub(crate) struct Job {
    pub session: SessionId,
    pub request: ProtoRequest,
    /// How the asker numbers it, so it can pull and give up.
    pub call: Option<(Asker, u64)>,
    /// Reads the whole answer without waiting to be pulled.
    pub auto: bool,
}

/// Runs the handler of the job's scheme and writes the answer to `sink`.
pub(crate) fn start(context: &mut Context, job: Job, sink: Box<dyn Sink>) {
    let state = super::shared(context);
    let serving = &state.net.serving;
    let serial = serving.next.get() + 1;
    serving.next.set(serial);
    serving.served.borrow_mut().insert(
        serial,
        Served {
            sink,
            reader: None,
            wants: 0,
            reading: false,
            auto: job.auto,
        },
    );
    if let Some(call) = job.call {
        serving.calls.borrow_mut().insert(call, serial);
    }
    let entry = url::Url::parse(&job.request.url)
        .ok()
        .and_then(|url| state.net.protocols.entry(job.session, url.scheme()));
    let Some(entry) = entry else {
        return fail(context, serial, crate::msg::ERR_UNKNOWN_URL_SCHEME);
    };
    if let Err(error) = run_handler(context, serial, &entry, &job) {
        log_error(context, &error);
        fail(context, serial, crate::msg::ERR_UNEXPECTED);
    }
}

/// The asker wants one more chunk of request `call`'s answer.
pub(crate) fn pull(context: &mut Context, who: Asker, call: u64) {
    let Some(serial) = super::shared(context).net.serving.serial_of(who, call) else {
        return;
    };
    if let Some(served) = super::shared(context)
        .net
        .serving
        .served
        .borrow_mut()
        .get_mut(&serial)
    {
        served.wants += 1;
    }
    read_next(context, serial);
}

/// The asker gave up on request `call`: the handler's body is cancelled.
pub(crate) fn cancel(context: &mut Context, who: Asker, call: u64) {
    let state = super::shared(context);
    let Some(serial) = state.net.serving.calls.borrow_mut().remove(&(who, call)) else {
        return;
    };
    let served = state.net.serving.served.borrow_mut().remove(&serial);
    if let Some(reader) = served.and_then(|served| served.reader) {
        // The producer is told to stop; whether it minds does not matter.
        let _ = invoke(&reader.into(), "cancel", &[], context);
    }
}

fn run_handler(context: &mut Context, serial: u64, entry: &Entry, job: &Job) -> JsResult<()> {
    match entry.kind {
        HandlerKind::Handle => {
            let request = web_request(context, &job.request)?;
            let answer = verve::call(
                context,
                &entry.handler,
                &JsValue::undefined(),
                &[request.into()],
            )?;
            let promise = JsPromise::resolve(answer, context)?;
            let (ok, err) = reactions(context, serial);
            promise.then(Some(ok), Some(err), context)?;
            Ok(())
        }
    }
}

/// The reactions to a handler's promise: its answer, and its rejection.
fn reactions(
    context: &mut Context,
    serial: u64,
) -> (
    boa_engine::object::builtins::JsFunction,
    boa_engine::object::builtins::JsFunction,
) {
    let ok = native::closure(
        context,
        "",
        1,
        |_, args, serial: &u64, cx| {
            answered(cx, *serial, &arg(args, 0));
            Ok(JsValue::undefined())
        },
        serial,
    );
    let err = native::closure(
        context,
        "",
        1,
        |_, args, serial: &u64, cx| {
            let error = verve::native::get(&arg(args, 0), "stack", cx).unwrap_or_default();
            verve::console_line(cx, Level::Error, verve::print(&error));
            fail(cx, *serial, crate::msg::ERR_UNEXPECTED);
            Ok(JsValue::undefined())
        },
        serial,
    );
    let function = |object: JsObject| {
        boa_engine::object::builtins::JsFunction::from_object(object).expect("a closure")
    };
    (function(ok), function(err))
}

/// The handler answered with `value`: a `Response` is sent, anything else is an error.
pub(crate) fn answered(context: &mut Context, serial: u64, value: &JsValue) {
    match send_response(context, serial, value) {
        Ok(true) => {}
        Ok(false) => fail(context, serial, crate::msg::ERR_UNEXPECTED),
        Err(error) => {
            log_error(context, &error);
            fail(context, serial, crate::msg::ERR_UNEXPECTED);
        }
    }
}

/// Writes `response` as the answer: its head now, its body as it is pulled. `false` when it is
/// not a response.
fn send_response(context: &mut Context, serial: u64, response: &JsValue) -> JsResult<bool> {
    if !valid(response, context)? {
        return Ok(false);
    }
    if get(response, "type", context)?
        .as_string()
        .is_some_and(|t| t.to_std_string_escaped() == "error")
    {
        fail(context, serial, crate::msg::ERR_FAILED);
        return Ok(true);
    }
    let status = get(response, "status", context)?
        .as_number()
        .unwrap_or(200.) as u16;
    let text = get(response, "statusText", context)?;
    let text = native::text_of(&text, context)?;
    let text = if text.is_empty() {
        reason(status).to_owned()
    } else {
        text
    };
    let list = super::define::header_pairs(&get(response, "headers", context)?, context)?;
    let body = get(response, "body", context)?;
    let state = super::shared(context);
    if let Some(served) = state.net.serving.served.borrow_mut().get_mut(&serial) {
        served.sink.head(status, text, list);
    } else {
        return Ok(true);
    }
    if body.is_null_or_undefined() {
        finish(context, serial);
        return Ok(true);
    }
    let reader = invoke(&body, "getReader", &[], context)?;
    if let Some(served) = state.net.serving.served.borrow_mut().get_mut(&serial) {
        served.reader = reader.as_object();
        if served.auto {
            served.wants = 1;
        }
    }
    read_next(context, serial);
    Ok(true)
}

/// A check of one field of a `Response`.
type Accepts = fn(&JsValue) -> bool;

/// Whether `value` is the shape of a `Response` (Electron's `validateResponse`: an object whose
/// own `status`, `statusText`, `headers` and `body`, when it has them, are of the right types).
fn valid(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    let Some(object) = value.as_object() else {
        return Ok(false);
    };
    if get(value, "type", context)?
        .as_string()
        .is_some_and(|t| t.to_std_string_escaped() == "error")
    {
        return Ok(true);
    }
    let fields: [(&str, Accepts); 4] = [
        ("status", |v| v.is_number()),
        ("statusText", |v| v.is_string()),
        ("headers", |v| v.is_object()),
        ("body", |v| v.is_object() || v.is_null()),
    ];
    for (name, accepts) in fields {
        let key = boa_engine::JsString::from(name);
        if object.has_own_property(key, context)? && !accepts(&get(value, name, context)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Reads the next chunk of the body when one is wanted and none is being read.
fn read_next(context: &mut Context, serial: u64) {
    let state = super::shared(context);
    let reader = {
        let mut served = state.net.serving.served.borrow_mut();
        let Some(served) = served.get_mut(&serial) else {
            return;
        };
        if served.reading || served.wants == 0 {
            return;
        }
        let Some(reader) = served.reader.clone() else {
            return;
        };
        served.reading = true;
        served.wants -= 1;
        reader
    };
    let outcome = invoke(&reader.into(), "read", &[], context)
        .and_then(|promise| native::then(context, &promise, on_read, on_read_error, serial));
    if let Err(error) = outcome {
        log_error(context, &error);
        fail(context, serial, crate::msg::ERR_UNEXPECTED);
    }
}

fn on_read(
    _: &JsValue,
    args: &[JsValue],
    serial: &u64,
    context: &mut Context,
) -> JsResult<JsValue> {
    let result = arg(args, 0);
    if get(&result, "done", context)?.to_boolean() {
        finish(context, *serial);
        return Ok(JsValue::undefined());
    }
    let bytes =
        verve::encoding::bytes_of(&get(&result, "value", context)?, context)?.unwrap_or_default();
    let state = super::shared(context);
    let auto = {
        let mut served = state.net.serving.served.borrow_mut();
        let Some(served) = served.get_mut(serial) else {
            return Ok(JsValue::undefined());
        };
        served.reading = false;
        for piece in bytes.chunks(crate::msg::BODY_CHUNK) {
            served.sink.chunk(piece.to_vec());
        }
        served.auto
    };
    if auto && let Some(served) = state.net.serving.served.borrow_mut().get_mut(serial) {
        served.wants += 1;
    }
    read_next(context, *serial);
    Ok(JsValue::undefined())
}

fn on_read_error(
    _: &JsValue,
    _: &[JsValue],
    serial: &u64,
    context: &mut Context,
) -> JsResult<JsValue> {
    fail(context, *serial, crate::msg::ERR_FAILED);
    Ok(JsValue::undefined())
}

/// The body is complete.
fn finish(context: &mut Context, serial: u64) {
    if let Some(mut served) = forget(context, serial) {
        served.sink.end();
    }
}

/// The request fails with Chromium's error number `code`.
pub(crate) fn fail(context: &mut Context, serial: u64, code: i32) {
    if let Some(mut served) = forget(context, serial) {
        served.sink.fail(NetError::from_code(code));
    }
}

fn forget(context: &mut Context, serial: u64) -> Option<Served> {
    let state = super::shared(context);
    state
        .net
        .serving
        .calls
        .borrow_mut()
        .retain(|_, s| *s != serial);
    state.net.serving.served.borrow_mut().remove(&serial)
}

/// A line on the main process's console, as Electron writes a throwing handler's error.
fn console_error(context: &mut Context, error: &JsValue) {
    let stack = get(error, "stack", context).unwrap_or_default();
    let shown = if stack.is_undefined() { error } else { &stack };
    verve::console_line(context, Level::Error, verve::print(shown));
}

/// [`console_error`] for an error Rust holds.
fn log_error(context: &mut Context, error: &boa_engine::JsError) {
    let value = error.clone().into_opaque(context).unwrap_or_default();
    console_error(context, &value);
}

/// The reason phrase of `status`; empty for one with none.
fn reason(status: u16) -> &'static str {
    match verve::status_text(status) {
        "unknown" => "",
        text => text,
    }
}

/// `new Request(url, { method, headers, body })` for the handler.
fn web_request(context: &mut Context, request: &ProtoRequest) -> JsResult<JsObject> {
    let init = native::object(context);
    let headers: Vec<JsValue> = request
        .headers
        .iter()
        .map(|(name, value)| native::array(context, vec![js_text(name), js_text(value)]))
        .collect();
    let headers = native::array(context, headers);
    init.set(
        boa_engine::js_string!("method"),
        js_text(&request.method),
        false,
        context,
    )?;
    init.set(boa_engine::js_string!("headers"), headers, false, context)?;
    if !matches!(request.method.as_str(), "GET" | "HEAD")
        && let Some(body) = &request.body
    {
        let bytes =
            boa_engine::object::builtins::JsUint8Array::from_iter(body.iter().copied(), context)?;
        init.set(boa_engine::js_string!("body"), bytes, false, context)?;
    }
    native::construct_global(context, "Request", &[js_text(&request.url), init.into()])
}

// ─── Askers ─────────────────────────────────────────────────────────────────────────────────

/// The main process's `net.fetch`: the answer goes through the kernel's `Responder`.
pub(crate) struct ToResponder(pub Option<Responder>);

impl Sink for ToResponder {
    fn head(&mut self, status: u16, text: String, headers: Vec<(String, String)>) {
        if let Some(respond) = &self.0 {
            respond.head(status, text, headers);
        }
    }
    fn chunk(&mut self, bytes: Vec<u8>) {
        if let Some(respond) = &self.0 {
            respond.chunk(bytes);
        }
    }
    fn end(&mut self) {
        if let Some(respond) = self.0.take() {
            respond.end();
        }
    }
    fn fail(&mut self, error: NetError) {
        if let Some(respond) = self.0.take() {
            respond.fail(error);
        }
    }
}
