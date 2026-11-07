//! `'console-message'`: a line of a page's console, heard as an event of its `webContents`.
//!
//! Electron 39.8.10 emits `(event, level, message, lineNumber, sourceId)`, where `event` is an
//! object with `level` (`'info'`, `'warning'`, `'error'` or `'debug'`), `message`, `lineNumber`,
//! `sourceId`, `frame` (the main frame) and `_level` (the number the positional `level` is: debug
//! 0, info 1, warning 2, error 3). A listener that declares more than the event is told once, on
//! the main process's console, that the positional arguments are deprecated.

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::Level;
use verve::native::{self, js_text};

use super::{contents_ipc, define, shared};
use crate::ids::ContentsId;

/// What Electron calls a level, and the number it gives it.
fn level_of(level: Level) -> (&'static str, u8) {
    match level {
        Level::Debug => ("debug", 0),
        Level::Log | Level::Info => ("info", 1),
        Level::Warn => ("warning", 2),
        Level::Error => ("error", 3),
    }
}

/// The deprecation line Electron writes, once, when a `'console-message'` listener takes more than
/// the event.
const DEPRECATED: &str = "(electron) 'console-message' arguments are deprecated and will be removed. Please use Event<WebContentsConsoleMessageEventParams> object instead.";

/// Page `contents` wrote `message` to its console at `line` of `source`.
pub(crate) fn console_message(
    context: &mut Context,
    contents: ContentsId,
    level: Level,
    message: String,
    place: (u32, String),
) {
    if shared(context).showing(contents).is_none() {
        return;
    }
    let result = contents_ipc::contents_object(context, contents)
        .and_then(|object| emit(context, &object, contents, (level, message), place));
    define::report(context, result);
}

fn emit(
    context: &mut Context,
    object: &JsObject,
    contents: ContentsId,
    (level, message): (Level, String),
    (line, source): (u32, String),
) -> JsResult<()> {
    let listeners = native::invoke(
        &object.clone().into(),
        "listeners",
        &[js_text("console-message")],
        context,
    )?;
    let listeners = native::list_of(&listeners, context)?;
    if listeners.is_empty() {
        return Ok(());
    }
    if wants_positional(&listeners, context)? {
        define::deprecate_once(context, "console-message", DEPRECATED)?;
    }
    let (name, number) = level_of(level);
    let frame = contents_ipc::page(context, contents)?.map(|page| JsValue::from(page.frame));
    let event = define::event(context)?;
    let fields = [
        ("_level", number.into()),
        ("frame", frame.unwrap_or_default()),
        ("level", js_text(name)),
        ("lineNumber", line.into()),
        ("message", js_text(&message)),
        ("sourceId", js_text(&source)),
    ];
    for (key, value) in fields {
        event.set(boa_engine::JsString::from(key), value, false, context)?;
    }
    let positional = [
        event.into(),
        number.into(),
        js_text(&message),
        line.into(),
        js_text(&source),
    ];
    define::emit(context, object, "console-message", &positional)
}

/// Whether a listener declares more than the event: Electron's `listener.length > 1`.
fn wants_positional(listeners: &[JsValue], context: &mut Context) -> JsResult<bool> {
    for listener in listeners {
        let length = native::get(listener, "length", context)?;
        if length.as_number().is_some_and(|n| n > 1.) {
            return Ok(true);
        }
    }
    Ok(false)
}
