//! `clipboard`: the system clipboard, read and written from the main process's thread (or a
//! page's, for one with `nodeIntegration` and no sandbox) without a trip through the shell. The
//! formats and the way Windows keeps them are [`crate::os::Clipboard`]'s; the argument errors are
//! Electron's native (gin) ones, frozen in `tests/electron/system-a/expected/system.json`.
//!
//! The `type` argument of Linux's selection clipboard is accepted and ignored.

use boa_engine::{Context, JsResult, JsValue};
use verve::encoding::bytes_of;
use verve::native::{self, arg};

use super::define::{Args, buffer_of};
use super::native_image::{image_of, wrap};
use crate::os::{Clip, Clipboard};

/// The `clipboard` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("readText", 0, |_, _, _| {
                Ok(native::js_text(&Clipboard::text()))
            }),
            ("writeText", 1, write_text),
            ("readHTML", 0, |_, _, _| {
                Ok(native::js_text(&Clipboard::html()))
            }),
            ("writeHTML", 1, write_html),
            ("readRTF", 0, |_, _, _| {
                Ok(native::js_text(&Clipboard::rtf()))
            }),
            ("writeRTF", 1, write_rtf),
            ("readImage", 0, |_, _, cx| wrap(cx, Clipboard::image())),
            ("writeImage", 1, write_image),
            ("readBookmark", 0, read_bookmark),
            ("writeBookmark", 2, write_bookmark),
            ("readFindText", 0, |_, _, _| Ok(native::js_text(""))),
            ("writeFindText", 1, write_find_text),
            ("readBuffer", 1, read_buffer),
            ("writeBuffer", 2, write_buffer),
            ("availableFormats", 0, available_formats),
            ("has", 1, has),
            ("clear", 0, |_, _, _| {
                Clipboard::clear();
                Ok(JsValue::undefined())
            }),
            ("write", 1, write),
        ],
    )?;
    Ok(module.into())
}

/// Replaces the clipboard with `clips`, answering `undefined` as Electron's writers do.
fn put(clips: &[Clip]) -> JsResult<JsValue> {
    // A clipboard another program will not let go of is a write that did not happen, which
    // Electron does not report either.
    Clipboard::write(clips);
    Ok(JsValue::undefined())
}

fn write_text(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    put(&[Clip::Text(Args::new(args).string(0, "text", context)?)])
}

fn write_html(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    put(&[Clip::Html(Args::new(args).string(0, "markup", context)?)])
}

fn write_rtf(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    put(&[Clip::Rtf(Args::new(args).string(0, "text", context)?)])
}

fn write_image(_: &JsValue, args: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let image = image_of(&arg(args, 0)).ok_or_else(|| Args::new(args).conversion_failure(0))?;
    put(&[Clip::Image(image)])
}

fn read_bookmark(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = Clipboard::bookmark();
    let fields = [
        ("title", native::js_text("")),
        ("url", native::js_text(&url)),
    ];
    Ok(super::define::record(context, &fields)?.into())
}

fn write_bookmark(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let args = Args::new(args);
    args.string(0, "title", context)?;
    put(&[Clip::Bookmark(args.string(1, "url", context)?)])
}

/// `writeFindText` is macOS's find pasteboard; there is none to write.
fn write_find_text(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Args::new(args).string(0, "text", context)?;
    Ok(JsValue::undefined())
}

fn read_buffer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let format = Args::new(args).string(0, "format", context)?;
    buffer_of(context, Clipboard::buffer(&format))
}

fn write_buffer(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let format = Args::new(args).string(0, "format", context)?;
    let Some(bytes) = bytes_of(&arg(args, 1), context)? else {
        return Err(super::define::error("buffer must be a node Buffer"));
    };
    put(&[Clip::Buffer { format, bytes }])
}

fn available_formats(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let formats = Clipboard::formats()
        .iter()
        .map(|f| native::js_text(f))
        .collect();
    Ok(native::array(context, formats))
}

fn has(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let format = Args::new(args).string(0, "format", context)?;
    Ok(Clipboard::has(&format).into())
}

/// `write({ text, html, image, rtf, bookmark })`: everything at once, so that a paste can take the
/// richest format it understands. `bookmark` is the title of the URL that `text` is.
fn write(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let data = Args::new(args).object_opt(0, "data", context)?;
    let fields = super::define::Fields::new(data.as_ref());
    let text = fields.string("text", context)?;
    let mut clips = Vec::new();
    clips.extend(text.clone().map(Clip::Text));
    clips.extend(fields.string("html", context)?.map(Clip::Html));
    clips.extend(fields.string("rtf", context)?.map(Clip::Rtf));
    if let Some(object) = &data {
        let image = object.get(boa_engine::js_string!("image"), context)?;
        clips.extend(image_of(&image).map(Clip::Image));
    }
    if let (Some(_), Some(url)) = (fields.string("bookmark", context)?, text) {
        clips.push(Clip::Bookmark(url));
    }
    put(&clips)
}
