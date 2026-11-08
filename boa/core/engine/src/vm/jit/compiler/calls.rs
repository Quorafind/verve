//! `Call`, compiled as a call of `jit_call` that runs the callee when it is a compiled
//! ordinary function, and as `jit_step` otherwise.

use cranelift_codegen::ir::{BlockArg, InstBuilder, MemFlagsData, types::I32, types::I64};

use crate::vm::jit::{
    call::{NOT_HANDLED, jit_call},
    runtime::REGS_OFFSET,
};

use super::Translator;

impl Translator<'_> {
    pub(super) fn call_direct(&mut self, pc: u32, next: u32, argument_count: u32) {
        let operands =
            [next, argument_count, 0].map(|value| self.b.ins().iconst(I32, i64::from(value)));
        let answer = self.call(self.helper_sig, jit_call as *const () as i64, &operands);
        // The callee may have moved the stack.
        let regs = self
            .b
            .ins()
            .load(I64, MemFlagsData::trusted(), self.state, REGS_OFFSET);
        self.b.def_var(self.regs, regs);
        let not_handled = self.b.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            answer,
            i64::from(NOT_HANDLED),
        );
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let handled = self.b.create_block();
        self.b.ins().brif(not_handled, slow, &[], handled, &[]);

        self.b.switch_to_block(handled);
        let is_next = self.b.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            answer,
            i64::from(next),
        );
        let other = self.b.create_block();
        let block = self.block(next);
        self.b.ins().brif(is_next, block, &[], other, &[]);
        self.b.switch_to_block(other);
        self.b.ins().jump(self.dispatch, &[BlockArg::Value(answer)]);

        self.b.switch_to_block(slow);
        self.step(pc, &[next]);
    }
}
