//! The older protocol calls (`registerFileProtocol`, `interceptStringProtocol`, ...): a handler
//! given `(request, callback)` that answers by calling `callback`. Whatever the callback is given
//! is made into the `Response` that `protocol.handle`'s handlers return, so one engine answers
//! every kind (`protocol_serve`). Electron 39.8.10 warns of none of them.

use boa_engine::object::builtins::JsUint8Array;
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, arg, get, invoke, js_text, object};

use super::define::{self, Fields};
use super::mime;
use super::protocol::{Entry, HandlerKind};
use super::protocol_serve::{answered, fail};
use crate::msg::ProtoRequest;

/// Calls the legacy handler of `entry` for `request`; the answer comes through the callback.
pub(crate) fn run(
    context: &mut Context,
    serial: u64,
    kind: HandlerKind,
    entry: &Entry,
    request: &ProtoRequest,
) -> JsResult<()> {
    let details = details_of(context, request)?;
    let callback = native::closure(context, "callback", 1, answer, (serial, kind));
    verve::call(
        context,
        &entry.handler,
        &JsValue::undefined(),
        &[details.into(), callback.into()],
    )?;
    Ok(())
}

/// The request as the older handlers get it: `{ url, method, referrer, headers, uploadData }`.
fn details_of(context: &mut Context, request: &ProtoRequest) -> JsResult<JsObject> {
    let headers = object(context);
    for (name, value) in &request.headers {
        headers.set(
            boa_engine::JsString::from(name.as_str()),
            js_text(value),
            false,
            context,
        )?;
    }
    let details = object(context);
    details.set(js_string!("url"), js_text(&request.url), false, context)?;
    details.set(
        js_string!("method"),
        js_text(&request.method),
        false,
        context,
    )?;
    details.set(
        js_string!("referrer"),
        js_text(&request.referrer),
        false,
        context,
    )?;
    details.set(js_string!("headers"), headers, false, context)?;
    if let Some(body) = &request.body {
        let bytes = super::define::buffer_of(context, body.clone())?;
        let part = object(context);
        part.set(js_string!("bytes"), bytes, false, context)?;
        part.set(js_string!("type"), js_text("rawData"), false, context)?;
        let upload = native::array(context, vec![part.into()]);
        details.set(js_string!("uploadData"), upload, false, context)?;
    }
    Ok(details)
}

/// The callback: what it was given becomes the answer.
fn answer(
    _: &JsValue,
    args: &[JsValue],
    (serial, kind): &(u64, HandlerKind),
    context: &mut Context,
) -> JsResult<JsValue> {
    let given = arg(args, 0);
    let object = given.as_object();
    let fields = Fields::new(object.as_ref());
    // `callback({ error: -6 })` fails the request with that Chromium error.
    if let Some(code) = fields.f64_opt("error", context)? {
        fail(context, *serial, code as i32);
        return Ok(JsValue::undefined());
    }
    match kind {
        HandlerKind::File => from_file(context, *serial, &given, &fields)?,
        HandlerKind::Stream => from_stream(context, *serial, &given, &fields)?,
        HandlerKind::Http => from_http(context, *serial, &fields)?,
        HandlerKind::Buffer | HandlerKind::String | HandlerKind::Handle => {
            from_data(context, *serial, &given, &fields)?
        }
    }
    Ok(JsValue::undefined())
}

/// The status and headers an answer object names, as a `Response` init; `content_type` is the
/// type to give when the object names none.
fn init_of(
    fields: &Fields,
    content_type: Option<String>,
    context: &mut Context,
) -> JsResult<JsObject> {
    let init = object(context);
    let status = fields.f64("statusCode", 200., context)?;
    init.set(js_string!("status"), status, false, context)?;
    let headers = object(context);
    if let Some(given) = fields.object("headers", context)? {
        for (name, value) in define::entries_of(&given, context)? {
            headers.set(boa_engine::JsString::from(name), value, false, context)?;
        }
    }
    if let Some(kind) = content_type
        && !headers.has_property(js_string!("content-type"), context)?
    {
        headers.set(js_string!("content-type"), js_text(&kind), false, context)?;
    }
    init.set(js_string!("headers"), headers, false, context)?;
    Ok(init)
}

/// `new Response(bytes, init)` handed to the engine.
fn respond(context: &mut Context, serial: u64, bytes: Vec<u8>, init: JsObject) -> JsResult<()> {
    let body = JsUint8Array::from_iter(bytes, context)?;
    let response = native::construct_global(context, "Response", &[body.into(), init.into()])?;
    answered(context, serial, &response.into());
    Ok(())
}

/// A file: the path is what the callback was given, or its `path`.
fn from_file(context: &mut Context, serial: u64, given: &JsValue, fields: &Fields) -> JsResult<()> {
    let path = match given.as_string() {
        Some(path) => path.to_std_string_escaped(),
        None => fields.string("path", context)?.unwrap_or_default(),
    };
    let path = std::path::PathBuf::from(path);
    let overlay = super::shared(context).hub.overlay();
    match crate::files::read(overlay.as_ref(), &path) {
        Ok(bytes) => {
            let init = init_of(fields, Some(mime::of_path(&path).to_owned()), context)?;
            respond(context, serial, bytes, init)
        }
        Err(error) => {
            fail(context, serial, error.code);
            Ok(())
        }
    }
}

/// A string or a buffer: the callback was given it, or an object with it as `data`.
fn from_data(context: &mut Context, serial: u64, given: &JsValue, fields: &Fields) -> JsResult<()> {
    let mut data = given.clone();
    if !data.is_string() && verve::encoding::bytes_of(&data, context)?.is_none() {
        data = get(given, "data", context)?;
    }
    let bytes = match data.as_string() {
        Some(text) => text.to_std_string_escaped().into_bytes(),
        None => verve::encoding::bytes_of(&data, context)?.unwrap_or_default(),
    };
    let charset = fields.string("charset", context)?;
    let kind = fields
        .string("mimeType", context)?
        .map(|mime| match charset {
            Some(charset) => format!("{mime}; charset={charset}"),
            None => mime,
        });
    let init = init_of(fields, kind, context)?;
    respond(context, serial, bytes, init)
}

/// A readable stream: collected whole, then sent.
fn from_stream(
    context: &mut Context,
    serial: u64,
    given: &JsValue,
    fields: &Fields,
) -> JsResult<()> {
    // The callback was given the stream itself, or an object with it as `data`.
    let stream = if get(given, "on", context)?.is_callable() {
        given.as_object()
    } else {
        fields.object("data", context)?
    };
    let Some(stream) = stream else {
        fail(context, serial, crate::msg::ERR_UNEXPECTED);
        return Ok(());
    };
    let init = init_of(fields, None, context)?;
    let chunks = native::array(context, Vec::new());
    let on_data = native::closure(
        context,
        "",
        1,
        |_, args, chunks: &JsValue, cx| invoke(chunks, "push", &[arg(args, 0)], cx),
        chunks.clone(),
    );
    let on_end = native::closure(
        context,
        "",
        0,
        |_, _, (serial, chunks, init): &(u64, JsValue, JsObject), cx| {
            let buffer = native::global(cx, "Buffer")?;
            let joined = invoke(&buffer, "concat", std::slice::from_ref(chunks), cx)?;
            let bytes = verve::encoding::bytes_of(&joined, cx)?.unwrap_or_default();
            respond(cx, *serial, bytes, init.clone())?;
            Ok(JsValue::undefined())
        },
        (serial, chunks, init),
    );
    let on_error = native::closure(
        context,
        "",
        1,
        |_, _, serial: &u64, cx| {
            fail(cx, *serial, crate::msg::ERR_FAILED);
            Ok(JsValue::undefined())
        },
        serial,
    );
    let stream: JsValue = stream.into();
    invoke(&stream, "on", &[js_text("data"), on_data.into()], context)?;
    invoke(&stream, "on", &[js_text("end"), on_end.into()], context)?;
    invoke(&stream, "on", &[js_text("error"), on_error.into()], context)?;
    Ok(())
}

/// An address to ask in the page's place: its response is the answer.
fn from_http(context: &mut Context, serial: u64, fields: &Fields) -> JsResult<()> {
    let Some(url) = fields.string("url", context)? else {
        fail(context, serial, crate::msg::ERR_UNEXPECTED);
        return Ok(());
    };
    let init = object(context);
    if let Some(method) = fields.string("method", context)? {
        init.set(js_string!("method"), js_text(&method), false, context)?;
    }
    if let Some(headers) = fields.object("headers", context)? {
        init.set(js_string!("headers"), headers, false, context)?;
    }
    let fetch = native::global(context, "fetch")?;
    let promise = verve::call(
        context,
        &fetch,
        &JsValue::undefined(),
        &[js_text(&url), init.into()],
    )?;
    native::then(
        context,
        &promise,
        |_, args, serial: &u64, cx| {
            answered(cx, *serial, &arg(args, 0));
            Ok(JsValue::undefined())
        },
        |_, _, serial: &u64, cx| {
            fail(cx, *serial, crate::msg::ERR_FAILED);
            Ok(JsValue::undefined())
        },
        serial,
    )?;
    Ok(())
}
