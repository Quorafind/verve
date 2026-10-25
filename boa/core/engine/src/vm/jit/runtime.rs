//! What compiled code runs with: the state it is handed, and the helpers it calls.
//!
//! Compiled code works on the interpreter's own registers, in place, in the VM stack. So at
//! every instruction boundary the frame is exactly what the interpreter would have, and leaving
//! compiled code for the interpreter needs no translation: the interpreter carries on at the
//! frame's `pc`.
//!
//! Anything compiled code does not do inline it hands to [`jit_step`], which runs that one
//! instruction with the interpreter's own handler. If the instruction leaves the frame — a call
//! pushed a callee, a return or a throw popped it — compiled code returns to the interpreter's
//! loop, which runs the new frame (compiled or not) and re-enters this code when the frame is
//! back on top.
//!
//! No helper lets a panic unwind into compiled code (there is no unwind information for it):
//! each catches it, compiled code returns, and [`execute`] resumes the panic.

use std::{
    any::Any,
    ops::ControlFlow,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    ptr,
};

use crate::{
    Context, JsValue,
    vm::{CodeBlock, CompletionRecord, opcode::OPCODE_HANDLERS, opcode::Opcode},
};

use super::JitCode;

/// What a helper returns when compiled code must return to the interpreter.
pub(crate) const EXIT: u32 = u32::MAX;

/// The signature of a compiled function: the state, and the index of the block to start at.
pub(crate) type EntryFn = unsafe extern "C" fn(*mut JitState, u32) -> u32;

/// The state compiled code runs with.
///
/// Compiled code reads `regs` (offset 0) directly; everything else only the helpers touch.
#[repr(C)]
pub(crate) struct JitState {
    /// The frame's first register, in the VM stack. Helpers update it when the stack moves.
    pub(crate) regs: *mut JsValue,
    context: *mut Context,
    code: *const JitCode,
    code_block: *const CodeBlock,
    frames_len: usize,
    rp: u32,
    /// How to go on once compiled code returns; `None` is to go on interpreting.
    outcome: Option<ControlFlow<CompletionRecord>>,
    panic: Option<Box<dyn Any + Send + 'static>>,
}

/// The offset of [`JitState::regs`], for compiled code.
pub(crate) const REGS_OFFSET: i32 = 0;
const _: () = assert!(std::mem::offset_of!(JitState, regs) == REGS_OFFSET as usize);
// Compiled code reads and writes registers as the 64-bit words of NaN-boxed values.
const _: () = assert!(size_of::<JsValue>() == 8);

/// Runs compiled code for the frame on top, from the block `block`.
pub(crate) fn execute(context: &mut Context, code: &JitCode, block: u32) -> ControlFlow<CompletionRecord> {
    context.vm.jit.stats.entries += 1;
    let frame = context.vm.frame();
    let rp = frame.rp;
    let code_block: *const CodeBlock = &raw const **frame.code_block();
    let frames_len = context.vm.frames.len();
    // SAFETY: the frame's registers are on the stack, from `rp` on.
    let regs = unsafe { context.vm.stack.stack.as_mut_ptr().add(rp as usize) };
    let mut state = JitState {
        regs,
        context: &raw mut *context,
        code: &raw const *code,
        code_block,
        frames_len,
        rp,
        outcome: None,
        panic: None,
    };
    // SAFETY: `code` was compiled for this frame's code block, `block` is one of its blocks,
    // and nothing uses `context` but compiled code and its helpers until it returns.
    unsafe { (code.entry())(&raw mut state, block) };
    if let Some(payload) = state.panic.take() {
        resume_unwind(payload);
    }
    state.outcome.take().unwrap_or(ControlFlow::Continue(()))
}

/// Runs the instruction at `pc` with the interpreter's handler. Returns the `pc` to go on at,
/// or [`EXIT`] if compiled code must return (the frame changed, or the run is over).
pub(crate) unsafe extern "C" fn jit_step(state: *mut JitState, pc: u32) -> u32 {
    // SAFETY: compiled code passes the state `execute` made, which outlives it.
    let state = unsafe { &mut *state };
    match catch_unwind(AssertUnwindSafe(|| step(state, pc))) {
        Ok(next) => next,
        Err(payload) => {
            state.panic = Some(payload);
            EXIT
        }
    }
}

fn step(state: &mut JitState, pc: u32) -> u32 {
    // SAFETY: the context outlives compiled code, which uses it only through helpers.
    let context = unsafe { &mut *state.context };
    let opcode = Opcode::decode(context.vm.frame().code_block.bytecode.bytes[pc as usize]);
    if let ControlFlow::Break(record) = OPCODE_HANDLERS[opcode as usize](context, pc as usize) {
        state.outcome = Some(ControlFlow::Break(record));
        return EXIT;
    }
    let vm = &mut context.vm;
    if vm.frames.len() != state.frames_len {
        return EXIT;
    }
    let frame = vm.frame();
    if frame.rp != state.rp || !ptr::eq(&raw const **frame.code_block(), state.code_block) {
        return EXIT;
    }
    let next = frame.pc;
    // The stack may have grown, and moved.
    // SAFETY: the frame's registers are on the stack, from `rp` on.
    state.regs = unsafe { vm.stack.stack.as_mut_ptr().add(state.rp as usize) };
    next
}

/// The block at `pc`, or [`EXIT`] if `pc` starts none; the frame's `pc` is already `pc`.
pub(crate) unsafe extern "C" fn jit_lookup(state: *mut JitState, pc: u32) -> u32 {
    // SAFETY: as in `jit_step`; the code outlives its run.
    let code = unsafe { &*(*state).code };
    code.block_for_pc(pc).unwrap_or(EXIT)
}

/// Stores `bits`, a value that is not a pointer, in register `index`, dropping what was there.
pub(crate) unsafe extern "C" fn jit_store(state: *mut JitState, index: u32, bits: u64) {
    // SAFETY: compiled code stores only to the frame's registers, and only values without a
    // pointer, which own nothing.
    unsafe {
        let slot = (*state).regs.add(index as usize);
        *slot = JsValue::from_raw_bits(bits);
    }
}
