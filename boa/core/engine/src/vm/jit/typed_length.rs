//! The site cache for the `length` of a typed array.
//!
//! The interpreter's shortcut for `ta.length` ([`typed_array_length_guard`]) walks the prototype
//! chain, looking `length` up in the receiver and in each prototype, to find that the accessor
//! there is the intrinsic getter; [`typed_array_length_read`] then reads the internal slots.
//! At one site the chain is almost never different from the last time, so the answer to the
//! guard is remembered here and checked by identity instead: the receiver's shape, the shape of
//! each prototype up to the one that holds the accessor, the accessor's slot, and the count of
//! collections (the same trust as the other site caches, see `layout.rs`). The read is never
//! cached: a detached buffer or a resized one changes the answer without changing any shape.
//!
//! Why this is enough, object by object (the receiver, then each prototype up to the one that
//! holds the accessor):
//!
//! - the object has the shape it had. A shared shape never changes: adding, removing or
//!   redefining a property, or changing the prototype, moves the object to another shape. The
//!   builtin prototypes (`Float64Array.prototype`, `%TypedArray%.prototype`) have *unique*
//!   shapes, which change in place in one way only, by adding a property (removing one, changing
//!   a property between data and accessor, and changing the prototype make a new shape; changing
//!   the attributes of a property of the same width keeps the shape, which matters here only for
//!   the accessor's slot, read below). A property added in place takes a slot of the object's
//!   storage, so the site also compares the storage's length, which then catches an own
//!   `length` added to a prototype after the site was filled;
//! - so the object has no `length` now, as it had none then, except for the last prototype,
//!   which has it at the slot remembered;
//! - a shape holds its prototype, so the receiver's shape fixes the first prototype, and each
//!   prototype's shape fixes the next: the chain walked is the chain filled, object by object,
//!   and an object does not change its class;
//! - what a shape cannot fix is the value in the slot, so the accessor's getter is looked at
//!   each time (its bits against the intrinsic getter's, remembered; no lookup):
//!   `Object.defineProperty` of a new getter keeps the shape;
//! - no shape is freed (and its address reused) while the count of collections is the one the
//!   site was filled at;
//! - an object with the receiver's shape need not be a typed array (`Object.create` of
//!   `Float64Array.prototype` has it): the read refuses such an object, and the access takes
//!   its ordinary path.

use std::{mem::ManuallyDrop, ptr::NonNull};

use boa_string::StaticJsStrings;

use super::layout::SiteCache;
use crate::{
    JsObject, JsValue,
    builtins::typed_array::object::{typed_array_length_guard, typed_array_length_read},
    property::PropertyKey,
};

impl SiteCache {
    /// How many prototypes a typed array's chain may have up to the one with the accessor:
    /// `Float64Array.prototype` and `%TypedArray%.prototype`, and one more for a subclass.
    pub(crate) const CHAIN: usize = 3;

    /// The `length` of `object`, a typed array whose `length` is the intrinsic getter's, or
    /// `None` when it is not one (the interpreter's guard says so, or the site's shapes say
    /// that `object` has the shape of one without being it).
    pub(crate) fn typed_array_length(&self, object: &JsObject) -> Option<JsValue> {
        // SAFETY: the cell lives as long as the thread, which runs this code.
        let collections = unsafe { (*boa_gc::raw::collections()).get() };
        if self.holds(object, collections)
            || (typed_array_length_guard(object) && {
                self.remember(object, collections);
                true
            })
        {
            typed_array_length_read(object)
        } else {
            None
        }
    }

    /// Whether `object` still has the chain this site remembers, down to the intrinsic getter.
    fn holds(&self, object: &JsObject, collections: u64) -> bool {
        let depth = self.depth.get() as usize;
        if self.collections.get() != collections || depth == 0 {
            return false;
        }
        let Ok(borrowed) = object.try_borrow() else {
            return false;
        };
        if borrowed.shape().words() != [self.shape[0].get(), self.shape[1].get()]
            || borrowed.properties().storage.len() != self.lens[0].get() as usize
        {
            return false;
        }
        for index in 0..depth {
            let address = self.objects[index].get() as *mut _;
            // SAFETY: the address is of a prototype the chain had when the site was filled, and
            // no collection has run since. The prototypes before this one have the shapes they
            // had then, and a shape holds its prototype, so it is alive; it is borrowed, never
            // dropped (`from_raw` takes a count that was not given).
            let prototype =
                ManuallyDrop::new(unsafe { JsObject::from_raw(NonNull::new_unchecked(address)) });
            let Ok(prototype) = prototype.try_borrow() else {
                return false;
            };
            let words = &self.chain[index];
            if prototype.shape().words() != [words[0].get(), words[1].get()]
                || prototype.properties().storage.len() != self.lens[index + 1].get() as usize
            {
                return false;
            }
            if index + 1 == depth {
                // The slot's value is the one thing a shape does not fix.
                return prototype
                    .properties()
                    .storage
                    .get(self.slot.get() as usize)
                    .is_some_and(|getter| getter.raw_bits() == self.getter.get());
            }
        }
        false
    }

    /// Remembers the chain of `object`, which the guard has just accepted, if the accessor is
    /// within [`CHAIN`](Self::CHAIN) prototypes.
    fn remember(&self, object: &JsObject, collections: u64) {
        // Nothing holds while the site is half written.
        self.depth.set(0);
        let Ok(borrowed) = object.try_borrow() else {
            return;
        };
        let key = PropertyKey::from(StaticJsStrings::LENGTH);
        let mut current = borrowed.shape().prototype();
        for index in 0..Self::CHAIN {
            let Some(prototype) = current else {
                return;
            };
            let Ok(borrowed_prototype) = prototype.try_borrow() else {
                return;
            };
            let shape = borrowed_prototype.shape();
            let [tag, pointer] = shape.words();
            self.chain[index][0].set(tag);
            self.chain[index][1].set(pointer);
            self.lens[index + 1].set(borrowed_prototype.properties().storage.len() as u32);
            // Keeps the count the clone took: it is given back at once.
            let raw = prototype.clone().into_raw();
            self.objects[index].set(raw.as_ptr() as u64);
            // SAFETY: `raw` is the pointer of the clone just made.
            drop(unsafe { JsObject::from_raw(raw) });
            if let Some(slot) = shape.lookup(&key) {
                let Some(getter) = borrowed_prototype
                    .properties()
                    .storage
                    .get(slot.index as usize)
                else {
                    return;
                };
                self.getter.set(getter.raw_bits());
                let [tag, pointer] = borrowed.shape().words();
                self.shape[0].set(tag);
                self.shape[1].set(pointer);
                self.lens[0].set(borrowed.properties().storage.len() as u32);
                self.slot.set(u64::from(slot.index));
                self.collections.set(collections);
                self.depth.set(index as u32 + 1);
                #[cfg(test)]
                super::layout::FILLS.with(|fills| fills.set(fills.get() + 1));
                return;
            }
            current = shape.prototype();
        }
    }
}
