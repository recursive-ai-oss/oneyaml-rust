//! Gate coverage: oneyaml's optional `miette` feature is built,
//! linted and tested by the per-package gates too, not only by
//! `cargo test -p oneyaml --features miette` (`miette`
//! support behind a feature flag). The full mapping is pinned by
//! oneyaml's own `tests/miette_diag.rs`; this checks the feature through a
//! dependent crate, as a user enables it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use miette::{Diagnostic as _, NarratableReportHandler};
use oneyaml::{DiagnosticReport, Document, Value, code};

#[test]
fn errors_and_document_diagnostics_are_miette_diagnostics() {
    let err = oneyaml::from_str::<Value>("a: b: c\n").unwrap_err();
    assert_eq!(miette::Diagnostic::code(&err).unwrap().to_string(), "Y0027");
    assert_eq!(err.severity(), Some(miette::Severity::Error));

    let src = "a: 1\na: 2\n";
    let parsed = Document::parse_recovering(src);
    let d = parsed.diagnostics()[0].clone();
    assert_eq!(d.code, code::DUPLICATE_KEY);
    let report = DiagnosticReport::new(d).with_source(src);
    let mut out = String::new();
    NarratableReportHandler::new()
        .render_report(&mut out, &report)
        .unwrap();
    assert!(out.contains("Y1013"), "{out}");
}
