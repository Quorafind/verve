//! `timers` and `timers/promises`: Node's timers, on the runtime's own clock (the one the host
//! drives).
//!
//! `require('timers').setTimeout` answers a `Timeout`, as Node's does — `ref`, `unref`,
//! `hasRef`, `refresh`, `close`, and its number when made a primitive, so `clearTimeout` (the
//! global one too) takes it. The global `setTimeout` keeps answering a number, as a browser's
//! (and Electron's renderer's) does. `timers/promises` has `setTimeout` and `setImmediate` as
//! promises and `setInterval` as an async iterator, each stopped by an `AbortSignal`, and
//! `scheduler.wait` and `scheduler.yield`.

use boa_engine::{
    Context, JsObject, JsResult, JsSymbol, JsValue, builtins::promise::ResolvingFunctions,
    js_string, native_function::NativeFunctionPointer, object::builtins::JsPromise,
};

use crate::native::{
    ClassSpec, arg, array, class, closure, exports, function, get, hidden, invoke, js_text, object,
};

/// The error an aborted timer rejects with: Node's `AbortError`, `cause` the signal's reason.
fn abort_error(signal: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let error = boa_engine::JsNativeError::error()
        .with_message("The operation was aborted")
        .into_opaque(context);
    error.set(js_string!("name"), js_text("AbortError"), false, context)?;
    error.set(js_string!("code"), js_text("ABORT_ERR"), false, context)?;
    let reason = get(signal, "reason", context)?;
    error.set(js_string!("cause"), reason, false, context)?;
    Ok(error.into())
}

fn delay_of(value: &JsValue, context: &mut Context) -> JsResult<f64> {
    if value.is_undefined() {
        Ok(1.)
    } else {
        value.to_number(context)
    }
}

// ─── Timeout ────────────────────────────────────────────────────────────────────────────────

/// The timer a `Timeout` stands for: its id, and what `refresh` needs to start it again.
fn timeout_id(this: &JsValue, context: &mut Context) -> JsResult<f64> {
    get(this, "_id", context)?.to_number(context)
}

const TIMEOUT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("ref", 0, |this, _, cx| {
        if let Some(t) = this.as_object() {
            hidden(&t, js_string!("_ref"), true, cx)?;
        }
        Ok(this.clone())
    }),
    ("unref", 0, |this, _, cx| {
        if let Some(t) = this.as_object() {
            hidden(&t, js_string!("_ref"), false, cx)?;
        }
        Ok(this.clone())
    }),
    ("hasRef", 0, |this, _, cx| {
        Ok(get(this, "_ref", cx)?.to_boolean().into())
    }),
    ("refresh", 0, |this, _, cx| {
        // Starts the same callback again from now, under a new id the `Timeout` keeps.
        let Some(timeout) = this.as_object() else {
            return Ok(this.clone());
        };
        let id = timeout_id(this, cx)? as u32;
        if crate::shared(cx).timers.borrow().contains_key(&id) {
            crate::clear_timer(cx, id);
            let callback = timeout.get(js_string!("_onTimeout"), cx)?;
            let delay = timeout.get(js_string!("_idleTimeout"), cx)?.to_number(cx)?;
            let repeat = timeout.get(js_string!("_repeat"), cx)?.to_boolean();
            let args = crate::native::list_of(&timeout.get(js_string!("_args"), cx)?, cx)?;
            let id = crate::set_timeout(cx, callback, delay, repeat, args);
            hidden(&timeout, js_string!("_id"), id, cx)?;
        }
        Ok(this.clone())
    }),
    ("close", 0, |this, _, cx| {
        let id = timeout_id(this, cx)?;
        crate::clear_timer(cx, id as u32);
        Ok(this.clone())
    }),
];

/// A `Timeout` for a timer just started.
fn timeout(
    args: &[JsValue],
    repeat: bool,
    immediate: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback = arg(args, 0);
    if !callback.is_callable() {
        return Err(crate::native::coded_error(
            context,
            &format!(
                "The \"callback\" argument must be of type function. Received {}",
                callback.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let (delay, rest) = if immediate {
        (0., args.iter().skip(1).cloned().collect::<Vec<_>>())
    } else {
        let delay = arg(args, 1);
        let delay = if delay.is_undefined() {
            0.
        } else {
            delay.to_number(context)?
        };
        (delay, args.iter().skip(2).cloned().collect())
    };
    let carried = crate::hooks::carry_callback(callback.clone(), context);
    let id = crate::set_timeout(context, carried, delay, repeat, rest.clone());
    let module = crate::modules::node_module(context, "timers")?;
    let name = if immediate { "Immediate" } else { "Timeout" };
    let class = get(&module, name, context)?;
    let prototype = get(&class, "prototype", context)?;
    let made = object(context);
    made.set_prototype(prototype.as_object());
    hidden(&made, js_string!("_id"), id, context)?;
    hidden(&made, js_string!("_ref"), true, context)?;
    hidden(&made, js_string!("_onTimeout"), callback, context)?;
    hidden(&made, js_string!("_idleTimeout"), delay, context)?;
    hidden(&made, js_string!("_repeat"), repeat, context)?;
    let rest = array(context, rest);
    hidden(&made, js_string!("_args"), rest, context)?;
    Ok(made.into())
}

const TIMERS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("setTimeout", 2, |_, args, cx| {
        timeout(args, false, false, cx)
    }),
    ("setInterval", 2, |_, args, cx| {
        timeout(args, true, false, cx)
    }),
    ("setImmediate", 1, |_, args, cx| {
        timeout(args, false, true, cx)
    }),
    ("clearTimeout", 1, clear),
    ("clearInterval", 1, clear),
    ("clearImmediate", 1, clear),
];

fn clear(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let given = arg(args, 0);
    let id = if given.is_object() {
        given.to_number(context)?
    } else {
        given.as_number().unwrap_or(f64::NAN)
    };
    if id.is_finite() {
        crate::clear_timer(context, id as u32);
    }
    Ok(JsValue::undefined())
}

pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, TIMERS)?;
    for name in ["Timeout", "Immediate"] {
        let (class, prototype) = class(
            context,
            ClassSpec {
                name,
                methods: TIMEOUT_METHODS,
                ..ClassSpec::default()
            },
        )?;
        // `+timeout`, `clearTimeout(timeout)`: its number.
        let primitive = function(context, "[Symbol.toPrimitive]", 1, |this, _, cx| {
            get(this, "_id", cx)
        });
        hidden(&prototype, JsSymbol::to_primitive(), primitive, context)?;
        module.set(boa_engine::JsString::from(name), class, false, context)?;
    }
    let promises = crate::modules::node_module(context, "timers/promises")?;
    module.set(js_string!("promises"), promises, false, context)?;
    Ok(module.into())
}

// ─── timers/promises ────────────────────────────────────────────────────────────────────────

/// Settles a pending timer promise: resolved with `value`.
fn fire(
    _: &JsValue,
    _: &[JsValue],
    (resolvers, value): &(ResolvingFunctions, JsValue),
    context: &mut Context,
) -> JsResult<JsValue> {
    resolvers
        .resolve
        .call(&JsValue::undefined(), std::slice::from_ref(value), context)?;
    Ok(JsValue::undefined())
}

/// An aborted signal's listener: the timer cleared, the promise rejected.
fn aborted(
    _: &JsValue,
    _: &[JsValue],
    (resolvers, id, signal): &(ResolvingFunctions, JsValue, JsValue),
    context: &mut Context,
) -> JsResult<JsValue> {
    if let Some(id) = id.as_number() {
        crate::clear_timer(context, id as u32);
    }
    let error = abort_error(signal, context)?;
    resolvers
        .reject
        .call(&JsValue::undefined(), &[error], context)?;
    Ok(JsValue::undefined())
}

/// `setTimeout(delay, value, { signal })` as a promise.
fn delayed(
    delay: f64,
    value: JsValue,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let signal = get(options, "signal", context)?;
    if signal.is_object() && get(&signal, "aborted", context)?.to_boolean() {
        let error = abort_error(&signal, context)?;
        return Ok(JsPromise::reject(boa_engine::JsError::from_opaque(error), context)?.into());
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    let callback = closure(context, "", 0, fire, (resolvers.clone(), value));
    let id = crate::set_timeout(context, callback.into(), delay, false, Vec::new());
    if signal.is_object() {
        let listener = closure(
            context,
            "",
            0,
            aborted,
            (resolvers, JsValue::from(id), signal.clone()),
        );
        let once = object(context);
        once.set(js_string!("once"), true, false, context)?;
        invoke(
            &signal,
            "addEventListener",
            &[js_text("abort"), listener.into(), once.into()],
            context,
        )?;
    }
    Ok(promise.into())
}

/// The state of a `setInterval` iterator, kept on it: its timer, the ticks not yet taken, and
/// the `next()` calls waiting for one.
fn interval_tick(
    _: &JsValue,
    _: &[JsValue],
    iterator: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let waiting =
        crate::native::list_of(&iterator.get(js_string!("__waiting"), context)?, context)?;
    let value = iterator.get(js_string!("__value"), context)?;
    if let Some((first, rest)) = waiting.split_first() {
        let rest = array(context, rest.to_vec());
        hidden(iterator, js_string!("__waiting"), rest, context)?;
        let result = step(value, false, context)?;
        crate::call(context, first, &JsValue::undefined(), &[result])?;
    } else {
        let ticks = iterator
            .get(js_string!("__ticks"), context)?
            .to_number(context)?;
        hidden(iterator, js_string!("__ticks"), ticks + 1., context)?;
    }
    Ok(JsValue::undefined())
}

fn step(value: JsValue, done: bool, context: &mut Context) -> JsResult<JsValue> {
    let result = object(context);
    result.set(js_string!("value"), value, false, context)?;
    result.set(js_string!("done"), done, false, context)?;
    Ok(result.into())
}

const INTERVAL_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("next", 0, |this, _, cx| {
        let Some(iterator) = this.as_object() else {
            return Ok(JsValue::undefined());
        };
        if iterator.get(js_string!("__done"), cx)?.to_boolean() {
            let done = step(JsValue::undefined(), true, cx)?;
            return Ok(JsPromise::resolve(done, cx)?.into());
        }
        let ticks = iterator.get(js_string!("__ticks"), cx)?.to_number(cx)?;
        if ticks > 0. {
            hidden(&iterator, js_string!("__ticks"), ticks - 1., cx)?;
            let value = iterator.get(js_string!("__value"), cx)?;
            let result = step(value, false, cx)?;
            return Ok(JsPromise::resolve(result, cx)?.into());
        }
        let (promise, resolvers) = JsPromise::new_pending(cx);
        let waiting = iterator.get(js_string!("__waiting"), cx)?;
        invoke(&waiting, "push", &[resolvers.resolve.into()], cx)?;
        Ok(promise.into())
    }),
    ("return", 0, |this, _, cx| {
        if let Some(iterator) = this.as_object() {
            let id = iterator.get(js_string!("__id"), cx)?.to_number(cx)?;
            crate::clear_timer(cx, id as u32);
            hidden(&iterator, js_string!("__done"), true, cx)?;
            for waiting in crate::native::list_of(&iterator.get(js_string!("__waiting"), cx)?, cx)?
            {
                let done = step(JsValue::undefined(), true, cx)?;
                crate::call(cx, &waiting, &JsValue::undefined(), &[done])?;
            }
        }
        let done = step(JsValue::undefined(), true, cx)?;
        Ok(JsPromise::resolve(done, cx)?.into())
    }),
];

fn interval_iterator(delay: f64, value: JsValue, context: &mut Context) -> JsResult<JsValue> {
    let iterator = object(context);
    crate::native::methods(context, &iterator, INTERVAL_METHODS)?;
    let itself = function(context, "[Symbol.asyncIterator]", 0, |this, _, _| {
        Ok(this.clone())
    });
    hidden(&iterator, JsSymbol::async_iterator(), itself, context)?;
    hidden(&iterator, js_string!("__value"), value, context)?;
    hidden(&iterator, js_string!("__ticks"), 0, context)?;
    let waiting = array(context, Vec::new());
    hidden(&iterator, js_string!("__waiting"), waiting, context)?;
    let tick = closure(context, "", 0, interval_tick, iterator.clone());
    let id = crate::set_timeout(context, tick.into(), delay, true, Vec::new());
    hidden(&iterator, js_string!("__id"), id, context)?;
    Ok(iterator.into())
}

const PROMISES: &[(&str, usize, NativeFunctionPointer)] = &[
    ("setTimeout", 3, |_, args, cx| {
        let delay = delay_of(&arg(args, 0), cx)?;
        delayed(delay, arg(args, 1), &arg(args, 2), cx)
    }),
    ("setImmediate", 2, |_, args, cx| {
        delayed(0., arg(args, 0), &arg(args, 1), cx)
    }),
    ("setInterval", 3, |_, args, cx| {
        let delay = delay_of(&arg(args, 0), cx)?;
        interval_iterator(delay, arg(args, 1), cx)
    }),
];

pub(crate) fn promises_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, PROMISES)?;
    let scheduler = object(context);
    exports(
        context,
        &scheduler,
        &[
            ("wait", 2, |_, args, cx| {
                let delay = delay_of(&arg(args, 0), cx)?;
                delayed(delay, JsValue::undefined(), &arg(args, 1), cx)
            }),
            ("yield", 0, |_, _, cx| {
                delayed(0., JsValue::undefined(), &JsValue::undefined(), cx)
            }),
        ],
    )?;
    module.set(js_string!("scheduler"), scheduler, false, context)?;
    Ok(module.into())
}
