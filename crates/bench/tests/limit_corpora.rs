//! The two limit corpora (no real-world input hits a default limit; every
//! adversarial input is refused under `untrusted()`), under every profile and
//! preset:
//!
//! 1. **Real-world corpus vs `default()`**: every input of the real-world
//!    corpus reads cleanly, so none hits a `Limits::default()` limit — the
//!    bench corpus's real-world shapes (Kubernetes, GitHub Actions,
//!    docker-compose with merge keys, ~300-document k8s stream, OpenAPI at
//!    100 KB and 10 MB),
//!    read by the parser, the `Stream`/`Document` tree, and serde (`Value`,
//!    every document), with the default settings, both compat presets, and
//!    merge keys. (The yaml-test-suite half of this check is
//!    `test-suite/tests/settings.rs`; the migration corpus's half is
//!    `oracles/tests/compat_differential.rs`.)
//! 2. **Adversarial corpus vs `untrusted()`**: every input is rejected with
//!    its `Y2xxx` code (`test-suite/tests/settings.rs`, parser and meaning
//!    layers); here, additionally, **compat presets never change limits**:
//!    under `Compat::serde_yaml()` and `Compat::yaml_1_1()` every adversarial
//!    input meets exactly the limit it meets under `Settings::new()`, and
//!    under the presets with `Limits::untrusted()` exactly its `untrusted()`
//!    code.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use bench::corpus::{CASES, Kind};
use oneyaml::{Code, CodeRange, Compat, Limits, Settings, Stream, Value};
use serde::Deserialize;
use test_suite::adversarial::{Adversarial, Layer, corpus};

/// The first limit diagnostic the parser reports, if any.
fn parser_limit(src: &str, settings: &Settings) -> Option<Code> {
    oneyaml_syntax::parse_with(src, settings)
        .iter()
        .filter_map(|e| e.diagnostic())
        .find(|d| d.code.range() == CodeRange::Limits)
        .map(|d| d.code)
}

/// Every problem a real-world input has under `settings`, all documents
/// and all three readers (empty: read cleanly). A real-world input must
/// not merely avoid limits: it must read.
fn problems(src: &str, settings: &Settings) -> Vec<String> {
    let mut out = Vec::new();
    // The parser: no error event at all (a limit is one).
    out.extend(
        oneyaml_syntax::parse_with(src, settings)
            .iter()
            .filter(|e| e.is_error())
            .filter_map(|e| e.diagnostic().map(|d| format!("parser: {d}"))),
    );
    // The tree: the stream parses, and every document converts (each one
    // is read, so a failing document does not hide the next).
    let mut tree_docs = 0usize;
    match Stream::parse_with(src, settings) {
        Err(e) => out.push(format!("stream: {e}")),
        Ok(stream) => {
            for (i, d) in stream.documents().enumerate() {
                tree_docs = tree_docs.saturating_add(1);
                if let Err(e) = d.to_value() {
                    out.push(format!("tree document {i}: {e}"));
                }
            }
        }
    }
    // Serde: every document of the stream deserializes (the iterator stops
    // after an error, so the count is checked against the tree's).
    let mut serde_docs = 0usize;
    for (i, doc) in oneyaml::Deserializer::from_str_with(src, settings).enumerate() {
        serde_docs = serde_docs.saturating_add(1);
        if let Err(e) = Value::deserialize(doc) {
            out.push(format!("serde document {i}: {e}"));
        }
    }
    if serde_docs != tree_docs {
        out.push(format!(
            "serde read {serde_docs} documents, the tree {tree_docs}"
        ));
    }
    out
}

fn presets() -> [(&'static str, Settings); 4] {
    [
        ("default", Settings::new()),
        ("merge_keys", Settings::new().merge_keys(true)),
        ("serde_yaml", Compat::serde_yaml()),
        ("yaml_1_1", Compat::yaml_1_1()),
    ]
}

#[test]
fn real_world_corpus_hits_no_default_limit() {
    let mut checked = 0usize;
    let mut bytes = 0usize;
    for case in CASES.iter().filter(|c| c.kind == Kind::RealWorld) {
        let src = case.generate(1);
        bytes = bytes.saturating_add(src.len());
        for (name, settings) in presets() {
            // The presets keep the default limits.
            assert_eq!(settings.get_limits(), &Limits::default(), "{name}");
            let found = problems(&src, &settings);
            assert!(found.is_empty(), "{}/{name}: {found:?}", case.name);
            checked += 1;
        }
    }
    // Seven real-world shapes (helm-values added later), including the
    // 10 MB OpenAPI document.
    assert_eq!(checked, 7 * 4);
    assert!(bytes > 11 * 1024 * 1024, "{bytes} bytes");
}

/// The limit an adversarial input meets under `settings`, at its layer.
fn adversarial_limit(case: &Adversarial, settings: &Settings) -> Option<Code> {
    let settings = settings
        .clone()
        .merge_keys(case.merge_keys || settings.get_merge_keys());
    match case.layer {
        Layer::Parse => parser_limit(&case.input, &settings),
        _ => oneyaml::from_str_with::<Value>(&case.input, &settings)
            .err()
            .filter(|e| e.code().range() == CodeRange::Limits)
            .map(|e| e.code()),
    }
}

#[test]
fn compat_presets_never_change_limits_on_the_adversarial_corpus() {
    let cases = corpus();
    assert!(cases.len() >= 17, "{} cases", cases.len());
    for case in &cases {
        let default = adversarial_limit(case, &Settings::new());
        assert_eq!(default, case.default, "{}: default()", case.name);
        let untrusted = adversarial_limit(case, &Settings::new().limits(Limits::untrusted()));
        assert_eq!(untrusted, Some(case.code), "{}: untrusted()", case.name);
        for (name, preset) in [
            ("serde_yaml", Compat::serde_yaml()),
            ("yaml_1_1", Compat::yaml_1_1()),
        ] {
            assert_eq!(
                adversarial_limit(case, &preset),
                default,
                "{}: {name} preset",
                case.name
            );
            assert_eq!(
                adversarial_limit(case, &preset.limits(Limits::untrusted())),
                Some(case.code),
                "{}: {name} preset with untrusted()",
                case.name
            );
        }
    }
}

/// The largest alias byte charge of any document of `src` under
/// `settings`: the smallest fixed `alias_byte_budget` every document of
/// the stream reads within (binary search; `None` if even 64 MiB fails).
fn byte_charge(src: &str, settings: &Settings) -> Option<usize> {
    let reads = |budget: usize| {
        let limits = settings.get_limits().clone().alias_byte_budget(budget);
        let s = settings.clone().limits(limits);
        oneyaml::Deserializer::from_str_with(src, &s).all(|d| Value::deserialize(d).is_ok())
    };
    if reads(0) {
        return Some(0);
    }
    let (mut lo, mut hi) = (0usize, 64 << 20);
    if !reads(hi) {
        return None;
    }
    // reads(hi) && !reads(lo)
    while hi.saturating_sub(lo) > 1 {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if reads(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    Some(hi)
}

/// The real-world corpus reads under the alias byte budget of
/// every profile (default and `untrusted()`), with every preset, and its
/// measured expansion ratios (bytes charged / input bytes) stay far below
/// the `untrusted()` factor of 2 (printed for the report).
#[test]
fn real_world_corpus_fits_every_alias_byte_budget() {
    for case in CASES.iter().filter(|c| c.kind == Kind::RealWorld) {
        let src = case.generate(1);
        for (name, settings) in presets() {
            let charge = byte_charge(&src, &settings)
                .unwrap_or_else(|| panic!("{}/{name}: over 64 MiB", case.name));
            let ratio = charge as f64 / src.len() as f64;
            println!(
                "{}/{name}: {} input bytes, {charge} bytes charged ({ratio:.4}x)",
                case.name,
                src.len()
            );
            assert!(ratio < 2.0, "{}/{name}: {ratio}", case.name);
            assert!(charge <= Limits::untrusted().get_alias_byte_budget(0).unwrap());
            // And under `untrusted()`'s byte budget the documents read.
            let bytes_only = Limits::default().alias_byte_budget_ratio(2, 8 << 20);
            let s = settings.clone().limits(bytes_only);
            let found = problems(&src, &s);
            assert!(found.is_empty(), "{}/{name}: {found:?}", case.name);
        }
    }
}
