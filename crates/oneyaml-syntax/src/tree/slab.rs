//! The overlay's slab of inserted arenas.
//!
//! Entries are reached by index in O(1) and **never move**: the first
//! [`INLINE`] entries live inside the slab itself (in the overlay's one
//! box), and the rest in chunks of sizes `INLINE`, `2·INLINE`, `4·INLINE`,
//! …, each allocated once at its final capacity, listed in a fixed array.
//! So adding an entry costs O(1) in the worst case (a growing `Vec` would
//! move every arena when it reallocates: an O(arenas) edit, which the
//! rule that edit cost is independent of edit history does not allow,
//! even amortised, right after a `fork` or `Clone`), and an overlay with at
//! most `INLINE` arenas allocates nothing for them beyond the arenas' own
//! data (the allocation-count gate; the map the overlay
//! had before allocated one node for them). Free entries are threaded into
//! a free list through themselves (no allocation either) and reused;
//! [`Slab::compacted`] rebuilds a slab. Loops only, no recursion.

use super::NONE;

/// Entries kept inside the slab itself.
const INLINE: usize = 4;

/// Chunks past the inline entries: chunk `c` holds `INLINE << c` entries,
/// enough for the whole `u32` index space.
const CHUNKS: usize = 31;

/// An entry: live, or free (with the next free entry, `NONE` at the end).
#[derive(Clone, Debug)]
enum Entry<T> {
    Live(T),
    Free(u32),
}

/// Entries of type `T` by `u32` index (see the module docs).
#[derive(Debug)]
pub(super) struct Slab<T> {
    inline: [Entry<T>; INLINE],
    /// Chunk `c` holds entries `INLINE·2^c .. INLINE·2^(c+1)`, with its
    /// full capacity from its allocation on.
    chunks: [Vec<Entry<T>>; CHUNKS],
    /// Entries ever added (live or free).
    len: u32,
    /// The first free entry (`NONE`: none), and how many there are.
    free_head: u32,
    free_len: u32,
    /// Most entries ever added (tests lower it to exercise running out,
    /// never a panic).
    #[cfg(test)]
    cap: u32,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Self {
            inline: core::array::from_fn(|_| Entry::Free(NONE)),
            chunks: core::array::from_fn(|_| Vec::new()),
            len: 0,
            free_head: NONE,
            free_len: 0,
            #[cfg(test)]
            cap: NONE,
        }
    }
}

impl<T: Clone> Clone for Slab<T> {
    /// A copy whose chunks keep their full capacity, so adding to the
    /// copy never moves an entry either.
    fn clone(&self) -> Self {
        Self {
            inline: self.inline.clone(),
            chunks: core::array::from_fn(|c| {
                let Some(src) = self.chunks.get(c) else {
                    return Vec::new();
                };
                if src.capacity() == 0 {
                    return Vec::new();
                }
                let mut v = Vec::with_capacity(chunk_cap(c));
                v.extend(src.iter().cloned());
                v
            }),
            len: self.len,
            free_head: self.free_head,
            free_len: self.free_len,
            #[cfg(test)]
            cap: self.cap,
        }
    }
}

/// The capacity of chunk `c`.
fn chunk_cap(c: usize) -> usize {
    u32::try_from(c)
        .ok()
        .and_then(|c| INLINE.checked_shl(c))
        .unwrap_or(0)
}

/// Where entry `i` lives: `None` for an inline entry, else its chunk and
/// offset.
#[inline]
fn place(i: u32) -> Option<(usize, usize)> {
    let i = i as usize;
    if i < INLINE {
        return None;
    }
    // i / INLINE ≥ 1: chunk c covers INLINE·2^c ≤ i < INLINE·2^(c+1).
    let q = i / INLINE;
    let c = (usize::BITS - 1).saturating_sub(q.leading_zeros()) as usize;
    let off = i.saturating_sub(chunk_cap(c));
    Some((c, off))
}

impl<T> Slab<T> {
    fn entry(&self, i: u32) -> Option<&Entry<T>> {
        if i >= self.len {
            return None;
        }
        match place(i) {
            None => self.inline.get(i as usize),
            Some((c, off)) => self.chunks.get(c)?.get(off),
        }
    }

    fn entry_mut(&mut self, i: u32) -> Option<&mut Entry<T>> {
        if i >= self.len {
            return None;
        }
        match place(i) {
            None => self.inline.get_mut(i as usize),
            Some((c, off)) => self.chunks.get_mut(c)?.get_mut(off),
        }
    }

    /// Entry `i`. O(1).
    #[inline]
    pub(super) fn get(&self, i: u32) -> Option<&T> {
        match self.entry(i)? {
            Entry::Live(t) => Some(t),
            Entry::Free(_) => None,
        }
    }

    /// Keeps `t` in a free entry or a new one and returns its index;
    /// `None` (`t` dropped) when the index space is exhausted. O(1).
    pub(super) fn insert(&mut self, t: T) -> Option<u32> {
        if self.free_head != NONE {
            let i = self.free_head;
            let entry = self.entry_mut(i)?;
            let Entry::Free(next) = *entry else {
                return None;
            };
            *entry = Entry::Live(t);
            self.free_head = next;
            self.free_len = self.free_len.saturating_sub(1);
            return Some(i);
        }
        let i = self.len;
        #[cfg(test)]
        if i >= self.cap {
            return None;
        }
        if i == NONE {
            return None;
        }
        match place(i) {
            None => {
                *self.inline.get_mut(i as usize)? = Entry::Live(t);
            }
            Some((c, _)) => {
                let chunk = self.chunks.get_mut(c)?;
                if chunk.capacity() == 0 {
                    *chunk = Vec::with_capacity(chunk_cap(c));
                }
                chunk.push(Entry::Live(t));
            }
        }
        self.len = i.saturating_add(1);
        Some(i)
    }

    /// Frees entry `i` and returns what it held. O(1).
    pub(super) fn remove(&mut self, i: u32) -> Option<T> {
        let head = self.free_head;
        let entry = self.entry_mut(i)?;
        if matches!(entry, Entry::Free(_)) {
            return None;
        }
        let Entry::Live(t) = core::mem::replace(entry, Entry::Free(head)) else {
            return None;
        };
        self.free_head = i;
        self.free_len = self.free_len.saturating_add(1);
        Some(t)
    }

    /// Free entries.
    pub(super) fn free_len(&self) -> usize {
        self.free_len as usize
    }

    /// Entries ever added (live or free): what [`Slab::compacted`] visits.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.len as usize
    }

    /// The live entries, in index order.
    #[cfg(test)]
    pub(super) fn iter(&self) -> impl Iterator<Item = &T> {
        fn live<T>(e: &Entry<T>) -> Option<&T> {
            match e {
                Entry::Live(t) => Some(t),
                Entry::Free(_) => None,
            }
        }
        let inline = self.inline.iter().take(self.len as usize).filter_map(live);
        inline.chain(self.chunks.iter().flatten().filter_map(live))
    }

    /// A slab of the live entries only, in index order, with the new index
    /// of each old one (`NONE` for a free entry). O(entries).
    pub(super) fn compacted(self) -> (Self, Vec<u32>) {
        let mut map = Vec::with_capacity(self.len as usize);
        let mut out = Self::default();
        #[cfg(test)]
        {
            out.cap = self.cap;
        }
        let len = self.len as usize;
        let all = self
            .inline
            .into_iter()
            .take(len)
            .chain(self.chunks.into_iter().flatten());
        for e in all {
            super::cost::count(super::cost::Step::Visit);
            match e {
                Entry::Live(t) => map.push(out.insert(t).unwrap_or(NONE)),
                Entry::Free(_) => map.push(NONE),
            }
        }
        (out, map)
    }

    /// Lowers the index space to `cap` entries (tests: running out).
    #[cfg(test)]
    pub(super) fn limit_entries(&mut self, cap: usize) {
        self.cap = u32::try_from(cap).unwrap_or(NONE);
    }

    /// Heap bytes held (for the retention tests): the chunks.
    #[cfg(test)]
    pub(super) fn heap_bytes(&self) -> usize {
        let each = core::mem::size_of::<Entry<T>>();
        self.chunks
            .iter()
            .map(|c| c.capacity().saturating_mul(each))
            .fold(0usize, usize::saturating_add)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn places_cover_the_index_space_in_order() {
        let mut want = (0usize, 0usize);
        for i in INLINE as u32..10_000 {
            let (c, off) = place(i).unwrap();
            assert!(off < chunk_cap(c), "{i}");
            assert_eq!((c, off), want, "{i}");
            want = if off + 1 == chunk_cap(c) {
                (c + 1, 0)
            } else {
                (c, off + 1)
            };
        }
        assert!(place(u32::MAX - 1).unwrap().0 < CHUNKS);
    }

    #[test]
    fn entries_never_move_and_are_reused() {
        let mut s: Slab<u64> = Slab::default();
        let ids: Vec<u32> = (0..100).map(|v| s.insert(v).unwrap()).collect();
        assert_eq!(ids, (0..100).collect::<Vec<u32>>());
        let first = s.get(10).unwrap() as *const u64;
        for v in 100..1000 {
            s.insert(v).unwrap();
        }
        // Chunks never reallocate: entry 10 is where it was.
        assert_eq!(s.get(10).unwrap() as *const u64, first);
        assert_eq!(s.remove(7), Some(7));
        assert_eq!(s.remove(7), None);
        assert_eq!(s.get(7), None);
        assert_eq!(s.remove(500), Some(500));
        assert_eq!(s.free_len(), 2);
        // Last freed, first reused.
        assert_eq!(s.insert(5000).unwrap(), 500);
        assert_eq!(s.insert(5001).unwrap(), 7);
        assert_eq!(s.get(7), Some(&5001));
        assert_eq!(s.free_len(), 0);
        assert_eq!(s.get(1000), None);
        // A copy keeps the capacities: adding to it moves nothing.
        let mut c = s.clone();
        let at = c.get(500).unwrap() as *const u64;
        c.insert(1).unwrap();
        assert_eq!(c.get(500).unwrap() as *const u64, at);
        for (k, ch) in c.chunks.iter().enumerate() {
            assert!(ch.capacity() == 0 || ch.capacity() == chunk_cap(k));
        }
        // Compaction keeps the live entries in order.
        for i in (0..1000).step_by(2) {
            s.remove(i);
        }
        let (s, map) = s.compacted();
        assert_eq!(s.iter().count(), 500);
        assert_eq!(map[0], NONE);
        assert_eq!(s.get(map[1]), Some(&1));
        assert_eq!(s.get(map[999]), Some(&999));
        assert_eq!(s.free_len(), 0);
        assert_eq!(s.len(), 500);
    }

    #[test]
    fn a_few_entries_allocate_nothing() {
        let mut s: Slab<u64> = Slab::default();
        for v in 0..INLINE as u64 {
            s.insert(v).unwrap();
        }
        s.remove(1);
        s.insert(9).unwrap();
        assert_eq!(s.heap_bytes(), 0);
        s.insert(10).unwrap();
        assert_eq!(s.heap_bytes(), INLINE * core::mem::size_of::<Entry<u64>>());
    }

    #[test]
    fn a_limited_slab_refuses_new_entries() {
        let mut s: Slab<u64> = Slab::default();
        s.limit_entries(2);
        assert_eq!(s.insert(1), Some(0));
        assert_eq!(s.insert(2), Some(1));
        assert_eq!(s.insert(3), None);
        s.remove(0);
        assert_eq!(s.insert(4), Some(0));
    }
}
