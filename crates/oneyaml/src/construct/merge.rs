//! Merge-key accumulation in O(n log n) for any shape.
//!
//! A mapping's meaning under merge keys is "its explicit entries, then the
//! entries of its merge sources that it does not have, earlier sources
//! first" (keys.rs). Computed naively, each level of a merge chain
//! (`{<<: {<<: … {k0: 0 … kN: N}}}`) copies everything it inherits: O(n ×
//! depth). [`MergeAcc`] makes the union cheap instead:
//!
//! * entries live in **chunks**, in order in a `VecDeque`, so
//!   a whole accumulation can be put in front of or behind another without
//!   moving the other's entries;
//! * an entry shadowed by an earlier one is **tombstoned** (`None`) in
//!   place, found through a key index;
//! * [`MergeAcc::append_later`] always moves the **smaller** side into the
//!   larger (small-to-large), so every entry moves O(log n) times in total.
//!
//! A merge chain then costs O(1) per level, and the one mapping whose
//! meaning is kept is materialised once. Both the `Value` builder and the
//! serde planner use this type with their own key and payload types, so
//! they follow the same rules in the same order.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hash};

#[cfg(test)]
thread_local! {
    /// Entries stored by [`MergeAcc`]s on this thread (the scaling tests).
    pub(crate) static MOVES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A chunk key and a position in that chunk.
type Pos = (i64, usize);

/// The positions of the live entries whose keys hash alike.
enum Slot {
    One(Pos),
    Many(Vec<Pos>),
}

impl Slot {
    fn positions(&self) -> &[Pos] {
        match self {
            Slot::One(p) => std::slice::from_ref(p),
            Slot::Many(v) => v,
        }
    }
}

/// Ordered entries with unique keys, built for cheap ordered unions.
///
/// The index maps key hashes to positions and compares keys in place, so
/// keys are stored once and never cloned (a complex key can be large).
pub(crate) struct MergeAcc<K, V> {
    /// Chunks in order; `None` is a shadowed entry. Chunk `i` has the
    /// stable key `front + i`.
    chunks: VecDeque<Vec<Option<(K, V)>>>,
    /// Key hash → positions of the live entries with that hash (hashed with
    /// `std`'s randomly seeded hasher).
    index: HashMap<u64, Slot>,
    hasher: RandomState,
    live: usize,
    /// The key of the first chunk.
    front: i64,
}

impl<K: Hash + Eq, V> Default for MergeAcc<K, V> {
    fn default() -> Self {
        Self {
            chunks: VecDeque::new(),
            index: HashMap::new(),
            hasher: RandomState::new(),
            live: 0,
            front: 0,
        }
    }
}

impl<K: Hash + Eq, V> MergeAcc<K, V> {
    /// Entries in order; a repeated key keeps its first occurrence.
    pub(crate) fn from_entries(entries: impl IntoIterator<Item = (K, V)>) -> Self {
        let mut acc = Self::default();
        acc.put(false, entries);
        acc
    }

    /// Number of live entries.
    pub(crate) fn len(&self) -> usize {
        self.live
    }

    /// The deque position of the chunk with key `key`.
    fn slot_of(&self, key: i64) -> Option<usize> {
        usize::try_from(key.checked_sub(self.front)?).ok()
    }

    fn entry(&self, (c, i): Pos) -> Option<&Option<(K, V)>> {
        self.chunks.get(self.slot_of(c)?)?.get(i)
    }

    /// The position of the live entry with key `key` (hash `h`).
    fn find(&self, h: u64, key: &K) -> Option<Pos> {
        let slot = self.index.get(&h)?;
        slot.positions()
            .iter()
            .copied()
            .find(|p| matches!(self.entry(*p), Some(Some((k, _))) if k == key))
    }

    /// Whether `key` is a live entry.
    #[cfg(test)]
    pub(crate) fn contains(&self, key: &K) -> bool {
        self.find(self.hasher.hash_one(key), key).is_some()
    }

    /// Stores the entries whose keys are new as a new first (`front`) or
    /// last chunk, if any.
    fn put(&mut self, front: bool, entries: impl IntoIterator<Item = (K, V)>) {
        // The chunk is stored first, so `find` sees its keys too.
        let at = if front {
            let Some(at) = self.front.checked_sub(1) else {
                return;
            };
            self.chunks.push_front(Vec::new());
            self.front = at;
            at
        } else {
            let len = i64::try_from(self.chunks.len()).unwrap_or(i64::MAX);
            let Some(at) = self.front.checked_add(len) else {
                return;
            };
            self.chunks.push_back(Vec::new());
            at
        };
        for (k, v) in entries {
            let h = self.hasher.hash_one(&k);
            if self.find(h, &k).is_some() {
                continue;
            }
            let chunk = if front {
                self.chunks.front_mut()
            } else {
                self.chunks.back_mut()
            };
            let Some(chunk) = chunk else {
                break;
            };
            #[cfg(test)]
            MOVES.with(|c| c.set(c.get().saturating_add(1)));
            let pos = (at, chunk.len());
            chunk.push(Some((k, v)));
            match self.index.get_mut(&h) {
                None => {
                    self.index.insert(h, Slot::One(pos));
                }
                Some(slot) => match slot {
                    Slot::One(p) => *slot = Slot::Many(vec![*p, pos]),
                    Slot::Many(v) => v.push(pos),
                },
            }
            self.live = self.live.saturating_add(1);
        }
        // An empty chunk is dropped again.
        if front {
            if self.chunks.front().is_some_and(Vec::is_empty) {
                self.chunks.pop_front();
                self.front = self.front.saturating_add(1);
            }
        } else if self.chunks.back().is_some_and(Vec::is_empty) {
            self.chunks.pop_back();
        }
    }

    /// Tombstones `key` if it is live.
    fn shadow(&mut self, key: &K) {
        let h = self.hasher.hash_one(key);
        let Some(pos) = self.find(h, key) else {
            return;
        };
        let empty = match self.index.get_mut(&h) {
            Some(Slot::Many(v)) => {
                v.retain(|p| *p != pos);
                v.is_empty()
            }
            _ => true,
        };
        if empty {
            self.index.remove(&h);
        }
        let entry = self
            .slot_of(pos.0)
            .and_then(|c| self.chunks.get_mut(c))
            .and_then(|ch| ch.get_mut(pos.1));
        if let Some(e) = entry {
            *e = None;
            self.live = self.live.saturating_sub(1);
        }
    }

    /// `self`, then the entries of `later` whose keys `self` does not have
    /// (earlier wins, order kept). Moves the smaller side: O(min(|self|,
    /// |later|)) plus shadowed entries, each dropped once.
    #[must_use]
    pub(crate) fn append_later(mut self, later: Self) -> Self {
        self.absorb_later(later);
        self
    }

    /// [`MergeAcc::append_later`] in place (no allocation of its own, so a
    /// boxed accumulation stays in its box).
    pub(crate) fn absorb_later(&mut self, mut later: Self) {
        if later.len() == 0 {
            return;
        }
        if self.len() >= later.len() {
            self.put(false, later.into_entries());
        } else {
            for (k, _) in self.chunks.iter().flatten().flatten() {
                later.shadow(k);
            }
            let earlier = std::mem::replace(self, later);
            self.put(true, earlier.into_entries());
        }
    }

    /// The live entries, in order.
    pub(crate) fn into_entries(self) -> impl Iterator<Item = (K, V)> {
        self.chunks.into_iter().flatten().flatten()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn acc(pairs: &[(&'static str, i32)]) -> MergeAcc<&'static str, i32> {
        MergeAcc::from_entries(pairs.iter().copied())
    }

    fn out(a: MergeAcc<&'static str, i32>) -> Vec<(&'static str, i32)> {
        a.into_entries().collect()
    }

    #[test]
    fn earlier_wins_order_kept_both_directions() {
        // Larger earlier side: the later one is appended.
        let a = acc(&[("a", 1), ("b", 1), ("c", 1)]).append_later(acc(&[("b", 2), ("d", 2)]));
        assert_eq!(out(a), [("a", 1), ("b", 1), ("c", 1), ("d", 2)]);
        // Larger later side: the earlier one is prepended, shadowing.
        let a = acc(&[("b", 1)]).append_later(acc(&[("a", 2), ("b", 2), ("c", 2)]));
        assert_eq!(out(a), [("b", 1), ("a", 2), ("c", 2)]);
        // Repeated keys in one list: the first stays.
        assert_eq!(out(acc(&[("x", 1), ("x", 2)])), [("x", 1)]);
        let e: MergeAcc<&str, i32> = MergeAcc::default();
        assert_eq!(e.len(), 0);
        let a = e.append_later(acc(&[("q", 1)]));
        assert!(a.contains(&"q"));
        assert_eq!(out(a.append_later(MergeAcc::default())), [("q", 1)]);
    }

    #[test]
    fn repeated_unions_match_a_naive_model() {
        // Many prepends and appends, against a Vec model.
        let mut a = acc(&[("k0", 0)]);
        let mut model = vec![("k0", 0)];
        let keys = ["k0", "k1", "k2", "k3", "k4", "k5", "k6", "k7"];
        for i in 0..40i32 {
            let k = keys[(i as usize * 5) % keys.len()];
            let side = acc(&[(k, i), (keys[(i as usize) % keys.len()], -i)]);
            let side_model: Vec<_> = out(acc(&[(k, i), (keys[(i as usize) % keys.len()], -i)]));
            if i % 3 == 0 {
                // `side` earlier.
                a = side.append_later(a);
                let mut m = side_model.clone();
                m.extend(
                    model
                        .iter()
                        .filter(|(x, _)| !side_model.iter().any(|(y, _)| y == x)),
                );
                model = m;
            } else {
                a = a.append_later(side);
                let extra: Vec<_> = side_model
                    .into_iter()
                    .filter(|(x, _)| !model.iter().any(|(y, _)| y == x))
                    .collect();
                model.extend(extra);
            }
        }
        assert_eq!(out(a), model);
    }
}
