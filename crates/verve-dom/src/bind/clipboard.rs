//! `navigator.clipboard` and `ClipboardItem`.
//!
//! The clipboard belongs to the shell, so the page only asks: a write becomes
//! [`Request::WriteClipboard`], and a read becomes [`Request::ReadClipboard`] whose answer the
//! embedder delivers with [`crate::Document::settle_clipboard`]. A read first asks the permission
//! backend for `clipboard-read` (P16): a page with no backend is refused with `NotAllowedError`.

use std::collections::HashMap;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, arg, arg_text, js_text, type_error};

use super::{Accessor, Method, WebIdlClass, dom_error, prototype_of_target, set, webidl_class};
use crate::clipboard::ClipboardData;
use crate::state::shared;

/// What a read of the clipboard answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadKind {
    /// `readText()`: the text.
    Text,
    /// `read()`: `ClipboardItem`s.
    Items,
}

/// The reads the shell has not answered yet, by the number each was asked with.
#[derive(Default)]
pub(crate) struct Pending {
    next: u64,
    reads: HashMap<u64, (ResolvingFunctions, ReadKind)>,
    /// The pastes waiting for the clipboard; `true` for one that takes the text alone.
    pastes: HashMap<u64, bool>,
}

/// A paste asks the shell for the clipboard (Ctrl/Cmd+V, the menu's Paste).
pub(crate) fn begin_paste(context: &mut Context, plain: bool) {
    let shared = shared(context);
    let mut state = shared.borrow_mut();
    state.clipboard.next += 1;
    let call = state.clipboard.next;
    state.clipboard.pastes.insert(call, plain);
    state.requests.push(crate::Request::ReadClipboard { call });
}

/// Whether `call` was a paste's question, and whether it takes the text alone.
pub(crate) fn take_paste(context: &mut Context, call: u64) -> Option<bool> {
    shared(context).borrow_mut().clipboard.pastes.remove(&call)
}

/// The permission was granted: asks the shell for the clipboard, to settle `resolvers` with its
/// answer.
pub(crate) fn begin_read(
    context: &mut Context,
    resolvers: &ResolvingFunctions,
    kind: ReadKind,
) -> JsResult<()> {
    let shared = shared(context);
    let mut state = shared.borrow_mut();
    state.clipboard.next += 1;
    let call = state.clipboard.next;
    state
        .clipboard
        .reads
        .insert(call, (resolvers.clone(), kind));
    state.requests.push(crate::Request::ReadClipboard { call });
    Ok(())
}

/// The shell answered read `call` with `data`.
pub(crate) fn finish_read(context: &mut Context, call: u64, data: &ClipboardData) -> JsResult<()> {
    let waiting = shared(context).borrow_mut().clipboard.reads.remove(&call);
    let Some((resolvers, kind)) = waiting else {
        return Ok(());
    };
    let value = match kind {
        ReadKind::Text => Ok(js_text(data.text.as_deref().unwrap_or(""))),
        ReadKind::Items => items_of(context, data),
    };
    let (function, value) = match value {
        Ok(value) => (&resolvers.resolve, value),
        Err(error) => (&resolvers.reject, error.into_opaque(context)?),
    };
    function.call(&JsValue::undefined(), &[value], context)?;
    Ok(())
}

/// `[ClipboardItem]` for what the clipboard holds: one item with a blob per format.
fn items_of(context: &mut Context, data: &ClipboardData) -> JsResult<JsValue> {
    let record = native::object(context);
    let formats = [
        (
            "text/plain",
            data.text.as_ref().map(|t| t.as_bytes().to_vec()),
        ),
        (
            "text/html",
            data.html.as_ref().map(|t| t.as_bytes().to_vec()),
        ),
        ("image/png", data.image_png.clone()),
    ];
    let mut any = false;
    for (kind, bytes) in formats {
        if let Some(bytes) = bytes {
            let blob = verve::blob::new_blob(bytes, kind, context)?;
            set(&record, kind, blob, context)?;
            any = true;
        }
    }
    if !any {
        return Ok(native::array(context, Vec::new()));
    }
    let (class, _) = crate::state::class_of(context, "ClipboardItem")
        .ok_or_else(|| type_error("no ClipboardItem"))?;
    let item = class.construct(&[record.into()], None, context)?;
    Ok(native::array(context, vec![item.into()]))
}

/// The rejection of a call made while the document does not have the focus.
fn unfocused(cx: &mut Context) -> Option<JsResult<JsValue>> {
    if shared(cx).borrow().window_focused {
        return None;
    }
    let error = dom_error(cx, "Document is not focused.", "NotAllowedError");
    Some(native::settled(Err(error), cx))
}

/// The promise constructor of the realm, not whatever a page has put in `Promise`.
fn promise_class(cx: &mut Context) -> JsValue {
    cx.intrinsics()
        .constructors()
        .promise()
        .constructor()
        .into()
}

fn write_text(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if let Some(refused) = unfocused(cx) {
        return refused;
    }
    let text = arg_text(args, 0, cx)?;
    shared(cx)
        .borrow_mut()
        .requests
        .push(crate::Request::WriteClipboard {
            text: Some(text),
            html: None,
            custom: Vec::new(),
        });
    native::settled(Ok(JsValue::undefined()), cx)
}

/// The formats an item was made with, in the order it listed them.
fn item_types(item: &JsValue, cx: &mut Context) -> JsResult<Vec<String>> {
    let types = native::get(item, "types", cx)?;
    native::list_of(&types, cx)?
        .iter()
        .map(|t| native::text_of(t, cx))
        .collect()
}

/// The formats a write can put on the clipboard.
const WRITABLE: &[&str] = &["text/plain", "text/html", "image/png"];

/// `clipboard.write([item])`: once the item's blobs are all there, the clipboard is written with
/// them. More than one item is refused, as Chromium refuses it (the message is in Electron
/// 39.8.10's binary).
fn write(_: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if let Some(refused) = unfocused(cx) {
        return refused;
    }
    let items = native::list_of(&arg(args, 0), cx)?;
    if items.len() > 1 {
        let error = dom_error(
            cx,
            "Support for multiple ClipboardItems is not implemented.",
            "NotAllowedError",
        );
        return native::settled(Err(error), cx);
    }
    let Some(item) = items.first() else {
        return native::settled(Ok(JsValue::undefined()), cx);
    };
    let types = item_types(item, cx)?;
    if let Some(kind) = types.iter().find(|t| !WRITABLE.contains(&t.as_str())) {
        let message = format!(
            "Failed to execute 'write' on 'Clipboard': Type {kind} not supported on write."
        );
        let error = dom_error(cx, &message, "NotAllowedError");
        return native::settled(Err(error), cx);
    }
    let promise = promise_class(cx);
    let mut pending = Vec::new();
    for kind in &types {
        let value = native::invoke(item, "getType", &[js_text(kind)], cx)?;
        pending.push(value);
    }
    let all = native::invoke(&promise, "all", &[native::array(cx, pending)], cx)?;
    let finish = native::closure(
        cx,
        "",
        1,
        |_, args, types: &Vec<String>, cx| {
            let blobs = native::list_of(&arg(args, 0), cx)?;
            let (mut text, mut html, mut custom) = (None, None, Vec::new());
            for (kind, blob) in types.iter().zip(&blobs) {
                let bytes = verve::blob::blob_bytes(blob, cx)?;
                match kind.as_str() {
                    "text/plain" => text = Some(String::from_utf8_lossy(&bytes).into_owned()),
                    "text/html" => html = Some(String::from_utf8_lossy(&bytes).into_owned()),
                    _ => custom.push((kind.clone(), bytes)),
                }
            }
            shared(cx)
                .borrow_mut()
                .requests
                .push(crate::Request::WriteClipboard { text, html, custom });
            Ok(JsValue::undefined())
        },
        types,
    );
    native::invoke(&all, "then", &[finish.into()], cx)
}

fn read_text(_: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if let Some(refused) = unfocused(cx) {
        return refused;
    }
    super::permissions::ask_to_read(cx, ReadKind::Text)
}

fn read(_: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if let Some(refused) = unfocused(cx) {
        return refused;
    }
    super::permissions::ask_to_read(cx, ReadKind::Items)
}

/// Gives `navigator` its `clipboard`.
pub(crate) fn install_clipboard(context: &mut Context, navigator: &JsObject) -> JsResult<()> {
    let clipboard = native::object(context);
    let methods: &[Method] = &[
        ("writeText", 1, write_text),
        ("readText", 0, read_text),
        ("write", 1, write),
        ("read", 0, read),
    ];
    native::methods(context, &clipboard, methods)?;
    navigator.set(js_string!("clipboard"), clipboard, false, context)?;
    Ok(())
}

fn construct_item(new_target: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let prototype = prototype_of_target(new_target, cx)?;
    let given = arg(args, 0);
    let Some(source) = given.as_object() else {
        return Err(type_error(
            "Failed to construct 'ClipboardItem': The provided value is not of type 'record'.",
        ));
    };
    if source.own_property_keys(cx)?.is_empty() {
        return Err(type_error(
            "Failed to construct 'ClipboardItem': At least one entry must be provided",
        ));
    }
    let item = native::object(cx);
    item.set_prototype(Some(prototype));
    let record = native::object(cx);
    for key in source.own_property_keys(cx)? {
        let name = key.to_string();
        let value = source.get(key, cx)?;
        // A blob, or a promise for one: a string is no clipboard item.
        let then = native::get(&value, "then", cx)?;
        if !verve::blob::is_blob(&value, cx) && !then.is_callable() {
            return Err(type_error(
                "Failed to construct 'ClipboardItem': The provided value is not a Blob or a Promise.",
            ));
        }
        set(&record, &name.to_ascii_lowercase(), value, cx)?;
    }
    native::hidden(&item, js_string!("__items"), record, cx)?;
    Ok(item.into())
}

const ITEM_METHODS: &[Method] = &[("getType", 1, |this, args, cx| {
    let kind = arg_text(args, 0, cx)?.to_ascii_lowercase();
    let record = this
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation"))?
        .get(js_string!("__items"), cx)?;
    let value = native::get(&record, &kind, cx)?;
    if value.is_undefined() {
        let error = dom_error(
            cx,
            "Failed to execute 'getType' on 'ClipboardItem': The type was not found",
            "NotFoundError",
        );
        return native::settled(Err(error), cx);
    }
    let promise = promise_class(cx);
    native::invoke(&promise, "resolve", &[value], cx)
})];

const ITEM_ACCESSORS: &[Accessor] = &[
    (
        "types",
        Some(|this, _, cx| {
            let record = record_of(this, cx)?;
            let keys = record
                .own_property_keys(cx)?
                .iter()
                .map(|k| js_text(&k.to_string()))
                .collect();
            Ok(native::array(cx, keys))
        }),
        None,
    ),
    (
        "presentationStyle",
        Some(|_, _, _| Ok(js_text("unspecified"))),
        None,
    ),
];

fn record_of(this: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    let hidden = this
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation: not a ClipboardItem"))?
        .get(js_string!("__items"), cx)?;
    hidden
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation: not a ClipboardItem"))
}

/// Installs `ClipboardItem`.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    webidl_class(
        context,
        WebIdlClass {
            name: "ClipboardItem",
            length: 1,
            construct: construct_item,
            parent: None,
            methods: ITEM_METHODS,
            accessors: ITEM_ACCESSORS,
            statics: &[("supports", 1, |_, args, cx| {
                let kind = arg_text(args, 0, cx)?.to_ascii_lowercase();
                Ok(["text/plain", "text/html", "image/png"]
                    .contains(&kind.as_str())
                    .into())
            })],
            global: true,
        },
    )?;
    Ok(())
}
