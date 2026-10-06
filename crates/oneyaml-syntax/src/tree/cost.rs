//! Deterministic cost counters of an edited tree's reads.
//!
//! The reads of an edited tree take two kinds of non-constant steps:
//!
//! * a **lookup**: an O(log) query of the overlay's maps (the runs by first
//!   slot, the link overrides, the arenas by first slot) or a treap query
//!   from the root (a prefix sum, a byte search);
//! * an **edge**: one move along a treap edge while stepping from a run to
//!   the next or previous one (amortised O(1) per run over a traversal).
//!
//! Edits count a third kind, a **visit**: an entry a compaction of the
//! arena slab touches (compactions are paid for by the frees
//! that trigger them, so visits per edit stay bounded).
//!
//! Tests count them to state the cost of sequential reads independently
//! of the machine: a full read of a tree with R runs takes O(R) lookups and
//! edges, whatever the number of elements. In library builds `count` is an
//! empty inline function: no state exists outside `cfg(test)` (there is
//! no interior mutability in the tree; the test counters are thread-local,
//! never part of `SyntaxTree`).

/// A kind of step counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// An O(log) query of an overlay map or from the treap's root.
    Lookup,
    /// One treap edge walked between neighbouring runs.
    Edge,
    /// One entry visited by a compaction of the arena slab (a treap item
    /// renumbered, a slab entry moved).
    Visit,
}

#[cfg(test)]
thread_local! {
    static COUNTS: core::cell::Cell<(u64, u64)> = const { core::cell::Cell::new((0, 0)) };
    static VISITS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

/// Counts one step (tests only; nothing in library builds).
#[inline(always)]
pub(super) fn count(step: Step) {
    #[cfg(test)]
    match step {
        Step::Visit => VISITS.with(|v| v.set(v.get().saturating_add(1))),
        _ => COUNTS.with(|c| {
            let (l, e) = c.get();
            c.set(match step {
                Step::Lookup => (l.saturating_add(1), e),
                _ => (l, e.saturating_add(1)),
            });
        }),
    }
    #[cfg(not(test))]
    let _ = step;
}

/// The steps counted on this thread since the last call, as `(lookups,
/// edges)`, and resets them.
#[cfg(test)]
pub(super) fn take() -> (u64, u64) {
    COUNTS.with(|c| c.replace((0, 0)))
}

/// The compaction visits counted on this thread since the last call, and
/// resets them.
#[cfg(test)]
pub(super) fn take_visits() -> u64 {
    VISITS.with(|v| v.replace(0))
}
