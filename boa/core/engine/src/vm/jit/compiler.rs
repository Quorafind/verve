//! Bytecode to Cranelift IR to machine code.
//!
//! A code block becomes one native function, `fn(state, block) -> u32`. Its blocks start at the
//! code block's *leaders*: `pc` 0, every branch target and exception handler, and every
//! instruction after a branch or after an instruction run by [`jit_step`]. A leader is where
//! compiled code can be entered: the interpreter re-enters at the instruction after a call
//! once the callee has returned, at a handler once it has caught, and in the middle of a loop
//! once the loop has got hot.
//!
//! Each instruction is either done inline — with a fast path for the common types and, when the
//! types are not those, a call to [`jit_step`] to run it as the interpreter does — or done by
//! [`jit_step`] alone. After [`jit_step`] compiled code goes on at the `pc` it answers: the next
//! instruction usually, a branch target, a handler; any other leader through `dispatch`; and if
//! there is none, or the frame changed, it returns to the interpreter.
//!
//! The inline fast paths compute exactly what the interpreter's fast paths compute, to the bit
//! (`value/operations.rs`), so where a value comes from never shows.

use std::{
    cell::OnceCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use cranelift_codegen::{
    Context as ClifContext,
    control::ControlPlane,
    ir::{
        AbiParam, Block, BlockArg, Function, InstBuilder, JumpTableData, MemFlagsData, SigRef,
        Signature, UserFuncName, Value,
        condcodes::{FloatCC, IntCC},
        types::{F64, I8, I32, I64},
    },
    isa::OwnedTargetIsa,
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::{
    JsValue,
    vm::{
        CodeBlock,
        opcode::{Instruction, Opcode},
    },
};

use super::{
    JitCode, JitOptions,
    helpers::{self, HelperFn},
    memory::{ExecutableCode, LiveCode},
    runtime::{
        EXIT, FRAME_OFFSET, INTERRUPT_OFFSET, LOOP_COUNT_OFFSET, LOOP_LIMIT_OFFSET, REGS_OFFSET,
        STEPS, jit_lookup, jit_store,
    },
};

/// The tag bits of NaN-boxed values (`value/inner/nan_boxed.rs`); checked against values made
/// by the engine in the tests.
pub(super) mod bits {
    /// The upper 32 bits of an `int32`.
    pub(crate) const INT32_HIGH: i64 = 0x7FF9_0000;
    pub(crate) const INT32_TAG: i64 = 0x7FF9_0000_0000_0000;
    pub(crate) const BOOLEAN: i64 = 0x7FFA_0000_0000_0000;
    pub(crate) const FALSE: i64 = BOOLEAN;
    pub(crate) const TRUE: i64 = BOOLEAN | 1;
    /// The pointer bits of a value that holds one.
    pub(crate) const POINTER_MASK: i64 = 0x0000_FFFF_FFFF_FFFF;
    /// An object's reference count is the first `u32` of its allocation (`GcHeader`, `repr(C)`
    /// first in the `repr(C)` `GcBox`). Above this, `inc_ref_count` panics: a call does it.
    pub(crate) const MAX_REF_COUNT: i64 = 0x7FFF_FFFE;
    /// `null`; `undefined` is `OTHER | 1`.
    pub(crate) const OTHER: i64 = 0x7FFB_0000_0000_0000;
    pub(crate) const UNDEFINED: i64 = OTHER | 1;
    pub(crate) const CANONICAL_NAN: i64 = 0x7FF8_0000_0000_0000;
    /// Values whose top 14 bits are these hold a pointer: objects, strings, symbols, bigints.
    pub(crate) const POINTER_TOP14: i64 = 0x1FFF;
    /// The top 16 bits of the tagged values compared by identity: booleans, `null` and
    /// `undefined`, objects and symbols.
    pub(crate) const TOP16_BOOLEAN: i64 = 0x7FFA;
    pub(crate) const TOP16_OTHER: i64 = 0x7FFB;
    pub(crate) const TOP16_OBJECT: i64 = 0x7FFC;
    pub(crate) const TOP16_SYMBOL: i64 = 0x7FFE;
}

thread_local! {
    static ISA: OnceCell<Result<OwnedTargetIsa, String>> = const { OnceCell::new() };
}

fn make_isa() -> Result<OwnedTargetIsa, String> {
    let mut flags = settings::builder();
    let set = |flags: &mut settings::Builder, name: &str, value: &str| {
        flags
            .set(name, value)
            .map_err(|error| format!("cranelift setting {name}={value}: {error}"))
    };
    set(&mut flags, "opt_level", "speed")?;
    set(
        &mut flags,
        "enable_verifier",
        if cfg!(debug_assertions) {
            "true"
        } else {
            "false"
        },
    )?;
    // Frames stay small, but never skip a guard page (Windows grows the stack by touching it).
    set(&mut flags, "enable_probestack", "true")?;
    set(&mut flags, "probestack_strategy", "inline")?;
    // Profilers and debuggers can walk through compiled frames.
    set(&mut flags, "preserve_frame_pointers", "true")?;
    let builder =
        cranelift_native::builder().map_err(|error| format!("no JIT for this host: {error}"))?;
    builder
        .finish(settings::Flags::new(flags))
        .map_err(|error| format!("cannot build the target ISA: {error}"))
}

/// Why a code block was not compiled.
#[derive(Debug)]
#[allow(dead_code, reason = "the reasons are for debugging, through `Debug`")]
pub(super) enum CompileError {
    /// This code block cannot or should not be compiled; others may be.
    Rejected(String),
    /// Nothing can be compiled on this host: no executable memory, or no Cranelift backend.
    Unavailable(String),
}

impl From<String> for CompileError {
    fn from(reason: String) -> Self {
        Self::Rejected(reason)
    }
}

impl From<&str> for CompileError {
    fn from(reason: &str) -> Self {
        Self::Rejected(reason.to_owned())
    }
}

#[cfg(test)]
thread_local! {
    /// Tests make the code generator panic, as a Cranelift bug would.
    pub(super) static PANIC_IN_CODEGEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Compiles `code_block`, or says why it cannot be. The code is counted in `live`.
pub(super) fn compile(
    code_block: &CodeBlock,
    options: &JitOptions,
    live: &Rc<LiveCode>,
) -> Result<JitCode, CompileError> {
    if code_block.is_async() || code_block.is_generator() {
        return Err("generators and async functions are not compiled".into());
    }
    if code_block.bytecode.bytes.len() > options.max_bytecode_len {
        return Err("too much bytecode".into());
    }
    let (block_pcs, profile) = leaders(code_block)?;
    // With a threshold of 0 everything is compiled, which is what tests want.
    if options.threshold > 0 && !profile.profitable() {
        return Err("too little to do inline".into());
    }
    ISA.with(|isa| {
        let isa = isa
            .get_or_init(make_isa)
            .as_ref()
            .map_err(|reason| CompileError::Unavailable(reason.clone()))?;
        #[cfg(test)]
        if PANIC_IN_CODEGEN.with(std::cell::Cell::get) {
            panic!("code generator panic (test)");
        }
        let function = translate(code_block, &block_pcs, isa);
        let mut context = ClifContext::for_function(function);
        let compiled = context
            .compile(&**isa, &mut ControlPlane::default())
            .map_err(|error| format!("cranelift: {:?}", error.inner))?;
        // Helpers are called through absolute addresses and constants live in the code itself,
        // so there is nothing to relocate; refuse code that would need it.
        if !compiled.buffer.relocs().is_empty() {
            return Err("compiled code needs relocations".into());
        }
        let code = ExecutableCode::new(compiled.code_buffer(), live)
            .map_err(|error| CompileError::Unavailable(error.0))?;
        Ok(JitCode {
            code,
            block_pcs: block_pcs.into_boxed_slice(),
        })
    })
}

/// Opcodes compiled code never runs: they suspend the frame or are not real instructions.
fn rejected(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Generator
            | Opcode::AsyncGenerator
            | Opcode::GeneratorYield
            | Opcode::AsyncGeneratorYield
            | Opcode::AsyncGeneratorClose
            | Opcode::Await
            | Opcode::CreatePromiseCapability
    ) || opcode.as_str() == "Reserved"
}

/// How an instruction is compiled.
enum Kind {
    /// Inline, falling through to the next instruction (with [`jit_step`] for other types).
    Inline,
    /// Inline, and it may go to these `pc`s rather than the next.
    Branch(Vec<u32>),
    /// Unconditionally to this `pc`.
    Jump(u32),
    /// By [`jit_step`] alone; it may go to these `pc`s besides the next.
    Step(Vec<u32>),
}

fn kind(instruction: &Instruction) -> Kind {
    match instruction {
        Instruction::StoreZero { .. }
        | Instruction::StoreOne { .. }
        | Instruction::StoreInt8 { .. }
        | Instruction::StoreInt16 { .. }
        | Instruction::StoreInt32 { .. }
        | Instruction::StoreFloat { .. }
        | Instruction::StoreDouble { .. }
        | Instruction::StoreNan { .. }
        | Instruction::StorePositiveInfinity { .. }
        | Instruction::StoreNegativeInfinity { .. }
        | Instruction::StoreNull { .. }
        | Instruction::StoreTrue { .. }
        | Instruction::StoreFalse { .. }
        | Instruction::StoreUndefined { .. }
        | Instruction::Move { .. }
        | Instruction::Add { .. }
        | Instruction::Sub { .. }
        | Instruction::Mul { .. }
        | Instruction::BitAnd { .. }
        | Instruction::BitOr { .. }
        | Instruction::BitXor { .. }
        | Instruction::ShiftLeft { .. }
        | Instruction::ShiftRight { .. }
        | Instruction::UnsignedShiftRight { .. }
        | Instruction::LessThan { .. }
        | Instruction::LessThanOrEq { .. }
        | Instruction::GreaterThan { .. }
        | Instruction::GreaterThanOrEq { .. }
        | Instruction::Eq { .. }
        | Instruction::NotEq { .. }
        | Instruction::StrictEq { .. }
        | Instruction::StrictNotEq { .. }
        | Instruction::Inc { .. }
        | Instruction::Dec { .. }
        | Instruction::LogicalNot { .. }
        | Instruction::PushFromRegister { .. }
        | Instruction::PopIntoRegister { .. }
        | Instruction::SetAccumulator { .. }
        | Instruction::GetArgument { .. }
        | Instruction::CheckReturn
        | Instruction::GetPropertyByName { .. }
        | Instruction::GetLengthProperty { .. }
        | Instruction::SetPropertyByName { .. }
        | Instruction::GetPropertyByValue { .. }
        | Instruction::SetPropertyByValue { .. } => Kind::Inline,
        Instruction::Jump { address } => Kind::Jump(address.as_u32()),
        Instruction::JumpIfTrue { address, .. }
        | Instruction::JumpIfFalse { address, .. }
        | Instruction::JumpIfNotUndefined { address, .. }
        | Instruction::JumpIfNullOrUndefined { address, .. }
        | Instruction::JumpIfNotLessThan { address, .. }
        | Instruction::JumpIfNotLessThanOrEqual { address, .. }
        | Instruction::JumpIfNotGreaterThan { address, .. }
        | Instruction::JumpIfNotGreaterThanOrEqual { address, .. }
        | Instruction::LogicalAnd { address, .. }
        | Instruction::LogicalOr { address, .. }
        | Instruction::Coalesce { address, .. } => Kind::Branch(vec![address.as_u32()]),
        // Branches the interpreter takes for us.
        Instruction::JumpIfNotEqual { address, .. }
        | Instruction::Case { address, .. }
        | Instruction::TemplateLookup { address, .. } => Kind::Step(vec![address.as_u32()]),
        Instruction::JumpTable { addresses, .. } => {
            Kind::Step(addresses.iter().map(|address| address.as_u32()).collect())
        }
        _ => Kind::Step(Vec::new()),
    }
}

/// The registers compiled code reads or writes itself for an inline instruction (the
/// interpreter's handlers, run by [`jit_step`], check their own).
fn inline_registers(instruction: &Instruction) -> Vec<u32> {
    match *instruction {
        Instruction::StoreZero { dst }
        | Instruction::StoreOne { dst }
        | Instruction::StoreInt8 { dst, .. }
        | Instruction::StoreInt16 { dst, .. }
        | Instruction::StoreInt32 { dst, .. }
        | Instruction::StoreFloat { dst, .. }
        | Instruction::StoreDouble { dst, .. }
        | Instruction::StoreNan { dst }
        | Instruction::StorePositiveInfinity { dst }
        | Instruction::StoreNegativeInfinity { dst }
        | Instruction::StoreNull { dst }
        | Instruction::StoreTrue { dst }
        | Instruction::StoreFalse { dst }
        | Instruction::StoreUndefined { dst }
        | Instruction::PopIntoRegister { dst }
        | Instruction::GetArgument { dst, .. } => vec![dst.into()],
        Instruction::PushFromRegister { src } | Instruction::SetAccumulator { src } => {
            vec![src.into()]
        }
        Instruction::GetPropertyByName { dst, value, .. }
        | Instruction::GetLengthProperty { dst, value, .. } => vec![dst.into(), value.into()],
        Instruction::SetPropertyByName { value, object, .. } => vec![value.into(), object.into()],
        Instruction::GetPropertyByValue {
            dst, key, object, ..
        } => vec![dst.into(), key.into(), object.into()],
        Instruction::SetPropertyByValue {
            value, key, object, ..
        } => vec![value.into(), key.into(), object.into()],
        Instruction::Move { dst, src }
        | Instruction::Inc { dst, src }
        | Instruction::Dec { dst, src } => vec![dst.into(), src.into()],
        Instruction::Add { dst, lhs, rhs }
        | Instruction::Sub { dst, lhs, rhs }
        | Instruction::Mul { dst, lhs, rhs }
        | Instruction::BitAnd { dst, lhs, rhs }
        | Instruction::BitOr { dst, lhs, rhs }
        | Instruction::BitXor { dst, lhs, rhs }
        | Instruction::ShiftLeft { dst, lhs, rhs }
        | Instruction::ShiftRight { dst, lhs, rhs }
        | Instruction::UnsignedShiftRight { dst, lhs, rhs }
        | Instruction::LessThan { dst, lhs, rhs }
        | Instruction::LessThanOrEq { dst, lhs, rhs }
        | Instruction::GreaterThan { dst, lhs, rhs }
        | Instruction::GreaterThanOrEq { dst, lhs, rhs }
        | Instruction::Eq { dst, lhs, rhs }
        | Instruction::NotEq { dst, lhs, rhs }
        | Instruction::StrictEq { dst, lhs, rhs }
        | Instruction::StrictNotEq { dst, lhs, rhs } => vec![dst.into(), lhs.into(), rhs.into()],
        Instruction::LogicalNot { value }
        | Instruction::JumpIfTrue { value, .. }
        | Instruction::JumpIfFalse { value, .. }
        | Instruction::JumpIfNotUndefined { value, .. }
        | Instruction::JumpIfNullOrUndefined { value, .. }
        | Instruction::LogicalAnd { value, .. }
        | Instruction::LogicalOr { value, .. }
        | Instruction::Coalesce { value, .. } => vec![value.into()],
        Instruction::JumpIfNotLessThan { lhs, rhs, .. }
        | Instruction::JumpIfNotLessThanOrEqual { lhs, rhs, .. }
        | Instruction::JumpIfNotGreaterThan { lhs, rhs, .. }
        | Instruction::JumpIfNotGreaterThanOrEqual { lhs, rhs, .. } => vec![lhs.into(), rhs.into()],
        _ => Vec::new(),
    }
}

/// Whether an inline instruction has a slow path through [`jit_step`]: all but the stores of
/// constants and the instructions whose fast helper always applies.
fn has_slow_path(instruction: &Instruction) -> bool {
    !matches!(
        instruction,
        Instruction::Move { .. }
            | Instruction::PushFromRegister { .. }
            | Instruction::SetAccumulator { .. }
            | Instruction::GetArgument { .. }
            | Instruction::StoreZero { .. }
            | Instruction::StoreOne { .. }
            | Instruction::StoreInt8 { .. }
            | Instruction::StoreInt16 { .. }
            | Instruction::StoreInt32 { .. }
            | Instruction::StoreFloat { .. }
            | Instruction::StoreDouble { .. }
            | Instruction::StoreNan { .. }
            | Instruction::StorePositiveInfinity { .. }
            | Instruction::StoreNegativeInfinity { .. }
            | Instruction::StoreNull { .. }
            | Instruction::StoreTrue { .. }
            | Instruction::StoreFalse { .. }
            | Instruction::StoreUndefined { .. }
    )
}

/// The byte offset of `register` from the frame's first register. Registers are checked against
/// the code block's register count in [`leaders`], which also bounds the count.
fn register_offset(register: u32) -> i32 {
    register
        .checked_mul(8)
        .and_then(|offset| i32::try_from(offset).ok())
        .expect("register operands are checked before translation")
}

/// What a code block is made of, to judge whether compiling it pays.
#[derive(Debug, Default)]
struct Profile {
    /// Each instruction's `pc`, whether it is done inline, and whether it is a call that may
    /// push a frame (and so leave compiled code and come back).
    instructions: Vec<(u32, bool, bool)>,
    /// Loops, as the spans from a backward branch's target to the branch.
    loops: Vec<(u32, u32)>,
}

impl Profile {
    /// Inline instructions a loop must do per call in it for compiled code to pay: an exit and
    /// an entry (a call) cost about what eight instructions done inline save.
    const INLINE_PER_CALL: usize = 8;

    /// Whether compiling pays: if the loops do enough inline work for the calls in them, or if
    /// at least half of straight-line code is inline.
    fn profitable(&self) -> bool {
        let in_loop = |pc: u32| {
            self.loops
                .iter()
                .any(|&(start, end)| start <= pc && pc <= end)
        };
        let (mut loop_inline, mut loop_calls, mut inline) = (0, 0, 0);
        for &(pc, is_inline, is_call) in &self.instructions {
            inline += usize::from(is_inline);
            if in_loop(pc) {
                loop_inline += usize::from(is_inline);
                loop_calls += usize::from(is_call);
            }
        }
        (loop_inline > 0 && loop_inline >= Self::INLINE_PER_CALL * loop_calls)
            || inline * 2 >= self.instructions.len()
    }
}

/// The `pc`s compiled code can start a block at, in order, and the code block's profile.
///
/// It also checks what translation relies on, refusing bytecode that breaks it rather than
/// trusting the bytecode compiler: every branch, jump-table and handler target is the start of
/// an instruction; the last instruction cannot fall off the end of the code; and every register
/// compiled code touches itself is below the code block's register count.
fn leaders(code_block: &CodeBlock) -> Result<(Vec<u32>, Profile), String> {
    let mut profile = Profile::default();
    let bytecode = &code_block.bytecode;
    let len = bytecode.bytes.len();
    if len == 0 || len >= EXIT as usize {
        return Err("no bytecode, or too much".into());
    }
    let register_count = code_block.register_count;
    if register_count
        .checked_mul(8)
        .and_then(|bytes| i32::try_from(bytes).ok())
        .is_none()
    {
        return Err("too many registers".into());
    }
    let mut starts = BTreeSet::new();
    let mut leaders = BTreeSet::from([0u32]);
    let handlers: Vec<u32> = code_block
        .handlers
        .iter()
        .map(|handler| handler.handler().as_u32())
        .collect();
    leaders.extend(handlers.iter().copied());
    let mut all_targets = handlers;
    let mut falls_off_the_end = false;
    let mut pc = 0;
    while pc < len {
        let opcode = Opcode::decode(bytecode.bytes[pc]);
        if rejected(opcode) {
            return Err(format!("{} is not compiled", opcode.as_str()));
        }
        let (instruction, next) = bytecode.next_instruction(pc);
        starts.insert(pc as u32);
        let is_call = matches!(
            opcode,
            Opcode::Call
                | Opcode::CallSpread
                | Opcode::CallEval
                | Opcode::CallEvalSpread
                | Opcode::New
                | Opcode::NewSpread
                | Opcode::SuperCall
                | Opcode::SuperCallSpread
                | Opcode::SuperCallDerived
        );
        if let Some(register) = inline_registers(&instruction)
            .into_iter()
            .find(|&register| register >= register_count)
        {
            return Err(format!(
                "{} uses register {register} of {register_count}",
                opcode.as_str()
            ));
        }
        let kind = kind(&instruction);
        let is_inline = opcode == Opcode::IncrementLoopIteration || !matches!(kind, Kind::Step(_));
        // Inline code goes on to the next instruction itself; at the end there is none.
        falls_off_the_end = matches!(kind, Kind::Inline | Kind::Branch(_));
        profile.instructions.push((pc as u32, is_inline, is_call));
        let (ends_block, targets) = match kind {
            Kind::Inline => (false, Vec::new()),
            Kind::Branch(targets) | Kind::Step(targets) => (true, targets),
            Kind::Jump(target) => (true, vec![target]),
        };
        for &target in &targets {
            if target <= pc as u32 {
                profile.loops.push((target, pc as u32));
            }
        }
        all_targets.extend(targets.iter().copied());
        // The next instruction starts a block after a branch or a step, and after an inline
        // instruction that may take its slow path, so that after the slow path compiled code
        // goes on there rather than returning to the interpreter.
        if ends_block || has_slow_path(&instruction) {
            leaders.extend(targets);
            leaders.insert(next as u32);
        }
        pc = next;
    }
    if pc != len {
        return Err("the last instruction runs past the end of the code".into());
    }
    if falls_off_the_end {
        return Err("the last instruction can fall off the end of the code".into());
    }
    if let Some(target) = all_targets.iter().find(|target| !starts.contains(target)) {
        return Err(format!("{target:#x} is not the start of an instruction"));
    }
    // Leaders are targets, handlers and next instructions; the one after the last is not code.
    leaders.retain(|leader| (*leader as usize) < len);
    Ok((leaders.into_iter().collect(), profile))
}

/// The machine-code side of a code block in translation.
struct Translator<'a> {
    b: FunctionBuilder<'a>,
    /// The code block's bytecode.
    bytes: &'a [u8],
    state: Value,
    regs: Variable,
    blocks: BTreeMap<u32, Block>,
    exit: Block,
    dispatch: Block,
    step_sig: SigRef,
    store_sig: SigRef,
    helper_sig: SigRef,
    lookup_sig: SigRef,
    /// Blocks that run an instruction with [`jit_step`], emitted after the instructions (a
    /// block must be finished before the next is started).
    slow: Vec<(Block, u32, Vec<u32>)>,
}

fn translate(code_block: &CodeBlock, block_pcs: &[u32], isa: &OwnedTargetIsa) -> Function {
    let call_conv = isa.default_call_conv();
    let ptr = isa.pointer_type();
    let mut signature = Signature::new(call_conv);
    signature
        .params
        .extend([AbiParam::new(ptr), AbiParam::new(I32)]);
    signature.returns.push(AbiParam::new(I32));
    let mut helper_signature = Signature::new(call_conv);
    helper_signature.params.extend([
        AbiParam::new(ptr),
        AbiParam::new(I32),
        AbiParam::new(I32),
        AbiParam::new(I32),
    ]);
    helper_signature.returns.push(AbiParam::new(I32));
    let mut store_signature = Signature::new(call_conv);
    store_signature
        .params
        .extend([AbiParam::new(ptr), AbiParam::new(I32), AbiParam::new(I64)]);

    let mut function = Function::with_name_signature(UserFuncName::default(), signature.clone());
    let mut builder_context = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut function, &mut builder_context);

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    let exit = b.create_block();
    let dispatch = b.create_block();
    b.append_block_param(dispatch, I32);
    let blocks: BTreeMap<u32, Block> = block_pcs.iter().map(|&pc| (pc, b.create_block())).collect();

    let step_sig = b.import_signature(signature.clone());
    let lookup_sig = b.import_signature(signature);
    let store_sig = b.import_signature(store_signature);
    let helper_sig = b.import_signature(helper_signature);

    b.switch_to_block(entry);
    let state = b.block_params(entry)[0];
    let start = b.block_params(entry)[1];
    let regs = b.declare_var(ptr);
    let first_regs = b
        .ins()
        .load(ptr, MemFlagsData::trusted(), state, REGS_OFFSET);
    b.def_var(regs, first_regs);

    let mut t = Translator {
        b,
        bytes: &code_block.bytecode.bytes,
        state,
        regs,
        blocks,
        exit,
        dispatch,
        step_sig,
        store_sig,
        helper_sig,
        lookup_sig,
        slow: Vec::new(),
    };
    t.jump_table(start);

    // Return to the interpreter.
    t.b.switch_to_block(exit);
    let zero = t.b.ins().iconst(I32, 0);
    t.b.ins().return_(&[zero]);

    // Go on at the `pc` a helper answered, if a block starts there.
    t.b.switch_to_block(dispatch);
    let pc = t.b.block_params(dispatch)[0];
    let is_exit = t.b.ins().icmp_imm_s(IntCC::Equal, pc, i64::from(EXIT));
    let lookup = t.b.create_block();
    t.b.ins().brif(is_exit, exit, &[], lookup, &[]);
    t.b.switch_to_block(lookup);
    let index = t.call(t.lookup_sig, jit_lookup as *const () as i64, &[pc]);
    t.jump_table(index);

    // The instructions.
    let bytecode = &code_block.bytecode;
    let len = bytecode.bytes.len();
    let mut pc = 0;
    let mut filled = true;
    while pc < len {
        let (instruction, next) = bytecode.next_instruction(pc);
        let (pc32, next32) = (pc as u32, next as u32);
        if let Some(&block) = t.blocks.get(&pc32) {
            if !filled {
                t.b.ins().jump(block, &[]);
            }
            t.b.switch_to_block(block);
        } else if filled {
            // Unreachable: after a jump, and no branch comes here.
            pc = next;
            continue;
        }
        // Pushes in a row (a call's `this`, function and arguments) are one helper call.
        if let Instruction::PushFromRegister { src } = instruction {
            let mut sources = vec![u32::from(src)];
            let mut end = next;
            while sources.len() < 3 && end < len && !t.blocks.contains_key(&(end as u32)) {
                let (following, after) = bytecode.next_instruction(end);
                let Instruction::PushFromRegister { src } = following else {
                    break;
                };
                sources.push(src.into());
                end = after;
            }
            let mut operands = [helpers::NO_REGISTER; 3];
            operands[..sources.len()].copy_from_slice(&sources);
            t.fast(
                helpers::jit_push,
                operands,
                Fast::MovesStack,
                pc32,
                end as u32,
            );
            filled = false;
            pc = end;
            continue;
        }
        let terminated = t.instruction(&instruction, pc32, next32);
        filled = terminated;
        pc = next;
    }
    if !filled {
        t.b.ins().jump(exit, &[]);
    }
    for (block, pc, expected) in std::mem::take(&mut t.slow) {
        t.b.switch_to_block(block);
        t.step(pc, &expected);
    }

    t.b.seal_all_blocks();
    t.b.finalize(isa.frontend_config());
    function
}

impl Translator<'_> {
    /// Branches to the block whose index is `index`, or to `exit` if there is none.
    fn jump_table(&mut self, index: Value) {
        let exit = self.b.func.dfg.block_call(self.exit, &[]);
        let targets: Vec<_> = self
            .blocks
            .values()
            .map(|&block| self.b.func.dfg.block_call(block, &[]))
            .collect();
        let table = self.b.create_jump_table(JumpTableData::new(exit, &targets));
        self.b.ins().br_table(index, table);
    }

    /// The block at `pc`, or `exit` if none starts there.
    fn block(&self, pc: u32) -> Block {
        self.blocks.get(&pc).copied().unwrap_or(self.exit)
    }

    fn call(&mut self, signature: SigRef, function: i64, args: &[Value]) -> Value {
        let callee = self.b.ins().iconst(I64, function);
        let mut all = vec![self.state];
        all.extend_from_slice(args);
        let call = self.b.ins().call_indirect(signature, callee, &all);
        self.b.inst_results(call)[0]
    }

    fn load(&mut self, register: u32) -> Value {
        let regs = self.b.use_var(self.regs);
        self.b.ins().load(
            I64,
            MemFlagsData::trusted(),
            regs,
            register_offset(register),
        )
    }

    /// Stores `bits` in `register`: in place if what is there holds no pointer, otherwise
    /// through [`jit_store`], which drops it. `bits` is a value without a pointer, or one whose
    /// reference compiled code owns and hands over to the register.
    fn store(&mut self, register: u32, bits: Value) {
        let old = self.load(register);
        let top = self.b.ins().ushr_imm_s(old, 50);
        let is_pointer = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, top, bits::POINTER_TOP14);
        let fast = self.b.create_block();
        let slow = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(is_pointer, slow, &[], fast, &[]);

        self.b.switch_to_block(fast);
        let regs = self.b.use_var(self.regs);
        self.b.ins().store(
            MemFlagsData::trusted(),
            bits,
            regs,
            register_offset(register),
        );
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(slow);
        let callee = self.b.ins().iconst(I64, jit_store as *const () as i64);
        let index = self.b.ins().iconst(I32, i64::from(register));
        self.b
            .ins()
            .call_indirect(self.store_sig, callee, &[self.state, index, bits]);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(done);
    }

    fn store_const(&mut self, register: u32, value: &JsValue) {
        let bits = self.b.ins().iconst(I64, value.raw_bits() as i64);
        self.store(register, bits);
    }

    /// Runs the instruction at `pc` with [`jit_step`], then goes on where it says: to one of
    /// `expected` (the next instruction first), or through `dispatch`.
    fn step(&mut self, pc: u32, expected: &[u32]) {
        let pc_value = self.b.ins().iconst(I32, i64::from(pc));
        let opcode = Opcode::decode(self.bytes[pc as usize]);
        let step = STEPS[opcode as usize];
        let next = self.call(self.step_sig, step as *const () as i64, &[pc_value]);
        let regs = self
            .b
            .ins()
            .load(I64, MemFlagsData::trusted(), self.state, REGS_OFFSET);
        self.b.def_var(self.regs, regs);
        for &target in expected {
            let is_target = self
                .b
                .ins()
                .icmp_imm_s(IntCC::Equal, next, i64::from(target));
            let otherwise = self.b.create_block();
            let block = self.block(target);
            self.b.ins().brif(is_target, block, &[], otherwise, &[]);
            self.b.switch_to_block(otherwise);
        }
        self.b.ins().jump(self.dispatch, &[BlockArg::Value(next)]);
    }

    /// A block that runs the instruction at `pc` with [`jit_step`].
    fn slow_block(&mut self, pc: u32, expected: &[u32]) -> Block {
        let slow = self.b.create_block();
        self.slow.push((slow, pc, expected.to_vec()));
        slow
    }

    fn is_int(&mut self, value: Value) -> Value {
        let high = self.b.ins().ushr_imm_s(value, 32);
        self.b
            .ins()
            .icmp_imm_s(IntCC::Equal, high, bits::INT32_HIGH)
    }

    /// Whether `value` is a number stored as an `f64`, as `bits::is_float` decides.
    fn is_float(&mut self, value: Value) -> Value {
        let top = self.b.ins().ushr_imm_s(value, 48);
        let exponent = self.b.ins().band_imm_s(top, 0x7FF0);
        let not_nan_space = self.b.ins().icmp_imm_s(IntCC::NotEqual, exponent, 0x7FF0);
        let kind = self.b.ins().band_imm_s(top, 0x7FFF);
        let infinity = self.b.ins().icmp_imm_s(IntCC::Equal, kind, 0x7FF0);
        let nan = self.b.ins().icmp_imm_s(IntCC::Equal, kind, 0x7FF8);
        let special = self.b.ins().bor(infinity, nan);
        self.b.ins().bor(not_nan_space, special)
    }

    fn is_number(&mut self, value: Value) -> Value {
        let int = self.is_int(value);
        let float = self.is_float(value);
        self.b.ins().bor(int, float)
    }

    fn as_f64(&mut self, value: Value) -> Value {
        let is_int = self.is_int(value);
        let int = self.b.ins().ireduce(I32, value);
        let from_int = self.b.ins().fcvt_from_sint(F64, int);
        let as_float = self.b.ins().bitcast(F64, MemFlagsData::new(), value);
        self.b.ins().select(is_int, from_int, as_float)
    }

    /// `ToInt32` of a number, as `f64_to_int32` computes it; `slow` for other types and for
    /// finite floats too large for an exact `i64`. Leaves the builder in a new block.
    fn int32_of(&mut self, value: Value, slow: Block) -> Value {
        let join = self.b.create_block();
        self.b.append_block_param(join, I32);
        let int_block = self.b.create_block();
        let not_int = self.b.create_block();
        let is_int = self.is_int(value);
        self.b.ins().brif(is_int, int_block, &[], not_int, &[]);

        self.b.switch_to_block(int_block);
        let int = self.b.ins().ireduce(I32, value);
        self.b.ins().jump(join, &[BlockArg::Value(int)]);

        self.b.switch_to_block(not_int);
        let is_float = self.is_float(value);
        let float_block = self.b.create_block();
        self.b.ins().brif(is_float, float_block, &[], slow, &[]);

        // Below 2^63 the truncation is an exact `i64`, whose low 32 bits are `ToInt32`.
        self.b.switch_to_block(float_block);
        let float = self.b.ins().bitcast(F64, MemFlagsData::new(), value);
        let magnitude = self.b.ins().fabs(float);
        let limit = self.b.ins().f64const(9_223_372_036_854_775_808.0);
        let in_range = self.b.ins().fcmp(FloatCC::LessThan, magnitude, limit);
        let convert = self.b.create_block();
        let out_of_range = self.b.create_block();
        self.b.ins().brif(in_range, convert, &[], out_of_range, &[]);

        self.b.switch_to_block(convert);
        let wide = self.b.ins().fcvt_to_sint_sat(I64, float);
        let int = self.b.ins().ireduce(I32, wide);
        self.b.ins().jump(join, &[BlockArg::Value(int)]);

        // `NaN` and the infinities are 0; other large floats are rare enough for `slow`.
        self.b.switch_to_block(out_of_range);
        let infinity = self.b.ins().f64const(f64::INFINITY);
        let is_infinite = self.b.ins().fcmp(FloatCC::Equal, magnitude, infinity);
        let is_nan = self.b.ins().fcmp(FloatCC::Unordered, float, float);
        let zero_result = self.b.ins().bor(is_infinite, is_nan);
        let zero = self.b.ins().iconst(I32, 0);
        self.b
            .ins()
            .brif(zero_result, join, &[BlockArg::Value(zero)], slow, &[]);

        self.b.switch_to_block(join);
        self.b.block_params(join)[0]
    }

    /// `IncrementLoopIteration`: unless an interrupt is asked for or the loop limit is reached
    /// (which [`jit_step`] turns into the interpreter's errors), counts the iteration.
    fn loop_iteration(&mut self, pc: u32, next: u32) {
        let slow = self.slow_block(pc, &[next]);
        let flag = self
            .b
            .ins()
            .load(I64, MemFlagsData::trusted(), self.state, INTERRUPT_OFFSET);
        let check_flag = self.b.create_block();
        let check_count = self.b.create_block();
        self.b.ins().brif(flag, check_flag, &[], check_count, &[]);

        self.b.switch_to_block(check_flag);
        let raised = self.b.ins().atomic_load(I8, MemFlagsData::trusted(), flag);
        self.b.ins().brif(raised, slow, &[], check_count, &[]);

        self.b.switch_to_block(check_count);
        let frame = self
            .b
            .ins()
            .load(I64, MemFlagsData::trusted(), self.state, FRAME_OFFSET);
        let count = self
            .b
            .ins()
            .load(I64, MemFlagsData::trusted(), frame, LOOP_COUNT_OFFSET);
        let limit = self
            .b
            .ins()
            .load(I64, MemFlagsData::trusted(), self.state, LOOP_LIMIT_OFFSET);
        let over = self.b.ins().icmp(IntCC::UnsignedGreaterThan, count, limit);
        let count_it = self.b.create_block();
        self.b.ins().brif(over, slow, &[], count_it, &[]);

        self.b.switch_to_block(count_it);
        let counted = self.b.ins().iadd_imm_s(count, 1);
        self.b
            .ins()
            .store(MemFlagsData::trusted(), counted, frame, LOOP_COUNT_OFFSET);
        let block = self.block(next);
        self.b.ins().jump(block, &[]);
    }

    fn box_int(&mut self, int: Value) -> Value {
        let wide = self.b.ins().uextend(I64, int);
        self.b.ins().bor_imm_s(wide, bits::INT32_TAG)
    }

    fn box_f64(&mut self, float: Value) -> Value {
        let raw = self.b.ins().bitcast(I64, MemFlagsData::new(), float);
        let is_nan = self.b.ins().fcmp(FloatCC::Unordered, float, float);
        let nan = self.b.ins().iconst(I64, bits::CANONICAL_NAN);
        self.b.ins().select(is_nan, nan, raw)
    }

    /// Branches on `value`'s truthiness, for the types that need no call; `slow` for the rest
    /// (strings, objects — `document.all` is falsy —, bigints).
    fn branch_truthy(&mut self, value: Value, truthy: Block, falsy: Block, slow: Block) {
        let without_low = self.b.ins().band_imm_s(value, !1);
        let is_bool = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, without_low, bits::BOOLEAN);
        let bool_block = self.b.create_block();
        let not_bool = self.b.create_block();
        self.b.ins().brif(is_bool, bool_block, &[], not_bool, &[]);

        self.b.switch_to_block(bool_block);
        let low = self.b.ins().band_imm_s(value, 1);
        self.b.ins().brif(low, truthy, &[], falsy, &[]);

        self.b.switch_to_block(not_bool);
        let is_nullish = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, without_low, bits::OTHER);
        let not_nullish = self.b.create_block();
        self.b.ins().brif(is_nullish, falsy, &[], not_nullish, &[]);

        self.b.switch_to_block(not_nullish);
        let is_int = self.is_int(value);
        let int_block = self.b.create_block();
        let not_int = self.b.create_block();
        self.b.ins().brif(is_int, int_block, &[], not_int, &[]);

        self.b.switch_to_block(int_block);
        let int = self.b.ins().ireduce(I32, value);
        self.b.ins().brif(int, truthy, &[], falsy, &[]);

        self.b.switch_to_block(not_int);
        let is_float = self.is_float(value);
        let float_block = self.b.create_block();
        self.b.ins().brif(is_float, float_block, &[], slow, &[]);

        self.b.switch_to_block(float_block);
        let float = self.b.ins().bitcast(F64, MemFlagsData::new(), value);
        let zero = self.b.ins().f64const(0.0);
        // Ordered and not equal: false for `0`, `-0` and `NaN`.
        let nonzero = self.b.ins().fcmp(FloatCC::OrderedNotEqual, float, zero);
        self.b.ins().brif(nonzero, truthy, &[], falsy, &[]);
    }

    /// Translates one instruction. Returns whether the block it ends in is terminated.
    #[allow(clippy::too_many_lines)]
    fn instruction(&mut self, instruction: &Instruction, pc: u32, next: u32) -> bool {
        match *instruction {
            Instruction::StoreZero { dst } => self.store_const(dst.into(), &JsValue::from(0)),
            Instruction::StoreOne { dst } => self.store_const(dst.into(), &JsValue::from(1)),
            Instruction::StoreInt8 { dst, value } => {
                self.store_const(dst.into(), &JsValue::from(i32::from(value)));
            }
            Instruction::StoreInt16 { dst, value } => {
                self.store_const(dst.into(), &JsValue::from(i32::from(value)));
            }
            Instruction::StoreInt32 { dst, value } => {
                self.store_const(dst.into(), &JsValue::from(value));
            }
            Instruction::StoreFloat { dst, value } => {
                self.store_const(dst.into(), &JsValue::from(value));
            }
            Instruction::StoreDouble { dst, value } => {
                self.store_const(dst.into(), &JsValue::from(value));
            }
            Instruction::StoreNan { dst } => self.store_const(dst.into(), &JsValue::nan()),
            Instruction::StorePositiveInfinity { dst } => {
                self.store_const(dst.into(), &JsValue::positive_infinity());
            }
            Instruction::StoreNegativeInfinity { dst } => {
                self.store_const(dst.into(), &JsValue::negative_infinity());
            }
            Instruction::StoreNull { dst } => self.store_const(dst.into(), &JsValue::null()),
            Instruction::StoreTrue { dst } => self.store_const(dst.into(), &JsValue::from(true)),
            Instruction::StoreFalse { dst } => self.store_const(dst.into(), &JsValue::from(false)),
            Instruction::StoreUndefined { dst } => {
                self.store_const(dst.into(), &JsValue::undefined());
            }
            Instruction::Move { dst, src } => {
                let value = self.load(src.into());
                let top = self.b.ins().ushr_imm_s(value, 50);
                let is_pointer = self
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::Equal, top, bits::POINTER_TOP14);
                let fast = self.b.create_block();
                let pointer = self.b.create_block();
                let object = self.b.create_block();
                let count = self.b.create_block();
                let clone = self.b.create_block();
                let done = self.b.create_block();
                self.b.ins().brif(is_pointer, pointer, &[], fast, &[]);
                self.b.switch_to_block(fast);
                self.store(dst.into(), value);
                self.b.ins().jump(done, &[]);
                // An object: one more reference, unless the count would overflow.
                self.b.switch_to_block(pointer);
                let top = self.b.ins().ushr_imm_s(value, 48);
                let is_object = self
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::Equal, top, bits::TOP16_OBJECT);
                self.b.ins().brif(is_object, object, &[], clone, &[]);
                self.b.switch_to_block(object);
                let address = self.b.ins().band_imm_s(value, bits::POINTER_MASK);
                let references = self.b.ins().load(I32, MemFlagsData::trusted(), address, 0);
                let at_limit = self.b.ins().icmp_imm_s(
                    IntCC::UnsignedGreaterThan,
                    references,
                    bits::MAX_REF_COUNT,
                );
                self.b.ins().brif(at_limit, clone, &[], count, &[]);
                self.b.switch_to_block(count);
                let counted = self.b.ins().iadd_imm_s(references, 1);
                self.b
                    .ins()
                    .store(MemFlagsData::trusted(), counted, address, 0);
                self.store(dst.into(), value);
                self.b.ins().jump(done, &[]);
                // Other pointers: a clone, by a helper.
                self.b.switch_to_block(clone);
                let operands = [dst.into(), src.into(), 0];
                self.fast(helpers::jit_move, operands, Fast::Always, pc, next);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
            }
            Instruction::PushFromRegister { src } => {
                let operands = [src.into(), helpers::NO_REGISTER, helpers::NO_REGISTER];
                self.fast(helpers::jit_push, operands, Fast::MovesStack, pc, next);
            }
            Instruction::PopIntoRegister { dst } => {
                self.fast(
                    helpers::jit_pop,
                    [dst.into(), 0, 0],
                    Fast::MaySlow,
                    pc,
                    next,
                );
            }
            Instruction::SetAccumulator { src } => {
                let f = helpers::jit_set_accumulator;
                self.fast(f, [src.into(), 0, 0], Fast::Always, pc, next);
            }
            Instruction::GetArgument { index, dst } => {
                let f = helpers::jit_get_argument;
                self.fast(f, [index.into(), dst.into(), 0], Fast::Always, pc, next);
            }
            Instruction::CheckReturn => {
                self.fast(helpers::jit_check_return, [0; 3], Fast::MaySlow, pc, next);
            }
            Instruction::GetPropertyByName {
                dst,
                value,
                ic_index,
            } => {
                let operands = [dst.into(), value.into(), ic_index.into()];
                self.fast(helpers::jit_get_property, operands, Fast::MaySlow, pc, next);
            }
            Instruction::GetLengthProperty {
                dst,
                value,
                ic_index,
            } => {
                let operands = [dst.into(), value.into(), ic_index.into()];
                self.fast(helpers::jit_get_length, operands, Fast::MaySlow, pc, next);
            }
            Instruction::SetPropertyByName {
                value,
                object,
                ic_index,
            } => {
                let operands = [value.into(), object.into(), ic_index.into()];
                self.fast(helpers::jit_set_property, operands, Fast::MaySlow, pc, next);
            }
            Instruction::GetPropertyByValue {
                dst, key, object, ..
            } => {
                let operands = [dst.into(), key.into(), object.into()];
                self.fast(helpers::jit_get_element, operands, Fast::MaySlow, pc, next);
            }
            Instruction::SetPropertyByValue {
                value, key, object, ..
            } => {
                let operands = [value.into(), key.into(), object.into()];
                self.fast(helpers::jit_set_element, operands, Fast::MaySlow, pc, next);
            }
            Instruction::Add { dst, lhs, rhs } => {
                self.arithmetic(Arith::Add, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::Sub { dst, lhs, rhs } => {
                self.arithmetic(Arith::Sub, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::Mul { dst, lhs, rhs } => {
                self.arithmetic(Arith::Mul, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::BitAnd { dst, lhs, rhs } => {
                self.bitwise(Bitwise::And, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::BitOr { dst, lhs, rhs } => {
                self.bitwise(Bitwise::Or, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::BitXor { dst, lhs, rhs } => {
                self.bitwise(Bitwise::Xor, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::ShiftLeft { dst, lhs, rhs } => {
                self.bitwise(Bitwise::Shl, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::ShiftRight { dst, lhs, rhs } => {
                self.bitwise(Bitwise::Shr, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::UnsignedShiftRight { dst, lhs, rhs } => {
                self.bitwise(Bitwise::Ushr, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::LessThan { dst, lhs, rhs } => {
                self.compare(Compare::Lt, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::LessThanOrEq { dst, lhs, rhs } => {
                self.compare(Compare::Le, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::GreaterThan { dst, lhs, rhs } => {
                self.compare(Compare::Gt, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::GreaterThanOrEq { dst, lhs, rhs } => {
                self.compare(Compare::Ge, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::Eq { dst, lhs, rhs } => {
                self.compare(Compare::Eq, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::NotEq { dst, lhs, rhs } => {
                self.compare(Compare::Ne, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::StrictEq { dst, lhs, rhs } => {
                self.strict_equals(false, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::StrictNotEq { dst, lhs, rhs } => {
                self.strict_equals(true, dst.into(), lhs.into(), rhs.into(), pc, next);
            }
            Instruction::Inc { dst, src } => self.increment(true, dst.into(), src.into(), pc, next),
            Instruction::Dec { dst, src } => {
                self.increment(false, dst.into(), src.into(), pc, next);
            }
            Instruction::LogicalNot { value } => {
                let register: u32 = value.into();
                let value = self.load(register);
                let truthy = self.b.create_block();
                let falsy = self.b.create_block();
                let done = self.b.create_block();
                let slow = self.slow_block(pc, &[next]);
                self.branch_truthy(value, truthy, falsy, slow);
                for (block, result) in [(truthy, bits::FALSE), (falsy, bits::TRUE)] {
                    self.b.switch_to_block(block);
                    let bits = self.b.ins().iconst(I64, result);
                    self.store(register, bits);
                    self.b.ins().jump(done, &[]);
                }
                self.b.switch_to_block(done);
            }

            Instruction::Jump { address } => {
                let block = self.block(address.as_u32());
                self.b.ins().jump(block, &[]);
                return true;
            }
            Instruction::JumpIfTrue { address, value }
            | Instruction::LogicalOr { address, value } => {
                let target = address.as_u32();
                let value = self.load(value.into());
                let slow = self.slow_block(pc, &[next, target]);
                let (taken, fallthrough) = (self.block(target), self.block(next));
                self.branch_truthy(value, taken, fallthrough, slow);
                return true;
            }
            Instruction::JumpIfFalse { address, value }
            | Instruction::LogicalAnd { address, value } => {
                let target = address.as_u32();
                let value = self.load(value.into());
                let slow = self.slow_block(pc, &[next, target]);
                let (taken, fallthrough) = (self.block(target), self.block(next));
                self.branch_truthy(value, fallthrough, taken, slow);
                return true;
            }
            Instruction::JumpIfNotUndefined { address, value } => {
                let value = self.load(value.into());
                let is_undefined = self
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::Equal, value, bits::UNDEFINED);
                let (taken, fallthrough) = (self.block(address.as_u32()), self.block(next));
                self.b
                    .ins()
                    .brif(is_undefined, fallthrough, &[], taken, &[]);
                return true;
            }
            Instruction::JumpIfNullOrUndefined { address, value }
            | Instruction::Coalesce { address, value } => {
                let is_jump = matches!(instruction, Instruction::JumpIfNullOrUndefined { .. });
                let value = self.load(value.into());
                let without_low = self.b.ins().band_imm_s(value, !1);
                let is_nullish = self
                    .b
                    .ins()
                    .icmp_imm_s(IntCC::Equal, without_low, bits::OTHER);
                let (taken, fallthrough) = (self.block(address.as_u32()), self.block(next));
                if is_jump {
                    self.b.ins().brif(is_nullish, taken, &[], fallthrough, &[]);
                } else {
                    self.b.ins().brif(is_nullish, fallthrough, &[], taken, &[]);
                }
                return true;
            }
            Instruction::JumpIfNotLessThan { address, lhs, rhs } => {
                self.compare_jump(
                    Compare::Lt,
                    address.as_u32(),
                    lhs.into(),
                    rhs.into(),
                    pc,
                    next,
                );
                return true;
            }
            Instruction::JumpIfNotLessThanOrEqual { address, lhs, rhs } => {
                self.compare_jump(
                    Compare::Le,
                    address.as_u32(),
                    lhs.into(),
                    rhs.into(),
                    pc,
                    next,
                );
                return true;
            }
            Instruction::JumpIfNotGreaterThan { address, lhs, rhs } => {
                self.compare_jump(
                    Compare::Gt,
                    address.as_u32(),
                    lhs.into(),
                    rhs.into(),
                    pc,
                    next,
                );
                return true;
            }
            Instruction::JumpIfNotGreaterThanOrEqual { address, lhs, rhs } => {
                self.compare_jump(
                    Compare::Ge,
                    address.as_u32(),
                    lhs.into(),
                    rhs.into(),
                    pc,
                    next,
                );
                return true;
            }
            Instruction::IncrementLoopIteration => {
                self.loop_iteration(pc, next);
                return true;
            }
            _ => {
                let expected = match kind(instruction) {
                    Kind::Step(targets) => targets,
                    _ => Vec::new(),
                };
                let mut all = vec![next];
                all.extend(expected);
                self.step(pc, &all);
                return true;
            }
        }
        false
    }

    /// Calls a fast helper with its operands, then goes on in a new block. If the helper
    /// answers anything but `DONE`, its fast path did not apply: compiled code runs the
    /// instruction with [`jit_step`] (`MaySlow`), or the helper panicked and compiled code
    /// returns, for `execute` to resume the panic (the others).
    fn fast(&mut self, function: HelperFn, operands: [u32; 3], fast: Fast, pc: u32, next: u32) {
        let operands = operands.map(|operand| self.b.ins().iconst(I32, i64::from(operand)));
        let status = self.call(self.helper_sig, function as *const () as i64, &operands);
        if fast == Fast::MovesStack {
            let regs = self
                .b
                .ins()
                .load(I64, MemFlagsData::trusted(), self.state, REGS_OFFSET);
            self.b.def_var(self.regs, regs);
        }
        let otherwise = if fast == Fast::MaySlow {
            self.slow_block(pc, &[next])
        } else {
            self.exit
        };
        let done = self.b.create_block();
        self.b.ins().brif(status, otherwise, &[], done, &[]);
        self.b.switch_to_block(done);
    }

    /// Goes on to the next instruction from the current block.
    fn continue_block(&mut self) -> Block {
        self.b.create_block()
    }

    fn arithmetic(&mut self, op: Arith, dst: u32, lhs: u32, rhs: u32, pc: u32, next: u32) {
        let a = self.load(lhs);
        let b = self.load(rhs);
        let slow = self.slow_block(pc, &[next]);
        let int_block = self.b.create_block();
        let not_int = self.b.create_block();
        let float_block = self.b.create_block();
        let int_store = self.b.create_block();
        self.b.append_block_param(int_store, I32);
        let done = self.continue_block();

        let a_int = self.is_int(a);
        let b_int = self.is_int(b);
        let both_int = self.b.ins().band(a_int, b_int);
        self.b.ins().brif(both_int, int_block, &[], not_int, &[]);

        // int32 op int32, unless it overflows (or, for `*`, the result is `-0`).
        self.b.switch_to_block(int_block);
        let x = self.b.ins().ireduce(I32, a);
        let y = self.b.ins().ireduce(I32, b);
        let (result, overflow) = match op {
            Arith::Add => self.b.ins().sadd_overflow(x, y),
            Arith::Sub => self.b.ins().ssub_overflow(x, y),
            Arith::Mul => self.b.ins().smul_overflow(x, y),
        };
        let to_float = if op == Arith::Mul {
            // `mul_fast` keeps a zero product an int only if neither factor is negative.
            let zero = self.b.ins().icmp_imm_s(IntCC::Equal, result, 0);
            let x_negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, x, 0);
            let y_negative = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, y, 0);
            let negative = self.b.ins().bor(x_negative, y_negative);
            let negative_zero = self.b.ins().band(zero, negative);
            self.b.ins().bor(overflow, negative_zero)
        } else {
            overflow
        };
        self.b.ins().brif(
            to_float,
            float_block,
            &[],
            int_store,
            &[BlockArg::Value(result)],
        );

        self.b.switch_to_block(int_store);
        let result = self.b.block_params(int_store)[0];
        let boxed = self.box_int(result);
        self.store(dst, boxed);
        self.b.ins().jump(done, &[]);

        // Numbers: as `f64`.
        self.b.switch_to_block(not_int);
        let a_number = self.is_number(a);
        let b_number = self.is_number(b);
        let both_number = self.b.ins().band(a_number, b_number);
        self.b.ins().brif(both_number, float_block, &[], slow, &[]);

        self.b.switch_to_block(float_block);
        let x = self.as_f64(a);
        let y = self.as_f64(b);
        let result = match op {
            Arith::Add => self.b.ins().fadd(x, y),
            Arith::Sub => self.b.ins().fsub(x, y),
            Arith::Mul => self.b.ins().fmul(x, y),
        };
        let boxed = self.box_f64(result);
        self.store(dst, boxed);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(done);
    }

    fn bitwise(&mut self, op: Bitwise, dst: u32, lhs: u32, rhs: u32, pc: u32, next: u32) {
        let a = self.load(lhs);
        let b = self.load(rhs);
        let slow = self.slow_block(pc, &[next]);
        let done = self.continue_block();
        let x = self.int32_of(a, slow);
        let y = self.int32_of(b, slow);
        // Cranelift's shifts take the amount modulo 32, as `wrapping_shl` and JavaScript do.
        let result = match op {
            Bitwise::And => self.b.ins().band(x, y),
            Bitwise::Or => self.b.ins().bor(x, y),
            Bitwise::Xor => self.b.ins().bxor(x, y),
            Bitwise::Shl => self.b.ins().ishl(x, y),
            Bitwise::Shr => self.b.ins().sshr(x, y),
            Bitwise::Ushr => self.b.ins().ushr(x, y),
        };
        if op == Bitwise::Ushr {
            // A `u32`: an int if it fits, an `f64` if not (`From<u32> for JsValue`).
            let fits = self
                .b
                .ins()
                .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, result, 0);
            let int_store = self.b.create_block();
            let float_store = self.b.create_block();
            self.b.ins().brif(fits, int_store, &[], float_store, &[]);
            self.b.switch_to_block(int_store);
            let boxed = self.box_int(result);
            self.store(dst, boxed);
            self.b.ins().jump(done, &[]);
            self.b.switch_to_block(float_store);
            let float = self.b.ins().fcvt_from_uint(F64, result);
            let boxed = self.box_f64(float);
            self.store(dst, boxed);
            self.b.ins().jump(done, &[]);
        } else {
            let boxed = self.box_int(result);
            self.store(dst, boxed);
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(done);
    }

    /// Compares two numbers, branching to `on_true`/`on_false`, or to `slow` for other types.
    fn numeric_compare(
        &mut self,
        op: Compare,
        a: Value,
        b: Value,
        on_true: Block,
        on_false: Block,
        slow: Block,
    ) {
        let int_block = self.b.create_block();
        let not_int = self.b.create_block();
        let float_block = self.b.create_block();
        let a_int = self.is_int(a);
        let b_int = self.is_int(b);
        let both_int = self.b.ins().band(a_int, b_int);
        self.b.ins().brif(both_int, int_block, &[], not_int, &[]);

        self.b.switch_to_block(int_block);
        let x = self.b.ins().ireduce(I32, a);
        let y = self.b.ins().ireduce(I32, b);
        let condition = match op {
            Compare::Lt => IntCC::SignedLessThan,
            Compare::Le => IntCC::SignedLessThanOrEqual,
            Compare::Gt => IntCC::SignedGreaterThan,
            Compare::Ge => IntCC::SignedGreaterThanOrEqual,
            Compare::Eq => IntCC::Equal,
            Compare::Ne => IntCC::NotEqual,
        };
        let result = self.b.ins().icmp(condition, x, y);
        self.b.ins().brif(result, on_true, &[], on_false, &[]);

        self.b.switch_to_block(not_int);
        let a_number = self.is_number(a);
        let b_number = self.is_number(b);
        let both_number = self.b.ins().band(a_number, b_number);
        self.b.ins().brif(both_number, float_block, &[], slow, &[]);

        self.b.switch_to_block(float_block);
        let x = self.as_f64(a);
        let y = self.as_f64(b);
        // As Rust's operators on `f64`: ordered, except `!=`, which is true for `NaN`.
        let condition = match op {
            Compare::Lt => FloatCC::LessThan,
            Compare::Le => FloatCC::LessThanOrEqual,
            Compare::Gt => FloatCC::GreaterThan,
            Compare::Ge => FloatCC::GreaterThanOrEqual,
            Compare::Eq => FloatCC::Equal,
            Compare::Ne => FloatCC::NotEqual,
        };
        let result = self.b.ins().fcmp(condition, x, y);
        self.b.ins().brif(result, on_true, &[], on_false, &[]);
    }

    fn compare(&mut self, op: Compare, dst: u32, lhs: u32, rhs: u32, pc: u32, next: u32) {
        let a = self.load(lhs);
        let b = self.load(rhs);
        let slow = self.slow_block(pc, &[next]);
        let on_true = self.b.create_block();
        let on_false = self.b.create_block();
        let done = self.continue_block();
        self.numeric_compare(op, a, b, on_true, on_false, slow);
        for (block, result) in [(on_true, bits::TRUE), (on_false, bits::FALSE)] {
            self.b.switch_to_block(block);
            let bits = self.b.ins().iconst(I64, result);
            self.store(dst, bits);
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(done);
    }

    /// `JumpIfNot<op>`: jumps to `target` unless `lhs <op> rhs`.
    fn compare_jump(&mut self, op: Compare, target: u32, lhs: u32, rhs: u32, pc: u32, next: u32) {
        let a = self.load(lhs);
        let b = self.load(rhs);
        let slow = self.slow_block(pc, &[next, target]);
        let (taken, fallthrough) = (self.block(target), self.block(next));
        self.numeric_compare(op, a, b, fallthrough, taken, slow);
    }

    /// `===` and `!==`: numbers by value; booleans, `null`, `undefined`, objects and symbols by
    /// identity, which is their bits. Strings and bigints compare by content: `slow`.
    fn strict_equals(&mut self, negate: bool, dst: u32, lhs: u32, rhs: u32, pc: u32, next: u32) {
        let a = self.load(lhs);
        let b = self.load(rhs);
        let slow = self.slow_block(pc, &[next]);
        let on_true = self.b.create_block();
        let on_false = self.b.create_block();
        let not_numbers = self.b.create_block();
        let by_identity = self.b.create_block();
        let done = self.continue_block();
        let (if_equal, if_not) = if negate {
            (on_false, on_true)
        } else {
            (on_true, on_false)
        };

        // Numbers, first as ints, then as `f64`; `slow` is never taken from here.
        let a_number = self.is_number(a);
        let b_number = self.is_number(b);
        let both_number = self.b.ins().band(a_number, b_number);
        let numbers = self.b.create_block();
        self.b
            .ins()
            .brif(both_number, numbers, &[], not_numbers, &[]);
        self.b.switch_to_block(numbers);
        self.numeric_compare(Compare::Eq, a, b, if_equal, if_not, slow);

        self.b.switch_to_block(not_numbers);
        let lhs_identity = self.compared_by_identity(a);
        let rhs_identity = self.compared_by_identity(b);
        let either = self.b.ins().bor(lhs_identity, rhs_identity);
        self.b.ins().brif(either, by_identity, &[], slow, &[]);

        self.b.switch_to_block(by_identity);
        let same = self.b.ins().icmp(IntCC::Equal, a, b);
        self.b.ins().brif(same, if_equal, &[], if_not, &[]);

        for (block, result) in [(on_true, bits::TRUE), (on_false, bits::FALSE)] {
            self.b.switch_to_block(block);
            let bits = self.b.ins().iconst(I64, result);
            self.store(dst, bits);
            self.b.ins().jump(done, &[]);
        }
        self.b.switch_to_block(done);
    }

    fn compared_by_identity(&mut self, value: Value) -> Value {
        let top = self.b.ins().ushr_imm_s(value, 48);
        let mut result = None;
        for tag in [
            bits::TOP16_BOOLEAN,
            bits::TOP16_OTHER,
            bits::TOP16_OBJECT,
            bits::TOP16_SYMBOL,
        ] {
            let is = self.b.ins().icmp_imm_s(IntCC::Equal, top, tag);
            result = Some(match result {
                Some(previous) => self.b.ins().bor(previous, is),
                None => is,
            });
        }
        result.expect("four tags")
    }

    /// `Inc`/`Dec` on an int that does not overflow: `src` keeps its value, `dst` gets the new
    /// one.
    fn increment(&mut self, up: bool, dst: u32, src: u32, pc: u32, next: u32) {
        let value = self.load(src);
        let slow = self.slow_block(pc, &[next]);
        let fast = self.b.create_block();
        let done = self.continue_block();
        let is_int = self.is_int(value);
        let int = self.b.ins().ireduce(I32, value);
        let limit = if up { i32::MAX } else { i32::MIN };
        let at_limit = self.b.ins().icmp_imm_s(IntCC::Equal, int, i64::from(limit));
        let not_limit = self.b.ins().bxor_imm_s(at_limit, 1);
        let ok = self.b.ins().band(is_int, not_limit);
        self.b.ins().brif(ok, fast, &[], slow, &[]);

        self.b.switch_to_block(fast);
        let result = self.b.ins().iadd_imm_s(int, if up { 1 } else { -1 });
        let boxed = self.box_int(result);
        self.store(dst, boxed);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }
}

/// What a fast helper may answer besides `DONE`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fast {
    /// Its fast path always applies; it fails only by panicking.
    Always,
    /// As `Always`, and it may move the VM stack, so the registers' address is reloaded.
    MovesStack,
    /// Its fast path may not apply.
    MaySlow,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arith {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bitwise {
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Ushr,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Compare {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}
