//! The members of `webContents` Verve records, answers with a constant or does not do: audio,
//! offscreen painting, DevTools (P43), printing, capture, the WebRTC policies and the rest of the
//! table Electron documents.
//!
//! Each still checks the page is there first, so a destroyed `webContents` throws as it does in
//! Electron; the ones Verve does not do say so with `ERR_NOT_SUPPORTED`, as the conventions
//! ask (`README.md`, "v1 明确不做").

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue};
use verve::native::{self, arg, js_text};

use super::define::{self, Args};
use super::shared;
use super::web_contents::record_of;
use crate::ids::ContentsId;

/// What a member answers.
enum Answer {
    /// `undefined`: it is recorded, or has nothing to do without a window system.
    Nothing,
    /// `false`.
    False,
    /// A promise resolved with `undefined`.
    Resolved,
    /// A promise rejected as not supported.
    Rejected,
    /// Throws as not supported.
    Throws,
}

use Answer::{False, Nothing, Rejected, Resolved, Throws};

const TABLE: &[(&str, usize, Answer)] = &[
    ("setIgnoreMenuShortcuts", 1, Nothing),
    ("enableDeviceEmulation", 1, Nothing),
    ("disableDeviceEmulation", 0, Nothing),
    ("startDrag", 1, Nothing),
    ("startPainting", 0, Nothing),
    ("stopPainting", 0, Nothing),
    ("invalidate", 0, Nothing),
    ("setFrameRate", 1, Nothing),
    ("setBackgroundThrottling", 1, Nothing),
    ("setImageAnimationPolicy", 1, Nothing),
    ("setWebRTCIPHandlingPolicy", 1, Nothing),
    ("setWebRTCUDPPortRange", 1, Nothing),
    ("stopFindInPage", 1, Nothing),
    ("openDevTools", 0, Nothing),
    ("closeDevTools", 0, Nothing),
    ("toggleDevTools", 0, Nothing),
    ("inspectElement", 2, Nothing),
    ("inspectSharedWorker", 0, Nothing),
    ("inspectServiceWorker", 0, Nothing),
    ("setDevToolsWebContents", 1, Nothing),
    ("addWorkSpace", 1, Nothing),
    ("removeWorkSpace", 1, Nothing),
    ("isBeingCaptured", 0, False),
    ("isOffscreen", 0, False),
    ("isPainting", 0, False),
    ("isDevToolsOpened", 0, False),
    ("isDevToolsFocused", 0, False),
    ("isCurrentlyAudible", 0, False),
    ("setVisualZoomLevelLimits", 2, Resolved),
    ("capturePage", 0, Rejected),
    ("printToPDF", 0, Rejected),
    ("savePage", 2, Rejected),
    ("takeHeapSnapshot", 1, Rejected),
    ("centerSelection", 0, Throws),
    ("adjustSelection", 1, Throws),
    ("replace", 1, Throws),
    ("replaceMisspelling", 1, Throws),
    ("beginFrameSubscription", 1, Throws),
    ("endFrameSubscription", 0, Throws),
];

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("setAudioMuted", 1, |this, args, cx| {
        let record = record_of(this, cx)?;
        record.page_state.audio_muted.set(arg(args, 0).to_boolean());
        Ok(JsValue::undefined())
    }),
    ("isAudioMuted", 0, |this, _, cx| {
        Ok(record_of(this, cx)?.page_state.audio_muted.get().into())
    }),
    ("getFrameRate", 0, |this, _, cx| {
        record_of(this, cx)?;
        Ok(60.into())
    }),
    ("getBackgroundThrottling", 0, |this, _, cx| {
        let record = record_of(this, cx)?;
        let prefs = shared(cx).hub.preferences_of(record.page.contents);
        Ok(prefs.is_none_or(|prefs| prefs.background_throttling).into())
    }),
    ("getWebRTCIPHandlingPolicy", 0, |this, _, cx| {
        record_of(this, cx)?;
        Ok(js_text("default"))
    }),
    ("getWebRTCUDPPortRange", 0, |this, _, cx| {
        record_of(this, cx)?;
        Ok(define::record(cx, &[("min", 0.into()), ("max", 0.into())])?.into())
    }),
    ("getPrintersAsync", 0, |this, _, cx| {
        record_of(this, cx)?;
        let none = native::array(cx, Vec::new());
        native::settled(Ok(none), cx)
    }),
    ("print", 2, |this, args, cx| {
        record_of(this, cx)?;
        if let Some(callback) = args.iter().find_map(JsValue::as_callable) {
            refuse_print(cx, callback.clone())?;
        }
        Ok(JsValue::undefined())
    }),
    ("findInPage", 2, find_in_page),
];

/// `findInPage(text)`: no search in v1. The request gets a number, and `found-in-page` says it
/// found nothing.
fn find_in_page(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    Args::new(args).string(0, "text", context)?;
    let request = shared(context).windows.next_call();
    let contents = record.page.contents;
    shared(context)
        .hub
        .to_main(move |guest| guest.found_in_page(contents, request));
    Ok(request.into())
}

/// `found-in-page` for `request`, said after `findInPage` returned its number: nothing was found.
pub(crate) fn found(context: &mut Context, contents: ContentsId, request: u64) -> JsResult<()> {
    if shared(context).showing(contents).is_none() {
        return Ok(());
    }
    let result = define::record(
        context,
        &[
            ("requestId", request.into()),
            ("activeMatchOrdinal", 0.into()),
            ("matches", 0.into()),
            ("finalUpdate", true.into()),
        ],
    )?;
    let object = super::contents_ipc::contents_object(context, contents)?;
    define::emit_reporting(context, &object, "found-in-page", &[result.into()]);
    Ok(())
}

/// Tells `callback` printing is not done, after the call returned.
fn refuse_print(context: &mut Context, callback: JsObject) -> JsResult<()> {
    let refuse = native::closure(
        context,
        "",
        0,
        |_, _, callback: &JsObject, cx| {
            let failure = [false.into(), js_text("Printing is not supported by Verve")];
            verve::call(
                cx,
                &callback.clone().into(),
                &JsValue::undefined(),
                &failure,
            )
        },
        callback,
    );
    let later = JsFunction::from_object(refuse).expect("a closure is a function");
    JsPromise::resolve(JsValue::undefined(), context)?.then(Some(later), None, context)?;
    Ok(())
}

/// `session`: the session of the page's `webPreferences.partition`.
fn session(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let contents = record.page.contents;
    let hub = std::sync::Arc::clone(&super::shared(context).hub);
    let session = hub
        .session_of(contents)
        .ok_or_else(super::define::destroyed)?;
    Ok(super::session::object_of(context, &session)?.into())
}

/// Gives `prototype` the members of this file.
pub(crate) fn install(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    for (index, (name, length, _)) in TABLE.iter().enumerate() {
        let method = native::closure(context, name, *length, answer, index);
        native::hidden(prototype, JsString::from(*name), method, context)?;
    }
    native::methods(context, prototype, METHODS)?;
    native::accessors(
        context,
        prototype,
        &[
            (
                "audioMuted",
                |this, _, cx| Ok(record_of(this, cx)?.page_state.audio_muted.get().into()),
                Some(|this, args, cx| {
                    record_of(this, cx)?
                        .page_state
                        .audio_muted
                        .set(arg(args, 0).to_boolean());
                    Ok(JsValue::undefined())
                }),
            ),
            ("session", session, None),
            (
                "devToolsWebContents",
                |this, _, cx| {
                    record_of(this, cx)?;
                    Ok(JsValue::null())
                },
                None,
            ),
        ],
    )
}

fn answer(
    this: &JsValue,
    _: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    record_of(this, context)?;
    let (name, _, answer) = &TABLE[*index];
    let api = format!("webContents.{name}");
    match answer {
        Nothing => Ok(JsValue::undefined()),
        False => Ok(false.into()),
        Resolved => native::settled(Ok(JsValue::undefined()), context),
        Rejected => define::rejected(context, &api),
        Throws => define::not_supported(context, &api),
    }
}
