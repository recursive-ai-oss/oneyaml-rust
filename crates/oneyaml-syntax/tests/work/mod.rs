//! Deterministic cost counters for the scaling tests.
//!
//! A scaling test states that an operation costs the same on a small and on
//! a huge input. It counts the steps the syntax tree takes (overlay lookups,
//! treap edges, compaction visits) instead of measuring time, so the result
//! is the same on every machine and under any load. A step count that
//! doubles when the input grows a hundredfold is a bug whatever the clock
//! says.
#![allow(dead_code)]

/// Forgets the steps counted so far on this thread.
pub fn reset() {
    let _ = oneyaml_syntax::tree::take_work();
}

/// The steps counted on this thread since [`reset`] (or the last call).
pub fn taken() -> u64 {
    oneyaml_syntax::tree::take_work()
}

/// The steps `f` takes on this thread.
pub fn of<R>(f: impl FnOnce() -> R) -> u64 {
    reset();
    let _ = f();
    taken()
}

/// How many times more steps the large run took than the small one. A run
/// that takes no steps (a refusal that never reads the tree) counts as one,
/// so zero against a large count still fails.
pub fn ratio(_name: &str, small: u64, large: u64) -> f64 {
    large as f64 / small.max(1) as f64
}
