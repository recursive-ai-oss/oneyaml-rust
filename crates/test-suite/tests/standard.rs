//! Our parser under the standard (comparison) criterion. The strict score is
//! pinned in `tests/parser.rs`; this pins the standard columns the
//! comparison table in `docs/conformance.md` reports for us.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use test_suite::standard::{ParserOracle, score};
use test_suite::{KnownDefects, default_data_dir, default_known_defects_path, load_suite};

#[test]
fn parser_standard_scores() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let known = KnownDefects::load(&default_known_defects_path()).unwrap();
    let r = score(&mut ParserOracle, &cases, &known);
    assert_eq!(r.standard(false), (308, 308));
    assert_eq!(r.standard(true), (94, 94));
    assert_eq!(r.panics(), 0);
    // Every valid input's event stream reproduces its bytes (C1).
    assert_eq!(r.round_trips(), 308);
    // Pre-error events: exact against the corrected expectations for every
    // error case; against the vendored ones, every corrected case is a
    // mismatch (4H7K's recorded `-DOC` before the error closes a document
    // that is still open, see tests/parser.rs), so the result changes
    // exactly for the corrected cases.
    assert_eq!(r.prefix(true), 94);
    assert_eq!(r.prefix(false), 94 - known.len());
    let wrong: Vec<&str> = r
        .cases
        .iter()
        .filter(|c| c.prefix_corrected == Some(false))
        .map(|c| c.id.as_str())
        .collect();
    assert!(wrong.is_empty(), "prefix mismatches: {wrong:?}");
    let differ: Vec<&str> = r
        .cases
        .iter()
        .filter(|c| c.prefix_vendored != c.prefix_corrected)
        .map(|c| c.id.as_str())
        .collect();
    let expected: Vec<&str> = known.ids();
    assert_eq!(differ, expected);
}
