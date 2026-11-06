//! The members of `webContents` that reach into the page: running a script, adding a sheet, the
//! editing commands and `sendInputEvent`.
//!
//! The page does these on its own thread; each member here sends one message, and a script's
//! result comes back as the promise's value ([`Calls`](super::page_calls::Calls)).

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue};
use verve::native::{self, arg};
use verve_dom::{Key, Modifiers, Pointer};

use super::define::{self, Args, Fields};
use super::shared;
use super::web_contents::record_of;
use crate::msg::{EditCommand, PointerKind, ToRenderer, ToRendererInput};

/// The editing methods that take nothing: their name and the command.
/// A method that does one editing command: its name, and the command.
type Command = (&'static str, fn() -> EditCommand);

const COMMANDS: &[Command] = &[
    ("undo", || EditCommand::Undo),
    ("redo", || EditCommand::Redo),
    ("cut", || EditCommand::Cut),
    ("copy", || EditCommand::Copy),
    ("paste", || EditCommand::Paste),
    ("pasteAndMatchStyle", || EditCommand::PasteAndMatchStyle),
    ("delete", || EditCommand::Delete),
    ("selectAll", || EditCommand::SelectAll),
    ("unselect", || EditCommand::Unselect),
    ("scrollToTop", || EditCommand::ScrollToTop),
    ("scrollToBottom", || EditCommand::ScrollToBottom),
];

/// Gives `prototype` the members of this file.
pub(crate) fn install(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    for (index, (name, _)) in COMMANDS.iter().enumerate() {
        let method = native::closure(context, name, 0, edit, index);
        native::hidden(prototype, JsString::from(*name), method, context)?;
    }
    native::methods(
        context,
        prototype,
        &[
            ("executeJavaScript", 1, execute),
            ("executeJavaScriptInIsolatedWorld", 2, execute_in_world),
            ("insertCSS", 1, insert_css),
            ("removeInsertedCSS", 1, remove_css),
            ("insertText", 1, insert_text),
            ("sendInputEvent", 1, send_input_event),
        ],
    )
}

/// `executeJavaScript(code[, userGesture])`: the promise of what the script gave. Electron
/// takes any value as the code, written out as text.
fn execute(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let code = native::text_of(&arg(args, 0), context)?;
    run(context, record.page.contents, code, 0)
}

/// `executeJavaScriptInIsolatedWorld(worldId, scripts)`: the scripts run one after the other in
/// the world, and the promise has what the last one gave.
fn execute_in_world(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let world = Args::new(args).f64(0, "worldId", context)? as i32;
    let mut code = Vec::new();
    for script in native::list_of(&arg(args, 1), context)? {
        let text = native::get(&script, "code", context)?;
        code.push(native::text_of(&text, context)?);
    }
    run(context, record.page.contents, code.join(";\n"), world)
}

fn run(
    context: &mut Context,
    contents: crate::ids::ContentsId,
    code: String,
    world: i32,
) -> JsResult<JsValue> {
    let calls = &shared(context).calls;
    calls.execute(context, contents, (code, world))
}

/// `insertCSS(css[, { cssOrigin }])`: the promise of the key `removeInsertedCSS` takes.
fn insert_css(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let css = Args::new(args).string(0, "css", context)?;
    let given = args.get(1).and_then(JsValue::as_object);
    let user = Fields::new(given.as_ref())
        .string("cssOrigin", context)?
        .as_deref()
        == Some("user");
    let number = record.page_state.sheets.get() + 1;
    record.page_state.sheets.set(number);
    let key = format!("webcontents-css-{number}");
    record.page.to_renderer(ToRenderer::InsertCss {
        key: key.clone(),
        css,
        user,
    });
    native::settled(Ok(native::js_text(&key)), context)
}

fn remove_css(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let key = Args::new(args).string(0, "key", context)?;
    record.page.to_renderer(ToRenderer::RemoveCss { key });
    native::settled(Ok(JsValue::undefined()), context)
}

fn edit(this: &JsValue, _: &[JsValue], index: &usize, context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    record
        .page
        .to_renderer(ToRenderer::EditCommand(COMMANDS[*index].1()));
    Ok(JsValue::undefined())
}

/// `insertText(text)`: typed into what has the focus; the promise settles once it is sent.
fn insert_text(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let text = Args::new(args).string(0, "text", context)?;
    record
        .page
        .to_renderer(ToRenderer::EditCommand(EditCommand::InsertText(text)));
    native::settled(Ok(JsValue::undefined()), context)
}

// ---------- sendInputEvent ----------

/// `sendInputEvent(event)`: the event reaches the page as a person's would, past
/// `before-input-event`.
fn send_input_event(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let Some(event) = arg(args, 0).as_object() else {
        return Err(native::type_error("Invalid event object"));
    };
    let hub = shared(context).hub.clone();
    for input in input_of(&event, context)? {
        hub.input(record.page.contents, input);
    }
    Ok(JsValue::undefined())
}

/// The input a script's event object stands for.
fn input_of(event: &JsObject, context: &mut Context) -> JsResult<Vec<ToRendererInput>> {
    let fields = Fields::new(Some(event));
    let kind = fields.string("type", context)?.unwrap_or_default();
    let modifiers = modifiers_of(event, context)?;
    let button = match fields.string("button", context)?.as_deref() {
        Some("middle") => 1,
        Some("right") => 2,
        _ => 0,
    };
    let pointer = Pointer {
        x: fields.f32("x", 0., context)?,
        y: fields.f32("y", 0., context)?,
        button,
        count: fields.f64("clickCount", 1., context)? as u32,
        modifiers,
    };
    let at = |kind| ToRendererInput::Pointer {
        kind,
        pointer,
        target: None,
    };
    let code = fields.string("keyCode", context)?.unwrap_or_default();
    let key = Key {
        modifiers,
        ..key_named(&code)
    };
    Ok(match kind.as_str() {
        "mouseDown" => vec![at(PointerKind::Down)],
        "mouseUp" => vec![at(PointerKind::Up)],
        "mouseMove" | "mouseEnter" | "mouseLeave" => vec![at(PointerKind::Move)],
        "contextMenu" => {
            let right = Pointer {
                button: 2,
                ..pointer
            };
            let at = |kind| ToRendererInput::Pointer {
                kind,
                pointer: right,
                target: None,
            };
            vec![at(PointerKind::Down), at(PointerKind::Up)]
        }
        "mouseWheel" => vec![ToRendererInput::Wheel {
            pointer,
            dx: fields.f32("deltaX", 0., context)?,
            dy: fields.f32("deltaY", 0., context)?,
            target: None,
        }],
        "keyDown" | "rawKeyDown" => vec![ToRendererInput::Key { down: true, key }],
        "keyUp" => vec![ToRendererInput::Key { down: false, key }],
        "char" => {
            vec![ToRendererInput::Text(code.chars().take(1).collect())]
        }
        _ => return Err(native::type_error("Invalid event object")),
    })
}

/// The modifier keys an event's `modifiers` list holds.
fn modifiers_of(event: &JsObject, context: &mut Context) -> JsResult<Modifiers> {
    let list = define::Fields::new(Some(event)).object("modifiers", context)?;
    let mut held = Modifiers::default();
    for item in
        native::list_of(&list.map_or(JsValue::undefined(), Into::into), context).unwrap_or_default()
    {
        match item
            .as_string()
            .map(|s| s.to_std_string_escaped().to_ascii_lowercase())
            .as_deref()
        {
            Some("shift") => held.shift = true,
            Some("control" | "ctrl") => held.ctrl = true,
            Some("alt") => held.alt = true,
            Some("meta" | "cmd" | "command" | "super") => held.meta = true,
            _ => {}
        }
    }
    Ok(held)
}

/// The key an Electron key code names (`'A'`, `'Enter'`, `'Left'`), as the page's `KeyboardEvent`
/// has it.
fn key_named(name: &str) -> Key {
    let mut chars = name.chars();
    if let (Some(only), None) = (chars.next(), chars.next()) {
        return Key::char(only);
    }
    match name {
        "Space" => Key::char(' '),
        "Left" => Key::named("ArrowLeft"),
        "Right" => Key::named("ArrowRight"),
        "Up" => Key::named("ArrowUp"),
        "Down" => Key::named("ArrowDown"),
        "Esc" => Key::named("Escape"),
        "Return" => Key::named("Enter"),
        other => Key::named(other),
    }
}
