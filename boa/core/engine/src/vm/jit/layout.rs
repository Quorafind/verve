//! Where compiled code finds an object's shape, properties and borrow flag, and the caches it
//! keeps for property accesses.
//!
//! The offsets come from `offset_of!` and from `boa_gc::raw`, except where a `Vec` keeps its
//! pointer and length, which Rust does not say and is found by looking at one. All of them are
//! checked against a real object, on each thread, before any code relies on them: if anything
//! does not match, property accesses are not compiled inline (the fast helpers still run them).

use std::{cell::Cell, cell::OnceCell, mem::offset_of, ptr};

use thin_vec::ThinVec;

use crate::{
    JsObject, JsValue,
    object::{
        ErasedObject, ErasedVTableObject, IndexedProperties, PropertyMap, VTABLE_OBJECT_OFFSETS,
    },
};

/// Byte offsets from the address an object value holds (its `GcBox`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ObjectLayout {
    /// The borrow flag of the object's `GcRefCell`, a `usize`.
    pub(crate) flag: i32,
    /// The object's vtable pointer.
    pub(crate) vtable: i32,
    /// The object's shape: two words.
    pub(crate) shape: i32,
    /// The pointer and the length of the property storage, a `Vec<JsValue>`.
    pub(crate) storage_ptr: i32,
    pub(crate) storage_len: i32,
    /// The object's `extensible` flag, a `bool`.
    pub(crate) extensible: i32,
    /// The elements: an `IndexedProperties`, a tag word ([`DENSE_I32`](Self::DENSE_I32), …)
    /// then the word of a `ThinVec` (the address of its header) for the dense kinds.
    pub(crate) indexed: i32,
    /// In a `ThinVec`'s header, where its length (a `usize`) is; its elements follow the
    /// header at `thin_data`, for `i32`, `f64` and `JsValue` alike.
    pub(crate) thin_len: i32,
    pub(crate) thin_data: i32,
    /// This thread's count of garbage collections (`boa_gc::raw::collections`).
    pub(crate) collections: *const Cell<u64>,
}

thread_local! {
    static LAYOUT: OnceCell<Option<ObjectLayout>> = const { OnceCell::new() };
}

/// The object layout, if it checked out on this thread.
pub(crate) fn object_layout() -> Option<ObjectLayout> {
    LAYOUT.with(|layout| *layout.get_or_init(compute))
}

impl ObjectLayout {
    /// The tags of the dense kinds of `IndexedProperties`.
    pub(crate) const DENSE_I32: i64 = 0;
    pub(crate) const DENSE_F64: i64 = 1;
    pub(crate) const DENSE_ELEMENT: i64 = 2;
}

fn offset(bytes: usize) -> Option<i32> {
    i32::try_from(bytes).ok()
}

/// Where a `Vec<JsValue>` keeps its pointer and its length, in bytes from its start.
fn vec_words() -> Option<(usize, usize)> {
    const WORDS: usize = size_of::<Vec<JsValue>>() / size_of::<usize>();
    if size_of::<Vec<JsValue>>() != WORDS * size_of::<usize>() {
        return None;
    }
    let mut vec: Vec<JsValue> = Vec::with_capacity(7);
    vec.extend([
        JsValue::undefined(),
        JsValue::undefined(),
        JsValue::undefined(),
    ]);
    // SAFETY: a `Vec` is `WORDS` initialized words.
    let words = unsafe { ptr::from_ref(&vec).cast::<[usize; WORDS]>().read() };
    let find = |value: usize| {
        let mut found = words.iter().enumerate().filter(|&(_, &w)| w == value);
        match (found.next(), found.next()) {
            (Some((index, _)), None) => Some(index * size_of::<usize>()),
            _ => None,
        }
    };
    let pointer = find(vec.as_ptr() as usize)?;
    let len = find(vec.len())?;
    (pointer != len).then_some((pointer, len))
}

/// Where a `ThinVec<T>` keeps its length in its header, and how far from the header its elements
/// start, for each of the element types arrays use; `None` unless they agree.
fn thin_vec_words() -> Option<(usize, usize)> {
    fn probe<T: Clone>(value: T) -> Option<(usize, usize)> {
        if size_of::<ThinVec<T>>() != size_of::<usize>() {
            return None;
        }
        let mut vec: ThinVec<T> = ThinVec::with_capacity(9);
        vec.extend([value.clone(), value.clone(), value]);
        // SAFETY: a `ThinVec` is the address of its header.
        let header = unsafe { ptr::from_ref(&vec).cast::<usize>().read() };
        // The header is two words, length and capacity in some order.
        // SAFETY: the header is at least two initialized words.
        let words = unsafe { (header as *const [usize; 2]).read() };
        let len = match words {
            [3, capacity] if capacity >= 9 => 0,
            [capacity, 3] if capacity >= 9 => size_of::<usize>(),
            _ => return None,
        };
        let data = (vec.as_ptr() as usize).checked_sub(header)?;
        Some((len, data))
    }
    let words = probe(7i32)?;
    (probe(0.5f64)? == words && probe(JsValue::undefined())? == words).then_some(words)
}

fn compute() -> Option<ObjectLayout> {
    let (vtable, cell) = VTABLE_OBJECT_OFFSETS;
    let base = boa_gc::raw::gc_box_value_offset::<ErasedVTableObject>();
    let cell = base + cell;
    let object = cell + boa_gc::raw::ref_cell_value_offset::<ErasedObject>();
    let properties = object + offset_of!(ErasedObject, properties);
    let storage = properties + offset_of!(PropertyMap, storage);
    let (vec_pointer, vec_len) = vec_words()?;
    let (thin_len, thin_data) = thin_vec_words()?;
    let layout = ObjectLayout {
        flag: offset(cell + boa_gc::raw::ref_cell_flag_offset::<ErasedObject>())?,
        vtable: offset(base + vtable)?,
        shape: offset(properties + offset_of!(PropertyMap, shape))?,
        storage_ptr: offset(storage + vec_pointer)?,
        storage_len: offset(storage + vec_len)?,
        extensible: offset(object + offset_of!(ErasedObject, extensible))?,
        indexed: offset(properties + offset_of!(PropertyMap, indexed_properties))?,
        thin_len: offset(thin_len)?,
        thin_data: offset(thin_data)?,
        collections: boa_gc::raw::collections(),
    };
    check(&layout).then_some(layout)
}

/// Reads a word at `offset` from an object's allocation.
///
/// # Safety
///
/// `offset` must be within the object's allocation, at a word-aligned initialized field.
unsafe fn word(object: &JsObject, offset: i32) -> u64 {
    let address = (JsValue::from(object.clone()).raw_bits() & 0x0000_FFFF_FFFF_FFFF) as usize
        + offset as usize;
    // SAFETY: guaranteed by the caller.
    unsafe { (address as *const u64).read() }
}

/// Reads a byte at `offset` from an object's allocation.
///
/// # Safety
///
/// `offset` must be within the object's allocation, at an initialized byte.
unsafe fn byte(object: &JsObject, offset: i32) -> u8 {
    let address = (JsValue::from(object.clone()).raw_bits() & 0x0000_FFFF_FFFF_FFFF) as usize
        + offset as usize;
    // SAFETY: guaranteed by the caller.
    unsafe { (address as *const u8).read() }
}

/// Whether `layout` reads what Rust says is in a real object.
fn check(layout: &ObjectLayout) -> bool {
    let object = JsObject::with_null_proto();
    object
        .borrow_mut()
        .properties_mut()
        .storage
        .extend([JsValue::from(1), JsValue::from(2)]);
    // SAFETY: the offsets were computed for this type; what they find is checked below.
    unsafe {
        let unused = word(&object, layout.flag) == boa_gc::raw::BORROW_UNUSED as u64;
        let reading = {
            let _borrow = object.borrow();
            word(&object, layout.flag) == 1
        };
        let writing = {
            let _borrow = object.borrow_mut();
            word(&object, layout.flag) == boa_gc::raw::BORROW_WRITING as u64
        };
        let borrowed = object.borrow();
        let properties = borrowed.properties();
        let shape = properties.shape.words()
            == [word(&object, layout.shape), word(&object, layout.shape + 8)];
        let storage = word(&object, layout.storage_ptr) == properties.storage.as_ptr() as u64
            && word(&object, layout.storage_len) == 2;
        let vtable = word(&object, layout.vtable) == ptr::from_ref(object.vtable()) as u64;
        let extensible = byte(&object, layout.extensible) == 1;
        drop(borrowed);
        let elements = check_elements(&object, layout);
        unused && reading && writing && shape && storage && vtable && extensible && elements
    }
}

/// Whether `layout` finds each dense kind of elements where Rust puts them.
///
/// # Safety
///
/// As [`word`].
unsafe fn check_elements(object: &JsObject, layout: &ObjectLayout) -> bool {
    let kinds = [
        (
            ObjectLayout::DENSE_I32,
            IndexedProperties::DenseI32(ThinVec::from([4, 5])),
            u64::from(5u32),
        ),
        (
            ObjectLayout::DENSE_F64,
            IndexedProperties::DenseF64(ThinVec::from([0.25, 2.5])),
            2.5f64.to_bits(),
        ),
        (
            ObjectLayout::DENSE_ELEMENT,
            IndexedProperties::DenseElement(ThinVec::from([JsValue::null(), JsValue::from(9)])),
            JsValue::from(9).raw_bits(),
        ),
    ];
    let mut ok = true;
    for (tag, elements, second) in kinds {
        object.borrow_mut().properties_mut().indexed_properties = elements;
        // SAFETY: as the caller guarantees; the header and elements are read as Rust made them.
        unsafe {
            let header = word(object, layout.indexed + 8) as usize;
            let len = (header as *const u64)
                .byte_add(layout.thin_len as usize)
                .read();
            let data = (header as *const u8).byte_add(layout.thin_data as usize);
            let value = if tag == ObjectLayout::DENSE_I32 {
                u64::from(data.byte_add(4).cast::<u32>().read_unaligned())
            } else {
                data.byte_add(8).cast::<u64>().read_unaligned()
            };
            ok &= word(object, layout.indexed) == tag as u64 && len == 2 && value == second;
        }
    }
    object.borrow_mut().properties_mut().indexed_properties = IndexedProperties::default();
    ok
}

/// What compiled code remembers about one property access: the shape of the last object whose
/// own data property it was, the property's storage slot, and the count of collections then.
/// The shape is still that shape as long as no collection has run since (shapes are freed only
/// by collections), and only shared shapes are kept, whose properties never change.
#[derive(Debug)]
#[repr(C)]
pub(crate) struct SiteCache {
    pub(crate) shape: [Cell<u64>; 2],
    pub(crate) collections: Cell<u64>,
    pub(crate) slot: Cell<u64>,
}

impl SiteCache {
    pub(crate) const SHAPE: i32 = offset_of!(Self, shape) as i32;
    pub(crate) const COLLECTIONS: i32 = offset_of!(Self, collections) as i32;
    pub(crate) const SLOT: i32 = offset_of!(Self, slot) as i32;

    pub(crate) fn new() -> Self {
        Self {
            shape: [Cell::new(0), Cell::new(0)],
            // Never a count of collections: matches nothing until filled.
            collections: Cell::new(u64::MAX),
            slot: Cell::new(0),
        }
    }

    /// Remembers that `object`'s own data property at `slot` is the one accessed, if its shape
    /// is shared.
    pub(crate) fn fill(&self, object: &JsObject, slot: u32, layout: &ObjectLayout) {
        let Ok(borrowed) = object.try_borrow() else {
            return;
        };
        let shape = borrowed.shape();
        if !shape.is_shared() {
            return;
        }
        let [tag, pointer] = shape.words();
        self.shape[0].set(tag);
        self.shape[1].set(pointer);
        self.slot.set(u64::from(slot));
        // SAFETY: the cell lives as long as the thread, which runs this code.
        self.collections.set(unsafe { (*layout.collections).get() });
        #[cfg(test)]
        FILLS.with(|fills| fills.set(fills.get() + 1));
    }
}

#[cfg(test)]
thread_local! {
    /// How many times a site was filled on this thread: each is a miss of the inline path.
    pub(crate) static FILLS: Cell<u64> = const { Cell::new(0) };
}
