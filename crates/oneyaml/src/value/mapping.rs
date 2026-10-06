//! [`Mapping`]: an insertion-ordered map with any-[`Value`] keys.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{Hash, Hasher};

use super::Value;
use super::walk;

/// One mapping entry. `digest` is the key's structural digest
/// ([`walk::digest`]), computed once on insertion: keys are immutable while
/// in a mapping, so the cache never goes stale. Re-indexing, lookups,
/// equality, and the digest of an enclosing mapping never re-walk a
/// (possibly deep) key, so building nested complex keys stays linear.
pub(crate) struct Entry {
    pub(crate) digest: u64,
    pub(crate) key: Value,
    pub(crate) value: Value,
}

/// Positions of all entries whose key hash is equal.
#[derive(Clone)]
enum Slot {
    One(usize),
    Many(Vec<usize>),
}

/// An insertion-ordered YAML mapping with any-[`Value`] keys.
///
/// Entries live in a `Vec` in insertion order; an index built on `std`'s
/// `HashMap` with its default, randomly seeded `RandomState` (hash-flooding
/// resistant) makes lookups O(1). Iteration always follows insertion
/// order.
///
/// Equality and hashing are **order-independent** (YAML 1.2.2 §3.2.1.1:
/// mapping keys are unordered): two mappings are equal when they hold
/// the same keys with equal values, in any order, and equal mappings hash
/// alike. So `? {a: 1, b: 2}` and `? {b: 2, a: 1}` are the same key
/// (duplicate-key detection relies on this). Iteration order is still
/// insertion order.
///
/// Both run in expected O(n) time: an entry is matched through the other
/// mapping's index, and a mapping's hash combines its entries' hashes
/// commutatively. Dropping, cloning, comparing, hashing, and
/// debug-formatting a mapping never recurse, however deeply its keys and
/// values nest.
///
/// ```
/// use oneyaml::{Mapping, Value};
///
/// let mut m = Mapping::with_capacity(4);
/// assert!(m.capacity() >= 4);
/// m.insert("image".into(), "nginx".into());
/// m.insert("replicas".into(), 2.into());
/// m.insert(Value::from(80), "http".into()); // any-`Value` keys
/// assert!(m.contains_key(&Value::from(80)));
/// assert_eq!(m.get_str("image"), Some(&Value::from("nginx")));
/// for v in m.values_mut() {
///     if let Some(n) = v.as_i64() {
///         *v = Value::from(n + 1);
///     }
/// }
/// for (k, v) in m.iter_mut() {
///     if k.as_str() == Some("image") {
///         *v = "nginx:1.27".into();
///     }
/// }
/// let values: Vec<&Value> = m.values().collect();
/// assert_eq!(values, [&Value::from("nginx:1.27"), &Value::from(3), &Value::from("http")]);
/// assert_eq!(m.remove_entry(&Value::from(80)), Some((Value::from(80), Value::from("http"))));
/// m.clear();
/// assert!(m.is_empty());
/// ```
pub struct Mapping {
    pub(crate) entries: Vec<Entry>,
    /// Key digest → positions of the entries with that digest.
    index: HashMap<u64, Slot, RandomState>,
}

/// A key to look up: a whole `Value`, or a string without allocating one.
#[derive(Clone, Copy)]
pub(crate) enum KeyRef<'a> {
    Value(&'a Value),
    Str(&'a str),
}

impl Hash for KeyRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Exactly what `Value::hash` writes for the same key.
        state.write_u64(self.digest());
    }
}

impl KeyRef<'_> {
    /// The key's structural digest; `Str` digests exactly like
    /// `Value::String`.
    fn digest(self) -> u64 {
        match self {
            KeyRef::Value(v) => walk::digest(v),
            KeyRef::Str(s) => walk::str_digest(s),
        }
    }

    fn matches(self, key: &Value) -> bool {
        match self {
            KeyRef::Value(v) => walk::eq(v, key),
            KeyRef::Str(s) => matches!(key, Value::String(k) if k == s),
        }
    }
}

impl Mapping {
    /// An empty mapping.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// An empty mapping with room for `capacity` entries.
    ///
    /// `capacity` is a hint: if that much cannot be reserved (e.g.
    /// `usize::MAX`), the mapping starts smaller and grows on demand
    /// instead of panicking.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut m = Self {
            entries: Vec::new(),
            index: HashMap::with_hasher(RandomState::new()),
        };
        // `Vec::with_capacity` panics on capacity overflow.
        if m.entries.try_reserve_exact(capacity).is_err() || m.index.try_reserve(capacity).is_err()
        {
            m.entries = Vec::new();
            m.index = HashMap::with_hasher(RandomState::new());
        }
        m
    }

    /// The capacity of the entry storage.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Positions of the entries whose key digest is `digest`.
    pub(crate) fn positions(&self, digest: u64) -> &[usize] {
        match self.index.get(&digest) {
            None => &[],
            Some(Slot::One(i)) => std::slice::from_ref(i),
            Some(Slot::Many(v)) => v,
        }
    }

    pub(crate) fn find_hashed(&self, digest: u64, key: KeyRef<'_>) -> Option<usize> {
        self.positions(digest).iter().copied().find(|i| {
            self.entries
                .get(*i)
                .is_some_and(|e| e.digest == digest && key.matches(&e.key))
        })
    }

    pub(crate) fn find(&self, key: KeyRef<'_>) -> Option<usize> {
        if self.entries.is_empty() {
            return None;
        }
        self.find_hashed(key.digest(), key)
    }

    /// The value for `key`.
    #[must_use]
    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.find(KeyRef::Value(key))
            .and_then(|i| self.entries.get(i))
            .map(|e| &e.value)
    }

    /// The value for a string key, without building a `Value`.
    #[must_use]
    pub fn get_str(&self, key: &str) -> Option<&Value> {
        self.find(KeyRef::Str(key))
            .and_then(|i| self.entries.get(i))
            .map(|e| &e.value)
    }

    /// The value for `key`, mutably.
    pub fn get_mut(&mut self, key: &Value) -> Option<&mut Value> {
        self.find(KeyRef::Value(key))
            .and_then(|i| self.entries.get_mut(i))
            .map(|e| &mut e.value)
    }

    pub(crate) fn get_mut_by(&mut self, key: KeyRef<'_>) -> Option<&mut Value> {
        self.find(key)
            .and_then(|i| self.entries.get_mut(i))
            .map(|e| &mut e.value)
    }

    /// True when `key` is present.
    #[must_use]
    pub fn contains_key(&self, key: &Value) -> bool {
        self.find(KeyRef::Value(key)).is_some()
    }

    /// The entry at insertion position `i`.
    #[must_use]
    pub fn get_index(&self, i: usize) -> Option<(&Value, &Value)> {
        self.entries.get(i).map(|e| (&e.key, &e.value))
    }

    /// Inserts `key: value`. If `key` was present, its value is replaced in
    /// place (the entry keeps its position) and the old value is returned.
    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        self.insert_full(key, value).1
    }

    /// [`Mapping::insert`] with a forged key digest, to force collisions.
    #[cfg(test)]
    pub(crate) fn insert_hashed(&mut self, hash: u64, key: Value, value: Value) -> Option<Value> {
        self.insert_hashed_full(hash, key, value).1
    }

    /// [`Mapping::insert`], also returning the entry's position.
    pub(crate) fn insert_full(&mut self, key: Value, value: Value) -> (usize, Option<Value>) {
        let digest = walk::digest(&key);
        self.insert_hashed_full(digest, key, value)
    }

    fn insert_hashed_full(
        &mut self,
        hash: u64,
        key: Value,
        value: Value,
    ) -> (usize, Option<Value>) {
        if let Some(i) = self.find_hashed(hash, KeyRef::Value(&key)) {
            if let Some(e) = self.entries.get_mut(i) {
                return (i, Some(std::mem::replace(&mut e.value, value)));
            }
        }
        let pos = self.entries.len();
        self.entries.push(Entry {
            digest: hash,
            key,
            value,
        });
        match self.index.get_mut(&hash) {
            None => {
                self.index.insert(hash, Slot::One(pos));
            }
            Some(slot) => match slot {
                Slot::One(first) => *slot = Slot::Many(vec![*first, pos]),
                Slot::Many(v) => v.push(pos),
            },
        }
        (pos, None)
    }

    /// Removes `key`, returning its value. Later entries keep their order
    /// (O(n), like `IndexMap::shift_remove`).
    pub fn remove(&mut self, key: &Value) -> Option<Value> {
        self.remove_entry(key).map(|(_, v)| v)
    }

    /// Removes `key`, returning the stored key and value. Later entries keep
    /// their order (O(n)).
    pub fn remove_entry(&mut self, key: &Value) -> Option<(Value, Value)> {
        let pos = self.find(KeyRef::Value(key))?;
        self.shift_remove_index(pos)
    }

    /// Removes the entry at position `pos`, shifting later entries down.
    pub(crate) fn shift_remove_index(&mut self, pos: usize) -> Option<(Value, Value)> {
        if pos >= self.entries.len() {
            return None;
        }
        let removed = self.entries.remove(pos);
        self.unindex(removed.digest, pos);
        // Shift the positions of every later entry down by one.
        for (j, e) in self.entries.iter().enumerate().skip(pos) {
            let old = j.saturating_add(1);
            if let Some(slot) = self.index.get_mut(&e.digest) {
                match slot {
                    Slot::One(i) if *i == old => *i = j,
                    Slot::Many(v) => v.iter_mut().filter(|i| **i == old).for_each(|i| *i = j),
                    Slot::One(_) => {}
                }
            }
        }
        let Entry { key, value, .. } = removed;
        Some((key, value))
    }

    fn unindex(&mut self, hash: u64, pos: usize) {
        let drop_slot = match self.index.get_mut(&hash) {
            Some(Slot::One(i)) => *i == pos,
            Some(slot @ Slot::Many(_)) => {
                if let Slot::Many(v) = slot {
                    v.retain(|i| *i != pos);
                    if let [only] = v.as_slice() {
                        *slot = Slot::One(*only);
                    }
                }
                false
            }
            None => false,
        };
        if drop_slot {
            self.index.remove(&hash);
        }
    }

    /// Removes every entry.
    pub fn clear(&mut self) {
        walk::drop_all(std::mem::take(&mut self.entries));
        self.index.clear();
    }

    /// Iterates entries in insertion order.
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            inner: self.entries.iter(),
        }
    }

    /// Iterates entries in insertion order with mutable values (keys stay
    /// immutable so the index stays valid).
    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&Value, &mut Value)> {
        self.entries.iter_mut().map(|e| (&e.key, &mut e.value))
    }

    /// Iterates keys in insertion order.
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &Value> {
        self.entries.iter().map(|e| &e.key)
    }

    /// Iterates values in insertion order.
    pub fn values(&self) -> impl ExactSizeIterator<Item = &Value> {
        self.entries.iter().map(|e| &e.value)
    }

    /// Iterates values in insertion order, mutably.
    pub fn values_mut(&mut self) -> impl ExactSizeIterator<Item = &mut Value> {
        self.entries.iter_mut().map(|e| &mut e.value)
    }

    /// Takes all entries out, leaving the mapping empty.
    pub(crate) fn take_entries(&mut self) -> Vec<Entry> {
        self.index.clear();
        std::mem::take(&mut self.entries)
    }

    /// A mapping with `entries` (already cloned, same digests, same order)
    /// and a clone of `like`'s index and hasher.
    pub(crate) fn from_cloned(entries: Vec<Entry>, like: &Mapping) -> Self {
        Self {
            entries,
            index: like.index.clone(),
        }
    }
}

impl Default for Mapping {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Mapping {
    // Iterative teardown; derived drop glue would recurse per level.
    fn drop(&mut self) {
        walk::drop_all(std::mem::take(&mut self.entries));
    }
}

impl Clone for Mapping {
    fn clone(&self) -> Self {
        let entries = self
            .entries
            .iter()
            .map(|e| Entry {
                digest: e.digest,
                key: walk::clone_value(&e.key),
                value: walk::clone_value(&e.value),
            })
            .collect();
        Self::from_cloned(entries, self)
    }
}

impl PartialEq for Mapping {
    fn eq(&self, other: &Self) -> bool {
        walk::eq_mappings(self, other)
    }
}

impl Eq for Mapping {}

impl Hash for Mapping {
    /// Order-independent, consistent with `==` (see the type docs).
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(walk::digest_mapping(self));
    }
}

impl fmt::Debug for Mapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        walk::debug_mapping(self, f)
    }
}

impl FromIterator<(Value, Value)> for Mapping {
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(iter: I) -> Self {
        let mut m = Mapping::new();
        m.extend(iter);
        m
    }
}

impl Extend<(Value, Value)> for Mapping {
    fn extend<I: IntoIterator<Item = (Value, Value)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

/// An owning iterator over a mapping's entries, in insertion order.
pub struct IntoIter {
    inner: std::vec::IntoIter<Entry>,
}

impl Iterator for IntoIter {
    type Item = (Value, Value);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|Entry { key, value, .. }| (key, value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for IntoIter {}

impl IntoIterator for Mapping {
    type Item = (Value, Value);
    type IntoIter = IntoIter;
    fn into_iter(mut self) -> IntoIter {
        IntoIter {
            inner: self.take_entries().into_iter(),
        }
    }
}

/// A borrowing iterator over a mapping's entries, in insertion order.
pub struct Iter<'a> {
    inner: std::slice::Iter<'a, Entry>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Value, &'a Value);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|e| (&e.key, &e.value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a Mapping {
    type Item = (&'a Value, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}
