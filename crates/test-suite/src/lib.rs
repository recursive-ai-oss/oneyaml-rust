//! yaml-test-suite conformance harness for oneyaml (unpublished).
//!
//! Loads the vendored suite ([`case`]), drives any [`producer::EventProducer`]
//! over each case, converts the resulting lossless event stream to the suite's
//! event-tree notation ([`notation`]), and scores it ([`runner`]):
//!
//! * every stream must satisfy the event-stream contract
//!   (`oneyaml_syntax::event::check_contract`), valid or not;
//! * a **valid** case passes when it has no error event and its tree notation
//!   equals `test.event`;
//! * an **invalid** case passes when it has an error event and the events
//!   before the first error render exactly to `test.event`;
//! * when [`ExpectedCodes`] has an entry for the case, the emitted diagnostic
//!   codes must match it.
//!
//! [`standard`] scores any parser (ours or a competitor's) under the weaker,
//! commonly published criterion, for comparison tables; the strict rules above
//! are unaffected.
//!
//! Run it with `cargo run -p test-suite -- [--case ID] [--verbose]`.
#![forbid(unsafe_code)]

pub mod adversarial;
pub mod case;
pub mod codes;
pub mod known;
pub mod notation;
pub mod producer;
pub mod runner;
pub mod standard;

pub use case::{Case, LoadError, default_data_dir, load_suite};
pub use codes::{ExpectedCodes, default_codes_path};
pub use known::{Correction, KnownDefects, default_known_defects_path};
pub use notation::{NotationError, to_tree_notation, to_tree_notation_prefix};
pub use producer::{EventProducer, Produced, StubProducer};
pub use runner::{
    CaseResult, Failure, Report, run_case, run_case_expecting, run_suite, run_suite_with,
};
