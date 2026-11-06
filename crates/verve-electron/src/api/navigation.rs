//! How a page gets to another document: the loads the main process starts (`loadURL`, `reload`,
//! the history's `goBack`), the ones the page asks for (`location.href`, a link, a form), and the
//! events and promises that follow.
//!
//! The order of events is Electron 39.8.10's (observed): a load says `did-start-loading`, then
//! `did-start-navigation`; one the page asked for says `did-start-navigation` first, then
//! `will-frame-navigate` and `will-navigate`, either of which a listener may cancel. Loads the
//! main process starts say neither will-event.

use std::rc::Rc;

use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, js_text};

use super::page_state::Intent;
use super::windows::WindowRecord;
use super::{contents_ipc, define, shared};
use crate::ids::ContentsId;
use crate::ipc;
use crate::msg::{LoadOptions, ToRenderer};

/// `ERR_INVALID_URL`, Chromium's error for an address that is no URL.
const INVALID_URL: i32 = -300;
/// `ERR_ABORTED`: a load another load took the place of.
pub(crate) const ABORTED: i32 = -3;

/// Starts the load of `url` in the page of `record` and answers the promise that settles when it
/// has loaded or failed — or `None` when nothing waits for it. An address that is no URL fails at
/// once, without a load.
pub(crate) fn start(
    context: &mut Context,
    record: &Rc<WindowRecord>,
    (url, options): (String, LoadOptions),
    intent: Intent,
    wait: bool,
) -> JsResult<Option<JsValue>> {
    if url::Url::parse(&url).is_err() {
        return invalid(context, record, wait).map(Some);
    }
    let page = &record.page_state;
    abort_pending(context, record, &url);
    page.intent.set(intent);
    page.target.replace(url.clone());
    let promise = wait.then(|| {
        let (promise, settle) = JsPromise::new_pending(context);
        *page.load.borrow_mut() = Some(settle);
        JsValue::from(promise)
    });
    record.page.to_renderer(ToRenderer::Load { url, options });
    Ok(promise)
}

/// A `loadURL` of something that is no URL: `did-fail-load` at once, and the promise is rejected.
fn invalid(context: &mut Context, record: &WindowRecord, wait: bool) -> JsResult<JsValue> {
    let code = INVALID_URL;
    if let Ok(object) = contents_ipc::contents_object(context, record.page.contents) {
        let args = [
            code.into(),
            js_text("ERR_INVALID_URL"),
            js_text(""),
            true.into(),
        ];
        define::emit_reporting(context, &object, "did-fail-load", &args);
    }
    if !wait {
        return Ok(JsValue::undefined());
    }
    let error = load_error(context, code, "ERR_INVALID_URL", "")?;
    native::settled(Err(boa_engine::JsError::from_opaque(error)), context)
}

/// The load in progress is replaced by one for `by`: its promise fails with `ERR_ABORTED`, naming
/// the address that replaced it.
fn abort_pending(context: &mut Context, record: &WindowRecord, by: &str) {
    if record.page_state.load.borrow().is_some() {
        settle_load(
            context,
            record,
            Err((ABORTED, "ERR_ABORTED".into(), by.to_owned())),
        );
    }
}

/// The load `record` waits for is over: `Ok` resolves its promise, `Err` is the code, the name of
/// the code and the address of a failure.
pub(crate) fn settle_load(
    context: &mut Context,
    record: &WindowRecord,
    outcome: Result<(), (i32, String, String)>,
) {
    let Some(wait) = record.page_state.load.take() else {
        return;
    };
    let settled = match outcome {
        Ok(()) => wait.resolve.call(&JsValue::undefined(), &[], context),
        Err((code, name, url)) => load_error(context, code, &name, &url)
            .and_then(|error| wait.reject.call(&JsValue::undefined(), &[error], context)),
    };
    define::report(context, settled);
}

/// The `Error` a failed load rejects with: `ERR_FILE_NOT_FOUND (-6) loading 'file:///x.html'`,
/// with `errno`, `code` and `url`.
fn load_error(context: &mut Context, code: i32, name: &str, url: &str) -> JsResult<JsValue> {
    let message = format!("{name} ({code}) loading '{url}'");
    let error = native::coded_error_value(context, &message, name);
    if let Some(object) = error.as_object() {
        object.set(js_string!("errno"), code, false, context)?;
        object.set(js_string!("url"), js_text(url), false, context)?;
    }
    Ok(error)
}

/// `reload()`: the page's address again, as the same entry of the history.
pub(crate) fn reload(context: &mut Context, record: &Rc<WindowRecord>) -> JsResult<()> {
    let url = record.page_state.url.borrow().clone();
    if !url.is_empty() {
        start(
            context,
            record,
            (url, LoadOptions::default()),
            Intent::Reload,
            false,
        )?;
    }
    Ok(())
}

/// Goes to the history's entry `index`, if there is one.
pub(crate) fn go_to_index(
    context: &mut Context,
    record: &Rc<WindowRecord>,
    index: i64,
) -> JsResult<()> {
    let url = {
        let history = record.page_state.history.borrow();
        usize::try_from(index).ok().and_then(|index| {
            history
                .entries()
                .get(index)
                .map(|entry| (index, entry.url.clone()))
        })
    };
    if let Some((index, url)) = url {
        start(
            context,
            record,
            (url, LoadOptions::default()),
            Intent::Go(index),
            false,
        )?;
    }
    Ok(())
}

/// Page `contents` asks to go to `url` (`location.href`, a link, a form): `will-frame-navigate`
/// and `will-navigate` may stop it; otherwise the page gets a document of its own.
pub(crate) fn page_navigates(
    context: &mut Context,
    contents: ContentsId,
    url: String,
    replace: bool,
) {
    let state = shared(context);
    let Some(record) = state.showing(contents) else {
        return;
    };
    let Ok(object) = contents_ipc::contents_object(context, contents) else {
        return;
    };
    record.page_state.announced.set(true);
    emit_navigation(context, &object, "did-start-navigation", (&url, false));
    for name in ["will-frame-navigate", "will-navigate"] {
        if emit_navigation(context, &object, name, (&url, false)) {
            record.page_state.announced.set(false);
            return;
        }
    }
    let intent = if replace {
        Intent::Replace
    } else {
        Intent::Push
    };
    let started = start(
        context,
        &record,
        (url, LoadOptions::default()),
        intent,
        false,
    );
    define::report(context, started);
}

/// Page `contents` asks to be loaded again (`location.reload()`): no will-event, as for `reload()`.
pub(crate) fn page_reloads(context: &mut Context, contents: ContentsId, url: String) {
    if let Some(record) = shared(context).showing(contents) {
        let options = LoadOptions::default();
        let started = start(context, &record, (url, options), Intent::Reload, false);
        define::report(context, started);
    }
}

/// Emits a navigation event on `object`: `(event, url, isInPlace, isMainFrame, processId,
/// routingId)`, where the event also carries the same as `details` (`url`, `isSameDocument`,
/// `isMainFrame`, `frame`, `initiator`, `processId`, `routingId`). Whether a listener prevented it.
pub(crate) fn emit_navigation(
    context: &mut Context,
    object: &JsObject,
    name: &str,
    (url, in_place): (&str, bool),
) -> bool {
    let Some(contents) = define::id_of(&object.clone().into(), contents_ipc::WEB_CONTENTS, context)
    else {
        return false;
    };
    let emitted = (|| {
        let process = ipc::process_id(contents);
        let frame = contents_ipc::page(context, contents)?
            .map_or(JsValue::undefined(), |page| page.frame.into());
        let event = define::event(context)?;
        for (key, value) in [
            ("url", js_text(url)),
            ("isSameDocument", in_place.into()),
            ("isMainFrame", true.into()),
            ("frame", frame),
            ("initiator", JsValue::null()),
            ("processId", process.into()),
            ("routingId", ipc::MAIN_FRAME.into()),
        ] {
            event.set(boa_engine::JsString::from(key), value, false, context)?;
        }
        let args = [
            js_text(url),
            in_place.into(),
            true.into(),
            process.into(),
            ipc::MAIN_FRAME.into(),
        ];
        define::emit_with_event(context, object, name, &event, &args)
    })();
    define::report(context, emitted).unwrap_or(false)
}
