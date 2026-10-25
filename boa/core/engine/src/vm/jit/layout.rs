//! Where compiled code finds an object's shape, properties and borrow flag, and the caches it
//! keeps for property accesses.
//!
//! The offsets come from `offset_of!` and from `boa_gc::raw`, except where a `Vec` keeps its
//! pointer and length, which Rust does not say and is found by looking at one. All of them are
//! checked against a real object, on each thread, before any code relies on them: if anything
//! does not match, property accesses are not compiled inline (the fast helpers still run them).

use std::{cell::Cell, cell::OnceCell, mem::offset_of, ptr};

use crate::{
    JsObject, JsValue,
    object::{ErasedObject, ErasedVTableObject, PropertyMap, VTABLE_OBJECT_OFFSETS},
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

fn compute() -> Option<ObjectLayout> {
    let (vtable, cell) = VTABLE_OBJECT_OFFSETS;
    let base = boa_gc::raw::gc_box_value_offset::<ErasedVTableObject>();
    let cell = base + cell;
    let object = cell + boa_gc::raw::ref_cell_value_offset::<ErasedObject>();
    let properties = object + offset_of!(ErasedObject, properties);
    let storage = properties + offset_of!(PropertyMap, storage);
    let (vec_pointer, vec_len) = vec_words()?;
    let layout = ObjectLayout {
        flag: offset(cell + boa_gc::raw::ref_cell_flag_offset::<ErasedObject>())?,
        vtable: offset(base + vtable)?,
        shape: offset(properties + offset_of!(PropertyMap, shape))?,
        storage_ptr: offset(storage + vec_pointer)?,
        storage_len: offset(storage + vec_len)?,
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
        unused && reading && writing && shape && storage && vtable
    }
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
