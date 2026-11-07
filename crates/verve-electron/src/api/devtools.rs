//! `webContents`' DevTools: `openDevTools()`, `closeDevTools()`, `toggleDevTools()`,
//! `isDevToolsOpened()`, `isDevToolsFocused()`, `devToolsWebContents` and `inspectElement(x, y)`.
//!
//! Verve has no Chromium DevTools. Opening them says `'devtools-opened'`, makes the page dump its
//! element tree to the terminal (`inspector.rs`) and sends the page's console there for as long as
//! they stay open; `options` are accepted and ignored, there is no DevTools `webContents`, and the
//! DevTools never have the focus. A window made with `webPreferences.devTools: false` cannot open
//! them (Electron 39.8.10: the call does nothing and no event is said). The `toggleDevTools` menu
//! role calls the same methods.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native;

use super::define::{self, Args};
use super::web_contents::record_of;
use super::{contents_ipc, shared};
use crate::ids::ContentsId;
use crate::msg::ToRenderer;

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("openDevTools", 0, |this, _, cx| {
        let record = record_of(this, cx)?;
        open(cx, record.page.contents);
        Ok(JsValue::undefined())
    }),
    ("closeDevTools", 0, |this, _, cx| {
        let record = record_of(this, cx)?;
        close(cx, record.page.contents);
        Ok(JsValue::undefined())
    }),
    ("toggleDevTools", 0, |this, _, cx| {
        let contents = record_of(this, cx)?.page.contents;
        if shared(cx).hub.is_devtools_open(contents) {
            close(cx, contents);
        } else {
            open(cx, contents);
        }
        Ok(JsValue::undefined())
    }),
    ("isDevToolsOpened", 0, |this, _, cx| {
        let contents = record_of(this, cx)?.page.contents;
        Ok(shared(cx).hub.is_devtools_open(contents).into())
    }),
    ("isDevToolsFocused", 0, |this, _, cx| {
        record_of(this, cx)?;
        Ok(false.into())
    }),
    ("inspectElement", 2, inspect_element),
];

/// Whether page `contents` may have DevTools: `webPreferences.devTools` is not `false`.
fn allowed(context: &Context, contents: ContentsId) -> bool {
    let hub = &shared(context).hub;
    hub.preferences_of(contents)
        .is_none_or(|prefs| prefs.dev_tools)
}

/// Opens the DevTools of page `contents`, unless they are open or the page has none.
fn open(context: &mut Context, contents: ContentsId) {
    let hub = &shared(context).hub;
    if !allowed(context, contents) || hub.is_devtools_open(contents) {
        return;
    }
    hub.set_devtools_open(contents, true);
    hub.to_renderer(contents, ToRenderer::InspectTree { at: None });
    said_later(context, contents, "devtools-opened");
}

/// Closes the DevTools of page `contents`, if they are open.
fn close(context: &mut Context, contents: ContentsId) {
    let hub = &shared(context).hub;
    if hub.is_devtools_open(contents) {
        hub.set_devtools_open(contents, false);
        said_later(context, contents, "devtools-closed");
    }
}

/// `inspectElement(x, y)`: the page writes the line of the element under the point, unless it
/// may have no DevTools. The DevTools stay as they are.
fn inspect_element(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let contents = record_of(this, context)?.page.contents;
    let (x, y) = {
        let args = Args::new(args);
        (args.f64(0, "x", context)?, args.f64(1, "y", context)?)
    };
    if !allowed(context, contents) {
        return Ok(JsValue::undefined());
    }
    let at = Some((x as f32, y as f32));
    shared(context)
        .hub
        .to_renderer(contents, ToRenderer::InspectTree { at });
    Ok(JsValue::undefined())
}

/// Says `event` on the page's `webContents` in a turn of its own, as Electron does once the
/// DevTools have come up, and not inside the call that asked.
fn said_later(context: &mut Context, contents: ContentsId, event: &'static str) {
    shared(context)
        .hub
        .to_main(move |guest| guest.devtools_event(contents, event));
}

/// `event` of page `contents`, said by the main process's turn that [`said_later`] queued.
pub(crate) fn said(context: &mut Context, contents: ContentsId, event: &str) -> JsResult<()> {
    if shared(context).showing(contents).is_none() {
        return Ok(());
    }
    let object = contents_ipc::contents_object(context, contents)?;
    define::emit(context, &object, event, &[])
}

/// Gives `prototype` the DevTools members.
pub(crate) fn install(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    native::methods(context, prototype, METHODS)?;
    native::accessors(
        context,
        prototype,
        &[(
            "devToolsWebContents",
            |this, _, cx| {
                record_of(this, cx)?;
                Ok(JsValue::null())
            },
            None,
        )],
    )
}
