//! The elements of typed arrays, read and written inline.
//!
//! An access that passes every guard below does what `typed_array_element_get_shortcut` and
//! `typed_array_element_set_shortcut` do for it, on the same bytes; any guard that fails goes to
//! the helper, which runs those and is the reference. The guards:
//!
//! - the object's vtable is the typed arrays' (so its data is a `TypedArray`);
//! - neither the view nor its buffer is borrowed for writing (a write also needs the buffer
//!   unborrowed, as `borrow_mut` does);
//! - the buffer is an `ArrayBuffer` (a `SharedArrayBuffer` is left to the helper) that is not
//!   detached, and the view has a fixed length (a length-tracking one is left to the helper);
//! - the kind is one that [`KIND_CLASSES`] has;
//! - the view lies within the buffer as it is now (a resizable buffer may have shrunk), and the
//!   index is within the view: the two checks `IsTypedArrayOutOfBounds` and `index < length`
//!   make, so no byte outside the view is touched;
//! - to write, the value is a number: no `ToNumber`, so no user code runs.
//!
//! Every field is read from the objects at each access: nothing is cached, so a detach, a
//! resize or a write by any other code is seen. The guards are three branches, to keep the code
//! of an access small (compile time is paid for each access in a function): the vtable, then
//! what the view says, then what the buffer says; a field is read only after the guard that
//! makes it safe to read.

use cranelift_codegen::ir::{
    Block, BlockArg, InstBuilder, MemFlagsData, Value,
    condcodes::IntCC,
    types::{F32, F64, I8, I16, I32, I64},
};

use super::Translator;
use crate::vm::jit::{
    layout::ObjectLayout,
    typed_elements::{ElementClass, KIND_CLASSES, TypedLayout},
};

use ElementClass::{Float32, Float64, Int8, Int16, Int32, Uint8, Uint16, Uint32};

/// The classes of a read, in the order the code tests them: the likeliest first.
const READ_CLASSES: [ElementClass; 8] =
    [Float64, Int32, Uint8, Float32, Int8, Int16, Uint16, Uint32];

impl Translator<'_> {
    /// Goes to `miss` unless `ok`; leaves the builder in a new block.
    fn require(&mut self, ok: Value, miss: Block) {
        let next = self.b.create_block();
        self.b.ins().brif(ok, next, &[], miss, &[]);
        self.b.switch_to_block(next);
    }

    /// Goes to `miss` unless the typed array at `object` can be read (`write` false) or written
    /// at `index`, which is not negative. Answers the kind's entry in [`KIND_CLASSES`] and the
    /// element's address, in a new block.
    fn typed_element(
        &mut self,
        layout: ObjectLayout,
        typed: TypedLayout,
        [object, index]: [Value; 2],
        write: bool,
        miss: Block,
    ) -> (Value, Value) {
        let vtable = self.load_at(I64, object, layout.vtable);
        let is_typed = self.b.ins().icmp_imm_s(IntCC::Equal, vtable, typed.vtable);
        self.require(is_typed, miss);

        // What the view says: not being written, over an `ArrayBuffer`, of a fixed length, and
        // of a kind with a class.
        let flag = self.load_at(I64, object, layout.flag);
        let tag = self.load_at(I64, object, typed.buffer_tag);
        let fixed = self.load_at(I64, object, typed.has_length);
        let kind = self.load_at(I8, object, typed.kind);
        let kind = self.b.ins().uextend(I64, kind);
        let table = self.b.ins().iconst(I64, KIND_CLASSES.as_ptr() as i64);
        let entry = self.b.ins().iadd(table, kind);
        let entry = self.load_at(I8, entry, 0);
        let entry = self.b.ins().uextend(I64, entry);
        let conditions = [
            self.b.ins().icmp_imm_s(IntCC::NotEqual, flag, -1),
            self.b
                .ins()
                .icmp_imm_s(IntCC::Equal, tag, typed.array_buffer_tag),
            self.b.ins().icmp_imm_s(IntCC::NotEqual, fixed, 0),
            self.b.ins().icmp_imm_s(IntCC::NotEqual, entry, 0),
        ];
        let ok = self.all(conditions);
        self.require(ok, miss);

        // What the buffer says: usable, not detached, and the view and the index inside it.
        let buffer = self.load_at(I64, object, typed.buffer);
        let buffer_flag = self.load_at(I64, buffer, layout.flag);
        let data = self.load_at(I64, buffer, typed.bytes);
        let buffer_len = self.load_at(I64, buffer, typed.bytes_len);
        let length = self.load_at(I64, object, typed.length);
        let byte_offset = self.load_at(I64, object, typed.byte_offset);
        let shift = self.b.ins().ushr_imm_s(entry, ElementClass::SHIFT_BITS);
        let shift = self.b.ins().band_imm_s(shift, 3);
        let bytes = self.b.ins().ishl(length, shift);
        let end = self.b.ins().iadd(byte_offset, bytes);
        let usable = if write {
            self.b.ins().icmp_imm_s(IntCC::Equal, buffer_flag, 0)
        } else {
            self.b.ins().icmp_imm_s(IntCC::NotEqual, buffer_flag, -1)
        };
        let conditions = [
            usable,
            self.b.ins().icmp_imm_s(IntCC::NotEqual, data, 0),
            self.b
                .ins()
                .icmp(IntCC::UnsignedLessThanOrEqual, end, buffer_len),
            self.b.ins().icmp(IntCC::UnsignedLessThan, index, length),
        ];
        let ok = self.all(conditions);
        self.require(ok, miss);

        let skip = self.b.ins().ishl(index, shift);
        let start = self.b.ins().iadd(data, byte_offset);
        (entry, self.b.ins().iadd(start, skip))
    }

    /// Whether all of `conditions` hold.
    fn all(&mut self, conditions: [Value; 4]) -> Value {
        let [a, b, c, d] = conditions;
        let first = self.b.ins().band(a, b);
        let second = self.b.ins().band(c, d);
        self.b.ins().band(first, second)
    }

    /// Branches on the class of `entry` to one new block for each of `classes`, in order; the
    /// last is taken without a test (no other class is in the kind table).
    fn class_blocks(&mut self, entry: Value, classes: &[ElementClass]) -> Vec<Block> {
        let class = self.b.ins().band_imm_s(entry, ElementClass::CLASS_MASK);
        let blocks: Vec<Block> = classes.iter().map(|_| self.b.create_block()).collect();
        for (&this, &block) in classes.iter().zip(&blocks).take(classes.len() - 1) {
            let is_class = self.b.ins().icmp_imm_s(IntCC::Equal, class, this as i64);
            let next = self.b.create_block();
            self.b.ins().brif(is_class, block, &[], next, &[]);
            self.b.switch_to_block(next);
        }
        self.b.ins().jump(blocks[classes.len() - 1], &[]);
        blocks
    }

    /// `GetPropertyByValue` of the typed array at `object`: the element boxed as the
    /// interpreter boxes it, a number of any kind as an `f64` (`NaN` the canonical one). Ends
    /// with a jump to `store`, a block that takes the value and stores it in the register.
    pub(super) fn typed_read(
        &mut self,
        layout: ObjectLayout,
        typed: TypedLayout,
        operands: [Value; 2],
        miss: Block,
        store: Block,
    ) {
        let (entry, element) = self.typed_element(layout, typed, operands, false, miss);
        let blocks = self.class_blocks(entry, &READ_CLASSES);
        for (&class, block) in READ_CLASSES.iter().zip(blocks) {
            self.b.switch_to_block(block);
            let boxed = self.typed_load(class, element);
            self.b.ins().jump(store, &[BlockArg::Value(boxed)]);
        }
    }

    /// The element of `class` at `element`, boxed.
    fn typed_load(&mut self, class: ElementClass, element: Value) -> Value {
        let int = match class {
            Float64 => {
                let number = self.load_at(F64, element, 0);
                return self.box_f64(number);
            }
            Float32 => {
                let single = self.load_at(F32, element, 0);
                let number = self.b.ins().fpromote(F64, single);
                return self.box_f64(number);
            }
            Int8 => {
                let byte = self.load_at(I8, element, 0);
                self.b.ins().sextend(I32, byte)
            }
            Uint8 => {
                let byte = self.load_at(I8, element, 0);
                self.b.ins().uextend(I32, byte)
            }
            Int16 => {
                let half = self.load_at(I16, element, 0);
                self.b.ins().sextend(I32, half)
            }
            Uint16 => {
                let half = self.load_at(I16, element, 0);
                self.b.ins().uextend(I32, half)
            }
            Int32 | Uint32 => self.load_at(I32, element, 0),
        };
        let number = if class == Uint32 {
            let wide = self.b.ins().uextend(I64, int);
            self.b.ins().fcvt_from_sint(F64, wide)
        } else {
            self.b.ins().fcvt_from_sint(F64, int)
        };
        // An integer is never `NaN`: no need to canonicalize it.
        self.b.ins().bitcast(I64, MemFlagsData::new(), number)
    }

    /// `SetPropertyByValue` of `new` to the typed array at `object`, as the interpreter's
    /// shortcut stores it: `Float64` the number, `Float32` the number rounded to `f32`, an
    /// integer kind `ToInt32` of it wrapped to the element's width (`int32_of`, which leaves
    /// the floats it cannot be exact for to the helper), `Uint8Clamped` an `int32` clamped to
    /// `0..=255` (a float, whose rounding is the helper's, is the helper's). A value that is
    /// not a number is left to the helper, whose conversion may run user code. Ends with a jump
    /// to `done`.
    pub(super) fn typed_write(
        &mut self,
        layout: ObjectLayout,
        typed: TypedLayout,
        operands: [Value; 2],
        new: Value,
        miss: Block,
        done: Block,
    ) {
        let (entry, element) = self.typed_element(layout, typed, operands, true, miss);
        let store = |this: &mut Self, value: Value| {
            this.b
                .ins()
                .store(MemFlagsData::trusted(), value, element, 0);
            this.b.ins().jump(done, &[]);
        };
        // The last class stands for the integer kinds, which differ only in width.
        let blocks = self.class_blocks(entry, &[Float64, Float32, Int32]);
        let &[float64, float32, integers] = blocks.as_slice() else {
            unreachable!("one block for each class");
        };

        self.b.switch_to_block(float64);
        let is_number = self.is_number(new);
        self.require(is_number, miss);
        let number = self.as_f64(new);
        store(self, number);

        self.b.switch_to_block(float32);
        let is_number = self.is_number(new);
        self.require(is_number, miss);
        let number = self.as_f64(new);
        let single = self.b.ins().fdemote(F32, number);
        store(self, single);

        self.b.switch_to_block(integers);
        let clamped = self.b.ins().band_imm_s(entry, ElementClass::CLAMPED);
        let (clamp, wrap) = (self.b.create_block(), self.b.create_block());
        self.b.ins().brif(clamped, clamp, &[], wrap, &[]);
        self.b.switch_to_block(clamp);
        let is_int = self.is_int(new);
        self.require(is_int, miss);
        let int = self.b.ins().ireduce(I32, new);
        let zero = self.b.ins().iconst(I32, 0);
        let positive = self.b.ins().smax(int, zero);
        let top = self.b.ins().iconst(I32, 255);
        let byte = self.b.ins().smin(positive, top);
        let byte = self.b.ins().ireduce(I8, byte);
        store(self, byte);

        // The low bits of `ToInt32`, as wide as the element.
        self.b.switch_to_block(wrap);
        let int = self.int32_of(new, miss);
        let [word, half, byte] = [(); 3].map(|()| self.b.create_block());
        let shift = self.b.ins().ushr_imm_s(entry, ElementClass::SHIFT_BITS);
        let is_word = self.b.ins().icmp_imm_s(IntCC::Equal, shift, 2);
        let not_word = self.b.create_block();
        self.b.ins().brif(is_word, word, &[], not_word, &[]);
        self.b.switch_to_block(not_word);
        let is_half = self.b.ins().icmp_imm_s(IntCC::Equal, shift, 1);
        self.b.ins().brif(is_half, half, &[], byte, &[]);
        self.b.switch_to_block(word);
        store(self, int);
        self.b.switch_to_block(half);
        let value = self.b.ins().ireduce(I16, int);
        store(self, value);
        self.b.switch_to_block(byte);
        let value = self.b.ins().ireduce(I8, int);
        store(self, value);
    }
}
