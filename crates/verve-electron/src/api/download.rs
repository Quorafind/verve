//! Downloads: what `session.downloadURL`, `webContents.downloadURL`, a link with a `download`
//! attribute and a response with `Content-Disposition: attachment` start, and the transfer that
//! writes the file (`download_manager_delegate.cc` and `download_item.cc` of Electron 39.8.10;
//! what `will-download` is given and when `updated` and `done` are heard are observed, see P16's
//! contract). The script's side of an item is [`super::download_item`].
//!
//! A download is asked of the session's `net` like any request of the main process, so it carries
//! the session's cookies and passes its `webRequest` listeners. Its head decides the file name,
//! and `will-download` is emitted once there is one; the body is then written to
//! `<path>.crdownload`, which becomes `<path>` when it is whole. A pause stops the request and
//! keeps the partial file; a resume asks for the rest with `Range`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg, get, invoke, js_text};

use super::define;
use super::download_item::{emit_done, emit_updated, item_object};
use crate::hub::Session;
use crate::ids::ContentsId;

/// How many finished downloads keep their item and object, for an app that asks one of them
/// after `done`. Electron's object answers for as long as it is held; Verve frees the oldest
/// beyond this.
const KEEP_FINISHED: usize = 256;

/// The least time between two `updated` events of one download.
const UPDATE_EVERY: Duration = Duration::from_millis(100);

/// Where a download is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Progressing,
    Completed,
    Cancelled,
    Interrupted,
}

impl State {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Progressing => "progressing",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}

/// One download.
pub(crate) struct Item {
    pub session: Arc<Session>,
    pub contents: Option<ContentsId>,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The name the page gave (`<a download="…">`), used when the response names none.
    pub suggested: Option<String>,
    pub filename: String,
    pub mime: String,
    pub total: u64,
    pub received: u64,
    pub state: State,
    pub paused: bool,
    /// The head has come and `will-download` was heard.
    pub started: bool,
    /// The partial file has been opened: the file's name is fixed from here on.
    pub saving: bool,
    pub save_path: Option<PathBuf>,
    pub dialog_options: Option<JsValue>,
    pub content_disposition: String,
    pub etag: String,
    pub last_modified: String,
    pub started_at: f64,
    file: Option<File>,
    /// The request being read: its controller (to stop it) and its body's reader.
    abort: Option<JsObject>,
    reader: Option<JsObject>,
    /// Which transfer of the item the callbacks in flight belong to; those of an earlier one
    /// (before a pause or a cancel) are ignored.
    run: u64,
    last_update: Option<Instant>,
}

/// What the main process keeps of downloads.
#[derive(Default)]
pub(crate) struct Objects {
    pub(crate) class: RefCell<Option<JsObject>>,
    pub(crate) items: RefCell<HashMap<u32, Item>>,
    pub(crate) objects: RefCell<HashMap<u32, JsObject>>,
    next: Cell<u32>,
    runs: Cell<u32>,
    /// The downloads that are over, oldest first: the last [`KEEP_FINISHED`] stay as objects an
    /// app may still ask, the rest are freed.
    finished: RefCell<VecDeque<u32>>,
}

/// What starts a download.
pub(crate) struct Start {
    pub session: Arc<Session>,
    pub contents: Option<ContentsId>,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub suggested: Option<String>,
}

/// Runs `read` on the item `id`, if it is still kept.
pub(crate) fn with_item<R>(
    context: &Context,
    id: u32,
    read: impl FnOnce(&mut Item) -> R,
) -> Option<R> {
    let state = super::shared(context);
    let mut items = state.net.downloads.items.borrow_mut();
    items.get_mut(&id).map(read)
}

/// Starts a download: the request goes out, and `will-download` is heard when its head is here.
pub(crate) fn start(context: &mut Context, request: Start) {
    let state = super::shared(context);
    let objects = &state.net.downloads;
    let id = objects.next.get() + 1;
    objects.next.set(id);
    let item = Item {
        session: request.session,
        contents: request.contents,
        url: request.url,
        headers: request.headers,
        suggested: request.suggested,
        filename: String::new(),
        mime: String::new(),
        total: 0,
        received: 0,
        state: State::Progressing,
        paused: false,
        started: false,
        saving: false,
        save_path: None,
        dialog_options: None,
        content_disposition: String::new(),
        etag: String::new(),
        last_modified: String::new(),
        started_at: crate::hub::cookies::now(),
        file: None,
        abort: None,
        reader: None,
        run: 0,
        last_update: None,
    };
    objects.items.borrow_mut().insert(id, item);
    transfer(context, id);
}

/// The number of the transfer the item is now on: its id above the count of its transfers.
fn next_run(context: &Context, id: u32) -> u64 {
    let objects = &super::shared(context).net.downloads;
    let count = objects.runs.get() + 1;
    objects.runs.set(count);
    (u64::from(id) << 32) | u64::from(count)
}

/// The item a callback of transfer `run` belongs to, if that transfer is still the item's.
fn current(context: &Context, run: u64) -> Option<u32> {
    let id = u32::try_from(run >> 32).ok()?;
    with_item(context, id, |item| {
        item.run == run && item.state == State::Progressing
    })
    .unwrap_or(false)
    .then_some(id)
}

/// Asks for the item's body from where it has got to, and goes on when the head comes.
pub(crate) fn transfer(context: &mut Context, id: u32) {
    let run = next_run(context, id);
    if let Err(error) = begin(context, id, run) {
        verve::report(context, &error);
        interrupt(context, id);
    }
}

fn begin(context: &mut Context, id: u32, run: u64) -> JsResult<()> {
    let Some((session, url, mut headers, offset, etag)) = with_item(context, id, |item| {
        item.run = run;
        let offset = item.received;
        let etag = item.etag.clone();
        (
            Arc::clone(&item.session),
            item.url.clone(),
            item.headers.clone(),
            offset,
            etag,
        )
    }) else {
        return Ok(());
    };
    if offset > 0 {
        headers.push(("Range".to_owned(), format!("bytes={offset}-")));
        if !etag.is_empty() {
            headers.push(("If-Range".to_owned(), etag));
        }
    }
    let controller = native::construct_global(context, "AbortController", &[])?;
    let signal = get(&controller.clone().into(), "signal", context)?;
    let list = headers
        .iter()
        .map(|(name, value)| (name.as_str(), js_text(value)))
        .collect::<Vec<_>>();
    let headers = define::record(context, &list)?;
    let init = define::record(context, &[("headers", headers.into()), ("signal", signal)])?;
    with_item(context, id, |item| item.abort = Some(controller));
    let promise = super::net::fetch(context, &session, &js_text(&url), &init.into())?;
    native::then(context, &promise, on_head, on_failed, run)?;
    Ok(())
}

/// The request failed before it had a head: the download is interrupted.
fn on_failed(_: &JsValue, _: &[JsValue], run: &u64, context: &mut Context) -> JsResult<JsValue> {
    if let Some(id) = current(context, *run) {
        interrupt(context, id);
    }
    Ok(JsValue::undefined())
}

/// The head of the answer is here.
fn on_head(_: &JsValue, args: &[JsValue], run: &u64, context: &mut Context) -> JsResult<JsValue> {
    let Some(id) = current(context, *run) else {
        return Ok(JsValue::undefined());
    };
    let response = arg(args, 0);
    let status = get(&response, "status", context)?.as_number().unwrap_or(0.) as u16;
    let resumed = with_item(context, id, |item| item.started).unwrap_or(false);
    let fine = if resumed {
        status == 200 || status == 206
    } else {
        (200..300).contains(&status)
    };
    if !fine {
        interrupt(context, id);
        return Ok(JsValue::undefined());
    }
    let body = get(&response, "body", context)?;
    let reader = invoke(&body, "getReader", &[], context)?.as_object();
    with_item(context, id, |item| item.reader = reader);
    if resumed {
        let restart = status == 200;
        let reopened = reopen(context, id, restart);
        if define::report(context, reopened).is_none() {
            interrupt(context, id);
            return Ok(JsValue::undefined());
        }
        emit_updated(context, id);
        read_next(context, *run);
        return Ok(JsValue::undefined());
    }
    // What a listener of `will-download` throws is the app's to hear, not this promise's.
    let announced = learn(context, id, &response).and_then(|()| announce(context, id));
    if define::report(context, announced).is_none() {
        interrupt(context, id);
    }
    Ok(JsValue::undefined())
}

/// What the head says about the file: its name, type, size and the rest.
fn learn(context: &mut Context, id: u32, response: &JsValue) -> JsResult<()> {
    let header = |name: &str, cx: &mut Context| -> JsResult<String> {
        let found = invoke(&get(response, "headers", cx)?, "get", &[js_text(name)], cx)?;
        Ok(found
            .as_string()
            .map(|text| text.to_std_string_escaped())
            .unwrap_or_default())
    };
    let disposition = header("content-disposition", context)?;
    let mime = header("content-type", context)?;
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let length = header("content-length", context)?
        .parse::<u64>()
        .unwrap_or(0);
    let etag = header("etag", context)?;
    let modified = header("last-modified", context)?;
    with_item(context, id, |item| {
        let name = file_name(&disposition, item.suggested.as_deref(), &item.url, &mime);
        item.filename = name;
        item.mime = if mime.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            mime
        };
        item.total = length;
        item.content_disposition = disposition;
        item.etag = etag;
        item.last_modified = modified;
    });
    Ok(())
}

/// The name a file is saved under: the response's `filename`, else the page's `download`, else
/// the last part of the address, with the extension of its type when it has none and the type
/// has one (`nosave` of `text/plain` is `nosave.txt`; `octet-stream` adds nothing).
fn file_name(disposition: &str, suggested: Option<&str>, url: &str, mime: &str) -> String {
    let from_url = || {
        url::Url::parse(url).ok().and_then(|url| {
            let last = url.path_segments()?.next_back()?.to_owned();
            (!last.is_empty()).then_some(last)
        })
    };
    let name = disposition_name(disposition)
        .or_else(|| suggested.map(str::to_owned))
        .or_else(from_url)
        .unwrap_or_else(|| "download".to_owned());
    let name = clean(&name);
    if name.contains('.') {
        return name;
    }
    match super::mime::extension_of(mime) {
        Some(extension) => format!("{name}.{extension}"),
        None => name,
    }
}

/// The `filename` a `Content-Disposition` gives; `filename*` (RFC 5987) before `filename`.
fn disposition_name(value: &str) -> Option<String> {
    let find = |key: &str| {
        value
            .split(';')
            .skip(1)
            .filter_map(|part| part.split_once('='))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case(key))
            .map(|(_, text)| text.trim().trim_matches('"').to_owned())
    };
    if let Some(encoded) = find("filename*") {
        let text = encoded.rsplit('\'').next().unwrap_or("");
        return Some(String::from_utf8_lossy(&verve::encoding::percent_decode(text)).into_owned());
    }
    find("filename")
}

/// A file name with no folders and no characters a file system refuses.
fn clean(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        "download".to_owned()
    } else {
        cleaned
    }
}

/// `will-download` is emitted: a listener may give the file a path or refuse the download; with
/// neither the person is asked where to save it.
fn announce(context: &mut Context, id: u32) -> JsResult<()> {
    let Some((session, contents)) = with_item(context, id, |item| {
        item.started = true;
        (Arc::clone(&item.session), item.contents)
    }) else {
        return Ok(());
    };
    let item = item_object(context, id)?;
    let target = match contents {
        Some(contents) => super::contents_ipc::contents_object(context, contents)?.into(),
        None => JsValue::null(),
    };
    let sessions = super::session::object_of(context, &session)?;
    let cancel = define::event(context)?;
    let args = [item.into(), target];
    let prevented = define::emit_with_event(context, &sessions, "will-download", &cancel, &args)?;
    if prevented {
        forget(context, id);
        return Ok(());
    }
    match with_item(context, id, |item| item.save_path.clone()).flatten() {
        Some(_) => save(context, id),
        None => ask_where(context, id),
    }
}

/// The download is over and nothing of it is kept: it was refused.
fn forget(context: &mut Context, id: u32) {
    let stopped = with_item(context, id, |item| {
        item.state = State::Cancelled;
        item.run = 0;
        (item.abort.take(), item.reader.take())
    });
    if let Some((abort, reader)) = stopped {
        stop(context, abort, reader);
    }
    retire(context, id);
}

/// Stops a request and the reading of its body.
fn stop(context: &mut Context, abort: Option<JsObject>, reader: Option<JsObject>) {
    if let Some(abort) = abort {
        let aborted = invoke(&abort.into(), "abort", &[], context);
        define::report(context, aborted);
    }
    if let Some(reader) = reader {
        // The body was aborted with the request, so cancelling it only says nobody reads it.
        let cancelled = invoke(&reader.into(), "cancel", &[], context);
        if let Ok(promise) = cancelled {
            let quiet = native::function(context, "", 1, |_, _, _| Ok(JsValue::undefined()));
            let _ = invoke(&promise, "catch", &[quiet.into()], context);
        }
    }
}

/// Asks where to save the file with the app's save dialog (`setSaveDialogOptions` apply).
fn ask_where(context: &mut Context, id: u32) -> JsResult<()> {
    let Some((options, name, run)) = with_item(context, id, |item| {
        (item.dialog_options.clone(), item.filename.clone(), item.run)
    }) else {
        return Ok(());
    };
    let electron = verve::require(context, "electron")?;
    let options = match options.and_then(|options| options.as_object()) {
        Some(options) => options,
        None => native::object(context),
    };
    let wanted = get(&options.clone().into(), "defaultPath", context)?;
    if wanted.is_undefined() {
        let folder = invoke(
            &get(&electron, "app", context)?,
            "getPath",
            &[js_text("downloads")],
            context,
        )?;
        let folder = native::text_of(&folder, context)?;
        let path = PathBuf::from(folder).join(&name);
        let text = js_text(&path.to_string_lossy());
        options.set(boa_engine::js_string!("defaultPath"), text, false, context)?;
    }
    let dialog = get(&electron, "dialog", context)?;
    let promise = invoke(&dialog, "showSaveDialog", &[options.into()], context)?;
    native::then(context, &promise, on_chosen, on_failed, run)?;
    Ok(())
}

/// The person answered the save dialog.
fn on_chosen(_: &JsValue, args: &[JsValue], run: &u64, context: &mut Context) -> JsResult<JsValue> {
    let Some(id) = current(context, *run) else {
        return Ok(JsValue::undefined());
    };
    let answer = arg(args, 0);
    let path = get(&answer, "filePath", context)?;
    let canceled = get(&answer, "canceled", context)?.to_boolean();
    let path = native::text_of(&path, context)?;
    if canceled || path.is_empty() {
        cancel(context, id);
    } else {
        with_item(context, id, |item| {
            item.save_path = Some(PathBuf::from(path))
        });
        save(context, id)?;
    }
    Ok(JsValue::undefined())
}

/// The path of the partial file: the file's own, with `.crdownload`.
fn partial_of(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".crdownload");
    PathBuf::from(name)
}

/// The file has a path: the partial file is opened, and the body is read into it.
fn save(context: &mut Context, id: u32) -> JsResult<()> {
    let opened = reopen(context, id, true);
    if define::report(context, opened).is_none() {
        interrupt(context, id);
        return Ok(());
    }
    emit_updated(context, id);
    let run = with_item(context, id, |item| item.run).unwrap_or(0);
    read_next(context, run);
    Ok(())
}

/// Opens the partial file: made again from the start with `restart`, else as it is, to go on
/// writing at its end.
fn reopen(context: &mut Context, id: u32, restart: bool) -> JsResult<()> {
    let Some(path) = with_item(context, id, |item| item.save_path.clone()).flatten() else {
        return Ok(());
    };
    let partial = partial_of(&path);
    let file = if restart {
        File::create(&partial)
    } else {
        std::fs::OpenOptions::new().write(true).open(&partial)
    };
    let mut file = file.map_err(|error| define::error(&error.to_string()))?;
    let end = if restart {
        0
    } else {
        file.seek(SeekFrom::End(0)).unwrap_or(0)
    };
    with_item(context, id, |item| {
        item.received = end;
        item.saving = true;
        item.file = Some(file);
    });
    Ok(())
}

/// Reads the next piece of the body.
fn read_next(context: &mut Context, run: u64) {
    let Some(id) = current(context, run) else {
        return;
    };
    let Some(reader) = with_item(context, id, |item| item.reader.clone()).flatten() else {
        return;
    };
    let outcome = invoke(&reader.into(), "read", &[], context)
        .and_then(|promise| native::then(context, &promise, on_read, on_failed, run));
    if let Err(error) = outcome {
        verve::report(context, &error);
        interrupt(context, id);
    }
}

fn on_read(_: &JsValue, args: &[JsValue], run: &u64, context: &mut Context) -> JsResult<JsValue> {
    let Some(id) = current(context, *run) else {
        return Ok(JsValue::undefined());
    };
    let result = arg(args, 0);
    if get(&result, "done", context)?.to_boolean() {
        complete(context, id);
        return Ok(JsValue::undefined());
    }
    let bytes =
        verve::encoding::bytes_of(&get(&result, "value", context)?, context)?.unwrap_or_default();
    let written = with_item(context, id, |item| {
        let file = item.file.as_mut()?;
        file.write_all(&bytes).ok()?;
        item.received += bytes.len() as u64;
        Some(())
    });
    if written.flatten().is_none() {
        interrupt(context, id);
        return Ok(JsValue::undefined());
    }
    let due = with_item(context, id, |item| {
        let due = item
            .last_update
            .is_none_or(|at| at.elapsed() >= UPDATE_EVERY);
        if due {
            item.last_update = Some(Instant::now());
        }
        due
    });
    if due == Some(true) {
        emit_updated(context, id);
    }
    read_next(context, *run);
    Ok(JsValue::undefined())
}

/// The whole body is in the partial file: it becomes the file.
fn complete(context: &mut Context, id: u32) {
    let moved = with_item(context, id, |item| {
        let path = item.save_path.clone()?;
        item.file = None;
        item.reader = None;
        item.abort = None;
        std::fs::rename(partial_of(&path), &path).ok()
    });
    if moved.flatten().is_none() {
        return interrupt(context, id);
    }
    finish(context, id, State::Completed);
}

/// The download ends in `state`, which `done` is told.
fn finish(context: &mut Context, id: u32, state: State) {
    with_item(context, id, |item| {
        item.state = state;
        item.run = 0;
    });
    emit_done(context, id);
    if state != State::Interrupted {
        retire(context, id);
    }
}

/// The download is over for good: the oldest finished ones beyond [`KEEP_FINISHED`] are freed,
/// with the objects scripts hold for them.
fn retire(context: &Context, id: u32) {
    let objects = &super::shared(context).net.downloads;
    let mut finished = objects.finished.borrow_mut();
    finished.push_back(id);
    while finished.len() > KEEP_FINISHED {
        if let Some(old) = finished.pop_front() {
            objects.items.borrow_mut().remove(&old);
            objects.objects.borrow_mut().remove(&old);
        }
    }
}

/// The transfer broke: the partial file stays, so that a resume can go on from it.
pub(crate) fn interrupt(context: &mut Context, id: u32) {
    let started = with_item(context, id, |item| {
        item.file = None;
        let stopped = (item.abort.take(), item.reader.take());
        (item.started, stopped)
    });
    let Some((started, (abort, reader))) = started else {
        return;
    };
    stop(context, abort, reader);
    if started {
        finish(context, id, State::Interrupted);
    } else {
        // It never got as far as a name: nothing was heard of it, and nothing is.
        with_item(context, id, |item| {
            item.state = State::Interrupted;
            item.run = 0;
        });
    }
}

/// `item.cancel()`: the transfer stops, the partial file is removed, and `done` says `cancelled`.
pub(crate) fn cancel(context: &mut Context, id: u32) {
    let stopped = with_item(context, id, |item| {
        if item.state != State::Progressing && !(item.state == State::Interrupted && item.started) {
            return None;
        }
        item.file = None;
        item.run = 0;
        let partial = item.save_path.as_deref().map(partial_of);
        Some((item.abort.take(), item.reader.take(), partial))
    });
    let Some(Some((abort, reader, partial))) = stopped else {
        return;
    };
    stop(context, abort, reader);
    if let Some(partial) = partial {
        // A partial file that is already gone is what cancelling wants; one that cannot be
        // removed is left for the person to delete, and the download is cancelled all the same.
        let _ = std::fs::remove_file(partial);
    }
    finish(context, id, State::Cancelled);
}

/// `item.pause()`: the request stops, the partial file stays, and `updated` says it is paused.
pub(crate) fn pause(context: &mut Context, id: u32) {
    let stopped = with_item(context, id, |item| {
        if item.state != State::Progressing || item.paused || !item.started {
            return None;
        }
        item.paused = true;
        item.run = 0;
        item.file = None;
        Some((item.abort.take(), item.reader.take()))
    });
    let Some(Some((abort, reader))) = stopped else {
        return;
    };
    stop(context, abort, reader);
    emit_updated(context, id);
}

/// `item.resume()`: the rest is asked for with `Range`.
pub(crate) fn resume(context: &mut Context, id: u32) {
    let ready = with_item(context, id, |item| {
        let again = item.paused || (item.state == State::Interrupted && item.started);
        if again {
            item.paused = false;
            item.state = State::Progressing;
        }
        again
    });
    if ready == Some(true) {
        transfer(context, id);
    }
}
