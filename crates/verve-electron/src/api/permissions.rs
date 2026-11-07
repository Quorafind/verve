//! `session.setPermissionRequestHandler`, `setPermissionCheckHandler` and
//! `setDevicePermissionHandler` (`session.cc`, `permission_manager.cc` of Electron 39.8.10; the
//! arguments the handlers are given and what asking each does are observed, see P16's contract).
//!
//! The handlers are functions in the main process's realm. A page that uses a permission asks
//! only when its session has the handler for it ([`Session::has_permission_handler`]), by
//! [`FromRenderer::Permission`](crate::msg::FromRenderer): the question reaches [`from_page`]
//! and the answer goes back as [`ToRenderer::PermissionReply`]. With no handler a permission is
//! allowed, as it is in Electron.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use url::Url;
use verve::native::{self, arg, js_text};

use super::define;
use crate::ids::ContentsId;
use crate::msg::{SessionId, ToRenderer};

/// What a request's callback does with the answer.
type Answer = Box<dyn FnOnce(bool, &mut Context)>;

/// The handlers the sessions have and the requests that wait for a callback.
#[derive(Default)]
pub(crate) struct Objects {
    /// The handlers: requests, checks, and devices (kept, and asked of nobody), by session.
    handlers: RefCell<HashMap<(SessionId, Which), JsValue>>,
    /// The requests whose handler has not called its callback yet, by serial.
    waiting: RefCell<HashMap<u64, Answer>>,
    serial: Cell<u64>,
}

/// Which handler of a session.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Which {
    Request,
    Check,
    Device,
}

/// `setPermissionRequestHandler(handler)` and its sisters: the handler replaces the last, and
/// `null` takes it away.
pub(crate) fn set_handler(
    context: &mut Context,
    this: &JsValue,
    args: &[JsValue],
    which: Which,
) -> JsResult<JsValue> {
    let session = super::session::session_of(this, context)?;
    let handler = arg(args, 0);
    if !handler.is_null() && !handler.is_callable() {
        return Err(native::type_error("Must pass null or function"));
    }
    let state = super::shared(context);
    let mut handlers = state.net.permissions.handlers.borrow_mut();
    if handler.is_null() {
        handlers.remove(&(session.id(), which));
    } else {
        handlers.insert((session.id(), which), handler);
    }
    match which {
        Which::Request => session.set_permission_handler(false, !arg(args, 0).is_null()),
        Which::Check => session.set_permission_handler(true, !arg(args, 0).is_null()),
        Which::Device => {}
    }
    Ok(JsValue::undefined())
}

/// A page's question about `permission`: the handler of its session answers, and the answer
/// goes back to the page. A page asks only when there is a handler; if there is none now (it
/// was taken away meanwhile), the permission is allowed.
pub(crate) fn from_page(
    context: &mut Context,
    contents: ContentsId,
    (call, session): (u64, SessionId),
    (permission, url, check_only): (&str, &str, bool),
) {
    let which = if check_only {
        Which::Check
    } else {
        Which::Request
    };
    let state = super::shared(context);
    let handler = state
        .net
        .permissions
        .handlers
        .borrow()
        .get(&(session, which))
        .cloned();
    let hub = Arc::clone(&state.hub);
    let reply = move |granted: bool, _: &mut Context| {
        hub.to_renderer(contents, ToRenderer::PermissionReply { call, granted });
    };
    let Some(handler) = handler else {
        return reply(true, context);
    };
    let outcome = if check_only {
        // A handler that throws denies, as a request handler that throws does: the page's
        // `permissions.query` must not wait for an answer that never comes.
        let granted = check(context, &handler, contents, permission, url).unwrap_or_else(|error| {
            verve::report(context, &error);
            false
        });
        reply(granted, context);
        Ok(())
    } else {
        request(
            context,
            &handler,
            contents,
            (permission, url),
            Box::new(reply),
        )
    };
    if let Some(error) = outcome.err() {
        verve::report(context, &error);
    }
}

/// The origin of `url` as a check handler is told it: with the slash a URL has after its host.
fn origin_of(url: &str) -> String {
    Url::parse(url).map_or_else(
        |_| url.to_owned(),
        |url| format!("{}/", url.origin().ascii_serialization()),
    )
}

/// What a check handler says: `(webContents, permission, requestingOrigin, details)`.
fn check(
    context: &mut Context,
    handler: &JsValue,
    contents: ContentsId,
    permission: &str,
    url: &str,
) -> JsResult<bool> {
    let origin = origin_of(url);
    let details = define::record(
        context,
        &[
            ("embeddingOrigin", js_text(&origin)),
            ("isMainFrame", true.into()),
            ("requestingUrl", js_text(url)),
        ],
    )?;
    let object = super::contents_ipc::contents_object(context, contents)?;
    let args = [
        object.into(),
        js_text(permission),
        js_text(&origin),
        details.into(),
    ];
    let granted = verve::call(context, handler, &JsValue::undefined(), &args)?;
    Ok(granted.to_boolean())
}

/// Asks a request handler: `(webContents, permission, callback, details)`. `answer` runs when
/// the handler calls its callback.
fn request(
    context: &mut Context,
    handler: &JsValue,
    contents: ContentsId,
    (permission, url): (&str, &str),
    answer: Answer,
) -> JsResult<()> {
    let state = super::shared(context);
    let serial = state.net.permissions.serial.get() + 1;
    state.net.permissions.serial.set(serial);
    state
        .net
        .permissions
        .waiting
        .borrow_mut()
        .insert(serial, answer);
    let details = define::record(
        context,
        &[
            ("isMainFrame", true.into()),
            ("requestingUrl", js_text(url)),
        ],
    )?;
    let object = super::contents_ipc::contents_object(context, contents)?;
    let args = [
        object.into(),
        js_text(permission),
        callback(context, serial).into(),
        details.into(),
    ];
    let outcome = verve::call(context, handler, &JsValue::undefined(), &args);
    if outcome.is_err() {
        // A handler that threw has not allowed anything.
        let waiting = state.net.permissions.waiting.borrow_mut().remove(&serial);
        if let Some(answer) = waiting {
            answer(false, context);
        }
    }
    outcome.map(|_| ())
}

/// The callback a request handler is given: its first call answers the request.
fn callback(context: &mut Context, serial: u64) -> JsObject {
    native::closure(
        context,
        "",
        1,
        |_, args, serial: &u64, cx| {
            let waiting = super::shared(cx)
                .net
                .permissions
                .waiting
                .borrow_mut()
                .remove(serial);
            if let Some(answer) = waiting {
                answer(arg(args, 0).to_boolean(), cx);
            }
            Ok(JsValue::undefined())
        },
        serial,
    )
}
