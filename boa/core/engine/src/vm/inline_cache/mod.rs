use arrayvec::ArrayVec;
use itertools::Itertools;
use std::{cell::Cell, fmt};

use boa_gc::GcRefCell;
use boa_macros::{Finalize, Trace};

use crate::{
    JsObject, JsString, JsValue,
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

/// A cache hit: the slot, and the prototype whose storage holds it (the receiver's own storage
/// when there is none).
pub(crate) struct Hit {
    pub(crate) slot: Slot,
    pub(crate) prototype: Option<JsObject>,
}

impl Hit {
    /// The storage value `offset` past the slot's index: `0` for a data value or a getter, `1`
    /// for a setter. A storage that is too short is a miss (`None`), never a panic: the entry's
    /// checks make that impossible, and this is the last guard if one is ever wrong.
    pub(crate) fn read(&self, receiver: &PropertyMap, offset: u32) -> Option<JsValue> {
        let index = (self.slot.index + offset) as usize;
        match &self.prototype {
            Some(prototype) => prototype
                .try_borrow()
                .ok()?
                .properties()
                .storage
                .get(index)
                .cloned(),
            None => receiver.storage.get(index).cloned(),
        }
    }
}

impl CacheEntry {
    /// The prototype that holds this entry's slot, if the entry still holds for a receiver of
    /// `shape`: `Some(None)` for an own slot, `None` for a prototype that has changed shape.
    ///
    /// A shape pins what `lookup` answers for a name unless it is a unique shape that has gained a
    /// property in place, which leaves the slots it already had alone. So the receiver's shape
    /// (which also pins the prototype) and the prototype's shape, both equal to the recorded
    /// ones, mean the slot is still where, and what, it was.
    fn holder(&self, shape: &Shape) -> Option<Option<JsObject>> {
        let Some(recorded) = &self.prototype_shape else {
            return Some(None);
        };
        let prototype = shape.prototype()?;
        let unchanged = recorded.points_to(prototype.try_borrow().ok()?.shape());
        unchanged.then_some(Some(prototype))
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
        let Some(prototype_shape) = self.holder_shape(shape, slot) else {
            return;
        };

        let mut entries = self.entries.borrow_mut();

        // Add a new entry if there's space.
        if entries
            .try_push(CacheEntry {
                shape: shape.into(),
                prototype_shape,
                slot,
            })
            .is_err()
        {
            // Polymorphic cache is full, transition to megamorphic.
            self.megamorphic.set(true);
            entries.clear();
        }
    }

    /// For a slot to record: `Some` of the shape to compare the prototype with (`None` for an own
    /// slot), if the slot is what the shape that holds it has now. The slot comes from a lookup
    /// that may have run a getter, which can change objects before the entry is made.
    ///
    /// A slot in a prototype is not recorded for a unique receiver: it gains own properties in
    /// place, keeping its shape, and one of them can shadow the prototype's.
    fn holder_shape(&self, shape: &Shape, slot: Slot) -> Option<Option<WeakShape>> {
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
        (current.index == slot.index && attributes(current) == attributes(slot))
            .then_some(prototype_shape)
    }

    /// The cached slot for `shape`, if the cache has it, leaving the cache as it is: compiled
    /// code's fast path, which falls back to the interpreter (and [`Self::get`]) on a miss.
    #[cfg(feature = "jit")]
    pub(crate) fn slot_for(&self, shape: &Shape) -> Option<Hit> {
        if self.megamorphic.get() {
            return None;
        }
        let entries = self.entries.try_borrow().ok()?;
        let entry = entries.iter().find(|entry| entry.shape.points_to(shape))?;
        Some(Hit {
            slot: entry.slot,
            prototype: entry.holder(shape)?,
        })
    }

    /// Returns the cached slot if a matching shape exists in the inline cache, and the prototype
    /// still has the shape it had when the entry was recorded.
    ///
    /// Opportunistically cleans up stale weak shape references during lookup, and the entry of a
    /// prototype that has changed, which the access then records again.
    pub(crate) fn get(&self, shape: &Shape) -> Option<Hit> {
        if self.megamorphic.get() {
            return None;
        }

        let mut entries = self.entries.borrow_mut();
        let mut i = 0;
        let shape_addr = shape.to_addr_usize();

        while i < entries.len() {
            if let Some(upgraded) = entries[i].shape.upgrade() {
                if upgraded.to_addr_usize() == shape_addr {
                    let slot = entries[i].slot;
                    return match entries[i].holder(shape) {
                        Some(prototype) => Some(Hit { slot, prototype }),
                        None => {
                            entries.swap_remove(i);
                            None
                        }
                    };
                }
                i += 1;
            } else {
                // Opportunistically clean up stale weak shapes.
                entries.swap_remove(i);
            }
        }

        None
    }
}
