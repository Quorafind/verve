//! What an error nobody caught says on the console of a realm that has a sink: where it was
//! thrown, taken from its stack.

use boa_engine::{Context, JsError, js_string};

use crate::{Level, SourcePos, console_line, console_line_at, describe, shared, thrown_at};

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

#[cfg(test)]
mod tests {
    use super::frame_position;

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
