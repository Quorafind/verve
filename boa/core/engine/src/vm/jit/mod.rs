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
    rc::Rc,
    time::Duration,
};

use crate::{
    Context,
    vm::{CodeBlock, CompletionRecord},
};

use self::{memory::ExecutableCode, runtime::EntryFn};

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
    /// How much machine code, in bytes, a context may compile in all; past it, no more.
    pub code_budget: usize,
}

impl Default for JitOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: 1000,
            max_bytecode_len: 32 * 1024,
            code_budget: 64 * 1024 * 1024,
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
    /// Bytes of machine code compiled.
    pub code_bytes: usize,
    /// Time spent compiling.
    pub compile_time: Duration,
}

/// The JIT's state in a [`Vm`](crate::vm::Vm).
#[derive(Debug)]
pub(crate) struct JitVm {
    pub(crate) options: JitOptions,
    pub(crate) stats: JitStats,
    /// Set when the frame on top may have changed or got hotter, so the interpreter's loop
    /// asks [`try_enter`] whether to run compiled code.
    pub(crate) pending: bool,
}

impl JitVm {
    pub(crate) fn new() -> Self {
        Self {
            options: JitOptions::from_env(),
            stats: JitStats::default(),
            pending: false,
        }
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
            if jit.stats.code_bytes >= options.code_budget {
                slot.tier.set(Tier::Rejected);
                jit.stats.rejected += 1;
                return None;
            }
            let start = std::time::Instant::now();
            let compiled = compiler::compile(code_block, &options);
            jit.stats.compile_time += start.elapsed();
            match compiled {
                Ok(code) => {
                    jit.stats.compiled += 1;
                    jit.stats.code_bytes += code.code.len();
                    let code = Rc::new(code);
                    slot.tier.set(Tier::Compiled);
                    *slot.code.borrow_mut() = Some(code.clone());
                    code
                }
                Err(_reason) => {
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
        self.vm.jit.stats
    }
}
