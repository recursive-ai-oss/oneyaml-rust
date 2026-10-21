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
    /// One navigation call of a [`Node`](super::Node) (parent, child,
    /// sibling, or one step of a children, descendants or walk iterator),
    /// counted for the tests' totals only.
    Nav,
}

#[cfg(any(test, feature = "__internal-checks"))]
thread_local! {
    static COUNTS: core::cell::Cell<(u64, u64)> = const { core::cell::Cell::new((0, 0)) };
    static VISITS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
    static NAVS: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
    static REREAD: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
    static DECODED: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

/// Counts one step (tests only; nothing in library builds).
#[inline(always)]
pub(super) fn count(step: Step) {
    #[cfg(any(test, feature = "__internal-checks"))]
    match step {
        Step::Visit => VISITS.with(|v| v.set(v.get().saturating_add(1))),
        Step::Nav => NAVS.with(|v| v.set(v.get().saturating_add(1))),
        _ => COUNTS.with(|c| {
            let (l, e) = c.get();
            c.set(match step {
                Step::Lookup => (l.saturating_add(1), e),
                _ => (l, e.saturating_add(1)),
            });
        }),
    }
    #[cfg(not(any(test, feature = "__internal-checks")))]
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

/// Every step counted on this thread since the last call (lookups, edges,
/// compaction visits and navigation calls together), and resets them.
#[cfg(feature = "__internal-checks")]
pub(super) fn take_total() -> u64 {
    let (lookups, edges) = COUNTS.with(|c| c.replace((0, 0)));
    let visits = VISITS.with(|v| v.replace(0));
    let navs = NAVS.with(|v| v.replace(0));
    lookups
        .saturating_add(edges)
        .saturating_add(visits)
        .saturating_add(navs)
}

/// Counts `bytes` of edited text re-read to validate an edit (tests only;
/// nothing in library builds).
#[inline(always)]
pub(super) fn count_reread(bytes: u64) {
    #[cfg(any(test, feature = "__internal-checks"))]
    REREAD.with(|v| v.set(v.get().saturating_add(bytes)));
    #[cfg(not(any(test, feature = "__internal-checks")))]
    let _ = bytes;
}

/// The bytes counted by [`count_reread`] on this thread since the last
/// call, and resets them.
#[cfg(feature = "__internal-checks")]
pub(super) fn take_reread() -> u64 {
    REREAD.with(|v| v.replace(0))
}

/// Counts `bytes` of scalar source text decoded by [`Node::value`](super::Node::value)
/// (tests only; nothing in library builds).
#[inline(always)]
pub(super) fn count_decoded(bytes: usize) {
    #[cfg(any(test, feature = "__internal-checks"))]
    DECODED.with(|v| {
        v.set(
            v.get()
                .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX)),
        )
    });
    #[cfg(not(any(test, feature = "__internal-checks")))]
    let _ = bytes;
}

/// The bytes counted by [`count_decoded`] on this thread since the last
/// call, and resets them.
#[cfg(feature = "__internal-checks")]
pub(super) fn take_decoded() -> u64 {
    DECODED.with(|v| v.replace(0))
}
