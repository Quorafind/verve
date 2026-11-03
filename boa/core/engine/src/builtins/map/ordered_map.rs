//! Implements a map type that preserves insertion order.

use crate::{JsData, JsValue, object::JsObject};
use boa_gc::{Finalize, Trace, custom_trace};
use indexmap::{Equivalent, IndexMap};
use std::{
    fmt::Debug,
    hash::{Hash, Hasher},
};

#[derive(PartialEq, Eq, Clone, Debug)]
pub(crate) enum MapKey {
    Key(JsValue),
    Empty(usize), // Necessary to ensure empty keys are still unique.
}

// This ensures that a MapKey::Key(value) hashes to the same as value. The derived PartialEq implementation still holds.
impl Hash for MapKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Key(v) => v.hash(state),
            Self::Empty(e) => e.hash(state),
        }
    }
}

impl Equivalent<MapKey> for JsValue {
    fn equivalent(&self, key: &MapKey) -> bool {
        match key {
            MapKey::Key(v) => v == self,
            MapKey::Empty(_) => false,
        }
    }
}

/// A structure wrapping `indexmap::IndexMap`.
#[derive(Finalize, JsData)]
pub struct OrderedMap<V> {
    map: IndexMap<MapKey, Option<V>>,
    lock: u32,
    empty_count: usize,
}

impl<V: Clone> Clone for OrderedMap<V> {
    fn clone(&self) -> Self {
        // The lock counts the iterators of the map this copy was taken from; it says nothing about
        // the copy, which nobody iterates yet. Carrying it over left the copy — `Set.prototype
        // .difference` and its siblings copy a set that may be being iterated — locked for good,
        // and a locked map never compacts.
        Self {
            map: self.map.clone(),
            lock: 0,
            empty_count: self.empty_count,
        }
    }
}

unsafe impl<V: Trace> Trace for OrderedMap<V> {
    custom_trace!(this, mark, {
        for (k, v) in &this.map {
            if let MapKey::Key(key) = k {
                mark(key);
            }
            mark(v);
        }
    });
}

impl<V: Debug> Debug for OrderedMap<V> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        self.map.fmt(formatter)
    }
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> OrderedMap<V> {
    /// Creates a new empty `OrderedMap`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: IndexMap::new(),
            lock: 0,
            empty_count: 0,
        }
    }

    /// Creates a new empty `OrderedMap` with the specified capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map: IndexMap::with_capacity(capacity),
            lock: 0,
            empty_count: 0,
        }
    }

    /// Return the number of key-value pairs in the map, including empty values.
    ///
    /// Computes in **O(1)** time.
    #[must_use]
    pub fn full_len(&self) -> usize {
        self.map.len()
    }

    /// Gets the number of key-value pairs in the map, not including empty values.
    ///
    /// Computes in **O(1)** time.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len() - self.empty_count
    }

    /// Returns true if the map contains no elements.
    ///
    /// Computes in **O(1)** time.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert a key-value pair in the map.
    ///
    /// If an equivalent key already exists in the map: the key remains and
    /// retains in its place in the order, its corresponding value is updated
    /// with `value` and the older value is returned inside `Some(_)`.
    ///
    /// If no equivalent key existed in the map: the new key-value pair is
    /// inserted, last in order, and `None` is returned.
    ///
    /// Computes in **O(1)** time (amortized average).
    pub fn insert(&mut self, key: JsValue, value: V) -> Option<V> {
        self.map.insert(MapKey::Key(key), Some(value)).flatten()
    }

    /// Remove the key-value pair equivalent to `key` and return
    /// its value.
    ///
    /// The pair is replaced by a tombstone, so the entries after it keep their place in the
    /// order; [`full_len`][Self::full_len] counts the tombstones, [`len`][Self::len] does not.
    ///
    /// Return `None` if `key` is not in map.
    ///
    /// Computes in **O(1)** time (amortized average).
    pub fn remove(&mut self, key: &JsValue) -> Option<V> {
        if !self.map.contains_key(key) {
            return None;
        }

        // `shift_remove` moves every entry after the removed one, which made deleting half the
        // keys of a map quadratic. A tombstone leaves a hole instead, O(1); the iterators skip
        // it, so the order they see is the same.
        self.map.insert(MapKey::Empty(self.empty_count), None);
        self.empty_count += 1;
        let value = self.map.swap_remove(key).flatten();
        self.compact();
        value
    }

    /// Removes all elements from the map and resets the counter of
    /// empty entries.
    ///
    /// An iterator holds the lock and reads entries by index, and the specification only empties
    /// each entry's key and value rather than removing it, so a locked map keeps the length and
    /// replaces every entry with a tombstone: the iterator keeps counting from where it was and
    /// goes on to see entries inserted after the `clear()`.
    pub fn clear(&mut self) {
        if self.lock == 0 {
            self.map.clear();
            self.map.shrink_to_fit();
            self.empty_count = 0;
            return;
        }

        let len = self.map.len();
        self.map = IndexMap::with_capacity(len);
        for index in 0..len {
            self.map.insert(MapKey::Empty(index), None);
        }
        self.empty_count = len;
    }

    /// Return a reference to the value stored for `key`, if it is present,
    /// else `None`.
    ///
    /// Computes in **O(1)** time (average).
    pub fn get(&self, key: &JsValue) -> Option<&V> {
        self.map.get(key).and_then(Option::as_ref)
    }

    /// Get a key-value pair by index.
    ///
    /// Valid indices are `0 <= index < self.full_len()`.
    ///
    /// Computes in O(1) time.
    #[must_use]
    pub fn get_index(&self, index: usize) -> Option<(&JsValue, &V)> {
        if let (MapKey::Key(key), Some(value)) = self.map.get_index(index)? {
            Some((key, value))
        } else {
            None
        }
    }

    /// Return an iterator over the key-value pairs of the map, in their order
    pub fn iter(&self) -> impl Iterator<Item = (&JsValue, &V)> {
        self.map.iter().filter_map(|o| {
            if let (MapKey::Key(key), Some(value)) = o {
                Some((key, value))
            } else {
                None
            }
        })
    }

    /// Return `true` if an equivalent to `key` exists in the map.
    ///
    /// Computes in **O(1)** time (average).
    #[must_use]
    pub fn contains_key(&self, key: &JsValue) -> bool {
        self.map.contains_key(key)
    }

    /// Increases the lock counter.
    ///
    /// This allows objects to be removed from the map during iteration without affecting the indexes until the iteration has completed.
    pub(crate) fn lock(&mut self) {
        self.lock += 1;
    }

    /// Decreases the lock counter and, if 0, removes all empty entries.
    pub(crate) fn unlock(&mut self) {
        self.lock -= 1;
        if self.lock == 0 {
            self.drop_tombstones();
        }
    }

    /// Removes the tombstones once they outnumber the entries that are left.
    ///
    /// Compaction is O(n) and halves the map, so it is paid for by the deletions that filled the
    /// tombstones — amortized O(1) per `remove`. An iterator reads entries by index and holds the
    /// lock, so the compaction waits for it to be released; an iterator that runs to its end
    /// releases it in `next`, which is what lets a `for…of` or an `Array.from` be followed by
    /// deletions that compact.
    fn compact(&mut self) {
        if self.lock == 0 && self.empty_count * 2 > self.map.len() {
            self.drop_tombstones();
        }
    }

    fn drop_tombstones(&mut self) {
        self.map.retain(|k, _| matches!(k, MapKey::Key(_)));
        self.empty_count = 0;
    }
}

pub(crate) struct MapLock<'a, V: Trace + 'static>(&'a JsObject<OrderedMap<V>>);

impl<'a, V: Trace + 'static> MapLock<'a, V> {
    pub(crate) fn new(js_object: &'a JsObject<OrderedMap<V>>) -> Self {
        js_object.borrow_mut().data_mut().lock();
        Self(js_object)
    }
}

impl<V: Trace + 'static> Drop for MapLock<'_, V> {
    fn drop(&mut self) {
        self.0.borrow_mut().data_mut().unlock();
    }
}
