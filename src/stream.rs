//! `stream`: Node's streams, natively — `Stream`, `Readable`, `Writable`, `Duplex`,
//! `Transform` and `PassThrough`, with `pipeline`, `finished`, `Readable.from`, and
//! `stream/promises`.
//!
//! They follow Node's own algorithms, step for step where it shows: what is buffered and when
//! `_read` is asked for more, when `data`, `readable`, `end`, `finish`, `drain`, `close` and
//! `error` are heard and in which order (later ones a tick later, as Node's `process.nextTick`
//! puts them), what `write` answers past the high-water mark, and how `destroy` ends both
//! sides. A stream's state is where Node keeps it, in `_readableState` and `_writableState`,
//! with Node's field names — libraries read `objectMode`, `ended`, `flowing`, `length`,
//! `highWaterMark` and the rest there, and some set them.
//!
//! Every class extends the ES5 way too (`Readable.call(this, options)` with `util.inherits`), as
//! the npm packages bundled into plugins mostly do; see `crate::native`. A `Duplex` is an
//! instance of `Writable` as well as of `Readable`, as in Node.
//!
//! `fs.createReadStream` and `fs.createWriteStream` are made here too ([`create_read_stream`],
//! [`create_write_stream`]), reading and writing the disk a chunk at a time.

use std::path::PathBuf;

use boa_engine::{
    Context, JsError, JsObject, JsResult, JsString, JsSymbol, JsValue, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsArray, JsPromise, JsUint8Array},
};

use crate::events::{abort_error, coded_type_error};
use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, function, get, hidden, invoke,
    js_text, list_of, object, text_of,
};

// ─── Small things ────────────────────────────────────────────────────────────────────────────

fn sget(state: &JsObject, key: &str, context: &mut Context) -> JsResult<JsValue> {
    state.get(JsString::from(key), context)
}

fn flag(state: &JsObject, key: &str, context: &mut Context) -> JsResult<bool> {
    Ok(sget(state, key, context)?.to_boolean())
}

fn num(state: &JsObject, key: &str, context: &mut Context) -> JsResult<f64> {
    let value = sget(state, key, context)?;
    match value.as_number() {
        Some(n) => Ok(n),
        None if value.is_null_or_undefined() => Ok(0.),
        None => value.to_number(context),
    }
}

fn put(
    state: &JsObject,
    key: &str,
    value: impl Into<JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    state.set(JsString::from(key), value, false, context)?;
    Ok(())
}

/// A stream's `_readableState`, when it has one.
fn rstate(stream: &JsValue, context: &mut Context) -> JsResult<Option<JsObject>> {
    Ok(get(stream, "_readableState", context)?.as_object())
}

/// A stream's `_writableState`, when it has one.
fn wstate(stream: &JsValue, context: &mut Context) -> JsResult<Option<JsObject>> {
    Ok(get(stream, "_writableState", context)?.as_object())
}

fn emit(stream: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    invoke(stream, "emit", args, context)
}

fn listener_count(stream: &JsValue, name: &str, context: &mut Context) -> JsResult<f64> {
    if !get(stream, "listenerCount", context)?.is_callable() {
        return Ok(0.);
    }
    invoke(stream, "listenerCount", &[js_text(name)], context)?.to_number(context)
}

/// `f` with `args` a tick from now, as Node's `process.nextTick`.
fn next_tick(context: &mut Context, f: NativeFunctionPointer, args: Vec<JsValue>) {
    let f = function(context, "", 0, f);
    crate::microtask(context, f.into(), args);
}

/// Calls the method `name` of any value, a string's included.
fn call_method(
    value: &JsValue,
    name: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let method = value
        .to_object(context)?
        .get(JsString::from(name), context)?;
    crate::call(context, &method, value, args)
}

fn error_value(context: &mut Context, message: &str, code: &str) -> JsValue {
    coded_error_value(context, message, code)
}

fn type_error_value(context: &mut Context, message: &str, code: &str) -> JsValue {
    coded_type_error(context, message, code)
        .into_opaque(context)
        .unwrap_or_default()
}

/// A native function holding a state object: the handlers `pipe`, `finished` and `pipeline`
/// make share one each.
type Handler = fn(&JsValue, &[JsValue], &JsObject, &mut Context) -> JsResult<JsValue>;

fn nop() -> NativeFunctionPointer {
    |_, _, _| Ok(JsValue::undefined())
}

/// Whether `value` is a `Uint8Array` — a `Buffer` is one.
fn is_bytes(value: &JsValue) -> bool {
    value
        .as_object()
        .is_some_and(|o| JsUint8Array::from_object(o.clone()).is_ok())
}

fn buffer_class(context: &mut Context) -> JsResult<JsValue> {
    crate::native::global(context, "Buffer")
}

fn is_buffer(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    if !is_bytes(value) {
        return Ok(false);
    }
    let class = buffer_class(context)?;
    value.instance_of(&class, context)
}

/// `Buffer.from(value, encoding)`.
fn to_buffer(value: &JsValue, encoding: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let class = buffer_class(context)?;
    let mut args = vec![value.clone()];
    if !encoding.is_null_or_undefined() && !is_empty_text(encoding) {
        args.push(encoding.clone());
    }
    invoke(&class, "from", &args, context)
}

fn is_empty_text(value: &JsValue) -> bool {
    value.as_string().is_some_and(|s| s.is_empty())
}

/// A chunk's length as a stream counts it: one in object mode, its `length` otherwise.
fn chunk_length(chunk: &JsValue, object_mode: bool, context: &mut Context) -> JsResult<f64> {
    if object_mode {
        return Ok(1.);
    }
    if let Some(text) = chunk.as_string() {
        return Ok(text.len() as f64);
    }
    get(chunk, "length", context)?.to_number(context)
}

/// The stream module itself, from inside: its classes and its settings.
fn module_value(context: &mut Context) -> JsResult<JsValue> {
    crate::modules::node_module(context, "stream")
}

fn module_class(context: &mut Context, name: &str) -> JsResult<JsObject> {
    let module = module_value(context)?;
    get(&module, name, context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("stream.{name} is not a class")))
}

fn class_prototype(context: &mut Context, name: &str) -> JsResult<JsObject> {
    module_class(context, name)?
        .get(js_string!("prototype"), context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("stream.{name} has no prototype")))
}

/// The high-water mark a stream gets when its options give none: 16 objects, or 64 KiB.
fn default_high_water_mark(object_mode: bool, context: &mut Context) -> JsResult<f64> {
    let module = module_value(context)?;
    let key = if object_mode {
        "__defaultObjectHighWaterMark"
    } else {
        "__defaultHighWaterMark"
    };
    let value = get(&module, key, context)?;
    Ok(value
        .as_number()
        .unwrap_or(if object_mode { 16. } else { 65536. }))
}

fn high_water_mark(
    options: &JsValue,
    duplex_key: &str,
    is_duplex: bool,
    object_mode: bool,
    context: &mut Context,
) -> JsResult<f64> {
    let mut given = get(options, "highWaterMark", context)?;
    if given.is_null_or_undefined() && is_duplex {
        given = get(options, duplex_key, context)?;
    }
    if given.is_null_or_undefined() {
        return default_high_water_mark(object_mode, context);
    }
    let n = given.to_number(context)?;
    if !n.is_finite() || n < 0. {
        let message = format!(
            "The value of \"options.highWaterMark\" is invalid. Received {}",
            crate::print(&given)
        );
        return Err(coded_type_error(context, &message, "ERR_INVALID_ARG_VALUE"));
    }
    Ok(n.floor())
}

// ─── Destroying, and errors ──────────────────────────────────────────────────────────────────

/// Keeps `error` as the first error of each side.
fn note_error(
    error: &JsValue,
    w: &Option<JsObject>,
    r: &Option<JsObject>,
    context: &mut Context,
) -> JsResult<()> {
    if error.is_null_or_undefined() {
        return Ok(());
    }
    for state in [w, r].into_iter().flatten() {
        if sget(state, "errored", context)?.is_null_or_undefined() {
            put(state, "errored", error.clone(), context)?;
        }
    }
    Ok(())
}

/// `destroy(error, callback)`: both sides marked destroyed, `_destroy` asked to let go, then
/// `error` (if any) and `close` heard a tick later.
fn destroy(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (error, callback) = (arg(args, 0), arg(args, 1));
    let r = rstate(this, context)?;
    let w = wstate(this, context)?;
    let already = [&w, &r]
        .into_iter()
        .flatten()
        .map(|s| flag(s, "destroyed", context))
        .collect::<JsResult<Vec<_>>>()?
        .into_iter()
        .any(|d| d);
    if already {
        if callback.is_callable() {
            crate::call(context, &callback, &JsValue::undefined(), &[])?;
        }
        return Ok(this.clone());
    }
    note_error(&error, &w, &r, context)?;
    for state in [&w, &r].into_iter().flatten() {
        put(state, "destroyed", true, context)?;
    }
    let constructed = [&w, &r]
        .into_iter()
        .flatten()
        .map(|s| flag(s, "constructed", context))
        .collect::<JsResult<Vec<_>>>()?
        .into_iter()
        .all(|c| c);
    if !constructed {
        // `_construct` has not called back: destroyed once it does.
        let pending = array(context, vec![error, callback]);
        if let Some(target) = this.as_object() {
            hidden(
                &target,
                js_string!("__destroyAfterConstruct"),
                pending,
                context,
            )?;
        }
        return Ok(this.clone());
    }
    run_destroy(this, error, callback, context)?;
    Ok(this.clone())
}

fn run_destroy(
    this: &JsValue,
    error: JsValue,
    callback: JsValue,
    context: &mut Context,
) -> JsResult<()> {
    let state = object(context);
    put(&state, "stream", this.clone(), context)?;
    put(&state, "callback", callback, context)?;
    put(&state, "called", false, context)?;
    let on_destroy = closure(
        context,
        "onDestroy",
        1,
        |_, args, state, cx| {
            if flag(state, "called", cx)? {
                return Ok(JsValue::undefined());
            }
            put(state, "called", true, cx)?;
            let stream = sget(state, "stream", cx)?;
            let error = arg(args, 0);
            let r = rstate(&stream, cx)?;
            let w = wstate(&stream, cx)?;
            note_error(&error, &w, &r, cx)?;
            for s in [&w, &r].into_iter().flatten() {
                put(s, "closed", true, cx)?;
            }
            let callback = sget(state, "callback", cx)?;
            if callback.is_callable() {
                crate::call(cx, &callback, &stream, std::slice::from_ref(&error))?;
            }
            if error.is_null_or_undefined() {
                next_tick(
                    cx,
                    |_, args, cx| emit_close(&arg(args, 0), cx),
                    vec![stream],
                );
            } else {
                next_tick(
                    cx,
                    |_, args, cx| {
                        emit_error(&arg(args, 0), &arg(args, 1), cx)?;
                        emit_close(&arg(args, 0), cx)
                    },
                    vec![stream, error],
                );
            }
            Ok(JsValue::undefined())
        },
        state,
    );
    let error = if error.is_undefined() {
        JsValue::null()
    } else {
        error
    };
    if let Err(thrown) = invoke(
        this,
        "_destroy",
        &[error, on_destroy.clone().into()],
        context,
    ) {
        let thrown = thrown.into_opaque(context)?;
        crate::call(
            context,
            &on_destroy.into(),
            &JsValue::undefined(),
            &[thrown],
        )?;
    }
    Ok(())
}

fn emit_close(stream: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let r = rstate(stream, context)?;
    let w = wstate(stream, context)?;
    let mut wants = false;
    for state in [&w, &r].into_iter().flatten() {
        put(state, "closeEmitted", true, context)?;
        wants |= flag(state, "emitClose", context)?;
    }
    if wants {
        emit(stream, &[js_text("close")], context)?;
    }
    Ok(JsValue::undefined())
}

fn emit_error(stream: &JsValue, error: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let r = rstate(stream, context)?;
    let w = wstate(stream, context)?;
    for state in [&w, &r].into_iter().flatten() {
        if flag(state, "errorEmitted", context)? {
            return Ok(JsValue::undefined());
        }
    }
    for state in [&w, &r].into_iter().flatten() {
        put(state, "errorEmitted", true, context)?;
    }
    emit(stream, &[js_text("error"), error.clone()], context)?;
    Ok(JsValue::undefined())
}

/// An error a stream ran into: it destroys itself with it when it destroys itself on its own
/// (`autoDestroy`), and is only told of it otherwise.
fn error_or_destroy(
    stream: &JsValue,
    error: JsValue,
    sync: bool,
    context: &mut Context,
) -> JsResult<()> {
    let r = rstate(stream, context)?;
    let w = wstate(stream, context)?;
    let mut auto = false;
    for state in [&w, &r].into_iter().flatten() {
        if flag(state, "destroyed", context)? {
            return Ok(());
        }
        auto |= flag(state, "autoDestroy", context)?;
    }
    if auto {
        invoke(stream, "destroy", &[error], context)?;
    } else if !error.is_null_or_undefined() {
        note_error(&error, &w, &r, context)?;
        if sync {
            next_tick(
                context,
                |_, args, cx| emit_error(&arg(args, 0), &arg(args, 1), cx),
                vec![stream.clone(), error],
            );
        } else {
            emit_error(stream, &error, context)?;
        }
    }
    Ok(())
}

/// `_undestroy`: a destroyed stream made new again, as Node's `Duplex` sometimes needs.
fn undestroy(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let r = rstate(this, context)?;
    let w = wstate(this, context)?;
    for state in [&w, &r].into_iter().flatten() {
        for key in ["destroyed", "closed", "closeEmitted", "errorEmitted"] {
            put(state, key, false, context)?;
        }
        put(state, "errored", JsValue::null(), context)?;
    }
    if let Some(r) = &r {
        for key in ["ended", "endEmitted"] {
            put(r, key, false, context)?;
        }
    }
    if let Some(w) = &w {
        for key in ["ended", "ending", "finalCalled", "prefinished", "finished"] {
            put(w, key, false, context)?;
        }
    }
    Ok(JsValue::undefined())
}

/// `_construct`, when the stream has one: called a tick from now, and the stream waits —
/// no `_read`, `_write` or `_destroy` — until it calls back.
fn construct_later(stream: &JsValue, context: &mut Context) -> JsResult<()> {
    if !get(stream, "_construct", context)?.is_callable() {
        return Ok(());
    }
    for state in [rstate(stream, context)?, wstate(stream, context)?]
        .into_iter()
        .flatten()
    {
        put(&state, "constructed", false, context)?;
    }
    next_tick(
        context,
        |_, args, cx| {
            let stream = arg(args, 0);
            let done = closure(
                cx,
                "onConstruct",
                1,
                |_, args, stream, cx| {
                    on_constructed(stream, arg(args, 0), cx)?;
                    Ok(JsValue::undefined())
                },
                stream.clone(),
            );
            if let Err(error) = invoke(&stream, "_construct", &[done.into()], cx) {
                let error = error.into_opaque(cx)?;
                on_constructed(&stream, error, cx)?;
            }
            Ok(JsValue::undefined())
        },
        vec![stream.clone()],
    );
    Ok(())
}

fn on_constructed(stream: &JsValue, error: JsValue, context: &mut Context) -> JsResult<()> {
    let r = rstate(stream, context)?;
    let w = wstate(stream, context)?;
    for state in [&w, &r].into_iter().flatten() {
        put(state, "constructed", true, context)?;
    }
    if let Some(target) = stream.as_object() {
        let pending = target.get(js_string!("__destroyAfterConstruct"), context)?;
        if pending.is_object() {
            let pending = list_of(&pending, context)?;
            run_destroy(stream, arg(&pending, 0), arg(&pending, 1), context)?;
            return Ok(());
        }
    }
    if !error.is_null_or_undefined() {
        invoke(stream, "destroy", &[error], context)?;
        return Ok(());
    }
    if let Some(r) = &r
        && flag(r, "needReadable", context)?
    {
        maybe_read_more(stream, r, context)?;
    }
    if let Some(w) = &w {
        if num(w, "pendingcb", context)? == 0. && need_finish(w, context)? {
            finish_maybe(stream, w, false, context)?;
        } else {
            clear_buffer(stream, w, context)?;
        }
    }
    Ok(())
}

// ─── Readable ────────────────────────────────────────────────────────────────────────────────

/// A readable side's state, Node's `ReadableState`.
fn readable_state(options: &JsValue, is_duplex: bool, context: &mut Context) -> JsResult<JsObject> {
    let state = object(context);
    if let Ok(prototype) = state_prototype(context, "ReadableState") {
        state.set_prototype(Some(prototype));
    }
    let object_mode = get(options, "objectMode", context)?.to_boolean()
        || (is_duplex && get(options, "readableObjectMode", context)?.to_boolean());
    let hwm = high_water_mark(
        options,
        "readableHighWaterMark",
        is_duplex,
        object_mode,
        context,
    )?;
    put(&state, "objectMode", object_mode, context)?;
    put(&state, "highWaterMark", hwm, context)?;
    let buffer = JsArray::new(context)?;
    put(&state, "buffer", buffer, context)?;
    put(&state, "length", 0, context)?;
    let pipes = JsArray::new(context)?;
    put(&state, "pipes", pipes, context)?;
    put(&state, "flowing", JsValue::null(), context)?;
    for key in [
        "ended",
        "endEmitted",
        "reading",
        "needReadable",
        "emittedReadable",
        "readableListening",
        "resumeScheduled",
        "errorEmitted",
        "destroyed",
        "closed",
        "closeEmitted",
        "multiAwaitDrain",
        "readingMore",
        "dataEmitted",
    ] {
        put(&state, key, false, context)?;
    }
    put(&state, "constructed", true, context)?;
    put(&state, "sync", true, context)?;
    put(&state, "paused", JsValue::null(), context)?;
    put(
        &state,
        "emitClose",
        !get(options, "emitClose", context)?.strict_equals(&false.into()),
        context,
    )?;
    put(
        &state,
        "autoDestroy",
        !get(options, "autoDestroy", context)?.strict_equals(&false.into()),
        context,
    )?;
    put(&state, "errored", JsValue::null(), context)?;
    let default_encoding = get(options, "defaultEncoding", context)?;
    put(
        &state,
        "defaultEncoding",
        if default_encoding.is_null_or_undefined() {
            js_text("utf8")
        } else {
            default_encoding
        },
        context,
    )?;
    put(&state, "awaitDrainWriters", JsValue::null(), context)?;
    put(&state, "decoder", JsValue::null(), context)?;
    put(&state, "encoding", JsValue::null(), context)?;
    let encoding = get(options, "encoding", context)?;
    if !encoding.is_null_or_undefined() {
        let decoder = crate::string_decoder::new_decoder(context, &encoding)?;
        let normalized = get(&decoder, "encoding", context)?;
        put(&state, "decoder", decoder, context)?;
        put(&state, "encoding", normalized, context)?;
    }
    Ok(state)
}

/// The prototype the module gives a stream's state objects: `Readable.ReadableState`'s and
/// `Writable.WritableState`'s.
fn state_prototype(context: &mut Context, name: &str) -> JsResult<JsObject> {
    let module = module_value(context)?;
    let owner = if name == "ReadableState" {
        "Readable"
    } else {
        "Writable"
    };
    let class = get(&get(&module, owner, context)?, name, context)?;
    get(&class, "prototype", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("{name} has no prototype")))
}

/// What `Readable`'s constructor does, on a stream made by `new` or handed to it by
/// `Readable.call(this, options)`.
fn readable_setup(
    target: &JsObject,
    options: &JsValue,
    is_duplex: bool,
    context: &mut Context,
) -> JsResult<()> {
    let state = readable_state(options, is_duplex, context)?;
    target.set(js_string!("_readableState"), state, false, context)?;
    if options.is_object() {
        for (option, method) in [
            ("read", "_read"),
            ("destroy", "_destroy"),
            ("construct", "_construct"),
        ] {
            let f = get(options, option, context)?;
            if f.is_callable() {
                target.set(JsString::from(method), f, false, context)?;
            }
        }
    }
    stream_setup(target, options, context)?;
    let signal = get(options, "signal", context)?;
    if signal.is_object() {
        add_abort_signal(&signal, &target.clone().into(), context)?;
    }
    Ok(())
}

/// What the legacy `Stream` constructor does: an `EventEmitter`'s set-up.
fn stream_setup(target: &JsObject, options: &JsValue, context: &mut Context) -> JsResult<()> {
    let emitter = crate::events::emitter_class(context)?;
    let init = emitter.get(js_string!("init"), context)?;
    crate::call(
        context,
        &init,
        &target.clone().into(),
        std::slice::from_ref(options),
    )?;
    Ok(())
}

fn readable_init(target: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let options = arg(args, 0);
    let duplex = class_prototype(context, "Duplex")?;
    let is_duplex = crate::native::is_instance(&target.clone().into(), &duplex);
    readable_setup(target, &options, is_duplex, context)?;
    construct_later(&target.clone().into(), context)
}

fn readable_add_chunk(
    stream: &JsValue,
    chunk: JsValue,
    encoding: JsValue,
    add_to_front: bool,
    context: &mut Context,
) -> JsResult<bool> {
    let state = rstate(stream, context)?
        .ok_or_else(|| crate::native::type_error("not a readable stream"))?;
    let object_mode = flag(&state, "objectMode", context)?;
    let mut chunk = chunk;
    let mut encoding = encoding;
    let mut error = JsValue::undefined();
    if !object_mode {
        if chunk.is_string() {
            let wanted = if encoding.is_null_or_undefined() || is_empty_text(&encoding) {
                sget(&state, "defaultEncoding", context)?
            } else {
                encoding.clone()
            };
            let state_encoding = sget(&state, "encoding", context)?;
            if !state_encoding.strict_equals(&wanted) {
                if add_to_front && !state_encoding.is_null_or_undefined() {
                    let bytes = to_buffer(&chunk, &wanted, context)?;
                    chunk = invoke(&bytes, "toString", &[state_encoding], context)?;
                    encoding = wanted;
                } else {
                    chunk = to_buffer(&chunk, &wanted, context)?;
                    encoding = js_text("");
                }
            } else {
                encoding = wanted;
            }
        } else if is_bytes(&chunk) {
            if !is_buffer(&chunk, context)? {
                chunk = to_buffer(&chunk, &JsValue::undefined(), context)?;
            }
            encoding = js_text("");
        } else if !chunk.is_null_or_undefined() {
            error = type_error_value(
                context,
                "The \"chunk\" argument must be of type string or an instance of Buffer, TypedArray, or DataView.",
                "ERR_INVALID_ARG_TYPE",
            );
        }
    }
    if !error.is_undefined() {
        error_or_destroy(stream, error, false, context)?;
    } else if chunk.is_null() {
        put(&state, "reading", false, context)?;
        on_eof_chunk(stream, &state, context)?;
    } else if object_mode || chunk_length(&chunk, false, context)? > 0. {
        if add_to_front {
            if flag(&state, "endEmitted", context)? {
                let error = error_value(
                    context,
                    "stream.unshift() after end event",
                    "ERR_STREAM_UNSHIFT_AFTER_END_EVENT",
                );
                error_or_destroy(stream, error, false, context)?;
            } else if flag(&state, "destroyed", context)?
                || !sget(&state, "errored", context)?.is_null_or_undefined()
            {
                return Ok(false);
            } else {
                add_chunk(stream, &state, chunk, true, context)?;
            }
        } else if flag(&state, "ended", context)? {
            let error = error_value(
                context,
                "stream.push() after EOF",
                "ERR_STREAM_PUSH_AFTER_EOF",
            );
            error_or_destroy(stream, error, false, context)?;
        } else if flag(&state, "destroyed", context)?
            || !sget(&state, "errored", context)?.is_null_or_undefined()
        {
            return Ok(false);
        } else {
            put(&state, "reading", false, context)?;
            let decoder = sget(&state, "decoder", context)?;
            if !decoder.is_null_or_undefined()
                && (encoding.is_null_or_undefined() || is_empty_text(&encoding))
            {
                chunk = invoke(&decoder, "write", &[chunk], context)?;
                if object_mode || chunk_length(&chunk, false, context)? != 0. {
                    add_chunk(stream, &state, chunk, false, context)?;
                } else {
                    maybe_read_more(stream, &state, context)?;
                }
            } else {
                add_chunk(stream, &state, chunk, false, context)?;
            }
        }
    } else if !add_to_front {
        put(&state, "reading", false, context)?;
        maybe_read_more(stream, &state, context)?;
    }
    let length = num(&state, "length", context)?;
    let hwm = num(&state, "highWaterMark", context)?;
    Ok(!flag(&state, "ended", context)? && (length < hwm || length == 0.))
}

fn add_chunk(
    stream: &JsValue,
    state: &JsObject,
    chunk: JsValue,
    add_to_front: bool,
    context: &mut Context,
) -> JsResult<()> {
    let object_mode = flag(state, "objectMode", context)?;
    if sget(state, "flowing", context)?.to_boolean()
        && num(state, "length", context)? == 0.
        && !flag(state, "sync", context)?
        && listener_count(stream, "data", context)? > 0.
    {
        clear_await_drain(state, context)?;
        put(state, "dataEmitted", true, context)?;
        emit(stream, &[js_text("data"), chunk], context)?;
    } else {
        let length = num(state, "length", context)? + chunk_length(&chunk, object_mode, context)?;
        put(state, "length", length, context)?;
        let buffer = sget(state, "buffer", context)?;
        invoke(
            &buffer,
            if add_to_front { "unshift" } else { "push" },
            &[chunk],
            context,
        )?;
        if flag(state, "needReadable", context)? {
            emit_readable(stream, state, context)?;
        }
    }
    maybe_read_more(stream, state, context)
}

fn clear_await_drain(state: &JsObject, context: &mut Context) -> JsResult<()> {
    if flag(state, "multiAwaitDrain", context)? {
        let writers = sget(state, "awaitDrainWriters", context)?;
        if writers.is_object() {
            invoke(&writers, "clear", &[], context)?;
        }
    } else {
        put(state, "awaitDrainWriters", JsValue::null(), context)?;
    }
    Ok(())
}

fn on_eof_chunk(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    if flag(state, "ended", context)? {
        return Ok(());
    }
    let decoder = sget(state, "decoder", context)?;
    if !decoder.is_null_or_undefined() {
        let rest = invoke(&decoder, "end", &[], context)?;
        let object_mode = flag(state, "objectMode", context)?;
        if chunk_length(&rest, false, context)? > 0. {
            invoke(
                &sget(state, "buffer", context)?,
                "push",
                std::slice::from_ref(&rest),
                context,
            )?;
            let length =
                num(state, "length", context)? + chunk_length(&rest, object_mode, context)?;
            put(state, "length", length, context)?;
        }
    }
    put(state, "ended", true, context)?;
    if flag(state, "sync", context)? {
        emit_readable(stream, state, context)
    } else {
        put(state, "needReadable", false, context)?;
        put(state, "emittedReadable", true, context)?;
        emit_readable_now(stream, context)
    }
}

fn emit_readable(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    put(state, "needReadable", false, context)?;
    if !flag(state, "emittedReadable", context)? {
        put(state, "emittedReadable", true, context)?;
        next_tick(
            context,
            |_, args, cx| {
                emit_readable_now(&arg(args, 0), cx)?;
                Ok(JsValue::undefined())
            },
            vec![stream.clone()],
        );
    }
    Ok(())
}

fn emit_readable_now(stream: &JsValue, context: &mut Context) -> JsResult<()> {
    let Some(state) = rstate(stream, context)? else {
        return Ok(());
    };
    let length = num(&state, "length", context)?;
    let ended = flag(&state, "ended", context)?;
    if !flag(&state, "destroyed", context)?
        && sget(&state, "errored", context)?.is_null_or_undefined()
        && (length > 0. || ended)
    {
        emit(stream, &[js_text("readable")], context)?;
        put(&state, "emittedReadable", false, context)?;
    }
    let flowing = sget(&state, "flowing", context)?.to_boolean();
    let length = num(&state, "length", context)?;
    let hwm = num(&state, "highWaterMark", context)?;
    let ended = flag(&state, "ended", context)?;
    put(
        &state,
        "needReadable",
        !flowing && !ended && length <= hwm,
        context,
    )?;
    flow(stream, &state, context)
}

fn maybe_read_more(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    if !flag(state, "readingMore", context)? && flag(state, "constructed", context)? {
        put(state, "readingMore", true, context)?;
        next_tick(
            context,
            |_, args, cx| {
                let stream = arg(args, 0);
                let Some(state) = rstate(&stream, cx)? else {
                    return Ok(JsValue::undefined());
                };
                loop {
                    let reading = flag(&state, "reading", cx)?;
                    let ended = flag(&state, "ended", cx)?;
                    let length = num(&state, "length", cx)?;
                    let hwm = num(&state, "highWaterMark", cx)?;
                    let flowing = sget(&state, "flowing", cx)?.to_boolean();
                    if reading || ended || !(length < hwm || (flowing && length == 0.)) {
                        break;
                    }
                    invoke(&stream, "read", &[0.into()], cx)?;
                    if length == num(&state, "length", cx)? {
                        break;
                    }
                }
                put(&state, "readingMore", false, cx)?;
                Ok(JsValue::undefined())
            },
            vec![stream.clone()],
        );
    }
    Ok(())
}

fn how_much_to_read(n: f64, state: &JsObject, context: &mut Context) -> JsResult<f64> {
    let length = num(state, "length", context)?;
    let ended = flag(state, "ended", context)?;
    if n <= 0. || (length == 0. && ended) {
        return Ok(0.);
    }
    if flag(state, "objectMode", context)? {
        return Ok(1.);
    }
    if n.is_nan() {
        if sget(state, "flowing", context)?.to_boolean() && length > 0. {
            let first = sget(state, "buffer", context)?;
            let first = get(&first, "0", context)?;
            return chunk_length(&first, false, context);
        }
        return Ok(length);
    }
    if n <= length {
        return Ok(n);
    }
    Ok(if ended { length } else { 0. })
}

/// Takes `n` from the buffer: whole chunks, and part of one when `n` ends inside it.
fn from_list(n: f64, state: &JsObject, context: &mut Context) -> JsResult<JsValue> {
    let length = num(state, "length", context)?;
    if length == 0. {
        return Ok(JsValue::null());
    }
    let buffer = sget(state, "buffer", context)?;
    if flag(state, "objectMode", context)? {
        return invoke(&buffer, "shift", &[], context);
    }
    let strings = !sget(state, "decoder", context)?.is_null_or_undefined();
    let chunks = list_of(&buffer, context)?;
    let all = n.is_nan() || n == 0. || n >= length;
    let mut taken = Vec::new();
    if all {
        taken = chunks;
        invoke(&buffer, "splice", &[0.into()], context)?;
    } else {
        let mut left = n;
        while left > 0. {
            let head = get(&buffer, "0", context)?;
            if head.is_undefined() {
                break;
            }
            let size = chunk_length(&head, false, context)?;
            if size <= left {
                taken.push(invoke(&buffer, "shift", &[], context)?);
                left -= size;
            } else {
                taken.push(call_method(
                    &head,
                    "slice",
                    &[0.into(), left.into()],
                    context,
                )?);
                let rest = call_method(&head, "slice", &[left.into()], context)?;
                if let Some(buffer) = buffer.as_object() {
                    buffer.set(0, rest, false, context)?;
                }
                left = 0.;
            }
        }
    }
    join_chunks(taken, strings, context)
}

fn join_chunks(chunks: Vec<JsValue>, strings: bool, context: &mut Context) -> JsResult<JsValue> {
    if chunks.len() == 1 {
        return Ok(chunks.into_iter().next().unwrap_or_default());
    }
    if strings || chunks.iter().all(JsValue::is_string) {
        let mut text = String::new();
        for chunk in &chunks {
            text.push_str(&text_of(chunk, context)?);
        }
        return Ok(js_text(&text));
    }
    let list = array(context, chunks);
    let class = buffer_class(context)?;
    invoke(&class, "concat", &[list], context)
}

fn end_readable(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    if !flag(state, "endEmitted", context)? {
        put(state, "ended", true, context)?;
        next_tick(
            context,
            |_, args, cx| {
                let stream = arg(args, 0);
                let Some(state) = rstate(&stream, cx)? else {
                    return Ok(JsValue::undefined());
                };
                if sget(&state, "errored", cx)?.is_null_or_undefined()
                    && !flag(&state, "closeEmitted", cx)?
                    && !flag(&state, "endEmitted", cx)?
                    && num(&state, "length", cx)? == 0.
                {
                    put(&state, "endEmitted", true, cx)?;
                    emit(&stream, &[js_text("end")], cx)?;
                    let w = wstate(&stream, cx)?;
                    let auto = match &w {
                        None => true,
                        Some(w) => {
                            flag(w, "autoDestroy", cx)?
                                && (flag(w, "finished", cx)?
                                    || sget(w, "writable", cx)?.strict_equals(&false.into()))
                        }
                    };
                    if flag(&state, "autoDestroy", cx)? && auto {
                        invoke(&stream, "destroy", &[], cx)?;
                    }
                }
                Ok(JsValue::undefined())
            },
            vec![stream.clone()],
        );
    }
    Ok(())
}

fn flow(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    while sget(state, "flowing", context)?.to_boolean() {
        if invoke(stream, "read", &[], context)?.is_null() {
            break;
        }
    }
    Ok(())
}

/// `read(n)`: Node's algorithm — asks `_read` for more when the buffer is under the high-water
/// mark, and answers what it can (all of it, when no `n`).
fn read(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state =
        rstate(this, context)?.ok_or_else(|| crate::native::type_error("not a readable stream"))?;
    let given = arg(args, 0);
    let mut n = if given.is_undefined() {
        f64::NAN
    } else {
        let v = given.to_number(context)?;
        if v.is_finite() { v.trunc() } else { v }
    };
    let original = n;
    let mut hwm = num(&state, "highWaterMark", context)?;
    if n > hwm {
        // Raised to the next power of two, as Node does.
        let mut next = 1.;
        while next < n {
            next *= 2.;
        }
        hwm = next;
        put(&state, "highWaterMark", hwm, context)?;
    }
    if n != 0. {
        put(&state, "emittedReadable", false, context)?;
    }
    let length = num(&state, "length", context)?;
    let ended = flag(&state, "ended", context)?;
    if n == 0.
        && flag(&state, "needReadable", context)?
        && ((if hwm != 0. {
            length >= hwm
        } else {
            length > 0.
        }) || ended)
    {
        if length == 0. && ended {
            end_readable(this, &state, context)?;
        } else {
            emit_readable(this, &state, context)?;
        }
        return Ok(JsValue::null());
    }
    n = how_much_to_read(n, &state, context)?;
    if n == 0. && ended {
        if num(&state, "length", context)? == 0. {
            end_readable(this, &state, context)?;
        }
        return Ok(JsValue::null());
    }
    let mut do_read = flag(&state, "needReadable", context)?;
    let length = num(&state, "length", context)?;
    if length == 0. || length - n < hwm {
        do_read = true;
    }
    if ended
        || flag(&state, "reading", context)?
        || flag(&state, "destroyed", context)?
        || !sget(&state, "errored", context)?.is_null_or_undefined()
        || !flag(&state, "constructed", context)?
    {
        do_read = false;
    } else if do_read {
        put(&state, "reading", true, context)?;
        put(&state, "sync", true, context)?;
        if length == 0. {
            put(&state, "needReadable", true, context)?;
        }
        if let Err(error) = invoke(this, "_read", &[hwm.into()], context) {
            let error = error.into_opaque(context)?;
            error_or_destroy(this, error, false, context)?;
        }
        put(&state, "sync", false, context)?;
        if !flag(&state, "reading", context)? {
            n = how_much_to_read(original, &state, context)?;
        }
    }
    let _ = do_read;
    let mut out = if n > 0. {
        from_list(n, &state, context)?
    } else {
        JsValue::null()
    };
    if out.is_null() {
        let length = num(&state, "length", context)?;
        put(&state, "needReadable", length <= hwm, context)?;
        n = 0.;
    } else {
        let length = num(&state, "length", context)? - n;
        put(&state, "length", length, context)?;
        clear_await_drain(&state, context)?;
    }
    if num(&state, "length", context)? == 0. {
        if !flag(&state, "ended", context)? {
            put(&state, "needReadable", true, context)?;
        }
        if original != n && flag(&state, "ended", context)? {
            end_readable(this, &state, context)?;
        }
    }
    if !out.is_null()
        && !flag(&state, "errorEmitted", context)?
        && !flag(&state, "closeEmitted", context)?
    {
        put(&state, "dataEmitted", true, context)?;
        emit(this, &[js_text("data"), out.clone()], context)?;
    }
    if out.is_undefined() {
        out = JsValue::null();
    }
    Ok(out)
}

fn resume(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(state) = rstate(this, context)? else {
        return Ok(this.clone());
    };
    if !sget(&state, "flowing", context)?.to_boolean() {
        // Flowing — unless someone listens for `readable`, which keeps it paused.
        let listening = flag(&state, "readableListening", context)?;
        put(&state, "flowing", !listening, context)?;
        if !flag(&state, "resumeScheduled", context)? {
            put(&state, "resumeScheduled", true, context)?;
            next_tick(
                context,
                |_, args, cx| {
                    let stream = arg(args, 0);
                    let Some(state) = rstate(&stream, cx)? else {
                        return Ok(JsValue::undefined());
                    };
                    if !flag(&state, "reading", cx)? {
                        invoke(&stream, "read", &[0.into()], cx)?;
                    }
                    put(&state, "resumeScheduled", false, cx)?;
                    emit(&stream, &[js_text("resume")], cx)?;
                    flow(&stream, &state, cx)?;
                    if sget(&state, "flowing", cx)?.to_boolean() && !flag(&state, "reading", cx)? {
                        invoke(&stream, "read", &[0.into()], cx)?;
                    }
                    Ok(JsValue::undefined())
                },
                vec![this.clone()],
            );
        }
    }
    put(&state, "paused", false, context)?;
    Ok(this.clone())
}

fn pause(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(state) = rstate(this, context)? else {
        return Ok(this.clone());
    };
    if !sget(&state, "flowing", context)?.strict_equals(&false.into()) {
        put(&state, "flowing", false, context)?;
        emit(this, &[js_text("pause")], context)?;
    }
    put(&state, "paused", true, context)?;
    Ok(this.clone())
}

/// `on`, as a readable stream has it: a `data` listener sets it flowing, a `readable` listener
/// pauses it and has it say when there is something to read.
fn readable_on(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = arg(args, 0);
    let out = crate::events::add_listener(this, &name, &arg(args, 1), false, context)?;
    let Some(state) = rstate(this, context)? else {
        return Ok(out);
    };
    let name = name
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    if name == "data" {
        let listening = listener_count(this, "readable", context)? > 0.;
        put(&state, "readableListening", listening, context)?;
        if !sget(&state, "flowing", context)?.strict_equals(&false.into()) {
            invoke(this, "resume", &[], context)?;
        }
    } else if name == "readable"
        && !flag(&state, "endEmitted", context)?
        && !flag(&state, "readableListening", context)?
    {
        put(&state, "readableListening", true, context)?;
        put(&state, "needReadable", true, context)?;
        put(&state, "flowing", false, context)?;
        put(&state, "emittedReadable", false, context)?;
        if num(&state, "length", context)? > 0. {
            emit_readable(this, &state, context)?;
        } else if !flag(&state, "reading", context)? {
            next_tick(
                context,
                |_, args, cx| invoke(&arg(args, 0), "read", &[0.into()], cx),
                vec![this.clone()],
            );
        }
    }
    Ok(out)
}

fn readable_remove_listener(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = arg(args, 0);
    let out = crate::events::remove_listener(this, &name, &arg(args, 1), context)?;
    if name
        .as_string()
        .is_some_and(|n| n.to_std_string_escaped() == "readable")
    {
        next_tick(context, update_readable_listening, vec![this.clone()]);
    }
    Ok(out)
}

fn update_readable_listening(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let stream = arg(args, 0);
    let Some(state) = rstate(&stream, context)? else {
        return Ok(JsValue::undefined());
    };
    let listening = listener_count(&stream, "readable", context)? > 0.;
    put(&state, "readableListening", listening, context)?;
    if flag(&state, "resumeScheduled", context)?
        && sget(&state, "paused", context)?.strict_equals(&false.into())
    {
        put(&state, "flowing", true, context)?;
    } else if listener_count(&stream, "data", context)? > 0. {
        invoke(&stream, "resume", &[], context)?;
    } else if !listening {
        put(&state, "flowing", JsValue::null(), context)?;
    }
    Ok(JsValue::undefined())
}

fn set_encoding(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(state) = rstate(this, context)? else {
        return Ok(this.clone());
    };
    let decoder = crate::string_decoder::new_decoder(context, &arg(args, 0))?;
    let encoding = get(&decoder, "encoding", context)?;
    put(&state, "decoder", decoder.clone(), context)?;
    put(&state, "encoding", encoding, context)?;
    // What is buffered already is text from now on.
    let buffer = sget(&state, "buffer", context)?;
    let mut text = String::new();
    for chunk in list_of(&buffer, context)? {
        let piece = invoke(&decoder, "write", &[chunk], context)?;
        text.push_str(&text_of(&piece, context)?);
    }
    invoke(&buffer, "splice", &[0.into()], context)?;
    if !text.is_empty() {
        invoke(&buffer, "push", &[js_text(&text)], context)?;
    }
    put(
        &state,
        "length",
        js_text(&text).as_string().map_or(0, |s| s.len()),
        context,
    )?;
    Ok(this.clone())
}

/// `pipe(destination, { end })`: every chunk written to the destination, paused while it asks
/// for a `drain`, and ended with this stream unless `end` is `false`.
fn readable_pipe(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let dest = arg(args, 0);
    let options = arg(args, 1);
    let state =
        rstate(this, context)?.ok_or_else(|| crate::native::type_error("not a readable stream"))?;
    let pipes = sget(&state, "pipes", context)?;
    if get(&pipes, "length", context)?.to_number(context)? == 1.
        && !flag(&state, "multiAwaitDrain", context)?
    {
        put(&state, "multiAwaitDrain", true, context)?;
        let writers = sget(&state, "awaitDrainWriters", context)?;
        let items = if writers.is_null_or_undefined() {
            Vec::new()
        } else {
            vec![writers]
        };
        let list = array(context, items);
        let set = crate::native::construct_global(context, "Set", &[list])?;
        put(&state, "awaitDrainWriters", set, context)?;
    }
    invoke(&pipes, "push", std::slice::from_ref(&dest), context)?;

    let process = crate::native::global(context, "process")?;
    let stdout = get(&process, "stdout", context)?;
    let stderr = get(&process, "stderr", context)?;
    let do_end = !get(&options, "end", context)?.strict_equals(&false.into())
        && dest != stdout
        && dest != stderr;

    let ps = object(context);
    put(&ps, "src", this.clone(), context)?;
    put(&ps, "dest", dest.clone(), context)?;
    put(&ps, "cleanedUp", false, context)?;
    let handlers: [(&str, usize, Handler); 7] = [
        ("onend", 0, |_, _, ps, cx| {
            let dest = sget(ps, "dest", cx)?;
            invoke(&dest, "end", &[], cx)
        }),
        ("unpipe", 0, |_, _, ps, cx| {
            let (src, dest) = (sget(ps, "src", cx)?, sget(ps, "dest", cx)?);
            invoke(&src, "unpipe", &[dest], cx)
        }),
        ("ondata", 1, |_, args, ps, cx| {
            let dest = sget(ps, "dest", cx)?;
            let written = invoke(&dest, "write", &[arg(args, 0)], cx)?;
            if written.strict_equals(&false.into()) {
                pipe_pause(ps, cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("onerror", 1, |_, args, ps, cx| {
            let error = arg(args, 0);
            let dest = sget(ps, "dest", cx)?;
            let unpipe = sget(ps, "unpipe", cx)?;
            crate::call(cx, &unpipe, &JsValue::undefined(), &[])?;
            let onerror = sget(ps, "onerror", cx)?;
            invoke(&dest, "removeListener", &[js_text("error"), onerror], cx)?;
            if listener_count(&dest, "error", cx)? == 0. {
                let s = match wstate(&dest, cx)? {
                    Some(w) => Some(w),
                    None => rstate(&dest, cx)?,
                };
                match s {
                    Some(s) if !flag(&s, "errorEmitted", cx)? => {
                        error_or_destroy(&dest, error, false, cx)?
                    }
                    _ => {
                        emit(&dest, &[js_text("error"), error], cx)?;
                    }
                }
            }
            Ok(JsValue::undefined())
        }),
        ("onclose", 0, |_, _, ps, cx| {
            let dest = sget(ps, "dest", cx)?;
            let onfinish = sget(ps, "onfinish", cx)?;
            invoke(&dest, "removeListener", &[js_text("finish"), onfinish], cx)?;
            let unpipe = sget(ps, "unpipe", cx)?;
            crate::call(cx, &unpipe, &JsValue::undefined(), &[])
        }),
        ("onfinish", 0, |_, _, ps, cx| {
            let dest = sget(ps, "dest", cx)?;
            let onclose = sget(ps, "onclose", cx)?;
            invoke(&dest, "removeListener", &[js_text("close"), onclose], cx)?;
            let unpipe = sget(ps, "unpipe", cx)?;
            crate::call(cx, &unpipe, &JsValue::undefined(), &[])
        }),
        ("onunpipe", 2, |_, args, ps, cx| {
            let (readable, info) = (arg(args, 0), arg(args, 1));
            if readable == sget(ps, "src", cx)?
                && let Some(info) = info.as_object()
                && info
                    .get(js_string!("hasUnpiped"), cx)?
                    .strict_equals(&false.into())
            {
                info.set(js_string!("hasUnpiped"), true, false, cx)?;
                pipe_cleanup(ps, cx)?;
            }
            Ok(JsValue::undefined())
        }),
    ];
    for (name, length, f) in handlers {
        let handler = closure(context, name, length, f, ps.clone());
        put(&ps, name, handler, context)?;
    }
    let end_handler = sget(&ps, if do_end { "onend" } else { "unpipe" }, context)?;
    put(&ps, "endFn", end_handler.clone(), context)?;
    if flag(&state, "endEmitted", context)? {
        crate::microtask(context, end_handler, Vec::new());
    } else {
        invoke(this, "once", &[js_text("end"), end_handler], context)?;
    }
    invoke(
        &dest,
        "on",
        &[js_text("unpipe"), sget(&ps, "onunpipe", context)?],
        context,
    )?;
    invoke(
        this,
        "on",
        &[js_text("data"), sget(&ps, "ondata", context)?],
        context,
    )?;
    let onerror = sget(&ps, "onerror", context)?;
    if get(&dest, "prependListener", context)?.is_callable() {
        invoke(
            &dest,
            "prependListener",
            &[js_text("error"), onerror],
            context,
        )?;
    } else {
        invoke(&dest, "on", &[js_text("error"), onerror], context)?;
    }
    invoke(
        &dest,
        "once",
        &[js_text("close"), sget(&ps, "onclose", context)?],
        context,
    )?;
    invoke(
        &dest,
        "once",
        &[js_text("finish"), sget(&ps, "onfinish", context)?],
        context,
    )?;
    emit(&dest, &[js_text("pipe"), this.clone()], context)?;
    if get(&dest, "writableNeedDrain", context)?.strict_equals(&true.into()) {
        pipe_pause(&ps, context)?;
    } else if !sget(&state, "flowing", context)?.to_boolean() {
        invoke(this, "resume", &[], context)?;
    }
    Ok(dest)
}

/// The destination asked for a `drain`: the source pauses until it has one.
fn pipe_pause(ps: &JsObject, context: &mut Context) -> JsResult<()> {
    let (src, dest) = (sget(ps, "src", context)?, sget(ps, "dest", context)?);
    if !flag(ps, "cleanedUp", context)?
        && let Some(state) = rstate(&src, context)?
    {
        let pipes = list_of(&sget(&state, "pipes", context)?, context)?;
        if pipes.len() == 1 && pipes[0] == dest {
            put(&state, "awaitDrainWriters", dest.clone(), context)?;
            put(&state, "multiAwaitDrain", false, context)?;
        } else if pipes.len() > 1 && pipes.contains(&dest) {
            let writers = sget(&state, "awaitDrainWriters", context)?;
            if writers.is_object() {
                invoke(&writers, "add", std::slice::from_ref(&dest), context)?;
            }
        }
        invoke(&src, "pause", &[], context)?;
    }
    if sget(ps, "ondrain", context)?.is_undefined() {
        let ondrain = closure(
            context,
            "ondrain",
            0,
            |_, _, ps, cx| {
                let (src, dest) = (sget(ps, "src", cx)?, sget(ps, "dest", cx)?);
                let Some(state) = rstate(&src, cx)? else {
                    return Ok(JsValue::undefined());
                };
                let writers = sget(&state, "awaitDrainWriters", cx)?;
                if writers == dest {
                    put(&state, "awaitDrainWriters", JsValue::null(), cx)?;
                } else if flag(&state, "multiAwaitDrain", cx)? && writers.is_object() {
                    invoke(&writers, "delete", &[dest], cx)?;
                }
                let writers = sget(&state, "awaitDrainWriters", cx)?;
                let empty = writers.is_null_or_undefined()
                    || (flag(&state, "multiAwaitDrain", cx)?
                        && get(&writers, "size", cx)?.to_number(cx)? == 0.);
                if empty && listener_count(&src, "data", cx)? > 0. {
                    invoke(&src, "resume", &[], cx)?;
                }
                Ok(JsValue::undefined())
            },
            ps.clone(),
        );
        put(ps, "ondrain", ondrain.clone(), context)?;
        invoke(&dest, "on", &[js_text("drain"), ondrain.into()], context)?;
    }
    Ok(())
}

fn pipe_cleanup(ps: &JsObject, context: &mut Context) -> JsResult<()> {
    let (src, dest) = (sget(ps, "src", context)?, sget(ps, "dest", context)?);
    for (target, event, handler) in [
        (&dest, "close", "onclose"),
        (&dest, "finish", "onfinish"),
        (&dest, "drain", "ondrain"),
        (&dest, "error", "onerror"),
        (&dest, "unpipe", "onunpipe"),
        (&src, "end", "onend"),
        (&src, "end", "unpipe"),
        (&src, "data", "ondata"),
    ] {
        let handler = sget(ps, handler, context)?;
        if handler.is_callable() {
            invoke(
                target,
                "removeListener",
                &[js_text(event), handler],
                context,
            )?;
        }
    }
    put(ps, "cleanedUp", true, context)?;
    let ondrain = sget(ps, "ondrain", context)?;
    if ondrain.is_callable()
        && let Some(state) = rstate(&src, context)?
        && !sget(&state, "awaitDrainWriters", context)?.is_null_or_undefined()
    {
        let needs = match wstate(&dest, context)? {
            Some(w) => flag(&w, "needDrain", context)?,
            None => true,
        };
        if needs {
            crate::call(context, &ondrain, &JsValue::undefined(), &[])?;
        }
    }
    Ok(())
}

fn readable_unpipe(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(state) = rstate(this, context)? else {
        return Ok(this.clone());
    };
    let dest = arg(args, 0);
    let pipes = sget(&state, "pipes", context)?;
    let list = list_of(&pipes, context)?;
    if list.is_empty() {
        return Ok(this.clone());
    }
    if dest.is_null_or_undefined() {
        invoke(&pipes, "splice", &[0.into()], context)?;
        invoke(this, "pause", &[], context)?;
        for dest in list {
            let info = object(context);
            put(&info, "hasUnpiped", false, context)?;
            emit(
                &dest,
                &[js_text("unpipe"), this.clone(), info.into()],
                context,
            )?;
        }
        return Ok(this.clone());
    }
    let Some(at) = list.iter().position(|d| *d == dest) else {
        return Ok(this.clone());
    };
    invoke(&pipes, "splice", &[(at as f64).into(), 1.into()], context)?;
    if list.len() == 1 {
        invoke(this, "pause", &[], context)?;
    }
    let info = object(context);
    put(&info, "hasUnpiped", false, context)?;
    emit(
        &dest,
        &[js_text("unpipe"), this.clone(), info.into()],
        context,
    )?;
    Ok(this.clone())
}

/// `wrap(oldStream)`: an old-style stream — one that only emits `data` and `end` — read as
/// this one.
fn readable_wrap(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let old = arg(args, 0);
    let pair = array(context, vec![this.clone(), old.clone()]);
    let on_data = closure(
        context,
        "ondata",
        1,
        |_, args, pair, cx| {
            let pair = list_of(pair, cx)?;
            let pushed = invoke(&pair[0], "push", &[arg(args, 0)], cx)?;
            if !pushed.to_boolean() && get(&pair[1], "pause", cx)?.is_callable() {
                invoke(&pair[1], "pause", &[], cx)?;
            }
            Ok(JsValue::undefined())
        },
        pair.clone(),
    );
    let on_end = closure(
        context,
        "onend",
        0,
        |_, _, pair, cx| {
            let pair = list_of(pair, cx)?;
            invoke(&pair[0], "push", &[JsValue::null()], cx)
        },
        pair.clone(),
    );
    let on_error = closure(
        context,
        "onerror",
        1,
        |_, args, pair, cx| {
            let pair = list_of(pair, cx)?;
            error_or_destroy(&pair[0], arg(args, 0), false, cx)?;
            Ok(JsValue::undefined())
        },
        pair.clone(),
    );
    let on_close = closure(
        context,
        "onclose",
        0,
        |_, _, pair, cx| {
            let pair = list_of(pair, cx)?;
            invoke(&pair[0], "destroy", &[], cx)
        },
        pair.clone(),
    );
    invoke(&old, "on", &[js_text("data"), on_data.into()], context)?;
    invoke(&old, "on", &[js_text("end"), on_end.into()], context)?;
    invoke(&old, "on", &[js_text("error"), on_error.into()], context)?;
    invoke(&old, "on", &[js_text("close"), on_close.into()], context)?;
    let read = closure(
        context,
        "_read",
        0,
        |_, _, pair, cx| {
            let pair = list_of(pair, cx)?;
            if get(&pair[1], "resume", cx)?.is_callable() {
                invoke(&pair[1], "resume", &[], cx)?;
            }
            Ok(JsValue::undefined())
        },
        pair,
    );
    if let Some(target) = this.as_object() {
        target.set(js_string!("_read"), read, false, context)?;
    }
    Ok(this.clone())
}

// ─── Reading as an async iterator ────────────────────────────────────────────────────────────

/// `readable[Symbol.asyncIterator]()`: chunks as they can be read; done at `end`; rejected by
/// an error. Leaving the loop early destroys the stream, as Node's does.
fn async_iterator(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let options = arg(args, 0);
    let state = object(context);
    put(&state, "stream", this.clone(), context)?;
    put(&state, "pending", JsValue::null(), context)?;
    put(&state, "error", JsValue::null(), context)?;
    put(&state, "done", false, context)?;
    put(
        &state,
        "destroyOnReturn",
        !get(&options, "destroyOnReturn", context)?.strict_equals(&false.into()),
        context,
    )?;
    let wake = closure(
        context,
        "next",
        0,
        |_, _, state, cx| {
            iterator_fulfil(state, cx)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    let on_end = closure(
        context,
        "onend",
        0,
        |_, _, state, cx| {
            put(state, "done", true, cx)?;
            iterator_fulfil(state, cx)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    let on_error = closure(
        context,
        "onerror",
        1,
        |_, args, state, cx| {
            put(state, "error", arg(args, 0), cx)?;
            iterator_fulfil(state, cx)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    let on_close = closure(
        context,
        "onclose",
        0,
        |_, _, state, cx| {
            let stream = sget(state, "stream", cx)?;
            // Closed before its end: premature, unless it ended or failed already.
            if let Some(r) = rstate(&stream, cx)?
                && !flag(&r, "endEmitted", cx)?
                && sget(state, "error", cx)?.is_null()
                && !flag(state, "done", cx)?
            {
                let errored = sget(&r, "errored", cx)?;
                let error = if errored.is_null_or_undefined() {
                    error_value(cx, "Premature close", "ERR_STREAM_PREMATURE_CLOSE")
                } else {
                    errored
                };
                put(state, "error", error, cx)?;
            }
            put(state, "done", true, cx)?;
            iterator_fulfil(state, cx)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    put(
        &state,
        "listeners",
        array(
            context,
            vec![
                wake.clone().into(),
                on_end.clone().into(),
                on_error.clone().into(),
                on_close.clone().into(),
            ],
        ),
        context,
    )?;
    invoke(this, "on", &[js_text("readable"), wake.into()], context)?;
    invoke(this, "on", &[js_text("end"), on_end.into()], context)?;
    invoke(this, "on", &[js_text("error"), on_error.into()], context)?;
    invoke(this, "on", &[js_text("close"), on_close.into()], context)?;

    let iterator = object(context);
    let next = closure(
        context,
        "next",
        0,
        |_, _, state, cx| {
            let (promise, resolvers) = JsPromise::new_pending(cx);
            let pending = array(cx, vec![resolvers.resolve.into(), resolvers.reject.into()]);
            put(state, "pending", pending, cx)?;
            iterator_fulfil(state, cx)?;
            Ok(promise.into())
        },
        state.clone(),
    );
    let finish = closure(
        context,
        "return",
        0,
        |_, _, state, cx| {
            let stream = sget(state, "stream", cx)?;
            iterator_detach(state, cx)?;
            put(state, "done", true, cx)?;
            let ended = match rstate(&stream, cx)? {
                Some(r) => flag(&r, "endEmitted", cx)?,
                None => true,
            };
            if flag(state, "destroyOnReturn", cx)? && !ended {
                invoke(&stream, "destroy", &[], cx)?;
            }
            let result = object(cx);
            put(&result, "value", JsValue::undefined(), cx)?;
            put(&result, "done", true, cx)?;
            Ok(JsPromise::resolve(result, cx)?.into())
        },
        state.clone(),
    );
    let itself = function(context, "[Symbol.asyncIterator]", 0, |this, _, _| {
        Ok(this.clone())
    });
    hidden(&iterator, js_string!("next"), next, context)?;
    hidden(&iterator, js_string!("return"), finish, context)?;
    hidden(&iterator, JsSymbol::async_iterator(), itself, context)?;
    Ok(iterator.into())
}

fn iterator_detach(state: &JsObject, context: &mut Context) -> JsResult<()> {
    let stream = sget(state, "stream", context)?;
    let listeners = list_of(&sget(state, "listeners", context)?, context)?;
    for (event, listener) in ["readable", "end", "error", "close"].iter().zip(listeners) {
        invoke(
            &stream,
            "removeListener",
            &[js_text(event), listener],
            context,
        )?;
    }
    Ok(())
}

/// Answers a waiting `next()` if it can be: a chunk read now, the error, or done.
fn iterator_fulfil(state: &JsObject, context: &mut Context) -> JsResult<()> {
    let pending = sget(state, "pending", context)?;
    if pending.is_null() {
        return Ok(());
    }
    let stream = sget(state, "stream", context)?;
    let destroyed = rstate(&stream, context)?
        .map(|r| flag(&r, "destroyed", context))
        .transpose()?
        .unwrap_or(true);
    let chunk = if destroyed {
        JsValue::null()
    } else {
        invoke(&stream, "read", &[], context)?
    };
    let pair = list_of(&pending, context)?;
    if !chunk.is_null() {
        put(state, "pending", JsValue::null(), context)?;
        let result = object(context);
        put(&result, "value", chunk, context)?;
        put(&result, "done", false, context)?;
        crate::call(context, &pair[0], &JsValue::undefined(), &[result.into()])?;
        return Ok(());
    }
    let error = sget(state, "error", context)?;
    if !error.is_null() {
        put(state, "pending", JsValue::null(), context)?;
        put(state, "error", JsValue::null(), context)?;
        put(state, "done", true, context)?;
        iterator_detach(state, context)?;
        crate::call(context, &pair[1], &JsValue::undefined(), &[error])?;
        return Ok(());
    }
    if flag(state, "done", context)? || destroyed {
        put(state, "pending", JsValue::null(), context)?;
        iterator_detach(state, context)?;
        let result = object(context);
        put(&result, "value", JsValue::undefined(), context)?;
        put(&result, "done", true, context)?;
        crate::call(context, &pair[0], &JsValue::undefined(), &[result.into()])?;
    }
    Ok(())
}

/// `readable.toArray()`: a promise of every chunk.
fn to_array(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    collect(this, JsValue::undefined(), context)
}

/// `readable.forEach(fn)`: `fn` for every chunk; a promise of the end.
fn for_each(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let f = arg(args, 0);
    if !f.is_callable() {
        return Err(coded_type_error(
            context,
            "The \"fn\" argument must be of type function.",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    collect(this, f, context)
}

fn collect(this: &JsValue, each: JsValue, context: &mut Context) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let state = object(context);
    put(&state, "chunks", JsArray::new(context)?, context)?;
    put(&state, "each", each, context)?;
    put(&state, "resolve", resolvers.resolve.clone(), context)?;
    put(&state, "reject", resolvers.reject.clone(), context)?;
    put(&state, "stream", this.clone(), context)?;
    let on_data = closure(
        context,
        "ondata",
        1,
        |_, args, state, cx| {
            let each = sget(state, "each", cx)?;
            if each.is_callable() {
                crate::call(cx, &each, &JsValue::undefined(), &[arg(args, 0)])?;
            } else {
                invoke(&sget(state, "chunks", cx)?, "push", &[arg(args, 0)], cx)?;
            }
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    let done = closure(
        context,
        "done",
        1,
        |_, args, state, cx| {
            let error = arg(args, 0);
            if error.is_null_or_undefined() {
                let resolve = sget(state, "resolve", cx)?;
                let value = if sget(state, "each", cx)?.is_callable() {
                    JsValue::undefined()
                } else {
                    sget(state, "chunks", cx)?
                };
                crate::call(cx, &resolve, &JsValue::undefined(), &[value])
            } else {
                let reject = sget(state, "reject", cx)?;
                crate::call(cx, &reject, &JsValue::undefined(), &[error])
            }
        },
        state,
    );
    eos(this, &JsValue::undefined(), done.into(), context)?;
    invoke(this, "on", &[js_text("data"), on_data.into()], context)?;
    Ok(promise.into())
}

// ─── Readable.from ───────────────────────────────────────────────────────────────────────────

/// `Readable.from(iterable, options)`: a stream of an iterable's or an async iterable's items
/// (objects, by default), read one `next()` at a time as the stream asks; a string or a buffer
/// is one chunk.
fn readable_from(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (iterable, options) = (arg(args, 0), arg(args, 1));
    let readable_class = module_class(context, "Readable")?;
    let settings = object(context);
    put(&settings, "objectMode", true, context)?;
    put(&settings, "highWaterMark", 1, context)?;
    if let Some(given) = options.as_object() {
        for key in given.own_property_keys(context)? {
            let value = given.get(key.clone(), context)?;
            settings.set(key, value, false, context)?;
        }
    }
    if iterable.is_string() || is_bytes(&iterable) {
        let stream = readable_class.construct(&[settings.into()], None, context)?;
        let read = closure(
            context,
            "read",
            0,
            |this, _, chunk, cx| {
                invoke(this, "push", std::slice::from_ref(chunk), cx)?;
                invoke(this, "push", &[JsValue::null()], cx)
            },
            iterable,
        );
        stream.set(js_string!("_read"), read, false, context)?;
        return Ok(stream.into());
    }
    let async_method = iterable
        .to_object(context)
        .and_then(|o| o.get(JsSymbol::async_iterator(), context))
        .unwrap_or_default();
    let iterator = if async_method.is_callable() {
        crate::call(context, &async_method, &iterable, &[])?
    } else {
        let method = iterable
            .to_object(context)
            .and_then(|o| o.get(JsSymbol::iterator(), context))
            .unwrap_or_default();
        if !method.is_callable() {
            return Err(coded_type_error(
                context,
                "The \"iterable\" argument must be an instance of Iterable.",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        crate::call(context, &method, &iterable, &[])?
    };
    let stream = readable_class.construct(&[settings.into()], None, context)?;
    let state = object(context);
    put(&state, "iterator", iterator, context)?;
    put(&state, "reading", false, context)?;
    put(&state, "stream", stream.clone(), context)?;
    let read = closure(
        context,
        "read",
        0,
        |_, _, state, cx| {
            if !flag(state, "reading", cx)? {
                put(state, "reading", true, cx)?;
                from_next(state, cx)?;
            }
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    let destroy = closure(
        context,
        "destroy",
        2,
        |_, args, state, cx| {
            let iterator = sget(state, "iterator", cx)?;
            if get(&iterator, "return", cx)?.is_callable() {
                let _ = invoke(&iterator, "return", &[], cx);
            }
            crate::call(cx, &arg(args, 1), &JsValue::undefined(), &[arg(args, 0)])
        },
        state,
    );
    stream.set(js_string!("_read"), read, false, context)?;
    stream.set(js_string!("_destroy"), destroy, false, context)?;
    Ok(stream.into())
}

/// The iterator's next item, pushed when it comes; the next asked for while the stream wants
/// more.
fn from_next(state: &JsObject, context: &mut Context) -> JsResult<()> {
    let iterator = sget(state, "iterator", context)?;
    let stream = sget(state, "stream", context)?;
    let result = match invoke(&iterator, "next", &[], context) {
        Ok(result) => result,
        Err(error) => {
            let error = error.into_opaque(context)?;
            invoke(&stream, "destroy", &[error], context)?;
            return Ok(());
        }
    };
    let on_result = closure(
        context,
        "onResult",
        1,
        |_, args, state, cx| {
            let result = arg(args, 0);
            let stream = sget(state, "stream", cx)?;
            if get(&result, "done", cx)?.to_boolean() {
                invoke(&stream, "push", &[JsValue::null()], cx)?;
                return Ok(JsValue::undefined());
            }
            let value = get(&result, "value", cx)?;
            // A sync iterator's promise is waited for, as Node waits for it.
            let on_value = closure(
                cx,
                "onValue",
                1,
                |_, args, state, cx| {
                    let stream = sget(state, "stream", cx)?;
                    let value = arg(args, 0);
                    if value.is_null() {
                        let error = type_error_value(
                            cx,
                            "May not write null values to stream",
                            "ERR_STREAM_NULL_VALUES",
                        );
                        invoke(&stream, "destroy", &[error], cx)?;
                        return Ok(JsValue::undefined());
                    }
                    if invoke(&stream, "push", &[value], cx)?.to_boolean() {
                        from_next(state, cx)?;
                    } else {
                        put(state, "reading", false, cx)?;
                    }
                    Ok(JsValue::undefined())
                },
                state.clone(),
            );
            then(&value, on_value.into(), on_failure(state, cx), cx)?;
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    let on_error = on_failure(state, context);
    then(&result, on_result.into(), on_error, context)
}

fn on_failure(state: &JsObject, context: &mut Context) -> JsValue {
    closure(
        context,
        "onError",
        1,
        |_, args, state, cx| {
            let stream = sget(state, "stream", cx)?;
            invoke(&stream, "destroy", &[arg(args, 0)], cx)
        },
        state.clone(),
    )
    .into()
}

/// `Promise.resolve(value).then(fulfilled, rejected)`.
fn then(
    value: &JsValue,
    fulfilled: JsValue,
    rejected: JsValue,
    context: &mut Context,
) -> JsResult<()> {
    let promise = JsPromise::resolve(value.clone(), context)?;
    let promise: JsValue = promise.into();
    invoke(&promise, "then", &[fulfilled, rejected], context)?;
    Ok(())
}

// ─── Writable ────────────────────────────────────────────────────────────────────────────────

fn writable_state(
    stream: &JsObject,
    options: &JsValue,
    is_duplex: bool,
    context: &mut Context,
) -> JsResult<JsObject> {
    let state = object(context);
    if let Ok(prototype) = state_prototype(context, "WritableState") {
        state.set_prototype(Some(prototype));
    }
    let object_mode = get(options, "objectMode", context)?.to_boolean()
        || (is_duplex && get(options, "writableObjectMode", context)?.to_boolean());
    let hwm = high_water_mark(
        options,
        "writableHighWaterMark",
        is_duplex,
        object_mode,
        context,
    )?;
    put(&state, "objectMode", object_mode, context)?;
    put(&state, "highWaterMark", hwm, context)?;
    for key in [
        "finalCalled",
        "needDrain",
        "ending",
        "ended",
        "finished",
        "destroyed",
        "writing",
        "bufferProcessing",
        "prefinished",
        "errorEmitted",
        "closed",
        "closeEmitted",
    ] {
        put(&state, key, false, context)?;
    }
    put(
        &state,
        "decodeStrings",
        !get(options, "decodeStrings", context)?.strict_equals(&false.into()),
        context,
    )?;
    let default_encoding = get(options, "defaultEncoding", context)?;
    put(
        &state,
        "defaultEncoding",
        if default_encoding.is_null_or_undefined() {
            js_text("utf8")
        } else {
            default_encoding
        },
        context,
    )?;
    put(&state, "length", 0, context)?;
    put(&state, "corked", 0, context)?;
    put(&state, "sync", true, context)?;
    let onwrite = closure(
        context,
        "onwrite",
        1,
        |_, args, stream, cx| {
            on_write(&stream.clone().into(), arg(args, 0), cx)?;
            Ok(JsValue::undefined())
        },
        stream.clone(),
    );
    put(&state, "onwrite", onwrite, context)?;
    put(&state, "writecb", JsValue::null(), context)?;
    put(&state, "writelen", 0, context)?;
    put(&state, "afterWriteTickInfo", JsValue::null(), context)?;
    put(&state, "buffered", JsArray::new(context)?, context)?;
    put(&state, "bufferedIndex", 0, context)?;
    put(&state, "allBuffers", true, context)?;
    put(&state, "allNoop", true, context)?;
    put(&state, "pendingcb", 0, context)?;
    put(&state, "constructed", true, context)?;
    put(
        &state,
        "emitClose",
        !get(options, "emitClose", context)?.strict_equals(&false.into()),
        context,
    )?;
    put(
        &state,
        "autoDestroy",
        !get(options, "autoDestroy", context)?.strict_equals(&false.into()),
        context,
    )?;
    put(&state, "errored", JsValue::null(), context)?;
    hidden(
        &state,
        js_string!("__onFinished"),
        JsArray::new(context)?,
        context,
    )?;
    Ok(state)
}

fn writable_setup(
    target: &JsObject,
    options: &JsValue,
    is_duplex: bool,
    context: &mut Context,
) -> JsResult<()> {
    let state = writable_state(target, options, is_duplex, context)?;
    target.set(js_string!("_writableState"), state, false, context)?;
    if options.is_object() {
        for (option, method) in [
            ("write", "_write"),
            ("writev", "_writev"),
            ("destroy", "_destroy"),
            ("final", "_final"),
            ("construct", "_construct"),
        ] {
            let f = get(options, option, context)?;
            if f.is_callable() {
                target.set(JsString::from(method), f, false, context)?;
            }
        }
    }
    Ok(())
}

fn writable_init(target: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let options = arg(args, 0);
    writable_setup(target, &options, false, context)?;
    stream_setup(target, &options, context)?;
    let signal = get(&options, "signal", context)?;
    if signal.is_object() {
        add_abort_signal(&signal, &target.clone().into(), context)?;
    }
    construct_later(&target.clone().into(), context)
}

/// `write(chunk, encoding, callback)`: written at once when nothing is being written, kept in
/// order otherwise; answers whether the buffer is still under the high-water mark.
fn write(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state =
        wstate(this, context)?.ok_or_else(|| crate::native::type_error("not a writable stream"))?;
    let mut chunk = arg(args, 0);
    let (mut encoding, mut callback) = (arg(args, 1), arg(args, 2));
    if encoding.is_callable() {
        callback = encoding;
        encoding = JsValue::null();
    }
    if encoding.is_null_or_undefined() || is_empty_text(&encoding) {
        encoding = sget(&state, "defaultEncoding", context)?;
    }
    if !callback.is_callable() {
        callback = function(context, "nop", 0, nop()).into();
    }
    if chunk.is_null() {
        return Err(coded_type_error(
            context,
            "May not write null values to stream",
            "ERR_STREAM_NULL_VALUES",
        ));
    }
    if !flag(&state, "objectMode", context)? {
        if chunk.is_string() {
            if flag(&state, "decodeStrings", context)? {
                chunk = to_buffer(&chunk, &encoding, context)?;
                encoding = js_text("buffer");
            }
        } else if is_bytes(&chunk) {
            if !is_buffer(&chunk, context)? {
                chunk = to_buffer(&chunk, &JsValue::undefined(), context)?;
            }
            encoding = js_text("buffer");
        } else {
            return Err(coded_type_error(
                context,
                "The \"chunk\" argument must be of type string or an instance of Buffer, TypedArray, or DataView.",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
    }
    let error = if flag(&state, "ending", context)? {
        Some(error_value(
            context,
            "write after end",
            "ERR_STREAM_WRITE_AFTER_END",
        ))
    } else if flag(&state, "destroyed", context)? {
        Some(error_value(
            context,
            "Cannot call write after a stream was destroyed",
            "ERR_STREAM_DESTROYED",
        ))
    } else {
        None
    };
    if let Some(error) = error {
        crate::microtask(context, callback, vec![error.clone()]);
        error_or_destroy(this, error.clone(), true, context)?;
        return Ok(error);
    }
    let pending = num(&state, "pendingcb", context)? + 1.;
    put(&state, "pendingcb", pending, context)?;
    Ok(write_or_buffer(this, &state, chunk, encoding, callback, context)?.into())
}

fn write_or_buffer(
    stream: &JsValue,
    state: &JsObject,
    chunk: JsValue,
    encoding: JsValue,
    callback: JsValue,
    context: &mut Context,
) -> JsResult<bool> {
    let object_mode = flag(state, "objectMode", context)?;
    let len = chunk_length(&chunk, object_mode, context)?;
    let length = num(state, "length", context)? + len;
    put(state, "length", length, context)?;
    let ok = length < num(state, "highWaterMark", context)?;
    if !ok {
        put(state, "needDrain", true, context)?;
    }
    if flag(state, "writing", context)?
        || num(state, "corked", context)? > 0.
        || !sget(state, "errored", context)?.is_null_or_undefined()
        || !flag(state, "constructed", context)?
    {
        let entry = object(context);
        put(&entry, "chunk", chunk, context)?;
        put(&entry, "encoding", encoding.clone(), context)?;
        put(&entry, "callback", callback.clone(), context)?;
        invoke(
            &sget(state, "buffered", context)?,
            "push",
            &[entry.into()],
            context,
        )?;
        if flag(state, "allBuffers", context)?
            && encoding
                .as_string()
                .is_none_or(|e| e.to_std_string_escaped() != "buffer")
        {
            put(state, "allBuffers", false, context)?;
        }
        if flag(state, "allNoop", context)?
            && get(&callback, "name", context)?
                .as_string()
                .is_none_or(|n| n.to_std_string_escaped() != "nop")
        {
            put(state, "allNoop", false, context)?;
        }
    } else {
        put(state, "writelen", len, context)?;
        put(state, "writecb", callback, context)?;
        put(state, "writing", true, context)?;
        put(state, "sync", true, context)?;
        let onwrite = sget(state, "onwrite", context)?;
        invoke(stream, "_write", &[chunk, encoding, onwrite], context)?;
        put(state, "sync", false, context)?;
    }
    Ok(ok
        && sget(state, "errored", context)?.is_null_or_undefined()
        && !flag(state, "destroyed", context)?)
}

#[allow(clippy::too_many_arguments)]
fn do_write(
    stream: &JsValue,
    state: &JsObject,
    writev: bool,
    len: f64,
    chunk: JsValue,
    encoding: JsValue,
    callback: JsValue,
    context: &mut Context,
) -> JsResult<()> {
    put(state, "writelen", len, context)?;
    put(state, "writecb", callback, context)?;
    put(state, "writing", true, context)?;
    put(state, "sync", true, context)?;
    let onwrite = sget(state, "onwrite", context)?;
    if flag(state, "destroyed", context)? {
        let error = error_value(
            context,
            "Cannot call write after a stream was destroyed",
            "ERR_STREAM_DESTROYED",
        );
        crate::call(context, &onwrite, &JsValue::undefined(), &[error])?;
    } else if writev {
        invoke(stream, "_writev", &[chunk, onwrite], context)?;
    } else {
        invoke(stream, "_write", &[chunk, encoding, onwrite], context)?;
    }
    put(state, "sync", false, context)?;
    Ok(())
}

/// What `_write` calls back with: the next buffered chunk written, `drain` and the write's
/// callback a tick later when it called back at once.
fn on_write(stream: &JsValue, error: JsValue, context: &mut Context) -> JsResult<()> {
    let Some(state) = wstate(stream, context)? else {
        return Ok(());
    };
    let sync = flag(&state, "sync", context)?;
    let callback = sget(&state, "writecb", context)?;
    if !callback.is_callable() {
        let error = error_value(
            context,
            "Callback called multiple times",
            "ERR_MULTIPLE_CALLBACK",
        );
        return error_or_destroy(stream, error, false, context);
    }
    put(&state, "writing", false, context)?;
    put(&state, "writecb", JsValue::null(), context)?;
    let length = num(&state, "length", context)? - num(&state, "writelen", context)?;
    put(&state, "length", length, context)?;
    put(&state, "writelen", 0, context)?;
    if !error.is_null_or_undefined() {
        if sget(&state, "errored", context)?.is_null_or_undefined() {
            put(&state, "errored", error.clone(), context)?;
        }
        if let Some(r) = rstate(stream, context)?
            && sget(&r, "errored", context)?.is_null_or_undefined()
        {
            put(&r, "errored", error.clone(), context)?;
        }
        let args = vec![stream.clone(), error, callback];
        if sync {
            next_tick(context, on_write_error, args);
        } else {
            on_write_error(&JsValue::undefined(), &args, context)?;
        }
        return Ok(());
    }
    let buffered = sget(&state, "buffered", context)?;
    if get(&buffered, "length", context)?.to_number(context)?
        > num(&state, "bufferedIndex", context)?
    {
        clear_buffer(stream, &state, context)?;
    }
    if sync {
        // Callbacks of writes that called back at once are called together, a tick later.
        let info = sget(&state, "afterWriteTickInfo", context)?;
        if let Some(info) = info.as_object()
            && info.get(js_string!("cb"), context)? == callback
        {
            let count = num(&info, "count", context)? + 1.;
            put(&info, "count", count, context)?;
        } else {
            let info = object(context);
            put(&info, "count", 1, context)?;
            put(&info, "cb", callback, context)?;
            put(&info, "stream", stream.clone(), context)?;
            put(&state, "afterWriteTickInfo", info.clone(), context)?;
            next_tick(
                context,
                |_, args, cx| {
                    let info = arg(args, 0);
                    let stream = get(&info, "stream", cx)?;
                    if let Some(state) = wstate(&stream, cx)? {
                        put(&state, "afterWriteTickInfo", JsValue::null(), cx)?;
                        let count = get(&info, "count", cx)?.to_number(cx)?;
                        let cb = get(&info, "cb", cx)?;
                        after_write(&stream, &state, count, cb, cx)?;
                    }
                    Ok(JsValue::undefined())
                },
                vec![info.into()],
            );
        }
    } else {
        after_write(stream, &state, 1., callback, context)?;
    }
    Ok(())
}

fn on_write_error(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (stream, error, callback) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if let Some(state) = wstate(&stream, context)? {
        let pending = num(&state, "pendingcb", context)? - 1.;
        put(&state, "pendingcb", pending, context)?;
        crate::call(
            context,
            &callback,
            &JsValue::undefined(),
            std::slice::from_ref(&error),
        )?;
        error_buffer(&state, context)?;
    }
    error_or_destroy(&stream, error, false, context)?;
    Ok(JsValue::undefined())
}

fn after_write(
    stream: &JsValue,
    state: &JsObject,
    count: f64,
    callback: JsValue,
    context: &mut Context,
) -> JsResult<()> {
    let need_drain = !flag(state, "ending", context)?
        && !flag(state, "destroyed", context)?
        && num(state, "length", context)? == 0.
        && flag(state, "needDrain", context)?;
    if need_drain {
        put(state, "needDrain", false, context)?;
        emit(stream, &[js_text("drain")], context)?;
    }
    for _ in 0..count as usize {
        let pending = num(state, "pendingcb", context)? - 1.;
        put(state, "pendingcb", pending, context)?;
        crate::call(
            context,
            &callback,
            &JsValue::undefined(),
            &[JsValue::null()],
        )?;
    }
    if flag(state, "destroyed", context)? {
        error_buffer(state, context)?;
    }
    finish_maybe(stream, state, false, context)
}

/// The writes still buffered when the stream failed or was destroyed: each told so.
fn error_buffer(state: &JsObject, context: &mut Context) -> JsResult<()> {
    if flag(state, "writing", context)? {
        return Ok(());
    }
    let buffered = sget(state, "buffered", context)?;
    let index = num(state, "bufferedIndex", context)? as usize;
    let object_mode = flag(state, "objectMode", context)?;
    let entries = list_of(&buffered, context)?;
    for entry in entries.iter().skip(index) {
        let chunk = get(entry, "chunk", context)?;
        let len = chunk_length(&chunk, object_mode, context)?;
        let length = num(state, "length", context)? - len;
        put(state, "length", length, context)?;
        let errored = sget(state, "errored", context)?;
        let error = if errored.is_null_or_undefined() {
            error_value(
                context,
                "Cannot call write after a stream was destroyed",
                "ERR_STREAM_DESTROYED",
            )
        } else {
            errored
        };
        let callback = get(entry, "callback", context)?;
        crate::call(context, &callback, &JsValue::undefined(), &[error])?;
    }
    let on_finished = sget(state, "__onFinished", context)?;
    let callbacks = list_of(&on_finished, context)?;
    invoke(&on_finished, "splice", &[0.into()], context)?;
    for callback in callbacks {
        let errored = sget(state, "errored", context)?;
        let error = if errored.is_null_or_undefined() {
            error_value(
                context,
                "Cannot call end after a stream was destroyed",
                "ERR_STREAM_DESTROYED",
            )
        } else {
            errored
        };
        crate::call(context, &callback, &JsValue::undefined(), &[error])?;
    }
    invoke(&buffered, "splice", &[0.into()], context)?;
    put(state, "bufferedIndex", 0, context)?;
    put(state, "allBuffers", true, context)?;
    put(state, "allNoop", true, context)?;
    Ok(())
}

/// Writes what was buffered while a write was under way (or the stream corked): all at once
/// through `_writev` when the stream has one, one by one otherwise.
fn clear_buffer(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    if num(state, "corked", context)? > 0.
        || flag(state, "bufferProcessing", context)?
        || flag(state, "destroyed", context)?
        || !flag(state, "constructed", context)?
    {
        return Ok(());
    }
    let buffered = sget(state, "buffered", context)?;
    let index = num(state, "bufferedIndex", context)? as usize;
    let entries = list_of(&buffered, context)?;
    let count = entries.len().saturating_sub(index);
    if count == 0 {
        return Ok(());
    }
    put(state, "bufferProcessing", true, context)?;
    let object_mode = flag(state, "objectMode", context)?;
    if count > 1 && get(stream, "_writev", context)?.is_callable() {
        let pending = num(state, "pendingcb", context)? - (count as f64 - 1.);
        put(state, "pendingcb", pending, context)?;
        let chunks: Vec<JsValue> = entries[index..].to_vec();
        let callbacks = {
            let mut list = Vec::new();
            for entry in &chunks {
                list.push(get(entry, "callback", context)?);
            }
            array(context, list)
        };
        let all = closure(
            context,
            "callback",
            1,
            |_, args, callbacks, cx| {
                for callback in list_of(callbacks, cx)? {
                    crate::call(cx, &callback, &JsValue::undefined(), &[arg(args, 0)])?;
                }
                Ok(JsValue::undefined())
            },
            callbacks,
        );
        let length = num(state, "length", context)?;
        let list = array(context, chunks);
        if let Some(list) = list.as_object() {
            list.set(
                js_string!("allBuffers"),
                flag(state, "allBuffers", context)?,
                false,
                context,
            )?;
        }
        invoke(&buffered, "splice", &[0.into()], context)?;
        put(state, "bufferedIndex", 0, context)?;
        put(state, "allBuffers", true, context)?;
        put(state, "allNoop", true, context)?;
        do_write(
            stream,
            state,
            true,
            length,
            list,
            js_text(""),
            all.into(),
            context,
        )?;
    } else {
        let mut at = index;
        loop {
            let entry = entries[at].clone();
            at += 1;
            let chunk = get(&entry, "chunk", context)?;
            let encoding = get(&entry, "encoding", context)?;
            let callback = get(&entry, "callback", context)?;
            let len = chunk_length(&chunk, object_mode, context)?;
            do_write(
                stream, state, false, len, chunk, encoding, callback, context,
            )?;
            if at >= entries.len() || flag(state, "writing", context)? {
                break;
            }
        }
        if at >= entries.len() {
            invoke(&buffered, "splice", &[0.into()], context)?;
            put(state, "bufferedIndex", 0, context)?;
            put(state, "allBuffers", true, context)?;
            put(state, "allNoop", true, context)?;
        } else {
            put(state, "bufferedIndex", at, context)?;
        }
    }
    put(state, "bufferProcessing", false, context)?;
    Ok(())
}

/// `end(chunk, encoding, callback)`: the last chunk written, then `finish` once everything
/// is.
fn end(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state =
        wstate(this, context)?.ok_or_else(|| crate::native::type_error("not a writable stream"))?;
    let (mut chunk, mut encoding, mut callback) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if chunk.is_callable() {
        callback = chunk;
        chunk = JsValue::null();
        encoding = JsValue::null();
    } else if encoding.is_callable() {
        callback = encoding;
        encoding = JsValue::null();
    }
    let mut error = JsValue::undefined();
    if !chunk.is_null_or_undefined() {
        let written = write(this, &[chunk, encoding], context)?;
        if written
            .as_object()
            .is_some_and(|o| o.is::<boa_engine::builtins::error::Error>())
        {
            error = written;
        }
    }
    if num(&state, "corked", context)? > 0. {
        put(&state, "corked", 1, context)?;
        uncork(this, &[], context)?;
    }
    if !error.is_undefined() {
        // The write failed; that is the error.
    } else if sget(&state, "errored", context)?.is_null_or_undefined()
        && !flag(&state, "ending", context)?
    {
        put(&state, "ending", true, context)?;
        finish_maybe(this, &state, true, context)?;
        put(&state, "ended", true, context)?;
    } else if flag(&state, "finished", context)? {
        error = error_value(
            context,
            "Cannot call end after a stream was finished",
            "ERR_STREAM_ALREADY_FINISHED",
        );
    } else if flag(&state, "destroyed", context)? {
        error = error_value(
            context,
            "Cannot call end after a stream was destroyed",
            "ERR_STREAM_DESTROYED",
        );
    }
    if callback.is_callable() {
        if !error.is_undefined() {
            crate::microtask(context, callback, vec![error]);
        } else if flag(&state, "finished", context)? {
            crate::microtask(context, callback, vec![JsValue::null()]);
        } else {
            invoke(
                &sget(&state, "__onFinished", context)?,
                "push",
                &[callback],
                context,
            )?;
        }
    }
    Ok(this.clone())
}

fn need_finish(state: &JsObject, context: &mut Context) -> JsResult<bool> {
    Ok(flag(state, "ending", context)?
        && !flag(state, "destroyed", context)?
        && flag(state, "constructed", context)?
        && num(state, "length", context)? == 0.
        && sget(state, "errored", context)?.is_null_or_undefined()
        && get(&sget(state, "buffered", context)?, "length", context)?.to_number(context)? == 0.
        && !flag(state, "finished", context)?
        && !flag(state, "writing", context)?
        && !flag(state, "errorEmitted", context)?
        && !flag(state, "closeEmitted", context)?)
}

fn call_final(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    let captured = object(context);
    put(&captured, "stream", stream.clone(), context)?;
    put(&captured, "called", false, context)?;
    let on_finish = closure(
        context,
        "onFinish",
        1,
        |_, args, captured, cx| {
            let stream = sget(captured, "stream", cx)?;
            let Some(state) = wstate(&stream, cx)? else {
                return Ok(JsValue::undefined());
            };
            if flag(captured, "called", cx)? {
                let error = error_value(
                    cx,
                    "Callback called multiple times",
                    "ERR_MULTIPLE_CALLBACK",
                );
                error_or_destroy(&stream, error, false, cx)?;
                return Ok(JsValue::undefined());
            }
            put(captured, "called", true, cx)?;
            let pending = num(&state, "pendingcb", cx)? - 1.;
            put(&state, "pendingcb", pending, cx)?;
            let error = arg(args, 0);
            if !error.is_null_or_undefined() {
                let on_finished = sget(&state, "__onFinished", cx)?;
                let callbacks = list_of(&on_finished, cx)?;
                invoke(&on_finished, "splice", &[0.into()], cx)?;
                for callback in callbacks {
                    crate::call(
                        cx,
                        &callback,
                        &JsValue::undefined(),
                        std::slice::from_ref(&error),
                    )?;
                }
                let sync = flag(&state, "sync", cx)?;
                error_or_destroy(&stream, error, sync, cx)?;
            } else if need_finish(&state, cx)? {
                put(&state, "prefinished", true, cx)?;
                emit(&stream, &[js_text("prefinish")], cx)?;
                let pending = num(&state, "pendingcb", cx)? + 1.;
                put(&state, "pendingcb", pending, cx)?;
                next_tick(
                    cx,
                    |_, args, cx| {
                        let stream = arg(args, 0);
                        if let Some(state) = wstate(&stream, cx)? {
                            finish(&stream, &state, cx)?;
                        }
                        Ok(JsValue::undefined())
                    },
                    vec![stream],
                );
            }
            Ok(JsValue::undefined())
        },
        captured,
    );
    put(state, "sync", true, context)?;
    let pending = num(state, "pendingcb", context)? + 1.;
    put(state, "pendingcb", pending, context)?;
    if let Err(error) = invoke(stream, "_final", &[on_finish.clone().into()], context) {
        let error = error.into_opaque(context)?;
        crate::call(context, &on_finish.into(), &JsValue::undefined(), &[error])?;
    }
    put(state, "sync", false, context)?;
    Ok(())
}

fn prefinish(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    if !flag(state, "prefinished", context)? && !flag(state, "finalCalled", context)? {
        if get(stream, "_final", context)?.is_callable() && !flag(state, "destroyed", context)? {
            put(state, "finalCalled", true, context)?;
            call_final(stream, state, context)?;
        } else {
            put(state, "prefinished", true, context)?;
            emit(stream, &[js_text("prefinish")], context)?;
        }
    }
    Ok(())
}

fn finish_maybe(
    stream: &JsValue,
    state: &JsObject,
    sync: bool,
    context: &mut Context,
) -> JsResult<()> {
    if !need_finish(state, context)? {
        return Ok(());
    }
    prefinish(stream, state, context)?;
    if num(state, "pendingcb", context)? == 0. {
        if sync {
            let pending = num(state, "pendingcb", context)? + 1.;
            put(state, "pendingcb", pending, context)?;
            next_tick(
                context,
                |_, args, cx| {
                    let stream = arg(args, 0);
                    if let Some(state) = wstate(&stream, cx)? {
                        if need_finish(&state, cx)? {
                            finish(&stream, &state, cx)?;
                        } else {
                            let pending = num(&state, "pendingcb", cx)? - 1.;
                            put(&state, "pendingcb", pending, cx)?;
                        }
                    }
                    Ok(JsValue::undefined())
                },
                vec![stream.clone()],
            );
        } else if need_finish(state, context)? {
            let pending = num(state, "pendingcb", context)? + 1.;
            put(state, "pendingcb", pending, context)?;
            finish(stream, state, context)?;
        }
    }
    Ok(())
}

fn finish(stream: &JsValue, state: &JsObject, context: &mut Context) -> JsResult<()> {
    let pending = num(state, "pendingcb", context)? - 1.;
    put(state, "pendingcb", pending, context)?;
    put(state, "finished", true, context)?;
    let on_finished = sget(state, "__onFinished", context)?;
    let callbacks = list_of(&on_finished, context)?;
    invoke(&on_finished, "splice", &[0.into()], context)?;
    for callback in callbacks {
        crate::call(
            context,
            &callback,
            &JsValue::undefined(),
            &[JsValue::null()],
        )?;
    }
    emit(stream, &[js_text("finish")], context)?;
    if flag(state, "autoDestroy", context)? {
        let auto = match rstate(stream, context)? {
            None => true,
            Some(r) => {
                flag(&r, "autoDestroy", context)?
                    && (flag(&r, "endEmitted", context)?
                        || sget(&r, "readable", context)?.strict_equals(&false.into()))
            }
        };
        if auto {
            invoke(stream, "destroy", &[], context)?;
        }
    }
    Ok(())
}

fn cork(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(state) = wstate(this, context)? {
        let corked = num(&state, "corked", context)? + 1.;
        put(&state, "corked", corked, context)?;
    }
    Ok(JsValue::undefined())
}

fn uncork(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(state) = wstate(this, context)? {
        let corked = num(&state, "corked", context)?;
        if corked > 0. {
            put(&state, "corked", corked - 1., context)?;
            if !flag(&state, "writing", context)? {
                clear_buffer(this, &state, context)?;
            }
        }
    }
    Ok(JsValue::undefined())
}

// ─── The classes' methods and accessors ──────────────────────────────────────────────────────

fn not_implemented(context: &mut Context, method: &str) -> JsError {
    let error = coded_error_value_js(
        context,
        &format!("The {method} method is not implemented"),
        "ERR_METHOD_NOT_IMPLEMENTED",
    );
    JsError::from_opaque(error)
}

fn coded_error_value_js(context: &mut Context, message: &str, code: &str) -> JsValue {
    error_value(context, message, code)
}

const STREAM_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[("pipe", 2, legacy_pipe)];

const READABLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("push", 2, |this, args, cx| {
        Ok(readable_add_chunk(this, arg(args, 0), arg(args, 1), false, cx)?.into())
    }),
    ("unshift", 2, |this, args, cx| {
        Ok(readable_add_chunk(this, arg(args, 0), arg(args, 1), true, cx)?.into())
    }),
    ("read", 1, read),
    ("_read", 1, |_, _, cx| Err(not_implemented(cx, "_read()"))),
    ("pipe", 2, readable_pipe),
    ("unpipe", 1, readable_unpipe),
    ("on", 2, readable_on),
    ("addListener", 2, readable_on),
    ("removeListener", 2, readable_remove_listener),
    ("off", 2, readable_remove_listener),
    ("removeAllListeners", 1, |this, args, cx| {
        let emitter = crate::events::emitter_class(cx)?;
        let prototype = emitter.get(js_string!("prototype"), cx)?;
        let method = get(&prototype, "removeAllListeners", cx)?;
        let out = crate::call(cx, &method, this, args)?;
        let name = arg(args, 0);
        if name.is_undefined()
            || name
                .as_string()
                .is_some_and(|n| n.to_std_string_escaped() == "readable")
        {
            next_tick(cx, update_readable_listening, vec![this.clone()]);
        }
        Ok(out)
    }),
    ("resume", 0, resume),
    ("pause", 0, pause),
    ("isPaused", 0, |this, _, cx| {
        let Some(state) = rstate(this, cx)? else {
            return Ok(false.into());
        };
        Ok((sget(&state, "paused", cx)?.strict_equals(&true.into())
            || sget(&state, "flowing", cx)?.strict_equals(&false.into()))
        .into())
    }),
    ("setEncoding", 1, set_encoding),
    ("wrap", 1, readable_wrap),
    ("destroy", 2, destroy),
    ("_destroy", 2, |_, args, cx| {
        crate::call(cx, &arg(args, 1), &JsValue::undefined(), &[arg(args, 0)])
    }),
    ("_undestroy", 0, undestroy),
    ("iterator", 1, async_iterator),
    ("toArray", 1, to_array),
    ("forEach", 2, for_each),
];

const WRITABLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("write", 3, write),
    ("end", 3, end),
    ("cork", 0, cork),
    ("uncork", 0, uncork),
    ("setDefaultEncoding", 1, |this, args, cx| {
        let encoding = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
        if crate::string_decoder::normalize_encoding(&encoding).is_none() && encoding != "buffer" {
            return Err(coded_type_error(
                cx,
                &format!("Unknown encoding: {encoding}"),
                "ERR_UNKNOWN_ENCODING",
            ));
        }
        if let Some(state) = wstate(this, cx)? {
            put(&state, "defaultEncoding", js_text(&encoding), cx)?;
        }
        Ok(this.clone())
    }),
    ("_write", 3, |this, args, cx| {
        if get(this, "_writev", cx)?.is_callable() {
            let entry = object(cx);
            put(&entry, "chunk", arg(args, 0), cx)?;
            put(&entry, "encoding", arg(args, 1), cx)?;
            let list = array(cx, vec![entry.into()]);
            return invoke(this, "_writev", &[list, arg(args, 2)], cx);
        }
        Err(not_implemented(cx, "_write()"))
    }),
    ("destroy", 2, writable_destroy),
    ("_destroy", 2, |_, args, cx| {
        crate::call(cx, &arg(args, 1), &JsValue::undefined(), &[arg(args, 0)])
    }),
    ("_undestroy", 0, undestroy),
];

/// A writable side's `destroy`: what is still buffered, and every `end` callback, told of it a
/// tick later, then the stream destroyed.
fn writable_destroy(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(state) = wstate(this, context)?
        && !flag(&state, "destroyed", context)?
    {
        let buffered =
            get(&sget(&state, "buffered", context)?, "length", context)?.to_number(context)?;
        let waiting =
            get(&sget(&state, "__onFinished", context)?, "length", context)?.to_number(context)?;
        if num(&state, "bufferedIndex", context)? < buffered || waiting > 0. {
            next_tick(
                context,
                |_, args, cx| {
                    if let Some(state) = wstate(&arg(args, 0), cx)? {
                        error_buffer(&state, cx)?;
                    }
                    Ok(JsValue::undefined())
                },
                vec![this.clone()],
            );
        }
    }
    destroy(this, args, context)
}

type Accessor = (
    &'static str,
    NativeFunctionPointer,
    Option<NativeFunctionPointer>,
);

/// A getter reading one field of the readable state (`false` without one).
macro_rules! rfield {
    ($name:literal, $field:literal) => {
        (
            $name,
            |this, _, cx| match rstate(this, cx)? {
                Some(state) => sget(&state, $field, cx),
                None => Ok(JsValue::undefined()),
            },
            None,
        )
    };
}

macro_rules! wfield {
    ($name:literal, $field:literal) => {
        (
            $name,
            |this, _, cx| match wstate(this, cx)? {
                Some(state) => sget(&state, $field, cx),
                None => Ok(JsValue::undefined()),
            },
            None,
        )
    };
}

fn either_state(this: &JsValue, context: &mut Context) -> JsResult<Option<JsObject>> {
    Ok(match wstate(this, context)? {
        Some(w) => Some(w),
        None => rstate(this, context)?,
    })
}

const COMMON_ACCESSORS: &[Accessor] = &[
    (
        "destroyed",
        |this, _, cx| {
            let mut destroyed = false;
            let mut any = false;
            for state in [rstate(this, cx)?, wstate(this, cx)?].into_iter().flatten() {
                any = true;
                destroyed |= flag(&state, "destroyed", cx)?;
            }
            Ok(if any { destroyed.into() } else { false.into() })
        },
        Some(|this, args, cx| {
            for state in [rstate(this, cx)?, wstate(this, cx)?].into_iter().flatten() {
                put(&state, "destroyed", arg(args, 0).to_boolean(), cx)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
    (
        "closed",
        |this, _, cx| match either_state(this, cx)? {
            Some(state) => sget(&state, "closed", cx),
            None => Ok(false.into()),
        },
        None,
    ),
    (
        "errored",
        |this, _, cx| match either_state(this, cx)? {
            Some(state) => sget(&state, "errored", cx),
            None => Ok(JsValue::null()),
        },
        None,
    ),
];

const READABLE_ACCESSORS: &[Accessor] = &[
    (
        "readable",
        |this, _, cx| {
            let Some(r) = rstate(this, cx)? else {
                return Ok(false.into());
            };
            Ok((!sget(&r, "readable", cx)?.strict_equals(&false.into())
                && !flag(&r, "destroyed", cx)?
                && !flag(&r, "errorEmitted", cx)?
                && !flag(&r, "endEmitted", cx)?)
            .into())
        },
        Some(|this, args, cx| {
            if let Some(r) = rstate(this, cx)? {
                put(&r, "readable", arg(args, 0).to_boolean(), cx)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
    rfield!("readableHighWaterMark", "highWaterMark"),
    rfield!("readableBuffer", "buffer"),
    rfield!("readableLength", "length"),
    rfield!("readableObjectMode", "objectMode"),
    rfield!("readableEncoding", "encoding"),
    rfield!("readableEnded", "endEmitted"),
    rfield!("readableDidRead", "dataEmitted"),
    (
        "readableFlowing",
        |this, _, cx| match rstate(this, cx)? {
            Some(state) => sget(&state, "flowing", cx),
            None => Ok(JsValue::null()),
        },
        Some(|this, args, cx| {
            if let Some(r) = rstate(this, cx)? {
                put(&r, "flowing", arg(args, 0), cx)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
    (
        "readableAborted",
        |this, _, cx| {
            let Some(r) = rstate(this, cx)? else {
                return Ok(false.into());
            };
            Ok(
                ((flag(&r, "destroyed", cx)? || !sget(&r, "errored", cx)?.is_null_or_undefined())
                    && !flag(&r, "endEmitted", cx)?)
                .into(),
            )
        },
        None,
    ),
];

const WRITABLE_ACCESSORS: &[Accessor] = &[
    (
        "writable",
        |this, _, cx| {
            let Some(w) = wstate(this, cx)? else {
                return Ok(false.into());
            };
            Ok((!sget(&w, "writable", cx)?.strict_equals(&false.into())
                && !flag(&w, "destroyed", cx)?
                && sget(&w, "errored", cx)?.is_null_or_undefined()
                && !flag(&w, "ending", cx)?
                && !flag(&w, "ended", cx)?)
            .into())
        },
        Some(|this, args, cx| {
            if let Some(w) = wstate(this, cx)? {
                put(&w, "writable", arg(args, 0).to_boolean(), cx)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
    wfield!("writableFinished", "finished"),
    wfield!("writableObjectMode", "objectMode"),
    wfield!("writableBuffer", "buffered"),
    wfield!("writableEnded", "ending"),
    wfield!("writableHighWaterMark", "highWaterMark"),
    wfield!("writableCorked", "corked"),
    wfield!("writableLength", "length"),
    (
        "writableNeedDrain",
        |this, _, cx| {
            let Some(w) = wstate(this, cx)? else {
                return Ok(false.into());
            };
            Ok((!flag(&w, "destroyed", cx)?
                && !flag(&w, "ending", cx)?
                && flag(&w, "needDrain", cx)?)
            .into())
        },
        None,
    ),
    (
        "writableAborted",
        |this, _, cx| {
            let Some(w) = wstate(this, cx)? else {
                return Ok(false.into());
            };
            Ok(
                ((flag(&w, "destroyed", cx)? || !sget(&w, "errored", cx)?.is_null_or_undefined())
                    && !flag(&w, "finished", cx)?)
                .into(),
            )
        },
        None,
    ),
];

/// The legacy `Stream.prototype.pipe`, for an old-style stream that only emits `data` and
/// `end`.
fn legacy_pipe(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (dest, options) = (arg(args, 0), arg(args, 1));
    let ps = object(context);
    put(&ps, "source", this.clone(), context)?;
    put(&ps, "dest", dest.clone(), context)?;
    put(&ps, "didOnEnd", false, context)?;
    let handlers: [(&str, usize, Handler); 6] = [
        ("ondata", 1, |_, args, ps, cx| {
            let (source, dest) = (sget(ps, "source", cx)?, sget(ps, "dest", cx)?);
            if get(&dest, "writable", cx)?.to_boolean()
                && invoke(&dest, "write", &[arg(args, 0)], cx)?.strict_equals(&false.into())
                && get(&source, "pause", cx)?.is_callable()
            {
                invoke(&source, "pause", &[], cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("ondrain", 0, |_, _, ps, cx| {
            let source = sget(ps, "source", cx)?;
            if get(&source, "readable", cx)?.to_boolean()
                && get(&source, "resume", cx)?.is_callable()
            {
                invoke(&source, "resume", &[], cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("onend", 0, |_, _, ps, cx| {
            if flag(ps, "didOnEnd", cx)? {
                return Ok(JsValue::undefined());
            }
            put(ps, "didOnEnd", true, cx)?;
            invoke(&sget(ps, "dest", cx)?, "end", &[], cx)
        }),
        ("onclose", 0, |_, _, ps, cx| {
            if flag(ps, "didOnEnd", cx)? {
                return Ok(JsValue::undefined());
            }
            put(ps, "didOnEnd", true, cx)?;
            let dest = sget(ps, "dest", cx)?;
            if get(&dest, "destroy", cx)?.is_callable() {
                invoke(&dest, "destroy", &[], cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("onerror", 1, |this, args, ps, cx| {
            legacy_cleanup(ps, cx)?;
            if listener_count(this, "error", cx)? == 0. {
                emit(this, &[js_text("error"), arg(args, 0)], cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("cleanup", 0, |_, _, ps, cx| {
            legacy_cleanup(ps, cx)?;
            Ok(JsValue::undefined())
        }),
    ];
    for (name, length, f) in handlers {
        let handler = closure(context, name, length, f, ps.clone());
        put(&ps, name, handler, context)?;
    }
    invoke(
        this,
        "on",
        &[js_text("data"), sget(&ps, "ondata", context)?],
        context,
    )?;
    invoke(
        &dest,
        "on",
        &[js_text("drain"), sget(&ps, "ondrain", context)?],
        context,
    )?;
    let is_stdio = get(&dest, "_isStdio", context)?.to_boolean();
    if !is_stdio && !get(&options, "end", context)?.strict_equals(&false.into()) {
        invoke(
            this,
            "on",
            &[js_text("end"), sget(&ps, "onend", context)?],
            context,
        )?;
        invoke(
            this,
            "on",
            &[js_text("close"), sget(&ps, "onclose", context)?],
            context,
        )?;
    }
    let onerror = sget(&ps, "onerror", context)?;
    for target in [this, &dest] {
        if get(target, "prependListener", context)?.is_callable() {
            invoke(
                target,
                "prependListener",
                &[js_text("error"), onerror.clone()],
                context,
            )?;
        } else {
            invoke(target, "on", &[js_text("error"), onerror.clone()], context)?;
        }
    }
    let cleanup = sget(&ps, "cleanup", context)?;
    invoke(this, "on", &[js_text("end"), cleanup.clone()], context)?;
    invoke(this, "on", &[js_text("close"), cleanup.clone()], context)?;
    invoke(&dest, "on", &[js_text("close"), cleanup], context)?;
    emit(&dest, &[js_text("pipe"), this.clone()], context)?;
    Ok(dest)
}

fn legacy_cleanup(ps: &JsObject, context: &mut Context) -> JsResult<()> {
    let (source, dest) = (sget(ps, "source", context)?, sget(ps, "dest", context)?);
    for (target, event, handler) in [
        (&source, "data", "ondata"),
        (&dest, "drain", "ondrain"),
        (&source, "end", "onend"),
        (&source, "close", "onclose"),
        (&source, "error", "onerror"),
        (&dest, "error", "onerror"),
        (&source, "end", "cleanup"),
        (&source, "close", "cleanup"),
        (&dest, "close", "cleanup"),
    ] {
        let handler = sget(ps, handler, context)?;
        invoke(
            target,
            "removeListener",
            &[js_text(event), handler],
            context,
        )?;
    }
    Ok(())
}

// ─── Duplex, Transform, PassThrough ──────────────────────────────────────────────────────────

fn duplex_setup(target: &JsObject, options: &JsValue, context: &mut Context) -> JsResult<()> {
    readable_setup(target, options, true, context)?;
    writable_setup(target, options, true, context)?;
    if options.is_object() {
        let allow = get(options, "allowHalfOpen", context)?;
        let allow_half_open = !allow.strict_equals(&false.into());
        target.set(js_string!("allowHalfOpen"), allow_half_open, false, context)?;
        if !allow_half_open {
            let end_writable = function(context, "onReadableEnd", 0, |this, _, cx| {
                // The writable side ends with the readable one, a tick later.
                next_tick(
                    cx,
                    |_, args, cx| invoke(&arg(args, 0), "end", &[], cx),
                    vec![this.clone()],
                );
                Ok(JsValue::undefined())
            });
            invoke(
                &target.clone().into(),
                "once",
                &[js_text("end"), end_writable.into()],
                context,
            )?;
        }
        if get(options, "readable", context)?.strict_equals(&false.into())
            && let Some(r) = rstate(&target.clone().into(), context)?
        {
            put(&r, "readable", false, context)?;
            put(&r, "ended", true, context)?;
            put(&r, "endEmitted", true, context)?;
        }
        if get(options, "writable", context)?.strict_equals(&false.into())
            && let Some(w) = wstate(&target.clone().into(), context)?
        {
            put(&w, "writable", false, context)?;
            put(&w, "ending", true, context)?;
            put(&w, "ended", true, context)?;
            put(&w, "finished", true, context)?;
        }
    } else {
        target.set(js_string!("allowHalfOpen"), true, false, context)?;
    }
    Ok(())
}

fn duplex_init(target: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    duplex_setup(target, &arg(args, 0), context)?;
    construct_later(&target.clone().into(), context)
}

fn transform_init(target: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let options = arg(args, 0);
    duplex_setup(target, &options, context)?;
    if let Some(r) = rstate(&target.clone().into(), context)? {
        // The readable side is written to from `_write`, never at once: as Node's.
        put(&r, "sync", false, context)?;
    }
    hidden(
        target,
        js_string!("__transformCallback"),
        JsValue::null(),
        context,
    )?;
    if options.is_object() {
        for (option, method) in [("transform", "_transform"), ("flush", "_flush")] {
            let f = get(&options, option, context)?;
            if f.is_callable() {
                target.set(JsString::from(method), f, false, context)?;
            }
        }
    }
    let prefinish = function(context, "prefinish", 0, |this, _, cx| {
        // A subclass's own `_final` takes the place of the flush; run it unless that happened.
        if !get(&get(this, "_final", cx)?, "__isTransformFinal", cx)?.to_boolean() {
            transform_final(this, &[], cx)?;
        }
        Ok(JsValue::undefined())
    });
    invoke(
        &target.clone().into(),
        "on",
        &[js_text("prefinish"), prefinish.into()],
        context,
    )?;
    construct_later(&target.clone().into(), context)
}

/// `_final` of a transform: `_flush` asked for what is left, then the readable side ended.
fn transform_final(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = arg(args, 0);
    let flush = get(this, "_flush", context)?;
    let destroyed = get(this, "destroyed", context)?.to_boolean();
    if flush.is_callable() && !destroyed {
        let pair = array(context, vec![this.clone(), callback]);
        let done = closure(
            context,
            "flushed",
            2,
            |_, args, pair, cx| {
                let pair = list_of(pair, cx)?;
                let (stream, callback) = (&pair[0], &pair[1]);
                let (error, data) = (arg(args, 0), arg(args, 1));
                if !error.is_null_or_undefined() {
                    if callback.is_callable() {
                        crate::call(cx, callback, &JsValue::undefined(), &[error])?;
                    } else {
                        invoke(stream, "destroy", &[error], cx)?;
                    }
                    return Ok(JsValue::undefined());
                }
                if !data.is_null_or_undefined() {
                    invoke(stream, "push", &[data], cx)?;
                }
                invoke(stream, "push", &[JsValue::null()], cx)?;
                if callback.is_callable() {
                    crate::call(cx, callback, &JsValue::undefined(), &[])?;
                }
                Ok(JsValue::undefined())
            },
            pair,
        );
        crate::call(context, &flush, this, &[done.into()])?;
    } else {
        invoke(this, "push", &[JsValue::null()], context)?;
        if callback.is_callable() {
            crate::call(context, &callback, &JsValue::undefined(), &[])?;
        }
    }
    Ok(JsValue::undefined())
}

/// `_write` of a transform: the chunk through `_transform`, its output pushed; the next write
/// waits while the readable side is full.
fn transform_write(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (chunk, encoding, callback) = (arg(args, 0), arg(args, 1), arg(args, 2));
    let r = rstate(this, context)?
        .ok_or_else(|| crate::native::type_error("not a transform stream"))?;
    let length = num(&r, "length", context)?;
    let captured = array(context, vec![this.clone(), callback, length.into()]);
    let done = closure(
        context,
        "afterTransform",
        2,
        |_, args, captured, cx| {
            let captured = list_of(captured, cx)?;
            let (stream, callback, length) =
                (&captured[0], &captured[1], captured[2].to_number(cx)?);
            let (error, value) = (arg(args, 0), arg(args, 1));
            if !error.is_null_or_undefined() {
                crate::call(cx, callback, &JsValue::undefined(), &[error])?;
                return Ok(JsValue::undefined());
            }
            if !value.is_null_or_undefined() {
                invoke(stream, "push", &[value], cx)?;
            }
            let Some(r) = rstate(stream, cx)? else {
                return Ok(JsValue::undefined());
            };
            let w = wstate(stream, cx)?;
            let w_ended = match &w {
                Some(w) => flag(w, "ended", cx)?,
                None => false,
            };
            if w_ended
                || length == num(&r, "length", cx)?
                || num(&r, "length", cx)? < num(&r, "highWaterMark", cx)?
            {
                crate::call(cx, callback, &JsValue::undefined(), &[])?;
            } else if let Some(target) = stream.as_object() {
                target.set(
                    js_string!("__transformCallback"),
                    callback.clone(),
                    false,
                    cx,
                )?;
            }
            Ok(JsValue::undefined())
        },
        captured,
    );
    invoke(this, "_transform", &[chunk, encoding, done.into()], context)?;
    Ok(JsValue::undefined())
}

/// `_read` of a transform: the write that waited for room goes on.
fn transform_read(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = get(this, "__transformCallback", context)?;
    if callback.is_callable() {
        if let Some(target) = this.as_object() {
            target.set(
                js_string!("__transformCallback"),
                JsValue::null(),
                false,
                context,
            )?;
        }
        crate::call(context, &callback, &JsValue::undefined(), &[])?;
    }
    Ok(JsValue::undefined())
}

const TRANSFORM_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("_transform", 3, |_, _, cx| {
        Err(not_implemented(cx, "_transform()"))
    }),
    ("_write", 3, transform_write),
    ("_read", 1, transform_read),
];

// ─── finished, pipeline, addAbortSignal ──────────────────────────────────────────────────────

/// `finished(stream, options, callback)`: `callback` once the stream is done — ended, finished,
/// closed (and closed early is an error), or failed. Answers a function that stops listening.
fn eos(
    stream: &JsValue,
    options: &JsValue,
    callback: JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let r = rstate(stream, context)?;
    let w = wstate(stream, context)?;
    let readable = match get(options, "readable", context)? {
        v if v.is_undefined() => r.is_some() || get(stream, "readable", context)?.is_boolean(),
        v => v.to_boolean(),
    };
    let writable = match get(options, "writable", context)? {
        v if v.is_undefined() => w.is_some() || get(stream, "writable", context)?.is_boolean(),
        v => v.to_boolean(),
    };
    let st = object(context);
    put(&st, "stream", stream.clone(), context)?;
    put(&st, "callback", callback, context)?;
    put(&st, "called", false, context)?;
    put(&st, "readable", readable, context)?;
    put(&st, "writable", writable, context)?;
    let readable_finished = match &r {
        Some(r) => flag(r, "endEmitted", context)?,
        None => !readable,
    };
    let writable_finished = match &w {
        Some(w) => flag(w, "finished", context)?,
        None => !writable,
    };
    put(&st, "readableFinished", readable_finished, context)?;
    put(&st, "writableFinished", writable_finished, context)?;
    // A stream that destroys itself and says `close` is done only then.
    let mut will_close = false;
    for state in [&r, &w].into_iter().flatten() {
        will_close |= flag(state, "autoDestroy", context)?
            && flag(state, "emitClose", context)?
            && !flag(state, "closed", context)?;
    }
    put(&st, "willEmitClose", will_close, context)?;

    let handlers: [(&str, usize, Handler); 4] = [
        ("onend", 0, |_, _, st, cx| {
            put(st, "readableFinished", true, cx)?;
            if flag(st, "willEmitClose", cx)?
                && !get(&sget(st, "stream", cx)?, "destroyed", cx)?.to_boolean()
            {
                let w = wstate(&sget(st, "stream", cx)?, cx)?;
                if w.is_none() || flag(st, "writableFinished", cx)? || !flag(st, "writable", cx)? {
                    return Ok(JsValue::undefined());
                }
            }
            if !flag(st, "writable", cx)? || flag(st, "writableFinished", cx)? {
                eos_done(st, JsValue::undefined(), cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("onfinish", 0, |_, _, st, cx| {
            put(st, "writableFinished", true, cx)?;
            if flag(st, "willEmitClose", cx)?
                && !get(&sget(st, "stream", cx)?, "destroyed", cx)?.to_boolean()
            {
                let r = rstate(&sget(st, "stream", cx)?, cx)?;
                if r.is_none() || flag(st, "readableFinished", cx)? || !flag(st, "readable", cx)? {
                    return Ok(JsValue::undefined());
                }
            }
            if !flag(st, "readable", cx)? || flag(st, "readableFinished", cx)? {
                eos_done(st, JsValue::undefined(), cx)?;
            }
            Ok(JsValue::undefined())
        }),
        ("onerror", 1, |_, args, st, cx| {
            eos_done(st, arg(args, 0), cx)?;
            Ok(JsValue::undefined())
        }),
        ("onclose", 0, |_, _, st, cx| eos_close(st, cx)),
    ];
    for (name, length, f) in handlers {
        let handler = closure(context, name, length, f, st.clone());
        put(&st, name, handler, context)?;
    }
    for (event, handler) in [
        ("end", "onend"),
        ("finish", "onfinish"),
        ("error", "onerror"),
        ("close", "onclose"),
    ] {
        invoke(
            stream,
            "on",
            &[js_text(event), sget(&st, handler, context)?],
            context,
        )?;
    }
    // Done already: said a tick from now.
    let closed = [&r, &w]
        .into_iter()
        .flatten()
        .map(|s| flag(s, "closed", context))
        .collect::<JsResult<Vec<_>>>()?
        .into_iter()
        .any(|c| c);
    let error_emitted = [&r, &w]
        .into_iter()
        .flatten()
        .map(|s| flag(s, "errorEmitted", context))
        .collect::<JsResult<Vec<_>>>()?
        .into_iter()
        .any(|c| c);
    let done = (!readable || readable_finished) && (!writable || writable_finished);
    if closed || error_emitted || (done && !will_close && (r.is_some() || w.is_some())) {
        let onclose = sget(&st, "onclose", context)?;
        crate::microtask(context, onclose, Vec::new());
    }
    let signal = get(options, "signal", context)?;
    if signal.is_object() {
        let on_abort = closure(
            context,
            "onabort",
            0,
            |_, _, st, cx| {
                let stream = sget(st, "stream", cx)?;
                let reason = crate::native::get(&sget(st, "signal", cx)?, "reason", cx)?;
                let error = abort_error(cx, reason);
                eos_done(st, error, cx)?;
                let _ = stream;
                Ok(JsValue::undefined())
            },
            st.clone(),
        );
        put(&st, "signal", signal.clone(), context)?;
        if get(&signal, "aborted", context)?.to_boolean() {
            crate::microtask(context, on_abort.into(), Vec::new());
        } else {
            put(&st, "onabort", on_abort.clone(), context)?;
            invoke(
                &signal,
                "addEventListener",
                &[js_text("abort"), on_abort.into()],
                context,
            )?;
        }
    }
    let cleanup = closure(
        context,
        "cleanup",
        0,
        |_, _, st, cx| {
            eos_cleanup(st, cx)?;
            Ok(JsValue::undefined())
        },
        st,
    );
    Ok(cleanup.into())
}

fn eos_close(st: &JsObject, context: &mut Context) -> JsResult<JsValue> {
    let stream = sget(st, "stream", context)?;
    let r = rstate(&stream, context)?;
    let w = wstate(&stream, context)?;
    for state in [&w, &r].into_iter().flatten() {
        let errored = sget(state, "errored", context)?;
        if !errored.is_null_or_undefined() && !errored.is_boolean() {
            eos_done(st, errored, context)?;
            return Ok(JsValue::undefined());
        }
    }
    let readable_done = match &r {
        Some(r) => flag(r, "endEmitted", context)?,
        None => true,
    } || flag(st, "readableFinished", context)?;
    let writable_done = match &w {
        Some(w) => flag(w, "finished", context)?,
        None => true,
    } || flag(st, "writableFinished", context)?;
    if (flag(st, "readable", context)? && !readable_done)
        || (flag(st, "writable", context)? && !writable_done)
    {
        let error = error_value(context, "Premature close", "ERR_STREAM_PREMATURE_CLOSE");
        eos_done(st, error, context)?;
    } else {
        eos_done(st, JsValue::undefined(), context)?;
    }
    Ok(JsValue::undefined())
}

fn eos_done(st: &JsObject, error: JsValue, context: &mut Context) -> JsResult<()> {
    if flag(st, "called", context)? {
        return Ok(());
    }
    put(st, "called", true, context)?;
    let onabort = sget(st, "onabort", context)?;
    if onabort.is_callable() {
        let signal = sget(st, "signal", context)?;
        invoke(
            &signal,
            "removeEventListener",
            &[js_text("abort"), onabort],
            context,
        )?;
    }
    let stream = sget(st, "stream", context)?;
    let callback = sget(st, "callback", context)?;
    let args = if error.is_undefined() {
        Vec::new()
    } else {
        vec![error]
    };
    crate::call(context, &callback, &stream, &args)?;
    Ok(())
}

fn eos_cleanup(st: &JsObject, context: &mut Context) -> JsResult<()> {
    let stream = sget(st, "stream", context)?;
    for (event, handler) in [
        ("end", "onend"),
        ("finish", "onfinish"),
        ("error", "onerror"),
        ("close", "onclose"),
    ] {
        let handler = sget(st, handler, context)?;
        invoke(
            &stream,
            "removeListener",
            &[js_text(event), handler],
            context,
        )?;
    }
    Ok(())
}

fn finished(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let stream = arg(args, 0);
    let (options, callback) = if arg(args, 1).is_callable() {
        (JsValue::undefined(), arg(args, 1))
    } else {
        (arg(args, 1), arg(args, 2))
    };
    if !callback.is_callable() {
        return Err(coded_type_error(
            context,
            "The \"callback\" argument must be of type function.",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    eos(&stream, &options, callback, context)
}

/// Whether `value` is a Node stream (it can `pipe` or `write`, and listens with `on`).
fn is_node_stream(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    Ok(value.is_object()
        && get(value, "on", context)?.is_callable()
        && (get(value, "pipe", context)?.is_callable()
            || get(value, "write", context)?.is_callable()))
}

/// `pipeline(source, ...transforms, destination, callback)`: each piped into the next; the
/// callback hears the first error (every stream then destroyed) or the end of the last.
/// A stage may be an async generator function, handed the stage before it; the source may be
/// any iterable.
fn pipeline(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut stages: Vec<JsValue> = args.to_vec();
    let callback = match stages.last() {
        Some(last) if last.is_callable() && !is_node_stream(last, context)? && stages.len() > 1 => {
            stages.pop().unwrap_or_default()
        }
        _ => {
            return Err(coded_type_error(
                context,
                "The \"callback\" argument must be of type function.",
                "ERR_INVALID_ARG_TYPE",
            ));
        }
    };
    if stages.len() == 1
        && stages[0].is_object()
        && stages[0].as_object().is_some_and(|o| o.is_array())
    {
        stages = list_of(&stages[0], context)?;
    }
    run_pipeline(stages, callback, JsValue::undefined(), context)
}

fn run_pipeline(
    stages: Vec<JsValue>,
    callback: JsValue,
    signal: JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    if stages.len() < 2 {
        return Err(coded_type_error(
            context,
            "The \"streams\" argument must be specified",
            "ERR_MISSING_ARGS",
        ));
    }
    let st = object(context);
    put(&st, "callback", callback, context)?;
    put(&st, "done", false, context)?;
    let streams = JsArray::new(context)?;
    put(&st, "streams", streams.clone(), context)?;
    put(&st, "remaining", 0, context)?;
    put(&st, "value", JsValue::undefined(), context)?;

    let last_index = stages.len() - 1;
    let mut previous = JsValue::undefined();
    let mut last_value_promise = JsValue::undefined();
    for (at, stage) in stages.into_iter().enumerate() {
        let current = if at == 0 {
            if is_node_stream(&stage, context)? {
                stage
            } else if stage.is_callable() {
                let produced = crate::call(context, &stage, &JsValue::undefined(), &[])?;
                readable_from(&JsValue::undefined(), &[produced], context)?
            } else {
                readable_from(&JsValue::undefined(), &[stage], context)?
            }
        } else if is_node_stream(&stage, context)? {
            invoke(&previous, "pipe", std::slice::from_ref(&stage), context)?;
            stage
        } else if stage.is_callable() {
            // An async generator function, given the stream before it.
            let produced =
                crate::call(context, &stage, &JsValue::undefined(), &[previous.clone()])?;
            if at == last_index {
                last_value_promise = produced;
                JsValue::undefined()
            } else {
                readable_from(&JsValue::undefined(), &[produced], context)?
            }
        } else {
            return Err(coded_type_error(
                context,
                "The \"streams\" argument must be a stream or a function",
                "ERR_INVALID_ARG_TYPE",
            ));
        };
        if current.is_object() {
            streams.push(current.clone(), context)?;
            let options = object(context);
            put(&options, "readable", at < last_index, context)?;
            put(&options, "writable", at > 0, context)?;
            let remaining = num(&st, "remaining", context)? + 1.;
            put(&st, "remaining", remaining, context)?;
            let pair = array(context, vec![st.clone().into(), (at == last_index).into()]);
            let on_done = closure(
                context,
                "onStreamDone",
                1,
                |_, args, pair, cx| {
                    let pair = list_of(pair, cx)?;
                    let st = pair[0].as_object().expect("the pipeline's state");
                    let error = arg(args, 0);
                    if !error.is_null_or_undefined() {
                        pipeline_finish(&st, error, cx)?;
                        return Ok(JsValue::undefined());
                    }
                    let remaining = num(&st, "remaining", cx)? - 1.;
                    put(&st, "remaining", remaining, cx)?;
                    if pair[1].to_boolean() || remaining <= 0. {
                        pipeline_finish(&st, JsValue::undefined(), cx)?;
                    }
                    Ok(JsValue::undefined())
                },
                pair,
            );
            eos(&current, &options.into(), on_done.into(), context)?;
            previous = current;
        }
    }
    if !last_value_promise.is_undefined() {
        let on_value = closure(
            context,
            "onValue",
            1,
            |_, args, st, cx| {
                put(st, "value", arg(args, 0), cx)?;
                pipeline_finish(st, JsValue::undefined(), cx)?;
                Ok(JsValue::undefined())
            },
            st.clone(),
        );
        let on_error = closure(
            context,
            "onError",
            1,
            |_, args, st, cx| {
                pipeline_finish(st, arg(args, 0), cx)?;
                Ok(JsValue::undefined())
            },
            st.clone(),
        );
        then(
            &last_value_promise,
            on_value.into(),
            on_error.into(),
            context,
        )?;
    }
    if signal.is_object() {
        put(&st, "signal", signal.clone(), context)?;
        let on_abort = closure(
            context,
            "onabort",
            0,
            |_, _, st, cx| {
                let reason = get(&sget(st, "signal", cx)?, "reason", cx)?;
                let error = abort_error(cx, reason);
                pipeline_finish(st, error, cx)?;
                Ok(JsValue::undefined())
            },
            st.clone(),
        );
        if get(&signal, "aborted", context)?.to_boolean() {
            crate::microtask(context, on_abort.into(), Vec::new());
        } else {
            invoke(
                &signal,
                "addEventListener",
                &[js_text("abort"), on_abort.into()],
                context,
            )?;
        }
    }
    Ok(previous)
}

fn pipeline_finish(st: &JsObject, error: JsValue, context: &mut Context) -> JsResult<()> {
    if flag(st, "done", context)? {
        return Ok(());
    }
    put(st, "done", true, context)?;
    if !error.is_null_or_undefined() {
        for stream in list_of(&sget(st, "streams", context)?, context)? {
            if get(&stream, "destroy", context)?.is_callable()
                && !get(&stream, "destroyed", context)?.to_boolean()
            {
                invoke(&stream, "destroy", std::slice::from_ref(&error), context)?;
            }
        }
    }
    let callback = sget(st, "callback", context)?;
    let value = sget(st, "value", context)?;
    let args = if error.is_null_or_undefined() {
        vec![JsValue::undefined(), value]
    } else {
        vec![error]
    };
    crate::microtask(context, callback, args);
    Ok(())
}

/// `stream/promises`' `pipeline`: the same, as a promise; a last argument that is not a stream
/// or a function is its options (`{ signal, end }`).
fn pipeline_promise(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut stages = args.to_vec();
    let mut signal = JsValue::undefined();
    if let Some(last) = stages.last()
        && last.is_object()
        && !last.is_callable()
        && !is_node_stream(last, context)?
        && !get(last, "next", context)?.is_callable()
        && last.as_object().is_some_and(|o| !o.is_array())
        && stages.len() > 1
    {
        let has_iterator = last
            .as_object()
            .map(|o| o.has_property(JsSymbol::iterator(), context))
            .transpose()?
            .unwrap_or(false)
            || last
                .as_object()
                .map(|o| o.has_property(JsSymbol::async_iterator(), context))
                .transpose()?
                .unwrap_or(false);
        if !has_iterator {
            let options = stages.pop().unwrap_or_default();
            signal = get(&options, "signal", context)?;
        }
    }
    if stages.len() == 1 && stages[0].as_object().is_some_and(|o| o.is_array()) {
        stages = list_of(&stages[0], context)?;
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    let pair = array(
        context,
        vec![resolvers.resolve.into(), resolvers.reject.into()],
    );
    let callback = closure(
        context,
        "callback",
        2,
        |_, args, pair, cx| {
            let pair = list_of(pair, cx)?;
            let error = arg(args, 0);
            if error.is_null_or_undefined() {
                crate::call(cx, &pair[0], &JsValue::undefined(), &[arg(args, 1)])
            } else {
                crate::call(cx, &pair[1], &JsValue::undefined(), &[error])
            }
        },
        pair.clone(),
    );
    if let Err(error) = run_pipeline(stages, callback.into(), signal, context) {
        let error = error.into_opaque(context)?;
        let pair = list_of(&pair, context)?;
        crate::call(context, &pair[1], &JsValue::undefined(), &[error])?;
    }
    Ok(promise.into())
}

fn finished_promise(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (stream, options) = (arg(args, 0), arg(args, 1));
    let (promise, resolvers) = JsPromise::new_pending(context);
    let pair = array(
        context,
        vec![resolvers.resolve.into(), resolvers.reject.into()],
    );
    let callback = closure(
        context,
        "callback",
        1,
        |_, args, pair, cx| {
            let pair = list_of(pair, cx)?;
            let error = arg(args, 0);
            if error.is_null_or_undefined() {
                crate::call(cx, &pair[0], &JsValue::undefined(), &[])
            } else {
                crate::call(cx, &pair[1], &JsValue::undefined(), &[error])
            }
        },
        pair,
    );
    eos(&stream, &options, callback.into(), context)?;
    Ok(promise.into())
}

/// `addAbortSignal(signal, stream)`: the stream destroyed with an `AbortError` when the
/// signal aborts.
fn add_abort_signal(
    signal: &JsValue,
    stream: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    if get(signal, "aborted", context)?.to_boolean() {
        let reason = get(signal, "reason", context)?;
        let error = abort_error(context, reason);
        invoke(stream, "destroy", &[error], context)?;
        return Ok(stream.clone());
    }
    let pair = array(context, vec![signal.clone(), stream.clone()]);
    let on_abort = closure(
        context,
        "onAbort",
        0,
        |_, _, pair, cx| {
            let pair = list_of(pair, cx)?;
            let reason = get(&pair[0], "reason", cx)?;
            let error = abort_error(cx, reason);
            invoke(&pair[1], "destroy", &[error], cx)
        },
        pair,
    );
    if get(signal, "addEventListener", context)?.is_callable() {
        invoke(
            signal,
            "addEventListener",
            &[js_text("abort"), on_abort.into()],
            context,
        )?;
    }
    Ok(stream.clone())
}

// ─── The module ──────────────────────────────────────────────────────────────────────────────

const MODULE_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("pipeline", 2, pipeline),
    ("finished", 3, finished),
    ("addAbortSignal", 2, |_, args, cx| {
        add_abort_signal(&arg(args, 0), &arg(args, 1), cx)
    }),
    ("isReadable", 1, |_, args, cx| {
        let stream = arg(args, 0);
        if rstate(&stream, cx)?.is_none() {
            return Ok(JsValue::null());
        }
        get(&stream, "readable", cx)
    }),
    ("isWritable", 1, |_, args, cx| {
        let stream = arg(args, 0);
        if wstate(&stream, cx)?.is_none() {
            return Ok(JsValue::null());
        }
        get(&stream, "writable", cx)
    }),
    ("isErrored", 1, |_, args, cx| {
        let stream = arg(args, 0);
        let mut errored = false;
        for state in [rstate(&stream, cx)?, wstate(&stream, cx)?]
            .into_iter()
            .flatten()
        {
            errored |= flag(&state, "errorEmitted", cx)?
                || !sget(&state, "errored", cx)?.is_null_or_undefined();
        }
        Ok(errored.into())
    }),
    ("isDisturbed", 1, |_, args, cx| {
        let stream = arg(args, 0);
        Ok(match rstate(&stream, cx)? {
            Some(r) => (flag(&r, "dataEmitted", cx)?
                || flag(&r, "endEmitted", cx)?
                || flag(&r, "destroyed", cx)?)
            .into(),
            None => false.into(),
        })
    }),
    ("isDestroyed", 1, |_, args, cx| {
        get(&arg(args, 0), "destroyed", cx)
    }),
    ("getDefaultHighWaterMark", 1, |_, args, cx| {
        Ok(default_high_water_mark(arg(args, 0).to_boolean(), cx)?.into())
    }),
    ("setDefaultHighWaterMark", 2, |_, args, cx| {
        let value = arg(args, 1).to_number(cx)?;
        if !value.is_finite() || value < 0. {
            return Err(coded_type_error(
                cx,
                "The value of \"value\" is invalid.",
                "ERR_INVALID_ARG_VALUE",
            ));
        }
        let module = module_value(cx)?;
        let key = if arg(args, 0).to_boolean() {
            "__defaultObjectHighWaterMark"
        } else {
            "__defaultHighWaterMark"
        };
        if let Some(module) = module.as_object() {
            hidden(&module, JsString::from(key), value.floor(), cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("_isUint8Array", 1, |_, args, _| {
        Ok(is_bytes(&arg(args, 0)).into())
    }),
    ("_uint8ArrayToBuffer", 1, |_, args, cx| {
        to_buffer(&arg(args, 0), &JsValue::undefined(), cx)
    }),
];

/// `require('stream')`: the legacy `Stream` class, with every other class and the functions
/// on it — `require('stream').Readable`, `require('stream').Stream === require('stream')`.
pub(crate) fn stream_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let emitter = crate::events::emitter_class(context)?;
    let (stream, stream_prototype) = class(
        context,
        ClassSpec {
            name: "Stream",
            length: 1,
            init: |target, args, cx| stream_setup(target, &arg(args, 0), cx),
            parent: Some(&emitter),
            ..Default::default()
        },
    )?;
    crate::native::exports(context, &stream_prototype, STREAM_METHODS)?;

    let (readable, readable_prototype) = class(
        context,
        ClassSpec {
            name: "Readable",
            length: 1,
            init: readable_init,
            parent: Some(&stream),
            accessors: READABLE_ACCESSORS,
            statics: &[("from", 2, readable_from)],
            ..Default::default()
        },
    )?;
    crate::native::exports(context, &readable_prototype, READABLE_METHODS)?;
    crate::native::accessors(context, &readable_prototype, COMMON_ACCESSORS)?;
    let iterator = function(context, "[Symbol.asyncIterator]", 0, async_iterator);
    hidden(
        &readable_prototype,
        JsSymbol::async_iterator(),
        iterator,
        context,
    )?;
    let (readable_state, _) = class(
        context,
        ClassSpec {
            name: "ReadableState",
            ..Default::default()
        },
    )?;
    readable.set(js_string!("ReadableState"), readable_state, false, context)?;

    let (writable, writable_prototype) = class(
        context,
        ClassSpec {
            name: "Writable",
            length: 1,
            init: writable_init,
            parent: Some(&stream),
            accessors: WRITABLE_ACCESSORS,
            ..Default::default()
        },
    )?;
    crate::native::exports(context, &writable_prototype, WRITABLE_METHODS)?;
    crate::native::accessors(context, &writable_prototype, COMMON_ACCESSORS)?;
    writable_prototype.set(js_string!("_writev"), JsValue::null(), false, context)?;
    let cannot_pipe = function(context, "pipe", 0, |this, _, cx| {
        let error = error_value(cx, "Cannot pipe, not readable", "ERR_STREAM_CANNOT_PIPE");
        error_or_destroy(this, error, false, cx)?;
        Ok(JsValue::undefined())
    });
    writable_prototype.set(js_string!("pipe"), cannot_pipe, false, context)?;
    let (writable_state, _) = class(
        context,
        ClassSpec {
            name: "WritableState",
            ..Default::default()
        },
    )?;
    writable.set(js_string!("WritableState"), writable_state, false, context)?;
    // A `Duplex` is a `Writable` too: anything with a writable side of this module's.
    let has_instance = function(context, "[Symbol.hasInstance]", 1, |this, args, cx| {
        let value = arg(args, 0);
        let Some(class) = this.as_object() else {
            return Ok(false.into());
        };
        let prototype = class.get(js_string!("prototype"), cx)?;
        if let Some(prototype) = prototype.as_object()
            && crate::native::is_instance(&value, &prototype)
        {
            return Ok(true.into());
        }
        let writable = module_class(cx, "Writable")?;
        if !JsObject::equals(&class, &writable) {
            return Ok(false.into());
        }
        let state = get(&value, "_writableState", cx)?;
        let state_prototype = state_prototype(cx, "WritableState")?;
        Ok(crate::native::is_instance(&state, &state_prototype).into())
    });
    let symbol = JsSymbol::has_instance();
    writable.define_property_or_throw(
        symbol,
        boa_engine::property::PropertyDescriptor::builder()
            .value(has_instance)
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;

    let (duplex, duplex_prototype) = class(
        context,
        ClassSpec {
            name: "Duplex",
            length: 1,
            init: duplex_init,
            parent: Some(&readable),
            accessors: WRITABLE_ACCESSORS,
            ..Default::default()
        },
    )?;
    // Writable's methods, as Node copies them onto Duplex.
    crate::native::exports(context, &duplex_prototype, &WRITABLE_METHODS[..6])?;
    crate::native::accessors(context, &duplex_prototype, COMMON_ACCESSORS)?;
    duplex_prototype.set(js_string!("_writev"), JsValue::null(), false, context)?;

    let (transform, transform_prototype) = class(
        context,
        ClassSpec {
            name: "Transform",
            length: 1,
            init: transform_init,
            parent: Some(&duplex),
            ..Default::default()
        },
    )?;
    crate::native::exports(context, &transform_prototype, TRANSFORM_METHODS)?;
    let final_fn = function(context, "final", 1, transform_final);
    final_fn.set(js_string!("__isTransformFinal"), true, false, context)?;
    transform_prototype.set(js_string!("_final"), final_fn, false, context)?;

    let (pass_through, pass_through_prototype) = class(
        context,
        ClassSpec {
            name: "PassThrough",
            length: 1,
            init: transform_init,
            parent: Some(&transform),
            ..Default::default()
        },
    )?;
    let pass = function(context, "_transform", 3, |_, args, cx| {
        crate::call(
            cx,
            &arg(args, 2),
            &JsValue::undefined(),
            &[JsValue::null(), arg(args, 0)],
        )
    });
    pass_through_prototype.set(js_string!("_transform"), pass, false, context)?;

    for (name, value) in [
        ("Stream", stream.clone()),
        ("Readable", readable),
        ("Writable", writable),
        ("Duplex", duplex),
        ("Transform", transform),
        ("PassThrough", pass_through),
    ] {
        stream.set(JsString::from(name), value, false, context)?;
    }
    crate::native::exports(context, &stream, MODULE_FUNCTIONS)?;
    let eos_fn = stream.get(js_string!("finished"), context)?;
    stream.set(js_string!("eos"), eos_fn, false, context)?;
    let promises = promises_object(context)?;
    stream.set(js_string!("promises"), promises, false, context)?;
    hidden(
        &stream,
        js_string!("__defaultHighWaterMark"),
        65536,
        context,
    )?;
    hidden(
        &stream,
        js_string!("__defaultObjectHighWaterMark"),
        16,
        context,
    )?;
    Ok(stream.into())
}

fn promises_object(context: &mut Context) -> JsResult<JsObject> {
    let promises = object(context);
    crate::native::exports(
        context,
        &promises,
        &[
            ("pipeline", 2, pipeline_promise),
            ("finished", 2, finished_promise),
        ],
    )?;
    Ok(promises)
}

/// `require('stream/promises')`.
pub(crate) fn promises_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let stream = crate::modules::node_module(context, "stream")?;
    get(&stream, "promises", context)
}

/// `require('_stream_readable')` and the other old names: the class itself.
pub(crate) fn legacy_module(context: &mut Context, name: &str) -> JsResult<JsValue> {
    let class = match name {
        "_stream_readable" => "Readable",
        "_stream_writable" => "Writable",
        "_stream_duplex" => "Duplex",
        "_stream_transform" => "Transform",
        _ => "PassThrough",
    };
    Ok(module_class(context, class)?.into())
}

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
pub(crate) fn create_read_stream(context: &mut Context, args: &[JsValue]) -> JsResult<JsValue> {
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
    let read = function(context, "_read", 1, |this, args, cx| {
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
        let outcome = (|| -> std::io::Result<Vec<u8>> {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(&path)?;
            file.seek(SeekFrom::Start(position as u64))?;
            let mut bytes = vec![0u8; size];
            let mut filled = 0;
            while filled < size {
                let n = file.read(&mut bytes[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            bytes.truncate(filled);
            Ok(bytes)
        })();
        match outcome {
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
    });
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
    next_tick(
        context,
        |_, args, cx| {
            let stream = arg(args, 0);
            let path = text_of(&get(&stream, "path", cx)?, cx)?;
            match std::fs::File::open(PathBuf::from(&path)) {
                Ok(_) => {
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
        },
        vec![stream.clone().into()],
    );
    Ok(stream.into())
}

/// `fs.createWriteStream(path, { flags })`: a `Writable` writing each chunk to the file as it
/// comes — the file made empty first (`'w'`), or added to (`'a'`). Says `open` and `ready` a
/// tick after it is made, `finish` and `close` when ended.
pub(crate) fn create_write_stream(context: &mut Context, args: &[JsValue]) -> JsResult<JsValue> {
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
    let opened = if flags.starts_with('a') {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
    };
    let failure = opened
        .err()
        .map(|error| fs_error(context, &error, &path, "open"));
    let write = function(context, "_write", 3, |this, args, cx| {
        let path = text_of(&get(this, "path", cx)?, cx)?;
        let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
        let callback = arg(args, 2);
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
    });
    stream.set(js_string!("_write"), write, false, context)?;
    let close = function(context, "close", 1, |this, args, cx| {
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
    });
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

#[cfg(test)]
mod tests {
    use crate::RuntimeX;

    /// The file streams, reached through a global made for the test: `fs` is not this
    /// module's to wire.
    #[test]
    fn file_streams_read_and_write_the_disk_a_chunk_at_a_time() {
        let mut rt = RuntimeX::new();
        let dir = std::env::temp_dir().join(format!("runtime-x-fs-streams-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.txt");
        std::fs::write(&source, "0123456789".repeat(1000)).unwrap();
        let target = dir.join("target.txt");
        let context = rt.context();
        let read = crate::native::function(context, "createReadStream", 2, |_, args, cx| {
            super::create_read_stream(cx, args)
        });
        let write = crate::native::function(context, "createWriteStream", 2, |_, args, cx| {
            super::create_write_stream(cx, args)
        });
        context
            .global_object()
            .set(
                boa_engine::js_string!("createReadStream"),
                read,
                false,
                context,
            )
            .unwrap();
        context
            .global_object()
            .set(
                boa_engine::js_string!("createWriteStream"),
                write,
                false,
                context,
            )
            .unwrap();
        let script = format!(
            r#"
            var log = [];
            var src = createReadStream({source:?}, {{ highWaterMark: 4096 }});
            var dst = createWriteStream({target:?});
            var chunks = 0;
            src.on('open', () => log.push('open'));
            src.on('data', () => chunks++);
            dst.on('finish', () => log.push('finish ' + dst.bytesWritten));
            dst.on('close', () => log.push('close'));
            src.pipe(dst);
            var part = '';
            createReadStream({source:?}, {{ start: 2, end: 5, encoding: 'utf8' }})
                .on('data', d => part += d).on('end', () => log.push('part ' + part));
            createReadStream({missing:?}).on('error', e => log.push(e.code));
            "#,
            source = source.to_string_lossy(),
            target = target.to_string_lossy(),
            missing = dir.join("missing.txt").to_string_lossy(),
        );
        rt.eval(&script).unwrap();
        for _ in 0..20 {
            rt.run_microtasks();
        }
        let log = crate::print(&rt.eval("log.join(',') + ' chunks ' + chunks").unwrap());
        assert!(log.contains("open"), "{log}");
        assert!(log.contains("part 2345"), "{log}");
        assert!(log.contains("ENOENT"), "{log}");
        assert!(log.contains("finish 10000"), "{log}");
        assert!(log.contains("close"), "{log}");
        assert!(log.ends_with("chunks 3"), "{log}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "0123456789".repeat(1000)
        );
    }
}
