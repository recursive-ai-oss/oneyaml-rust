//! The `miette` feature: `Error` and `DiagnosticReport` implement
//! `miette::Diagnostic` with code, severity, labels, help (plus fixes),
//! and the code's documentation url. Runs only with `--features miette`.
#![cfg(feature = "miette")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use miette::{Diagnostic as _, NarratableReportHandler, Report};
use oneyaml::{DiagnosticReport, Document, Value, code};

fn narrate(d: &dyn miette::Diagnostic) -> String {
    let mut out = String::new();
    NarratableReportHandler::new()
        .render_report(&mut out, d)
        .unwrap();
    out
}

#[test]
fn a_syntax_error_maps_every_field() {
    let src = "a: b: c\n";
    let err = oneyaml::from_str::<Value>(src).unwrap_err();
    assert_eq!(err.code().to_string(), "Y0027");
    assert_eq!(miette::Diagnostic::code(&err).unwrap().to_string(), "Y0027");
    assert_eq!(err.severity(), Some(miette::Severity::Error));
    let url = err.url().unwrap().to_string();
    assert_eq!(
        url,
        "https://docs.rs/oneyaml-core/latest/oneyaml_core/code/constant.INLINE_COLON_IN_PLAIN_VALUE.html"
    );
    let help = err.help().unwrap().to_string();
    assert!(help.contains("quote the whole value"), "{help}");
    assert!(help.contains("suggestion: quote the value: write `'b: c'`"));
    let labels: Vec<_> = err.labels().unwrap().collect();
    assert_eq!(labels.len(), 2);
    assert!(labels[0].primary());
    assert_eq!((labels[0].offset(), labels[0].len()), (5, 2));
    assert_eq!(labels[1].label(), Some("this `: ` starts a mapping"));

    let report = Report::new(err).with_source_code(src.to_owned());
    let text = narrate(report.as_ref());
    assert!(text.contains("Y0027"), "{text}");
    assert!(text.contains("this `: ` starts a mapping"), "{text}");
}

#[test]
fn a_serde_error_has_its_span_and_no_extra_labels() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct Port {
        port: u16,
    }
    let src = "port: 99999\n";
    let err = oneyaml::from_str::<Port>(src).unwrap_err();
    let labels: Vec<_> = err.labels().unwrap().collect();
    assert_eq!(labels.len(), 1);
    assert_eq!((labels[0].offset(), labels[0].len()), (6, 5));
    // An error without a position (a failed read) has no labels.
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("boom"))
        }
    }
    let err = oneyaml::from_reader::<_, Value>(Broken).unwrap_err();
    assert_eq!(err.code(), code::IO_ERROR);
    assert!(err.span().is_none());
    assert!(err.labels().is_none());
    assert!(err.help().is_none());
}

#[test]
fn document_diagnostics_through_the_wrapper() {
    let src = "a: 1\na: 2\n";
    let parsed = Document::parse_recovering(src);
    let d = parsed.diagnostics()[0].clone();
    assert_eq!(d.code, code::DUPLICATE_KEY);
    let report = DiagnosticReport::new(d).with_source(src);
    assert!(report.source_code().is_some());
    let text = narrate(&report);
    assert!(text.contains("first defined here"), "{text}");
    assert!(text.contains("Y1013"), "{text}");
    assert!(text.contains("constant.DUPLICATE_KEY.html"), "{text}");

    // Warnings map to miette warnings.
    let src = "%YAML 1.1\n---\na: 1\n";
    let parsed = Document::parse_recovering(src);
    let w = DiagnosticReport::from(parsed.diagnostics()[0].clone());
    assert_eq!(w.severity(), Some(miette::Severity::Warning));
    assert_eq!(w.to_string(), parsed.diagnostics()[0].message);
}

#[test]
fn out_of_range_spans_do_not_panic() {
    let d = oneyaml::Diagnostic::error(
        code::CUSTOM,
        "far away",
        oneyaml::Label::new(oneyaml::Span::new(90, 99), "here"),
    );
    let report = DiagnosticReport::new(d).with_source("short\n");
    // miette reports the bad span as a formatting error; never a panic.
    let mut out = String::new();
    let _ = NarratableReportHandler::new().render_report(&mut out, &report);
}

/// The help (with its fixes) is bounded as `render` bounds it:
/// a fix quoting a 1 MB value never repeats it (a 50 MB value gave a
/// 52 MB miette output before).
#[test]
fn help_and_fixes_are_bounded() {
    let src = format!("title: The farm: {}\n", "x".repeat(1 << 20));
    let settings = oneyaml::Settings::new().lenient_inline_colons(true);
    let doc = Document::parse_with(src.as_str(), &settings).unwrap();
    let d = doc.diagnostics()[0].clone();
    assert_eq!(d.code, code::LENIENT_INLINE_COLON);
    assert!(
        d.suggestions
            .iter()
            .any(|s| s.edits.iter().any(|e| e.text.len() > 1 << 20))
    );
    let report = DiagnosticReport::new(d).with_source(src.clone());
    let help = report.help().unwrap().to_string();
    assert!(help.len() < 8 * 1024, "{}", help.len());
    assert!(help.contains('\u{2026}'));
    // The handler shows the source line itself (miette's snippet); the
    // rest of its output is bounded.
    let text = narrate(&report);
    assert!(text.len() < src.len() + 64 * 1024, "{}", text.len());
}
