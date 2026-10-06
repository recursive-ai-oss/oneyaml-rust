//! Stream and document level (YAML 1.2.2 §9): byte order marks, directives
//! (§6.8), document markers, and what may follow a document's root node.

use std::collections::BTreeMap;

use oneyaml_core::code::{
    CONTENT_AFTER_DOCUMENT, DIRECTIVE_IN_DOCUMENT, DIRECTIVE_WITHOUT_DOCUMENT, DUPLICATE_DIRECTIVE,
    MALFORMED_DIRECTIVE, RESERVED_DIRECTIVE_IGNORED, SCALAR_TOO_LONG, TOO_MANY_DOCUMENTS,
    YAML_VERSION_1_0, YAML_VERSION_1_1, YAML_VERSION_NEWER_MINOR, YAML_VERSION_UNSUPPORTED_MAJOR,
};
use oneyaml_core::{Code, Diagnostic, Label, Span};

use super::scan::{
    TagPrefix, byte, first_mark, is_break, is_white, line_end, marker_at, skip_white, uri_end,
    valid_handle,
};
use super::{Frame, NodeCtx, Parser, State};
use crate::event::{DirectiveKind, EventKind, TriviaKind};

/// Context of a document's root node: `s-l+block-node(-1, block-in)` inside
/// `l-bare-document` (production 207).
pub(super) const ROOT: NodeCtx = NodeCtx {
    parent: None,
    compact: false,
    seq_at_parent: false,
};

/// Where the run of byte order marks that directly precedes `i` from the
/// start of its line begins (`i` when there is none): a diagnostic's
/// location counts no invisible mark before the offending text.
fn marks_before(src: &str, i: usize) -> usize {
    let mark = '\u{feff}'.len_utf8();
    let mut at = i;
    while at >= mark && src.get(at.saturating_sub(mark)..at) == Some("\u{feff}") {
        at = at.saturating_sub(mark);
    }
    let line_start = at == 0 || matches!(byte(src, at.saturating_sub(1)), Some(b'\n' | b'\r'));
    if line_start { at } else { i }
}

/// `d` with its primary label starting at `from` (see [`marks_before`]).
fn from_mark(mut d: Diagnostic, from: usize) -> Diagnostic {
    d.primary.span = Span::new(from.min(d.primary.span.start), d.primary.span.end);
    d
}

/// `ns-yaml-version` (production 87): `digits.digits`, returned split.
fn version_parts(v: &str) -> Option<(&str, &str)> {
    let (major, minor) = v.split_once('.')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(major) && digits(minor)).then_some((major, minor))
}

/// A decimal digit string's value, saturating at `u64::MAX`.
fn number(s: &str) -> u64 {
    s.bytes().fold(0u64, |acc, b| {
        acc.saturating_mul(10)
            .saturating_add(u64::from(b.saturating_sub(b'0')))
    })
}

/// A directive parsed by [`parse_directive`].
pub(super) struct ParsedDirective<'a> {
    pub(super) kind: DirectiveKind<'a>,
    /// End of the last parameter (the end of the directive's event).
    pub(super) end: usize,
    /// For `%TAG`, where its prefix starts.
    pub(super) prefix_start: usize,
}

/// `l-directive` (production 82) whose `%` is at `start`: the directive's
/// meaning, or `None` when malformed. Used by the parser and, on the event's
/// span, by [`Sourced::directive`](crate::event::Sourced::directive) (one
/// reading). No allocation.
pub(super) fn parse_directive(src: &str, start: usize) -> Option<ParsedDirective<'_>> {
    let le = line_end(src, start);
    let name_start = start.saturating_add(1);
    let word_end = |from: usize| {
        let n = src.as_bytes().get(from..le).map_or(0, |r| {
            r.iter()
                .take_while(|b| !is_white(**b) && !is_break(**b))
                .count()
        });
        from.saturating_add(n)
    };
    let name_end = word_end(name_start);
    // Parameters: white-separated words up to a comment (production 75).
    // The first two, the last, and how many.
    let mut first: Option<(usize, usize)> = None;
    let mut second: Option<(usize, usize)> = None;
    let mut last: Option<(usize, usize)> = None;
    let mut count = 0usize;
    let mut j = name_end;
    loop {
        let k = skip_white(src, j);
        if k >= le || k == j || byte(src, k) == Some(b'#') {
            break;
        }
        let e = word_end(k);
        match count {
            0 => first = Some((k, e)),
            1 => second = Some((k, e)),
            _ => {}
        }
        last = Some((k, e));
        count = count.saturating_add(1);
        j = e;
    }
    let name = src.get(name_start..name_end).unwrap_or("");
    let text = |r: (usize, usize)| src.get(r.0..r.1).unwrap_or("");
    let end = last.map_or(name_end, |p| p.1);
    // The name and parameters are `ns-char`s (productions 83-85), which
    // exclude a byte order mark.
    if src.get(start..end).is_some_and(|t| t.contains('\u{feff}')) {
        return None;
    }
    let mut prefix_start = 0;
    let kind = match (name, count, first, second) {
        ("", ..) => None,
        ("YAML", 1, Some(v), _) => version_parts(text(v)).map(|_| DirectiveKind::yaml(text(v))),
        ("TAG", 2, Some(h), Some(p)) => {
            let (handle, prefix) = (text(h), text(p));
            // ns-tag-prefix (production 93).
            let prefix_ok = uri_end(src, p.0, false) == p.1
                && !matches!(byte(src, p.0), Some(b',' | b'[' | b']'));
            prefix_start = p.0;
            (valid_handle(handle) && prefix_ok).then_some(DirectiveKind::tag(handle, prefix))
        }
        ("YAML" | "TAG", ..) => None,
        (name, ..) => {
            let params = first.map_or("", |p| src.get(p.0..end).unwrap_or(""));
            Some(DirectiveKind::reserved(name, params))
        }
    }?;
    Some(ParsedDirective {
        kind,
        end,
        prefix_start,
    })
}

impl<'a> Parser<'a> {
    /// `l-yaml-stream` (production 211) between documents: `l-document-prefix`
    /// (202), directives, `c-directives-end` (203), `c-document-end` (204),
    /// and the start of a bare document (207).
    pub(super) fn stream(&mut self) {
        self.scan_trivia(true);
        let src = self.src;
        let i = self.pos;
        if core::mem::take(&mut self.doc_end_line) && !self.fresh && i < src.len() {
            self.fail(
                CONTENT_AFTER_DOCUMENT,
                "only a comment may follow `...` on its line",
                false,
            );
            return;
        }
        if i >= src.len() {
            self.flush();
            self.prefix_without_end = None;
            if core::mem::take(&mut self.directives_pending) {
                self.soft_error(
                    DIRECTIVE_WITHOUT_DOCUMENT,
                    "directives must be followed by `---`",
                );
            }
            self.push(EventKind::StreamEnd, i, i);
            self.state = State::Done;
            return;
        }
        let line_head = self.fresh && i == self.line_start;
        let prefix_mark = self.prefix_without_end.take();
        // Production 211: after a document that no `...` ended, a byte
        // order mark (a document prefix) may be followed by an explicit
        // document, or by `l-document-suffix` (`...`,
        // read below as a document end marker between documents, after
        // which a bare document may follow again).
        let allowed = line_head && (marker_at(src, i, "---") || marker_at(src, i, "..."));
        if let Some(mark) = prefix_mark.filter(|_| !allowed) {
            // Anything else is refused. The diagnostic
            // points at the invisible mark that ended the document, and
            // its location does not count marks that start the offending
            // line.
            let refine = |d: Diagnostic| {
                let mark = Span::new(mark, mark.saturating_add('\u{feff}'.len_utf8()));
                d.with_secondary(Label::new(
                    mark,
                    "this byte order mark (U+FEFF) ended the document",
                ))
                .with_help(
                    "a byte order mark (U+FEFF, invisible in most editors) at the start of a line ends the document before it; remove the mark, or start the next document with `---`",
                )
            };
            let from = marks_before(src, i);
            if line_head && byte(src, i) == Some(b'%') {
                self.fail_with(
                    DIRECTIVE_IN_DOCUMENT,
                    "a directive must follow a document ended with `...`",
                    false,
                    |d| from_mark(refine(d), from),
                );
                return;
            }
            // Recovery keeps what follows: it is read as the next (bare)
            // document, after this error.
            self.flush();
            let end = line_end(src, i);
            let d = Diagnostic::error(
                CONTENT_AFTER_DOCUMENT,
                "a document not ended with `...` can be followed only by one starting with `---`",
                Label::new(Span::new(from, end), ""),
            );
            self.push(EventKind::Error(Box::new(refine(d))), i, i);
        }
        let starts_document = line_head && marker_at(src, i, "---")
            || !self.directives_pending && !(line_head && marker_at(src, i, "..."));
        if line_head && byte(src, i) == Some(b'%') {
            self.directive();
        } else if starts_document && self.documents >= self.max_documents {
            // `Limits::max_documents`; the rest of the stream is
            // skipped.
            self.halt(TOO_MANY_DOCUMENTS, "the stream has too many documents");
        } else if line_head && marker_at(src, i, "---") {
            self.flush();
            self.token(EventKind::document_start(true), i.saturating_add(3));
            self.start_document();
            self.state = State::Node(ROOT);
        } else if self.directives_pending {
            self.fail(
                DIRECTIVE_WITHOUT_DOCUMENT,
                "directives must be followed by `---`",
                false,
            );
        } else if line_head && marker_at(src, i, "...") {
            self.flush();
            self.token(
                EventKind::Trivia(TriviaKind::DocumentEndMarker),
                i.saturating_add(3),
            );
            self.doc_end_line = true;
        } else {
            // l-bare-document (production 207): an implicit document whose
            // start is emitted with its first node event.
            self.start_document();
            self.doc_start_deferred = true;
            self.state = State::Node(ROOT);
        }
    }

    fn start_document(&mut self) {
        self.documents = self.documents.saturating_add(1);
        self.stack.push(Frame::Document { root_done: false });
        self.tags = core::mem::take(&mut self.pending_tags);
        self.yaml_seen = false;
        self.directives_pending = false;
    }

    /// After a document's root node: another document, `...`, or the end.
    pub(super) fn after_root(&mut self) {
        let src = self.src;
        let i = self.pos;
        let line_head = self.fresh && i == self.line_start;
        let prefix = self.prefix_mark_at(i);
        if i >= src.len() || line_head && marker_at(src, i, "---") || prefix {
            // Implicit end, "ends early" (C4): before the pending trivia.
            // A byte order mark at the start of the line ends it too: it
            // starts a document prefix (production 211).
            self.zero(EventKind::document_end(false));
            self.stack.pop();
            self.prefix_without_end = prefix.then_some(i);
            self.state = State::Stream;
        } else if line_head && marker_at(src, i, "...") {
            self.flush();
            self.token(EventKind::document_end(true), i.saturating_add(3));
            self.stack.pop();
            self.doc_end_line = true;
            self.state = State::Stream;
        } else if self.fresh {
            // A new line after a complete root: the document is certainly
            // over (implicit end, C4), and what follows cannot start another
            // one without `...` or `---` (production 211). If the root's own
            // end is still unconfirmed (deferred), the document stays open
            // and the error is repaired inside it.
            // Timing convention T4 (parser module docs), not a contract rule.
            // Only a plain scalar root needs the next line to end it: having
            // looked, the parser knows the document is over before reading
            // the offending content. A root that ends at its own delimiter
            // (quote, bracket) or whose end is still deferred stays open.
            let plain_root = self.deferred.is_empty() && self.out.last_plain;
            if plain_root {
                self.zero(EventKind::document_end(false));
                self.stack.pop();
            }
            if line_head && byte(src, i) == Some(b'%') {
                self.fail(
                    DIRECTIVE_IN_DOCUMENT,
                    "a directive must follow a document ended with `...`",
                    false,
                );
            } else {
                self.fail(
                    CONTENT_AFTER_DOCUMENT,
                    "content after the document's root node",
                    false,
                );
            }
        } else {
            self.unexpected_content();
        }
    }

    pub(super) fn warning(&mut self, code: Code, message: &str) {
        let at = self.emit_pos;
        let d = Diagnostic::warning(code, message, Label::new(Span::empty(at), ""));
        self.zero(EventKind::Warning(Box::new(d)));
    }

    /// `l-directive` (production 82) at the start of a line.
    fn directive(&mut self) {
        self.flush();
        let Some(d) = parse_directive(self.src, self.pos) else {
            // A byte order mark on the line is reported too.
            let mark = first_mark(self.src, self.pos, line_end(self.src, self.pos));
            self.mark_then_fail(mark, MALFORMED_DIRECTIVE, "malformed directive", false);
            return;
        };
        // A `%TAG` prefix is read once, here, and bounded
        // like a scalar, before the directive is accepted.
        let prefix = match d.kind {
            DirectiveKind::Tag { .. } => match TagPrefix::read(self.src, d.prefix_start) {
                Some(p) if p.read.len > self.max_scalar_len => {
                    self.fail(SCALAR_TOO_LONG, "the %TAG prefix is too long", false);
                    return;
                }
                Some(p) => Some(p),
                None => {
                    // `parse_directive` accepts only a non-empty prefix.
                    debug_assert!(false, "a %TAG directive without a prefix");
                    self.fail_internal("a %TAG directive without a prefix", false);
                    return;
                }
            },
            _ => None,
        };
        self.token(EventKind::Directive(d.kind.directive()), d.end);
        self.directives_pending = true;
        match (d.kind, prefix) {
            (DirectiveKind::Yaml { version }, _) => self.yaml_directive(version),
            (DirectiveKind::Tag { handle, .. }, Some(p)) => self.tag_directive(handle, p),
            _ => self.warning(RESERVED_DIRECTIVE_IGNORED, "reserved directive ignored"),
        }
    }

    /// The `%YAML` policy (§6.8.1).
    fn yaml_directive(&mut self, version: &str) {
        let (major, minor) = version_parts(version).unwrap_or(("1", "2"));
        let (major, minor) = (number(major), number(minor));
        if core::mem::replace(&mut self.yaml_seen, true) {
            self.soft_error(DUPLICATE_DIRECTIVE, "duplicate %YAML directive");
        } else if major != 1 {
            self.soft_error(
                YAML_VERSION_UNSUPPORTED_MAJOR,
                "unsupported %YAML major version",
            );
        } else if minor == 0 {
            // Like 1.1 below; 1.1 is the closest supported version.
            let message = if self.honor_version_directive {
                "%YAML 1.0 document: honor_version_directive applies the YAML 1.1 preset"
            } else {
                "%YAML 1.0 document parsed as YAML 1.2"
            };
            self.warning(YAML_VERSION_1_0, message);
        } else if minor == 1 {
            // Under `honor_version_directive` the document switches to
            // the YAML 1.1 preset (applied by the resolver), so the warning
            // must not claim it is read as YAML 1.2.
            let message = if self.honor_version_directive {
                "%YAML 1.1 document: honor_version_directive applies the YAML 1.1 preset"
            } else {
                "%YAML 1.1 document parsed as YAML 1.2"
            };
            self.warning(YAML_VERSION_1_1, message);
        } else if minor > 2 {
            self.warning(
                YAML_VERSION_NEWER_MINOR,
                "%YAML 1.x (x > 2) document processed as YAML 1.2",
            );
        }
    }

    /// `%TAG` (§6.8.2): declares a handle for the next document.
    /// The handle maps to its prefix's position: tags are resolved from the
    /// source on demand (`Properties::tag_prefix`).
    fn tag_directive(&mut self, handle: &'a str, prefix: TagPrefix) {
        let tags: &mut BTreeMap<&'a str, TagPrefix> = &mut self.pending_tags;
        if tags.contains_key(handle) {
            self.soft_error(DUPLICATE_DIRECTIVE, "duplicate %TAG handle");
        } else {
            tags.insert(handle, prefix);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version_parts("1.2"), Some(("1", "2")));
        assert_eq!(version_parts("1.2#"), None);
        assert_eq!(version_parts("1"), None);
        assert_eq!(number("99999999999999999999999"), u64::MAX);
        assert_eq!(number("012"), 12);
    }
}
