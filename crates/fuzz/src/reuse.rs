//! The deterministic fuzzes and differentials of `bench/tests`, included by
//! path so that the coverage-guided targets check **exactly** the
//! invariants they check (one source of truth for each oracle): the
//! consistency checks, the `Document`, comment, and `Stream` edit-script
//! fuzzes, and the `from_value` / node-read differentials.
//!
//! Only their `#[test]` entry points (compiled out here) choose inputs;
//! the targets hand them fuzzer bytes instead. Items only those entry
//! points use are dead code in this crate.

#[allow(dead_code, unused_imports)]
#[path = "../../bench/tests/consistency.rs"]
pub(crate) mod consistency;

#[allow(dead_code, unused_imports)]
#[path = "../../bench/tests/edit_fuzz.rs"]
pub(crate) mod edit_fuzz;

#[allow(dead_code, unused_imports)]
#[path = "../../bench/tests/comment_fuzz.rs"]
pub(crate) mod comment_fuzz;

#[allow(dead_code, unused_imports)]
#[path = "../../bench/tests/stream_edit_fuzz.rs"]
pub(crate) mod stream_edit_fuzz;

#[allow(dead_code, unused_imports)]
#[path = "../../bench/tests/value_differential.rs"]
pub(crate) mod value_differential;

#[allow(dead_code, unused_imports)]
#[path = "../../bench/tests/cap_invariance.rs"]
pub(crate) mod cap_invariance;

/// The documented exception the edit oracles excuse.
#[path = "../../bench/tests/known/mod.rs"]
pub(crate) mod known;

/// The edit battery of the syntax tree (its edits and how they
/// are applied), for the `tree_edit` target.
#[allow(dead_code, unused_imports)]
#[path = "../../oneyaml-syntax/tests/reparse_sweep.rs"]
pub(crate) mod reparse_sweep;

#[allow(unused_imports)]
#[path = "../../oneyaml/tests/ser_common/mod.rs"]
pub(crate) mod ser_common;
