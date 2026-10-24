//! `events`: Node's `EventEmitter`, natively, as `require('events')` answers it.
//!
//! The listeners are kept where Node keeps them, because libraries reach for them there: an
//! emitter's `_events` is an object with no prototype, holding for each event either its one
//! listener or an array of several, and `_eventsCount` says how many events have any. A
//! listener added with `once` is a wrapper with the function it stands for as `listener`, so
//! `off(f)` finds it and `rawListeners` shows it.
//!
//! Beyond the emitter's own methods the module has Node's statics: `once` (a promise of the
//! next event, also from an `EventTarget`), `on` (an async iterator of events), `errorMonitor`,
//! `getEventListeners`, `setMaxListeners`, `addAbortListener`, `listenerCount`.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsArray, JsPromise},
    property::PropertyKey,
};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error, get, hidden, invoke, js_text, list_of,
    object,
};

/// A `TypeError` with Node's `code`: what a bad argument throws.
pub(crate) fn coded_type_error(context: &mut Context, message: &str, code: &str) -> JsError {
    let error = JsNativeError::typ()
        .with_message(message.to_owned())
        .into_opaque(context);
    let _ = error.set(js_string!("code"), JsString::from(code), false, context);
    JsError::from_opaque(error.into())
}

/// An `AbortError`, as Node's `events` and `stream` reject with when a signal aborts.
pub(crate) fn abort_error(context: &mut Context, reason: JsValue) -> JsValue {
    let error = JsNativeError::error()
        .with_message("The operation was aborted")
        .into_opaque(context);
    let _ = error.set(js_string!("name"), js_string!("AbortError"), false, context);
    let _ = error.set(js_string!("code"), js_string!("ABORT_ERR"), false, context);
    if !reason.is_undefined() {
        let _ = error.set(js_string!("cause"), reason, false, context);
    }
    error.into()
}

/// The `EventEmitter` class, made once for the realm.
pub(crate) fn emitter_class(context: &mut Context) -> JsResult<JsObject> {
    crate::modules::node_module(context, "events")?
        .as_object()
        .ok_or_else(|| crate::native::type_error("EventEmitter is not a class"))
}

// ─── Where listeners are kept ────────────────────────────────────────────────────────────────

/// The emitter's `_events`, made when it has none (an object that took `EventEmitter`'s
/// methods without running its constructor).
fn events_of(target: &JsObject, context: &mut Context) -> JsResult<JsObject> {
    let events = target.get(js_string!("_events"), context)?;
    if let Some(events) = events.as_object() {
        return Ok(events);
    }
    let events = JsObject::with_null_proto();
    target.set(js_string!("_events"), events.clone(), false, context)?;
    target.set(js_string!("_eventsCount"), 0, false, context)?;
    Ok(events)
}

fn count_of(target: &JsObject, context: &mut Context) -> JsResult<f64> {
    let count = target.get(js_string!("_eventsCount"), context)?;
    Ok(if count.is_number() {
        count.to_number(context)?
    } else {
        0.
    })
}

/// What is kept for `name`: nothing, one listener, or an array of them.
fn kept(target: &JsValue, name: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let Some(target) = target.as_object() else {
        return Ok(JsValue::undefined());
    };
    let events = target.get(js_string!("_events"), context)?;
    let Some(events) = events.as_object() else {
        return Ok(JsValue::undefined());
    };
    let key = name.to_property_key(context)?;
    events.get(key, context)
}

/// The listeners of `name`, as they are kept (once-wrappers and all).
pub(crate) fn raw_listeners(
    target: &JsValue,
    name: &JsValue,
    context: &mut Context,
) -> JsResult<Vec<JsValue>> {
    let handler = kept(target, name, context)?;
    if handler.is_callable() {
        return Ok(vec![handler]);
    }
    if handler.is_object() {
        return list_of(&handler, context);
    }
    Ok(Vec::new())
}

/// The function a kept listener stands for: a once-wrapper's `listener`, or itself.
fn unwrapped(listener: &JsValue, context: &mut Context) -> JsValue {
    match listener.as_object() {
        Some(object) => match object.get(js_string!("listener"), context) {
            Ok(own) if own.is_callable() => own,
            _ => listener.clone(),
        },
        None => listener.clone(),
    }
}

fn is_listener_of(kept: &JsValue, listener: &JsValue, context: &mut Context) -> bool {
    kept == listener || unwrapped(kept, context) == *listener
}

/// How many listeners an emitter allows each event before warning: its own, or the default.
fn max_listeners(target: &JsObject, context: &mut Context) -> JsResult<f64> {
    let own = target.get(js_string!("_maxListeners"), context)?;
    if own.is_number() {
        return own.to_number(context);
    }
    let class = emitter_class(context)?;
    class
        .get(js_string!("defaultMaxListeners"), context)?
        .to_number(context)
}

// ─── Adding, removing, emitting ──────────────────────────────────────────────────────────────

fn check_listener(listener: &JsValue, context: &mut Context) -> JsResult<()> {
    if listener.is_callable() {
        return Ok(());
    }
    Err(coded_type_error(
        context,
        &format!(
            "The \"listener\" argument must be of type function. Received {}",
            describe_received(listener)
        ),
        "ERR_INVALID_ARG_TYPE",
    ))
}

fn describe_received(value: &JsValue) -> String {
    if value.is_undefined() {
        "undefined".into()
    } else if value.is_null() {
        "null".into()
    } else if value.is_object() {
        "an instance of Object".into()
    } else {
        format!("type {} ({})", value.type_of(), crate::print(value))
    }
}

/// `on` / `prependListener`: `newListener` heard first, then the listener kept — and the
/// leak warning when an event has more listeners than the emitter allows.
pub(crate) fn add_listener(
    this: &JsValue,
    name: &JsValue,
    listener: &JsValue,
    prepend: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    check_listener(listener, context)?;
    let Some(target) = this.as_object() else {
        return Ok(this.clone());
    };
    let events = events_of(&target, context)?;
    if events.has_own_property(js_string!("newListener"), context)? {
        let announced = unwrapped(listener, context);
        invoke(
            this,
            "emit",
            &[js_text("newListener"), name.clone(), announced],
            context,
        )?;
    }
    // `newListener` may have changed `_events`.
    let events = events_of(&target, context)?;
    let key = name.to_property_key(context)?;
    let existing = events.get(key.clone(), context)?;
    if existing.is_undefined() {
        events.set(key, listener.clone(), false, context)?;
        let count = count_of(&target, context)?;
        target.set(js_string!("_eventsCount"), count + 1., false, context)?;
        return Ok(this.clone());
    }
    let list = if existing.is_callable() {
        let items = if prepend {
            vec![listener.clone(), existing]
        } else {
            vec![existing, listener.clone()]
        };
        let list = JsArray::from_iter(items, context);
        events.set(key, list.clone(), false, context)?;
        list
    } else {
        let list = existing
            .as_object()
            .and_then(|o| JsArray::from_object(o.clone()).ok())
            .ok_or_else(|| crate::native::type_error("_events holds something not a listener"))?;
        if prepend {
            list.unshift(std::slice::from_ref(listener), context)?;
        } else {
            list.push(listener.clone(), context)?;
        }
        list
    };
    let max = max_listeners(&target, context)?;
    let length = list.length(context)? as f64;
    if max > 0. && length > max && !list.get(js_string!("warned"), context)?.to_boolean() {
        list.set(js_string!("warned"), true, false, context)?;
        warn_leak(this, name, length, max, context)?;
    }
    Ok(this.clone())
}

/// Node's `MaxListenersExceededWarning`, through `process.emitWarning`.
fn warn_leak(
    this: &JsValue,
    name: &JsValue,
    count: f64,
    max: f64,
    context: &mut Context,
) -> JsResult<()> {
    let kind = get(&get(this, "constructor", context)?, "name", context)?;
    let kind = if kind.is_string() {
        crate::print(&kind)
    } else {
        "EventEmitter".into()
    };
    let event = match name.as_symbol() {
        Some(symbol) => symbol.descriptive_string().to_std_string_escaped(),
        None => crate::print(name),
    };
    let message = format!(
        "Possible EventEmitter memory leak detected. {count} {event} listeners added to [{kind}]. \
         MaxListeners is {max}. Use emitter.setMaxListeners() to increase limit"
    );
    let warning = JsNativeError::error()
        .with_message(message)
        .into_opaque(context);
    warning.set(
        js_string!("name"),
        js_string!("MaxListenersExceededWarning"),
        false,
        context,
    )?;
    warning.set(js_string!("emitter"), this.clone(), false, context)?;
    warning.set(js_string!("type"), name.clone(), false, context)?;
    warning.set(js_string!("count"), count, false, context)?;
    let process = crate::native::global(context, "process")?;
    if get(&process, "emitWarning", context)?.is_callable() {
        invoke(&process, "emitWarning", &[warning.into()], context)?;
    }
    Ok(())
}

/// `once`'s wrapper: takes itself off, then calls the listener — once, however often it is
/// heard. Its state is `{ fired, target, type, listener, wrapFn }`, as Node's.
fn once_call(
    _: &JsValue,
    args: &[JsValue],
    state: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    if state.get(js_string!("fired"), context)?.to_boolean() {
        return Ok(JsValue::undefined());
    }
    state.set(js_string!("fired"), true, false, context)?;
    let target = state.get(js_string!("target"), context)?;
    let name = state.get(js_string!("type"), context)?;
    let wrapper = state.get(js_string!("wrapFn"), context)?;
    invoke(&target, "removeListener", &[name, wrapper], context)?;
    let listener = state.get(js_string!("listener"), context)?;
    crate::call(context, &listener, &target, args)
}

fn once_wrapper(
    this: &JsValue,
    name: &JsValue,
    listener: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    check_listener(listener, context)?;
    let state = object(context);
    state.set(js_string!("fired"), false, false, context)?;
    state.set(js_string!("target"), this.clone(), false, context)?;
    state.set(js_string!("type"), name.clone(), false, context)?;
    state.set(js_string!("listener"), listener.clone(), false, context)?;
    let wrapper = closure(context, "bound onceWrapper", 0, once_call, state.clone());
    wrapper.set(js_string!("listener"), listener.clone(), false, context)?;
    state.set(js_string!("wrapFn"), wrapper.clone(), false, context)?;
    Ok(wrapper.into())
}

/// `removeListener`: the last one added that is `listener` (or stands for it), then
/// `removeListener` heard.
pub(crate) fn remove_listener(
    this: &JsValue,
    name: &JsValue,
    listener: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    check_listener(listener, context)?;
    let Some(target) = this.as_object() else {
        return Ok(this.clone());
    };
    let events = target.get(js_string!("_events"), context)?;
    let Some(events) = events.as_object() else {
        return Ok(this.clone());
    };
    let key = name.to_property_key(context)?;
    let handler = events.get(key.clone(), context)?;
    if handler.is_undefined() {
        return Ok(this.clone());
    }
    let removed = if handler.is_callable() {
        if !is_listener_of(&handler, listener, context) {
            return Ok(this.clone());
        }
        forget_event(&target, &events, key, context)?;
        handler
    } else {
        let Some(list) = handler
            .as_object()
            .and_then(|o| JsArray::from_object(o.clone()).ok())
        else {
            return Ok(this.clone());
        };
        let items = list_of(&handler, context)?;
        let Some(at) = items
            .iter()
            .rposition(|item| is_listener_of(item, listener, context))
        else {
            return Ok(this.clone());
        };
        let removed = items[at].clone();
        let mut rest = items;
        rest.remove(at);
        match rest.len() {
            0 => forget_event(&target, &events, key, context)?,
            1 => {
                events.set(key, rest[0].clone(), false, context)?;
            }
            _ => {
                // Spliced in place, as Node does: whoever holds the array sees it shrink.
                let splice = list.get(js_string!("splice"), context)?;
                crate::call(
                    context,
                    &splice,
                    &list.clone().into(),
                    &[JsValue::from(at as f64), 1.into()],
                )?;
            }
        }
        removed
    };
    if events.has_own_property(js_string!("removeListener"), context)? {
        let announced = unwrapped(&removed, context);
        invoke(
            this,
            "emit",
            &[js_text("removeListener"), name.clone(), announced],
            context,
        )?;
    }
    Ok(this.clone())
}

/// Takes an event off `_events`, and counts it gone.
fn forget_event(
    target: &JsObject,
    events: &JsObject,
    key: PropertyKey,
    context: &mut Context,
) -> JsResult<()> {
    let count = count_of(target, context)? - 1.;
    if count <= 0. {
        target.set(
            js_string!("_events"),
            JsObject::with_null_proto(),
            false,
            context,
        )?;
        target.set(js_string!("_eventsCount"), 0, false, context)?;
    } else {
        events.delete_property_or_throw(key, context)?;
        target.set(js_string!("_eventsCount"), count, false, context)?;
    }
    Ok(())
}

fn remove_all_listeners(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(target) = this.as_object() else {
        return Ok(this.clone());
    };
    let events = target.get(js_string!("_events"), context)?;
    let Some(events) = events.as_object() else {
        return Ok(this.clone());
    };
    let name = arg(args, 0);
    // Nobody listening for `removeListener`: nothing to announce, so simply forgotten.
    if !events.has_own_property(js_string!("removeListener"), context)? {
        if args.is_empty() || name.is_undefined() {
            target.set(
                js_string!("_events"),
                JsObject::with_null_proto(),
                false,
                context,
            )?;
            target.set(js_string!("_eventsCount"), 0, false, context)?;
        } else {
            let key = name.to_property_key(context)?;
            if !events.get(key.clone(), context)?.is_undefined() {
                forget_event(&target, &events, key, context)?;
            }
        }
        return Ok(this.clone());
    }
    if args.is_empty() || name.is_undefined() {
        for key in events.own_property_keys(context)? {
            if matches!(&key, PropertyKey::String(s) if s.to_std_string_escaped() == "removeListener")
            {
                continue;
            }
            remove_all_listeners(this, &[JsValue::from(key)], context)?;
        }
        remove_all_listeners(this, &[js_text("removeListener")], context)?;
        target.set(
            js_string!("_events"),
            JsObject::with_null_proto(),
            false,
            context,
        )?;
        target.set(js_string!("_eventsCount"), 0, false, context)?;
        return Ok(this.clone());
    }
    // Last added first, as Node takes them off.
    let listeners = raw_listeners(this, &name, context)?;
    for listener in listeners.iter().rev() {
        remove_listener(this, &name, listener, context)?;
    }
    Ok(this.clone())
}

/// `emit`: every listener of `name`, in order, on a copy of the list — one added or removed
/// while they are heard changes the next `emit`, not this one. An `error` nobody hears is
/// thrown.
pub(crate) fn emit(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = arg(args, 0);
    let rest = &args[1.min(args.len())..];
    let is_error = name
        .as_string()
        .is_some_and(|n| n.to_std_string_escaped() == "error");
    if is_error {
        let monitor = emitter_class(context)?.get(js_string!("errorMonitor"), context)?;
        if !kept(this, &monitor, context)?.is_undefined() {
            let mut monitored = vec![monitor];
            monitored.extend(rest.iter().cloned());
            emit(this, &monitored, context)?;
        }
    }
    let handler = kept(this, &name, context)?;
    if handler.is_undefined() {
        if is_error {
            return Err(unhandled_error(arg(rest, 0), context));
        }
        return Ok(false.into());
    }
    if handler.is_callable() {
        crate::call(context, &handler, this, rest)?;
    } else {
        for listener in list_of(&handler, context)? {
            crate::call(context, &listener, this, rest)?;
        }
    }
    Ok(true.into())
}

/// What an unheard `error` throws: the error itself, or Node's `ERR_UNHANDLED_ERROR` around a
/// value that is not one.
fn unhandled_error(error: JsValue, context: &mut Context) -> JsError {
    if let Some(object) = error.as_object()
        && object.is::<boa_engine::builtins::error::Error>()
    {
        return JsError::from_opaque(error);
    }
    let shown = if error.is_undefined() {
        String::new()
    } else if error.is_string() {
        format!(" ('{}')", crate::print(&error))
    } else {
        format!(" ({})", crate::print(&error))
    };
    let wrapped = coded_error(
        context,
        &format!("Unhandled error.{shown}"),
        "ERR_UNHANDLED_ERROR",
    );
    if let Ok(value) = wrapped.clone().into_opaque(context)
        && let Some(object) = value.as_object()
    {
        let _ = object.set(js_string!("context"), error, false, context);
    }
    wrapped
}

const EMITTER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("on", 2, |this, args, cx| {
        add_listener(this, &arg(args, 0), &arg(args, 1), false, cx)
    }),
    ("addListener", 2, |this, args, cx| {
        add_listener(this, &arg(args, 0), &arg(args, 1), false, cx)
    }),
    ("prependListener", 2, |this, args, cx| {
        add_listener(this, &arg(args, 0), &arg(args, 1), true, cx)
    }),
    ("once", 2, |this, args, cx| {
        let wrapper = once_wrapper(this, &arg(args, 0), &arg(args, 1), cx)?;
        add_listener(this, &arg(args, 0), &wrapper, false, cx)
    }),
    ("prependOnceListener", 2, |this, args, cx| {
        let wrapper = once_wrapper(this, &arg(args, 0), &arg(args, 1), cx)?;
        add_listener(this, &arg(args, 0), &wrapper, true, cx)
    }),
    ("off", 2, |this, args, cx| {
        remove_listener(this, &arg(args, 0), &arg(args, 1), cx)
    }),
    ("removeListener", 2, |this, args, cx| {
        remove_listener(this, &arg(args, 0), &arg(args, 1), cx)
    }),
    ("removeAllListeners", 1, remove_all_listeners),
    ("emit", 1, emit),
    ("listeners", 1, |this, args, cx| {
        let list = raw_listeners(this, &arg(args, 0), cx)?;
        let list = list.iter().map(|l| unwrapped(l, cx)).collect();
        Ok(array(cx, list))
    }),
    ("rawListeners", 1, |this, args, cx| {
        let list = raw_listeners(this, &arg(args, 0), cx)?;
        Ok(array(cx, list))
    }),
    ("listenerCount", 2, |this, args, cx| {
        let list = raw_listeners(this, &arg(args, 0), cx)?;
        let listener = arg(args, 1);
        if listener.is_null_or_undefined() {
            return Ok(list.len().into());
        }
        let count = list
            .iter()
            .filter(|l| is_listener_of(l, &listener, cx))
            .count();
        Ok(count.into())
    }),
    ("eventNames", 0, |this, _, cx| {
        let names = match get(this, "_events", cx)?.as_object() {
            Some(events) => events
                .own_property_keys(cx)?
                .into_iter()
                .map(JsValue::from)
                .collect(),
            None => Vec::new(),
        };
        Ok(array(cx, names))
    }),
    ("setMaxListeners", 1, |this, args, cx| {
        let n = arg(args, 0);
        let value = n.to_number(cx)?;
        if !n.is_number() || value.is_nan() || value < 0. {
            return Err(JsNativeError::range()
                .with_message(format!(
                    "The value of \"n\" is out of range. It must be a non-negative number. Received {}",
                    crate::print(&n)
                ))
                .into());
        }
        if let Some(target) = this.as_object() {
            target.set(js_string!("_maxListeners"), value, false, cx)?;
        }
        Ok(this.clone())
    }),
    ("getMaxListeners", 0, |this, _, cx| match this.as_object() {
        Some(target) => Ok(max_listeners(&target, cx)?.into()),
        None => Ok(10.into()),
    }),
];

/// `EventEmitter.init`, and what the constructor does: `_events` made unless the object has
/// its own, `_maxListeners` kept, `captureRejections` read and kept.
fn init_emitter(target: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    let events = target.get(js_string!("_events"), context)?;
    let inherited = match target.prototype() {
        Some(prototype) => prototype.get(js_string!("_events"), context)?,
        None => JsValue::undefined(),
    };
    if events.is_undefined() || events == inherited {
        target.set(
            js_string!("_events"),
            JsObject::with_null_proto(),
            false,
            context,
        )?;
        target.set(js_string!("_eventsCount"), 0, false, context)?;
    }
    let max = target.get(js_string!("_maxListeners"), context)?;
    if !max.to_boolean() {
        target.set(
            js_string!("_maxListeners"),
            JsValue::undefined(),
            false,
            context,
        )?;
    }
    let capture = get(&arg(args, 0), "captureRejections", context)?;
    if !capture.is_undefined() {
        hidden(
            target,
            js_string!("_captureRejections"),
            capture.to_boolean(),
            context,
        )?;
    }
    Ok(())
}

// ─── The statics ─────────────────────────────────────────────────────────────────────────────

fn iterator_result(value: JsValue, done: bool, context: &mut Context) -> JsResult<JsValue> {
    let result = object(context);
    result.set(js_string!("value"), value, false, context)?;
    result.set(js_string!("done"), done, false, context)?;
    Ok(result.into())
}

/// Whether `target` is an `EventTarget` rather than an emitter: it listens with
/// `addEventListener` and has no `on`.
fn is_event_target(target: &JsValue, context: &mut Context) -> JsResult<bool> {
    Ok(get(target, "addEventListener", context)?.is_callable()
        && !get(target, "on", context)?.is_callable())
}

/// `EventEmitter.once(emitter, name, { signal })`: a promise of the arguments of the next
/// `name`, rejected by an `error` heard first, or by the signal aborting.
fn static_once(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (emitter, name, options) = (arg(args, 0), arg(args, 1), arg(args, 2));
    let signal = get(&options, "signal", context)?;
    if get(&signal, "aborted", context)?.to_boolean() {
        let reason = get(&signal, "reason", context)?;
        let error = abort_error(context, reason);
        return Ok(JsPromise::reject(JsError::from_opaque(error), context)?.into());
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    let state = object(context);
    state.set(js_string!("emitter"), emitter.clone(), false, context)?;
    state.set(js_string!("name"), name.clone(), false, context)?;
    state.set(js_string!("signal"), signal.clone(), false, context)?;
    state.set(
        js_string!("resolve"),
        resolvers.resolve.clone(),
        false,
        context,
    )?;
    state.set(
        js_string!("reject"),
        resolvers.reject.clone(),
        false,
        context,
    )?;
    let resolver = closure(
        context,
        "resolver",
        0,
        |_, args, state, cx| {
            once_settle(state, cx)?;
            let value = array(cx, args.to_vec());
            let resolve = state.get(js_string!("resolve"), cx)?;
            crate::call(cx, &resolve, &JsValue::undefined(), &[value])
        },
        state.clone(),
    );
    state.set(js_string!("resolver"), resolver.clone(), false, context)?;
    if is_event_target(&emitter, context)? {
        let options = object(context);
        options.set(js_string!("once"), true, false, context)?;
        invoke(
            &emitter,
            "addEventListener",
            &[name.clone(), resolver.into(), options.into()],
            context,
        )?;
    } else {
        invoke(&emitter, "once", &[name.clone(), resolver.into()], context)?;
        let is_error = name
            .as_string()
            .is_some_and(|n| n.to_std_string_escaped() == "error");
        if !is_error {
            let on_error = closure(
                context,
                "errorListener",
                1,
                |_, args, state, cx| {
                    once_settle(state, cx)?;
                    let reject = state.get(js_string!("reject"), cx)?;
                    crate::call(cx, &reject, &JsValue::undefined(), &[arg(args, 0)])
                },
                state.clone(),
            );
            state.set(
                js_string!("errorListener"),
                on_error.clone(),
                false,
                context,
            )?;
            invoke(
                &emitter,
                "once",
                &[js_text("error"), on_error.into()],
                context,
            )?;
        }
    }
    if signal.is_object() {
        let on_abort = closure(
            context,
            "abortListener",
            0,
            |_, _, state, cx| {
                once_settle(state, cx)?;
                let signal = state.get(js_string!("signal"), cx)?;
                let reason = get(&signal, "reason", cx)?;
                let error = abort_error(cx, reason);
                let reject = state.get(js_string!("reject"), cx)?;
                crate::call(cx, &reject, &JsValue::undefined(), &[error])
            },
            state.clone(),
        );
        state.set(
            js_string!("abortListener"),
            on_abort.clone(),
            false,
            context,
        )?;
        let options = object(context);
        options.set(js_string!("once"), true, false, context)?;
        invoke(
            &signal,
            "addEventListener",
            &[js_text("abort"), on_abort.into(), options.into()],
            context,
        )?;
    }
    Ok(promise.into())
}

/// The listeners `EventEmitter.once` added, taken off once it settles.
fn once_settle(state: &JsObject, context: &mut Context) -> JsResult<()> {
    let emitter = state.get(js_string!("emitter"), context)?;
    let name = state.get(js_string!("name"), context)?;
    let resolver = state.get(js_string!("resolver"), context)?;
    if is_event_target(&emitter, context)? {
        invoke(&emitter, "removeEventListener", &[name, resolver], context)?;
    } else {
        invoke(&emitter, "removeListener", &[name, resolver], context)?;
        let on_error = state.get(js_string!("errorListener"), context)?;
        if on_error.is_callable() {
            invoke(
                &emitter,
                "removeListener",
                &[js_text("error"), on_error],
                context,
            )?;
        }
    }
    let on_abort = state.get(js_string!("abortListener"), context)?;
    if on_abort.is_callable() {
        let signal = state.get(js_string!("signal"), context)?;
        invoke(
            &signal,
            "removeEventListener",
            &[js_text("abort"), on_abort],
            context,
        )?;
    }
    Ok(())
}

/// `EventEmitter.on(emitter, name, { signal, close })`: an async iterator of the arguments of
/// every `name`, kept in order until they are asked for; an `error` ends it by rejecting.
fn static_on(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (emitter, name, options) = (arg(args, 0), arg(args, 1), arg(args, 2));
    let state = object(context);
    state.set(js_string!("emitter"), emitter.clone(), false, context)?;
    state.set(js_string!("name"), name.clone(), false, context)?;
    let queue = JsArray::new(context)?;
    state.set(js_string!("queue"), queue, false, context)?;
    let pending = JsArray::new(context)?;
    state.set(js_string!("pending"), pending, false, context)?;
    state.set(js_string!("finished"), false, false, context)?;
    state.set(js_string!("error"), JsValue::null(), false, context)?;

    let listener = closure(
        context,
        "eventHandler",
        0,
        |_, args, state, cx| {
            let value = if is_event_target(&state.get(js_string!("emitter"), cx)?, cx)? {
                arg(args, 0)
            } else {
                array(cx, args.to_vec())
            };
            match take_pending(state, cx)? {
                Some((resolve, _)) => {
                    let result = iterator_result(value, false, cx)?;
                    crate::call(cx, &resolve, &JsValue::undefined(), &[result])?;
                }
                None => {
                    invoke(&state.get(js_string!("queue"), cx)?, "push", &[value], cx)?;
                }
            }
            Ok(JsValue::undefined())
        },
        state.clone(),
    );
    state.set(js_string!("listener"), listener.clone(), false, context)?;
    let target_is_dom = is_event_target(&emitter, context)?;
    if target_is_dom {
        invoke(
            &emitter,
            "addEventListener",
            &[name.clone(), listener.into()],
            context,
        )?;
    } else {
        invoke(&emitter, "on", &[name.clone(), listener.into()], context)?;
        let is_error = name
            .as_string()
            .is_some_and(|n| n.to_std_string_escaped() == "error");
        if !is_error {
            let on_error = closure(
                context,
                "errorHandler",
                1,
                |_, args, state, cx| {
                    on_iterator_stop(state, cx)?;
                    match take_pending(state, cx)? {
                        Some((_, reject)) => {
                            crate::call(cx, &reject, &JsValue::undefined(), &[arg(args, 0)])?;
                        }
                        None => state
                            .set(js_string!("error"), arg(args, 0), false, cx)
                            .map(|_| ())?,
                    }
                    Ok(JsValue::undefined())
                },
                state.clone(),
            );
            state.set(
                js_string!("errorListener"),
                on_error.clone(),
                false,
                context,
            )?;
            invoke(
                &emitter,
                "on",
                &[js_text("error"), on_error.into()],
                context,
            )?;
        }
        // `close`: events that end the iteration.
        let closers = list_of(&get(&options, "close", context)?, context)?;
        if !closers.is_empty() {
            let on_close = closure(
                context,
                "closeHandler",
                0,
                |_, _, state, cx| {
                    iterator_return(state, cx)?;
                    Ok(JsValue::undefined())
                },
                state.clone(),
            );
            state.set(
                js_string!("closeListener"),
                on_close.clone(),
                false,
                context,
            )?;
            state.set(
                js_string!("closers"),
                array(context, closers.clone()),
                false,
                context,
            )?;
            for closer in closers {
                invoke(&emitter, "on", &[closer, on_close.clone().into()], context)?;
            }
        }
    }
    let signal = get(&options, "signal", context)?;
    if signal.is_object() {
        state.set(js_string!("signal"), signal.clone(), false, context)?;
        if get(&signal, "aborted", context)?.to_boolean() {
            let reason = get(&signal, "reason", context)?;
            let error = abort_error(context, reason);
            state.set(js_string!("error"), error, false, context)?;
            on_iterator_stop(&state, context)?;
        } else {
            let on_abort = closure(
                context,
                "abortListener",
                0,
                |_, _, state, cx| {
                    let signal = state.get(js_string!("signal"), cx)?;
                    let reason = get(&signal, "reason", cx)?;
                    let error = abort_error(cx, reason);
                    on_iterator_stop(state, cx)?;
                    match take_pending(state, cx)? {
                        Some((_, reject)) => {
                            crate::call(cx, &reject, &JsValue::undefined(), &[error])?;
                        }
                        None => state
                            .set(js_string!("error"), error, false, cx)
                            .map(|_| ())?,
                    }
                    Ok(JsValue::undefined())
                },
                state.clone(),
            );
            state.set(
                js_string!("abortListener"),
                on_abort.clone(),
                false,
                context,
            )?;
            invoke(
                &signal,
                "addEventListener",
                &[js_text("abort"), on_abort.into()],
                context,
            )?;
        }
    }

    let iterator = object(context);
    let next = closure(
        context,
        "next",
        0,
        |_, _, state, cx| {
            let queue = state.get(js_string!("queue"), cx)?;
            if get(&queue, "length", cx)?.to_number(cx)? > 0. {
                let value = invoke(&queue, "shift", &[], cx)?;
                let result = iterator_result(value, false, cx)?;
                return Ok(JsPromise::resolve(result, cx)?.into());
            }
            let error = state.get(js_string!("error"), cx)?;
            if !error.is_null() {
                state.set(js_string!("error"), JsValue::null(), false, cx)?;
                state.set(js_string!("finished"), true, false, cx)?;
                return Ok(JsPromise::reject(JsError::from_opaque(error), cx)?.into());
            }
            if state.get(js_string!("finished"), cx)?.to_boolean() {
                let result = iterator_result(JsValue::undefined(), true, cx)?;
                return Ok(JsPromise::resolve(result, cx)?.into());
            }
            let (promise, resolvers) = JsPromise::new_pending(cx);
            let pair = array(cx, vec![resolvers.resolve.into(), resolvers.reject.into()]);
            invoke(&state.get(js_string!("pending"), cx)?, "push", &[pair], cx)?;
            Ok(promise.into())
        },
        state.clone(),
    );
    let finish = closure(
        context,
        "return",
        0,
        |_, _, state, cx| {
            iterator_return(state, cx)?;
            let result = iterator_result(JsValue::undefined(), true, cx)?;
            Ok(JsPromise::resolve(result, cx)?.into())
        },
        state.clone(),
    );
    let throw = closure(
        context,
        "throw",
        1,
        |_, args, state, cx| {
            on_iterator_stop(state, cx)?;
            state.set(js_string!("finished"), true, false, cx)?;
            Ok(JsPromise::reject(JsError::from_opaque(arg(args, 0)), cx)?.into())
        },
        state.clone(),
    );
    let itself = crate::native::function(context, "[Symbol.asyncIterator]", 0, |this, _, _| {
        Ok(this.clone())
    });
    hidden(&iterator, js_string!("next"), next, context)?;
    hidden(&iterator, js_string!("return"), finish, context)?;
    hidden(&iterator, js_string!("throw"), throw, context)?;
    hidden(&iterator, JsSymbol::async_iterator(), itself, context)?;
    Ok(iterator.into())
}

/// The next waiting `next()`'s resolving functions, if one waits.
fn take_pending(state: &JsObject, context: &mut Context) -> JsResult<Option<(JsValue, JsValue)>> {
    let pending = state.get(js_string!("pending"), context)?;
    if get(&pending, "length", context)?.to_number(context)? == 0. {
        return Ok(None);
    }
    let pair = invoke(&pending, "shift", &[], context)?;
    let pair = list_of(&pair, context)?;
    Ok(Some((arg(&pair, 0), arg(&pair, 1))))
}

/// The iterator's listeners taken off: it hears nothing more.
fn on_iterator_stop(state: &JsObject, context: &mut Context) -> JsResult<()> {
    state.set(js_string!("finished"), true, false, context)?;
    let emitter = state.get(js_string!("emitter"), context)?;
    let name = state.get(js_string!("name"), context)?;
    let listener = state.get(js_string!("listener"), context)?;
    if is_event_target(&emitter, context)? {
        invoke(&emitter, "removeEventListener", &[name, listener], context)?;
    } else {
        invoke(&emitter, "removeListener", &[name, listener], context)?;
        let on_error = state.get(js_string!("errorListener"), context)?;
        if on_error.is_callable() {
            invoke(
                &emitter,
                "removeListener",
                &[js_text("error"), on_error],
                context,
            )?;
        }
        let on_close = state.get(js_string!("closeListener"), context)?;
        if on_close.is_callable() {
            for closer in list_of(&state.get(js_string!("closers"), context)?, context)? {
                invoke(
                    &emitter,
                    "removeListener",
                    &[closer, on_close.clone()],
                    context,
                )?;
            }
        }
    }
    let on_abort = state.get(js_string!("abortListener"), context)?;
    if on_abort.is_callable() {
        let signal = state.get(js_string!("signal"), context)?;
        invoke(
            &signal,
            "removeEventListener",
            &[js_text("abort"), on_abort],
            context,
        )?;
    }
    Ok(())
}

/// The iterator ended from outside (`return()`, a `close` event): every waiting `next()` told
/// it is done.
fn iterator_return(state: &JsObject, context: &mut Context) -> JsResult<()> {
    on_iterator_stop(state, context)?;
    while let Some((resolve, _)) = take_pending(state, context)? {
        let result = iterator_result(JsValue::undefined(), true, context)?;
        crate::call(context, &resolve, &JsValue::undefined(), &[result])?;
    }
    Ok(())
}

const STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("once", 3, static_once),
    ("on", 3, static_on),
    ("init", 1, |this, args, cx| {
        if let Some(target) = this.as_object() {
            init_emitter(&target, args, cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("listenerCount", 2, |_, args, cx| {
        invoke(&arg(args, 0), "listenerCount", &[arg(args, 1)], cx)
    }),
    ("getEventListeners", 2, |_, args, cx| {
        let target = arg(args, 0);
        if get(&target, "listeners", cx)?.is_callable() {
            return invoke(&target, "listeners", &[arg(args, 1)], cx);
        }
        Ok(array(cx, Vec::new()))
    }),
    ("getMaxListeners", 1, |_, args, cx| {
        let target = arg(args, 0);
        if get(&target, "getMaxListeners", cx)?.is_callable() {
            return invoke(&target, "getMaxListeners", &[], cx);
        }
        emitter_class(cx)?.get(js_string!("defaultMaxListeners"), cx)
    }),
    ("setMaxListeners", 1, |_, args, cx| {
        let n = arg(args, 0);
        let n = if n.is_undefined() { 10.into() } else { n };
        if args.len() <= 1 {
            emitter_class(cx)?.set(js_string!("defaultMaxListeners"), n, false, cx)?;
            return Ok(JsValue::undefined());
        }
        for target in &args[1..] {
            if get(target, "setMaxListeners", cx)?.is_callable() {
                invoke(target, "setMaxListeners", std::slice::from_ref(&n), cx)?;
            }
        }
        Ok(JsValue::undefined())
    }),
    ("addAbortListener", 2, |_, args, cx| {
        let (signal, listener) = (arg(args, 0), arg(args, 1));
        check_listener(&listener, cx)?;
        if get(&signal, "aborted", cx)?.to_boolean() {
            crate::microtask(cx, listener.clone(), Vec::new());
        } else {
            let options = object(cx);
            options.set(js_string!("once"), true, false, cx)?;
            invoke(
                &signal,
                "addEventListener",
                &[js_text("abort"), listener.clone(), options.into()],
                cx,
            )?;
        }
        // A disposable: `[Symbol.dispose]()` takes the listener off.
        let disposable = object(cx);
        let pair = array(cx, vec![signal, listener]);
        let dispose = closure(
            cx,
            "dispose",
            0,
            |_, _, pair, cx| {
                let pair = list_of(pair, cx)?;
                if get(&pair[0], "removeEventListener", cx)?.is_callable() {
                    invoke(
                        &pair[0],
                        "removeEventListener",
                        &[js_text("abort"), pair[1].clone()],
                        cx,
                    )?;
                }
                Ok(JsValue::undefined())
            },
            pair,
        );
        let symbol = get(&crate::native::global(cx, "Symbol")?, "dispose", cx)?;
        if let Some(symbol) = symbol.as_symbol() {
            hidden(&disposable, symbol, dispose.clone(), cx)?;
        }
        hidden(&disposable, js_string!("dispose"), dispose, cx)?;
        Ok(disposable.into())
    }),
];

/// `require('events')`: the `EventEmitter` class, which is the module, with `EventEmitter`,
/// `EventEmitterAsyncResource` and the statics on it.
pub(crate) fn events_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    // Constructed — `new EventEmitter()`, `super()` — or called on an object as
    // `EventEmitter.call(this)` by older code: see `crate::native`.
    let (emitter, prototype) = class(
        context,
        ClassSpec {
            name: "EventEmitter",
            length: 1,
            init: init_emitter,
            statics: STATICS,
            ..Default::default()
        },
    )?;
    // Enumerable, as Node assigns them: mixins copy them with `Object.assign` or `for…in`.
    crate::native::exports(context, &prototype, EMITTER_METHODS)?;
    // As Node's prototype has them: an emitter whose constructor never ran still reads them.
    prototype.set(js_string!("_events"), JsValue::undefined(), false, context)?;
    prototype.set(js_string!("_eventsCount"), 0, false, context)?;
    prototype.set(
        js_string!("_maxListeners"),
        JsValue::undefined(),
        false,
        context,
    )?;
    emitter.set(js_string!("EventEmitter"), emitter.clone(), false, context)?;
    emitter.set(js_string!("defaultMaxListeners"), 10, false, context)?;
    emitter.set(js_string!("captureRejections"), false, false, context)?;
    emitter.set(js_string!("usingDomains"), false, false, context)?;
    let monitor = JsSymbol::new(Some(js_string!("events.errorMonitor")))
        .ok_or_else(|| crate::native::type_error("no more symbols"))?;
    emitter.set(js_string!("errorMonitor"), monitor, false, context)?;
    let symbol = crate::native::global(context, "Symbol")?;
    let rejection = invoke(&symbol, "for", &[js_text("nodejs.rejection")], context)?;
    emitter.set(
        js_string!("captureRejectionSymbol"),
        rejection,
        false,
        context,
    )?;
    let (resource, _) = class(
        context,
        ClassSpec {
            name: "EventEmitterAsyncResource",
            length: 1,
            init: init_emitter,
            parent: Some(&emitter),
            ..Default::default()
        },
    )?;
    emitter.set(
        js_string!("EventEmitterAsyncResource"),
        resource,
        false,
        context,
    )?;
    Ok(emitter.into())
}
