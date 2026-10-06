//! Settings against the vendored suite and the adversarial corpus:
//!
//! * Limits that real inputs never hit: no suite input hits a
//!   `Limits::default()` limit (nor an `untrusted()` one).
//! * Limits that refuse adversarial input: every adversarial input is rejected
//!   by `Limits::untrusted()` with its limit code, at its layer (the parser, or
//!   reading the meaning for the alias budget), and `default()` accepts or
//!   rejects it as the corpus documents.
//! * `lenient_inline_colons`: off, the suite scores exactly as the
//!   default parser; on, only ZCZ6 changes (it becomes one string and a
//!   warning).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml_core::{CodeRange, Limits, Settings};
use oneyaml_syntax::event::EventKind;
use oneyaml_syntax::{check_contract, parse, parse_with};
use test_suite::adversarial::{Adversarial, Layer, corpus};
use test_suite::producer::{ParserProducer, SettingsProducer};
use test_suite::{
    ExpectedCodes, KnownDefects, default_codes_path, default_data_dir, default_known_defects_path,
    load_suite, run_suite_with,
};

fn inputs() -> Vec<(String, String)> {
    load_suite(&default_data_dir())
        .unwrap()
        .into_iter()
        .map(|c| {
            (
                c.id,
                String::from_utf8(c.input).expect("the suite is UTF-8"),
            )
        })
        .collect()
}

#[test]
fn no_suite_input_hits_a_limit() {
    for limits in [Limits::default(), Limits::untrusted()] {
        let settings = Settings::new().limits(limits.clone());
        for (id, input) in inputs() {
            let events = parse_with(&input, &settings);
            let hit: Vec<String> = events
                .iter()
                .filter_map(|e| e.diagnostic())
                .filter(|d| d.code.range() == CodeRange::Limits)
                .map(|d| d.to_string())
                .collect();
            assert!(hit.is_empty(), "{id} under {limits:?}: {hit:?}");
        }
    }
}

/// A meaning-layer case: the parser (and so the tree) accepts it;
/// reading its `Value` under `untrusted()` fails with its code, and under
/// `default()` as documented.
fn check_meaning_case(case: &Adversarial) {
    let untrusted = Settings::new()
        .limits(Limits::untrusted())
        .merge_keys(case.merge_keys);
    let default = Settings::new().merge_keys(case.merge_keys);
    for s in [&untrusted, &default] {
        let events = parse_with(&case.input, s);
        check_contract(&case.input, &events).unwrap();
        assert!(!events.iter().any(|e| e.is_error()), "{}", case.name);
    }
    let e = oneyaml::from_str_with::<oneyaml::Value>(&case.input, &untrusted).unwrap_err();
    assert_eq!(e.code(), case.code, "{}: {e}", case.name);
    let help = e.diagnostic().help.as_deref().unwrap_or("");
    assert!(help.starts_with("raise Limits::"), "{}: {help}", case.name);
    let got = oneyaml::from_str_with::<oneyaml::Value>(&case.input, &default).map_err(|e| e.code());
    match case.default {
        None => assert!(got.is_ok(), "{} under default(): {got:?}", case.name),
        Some(code) => assert_eq!(got.err(), Some(code), "{} under default()", case.name),
    }
}

#[test]
fn untrusted_rejects_the_adversarial_corpus() {
    let untrusted = Settings::new().limits(Limits::untrusted());
    let default = Settings::new();
    let mut meaning = 0usize;
    for case in corpus() {
        if case.layer == Layer::Meaning {
            check_meaning_case(&case);
            meaning += 1;
            continue;
        }
        assert_eq!(
            case.default, None,
            "{}: parser cases pass default()",
            case.name
        );
        let events = parse_with(&case.input, &untrusted);
        check_contract(&case.input, &events).unwrap();
        let first = events
            .iter()
            .find(|e| e.is_error())
            .and_then(|e| e.diagnostic())
            .unwrap_or_else(|| panic!("{}: not rejected", case.name));
        assert_eq!(first.code, case.code, "{}: {first}", case.name);
        assert!(
            first
                .help
                .as_deref()
                .is_some_and(|h| h.starts_with("raise Limits::")),
            "{}: {:?}",
            case.name,
            first.help
        );
        // The same input is fine under the default limits.
        let events = parse_with(&case.input, &default);
        let errors: Vec<String> = events
            .iter()
            .filter(|e| e.is_error())
            .filter_map(|e| e.diagnostic().map(ToString::to_string))
            .collect();
        assert!(
            errors.is_empty(),
            "{} under default(): {errors:?}",
            case.name
        );
    }
    // Billion laughs, alias-to-merge-key amplification, nested chains;
    // copies of a large scalar and a repeated `%TAG` prefix.
    assert_eq!(meaning, 5);
}

#[test]
fn lenient_off_scores_like_the_default_parser() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let codes = ExpectedCodes::load(&default_codes_path()).unwrap();
    let known = KnownDefects::load(&default_known_defects_path()).unwrap();
    let base = run_suite_with(&mut ParserProducer, &cases, None, &codes, &known);
    let mut off = SettingsProducer::new("off", Settings::new().lenient_inline_colons(false));
    let off = run_suite_with(&mut off, &cases, None, &codes, &known);
    // 401: 4H7K fails under every setting (see tests/parser.rs).
    assert_eq!(off.passed(), 401);
    assert_eq!(off.passing_ids(), base.passing_ids());
}

#[test]
fn lenient_on_changes_only_zcz6() {
    let lenient = Settings::new().lenient_inline_colons(true);
    let mut changed = Vec::new();
    for (id, input) in inputs() {
        if parse(&input) != parse_with(&input, &lenient) {
            changed.push(id);
        }
    }
    assert_eq!(changed, ["ZCZ6"]);

    // ZCZ6 (`a: b: c: d`) becomes one string plus a warning.
    let src = "a: b: c: d\n";
    let events = parse_with(src, &lenient);
    let semantic: Vec<String> = events
        .sourced()
        .filter_map(|e| match e.kind() {
            EventKind::Scalar { .. } => Some(format!("={}", e.value().unwrap())),
            EventKind::Warning(d) | EventKind::Error(d) => Some(d.code.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(semantic, ["=a", "=b: c: d", "Y0028"]);

    // Scored with the setting on, ZCZ6 is the one case that no longer
    // passes: it is an error case in the suite and now yields no error.
    // (4H7K fails with or without the setting, see tests/parser.rs.)
    let cases = load_suite(&default_data_dir()).unwrap();
    let codes = ExpectedCodes::load(&default_codes_path()).unwrap();
    let known = KnownDefects::load(&default_known_defects_path()).unwrap();
    let report = run_suite_with(
        &mut SettingsProducer::lenient(),
        &cases,
        None,
        &codes,
        &known,
    );
    let failed: Vec<(&str, &str)> = report
        .results
        .iter()
        .filter_map(|r| r.outcome.as_ref().err().map(|f| (r.id.as_str(), f.label())))
        .collect();
    assert_eq!(
        failed,
        [("4H7K", "error-prefix-mismatch"), ("ZCZ6", "missing-error")]
    );
    assert_eq!(report.passed(), 400);
}

#[test]
fn every_truncated_suite_input_conforms_when_lenient() {
    let lenient = Settings::new().lenient_inline_colons(true);
    for (id, input) in inputs() {
        for (i, _) in input.char_indices() {
            let src = &input[..i];
            let events = parse_with(src, &lenient);
            if let Err(err) = check_contract(src, &events) {
                panic!("{id} truncated at {i}: {err}");
            }
        }
    }
}
