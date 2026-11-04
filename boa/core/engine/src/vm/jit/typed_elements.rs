//! Where compiled code finds the elements of a typed array, and the element kinds it reads and
//! writes inline.
//!
//! Like [`ObjectLayout`], the offsets are found by looking at real objects, and checked against
//! them before any code relies on them: if anything does not match, typed arrays are left to the
//! helpers. The objects need no context (they have no prototype).
//!
//! What compiled code relies on, besides the offsets:
//!
//! - an object whose vtable is that of a `TypedArray` holds one, and no other object does;
//! - a `BufferObject` is two words: the address of the buffer's `GcBox` (the address an object
//!   value holds) and a tag that tells an `ArrayBuffer` from a `SharedArrayBuffer`;
//! - an `ArrayBuffer`'s bytes are a pointer, zero once the buffer is detached, and a length;
//! - `[[ArrayLength]]` is an `Option<u64>`: two words, the length and one that is zero for `auto`;
//! - the kind is a byte holding the kind's discriminant.

use std::ptr;

use super::layout::{ObjectLayout, word};
use crate::{
    JsObject, JsValue,
    builtins::{
        array_buffer::{AlignedVec, ArrayBuffer, BufferObject},
        typed_array::{TypedArrayKind, object::TypedArray},
    },
};

/// How compiled code reads and writes an element of one kind: the class says how to load, store
/// and convert it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElementClass {
    Int8 = 1,
    Uint8 = 2,
    Int16 = 3,
    Uint16 = 4,
    Int32 = 5,
    Uint32 = 6,
    Float32 = 7,
    Float64 = 8,
}

impl ElementClass {
    /// `log2` of the size of an element in bytes.
    const fn shift(self) -> u8 {
        match self {
            Self::Int8 | Self::Uint8 => 0,
            Self::Int16 | Self::Uint16 => 1,
            Self::Int32 | Self::Uint32 | Self::Float32 => 2,
            Self::Float64 => 3,
        }
    }

    /// The byte of [`KIND_CLASSES`]: the class code in the low four bits, the shift above.
    const fn entry(self) -> u8 {
        self as u8 | self.shift() << 4
    }

    /// The mask of the class code in an entry, and the shift and the flag above it.
    pub(crate) const CLASS_MASK: i64 = 15;
    pub(crate) const SHIFT_BITS: i64 = 4;
    /// Set for `Uint8Clamped`, whose class is [`Uint8`](Self::Uint8): a write clamps to `0..=255`.
    pub(crate) const CLAMPED: i64 = 0x80;
}

/// The kinds compiled code reads and writes inline, with their table entries. The others (`BigInt`s,
/// whose values are allocated, and `Float16`) are left to the helpers.
const KINDS: [(TypedArrayKind, u8); 9] = [
    (TypedArrayKind::Int8, ElementClass::Int8.entry()),
    (TypedArrayKind::Uint8, ElementClass::Uint8.entry()),
    (
        TypedArrayKind::Uint8Clamped,
        ElementClass::Uint8.entry() | ElementClass::CLAMPED as u8,
    ),
    (TypedArrayKind::Int16, ElementClass::Int16.entry()),
    (TypedArrayKind::Uint16, ElementClass::Uint16.entry()),
    (TypedArrayKind::Int32, ElementClass::Int32.entry()),
    (TypedArrayKind::Uint32, ElementClass::Uint32.entry()),
    (TypedArrayKind::Float32, ElementClass::Float32.entry()),
    (TypedArrayKind::Float64, ElementClass::Float64.entry()),
];

/// The entry of each kind, by its discriminant; zero for a kind left to the helpers.
pub(crate) static KIND_CLASSES: [u8; 16] = {
    let mut table = [0; 16];
    let mut index = 0;
    while index < KINDS.len() {
        table[KINDS[index].0 as usize] = KINDS[index].1;
        index += 1;
    }
    table
};

/// Byte offsets from the address an object value holds (its `GcBox`), and constants, for
/// reading the elements of a typed array.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypedLayout {
    /// The vtable of a typed array.
    pub(crate) vtable: i64,
    /// In a typed array: the tag of the buffer and its address, the length and the word that is
    /// zero for a length-tracking view, the byte offset and the kind (a byte).
    pub(crate) buffer_tag: i32,
    pub(crate) buffer: i32,
    pub(crate) length: i32,
    pub(crate) has_length: i32,
    pub(crate) byte_offset: i32,
    pub(crate) kind: i32,
    /// The tag of an `ArrayBuffer`.
    pub(crate) array_buffer_tag: i64,
    /// In an `ArrayBuffer`: the pointer to its bytes (zero once detached) and their length.
    pub(crate) bytes: i32,
    pub(crate) bytes_len: i32,
}

/// The address a value holding `object` has in its pointer bits.
fn address(object: &JsObject) -> usize {
    (JsValue::from(object.clone()).raw_bits() & 0x0000_FFFF_FFFF_FFFF) as usize
}

/// The `N` words at `base + offset`.
///
/// # Safety
///
/// They must be initialized words of a live allocation.
unsafe fn words<const N: usize>(base: usize, offset: usize) -> [u64; N] {
    // SAFETY: guaranteed by the caller.
    unsafe { ((base + offset) as *const [u64; N]).read() }
}

/// The index of the one word in `words` that is `value`.
fn find(words: &[u64], value: u64) -> Option<usize> {
    let mut found = words.iter().enumerate().filter(|&(_, &w)| w == value);
    match (found.next(), found.next()) {
        (Some((index, _)), None) => Some(index),
        _ => None,
    }
}

fn offset(bytes: usize) -> Option<i32> {
    i32::try_from(bytes).ok()
}

/// A view of 8 bytes at `byte_offset` of a buffer, of the given kind and length.
fn view(buffer: &JsObject, kind: TypedArrayKind, length: Option<u64>) -> Option<JsObject> {
    let buffer = buffer.clone().downcast::<ArrayBuffer>().ok()?;
    let data = TypedArray::new(BufferObject::Buffer(buffer), kind, 8, None, length);
    Some(JsObject::from_proto_and_data(None, data))
}

/// The layout of typed arrays, if every offset checks out against real objects.
pub(crate) fn compute(layout: &ObjectLayout) -> Option<TypedLayout> {
    const LEN: u64 = 40;
    const LENGTH: u64 = 0x5A5A_1234_5678;
    if size_of::<BufferObject>() != 16 || size_of::<Option<u64>>() != 16 {
        return None;
    }

    let mut bytes = AlignedVec::<u8>::with_capacity(64, 128);
    bytes.resize(LEN as usize, 0);
    let pointer = bytes.as_ptr() as u64;
    let buffer =
        JsObject::from_proto_and_data(None, ArrayBuffer::from_data(bytes, JsValue::undefined()));
    let buffer_address = address(&buffer);
    let array = view(&buffer, TypedArrayKind::Uint16, Some(LENGTH))?;
    let tracking = view(&buffer, TypedArrayKind::Uint16, None)?;
    let array_address = address(&array);

    // SAFETY: the words are those of the fields of live objects, as `size_of` and the
    // fields' types say.
    let typed = unsafe {
        // The `ArrayBuffer` is at the start of the object's data; find its words.
        let data = ptr::from_ref(&*buffer.downcast_ref::<ArrayBuffer>()?) as usize - buffer_address;
        let buffer_words = words::<{ size_of::<ArrayBuffer>() / 8 }>(buffer_address, data);
        let ta = ptr::from_ref(&*array.downcast_ref::<TypedArray>()?) as usize - array_address;
        let [first, second] = words::<2>(array_address, ta + TypedArray::BUFFER_OFFSET);
        let buffer_index = find(&[first, second], buffer_address as u64)?;
        let tag_index = 1 - buffer_index;
        let lengths = words::<2>(array_address, ta + TypedArray::ARRAY_LENGTH_OFFSET);
        let length_index = find(&lengths, LENGTH)?;
        let has_index = 1 - length_index;
        TypedLayout {
            vtable: ptr::from_ref(array.vtable()) as i64,
            buffer_tag: offset(ta + TypedArray::BUFFER_OFFSET + tag_index * 8)?,
            buffer: offset(ta + TypedArray::BUFFER_OFFSET + buffer_index * 8)?,
            length: offset(ta + TypedArray::ARRAY_LENGTH_OFFSET + length_index * 8)?,
            has_length: offset(ta + TypedArray::ARRAY_LENGTH_OFFSET + has_index * 8)?,
            byte_offset: offset(ta + TypedArray::BYTE_OFFSET_OFFSET)?,
            kind: offset(ta + TypedArray::KIND_OFFSET)?,
            array_buffer_tag: [first, second][tag_index] as i64,
            bytes: offset(data + find(&buffer_words, pointer)? * 8)?,
            bytes_len: offset(data + find(&buffer_words, LEN)? * 8)?,
        }
    };
    check(layout, &typed, &buffer, &array, &tracking).then_some(typed)
}

/// Whether `typed` reads what Rust says is in the objects it was found from: a view over a live
/// buffer, a length-tracking one, the kinds, and the buffer once detached.
fn check(
    layout: &ObjectLayout,
    typed: &TypedLayout,
    buffer: &JsObject,
    array: &JsObject,
    tracking: &JsObject,
) -> bool {
    // SAFETY: the offsets are those of fields of the live objects, as found above.
    let live = unsafe {
        word(array, layout.vtable) == typed.vtable as u64
            && word(array, layout.flag) == boa_gc::raw::BORROW_UNUSED as u64
            && word(array, typed.buffer_tag) == typed.array_buffer_tag as u64
            && word(array, typed.buffer) == address(buffer) as u64
            && word(array, typed.has_length) != 0
            && word(tracking, typed.has_length) == 0
            && word(array, typed.byte_offset) == 8
            && word(buffer, typed.bytes_len) == 40
            && word(buffer, typed.bytes) != 0
            && word(buffer, layout.flag) == boa_gc::raw::BORROW_UNUSED as u64
    };
    live && KINDS
        .iter()
        .all(|&(kind, _)| kind_agrees(typed, buffer, kind))
        && detaches(typed, buffer)
}

/// Whether the byte at the kind's offset is the kind's discriminant, as [`KIND_CLASSES`] has it.
fn kind_agrees(typed: &TypedLayout, buffer: &JsObject, kind: TypedArrayKind) -> bool {
    let Some(array) = view(buffer, kind, Some(1)) else {
        return false;
    };
    let address = address(&array);
    // SAFETY: the offset is that of the kind's byte in a live object.
    let byte = unsafe { ((address + typed.kind as usize) as *const u8).read() };
    byte == kind as u8
}

/// Whether the pointer to the bytes is zero once the buffer is detached.
fn detaches(typed: &TypedLayout, buffer: &JsObject) -> bool {
    let detached = buffer
        .downcast_mut::<ArrayBuffer>()
        .is_some_and(|mut data| data.detach(&JsValue::undefined()).is_ok());
    // SAFETY: the offset is that of the pointer, a word of the buffer.
    detached && unsafe { word(buffer, typed.bytes) == 0 }
}
