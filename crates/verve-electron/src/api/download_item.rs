//! `DownloadItem`, what `will-download` is given, and the calls that start a download
//! (`session.downloadURL`, `webContents.downloadURL`, a page's `<a download>` and attachments).
//! The transfer itself is [`super::download`].

use std::path::PathBuf;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};

use super::define::{self, Args, Fields, Kind};
use super::download::{self, Start, State, with_item};
use super::session::session_of;
use crate::ids::ContentsId;

/// The class of a download's object.
const ITEM: Kind = Kind("DownloadItem");

/// The object of download `id`, made the first time.
pub(crate) fn item_object(context: &mut Context, id: u32) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(found) = state.net.downloads.objects.borrow().get(&id) {
        return Ok(found.clone());
    }
    let class = item_class(context)?;
    let made = class.construct(&[id.into()], None, context)?;
    state
        .net
        .downloads
        .objects
        .borrow_mut()
        .insert(id, made.clone());
    Ok(made)
}

fn item_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.downloads.class.borrow().clone() {
        return Ok(class);
    }
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "DownloadItem",
            init: |this, args, cx| {
                let id = arg(args, 0).as_number().unwrap_or(0.) as u32;
                define::bind(this, ITEM, id, cx);
                verve::events::emitter_init(this, &[], cx)
            },
            parent: Some(&emitter),
            methods: &[
                ("getURL", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(&i.url))
                }),
                ("getFilename", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(&i.filename))
                }),
                ("getMimeType", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(&i.mime))
                }),
                ("getState", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(i.state.name()))
                }),
                ("getSavePath", 0, |this, _, cx| {
                    field(this, cx, save_path_text)
                }),
                ("getContentDisposition", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(&i.content_disposition))
                }),
                ("getLastModifiedTime", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(&i.last_modified))
                }),
                ("getETag", 0, |this, _, cx| {
                    field(this, cx, |i| js_text(&i.etag))
                }),
                ("getTotalBytes", 0, |this, _, cx| {
                    field(this, cx, |i| (i.total as f64).into())
                }),
                ("getReceivedBytes", 0, |this, _, cx| {
                    field(this, cx, |i| (i.received as f64).into())
                }),
                ("getStartTime", 0, |this, _, cx| {
                    field(this, cx, |i| i.started_at.into())
                }),
                ("getPercentComplete", 0, |this, _, cx| {
                    field(this, cx, percent_complete)
                }),
                ("isPaused", 0, |this, _, cx| {
                    field(this, cx, |i| i.paused.into())
                }),
                ("hasUserGesture", 0, |this, _, cx| {
                    field(this, cx, |_| false.into())
                }),
                ("canResume", 0, |this, _, cx| field(this, cx, can_resume)),
                ("getURLChain", 0, get_url_chain),
                ("setSavePath", 1, set_save_path),
                ("setSaveDialogOptions", 1, set_save_dialog_options),
                ("getSaveDialogOptions", 0, get_save_dialog_options),
                ("pause", 0, |this, _, cx| act(this, cx, download::pause)),
                ("resume", 0, |this, _, cx| act(this, cx, download::resume)),
                ("cancel", 0, |this, _, cx| act(this, cx, download::cancel)),
            ],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    *state.net.downloads.class.borrow_mut() = Some(class.clone());
    Ok(class)
}

/// The download `this` stands for.
fn id_of(this: &JsValue, context: &mut Context) -> JsResult<u32> {
    let state = super::shared(context);
    let (id, ()) = define::live(this, ITEM, context, |id| {
        state
            .net
            .downloads
            .items
            .borrow()
            .contains_key(&id)
            .then_some(())
    })?;
    Ok(id)
}

/// One value read off the item `this` stands for; a download that is gone reads as `undefined`.
fn field(
    this: &JsValue,
    context: &mut Context,
    read: impl FnOnce(&download::Item) -> JsValue,
) -> JsResult<JsValue> {
    let id = id_of(this, context)?;
    Ok(with_item(context, id, |item| read(item)).unwrap_or_default())
}

fn save_path_text(item: &download::Item) -> JsValue {
    let path = item.save_path.as_ref();
    js_text(
        &path
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
}

fn percent_complete(item: &download::Item) -> JsValue {
    let percent = (item.received * 100).checked_div(item.total).unwrap_or(0);
    (percent as f64).into()
}

fn can_resume(item: &download::Item) -> JsValue {
    (item.started && (item.paused || item.state == State::Interrupted)).into()
}

/// `pause()`, `resume()` and `cancel()`.
fn act(this: &JsValue, context: &mut Context, action: fn(&mut Context, u32)) -> JsResult<JsValue> {
    let id = id_of(this, context)?;
    action(context, id);
    Ok(JsValue::undefined())
}

fn get_url_chain(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = id_of(this, context)?;
    let url = with_item(context, id, |item| item.url.clone()).unwrap_or_default();
    Ok(native::array(context, vec![js_text(&url)]))
}

/// `setSavePath(path)`: where the file goes, instead of asking.
fn set_save_path(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = id_of(this, context)?;
    let path = Args::new(args).string(0, "path", context)?;
    // Electron's own check of the path is not observed: "Path must be absolute" is Verve's.
    if !std::path::Path::new(&path).is_absolute() {
        return Err(define::error("Path must be absolute"));
    }
    // Once saving has begun the file has a name, and a new one would break the rename.
    with_item(context, id, |item| {
        if !item.saving {
            item.save_path = Some(PathBuf::from(path));
        }
    });
    Ok(JsValue::undefined())
}

fn set_save_dialog_options(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = id_of(this, context)?;
    Args::new(args).require(1)?;
    let options = arg(args, 0);
    with_item(context, id, |item| item.dialog_options = Some(options));
    Ok(JsValue::undefined())
}

fn get_save_dialog_options(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = id_of(this, context)?;
    let options = with_item(context, id, |item| item.dialog_options.clone()).flatten();
    Ok(match options {
        Some(options) => options,
        None => native::object(context).into(),
    })
}

/// Tells the item's `updated` listeners it is progressing (or paused).
pub(crate) fn emit_updated(context: &mut Context, id: u32) {
    let Ok(object) = item_object(context, id) else {
        return;
    };
    let state = with_item(context, id, |item| item.state.name()).unwrap_or("progressing");
    define::emit_reporting(context, &object, "updated", &[js_text(state)]);
}

/// Tells the item's `done` listeners how it ended.
pub(crate) fn emit_done(context: &mut Context, id: u32) {
    let Ok(object) = item_object(context, id) else {
        return;
    };
    let state = with_item(context, id, |item| item.state.name()).unwrap_or("interrupted");
    define::emit_reporting(context, &object, "done", &[js_text(state)]);
}

// ─── What starts one ────────────────────────────────────────────────────────────────────────

/// `downloadURL(url[, { headers }])` of a session or a `webContents`: the download starts.
pub(crate) fn download_url(
    context: &mut Context,
    session: &std::sync::Arc<crate::hub::Session>,
    contents: Option<ContentsId>,
    args: &[JsValue],
) -> JsResult<JsValue> {
    let url = Args::new(args).string(0, "url", context)?;
    let options = arg(args, 1).as_object();
    let fields = Fields::new(options.as_ref());
    let headers = match fields.object("headers", context)? {
        Some(object) => {
            let mut list = Vec::new();
            for (name, value) in define::entries_of(&object, context)? {
                list.push((name, native::text_of(&value, context)?));
            }
            list
        }
        None => Vec::new(),
    };
    download::start(
        context,
        Start {
            session: std::sync::Arc::clone(session),
            contents,
            url,
            headers,
            suggested: None,
        },
    );
    Ok(JsValue::undefined())
}

/// `session.downloadURL`.
pub(crate) fn session_download_url(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    download_url(context, &session, None, args)
}

/// A page asked for `url` to be saved: a link's `download`, or an attachment.
pub(crate) fn from_page(
    context: &mut Context,
    contents: ContentsId,
    url: String,
    suggested: Option<String>,
) {
    let Some(session) = super::shared(context).hub.session_of(contents) else {
        return;
    };
    download::start(
        context,
        Start {
            session,
            contents: Some(contents),
            url,
            headers: Vec::new(),
            suggested,
        },
    );
}

/// `webContents.downloadURL`, which the table of stubs leaves out.
pub(crate) fn install_contents(context: &mut Context, prototype: &JsObject) -> JsResult<()> {
    native::methods(
        context,
        prototype,
        &[("downloadURL", 1, |this, args, cx| {
            let record = super::web_contents::record_of(this, cx)?;
            let contents = record.page.contents;
            let Some(session) = super::shared(cx).hub.session_of(contents) else {
                return Err(define::destroyed());
            };
            download_url(cx, &session, Some(contents), args)
        })],
    )
}
