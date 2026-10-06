//! Consistency check (1) and a byte-identical round trip over the whole
//! vendored suite: every input, valid or invalid, and every truncation of every
//! input builds a syntax tree that agrees with its event stream and emits the
//! input byte for byte. (The harness applies the same check to every case it
//! scores; see `tests/parser.rs` for the score.)
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::parse_with;
use oneyaml_syntax::tree::{SyntaxTree, check_tree};
use test_suite::{default_data_dir, load_suite};

fn check(src: &str, settings: &Settings, what: &str) {
    let events = parse_with(src, settings);
    let tree = SyntaxTree::from_events(events.sourced());
    if let Err(err) = check_tree(&events, &tree) {
        panic!("{what}: {err}\n{src:?}");
    }
    // The pull-parser path builds the same tree.
    assert_eq!(SyntaxTree::parse_with(src, settings), tree, "{what}");
    assert_eq!(tree.emit().as_bytes(), src.as_bytes(), "{what}");
}

#[test]
fn every_suite_input_round_trips_through_the_tree() {
    let cases = load_suite(&default_data_dir()).unwrap();
    assert!(cases.len() > 400);
    let settings = [
        Settings::new(),
        Settings::new().lenient_inline_colons(true),
        Settings::new().limits(Limits::untrusted()),
    ];
    for case in &cases {
        let input = core::str::from_utf8(&case.input).unwrap();
        for s in &settings {
            check(input, s, &case.id);
        }
    }
}

#[test]
fn every_truncated_suite_input_round_trips_through_the_tree() {
    let settings = Settings::new();
    for case in load_suite(&default_data_dir()).unwrap() {
        let Ok(input) = core::str::from_utf8(&case.input) else {
            continue;
        };
        for (i, _) in input.char_indices() {
            check(
                &input[..i],
                &settings,
                &format!("{} truncated at {i}", case.id),
            );
        }
    }
}
