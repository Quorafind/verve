//! The inline cache for a property that an assignment creates: `this.x = v` on an object that has
//! no `x`, which is how a constructor builds an object.
//!
//! Without it every such assignment looks the name up in the object and in each prototype, looks
//! the transition up in the shape, and stores the value; with it a site remembers the shape it
//! saw, the shape that came out, and what its prototype chain looked like, and a hit swaps the
//! shape and pushes the value. The result is what `OrdinarySet` would have done: it is only used
//! where that is known without running anything (see [`AddEntry`]).

use arrayvec::ArrayVec;
use boa_macros::{Finalize, Trace};

use crate::{
    JsObject, JsValue,
    object::{
        internal_methods::{InternalObjectMethods, ORDINARY_INTERNAL_METHODS},
        shape::{Shape, WeakShape, slot::Slot},
    },
    property::PropertyKey,
};

/// The most shapes (so, the sites with different receivers) a cache remembers, and the longest
/// prototype chain an entry validates.
const CAPACITY: usize = 4;

/// Whether `vtable` runs the ordinary object's property internal methods.
///
/// A pointer comparison against `ORDINARY_INTERNAL_METHODS` would compare the address of a `const`
/// that the compiler is free to materialise more than once, so the methods are compared by the
/// function they call instead. The three property methods the shortcut replaces are checked, and so
/// is `[[Set]]`: the object's own `[[Set]]` is what runs first, and `OrdinarySet` on a missing key
/// calls `[[Set]]` on the prototype, so an override of it on the object or on any prototype changes
/// the outcome.
pub(crate) fn has_ordinary_property_methods(vtable: &InternalObjectMethods) -> bool {
    std::ptr::fn_addr_eq(
        vtable.__get_own_property__,
        ORDINARY_INTERNAL_METHODS.__get_own_property__,
    ) && std::ptr::fn_addr_eq(
        vtable.__get_prototype_of__,
        ORDINARY_INTERNAL_METHODS.__get_prototype_of__,
    ) && std::ptr::fn_addr_eq(
        vtable.__define_own_property__,
        ORDINARY_INTERNAL_METHODS.__define_own_property__,
    ) && std::ptr::fn_addr_eq(vtable.__set__, ORDINARY_INTERNAL_METHODS.__set__)
}

/// What a prototype was when an entry was recorded: its shape and, for a unique shape, the number
/// of properties in it.
#[derive(Clone, Debug, Trace, Finalize)]
struct Stamp {
    shape: WeakShape,
    /// A unique shape gains properties in place and keeps its identity, so its identity alone
    /// does not say it still lacks the name. Everything else it can do (lose a property, change
    /// one's kind or attributes, change prototype) makes a new shape, so identity and count
    /// together pin its table. Zero for a shared shape, which never changes.
    #[unsafe_ignore_trace]
    properties: u32,
}

impl Stamp {
    fn of(shape: &Shape) -> Self {
        Self {
            shape: shape.into(),
            properties: shape.unique_property_count(),
        }
    }

    fn holds(&self, shape: &Shape) -> bool {
        self.shape.points_to(shape) && self.properties == shape.unique_property_count()
    }
}

/// One way a site added a property: the receiver's shape, the shape it had afterwards, and the
/// prototype chain the name was found missing from.
///
/// A hit needs the receiver to be ordinary and extensible with the recorded shape, and each
/// prototype to have the recorded [`Stamp`]. Why that is the whole of `[[Set]]`:
///
/// - The receiver's shape is shared, so it is immutable: it lacks the name, has the prototype
///   it has, and the transition to the shape after is the one an insert takes.
/// - A shape holds its prototype, so the receiver's shape pins the nearest prototype, that
///   prototype's shape pins the next, and so on to the end of the chain: the objects are the
///   ones recorded. An object's internal methods never change, and were ordinary when recorded.
/// - A prototype's shape and [`Stamp::properties`] pin its table, so the name is still missing
///   from every prototype: no setter, no read-only property, nothing that `[[Set]]` would
///   find.
/// - Whether the receiver is extensible is not in the shape, so it is read on every hit.
#[derive(Clone, Debug, Trace, Finalize)]
struct AddEntry {
    from: WeakShape,
    to: WeakShape,
    /// The index of the new property in the storage: the storage's length before the insert.
    #[unsafe_ignore_trace]
    index: u32,
    /// The prototypes, nearest first.
    chain: ArrayVec<Stamp, CAPACITY>,
}

impl AddEntry {
    /// Whether the prototype chain of an object with shape `shape` is the one recorded.
    fn chain_holds(&self, shape: &Shape) -> bool {
        let mut prototype = shape.prototype();
        for stamp in &self.chain {
            let Some(object) = prototype else {
                return false;
            };
            let Ok(object) = object.try_borrow() else {
                return false;
            };
            if !stamp.holds(object.shape()) {
                return false;
            }
            prototype = object.shape().prototype();
        }
        prototype.is_none()
    }
}

/// The adds a site has seen.
#[derive(Clone, Debug, Default, Trace, Finalize)]
pub(crate) struct AddCache {
    entries: boa_gc::GcRefCell<Vec<AddEntry>>,
    /// Whether the site has seen too many shapes: it records no more.
    #[unsafe_ignore_trace]
    full: std::cell::Cell<bool>,
}

impl AddCache {
    /// How many transitions the cache holds.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether the site has seen too many shapes to be cached.
    #[cfg(test)]
    pub(crate) fn is_full(&self) -> bool {
        self.full.get()
    }

    /// Adds the property `index` slots into the storage of `object` with `value`, if the cache has
    /// the transition for the object's shape and it still holds. Returns whether it did.
    #[inline]
    pub(crate) fn add(&self, object: &JsObject, value: &JsValue) -> bool {
        let Ok(entries) = self.entries.try_borrow() else {
            return false;
        };
        if entries.is_empty() || !has_ordinary_property_methods(object.vtable()) {
            return false;
        }
        let Ok(mut object) = object.try_borrow_mut() else {
            return false;
        };
        if !object.extensible {
            return false;
        }
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.from.points_to(object.shape()))
        else {
            return false;
        };
        let properties = object.properties_mut();
        if properties.storage.len() != entry.index as usize || !entry.chain_holds(&properties.shape)
        {
            return false;
        }
        let Some(to) = entry.to.upgrade() else {
            return false;
        };
        properties.shape = to;
        properties.storage.push(value.clone());
        true
    }

    /// Records that adding `key` to `object` (an ordinary, extensible object, whose shape was
    /// `from` before) took it to its present shape, if the transition can be trusted.
    ///
    /// The caller has just made the insert, which it only does when `key` is missing from the
    /// whole chain and nothing has run since.
    pub(crate) fn record(&self, from: &Shape, key: &PropertyKey, object: &JsObject) {
        if self.full.get() || !from.is_shared() {
            return;
        }
        let Ok(borrowed) = object.try_borrow() else {
            return;
        };
        let to = borrowed.shape();
        let Some(Slot { index, attributes }) = to.lookup(key) else {
            return;
        };
        if !to.is_shared()
            || attributes.is_accessor_descriptor()
            || index as usize + 1 != borrowed.properties().storage.len()
        {
            return;
        }
        let Some(chain) = chain_of(to) else {
            return;
        };
        let mut entries = self.entries.borrow_mut();
        if entries.iter().any(|entry| entry.from.points_to(from)) {
            // The entry is out of date: a prototype changed, or the shape's was collected.
            entries.retain(|entry| !entry.from.points_to(from));
        }
        if entries.len() == CAPACITY {
            // Shapes that were collected leave room before the site counts as having too many.
            entries.retain(|entry| entry.from.upgrade().is_some());
        }
        if entries.len() == CAPACITY {
            self.full.set(true);
            entries.clear();
            return;
        }
        entries.push(AddEntry {
            from: from.into(),
            to: to.into(),
            index,
            chain,
        });
    }
}

/// The stamps of the prototypes of an object with shape `shape`, nearest first, or `None` if the
/// chain is longer than an entry holds or has an object whose methods are not ordinary.
fn chain_of(shape: &Shape) -> Option<ArrayVec<Stamp, CAPACITY>> {
    let mut chain = ArrayVec::new();
    let mut prototype = shape.prototype();
    while let Some(object) = prototype {
        if !has_ordinary_property_methods(object.vtable()) {
            return None;
        }
        let object = object.try_borrow().ok()?;
        chain.try_push(Stamp::of(object.shape())).ok()?;
        prototype = object.shape().prototype();
    }
    Some(chain)
}
