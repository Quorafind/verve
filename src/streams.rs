//! The WHATWG Streams standard: `ReadableStream`, `WritableStream` and `TransformStream`, their
//! readers, writers and controllers, the two queuing strategies, and `TextEncoderStream` and
//! `TextDecoderStream` — what bundled npm code reaches for when it streams bytes the web's way
//! (zip.js's codecs are `TransformStream`s, the AI SDKs read responses with `getReader()`,
//! `eventsource-parser` pipes through a `TransformStream`).
//!
//! Each stream keeps its state in Rust, on the object JavaScript holds: a `ReadableStream` is an
//! object made with a [`Readable`] inside it, so a subclass (`class Lines extends
//! TransformStream`) is one too — `super()` hands the subclass's `new.target` to the native
//! constructor, which makes the object on its prototype. The algorithms are the standard's,
//! simplified where nothing observable changes: queues, backpressure by `highWaterMark` and
//! `size()`, `pull` called while the queue wants more, `pipeTo` and `tee` built on the same
//! readers and writers a script would use. Byte streams (`type: "bytes"`) queue their chunks as
//! bytes, and a BYOB reader's `read(view)` fills the view from them; a controller's
//! `byobRequest` is always `null`, which sources written for both kinds read as "enqueue".
//!
//! Every class is made the first time any of them is read from the global object, and kept for
//! the realm.

use std::collections::VecDeque;
use std::rc::Rc;

use boa_engine::{
    Context, JsData, JsError, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction,
    builtins::promise::ResolvingFunctions,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArrayBuffer, JsFunction, JsPromise, JsUint8Array},
    },
    property::PropertyDescriptor,
};
use boa_gc::{Finalize, Trace};

use crate::native::{arg, array, get, hidden, invoke, js_text, object, text_of, type_error};

// ─── Promises from Rust ─────────────────────────────────────────────────────────────────────

/// A promise not yet settled, and the functions that settle it.
pub(crate) fn pending(context: &mut Context) -> (JsObject, ResolvingFunctions) {
    let (promise, resolvers) = JsPromise::new_pending(context);
    (promise.into(), resolvers)
}

fn resolve(resolvers: &ResolvingFunctions, value: JsValue, context: &mut Context) {
    let _ = resolvers
        .resolve
        .call(&JsValue::undefined(), &[value], context);
}

fn reject(resolvers: &ResolvingFunctions, reason: JsValue, context: &mut Context) {
    let _ = resolvers
        .reject
        .call(&JsValue::undefined(), &[reason], context);
}

fn resolved(value: JsValue, context: &mut Context) -> JsResult<JsValue> {
    Ok(JsPromise::resolve(value, context)?.into())
}

fn rejected(reason: JsValue, context: &mut Context) -> JsResult<JsValue> {
    Ok(JsPromise::reject(JsError::from_opaque(reason), context)?.into())
}

/// An error as the value a promise is rejected with.
fn error_value(error: JsError, context: &mut Context) -> JsValue {
    error.into_opaque(context).unwrap_or_default()
}

fn type_error_value(message: &str, context: &mut Context) -> JsValue {
    error_value(type_error(message), context)
}

/// What a reader's `read()` answers: `{ value, done }`.
fn step_result(value: JsValue, done: bool, context: &mut Context) -> JsValue {
    let result = object(context);
    let _ = result.set(js_string!("value"), value, false, context);
    let _ = result.set(js_string!("done"), done, false, context);
    result.into()
}

type Reaction<T> = fn(&T, JsValue, &mut Context) -> JsResult<JsValue>;

/// `Promise.resolve(value).then(ok, err)`, with native reactions that are handed `captures`:
/// answers the promise `then` makes.
pub(crate) fn when<T: Trace + Clone + 'static>(
    context: &mut Context,
    value: JsValue,
    captures: T,
    ok: Reaction<T>,
    err: Reaction<T>,
) -> JsResult<JsObject> {
    let promise = JsPromise::resolve(value, context)?;
    let on_ok = reaction(context, captures.clone(), ok);
    let on_err = reaction(context, captures, err);
    Ok(promise.then(Some(on_ok), Some(on_err), context)?.into())
}

fn reaction<T: Trace + 'static>(context: &mut Context, captures: T, f: Reaction<T>) -> JsFunction {
    FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            move |_, args, captures: &T, context| f(captures, arg(args, 0), context),
            captures,
        ),
    )
    .length(1)
    .build()
}

/// A reaction that rethrows: a rejection kept a rejection.
fn rethrow<T>(_: &T, reason: JsValue, _: &mut Context) -> JsResult<JsValue> {
    Err(JsError::from_opaque(reason))
}

fn ignore<T>(_: &T, _: JsValue, _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

/// Calls `method` of an underlying source, sink or transformer with it as `this`, and answers a
/// promise of what it answered — rejected if it threw, resolved with nothing if there is no such
/// method.
fn promise_call(
    target: &JsValue,
    method: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if method.is_undefined() {
        return resolved(JsValue::undefined(), context);
    }
    match crate::call(context, method, target, args) {
        Ok(value) => resolved(value, context),
        Err(error) => {
            let reason = error_value(error, context);
            rejected(reason, context)
        }
    }
}

/// The method `name` of `target`, which must be a function or absent.
fn method_of(target: &JsValue, name: &str, context: &mut Context) -> JsResult<JsValue> {
    let method = get(target, name, context)?;
    if !method.is_undefined() && !method.is_callable() {
        return Err(type_error(format!("'{name}' must be a function")));
    }
    Ok(method)
}

/// A native function holding `captures`.
fn native<T: Trace + 'static>(
    context: &mut Context,
    name: &str,
    length: usize,
    captures: T,
    f: fn(&JsValue, &[JsValue], &T, &mut Context) -> JsResult<JsValue>,
) -> JsValue {
    crate::native::closure(context, name, length, f, captures).into()
}

// ─── Bytes ──────────────────────────────────────────────────────────────────────────────────

fn u8array(bytes: Vec<u8>, context: &mut Context) -> JsResult<JsValue> {
    Ok(JsUint8Array::from_iter(bytes, context)?.into())
}

/// Whether `value` is an `ArrayBuffer` view — a typed array or a `DataView`.
fn is_view(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    let Some(object) = value.as_object() else {
        return Ok(false);
    };
    if JsArrayBuffer::from_object(object.clone()).is_ok() {
        return Ok(false);
    }
    let buffer = object.get(js_string!("buffer"), context)?;
    Ok(buffer
        .as_object()
        .is_some_and(|b| JsArrayBuffer::from_object(b.clone()).is_ok()))
}

/// Writes `bytes` into the memory `view` looks at, from its start.
fn write_into(view: &JsObject, bytes: &[u8], context: &mut Context) -> JsResult<()> {
    let buffer = view.get(js_string!("buffer"), context)?;
    let offset = view
        .get(js_string!("byteOffset"), context)?
        .to_length(context)? as usize;
    let buffer = buffer
        .as_object()
        .and_then(|b| JsArrayBuffer::from_object(b.clone()).ok())
        .ok_or_else(|| type_error("the view's buffer is detached"))?;
    let mut data = buffer
        .data_mut()
        .ok_or_else(|| type_error("the view's buffer is detached"))?;
    let end = (offset + bytes.len()).min(data.len());
    data[offset..end].copy_from_slice(&bytes[..end - offset]);
    Ok(())
}

/// A view of the first `count` bytes `view` looks at, of `view`'s own kind.
fn view_of_first(view: &JsObject, count: usize, context: &mut Context) -> JsResult<JsValue> {
    let constructor = view.get(js_string!("constructor"), context)?;
    let buffer = view.get(js_string!("buffer"), context)?;
    let offset = view.get(js_string!("byteOffset"), context)?;
    let per = view.get(js_string!("BYTES_PER_ELEMENT"), context)?;
    let per = if per.is_undefined() {
        1
    } else {
        per.to_length(context)?.max(1) as usize
    };
    match constructor.as_object().filter(|c| c.is_constructor()) {
        Some(constructor) => Ok(constructor
            .construct(&[buffer, offset, (count / per).into()], None, context)?
            .into()),
        None => {
            let uint8 = context
                .intrinsics()
                .constructors()
                .typed_uint8_array()
                .constructor();
            Ok(uint8
                .construct(&[buffer, offset, count.into()], None, context)?
                .into())
        }
    }
}

// ─── The classes, made once ─────────────────────────────────────────────────────────────────

/// Every class of the standard, for the realm: made at the first touch of any of them.
pub(crate) struct Classes {
    pub readable: JsObject,
    readable_proto: JsObject,
    pub default_reader: JsObject,
    reader_proto: JsObject,
    pub byob_reader: JsObject,
    byob_reader_proto: JsObject,
    pub default_controller: JsObject,
    controller_proto: JsObject,
    pub byte_controller: JsObject,
    byte_controller_proto: JsObject,
    pub writable: JsObject,
    writable_proto: JsObject,
    pub writer: JsObject,
    writer_proto: JsObject,
    pub writable_controller: JsObject,
    writable_controller_proto: JsObject,
    pub transform: JsObject,
    transform_proto: JsObject,
    pub transform_controller: JsObject,
    transform_controller_proto: JsObject,
    iterator_proto: JsObject,
    pub count_strategy: JsObject,
    pub byte_strategy: JsObject,
    pub text_encoder_stream: JsObject,
    text_encoder_proto: JsObject,
    pub text_decoder_stream: JsObject,
    text_decoder_proto: JsObject,
}

pub(crate) fn classes(context: &mut Context) -> JsResult<Rc<Classes>> {
    if let Some(classes) = context.get_data::<Rc<Classes>>() {
        return Ok(classes.clone());
    }
    let classes = Rc::new(make_classes(context)?);
    context.insert_data(classes.clone());
    Ok(classes)
}

/// A web class: a constructor that must be called with `new` (`construct` is handed
/// `new.target` as `this`), its prototype with `methods` and `accessors`, and `statics`.
fn web_class(
    context: &mut Context,
    name: &str,
    length: usize,
    construct: NativeFunctionPointer,
    methods: &[(&str, usize, NativeFunctionPointer)],
    accessors: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)],
    statics: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<(JsObject, JsObject)> {
    let constructor: JsObject =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(construct))
            .name(JsString::from(name))
            .length(length)
            .constructor(true)
            .build()
            .into();
    let prototype = object(context);
    crate::native::methods(context, &prototype, methods)?;
    crate::native::accessors(context, &prototype, accessors)?;
    hidden(
        &prototype,
        js_string!("constructor"),
        constructor.clone(),
        context,
    )?;
    prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(js_text(name))
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    crate::native::methods(context, &constructor, statics)?;
    Ok((constructor, prototype))
}

/// A web class's own method under a symbol: `[Symbol.asyncIterator]`.
fn symbol_method(
    context: &mut Context,
    target: &JsObject,
    symbol: JsSymbol,
    name: &str,
    f: NativeFunctionPointer,
) -> JsResult<()> {
    let method = crate::native::function(context, name, 0, f);
    target.define_property_or_throw(
        symbol,
        PropertyDescriptor::builder()
            .value(method)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    Ok(())
}

fn illegal(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(type_error("Illegal constructor"))
}

/// The prototype the object a constructor makes is made on: `new.target`'s, which `new` (or a
/// subclass's `super()`) hands a native constructor as `this`.
fn prototype_for(
    new_target: &JsValue,
    name: &str,
    fallback: JsObject,
    context: &mut Context,
) -> JsResult<JsObject> {
    let Some(new_target) = new_target.as_object() else {
        return Err(type_error(format!(
            "Class constructor {name} cannot be invoked without 'new'"
        )));
    };
    Ok(new_target
        .get(js_string!("prototype"), context)?
        .as_object()
        .unwrap_or(fallback))
}

fn make_classes(context: &mut Context) -> JsResult<Classes> {
    let (readable, readable_proto) = web_class(
        context,
        "ReadableStream",
        0,
        construct_readable,
        READABLE_METHODS,
        &[(
            "locked",
            |this, _, _| {
                Ok(readable_of(this)?
                    .downcast_ref::<Readable>()
                    .is_some_and(|r| r.reader.is_some())
                    .into())
            },
            None,
        )],
        &[("from", 1, readable_from)],
    )?;
    symbol_method(
        context,
        &readable_proto,
        JsSymbol::async_iterator(),
        "[Symbol.asyncIterator]",
        readable_values,
    )?;
    let (default_reader, reader_proto) = web_class(
        context,
        "ReadableStreamDefaultReader",
        1,
        |this, args, cx| {
            let proto = classes(cx)?.reader_proto.clone();
            let proto = prototype_for(this, "ReadableStreamDefaultReader", proto, cx)?;
            let stream = readable_of(&arg(args, 0))?;
            Ok(acquire_reader_on(cx, &stream, false, proto)?.into())
        },
        READER_METHODS,
        &[("closed", reader_closed, None)],
        &[],
    )?;
    let (byob_reader, byob_reader_proto) = web_class(
        context,
        "ReadableStreamBYOBReader",
        1,
        |this, args, cx| {
            let proto = classes(cx)?.byob_reader_proto.clone();
            let proto = prototype_for(this, "ReadableStreamBYOBReader", proto, cx)?;
            let stream = readable_of(&arg(args, 0))?;
            Ok(acquire_reader_on(cx, &stream, true, proto)?.into())
        },
        BYOB_READER_METHODS,
        &[("closed", reader_closed, None)],
        &[],
    )?;
    let (default_controller, controller_proto) = web_class(
        context,
        "ReadableStreamDefaultController",
        0,
        illegal,
        CONTROLLER_METHODS,
        &[("desiredSize", controller_desired_size, None)],
        &[],
    )?;
    let (byte_controller, byte_controller_proto) = web_class(
        context,
        "ReadableByteStreamController",
        0,
        illegal,
        CONTROLLER_METHODS,
        &[
            ("desiredSize", controller_desired_size, None),
            ("byobRequest", |_, _, _| Ok(JsValue::null()), None),
        ],
        &[],
    )?;
    let (writable, writable_proto) = web_class(
        context,
        "WritableStream",
        0,
        construct_writable,
        WRITABLE_METHODS,
        &[(
            "locked",
            |this, _, _| {
                Ok(writable_of(this)?
                    .downcast_ref::<Writable>()
                    .is_some_and(|w| w.writer.is_some())
                    .into())
            },
            None,
        )],
        &[],
    )?;
    let (writer, writer_proto) = web_class(
        context,
        "WritableStreamDefaultWriter",
        1,
        |this, args, cx| {
            let proto = classes(cx)?.writer_proto.clone();
            let proto = prototype_for(this, "WritableStreamDefaultWriter", proto, cx)?;
            let stream = writable_of(&arg(args, 0))?;
            Ok(acquire_writer_on(cx, &stream, proto)?.into())
        },
        WRITER_METHODS,
        &[
            (
                "ready",
                |this, _, _| {
                    Ok(writer_of(this)?
                        .downcast_ref::<Writer>()
                        .map(|w| JsValue::from(w.ready.clone()))
                        .unwrap_or_default())
                },
                None,
            ),
            (
                "closed",
                |this, _, _| {
                    Ok(writer_of(this)?
                        .downcast_ref::<Writer>()
                        .map(|w| JsValue::from(w.closed.clone()))
                        .unwrap_or_default())
                },
                None,
            ),
            ("desiredSize", writer_desired_size, None),
        ],
        &[],
    )?;
    let (writable_controller, writable_controller_proto) = web_class(
        context,
        "WritableStreamDefaultController",
        0,
        illegal,
        &[("error", 1, |this, args, cx| {
            let stream = controlled_writable(this)?;
            if with_writable(&stream, |w| w.phase) == WPhase::Writable {
                start_erroring(cx, &stream, arg(args, 0));
            }
            Ok(JsValue::undefined())
        })],
        &[(
            "signal",
            |this, _, cx| {
                let stream = controlled_writable(this)?;
                let controller = with_writable(&stream, |w| w.abort_controller.clone());
                get(&controller, "signal", cx)
            },
            None,
        )],
        &[],
    )?;
    let (transform, transform_proto) = web_class(
        context,
        "TransformStream",
        0,
        construct_transform,
        &[],
        &[
            (
                "readable",
                |this, _, _| {
                    Ok(transform_of(this)?
                        .downcast_ref::<Transform>()
                        .and_then(|t| t.readable.clone())
                        .map(JsValue::from)
                        .unwrap_or_default())
                },
                None,
            ),
            (
                "writable",
                |this, _, _| {
                    Ok(transform_of(this)?
                        .downcast_ref::<Transform>()
                        .and_then(|t| t.writable.clone())
                        .map(JsValue::from)
                        .unwrap_or_default())
                },
                None,
            ),
        ],
        &[],
    )?;
    let (transform_controller, transform_controller_proto) = web_class(
        context,
        "TransformStreamDefaultController",
        0,
        illegal,
        TRANSFORM_CONTROLLER_METHODS,
        &[(
            "desiredSize",
            |this, _, _| {
                let transform = controlled_transform(this)?;
                let readable =
                    with_transform(&transform, |t| t.readable.clone()).expect("a readable side");
                Ok(desired_size(&readable).map_or(JsValue::null(), JsValue::from))
            },
            None,
        )],
        &[],
    )?;
    let iterator_proto = object(context);
    let async_iterator = context
        .intrinsics()
        .objects()
        .iterator_prototypes()
        .async_iterator();
    iterator_proto.set_prototype(Some(async_iterator));
    crate::native::methods(context, &iterator_proto, ITERATOR_METHODS)?;
    let (count_strategy, _) = web_class(
        context,
        "CountQueuingStrategy",
        1,
        |this, args, cx| construct_strategy(this, args, cx, "CountQueuingStrategy"),
        &[("size", 0, |_, _, _| Ok(1.into()))],
        &[],
        &[],
    )?;
    let (byte_strategy, _) = web_class(
        context,
        "ByteLengthQueuingStrategy",
        1,
        |this, args, cx| construct_strategy(this, args, cx, "ByteLengthQueuingStrategy"),
        &[("size", 1, |_, args, cx| {
            get(&arg(args, 0), "byteLength", cx)
        })],
        &[],
        &[],
    )?;
    let (text_encoder_stream, text_encoder_proto) = pair_class(
        context,
        "TextEncoderStream",
        0,
        construct_text_encoder_stream,
        &[("encoding", |_, _, _| Ok(js_text("utf-8")), None)],
    )?;
    let (text_decoder_stream, text_decoder_proto) = pair_class(
        context,
        "TextDecoderStream",
        0,
        construct_text_decoder_stream,
        &[
            (
                "encoding",
                |this, _, cx| pair_extra(this, "encoding", cx),
                None,
            ),
            ("fatal", |this, _, cx| pair_extra(this, "fatal", cx), None),
            (
                "ignoreBOM",
                |this, _, cx| pair_extra(this, "ignoreBOM", cx),
                None,
            ),
        ],
    )?;
    Ok(Classes {
        readable,
        readable_proto,
        default_reader,
        reader_proto,
        byob_reader,
        byob_reader_proto,
        default_controller,
        controller_proto,
        byte_controller,
        byte_controller_proto,
        writable,
        writable_proto,
        writer,
        writer_proto,
        writable_controller,
        writable_controller_proto,
        transform,
        transform_proto,
        transform_controller,
        transform_controller_proto,
        iterator_proto,
        count_strategy,
        byte_strategy,
        text_encoder_stream,
        text_encoder_proto,
        text_decoder_stream,
        text_decoder_proto,
    })
}

/// A queuing strategy: its `highWaterMark`, which it must be given, as its own.
fn construct_strategy(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    name: &str,
) -> JsResult<JsValue> {
    let Some(new_target) = this.as_object() else {
        return Err(type_error(format!(
            "Class constructor {name} cannot be invoked without 'new'"
        )));
    };
    let init = arg(args, 0);
    if !init.is_object() {
        return Err(type_error(format!(
            "{name}: the argument must be an object with a highWaterMark"
        )));
    }
    let mark = get(&init, "highWaterMark", context)?;
    if mark.is_undefined() {
        return Err(type_error(format!("{name}: highWaterMark is required")));
    }
    let mark = mark.to_number(context)?;
    let strategy = object(context);
    if let Some(prototype) = new_target
        .get(js_string!("prototype"), context)?
        .as_object()
    {
        strategy.set_prototype(Some(prototype));
    }
    strategy.define_property_or_throw(
        js_string!("highWaterMark"),
        PropertyDescriptor::builder()
            .value(mark)
            .writable(false)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    Ok(strategy.into())
}

/// A strategy's high-water mark (`default` when it names none) and its `size`.
fn strategy_of(
    strategy: &JsValue,
    default: f64,
    context: &mut Context,
) -> JsResult<(f64, JsValue)> {
    let mark = get(strategy, "highWaterMark", context)?;
    let mark = if mark.is_undefined() {
        default
    } else {
        mark.to_number(context)?
    };
    if mark.is_nan() || mark < 0. {
        return Err(crate::native::range_error("Invalid highWaterMark"));
    }
    let size = get(strategy, "size", context)?;
    if !size.is_undefined() && !size.is_callable() {
        return Err(type_error("'size' must be a function"));
    }
    Ok((mark, size))
}

// ─── ReadableStream ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Open,
    Closed,
    Errored,
}

#[derive(Trace, Finalize)]
struct Chunk {
    value: JsValue,
    #[unsafe_ignore_trace]
    size: f64,
}

/// A `read()` waiting for a chunk: a default reader's, or a BYOB reader's with the view to fill.
#[derive(Trace, Finalize)]
enum ReadRequest {
    Chunk(ResolvingFunctions),
    Into(ResolvingFunctions, JsObject),
}

impl ReadRequest {
    fn resolvers(&self) -> &ResolvingFunctions {
        match self {
            ReadRequest::Chunk(r) | ReadRequest::Into(r, _) => r,
        }
    }
}

/// A `ReadableStream`, and its controller's state: the queue, the strategy, the source.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct Readable {
    #[unsafe_ignore_trace]
    phase: Phase,
    error: JsValue,
    reader: Option<JsObject>,
    #[unsafe_ignore_trace]
    disturbed: bool,
    controller: Option<JsObject>,
    #[unsafe_ignore_trace]
    bytes: bool,
    queue: VecDeque<Chunk>,
    #[unsafe_ignore_trace]
    queue_size: f64,
    #[unsafe_ignore_trace]
    started: bool,
    #[unsafe_ignore_trace]
    close_requested: bool,
    #[unsafe_ignore_trace]
    pulling: bool,
    #[unsafe_ignore_trace]
    pull_again: bool,
    #[unsafe_ignore_trace]
    high_water_mark: f64,
    size: JsValue,
    source: JsValue,
    pull: JsValue,
    cancel: JsValue,
    requests: VecDeque<ReadRequest>,
}

impl Readable {
    fn new() -> Self {
        Readable {
            phase: Phase::Open,
            error: JsValue::undefined(),
            reader: None,
            disturbed: false,
            controller: None,
            bytes: false,
            queue: VecDeque::new(),
            queue_size: 0.,
            started: false,
            close_requested: false,
            pulling: false,
            pull_again: false,
            high_water_mark: 1.,
            size: JsValue::undefined(),
            source: JsValue::undefined(),
            pull: JsValue::undefined(),
            cancel: JsValue::undefined(),
            requests: VecDeque::new(),
        }
    }
}

/// The stream's state, changed by `f` — never across a call into JavaScript.
fn with_readable<R>(stream: &JsObject, f: impl FnOnce(&mut Readable) -> R) -> R {
    f(&mut stream.downcast_mut::<Readable>().expect("a ReadableStream"))
}

/// `value` as a `ReadableStream`, or the TypeError a method called on something else throws.
fn readable_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Readable>())
        .ok_or_else(|| type_error("not a ReadableStream"))
}

/// Whether `value` is a `ReadableStream` made here.
pub(crate) fn is_readable(value: &JsValue) -> bool {
    value.as_object().is_some_and(|o| o.is::<Readable>())
}

/// Whether the stream has a reader.
pub(crate) fn is_locked(stream: &JsObject) -> bool {
    with_readable(stream, |r| r.reader.is_some())
}

/// Whether the stream has been read from or cancelled.
pub(crate) fn is_disturbed(stream: &JsObject) -> bool {
    with_readable(stream, |r| r.disturbed)
}

fn construct_readable(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let proto = classes(context)?.readable_proto.clone();
    let proto = prototype_for(this, "ReadableStream", proto, context)?;
    let stream = JsObject::from_proto_and_data(Some(proto), Readable::new());
    setup_readable(context, &stream, &arg(args, 0), &arg(args, 1))?;
    Ok(stream.into())
}

/// A `ReadableStream` of `source` and `strategy`, as `new ReadableStream(source, strategy)`.
pub(crate) fn new_readable(
    context: &mut Context,
    source: &JsValue,
    strategy: &JsValue,
) -> JsResult<JsObject> {
    let proto = classes(context)?.readable_proto.clone();
    let stream = JsObject::from_proto_and_data(Some(proto), Readable::new());
    setup_readable(context, &stream, source, strategy)?;
    Ok(stream)
}

/// A `ReadableStream` of these chunks of bytes, closed after them: a blob's, a response's.
pub(crate) fn readable_of_chunks(
    context: &mut Context,
    chunks: Vec<Vec<u8>>,
) -> JsResult<JsObject> {
    let stream = new_readable(context, &JsValue::undefined(), &JsValue::undefined())?;
    for chunk in chunks {
        let chunk = u8array(chunk, context)?;
        enqueue(context, &stream, chunk)?;
    }
    controller_close(context, &stream)?;
    Ok(stream)
}

fn setup_readable(
    context: &mut Context,
    stream: &JsObject,
    source: &JsValue,
    strategy: &JsValue,
) -> JsResult<()> {
    let source = if source.is_null() {
        JsValue::undefined()
    } else {
        source.clone()
    };
    let kind = get(&source, "type", context)?;
    let bytes = match kind {
        k if k.is_undefined() => false,
        k => match text_of(&k, context)?.as_str() {
            "bytes" => true,
            other => {
                return Err(crate::native::range_error(format!(
                    "Invalid type '{other}' for a ReadableStream source"
                )));
            }
        },
    };
    let (mark, size) = strategy_of(strategy, if bytes { 0. } else { 1. }, context)?;
    if bytes && !size.is_undefined() {
        return Err(crate::native::range_error(
            "A byte stream's strategy cannot have a size",
        ));
    }
    let start = method_of(&source, "start", context)?;
    let pull = method_of(&source, "pull", context)?;
    let cancel = method_of(&source, "cancel", context)?;
    let classes = classes(context)?;
    let proto = if bytes {
        classes.byte_controller_proto.clone()
    } else {
        classes.controller_proto.clone()
    };
    let controller = JsObject::from_proto_and_data(
        Some(proto),
        Controlled {
            stream: stream.clone(),
        },
    );
    with_readable(stream, |r| {
        r.bytes = bytes;
        r.high_water_mark = mark;
        r.size = size;
        r.source = source.clone();
        r.pull = pull;
        r.cancel = cancel;
        r.controller = Some(controller.clone());
    });
    let started = if start.is_undefined() {
        JsValue::undefined()
    } else {
        crate::call(context, &start, &source, &[controller.into()])?
    };
    when(
        context,
        started,
        stream.clone(),
        |stream, _, cx| {
            with_readable(stream, |r| r.started = true);
            pull_if_needed(cx, stream);
            Ok(JsValue::undefined())
        },
        |stream, reason, cx| {
            readable_error(cx, stream, reason);
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

/// What a controller holds: the stream it controls.
#[derive(Trace, Finalize, JsData)]
struct Controlled {
    stream: JsObject,
}

fn controlled_readable(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<Controlled>().map(|c| c.stream.clone()))
        .filter(|s| s.is::<Readable>())
        .ok_or_else(|| type_error("not a ReadableStream controller"))
}

fn desired_size(stream: &JsObject) -> Option<f64> {
    with_readable(stream, |r| match r.phase {
        Phase::Errored => None,
        Phase::Closed => Some(0.),
        Phase::Open => Some(r.high_water_mark - r.queue_size),
    })
}

fn can_close_or_enqueue(stream: &JsObject) -> bool {
    with_readable(stream, |r| r.phase == Phase::Open && !r.close_requested)
}

/// Calls the source's `pull` if the stream wants more and is not already waiting for it.
fn pull_if_needed(context: &mut Context, stream: &JsObject) {
    let call = with_readable(stream, |r| {
        if !r.started || r.close_requested || r.phase != Phase::Open {
            return None;
        }
        let wants =
            (r.reader.is_some() && !r.requests.is_empty()) || r.high_water_mark - r.queue_size > 0.;
        if !wants {
            return None;
        }
        if r.pulling {
            r.pull_again = true;
            return None;
        }
        r.pulling = true;
        Some((r.source.clone(), r.pull.clone(), r.controller.clone()))
    });
    let Some((source, pull, controller)) = call else {
        return;
    };
    let controller: JsValue = controller.map(JsValue::from).unwrap_or_default();
    let Ok(promise) = promise_call(&source, &pull, &[controller], context) else {
        return;
    };
    let _ = when(
        context,
        promise,
        stream.clone(),
        |stream, _, cx| {
            let again = with_readable(stream, |r| {
                r.pulling = false;
                std::mem::take(&mut r.pull_again)
            });
            if again {
                pull_if_needed(cx, stream);
            }
            Ok(JsValue::undefined())
        },
        |stream, reason, cx| {
            readable_error(cx, stream, reason);
            Ok(JsValue::undefined())
        },
    );
}

/// The stream closed: every waiting `read()` answers `done`, and its reader's `closed` resolves.
fn readable_close(context: &mut Context, stream: &JsObject) {
    let taken = with_readable(stream, |r| {
        if r.phase != Phase::Open {
            return None;
        }
        r.phase = Phase::Closed;
        Some((std::mem::take(&mut r.requests), r.reader.clone()))
    });
    let Some((requests, reader)) = taken else {
        return;
    };
    for request in requests {
        let done = step_result(JsValue::undefined(), true, context);
        resolve(request.resolvers(), done, context);
    }
    if let Some(reader) = reader {
        settle_closed(context, &reader, Ok(()));
    }
}

/// The stream failed with `reason`: its queue is dropped, every waiting `read()` and its
/// reader's `closed` reject.
fn readable_error(context: &mut Context, stream: &JsObject, reason: JsValue) {
    let taken = with_readable(stream, |r| {
        if r.phase != Phase::Open {
            return None;
        }
        r.phase = Phase::Errored;
        r.error = reason.clone();
        r.queue.clear();
        r.queue_size = 0.;
        Some((std::mem::take(&mut r.requests), r.reader.clone()))
    });
    let Some((requests, reader)) = taken else {
        return;
    };
    for request in requests {
        reject(request.resolvers(), reason.clone(), context);
    }
    if let Some(reader) = reader {
        settle_closed(context, &reader, Err(reason));
    }
}

/// The controller's `close()`: closes once the queue is read.
fn controller_close(context: &mut Context, stream: &JsObject) -> JsResult<()> {
    if !can_close_or_enqueue(stream) {
        return Err(type_error(
            "The stream is not in a state that permits close",
        ));
    }
    let empty = with_readable(stream, |r| {
        r.close_requested = true;
        r.queue.is_empty()
    });
    if empty {
        readable_close(context, stream);
    }
    Ok(())
}

/// Fills a BYOB reader's `view` from `bytes`: answers the filled part of the view, and what
/// did not fit.
fn fill(view: &JsObject, bytes: &[u8], context: &mut Context) -> JsResult<(JsValue, Vec<u8>)> {
    let room = view
        .get(js_string!("byteLength"), context)?
        .to_length(context)? as usize;
    let count = room.min(bytes.len());
    write_into(view, &bytes[..count], context)?;
    let filled = view_of_first(view, count, context)?;
    Ok((filled, bytes[count..].to_vec()))
}

/// The controller's `enqueue(chunk)`: to a waiting `read()` if there is one, otherwise onto the
/// queue.
fn enqueue(context: &mut Context, stream: &JsObject, chunk: JsValue) -> JsResult<()> {
    if !can_close_or_enqueue(stream) {
        return Err(type_error(
            "The stream is not in a state that permits enqueue",
        ));
    }
    let bytes = with_readable(stream, |r| r.bytes);
    let chunk = if bytes {
        if !is_view(&chunk, context)? {
            return Err(type_error(
                "A byte stream's chunk must be an ArrayBuffer view",
            ));
        }
        let data = crate::encoding::bytes_of(&chunk, context)?.unwrap_or_default();
        u8array(data, context)?
    } else {
        chunk
    };
    let request = with_readable(stream, |r| {
        if r.queue.is_empty() && r.reader.is_some() {
            r.requests.pop_front()
        } else {
            None
        }
    });
    match &request {
        Some(ReadRequest::Chunk(resolvers)) => {
            let result = step_result(chunk, false, context);
            resolve(resolvers, result, context);
        }
        Some(ReadRequest::Into(resolvers, view)) => {
            let data = crate::encoding::bytes_of(&chunk, context)?.unwrap_or_default();
            let (filled, rest) = fill(view, &data, context)?;
            if !rest.is_empty() {
                let size = rest.len() as f64;
                let rest = u8array(rest, context)?;
                with_readable(stream, |r| {
                    r.queue.push_back(Chunk { value: rest, size });
                    r.queue_size += size;
                });
            }
            let result = step_result(filled, false, context);
            resolve(resolvers, result, context);
        }
        None => {
            let size = if bytes {
                get(&chunk, "byteLength", context)?.to_number(context)?
            } else {
                let size_fn = with_readable(stream, |r| r.size.clone());
                if size_fn.is_undefined() {
                    1.
                } else {
                    match crate::call(
                        context,
                        &size_fn,
                        &JsValue::undefined(),
                        std::slice::from_ref(&chunk),
                    )
                    .and_then(|s| s.to_number(context))
                    {
                        Ok(size) => size,
                        Err(error) => {
                            let reason = error_value(error, context);
                            readable_error(context, stream, reason.clone());
                            return Err(JsError::from_opaque(reason));
                        }
                    }
                }
            };
            if !size.is_finite() || size < 0. {
                let error = crate::native::range_error(
                    "The chunk's size is not a finite, non-negative number",
                );
                let reason = error_value(error, context);
                readable_error(context, stream, reason.clone());
                return Err(JsError::from_opaque(reason));
            }
            with_readable(stream, |r| {
                r.queue.push_back(Chunk { value: chunk, size });
                r.queue_size += size;
            });
        }
    }
    pull_if_needed(context, stream);
    Ok(())
}

/// A reader's `read()`: a queued chunk at once, `done` if closed, the error if failed, otherwise
/// a wait for the next `enqueue`.
fn stream_read(context: &mut Context, stream: &JsObject, request: ReadRequest) -> JsResult<()> {
    let phase = with_readable(stream, |r| {
        r.disturbed = true;
        (r.phase, r.error.clone())
    });
    match phase {
        (Phase::Closed, _) => {
            let done = step_result(JsValue::undefined(), true, context);
            resolve(request.resolvers(), done, context);
        }
        (Phase::Errored, error) => reject(request.resolvers(), error, context),
        (Phase::Open, _) => {
            let front = with_readable(stream, |r| r.queue.pop_front());
            match front {
                None => {
                    with_readable(stream, |r| r.requests.push_back(request));
                    pull_if_needed(context, stream);
                }
                Some(chunk) => {
                    let value = match &request {
                        ReadRequest::Chunk(_) => {
                            with_readable(stream, |r| {
                                r.queue_size = (r.queue_size - chunk.size).max(0.)
                            });
                            chunk.value.clone()
                        }
                        ReadRequest::Into(_, view) => {
                            let data = crate::encoding::bytes_of(&chunk.value, context)?
                                .unwrap_or_default();
                            let (filled, rest) = fill(view, &data, context)?;
                            let used = (data.len() - rest.len()) as f64;
                            if !rest.is_empty() {
                                let size = rest.len() as f64;
                                let rest = u8array(rest, context)?;
                                with_readable(stream, |r| {
                                    r.queue.push_front(Chunk { value: rest, size })
                                });
                            }
                            with_readable(stream, |r| r.queue_size = (r.queue_size - used).max(0.));
                            filled
                        }
                    };
                    let close = with_readable(stream, |r| r.close_requested && r.queue.is_empty());
                    if close {
                        readable_close(context, stream);
                    } else {
                        pull_if_needed(context, stream);
                    }
                    let result = step_result(value, false, context);
                    resolve(request.resolvers(), result, context);
                }
            }
        }
    }
    Ok(())
}

/// Cancels the stream with `reason`: it closes, its queue is dropped, and the source's `cancel`
/// is told. Answers a promise of nothing.
pub(crate) fn cancel_stream(
    context: &mut Context,
    stream: &JsObject,
    reason: JsValue,
) -> JsResult<JsValue> {
    let (phase, error) = with_readable(stream, |r| {
        r.disturbed = true;
        (r.phase, r.error.clone())
    });
    match phase {
        Phase::Closed => return resolved(JsValue::undefined(), context),
        Phase::Errored => return rejected(error, context),
        Phase::Open => {}
    }
    readable_close(context, stream);
    let (source, cancel) = with_readable(stream, |r| {
        r.queue.clear();
        r.queue_size = 0.;
        (r.source.clone(), r.cancel.clone())
    });
    let promise = promise_call(&source, &cancel, &[reason], context)?;
    Ok(when(context, promise, (), ignore, rethrow)?.into())
}

const CONTROLLER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("enqueue", 1, |this, args, cx| {
        let stream = controlled_readable(this)?;
        enqueue(cx, &stream, arg(args, 0))?;
        Ok(JsValue::undefined())
    }),
    ("close", 0, |this, _, cx| {
        let stream = controlled_readable(this)?;
        controller_close(cx, &stream)?;
        Ok(JsValue::undefined())
    }),
    ("error", 1, |this, args, cx| {
        let stream = controlled_readable(this)?;
        readable_error(cx, &stream, arg(args, 0));
        Ok(JsValue::undefined())
    }),
];

fn controller_desired_size(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let stream = controlled_readable(this)?;
    Ok(desired_size(&stream).map_or(JsValue::null(), JsValue::from))
}

const READABLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("cancel", 1, |this, args, cx| {
        let stream = match readable_of(this) {
            Ok(stream) => stream,
            Err(error) => {
                let reason = error_value(error, cx);
                return rejected(reason, cx);
            }
        };
        if is_locked(&stream) {
            let reason = type_error_value("Cannot cancel a locked ReadableStream", cx);
            return rejected(reason, cx);
        }
        cancel_stream(cx, &stream, arg(args, 0))
    }),
    ("getReader", 1, |this, args, cx| {
        let stream = readable_of(this)?;
        let mode = get(&arg(args, 0), "mode", cx)?;
        let byob = match mode {
            m if m.is_undefined() => false,
            m => match text_of(&m, cx)?.as_str() {
                "byob" => true,
                other => return Err(type_error(format!("'{other}' is not a valid reader mode"))),
            },
        };
        Ok(acquire_reader(cx, &stream, byob)?.into())
    }),
    ("pipeTo", 2, |this, args, cx| match readable_of(this) {
        Ok(stream) => pipe_to(cx, &stream, &arg(args, 0), &arg(args, 1)),
        Err(error) => {
            let reason = error_value(error, cx);
            rejected(reason, cx)
        }
    }),
    ("pipeThrough", 2, |this, args, cx| {
        let stream = readable_of(this)?;
        let pair = arg(args, 0);
        let writable = get(&pair, "writable", cx)?;
        let readable = get(&pair, "readable", cx)?;
        let writable_stream = writable_of(&writable)?;
        readable_of(&readable)?;
        if is_locked(&stream) {
            return Err(type_error("Cannot pipe a locked ReadableStream"));
        }
        if with_writable(&writable_stream, |w| w.writer.is_some()) {
            return Err(type_error("Cannot pipe to a locked WritableStream"));
        }
        let promise = pipe_to(cx, &stream, &writable, &arg(args, 1))?;
        when(cx, promise, (), ignore, ignore)?;
        Ok(readable)
    }),
    ("tee", 0, |this, _, cx| {
        let stream = readable_of(this)?;
        let (a, b) = tee(cx, &stream)?;
        Ok(array(cx, vec![a.into(), b.into()]))
    }),
    ("values", 1, readable_values),
];

// ─── Readers ────────────────────────────────────────────────────────────────────────────────

/// A reader: the stream it is locked to, and its `closed` promise.
#[derive(Trace, Finalize, JsData)]
struct Reader {
    stream: Option<JsObject>,
    closed: JsObject,
    closed_resolvers: Option<ResolvingFunctions>,
}

fn reader_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Reader>())
        .ok_or_else(|| type_error("not a ReadableStream reader"))
}

/// Locks `stream` to a new reader (a BYOB one when `byob`).
pub(crate) fn acquire_reader(
    context: &mut Context,
    stream: &JsObject,
    byob: bool,
) -> JsResult<JsObject> {
    let classes = classes(context)?;
    let proto = if byob {
        classes.byob_reader_proto.clone()
    } else {
        classes.reader_proto.clone()
    };
    acquire_reader_on(context, stream, byob, proto)
}

fn acquire_reader_on(
    context: &mut Context,
    stream: &JsObject,
    byob: bool,
    proto: JsObject,
) -> JsResult<JsObject> {
    let (locked, bytes, phase, error) = with_readable(stream, |r| {
        (r.reader.is_some(), r.bytes, r.phase, r.error.clone())
    });
    if locked {
        return Err(type_error("ReadableStream is locked"));
    }
    if byob && !bytes {
        return Err(type_error(
            "A BYOB reader needs a byte stream (type: 'bytes')",
        ));
    }
    let (closed, resolvers) = pending(context);
    let resolvers = match phase {
        Phase::Open => Some(resolvers),
        Phase::Closed => {
            resolve(&resolvers, JsValue::undefined(), context);
            None
        }
        Phase::Errored => {
            reject(&resolvers, error, context);
            None
        }
    };
    let reader = JsObject::from_proto_and_data(
        Some(proto),
        Reader {
            stream: Some(stream.clone()),
            closed,
            closed_resolvers: resolvers,
        },
    );
    with_readable(stream, |r| r.reader = Some(reader.clone()));
    Ok(reader)
}

/// Settles a reader's `closed`; once settled, it is replaced by a promise already settled
/// (what `releaseLock` does).
fn settle_closed(context: &mut Context, reader: &JsObject, outcome: Result<(), JsValue>) {
    let resolvers = reader
        .downcast_mut::<Reader>()
        .and_then(|mut r| r.closed_resolvers.take());
    match (resolvers, outcome) {
        (Some(resolvers), Ok(())) => resolve(&resolvers, JsValue::undefined(), context),
        (Some(resolvers), Err(reason)) => reject(&resolvers, reason, context),
        (None, Ok(())) => {}
        (None, Err(reason)) => {
            let (closed, resolvers) = pending(context);
            reject(&resolvers, reason, context);
            if let Some(mut r) = reader.downcast_mut::<Reader>() {
                r.closed = closed;
            }
        }
    }
}

/// A reader's `read()` as a promise, from Rust.
pub(crate) fn read(context: &mut Context, reader: &JsObject) -> JsResult<JsObject> {
    let (promise, resolvers) = pending(context);
    let stream = reader
        .downcast_ref::<Reader>()
        .and_then(|r| r.stream.clone());
    match stream {
        None => {
            let reason = type_error_value("This reader has been released", context);
            reject(&resolvers, reason, context);
        }
        Some(stream) => stream_read(context, &stream, ReadRequest::Chunk(resolvers))?,
    }
    Ok(promise)
}

/// Lets go of the stream: `read()`s still waiting reject, and `closed` rejects.
pub(crate) fn release_reader(context: &mut Context, reader: &JsObject) {
    let Some(stream) = reader
        .downcast_ref::<Reader>()
        .and_then(|r| r.stream.clone())
    else {
        return;
    };
    let requests = with_readable(&stream, |r| {
        r.reader = None;
        std::mem::take(&mut r.requests)
    });
    for request in requests {
        let reason = type_error_value("The reader was released", context);
        reject(request.resolvers(), reason, context);
    }
    let reason = type_error_value("The reader was released", context);
    settle_closed(context, reader, Err(reason));
    if let Some(mut r) = reader.downcast_mut::<Reader>() {
        r.stream = None;
    }
}

fn reader_closed(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let reader = reader_of(this)?;
    Ok(reader
        .downcast_ref::<Reader>()
        .map(|r| JsValue::from(r.closed.clone()))
        .unwrap_or_default())
}

fn reader_cancel(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let reader = reader_of(this)?;
    match reader
        .downcast_ref::<Reader>()
        .and_then(|r| r.stream.clone())
    {
        Some(stream) => cancel_stream(context, &stream, arg(args, 0)),
        None => {
            let reason = type_error_value("This reader has been released", context);
            rejected(reason, context)
        }
    }
}

fn reader_release(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let reader = reader_of(this)?;
    release_reader(context, &reader);
    Ok(JsValue::undefined())
}

const READER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("read", 0, |this, _, cx| {
        let reader = reader_of(this)?;
        Ok(read(cx, &reader)?.into())
    }),
    ("cancel", 1, reader_cancel),
    ("releaseLock", 0, reader_release),
];

const BYOB_READER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("read", 1, |this, args, cx| {
        let reader = reader_of(this)?;
        let view = arg(args, 0);
        if !is_view(&view, cx)? || get(&view, "byteLength", cx)?.to_number(cx)? == 0. {
            let reason = type_error_value("read(view) needs a non-empty ArrayBuffer view", cx);
            return rejected(reason, cx);
        }
        let view = view.as_object().expect("a view");
        let (promise, resolvers) = pending(cx);
        match reader
            .downcast_ref::<Reader>()
            .and_then(|r| r.stream.clone())
        {
            None => {
                let reason = type_error_value("This reader has been released", cx);
                reject(&resolvers, reason, cx);
            }
            Some(stream) => stream_read(cx, &stream, ReadRequest::Into(resolvers, view))?,
        }
        Ok(promise.into())
    }),
    ("cancel", 1, reader_cancel),
    ("releaseLock", 0, reader_release),
];

// ─── Async iteration ────────────────────────────────────────────────────────────────────────

/// `for await (const chunk of stream)`: a reader read until done, released then.
#[derive(Trace, Finalize, JsData)]
struct Iteration {
    reader: JsObject,
    #[unsafe_ignore_trace]
    prevent_cancel: bool,
    #[unsafe_ignore_trace]
    done: bool,
}

fn readable_values(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let stream = readable_of(this)?;
    let prevent_cancel = get(&arg(args, 0), "preventCancel", context)?.to_boolean();
    let reader = acquire_reader(context, &stream, false)?;
    let proto = classes(context)?.iterator_proto.clone();
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Iteration {
            reader,
            prevent_cancel,
            done: false,
        },
    )
    .into())
}

fn iteration_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Iteration>())
        .ok_or_else(|| type_error("not a ReadableStream async iterator"))
}

const ITERATOR_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("next", 0, |this, _, cx| {
        let iteration = iteration_of(this)?;
        let (reader, done) = {
            let i = iteration.downcast_ref::<Iteration>().expect("an iteration");
            (i.reader.clone(), i.done)
        };
        if done {
            let result = step_result(JsValue::undefined(), true, cx);
            return resolved(result, cx);
        }
        let promise = read(cx, &reader)?;
        Ok(when(
            cx,
            promise.into(),
            iteration,
            |iteration, result, cx| {
                if get(&result, "done", cx)?.to_boolean() {
                    let reader = {
                        let mut i = iteration.downcast_mut::<Iteration>().expect("an iteration");
                        i.done = true;
                        i.reader.clone()
                    };
                    release_reader(cx, &reader);
                }
                Ok(result)
            },
            |iteration, reason, cx| {
                let reader = {
                    let mut i = iteration.downcast_mut::<Iteration>().expect("an iteration");
                    i.done = true;
                    i.reader.clone()
                };
                release_reader(cx, &reader);
                Err(JsError::from_opaque(reason))
            },
        )?
        .into())
    }),
    ("return", 1, |this, args, cx| {
        let iteration = iteration_of(this)?;
        let value = arg(args, 0);
        let (reader, prevent_cancel, done) = {
            let mut i = iteration.downcast_mut::<Iteration>().expect("an iteration");
            let was = i.done;
            i.done = true;
            (i.reader.clone(), i.prevent_cancel, was)
        };
        let result = step_result(value, true, cx);
        if done {
            return resolved(result, cx);
        }
        let stream = reader
            .downcast_ref::<Reader>()
            .and_then(|r| r.stream.clone());
        let cancelled = match stream {
            Some(stream) if !prevent_cancel => cancel_stream(cx, &stream, arg(args, 0))?,
            _ => resolved(JsValue::undefined(), cx)?,
        };
        release_reader(cx, &reader);
        Ok(when(
            cx,
            cancelled,
            result,
            |result, _, _| Ok(result.clone()),
            rethrow,
        )?
        .into())
    }),
];

// ─── ReadableStream.from ────────────────────────────────────────────────────────────────────

/// `ReadableStream.from(iterable)`: a stream pulling from an async iterator — or a sync one,
/// whose values are awaited.
fn readable_from(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let iterable = arg(args, 0);
    if iterable.is_null_or_undefined() {
        return Err(type_error(
            "ReadableStream.from: the argument is not iterable",
        ));
    }
    let holder = iterable.to_object(context)?;
    let async_method = holder.get(JsSymbol::async_iterator(), context)?;
    let (method, is_async) = if !async_method.is_null_or_undefined() {
        (async_method, true)
    } else {
        let method = holder.get(JsSymbol::iterator(), context)?;
        if method.is_null_or_undefined() {
            return Err(type_error(
                "ReadableStream.from: the argument is not iterable",
            ));
        }
        (method, false)
    };
    let iterator = crate::call(context, &method, &iterable, &[])?;
    if !iterator.is_object() {
        return Err(type_error("The iterator method did not answer an object"));
    }
    let next = get(&iterator, "next", context)?;
    let state = (iterator, next, is_async);
    let source = object(context);
    let pull = native(
        context,
        "pull",
        1,
        state.clone(),
        |_, args, (iterator, next, is_async), cx| {
            let controller = arg(args, 0);
            let result = match crate::call(cx, next, iterator, &[]) {
                Ok(result) => result,
                Err(error) => {
                    let reason = error_value(error, cx);
                    return rejected(reason, cx);
                }
            };
            let _ = is_async;
            Ok(when(
                cx,
                result,
                controller,
                |controller, result, cx| {
                    if !result.is_object() {
                        return Err(type_error("The iterator's result is not an object"));
                    }
                    if get(&result, "done", cx)?.to_boolean() {
                        invoke(controller, "close", &[], cx)?;
                        return Ok(JsValue::undefined());
                    }
                    let value = get(&result, "value", cx)?;
                    // A sync iterator's value may be a promise, awaited as `for await` would.
                    Ok(when(
                        cx,
                        value,
                        controller.clone(),
                        |controller, value, cx| invoke(controller, "enqueue", &[value], cx),
                        rethrow,
                    )?
                    .into())
                },
                rethrow,
            )?
            .into())
        },
    );
    let cancel = native(
        context,
        "cancel",
        1,
        state,
        |_, args, (iterator, _, _), cx| {
            let ret = match get(iterator, "return", cx) {
                Ok(ret) => ret,
                Err(error) => {
                    let reason = error_value(error, cx);
                    return rejected(reason, cx);
                }
            };
            if ret.is_null_or_undefined() {
                return resolved(JsValue::undefined(), cx);
            }
            let result = promise_call(iterator, &ret, &[arg(args, 0)], cx)?;
            Ok(when(
                cx,
                result,
                (),
                |_, result, _| {
                    if !result.is_object() {
                        return Err(type_error(
                            "The iterator's return() did not answer an object",
                        ));
                    }
                    Ok(JsValue::undefined())
                },
                rethrow,
            )?
            .into())
        },
    );
    source.set(js_string!("pull"), pull, false, context)?;
    source.set(js_string!("cancel"), cancel, false, context)?;
    let strategy = object(context);
    strategy.set(js_string!("highWaterMark"), 0, false, context)?;
    Ok(new_readable(context, &source.into(), &strategy.into())?.into())
}

// ─── tee ────────────────────────────────────────────────────────────────────────────────────

/// What two branches of a `tee` share: the reader of the stream they split, and whether each
/// was cancelled, and why.
#[derive(Trace, Finalize, JsData)]
struct Tee {
    stream: JsObject,
    reader: JsObject,
    branches: Vec<JsObject>,
    #[unsafe_ignore_trace]
    reading: bool,
    #[unsafe_ignore_trace]
    read_again: bool,
    #[unsafe_ignore_trace]
    cancelled: [bool; 2],
    reasons: Vec<JsValue>,
    #[unsafe_ignore_trace]
    bytes: bool,
    cancel_promise: JsObject,
    cancel_resolvers: Option<ResolvingFunctions>,
}

/// Splits `stream` into two that each see every chunk.
pub(crate) fn tee(context: &mut Context, stream: &JsObject) -> JsResult<(JsObject, JsObject)> {
    let reader = acquire_reader(context, stream, false)?;
    let (cancel_promise, cancel_resolvers) = pending(context);
    let bytes = with_readable(stream, |r| r.bytes);
    let tee = JsObject::from_proto_and_data(
        None,
        Tee {
            stream: stream.clone(),
            reader: reader.clone(),
            branches: Vec::new(),
            reading: false,
            read_again: false,
            cancelled: [false, false],
            reasons: vec![JsValue::undefined(), JsValue::undefined()],
            bytes,
            cancel_promise,
            cancel_resolvers: Some(cancel_resolvers),
        },
    );
    let mut branches = Vec::new();
    for which in 0..2usize {
        let source = object(context);
        let pull = native(context, "pull", 1, tee.clone(), |_, _, tee, cx| {
            tee_pull(cx, tee)?;
            resolved(JsValue::undefined(), cx)
        });
        let cancel: JsValue = if which == 0 {
            native(context, "cancel", 1, tee.clone(), |_, args, tee, cx| {
                tee_cancel(cx, tee, 0, arg(args, 0))
            })
        } else {
            native(context, "cancel", 1, tee.clone(), |_, args, tee, cx| {
                tee_cancel(cx, tee, 1, arg(args, 0))
            })
        };
        source.set(js_string!("pull"), pull, false, context)?;
        source.set(js_string!("cancel"), cancel, false, context)?;
        branches.push(new_readable(
            context,
            &source.into(),
            &JsValue::undefined(),
        )?);
    }
    tee.downcast_mut::<Tee>().expect("a tee").branches = branches.clone();
    // The stream failing fails both branches.
    let closed = reader
        .downcast_ref::<Reader>()
        .map(|r| r.closed.clone())
        .expect("a reader");
    when(context, closed.into(), tee, ignore, |tee, reason, cx| {
        let branches = tee
            .downcast_ref::<Tee>()
            .map(|t| t.branches.clone())
            .unwrap_or_default();
        for branch in branches {
            readable_error(cx, &branch, reason.clone());
        }
        let resolvers = tee
            .downcast_mut::<Tee>()
            .and_then(|mut t| t.cancel_resolvers.take());
        if let Some(resolvers) = resolvers {
            resolve(&resolvers, JsValue::undefined(), cx);
        }
        Ok(JsValue::undefined())
    })?;
    Ok((branches[0].clone(), branches[1].clone()))
}

fn tee_pull(context: &mut Context, tee: &JsObject) -> JsResult<()> {
    let reader = {
        let mut t = tee.downcast_mut::<Tee>().expect("a tee");
        if t.reading {
            t.read_again = true;
            return Ok(());
        }
        t.reading = true;
        t.reader.clone()
    };
    let promise = read(context, &reader)?;
    when(
        context,
        promise.into(),
        tee.clone(),
        |tee, result, cx| {
            let (branches, cancelled, bytes) = {
                let t = tee.downcast_ref::<Tee>().expect("a tee");
                (t.branches.clone(), t.cancelled, t.bytes)
            };
            if get(&result, "done", cx)?.to_boolean() {
                tee.downcast_mut::<Tee>().expect("a tee").reading = false;
                for (at, branch) in branches.iter().enumerate() {
                    if !cancelled[at] && can_close_or_enqueue(branch) {
                        controller_close(cx, branch)?;
                    }
                }
                if !(cancelled[0] && cancelled[1]) {
                    let resolvers = tee
                        .downcast_mut::<Tee>()
                        .and_then(|mut t| t.cancel_resolvers.take());
                    if let Some(resolvers) = resolvers {
                        resolve(&resolvers, JsValue::undefined(), cx);
                    }
                }
                return Ok(JsValue::undefined());
            }
            let chunk = get(&result, "value", cx)?;
            tee.downcast_mut::<Tee>().expect("a tee").read_again = false;
            for (at, branch) in branches.iter().enumerate() {
                if cancelled[at] || !can_close_or_enqueue(branch) {
                    continue;
                }
                // A byte stream's second branch gets its own copy, as the standard has it.
                let chunk = if bytes && at == 1 {
                    let data = crate::encoding::bytes_of(&chunk, cx)?.unwrap_or_default();
                    u8array(data, cx)?
                } else {
                    chunk.clone()
                };
                enqueue(cx, branch, chunk)?;
            }
            let again = {
                let mut t = tee.downcast_mut::<Tee>().expect("a tee");
                t.reading = false;
                std::mem::take(&mut t.read_again)
            };
            if again {
                tee_pull(cx, tee)?;
            }
            Ok(JsValue::undefined())
        },
        |tee, _, _| {
            tee.downcast_mut::<Tee>().expect("a tee").reading = false;
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

/// A branch cancelled: once both are, the stream is, with both reasons.
fn tee_cancel(
    context: &mut Context,
    tee: &JsObject,
    which: usize,
    reason: JsValue,
) -> JsResult<JsValue> {
    let (both, stream, reasons, promise) = {
        let mut t = tee.downcast_mut::<Tee>().expect("a tee");
        t.cancelled[which] = true;
        t.reasons[which] = reason;
        (
            t.cancelled[0] && t.cancelled[1],
            t.stream.clone(),
            t.reasons.clone(),
            t.cancel_promise.clone(),
        )
    };
    if both {
        let composite = array(context, reasons);
        let result = cancel_stream(context, &stream, composite)?;
        let resolvers = tee
            .downcast_mut::<Tee>()
            .and_then(|mut t| t.cancel_resolvers.take());
        if let Some(resolvers) = resolvers {
            resolve(&resolvers, result, context);
        }
    }
    Ok(promise.into())
}

// ─── pipeTo ─────────────────────────────────────────────────────────────────────────────────

/// A pipe under way: the reader and writer it holds, and what it was told not to do.
#[derive(Trace, Finalize, JsData)]
struct Pipe {
    source: JsObject,
    dest: JsObject,
    reader: JsObject,
    writer: JsObject,
    #[unsafe_ignore_trace]
    prevent_close: bool,
    #[unsafe_ignore_trace]
    prevent_abort: bool,
    #[unsafe_ignore_trace]
    prevent_cancel: bool,
    #[unsafe_ignore_trace]
    shutting_down: bool,
    #[unsafe_ignore_trace]
    finished: bool,
    signal: JsValue,
    listener: JsValue,
    resolvers: ResolvingFunctions,
}

/// `source.pipeTo(dest, options)`: answers a promise settled when the pipe is over.
fn pipe_to(
    context: &mut Context,
    source: &JsObject,
    dest: &JsValue,
    options: &JsValue,
) -> JsResult<JsValue> {
    let Ok(dest) = writable_of(dest) else {
        let reason = type_error_value("pipeTo: the destination is not a WritableStream", context);
        return rejected(reason, context);
    };
    let flag = |name: &str, cx: &mut Context| -> JsResult<bool> {
        Ok(get(options, name, cx)?.to_boolean())
    };
    let prevent_close = flag("preventClose", context)?;
    let prevent_abort = flag("preventAbort", context)?;
    let prevent_cancel = flag("preventCancel", context)?;
    let signal = get(options, "signal", context)?;
    if is_locked(source) {
        let reason = type_error_value("pipeTo: the ReadableStream is locked", context);
        return rejected(reason, context);
    }
    if with_writable(&dest, |w| w.writer.is_some()) {
        let reason = type_error_value("pipeTo: the WritableStream is locked", context);
        return rejected(reason, context);
    }
    let reader = acquire_reader(context, source, false)?;
    let writer = acquire_writer(context, &dest)?;
    let (promise, resolvers) = pending(context);
    let pipe = JsObject::from_proto_and_data(
        None,
        Pipe {
            source: source.clone(),
            dest: dest.clone(),
            reader: reader.clone(),
            writer: writer.clone(),
            prevent_close,
            prevent_abort,
            prevent_cancel,
            shutting_down: false,
            finished: false,
            signal: signal.clone(),
            listener: JsValue::undefined(),
            resolvers,
        },
    );
    if signal.is_object() {
        if get(&signal, "aborted", context)?.to_boolean() {
            pipe_abort(context, &pipe)?;
            return Ok(promise.into());
        }
        let listener = native(context, "abort", 1, pipe.clone(), |_, _, pipe, cx| {
            pipe_abort(cx, pipe)?;
            Ok(JsValue::undefined())
        });
        pipe.downcast_mut::<Pipe>().expect("a pipe").listener = listener.clone();
        invoke(
            &signal,
            "addEventListener",
            &[js_text("abort"), listener],
            context,
        )?;
    }
    // The source failing, and the destination failing, each end the pipe.
    let closed = reader
        .downcast_ref::<Reader>()
        .map(|r| r.closed.clone())
        .expect("a reader");
    when(
        context,
        closed.into(),
        pipe.clone(),
        ignore,
        |pipe, reason, cx| {
            pipe_source_errored(cx, pipe, reason)?;
            Ok(JsValue::undefined())
        },
    )?;
    let closed = writer
        .downcast_ref::<Writer>()
        .map(|w| w.closed.clone())
        .expect("a writer");
    when(
        context,
        closed.into(),
        pipe.clone(),
        ignore,
        |pipe, reason, cx| {
            pipe_dest_errored(cx, pipe, reason)?;
            Ok(JsValue::undefined())
        },
    )?;
    // A source already closed, or failed; a destination already closing.
    match with_readable(source, |r| (r.phase, r.error.clone())) {
        (Phase::Errored, error) => {
            pipe_source_errored(context, &pipe, error)?;
            return Ok(promise.into());
        }
        (Phase::Closed, _) => {
            pipe_source_closed(context, &pipe)?;
            return Ok(promise.into());
        }
        _ => {}
    }
    let closing = with_writable(&dest, |w| {
        w.close_resolvers.is_some() || w.phase == WPhase::Closed
    });
    if closing {
        let reason = type_error_value("pipeTo: the destination is closed", context);
        pipe_dest_errored(context, &pipe, reason)?;
        return Ok(promise.into());
    }
    pipe_step(context, &pipe)?;
    Ok(promise.into())
}

fn pipe_flags(pipe: &JsObject) -> (bool, bool, bool, bool) {
    let p = pipe.downcast_ref::<Pipe>().expect("a pipe");
    (
        p.shutting_down,
        p.prevent_close,
        p.prevent_abort,
        p.prevent_cancel,
    )
}

/// Starts the pipe's shutting down: answers whether it had already started.
fn begin_shutdown(pipe: &JsObject) -> bool {
    let mut p = pipe.downcast_mut::<Pipe>().expect("a pipe");
    std::mem::replace(&mut p.shutting_down, true)
}

/// Waits for the writer to be ready, reads a chunk, writes it, and goes round again.
fn pipe_step(context: &mut Context, pipe: &JsObject) -> JsResult<()> {
    if pipe_flags(pipe).0 {
        return Ok(());
    }
    let ready = pipe
        .downcast_ref::<Pipe>()
        .and_then(|p| p.writer.downcast_ref::<Writer>().map(|w| w.ready.clone()))
        .expect("a writer");
    when(
        context,
        ready.into(),
        pipe.clone(),
        |pipe, _, cx| {
            if pipe_flags(pipe).0 {
                return Ok(JsValue::undefined());
            }
            let reader = pipe
                .downcast_ref::<Pipe>()
                .map(|p| p.reader.clone())
                .expect("a pipe");
            let promise = read(cx, &reader)?;
            when(
                cx,
                promise.into(),
                pipe.clone(),
                |pipe, result, cx| {
                    if pipe_flags(pipe).0 {
                        return Ok(JsValue::undefined());
                    }
                    if get(&result, "done", cx)?.to_boolean() {
                        pipe_source_closed(cx, pipe)?;
                        return Ok(JsValue::undefined());
                    }
                    let chunk = get(&result, "value", cx)?;
                    let writer = pipe
                        .downcast_ref::<Pipe>()
                        .map(|p| p.writer.clone())
                        .expect("a pipe");
                    let written = writer_write(cx, &writer, chunk)?;
                    when(cx, written, (), ignore, ignore)?;
                    pipe_step(cx, pipe)?;
                    Ok(JsValue::undefined())
                },
                ignore,
            )?;
            Ok(JsValue::undefined())
        },
        ignore,
    )?;
    Ok(())
}

fn pipe_source_closed(context: &mut Context, pipe: &JsObject) -> JsResult<()> {
    if begin_shutdown(pipe) {
        return Ok(());
    }
    let (_, prevent_close, _, _) = pipe_flags(pipe);
    if prevent_close {
        return pipe_finish(context, pipe, Ok(()));
    }
    let writer = pipe
        .downcast_ref::<Pipe>()
        .map(|p| p.writer.clone())
        .expect("a pipe");
    let closed = writer_close(context, &writer)?;
    when(
        context,
        closed,
        pipe.clone(),
        |pipe, _, cx| {
            pipe_finish(cx, pipe, Ok(()))?;
            Ok(JsValue::undefined())
        },
        |pipe, reason, cx| {
            pipe_finish(cx, pipe, Err(reason))?;
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

fn pipe_source_errored(context: &mut Context, pipe: &JsObject, reason: JsValue) -> JsResult<()> {
    if begin_shutdown(pipe) {
        return Ok(());
    }
    let (_, _, prevent_abort, _) = pipe_flags(pipe);
    if prevent_abort {
        return pipe_finish(context, pipe, Err(reason));
    }
    let dest = pipe
        .downcast_ref::<Pipe>()
        .map(|p| p.dest.clone())
        .expect("a pipe");
    let aborted = writable_abort(context, &dest, reason.clone())?;
    when(
        context,
        aborted,
        (pipe.clone(), reason),
        |(pipe, reason), _, cx| {
            pipe_finish(cx, pipe, Err(reason.clone()))?;
            Ok(JsValue::undefined())
        },
        |(pipe, _), error, cx| {
            pipe_finish(cx, pipe, Err(error))?;
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

fn pipe_dest_errored(context: &mut Context, pipe: &JsObject, reason: JsValue) -> JsResult<()> {
    if begin_shutdown(pipe) {
        return Ok(());
    }
    let (_, _, _, prevent_cancel) = pipe_flags(pipe);
    if prevent_cancel {
        return pipe_finish(context, pipe, Err(reason));
    }
    let source = pipe
        .downcast_ref::<Pipe>()
        .map(|p| p.source.clone())
        .expect("a pipe");
    let cancelled = cancel_stream(context, &source, reason.clone())?;
    when(
        context,
        cancelled,
        (pipe.clone(), reason),
        |(pipe, reason), _, cx| {
            pipe_finish(cx, pipe, Err(reason.clone()))?;
            Ok(JsValue::undefined())
        },
        |(pipe, _), error, cx| {
            pipe_finish(cx, pipe, Err(error))?;
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

/// The signal aborted: the destination aborted and the source cancelled (unless told not to),
/// and the pipe rejects with the signal's reason.
fn pipe_abort(context: &mut Context, pipe: &JsObject) -> JsResult<()> {
    if begin_shutdown(pipe) {
        return Ok(());
    }
    let (_, _, prevent_abort, prevent_cancel) = pipe_flags(pipe);
    let (source, dest, signal) = {
        let p = pipe.downcast_ref::<Pipe>().expect("a pipe");
        (p.source.clone(), p.dest.clone(), p.signal.clone())
    };
    let reason = get(&signal, "reason", context)?;
    let aborted = if prevent_abort || with_writable(&dest, |w| w.phase) != WPhase::Writable {
        resolved(JsValue::undefined(), context)?
    } else {
        writable_abort(context, &dest, reason.clone())?
    };
    let cancelled = if prevent_cancel || with_readable(&source, |r| r.phase) != Phase::Open {
        resolved(JsValue::undefined(), context)?
    } else {
        cancel_stream(context, &source, reason.clone())?
    };
    let both = (pipe.clone(), reason, cancelled);
    when(
        context,
        aborted,
        both,
        |(pipe, reason, cancelled), _, cx| {
            when(
                cx,
                cancelled.clone(),
                (pipe.clone(), reason.clone()),
                |(pipe, reason), _, cx| {
                    pipe_finish(cx, pipe, Err(reason.clone()))?;
                    Ok(JsValue::undefined())
                },
                |(pipe, reason), _, cx| {
                    pipe_finish(cx, pipe, Err(reason.clone()))?;
                    Ok(JsValue::undefined())
                },
            )?;
            Ok(JsValue::undefined())
        },
        |(pipe, reason, _), _, cx| {
            pipe_finish(cx, pipe, Err(reason.clone()))?;
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

/// The pipe over: both locks released, the signal no longer listened to, its promise settled.
fn pipe_finish(
    context: &mut Context,
    pipe: &JsObject,
    outcome: Result<(), JsValue>,
) -> JsResult<()> {
    let (reader, writer, signal, listener, resolvers) = {
        let mut p = pipe.downcast_mut::<Pipe>().expect("a pipe");
        if p.finished {
            return Ok(());
        }
        p.finished = true;
        (
            p.reader.clone(),
            p.writer.clone(),
            p.signal.clone(),
            p.listener.clone(),
            p.resolvers.clone(),
        )
    };
    release_writer(context, &writer);
    release_reader(context, &reader);
    if !listener.is_undefined() {
        let _ = invoke(
            &signal,
            "removeEventListener",
            &[js_text("abort"), listener],
            context,
        );
    }
    match outcome {
        Ok(()) => resolve(&resolvers, JsValue::undefined(), context),
        Err(reason) => reject(&resolvers, reason, context),
    }
    Ok(())
}

// ─── WritableStream ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WPhase {
    Writable,
    Erroring,
    Errored,
    Closed,
}

#[derive(Trace, Finalize)]
struct Write {
    chunk: JsValue,
    #[unsafe_ignore_trace]
    size: f64,
    resolvers: ResolvingFunctions,
}

#[derive(Trace, Finalize)]
struct PendingAbort {
    promise: JsObject,
    resolvers: ResolvingFunctions,
    reason: JsValue,
    #[unsafe_ignore_trace]
    was_erroring: bool,
}

/// A `WritableStream`, and its controller's state: the queue of writes, the sink, the strategy.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct Writable {
    #[unsafe_ignore_trace]
    phase: WPhase,
    error: JsValue,
    writer: Option<JsObject>,
    controller: Option<JsObject>,
    queue: VecDeque<Write>,
    #[unsafe_ignore_trace]
    queue_size: f64,
    #[unsafe_ignore_trace]
    in_flight_write: bool,
    #[unsafe_ignore_trace]
    in_flight_close: bool,
    close_resolvers: Option<ResolvingFunctions>,
    abort: Option<PendingAbort>,
    #[unsafe_ignore_trace]
    backpressure: bool,
    #[unsafe_ignore_trace]
    started: bool,
    #[unsafe_ignore_trace]
    high_water_mark: f64,
    size: JsValue,
    sink: JsValue,
    write: JsValue,
    close: JsValue,
    abort_fn: JsValue,
    abort_controller: JsValue,
}

fn with_writable<R>(stream: &JsObject, f: impl FnOnce(&mut Writable) -> R) -> R {
    f(&mut stream.downcast_mut::<Writable>().expect("a WritableStream"))
}

fn writable_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Writable>())
        .ok_or_else(|| type_error("not a WritableStream"))
}

fn controlled_writable(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<Controlled>().map(|c| c.stream.clone()))
        .filter(|s| s.is::<Writable>())
        .ok_or_else(|| type_error("not a WritableStream controller"))
}

fn construct_writable(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let proto = classes(context)?.writable_proto.clone();
    let proto = prototype_for(this, "WritableStream", proto, context)?;
    let stream = JsObject::from_proto_and_data(Some(proto), new_writable_state());
    setup_writable(context, &stream, &arg(args, 0), &arg(args, 1))?;
    Ok(stream.into())
}

fn new_writable_state() -> Writable {
    Writable {
        phase: WPhase::Writable,
        error: JsValue::undefined(),
        writer: None,
        controller: None,
        queue: VecDeque::new(),
        queue_size: 0.,
        in_flight_write: false,
        in_flight_close: false,
        close_resolvers: None,
        abort: None,
        backpressure: false,
        started: false,
        high_water_mark: 1.,
        size: JsValue::undefined(),
        sink: JsValue::undefined(),
        write: JsValue::undefined(),
        close: JsValue::undefined(),
        abort_fn: JsValue::undefined(),
        abort_controller: JsValue::undefined(),
    }
}

/// A `WritableStream` of `sink` and `strategy`, as `new WritableStream(sink, strategy)`.
pub(crate) fn new_writable(
    context: &mut Context,
    sink: &JsValue,
    strategy: &JsValue,
) -> JsResult<JsObject> {
    let proto = classes(context)?.writable_proto.clone();
    let stream = JsObject::from_proto_and_data(Some(proto), new_writable_state());
    setup_writable(context, &stream, sink, strategy)?;
    Ok(stream)
}

fn setup_writable(
    context: &mut Context,
    stream: &JsObject,
    sink: &JsValue,
    strategy: &JsValue,
) -> JsResult<()> {
    let sink = if sink.is_null() {
        JsValue::undefined()
    } else {
        sink.clone()
    };
    if !get(&sink, "type", context)?.is_undefined() {
        return Err(crate::native::range_error(
            "Invalid type for a WritableStream sink",
        ));
    }
    let (mark, size) = strategy_of(strategy, 1., context)?;
    let start = method_of(&sink, "start", context)?;
    let write = method_of(&sink, "write", context)?;
    let close = method_of(&sink, "close", context)?;
    let abort = method_of(&sink, "abort", context)?;
    let proto = classes(context)?.writable_controller_proto.clone();
    let controller = JsObject::from_proto_and_data(
        Some(proto),
        Controlled {
            stream: stream.clone(),
        },
    );
    // The controller's `signal`, aborted when the stream is.
    let abort_controller = crate::native::construct_global(context, "AbortController", &[])
        .map(JsValue::from)
        .unwrap_or_default();
    with_writable(stream, |w| {
        w.high_water_mark = mark;
        w.size = size;
        w.sink = sink.clone();
        w.write = write;
        w.close = close;
        w.abort_fn = abort;
        w.controller = Some(controller.clone());
        w.abort_controller = abort_controller;
        w.backpressure = mark <= 0.;
    });
    let started = if start.is_undefined() {
        JsValue::undefined()
    } else {
        crate::call(context, &start, &sink, &[controller.into()])?
    };
    when(
        context,
        started,
        stream.clone(),
        |stream, _, cx| {
            with_writable(stream, |w| w.started = true);
            advance(cx, stream)?;
            Ok(JsValue::undefined())
        },
        |stream, reason, cx| {
            with_writable(stream, |w| w.started = true);
            deal_with_rejection(cx, stream, reason)?;
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

/// A writer: the stream it is locked to, and its `ready` and `closed` promises.
#[derive(Trace, Finalize, JsData)]
struct Writer {
    stream: Option<JsObject>,
    ready: JsObject,
    ready_resolvers: Option<ResolvingFunctions>,
    closed: JsObject,
    closed_resolvers: Option<ResolvingFunctions>,
}

fn writer_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Writer>())
        .ok_or_else(|| type_error("not a WritableStream writer"))
}

/// Locks `stream` to a new writer.
pub(crate) fn acquire_writer(context: &mut Context, stream: &JsObject) -> JsResult<JsObject> {
    let proto = classes(context)?.writer_proto.clone();
    acquire_writer_on(context, stream, proto)
}

fn acquire_writer_on(
    context: &mut Context,
    stream: &JsObject,
    proto: JsObject,
) -> JsResult<JsObject> {
    let (locked, phase, error, backpressure, closing) = with_writable(stream, |w| {
        (
            w.writer.is_some(),
            w.phase,
            w.error.clone(),
            w.backpressure,
            w.close_resolvers.is_some(),
        )
    });
    if locked {
        return Err(type_error("WritableStream is locked"));
    }
    let (ready, ready_resolvers) = pending(context);
    let (closed, closed_resolvers) = pending(context);
    let ready_resolvers = match phase {
        WPhase::Writable if backpressure && !closing => Some(ready_resolvers),
        WPhase::Erroring | WPhase::Errored => {
            reject(&ready_resolvers, error.clone(), context);
            None
        }
        _ => {
            resolve(&ready_resolvers, JsValue::undefined(), context);
            None
        }
    };
    let closed_resolvers = match phase {
        WPhase::Writable | WPhase::Erroring => Some(closed_resolvers),
        WPhase::Closed => {
            resolve(&closed_resolvers, JsValue::undefined(), context);
            None
        }
        WPhase::Errored => {
            reject(&closed_resolvers, error, context);
            None
        }
    };
    let writer = JsObject::from_proto_and_data(
        Some(proto),
        Writer {
            stream: Some(stream.clone()),
            ready,
            ready_resolvers,
            closed,
            closed_resolvers,
        },
    );
    with_writable(stream, |w| w.writer = Some(writer.clone()));
    Ok(writer)
}

/// Settles a writer's `ready` (or `closed`), replacing it with a settled promise if it was
/// settled already.
fn settle_writer(
    context: &mut Context,
    writer: &JsObject,
    ready: bool,
    outcome: Result<(), JsValue>,
) {
    let resolvers = writer.downcast_mut::<Writer>().and_then(|mut w| {
        if ready {
            w.ready_resolvers.take()
        } else {
            w.closed_resolvers.take()
        }
    });
    match (resolvers, outcome) {
        (Some(resolvers), Ok(())) => resolve(&resolvers, JsValue::undefined(), context),
        (Some(resolvers), Err(reason)) => reject(&resolvers, reason, context),
        (None, outcome) => {
            let (promise, resolvers) = pending(context);
            match outcome {
                Ok(()) => resolve(&resolvers, JsValue::undefined(), context),
                Err(reason) => reject(&resolvers, reason, context),
            }
            if let Some(mut w) = writer.downcast_mut::<Writer>() {
                if ready {
                    w.ready = promise;
                } else {
                    w.closed = promise;
                }
            }
        }
    }
}

/// The writer's `ready` made a wait again: backpressure began.
fn reset_ready(context: &mut Context, writer: &JsObject) {
    let (promise, resolvers) = pending(context);
    if let Some(mut w) = writer.downcast_mut::<Writer>()
        && w.ready_resolvers.is_none()
    {
        w.ready = promise;
        w.ready_resolvers = Some(resolvers);
    }
}

fn update_backpressure(context: &mut Context, stream: &JsObject) {
    let change = with_writable(stream, |w| {
        if w.phase != WPhase::Writable || w.close_resolvers.is_some() {
            return None;
        }
        let now = w.high_water_mark - w.queue_size <= 0.;
        if now == w.backpressure {
            return None;
        }
        w.backpressure = now;
        Some((now, w.writer.clone()))
    });
    if let Some((now, Some(writer))) = change {
        if now {
            reset_ready(context, &writer);
        } else {
            settle_writer(context, &writer, true, Ok(()));
        }
    }
}

enum Next {
    Nothing,
    FinishErroring,
    Close(JsValue, JsValue),
    Write(JsValue, JsValue, JsValue, JsValue),
}

/// Hands the sink the next write, or the close, once it is ready for it.
fn advance(context: &mut Context, stream: &JsObject) -> JsResult<()> {
    let next = with_writable(stream, |w| {
        if !w.started || w.in_flight_write || w.in_flight_close {
            return Next::Nothing;
        }
        if w.phase == WPhase::Erroring {
            return Next::FinishErroring;
        }
        if w.phase != WPhase::Writable {
            return Next::Nothing;
        }
        let controller: JsValue = w.controller.clone().map(JsValue::from).unwrap_or_default();
        match w.queue.front() {
            None if w.close_resolvers.is_some() => {
                w.in_flight_close = true;
                Next::Close(w.sink.clone(), w.close.clone())
            }
            None => Next::Nothing,
            Some(write) => {
                w.in_flight_write = true;
                Next::Write(
                    w.sink.clone(),
                    w.write.clone(),
                    write.chunk.clone(),
                    controller,
                )
            }
        }
    });
    match next {
        Next::Nothing => {}
        Next::FinishErroring => finish_erroring(context, stream)?,
        Next::Close(sink, close) => {
            let promise = promise_call(&sink, &close, &[], context)?;
            when(
                context,
                promise,
                stream.clone(),
                |stream, _, cx| {
                    let (resolvers, abort, writer) = with_writable(stream, |w| {
                        w.in_flight_close = false;
                        if w.phase == WPhase::Erroring {
                            w.error = JsValue::undefined();
                        }
                        w.phase = WPhase::Closed;
                        (w.close_resolvers.take(), w.abort.take(), w.writer.clone())
                    });
                    if let Some(resolvers) = resolvers {
                        resolve(&resolvers, JsValue::undefined(), cx);
                    }
                    if let Some(abort) = abort {
                        resolve(&abort.resolvers, JsValue::undefined(), cx);
                    }
                    if let Some(writer) = writer {
                        settle_writer(cx, &writer, false, Ok(()));
                    }
                    Ok(JsValue::undefined())
                },
                |stream, reason, cx| {
                    let (resolvers, abort) = with_writable(stream, |w| {
                        w.in_flight_close = false;
                        (w.close_resolvers.take(), w.abort.take())
                    });
                    if let Some(resolvers) = resolvers {
                        reject(&resolvers, reason.clone(), cx);
                    }
                    if let Some(abort) = abort {
                        reject(&abort.resolvers, reason.clone(), cx);
                    }
                    deal_with_rejection(cx, stream, reason)?;
                    Ok(JsValue::undefined())
                },
            )?;
        }
        Next::Write(sink, write, chunk, controller) => {
            let promise = promise_call(&sink, &write, &[chunk, controller], context)?;
            when(
                context,
                promise,
                stream.clone(),
                |stream, _, cx| {
                    let done = with_writable(stream, |w| {
                        w.in_flight_write = false;
                        let done = w.queue.pop_front();
                        if let Some(done) = &done {
                            w.queue_size = (w.queue_size - done.size).max(0.);
                        }
                        done
                    });
                    if let Some(done) = done {
                        resolve(&done.resolvers, JsValue::undefined(), cx);
                    }
                    update_backpressure(cx, stream);
                    advance(cx, stream)?;
                    Ok(JsValue::undefined())
                },
                |stream, reason, cx| {
                    let done = with_writable(stream, |w| {
                        w.in_flight_write = false;
                        let done = w.queue.pop_front();
                        if let Some(done) = &done {
                            w.queue_size = (w.queue_size - done.size).max(0.);
                        }
                        done
                    });
                    if let Some(done) = done {
                        reject(&done.resolvers, reason.clone(), cx);
                    }
                    deal_with_rejection(cx, stream, reason)?;
                    Ok(JsValue::undefined())
                },
            )?;
        }
    }
    Ok(())
}

/// The sink failed: the stream errors (or, already erroring, finishes doing so).
fn deal_with_rejection(context: &mut Context, stream: &JsObject, reason: JsValue) -> JsResult<()> {
    match with_writable(stream, |w| w.phase) {
        WPhase::Writable => start_erroring(context, stream, reason),
        WPhase::Erroring => finish_erroring(context, stream)?,
        _ => {}
    }
    Ok(())
}

/// The stream starts failing with `reason`: writes still queued will reject once the one under
/// way, if any, is over.
fn start_erroring(context: &mut Context, stream: &JsObject, reason: JsValue) {
    let (writer, idle) = with_writable(stream, |w| {
        w.phase = WPhase::Erroring;
        w.error = reason.clone();
        (
            w.writer.clone(),
            !w.in_flight_write && !w.in_flight_close && w.started,
        )
    });
    if let Some(writer) = writer {
        settle_writer(context, &writer, true, Err(reason));
    }
    if idle {
        let _ = finish_erroring(context, stream);
    }
}

fn finish_erroring(context: &mut Context, stream: &JsObject) -> JsResult<()> {
    let (writes, abort, error, sink, abort_fn) = with_writable(stream, |w| {
        w.phase = WPhase::Errored;
        w.queue_size = 0.;
        (
            std::mem::take(&mut w.queue),
            w.abort.take(),
            w.error.clone(),
            w.sink.clone(),
            w.abort_fn.clone(),
        )
    });
    for write in writes {
        reject(&write.resolvers, error.clone(), context);
    }
    let Some(abort) = abort else {
        reject_close_and_closed(context, stream);
        return Ok(());
    };
    if abort.was_erroring {
        reject(&abort.resolvers, error, context);
        reject_close_and_closed(context, stream);
        return Ok(());
    }
    let promise = promise_call(
        &sink,
        &abort_fn,
        std::slice::from_ref(&abort.reason),
        context,
    )?;
    when(
        context,
        promise,
        (stream.clone(), abort.resolvers.clone()),
        |(stream, resolvers), _, cx| {
            resolve(resolvers, JsValue::undefined(), cx);
            reject_close_and_closed(cx, stream);
            Ok(JsValue::undefined())
        },
        |(stream, resolvers), reason, cx| {
            reject(resolvers, reason, cx);
            reject_close_and_closed(cx, stream);
            Ok(JsValue::undefined())
        },
    )?;
    Ok(())
}

fn reject_close_and_closed(context: &mut Context, stream: &JsObject) {
    let (resolvers, writer, error) = with_writable(stream, |w| {
        (w.close_resolvers.take(), w.writer.clone(), w.error.clone())
    });
    if let Some(resolvers) = resolvers {
        reject(&resolvers, error.clone(), context);
    }
    if let Some(writer) = writer {
        settle_writer(context, &writer, false, Err(error));
    }
}

/// Aborts the stream with `reason`: its signal aborts, queued writes reject, and the sink's
/// `abort` is told. Answers a promise of nothing.
pub(crate) fn writable_abort(
    context: &mut Context,
    stream: &JsObject,
    reason: JsValue,
) -> JsResult<JsValue> {
    let (phase, controller) = with_writable(stream, |w| (w.phase, w.abort_controller.clone()));
    if matches!(phase, WPhase::Closed | WPhase::Errored) {
        return resolved(JsValue::undefined(), context);
    }
    if controller.is_object() {
        let _ = invoke(&controller, "abort", std::slice::from_ref(&reason), context);
    }
    let (phase, existing) = with_writable(stream, |w| {
        (w.phase, w.abort.as_ref().map(|a| a.promise.clone()))
    });
    if matches!(phase, WPhase::Closed | WPhase::Errored) {
        return resolved(JsValue::undefined(), context);
    }
    if let Some(existing) = existing {
        return Ok(existing.into());
    }
    let was_erroring = phase == WPhase::Erroring;
    let reason = if was_erroring {
        JsValue::undefined()
    } else {
        reason
    };
    let (promise, resolvers) = pending(context);
    with_writable(stream, |w| {
        w.abort = Some(PendingAbort {
            promise: promise.clone(),
            resolvers,
            reason: reason.clone(),
            was_erroring,
        });
    });
    if !was_erroring {
        start_erroring(context, stream, reason);
    }
    Ok(promise.into())
}

fn writable_close(context: &mut Context, stream: &JsObject) -> JsResult<JsValue> {
    let (phase, closing, writer, backpressure) = with_writable(stream, |w| {
        (
            w.phase,
            w.close_resolvers.is_some(),
            w.writer.clone(),
            w.backpressure,
        )
    });
    if matches!(phase, WPhase::Closed | WPhase::Errored) || closing {
        let reason = type_error_value("The stream is closed or closing", context);
        return rejected(reason, context);
    }
    let (promise, resolvers) = pending(context);
    with_writable(stream, |w| w.close_resolvers = Some(resolvers));
    if let Some(writer) = writer
        && backpressure
        && phase == WPhase::Writable
    {
        settle_writer(context, &writer, true, Ok(()));
    }
    advance(context, stream)?;
    Ok(promise.into())
}

/// A writer's `write(chunk)`, from Rust.
pub(crate) fn writer_write(
    context: &mut Context,
    writer: &JsObject,
    chunk: JsValue,
) -> JsResult<JsValue> {
    let Some(stream) = writer
        .downcast_ref::<Writer>()
        .and_then(|w| w.stream.clone())
    else {
        let reason = type_error_value("This writer has been released", context);
        return rejected(reason, context);
    };
    let size_fn = with_writable(&stream, |w| w.size.clone());
    let size = if size_fn.is_undefined() {
        Ok(1.)
    } else {
        crate::call(
            context,
            &size_fn,
            &JsValue::undefined(),
            std::slice::from_ref(&chunk),
        )
        .and_then(|s| s.to_number(context))
    };
    let size = match size {
        Ok(size) => size,
        Err(error) => {
            let reason = error_value(error, context);
            if with_writable(&stream, |w| w.phase) == WPhase::Writable {
                start_erroring(context, &stream, reason.clone());
            }
            return rejected(reason, context);
        }
    };
    let (phase, error, closing) = with_writable(&stream, |w| {
        (w.phase, w.error.clone(), w.close_resolvers.is_some())
    });
    match phase {
        WPhase::Errored | WPhase::Erroring => return rejected(error, context),
        WPhase::Closed => {
            let reason = type_error_value("The stream is closed", context);
            return rejected(reason, context);
        }
        WPhase::Writable if closing => {
            let reason = type_error_value("The stream is closing", context);
            return rejected(reason, context);
        }
        WPhase::Writable => {}
    }
    let (promise, resolvers) = pending(context);
    with_writable(&stream, |w| {
        w.queue.push_back(Write {
            chunk,
            size,
            resolvers,
        });
        w.queue_size += size;
    });
    update_backpressure(context, &stream);
    advance(context, &stream)?;
    Ok(promise.into())
}

/// A writer's `close()`, from Rust.
pub(crate) fn writer_close(context: &mut Context, writer: &JsObject) -> JsResult<JsValue> {
    match writer
        .downcast_ref::<Writer>()
        .and_then(|w| w.stream.clone())
    {
        Some(stream) => writable_close(context, &stream),
        None => {
            let reason = type_error_value("This writer has been released", context);
            rejected(reason, context)
        }
    }
}

/// Lets go of the stream: `ready` and `closed` reject.
pub(crate) fn release_writer(context: &mut Context, writer: &JsObject) {
    let Some(stream) = writer
        .downcast_ref::<Writer>()
        .and_then(|w| w.stream.clone())
    else {
        return;
    };
    with_writable(&stream, |w| w.writer = None);
    let reason = type_error_value("The writer was released", context);
    settle_writer(context, writer, true, Err(reason.clone()));
    settle_writer(context, writer, false, Err(reason));
    if let Some(mut w) = writer.downcast_mut::<Writer>() {
        w.stream = None;
    }
}

fn writer_desired_size(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let writer = writer_of(this)?;
    let stream = writer
        .downcast_ref::<Writer>()
        .and_then(|w| w.stream.clone())
        .ok_or_else(|| type_error("This writer has been released"))?;
    Ok(with_writable(&stream, |w| match w.phase {
        WPhase::Errored | WPhase::Erroring => JsValue::null(),
        WPhase::Closed => 0.into(),
        WPhase::Writable => (w.high_water_mark - w.queue_size).into(),
    }))
}

const WRITER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("write", 1, |this, args, cx| match writer_of(this) {
        Ok(writer) => writer_write(cx, &writer, arg(args, 0)),
        Err(error) => {
            let reason = error_value(error, cx);
            rejected(reason, cx)
        }
    }),
    ("close", 0, |this, _, cx| match writer_of(this) {
        Ok(writer) => writer_close(cx, &writer),
        Err(error) => {
            let reason = error_value(error, cx);
            rejected(reason, cx)
        }
    }),
    ("abort", 1, |this, args, cx| {
        let writer = writer_of(this)?;
        match writer
            .downcast_ref::<Writer>()
            .and_then(|w| w.stream.clone())
        {
            Some(stream) => writable_abort(cx, &stream, arg(args, 0)),
            None => {
                let reason = type_error_value("This writer has been released", cx);
                rejected(reason, cx)
            }
        }
    }),
    ("releaseLock", 0, |this, _, cx| {
        let writer = writer_of(this)?;
        release_writer(cx, &writer);
        Ok(JsValue::undefined())
    }),
];

const WRITABLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("abort", 1, |this, args, cx| {
        let stream = writable_of(this)?;
        if with_writable(&stream, |w| w.writer.is_some()) {
            let reason = type_error_value("Cannot abort a locked WritableStream", cx);
            return rejected(reason, cx);
        }
        writable_abort(cx, &stream, arg(args, 0))
    }),
    ("close", 0, |this, _, cx| {
        let stream = writable_of(this)?;
        if with_writable(&stream, |w| w.writer.is_some()) {
            let reason = type_error_value("Cannot close a locked WritableStream", cx);
            return rejected(reason, cx);
        }
        writable_close(cx, &stream)
    }),
    ("getWriter", 0, |this, _, cx| {
        let stream = writable_of(this)?;
        Ok(acquire_writer(cx, &stream)?.into())
    }),
];

// ─── TransformStream ────────────────────────────────────────────────────────────────────────

/// A `TransformStream`: its two sides, the transformer, and the backpressure between them.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct Transform {
    readable: Option<JsObject>,
    writable: Option<JsObject>,
    #[unsafe_ignore_trace]
    backpressure: bool,
    backpressure_changed: Option<(JsObject, ResolvingFunctions)>,
    controller: Option<JsObject>,
    transformer: JsValue,
    transform: JsValue,
    flush: JsValue,
    cancel: JsValue,
}

fn with_transform<R>(stream: &JsObject, f: impl FnOnce(&mut Transform) -> R) -> R {
    f(&mut stream
        .downcast_mut::<Transform>()
        .expect("a TransformStream"))
}

fn transform_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Transform>())
        .ok_or_else(|| type_error("not a TransformStream"))
}

fn controlled_transform(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .and_then(|o| o.downcast_ref::<Controlled>().map(|c| c.stream.clone()))
        .filter(|s| s.is::<Transform>())
        .ok_or_else(|| type_error("not a TransformStream controller"))
}

fn construct_transform(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let proto = classes(context)?.transform_proto.clone();
    let proto = prototype_for(this, "TransformStream", proto, context)?;
    Ok(make_transform(context, proto, &arg(args, 0), &arg(args, 1), &arg(args, 2))?.into())
}

/// A `TransformStream` of `transformer` — an object whose `start`, `transform`, `flush` and
/// `cancel` may be native functions — as `new TransformStream(transformer)`. A
/// `CompressionStream`, say, is one of these with a transformer holding a codec.
pub(crate) fn new_transform(context: &mut Context, transformer: &JsValue) -> JsResult<JsObject> {
    let proto = classes(context)?.transform_proto.clone();
    make_transform(
        context,
        proto,
        transformer,
        &JsValue::undefined(),
        &JsValue::undefined(),
    )
}

fn make_transform(
    context: &mut Context,
    proto: JsObject,
    transformer: &JsValue,
    writable_strategy: &JsValue,
    readable_strategy: &JsValue,
) -> JsResult<JsObject> {
    let transformer = if transformer.is_null() {
        JsValue::undefined()
    } else {
        transformer.clone()
    };
    if !get(&transformer, "readableType", context)?.is_undefined()
        || !get(&transformer, "writableType", context)?.is_undefined()
    {
        return Err(crate::native::range_error(
            "Invalid readableType or writableType for a transformer",
        ));
    }
    let start = method_of(&transformer, "start", context)?;
    let transform_fn = method_of(&transformer, "transform", context)?;
    let flush = method_of(&transformer, "flush", context)?;
    let cancel = method_of(&transformer, "cancel", context)?;
    let stream = JsObject::from_proto_and_data(
        Some(proto),
        Transform {
            readable: None,
            writable: None,
            backpressure: false,
            backpressure_changed: None,
            controller: None,
            transformer: transformer.clone(),
            transform: transform_fn,
            flush,
            cancel,
        },
    );
    let (start_promise, start_resolvers) = pending(context);
    let sink = object(context);
    let starting = native(
        context,
        "start",
        0,
        start_promise.clone(),
        |_, _, promise, _| Ok(promise.clone().into()),
    );
    sink.set(js_string!("start"), starting.clone(), false, context)?;
    let write = native(
        context,
        "write",
        1,
        stream.clone(),
        |_, args, stream, cx| sink_write(cx, stream, arg(args, 0)),
    );
    sink.set(js_string!("write"), write, false, context)?;
    let close = native(context, "close", 0, stream.clone(), |_, _, stream, cx| {
        sink_close(cx, stream)
    });
    sink.set(js_string!("close"), close, false, context)?;
    let abort = native(
        context,
        "abort",
        1,
        stream.clone(),
        |_, args, stream, cx| sink_abort(cx, stream, arg(args, 0)),
    );
    sink.set(js_string!("abort"), abort, false, context)?;
    let source = object(context);
    source.set(js_string!("start"), starting, false, context)?;
    let pull = native(context, "pull", 0, stream.clone(), |_, _, stream, cx| {
        set_backpressure(cx, stream, false);
        let changed = with_transform(stream, |t| {
            t.backpressure_changed.as_ref().map(|(p, _)| p.clone())
        });
        Ok(changed.map(JsValue::from).unwrap_or_default())
    });
    source.set(js_string!("pull"), pull, false, context)?;
    let cancel = native(
        context,
        "cancel",
        1,
        stream.clone(),
        |_, args, stream, cx| source_cancel(cx, stream, arg(args, 0)),
    );
    source.set(js_string!("cancel"), cancel, false, context)?;
    let writable = new_writable(context, &sink.into(), writable_strategy)?;
    let readable_strategy = if readable_strategy.is_undefined() {
        let strategy = object(context);
        strategy.set(js_string!("highWaterMark"), 0, false, context)?;
        strategy.into()
    } else {
        readable_strategy.clone()
    };
    let readable = new_readable(context, &source.into(), &readable_strategy)?;
    let proto = classes(context)?.transform_controller_proto.clone();
    let controller = JsObject::from_proto_and_data(
        Some(proto),
        Controlled {
            stream: stream.clone(),
        },
    );
    with_transform(&stream, |t| {
        t.readable = Some(readable);
        t.writable = Some(writable);
        t.controller = Some(controller.clone());
    });
    set_backpressure(context, &stream, true);
    let started = if start.is_undefined() {
        JsValue::undefined()
    } else {
        crate::call(context, &start, &transformer, &[controller.into()])?
    };
    resolve(&start_resolvers, started, context);
    Ok(stream)
}

fn transform_sides(stream: &JsObject) -> (JsObject, JsObject) {
    with_transform(stream, |t| {
        (
            t.readable.clone().expect("a readable side"),
            t.writable.clone().expect("a writable side"),
        )
    })
}

fn set_backpressure(context: &mut Context, stream: &JsObject, value: bool) {
    let old = with_transform(stream, |t| t.backpressure_changed.take());
    if let Some((_, resolvers)) = old {
        resolve(&resolvers, JsValue::undefined(), context);
    }
    let fresh = pending(context);
    with_transform(stream, |t| {
        t.backpressure_changed = Some(fresh);
        t.backpressure = value;
    });
}

/// The writable side fails with `reason`, and a write waiting on backpressure is let go.
fn error_writable_and_unblock(context: &mut Context, stream: &JsObject, reason: JsValue) {
    let (_, writable) = transform_sides(stream);
    if with_writable(&writable, |w| w.phase) == WPhase::Writable {
        start_erroring(context, &writable, reason);
    }
    if with_transform(stream, |t| t.backpressure) {
        set_backpressure(context, stream, false);
    }
}

fn transform_error(context: &mut Context, stream: &JsObject, reason: JsValue) {
    let (readable, _) = transform_sides(stream);
    readable_error(context, &readable, reason.clone());
    error_writable_and_unblock(context, stream, reason);
}

/// The controller's `enqueue(chunk)`, from Rust: what a native transformer calls.
pub(crate) fn transform_enqueue(
    context: &mut Context,
    controller: &JsValue,
    chunk: JsValue,
) -> JsResult<()> {
    let stream = controlled_transform(controller)?;
    let (readable, _) = transform_sides(&stream);
    if !can_close_or_enqueue(&readable) {
        return Err(type_error(
            "The readable side is not in a state that permits enqueue",
        ));
    }
    if let Err(error) = enqueue(context, &readable, chunk) {
        let reason = error_value(error, context);
        error_writable_and_unblock(context, &stream, reason);
        let stored = with_readable(&readable, |r| r.error.clone());
        return Err(JsError::from_opaque(stored));
    }
    let full = desired_size(&readable).is_none_or(|d| d <= 0.);
    if full && !with_transform(&stream, |t| t.backpressure) {
        set_backpressure(context, &stream, true);
    }
    Ok(())
}

fn perform_transform(
    context: &mut Context,
    stream: &JsObject,
    chunk: JsValue,
) -> JsResult<JsValue> {
    let (transformer, transform, controller) = with_transform(stream, |t| {
        (
            t.transformer.clone(),
            t.transform.clone(),
            t.controller.clone(),
        )
    });
    let controller: JsValue = controller.map(JsValue::from).unwrap_or_default();
    let promise = if transform.is_undefined() {
        // No transformer: what is written is what is read.
        match transform_enqueue(context, &controller, chunk) {
            Ok(()) => resolved(JsValue::undefined(), context)?,
            Err(error) => {
                let reason = error_value(error, context);
                rejected(reason, context)?
            }
        }
    } else {
        promise_call(&transformer, &transform, &[chunk, controller], context)?
    };
    Ok(when(
        context,
        promise,
        stream.clone(),
        |_, _, _| Ok(JsValue::undefined()),
        |stream, reason, cx| {
            transform_error(cx, stream, reason.clone());
            Err(JsError::from_opaque(reason))
        },
    )?
    .into())
}

fn sink_write(context: &mut Context, stream: &JsObject, chunk: JsValue) -> JsResult<JsValue> {
    let (backpressure, changed) = with_transform(stream, |t| {
        (
            t.backpressure,
            t.backpressure_changed.as_ref().map(|(p, _)| p.clone()),
        )
    });
    if backpressure && let Some(changed) = changed {
        return Ok(when(
            context,
            changed.into(),
            (stream.clone(), chunk),
            |(stream, chunk), _, cx| {
                let (_, writable) = transform_sides(stream);
                let (phase, error) = with_writable(&writable, |w| (w.phase, w.error.clone()));
                if phase == WPhase::Erroring {
                    return Err(JsError::from_opaque(error));
                }
                perform_transform(cx, stream, chunk.clone())
            },
            rethrow,
        )?
        .into());
    }
    perform_transform(context, stream, chunk)
}

fn sink_close(context: &mut Context, stream: &JsObject) -> JsResult<JsValue> {
    let (transformer, flush, controller) = with_transform(stream, |t| {
        (t.transformer.clone(), t.flush.clone(), t.controller.clone())
    });
    let controller: JsValue = controller.map(JsValue::from).unwrap_or_default();
    let promise = promise_call(&transformer, &flush, &[controller], context)?;
    Ok(when(
        context,
        promise,
        stream.clone(),
        |stream, _, cx| {
            let (readable, _) = transform_sides(stream);
            let (phase, error) = with_readable(&readable, |r| (r.phase, r.error.clone()));
            if phase == Phase::Errored {
                return Err(JsError::from_opaque(error));
            }
            if can_close_or_enqueue(&readable) {
                controller_close(cx, &readable)?;
            }
            Ok(JsValue::undefined())
        },
        |stream, reason, cx| {
            let (readable, _) = transform_sides(stream);
            readable_error(cx, &readable, reason.clone());
            Err(JsError::from_opaque(reason))
        },
    )?
    .into())
}

fn sink_abort(context: &mut Context, stream: &JsObject, reason: JsValue) -> JsResult<JsValue> {
    let (transformer, cancel) =
        with_transform(stream, |t| (t.transformer.clone(), t.cancel.clone()));
    let promise = promise_call(
        &transformer,
        &cancel,
        std::slice::from_ref(&reason),
        context,
    )?;
    Ok(when(
        context,
        promise,
        (stream.clone(), reason),
        |(stream, reason), _, cx| {
            let (readable, _) = transform_sides(stream);
            readable_error(cx, &readable, reason.clone());
            Ok(JsValue::undefined())
        },
        |(stream, _), error, cx| {
            let (readable, _) = transform_sides(stream);
            readable_error(cx, &readable, error.clone());
            Err(JsError::from_opaque(error))
        },
    )?
    .into())
}

fn source_cancel(context: &mut Context, stream: &JsObject, reason: JsValue) -> JsResult<JsValue> {
    let (transformer, cancel) =
        with_transform(stream, |t| (t.transformer.clone(), t.cancel.clone()));
    let promise = promise_call(
        &transformer,
        &cancel,
        std::slice::from_ref(&reason),
        context,
    )?;
    Ok(when(
        context,
        promise,
        (stream.clone(), reason),
        |(stream, reason), _, cx| {
            error_writable_and_unblock(cx, stream, reason.clone());
            Ok(JsValue::undefined())
        },
        |(stream, _), error, cx| {
            error_writable_and_unblock(cx, stream, error.clone());
            Err(JsError::from_opaque(error))
        },
    )?
    .into())
}

const TRANSFORM_CONTROLLER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("enqueue", 1, |this, args, cx| {
        transform_enqueue(cx, this, arg(args, 0))?;
        Ok(JsValue::undefined())
    }),
    ("error", 1, |this, args, cx| {
        let stream = controlled_transform(this)?;
        transform_error(cx, &stream, arg(args, 0));
        Ok(JsValue::undefined())
    }),
    ("terminate", 0, |this, _, cx| {
        let stream = controlled_transform(this)?;
        let (readable, _) = transform_sides(&stream);
        if can_close_or_enqueue(&readable) {
            controller_close(cx, &readable)?;
        }
        let reason = type_error_value("The TransformStream was terminated", cx);
        error_writable_and_unblock(cx, &stream, reason);
        Ok(JsValue::undefined())
    }),
];

// ─── A transform with two sides, as a class of its own ──────────────────────────────────────

/// What a `TextEncoderStream`-like class holds: the `TransformStream` doing the work, and
/// whatever else its getters answer.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct Pair {
    transform: JsObject,
    extra: JsValue,
}

fn pair_of(value: &JsValue) -> JsResult<JsObject> {
    value
        .as_object()
        .filter(|o| o.is::<Pair>())
        .ok_or_else(|| type_error("Illegal invocation"))
}

fn pair_extra(this: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    let pair = pair_of(this)?;
    let extra = pair
        .downcast_ref::<Pair>()
        .map(|p| p.extra.clone())
        .unwrap_or_default();
    get(&extra, key, context)
}

/// A class whose instances have a `readable` and a `writable` side — `TextDecoderStream`,
/// `CompressionStream` — made by `construct`, which makes them with [`pair_instance`].
pub(crate) fn pair_class(
    context: &mut Context,
    name: &str,
    length: usize,
    construct: NativeFunctionPointer,
    accessors: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)],
) -> JsResult<(JsObject, JsObject)> {
    let mut all: Vec<(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)> = vec![
        (
            "readable",
            |this, _, _| {
                let pair = pair_of(this)?;
                let transform = pair
                    .downcast_ref::<Pair>()
                    .map(|p| p.transform.clone())
                    .expect("a pair");
                Ok(transform_sides(&transform).0.into())
            },
            None,
        ),
        (
            "writable",
            |this, _, _| {
                let pair = pair_of(this)?;
                let transform = pair
                    .downcast_ref::<Pair>()
                    .map(|p| p.transform.clone())
                    .expect("a pair");
                Ok(transform_sides(&transform).1.into())
            },
            None,
        ),
    ];
    all.extend_from_slice(accessors);
    web_class(context, name, length, construct, &[], &all, &[])
}

/// An instance of a [`pair_class`]: on `new.target`'s prototype (`fallback` when it has none),
/// with the transform doing the work and `extra` for its own getters.
pub(crate) fn pair_instance(
    context: &mut Context,
    new_target: &JsValue,
    name: &str,
    fallback: JsObject,
    transform: JsObject,
    extra: JsValue,
) -> JsResult<JsObject> {
    let proto = prototype_for(new_target, name, fallback, context)?;
    Ok(JsObject::from_proto_and_data(
        Some(proto),
        Pair { transform, extra },
    ))
}

/// A `TransformStream` whose transformer is native: `transform` is called with each chunk and
/// the controller (enqueue with [`transform_enqueue`]), `flush` once at the end; both are
/// handed `state`.
pub(crate) fn native_transform<T: Trace + Clone + 'static>(
    context: &mut Context,
    state: T,
    transform: fn(&T, JsValue, &JsValue, &mut Context) -> JsResult<()>,
    flush: fn(&T, &JsValue, &mut Context) -> JsResult<()>,
) -> JsResult<JsObject> {
    let transformer = object(context);
    let on_chunk = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            move |_, args, state: &T, cx| {
                transform(state, arg(args, 0), &arg(args, 1), cx)?;
                Ok(JsValue::undefined())
            },
            state.clone(),
        ),
    )
    .name(js_string!("transform"))
    .length(2)
    .build();
    let on_end = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            move |_, args, state: &T, cx| {
                flush(state, &arg(args, 0), cx)?;
                Ok(JsValue::undefined())
            },
            state,
        ),
    )
    .name(js_string!("flush"))
    .length(1)
    .build();
    transformer.set(js_string!("transform"), on_chunk, false, context)?;
    transformer.set(js_string!("flush"), on_end, false, context)?;
    new_transform(context, &transformer.into())
}

// ─── TextEncoderStream and TextDecoderStream ────────────────────────────────────────────────

/// A text stream's carry-over between chunks: a high surrogate waiting for its pair, or the
/// bytes of a character split across chunks.
#[derive(Trace, Finalize, JsData, Default)]
struct TextCarry {
    #[unsafe_ignore_trace]
    high_surrogate: Option<u16>,
    #[unsafe_ignore_trace]
    pending: Vec<u8>,
    #[unsafe_ignore_trace]
    bom_seen: bool,
    #[unsafe_ignore_trace]
    encoding: String,
    #[unsafe_ignore_trace]
    fatal: bool,
    #[unsafe_ignore_trace]
    ignore_bom: bool,
}

fn construct_text_encoder_stream(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let fallback = classes(context)?.text_encoder_proto.clone();
    if this.is_undefined() {
        return Err(type_error(
            "Class constructor TextEncoderStream cannot be invoked without 'new'",
        ));
    }
    let carry = JsObject::from_proto_and_data(None, TextCarry::default());
    let transform = native_transform(
        context,
        carry,
        |carry, chunk, controller, cx| {
            let text = chunk.to_string(cx)?;
            let mut units = text.to_vec();
            if let Some(high) = carry
                .downcast_mut::<TextCarry>()
                .and_then(|mut c| c.high_surrogate.take())
            {
                units.insert(0, high);
            }
            if units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
                let high = units.pop();
                carry
                    .downcast_mut::<TextCarry>()
                    .expect("a carry")
                    .high_surrogate = high;
            }
            let text: String = char::decode_utf16(units)
                .map(|c| c.unwrap_or('\u{FFFD}'))
                .collect();
            if !text.is_empty() {
                let bytes = u8array(text.into_bytes(), cx)?;
                transform_enqueue(cx, controller, bytes)?;
            }
            Ok(())
        },
        |carry, controller, cx| {
            if carry
                .downcast_mut::<TextCarry>()
                .and_then(|mut c| c.high_surrogate.take())
                .is_some()
            {
                let bytes = u8array("\u{FFFD}".as_bytes().to_vec(), cx)?;
                transform_enqueue(cx, controller, bytes)?;
            }
            Ok(())
        },
    )?;
    Ok(pair_instance(
        context,
        this,
        "TextEncoderStream",
        fallback,
        transform,
        JsValue::undefined(),
    )?
    .into())
}

/// The encoding a `TextDecoder` label names, as `TextDecoder` reads them.
fn decoder_encoding(label: &str) -> Option<&'static str> {
    match label.trim().to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some("utf-8"),
        "utf-16le" | "utf-16" => Some("utf-16le"),
        "latin1" | "iso-8859-1" | "windows-1252" | "ascii" | "us-ascii" => Some("windows-1252"),
        _ => None,
    }
}

/// Decodes what is pending in `carry` — all of it when `last` — leaving a character split
/// across chunks for the next one.
fn decode_carry(carry: &mut TextCarry, last: bool) -> Result<String, &'static str> {
    let mut out = String::new();
    match carry.encoding.as_str() {
        "utf-16le" => {
            if !carry.bom_seen && !carry.ignore_bom {
                if carry.pending.len() < 2 && !last {
                    return Ok(out);
                }
                if carry.pending.starts_with(&[0xff, 0xfe]) {
                    carry.pending.drain(..2);
                }
            }
            carry.bom_seen = true;
            let whole = carry.pending.len() / 2 * 2;
            let mut units: Vec<u16> = carry.pending[..whole]
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            let mut keep: Vec<u8> = carry.pending[whole..].to_vec();
            if !last && units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
                let high = units.pop().expect("a unit");
                let mut held = high.to_le_bytes().to_vec();
                held.extend(keep);
                keep = held;
            }
            for c in char::decode_utf16(units) {
                match c {
                    Ok(c) => out.push(c),
                    Err(_) if carry.fatal => return Err("The data is not valid utf-16le"),
                    Err(_) => out.push('\u{FFFD}'),
                }
            }
            if last && !keep.is_empty() {
                if carry.fatal {
                    return Err("The data ends in the middle of a character");
                }
                out.push('\u{FFFD}');
                keep.clear();
            }
            carry.pending = keep;
        }
        "windows-1252" => {
            out.extend(carry.pending.drain(..).map(char::from));
        }
        _ => {
            if !carry.bom_seen && !carry.ignore_bom {
                if carry.pending.len() < 3
                    && [0xef, 0xbb, 0xbf].starts_with(&carry.pending)
                    && !last
                {
                    return Ok(out);
                }
                if carry.pending.starts_with(&[0xef, 0xbb, 0xbf]) {
                    carry.pending.drain(..3);
                }
            }
            carry.bom_seen = true;
            let mut at = 0;
            loop {
                match std::str::from_utf8(&carry.pending[at..]) {
                    Ok(text) => {
                        out.push_str(text);
                        at = carry.pending.len();
                        break;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        out.push_str(
                            std::str::from_utf8(&carry.pending[at..at + valid])
                                .expect("valid utf-8"),
                        );
                        at += valid;
                        match error.error_len() {
                            Some(bad) => {
                                if carry.fatal {
                                    return Err("The data is not valid utf-8");
                                }
                                out.push('\u{FFFD}');
                                at += bad;
                            }
                            None if last => {
                                if carry.fatal {
                                    return Err("The data ends in the middle of a character");
                                }
                                out.push('\u{FFFD}');
                                at = carry.pending.len();
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }
            carry.pending.drain(..at);
        }
    }
    Ok(out)
}

fn construct_text_decoder_stream(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let fallback = classes(context)?.text_decoder_proto.clone();
    if this.is_undefined() {
        return Err(type_error(
            "Class constructor TextDecoderStream cannot be invoked without 'new'",
        ));
    }
    let label = match arg(args, 0) {
        l if l.is_undefined() => "utf-8".to_owned(),
        l => text_of(&l, context)?,
    };
    let encoding = decoder_encoding(&label).ok_or_else(|| {
        crate::native::range_error(format!(
            "TextDecoderStream: the encoding '{label}' is not supported"
        ))
    })?;
    let options = arg(args, 1);
    let fatal = get(&options, "fatal", context)?.to_boolean();
    let ignore_bom = get(&options, "ignoreBOM", context)?.to_boolean();
    let carry = JsObject::from_proto_and_data(
        None,
        TextCarry {
            encoding: encoding.to_owned(),
            fatal,
            ignore_bom,
            high_surrogate: None,
            pending: Vec::new(),
            bom_seen: false,
        },
    );
    let transform = native_transform(
        context,
        carry,
        |carry, chunk, controller, cx| {
            let bytes = crate::encoding::bytes_of(&chunk, cx)?.ok_or_else(|| {
                type_error("TextDecoderStream: a chunk must be an ArrayBuffer or a view of one")
            })?;
            let text = {
                let mut c = carry.downcast_mut::<TextCarry>().expect("a carry");
                c.pending.extend(bytes);
                decode_carry(&mut c, false)
            };
            let text = text.map_err(type_error)?;
            if !text.is_empty() {
                transform_enqueue(cx, controller, js_text(&text))?;
            }
            Ok(())
        },
        |carry, controller, cx| {
            let text = decode_carry(
                &mut carry.downcast_mut::<TextCarry>().expect("a carry"),
                true,
            )
            .map_err(type_error)?;
            if !text.is_empty() {
                transform_enqueue(cx, controller, js_text(&text))?;
            }
            Ok(())
        },
    )?;
    let extra = object(context);
    extra.set(js_string!("encoding"), js_text(encoding), false, context)?;
    extra.set(js_string!("fatal"), fatal, false, context)?;
    extra.set(js_string!("ignoreBOM"), ignore_bom, false, context)?;
    Ok(pair_instance(
        context,
        this,
        "TextDecoderStream",
        fallback,
        transform,
        extra.into(),
    )?
    .into())
}

// ─── Installing them ────────────────────────────────────────────────────────────────────────

/// The globals, each made (with the rest) the first time one is read.
const GLOBALS: &[(&str, crate::native::GlobalMaker)] = &[
    ("ReadableStream", |cx| {
        Ok(classes(cx)?.readable.clone().into())
    }),
    ("ReadableStreamDefaultReader", |cx| {
        Ok(classes(cx)?.default_reader.clone().into())
    }),
    ("ReadableStreamBYOBReader", |cx| {
        Ok(classes(cx)?.byob_reader.clone().into())
    }),
    ("ReadableStreamDefaultController", |cx| {
        Ok(classes(cx)?.default_controller.clone().into())
    }),
    ("ReadableByteStreamController", |cx| {
        Ok(classes(cx)?.byte_controller.clone().into())
    }),
    ("WritableStream", |cx| {
        Ok(classes(cx)?.writable.clone().into())
    }),
    ("WritableStreamDefaultWriter", |cx| {
        Ok(classes(cx)?.writer.clone().into())
    }),
    ("WritableStreamDefaultController", |cx| {
        Ok(classes(cx)?.writable_controller.clone().into())
    }),
    ("TransformStream", |cx| {
        Ok(classes(cx)?.transform.clone().into())
    }),
    ("TransformStreamDefaultController", |cx| {
        Ok(classes(cx)?.transform_controller.clone().into())
    }),
    ("CountQueuingStrategy", |cx| {
        Ok(classes(cx)?.count_strategy.clone().into())
    }),
    ("ByteLengthQueuingStrategy", |cx| {
        Ok(classes(cx)?.byte_strategy.clone().into())
    }),
    ("TextEncoderStream", |cx| {
        Ok(classes(cx)?.text_encoder_stream.clone().into())
    }),
    ("TextDecoderStream", |cx| {
        Ok(classes(cx)?.text_decoder_stream.clone().into())
    }),
];

pub fn install(context: &mut Context) -> JsResult<()> {
    for (name, make) in GLOBALS {
        crate::native::lazy_global(context, name, *make)?;
    }
    Ok(())
}

/// `require('stream/web')`: the classes, as the globals are now — with `CompressionStream` and
/// `DecompressionStream` when the realm has them.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let names = GLOBALS
        .iter()
        .map(|(name, _)| *name)
        .chain(["CompressionStream", "DecompressionStream"]);
    for name in names {
        let value = crate::native::global(context, name)?;
        if !value.is_undefined() {
            module.set(JsString::from(name), value, false, context)?;
        }
    }
    Ok(module.into())
}
