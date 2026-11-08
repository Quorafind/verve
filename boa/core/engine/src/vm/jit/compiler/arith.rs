//! `/`: the numbers the interpreter's `div_fast` answers, compiled inline.

use cranelift_codegen::ir::{InstBuilder, condcodes::IntCC, types::I32};

use super::Translator;

impl Translator<'_> {
    /// `/`, as `div_fast` computes it. Two `int32`s with an exact quotient give an `int32`.
    /// Other quotients, a divisor of `0` and `i32::MIN / -1` are `f64` divisions (which give
    /// the infinities and `NaN`), as are mixed and `f64` operands; the result is boxed as a
    /// float. A dividend of `0` takes the slow path, since `div_fast` answers an `int32` zero
    /// for `0 / -5`, where JavaScript has `-0`, and the interpreter is left to decide that.
    pub(super) fn divide(&mut self, dst: u32, lhs: u32, rhs: u32, pc: u32, next: u32) {
        let a = self.load(lhs);
        let b = self.load(rhs);
        let slow = self.slow_block(pc, &[next]);
        let ints = self.b.create_block();
        let not_int = self.b.create_block();
        let check = self.b.create_block();
        let divide = self.b.create_block();
        let float_block = self.b.create_block();
        let int_store = self.b.create_block();
        self.b.append_block_param(int_store, I32);
        let done = self.continue_block();

        let a_int = self.is_int(a);
        let b_int = self.is_int(b);
        let both_int = self.b.ins().band(a_int, b_int);
        self.b.ins().brif(both_int, ints, &[], not_int, &[]);

        self.b.switch_to_block(ints);
        let x = self.b.ins().ireduce(I32, a);
        let y = self.b.ins().ireduce(I32, b);
        let x_zero = self.b.ins().icmp_imm_s(IntCC::Equal, x, 0);
        self.b.ins().brif(x_zero, slow, &[], check, &[]);

        // `sdiv` traps on a zero divisor and on `i32::MIN / -1`: both are float divisions.
        self.b.switch_to_block(check);
        let y_zero = self.b.ins().icmp_imm_s(IntCC::Equal, y, 0);
        let x_min = self
            .b
            .ins()
            .icmp_imm_s(IntCC::Equal, x, i64::from(i32::MIN));
        let y_minus_one = self.b.ins().icmp_imm_s(IntCC::Equal, y, -1);
        let overflow = self.b.ins().band(x_min, y_minus_one);
        let unsafe_divisor = self.b.ins().bor(y_zero, overflow);
        self.b
            .ins()
            .brif(unsafe_divisor, float_block, &[], divide, &[]);

        self.b.switch_to_block(divide);
        let quotient = self.b.ins().sdiv(x, y);
        let remainder = self.b.ins().srem(x, y);
        let exact = self.b.ins().icmp_imm_s(IntCC::Equal, remainder, 0);
        self.b.ins().brif(
            exact,
            int_store,
            &[cranelift_codegen::ir::BlockArg::Value(quotient)],
            float_block,
            &[],
        );

        self.b.switch_to_block(int_store);
        let quotient = self.b.block_params(int_store)[0];
        let boxed = self.box_int(quotient);
        self.store(dst, boxed);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(not_int);
        let a_number = self.is_number(a);
        let b_number = self.is_number(b);
        let both_number = self.b.ins().band(a_number, b_number);
        self.b.ins().brif(both_number, float_block, &[], slow, &[]);

        self.b.switch_to_block(float_block);
        let x = self.as_f64(a);
        let y = self.as_f64(b);
        let quotient = self.b.ins().fdiv(x, y);
        let boxed = self.box_f64(quotient);
        self.store(dst, boxed);
        self.b.ins().jump(done, &[]);

        self.b.switch_to_block(done);
    }
}
