//! Fast helpers: the common case of an instruction, done directly, with its operands decoded by
//! the compiler.
//!
//! Each helper either does exactly what the interpreter's handler would do in that case and
//! answers [`DONE`], or changes nothing and answers [`SLOW`], and compiled code runs the
//! instruction with [`jit_step`](super::runtime::jit_step) instead. So a fast path only has to
//! be right about when it applies; whatever it does not cover is the interpreter's.
//!
//! None of them can throw, call JavaScript, or observe `frame.pc`, which is why they need not
//! set it. A panic is caught, kept in the state, and turned into [`SLOW`]; `jit_step` then
//! leaves compiled code at once, and [`execute`](super::runtime::execute) resumes the panic.
//!
//! Registers are reached through the VM stack from the frame's `rp`, bounds checked; the
//! compiler has also checked every register operand against the code block's register count.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{Context, JsObject, JsValue, object::shape::slot::SlotAttributes, vm::CodeBlock};

use super::runtime::JitState;

/// The fast path applied.
pub(crate) const DONE: u32 = 0;
/// The fast path does not apply: run the instruction with `jit_step`.
pub(crate) const SLOW: u32 = 1;

/// The signature of every fast helper: the state and up to three operands.
pub(crate) type HelperFn = unsafe extern "C" fn(*mut JitState, u32, u32, u32) -> u32;

/// Runs `fast` with the context and the code block, turning a panic into [`SLOW`].
#[inline]
fn guarded(
    state: *mut JitState,
    fast: impl FnOnce(&mut Context, &CodeBlock, usize) -> Option<()>,
) -> u32 {
    // SAFETY: compiled code passes the state `execute` made, which outlives it.
    let state = unsafe { &mut *state };
    // SAFETY: the context and the code block outlive compiled code, which uses them only
    // through helpers, one at a time.
    let (context, code_block) = unsafe { (&mut *state.context, &*state.code_block) };
    let rp = state.rp as usize;
    match catch_unwind(AssertUnwindSafe(|| fast(context, code_block, rp))) {
        Ok(Some(())) => DONE,
        Ok(None) => SLOW,
        Err(payload) => {
            state.panic = Some(payload);
            SLOW
        }
    }
}

/// Like [`guarded`], for a helper that may move the VM stack: compiled code reloads the
/// registers' address from the state after it.
#[inline]
fn guarded_moving(
    state: *mut JitState,
    fast: impl FnOnce(&mut Context, &CodeBlock, usize) -> Option<()>,
) -> u32 {
    let status = guarded(state, fast);
    // SAFETY: as in `guarded`.
    let state = unsafe { &mut *state };
    // SAFETY: as in `guarded`; the frame's registers are on the stack, from `rp` on.
    unsafe {
        let context = &mut *state.context;
        state.regs = context.vm.stack.stack.as_mut_ptr().add(state.rp as usize);
    }
    status
}

fn register(context: &Context, rp: usize, index: u32) -> &JsValue {
    &context.vm.stack.stack[rp + index as usize]
}

fn set_register(context: &mut Context, rp: usize, index: u32, value: JsValue) {
    context.vm.stack.stack[rp + index as usize] = value;
}

/// `Move` of a value that holds a pointer (the rest is inline): a clone.
pub(crate) unsafe extern "C" fn jit_move(state: *mut JitState, dst: u32, src: u32, _: u32) -> u32 {
    guarded(state, |context, _, rp| {
        let value = register(context, rp, src).clone();
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// An operand that names no register.
pub(crate) const NO_REGISTER: u32 = u32::MAX;

/// Up to three `PushFromRegister`s in a row; [`NO_REGISTER`] ends the list.
pub(crate) unsafe extern "C" fn jit_push(state: *mut JitState, a: u32, b: u32, c: u32) -> u32 {
    guarded_moving(state, |context, _, rp| {
        for src in [a, b, c] {
            if src == NO_REGISTER {
                break;
            }
            let value = register(context, rp, src).clone();
            context.vm.stack.push(value);
        }
        Some(())
    })
}

/// `PopIntoRegister`, when there is a value above the registers to pop.
pub(crate) unsafe extern "C" fn jit_pop(state: *mut JitState, dst: u32, _: u32, _: u32) -> u32 {
    guarded(state, |context, code_block, rp| {
        let stack = &mut context.vm.stack.stack;
        if stack.len() <= rp + code_block.register_count as usize {
            return None;
        }
        let value = stack.pop()?;
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `SetAccumulator`.
pub(crate) unsafe extern "C" fn jit_set_accumulator(
    state: *mut JitState,
    src: u32,
    _: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let value = register(context, rp, src).clone();
        context.vm.set_return_value(value);
        Some(())
    })
}

/// `GetArgument`.
pub(crate) unsafe extern "C" fn jit_get_argument(
    state: *mut JitState,
    index: u32,
    dst: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let value = context
            .vm
            .stack
            .get_argument(context.vm.frame(), index as usize)
            .cloned()
            .unwrap_or_default();
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `CheckReturn` of a frame that is not a constructor's, which does nothing.
pub(crate) unsafe extern "C" fn jit_check_return(
    state: *mut JitState,
    _: u32,
    _: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, _, _| {
        (!context.vm.frame().construct()).then_some(())
    })
}

/// The value of the data property `ic` caches for `object`, if the cache has its shape.
/// Accessors are the slow path's: they call.
fn cached_data_property(object: &JsValue, code_block: &CodeBlock, ic: u32) -> Option<JsValue> {
    let ic = code_block.ic.get(ic as usize)?;
    object.with_object(|object| {
        let borrowed = object.try_borrow().ok()?;
        let slot = ic.slot_for(borrowed.shape())?;
        if slot.attributes.is_accessor_descriptor() {
            return None;
        }
        let index = slot.index as usize;
        if slot.attributes.contains(SlotAttributes::PROTOTYPE) {
            let prototype = borrowed.shape().prototype()?;
            let prototype = prototype.try_borrow().ok()?;
            prototype.properties().storage.get(index).cloned()
        } else {
            borrowed.properties().storage.get(index).cloned()
        }
    })?
}

/// `GetPropertyByName`: an object whose shape the inline cache has, for a data property.
pub(crate) unsafe extern "C" fn jit_get_property(
    state: *mut JitState,
    dst: u32,
    object: u32,
    ic: u32,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let value = cached_data_property(register(context, rp, object), code_block, ic)?;
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `GetLengthProperty`: of an array, a string, or as `GetPropertyByName`.
pub(crate) unsafe extern "C" fn jit_get_length(
    state: *mut JitState,
    dst: u32,
    object: u32,
    ic: u32,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let base = register(context, rp, object);
        let value = if base.with_object(JsObject::is_array) == Some(true) {
            base.with_object(|array| {
                array
                    .try_borrow()
                    .ok()?
                    .properties()
                    .storage
                    .first()
                    .cloned()
            })??
        } else if let Some(string) = base.as_string() {
            JsValue::from(string.len() as u32)
        } else {
            cached_data_property(base, code_block, ic)?
        };
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `SetPropertyByName`: an object whose shape the inline cache has, for its own data property.
pub(crate) unsafe extern "C" fn jit_set_property(
    state: *mut JitState,
    value: u32,
    object: u32,
    ic: u32,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let ic = code_block.ic.get(ic as usize)?;
        let value = register(context, rp, value).clone();
        register(context, rp, object).with_object(|object| {
            let mut borrowed = object.try_borrow_mut().ok()?;
            let slot = ic.slot_for(borrowed.shape())?;
            if slot.attributes.is_accessor_descriptor()
                || slot.attributes.contains(SlotAttributes::PROTOTYPE)
            {
                return None;
            }
            *borrowed
                .properties_mut()
                .storage
                .get_mut(slot.index as usize)? = value;
            Some(())
        })?
    })
}

/// An `int32` key that is an array index.
fn index_key(key: &JsValue) -> Option<u32> {
    u32::try_from(key.as_integer32()?).ok()
}

/// `GetPropertyByValue`: an element of a dense array, by an `int32` index.
pub(crate) unsafe extern "C" fn jit_get_element(
    state: *mut JitState,
    dst: u32,
    key: u32,
    object: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let index = index_key(register(context, rp, key))?;
        let value = register(context, rp, object).with_object(|object| {
            if !object.is_array() {
                return None;
            }
            object
                .try_borrow()
                .ok()?
                .properties()
                .get_dense_property(index)
        })??;
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `SetPropertyByValue`: an element of a dense, extensible array, by an `int32` index.
pub(crate) unsafe extern "C" fn jit_set_element(
    state: *mut JitState,
    value: u32,
    key: u32,
    object: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let index = index_key(register(context, rp, key))?;
        let value = register(context, rp, value);
        register(context, rp, object).with_object(|object| {
            if !object.is_array() {
                return None;
            }
            let mut borrowed = object.try_borrow_mut().ok()?;
            if !borrowed.extensible {
                return None;
            }
            borrowed
                .properties_mut()
                .set_dense_property(index, value)
                .then_some(())
        })?
    })
}
