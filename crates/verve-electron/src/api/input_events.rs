//! The input events `webContents` tells the main process of: `before-input-event`, which may stop
//! a key reaching the page, and `context-menu`, which the page asks for with a right click.
//!
//! The shell does not wait for JavaScript, so a key goes to the main process first only while a
//! `before-input-event` listener exists: adding the first listener tells the Hub to have the shell
//! send the page's keys here ([`FromShell::KeyForContents`]), removing the last one stops it.
//!
//! [`FromShell::KeyForContents`]: crate::msg::FromShell::KeyForContents

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, js_text};
use verve_dom::Key;

use super::window_open::referrer_of;
use super::windows::WindowRecord;
use super::{contents_ipc, define, shared};
use crate::ids::ContentsId;
use crate::msg::{ContextMenuParams, ToRendererInput};

const BEFORE_INPUT: &str = "before-input-event";

/// Has the page's `webContents` object `object` tell the Hub when it gains or loses its last
/// `before-input-event` listener.
pub(crate) fn watch_listeners(
    context: &mut Context,
    object: &JsObject,
    id: ContentsId,
) -> JsResult<()> {
    let added = native::closure(
        context,
        "",
        2,
        |_, args, id: &ContentsId, cx| {
            let name = native::text_of(&native::arg(args, 0), cx).unwrap_or_default();
            if name == BEFORE_INPUT {
                shared(cx).hub.set_intercept_keys(*id, true);
            }
            Ok(JsValue::undefined())
        },
        id,
    );
    let removed = native::closure(
        context,
        "",
        2,
        |this, _, id: &ContentsId, cx| {
            let count = native::invoke(this, "listenerCount", &[js_text(BEFORE_INPUT)], cx)?;
            if count.as_number() == Some(0.) {
                shared(cx).hub.set_intercept_keys(*id, false);
            }
            Ok(JsValue::undefined())
        },
        id,
    );
    let this = JsValue::from(object.clone());
    native::invoke(
        &this,
        "on",
        &[js_text("newListener"), added.into()],
        context,
    )?;
    native::invoke(
        &this,
        "on",
        &[js_text("removeListener"), removed.into()],
        context,
    )?;
    Ok(())
}

/// The shell sent a key meant for page `contents`: `before-input-event`, and the page gets the key
/// unless a listener prevented it. Electron 39.8.10 (a visible window, `p32` menu probes): a key
/// down a listener prevented reaches neither the page nor, as its key up, `before-input-event`.
pub(crate) fn key_for(context: &mut Context, contents: ContentsId, down: bool, key: Key) {
    let menus = super::menu_state::state(context);
    if !down && menus.release(contents, &key.code) {
        return;
    }
    let prevented = match contents_ipc::contents_object(context, contents) {
        Ok(object) => {
            let input = input_object(context, down, &key);
            let emitted = input.and_then(|input| {
                define::emit_cancelable(context, &object, BEFORE_INPUT, &[input.into()])
            });
            define::report(context, emitted).unwrap_or(false)
        }
        Err(_) => false,
    };
    if prevented {
        if down {
            menus.swallow(contents, &key.code);
        }
        return;
    }
    if down {
        menus.pass(contents, &key.code);
    }
    shared(context)
        .hub
        .input(contents, ToRendererInput::Key { down, key });
}

/// The page of `contents` left a key down alone (it neither prevented it nor used it for more
/// than typing): the menu's accelerator for it, if any, is chosen. The page heard the key first
/// (Electron 39.8.10 on Windows and Linux; macOS was not observed and follows the same rule).
pub(crate) fn key_unhandled(context: &mut Context, contents: ContentsId, key: Key) {
    let window = shared(context).hub.window_of(contents);
    let menus = super::menu_state::state(context);
    let chosen = window.and_then(|window| menus.item_for(window, &key));
    if let Some(item) = chosen {
        super::menu_state::clicked(context, item, window, true, key.modifiers);
    }
}

/// The `input` a `before-input-event` listener is given.
fn input_object(context: &mut Context, down: bool, key: &Key) -> JsResult<JsObject> {
    let held = [
        ("shift", key.modifiers.shift),
        ("control", key.modifiers.ctrl),
        ("alt", key.modifiers.alt),
        ("meta", key.modifiers.meta),
    ];
    let names = held
        .iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| js_text(name))
        .collect();
    let modifiers = native::array(context, names);
    let mut fields = vec![
        ("type", js_text(if down { "keyDown" } else { "keyUp" })),
        ("key", js_text(&key.key)),
        ("code", js_text(&key.code)),
        ("isAutoRepeat", key.repeat.into()),
        ("isComposing", false.into()),
        ("location", 0.into()),
        ("modifiers", modifiers),
    ];
    fields.extend(held.iter().map(|(name, on)| (*name, JsValue::from(*on))));
    define::record(context, &fields)
}

/// The page asked for a context menu: `context-menu(event, params)`. Electron shows none of its
/// own; with no listener nothing happens.
pub(crate) fn context_menu(context: &mut Context, contents: ContentsId, params: ContextMenuParams) {
    let emitted = (|| {
        let Some(record) = shared(context).showing(contents) else {
            return Ok(());
        };
        let object = contents_ipc::contents_object(context, contents)?;
        let event = define::event(context)?;
        let params = params_object(context, &record, &params)?;
        define::emit(
            context,
            &object,
            "context-menu",
            &[event.into(), params.into()],
        )
    })();
    define::report(context, emitted);
}

/// What the page can do with the text under the menu: edit it when it is a field, copy what is
/// selected.
fn edit_flags(context: &mut Context, params: &ContextMenuParams) -> JsResult<JsObject> {
    let (editable, selected) = (params.is_editable, !params.selection_text.is_empty());
    let flags = [
        ("canUndo", editable),
        ("canRedo", editable),
        ("canCut", editable && selected),
        ("canCopy", selected),
        ("canPaste", editable),
        ("canDelete", editable && selected),
        ("canSelectAll", true),
        ("canEditRichly", editable),
    ];
    let flags: Vec<_> = flags
        .iter()
        .map(|(name, on)| (*name, JsValue::from(*on)))
        .collect();
    define::record(context, &flags)
}

/// `mediaFlags`: Verve plays no media, so every one is false.
fn media_flags(context: &mut Context) -> JsResult<JsObject> {
    let names = [
        "inError",
        "isPaused",
        "isMuted",
        "hasAudio",
        "isLooping",
        "isControlsVisible",
        "canToggleControls",
        "canPrint",
        "canSave",
        "canShowPictureInPicture",
        "isShowingPictureInPicture",
        "canRotate",
        "canLoop",
    ];
    let flags: Vec<_> = names
        .iter()
        .map(|name| (*name, JsValue::from(false)))
        .collect();
    define::record(context, &flags)
}

/// The `params` of `context-menu`: what the page knew, and the answers Verve has for the rest.
fn params_object(
    context: &mut Context,
    record: &WindowRecord,
    params: &ContextMenuParams,
) -> JsResult<JsObject> {
    let media = media_flags(context)?;
    let frame = contents_ipc::page(context, record.page.contents)?
        .map_or(JsValue::undefined(), |page| page.frame.into());
    let referrer = referrer_of(context, record)?;
    let rect = define::record(
        context,
        &[
            ("x", params.x.into()),
            ("y", params.y.into()),
            ("width", 0.into()),
            ("height", 0.into()),
        ],
    )?;
    let suggestions = native::array(context, Vec::new());
    let edit_flags = edit_flags(context, params)?;
    define::record(
        context,
        &[
            ("x", params.x.into()),
            ("y", params.y.into()),
            ("frame", frame),
            ("linkURL", js_text(&params.link_url)),
            ("linkText", js_text(&params.link_text)),
            ("pageURL", js_text(&record.page_state.url.borrow())),
            ("frameURL", js_text("")),
            ("srcURL", js_text(&params.src_url)),
            ("mediaType", js_text(&params.media_type)),
            ("hasImageContents", (params.media_type == "image").into()),
            ("isEditable", params.is_editable.into()),
            ("selectionText", js_text(&params.selection_text)),
            ("titleText", js_text(&params.title_text)),
            ("altText", js_text(&params.alt_text)),
            ("suggestedFilename", js_text("")),
            ("selectionRect", rect.into()),
            ("selectionStartOffset", 0.into()),
            ("referrerPolicy", referrer.into()),
            ("misspelledWord", js_text("")),
            ("dictionarySuggestions", suggestions),
            ("frameCharset", js_text("UTF-8")),
            ("formControlType", js_text("none")),
            ("spellcheckEnabled", true.into()),
            ("menuSourceType", js_text("mouse")),
            ("mediaFlags", media.into()),
            ("editFlags", edit_flags.into()),
        ],
    )
}
