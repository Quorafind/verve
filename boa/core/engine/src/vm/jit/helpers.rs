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

use boa_ast::scope::BindingLocatorScope;

use crate::{
    Context, JsObject, JsValue,
    builtins::{
        Array,
        typed_array::object::{
            TypedArray, typed_array_element_get_shortcut, typed_array_element_set_shortcut,
        },
    },
    environments::Environment,
    object::shape::slot::SlotAttributes,
    vm::CodeBlock,
};

use super::{
    layout::{SiteCache, object_layout},
    runtime::JitState,
};

/// The fast path applied.
pub(crate) const DONE: u32 = 0;
/// The fast path does not apply: run the instruction with `jit_step`.
pub(crate) const SLOW: u32 = 1;

/// The signature of every fast helper: the state and up to three operands.
pub(crate) type HelperFn = unsafe extern "C" fn(*mut JitState, u32, u32, u32) -> u32;

/// A fast helper that also fills the cache of the access compiled inline.
pub(crate) type SiteHelperFn =
    unsafe extern "C" fn(*mut JitState, u32, u32, u32, *const SiteCache) -> u32;

/// Remembers in `site` that `object`'s own data property at `slot` was accessed.
fn fill(site: *const SiteCache, object: &JsObject, slot: u32) {
    if let Some(layout) = object_layout() {
        // SAFETY: compiled code passes one of its own sites, which it keeps alive.
        unsafe { (*site).fill(object, slot, &layout) };
    }
}

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

/// `GetName` of a binding the interpreter finds where the bytecode compiler put it: in a
/// declarative environment on the frame's chain, or the global declarative one, with no
/// `with`, no binding an `eval` may have added, and not deleted. The binding must be
/// initialized; anything else is the slow path's (it may throw).
pub(crate) unsafe extern "C" fn jit_get_name(
    state: *mut JitState,
    dst: u32,
    index: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let value = binding_value(context, code_block, index, None)?;
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `GetNameGlobal`: as `GetName`, and a binding on the global object is read through the
/// instruction's inline cache, as the interpreter reads it, when it is a data property.
pub(crate) unsafe extern "C" fn jit_get_name_global(
    state: *mut JitState,
    dst: u32,
    index: u32,
    ic: u32,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let value = binding_value(context, code_block, index, Some(ic))?;
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// The value of binding `index` where `find_runtime_binding` leaves it and `get_binding`
/// reads it, when nothing can have moved it: the frame's current environment is declarative,
/// with no `with` and no binding an `eval` may have added, and the binding is not deleted. A
/// binding on the global object is read only through `ic` (for `GetNameGlobal`).
fn binding_value(
    context: &Context,
    code_block: &CodeBlock,
    index: u32,
    ic: Option<u32>,
) -> Option<JsValue> {
    let locator = code_block.bindings.get(index as usize)?;
    if context.is_deleted_binding(locator) {
        return None;
    }
    let frame = context.vm.frame();
    let global = frame.realm.environment();
    let current = frame.environments.current_declarative_ref(global)?;
    if current.with() || current.poisoned() {
        return None;
    }
    match locator.scope() {
        BindingLocatorScope::GlobalDeclarative => global.get(locator.binding_index()),
        BindingLocatorScope::Stack(environment) => match context.environment_expect(environment) {
            Environment::Declarative(environment) => environment.get(locator.binding_index()),
            Environment::Object(_) => None,
        },
        BindingLocatorScope::GlobalObject => {
            let global_object = JsValue::from(frame.realm.global_object().clone());
            cached_data_property(&global_object, code_block, ic?, None)
        }
    }
}

/// The value of the data property `ic` caches for `object`, if the cache has its shape.
/// Accessors are the slow path's: they call. An own property is remembered in `site`, if
/// there is one.
fn cached_data_property(
    object: &JsValue,
    code_block: &CodeBlock,
    ic: u32,
    site: Option<*const SiteCache>,
) -> Option<JsValue> {
    let ic = code_block.ic.get(ic as usize)?;
    object.with_object(|object| {
        let borrowed = object.try_borrow().ok()?;
        let (slot, value) = ic.slot_for(borrowed.shape(), borrowed.properties(), false)?;
        if slot.attributes.is_accessor_descriptor() {
            return None;
        }
        if let Some(site) = site
            && !slot.attributes.contains(SlotAttributes::PROTOTYPE)
        {
            fill(site, object, slot.index);
        }
        Some(value)
    })?
}

/// `GetPropertyByName`: an object whose shape the inline cache has, for a data property.
pub(crate) unsafe extern "C" fn jit_get_property(
    state: *mut JitState,
    dst: u32,
    object: u32,
    ic: u32,
    site: *const SiteCache,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let value =
            cached_data_property(register(context, rp, object), code_block, ic, Some(site))?;
        set_register(context, rp, dst, value);
        Some(())
    })
}

/// `GetLengthProperty`: of an array, a string, a typed array (through the site's cache of the
/// guard), or as `GetPropertyByName`.
pub(crate) unsafe extern "C" fn jit_get_length(
    state: *mut JitState,
    dst: u32,
    object: u32,
    ic: u32,
    site: *const SiteCache,
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
        } else if let Some(length) = base
            .with_object(|object| {
                // SAFETY: compiled code passes one of its own sites, which it keeps alive.
                let site = unsafe { &*site };
                site.typed_array_length(object)
            })
            .flatten()
        {
            length
        } else {
            cached_data_property(base, code_block, ic, None)?
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
    site: *const SiteCache,
) -> u32 {
    guarded(state, |context, code_block, rp| {
        let ic = code_block.ic.get(ic as usize)?;
        let value = register(context, rp, value).clone();
        register(context, rp, object).with_object(|object| {
            let mut borrowed = object.try_borrow_mut().ok()?;
            let Some((slot, _)) = ic.slot_for(borrowed.shape(), borrowed.properties(), true) else {
                drop(borrowed);
                return ic.adds.add(object, &value).then_some(());
            };
            if slot.attributes.is_accessor_descriptor()
                || slot.attributes.contains(SlotAttributes::PROTOTYPE)
            {
                return None;
            }
            *borrowed
                .properties_mut()
                .storage
                .get_mut(slot.index as usize)? = value;
            drop(borrowed);
            fill(site, object, slot.index);
            Some(())
        })?
    })
}

/// `IteratorNext` of a built-in iterator that can be stepped without running JavaScript.
pub(crate) unsafe extern "C" fn jit_iterator_next(
    state: *mut JitState,
    _: u32,
    _: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, _, _| {
        // The record is taken out while it is stepped, as the interpreter's handler does.
        let mut record = context.vm.frame_mut().iterators.pop()?;
        let stepped = record.step_quietly(context);
        context.vm.frame_mut().iterators.push(record);
        stepped.ok()
    })
}

/// `IteratorDone`: the `done` of the current iterator record.
pub(crate) unsafe extern "C" fn jit_iterator_done(
    state: *mut JitState,
    dst: u32,
    _: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let done = context.vm.frame().iterators.last()?.done();
        set_register(context, rp, dst, done.into());
        Some(())
    })
}

/// `IteratorValue` of a record whose last step was made without a result object.
pub(crate) unsafe extern "C" fn jit_iterator_value(
    state: *mut JitState,
    dst: u32,
    _: u32,
    _: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let value = context
            .vm
            .frame()
            .iterators
            .last()?
            .unboxed_value()?
            .clone();
        set_register(context, rp, dst, value);
        Some(())
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
                return typed_array_element_get_shortcut(object, index);
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

/// `SetPropertyByValue`: an element of a dense, extensible array, by an `int32` index, or one
/// appended to a plain dense array, or one of a typed array (as the interpreter's shortcut
/// has it). The receiver is the object: compiled code leaves other stores to the interpreter.
pub(crate) unsafe extern "C" fn jit_set_element(
    state: *mut JitState,
    value: u32,
    key: u32,
    object: u32,
) -> u32 {
    guarded(state, |context, _, rp| {
        let index = index_key(register(context, rp, key))?;
        let value = register(context, rp, value).clone();
        let object = register(context, rp, object).as_object()?.clone();
        if object.is::<TypedArray>() {
            return typed_array_element_set_shortcut(&object, index, &value, context)?.ok();
        }
        if !object.is_array() {
            return None;
        }
        {
            let mut borrowed = object.try_borrow_mut().ok()?;
            if !borrowed.extensible {
                return None;
            }
            if borrowed.properties_mut().set_dense_property(index, &value) {
                return Some(());
            }
        }
        append_element(context, &object, index, &value)
    })
}

/// `Set` of the element just past a plain dense array's elements: no element is on the
/// prototype chain, so the element is defined there, and `length` grows to cover it if it does
/// not already (`Array`'s `[[DefineOwnProperty]]`; the array shape has a writable `length`).
/// Changes nothing and answers `None` for any other array, index or `length`.
fn append_element(context: &Context, object: &JsObject, index: u32, value: &JsValue) -> Option<()> {
    let (mut borrowed, len, elements) = Array::plain_dense_array(object, context)?;
    // `length` is an array index, so `index + 1` is a valid one.
    if index as usize != elements || len < elements as f64 || index == u32::MAX {
        return None;
    }
    let properties = borrowed.properties_mut();
    if !properties.indexed_properties.push_dense(value) {
        return None;
    }
    if len.to_bits() == f64::from(index).to_bits() {
        // NOTE: The "length" property is the first slot.
        properties.storage[0] = JsValue::new(u64::from(index) + 1);
    }
    Some(())
}
