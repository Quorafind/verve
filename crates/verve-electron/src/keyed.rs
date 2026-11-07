//! [`Keyed`]: the few things a program holds by number (a menu's native copy, a tray icon, a
//! hotkey), in one small map.
//!
//! These tables hold a handful of entries, so a list searched in order beats a tree: the code of
//! each method is shared by every kind of value it holds, where a `BTreeMap` has a copy of its
//! insert and remove for each.

/// A map from numbers to `V`, searched in order. Not for tables that grow past a few dozen entries.
#[derive(Clone, Debug)]
pub struct Keyed<V>(Vec<(u32, V)>);

impl<V> Default for Keyed<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<V> Keyed<V> {
    /// An empty map.
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    fn position(&self, key: u32) -> Option<usize> {
        self.0.iter().position(|(have, _)| *have == key)
    }

    /// The value under `key`.
    pub fn get(&self, key: &u32) -> Option<&V> {
        self.position(*key).map(|at| &self.0[at].1)
    }

    /// The value under `key`, to change.
    pub fn get_mut(&mut self, key: &u32) -> Option<&mut V> {
        self.position(*key).map(|at| &mut self.0[at].1)
    }

    /// Whether there is a value under `key`.
    pub fn contains_key(&self, key: &u32) -> bool {
        self.position(*key).is_some()
    }

    /// Puts `value` under `key`; the value it replaced, if any.
    pub fn insert(&mut self, key: u32, value: V) -> Option<V> {
        match self.position(key) {
            Some(at) => Some(std::mem::replace(&mut self.0[at].1, value)),
            None => {
                self.0.push((key, value));
                None
            }
        }
    }

    /// Takes the value under `key` out.
    pub fn remove(&mut self, key: &u32) -> Option<V> {
        self.position(*key).map(|at| self.0.remove(at).1)
    }

    /// Takes every value out.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The keys, in the order they were put in.
    pub fn keys(&self) -> impl Iterator<Item = &u32> {
        self.0.iter().map(|(key, _)| key)
    }

    /// The values, in the order they were put in.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.0.iter().map(|(_, value)| value)
    }

    /// The keys and values, in the order they were put in.
    pub fn iter(&self) -> impl Iterator<Item = (&u32, &V)> {
        self.0.iter().map(|(key, value)| (key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_holds_its_latest_value_until_it_is_removed() {
        let mut map = Keyed::new();
        assert_eq!(map.insert(3, "a"), None);
        assert_eq!(map.insert(3, "b"), Some("a"));
        assert_eq!(map.insert(1, "c"), None);
        assert_eq!(map.get(&3), Some(&"b"));
        assert!(map.contains_key(&1));
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), [3, 1]);
        assert_eq!(map.remove(&3), Some("b"));
        assert_eq!(map.remove(&3), None);
        assert_eq!(map.values().count(), 1);
    }
}
