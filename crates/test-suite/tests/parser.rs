//! The real parser against the vendored suite: every stream conforms to the
//! event contract (C1-C6), including for every truncation of every input
//! (no panic), and the score does not regress.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml_syntax::{check_contract, parse};
use test_suite::producer::ParserProducer;
use test_suite::{
    ExpectedCodes, KnownDefects, default_codes_path, default_data_dir, default_known_defects_path,
    load_suite, run_suite_with,
};

#[test]
fn parser_streams_always_conform() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let codes = ExpectedCodes::load(&default_codes_path()).unwrap();
    let known = KnownDefects::load(&default_known_defects_path()).unwrap();
    let report = run_suite_with(&mut ParserProducer, &cases, None, &codes, &known);
    let contract: Vec<&str> = report
        .results
        .iter()
        .filter(|r| r.outcome.as_ref().is_err_and(|f| f.label() == "contract"))
        .map(|r| r.id.as_str())
        .collect();
    assert!(contract.is_empty(), "contract failures: {contract:?}");
    // Ratchet (all genuine; 9 cases are judged exactly against
    // corrected expectations, see known-defects.txt). The one failure is
    // 4H7K: the recorded `-DOC` before the error on `[ a, b, c ] ]` is kept
    // (the reference parser emits it) and no principled timing rule
    // produces it without contradicting 9JBA's recorded tree.
    assert_eq!(report.passed(), 401, "passed {}", report.passed());
    assert_eq!(
        report.split(false).0,
        308,
        "valid {:?}",
        report.split(false)
    );
    assert_eq!(report.split(true).0, 93, "error {:?}", report.split(true));
    assert_eq!(report.failed(), 1, "failed {}", report.failed());
    let failing: Vec<(&str, &str)> = report
        .results
        .iter()
        .filter_map(|r| r.outcome.as_ref().err().map(|f| (r.id.as_str(), f.label())))
        .collect();
    assert_eq!(failing, [("4H7K", "error-prefix-mismatch")]);
    assert_eq!(report.corrected(), known.len());
    // Every passing error case has its diagnosis pinned.
    let unpinned: Vec<&str> = report
        .results
        .iter()
        .filter(|r| r.expect_error && r.outcome.is_ok() && codes.get(&r.id).is_none())
        .map(|r| r.id.as_str())
        .collect();
    assert!(
        unpinned.is_empty(),
        "passing error cases without codes: {unpinned:?}"
    );
}

#[test]
fn every_truncated_suite_input_conforms() {
    for case in load_suite(&default_data_dir()).unwrap() {
        let Ok(input) = core::str::from_utf8(&case.input) else {
            continue;
        };
        for (i, _) in input.char_indices() {
            let src = &input[..i];
            let events = parse(src);
            if let Err(err) = check_contract(src, &events) {
                panic!("{} truncated at {i}: {err}", case.id);
            }
        }
    }
}
