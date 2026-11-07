//! Clipboard events and the commands behind them: `copy`, `cut` and `paste`.
//!
//! The clipboard belongs to the shell. A copy tells it what to hold ([`crate::Request::WriteClipboard`]);
//! a paste is handed what it holds ([`ClipboardData`]), which the shell read before the paste
//! began. Sequences are the Clipboard API's and HTML's, as Chromium runs them (`Editor::
//! DispatchClipboardEvent`): the event is cancelable and fires at the focused element, else at
//! the selection's element, else at `<body>`; when nothing cancels it the default action runs
//! on the selection (or a text field's own selection); when a listener cancels it, what it put
//! in `clipboardData` is what gets written. These were taken from the specification and the
//! source, not observed: copying in a real Electron would write the user's clipboard.

use std::path::{Path, PathBuf};

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, js_text};

use crate::bind::data_transfer::{self, Entry, Mode};
use crate::bind::events::fire_with;
use crate::bind::node::{read_dom, with_dom, wrap};
use crate::dom::{Contents, Dom, NodeId};
use crate::{Document, Request};

/// What the clipboard holds, as the shell read it: handed to a paste, or to a script that reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipboardData {
    pub text: Option<String>,
    pub html: Option<String>,
    /// Files copied in the system's file manager.
    pub files: Vec<PathBuf>,
    /// A picture, as PNG.
    pub image_png: Option<Vec<u8>>,
}

/// The most a dropped or pasted file is read whole: a larger file is left out.
const FILE_LIMIT: u64 = 256 * 1024 * 1024;

/// A guess at a file's MIME type from its name, as the platform's table would answer.
fn mime_of(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "txt" | "text" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "",
    }
}

/// A `File` for the file at `path`, which `webUtils.getPathForFile` can name again. `None` when
/// it cannot be read or is too large.
pub(crate) fn file_object(context: &mut Context, path: &Path) -> Option<JsObject> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > FILE_LIMIT {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let file = verve::blob::file_with_path(context, bytes, &name, mime_of(path), path).ok()?;
    file.as_object().map(|o| o.clone())
}

/// A `File` for a picture the clipboard held.
fn image_object(context: &mut Context, png: &[u8]) -> JsResult<JsObject> {
    let class = native::global(context, "File")?;
    let part = boa_engine::object::builtins::JsUint8Array::from_iter(png.iter().copied(), context)?;
    let parts = native::array(context, vec![part.into()]);
    let options = native::object(context);
    options.set(js_string!("type"), js_text("image/png"), false, context)?;
    class
        .as_object()
        .ok_or_else(|| native::type_error("no File"))?
        .construct(
            &[parts, js_text("image.png"), options.into()],
            None,
            context,
        )
}

/// The element a clipboard event goes to: the one with the focus, else the one the selection
/// starts in, else `<body>`.
fn event_target(cx: &Context) -> NodeId {
    read_dom(cx, |dom| {
        let focused = dom.focused.filter(|f| dom.exists(*f));
        let selected = || {
            let (range, _) = dom.selection()?;
            let node = range.start.0;
            if dom.get(node).is_some_and(|n| n.is_element()) {
                Some(node)
            } else {
                dom.parent_element(node)
            }
        };
        focused.or_else(selected).unwrap_or(dom.body)
    })
}

fn is_field(dom: &Dom, id: NodeId) -> bool {
    matches!(
        dom.get(id).and_then(|n| n.tag()),
        Some("input" | "textarea")
    )
}

/// What the selection holds.
struct Held {
    text: String,
    html: Option<String>,
}

fn held_by(cx: &mut Context, target: NodeId) -> Held {
    with_dom(cx, |dom| {
        if is_field(dom, target) {
            crate::bind::element::ensure_draft(dom, target);
            let text = dom
                .get(target)
                .and_then(|n| n.draft.as_ref())
                .map(|d| d.selected_text().to_owned())
                .unwrap_or_default();
            return Held { text, html: None };
        }
        let Some((range, _)) = dom.selection() else {
            return Held {
                text: String::new(),
                html: None,
            };
        };
        let text = dom.range_text(range);
        let html = (!text.is_empty()).then(|| {
            let (fragment, _) = dom.copy_contents(range, Contents::Clone);
            let markup = crate::html::inner(dom, fragment);
            dom.detach(fragment);
            markup
        });
        Held { text, html }
    })
}

/// Tells the shell to write what a listener put in `transfer`.
fn write_transfer(cx: &mut Context, transfer: &JsObject) {
    let (mut text, mut html, mut custom) = (None, None, Vec::new());
    for entry in data_transfer::entries_of(transfer) {
        if let Entry::Text { kind, data } = &entry {
            match kind.as_str() {
                "text/plain" => text = Some(data.clone()),
                "text/html" => html = Some(data.clone()),
                _ => custom.push((kind.clone(), data.clone().into_bytes())),
            }
        }
    }
    if text.is_some() || html.is_some() || !custom.is_empty() {
        crate::state::shared(cx)
            .borrow_mut()
            .requests
            .push(Request::WriteClipboard { text, html, custom });
    }
}

/// Fires a clipboard event of `kind` at `target`, carrying `transfer`.
fn fire_clipboard(
    cx: &mut Context,
    target: NodeId,
    kind: &str,
    transfer: &JsObject,
) -> JsResult<bool> {
    let node = wrap(cx, target);
    let init = [("clipboardData", JsValue::from(transfer.clone()))];
    fire_with(
        cx,
        &node,
        "ClipboardEvent",
        kind,
        &init,
        true,
        |_, _| Ok(()),
    )
    .map(|(allowed, _)| allowed)
}

/// Removes a text field's selected text as a cut does: `deleteByCut`.
fn cut_from_field(cx: &mut Context, field: NodeId, user: bool) -> JsResult<bool> {
    let node = wrap(cx, field);
    let init = [
        ("inputType", js_text("deleteByCut")),
        ("data", JsValue::null()),
    ];
    if user
        && !fire_with(
            cx,
            &node,
            "InputEvent",
            "beforeinput",
            &init,
            true,
            |_, _| Ok(()),
        )?
        .0
    {
        return Ok(false);
    }
    let changed = with_dom(cx, |dom| {
        crate::bind::element::ensure_draft(dom, field);
        let draft = dom.get_mut(field).and_then(|n| n.draft.as_mut());
        draft.is_some_and(|d| d.take_selection())
    });
    if changed {
        fire_with(cx, &node, "InputEvent", "input", &init, true, |_, _| Ok(()))?;
    }
    Ok(changed)
}

/// `copy` (or `cut`): the event, then the default action unless a listener canceled it.
/// `user` is whether a person asked, so that the cut's `beforeinput` is fired. Answers whether
/// anything was copied.
pub(crate) fn copy_or_cut(cx: &mut Context, cut: bool, user: bool) -> JsResult<bool> {
    let target = event_target(cx);
    // Only a field or an editable region can be cut from.
    let editable = read_dom(cx, |dom| {
        is_field(dom, target) || dom.editing_host(target).is_some()
    });
    if cut && !editable {
        return Ok(false);
    }
    let held = held_by(cx, target);
    let transfer = data_transfer::create(cx, Mode::Write, Vec::new(), "uninitialized");
    let kind = if cut { "cut" } else { "copy" };
    if !fire_clipboard(cx, target, kind, &transfer)? {
        write_transfer(cx, &transfer);
        return Ok(true);
    }
    if held.text.is_empty() {
        return Ok(false);
    }
    crate::state::shared(cx)
        .borrow_mut()
        .requests
        .push(Request::WriteClipboard {
            text: Some(held.text),
            html: held.html,
            custom: Vec::new(),
        });
    if cut {
        let host = read_dom(cx, |dom| {
            if is_field(dom, target) {
                None
            } else {
                dom.editing_host(target)
            }
        });
        match host {
            Some(host) => crate::editing::cut_selection(cx, host, user)?,
            None if read_dom(cx, |dom| is_field(dom, target)) => cut_from_field(cx, target, user)?,
            None => false,
        };
    }
    Ok(true)
}

/// A `paste` event, fired: whether it was left alone, where it went, and what it carried.
struct Pasted {
    allowed: bool,
    target: NodeId,
    transfer: JsObject,
}

fn fire_paste(cx: &mut Context, data: &ClipboardData) -> JsResult<Pasted> {
    let target = event_target(cx);
    let mut entries = Vec::new();
    if let Some(text) = &data.text {
        entries.push(Entry::text("text/plain", text));
    }
    if let Some(html) = &data.html {
        entries.push(Entry::text("text/html", html));
    }
    for path in &data.files {
        if let Some(file) = file_object(cx, path) {
            entries.push(Entry::File(file));
        }
    }
    if let Some(png) = &data.image_png {
        entries.push(Entry::File(image_object(cx, png)?));
    }
    let transfer = data_transfer::create(cx, Mode::Read, entries, "uninitialized");
    let allowed = fire_clipboard(cx, target, "paste", &transfer)?;
    Ok(Pasted {
        allowed,
        target,
        transfer,
    })
}

impl Document {
    /// A person pressed, released or typed: for the next five seconds a script may copy and cut
    /// (transient activation, as in Chromium). Trusted input sets it; menu and keybinding
    /// commands need none.
    pub(crate) fn activate(&self) {
        self.state.borrow_mut().activation = Some(std::time::Instant::now());
    }

    /// A person copies (the shortcut, the menu): the `copy` event, then the selection to the
    /// shell as [`Request::WriteClipboard`]. Answers whether anything was copied.
    pub fn copy(&self, runtime: &mut verve::Runtime) -> bool {
        self.clipboard_command(runtime, |cx| copy_or_cut(cx, false, true))
    }

    /// A person cuts: as [`Document::copy`], then the selection is deleted (`deleteByCut`).
    pub fn cut(&self, runtime: &mut verve::Runtime) -> bool {
        self.clipboard_command(runtime, |cx| copy_or_cut(cx, true, true))
    }

    pub(crate) fn clipboard_command(
        &self,
        runtime: &mut verve::Runtime,
        run: impl FnOnce(&mut Context) -> JsResult<bool>,
    ) -> bool {
        let cx = runtime.context();
        let done = run(cx).unwrap_or_else(|error| {
            verve::report(cx, &error);
            false
        });
        runtime.run_microtasks();
        done
    }

    /// A person pastes what the clipboard held (`data`): the `paste` event, then, unless a
    /// listener canceled it, `insertFromPaste` into the field or editable region that has the
    /// focus. Markup pasted into a region is cleaned of script first.
    pub fn paste(&self, runtime: &mut verve::Runtime, data: &ClipboardData) {
        self.paste_as(runtime, data, false);
    }

    /// [`Document::paste`] as the text alone, without the clipboard's formatting
    /// (`pasteAndMatchStyle`).
    pub fn paste_plain(&self, runtime: &mut verve::Runtime, data: &ClipboardData) {
        self.paste_as(runtime, data, true);
    }

    fn paste_as(&self, runtime: &mut verve::Runtime, data: &ClipboardData, plain: bool) {
        let cx = runtime.context();
        let pasted = match fire_paste(cx, data) {
            Ok(pasted) => pasted,
            Err(error) => {
                verve::report(cx, &error);
                return;
            }
        };
        if pasted.allowed {
            if self.is_text_field(pasted.target) {
                let text = crate::editing::pasted_text(
                    runtime.context(),
                    data.text.clone(),
                    data.html.as_deref(),
                );
                self.insert_into(runtime, pasted.target, &text, "insertFromPaste");
            } else if let Some(host) = self.editable_host(pasted.target) {
                let cx = runtime.context();
                if let Err(error) = crate::editing::paste(cx, host, &pasted.transfer, plain) {
                    verve::report(cx, &error);
                }
            }
        }
        runtime.run_microtasks();
    }

    /// Ctrl/Cmd+V, or the menu's Paste: the clipboard is the shell's, so the document asks for it
    /// ([`Request::ReadClipboard`]) and pastes what [`Document::settle_clipboard`] is handed.
    /// `plain` pastes the text alone (`pasteAndMatchStyle`, Ctrl+Shift+V).
    pub fn request_paste(&self, runtime: &mut verve::Runtime, plain: bool) {
        crate::bind::clipboard::begin_paste(runtime.context(), plain);
    }

    /// The shell answered `call`, a `navigator.clipboard` read, with what the clipboard holds.
    pub fn settle_clipboard(&self, runtime: &mut verve::Runtime, call: u64, data: &ClipboardData) {
        let paste = crate::bind::clipboard::take_paste(runtime.context(), call);
        if let Some(plain) = paste {
            self.paste_as(runtime, data, plain);
            return;
        }
        let cx = runtime.context();
        if let Err(error) = crate::bind::clipboard::finish_read(cx, call, data) {
            verve::report(cx, &error);
        }
        runtime.run_microtasks();
    }
}
