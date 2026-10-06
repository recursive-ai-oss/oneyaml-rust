//! Tier-1 source diagnostics.

use crate::{Code, Path, Span};

/// How serious a [`Diagnostic`] is.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    /// The input is invalid; strict entry points fail.
    Error,
    /// The input is valid but something deserves attention.
    Warning,
}

impl core::fmt::Display for Severity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
        })
    }
}

/// A span with a message, pointing at source text.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Label {
    /// The source range.
    pub span: Span,
    /// What this label says about the range (may be empty).
    pub message: String,
}

impl Label {
    /// Creates a label.
    #[must_use]
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// How safely a [`Suggestion`] can be applied without a human looking at it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// Applying the edits fixes the problem and keeps the evident intent; an
    /// editor or LSP may apply it automatically (e.g. as a quick fix).
    MachineApplicable,
    /// The edits are probably right but should be reviewed before applying.
    MaybeIncorrect,
}

/// One text edit of a [`Suggestion`]: replace the source bytes in `span`
/// with `text`. An empty span inserts; an empty `text` deletes.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Replacement {
    /// The UTF-8 byte range of the source to replace.
    pub span: Span,
    /// The new text.
    pub text: String,
}

impl Replacement {
    /// Creates a replacement.
    #[must_use]
    pub fn new(span: Span, text: impl Into<String>) -> Self {
        Self {
            span,
            text: text.into(),
        }
    }
}

/// A suggested fix: a message and a set of non-overlapping source edits,
/// applied together. Maps directly onto an LSP code action or a
/// rustc-style suggestion.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Suggestion {
    /// What the fix does, e.g. "quote the value".
    pub message: String,
    /// The edits, sorted by position and non-overlapping.
    pub edits: Vec<Replacement>,
    /// Whether the fix may be applied without review.
    pub applicability: Applicability,
}

impl Suggestion {
    /// Creates a suggestion.
    #[must_use]
    pub fn new(
        message: impl Into<String>,
        edits: Vec<Replacement>,
        applicability: Applicability,
    ) -> Self {
        Self {
            message: message.into(),
            edits,
            applicability,
        }
    }

    /// Applies the edits to `source` and returns the new text, or `None` if
    /// an edit's span is out of bounds, not on `char` boundaries, or overlaps
    /// (or precedes) the previous edit.
    #[must_use]
    pub fn apply(&self, source: &str) -> Option<String> {
        let mut out = String::with_capacity(source.len());
        let mut at = 0usize;
        for edit in &self.edits {
            if edit.span.start < at || edit.span.end < edit.span.start {
                return None;
            }
            out.push_str(source.get(at..edit.span.start)?);
            source.get(edit.span.start..edit.span.end)?;
            out.push_str(&edit.text);
            at = edit.span.end;
        }
        out.push_str(source.get(at..)?);
        Some(out)
    }
}

/// A source diagnostic: a stable code, severity, message, labels, optional
/// help, optional path to the affected node, and suggested fixes.
///
/// Built by the parser and readers; applications that check YAML
/// themselves (a linter, a schema check) can build their own:
///
/// ```
/// use oneyaml_core::{
///     code, path, Applicability, Diagnostic, Label, Replacement, Severity, Span, Suggestion,
/// };
///
/// let src = "replicas: '3'\n";
/// let value = Span::new(10, 13);
/// let fix = Suggestion::new(
///     "write the number unquoted",
///     vec![Replacement::new(value, "3")],
///     Applicability::MaybeIncorrect,
/// );
/// let d = Diagnostic::warning(code::CUSTOM, "`replicas` is a string", Label::new(value, "quoted"))
///     .with_secondary(Label::new(Span::new(0, 8), "this key"))
///     .with_help("Kubernetes reads `replicas` as an integer")
///     .with_path(path!["replicas"])
///     .with_suggestion(fix);
/// assert_eq!(d.severity, Severity::Warning);
/// assert_eq!(d.secondary.len(), 1);
/// assert_eq!(d.path.as_ref().map(ToString::to_string).as_deref(), Some("replicas"));
/// assert_eq!(d.suggestions[0].apply(src).as_deref(), Some("replicas: 3\n"));
/// let e = Diagnostic::error(code::CUSTOM, "same, as an error", Label::new(value, ""));
/// assert_eq!(e.severity, Severity::Error);
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// Stable `Y####` code.
    pub code: Code,
    /// Error or warning.
    pub severity: Severity,
    /// Main message.
    pub message: String,
    /// The main location.
    pub primary: Label,
    /// Related locations, e.g. "first defined here".
    pub secondary: Vec<Label>,
    /// How to fix it; for limit diagnostics, names the setting to raise.
    pub help: Option<String>,
    /// Path to the affected node, when known.
    pub path: Option<Path>,
    /// Suggested fixes, most preferred first (may be empty).
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    /// Creates a diagnostic with no secondary labels, help, or path.
    #[must_use]
    pub fn new(code: Code, severity: Severity, message: impl Into<String>, primary: Label) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            primary,
            secondary: Vec::new(),
            help: None,
            path: None,
            suggestions: Vec::new(),
        }
    }

    /// Shorthand for [`Diagnostic::new`] with [`Severity::Error`].
    #[must_use]
    pub fn error(code: Code, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Error, message, primary)
    }

    /// Shorthand for [`Diagnostic::new`] with [`Severity::Warning`].
    #[must_use]
    pub fn warning(code: Code, message: impl Into<String>, primary: Label) -> Self {
        Self::new(code, Severity::Warning, message, primary)
    }

    /// Adds a secondary label.
    #[must_use]
    pub fn with_secondary(mut self, label: Label) -> Self {
        self.secondary.push(label);
        self
    }

    /// Sets the help text.
    #[must_use]
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Sets the path.
    #[must_use]
    pub fn with_path(mut self, path: Path) -> Self {
        self.path = Some(path);
        self
    }

    /// Adds a suggested fix.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }
}

impl core::fmt::Display for Diagnostic {
    /// Compact one-line form: `error[Y0001]: message`, plus ` at <path>` when
    /// a path is known.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)?;
        if let Some(path) = &self.path {
            write!(f, " at {path}")?;
        }
        Ok(())
    }
}

impl core::error::Error for Diagnostic {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn builders_and_display() {
        let code = Code::new(1003).unwrap();
        let d = Diagnostic::warning(code, "ignored", Label::new(Span::new(0, 4), "here"))
            .with_secondary(Label::new(Span::new(5, 6), "also"))
            .with_help("remove it")
            .with_path(Path::new().with_key("a").with_index(2));
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.secondary.len(), 1);
        assert_eq!(d.help.as_deref(), Some("remove it"));
        assert_eq!(d.to_string(), "warning[Y1003]: ignored at a[2]");

        let e = Diagnostic::error(code, "bad", Label::new(Span::empty(0), ""));
        assert_eq!(e.severity, Severity::Error);
        assert_eq!(e.path, None);
        assert_eq!(e.to_string(), "error[Y1003]: bad");
        assert_eq!(
            e,
            Diagnostic::new(code, Severity::Error, "bad", Label::new(Span::empty(0), ""))
        );
        assert!(e.suggestions.is_empty());
    }

    #[test]
    fn suggestions_apply() {
        let src = "a: b: c\nd: é\n";
        let quote = Suggestion::new(
            "quote the value",
            vec![Replacement::new(Span::new(3, 7), "'b: c'")],
            Applicability::MachineApplicable,
        );
        assert_eq!(quote.apply(src).unwrap(), "a: 'b: c'\nd: é\n");
        let multi = Suggestion::new(
            "insert and delete",
            vec![
                Replacement::new(Span::empty(0), "# x\n"),
                Replacement::new(Span::new(8, 13), ""),
            ],
            Applicability::MaybeIncorrect,
        );
        assert_eq!(multi.apply(src).unwrap(), "# x\na: b: c\n\n");
        // Overlapping, out-of-order, inverted, out-of-bounds, and
        // non-boundary edits are refused, never a panic.
        for bad in [
            vec![
                Replacement::new(Span::new(0, 4), ""),
                Replacement::new(Span::new(3, 5), ""),
            ],
            vec![Replacement::new(Span::new(5, 4), "")],
            vec![Replacement::new(Span::new(0, 99), "")],
            vec![Replacement::new(Span::new(0, 12), "")],
        ] {
            let s = Suggestion::new("bad", bad, Applicability::MaybeIncorrect);
            assert_eq!(s.apply(src), None);
        }
        let d = Diagnostic::error(Code::new(6).unwrap(), "x", Label::new(Span::empty(0), ""))
            .with_suggestion(quote.clone());
        assert_eq!(d.suggestions, vec![quote]);
    }
}
