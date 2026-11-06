//! `AbortController` and `AbortSignal`, and the iteration `AbortSignal.any` takes its signals by.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsError, JsObject, JsResult, JsValue, js_string};

use super::{Kind, TargetData, dispatch, dom, dom_exception, new_event, take_off, with_target};
use crate::native::{arg, closure, type_error};

/// A signal not aborted, of the realm's own `AbortSignal`.
pub(crate) fn new_signal(context: &mut Context) -> JsResult<JsObject> {
    let prototype = dom(context)?.abort_signal_proto.clone();
    Ok(JsObject::from_proto_and_data(
        prototype,
        TargetData::new(Kind::Signal {
            aborted: false,
            reason: JsValue::undefined(),
            removals: Vec::new(),
            dependents: Vec::new(),
        }),
    ))
}

fn signal_state(this: &JsValue) -> JsResult<(bool, JsValue)> {
    with_target(this, |data| match &*data.kind.borrow() {
        Kind::Signal {
            aborted, reason, ..
        } => Ok((*aborted, reason.clone())),
        _ => Err(type_error("Illegal invocation")),
    })?
}

/// Aborts `signal` with `reason` (an `AbortError` when none is given): the listeners added
/// with it taken off their targets, its `abort` event fired, then the signals made from it by
/// `AbortSignal.any` aborted with the same reason.
pub(crate) fn abort(signal: &JsObject, reason: JsValue, context: &mut Context) -> JsResult<()> {
    let reason = if reason.is_undefined() {
        dom_exception(context, "This operation was aborted", "AbortError")
    } else {
        reason
    };
    let taken = with_target(&signal.clone().into(), |data| {
        let mut kind = data.kind.borrow_mut();
        let Kind::Signal {
            aborted,
            reason: own,
            removals,
            dependents,
        } = &mut *kind
        else {
            return Err(type_error("not an AbortSignal"));
        };
        if *aborted {
            return Ok(None);
        }
        *aborted = true;
        *own = reason.clone();
        Ok(Some((std::mem::take(removals), std::mem::take(dependents))))
    })??;
    let Some((removals, dependents)) = taken else {
        return Ok(());
    };
    for (target, listener) in removals {
        take_off(&target, &listener);
    }
    let event = new_event(context, "abort")?;
    dispatch(signal, &event, context)?;
    for dependent in dependents {
        abort(&dependent, reason.clone(), context)?;
    }
    Ok(())
}

pub(super) const SIGNAL_METHODS: &[(&str, usize, NativeFunctionPointer)] =
    &[("throwIfAborted", 0, |this, _, _| {
        let (aborted, reason) = signal_state(this)?;
        if aborted {
            return Err(JsError::from_opaque(reason));
        }
        Ok(JsValue::undefined())
    })];

pub(super) const SIGNAL_ACCESSORS: &[(
    &str,
    NativeFunctionPointer,
    Option<NativeFunctionPointer>,
)] = &[
    (
        "aborted",
        |this, _, _| Ok(signal_state(this)?.0.into()),
        None,
    ),
    ("reason", |this, _, _| Ok(signal_state(this)?.1), None),
];

pub(super) const SIGNAL_STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("abort", 1, |_, args, cx| {
        let signal = new_signal(cx)?;
        abort(&signal, arg(args, 0), cx)?;
        Ok(signal.into())
    }),
    ("timeout", 1, |_, args, cx| {
        let ms = arg(args, 0).to_number(cx)?;
        if !ms.is_finite() || ms < 0. {
            return Err(type_error(
                "AbortSignal.timeout: the delay must be a non-negative number",
            ));
        }
        let signal = new_signal(cx)?;
        let fire = closure(
            cx,
            "",
            0,
            |_, _, signal: &JsObject, cx| {
                let reason = dom_exception(cx, "signal timed out", "TimeoutError");
                abort(signal, reason, cx)?;
                Ok(JsValue::undefined())
            },
            signal.clone(),
        );
        crate::set_timeout(cx, fire.into(), ms, false, Vec::new());
        Ok(signal.into())
    }),
    ("any", 1, |_, args, cx| {
        let signals = iterate(&arg(args, 0), cx)?;
        let result = new_signal(cx)?;
        for source in &signals {
            let (aborted, reason) = signal_state(source)?;
            if aborted {
                abort(&result, reason, cx)?;
                return Ok(result.into());
            }
        }
        for source in signals {
            with_target(&source, |data| {
                if let Kind::Signal { dependents, .. } = &mut *data.kind.borrow_mut() {
                    dependents.push(result.clone());
                }
            })?;
        }
        Ok(result.into())
    }),
];

/// The items of an iterable, as a list: what `AbortSignal.any` is given.
pub(crate) fn iterate(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let array = crate::call(
        context,
        &from,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    crate::native::list_of(&array, context)
}
