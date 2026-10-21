//! Pins every oracle's standard scores (the table in `docs/conformance.md`).
//! Versions are pinned with `=` in Cargo.toml, so a change here means an
//! adapter or harness change, which must be explained.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use test_suite::standard::{StandardReport, score};
use test_suite::{KnownDefects, default_data_dir, default_known_defects_path, load_suite};

fn reports() -> Vec<StandardReport> {
    let cases = load_suite(&default_data_dir()).unwrap();
    let known = KnownDefects::load(&default_known_defects_path()).unwrap();
    oracles::all()
        .iter_mut()
        .map(|o| score(o.as_mut(), &cases, &known))
        .collect()
}

/// (name, standard valid, standard error, accepts valid, rejects invalid,
/// prefix vendored, prefix corrected, round trips)
type Row = (
    &'static str,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
);

#[test]
fn scores_are_pinned() {
    let expected: [Row; 6] = [
        // Every error prefix matches its corrected expectation; 11 differ
        // from the vendored ones, 4H7K among them (see docs/conformance.md).
        ("oneyaml-syntax", 308, 94, 308, 94, 83, 94, 308),
        ("saphyr-parser", 308, 94, 308, 94, 55, 55, 0),
        // Reproduces the recorded 4H7K prefix, `-DOC` included, which the
        // corrected expectation drops, so its corrected count is one lower
        // than its vendored one.
        ("yaml-rust2", 308, 94, 308, 94, 57, 56, 0),
        // Identical to matrix.yaml.info's c-libyaml-event row (same suite
        // commit 6e6c296a): 252 ok, 78/94 invalid, 330/402. This is the
        // cross-check that the full-fidelity adapter adds no failures.
        ("libyaml (unsafe-libyaml)", 252, 78, 257, 78, 50, 55, 0),
        ("yaml-edit", 0, 25, 308, 25, 0, 0, 308),
        ("yaml-edit + Validator", 0, 94, 290, 94, 0, 0, 308),
    ];
    let actual: Vec<Row> = reports()
        .iter()
        .map(|r| {
            assert_eq!(r.panics(), 0, "{} panicked", r.name);
            assert_eq!(r.truncations(), 0, "{} cut off at the event cap", r.name);
            let valid = if r.profile.is_some() {
                r.standard(false).0
            } else {
                0
            };
            (
                r.name,
                valid,
                r.standard(true).0,
                r.classified(false),
                r.classified(true),
                r.prefix(false),
                r.prefix(true),
                r.round_trips(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

/// The README's generated conformance line is exactly what
/// `cargo run -p oracles --release -- --readme` writes from this scorer
/// and the strict harness today, at whichever ref its link names.
#[test]
fn readme_conformance_line_is_current() {
    use test_suite::producer::ParserProducer;
    use test_suite::{ExpectedCodes, default_codes_path, run_suite_with};

    let root = oracles::readme::workspace_root();
    let readme = std::fs::read_to_string(root.join(oracles::readme::README)).unwrap();
    let shown = oracles::readme::current(&readme).unwrap();
    let git_ref = shown
        .split("/blob/")
        .nth(1)
        .and_then(|rest| rest.split("/docs/conformance.md").next())
        .unwrap();

    let cases = load_suite(&default_data_dir()).unwrap();
    let known = KnownDefects::load(&default_known_defects_path()).unwrap();
    let codes = ExpectedCodes::load(&default_codes_path()).unwrap();
    let ours = score(&mut test_suite::standard::ParserOracle, &cases, &known);
    let strict = run_suite_with(&mut ParserProducer, &cases, None, &codes, &known);
    let release = oracles::readme::release(&default_data_dir()).unwrap();
    let c = oracles::readme::conformance(release, &ours, &strict).unwrap();
    assert_eq!(
        shown,
        oracles::readme::line(&c, git_ref).unwrap(),
        "regenerate: cargo run -p oracles --release -- --readme --ref {git_ref}"
    );
}
