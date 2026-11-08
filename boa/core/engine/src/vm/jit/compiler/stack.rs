//! The instructions that move values between registers, the VM stack and the frame's
//! arguments, compiled inline: `PushFromRegister`, `PopIntoRegister`, `GetArgument` and
//! `CheckReturn`. Each leaves what it cannot do (a full stack, a pointer that is not an object,
//! a constructor's frame) to the fast helper or `jit_step`, which do exactly what the
//! interpreter does.

use cranelift_codegen::ir::{
    InstBuilder, MemFlagsData, condcodes::IntCC, types::I8, types::I32, types::I64,
};

use crate::JsValue;
use crate::vm::jit::{
    helpers,
    runtime::{
        ARGUMENT_COUNT_OFFSET, FLAGS_OFFSET, FP_OFFSET, FRAME_OFFSET, RP_OFFSET, STACK_OFFSET,
    },
    stack_layout::StackLayout,
};

use super::{Fast, Translator};

impl Translator<'_> {
    /// `PushFromRegister` of `src`: the value is copied onto the stack's `Vec` when there is
    /// room and it holds no pointer or an object (one more reference is counted); otherwise
    /// the helper pushes it (and may move the stack).
    pub(super) fn push_inline(&mut self, layout: StackLayout, src: u32, pc: u32, next: u32) {
        let vec = self.load_word(self.state, STACK_OFFSET);
        let len = self.load_word(vec, layout.len);
        let cap = self.load_word(vec, layout.cap);
        let value = self.load(src);
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let typed = self.b.create_block();
        let pointer = self.b.create_block();
        let write = self.b.create_block();
        let done = self.b.create_block();
        let full = self.b.ins().icmp(IntCC::Equal, len, cap);
        self.b.ins().brif(full, slow, &[], typed, &[]);

        self.b.switch_to_block(typed);
        self.branch_pointer(value, pointer, write);
        self.b.switch_to_block(pointer);
        self.count_reference(value, slow);
        self.b.ins().jump(write, &[]);

        self.b.switch_to_block(write);
        let elements = self.load_word(vec, layout.ptr);
        let offset = self.b.ins().ishl_imm_s(len, 3);
        let address = self.b.ins().iadd(elements, offset);
        self.b
            .ins()
            .store(MemFlagsData::trusted(), value, address, 0);
        let grown = self.b.ins().iadd_imm_s(len, 1);
        self.b
            .ins()
            .store(MemFlagsData::trusted(), grown, vec, layout.len);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(slow);
        let operands = [src, helpers::NO_REGISTER, helpers::NO_REGISTER];
        self.fast(helpers::jit_push, operands, Fast::MovesStack, pc, next);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }

    /// `PopIntoRegister` into `dst`, when there is a value above the frame's registers: the
    /// value moves to the register, whose old value is dropped.
    pub(super) fn pop_inline(&mut self, layout: StackLayout, dst: u32, pc: u32, next: u32) {
        let slow = self.slow_block(pc, &[next]);
        let vec = self.load_word(self.state, STACK_OFFSET);
        let len = self.load_word(vec, layout.len);
        let rp = self
            .b
            .ins()
            .load(I32, MemFlagsData::trusted(), self.state, RP_OFFSET);
        let rp = self.b.ins().uextend(I64, rp);
        let limit = self.b.ins().iadd_imm_s(rp, i64::from(self.register_count));
        let above = self.b.ins().icmp(IntCC::UnsignedGreaterThan, len, limit);
        let pop = self.b.create_block();
        self.b.ins().brif(above, pop, &[], slow, &[]);

        self.b.switch_to_block(pop);
        let shrunk = self.b.ins().iadd_imm_s(len, -1);
        self.b
            .ins()
            .store(MemFlagsData::trusted(), shrunk, vec, layout.len);
        let elements = self.load_word(vec, layout.ptr);
        let offset = self.b.ins().ishl_imm_s(shrunk, 3);
        let address = self.b.ins().iadd(elements, offset);
        let value = self.b.ins().load(I64, MemFlagsData::trusted(), address, 0);
        self.store(dst, value);
    }

    /// `GetArgument` of argument `index` into `dst`: `undefined` past the arguments given, a
    /// copy otherwise (an object's reference is counted; other pointers take the helper).
    pub(super) fn argument_inline(
        &mut self,
        layout: StackLayout,
        [index, dst]: [u32; 2],
        pc: u32,
        next: u32,
    ) {
        let frame = self.load_word(self.state, FRAME_OFFSET);
        let count = self
            .b
            .ins()
            .load(I32, MemFlagsData::trusted(), frame, ARGUMENT_COUNT_OFFSET);
        let given = self
            .b
            .ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, count, i64::from(index));
        let have = self.b.create_block();
        let missing = self.b.create_block();
        let pointer = self.b.create_block();
        let plain = self.b.create_block();
        let other = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(given, have, &[], missing, &[]);

        self.b.switch_to_block(missing);
        self.store_const(dst, &JsValue::undefined());
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(have);
        let fp = self
            .b
            .ins()
            .load(I32, MemFlagsData::trusted(), frame, FP_OFFSET);
        let fp = self.b.ins().uextend(I64, fp);
        // The arguments follow `this` and the function.
        let slot = self.b.ins().iadd_imm_s(fp, i64::from(index) + 2);
        let vec = self.load_word(self.state, STACK_OFFSET);
        let elements = self.load_word(vec, layout.ptr);
        let offset = self.b.ins().ishl_imm_s(slot, 3);
        let address = self.b.ins().iadd(elements, offset);
        let value = self.b.ins().load(I64, MemFlagsData::trusted(), address, 0);
        self.branch_pointer(value, pointer, plain);
        self.b.switch_to_block(plain);
        self.store(dst, value);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(pointer);
        self.count_reference(value, other);
        self.store(dst, value);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(other);
        let f = helpers::jit_get_argument;
        self.fast(f, [index, dst, 0], Fast::Always, pc, next);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
    }

    /// `CheckReturn`, which does nothing in a frame that is not a constructor's.
    pub(super) fn check_return_inline(&mut self, layout: StackLayout, pc: u32, next: u32) {
        let slow = self.slow_block(pc, &[next]);
        let frame = self.load_word(self.state, FRAME_OFFSET);
        let flags = self
            .b
            .ins()
            .load(I8, MemFlagsData::trusted(), frame, FLAGS_OFFSET);
        let construct = self.b.ins().band_imm_s(flags, layout.construct);
        let go_on = self.b.create_block();
        self.b.ins().brif(construct, slow, &[], go_on, &[]);
        self.b.switch_to_block(go_on);
    }

    fn load_word(
        &mut self,
        address: cranelift_codegen::ir::Value,
        offset: i32,
    ) -> cranelift_codegen::ir::Value {
        self.b
            .ins()
            .load(I64, MemFlagsData::trusted(), address, offset)
    }
}
