//! Web Workers: `new Worker(url)` runs a script in a realm of its own — another runtime, on a
//! thread of its own — and the two speak by `postMessage`, as a page and its worker do.
//!
//! A worker works while its page does not wait for it: what it does — Dataview indexing a vault
//! note by note, say — happens on its thread, with its own collector, and the window the page is
//! drawn in goes on drawing. A message is copied out of one realm into memory neither owns
//! ([`Cloned`]: objects, arrays, dates, maps, sets, bytes, errors, cycles kept), sent across, and
//! made again on the other side: the worker hears it as soon as it can, its page on its beat
//! ([`crate::Runtime::run_workers`]), never waiting. What a worker writes to its console comes
//! out on its page's.
//!
//! The script comes from a `blob:` address — `URL.createObjectURL(new Blob([code]))`, how
//! bundles ship their workers — or a `data:` one.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArray, JsArrayBuffer, JsDate, JsMap, JsSet, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};

use crate::{Level, Runtime, shared};

/// A worker, as its page holds it: the way to its thread, and what came back from it.
pub(crate) struct WorkerSlot {
    pub id: u32,
    pub object: JsObject,
    to: Sender<ToWorker>,
    from: Receiver<FromWorker>,
    /// What came back and the page has not heard yet.
    heard: VecDeque<FromWorker>,
    progress: Arc<Progress>,
    pub closed: bool,
}

/// A worker's side of its messages, kept in its own runtime: what it sent, and whether it closed.
#[derive(Default)]
pub(crate) struct Port {
    pub outbox: Vec<JsValue>,
    pub closed: bool,
}

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

fn function(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

// ─── A value between threads ────────────────────────────────────────────────────────────────

/// A value copied out of a realm into memory no realm owns, so it can cross to another thread:
/// what a page and its worker send each other. Objects are numbered in the order they were
/// first met, and met again they are [`Cloned::Seen`] — cycles and shared parts come out as
/// they went in.
#[derive(Debug)]
pub(crate) enum Cloned {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    /// UTF-16, as JavaScript keeps it: a lone surrogate crosses as it is.
    String(Vec<u16>),
    BigInt(String),
    Date(f64),
    Buffer(Vec<u8>),
    /// A typed array or a view, made a `Uint8Array` on the other side.
    Bytes(Vec<u8>),
    Array(Vec<Cloned>),
    Object(Vec<(Vec<u16>, Cloned)>),
    /// An error: its own properties, with its name and message among them.
    Error(Vec<(Vec<u16>, Cloned)>),
    Map(Vec<(Cloned, Cloned)>),
    Set(Vec<Cloned>),
    Seen(usize),
}

/// `value`, out of `context`'s realm, as `structuredClone` copies it.
pub(crate) fn clone_out(value: &JsValue, context: &mut Context) -> JsResult<Cloned> {
    let mut seen = HashMap::new();
    clone_value(value, context, &mut seen)
}

fn clone_value(
    value: &JsValue,
    from: &mut Context,
    seen: &mut HashMap<JsObject, usize>,
) -> JsResult<Cloned> {
    if let Some(text) = value.as_string() {
        return Ok(Cloned::String(text.iter().collect()));
    }
    let Some(source) = value.as_object() else {
        if value.is_symbol() {
            return Err(type_error("DataCloneError: a Symbol could not be cloned"));
        }
        if value.is_bigint() {
            return Ok(Cloned::BigInt(
                value.to_string(from)?.to_std_string_escaped(),
            ));
        }
        if let Some(n) = value.as_number() {
            return Ok(Cloned::Number(n));
        }
        if let Some(b) = value.as_boolean() {
            return Ok(Cloned::Bool(b));
        }
        return Ok(if value.is_null() {
            Cloned::Null
        } else {
            Cloned::Undefined
        });
    };
    if let Some(at) = seen.get(&source) {
        return Ok(Cloned::Seen(*at));
    }
    if source.is_callable() {
        return Err(type_error("DataCloneError: a function could not be cloned"));
    }
    // Numbered before what is inside it, so what refers back to it finds it.
    let number = seen.len();
    seen.insert(source.clone(), number);
    if let Ok(date) = JsDate::from_object(source.clone()) {
        return Ok(Cloned::Date(
            date.get_time(from)?.as_number().unwrap_or(f64::NAN),
        ));
    }
    if let Ok(buffer) = JsArrayBuffer::from_object(source.clone()) {
        return Ok(Cloned::Buffer(buffer.to_vec().unwrap_or_default()));
    }
    if let Some(bytes) = crate::encoding::bytes_of(value, from)? {
        return Ok(Cloned::Bytes(bytes));
    }
    if let Ok(map) = JsMap::from_object(source.clone()) {
        let mut pairs = Vec::new();
        for pair in entries(&map.entries(from)?.into(), from)? {
            let pair = entries(&pair, from)?;
            let key = clone_value(pair.first().unwrap_or(&JsValue::undefined()), from, seen)?;
            let item = clone_value(pair.get(1).unwrap_or(&JsValue::undefined()), from, seen)?;
            pairs.push((key, item));
        }
        return Ok(Cloned::Map(pairs));
    }
    if let Ok(set) = JsSet::from_object(source.clone()) {
        let mut items = Vec::new();
        for item in entries(&set.values(from)?.into(), from)? {
            items.push(clone_value(&item, from, seen)?);
        }
        return Ok(Cloned::Set(items));
    }
    if source.is_array() {
        let array = JsArray::from_object(source.clone())?;
        let length = array.length(from)?;
        let mut items = Vec::with_capacity(length as usize);
        for at in 0..length {
            items.push(clone_value(&array.get(at, from)?, from, seen)?);
        }
        return Ok(Cloned::Array(items));
    }
    let keys_fn = from
        .intrinsics()
        .constructors()
        .object()
        .constructor()
        .get(js_string!("keys"), from)?;
    let keys = crate::call(
        from,
        &keys_fn,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    let error = source.is::<boa_engine::builtins::error::Error>();
    let mut names: Vec<JsString> = Vec::new();
    if let Some(keys) = keys.as_object() {
        let keys = JsArray::from_object(keys.clone())?;
        for at in 0..keys.length(from)? {
            names.push(keys.get(at, from)?.to_string(from)?);
        }
    }
    // An error keeps its name and message, which are not its own enumerable properties.
    if error {
        for key in [js_string!("name"), js_string!("message")] {
            if !names.contains(&key) {
                names.push(key);
            }
        }
    }
    let mut fields = Vec::with_capacity(names.len());
    for name in names {
        let item = source.get(name.clone(), from)?;
        fields.push((name.iter().collect(), clone_value(&item, from, seen)?));
    }
    Ok(if error {
        Cloned::Error(fields)
    } else {
        Cloned::Object(fields)
    })
}

/// `cloned`, made again in `context`'s realm.
pub(crate) fn clone_in(cloned: &Cloned, context: &mut Context) -> JsResult<JsValue> {
    let mut made = Vec::new();
    make(cloned, context, &mut made)
}

fn make(cloned: &Cloned, to: &mut Context, made: &mut Vec<JsObject>) -> JsResult<JsValue> {
    Ok(match cloned {
        Cloned::Undefined => JsValue::undefined(),
        Cloned::Null => JsValue::null(),
        Cloned::Bool(b) => (*b).into(),
        Cloned::Number(n) => (*n).into(),
        Cloned::String(units) => JsString::from(units.as_slice()).into(),
        Cloned::BigInt(text) => {
            boa_engine::JsBigInt::from_string(text).map_or(JsValue::undefined(), JsValue::from)
        }
        Cloned::Seen(at) => made
            .get(*at)
            .cloned()
            .map_or(JsValue::undefined(), JsValue::from),
        Cloned::Date(time) => {
            let date = JsDate::new(to);
            date.set_time(*time, to)?;
            made.push(date.clone().into());
            date.into()
        }
        Cloned::Buffer(bytes) => {
            let buffer = JsUint8Array::from_iter(bytes.iter().copied(), to)?.buffer(to)?;
            if let Some(object) = buffer.as_object() {
                made.push(object.clone());
            }
            buffer
        }
        Cloned::Bytes(bytes) => {
            let array = JsUint8Array::from_iter(bytes.iter().copied(), to)?;
            made.push(array.clone().into());
            array.into()
        }
        Cloned::Map(pairs) => {
            let map = JsMap::new(to);
            made.push(map.clone().into());
            for (key, item) in pairs {
                let key = make(key, to, made)?;
                let item = make(item, to, made)?;
                map.set(key, item, to)?;
            }
            map.into()
        }
        Cloned::Set(items) => {
            let set = JsSet::new(to);
            made.push(set.clone().into());
            for item in items {
                let item = make(item, to, made)?;
                set.add(item, to)?;
            }
            set.into()
        }
        Cloned::Array(items) => {
            let array = JsArray::new(to)?;
            made.push(array.clone().into());
            for item in items {
                let item = make(item, to, made)?;
                array.push(item, to)?;
            }
            array.into()
        }
        Cloned::Object(fields) | Cloned::Error(fields) => {
            let object = match cloned {
                Cloned::Error(_) => JsNativeError::error().into_opaque(to),
                _ => JsObject::with_object_proto(to.intrinsics()),
            };
            made.push(object.clone());
            for (name, item) in fields {
                let item = make(item, to, made)?;
                object.set(JsString::from(name.as_slice()), item, false, to)?;
            }
            object.into()
        }
    })
}

/// The items of an array or an iterator, as a list.
fn entries(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let list = crate::call(
        context,
        &from,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    let Some(list) = list.as_object() else {
        return Ok(Vec::new());
    };
    let list = JsArray::from_object(list)?;
    (0..list.length(context)?)
        .map(|at| list.get(at, context))
        .collect()
}

// ─── The worker's own realm ─────────────────────────────────────────────────────────────────

/// A runtime made a worker's: `self`, `postMessage`, `close`, the `message` listeners.
fn worker_runtime(name: &str, grant: Grant) -> JsResult<Runtime> {
    let mut runtime = Runtime::new();
    runtime.set_permissions(grant.permissions.clone());
    runtime.set_worker_permissions(grant.permissions);
    for pem in &grant.trusted {
        let _ = runtime.add_trusted_certificates(pem);
    }
    runtime.set_proxy_env(grant.proxy);
    let context = runtime.context();
    shared(context)
        .code_cache_protection
        .borrow_mut()
        .inherit(grant.code_cache_protection);
    *shared(context).port.borrow_mut() = Some(Port::default());
    let global = context.global_object();
    global.set(js_string!("self"), global.clone(), false, context)?;
    global.set(js_string!("name"), JsString::from(name), false, context)?;
    // There to be assigned, as a worker's global scope has them — `onmessage = …` in a strict
    // script needs the property to be there first.
    for handler in ["onmessage", "onerror", "onmessageerror"] {
        context.register_global_property(
            JsString::from(handler),
            JsValue::null(),
            Attribute::all(),
        )?;
    }
    let listeners = JsArray::new(context)?;
    global.define_property_or_throw(
        js_string!("__listeners"),
        PropertyDescriptor::builder()
            .value(listeners)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    let functions: [(&str, usize, NativeFunctionPointer); 5] = [
        ("postMessage", 1, |_, args, cx| {
            if let Some(port) = shared(cx).port.borrow_mut().as_mut() {
                port.outbox.push(arg(args, 0));
            }
            Ok(JsValue::undefined())
        }),
        ("close", 0, |_, _, cx| {
            if let Some(port) = shared(cx).port.borrow_mut().as_mut() {
                port.closed = true;
            }
            Ok(JsValue::undefined())
        }),
        ("addEventListener", 2, |_, args, cx| {
            add_listener(&cx.global_object(), args, cx)
        }),
        ("removeEventListener", 2, |_, args, cx| {
            remove_listener(&cx.global_object(), args, cx)
        }),
        ("importScripts", 0, |_, _, _| {
            Err(type_error(
                "importScripts: a worker here runs the one script it was given",
            ))
        }),
    ];
    for (name, length, f) in functions {
        let function = function(context, name, length, f);
        context.register_global_property(JsString::from(name), function, Attribute::all())?;
    }
    Ok(runtime)
}

fn add_listener(target: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = arg(args, 0).to_string(context)?.to_std_string_escaped();
    let listener = arg(args, 1);
    if kind != "message" || !listener.is_callable() {
        return Ok(JsValue::undefined());
    }
    if let Some(list) = target.get(js_string!("__listeners"), context)?.as_object() {
        JsArray::from_object(list)?.push(listener, context)?;
    }
    Ok(JsValue::undefined())
}

fn remove_listener(
    target: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let listener = arg(args, 1);
    let Some(list) = target.get(js_string!("__listeners"), context)?.as_object() else {
        return Ok(JsValue::undefined());
    };
    let kept: Vec<JsValue> = entries(&list.into(), context)?
        .into_iter()
        .filter(|l| *l != listener)
        .collect();
    let fresh = JsArray::from_iter(kept, context);
    target.set(js_string!("__listeners"), fresh, false, context)?;
    Ok(JsValue::undefined())
}

/// A `message` event carrying `data`, heard by `target`'s `onmessage` and its listeners.
fn dispatch_message(target: &JsObject, data: JsValue, context: &mut Context) {
    let event = JsObject::with_object_proto(context.intrinsics());
    let _ = event.set(js_string!("type"), js_string!("message"), false, context);
    let _ = event.set(js_string!("data"), data, false, context);
    let _ = event.set(js_string!("target"), target.clone(), false, context);
    let mut handlers = Vec::new();
    if let Ok(handler) = target.get(js_string!("onmessage"), context)
        && handler.is_callable()
    {
        handlers.push(handler);
    }
    if let Ok(list) = target.get(js_string!("__listeners"), context)
        && let Ok(list) = entries(&list, context)
    {
        handlers.extend(list);
    }
    for handler in handlers {
        if let Err(error) = crate::call(
            context,
            &handler,
            &target.clone().into(),
            &[event.clone().into()],
        ) {
            crate::report(context, &error);
        }
    }
}

// ─── The worker's thread ────────────────────────────────────────────────────────────────────

/// What a page says to its worker.
enum ToWorker {
    Message(Cloned),
    Terminate,
}

/// What a worker says to its page.
pub(crate) enum FromWorker {
    Message(Cloned),
    Console(Level, String),
    Closed,
}

/// What a page and its worker's thread both see.
pub(crate) struct Progress {
    /// Work the worker has not finished with yet: its own script, counted from the moment it is
    /// made, and every message sent to it.
    pending: AtomicUsize,
    /// When the worker's next timer is due, in milliseconds since the Unix epoch; 0 for none.
    next_timer: AtomicU64,
}

impl Progress {
    /// A worker's progress as it is made: its script still to run.
    fn starting() -> Self {
        Self {
            pending: AtomicUsize::new(1),
            next_timer: AtomicU64::new(0),
        }
    }
}

/// The worker's script, counted in [`Progress::pending`] until it has run and what it sent is on
/// its way — or the worker ends before then. A page waiting on a worker that has not been sent
/// anything yet still hears what its script posts.
struct Startup<'a>(&'a Progress);

impl Drop for Startup<'_> {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

fn epoch_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64() * 1000.)
}

/// How big a worker thread's stack is: the bundles plugins ship are parsed on it.
const WORKER_STACK: usize = 16 * 1024 * 1024;

/// What a worker's realm is granted, as its page's embedder set it for the page's workers.
struct Grant {
    permissions: crate::Permissions,
    code_cache_protection: crate::code_cache::Protection,
    trusted: Vec<Vec<u8>>,
    proxy: Option<crate::ProxyEnv>,
}

/// What a worker starts with: its name, its script and where it came from, and its grant.
struct Launch {
    name: String,
    script: String,
    url: String,
    module: bool,
    grant: Grant,
    /// The page's waker, told whenever the worker sends it something.
    wake: Arc<crate::WakeSlot>,
}

/// A worker's life, on its thread: the script run, then every message heard and every timer
/// fired as they come, until its page lets go of it, terminates it, or it closes itself.
fn worker_thread(
    launch: Launch,
    inbox: Receiver<ToWorker>,
    out: Sender<FromWorker>,
    progress: Arc<Progress>,
) {
    let Launch {
        name,
        script,
        url,
        module,
        grant,
        wake,
    } = launch;
    let out = Outbox { out, wake };
    let startup = Startup(&progress);
    let mut runtime = match worker_runtime(&name, grant) {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = out.send(FromWorker::Console(
                Level::Error,
                format!("Uncaught (in worker) {error}"),
            ));
            let _ = out.send(FromWorker::Closed);
            return;
        }
    };
    let evaluated = if module {
        runtime
            .load_esm(std::path::Path::new(&url), &script)
            .map(|promise| {
                let rejected =
                    crate::native::function(runtime.context(), "", 1, |_, args, context| {
                        crate::report(context, &JsError::from_opaque(arg(args, 0)));
                        Ok(JsValue::undefined())
                    });
                let _ = promise.then(
                    None,
                    boa_engine::object::builtins::JsFunction::from_object(rejected),
                    runtime.context(),
                );
                JsValue::undefined()
            })
    } else {
        runtime
            .context()
            .eval(boa_engine::Source::from_bytes(&script).with_path(std::path::Path::new(&url)))
            .map_err(|error| crate::describe(runtime.context(), &error))
    };
    runtime.run_microtasks();
    if let Err(error) = evaluated {
        let _ = out.send(FromWorker::Console(
            Level::Error,
            format!("Uncaught (in worker) {error}"),
        ));
    }
    if !flush(&mut runtime, &out, &progress) {
        return;
    }
    drop(startup);
    loop {
        // Asleep until a message comes or a timer is due — a worker with workers of its own
        // looks in on them now and then.
        let wait = runtime.next_due().map(|due| (due - runtime.now()).max(0.));
        let first = match wait {
            Some(ms) => match inbox.recv_timeout(Duration::from_secs_f64(ms / 1000.)) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match inbox.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            },
        };
        let mut messages: Vec<ToWorker> = first.into_iter().collect();
        while let Ok(message) = inbox.try_recv() {
            messages.push(message);
        }
        for message in messages {
            match message {
                ToWorker::Terminate => return,
                ToWorker::Message(cloned) => {
                    match clone_in(&cloned, runtime.context()) {
                        Ok(data) => {
                            let global = runtime.context().global_object();
                            dispatch_message(&global, data, runtime.context());
                        }
                        Err(error) => crate::report(runtime.context(), &error),
                    }
                    runtime.run_microtasks();
                    let open = flush(&mut runtime, &out, &progress);
                    // Done with only once what it answered is on its way: a page that sees no
                    // message pending never misses the answer to one.
                    progress.pending.fetch_sub(1, Ordering::SeqCst);
                    if !open {
                        return;
                    }
                }
            }
        }
        runtime.run_workers();
        runtime.run_due();
        runtime.run_microtasks();
        if !flush(&mut runtime, &out, &progress) {
            return;
        }
    }
}

/// The worker's way to its page: every send wakes the page's host.
struct Outbox {
    out: Sender<FromWorker>,
    wake: Arc<crate::WakeSlot>,
}

impl Outbox {
    fn send(&self, event: FromWorker) -> Result<(), mpsc::SendError<FromWorker>> {
        let sent = self.out.send(event);
        self.wake.wake();
        sent
    }
}

/// What the worker sent and wrote since it last looked, sent to its page, and when its next
/// timer is due. Answers whether it is still open.
fn flush(runtime: &mut Runtime, out: &Outbox, progress: &Progress) -> bool {
    for line in runtime.take_console() {
        let _ = out.send(FromWorker::Console(
            line.level,
            format!("[worker] {}", line.text),
        ));
    }
    let (outbox, closed) = shared(runtime.context())
        .port
        .borrow_mut()
        .as_mut()
        .map(|p| (std::mem::take(&mut p.outbox), p.closed))
        .unwrap_or_default();
    for message in outbox {
        match clone_out(&message, runtime.context()) {
            Ok(cloned) => {
                let _ = out.send(FromWorker::Message(cloned));
            }
            Err(error) => {
                let text = error.to_string();
                let _ = out.send(FromWorker::Console(
                    Level::Error,
                    format!("[worker] {text}"),
                ));
            }
        }
    }
    let next = runtime
        .next_due()
        .map_or(0, |due| (epoch_ms() + (due - runtime.now()).max(0.)) as u64);
    progress.next_timer.store(next, Ordering::SeqCst);
    if closed {
        let _ = out.send(FromWorker::Closed);
    }
    !closed
}

// ─── The page's side ────────────────────────────────────────────────────────────────────────

/// The script a worker was made from: what a `blob:` address stands for, or a `data:` one.
fn script_of(url: &str, context: &Context) -> Option<String> {
    if let Some(bytes) = shared(context).blobs.borrow().get(url) {
        return Some(String::from_utf8_lossy(bytes).into_owned());
    }
    if let Ok(url) = url::Url::parse(url)
        && url.scheme() == "file"
        && let Ok(path) = crate::esm_resolve::file_path(&url)
    {
        return std::fs::read_to_string(path).ok();
    }
    let rest = url.strip_prefix("data:")?;
    let (header, body) = rest.split_once(',')?;
    if header.ends_with(";base64") {
        let bytes = crate::encoding::unbase64(body)?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    } else {
        crate::esm_resolve::percent_decode(body).ok()
    }
}

fn construct_worker(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    crate::permission::worker(context)?;
    let url = arg(args, 0).to_string(context)?.to_std_string_escaped();
    if let Ok(parsed) = url::Url::parse(&url)
        && parsed.scheme() == "file"
        && let Ok(path) = crate::esm_resolve::file_path(&parsed)
    {
        crate::permission::read(context, &path)?;
    }
    let script = script_of(&url, context).ok_or_else(|| {
        type_error(format!(
            "Worker: '{url}' is not a script here — a blob: or data: address is"
        ))
    })?;
    let options = arg(args, 1);
    let kind = crate::native::get(&options, "type", context)?;
    let module = !kind.is_undefined() && crate::native::text_of(&kind, context)? == "module";
    if !kind.is_undefined() && !module && crate::native::text_of(&kind, context)? != "classic" {
        return Err(type_error("Worker type must be classic or module"));
    }
    let name = match options.as_object() {
        Some(o) => o.get(js_string!("name"), context)?,
        None => JsValue::undefined(),
    };
    let name = if name.is_undefined() {
        String::new()
    } else {
        name.to_string(context)?.to_std_string_escaped()
    };
    let (to, inbox) = mpsc::channel();
    let (out, from) = mpsc::channel();
    let progress = Arc::new(Progress::starting());
    let thread_progress = Arc::clone(&progress);
    let grant = {
        let state = shared(context);
        let network = state.network.borrow();
        // The network is what the embedder granted workers; the disk and workers are the
        // page's own, so a worker is no way around them.
        Grant {
            permissions: crate::Permissions {
                fs_read: network.permissions.fs_read.clone(),
                fs_write: network.permissions.fs_write.clone(),
                worker: network.permissions.worker,
                ..network.worker_permissions.clone()
            },
            code_cache_protection: state.code_cache_protection.borrow().clone(),
            trusted: network.trusted.clone(),
            proxy: network.proxy.as_ref().map(|(env, _)| env.clone()),
        }
    };
    let wake = shared(context).wake.clone();
    let thread_name = if name.is_empty() {
        "worker".to_owned()
    } else {
        format!("worker {name}")
    };
    std::thread::Builder::new()
        .name(thread_name)
        .stack_size(WORKER_STACK)
        .spawn(move || {
            crate::set_thread_stack(WORKER_STACK);
            let launch = Launch {
                name,
                script,
                url,
                module,
                grant,
                wake,
            };
            worker_thread(launch, inbox, out, thread_progress)
        })
        .map_err(|error| type_error(format!("Worker: no thread to run it on ({error})")))?;
    let object = JsObject::with_object_proto(context.intrinsics());
    if let Some(constructor) = this.as_object() {
        let prototype = constructor.get(js_string!("prototype"), context)?;
        object.set_prototype(prototype.as_object());
    }
    let listeners = JsArray::new(context)?;
    object.define_property_or_throw(
        js_string!("__listeners"),
        PropertyDescriptor::builder()
            .value(listeners)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    object.set(js_string!("onmessage"), JsValue::null(), false, context)?;
    object.set(js_string!("onerror"), JsValue::null(), false, context)?;
    let state = shared(context);
    let id = state.next_timer.get();
    state.next_timer.set(id + 1);
    object.define_property_or_throw(
        js_string!("__worker"),
        PropertyDescriptor::builder()
            .value(id)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    state.workers.borrow_mut().push(WorkerSlot {
        id,
        object: object.clone(),
        to,
        from,
        heard: VecDeque::new(),
        progress,
        closed: false,
    });
    Ok(object.into())
}

fn worker_id(this: &JsValue, context: &mut Context) -> JsResult<Option<u32>> {
    let Some(object) = this.as_object() else {
        return Ok(None);
    };
    let id = object.get(js_string!("__worker"), context)?;
    Ok(id.as_number().map(|n| n as u32))
}

const WORKER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    // The message copied now, and sent to the worker's thread, which hears it when it can.
    ("postMessage", 2, |this, args, cx| {
        let Some(id) = worker_id(this, cx)? else {
            return Ok(JsValue::undefined());
        };
        let cloned = clone_out(&arg(args, 0), cx)?;
        let state = shared(cx);
        let workers = state.workers.borrow();
        if let Some(slot) = workers.iter().find(|w| w.id == id && !w.closed) {
            slot.progress.pending.fetch_add(1, Ordering::SeqCst);
            if slot.to.send(ToWorker::Message(cloned)).is_err() {
                slot.progress.pending.fetch_sub(1, Ordering::SeqCst);
            }
        }
        Ok(JsValue::undefined())
    }),
    ("terminate", 0, |this, _, cx| {
        if let Some(id) = worker_id(this, cx)? {
            let state = shared(cx);
            let mut workers = state.workers.borrow_mut();
            if let Some(slot) = workers.iter().find(|w| w.id == id) {
                let _ = slot.to.send(ToWorker::Terminate);
            }
            workers.retain(|w| w.id != id);
        }
        Ok(JsValue::undefined())
    }),
    ("addEventListener", 2, |this, args, cx| {
        match this.as_object() {
            Some(object) => add_listener(&object, args, cx),
            None => Ok(JsValue::undefined()),
        }
    }),
    ("removeEventListener", 2, |this, args, cx| {
        match this.as_object() {
            Some(object) => remove_listener(&object, args, cx),
            None => Ok(JsValue::undefined()),
        }
    }),
];

/// What every worker sent since the page last looked, taken off its channel and kept.
fn gather(page: &Context) {
    let state = shared(page);
    let mut workers = state.workers.borrow_mut();
    for slot in workers.iter_mut() {
        while let Ok(event) = slot.from.try_recv() {
            slot.heard.push_back(event);
        }
    }
}

/// What the workers sent, heard by their page: messages by their `Worker` objects, console
/// lines on the page's console. Never waits for a worker — a worker still busy is answered on a
/// later beat — and stops at `deadline`, at least one message heard: what four workers sent in
/// one beat is heard over the next few, rather than all at once in one that holds the window.
/// Answers whether anything was heard.
pub(crate) fn run_workers(page: &mut Context, deadline: Option<std::time::Instant>) -> bool {
    gather(page);
    let mut busy = false;
    loop {
        // One event at a time, the workers in turn, so none waits behind another's backlog.
        let next = {
            let state = shared(page);
            let mut workers = state.workers.borrow_mut();
            let mut found = None;
            for slot in workers.iter_mut() {
                if let Some(event) = slot.heard.pop_front() {
                    found = Some((slot.id, slot.object.clone(), event));
                    break;
                }
            }
            if let Some((id, ..)) = found {
                // The one heard from goes to the back of the line.
                if let Some(at) = workers.iter().position(|w| w.id == id) {
                    let slot = workers.remove(at);
                    workers.push(slot);
                }
            }
            found
        };
        let Some((id, object, event)) = next else {
            break;
        };
        busy = true;
        {
            match event {
                FromWorker::Message(cloned) => match clone_in(&cloned, page) {
                    Ok(data) => dispatch_message(&object, data, page),
                    Err(error) => crate::report(page, &error),
                },
                FromWorker::Console(level, text) => crate::console_line(page, level, text),
                FromWorker::Closed => {
                    shared(page).workers.borrow_mut().retain(|w| w.id != id);
                }
            }
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            break;
        }
    }
    busy
}

/// How long until a worker has something for its page, in milliseconds from now: now when it
/// has sent something or is working on a message, its next timer otherwise.
///
/// With a waker set (`woken`), a worker busy with a message or waiting for its own timer wakes
/// its page when it has something, so only what was already heard is due now.
pub(crate) fn next_due(page: &Context, woken: bool) -> Option<crate::Millis> {
    gather(page);
    let state = shared(page);
    let workers = state.workers.borrow();
    let now = epoch_ms();
    let mut next: Option<crate::Millis> = None;
    for worker in workers.iter().filter(|w| !w.closed) {
        let due = if !worker.heard.is_empty() {
            Some(0.)
        } else if woken {
            None
        } else if worker.progress.pending.load(Ordering::SeqCst) > 0 {
            Some(0.)
        } else {
            match worker.progress.next_timer.load(Ordering::SeqCst) {
                0 => None,
                at => Some((at as f64 - now).max(0.)),
            }
        };
        next = match (next, due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
    next
}

/// Whether a worker this realm started is still running: what keeps a process
/// (`verve run`) from exiting.
pub(crate) fn alive(page: &Context) -> bool {
    shared(page).workers.borrow().iter().any(|w| !w.closed)
}

/// Waits, up to `timeout`, until a worker that is working on a message has something for its
/// page. Answers whether one has. For a host on a clock of its own — a test's — that has to
/// let the workers' threads catch up.
pub(crate) fn wait(page: &Context, timeout: Duration) -> bool {
    let started = std::time::Instant::now();
    loop {
        gather(page);
        let (heard, working) = {
            let state = shared(page);
            let workers = state.workers.borrow();
            (
                workers.iter().any(|w| !w.heard.is_empty()),
                workers
                    .iter()
                    .any(|w| w.progress.pending.load(Ordering::SeqCst) > 0),
            )
        };
        if heard {
            return true;
        }
        if !working || started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let class: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(construct_worker),
    )
    .name(js_string!("Worker"))
    .length(2)
    .constructor(true)
    .build()
    .into();
    let prototype = JsObject::with_object_proto(context.intrinsics());
    for (name, length, f) in WORKER_METHODS {
        let method = function(context, name, *length, *f);
        prototype.set(JsString::from(*name), method, false, context)?;
    }
    prototype.set(js_string!("constructor"), class.clone(), false, context)?;
    class.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    context.register_global_property(
        js_string!("Worker"),
        class,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok(())
}
