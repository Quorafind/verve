//! A baseline JIT: hot bytecode functions compiled to native code with Cranelift.
//!
//! The design is that of a baseline ("Sparkplug"-style) compiler. Compiled code keeps the
//! interpreter's frame exactly: the same [`CallFrame`](crate::vm::CallFrame), the same
//! registers in the VM stack, the same `pc` at every helper call. So
//!
//! - an instruction compiled code does not do inline is run by the interpreter's own handler
//!   ([`runtime::jit_step`]), with the same result to the bit;
//! - leaving compiled code for the interpreter, at any instruction, is just returning — there is
//!   nothing to deoptimise;
//! - calls, returns, exceptions, the shadow stack, generators and the garbage collector all work
//!   as they do for the interpreter, because compiled code never makes a frame of its own: a call
//!   returns to the interpreter's loop, which pushes the callee and enters *its* compiled code,
//!   and re-enters the caller's when the callee returns. Native recursion does not grow with
//!   JavaScript recursion.
//!
//! What compiled code does inline is where the interpreter spends its time on plain code:
//! moving and storing registers, `int32` and `f64` arithmetic and comparisons, and branches,
//! with no dispatch between instructions. See `docs/jit.md` in Verve for the plan beyond this.
//!
//! The JIT is off unless the `jit` feature is built, and then can be switched off at run time
//! with [`JitOptions::enabled`] (or `BOA_JIT=0` in the environment).

#[cfg(any(not(target_pointer_width = "64"), feature = "jsvalue-enum"))]
compile_error!("the `jit` feature needs NaN-boxed values on a 64-bit target");

mod compiler;
mod memory;
mod runtime;

#[cfg(test)]
mod tests;

use std::{
    cell::{Cell, RefCell},
    fmt,
    ops::ControlFlow,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

use crate::{
    Context,
    vm::{CodeBlock, CompletionRecord},
};

use self::{
    compiler::CompileError,
    memory::{ExecutableCode, LiveCode},
    runtime::EntryFn,
};

/// How the JIT behaves. Set it with [`Context::jit_options_mut`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitOptions {
    /// Whether hot functions are compiled, and compiled code runs.
    pub enabled: bool,
    /// How hot a function gets before it is compiled: one for each call, return to it and loop
    /// iteration in it. `0` compiles a function the first time it runs.
    pub threshold: u32,
    /// Functions with more bytecode than this, in bytes, are never compiled.
    pub max_bytecode_len: usize,
    /// How much machine code, in bytes rounded up to pages, may be mapped at once. Code is
    /// unmapped when its function is collected, which gives the bytes back.
    pub max_live_code_bytes: usize,
    /// How much machine code, in bytes rounded up to pages, a context may compile in its
    /// lifetime, given back or not: a bound on the time and memory compiling can take.
    pub max_total_code_bytes: usize,
    /// How many functions a context may compile in its lifetime.
    pub max_compiled_functions: u64,
}

impl Default for JitOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 1000,
            max_bytecode_len: 32 * 1024,
            max_live_code_bytes: 64 * 1024 * 1024,
            max_total_code_bytes: 256 * 1024 * 1024,
            max_compiled_functions: 10_000,
        }
    }
}

impl JitOptions {
    /// The defaults, with what the environment overrides: `BOA_JIT=0` (or `off`, `false`)
    /// disables the JIT, `BOA_JIT_THRESHOLD=<n>` sets [`JitOptions::threshold`].
    #[must_use]
    pub fn from_env() -> Self {
        let mut options = Self::default();
        if let Ok(value) = std::env::var("BOA_JIT") {
            options.enabled = !matches!(value.trim(), "0" | "off" | "false" | "no");
        }
        if let Some(threshold) = std::env::var("BOA_JIT_THRESHOLD")
            .ok()
            .and_then(|value| value.trim().parse().ok())
        {
            options.threshold = threshold;
        }
        options
    }
}

/// What the JIT has done in a context, from [`Context::jit_stats`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitStats {
    /// Functions compiled.
    pub compiled: u64,
    /// Functions found hot that could not be compiled (generators, async functions, functions
    /// too large, …); they stay interpreted.
    pub rejected: u64,
    /// Times compiled code was entered.
    pub entries: u64,
    /// Bytes of machine code compiled in the context's lifetime, rounded up to pages.
    pub code_bytes: usize,
    /// Bytes of machine code mapped now, rounded up to pages.
    pub live_code_bytes: usize,
    /// Functions whose machine code is mapped now.
    pub live_functions: usize,
    /// Time spent compiling.
    pub compile_time: Duration,
}

/// Why the JIT is off for the whole process, whatever the options say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitUnavailable {
    /// The code generator panicked once; it is not trusted again.
    CompilerPanicked,
    /// The host refused executable memory (a policy such as Windows' Arbitrary Code Guard, or
    /// macOS' hardened runtime without the entitlement), or Cranelift has no backend for it.
    NoExecutableMemory,
}

/// Why the JIT is off for the whole process, if it is. Once it is, it stays off: compiling
/// would fail the same way again, at the cost of a full compile each time.
#[must_use]
pub fn jit_unavailable() -> Option<JitUnavailable> {
    match latch::get() {
        latch::COMPILER_PANICKED => Some(JitUnavailable::CompilerPanicked),
        latch::NO_EXECUTABLE_MEMORY => Some(JitUnavailable::NoExecutableMemory),
        _ => None,
    }
}

/// The process-wide switch behind [`jit_unavailable`]. Tests keep it per thread, so a test that
/// trips it does not switch the JIT off for the tests running beside it.
mod latch {
    pub(super) const AVAILABLE: u8 = 0;
    pub(super) const COMPILER_PANICKED: u8 = 1;
    pub(super) const NO_EXECUTABLE_MEMORY: u8 = 2;

    #[cfg(not(test))]
    static LATCH: super::AtomicU8 = super::AtomicU8::new(AVAILABLE);

    #[cfg(not(test))]
    pub(super) fn get() -> u8 {
        LATCH.load(super::Ordering::Relaxed)
    }

    /// Switches the JIT off for good, keeping the first reason.
    #[cfg(not(test))]
    pub(super) fn set(reason: u8) {
        let _ = LATCH.compare_exchange(
            AVAILABLE,
            reason,
            super::Ordering::Relaxed,
            super::Ordering::Relaxed,
        );
    }

    #[cfg(test)]
    thread_local! {
        static LATCH: std::cell::Cell<u8> = const { std::cell::Cell::new(AVAILABLE) };
    }

    #[cfg(test)]
    pub(super) fn get() -> u8 {
        LATCH.with(std::cell::Cell::get)
    }

    #[cfg(test)]
    pub(super) fn set(reason: u8) {
        LATCH.with(|latch| {
            if latch.get() == AVAILABLE {
                latch.set(reason);
            }
        });
    }

    #[cfg(test)]
    pub(super) fn reset() {
        LATCH.with(|latch| latch.set(AVAILABLE));
    }
}

/// The JIT's state in a [`Vm`](crate::vm::Vm).
#[derive(Debug)]
pub(crate) struct JitVm {
    pub(crate) options: JitOptions,
    pub(crate) stats: JitStats,
    /// Set when the frame on top may have changed or got hotter, so the interpreter's loop
    /// asks [`try_enter`] whether to run compiled code.
    pub(crate) pending: bool,
    /// The machine code the context's functions have mapped now; each function's code gives
    /// its pages back here when it is dropped.
    live: Rc<LiveCode>,
}

impl JitVm {
    pub(crate) fn new() -> Self {
        Self {
            options: JitOptions::from_env(),
            stats: JitStats::default(),
            pending: false,
            live: Rc::default(),
        }
    }

    /// Whether the limits leave room for another function.
    fn has_room(&self) -> bool {
        let options = &self.options;
        self.stats.compiled < options.max_compiled_functions
            && self.stats.code_bytes < options.max_total_code_bytes
            && self.live.bytes() < options.max_live_code_bytes
    }
}

/// Compiled code for a code block, and where it can be entered.
pub(crate) struct JitCode {
    code: ExecutableCode,
    /// The `pc` each block starts at, in order: a block's index is its position here.
    block_pcs: Box<[u32]>,
}

impl fmt::Debug for JitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JitCode")
            .field("code", &self.code)
            .field("blocks", &self.block_pcs.len())
            .finish()
    }
}

impl JitCode {
    fn entry(&self) -> EntryFn {
        // SAFETY: the code starts with the compiled function, whose signature is `EntryFn`.
        unsafe { std::mem::transmute::<*const u8, EntryFn>(self.code.as_ptr()) }
    }

    /// The index of the block that starts at `pc`, if one does.
    pub(crate) fn block_for_pc(&self, pc: u32) -> Option<u32> {
        self.block_pcs
            .binary_search(&pc)
            .ok()
            .map(|index| index as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Interpreted,
    Compiled,
    Rejected,
}

/// A code block's JIT state: how hot it is, and its compiled code.
pub(crate) struct JitSlot {
    hotness: Cell<u32>,
    tier: Cell<Tier>,
    code: RefCell<Option<Rc<JitCode>>>,
}

impl Default for JitSlot {
    fn default() -> Self {
        Self {
            hotness: Cell::new(0),
            tier: Cell::new(Tier::Interpreted),
            code: RefCell::new(None),
        }
    }
}

impl Clone for JitSlot {
    /// A copy of a code block starts cold.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl fmt::Debug for JitSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JitSlot")
            .field("hotness", &self.hotness.get())
            .field("tier", &self.tier.get())
            .finish_non_exhaustive()
    }
}

impl JitSlot {
    /// Whether the loop counter should ask [`try_enter`] about this block: it may yet be
    /// compiled, or it is and could be entered.
    pub(crate) fn wants_entry(&self) -> bool {
        self.tier.get() != Tier::Rejected
    }
}

/// Called by the interpreter's loop when [`JitVm::pending`] is set: counts the frame on top
/// hotter, compiles it when it is hot enough, and runs its compiled code if there is a block at
/// its `pc`. `None` is to go on interpreting.
#[inline(never)]
pub(crate) fn try_enter(context: &mut Context) -> Option<ControlFlow<CompletionRecord>> {
    // Fuzzing counts instructions, which compiled code does not.
    if cfg!(feature = "fuzz") {
        return None;
    }
    // Tracing shows every instruction the interpreter runs.
    #[cfg(feature = "trace")]
    if context.vm.trace {
        return None;
    }
    let options = context.vm.jit.options;
    // The dummy frame at the bottom never runs code.
    if !options.enabled || context.vm.frames.len() <= 1 {
        return None;
    }
    let frame = context.vm.frame();
    let pc = frame.pc;
    let code_block: *const CodeBlock = &raw const **frame.code_block();
    // SAFETY: the frame on top keeps its code block alive, and nothing here pops it.
    let code_block = unsafe { &*code_block };
    let slot = &code_block.jit;
    let code = match slot.tier.get() {
        Tier::Rejected => return None,
        Tier::Compiled => slot.code.borrow().clone()?,
        Tier::Interpreted => {
            let hotness = slot.hotness.get().saturating_add(1);
            slot.hotness.set(hotness);
            if hotness <= options.threshold {
                return None;
            }
            let jit = &mut context.vm.jit;
            if latch::get() != latch::AVAILABLE || !jit.has_room() {
                slot.tier.set(Tier::Rejected);
                jit.stats.rejected += 1;
                return None;
            }
            let start = std::time::Instant::now();
            // A panic in the code generator is a bug in it, not in the script: the function
            // stays interpreted, and nothing is compiled again in this process.
            let compiled = catch_unwind(AssertUnwindSafe(|| {
                compiler::compile(code_block, &options, &jit.live)
            }));
            jit.stats.compile_time += start.elapsed();
            match compiled {
                Ok(Ok(code)) => {
                    jit.stats.compiled += 1;
                    jit.stats.code_bytes += code.code.mapped_len();
                    let code = Rc::new(code);
                    slot.tier.set(Tier::Compiled);
                    *slot.code.borrow_mut() = Some(code.clone());
                    code
                }
                Ok(Err(error)) => {
                    if let CompileError::Unavailable(_) = error {
                        latch::set(latch::NO_EXECUTABLE_MEMORY);
                    }
                    slot.tier.set(Tier::Rejected);
                    jit.stats.rejected += 1;
                    return None;
                }
                Err(_panic) => {
                    latch::set(latch::COMPILER_PANICKED);
                    slot.tier.set(Tier::Rejected);
                    jit.stats.rejected += 1;
                    return None;
                }
            }
        }
    };
    let block = code.block_for_pc(pc)?;
    Some(runtime::execute(context, &code, block))
}

impl Context {
    /// The JIT's options for this context.
    #[must_use]
    pub fn jit_options(&self) -> &JitOptions {
        &self.vm.jit.options
    }

    /// The JIT's options for this context, to change. Turning it off stops compiled code from
    /// running from then on; what was compiled is kept.
    pub fn jit_options_mut(&mut self) -> &mut JitOptions {
        &mut self.vm.jit.options
    }

    /// What the JIT has done in this context.
    #[must_use]
    pub fn jit_stats(&self) -> JitStats {
        let jit = &self.vm.jit;
        JitStats {
            live_code_bytes: jit.live.bytes(),
            live_functions: jit.live.functions(),
            ..jit.stats
        }
    }
}
