use arrayvec::ArrayVec;
use itertools::Itertools;
use std::{cell::Cell, fmt};

use boa_gc::GcRefCell;
use boa_macros::{Finalize, Trace};

use crate::{
    JsString, JsValue,
    object::{
        PropertyMap,
        shape::{
            Shape, WeakShape,
            slot::{Slot, SlotAttributes},
        },
    },
    property::PropertyKey,
};

#[cfg(test)]
mod tests;

pub(crate) const PIC_CAPACITY: usize = 4;

/// A cached shape-to-slot mapping for a polymorphic inline cache.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct CacheEntry {
    /// A weak reference is kept to the shape to avoid the shape preventing deallocation.
    pub(crate) shape: WeakShape,
    /// For a slot in the receiver's prototype, the shape that prototype had when the entry was
    /// recorded; a hit needs the prototype to still have it.
    pub(crate) prototype_shape: Option<WeakShape>,
    #[unsafe_ignore_trace]
    pub(crate) slot: Slot,
}

/// What reading a cache entry for a receiver found.
enum Read {
    /// The slot's value.
    Value(JsValue),
    /// The prototype has changed shape since the entry was recorded: the entry is stale.
    Stale,
    /// The storage cannot be read (borrowed, or too short): a miss that leaves the entry alone.
    Unreadable,
}

impl CacheEntry {
    /// Reads the entry's slot for a receiver of `shape` whose own properties are `receiver`: for
    /// a get, the data value or the getter; for a set, the setter, and nothing (the value is
    /// `undefined`) for a data property, which the caller writes. A storage that is too short is a miss, never a
    /// panic: the checks below make that impossible, and this is the last guard if one is ever
    /// wrong.
    ///
    /// For a slot in the prototype the prototype must still have the shape recorded. A shape pins
    /// what `lookup` answers for a name unless it is a unique shape that has gained a property
    /// in place, which leaves the slots it already had alone. So the receiver's shape (which also
    /// pins the prototype) and the prototype's shape, both equal to the recorded ones, mean the
    /// slot is still where, and what, it was.
    #[inline]
    fn read(&self, shape: &Shape, receiver: &PropertyMap, for_set: bool) -> Read {
        let accessor = self.slot.attributes.is_accessor_descriptor();
        let index = (self.slot.index + u32::from(for_set && accessor)) as usize;
        let Some(recorded) = &self.prototype_shape else {
            return if for_set && !accessor {
                Read::Value(JsValue::undefined())
            } else {
                receiver
                    .storage
                    .get(index)
                    .map_or(Read::Unreadable, |value| Read::Value(value.clone()))
            };
        };
        let Some(prototype) = shape.prototype() else {
            return Read::Unreadable;
        };
        let Ok(prototype) = prototype.try_borrow() else {
            return Read::Unreadable;
        };
        if !recorded.points_to(prototype.shape()) {
            return Read::Stale;
        }
        prototype
            .properties()
            .storage
            .get(index)
            .map_or(Read::Unreadable, |value| Read::Value(value.clone()))
    }
}

/// An inline cache entry for a property access.
#[derive(Clone, Debug, Trace, Finalize)]
pub(crate) struct InlineCache {
    /// The property that is accessed.
    pub(crate) name: JsString,

    /// Multiple cached shape-to-slot entries.
    pub(crate) entries: GcRefCell<ArrayVec<CacheEntry, PIC_CAPACITY>>,

    /// Whether this access site has seen too many shapes and should no longer be cached.
    #[unsafe_ignore_trace]
    pub(crate) megamorphic: Cell<bool>,
}

impl fmt::Display for InlineCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(name:{} entries:", self.name.display_escaped())?;

        if self.megamorphic.get() {
            return write!(f, "(megamorphic))");
        }

        let entries = self.entries.borrow();
        let entries = entries.iter().map(|e| e.shape.to_addr_usize()).format(", ");

        write!(f, "({entries:#x}))")
    }
}

impl InlineCache {
    pub(crate) fn new(name: JsString) -> Self {
        Self {
            name,
            entries: GcRefCell::new(ArrayVec::new()),
            megamorphic: Cell::new(false),
        }
    }

    /// Records `slot` for receivers of `shape`, unless it cannot be trusted to stay valid.
    pub(crate) fn set(&self, shape: &Shape, slot: Slot) {
        if self.megamorphic.get() {
            return;
        }
        let Some(entry) = self.new_entry(shape, slot) else {
            return;
        };

        let mut entries = self.entries.borrow_mut();

        // Add a new entry if there's space.
        if entries.try_push(entry).is_err() {
            // Polymorphic cache is full, transition to megamorphic.
            self.megamorphic.set(true);
            entries.clear();
        }
    }

    /// The entry for a slot to record, if the slot is what the shape that holds it has now.
    /// The slot comes from a lookup that may have run a getter, which can change objects before
    /// the entry is made.
    ///
    /// A slot in a prototype is not recorded for a unique receiver: it gains own properties in
    /// place, keeping its shape, and one of them can shadow the prototype's.
    fn new_entry(&self, shape: &Shape, slot: Slot) -> Option<CacheEntry> {
        let (holder, prototype_shape) = if slot.attributes.contains(SlotAttributes::PROTOTYPE) {
            if shape.is_unique() {
                return None;
            }
            let prototype = shape.prototype()?;
            let prototype_shape = prototype.try_borrow().ok()?.shape().clone();
            let weak = WeakShape::from(&prototype_shape);
            (prototype_shape, Some(weak))
        } else {
            (shape.clone(), None)
        };
        let current = holder.lookup(&PropertyKey::from(self.name.clone()))?;
        let attributes = |slot: Slot| slot.attributes & !SlotAttributes::INLINE_CACHE_BITS;
        (current.index == slot.index && attributes(current) == attributes(slot)).then(|| {
            CacheEntry {
                shape: shape.into(),
                prototype_shape,
                slot,
            }
        })
    }

    /// The cached slot for `shape` and its value (see [`CacheEntry::read`]), if the cache has
    /// it, leaving the cache as it is: compiled code's fast path, which falls back to the
    /// interpreter (and [`Self::get`]) on a miss.
    #[cfg(feature = "jit")]
    pub(crate) fn slot_for(
        &self,
        shape: &Shape,
        receiver: &PropertyMap,
        for_set: bool,
    ) -> Option<(Slot, JsValue)> {
        if self.megamorphic.get() {
            return None;
        }
        let entries = self.entries.try_borrow().ok()?;
        let entry = entries.iter().find(|entry| entry.shape.points_to(shape))?;
        match entry.read(shape, receiver, for_set) {
            Read::Value(value) => Some((entry.slot, value)),
            Read::Stale | Read::Unreadable => None,
        }
    }

    /// Returns the cached slot for `shape` and its value (see [`CacheEntry::read`]), if the
    /// cache has an entry for the shape that still holds.
    ///
    /// Opportunistically cleans up stale weak shape references on a miss, and the entry of a
    /// prototype that has changed, which the access then records again.
    pub(crate) fn get(
        &self,
        shape: &Shape,
        receiver: &PropertyMap,
        for_set: bool,
    ) -> Option<(Slot, JsValue)> {
        if self.megamorphic.get() {
            return None;
        }

        let mut entries = self.entries.borrow_mut();
        let Some(i) = entries
            .iter()
            .position(|entry| entry.shape.points_to(shape))
        else {
            entries.retain(|entry| entry.shape.upgrade().is_some());
            return None;
        };
        match entries[i].read(shape, receiver, for_set) {
            Read::Value(value) => Some((entries[i].slot, value)),
            Read::Unreadable => None,
            Read::Stale => {
                entries.swap_remove(i);
                None
            }
        }
    }
}
