//! What an error nobody caught says on the console of a realm that has a sink: where it was
//! thrown, taken from its stack.

use std::sync::atomic::{AtomicBool, Ordering};

use boa_engine::property::PropertyKey;
use boa_engine::{Context, JsError, JsObject, JsString, JsSymbol, JsValue, js_string};

use crate::native::{self, invoke, js_text};
use crate::{Level, SourcePos, console_line, console_line_at, describe, shared, thrown_at};

/// Whether the first warning of the process has said how to trace warnings.
static TRACE_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

/// Writes the console line of an error nobody caught, `prefix` first, and answers the line with
/// the frames it was thrown from. A console that has a sink is told where the error was thrown
/// instead of getting the frames in the text, and a string that was thrown without its quotes, as
/// a browser's console is.
pub(crate) fn console_uncaught(context: &mut Context, prefix: &str, error: &JsError) -> String {
    let hosted = shared(context).console_sink.borrow().is_some();
    let text = match hosted.then(|| thrown_string(context, error)).flatten() {
        Some(text) => text,
        None => describe(context, error),
    };
    let frames = thrown_at(context, error);
    let line = if frames.is_empty() {
        format!("{prefix} {text}")
    } else {
        format!("{prefix} {text} ({frames})")
    };
    match hosted.then(|| thrown_position(context, error)).flatten() {
        Some(at) => console_line_at(context, Level::Error, format!("{prefix} {text}"), Some(at)),
        None => console_line(context, Level::Error, line.clone()),
    }
    line
}

/// The text of a string that was thrown, as a browser's console writes it: without quotes.
fn thrown_string(context: &mut Context, error: &JsError) -> Option<String> {
    let value = error.clone().into_opaque(context).ok()?;
    Some(value.as_string()?.to_std_string_escaped())
}

/// Where an error was thrown: the first frame of its stack that is a script's.
fn thrown_position(context: &mut Context, error: &JsError) -> Option<SourcePos> {
    let value = error.clone().into_opaque(context).ok()?;
    let stack = value.as_object()?.get(js_string!("stack"), context).ok()?;
    let stack = stack.as_string()?.to_std_string_escaped();
    stack.lines().find_map(|frame| frame_position(frame.trim()))
}

/// The place a stack frame names, if it names one in a script. Boa writes `at name (file:line:column)`
/// for a named function and `at file:line:column` for top-level code and an unnamed function.
fn frame_position(frame: &str) -> Option<SourcePos> {
    let frame = frame.strip_prefix("at ")?;
    let place = match frame.strip_suffix(')') {
        Some(inner) => inner.rsplit_once('(')?.1,
        None => frame,
    };
    let mut parts = place.rsplitn(3, ':');
    let column = parts.next()?.parse().ok()?;
    let line = parts.next()?.parse().ok()?;
    let file = parts.next()?;
    if file == "<anonymous>" {
        return None;
    }
    Some(SourcePos {
        file: file.to_owned(),
        line,
        column,
    })
}

/// What Node adds to the first warning it writes: "(Use `node --trace-warnings ...` to show where
/// the warning was created)", named by the program's own file name — with `--trace-deprecation`
/// when the warning is a `DeprecationWarning`. Only a host that reports its uncaught errors has it
/// (an Electron main process); `verve run` writes its warnings as before.
pub(crate) fn trace_hint(context: &mut Context, warning: &str) -> Option<String> {
    if !crate::process::reports_to_host(context) || TRACE_HINT_SHOWN.swap(true, Ordering::SeqCst) {
        return None;
    }
    let process = native::global(context, "process").ok()?;
    let argv0 = native::get(&process, "argv0", context).ok()?;
    let argv0 = argv0.as_string().map(|s| s.to_std_string_escaped());
    Some(hint_text(argv0.as_deref().unwrap_or("node"), warning))
}

/// The hint for a first warning named `warning`, for a program started as `argv0`.
fn hint_text(argv0: &str, warning: &str) -> String {
    let name = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    let flag = if warning == "DeprecationWarning" {
        "--trace-deprecation"
    } else {
        "--trace-warnings"
    };
    format!("(Use `{name} {flag} ...` to show where the warning was created)")
}

/// A rejection nobody handled, in a host that reports its uncaught errors (an Electron main
/// process): Electron 39.8.10 runs Node in the `warn-with-error-code` mode (not `warn`, which
/// warns even when a listener heard the rejection), so the rejection is two warnings — the reason
/// (its stack, if it is an error) and the advice with the rejection's number `id` — and no
/// `uncaughtException`, no error box, and the process goes on with `process.exitCode` set to 1.
pub(crate) fn warn_unhandled_rejection(context: &mut Context, reason: &JsValue, id: u64) {
    let process = native::global(context, "process").unwrap_or_default();
    let text = reason_text(context, reason);
    let kind = js_text("UnhandledPromiseRejectionWarning");
    let _ = invoke(&process, "emitWarning", &[js_text(&text), kind], context);
    warn(context, "UnhandledPromiseRejectionWarning", &advice(id));
    if let Some(process) = process.as_object() {
        let _ = process.set(js_string!("exitCode"), 1, false, context);
    }
}

/// A rejection that was warned of and has a handler now: Node's second look at it, when nobody
/// listens for `rejectionHandled` (Electron 39.8.10 writes it).
pub(crate) fn warn_handled_rejection(context: &mut Context, id: u64) {
    let message = format!("Promise rejection was handled asynchronously (rejection id: {id})");
    warn(context, "PromiseRejectionHandledWarning", &message);
}

/// `process.emitWarning` of an error named `name`.
fn warn(context: &mut Context, name: &str, message: &str) {
    let process = native::global(context, "process").unwrap_or_default();
    let warning = boa_engine::JsNativeError::error()
        .with_message(message.to_owned())
        .into_opaque(context);
    let _ = warning.set(js_string!("name"), js_text(name), false, context);
    let _ = invoke(&process, "emitWarning", &[warning.into()], context);
}

/// The value of the data property `key` of `object` or of the nearest prototype that has one;
/// `None` when the first property found is an accessor, or when there is none. Nothing runs.
fn data_property(object: &JsObject, key: &PropertyKey) -> Option<JsValue> {
    let mut at = Some(object.clone());
    while let Some(current) = at {
        let found = current.borrow().properties().get(key);
        if let Some(descriptor) = found {
            return descriptor.value().cloned();
        }
        at = current.prototype();
    }
    None
}

/// The text of the string data property `name` of `object`, as `data_property` finds it.
fn data_text(object: &JsObject, name: JsString) -> Option<String> {
    let value = data_property(object, &PropertyKey::from(name))?;
    Some(value.as_string()?.to_std_string_escaped())
}

/// What Node writes of a rejection's reason: the `stack` an error-like object owns, else V8's
/// `NoSideEffectsToString` of it (Electron 39.8.10, observed): a function's source, a BigInt's
/// digits, `#<Name>` for an object whose `toString` is `Object.prototype.toString`, `Name: message`
/// for one whose `toString` is `Error.prototype.toString`, else `[object Tag]`. No getter and no
/// user function runs. Anything else is shown as `util.inspect` shows it.
fn reason_text(context: &mut Context, reason: &JsValue) -> String {
    if let Some(text) = reason.as_string() {
        return text.to_std_string_escaped();
    }
    if let Some(number) = reason.as_bigint() {
        return number.to_string();
    }
    let Some(object) = reason.as_object() else {
        return crate::inspect_value(context, reason);
    };
    let own_stack = object
        .borrow()
        .properties()
        .get(&js_string!("stack").into());
    let stack = own_stack.and_then(|d| d.value().cloned());
    if let Some(stack) = stack.as_ref().and_then(JsValue::as_string) {
        return stack.to_std_string_escaped();
    }
    let constructors = context.intrinsics().constructors();
    if object.is_callable() {
        let function = constructors.function().prototype();
        let to_string = data_property(&function, &js_string!("toString").into());
        let text = to_string
            .as_ref()
            .and_then(JsValue::as_callable)
            .and_then(|f| f.call(reason, &[], context).ok());
        return match text.as_ref().and_then(JsValue::as_string) {
            Some(text) => text.to_std_string_escaped(),
            None => crate::inspect_value(context, reason),
        };
    }
    let object_to_string = {
        let prototype = constructors.object().prototype();
        data_property(&prototype, &js_string!("toString").into())
    };
    let error_to_string = {
        let prototype = constructors.error().prototype();
        data_property(&prototype, &js_string!("toString").into())
    };
    let to_string = data_property(&object, &js_string!("toString").into());
    let is = |known: &Option<JsValue>| match (&to_string, known) {
        (Some(a), Some(b)) => a
            .as_object()
            .zip(b.as_object())
            .is_some_and(|(a, b)| JsObject::equals(&a, &b)),
        _ => false,
    };
    if is(&object_to_string) {
        // A `constructor` that is an accessor is not run: the name is not known.
        let constructor = data_property(&object, &js_string!("constructor").into());
        let name = constructor
            .as_ref()
            .and_then(JsValue::as_object)
            .and_then(|c| data_text(&c, js_string!("name")))
            .filter(|name| !name.is_empty());
        if let Some(name) = name {
            return format!("#<{name}>");
        }
    }
    if is(&error_to_string) {
        let name = data_text(&object, js_string!("name")).unwrap_or_else(|| "Error".to_owned());
        let message = data_text(&object, js_string!("message")).unwrap_or_default();
        return match (name.is_empty(), message.is_empty()) {
            (true, _) => message,
            (_, true) => name,
            _ => format!("{name}: {message}"),
        };
    }
    let tag = data_property(&object, &JsSymbol::to_string_tag().into())
        .and_then(|tag| tag.as_string().map(|s| s.to_std_string_escaped()));
    let builtin = if object.is_array() {
        "Array"
    } else if object.is::<boa_engine::builtins::date::Date>() {
        "Date"
    } else if object.is::<boa_engine::builtins::regexp::RegExp>() {
        "RegExp"
    } else {
        "Object"
    };
    format!("[object {}]", tag.as_deref().unwrap_or(builtin))
}

/// Node's advice for an unhandled rejection numbered `id`.
fn advice(id: u64) -> String {
    format!(
        "Unhandled promise rejection. This error originated either by throwing inside of an \
         async function without a catch block, or by rejecting a promise which was not handled \
         with .catch(). To terminate the node process on unhandled promise rejection, use the \
         CLI flag `--unhandled-rejections=strict` (see \
         https://nodejs.org/api/cli.html#cli_unhandled_rejections_mode). (rejection id: {id})"
    )
}

#[cfg(test)]
mod tests {
    use super::{advice, frame_position, hint_text};

    #[test]
    fn the_hint_names_the_flag_of_the_first_warning_and_the_program() {
        let plain = hint_text("C:\\app\\electron.exe", "UnhandledPromiseRejectionWarning");
        assert_eq!(
            plain,
            "(Use `electron --trace-warnings ...` to show where the warning was created)"
        );
        let deprecated = hint_text("/usr/bin/node", "DeprecationWarning");
        assert_eq!(
            deprecated,
            "(Use `node --trace-deprecation ...` to show where the warning was created)"
        );
    }

    #[test]
    fn the_advice_names_the_rejection_and_nothing_of_its_reason() {
        let text = advice(3);
        assert!(text.starts_with("Unhandled promise rejection. This error originated"));
        assert!(text.ends_with("(rejection id: 3)"));
        assert!(text.contains("`--unhandled-rejections=strict` (see https://nodejs.org"));
    }

    fn place(frame: &str) -> Option<(String, u32, u32)> {
        frame_position(frame).map(|at| (at.file, at.line, at.column))
    }

    #[test]
    fn a_frame_names_its_place_with_or_without_a_function() {
        let named = place("at run (file:///app/page.js:3:9)");
        assert_eq!(named, Some(("file:///app/page.js".to_owned(), 3, 9)));
        let bare = place("at file:///C:/app/page.js:2:26");
        assert_eq!(bare, Some(("file:///C:/app/page.js".to_owned(), 2, 26)));
    }

    #[test]
    fn a_frame_without_a_script_names_no_place() {
        assert_eq!(place("at <anonymous> (native)"), None);
        assert_eq!(place("at <anonymous>:1:2"), None);
        assert_eq!(place("Error: boom"), None);
    }
}
