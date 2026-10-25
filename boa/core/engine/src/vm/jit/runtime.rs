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
    mem::offset_of,
    ops::ControlFlow,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    ptr,
    sync::atomic::AtomicBool,
};

use crate::{
    Context, JsValue,
    vm::{CallFrame, CodeBlock, CompletionRecord, Vm, opcode::OPCODE_HANDLERS},
};

use super::JitCode;

/// What a helper returns when compiled code must return to the interpreter.
pub(crate) const EXIT: u32 = u32::MAX;

/// The signature of a compiled function: the state, and the index of the block to start at.
pub(crate) type EntryFn = unsafe extern "C" fn(*mut JitState, u32) -> u32;

/// The state compiled code runs with.
///
/// Compiled code reads the first four fields directly; the rest only the helpers touch.
#[repr(C)]
pub(crate) struct JitState {
    /// The frame's first register, in the VM stack. Helpers update it when the stack moves.
    pub(crate) regs: *mut JsValue,
    /// The frame, in the VM's frames. Helpers update it when the frames move.
    frame: *mut CallFrame,
    /// The flag another thread sets to interrupt the run, or null if there is none.
    interrupt: *const AtomicBool,
    /// `RuntimeLimits::loop_iteration_limit`.
    loop_limit: u64,
    pub(super) context: *mut Context,
    code: *const JitCode,
    pub(super) code_block: *const CodeBlock,
    frames_len: usize,
    pub(super) rp: u32,
    /// How to go on once compiled code returns; `None` is to go on interpreting.
    outcome: Option<ControlFlow<CompletionRecord>>,
    pub(super) panic: Option<Box<dyn Any + Send + 'static>>,
}

/// The offsets of what compiled code reads in [`JitState`] and [`CallFrame`].
pub(crate) const REGS_OFFSET: i32 = offset_of!(JitState, regs) as i32;
pub(crate) const FRAME_OFFSET: i32 = offset_of!(JitState, frame) as i32;
pub(crate) const INTERRUPT_OFFSET: i32 = offset_of!(JitState, interrupt) as i32;
pub(crate) const LOOP_LIMIT_OFFSET: i32 = offset_of!(JitState, loop_limit) as i32;
pub(crate) const LOOP_COUNT_OFFSET: i32 = offset_of!(CallFrame, loop_iteration_count) as i32;
const _: () = assert!(size_of::<u64>() == size_of::<*const ()>());
// Compiled code reads and writes registers as the 64-bit words of NaN-boxed values.
const _: () = assert!(size_of::<JsValue>() == 8);

/// Runs compiled code for the frame on top, from the block `block`.
pub(crate) fn execute(
    context: &mut Context,
    code: &JitCode,
    block: u32,
) -> ControlFlow<CompletionRecord> {
    context.vm.jit.stats.entries += 1;
    let frame = context.vm.frame();
    let rp = frame.rp;
    let code_block: *const CodeBlock = &raw const **frame.code_block();
    let frames_len = context.vm.frames.len();
    // SAFETY: the frame's registers are on the stack, from `rp` on.
    let regs = unsafe { context.vm.stack.stack.as_mut_ptr().add(rp as usize) };
    let (frame, interrupt, loop_limit) = frame_state(&mut context.vm);
    let mut state = JitState {
        regs,
        frame,
        interrupt,
        loop_limit,
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

/// The signature of [`jit_step`]: the state and the instruction's `pc`.
pub(crate) type StepFn = unsafe extern "C" fn(*mut JitState, u32) -> u32;

/// Runs the instruction at `pc`, whose opcode is `OP`, with the interpreter's handler. Returns
/// the `pc` to go on at, or [`EXIT`] if compiled code must return (the frame changed, or the
/// run is over). Compiled code calls the instance for the opcode it knows ([`STEPS`]), so the
/// handler is called directly, with no decoding of the opcode or table.
pub(crate) unsafe extern "C" fn jit_step<const OP: usize>(state: *mut JitState, pc: u32) -> u32 {
    // SAFETY: compiled code passes the state `execute` made, which outlives it.
    let state = unsafe { &mut *state };
    // A fast helper panicked and went to the slow path: leave, and let `execute` resume it.
    if state.panic.is_some() {
        return EXIT;
    }
    match catch_unwind(AssertUnwindSafe(|| step(state, pc, OPCODE_HANDLERS[OP]))) {
        Ok(next) => next,
        Err(payload) => {
            state.panic = Some(payload);
            EXIT
        }
    }
}

macro_rules! steps {
    ($($op:literal)*) => { [$(jit_step::<$op>),*] };
}

/// [`jit_step`] for each opcode.
pub(crate) static STEPS: [StepFn; 256] = steps!(
    0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
    32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61
    62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79 80 81 82 83 84 85 86 87 88 89 90 91
    92 93 94 95 96 97 98 99 100 101 102 103 104 105 106 107 108 109 110 111 112 113 114 115
    116 117 118 119 120 121 122 123 124 125 126 127 128 129 130 131 132 133 134 135 136 137
    138 139 140 141 142 143 144 145 146 147 148 149 150 151 152 153 154 155 156 157 158 159
    160 161 162 163 164 165 166 167 168 169 170 171 172 173 174 175 176 177 178 179 180 181
    182 183 184 185 186 187 188 189 190 191 192 193 194 195 196 197 198 199 200 201 202 203
    204 205 206 207 208 209 210 211 212 213 214 215 216 217 218 219 220 221 222 223 224 225
    226 227 228 229 230 231 232 233 234 235 236 237 238 239 240 241 242 243 244 245 246 247
    248 249 250 251 252 253 254 255
);

/// How deep compiled functions may nest on the native stack, calling one another without
/// returning to the interpreter's loop. Past it a call returns to the loop, as in phase 1.
pub(crate) const MAX_NESTED: u32 = 64;

// Inlined into each `jit_step::<OP>`, so that its handler is a direct call.
#[allow(
    clippy::inline_always,
    reason = "the handler must be a direct call in each instance"
)]
#[inline(always)]
fn step(
    state: &mut JitState,
    pc: u32,
    handler: fn(&mut Context, usize) -> ControlFlow<CompletionRecord>,
) -> u32 {
    // SAFETY: the context outlives compiled code, which uses it only through helpers.
    let context = unsafe { &mut *state.context };
    if let ControlFlow::Break(record) = handler(context, pc as usize) {
        state.outcome = Some(ControlFlow::Break(record));
        return EXIT;
    }
    if context.vm.frames.len() > state.frames_len && !run_callees(state, context) {
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
    // The stack and the frames may have grown, and moved.
    // SAFETY: the frame's registers are on the stack, from `rp` on.
    state.regs = unsafe { vm.stack.stack.as_mut_ptr().add(state.rp as usize) };
    (state.frame, state.interrupt, state.loop_limit) = frame_state(vm);
    next
}

/// The instruction just run pushed frames above this one (a call): runs them in their compiled
/// code, nested on the native stack, until this frame is back on top. Returns `false` if that
/// cannot be done — a callee is not compiled, the nesting is too deep, or the run ended — and
/// compiled code must return to the interpreter's loop, which carries on from the frames as
/// they are.
#[inline(never)]
fn run_callees(state: &mut JitState, context: &mut Context) -> bool {
    loop {
        let frames = context.vm.frames.len();
        if frames <= state.frames_len {
            return frames == state.frames_len;
        }
        let vm = &context.vm;
        if vm.jit.nested >= MAX_NESTED
            || vm
                .native_stack_base
                .saturating_sub(crate::vm::native_stack_position())
                > vm.runtime_limits.native_stack_limit() / 4
        {
            return false;
        }
        let Some((code, block)) = super::prepare(context) else {
            return false;
        };
        let flow = {
            let _depth = Nesting::enter(&raw mut context.vm.jit.nested);
            // A panic in the callee is resumed by `execute`, and caught by this helper's caller.
            execute(context, &code, block)
        };
        if let ControlFlow::Break(record) = flow {
            state.outcome = Some(ControlFlow::Break(record));
            return false;
        }
    }
}

/// Counts one level of nesting while it lives, panics included.
struct Nesting(*mut u32);

impl Nesting {
    fn enter(nested: *mut u32) -> Self {
        // SAFETY: `nested` is the VM's counter, which outlives the call it counts.
        unsafe { *nested += 1 };
        Self(nested)
    }
}

impl Drop for Nesting {
    fn drop(&mut self) {
        // SAFETY: as in `enter`.
        unsafe { *self.0 -= 1 };
    }
}

/// The frame on top, the interrupt flag, and the loop limit, for compiled code.
fn frame_state(vm: &mut Vm) -> (*mut CallFrame, *const AtomicBool, u64) {
    let frame = ptr::from_mut(vm.frame_mut());
    let interrupt = vm.interrupt.as_deref().map_or(ptr::null(), ptr::from_ref);
    (frame, interrupt, vm.runtime_limits.loop_iteration_limit())
}

/// The block at `pc`, or [`EXIT`] if `pc` starts none; the frame's `pc` is already `pc`.
pub(crate) unsafe extern "C" fn jit_lookup(state: *mut JitState, pc: u32) -> u32 {
    // SAFETY: as in `jit_step`; the code outlives its run.
    let code = unsafe { &*(*state).code };
    code.block_for_pc(pc).unwrap_or(EXIT)
}

/// Stores `bits` in register `index`, dropping what was there. `bits` is a value without a
/// pointer, or one whose reference compiled code owns (it counted it) and hands over.
pub(crate) unsafe extern "C" fn jit_store(state: *mut JitState, index: u32, bits: u64) {
    // SAFETY: compiled code stores only to the frame's registers, and only values that own
    // nothing or own the reference they hold.
    unsafe {
        let slot = (*state).regs.add(index as usize);
        *slot = JsValue::from_raw_bits(bits);
    }
}
