//! `fs.createReadStream` and `fs.createWriteStream`: the `stream` module's `Readable` and
//! `Writable` over one file, a chunk at a time.
//!
//! A file stream is made by an `fs` export and keeps that export's [`View`] for as long as it
//! runs, so one made by `original-fs` reads the disk alone from its first chunk to its last.
//! `open` and `ready` come a tick after the stream is made, as Node sends them, and a file that
//! cannot be opened is found then rather than when the first chunk is asked for.

use std::cell::RefCell;

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};

use crate::fs::sys::Held;
use crate::native::{arg, closure, function, get, hidden, invoke, js_text, object, text_of};
use crate::vfs::View;

use super::{emit, error_value, flag, module_class, next_tick, put, sget, wstate};

// ─── fs.createReadStream, fs.createWriteStream ───────────────────────────────────────────────

fn fs_path(value: &JsValue, context: &mut Context) -> JsResult<String> {
    if let Some(bytes) = crate::encoding::bytes_of(value, context)? {
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    let text = match value.as_object() {
        // A `URL`.
        Some(url) if url.has_property(js_string!("href"), context)? => {
            text_of(&url.get(js_string!("href"), context)?, context)?
        }
        _ => text_of(value, context)?,
    };
    Ok(match text.strip_prefix("file://") {
        Some(rest) if rest.get(2..3) == Some(":") && rest.starts_with('/') => rest[1..].to_owned(),
        Some(rest) => rest.to_owned(),
        None => text,
    })
}

fn fs_error(context: &mut Context, error: &std::io::Error, path: &str, call: &str) -> JsValue {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        std::io::ErrorKind::IsADirectory => "EISDIR",
        _ => "EIO",
    };
    let value = error_value(context, &format!("{code}: {error}, {call} '{path}'"), code);
    if let Some(object) = value.as_object() {
        let _ = object.set(js_string!("path"), js_text(path), false, context);
        let _ = object.set(js_string!("syscall"), js_text(call), false, context);
    }
    value
}

/// The options of `createReadStream`/`createWriteStream`: an object, or an encoding's name.
fn fs_options(value: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    if let Some(options) = value.as_object().filter(|_| !value.is_string()) {
        let copy = object(context);
        for key in options.own_property_keys(context)? {
            let item = options.get(key.clone(), context)?;
            copy.set(key, item, false, context)?;
        }
        return Ok(copy);
    }
    let options = object(context);
    if value.is_string() {
        put(&options, "encoding", value.clone(), context)?;
    }
    Ok(options)
}

/// `fs.createReadStream(path, { start, end, highWaterMark, encoding })`: a `Readable` reading
/// the file a chunk (64 KiB unless asked otherwise) at a time, from `start` to `end`
/// inclusive. Says `open` and `ready` a tick after it is made; a file that cannot be opened
/// destroys it with the error.
pub(crate) fn create_read_stream(
    view: &View,
    context: &mut Context,
    args: &[JsValue],
) -> JsResult<JsValue> {
    let path = fs_path(&arg(args, 0), context)?;
    let options = fs_options(&arg(args, 1), context)?;
    if sget(&options, "highWaterMark", context)?.is_undefined() {
        put(&options, "highWaterMark", 65536, context)?;
    }
    let start = sget(&options, "start", context)?;
    let start = if start.is_undefined() {
        0.
    } else {
        start.to_number(context)?
    };
    let end = sget(&options, "end", context)?;
    let end = if end.is_undefined() {
        f64::INFINITY
    } else {
        end.to_number(context)?
    };
    let readable = module_class(context, "Readable")?;
    let stream = readable.construct(&[options.into()], None, context)?;
    stream.set(js_string!("path"), js_text(&path), false, context)?;
    stream.set(js_string!("bytesRead"), 0, false, context)?;
    stream.set(js_string!("pending"), true, false, context)?;
    stream.set(js_string!("fd"), JsValue::null(), false, context)?;
    stream.set(js_string!("flags"), js_text("r"), false, context)?;
    stream.set(js_string!("start"), start, false, context)?;
    stream.set(js_string!("end"), end, false, context)?;
    hidden(&stream, js_string!("__position"), start, context)?;
    let source = ReadSource {
        view: view.clone(),
        held: RefCell::new(None),
    };
    let read = closure(context, "_read", 1, read_chunk, source);
    stream.set(js_string!("_read"), read, false, context)?;
    let close = function(context, "close", 1, |this, args, cx| {
        let callback = arg(args, 0);
        if callback.is_callable() {
            invoke(this, "once", &[js_text("close"), callback], cx)?;
        }
        invoke(this, "destroy", &[], cx)
    });
    stream.set(js_string!("close"), close, false, context)?;
    // Opened a tick from now, as Node opens it: `open` with a descriptor, then `ready` — or
    // the error.
    let opened = closure(context, "", 0, open_tick, view.clone());
    crate::microtask(context, opened.into(), vec![stream.clone().into()]);
    Ok(stream.into())
}

/// What a file stream reads through: the view it was made with, and the bytes of an overlay's
/// file once they have been asked for.
struct ReadSource {
    view: View,
    held: RefCell<Held>,
}

impl boa_gc::Finalize for ReadSource {}

// SAFETY: a view and bytes hold no garbage-collected value.
unsafe impl boa_gc::Trace for ReadSource {
    boa_gc::empty_trace!();
}

/// One read of a file stream: the chunk at its position, through the view it was made with.
fn read_chunk(
    this: &JsValue,
    args: &[JsValue],
    source: &ReadSource,
    cx: &mut Context,
) -> JsResult<JsValue> {
    let path = text_of(&get(this, "path", cx)?, cx)?;
    let position = get(this, "__position", cx)?.to_number(cx)?;
    let end = get(this, "end", cx)?.to_number(cx)?;
    let wanted = arg(args, 0).to_number(cx)?.max(1.);
    let left = end - position + 1.;
    if left <= 0. {
        invoke(this, "push", &[JsValue::null()], cx)?;
        return Ok(JsValue::undefined());
    }
    let size = wanted.min(left).min(16. * 1024. * 1024.) as usize;
    // `path` is the stream's own property, which code can change between reads.
    if let Err(error) = crate::permission::read(cx, &path) {
        let error = error.into_opaque(cx)?;
        invoke(this, "destroy", &[error], cx)?;
        return Ok(JsValue::undefined());
    }
    let read = crate::fs::sys::read_range(
        &source.view,
        std::path::Path::new(&path),
        &mut source.held.borrow_mut(),
        position as u64,
        size,
    );
    match read {
        Err(error) => {
            let error = fs_error(cx, &error, &path, "read");
            invoke(this, "destroy", &[error], cx)?;
        }
        Ok(bytes) if bytes.is_empty() => {
            invoke(this, "push", &[JsValue::null()], cx)?;
        }
        Ok(bytes) => {
            let count = bytes.len() as f64;
            if let Some(target) = this.as_object() {
                target.set(js_string!("__position"), position + count, false, cx)?;
                let read = target.get(js_string!("bytesRead"), cx)?.to_number(cx)?;
                target.set(js_string!("bytesRead"), read + count, false, cx)?;
            }
            let buffer = crate::encoding::make_buffer(cx, bytes)?;
            invoke(this, "push", &[buffer], cx)?;
        }
    }
    Ok(JsValue::undefined())
}

/// The stream's opening, a tick after it was made: `open` and `ready`, or the error that
/// destroys it.
fn open_tick(_: &JsValue, args: &[JsValue], view: &View, cx: &mut Context) -> JsResult<JsValue> {
    let stream = arg(args, 0);
    let path = text_of(&get(&stream, "path", cx)?, cx)?;
    if let Err(error) = crate::permission::read(cx, &path) {
        let error = error.into_opaque(cx)?;
        invoke(&stream, "destroy", &[error], cx)?;
        return Ok(JsValue::undefined());
    }
    match crate::fs::sys::probe_open(view, std::path::Path::new(&path)) {
        Ok(()) => {
            if let Some(target) = stream.as_object() {
                target.set(js_string!("pending"), false, false, cx)?;
                target.set(js_string!("fd"), 3, false, cx)?;
            }
            emit(&stream, &[js_text("open"), 3.into()], cx)?;
            emit(&stream, &[js_text("ready")], cx)?;
        }
        Err(error) => {
            let error = fs_error(cx, &error, &path, "open");
            invoke(&stream, "destroy", &[error], cx)?;
        }
    }
    Ok(JsValue::undefined())
}

/// Opens the file for the stream to write, made empty (`'w'`) or kept (`'a'`): what went wrong,
/// as the error the stream is destroyed with, or nothing.
fn open_target(
    view: &View,
    context: &mut Context,
    path: &str,
    flags: &str,
) -> JsResult<Option<JsValue>> {
    let path = path.to_owned();
    let flags = flags.to_owned();
    let allowed = crate::fs::may_write(view, context, &path, "open", crate::fs::Scope::Path);
    let opened = if let Err(denied) = allowed {
        Err(Err(denied))
    } else if flags.starts_with('a') {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(Ok)
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(Ok)
    };
    let failure = match opened {
        Ok(_) => None,
        Err(Ok(error)) => Some(fs_error(context, &error, &path, "open")),
        Err(Err(denied)) => Some(denied.into_opaque(context)?),
    };
    Ok(failure)
}

/// One `_write` of a file stream: the chunk appended to the file, or the error it fails with.
fn write_chunk(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let path = text_of(&get(this, "path", cx)?, cx)?;
    let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
    let callback = arg(args, 2);
    if let Err(error) = crate::permission::write(cx, &path) {
        let error = error.into_opaque(cx)?;
        return crate::call(cx, &callback, &JsValue::undefined(), &[error]);
    }
    let outcome = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(&bytes)
    })();
    match outcome {
        Ok(()) => {
            if let Some(target) = this.as_object() {
                let written = target.get(js_string!("bytesWritten"), cx)?.to_number(cx)?;
                target.set(
                    js_string!("bytesWritten"),
                    written + bytes.len() as f64,
                    false,
                    cx,
                )?;
            }
            crate::call(cx, &callback, &JsValue::undefined(), &[])
        }
        Err(error) => {
            let error = fs_error(cx, &error, &path, "write");
            crate::call(cx, &callback, &JsValue::undefined(), &[error])
        }
    }
}

/// `close` and `destroySoon` of a write stream: what was written is flushed (`end`) before it
/// is destroyed.
fn close_write(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let callback = arg(args, 0);
    if callback.is_callable() {
        invoke(this, "once", &[js_text("close"), callback], cx)?;
    }
    let ended = match wstate(this, cx)? {
        Some(w) => flag(&w, "ending", cx)?,
        None => true,
    };
    if ended {
        invoke(this, "destroy", &[], cx)
    } else {
        invoke(this, "end", &[], cx)
    }
}

/// `fs.createWriteStream(path, { flags })`: a `Writable` writing each chunk to the file as it
/// comes — the file made empty first (`'w'`), or added to (`'a'`). Says `open` and `ready` a
/// tick after it is made, `finish` and `close` when ended.
pub(crate) fn create_write_stream(
    view: &View,
    context: &mut Context,
    args: &[JsValue],
) -> JsResult<JsValue> {
    let path = fs_path(&arg(args, 0), context)?;
    let options = fs_options(&arg(args, 1), context)?;
    let flags = sget(&options, "flags", context)?;
    let flags = if flags.is_undefined() {
        "w".to_owned()
    } else {
        text_of(&flags, context)?
    };
    let writable = module_class(context, "Writable")?;
    let encoding = sget(&options, "encoding", context)?;
    if !encoding.is_undefined() {
        put(&options, "defaultEncoding", encoding, context)?;
    }
    let stream = writable.construct(&[options.into()], None, context)?;
    stream.set(js_string!("path"), js_text(&path), false, context)?;
    stream.set(js_string!("bytesWritten"), 0, false, context)?;
    stream.set(js_string!("pending"), true, false, context)?;
    stream.set(js_string!("fd"), JsValue::null(), false, context)?;
    stream.set(js_string!("flags"), js_text(&flags), false, context)?;
    let failure = open_target(view, context, &path, &flags)?;
    let write = function(context, "_write", 3, write_chunk);
    stream.set(js_string!("_write"), write, false, context)?;
    let close = function(context, "close", 1, close_write);
    stream.set(js_string!("close"), close.clone(), false, context)?;
    stream.set(js_string!("destroySoon"), close, false, context)?;
    next_tick(
        context,
        |_, args, cx| {
            let (stream, failure) = (arg(args, 0), arg(args, 1));
            if !failure.is_undefined() {
                invoke(&stream, "destroy", &[failure], cx)?;
                return Ok(JsValue::undefined());
            }
            if let Some(target) = stream.as_object() {
                target.set(js_string!("pending"), false, false, cx)?;
                target.set(js_string!("fd"), 3, false, cx)?;
            }
            emit(&stream, &[js_text("open"), 3.into()], cx)?;
            emit(&stream, &[js_text("ready")], cx)?;
            Ok(JsValue::undefined())
        },
        vec![stream.clone().into(), failure.unwrap_or_default()],
    );
    Ok(stream.into())
}
