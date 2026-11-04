//! `Call` of a compiled ordinary function, without the interpreter's generic call path.
//!
//! The interpreter's `Call` asks the callee's internal method to call it, which answers what is
//! left to do and is resolved: for an ordinary function that ends in
//! [`enter_ordinary_function`], which pushes the frame. Here compiled code asks for that
//! directly, when the callee is such a function with compiled code: the frame, `this`, the
//! environments and the shadow stack are the interpreter's own. Anything else (a native or a
//! bound function, a proxy, a class constructor, code not compiled yet, a limit about to be
//! passed, an interrupt) is left to `jit_step`, which does what the interpreter does.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{
    Context, JsObject,
    builtins::function::{FunctionParts, OrdinaryFunction, enter_ordinary_function},
};

use super::runtime::{EXIT, JitState, finish};

/// What [`jit_call`] answers when it did nothing: compiled code runs the instruction with
/// `jit_step`.
pub(crate) const NOT_HANDLED: u32 = EXIT - 1;

/// `Call` with `argument_count` arguments, whose next instruction is at `next`. Answers the
/// `pc` to go on at, as `jit_step` does, or [`NOT_HANDLED`].
pub(crate) unsafe extern "C" fn jit_call(
    state: *mut JitState,
    next: u32,
    argument_count: u32,
    _: u32,
) -> u32 {
    // SAFETY: compiled code passes the state `execute` made, which outlives it.
    let state = unsafe { &mut *state };
    if state.panic.is_some() {
        return EXIT;
    }
    match catch_unwind(AssertUnwindSafe(|| {
        call(state, next, argument_count as usize)
    })) {
        Ok(answer) => answer,
        Err(payload) => {
            state.panic = Some(payload);
            EXIT
        }
    }
}

fn call(state: &mut JitState, next: u32, argument_count: usize) -> u32 {
    // SAFETY: the context outlives compiled code, which uses it only through helpers.
    let context = unsafe { &mut *state.context };
    let Some((function, parts)) = compiled_callee(context, argument_count) else {
        return NOT_HANDLED;
    };
    // What the interpreter's handler does first: the `pc` the frame goes on at.
    context.vm.frame_mut().pc = next;
    let flow = match enter_ordinary_function(context, &function, parts, argument_count) {
        Ok(()) => std::ops::ControlFlow::Continue(()),
        Err(error) => context.handle_error(error),
    };
    finish(state, flow)
}

/// The callee of a `Call` with `argument_count` arguments, and what its frame is made from, if
/// it is an ordinary function that may be called and its code is compiled. Changes nothing.
fn compiled_callee(context: &Context, argument_count: usize) -> Option<(JsObject, FunctionParts)> {
    let vm = &context.vm;
    let callee = vm.stack.calling_convention_get_function(argument_count);
    let object = callee.as_object()?;
    let function = object.try_downcast_ref::<OrdinaryFunction>()?;
    let code = &function.code;
    if code.is_lazy() || code.is_class_constructor() || !code.jit.is_compiled() {
        return None;
    }
    // The interpreter's call checks the limits first, and an interrupt is cleared by the check
    // that throws it, which is left to it.
    if context.runtime_limits_exceeded()
        || vm
            .interrupt
            .as_deref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed))
    {
        return None;
    }
    let parts = (
        code.clone(),
        function.environments.clone(),
        function.script_or_module.clone(),
        function.realm().clone(),
    );
    Some((object.clone(), parts))
}
