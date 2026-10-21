//! Node heads: properties, then an alias or flow scalar, then possibly the
//! `:` that makes it an implicit key (`ns-l-block-map-implicit-entry`,
//! production 192; `ns-flow-yaml-node`, production 159).
//!
//! # Inline colons in plain values
//!
//! A block mapping value that starts on its entry's line cannot be a block
//! mapping (`s-l+block-node`, production 196, allows a compact collection
//! only after `-`, `?`, or an explicit `:`). So in `title: The farm: a
//! banana story`, the second `: ` is an error. Exactly this case is
//! diagnosed as `INLINE_COLON_IN_PLAIN_VALUE` (`Y0027`), with a fix that
//! single-quotes the value:
//!
//! * the head is the value of a **block mapping** entry (after an implicit
//!   key's `:`, or after `:` with an empty key), including compact mappings
//!   in sequence entries (`- a: b: c`);
//! * it is on the **same line** as that `:` (not after a line break);
//! * its content is a **plain** scalar (optionally after an anchor or tag on
//!   the same line) whose first line is followed by `: `.
//!
//! Not covered, and unchanged: flow context; quoted, alias, and flow
//! collection values (`a: 'b': c`); values after `---`, `-`, `?`, or an
//! explicit-value `:`; values on a later line; and every valid input.
//!
//! With `lenient_inline_colons` on, the same case instead yields one plain
//! scalar: its first line runs to the end of the line (stopping only before
//! ` #`, a line break, or a non-printable character), then continues over
//! more-indented lines under the normal plain-scalar rules, so a `: ` on a
//! continuation line is still an error (`Y0007`). A `LENIENT_INLINE_COLON`
//! warning (`Y0028`) with the same quoting fix always follows the scalar.

use oneyaml_core::code::{
    ALIAS_WITH_PROPERTIES, DUPLICATE_PROPERTY, IMPLICIT_KEY_TOO_LONG, INLINE_COLON_IN_PLAIN_VALUE,
    INVALID_ANCHOR, INVALID_NODE_START, LENIENT_INLINE_COLON, MISSING_MAPPING_COLON,
    MULTILINE_IMPLICIT_KEY, QUOTED_SCALAR_INDENTATION, SCALAR_TOO_LONG, UNEXPECTED_CONTENT,
};
use oneyaml_core::{Applicability, Code, Diagnostic, Label, Replacement, Span, Suggestion};

use super::block::HeadMode;
use super::polish::{self, Glued, Place};
use super::scalar::{Count, QuotedError};
use super::scan::{
    anchor_end, break_or_end, byte, indicator_at, is_nb_json, is_plain_first, key_too_long,
    mark_at, plain_line_end, plain_line_end_lenient, skip_white,
};
use super::{Frame, MapPhase, NodeCtx, Parser, State, scalar};
use crate::event::{
    CollectionStyle, Event, EventKind, Indicator, Properties, ScalarStyle, TriviaKind,
};

/// The content token of a head.
enum Content<'a> {
    /// An alias covering `start..end`.
    Alias(&'a str),
    /// A flow scalar; `first_end` is the end of its first line (plain only).
    Scalar {
        style: ScalarStyle,
        /// The value's length in bytes, as far as scanned.
        len: usize,
        multiline: bool,
        /// End of the first line (the whole scalar when quoted).
        first_end: usize,
    },
    /// No content: properties directly followed by `:` (an empty key).
    Empty,
}

/// A scanned head, ready to be emitted as a key.
struct KeyHead<'a> {
    events: Vec<Event<'a>>,
    props: Properties,
    content: Content<'a>,
    content_start: usize,
    content_end: usize,
    props_end: usize,
    colon: usize,
}

fn event<'a>(kind: EventKind, start: usize, end: usize) -> Event<'a> {
    Event::new(kind, Span::new(start, end))
}

/// The fix for an inline colon: single-quote `src[start..end]` (the
/// preferred quote), doubling any `'`. A single-quoted scalar folds lines
/// exactly like the plain one it replaces, so the value is unchanged.
///
/// A byte order mark in plain text is misplaced (`Y0032`, reported
/// with its own fix: remove it), never content, so it is left out: inside
/// quotes it would become part of the value (`nb-json` allows it there),
/// and the invisible mark would hide in the suggested text.
pub(super) fn quote_fix(
    src: &str,
    start: usize,
    end: usize,
    applicability: Applicability,
) -> Suggestion {
    let text = src.get(start..end).unwrap_or("");
    let mut quoted = String::with_capacity(text.len().saturating_add(2));
    quoted.push('\'');
    for c in text.chars().filter(|c| *c != '\u{feff}') {
        if c == '\'' {
            quoted.push('\'');
        }
        quoted.push(c);
    }
    quoted.push('\'');
    Suggestion::new(
        "quote the value",
        vec![Replacement::new(Span::new(start, end), quoted)],
        applicability,
    )
}

/// Where the text glued to a plain scalar through a byte order mark at
/// `end` ends on its line: before a comment (`#` after white space) and
/// trailing white space. `None` when no mark is glued at `end`.
fn glued_after_mark(src: &str, end: usize) -> Option<usize> {
    let rest = src.get(end..)?;
    if !rest.starts_with('\u{feff}') {
        return None;
    }
    let line = rest.find(['\n', '\r']).unwrap_or(rest.len());
    let line = rest.get(..line)?;
    let mut cut = line.len();
    let mut prev_white = false;
    for (k, c) in line.char_indices() {
        if c == '#' && prev_white {
            cut = k;
            break;
        }
        prev_white = c == ' ' || c == '\t';
    }
    let text = line.get(..cut)?.trim_end_matches([' ', '\t']);
    end.checked_add(text.len())
}

/// Drops a head's trailing white space event, which stays pending trivia.
fn pop_white(events: &mut Vec<Event<'_>>) {
    if matches!(
        events.last().map(Event::kind),
        Some(EventKind::Trivia(TriviaKind::Whitespace))
    ) {
        events.pop();
    }
}

impl<'a> Parser<'a> {
    /// Parses a node head at the scan position (see [`HeadMode`]).
    pub(super) fn head(&mut self, mode: HeadMode) {
        self.mark_before_error = None;
        let slot_open = matches!(mode, HeadMode::Node { .. });
        let mut events = self.take_scratch();
        let (props, props_end, i) = match self.scan_props(&mut events) {
            Ok(scanned) => scanned,
            Err((code, message)) => return self.fail(code, message, slot_open),
        };
        let src = self.src;
        let head_col = if self.fresh {
            self.line_indent
        } else {
            self.col()
        };
        // Byte order marks skipped among the properties are no
        // columns; the rest of the line is counted without them (as
        // `misplaced_bom` does).
        let marks = events.iter().filter(|e| e.is_error()).count();
        self.line_start = self
            .line_start
            .saturating_add(marks.saturating_mul('\u{feff}'.len_utf8()))
            .min(i);

        // Properties alone on their line belong to the node that follows
        // (production 200's `s-l-comments` after properties).
        if !props.is_empty() && (break_or_end(src, i) || self.comment_at(i)) {
            let HeadMode::Node { ctx, .. } = mode else {
                // The line break (or comment) after the properties is the
                // first character that cannot belong: a key and its `:`
                // share a line.
                self.blame.set(Some(i));
                return self.fail(MISSING_MAPPING_COLON, "properties without a key", false);
            };
            if !self.merge_props(props) {
                return self.fail(DUPLICATE_PROPERTY, "duplicate anchor or tag", true);
            }
            if i > props_end {
                pop_white(&mut events); // trailing white space stays trivia
            }
            self.begin_content();
            self.commit_scratch(events);
            self.state = State::Node(NodeCtx {
                compact: false,
                ..ctx
            });
            return;
        }

        // The content token (productions 104, 109-135).
        let parent = match mode {
            HeadMode::Node { ctx, .. } => ctx.parent,
            HeadMode::Key { indent } => Some(indent),
        };
        let content_start = i;
        let content = match byte(src, i) {
            _ if !props.is_empty() && indicator_at(src, i, b':') => Ok(Content::Empty),
            _ if !props.is_empty()
                && (indicator_at(src, i, b'-') || indicator_at(src, i, b'?')) =>
            {
                let (code, message) = self.collection_error();
                return self.fail(code, message, slot_open);
            }
            Some(b'*') if !props.is_empty() => {
                Err((ALIAS_WITH_PROPERTIES, "an alias cannot have properties"))
            }
            Some(b'*') => {
                let from = i.saturating_add(1);
                let end = anchor_end(src, from);
                if end == from {
                    Err((INVALID_ANCHOR, "empty alias name"))
                } else {
                    Ok(Content::Alias(src.get(from..end).unwrap_or("")))
                }
            }
            Some(q @ (b'"' | b'\'')) => {
                let double = q == b'"';
                let faulty = |e: QuotedError| {
                    if e.0 == QUOTED_SCALAR_INDENTATION {
                        let at = scalar::indentation_fault(src, i, parent, double);
                        self.blame.set(at);
                    }
                    e
                };
                scalar::quoted::<Count>(src, i, parent, double)
                    .map_err(faulty)
                    .map(|f| Content::Scalar {
                        style: if q == b'"' {
                            ScalarStyle::DoubleQuoted
                        } else {
                            ScalarStyle::SingleQuoted
                        },
                        len: f.len,
                        multiline: f.multiline,
                        first_end: f.end,
                    })
            }
            Some(b'|' | b'>') => match mode {
                HeadMode::Node { ctx, .. } => return self.block_scalar(ctx, events, props),
                HeadMode::Key { .. } => Err((
                    MULTILINE_IMPLICIT_KEY,
                    "a block scalar cannot be an implicit key",
                )),
            },
            Some(b'[' | b'{') => return self.flow_head(mode, events, props, i),
            _ if is_plain_first(src, i, false) => {
                let end = plain_line_end(src, i, false);
                Ok(Content::Scalar {
                    style: ScalarStyle::Plain,
                    len: end.saturating_sub(i),
                    multiline: false,
                    first_end: end,
                })
            }
            _ => Err((INVALID_NODE_START, "this character cannot start a node")),
        };
        let content = match content {
            Ok(c) => c,
            Err((code, message)) => {
                // A head that may still become a key ends at its `: `.
                let clash = self.props.anchor.is_some() && props.anchor.is_some()
                    || self.props.tag.is_some() && props.tag.is_some();
                let place = match mode {
                    HeadMode::Key { indent } => Place {
                        key: true,
                        key_only: true,
                        parent: Some(indent),
                        flow: false,
                        sound: !clash,
                    },
                    HeadMode::Node { ctx, can_open } => Place {
                        key: can_open,
                        key_only: false,
                        parent: ctx.parent,
                        flow: false,
                        sound: !clash,
                    },
                };
                return self.fail_content(code, message, slot_open, content_start, place);
            }
        };
        if let Content::Scalar { len, .. } = &content {
            if !self.scalar_len_ok(*len) {
                return self.fail(SCALAR_TOO_LONG, "the scalar is too long", slot_open);
            }
        }
        if self.doc_start_deferred {
            // Content has started: the implicit document certainly exists
            // (timing convention T1).
            self.begin_content();
        }
        let content_end = match &content {
            Content::Alias(name) => content_start.saturating_add(1).saturating_add(name.len()),
            Content::Scalar { first_end, .. } => *first_end,
            Content::Empty => props_end,
        };
        let multiline = matches!(
            content,
            Content::Scalar {
                multiline: true,
                ..
            }
        );
        // Byte order marks between an implicit key and its `:`
        // are skipped as if absent (each is reported, `commit_key`); marks
        // not followed by a `:` are reported with the error they cause.
        let mut colon = skip_white(src, content_end);
        let first_mark = mark_at(src, colon).then_some(colon);
        while mark_at(src, colon) {
            colon = skip_white(src, colon.saturating_add('\u{feff}'.len_utf8()));
        }
        let is_key = indicator_at(src, colon, b':');
        if !is_key {
            colon = skip_white(src, content_end);
        }
        self.mark_before_error = first_mark;
        let key = KeyHead {
            events,
            props,
            content,
            content_start,
            content_end,
            props_end,
            colon,
        };
        if is_key {
            if let HeadMode::Node { ctx, .. } = mode {
                if self.inline_colon_value(ctx, &key.content) {
                    return self.inline_colon(key, ctx);
                }
            }
        }
        if !is_key && matches!(mode, HeadMode::Node { .. }) {
            // A node, not a key: a mark after it is reported by `next`.
            self.mark_before_error = None;
        }
        if is_key && !multiline && key_too_long(src, self.pos, colon) {
            let message = "an implicit key must be at most 1024 characters";
            // Up to the `:` the text can still be a longer scalar; the
            // character after it settles that it was a key.
            self.blame.set(Some(colon.saturating_add(1)));
            return self.fail(IMPLICIT_KEY_TOO_LONG, message, slot_open);
        }

        let (ctx, can_open) = match mode {
            HeadMode::Key { indent } => {
                if !is_key {
                    // A plain key continued on the next lines and then
                    // followed by `:` is a multi-line key (production 154),
                    // not a missing `:`.
                    let continued = match &key.content {
                        Content::Scalar {
                            style: ScalarStyle::Plain,
                            first_end,
                            ..
                        } => {
                            let f = scalar::plain(
                                src,
                                key.content_start,
                                *first_end,
                                Some(indent),
                                false,
                            );
                            f.multiline && indicator_at(src, skip_white(src, f.end), b':')
                        }
                        _ => false,
                    };
                    if continued {
                        let message = "an implicit key must be on one line";
                        self.blame.set(Some(colon.saturating_add(1)));
                        return self.fail(MULTILINE_IMPLICIT_KEY, message, false);
                    }
                    return self.missing_colon(&key);
                }
                if multiline {
                    return self.fail(
                        MULTILINE_IMPLICIT_KEY,
                        "an implicit key must be on one line",
                        false,
                    );
                }
                self.begin_content();
                self.commit_key(key);
                self.set_key_value(indent);
                return;
            }
            HeadMode::Node { ctx, can_open } => (ctx, can_open),
        };

        let quoted = matches!(
            key.content,
            Content::Scalar {
                style: ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted,
                ..
            }
        );
        if is_key && !multiline && !can_open && quoted {
            // A complete quoted node followed by a `:` that cannot start a
            // mapping here: emit the node, then report the `:` (timing
            // convention T3).
            // A failed node was already reported; never report twice.
            if !self.emit_node(key, ctx) {
                return;
            }
            self.scan_trivia(false);
            let (code, message) = self.collection_error();
            return self.fail(code, message, false);
        }
        if is_key {
            if multiline {
                return self.fail(
                    MULTILINE_IMPLICIT_KEY,
                    "an implicit key must be on one line",
                    true,
                );
            }
            if !can_open {
                let (code, message) = self.collection_error();
                return self.fail(code, message, true);
            }
            // A new block mapping whose first key is this head; properties
            // from earlier lines belong to the mapping.
            if self.fail_if_too_deep(self.block_depth().saturating_add(1)) {
                return;
            }
            self.begin_content();
            let map_props = core::mem::take(&mut self.props);
            self.zero_node(EventKind::mapping_start(CollectionStyle::Block), map_props);
            self.stack.push(Frame::Map {
                indent: head_col,
                phase: MapPhase::Value,
            });
            self.commit_key(key);
            self.set_key_value(head_col);
            return;
        }

        let _emitted = self.emit_node(key, ctx);
    }

    /// Emits a head that is a complete node (not a key), extending a plain
    /// scalar over its continuation lines.
    #[must_use]
    fn emit_node(&mut self, key: KeyHead<'a>, ctx: NodeCtx) -> bool {
        // A mark after a node is reported by the trivia scan.
        self.mark_before_error = None;
        let src = self.src;
        let KeyHead {
            mut events,
            props,
            content,
            content_start,
            content_end,
            ..
        } = key;
        // `scalar`: a scalar (else an alias or nothing), its style and end.
        let (scalar, style, end) = match content {
            Content::Scalar {
                style: ScalarStyle::Plain,
                first_end,
                ..
            } => {
                let f = scalar::plain(src, content_start, first_end, ctx.parent, false);
                if f.multiline && indicator_at(src, skip_white(src, f.end), b':') {
                    // A multi-line plain scalar cannot be a key (production
                    // 154); report before emitting it (timing convention T3).
                    // It began as a node (`a: b` then `  c: d`), so the
                    // message says how the node became a key.
                    let message = "this plain scalar runs onto the next line and is followed by `:`, so it reads as a key, and a key must be on one line";
                    let nested = ctx.parent.is_some();
                    // The `:` may still be part of a longer scalar (`c:d`);
                    // what follows it is the first character that cannot
                    // belong.
                    let colon = skip_white(src, f.end);
                    self.blame.set(Some(colon.saturating_add(1)));
                    self.fail_with(MULTILINE_IMPLICIT_KEY, message, true, |d| {
                        if nested {
                            d.with_help(
                                "a value cannot also have nested entries; put the nested entries under a key of their own, or quote the value if the `:` is part of it",
                            )
                        } else {
                            d
                        }
                    });
                    return false;
                }
                if !self.scalar_len_ok(f.len) {
                    self.fail(SCALAR_TOO_LONG, "the scalar is too long", true);
                    return false;
                }
                (true, ScalarStyle::Plain, f.end)
            }
            Content::Scalar {
                style, first_end, ..
            } => {
                self.last_quoted = (content_start, first_end);
                (true, style, first_end)
            }
            Content::Alias(_) | Content::Empty => (false, ScalarStyle::Plain, content_end),
        };
        if !scalar && !self.props.is_empty() {
            // Properties on an earlier line cannot apply to an alias
            // (production 104); as a key the alias would leave them to the
            // mapping instead.
            self.fail(
                ALIAS_WITH_PROPERTIES,
                "an alias cannot have properties",
                true,
            );
            return false;
        }
        if !self.merge_props(props) {
            self.fail(DUPLICATE_PROPERTY, "duplicate anchor or tag", true);
            return false;
        }
        let props = core::mem::take(&mut self.props);
        let span = Span::new(content_start, end);
        events.push(if scalar {
            Event::scalar(span, style, props, None)
        } else {
            Event::new(EventKind::Alias, span)
        });
        self.begin_content();
        self.commit_scratch(events);
        self.child_done();
        true
    }

    /// The head is a plain block mapping value on its entry's line, so a
    /// `: ` after its first line is an inline colon (module docs).
    fn inline_colon_value(&self, ctx: NodeCtx, content: &Content<'a>) -> bool {
        let plain = matches!(
            content,
            Content::Scalar {
                style: ScalarStyle::Plain,
                ..
            }
        );
        let in_value = matches!(
            self.stack.last(),
            Some(Frame::Map {
                phase: MapPhase::Value,
                ..
            })
        );
        plain && in_value && !self.fresh && !ctx.compact
    }

    /// An inline colon in a plain value (module docs): the YAML 1.2 error with
    /// its quoting fix, or, under `lenient_inline_colons`, one plain scalar
    /// and a warning.
    fn inline_colon(&mut self, mut key: KeyHead<'a>, ctx: NodeCtx) {
        self.mark_before_error = None;
        let src = self.src;
        let start = key.content_start;
        let colon = key.colon;
        let first_end = plain_line_end_lenient(src, start);
        let full = scalar::plain(src, start, first_end, ctx.parent, false);
        // Quoting fixes the whole problem unless a continuation line also
        // has a `: ` (then it is a multi-line key, reported on its own).
        // A character a quoted scalar cannot hold (a C0 control read into
        // the lenient first line) keeps the quoted value invalid.
        let quotable = src
            .get(start..full.end)
            .is_some_and(|t| t.chars().all(|c| is_nb_json(c) || c == '\n' || c == '\r'));
        // ... and so does anything left on the line after the scalar
        // (it stopped before a character a plain scalar cannot hold).
        let rest = skip_white(src, full.end);
        let clean_rest = match byte(src, rest) {
            None | Some(b'\n' | b'\r') => true,
            Some(b'#') => rest > full.end,
            _ => false,
        };
        let applicability = if !quotable
            || !clean_rest
            || full.multiline && indicator_at(src, skip_white(src, full.end), b':')
        {
            Applicability::MaybeIncorrect
        } else {
            Applicability::MachineApplicable
        };
        // A byte order mark ends the plain scalar (it is no
        // `ns-char`), but the text glued after it belongs to the value the
        // writer meant (`a: b\u{feff}c`): the fix quotes through the rest
        // of the line (before a comment), leaving the mark out. It overlaps
        // the mark's own fix, so it is only offered (maybe-incorrect).
        let fix = match glued_after_mark(src, full.end).filter(|_| !full.multiline) {
            Some(end) => quote_fix(src, start, end, Applicability::MaybeIncorrect),
            None => quote_fix(src, start, full.end, applicability),
        };
        let colon_label = Span::new(colon, colon.saturating_add(1));
        if !self.lenient_inline_colons {
            // `b:` can still be the start of a longer scalar; the character
            // after the colon is the first that cannot belong.
            self.blame.set(Some(colon.saturating_add(1)));
            let message = "`: ` inside an unquoted value starts a nested mapping, which cannot begin on this line";
            return self.fail_with(INLINE_COLON_IN_PLAIN_VALUE, message, true, |d| {
                d.with_secondary(Label::new(colon_label, "this `: ` starts a mapping"))
                    .with_help(
                        "to keep the `: ` as text, quote the whole value; a nested mapping \
                         must start on its own, more indented line \
                         (Settings::lenient_inline_colons accepts such values with a warning)",
                    )
                    .with_suggestion(fix)
            });
        }
        // The opt-in leniency: the rest of the line is part of the plain
        // scalar.
        key.content = Content::Scalar {
            style: ScalarStyle::Plain,
            len: first_end.saturating_sub(start),
            multiline: false,
            first_end,
        };
        key.content_end = first_end;
        if self.emit_node(key, ctx) {
            let warning = Diagnostic::warning(
                LENIENT_INLINE_COLON,
                "`: ` inside an unquoted value was read as text (lenient_inline_colons)",
                Label::new(colon_label, "read as text, not as the start of a mapping"),
            )
            .with_help("quote the value to make the document portable YAML")
            .with_suggestion(fix);
            self.zero(EventKind::Warning(Box::new(warning)));
        }
    }

    /// A key line without `:` (`MISSING_MAPPING_COLON`). When the key is a
    /// plain scalar holding a `:` glued to the next character (`b:2`), the
    /// diagnostic points at that `:` and offers the space (diagnostic polish).
    fn missing_colon(&mut self, key: &KeyHead<'a>) {
        let src = self.src;
        let glued = match key.content {
            Content::Scalar {
                style: ScalarStyle::Plain,
                first_end,
                ..
            } => src.get(key.content_start..first_end).and_then(|text| {
                let k = text.char_indices().skip(1).find(|(_, c)| *c == ':')?.0;
                key.content_start.checked_add(k)
            }),
            _ => None,
        };
        let message = "a mapping key needs `:`";
        // The key line ends where its text does: no `:` follows.
        self.blame.set(Some(key.content_end));
        let Some(at) = glued else {
            return self.fail(MISSING_MAPPING_COLON, message, false);
        };
        let after = at.saturating_add(1);
        self.fail_with(MISSING_MAPPING_COLON, message, false, |d| {
            d.with_secondary(Label::new(Span::new(at, after), "no space after this `:`"))
                .with_help(
                    "a `:` separates a key from its value only when a space or the end of \
                     the line follows it, so this line is one plain scalar, not `key: value`",
                )
                .with_suggestion(Suggestion::new(
                    "add a space after the `:`",
                    vec![Replacement::new(Span::empty(after), " ")],
                    Applicability::MaybeIncorrect,
                ))
        });
    }

    /// A failed node head's content at `at` (diagnostic polish, `polish.rs`):
    /// the same diagnostic, with labels, help, and fixes where they apply.
    fn fail_content(
        &mut self,
        code: Code,
        message: &str,
        slot_open: bool,
        at: usize,
        place: Place,
    ) {
        match polish::content(self.src, code, at, place) {
            Some(p) => self.fail_with(code, message, slot_open, |d| p.apply(d)),
            None => self.fail(code, message, slot_open),
        }
    }

    /// Content right after a node (`UNEXPECTED_CONTENT`), polished when it
    /// is glued to the closing quote of the quoted scalar just parsed
    /// (`'it's'`, `"a"#c`).
    pub(super) fn unexpected_content(&mut self) {
        let message = self.glued_to_quote_message();
        let parent = match self.stack.last() {
            Some(Frame::Map { indent, .. } | Frame::Seq { indent }) => Some(*indent),
            _ => None,
        };
        match polish::after_quote(self.src, self.last_quoted, self.pos, parent) {
            Some(p) => self.fail_with(UNEXPECTED_CONTENT, message, false, |d| p.apply(d)),
            None => self.fail(UNEXPECTED_CONTENT, message, false),
        }
    }

    /// The message for content right after a node. For text glued to the
    /// closing quote of the quoted scalar just parsed it names the cause
    /// (an inner quote only when the text runs to the same quote again),
    /// as most readers never see the labels; the help says what to write.
    fn glued_to_quote_message(&self) -> &'static str {
        match polish::glued(self.src, self.last_quoted, self.pos) {
            Some(Glued::Comment) => "a `#` right after a closing quote does not start a comment",
            Some(Glued::InnerQuote(b'\'')) => "a `'` inside a single-quoted scalar ends it early",
            Some(Glued::InnerQuote(_)) => "a `\"` inside a double-quoted scalar ends it early",
            Some(Glued::Text(_)) => {
                "unexpected text right after the closing quote of a quoted scalar"
            }
            None => "unexpected content after a node",
        }
    }

    /// After an implicit key's `:`: the value (`c-l-block-map-implicit-value`,
    /// production 194) of the mapping with keys at `indent`.
    pub(super) fn set_key_value(&mut self, indent: usize) {
        if let Some(Frame::Map { phase, .. }) = self.stack.last_mut() {
            *phase = MapPhase::Value;
        }
        self.state = State::Node(NodeCtx {
            parent: Some(indent),
            compact: false,
            seq_at_parent: true,
        });
    }

    /// Emits a key head: properties, the key node, white space, and `:`.
    fn commit_key(&mut self, key: KeyHead<'a>) {
        let KeyHead {
            mut events,
            props,
            content,
            content_start,
            content_end,
            props_end,
            colon,
        } = key;
        let node = match content {
            Content::Alias(_) => event(EventKind::Alias, content_start, content_end),
            Content::Scalar { style, .. } => {
                Event::scalar(Span::new(content_start, content_end), style, props, None)
            }
            Content::Empty => {
                // Placement rule 1: right after the last property.
                if content_start > props_end {
                    pop_white(&mut events);
                }
                Event::scalar(Span::empty(props_end), ScalarStyle::Plain, props, None)
            }
        };
        events.push(node);
        // White space, and byte order marks skipped as if absent,
        // between the key and its `:`.
        self.mark_before_error = None;
        let src = self.src;
        let mut at = content_end;
        while at < colon {
            let white = skip_white(src, at);
            if white > at {
                events.push(event(EventKind::Trivia(TriviaKind::Whitespace), at, white));
                at = white;
            } else if mark_at(src, at) {
                let end = at.saturating_add('\u{feff}'.len_utf8());
                events.push(event(
                    EventKind::Error(super::misplaced_bom_diagnostic(at, end)),
                    at,
                    end,
                ));
                at = end;
            } else {
                // `colon` was found by skipping only these.
                debug_assert!(false, "unexpected byte before a key's `:`");
                events.push(event(
                    EventKind::Error(super::internal(
                        "bytes before a key's `:`",
                        Span::new(at, colon),
                    )),
                    at,
                    colon,
                ));
                at = colon;
            }
        }
        events.push(event(
            EventKind::Trivia(TriviaKind::Indicator(Indicator::MappingValue)),
            colon,
            colon.saturating_add(1),
        ));
        self.commit_scratch(events);
    }
}
