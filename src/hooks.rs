//! Node's modules for watching a program run — `async_hooks`, `diagnostics_channel`,
//! `perf_hooks`, `inspector`, `trace_events` — as far as libraries bundled into plugins use them:
//! SDKs that keep a request's context in an `AsyncLocalStorage`, or publish to a diagnostics
//! channel nobody here subscribes to.
//!
//! `AsyncLocalStorage` keeps its store for the synchronous part of `run`, and carries it into the
//! callbacks queued while it is set: microtasks (`queueMicrotask`, `process.nextTick`) and
//! timers. A promise reaction runs with the stores set where it was *registered* — the `then`,
//! the `await` — as Node's does, so an `await` on a promise that settles after `run` returned
//! still resumes inside `run`'s store ([`Hooks`]). `AsyncResource` captures the stores where it
//! is made and restores them in `runInAsyncScope`. `createHook` answers a hook that is enabled
//! and disabled but never called.
//!
//! The same hooks note promises rejected with no handler, and tell `process` of them —
//! `unhandledRejection`, `rejectionHandled` — once the microtasks are done.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use boa_engine::{
    Context, JsData, JsError, JsObject, JsResult, JsString, JsValue,
    builtins::promise::{OperationType, Promise, PromiseState},
    context::HostHooks,
    job::{JobCallback, PromiseJob},
    js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsFunction, JsPromise},
};
use boa_gc::{Finalize, Trace};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error, exports, function, get, hidden, js_text,
    list_of, object,
};

// ─── The current stores ─────────────────────────────────────────────────────────────────────

/// Which `AsyncLocalStorage` holds which store, now.
#[derive(Default)]
struct Stores {
    active: RefCell<Vec<(JsObject, JsValue)>>,
    next_id: std::cell::Cell<u32>,
}

fn stores(context: &mut Context) -> Rc<Stores> {
    if let Some(stores) = context.get_data::<Rc<Stores>>() {
        return stores.clone();
    }
    let stores = Rc::new(Stores::default());
    context.insert_data(stores.clone());
    stores
}

fn current(context: &Context) -> Vec<(JsObject, JsValue)> {
    context
        .get_data::<Rc<Stores>>()
        .map(|s| s.active.borrow().clone())
        .unwrap_or_default()
}

/// Runs `f` with `snapshot` as the current stores, and puts back what was there before.
fn within<R>(
    context: &mut Context,
    snapshot: Vec<(JsObject, JsValue)>,
    f: impl FnOnce(&mut Context) -> R,
) -> R {
    let state = stores(context);
    let before = state.active.replace(snapshot);
    let out = f(context);
    *state.active.borrow_mut() = before;
    out
}

/// A promise job queued while a store is set, made to run with that store set again — called
/// by the runtime's job queue for every job it is given.
pub(crate) fn carry(job: PromiseJob, context: &Context) -> PromiseJob {
    let snapshot = current(context);
    if snapshot.is_empty() {
        return job;
    }
    PromiseJob::new(move |context| within(context, snapshot, |context| job.call(context)))
}

/// A timer's callback, made to run with the stores set when it was started.
pub(crate) fn carry_callback(callback: JsValue, context: &mut Context) -> JsValue {
    let snapshot = current(context);
    if snapshot.is_empty() {
        return callback;
    }
    let pairs = snapshot_value(snapshot, context);
    closure(
        context,
        "",
        0,
        |this, args, (callback, pairs): &(JsValue, JsValue), cx| {
            let snapshot = snapshot_of(pairs, cx)?;
            within(cx, snapshot, |cx| crate::call(cx, callback, this, args))
        },
        (callback, pairs),
    )
    .into()
}

// ─── The host's hooks ───────────────────────────────────────────────────────────────────────

/// The runtime's answers to the engine's host hooks: a promise reaction runs with the stores
/// set where it was registered, and a promise rejected with no handler is noted for
/// [`report_rejections`].
pub(crate) struct Hooks;

/// The stores a reaction was registered under, kept on its job callback. Untraced: what it holds
/// is held from Rust, and so is a root.
#[derive(Trace, Finalize, JsData)]
struct Registered(#[unsafe_ignore_trace] Vec<(JsObject, JsValue)>);

impl HostHooks for Hooks {
    fn make_job_callback(&self, callback: JsFunction, context: &mut Context) -> JobCallback {
        JobCallback::new(callback, Registered(current(context)))
    }

    fn call_job_callback(
        &self,
        job: &JobCallback,
        this: &JsValue,
        args: &[JsValue],
        context: &mut Context,
    ) -> JsResult<JsValue> {
        match job.host_defined().downcast_ref::<Registered>() {
            Some(Registered(snapshot)) => within(context, snapshot.clone(), |context| {
                job.callback().call(this, args, context)
            }),
            None => job.callback().call(this, args, context),
        }
    }

    fn promise_rejection_tracker(
        &self,
        promise: &JsObject<Promise>,
        operation: OperationType,
        context: &mut Context,
    ) {
        let promise = promise.clone().upcast();
        let state = rejections(context);
        match operation {
            OperationType::Reject => state.pending.borrow_mut().push(promise),
            OperationType::Handle => {
                let mut pending = state.pending.borrow_mut();
                if let Some(at) = pending.iter().position(|p| JsObject::equals(p, &promise)) {
                    // Handled within the same turn: it was never unhandled.
                    pending.remove(at);
                    return;
                }
                drop(pending);
                let mut reported = state.reported.borrow_mut();
                if let Some(at) = reported.iter().position(|p| JsObject::equals(p, &promise)) {
                    reported.remove(at);
                    state.handled_late.borrow_mut().push(promise);
                }
            }
        }
    }
}

/// Promises rejected with no handler, from the rejection to the end of the turn, and after.
#[derive(Default)]
struct Rejections {
    /// Rejected with no handler, not yet reported.
    pending: RefCell<Vec<JsObject>>,
    /// Reported as unhandled, so a handler added later is `rejectionHandled`. Only the latest
    /// [`REMEMBERED_REJECTIONS`] are kept: each holds its reason alive, and a program that
    /// leaves hundreds unhandled is not going to handle the oldest.
    reported: RefCell<VecDeque<JsObject>>,
    /// Reported, and handled after all.
    handled_late: RefCell<Vec<JsObject>>,
}

const REMEMBERED_REJECTIONS: usize = 256;

fn rejections(context: &mut Context) -> Rc<Rejections> {
    if let Some(state) = context.get_data::<Rc<Rejections>>() {
        return state.clone();
    }
    let state = Rc::new(Rejections::default());
    context.insert_data(state.clone());
    state
}

/// Tells `process` of the rejections this turn left unhandled — `unhandledRejection`, with the
/// reason and the promise — and of reported ones that have a handler now (`rejectionHandled`).
/// With nobody listening for `unhandledRejection`, the reason goes to the console, as an uncaught
/// error does. Called when the microtask queue is empty; `true` when it said anything, since a
/// listener may have queued microtasks of its own.
pub(crate) fn report_rejections(context: &mut Context) -> bool {
    let Some(state) = context.get_data::<Rc<Rejections>>().cloned() else {
        return false;
    };
    let pending = std::mem::take(&mut *state.pending.borrow_mut());
    let handled_late = std::mem::take(&mut *state.handled_late.borrow_mut());
    if pending.is_empty() && handled_late.is_empty() {
        return false;
    }
    let process = crate::native::global(context, "process").unwrap_or_default();
    for promise in pending {
        let Ok(PromiseState::Rejected(reason)) =
            JsPromise::from_object(promise.clone()).map(|p| p.state())
        else {
            continue;
        };
        let heard = emit(
            &process,
            &[
                js_text("unhandledRejection"),
                reason.clone(),
                promise.clone().into(),
            ],
            context,
        );
        if !heard && crate::process::profile(context).is_some() {
            crate::process::unhandled_rejection(context, reason);
        } else if !heard {
            let error = JsError::from_opaque(reason);
            let text = crate::describe(context, &error);
            let frames = crate::thrown_at(context, &error);
            let line = if frames.is_empty() {
                format!("Uncaught (in promise) {text}")
            } else {
                format!("Uncaught (in promise) {text} ({frames})")
            };
            crate::console_line(context, crate::Level::Error, line);
        }
        let mut reported = state.reported.borrow_mut();
        if reported.len() == REMEMBERED_REJECTIONS {
            reported.pop_front();
        }
        reported.push_back(promise);
    }
    for promise in handled_late {
        emit(
            &process,
            &[js_text("rejectionHandled"), promise.into()],
            context,
        );
    }
    true
}

/// `process.emit(...)`, and whether anyone listened. A listener that throws is an uncaught error
/// like any other, and counts as having heard.
fn emit(process: &JsValue, args: &[JsValue], context: &mut Context) -> bool {
    if !process.is_object() {
        return false;
    }
    match crate::native::invoke(process, "emit", args, context) {
        Ok(heard) => heard.to_boolean(),
        Err(error) => {
            crate::report(context, &error);
            true
        }
    }
}

/// Stores as a value a function can hold: `[[storage, store], …]`.
fn snapshot_value(snapshot: Vec<(JsObject, JsValue)>, context: &mut Context) -> JsValue {
    let pairs: Vec<JsValue> = snapshot
        .into_iter()
        .map(|(storage, store)| array(context, vec![storage.into(), store]))
        .collect();
    array(context, pairs)
}

fn snapshot_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<(JsObject, JsValue)>> {
    let mut out = Vec::new();
    for pair in list_of(value, context)? {
        let pair = list_of(&pair, context)?;
        if let (Some(storage), Some(store)) =
            (pair.first().and_then(JsValue::as_object), pair.get(1))
        {
            out.push((storage, store.clone()));
        }
    }
    Ok(out)
}

/// The current stores with `storage` holding `store` (or none, `None`).
fn with_store(
    context: &Context,
    storage: &JsObject,
    store: Option<JsValue>,
) -> Vec<(JsObject, JsValue)> {
    let mut snapshot: Vec<_> = current(context)
        .into_iter()
        .filter(|(s, _)| !JsObject::equals(s, storage))
        .collect();
    if let Some(store) = store {
        snapshot.push((storage.clone(), store));
    }
    snapshot
}

fn call_rest(context: &mut Context, args: &[JsValue], from: usize) -> JsResult<JsValue> {
    let f = arg(args, from);
    let rest: Vec<JsValue> = args.iter().skip(from + 1).cloned().collect();
    crate::call(context, &f, &JsValue::undefined(), &rest)
}

// ─── AsyncLocalStorage ──────────────────────────────────────────────────────────────────────

const STORAGE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("run", 2, |this, args, cx| {
        let storage = this
            .as_object()
            .ok_or_else(|| crate::native::type_error("not an AsyncLocalStorage"))?;
        let snapshot = with_store(cx, &storage, Some(arg(args, 0)));
        within(cx, snapshot, |cx| call_rest(cx, args, 1))
    }),
    ("exit", 1, |this, args, cx| {
        let storage = this
            .as_object()
            .ok_or_else(|| crate::native::type_error("not an AsyncLocalStorage"))?;
        let snapshot = with_store(cx, &storage, None);
        within(cx, snapshot, |cx| call_rest(cx, args, 0))
    }),
    ("getStore", 0, |this, _, cx| {
        let Some(storage) = this.as_object() else {
            return Ok(JsValue::undefined());
        };
        Ok(current(cx)
            .into_iter()
            .find(|(s, _)| JsObject::equals(s, &storage))
            .map(|(_, store)| store)
            .unwrap_or_default())
    }),
    ("enterWith", 1, |this, args, cx| {
        let storage = this
            .as_object()
            .ok_or_else(|| crate::native::type_error("not an AsyncLocalStorage"))?;
        let snapshot = with_store(cx, &storage, Some(arg(args, 0)));
        *stores(cx).active.borrow_mut() = snapshot;
        Ok(JsValue::undefined())
    }),
    ("disable", 0, |this, _, cx| {
        if let Some(storage) = this.as_object() {
            let snapshot = with_store(cx, &storage, None);
            *stores(cx).active.borrow_mut() = snapshot;
        }
        Ok(JsValue::undefined())
    }),
];

/// `AsyncLocalStorage.snapshot()`: a function that runs what it is given with the stores of now.
fn snapshot(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let pairs = snapshot_value(current(context), context);
    Ok(closure(
        context,
        "runInAsyncScope",
        1,
        |_, args, pairs: &JsValue, cx| {
            let snapshot = snapshot_of(pairs, cx)?;
            within(cx, snapshot, |cx| call_rest(cx, args, 0))
        },
        pairs,
    )
    .into())
}

/// `bind(fn)`: `fn`, made to run with the stores of now.
fn bound(context: &mut Context, f: JsValue, this: JsValue) -> JsValue {
    let pairs = snapshot_value(current(context), context);
    closure(
        context,
        "bound",
        0,
        |call_this, args, (f, this, pairs): &(JsValue, JsValue, JsValue), cx| {
            let snapshot = snapshot_of(pairs, cx)?;
            let this = if this.is_undefined() {
                call_this.clone()
            } else {
                this.clone()
            };
            within(cx, snapshot, |cx| crate::call(cx, f, &this, args))
        },
        (f, this, pairs),
    )
    .into()
}

// ─── AsyncResource ──────────────────────────────────────────────────────────────────────────

fn resource_snapshot(this: &JsValue, context: &mut Context) -> JsResult<Vec<(JsObject, JsValue)>> {
    let pairs = get(this, "__stores", context)?;
    snapshot_of(&pairs, context)
}

fn next_async_id(context: &mut Context) -> u32 {
    let state = stores(context);
    let id = state.next_id.get() + 1;
    state.next_id.set(id);
    // 1 is the top level's own.
    id + 1
}

const RESOURCE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("runInAsyncScope", 2, |this, args, cx| {
        let snapshot = resource_snapshot(this, cx)?;
        let f = arg(args, 0);
        let call_this = arg(args, 1);
        let rest: Vec<JsValue> = args.iter().skip(2).cloned().collect();
        within(cx, snapshot, |cx| crate::call(cx, &f, &call_this, &rest))
    }),
    ("bind", 2, |this, args, cx| {
        let snapshot = resource_snapshot(this, cx)?;
        let f = arg(args, 0);
        within(cx, snapshot, |cx| Ok(bound(cx, f, arg(args, 1))))
    }),
    ("emitDestroy", 0, |this, _, _| Ok(this.clone())),
    ("asyncId", 0, |this, _, cx| get(this, "__asyncId", cx)),
    ("triggerAsyncId", 0, |this, _, cx| {
        get(this, "__triggerId", cx)
    }),
];

const ASYNC_HOOKS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("executionAsyncId", 0, |_, _, _| Ok(1.into())),
    ("triggerAsyncId", 0, |_, _, _| Ok(0.into())),
    (
        "executionAsyncResource",
        0,
        |_, _, cx| Ok(object(cx).into()),
    ),
    ("createHook", 1, |_, _, cx| {
        let hook = object(cx);
        exports(
            cx,
            &hook,
            &[
                ("enable", 0, |this, _, _| Ok(this.clone())),
                ("disable", 0, |this, _, _| Ok(this.clone())),
            ],
        )?;
        Ok(hook.into())
    }),
];

pub(crate) fn async_hooks_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, ASYNC_HOOKS)?;
    let (storage, _) = class(
        context,
        ClassSpec {
            name: "AsyncLocalStorage",
            init: |this, args, cx| {
                let name = get(&arg(args, 0), "name", cx)?;
                this.set(
                    js_string!("name"),
                    if name.is_undefined() {
                        js_text("")
                    } else {
                        name
                    },
                    false,
                    cx,
                )?;
                Ok(())
            },
            methods: STORAGE_METHODS,
            statics: &[
                ("snapshot", 0, snapshot),
                ("bind", 1, |_, args, cx| {
                    Ok(bound(cx, arg(args, 0), JsValue::undefined()))
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("AsyncLocalStorage"), storage, false, context)?;
    let (resource, _) = class(
        context,
        ClassSpec {
            name: "AsyncResource",
            length: 1,
            init: |this, args, cx| {
                let kind = arg(args, 0);
                if !kind.is_string() {
                    return Err(coded_error(
                        cx,
                        "The \"type\" argument must be of type string",
                        "ERR_INVALID_ARG_TYPE",
                    ));
                }
                hidden(this, js_string!("__type"), kind, cx)?;
                let pairs = snapshot_value(current(cx), cx);
                hidden(this, js_string!("__stores"), pairs, cx)?;
                let id = next_async_id(cx);
                hidden(this, js_string!("__asyncId"), id, cx)?;
                let trigger = get(&arg(args, 1), "triggerAsyncId", cx)?;
                hidden(
                    this,
                    js_string!("__triggerId"),
                    if trigger.is_number() {
                        trigger
                    } else {
                        1.into()
                    },
                    cx,
                )?;
                Ok(())
            },
            methods: RESOURCE_METHODS,
            statics: &[("bind", 3, |_, args, cx| {
                Ok(bound(cx, arg(args, 0), arg(args, 2)))
            })],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("AsyncResource"), resource, false, context)?;
    let providers = object(context);
    module.set(js_string!("asyncWrapProviders"), providers, false, context)?;
    Ok(module.into())
}

// ─── diagnostics_channel ────────────────────────────────────────────────────────────────────

/// The realm's channels, by name, so `channel(name)` answers the same one each time.
struct Channels(JsObject);

fn channel(context: &mut Context, name: &JsValue) -> JsResult<JsValue> {
    let registry = match context.get_data::<Channels>() {
        Some(channels) => channels.0.clone(),
        None => {
            let registry = JsObject::with_null_proto();
            context.insert_data(Channels(registry.clone()));
            registry
        }
    };
    let key = name.to_property_key(context)?;
    let existing = registry.get(key.clone(), context)?;
    if existing.is_object() {
        return Ok(existing);
    }
    let module = crate::modules::node_module(context, "diagnostics_channel")?;
    let class = get(&module, "Channel", context)?;
    let class = class
        .as_object()
        .ok_or_else(|| crate::native::type_error("Channel is not a class"))?;
    let made: JsValue = class
        .construct(std::slice::from_ref(name), None, context)?
        .into();
    registry.set(key, made.clone(), false, context)?;
    Ok(made)
}

fn subscribers(this: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    list_of(&get(this, "__subscribers", context)?, context)
}

fn set_subscribers(this: &JsValue, list: Vec<JsValue>, context: &mut Context) -> JsResult<()> {
    if let Some(channel) = this.as_object() {
        let list = array(context, list);
        hidden(&channel, js_string!("__subscribers"), list, context)?;
    }
    Ok(())
}

const CHANNEL_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("subscribe", 1, |this, args, cx| {
        let mut list = subscribers(this, cx)?;
        list.push(arg(args, 0));
        set_subscribers(this, list, cx)?;
        Ok(JsValue::undefined())
    }),
    ("unsubscribe", 1, |this, args, cx| {
        let mut list = subscribers(this, cx)?;
        let target = arg(args, 0);
        let Some(at) = list.iter().position(|s| *s == target) else {
            return Ok(false.into());
        };
        list.remove(at);
        set_subscribers(this, list, cx)?;
        Ok(true.into())
    }),
    ("publish", 1, |this, args, cx| {
        let name = get(this, "name", cx)?;
        for subscriber in subscribers(this, cx)? {
            // A subscriber that throws does not stop the others; Node reports it the same way.
            if let Err(error) = crate::call(
                cx,
                &subscriber,
                &JsValue::undefined(),
                &[arg(args, 0), name.clone()],
            ) {
                crate::report(cx, &error);
            }
        }
        Ok(JsValue::undefined())
    }),
    ("bindStore", 2, |_, _, _| Ok(JsValue::undefined())),
    ("unbindStore", 1, |_, _, _| Ok(false.into())),
    ("runStores", 3, |_, args, cx| {
        let f = arg(args, 1);
        let rest: Vec<JsValue> = args.iter().skip(3).cloned().collect();
        crate::call(cx, &f, &arg(args, 2), &rest)
    }),
];

const CHANNEL_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[(
    "hasSubscribers",
    |this, _, cx| Ok((!subscribers(this, cx)?.is_empty()).into()),
    None,
)];

const TRACING_EVENTS: [&str; 5] = ["start", "end", "asyncStart", "asyncEnd", "error"];

/// Publishes `message` on the tracing channel's `event` channel.
fn trace(this: &JsValue, event: &str, message: &JsValue, context: &mut Context) -> JsResult<()> {
    let channel = get(this, event, context)?;
    crate::native::invoke(&channel, "publish", std::slice::from_ref(message), context)?;
    Ok(())
}

const TRACING_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("subscribe", 1, |this, args, cx| {
        let handlers = arg(args, 0);
        for event in TRACING_EVENTS {
            let handler = get(&handlers, event, cx)?;
            if handler.is_callable() {
                let channel = get(this, event, cx)?;
                crate::native::invoke(&channel, "subscribe", &[handler], cx)?;
            }
        }
        Ok(JsValue::undefined())
    }),
    ("unsubscribe", 1, |this, args, cx| {
        let handlers = arg(args, 0);
        let mut all = true;
        for event in TRACING_EVENTS {
            let handler = get(&handlers, event, cx)?;
            if handler.is_callable() {
                let channel = get(this, event, cx)?;
                all &= crate::native::invoke(&channel, "unsubscribe", &[handler], cx)?.to_boolean();
            }
        }
        Ok(all.into())
    }),
    ("traceSync", 3, |this, args, cx| {
        let message = arg(args, 1);
        trace(this, "start", &message, cx)?;
        let rest: Vec<JsValue> = args.iter().skip(3).cloned().collect();
        let outcome = crate::call(cx, &arg(args, 0), &arg(args, 2), &rest);
        match outcome {
            Ok(value) => {
                if let Some(m) = message.as_object() {
                    m.set(js_string!("result"), value.clone(), false, cx)?;
                }
                trace(this, "end", &message, cx)?;
                Ok(value)
            }
            Err(error) => {
                if let Some(m) = message.as_object() {
                    let e = error.clone().into_opaque(cx)?;
                    m.set(js_string!("error"), e, false, cx)?;
                }
                trace(this, "error", &message, cx)?;
                trace(this, "end", &message, cx)?;
                Err(error)
            }
        }
    }),
    ("tracePromise", 3, |this, args, cx| {
        let message = arg(args, 1);
        trace(this, "start", &message, cx)?;
        let rest: Vec<JsValue> = args.iter().skip(3).cloned().collect();
        let outcome = crate::call(cx, &arg(args, 0), &arg(args, 2), &rest);
        trace(this, "end", &message, cx)?;
        outcome
    }),
    ("traceCallback", 4, |this, args, cx| {
        let message = arg(args, 2);
        trace(this, "start", &message, cx)?;
        let rest: Vec<JsValue> = args.iter().skip(4).cloned().collect();
        let outcome = crate::call(cx, &arg(args, 0), &arg(args, 3), &rest);
        trace(this, "end", &message, cx)?;
        outcome
    }),
];

const TRACING_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[(
    "hasSubscribers",
    |this, _, cx| {
        for event in TRACING_EVENTS {
            let channel = get(this, event, cx)?;
            if get(&channel, "hasSubscribers", cx)?.to_boolean() {
                return Ok(true.into());
            }
        }
        Ok(false.into())
    },
    None,
)];

const DIAGNOSTICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("channel", 1, |_, args, cx| channel(cx, &arg(args, 0))),
    ("hasSubscribers", 1, |_, args, cx| {
        let channel = channel(cx, &arg(args, 0))?;
        get(&channel, "hasSubscribers", cx)
    }),
    ("subscribe", 2, |_, args, cx| {
        let channel = channel(cx, &arg(args, 0))?;
        crate::native::invoke(&channel, "subscribe", &[arg(args, 1)], cx)
    }),
    ("unsubscribe", 2, |_, args, cx| {
        let channel = channel(cx, &arg(args, 0))?;
        crate::native::invoke(&channel, "unsubscribe", &[arg(args, 1)], cx)
    }),
    ("tracingChannel", 1, |_, args, cx| {
        let given = arg(args, 0);
        let module = crate::modules::node_module(cx, "diagnostics_channel")?;
        let prototype = get(&get(&module, "TracingChannel", cx)?, "prototype", cx)?;
        let tracing = object(cx);
        tracing.set_prototype(prototype.as_object());
        for event in TRACING_EVENTS {
            let channel = if given.is_string() {
                let name = format!("tracing:{}:{event}", crate::print(&given));
                channel(cx, &js_text(&name))?
            } else {
                get(&given, event, cx)?
            };
            tracing.set(JsString::from(event), channel, false, cx)?;
        }
        Ok(tracing.into())
    }),
];

pub(crate) fn diagnostics_channel_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, DIAGNOSTICS)?;
    let (channel_class, _) = class(
        context,
        ClassSpec {
            name: "Channel",
            length: 1,
            init: |this, args, cx| {
                this.set(js_string!("name"), arg(args, 0), false, cx)?;
                let list = array(cx, Vec::new());
                hidden(this, js_string!("__subscribers"), list, cx)?;
                Ok(())
            },
            methods: CHANNEL_METHODS,
            accessors: CHANNEL_ACCESSORS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Channel"), channel_class, false, context)?;
    let (tracing, _) = class(
        context,
        ClassSpec {
            name: "TracingChannel",
            methods: TRACING_METHODS,
            accessors: TRACING_ACCESSORS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("TracingChannel"), tracing, false, context)?;
    Ok(module.into())
}

// ─── perf_hooks ─────────────────────────────────────────────────────────────────────────────

/// A histogram that records nothing it is not handed: `monitorEventLoopDelay` and
/// `createHistogram`'s, with the numbers of an empty one.
fn histogram(context: &mut Context) -> JsResult<JsValue> {
    let h = object(context);
    for (key, value) in [
        ("min", 9_223_372_036_854_776_000.),
        ("max", 0.),
        ("mean", f64::NAN),
        ("stddev", f64::NAN),
        ("exceeds", 0.),
        ("count", 0.),
    ] {
        h.set(JsString::from(key), value, false, context)?;
    }
    let percentiles = crate::native::construct_global(context, "Map", &[])?;
    h.set(js_string!("percentiles"), percentiles, false, context)?;
    exports(
        context,
        &h,
        &[
            ("enable", 0, |_, _, _| Ok(true.into())),
            ("disable", 0, |_, _, _| Ok(true.into())),
            ("reset", 0, |_, _, _| Ok(JsValue::undefined())),
            ("percentile", 1, |_, _, _| Ok(0.into())),
            ("record", 1, |this, args, cx| {
                let value = arg(args, 0).to_number(cx)?;
                if let Some(h) = this.as_object() {
                    let count = h.get(js_string!("count"), cx)?.to_number(cx)?;
                    let min = h.get(js_string!("min"), cx)?.to_number(cx)?;
                    let max = h.get(js_string!("max"), cx)?.to_number(cx)?;
                    let mean = h.get(js_string!("mean"), cx)?.to_number(cx)?;
                    let mean = if count == 0. {
                        value
                    } else {
                        (mean * count + value) / (count + 1.)
                    };
                    h.set(js_string!("count"), count + 1., false, cx)?;
                    h.set(js_string!("min"), min.min(value), false, cx)?;
                    h.set(js_string!("max"), max.max(value), false, cx)?;
                    h.set(js_string!("mean"), mean, false, cx)?;
                }
                Ok(JsValue::undefined())
            }),
            ("recordDelta", 0, |_, _, _| Ok(JsValue::undefined())),
        ],
    )?;
    Ok(h.into())
}

pub(crate) fn perf_hooks_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let performance = crate::native::global(context, "performance")?;
    module.set(js_string!("performance"), performance, false, context)?;
    let (observer, _) = class(
        context,
        ClassSpec {
            name: "PerformanceObserver",
            length: 1,
            init: |this, args, cx| {
                hidden(this, js_string!("__callback"), arg(args, 0), cx)?;
                Ok(())
            },
            methods: &[
                ("observe", 1, |_, _, _| Ok(JsValue::undefined())),
                ("disconnect", 0, |_, _, _| Ok(JsValue::undefined())),
                ("takeRecords", 0, |_, _, cx| Ok(array(cx, Vec::new()))),
            ],
            ..ClassSpec::default()
        },
    )?;
    let types = array(
        context,
        ["mark", "measure", "function", "gc", "http", "resource"]
            .iter()
            .map(|t| js_text(t))
            .collect(),
    );
    observer.set(js_string!("supportedEntryTypes"), types, false, context)?;
    module.set(js_string!("PerformanceObserver"), observer, false, context)?;
    for name in [
        "PerformanceEntry",
        "PerformanceMark",
        "PerformanceMeasure",
        "PerformanceObserverEntryList",
        "PerformanceResourceTiming",
    ] {
        let (class, _) = class(
            context,
            ClassSpec {
                name,
                init: |this, args, cx| {
                    this.set(js_string!("name"), arg(args, 0), false, cx)?;
                    this.set(js_string!("entryType"), js_text("mark"), false, cx)?;
                    this.set(js_string!("startTime"), crate::now(cx), false, cx)?;
                    this.set(js_string!("duration"), 0, false, cx)?;
                    Ok(())
                },
                ..ClassSpec::default()
            },
        )?;
        module.set(JsString::from(name), class, false, context)?;
    }
    exports(
        context,
        &module,
        &[
            ("monitorEventLoopDelay", 1, |_, _, cx| histogram(cx)),
            ("createHistogram", 1, |_, _, cx| histogram(cx)),
        ],
    )?;
    let constants = crate::os::constants_object(
        context,
        &[
            ("NODE_PERFORMANCE_GC_MAJOR", 4),
            ("NODE_PERFORMANCE_GC_MINOR", 1),
            ("NODE_PERFORMANCE_GC_INCREMENTAL", 8),
            ("NODE_PERFORMANCE_GC_WEAKCB", 16),
            ("NODE_PERFORMANCE_GC_FLAGS_FORCED", 4),
        ],
    )?;
    module.set(js_string!("constants"), constants, false, context)?;
    Ok(module.into())
}

// ─── inspector, trace_events ────────────────────────────────────────────────────────────────

pub(crate) fn inspector_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(
        context,
        &module,
        &[
            ("open", 3, |_, _, cx| {
                let disposable = object(cx);
                Ok(disposable.into())
            }),
            ("close", 0, |_, _, _| Ok(JsValue::undefined())),
            ("url", 0, |_, _, _| Ok(JsValue::undefined())),
            ("waitForDebugger", 0, |_, _, cx| {
                Err(coded_error(
                    cx,
                    "Inspector is not active",
                    "ERR_INSPECTOR_NOT_ACTIVE",
                ))
            }),
        ],
    )?;
    let console = crate::native::global(context, "console")?;
    module.set(js_string!("console"), console, false, context)?;
    let emitter = crate::modules::node_module(context, "events")?;
    let emitter = emitter.as_object();
    let (session, _) = class(
        context,
        ClassSpec {
            name: "Session",
            parent: emitter.as_ref(),
            init: |this, _, cx| {
                let events = object(cx);
                this.set(js_string!("_events"), events, false, cx)?;
                Ok(())
            },
            methods: &[
                ("connect", 0, |_, _, _| Ok(JsValue::undefined())),
                ("connectToMainThread", 0, |_, _, _| Ok(JsValue::undefined())),
                ("disconnect", 0, |_, _, _| Ok(JsValue::undefined())),
                ("post", 3, |_, args, cx| {
                    let error = crate::native::coded_error_value(
                        cx,
                        "The inspector is not available in this runtime",
                        "ERR_INSPECTOR_NOT_AVAILABLE",
                    );
                    match args.iter().find(|a| a.is_callable()) {
                        Some(callback) => {
                            crate::microtask(cx, callback.clone(), vec![error]);
                            Ok(JsValue::undefined())
                        }
                        None => Err(boa_engine::JsError::from_opaque(error)),
                    }
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Session"), session, false, context)?;
    let network = object(context);
    for name in [
        "requestWillBeSent",
        "responseReceived",
        "loadingFinished",
        "loadingFailed",
    ] {
        let f = function(context, name, 1, |_, _, _| Ok(JsValue::undefined()));
        network.set(JsString::from(name), f, false, context)?;
    }
    module.set(js_string!("Network"), network, false, context)?;
    Ok(module.into())
}

pub(crate) fn trace_events_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(
        context,
        &module,
        &[
            ("createTracing", 1, |_, args, cx| {
                let tracing = object(cx);
                let categories = get(&arg(args, 0), "categories", cx)?;
                let categories = list_of(&categories, cx)?
                    .iter()
                    .map(crate::print)
                    .collect::<Vec<_>>()
                    .join(",");
                tracing.set(js_string!("categories"), js_text(&categories), false, cx)?;
                tracing.set(js_string!("enabled"), false, false, cx)?;
                exports(
                    cx,
                    &tracing,
                    &[
                        ("enable", 0, |this, _, cx| {
                            if let Some(t) = this.as_object() {
                                t.set(js_string!("enabled"), true, false, cx)?;
                            }
                            Ok(JsValue::undefined())
                        }),
                        ("disable", 0, |this, _, cx| {
                            if let Some(t) = this.as_object() {
                                t.set(js_string!("enabled"), false, false, cx)?;
                            }
                            Ok(JsValue::undefined())
                        }),
                    ],
                )?;
                Ok(tracing.into())
            }),
            (
                "getEnabledCategories",
                0,
                |_, _, _| Ok(JsValue::undefined()),
            ),
        ],
    )?;
    Ok(module.into())
}
