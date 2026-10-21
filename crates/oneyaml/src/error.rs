//! The opaque top-level [`Error`].
//!
//! An error is one [`Diagnostic`] (stable `Y####` code, message, span, path)
//! plus the warnings recorded before it. Path and position are attached
//! **while the error unwinds** (each collection level prepends its segment
//! on the `Err` branch only) and the line/column are computed once, at the
//! entry point, from the source: nothing is tracked on the happy path.

use std::fmt;

use oneyaml_core::code::{self, Code, CodeRange};
use oneyaml_core::{ColumnUnit, Diagnostic, Label, LineIndex, Path, PathSegment, Severity, Span};

/// What kind of problem an [`Error`] reports (from its code's range).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Invalid YAML syntax or encoding (`Y0001–Y0999`).
    Syntax,
    /// Schema, tag, alias, duplicate-key, or settings problems
    /// (`Y1000–Y1999`).
    Schema,
    /// A resource limit was exceeded (`Y2000–Y2999`); the help names the
    /// setting to raise.
    Limit,
    /// The YAML is valid but does not fit the target type (`Y3000–Y3999`).
    Data,
    /// Reading the input ([`code::IO_ERROR`]) or writing the output
    /// ([`code::WRITE_ERROR`]) failed.
    Io,
    /// A bug in oneyaml ([`code::INTERNAL_ERROR`]).
    Internal,
}

/// A 1-based line and column (in `char`s) plus the byte offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    line: usize,
    column: usize,
    index: usize,
}

impl Location {
    /// 1-based line.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }
    /// 1-based column, counted in `char`s.
    #[must_use]
    pub const fn column(&self) -> usize {
        self.column
    }
    /// UTF-8 byte offset into the source.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// The location of byte `index` in `src`; `None` if it is out of bounds
    /// or not on a `char` boundary. Handles `\n`, `\r\n`, and `\r` (all
    /// through [`LineIndex`], the one position conversion).
    pub(crate) fn of(src: &str, index: usize) -> Option<Self> {
        Self::indexed(&LineIndex::new(src), index)
    }

    /// [`Location::of`] through an index built once (O(log n), for
    /// callers that locate many errors in one source).
    pub(crate) fn indexed(lines: &LineIndex, index: usize) -> Option<Self> {
        let at = lines.line_col(index, ColumnUnit::Char)?;
        Some(Self {
            line: at.line.saturating_add(1),
            column: at.column.saturating_add(1),
            index,
        })
    }
}

/// An error from a oneyaml entry point: opaque and boxed, so `Result<T,
/// Error>` stays one pointer wide.
///
/// [`Display`](fmt::Display) is compact: `message at path (line:column)`,
/// e.g. `invalid value: integer 99999, expected u16 at spec.ports[2].port
/// (14:7)`. [`Error::diagnostic`] has the full structured form.
///
/// # Diagnostics
///
/// Every error and warning is a [`Diagnostic`] with a stable `Y####` code
/// (the [`code`](crate::code) module), a primary label (a UTF-8 byte span),
/// secondary labels, help, the path to the node, and suggested fixes.
/// [`Error::render`] draws a rustc-style snippet, with no dependencies:
///
/// ```
/// use oneyaml::{Applicability, Value};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let src = "title: The farm: a banana story\n";
/// let err = oneyaml::from_str::<Value>(src).err().ok_or("expected an error")?;
/// assert_eq!(err.code(), oneyaml::code::INLINE_COLON_IN_PLAIN_VALUE);
/// let at = err.location().ok_or("no location")?;
/// assert_eq!((at.line(), at.column(), at.index()), (1, 17, 16));
/// assert_eq!(
///     err.render(src),
///     "error[Y0027]: `: ` inside an unquoted value starts a nested mapping, which cannot begin on this line
///  --> 1:17
///   |
/// 1 | title: The farm: a banana story
///   |                - this `: ` starts a mapping
///   |                 ^^^^^^^^^^^^^^^
///   |
///   = help: to keep the `: ` as text, quote the whole value; a nested mapping must start on its own, more indented line (Settings::lenient_inline_colons accepts such values with a warning)
///   = path: title
///   = suggestion: quote the value: replace `The farm: a banana story` with `'The farm: a banana story'`
/// "
/// );
///
/// // A machine-applicable fix is safe to apply without review.
/// let fix = err.diagnostic().suggestions.first().ok_or("no fix")?;
/// assert_eq!(fix.applicability, Applicability::MachineApplicable);
/// let fixed = fix.apply(src).ok_or("the fix does not apply")?;
/// assert_eq!(fixed, "title: 'The farm: a banana story'\n");
/// assert!(oneyaml::from_str::<Value>(&fixed).is_ok());
/// # Ok(())
/// # }
/// ```
///
/// Fixes are offered for common mistakes (an unquoted `: `, a `'` inside
/// single quotes, a Windows path in double quotes, a missing closing quote
/// or bracket, a reserved `@` or `` ` ``, `key:value` without a space, a
/// YAML 1.1 boolean such as `yes` read into a `bool`). A
/// fix is `MachineApplicable` only when applying it makes the node valid
/// where it stands and nothing after it can read differently; otherwise it
/// is `MaybeIncorrect`, to be reviewed before applying. [`Render`](crate::Render)
/// renders the diagnostics of a recovering parse and the warnings that
/// `from_str` skips. With the `miette` feature, `Error` is a
/// `miette::Diagnostic`.
pub struct Error(Box<Inner>);

struct Inner {
    kind: ErrorKind,
    diagnostic: Diagnostic,
    has_span: bool,
    /// Path segments, innermost first (pushed while unwinding).
    rev_path: Vec<PathSegment>,
    location: Option<Location>,
    warnings: Vec<Diagnostic>,
    io: Option<std::io::Error>,
}

/// The most characters of one path segment (a key) an error keeps; longer
/// keys end with `…` (a key may be megabytes, and alias
/// expansion repeats it).
pub(crate) const MAX_SEGMENT_CHARS: usize = 64;

/// The most bytes of an error message; a longer one ends with
/// [`TRUNCATED`]. Spans are kept exact.
pub(crate) const MAX_MESSAGE_BYTES: usize = 3072;

/// The most bytes of an error's path that `Display` and `render` show
/// (then `…`), so an error shows at most about 11 KiB (8 KiB
/// keeps a path of indexes at the default `max_depth` whole).
/// [`Error::path`] keeps every segment: their number is bounded by
/// `Limits::max_depth`, and each by [`MAX_SEGMENT_CHARS`].
pub(crate) const MAX_PATH_BYTES: usize = 8192;

/// Ends a message cut at [`MAX_MESSAGE_BYTES`].
pub(crate) const TRUNCATED: &str = "… [message truncated]";

/// A writer that keeps at most `left` units (characters or bytes) and then
/// fails, so a formatter stops early: what it formats is never built in
/// full.
struct Bounded {
    out: String,
    left: usize,
    bytes: bool,
    cut: bool,
}

impl fmt::Write for Bounded {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for (i, c) in s.char_indices() {
            let cost = if self.bytes { c.len_utf8() } else { 1 };
            if cost > self.left {
                self.out.push_str(s.get(..i).unwrap_or(""));
                self.cut = true;
                return Err(fmt::Error);
            }
            self.left = self.left.saturating_sub(cost);
        }
        self.out.push_str(s);
        Ok(())
    }
}

fn bounded(args: fmt::Arguments<'_>, left: usize, bytes: bool) -> (String, bool) {
    let mut w = Bounded {
        out: String::new(),
        left,
        bytes,
        cut: false,
    };
    // An error is the writer's own stop signal (or a `Display` failing,
    // which leaves what was written).
    let _ = fmt::write(&mut w, args);
    (w.out, w.cut)
}

/// At most `max` characters of `args`, then `…` when cut: built within
/// the bound.
pub(crate) fn short_chars(args: fmt::Arguments<'_>, max: usize) -> String {
    let (mut s, cut) = bounded(args, max, false);
    if cut {
        s.push('…');
    }
    s
}

/// A path segment for `key` ([`MAX_SEGMENT_CHARS`]).
pub(crate) fn segment(key: &str) -> String {
    short_chars(format_args!("{key}"), MAX_SEGMENT_CHARS)
}

/// A message within [`MAX_MESSAGE_BYTES`], built within the bound.
pub(crate) fn message(args: fmt::Arguments<'_>) -> String {
    let room = MAX_MESSAGE_BYTES.saturating_sub(TRUNCATED.len());
    let (mut s, cut) = bounded(args, room, true);
    if cut {
        s.push_str(TRUNCATED);
    }
    s
}

/// A path as shown: at most [`MAX_PATH_BYTES`], then `…`, built within
/// the bound.
pub(crate) fn shown_path(path: &Path) -> String {
    let (mut s, cut) = bounded(format_args!("{path}"), MAX_PATH_BYTES, true);
    if cut {
        s.push('…');
    }
    s
}

/// `text` cut to [`MAX_MESSAGE_BYTES`] (for messages already built).
fn cap_message(text: &mut String) {
    if text.len() > MAX_MESSAGE_BYTES {
        let room = MAX_MESSAGE_BYTES.saturating_sub(TRUNCATED.len());
        let mut at = room.min(text.len());
        while !text.is_char_boundary(at) {
            at = at.saturating_sub(1);
        }
        text.truncate(at);
        text.push_str(TRUNCATED);
    }
}

fn kind_of(code: Code) -> ErrorKind {
    if code == code::IO_ERROR || code == code::WRITE_ERROR {
        return ErrorKind::Io;
    }
    match code.range() {
        CodeRange::Syntax => ErrorKind::Syntax,
        CodeRange::Schema => ErrorKind::Schema,
        CodeRange::Limits => ErrorKind::Limit,
        CodeRange::Serde => ErrorKind::Data,
        _ => ErrorKind::Internal,
    }
}

impl Error {
    /// An error with `code` and `message` at `span` (if known).
    pub(crate) fn new(code: Code, message: impl Into<String>, span: Option<Span>) -> Self {
        let label = Label::new(span.unwrap_or(Span::empty(0)), "");
        Self::build(Diagnostic::error(code, message, label), span.is_some())
    }

    /// An error from a diagnostic that already has its span.
    pub(crate) fn from_diagnostic(diagnostic: Diagnostic) -> Self {
        Self::build(diagnostic, true)
    }

    fn build(mut diagnostic: Diagnostic, has_span: bool) -> Self {
        // Every message is bounded.
        cap_message(&mut diagnostic.message);
        Self(Box::new(Inner {
            kind: kind_of(diagnostic.code),
            diagnostic,
            has_span,
            rev_path: Vec::new(),
            location: None,
            warnings: Vec::new(),
            io: None,
        }))
    }

    /// The release-build outcome of a failed internal invariant.
    pub(crate) fn internal(what: &str, span: Option<Span>) -> Self {
        Self::new(
            code::INTERNAL_ERROR,
            format!("internal error: {what} (a bug in oneyaml; please report it)"),
            span,
        )
    }

    pub(crate) fn io(err: std::io::Error) -> Self {
        let mut e = Self::new(code::IO_ERROR, format!("I/O error: {err}"), None);
        e.0.io = Some(err);
        e
    }

    /// Writing the output failed (`to_writer`, `Serializer`).
    pub(crate) fn write(err: std::io::Error) -> Self {
        let mut e = Self::new(
            code::WRITE_ERROR,
            format!("I/O error while writing: {err}"),
            None,
        );
        e.0.io = Some(err);
        e
    }

    /// Adds `help` to the diagnostic.
    pub(crate) fn with_help(mut self, help: impl Into<String>) -> Self {
        // A help text is bounded as a message is.
        let mut help = help.into();
        cap_message(&mut help);
        self.0.diagnostic.help = Some(help);
        self
    }

    /// Adds a suggested fix.
    pub(crate) fn with_suggestion(mut self, suggestion: oneyaml_core::Suggestion) -> Self {
        self.0.diagnostic.suggestions.push(suggestion);
        self
    }

    /// Adds a secondary label.
    pub(crate) fn with_secondary(mut self, label: Label) -> Self {
        self.0.diagnostic.secondary.push(label);
        self
    }

    /// Sets the span unless one is known already (the innermost node that
    /// knows its span wins). Called on the `Err` path only.
    pub(crate) fn at(mut self, span: Span) -> Self {
        if !self.0.has_span {
            self.0.diagnostic.primary.span = span;
            self.0.has_span = true;
        }
        self
    }

    /// Forgets the span (and location): for errors about a
    /// [`Value`](crate::Value), which has no source. The shared scalar
    /// visitor always names a span; a `Value` read gives it a placeholder,
    /// dropped here at the entry point.
    pub(crate) fn without_span(mut self) -> Self {
        let inner = &mut *self.0;
        inner.has_span = false;
        inner.diagnostic.primary.span = Span::empty(0);
        inner.location = None;
        self
    }

    /// Prepends a path segment (called while unwinding, innermost first),
    /// at most [`MAX_SEGMENT_CHARS`] of `key`.
    pub(crate) fn in_key(mut self, key: &str) -> Self {
        self.0.rev_path.push(PathSegment::Key(segment(key)));
        self
    }

    /// Prepends a sequence index segment.
    pub(crate) fn in_index(mut self, index: usize) -> Self {
        self.0.rev_path.push(PathSegment::Index(index));
        self
    }

    /// Final step at the entry point: location from `src`, the path into
    /// the diagnostic, and the warnings recorded before the error.
    /// Sets the line and column through `lines` (the source's index), so
    /// [`Error::finish`] need not index the source again.
    pub(crate) fn locate(mut self, lines: &LineIndex) -> Self {
        let inner = &mut *self.0;
        if inner.location.is_none() && inner.has_span {
            inner.location = Location::indexed(lines, inner.diagnostic.primary.span.start);
        }
        self
    }

    pub(crate) fn finish(mut self, src: Option<&str>, warnings: Vec<Diagnostic>) -> Self {
        let inner = &mut *self.0;
        if inner.diagnostic.path.is_none() && !inner.rev_path.is_empty() {
            let mut path = Path::new();
            for seg in inner.rev_path.drain(..).rev() {
                match seg {
                    PathSegment::Key(k) => path.push_key(k),
                    PathSegment::Index(i) => path.push_index(i),
                    _ => {}
                }
            }
            inner.diagnostic.path = Some(path);
        }
        if inner.location.is_none() && inner.has_span {
            inner.location = src.and_then(|s| Location::of(s, inner.diagnostic.primary.span.start));
        }
        if inner.warnings.is_empty() {
            inner.warnings = warnings;
        }
        self
    }

    /// The kind of problem.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.0.kind
    }

    /// The stable `Y####` code.
    #[must_use]
    pub fn code(&self) -> Code {
        self.0.diagnostic.code
    }

    /// The full diagnostic: code, message, labels, help, and path.
    #[must_use]
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.0.diagnostic
    }

    /// The warnings recorded before the error (e.g. a `%YAML 1.1`
    /// directive), in source order.
    #[must_use]
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.0.warnings
    }

    /// The path to the offending node, e.g. `spec.ports[2].port`.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.0.diagnostic.path.as_ref()
    }

    /// The offending source range (UTF-8 bytes), when known.
    #[must_use]
    pub fn span(&self) -> Option<Span> {
        self.0.has_span.then_some(self.0.diagnostic.primary.span)
    }

    /// The line and column of the span's start, when known.
    #[must_use]
    pub fn location(&self) -> Option<Location> {
        self.0.location
    }

    /// A rustc-style snippet of `src` (the text this error was read from)
    /// showing the code, message, labelled lines, help, path, and fixes.
    /// `Display` stays the compact one-line form.
    ///
    /// No index is needed: the error already knows its primary line and
    /// column, so the cost is the lines shown (plus the text between the
    /// primary and any secondary label). Never panics, whatever `src` is;
    /// see [`Render`](crate::Render) for the details.
    ///
    /// ```
    /// #[derive(Debug, serde::Deserialize)]
    /// struct Port {
    ///     port: u16,
    /// }
    /// let src = "port: 99999\n";
    /// let err = oneyaml::from_str::<Port>(src).unwrap_err();
    /// let snippet = err.render(src);
    /// assert!(snippet.contains("1 | port: 99999"));
    /// assert!(snippet.contains("= path: port"));
    /// ```
    #[must_use]
    pub fn render(&self, src: &str) -> String {
        let inner = &*self.0;
        match inner.location {
            Some(l) => {
                let numbers = crate::render::Numbers::Anchor {
                    at: l.index,
                    line: l.line.saturating_sub(1),
                    col: l.column.saturating_sub(1),
                };
                crate::render::render(&inner.diagnostic, src, numbers, inner.has_span)
            }
            None => self.render_indexed(src, &LineIndex::new(src)),
        }
    }

    /// [`Error::render`] with line numbers from `index`, an index of `src`.
    #[must_use]
    pub fn render_indexed(&self, src: &str, index: &LineIndex) -> String {
        let numbers = crate::render::Numbers::Index(index);
        crate::render::render(&self.0.diagnostic, src, numbers, self.0.has_span)
    }
}

impl crate::Render for Error {
    fn render(&self, src: &str) -> String {
        Error::render(self, src)
    }

    fn render_indexed(&self, src: &str, index: &LineIndex) -> String {
        Error::render_indexed(self, src, index)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.diagnostic.message)?;
        if let Some(path) = self.path().filter(|p| !p.is_empty()) {
            // At most `MAX_PATH_BYTES` of it, built in the bound.
            write!(f, " at {}", shown_path(path))?;
        }
        if let Some(l) = self.0.location {
            write!(f, " ({}:{})", l.line, l.column)?;
        }
        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error({}: {self})", self.code())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0
            .io
            .as_ref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// A warning diagnostic without extra labels.
pub(crate) fn warning(code: Code, message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new(code, Severity::Warning, message, Label::new(span, ""))
}

// serde's own messages, with our codes (Y3xxx for serde).
impl serde::de::Error for Error {
    // Messages quoting input text are built within the bound.
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self::new(code::CUSTOM, message(format_args!("{msg}")), None)
    }

    fn invalid_type(unexp: serde::de::Unexpected<'_>, exp: &dyn serde::de::Expected) -> Self {
        Self::new(
            code::TYPE_MISMATCH,
            message(format_args!("invalid type: {unexp}, expected {exp}")),
            None,
        )
    }

    fn invalid_value(unexp: serde::de::Unexpected<'_>, exp: &dyn serde::de::Expected) -> Self {
        Self::new(
            code::INVALID_VALUE,
            message(format_args!("invalid value: {unexp}, expected {exp}")),
            None,
        )
    }

    fn invalid_length(len: usize, exp: &dyn serde::de::Expected) -> Self {
        Self::new(
            code::INVALID_LENGTH,
            message(format_args!("invalid length {len}, expected {exp}")),
            None,
        )
    }

    fn unknown_variant(variant: &str, expected: &'static [&'static str]) -> Self {
        let variant = segment(variant);
        Self::new(
            code::UNKNOWN_VARIANT,
            message(format_args!(
                "unknown variant `{variant}`, {}",
                one_of(expected)
            )),
            None,
        )
    }

    fn unknown_field(field: &str, expected: &'static [&'static str]) -> Self {
        let field = segment(field);
        Self::new(
            code::UNKNOWN_FIELD,
            message(format_args!(
                "unknown field `{field}`, {}",
                one_of(expected)
            )),
            None,
        )
    }

    fn missing_field(field: &'static str) -> Self {
        Self::new(
            code::MISSING_FIELD,
            format!("missing field `{field}`"),
            None,
        )
    }

    fn duplicate_field(field: &'static str) -> Self {
        Self::new(
            code::DUPLICATE_FIELD,
            format!("duplicate field `{field}`"),
            None,
        )
    }
}

// The value's own `Serialize` errors (Y3xxx for serde).
impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        // Our own hand-over's teardown refusal keeps its
        // code. Recognised by type name, which is not unique across
        // copies of oneyaml: the message type is private, so only another
        // copy of oneyaml has that path (and means the same refusal).
        if crate::value::is_tls_gone::<T>() {
            return crate::value::tls_gone_ser();
        }
        Self::new(code::SERIALIZE_CUSTOM, message(format_args!("{msg}")), None)
    }
}

fn one_of(names: &[&str]) -> String {
    match names {
        [] => "there are none".to_owned(),
        [one] => format!("expected `{one}`"),
        [a, b] => format!("expected `{a}` or `{b}`"),
        _ => {
            let list: Vec<String> = names.iter().map(|n| format!("`{n}`")).collect();
            format!("expected one of {}", list.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use serde::de::Error as _;

    #[test]
    fn location_counts_all_breaks() {
        let src = "a\r\nb\rc\nd\u{e9}x";
        let l = Location::of(src, src.find('x').unwrap()).unwrap();
        assert_eq!((l.line(), l.column()), (4, 3));
        assert_eq!(Location::of(src, 0).unwrap().line(), 1);
        assert_eq!(Location::of(src, 99), None);
        assert_eq!(Location::of("\u{e9}", 1), None);
    }

    #[test]
    fn display_is_compact_and_kind_follows_code() {
        let e = Error::custom("invalid port: 99999")
            .at(Span::new(4, 9))
            .in_key("port")
            .in_index(2)
            .in_key("ports")
            .in_key("spec")
            .at(Span::new(0, 1))
            .finish(Some("a:\n  99999"), Vec::new());
        assert_eq!(
            e.to_string(),
            "invalid port: 99999 at spec.ports[2].port (2:2)"
        );
        assert_eq!(e.kind(), ErrorKind::Data);
        assert_eq!(e.code(), code::CUSTOM);
        assert_eq!(e.span(), Some(Span::new(4, 9)));
        assert_eq!(e.path().unwrap().to_string(), "spec.ports[2].port");
        let no_span = Error::missing_field("x").finish(None, Vec::new());
        assert_eq!(no_span.to_string(), "missing field `x`");
        assert_eq!(no_span.span(), None);
        assert_eq!(Error::internal("x", None).kind(), ErrorKind::Internal);
        let io = Error::io(std::io::Error::other("boom"));
        assert_eq!(io.kind(), ErrorKind::Io);
        assert!(std::error::Error::source(&io).is_some());
        assert!(format!("{io:?}").contains("Y3010"));
        assert_eq!(one_of(&[]), "there are none");
        assert_eq!(one_of(&["a", "b"]), "expected `a` or `b`");
    }
}
