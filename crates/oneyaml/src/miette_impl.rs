//! `miette` integration behind the `miette` feature (rendering;
//! `miette` is the one optional diagnostics dependency).
//!
//! [`Error`] implements [`miette::Diagnostic`] directly. [`Diagnostic`]
//! lives in `oneyaml-core`, which has no `miette` dependency,
//! so the orphan rule rules out implementing the trait for it here; a
//! [`DiagnosticReport`] wraps one instead.
//!
//! Mapping: the code is `Y####`, the severity maps one to one, labels keep
//! their spans (primary label marked primary), help is the diagnostic's
//! help followed by one line per suggested fix, and the url is the code's
//! documentation page ([`docs_url`]).

use std::fmt;

use miette::{LabeledSpan, SourceCode};
use oneyaml_core::{Applicability, Code, Diagnostic, Label, Severity, Suggestion};

use crate::Error;
use crate::error::message;

/// The documentation page of `code`: its constant in `oneyaml-core`'s
/// code registry on docs.rs (each registry constant documents the code's
/// meaning). `None` for a code that is not registered.
fn docs_url(code: Code) -> Option<String> {
    let info = code.info()?;
    Some(format!(
        "https://docs.rs/oneyaml-core/latest/oneyaml_core/code/constant.{}.html",
        info.name
    ))
}

fn severity(s: Severity) -> miette::Severity {
    match s {
        Severity::Warning => miette::Severity::Warning,
        _ => miette::Severity::Error,
    }
}

fn labeled(label: &Label, primary: bool) -> LabeledSpan {
    // Bounded as `render` bounds it.
    let text = (!label.message.is_empty()).then(|| message(format_args!("{}", label.message)));
    let span = label.span.start..label.span.end.max(label.span.start);
    if primary {
        LabeledSpan::new_primary_with_span(text, span)
    } else {
        LabeledSpan::new_with_span(text, span)
    }
}

/// The help text: the diagnostic's help, then one line per suggested fix
/// (miette has no notion of fixes; editors should read
/// [`Diagnostic::suggestions`] instead).
///
/// Bounded as [`crate::Render`] bounds it: the help and each
/// fix's message at most 3 KiB (then `… [message truncated]`), a fix's
/// new text at most 60 characters (then `…`), so a fix that rewrites a
/// 50 MB value never repeats it.
fn help_text(d: &Diagnostic) -> Option<String> {
    let mut out = d
        .help
        .as_ref()
        .map(|h| message(format_args!("{h}")))
        .unwrap_or_default();
    for Suggestion {
        message: text,
        edits,
        applicability,
        ..
    } in &d.suggestions
    {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(if *applicability == Applicability::MachineApplicable {
            "suggestion: "
        } else {
            "suggestion (may be incorrect): "
        });
        out.push_str(&message(format_args!("{text}")));
        if let [edit] = edits.as_slice() {
            if !edit.text.is_empty() {
                out.push_str(": write `");
                out.push_str(&crate::render::excerpt(&edit.text));
                out.push('`');
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn labels(d: &Diagnostic, has_span: bool) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
    let primary = has_span.then(|| labeled(&d.primary, true));
    let secondary = d.secondary.iter().map(|l| labeled(l, false));
    let all: Vec<LabeledSpan> = primary.into_iter().chain(secondary).collect();
    (!all.is_empty()).then(|| Box::new(all.into_iter()) as Box<dyn Iterator<Item = LabeledSpan>>)
}

#[cfg_attr(docsrs, doc(cfg(feature = "miette")))]
impl miette::Diagnostic for Error {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code()))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(severity(self.diagnostic().severity))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        help_text(self.diagnostic()).map(|h| Box::new(h) as Box<dyn fmt::Display>)
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        docs_url(self.code()).map(|u| Box::new(u) as Box<dyn fmt::Display>)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        labels(self.diagnostic(), self.span().is_some())
    }
}

/// A [`Diagnostic`] as a [`miette::Diagnostic`] (feature `miette`),
/// optionally carrying the source text its spans point into.
///
/// `oneyaml::Error` implements `miette::Diagnostic` itself; this wrapper
/// serves the diagnostics of [`Document::diagnostics`](crate::Document::diagnostics),
/// [`Parsed::diagnostics`](crate::Parsed::diagnostics) and
/// [`from_str_with_diagnostics`](crate::from_str_with_diagnostics).
///
/// ```
/// use oneyaml::{Document, DiagnosticReport};
///
/// let src = "a: b: c\n";
/// let parsed = Document::parse_recovering(src);
/// let report = DiagnosticReport::new(parsed.diagnostics()[0].clone()).with_source(src);
/// assert_eq!(report.diagnostic().code, oneyaml::code::INLINE_COLON_IN_PLAIN_VALUE);
/// assert_eq!(report.clone().into_diagnostic(), parsed.diagnostics()[0]);
/// let report = miette::Report::new(report);
/// assert_eq!(report.code().unwrap().to_string(), "Y0027");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(docsrs, doc(cfg(feature = "miette")))]
pub struct DiagnosticReport {
    diagnostic: Diagnostic,
    source: Option<String>,
}

impl DiagnosticReport {
    /// Wraps `diagnostic`, with no source attached.
    #[must_use]
    pub fn new(diagnostic: Diagnostic) -> Self {
        Self {
            diagnostic,
            source: None,
        }
    }

    /// Attaches the source text, so a `miette` handler can draw snippets
    /// without `Report::with_source_code`.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The wrapped diagnostic.
    #[must_use]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }

    /// The wrapped diagnostic, by value.
    #[must_use]
    pub fn into_diagnostic(self) -> Diagnostic {
        self.diagnostic
    }
}

impl From<Diagnostic> for DiagnosticReport {
    fn from(diagnostic: Diagnostic) -> Self {
        Self::new(diagnostic)
    }
}

impl fmt::Display for DiagnosticReport {
    /// The message and, when known, ` at <path>` (the code and severity are
    /// shown by the `miette` handler).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Bounded as `Error`'s `Display` is.
        f.write_str(&message(format_args!("{}", self.diagnostic.message)))?;
        if let Some(path) = self.diagnostic.path.as_ref().filter(|p| !p.is_empty()) {
            write!(f, " at {}", crate::error::shown_path(path))?;
        }
        Ok(())
    }
}

impl std::error::Error for DiagnosticReport {}

#[cfg_attr(docsrs, doc(cfg(feature = "miette")))]
impl miette::Diagnostic for DiagnosticReport {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.diagnostic.code))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(severity(self.diagnostic.severity))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        help_text(&self.diagnostic).map(|h| Box::new(h) as Box<dyn fmt::Display>)
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        docs_url(self.diagnostic.code).map(|u| Box::new(u) as Box<dyn fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.source.as_ref().map(|s| s as &dyn SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        labels(&self.diagnostic, true)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use miette::Diagnostic as _;

    #[test]
    fn every_registered_code_has_a_url() {
        for info in oneyaml_core::code::REGISTRY {
            let url = docs_url(info.code).unwrap();
            assert!(url.ends_with(&format!("constant.{}.html", info.name)));
        }
        assert_eq!(docs_url(Code::new(999).unwrap()), None);
    }

    #[test]
    fn severity_and_labels_map() {
        let d = Diagnostic::warning(
            oneyaml_core::code::DUPLICATE_KEY_LAST_WINS,
            "dup",
            Label::new(oneyaml_core::Span::new(4, 2), ""),
        )
        .with_secondary(Label::new(oneyaml_core::Span::new(0, 1), "first"));
        let r = DiagnosticReport::from(d.clone());
        assert_eq!(r.severity(), Some(miette::Severity::Warning));
        let labels: Vec<LabeledSpan> = r.labels().unwrap().collect();
        assert_eq!(labels.len(), 2);
        assert!(labels[0].primary());
        assert_eq!(labels[0].len(), 0); // an inverted span is read as empty
        assert_eq!(labels[1].label(), Some("first"));
        assert!(r.help().is_none());
        assert!(r.source_code().is_none());
        assert_eq!(r.clone().into_diagnostic(), d);
        assert_eq!(r.diagnostic(), &d);
    }
}
