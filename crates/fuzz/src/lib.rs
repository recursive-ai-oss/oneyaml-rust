//! Coverage-guided fuzz targets for oneyaml (extended fuzzing:
//! no unsafe, no panics, a byte-identical round trip, one interpretation).
//!
//! Every target is a plain `fn(&[u8])` that panics exactly when an
//! invariant breaks, so the same code runs three ways:
//!
//! * under libFuzzer (`fuzz_targets/*.rs`, feature `libfuzzer`, driven by
//!   `fuzz.sh`);
//! * as the corpus replay in the normal test gates (`cargo test -p fuzz`,
//!   `tests/replay.rs`), over the committed minimised corpora and every
//!   regression repro;
//! * by hand on one file (`cargo run -p fuzz --bin fuzz-seed -- run
//!   <target> <file>`).
//!
//! See `README.md` for the targets, the input formats, and the commands.
#![forbid(unsafe_code)]
// The reused fuzzes each bring their own copy of their helper modules
// (`generator`, `owners`, `documented`), as their own test crates do in
// `bench/tests`; the copies never mix.
#![allow(clippy::duplicate_mod)]

pub mod extra;
mod reuse;
pub mod targets;

/// A target: its name (the `fuzz_targets/<name>.rs` binary and the
/// `corpus/<name>` directory) and its body.
pub type Target = (&'static str, fn(&[u8]));

/// Every target, in the order `fuzz.sh` runs them.
pub const TARGETS: &[Target] = &[
    ("parse", targets::parse),
    ("consistency", targets::consistency),
    ("from_slice", targets::from_slice),
    ("serde", targets::serde),
    ("edit", targets::edit),
    ("render", targets::render),
    ("ser", targets::ser),
    ("tree_edit", extra::tree_edit),
    ("cap", extra::cap),
    ("handles", extra::handles),
];

/// How many edit-oracle mismatches this process excused as the one
/// documented exception (the `-syntax` tree docs, *Recovered input*, "Known
/// exception"; `bench/tests/known/mod.rs`), over every oracle copy.
#[must_use]
pub fn known_exception_hits() -> usize {
    reuse::known::hits().saturating_add(reuse::stream_edit_fuzz::known::hits())
}

/// Whether the mismatch between the edited tree `edited` and `again`, its
/// re-parse, is the documented exception (counted as a hit).
#[must_use]
pub fn known_exception(
    edited: &oneyaml_syntax::tree::SyntaxTree,
    again: &oneyaml_syntax::tree::SyntaxTree,
) -> bool {
    reuse::known::excused(edited, again)
}

/// The target called `name`.
#[must_use]
pub fn target(name: &str) -> Option<fn(&[u8])> {
    TARGETS.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}
