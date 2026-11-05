//! What the two sides of Electron's IPC agree on, apart from the objects each shows a script:
//! how a call's arguments cross the thread boundary, what a failed call is told as, and the
//! numbers the events and frames report.
//!
//! Electron carries IPC with the HTML structured clone, so everything goes through
//! [`verve::clone`]; what Electron adds is the shape of the failures, frozen from 39.8.10 in
//! `tests/ipc.rs`.

use std::sync::atomic::{AtomicU64, Ordering};

use boa_engine::{Context, JsError, JsResult, JsValue, js_string};
use verve::clone::Cloned;
use verve::native::{self, js_text};

use crate::ids::ContentsId;

/// The frame id of the main frame, which is the only frame a page has.
pub(crate) const MAIN_FRAME: u32 = 1;

/// What a failed `invoke` is told when its handler gave no usable error: Electron's own code
/// fails on `null` and `undefined`, and the page hears that the reply never came.
pub(crate) const NEVER_SENT: &str = "reply was never sent";

/// A number no earlier `invoke` of this process had, so a reply that comes late — to a page whose
/// isolate was replaced by a navigation — is never taken for the answer to a newer call.
pub(crate) fn next_call() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The process id the events of page `contents` report, and `webContents.getProcessId()` answers.
pub(crate) fn process_id(contents: ContentsId) -> u32 {
    1000 + contents
}

/// An `Error` made the way a script's `new Error(message)` is, so it prints with a stack.
pub(crate) fn error_value(message: &str, context: &mut Context) -> JsResult<JsValue> {
    let error = native::construct_global(context, "Error", &[js_text(message)])?;
    // Made from Rust, there is no script to take a stack from: the error says it came from here.
    let stack = js_text(&format!("Error: {message}\n    at <anonymous> (native)"));
    native::hidden(&error, js_string!("stack"), stack, context)?;
    Ok(error.into())
}

/// `Error: An object could not be cloned.`, which every way of sending a value that cannot be
/// cloned fails with.
pub(crate) fn clone_error(context: &mut Context) -> JsError {
    match error_value("An object could not be cloned.", context) {
        Ok(error) => JsError::from_opaque(error),
        Err(error) => error,
    }
}

/// The arguments of one call, as the one clone they travel in: arguments that share an object
/// still share it on the other side.
///
/// # Errors
///
/// [`clone_error`] for an argument that cannot be cloned.
pub(crate) fn pack(args: &[JsValue], context: &mut Context) -> JsResult<Vec<Cloned>> {
    let all = native::array(context, args.to_vec());
    Ok(vec![clone(&all, context)?])
}

/// `value` as a clone, or [`clone_error`].
pub(crate) fn clone(value: &JsValue, context: &mut Context) -> JsResult<Cloned> {
    Cloned::from_value(value, context).map_err(|_| clone_error(context))
}

/// The arguments [`pack`] packed, made again in `context`'s realm.
///
/// # Errors
///
/// What the realm refuses to make.
pub(crate) fn unpack(args: &[Cloned], context: &mut Context) -> JsResult<Vec<JsValue>> {
    match args.first() {
        Some(all) => native::list_of(&all.to_value(context)?, context),
        None => Ok(Vec::new()),
    }
}

/// What the page that invoked is told of a handler's failure `error`: `String(error)`.
pub(crate) fn error_text(error: &JsValue, context: &mut Context) -> String {
    if error.is_null_or_undefined() {
        return NEVER_SENT.to_owned();
    }
    error.to_string(context).map_or_else(
        |_| NEVER_SENT.to_owned(),
        |text| text.to_std_string_escaped(),
    )
}

/// A failed reply as the page hears it: the text alone, in a clone.
pub(crate) fn failure(text: &str, context: &mut Context) -> Cloned {
    Cloned::from_value(&js_text(text), context).unwrap_or_else(|_| Cloned::undefined())
}
