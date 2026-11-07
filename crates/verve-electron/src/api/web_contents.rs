//! `WebContents`, Electron 39's: the page of a window, as the main process's script holds it. The
//! class extends `EventEmitter` and cannot be constructed by a script; a window makes its own.
//!
//! The members are in tables, over the state [`PageState`](super::page_state::PageState) keeps:
//! this file has loading, the address and title, zoom, the user agent, the process and the
//! lifecycle; [`contents_ipc`] has the IPC members, [`contents_page`] the ones that reach into the
//! page, [`contents_history`] `navigationHistory`, and [`contents_stubs`] the ones Verve records
//! or does not do. `webContents` the module (`getAllWebContents` and the others) is
//! [`make_module`].

use std::rc::Rc;

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::events::emitter_init;
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};

use super::contents_ipc::{self, WEB_CONTENTS};
use super::define::{self, Args, Fields};
use super::page_state::Intent;
use super::web_frame::LEVEL_STEP;
use super::windows::WindowRecord;
use super::{
    app, contents_history, contents_page, contents_stubs, devtools, navigation, shared,
    web_frame_main,
};
use crate::ids::ContentsId;
use crate::ipc;
use crate::msg::{LoadOptions, ToRenderer, ToRendererInput};

/// The `WebContents` class a realm made, for the pages it makes after it.
struct ClassData(JsObject);

/// A new `WebContents`, not yet bound to a page.
pub(crate) fn construct(context: &mut Context) -> JsResult<JsObject> {
    let class = match context.get_data::<Rc<ClassData>>() {
        Some(data) => data.0.clone(),
        None => {
            let made = make_class(context)?;
            context.insert_data(Rc::new(ClassData(made.clone())));
            made
        }
    };
    class.construct(&[], None, context)
}

fn make_class(context: &mut Context) -> JsResult<JsObject> {
    let emitter = define::emitter(context)?;
    let (class, prototype) = native::class(
        context,
        ClassSpec {
            name: "WebContents",
            init: emitter_init,
            parent: Some(&emitter),
            accessors: ACCESSORS,
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    native::methods(context, &prototype, METHODS)?;
    native::methods(context, &prototype, contents_ipc::METHODS)?;
    native::accessors(context, &prototype, contents_ipc::ACCESSORS)?;
    contents_page::install(context, &prototype)?;
    contents_history::install_legacy(context, &prototype)?;
    contents_stubs::install(context, &prototype)?;
    devtools::install(context, &prototype)?;
    super::download_item::install_contents(context, &prototype)?;
    Ok(class)
}

/// The window of the page `this` stands for, while a script can use it: `Object has been
/// destroyed` once it is gone.
pub(crate) fn record_of(this: &JsValue, context: &mut Context) -> JsResult<Rc<WindowRecord>> {
    let state = shared(context);
    define::live(this, WEB_CONTENTS, context, |id| state.showing(id)).map(|(_, record)| record)
}

/// `webContents.id`, which a page keeps answering after it is destroyed.
fn id(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    define::id_of(this, WEB_CONTENTS, context)
        .map_or_else(|| Err(define::destroyed()), |id| Ok(id.into()))
}

/// Answers `read` of the page `this` stands for.
fn read(
    this: &JsValue,
    context: &mut Context,
    read: impl FnOnce(&WindowRecord) -> JsValue,
) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    Ok(read(&record))
}

// ---------- loading ----------

/// `loadURL(url[, options])`, `BrowserWindow.loadURL` too: the promise settles when the page has
/// loaded or failed.
pub(crate) fn load_url(
    record: &Rc<WindowRecord>,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = Args::new(&[arg(args, 0)]).string(0, "url", context)?;
    let given = args.get(1).and_then(JsValue::as_object);
    let fields = Fields::new(given.as_ref());
    let options = LoadOptions {
        http_referrer: fields.string("httpReferrer", context)?,
        user_agent: fields.string("userAgent", context)?,
        extra_headers: fields.string("extraHeaders", context)?,
    };
    let promise = navigation::start(context, record, (url, options), Intent::Push, true)?;
    Ok(promise.unwrap_or_default())
}

/// `loadFile(path[, { query, search, hash }])`: `loadURL` of the file's `file:` address.
pub(crate) fn load_file(
    record: &Rc<WindowRecord>,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let path = Args::new(&[arg(args, 0)]).string(0, "filePath", context)?;
    let given = args.get(1).and_then(JsValue::as_object);
    let fields = Fields::new(given.as_ref());
    let search = fields.string("search", context)?;
    let hash = fields.string("hash", context)?;
    let query = match fields.object("query", context)? {
        Some(object) => Some(verve::query_string(&object.into(), context)?),
        None => None,
    };
    let base = shared(context).hub.options().app_path.clone();
    let path = base.join(path);
    let mut url = url::Url::from_file_path(&path).map_err(|()| {
        native::type_error(format!(
            "loadFile: {} is not an absolute path",
            path.display()
        ))
    })?;
    match (search, query) {
        (Some(search), _) => url.set_query(Some(search.trim_start_matches('?'))),
        (None, Some(query)) => url.set_query(Some(&query)),
        _ => {}
    }
    url.set_fragment(hash.as_deref().map(|hash| hash.trim_start_matches('#')));
    let promise = navigation::start(
        context,
        record,
        (url.to_string(), LoadOptions::default()),
        Intent::Push,
        true,
    )?;
    Ok(promise.unwrap_or_default())
}

/// `webContents.close([{ waitForBeforeUnload }])`: the page may refuse with `waitForBeforeUnload`;
/// without it the window goes at once.
fn close(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let given = args.first().and_then(JsValue::as_object);
    if Fields::new(given.as_ref()).bool("waitForBeforeUnload", false, context)? {
        super::window_lifecycle::ask_page(context, &record);
    } else {
        super::window_lifecycle::finish_close(context, &record);
    }
    Ok(JsValue::undefined())
}

/// `forcefullyCrashRenderer()`: the page's process ends, and a load makes it again.
fn force_crash(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    shared(context).hub.kill_renderer(record.page.contents);
    Ok(JsValue::undefined())
}

fn reload(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    navigation::reload(context, &record)?;
    Ok(JsValue::undefined())
}

fn focus(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    shared(context)
        .hub
        .input(record.page.contents, ToRendererInput::Focus(true));
    Ok(JsValue::undefined())
}

// ---------- user agent and zoom ----------

fn user_agent(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let own = record.page_state.user_agent.borrow().clone();
    Ok(js_text(&own.unwrap_or_else(|| app::user_agent(context))))
}

fn set_user_agent(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let agent = Args::new(args).string(0, "userAgent", context)?;
    *record.page_state.user_agent.borrow_mut() = Some(agent);
    Ok(JsValue::undefined())
}

/// The zoom factor of the page: what was set, else what `webPreferences.zoomFactor` said.
fn zoom_factor_of(context: &Context, record: &WindowRecord) -> f64 {
    let preferred = || shared(context).hub.preferences_of(record.page.contents);
    record
        .page_state
        .zoom_factor
        .get()
        .or_else(|| preferred().map(|p| p.zoom_factor))
        .unwrap_or(1.)
}

fn zoom_factor(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    Ok(zoom_factor_of(context, &record).into())
}

fn zoom_level(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    Ok((zoom_factor_of(context, &record).ln() / LEVEL_STEP.ln()).into())
}

fn set_zoom(record: &WindowRecord, factor: f64) {
    record.page_state.zoom_factor.set(Some(factor));
    record.page.to_renderer(ToRenderer::SetZoomFactor(factor));
}

fn set_zoom_factor(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let factor = Args::new(args).f64(0, "factor", context)?;
    if factor > 0. {
        set_zoom(&record, factor);
    }
    Ok(JsValue::undefined())
}

fn set_zoom_level(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let level = Args::new(args).f64(0, "level", context)?;
    set_zoom(&record, LEVEL_STEP.powf(level));
    Ok(JsValue::undefined())
}

/// The page set its own zoom factor (`webFrame.setZoomFactor`): `getZoomFactor()` says so.
pub(crate) fn zoom_changed(context: &mut Context, contents: ContentsId, factor: f64) {
    if let Some(record) = shared(context).showing(contents) {
        record.page_state.zoom_factor.set(Some(factor));
    }
}

// ---------- the tables ----------

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("loadURL", 1, |this, args, cx| {
        let record = record_of(this, cx)?;
        load_url(&record, args, cx)
    }),
    ("loadFile", 1, |this, args, cx| {
        let record = record_of(this, cx)?;
        load_file(&record, args, cx)
    }),
    ("getURL", 0, |this, _, cx| {
        read(this, cx, |r| js_text(&r.page_state.url.borrow()))
    }),
    ("getTitle", 0, |this, _, cx| {
        read(this, cx, |r| js_text(&r.page_state.title.borrow()))
    }),
    ("isLoading", 0, |this, _, cx| {
        read(this, cx, |r| r.page_state.loading.get().into())
    }),
    ("isLoadingMainFrame", 0, |this, _, cx| {
        read(this, cx, |r| r.page_state.loading.get().into())
    }),
    ("isWaitingForResponse", 0, |this, _, cx| {
        read(this, cx, |r| r.page_state.loading.get().into())
    }),
    ("isCrashed", 0, |this, _, cx| {
        read(this, cx, |r| r.page_state.crashed.get().into())
    }),
    ("isFocused", 0, |this, _, cx| {
        read(this, cx, |r| r.state.borrow().focused.into())
    }),
    ("reload", 0, reload),
    ("reloadIgnoringCache", 0, reload),
    ("stop", 0, |this, _, cx| {
        read(this, cx, |_| JsValue::undefined())
    }),
    ("forcefullyCrashRenderer", 0, force_crash),
    ("close", 0, close),
    ("focus", 0, focus),
    ("getUserAgent", 0, user_agent),
    ("setUserAgent", 1, set_user_agent),
    ("getZoomFactor", 0, zoom_factor),
    ("setZoomFactor", 1, set_zoom_factor),
    ("getZoomLevel", 0, zoom_level),
    ("setZoomLevel", 1, set_zoom_level),
    ("getProcessId", 0, |this, _, cx| {
        read(this, cx, |r| ipc::process_id(r.page.contents).into())
    }),
    ("getOSProcessId", 0, |this, _, cx| {
        read(this, cx, |_| std::process::id().into())
    }),
    ("getType", 0, |this, _, cx| {
        read(this, cx, |_| js_text("window"))
    }),
];

const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    ("id", id, None),
    (
        "navigationHistory",
        contents_history::navigation_history,
        None,
    ),
    ("userAgent", user_agent, Some(set_user_agent)),
    ("zoomFactor", zoom_factor, Some(set_zoom_factor)),
    ("zoomLevel", zoom_level, Some(set_zoom_level)),
    (
        "hostWebContents",
        |this, _, cx| read(this, cx, |_| JsValue::null()),
        None,
    ),
    ("opener", opener, None),
    (
        "focusedFrame",
        |this, _, cx| {
            let record = record_of(this, cx)?;
            let page = contents_ipc::page(cx, record.page.contents)?;
            Ok(page.map_or(JsValue::null(), |page| page.frame.into()))
        },
        None,
    ),
];

/// `opener`: the main frame of the page that opened this one with `window.open`.
fn opener(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let record = record_of(this, context)?;
    let Some(opener) = record.page_state.opener.get() else {
        return Ok(JsValue::null());
    };
    let page = contents_ipc::page(context, opener)?;
    Ok(page.map_or(JsValue::null(), |page| page.frame.into()))
}

// ---------- the module ----------

/// `require('electron').webContents`: the statics of the class.
pub(crate) fn make_module(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("getAllWebContents", 0, |_, _, cx| {
                let windows = shared(cx).windows.newest_first();
                let mut all = Vec::new();
                for record in windows {
                    all.push(contents_ipc::contents_object(cx, record.page.contents)?.into());
                }
                Ok(native::array(cx, all))
            }),
            ("getFocusedWebContents", 0, |_, _, cx| {
                let windows = shared(cx).windows.newest_first();
                match windows.iter().find(|w| w.state.borrow().focused) {
                    Some(record) => {
                        Ok(contents_ipc::contents_object(cx, record.page.contents)?.into())
                    }
                    None => Ok(JsValue::null()),
                }
            }),
            ("fromId", 1, |_, args, cx| {
                let id = Args::new(args).f64(0, "id", cx)?;
                found(cx, shared(cx).showing(id as ContentsId))
            }),
            ("fromFrame", 1, |_, args, cx| {
                let id = web_frame_main::contents_of(&arg(args, 0), cx);
                found(cx, id.and_then(|id| shared(cx).showing(id)))
            }),
            (
                "fromDevToolsTargetId",
                1,
                |_, _, _| Ok(JsValue::undefined()),
            ),
        ],
    )?;
    Ok(module.into())
}

/// The `webContents` of `record`'s page, or `undefined`: what the statics answer for a page that is
/// not there.
fn found(context: &mut Context, record: Option<Rc<WindowRecord>>) -> JsResult<JsValue> {
    match record {
        Some(record) => Ok(contents_ipc::contents_object(context, record.page.contents)?.into()),
        None => Ok(JsValue::undefined()),
    }
}
