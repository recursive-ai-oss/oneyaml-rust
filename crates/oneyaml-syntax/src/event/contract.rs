//! The executable form of the event-stream contract (C1–C6 in the
//! [module docs](super)).

use oneyaml_core::{Severity, Span};

use super::{
    BlockHeader, Chomping, CollectionStyle, DirectiveKind, Event, EventKind, Indicator,
    ScalarStyle, Tag, TagForm, TriviaKind,
};

/// A contract violation at a given event.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractError {
    /// Index of the offending event (`events.len()` for "missing at end").
    pub index: usize,
    /// What is wrong.
    pub violation: Violation,
}

/// The rule an event stream breaks.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// C1: the event starts at `found`, the previous one ended at `expected`.
    Discontiguous {
        /// Where the event should start.
        expected: usize,
        /// Where it starts.
        found: usize,
    },
    /// C1: inverted, out of bounds, or not on `char` boundaries.
    InvalidSpan {
        /// The span.
        span: Span,
    },
    /// C1: the events end at `covered`, the source is `len` bytes.
    Incomplete {
        /// Bytes covered.
        covered: usize,
        /// Source length.
        len: usize,
    },
    /// C1: a trivia event is zero-width.
    EmptyTrivia,
    /// C2: the bytes do not match the event kind.
    BadBytes {
        /// What the bytes should have been.
        expected: &'static str,
    },
    /// C2: a line break inside an event that may not contain one.
    LineBreak,
    /// C2: payload meaning disagrees with the bytes.
    ValueMismatch {
        /// Which payload.
        detail: &'static str,
    },
    /// C3: the event is not allowed where it occurs.
    Misplaced {
        /// The event, in suite notation (`+MAP`, `-SEQ`, …) or indicator text.
        found: &'static str,
        /// The innermost open frame (`STR`, `DOC`, `MAP`, `SEQ`, `none`).
        open: &'static str,
    },
    /// C3: a document, mapping, or flow pair has the wrong number of nodes.
    ChildCount {
        /// `DOC`, `MAP`, or `PAIR`.
        container: &'static str,
        /// Nodes counted.
        count: usize,
    },
    /// C3: the first event is not `StreamStart`.
    MissingStreamStart,
    /// C3: the last event is not `StreamEnd`.
    MissingStreamEnd,
    /// C3: an event after `StreamEnd`.
    AfterStreamEnd,
    /// C3: `StreamEnd` while something is still open.
    Unclosed {
        /// The innermost open frame.
        open: &'static str,
    },
    /// C3: a node's properties do not match the preceding property trivia.
    PropertyMismatch,
    /// C3: property trivia not followed by a node carrying it.
    DanglingProperty,
    /// C3: two anchors (or two tags, or two headers) for one node.
    DuplicateProperty,
    /// C5: block header payload disagrees with the header trivia.
    BlockHeaderMismatch,
    /// C5: header trivia not followed by its block scalar.
    DanglingBlockHeader,
    /// C5: block scalar span in the wrong place.
    BlockScalarPlacement,
    /// C4: a zero-width start is followed by whitespace/break/comment trivia.
    StartNotLate,
    /// C4: a zero-width end is preceded by trivia.
    EndNotEarly,
    /// C3: a directive not followed by an explicit document start.
    DirectiveWithoutDocument,
    /// C3: an implicit document start after a document without `...`.
    ImplicitDocumentStart,
    /// C6: an `Error`/`Warning` diagnostic with the other severity.
    SeverityMismatch,
    /// C6: `Skipped` trivia before any error.
    SkippedWithoutError,
    /// C6: a zero-width flow collection end without an error inside it.
    ZeroWidthFlowEnd,
    /// C4: an empty node (zero-width plain scalar) in the wrong place.
    EmptyNodePlacement,
    /// C3: a directive after a document that was not ended with `...`.
    DirectiveAfterUnterminatedDocument,
}

impl core::fmt::Display for Violation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Discontiguous { expected, found } => {
                write!(f, "starts at byte {found}, expected {expected}")
            }
            Self::InvalidSpan { span } => write!(f, "invalid span {}..{}", span.start, span.end),
            Self::Incomplete { covered, len } => {
                write!(f, "events cover {covered} of {len} source bytes")
            }
            Self::EmptyTrivia => f.write_str("zero-width trivia"),
            Self::BadBytes { expected } => write!(f, "bytes are not {expected}"),
            Self::LineBreak => f.write_str("line break outside LineBreak trivia or a scalar"),
            Self::ValueMismatch { detail } => write!(f, "payload disagrees with bytes: {detail}"),
            Self::Misplaced { found, open } => write!(f, "unexpected {found} while {open} is open"),
            Self::ChildCount { container, count } => {
                write!(f, "{container} has {count} nodes")
            }
            Self::MissingStreamStart => f.write_str("stream does not begin with StreamStart"),
            Self::MissingStreamEnd => f.write_str("stream does not end with StreamEnd"),
            Self::AfterStreamEnd => f.write_str("event after StreamEnd"),
            Self::Unclosed { open } => write!(f, "stream ended while {open} is open"),
            Self::PropertyMismatch => f.write_str("node properties do not match property trivia"),
            Self::DanglingProperty => f.write_str("property trivia without a node"),
            Self::DuplicateProperty => f.write_str("duplicate property"),
            Self::BlockHeaderMismatch => f.write_str("block header payload disagrees with header"),
            Self::DanglingBlockHeader => f.write_str("block header without its block scalar"),
            Self::BlockScalarPlacement => f.write_str("block scalar span misplaced"),
            Self::StartNotLate => f.write_str("zero-width start is followed by trivia"),
            Self::EndNotEarly => f.write_str("zero-width end is preceded by trivia"),
            Self::DirectiveWithoutDocument => f.write_str("directive without explicit document"),
            Self::ImplicitDocumentStart => {
                f.write_str("implicit document start after a document without `...`")
            }
            Self::SeverityMismatch => f.write_str("diagnostic severity does not match event"),
            Self::SkippedWithoutError => f.write_str("skipped bytes before any error"),
            Self::ZeroWidthFlowEnd => f.write_str("zero-width flow end without an error"),
            Self::EmptyNodePlacement => f.write_str("empty node misplaced"),
            Self::DirectiveAfterUnterminatedDocument => {
                f.write_str("directive after a document not ended with `...`")
            }
        }
    }
}

impl core::fmt::Display for ContractError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "event {}: {}", self.index, self.violation)
    }
}

impl core::error::Error for ContractError {}

const fn fail<T>(index: usize, violation: Violation) -> Result<T, ContractError> {
    Err(ContractError { index, violation })
}

/// Checks C1 only (tiling). When `Ok`, concatenating the spanned text of
/// `events` ([`Sourced::text`](super::Sourced::text)) reproduces `source` byte for byte.
///
/// # Errors
///
/// The first tiling violation.
pub fn check_tiling(source: &str, events: &[Event<'_>]) -> Result<(), ContractError> {
    let mut cursor = 0usize;
    for (index, event) in events.iter().enumerate() {
        let span = event.span();
        if span.start != cursor {
            return fail(
                index,
                Violation::Discontiguous {
                    expected: cursor,
                    found: span.start,
                },
            );
        }
        if event.source(source).is_none() {
            return fail(index, Violation::InvalidSpan { span });
        }
        if event.is_trivia() && span.is_empty() {
            return fail(index, Violation::EmptyTrivia);
        }
        cursor = span.end;
    }
    if cursor == source.len() {
        Ok(())
    } else {
        fail(
            events.len(),
            Violation::Incomplete {
                covered: cursor,
                len: source.len(),
            },
        )
    }
}

/// Checks the whole contract (C1–C6) for `events` over `source`.
///
/// Runs in one pass with an explicit stack.
///
/// # Errors
///
/// The first violation found.
pub fn check_contract<'a>(source: &'a str, events: &[Event<'a>]) -> Result<(), ContractError> {
    check_tiling(source, events)?;
    let mut checker = Checker::new(source, events);
    for (index, event) in events.iter().enumerate() {
        checker
            .step(index, event)
            .map_err(|violation| ContractError { index, violation })?;
    }
    checker.finish()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    Stream,
    Document,
    Mapping(CollectionStyle),
    Sequence(CollectionStyle),
}

struct Frame {
    kind: FrameKind,
    children: usize,
    error_inside: bool,
    /// Mappings: a `:` was seen for the current entry.
    colon: bool,
    /// Mappings: a `?` was seen for the current entry.
    question: bool,
}

impl Frame {
    const fn new(kind: FrameKind) -> Self {
        Self {
            kind,
            children: 0,
            error_inside: false,
            colon: false,
            question: false,
        }
    }
}

/// Which node slot a node event fills (decides C4 empty-node placement).
#[derive(Clone, Copy)]
enum Slot {
    Document,
    BlockSequence,
    FlowSequence,
    Key { question: bool },
    Value { colon: bool },
}

const fn frame_name(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::Stream => "STR",
        FrameKind::Document => "DOC",
        FrameKind::Mapping(_) => "MAP",
        FrameKind::Sequence(_) => "SEQ",
    }
}

fn has_break(s: &str) -> bool {
    s.contains(['\n', '\r'])
}

fn is_blank_char(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// `!`, `!!`, or `!word!` (§6.8.2.1, productions 89–92).
fn valid_handle(handle: &str) -> bool {
    if handle == "!" || handle == "!!" {
        return true;
    }
    handle
        .strip_prefix('!')
        .and_then(|h| h.strip_suffix('!'))
        .is_some_and(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn tag_matches(text: &str, tag: &Tag<'_>) -> bool {
    if tag.resolved.is_empty() {
        return false;
    }
    match tag.form {
        TagForm::Verbatim => {
            tag.handle.is_empty()
                && !tag.suffix.is_empty()
                && text.strip_prefix("!<").and_then(|t| t.strip_suffix('>')) == Some(tag.suffix)
        }
        // ns-tag-char excludes `!` and flow indicators (§6.9.1, production
        // 40), so the handle/suffix split is unique.
        TagForm::Shorthand => {
            valid_handle(tag.handle)
                && !tag.suffix.is_empty()
                && !tag.suffix.contains(['!', ',', '[', ']', '{', '}'])
                && text.strip_prefix(tag.handle) == Some(tag.suffix)
        }
        TagForm::NonSpecific => {
            text == "!" && tag.handle == "!" && tag.suffix.is_empty() && tag.resolved == "!"
        }
    }
}

fn directive_matches(text: &str, directive: &DirectiveKind<'_>) -> bool {
    let Some(body) = text.strip_prefix('%') else {
        return false;
    };
    if text.ends_with([' ', '\t']) {
        return false;
    }
    let mut tokens = body.split([' ', '\t']).filter(|t| !t.is_empty());
    let name = tokens.next().unwrap_or("");
    if name.is_empty() || !body.starts_with(name) {
        return false;
    }
    match *directive {
        DirectiveKind::Yaml { version } => {
            name == "YAML" && tokens.next() == Some(version) && tokens.next().is_none()
        }
        DirectiveKind::Tag { handle, prefix } => {
            name == "TAG"
                && valid_handle(handle)
                && tokens.next() == Some(handle)
                && tokens.next() == Some(prefix)
                && tokens.next().is_none()
        }
        DirectiveKind::Reserved {
            name: reserved,
            params,
        } => {
            reserved == name
                && name != "YAML"
                && name != "TAG"
                && body
                    .get(name.len()..)
                    .map(|rest| rest.trim_start_matches([' ', '\t']))
                    == Some(params)
        }
    }
}

/// Parses a header's indicators (§8.1.1, production 162) and compares them
/// with the payload.
fn header_matches(text: &str, style: ScalarStyle, header: &BlockHeader) -> bool {
    let mut chars = text.chars();
    let lead = match style {
        ScalarStyle::Literal => '|',
        ScalarStyle::Folded => '>',
        _ => return false,
    };
    if chars.next() != Some(lead) {
        return false;
    }
    let mut indent: Option<u8> = None;
    let mut chomping = Chomping::Clip;
    let mut seen_chomp = false;
    for c in chars {
        match c {
            '1'..='9' if indent.is_none() => {
                indent = c.to_digit(10).and_then(|d| u8::try_from(d).ok());
            }
            '+' | '-' if !seen_chomp => {
                seen_chomp = true;
                chomping = if c == '+' {
                    Chomping::Keep
                } else {
                    Chomping::Strip
                };
            }
            _ => return false,
        }
    }
    indent == header.indent_indicator && chomping == header.chomping
}

/// `ns-char` (production 34): a printable character (production 1) other
/// than space, tab, a line break, or a byte order mark.
const fn is_ns_char(c: char) -> bool {
    matches!(c,
        '\x21'..='\x7e' | '\u{85}' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fefe}'
        | '\u{ff00}'..='\u{fffd}' | '\u{10000}'..)
}

/// Anchor and alias names: `ns-anchor-char+` (§6.9.2, production 102), i.e.
/// `ns-char` minus the flow indicators. Unicode white space other than space
/// and tab (e.g. U+00A0) *is* allowed.
fn valid_anchor_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| is_ns_char(c) && !matches!(c, ',' | '[' | ']' | '{' | '}'))
}

/// A line consisting only of at most `indent` spaces (an `l-empty` line,
/// production 70, when followed by a break).
fn is_empty_line(line: &str, indent: usize) -> bool {
    line.bytes().all(|b| b == b' ') && line.len() <= indent
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// A line that would continue a block scalar with content indentation `n`:
/// at least `n` spaces followed by more text (productions 173, 181). At
/// `n == 0` a document marker ends the scalar instead.
fn is_content_line(line: &str, n: usize) -> bool {
    let marker = (line.starts_with("---") || line.starts_with("..."))
        && line
            .get(3..)
            .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t']));
    // A byte order mark at column 0 starts a document prefix, never
    // content (production 211).
    let prefix = line.starts_with('\u{feff}');
    leading_spaces(line) >= n && line.len() > n && !(n == 0 && (marker || prefix))
}

/// Splits text into lines at `\n`, `\r\n`, or `\r`, yielding each line
/// without its break and whether it had one. A trailing empty segment after a
/// final break is not yielded.
struct Lines<'t> {
    rest: &'t str,
}

impl<'t> Iterator for Lines<'t> {
    type Item = (&'t str, bool);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let Some(at) = self.rest.find(['\r', '\n']) else {
            let line = self.rest;
            self.rest = "";
            return Some((line, false));
        };
        let line = self.rest.get(..at).unwrap_or("");
        let from_break = self.rest.get(at..).unwrap_or("");
        let width = if from_break.starts_with("\r\n") { 2 } else { 1 };
        self.rest = from_break.get(width..).unwrap_or("");
        Some((line, true))
    }
}

const fn lines(text: &str) -> Lines<'_> {
    Lines { rest: text }
}

struct Checker<'e, 'a> {
    source: &'a str,
    events: &'e [Event<'a>],
    stack: Vec<Frame>,
    started: bool,
    ended: bool,
    errors_seen: bool,
    pending_anchor: Option<Span>,
    pending_tag: Option<Span>,
    pending_header: Option<Span>,
    breaks_since_header: usize,
    pending_directive: bool,
    error_since_directive: bool,
    docs_seen: bool,
    last_end_explicit: bool,
    error_since_doc_end: bool,
    /// A byte order mark (a document prefix) at stream level since the
    /// last document end (production 211).
    prefix_since_doc_end: bool,
}

type Step = Result<(), Violation>;

impl<'e, 'a> Checker<'e, 'a> {
    const fn new(source: &'a str, events: &'e [Event<'a>]) -> Self {
        Self {
            source,
            events,
            stack: Vec::new(),
            started: false,
            ended: false,
            errors_seen: false,
            pending_anchor: None,
            pending_tag: None,
            pending_header: None,
            breaks_since_header: 0,
            pending_directive: false,
            error_since_directive: false,
            docs_seen: false,
            last_end_explicit: false,
            error_since_doc_end: false,
            prefix_since_doc_end: false,
        }
    }

    fn finish(self) -> Result<(), ContractError> {
        if !self.started {
            return fail(0, Violation::MissingStreamStart);
        }
        if !self.ended {
            return fail(self.events.len(), Violation::MissingStreamEnd);
        }
        Ok(())
    }

    fn top(&self) -> Option<FrameKind> {
        self.stack.last().map(|f| f.kind)
    }

    fn open(&self) -> &'static str {
        self.top().map_or("none", frame_name)
    }

    fn misplaced(&self, found: &'static str) -> Violation {
        Violation::Misplaced {
            found,
            open: self.open(),
        }
    }

    fn text(&self, span: Span) -> Result<&'a str, Violation> {
        self.source
            .get(span.start..span.end)
            .ok_or(Violation::InvalidSpan { span })
    }

    fn no_pending(&self) -> Step {
        if self.pending_anchor.is_some() || self.pending_tag.is_some() {
            return Err(Violation::DanglingProperty);
        }
        if self.pending_header.is_some() {
            return Err(Violation::DanglingBlockHeader);
        }
        Ok(())
    }

    /// C4 "starts late".
    fn start_late(&self, index: usize) -> Step {
        let next = self
            .events
            .get(index.saturating_add(1)..)
            .and_then(|rest| rest.iter().find(|e| !e.is_warning()));
        match next.map(Event::kind) {
            Some(EventKind::Trivia(
                TriviaKind::Anchor
                | TriviaKind::Tag
                | TriviaKind::Indicator(_)
                | TriviaKind::BlockScalarHeader,
            )) => Ok(()),
            Some(EventKind::Trivia(_)) | None => Err(Violation::StartNotLate),
            Some(_) => Ok(()),
        }
    }

    fn prev_non_warning(&self, index: usize) -> Option<&'e Event<'a>> {
        self.events
            .get(..index)
            .and_then(|before| before.iter().rev().find(|e| !e.is_warning()))
    }

    /// C4 "ends early".
    fn end_early(&self, index: usize) -> Step {
        match self.prev_non_warning(index) {
            Some(e) if e.is_trivia() => Err(Violation::EndNotEarly),
            _ => Ok(()),
        }
    }

    fn pop(&mut self) -> Option<Frame> {
        let frame = self.stack.pop()?;
        if frame.error_inside {
            if let Some(parent) = self.stack.last_mut() {
                parent.error_inside = true;
            }
        }
        Some(frame)
    }

    fn step(&mut self, index: usize, event: &Event<'a>) -> Step {
        if self.ended {
            return Err(Violation::AfterStreamEnd);
        }
        if !self.started && !matches!(event.kind(), EventKind::StreamStart) {
            return Err(Violation::MissingStreamStart);
        }
        let span = event.span();
        let text = self.text(span)?;
        let may_break = matches!(
            event.kind(),
            EventKind::Trivia(TriviaKind::LineBreak) | EventKind::Scalar { .. }
        );
        if !may_break && has_break(text) {
            return Err(Violation::LineBreak);
        }
        let zero_width = |what: &'static str| {
            if span.is_empty() {
                Ok(())
            } else {
                Err(Violation::BadBytes { expected: what })
            }
        };
        let exact = |want: &'static str| {
            if text == want {
                Ok(())
            } else {
                Err(Violation::BadBytes { expected: want })
            }
        };

        match event.kind() {
            EventKind::StreamStart => {
                zero_width("zero-width")?;
                if self.started {
                    return Err(self.misplaced("+STR"));
                }
                self.started = true;
                self.stack.push(Frame::new(FrameKind::Stream));
            }
            EventKind::StreamEnd => {
                zero_width("zero-width")?;
                self.no_pending()?;
                if self.pending_directive && !self.error_since_directive {
                    return Err(Violation::DirectiveWithoutDocument);
                }
                if self.top() != Some(FrameKind::Stream) || self.stack.len() != 1 {
                    return Err(Violation::Unclosed { open: self.open() });
                }
                self.pop();
                self.ended = true;
            }
            EventKind::Directive(which) => {
                self.no_pending()?;
                if self.top() != Some(FrameKind::Stream) {
                    return Err(self.misplaced("%"));
                }
                // l-directive-document may only follow a document ended by
                // `...` (productions 210-211).
                if self.docs_seen && !self.last_end_explicit && !self.error_since_doc_end {
                    return Err(Violation::DirectiveAfterUnterminatedDocument);
                }
                let read = event.directive(self.source);
                let ok =
                    read.is_some_and(|d| d.directive() == *which && directive_matches(text, &d));
                if !ok {
                    return Err(Violation::ValueMismatch {
                        detail: "directive payload",
                    });
                }
                self.pending_directive = true;
                self.error_since_directive = false;
            }
            EventKind::DocumentStart { explicit } => {
                self.no_pending()?;
                if self.top() != Some(FrameKind::Stream) {
                    return Err(self.misplaced("+DOC"));
                }
                if *explicit {
                    exact("---")?;
                } else {
                    zero_width("zero-width")?;
                    if self.pending_directive && !self.error_since_directive {
                        return Err(Violation::DirectiveWithoutDocument);
                    }
                    if self.docs_seen && !self.last_end_explicit && !self.error_since_doc_end {
                        return Err(Violation::ImplicitDocumentStart);
                    }
                    self.start_late(index)?;
                }
                self.pending_directive = false;
                self.docs_seen = true;
                self.stack.push(Frame::new(FrameKind::Document));
            }
            EventKind::DocumentEnd { explicit } => {
                self.no_pending()?;
                let open = self.open();
                let frame = match self.pop() {
                    Some(frame) if frame.kind == FrameKind::Document => frame,
                    _ => {
                        return Err(Violation::Misplaced {
                            found: "-DOC",
                            open,
                        });
                    }
                };
                if frame.children != 1 {
                    return Err(Violation::ChildCount {
                        container: "DOC",
                        count: frame.children,
                    });
                }
                if *explicit {
                    exact("...")?;
                } else {
                    zero_width("zero-width")?;
                    if !frame.error_inside {
                        self.end_early(index)?;
                    }
                }
                self.last_end_explicit = *explicit;
                self.error_since_doc_end = false;
                self.prefix_since_doc_end = false;
            }
            EventKind::MappingStart { style, .. } => {
                if *style == CollectionStyle::FlowPair
                    && self.top() != Some(FrameKind::Sequence(CollectionStyle::Flow))
                {
                    return Err(self.misplaced("+MAP (flow pair)"));
                }
                self.node(event, "+MAP")?;
                match style {
                    CollectionStyle::Flow => exact("{")?,
                    CollectionStyle::Block | CollectionStyle::FlowPair => {
                        zero_width("zero-width")?;
                        self.start_late(index)?;
                    }
                }
                self.stack.push(Frame::new(FrameKind::Mapping(*style)));
            }
            EventKind::SequenceStart { style, .. } => {
                self.node(event, "+SEQ")?;
                match style {
                    CollectionStyle::Flow => exact("[")?,
                    CollectionStyle::Block => {
                        zero_width("zero-width")?;
                        self.start_late(index)?;
                    }
                    CollectionStyle::FlowPair => return Err(self.misplaced("+SEQ (flow pair)")),
                }
                self.stack.push(Frame::new(FrameKind::Sequence(*style)));
            }
            EventKind::MappingEnd | EventKind::SequenceEnd => {
                self.no_pending()?;
                let is_map = matches!(event.kind(), EventKind::MappingEnd);
                let (found, close) = if is_map { ("-MAP", "}") } else { ("-SEQ", "]") };
                let open = self.open();
                let frame = self.pop();
                let (frame, style) = match (frame, is_map) {
                    (
                        Some(
                            f @ Frame {
                                kind: FrameKind::Mapping(s),
                                ..
                            },
                        ),
                        true,
                    )
                    | (
                        Some(
                            f @ Frame {
                                kind: FrameKind::Sequence(s),
                                ..
                            },
                        ),
                        false,
                    ) => (f, s),
                    _ => return Err(Violation::Misplaced { found, open }),
                };
                let odd = frame.children & 1 == 1;
                if is_map && (odd || (style == CollectionStyle::FlowPair && frame.children != 2)) {
                    return Err(Violation::ChildCount {
                        container: if style == CollectionStyle::FlowPair {
                            "PAIR"
                        } else {
                            "MAP"
                        },
                        count: frame.children,
                    });
                }
                match style {
                    CollectionStyle::Flow if span.is_empty() => {
                        if !frame.error_inside {
                            return Err(Violation::ZeroWidthFlowEnd);
                        }
                    }
                    CollectionStyle::Flow => exact(close)?,
                    CollectionStyle::Block | CollectionStyle::FlowPair => {
                        zero_width("zero-width")?;
                        if !frame.error_inside {
                            self.end_early(index)?;
                        }
                    }
                }
            }
            EventKind::Scalar { style, info } => {
                let slot = self.node(event, "=VAL")?;
                if span.is_empty() && *style == ScalarStyle::Plain {
                    self.empty_node(index, slot, !info.has_no_properties())?;
                }
                self.scalar(index, event, text, *style)?;
            }
            EventKind::Alias => {
                self.node(event, "=ALI")?;
                let name = event.alias_name(self.source).unwrap_or("");
                if !valid_anchor_name(name) || text.strip_prefix('*') != Some(name) {
                    return Err(Violation::BadBytes {
                        expected: "`*` followed by the alias name",
                    });
                }
            }
            EventKind::Trivia(kind) => self.trivia(span, text, *kind)?,
            EventKind::Warning(diagnostic) => {
                zero_width("zero-width")?;
                if diagnostic.severity != Severity::Warning {
                    return Err(Violation::SeverityMismatch);
                }
            }
            EventKind::Error(diagnostic) => {
                if diagnostic.severity != Severity::Error {
                    return Err(Violation::SeverityMismatch);
                }
                self.errors_seen = true;
                // R2: an error abandons a pending block scalar header; the
                // repair is an empty plain scalar, never that block scalar.
                self.pending_header = None;
                self.breaks_since_header = 0;
                self.error_since_directive = true;
                self.error_since_doc_end = true;
                if let Some(frame) = self.stack.last_mut() {
                    frame.error_inside = true;
                }
            }
        }
        Ok(())
    }

    /// A node event: context, child count, and property agreement (C3).
    /// Returns the slot the node fills.
    fn node(&mut self, event: &Event<'a>, found: &'static str) -> Result<Slot, Violation> {
        let open = self.open();
        let frame = match self.stack.last_mut() {
            Some(frame) if frame.kind != FrameKind::Stream => frame,
            _ => return Err(Violation::Misplaced { found, open }),
        };
        let slot = match frame.kind {
            FrameKind::Mapping(_) if frame.children & 1 == 0 => Slot::Key {
                question: frame.question,
            },
            FrameKind::Mapping(_) => {
                let colon = frame.colon;
                frame.colon = false;
                frame.question = false;
                Slot::Value { colon }
            }
            FrameKind::Sequence(CollectionStyle::Block) => Slot::BlockSequence,
            FrameKind::Sequence(_) => Slot::FlowSequence,
            FrameKind::Document | FrameKind::Stream => Slot::Document,
        };
        let container = match frame.kind {
            FrameKind::Stream => None,
            FrameKind::Document => Some("DOC"),
            FrameKind::Mapping(CollectionStyle::FlowPair) => Some("PAIR"),
            FrameKind::Mapping(_) | FrameKind::Sequence(_) => None,
        };
        frame.children = frame.children.saturating_add(1);
        let limit = if container == Some("DOC") { 1 } else { 2 };
        if let Some(container) = container {
            if frame.children > limit {
                return Err(Violation::ChildCount {
                    container,
                    count: frame.children,
                });
            }
        }

        // The positions the node carries must be exactly the property trivia
        // before it, and read back as the same spans.
        let props = event.properties().unwrap_or_default();
        let anchor = props.anchor.map(|_| event.anchor(self.source));
        let tag = props.tag.map(|_| event.tag(self.source));
        let anchor_span = anchor.map(|a| a.map(|a| a.span));
        let tag_span = tag.as_ref().map(|t| t.as_ref().map(|t| t.span));
        if anchor_span != self.pending_anchor.map(Some) || tag_span != self.pending_tag.map(Some) {
            return Err(Violation::PropertyMismatch);
        }
        if props.tag.is_none() && props.tag_prefix.is_some() {
            return Err(Violation::ValueMismatch { detail: "tag" });
        }
        if let Some(Some(anchor)) = anchor {
            let text = self.text(anchor.span)?;
            if !valid_anchor_name(anchor.name) || text.strip_prefix('&') != Some(anchor.name) {
                return Err(Violation::ValueMismatch {
                    detail: "anchor name",
                });
            }
        }
        if let Some(Some(tag)) = &tag {
            if !tag_matches(self.text(tag.span)?, tag) {
                return Err(Violation::ValueMismatch { detail: "tag" });
            }
        }
        self.pending_anchor = None;
        self.pending_tag = None;
        Ok(slot)
    }

    /// C4 empty-node placement. An empty node sits immediately after (1) its
    /// last property, else (2) the indicator that introduced it (`---`, `-`,
    /// `?`, `:`), else (3) for a value without `:`, its key; an empty key
    /// without `?` sits immediately before its `:`. A repair node (R2) sits
    /// immediately after its `Error`.
    fn empty_node(&self, index: usize, slot: Slot, has_props: bool) -> Step {
        let prev = self.prev_non_warning(index).map(Event::kind);
        let is = |want: Indicator| matches!(prev, Some(EventKind::Trivia(TriviaKind::Indicator(i))) if *i == want);
        let ok = if matches!(prev, Some(EventKind::Error(_))) {
            true
        } else if has_props {
            matches!(
                prev,
                Some(EventKind::Trivia(TriviaKind::Anchor | TriviaKind::Tag))
            )
        } else {
            match slot {
                Slot::Document => {
                    matches!(prev, Some(EventKind::DocumentStart { explicit: true }))
                }
                Slot::BlockSequence => is(Indicator::SequenceEntry),
                Slot::FlowSequence => false,
                Slot::Key { question: true } => is(Indicator::MappingKey),
                Slot::Key { question: false } => matches!(
                    self.events
                        .get(index.saturating_add(1)..)
                        .and_then(|rest| rest.iter().find(|e| !e.is_warning()))
                        .map(Event::kind),
                    Some(EventKind::Trivia(TriviaKind::Indicator(
                        Indicator::MappingValue
                    )))
                ),
                Slot::Value { colon: true } => is(Indicator::MappingValue),
                Slot::Value { colon: false } => matches!(
                    prev,
                    Some(
                        EventKind::Scalar { .. }
                            | EventKind::Alias
                            | EventKind::MappingEnd
                            | EventKind::SequenceEnd
                    )
                ),
            }
        };
        if ok {
            Ok(())
        } else {
            Err(Violation::EmptyNodePlacement)
        }
    }

    fn scalar(&mut self, index: usize, event: &Event<'a>, text: &str, style: ScalarStyle) -> Step {
        let block_style = matches!(style, ScalarStyle::Literal | ScalarStyle::Folded);
        if !block_style {
            if self.pending_header.is_some() {
                return Err(Violation::DanglingBlockHeader);
            }
            if event.block_header().is_some() {
                return Err(Violation::BlockHeaderMismatch);
            }
        }
        match style {
            ScalarStyle::Plain => {
                if text.starts_with(is_blank_char) || text.ends_with(is_blank_char) {
                    return Err(Violation::BadBytes {
                        expected: "a plain scalar without surrounding whitespace",
                    });
                }
            }
            ScalarStyle::SingleQuoted => {
                let quoted = text.len() >= 2 && text.starts_with('\'') && text.ends_with('\'');
                if !quoted {
                    return Err(Violation::BadBytes {
                        expected: "a '…' scalar",
                    });
                }
            }
            ScalarStyle::DoubleQuoted => {
                let inner = text
                    .strip_prefix('"')
                    .and_then(|t| t.strip_suffix('"'))
                    .ok_or(Violation::BadBytes {
                        expected: "a \"…\" scalar",
                    })?;
                let backslashes = inner.bytes().rev().take_while(|b| *b == b'\\').count();
                if backslashes & 1 == 1 {
                    return Err(Violation::BadBytes {
                        expected: "a \"…\" scalar with an unescaped closing quote",
                    });
                }
            }
            ScalarStyle::Literal | ScalarStyle::Folded => {
                return self.block_scalar(index, event, text, style);
            }
        }
        // The bytes are one complete scalar of this style, so its value
        // (escapes, folding) decodes. `Sourced::value` reads it on demand, so
        // a value cannot disagree with its bytes (module docs,
        // *Representation*).
        if event.value(self.source).is_none() {
            return Err(Violation::ValueMismatch {
                detail: "scalar value",
            });
        }
        Ok(())
    }

    /// C5.
    fn block_scalar(
        &mut self,
        index: usize,
        event: &Event<'a>,
        text: &str,
        style: ScalarStyle,
    ) -> Step {
        let span = event.span();
        let header = event.block_header().ok_or(Violation::BlockHeaderMismatch)?;
        if self.pending_header != Some(header.span) {
            return Err(Violation::BlockHeaderMismatch);
        }
        if !header_matches(self.text(header.span)?, style, &header) {
            return Err(Violation::BlockHeaderMismatch);
        }
        let at_eof = span.is_empty() && span.start == self.source.len();
        let after_break = matches!(
            self.prev_non_warning(index).map(Event::kind),
            Some(EventKind::Trivia(TriviaKind::LineBreak))
        );
        let breaks_ok = if at_eof {
            self.breaks_since_header <= 1
        } else {
            self.breaks_since_header == 1
        };
        if !breaks_ok || !(after_break || at_eof) {
            return Err(Violation::BlockScalarPlacement);
        }
        self.pending_header = None;
        self.breaks_since_header = 0;

        let n = header.content_indent;
        let mut last: Option<(&str, bool)> = None;
        for (line, has_break) in lines(text) {
            if !is_empty_line(line, n) && leading_spaces(line) < n {
                return Err(Violation::BlockScalarPlacement);
            }
            last = Some((line, has_break));
        }
        if let Some((line, has_break)) = last {
            // A span ends at a line break, or at end of input on a content
            // line: a final unterminated line of at most `n` spaces is
            // trivia after the scalar, never part of it.
            if !has_break && (span.end != self.source.len() || is_empty_line(line, n)) {
                return Err(Violation::BlockScalarPlacement);
            }
            // Strip/clip: trailing empty lines are outside the span.
            if header.chomping != Chomping::Keep && is_empty_line(line, n) {
                return Err(Violation::BlockScalarPlacement);
            }
        }
        // What follows must not belong to the scalar: no content line after
        // any empty lines, and for keep no empty line at all.
        let rest = self.source.get(span.end..).unwrap_or("");
        let mut empty_follows = false;
        for (line, has_break) in lines(rest) {
            if has_break && is_empty_line(line, n) {
                empty_follows = true;
                continue;
            }
            if is_content_line(line, n) {
                return Err(Violation::BlockScalarPlacement);
            }
            break;
        }
        if header.chomping == Chomping::Keep && empty_follows {
            return Err(Violation::BlockScalarPlacement);
        }
        // The span is exactly the content lines, so the value (indentation
        // removed, folding, chomping) decodes; e.g. a document marker line
        // at indentation 0 is not content.
        if event.value(self.source).is_none() {
            return Err(Violation::ValueMismatch {
                detail: "block scalar value",
            });
        }
        Ok(())
    }

    fn trivia(&mut self, span: Span, text: &str, kind: TriviaKind) -> Step {
        if self.pending_header.is_some() {
            match kind {
                TriviaKind::Whitespace | TriviaKind::Comment => {}
                TriviaKind::LineBreak => {
                    self.breaks_since_header = self.breaks_since_header.saturating_add(1);
                }
                _ => return Err(Violation::DanglingBlockHeader),
            }
        }
        let bad = |expected: &'static str| Err(Violation::BadBytes { expected });
        let in_stream = self.top() == Some(FrameKind::Stream);
        match kind {
            TriviaKind::ByteOrderMark => {
                if text != "\u{feff}" {
                    return bad("U+FEFF");
                }
                if !in_stream {
                    return Err(self.misplaced("BOM"));
                }
                self.prefix_since_doc_end = true;
            }
            TriviaKind::Whitespace => {
                if !text.bytes().all(|b| b == b' ' || b == b'\t') {
                    return bad("spaces and tabs");
                }
            }
            TriviaKind::LineBreak => {
                let lone_cr_before_lf =
                    text == "\r" && self.source.as_bytes().get(span.end) == Some(&b'\n');
                if !matches!(text, "\n" | "\r\n" | "\r") || lone_cr_before_lf {
                    return bad("one line break");
                }
            }
            TriviaKind::Comment => {
                let before = self
                    .source
                    .get(..span.start)
                    .and_then(|s| s.chars().next_back());
                let separated =
                    matches!(before, None | Some(' ' | '\t' | '\n' | '\r' | '\u{feff}'));
                if !text.starts_with('#') || !separated {
                    return bad("a comment separated from preceding text");
                }
            }
            TriviaKind::Indicator(indicator) => {
                if text != indicator.as_str() {
                    return bad("the indicator character");
                }
                let top = self.top();
                let allowed = match indicator {
                    Indicator::SequenceEntry => {
                        top == Some(FrameKind::Sequence(CollectionStyle::Block))
                    }
                    Indicator::MappingKey | Indicator::MappingValue => {
                        matches!(top, Some(FrameKind::Mapping(_)))
                    }
                    Indicator::FlowEntry => matches!(
                        top,
                        Some(
                            FrameKind::Sequence(CollectionStyle::Flow)
                                | FrameKind::Mapping(CollectionStyle::Flow)
                        )
                    ),
                };
                if !allowed {
                    return Err(self.misplaced(indicator.as_str()));
                }
                if let Some(frame) = self.stack.last_mut() {
                    match indicator {
                        Indicator::MappingKey => frame.question = true,
                        Indicator::MappingValue => frame.colon = true,
                        Indicator::SequenceEntry | Indicator::FlowEntry => {}
                    }
                }
            }
            TriviaKind::Anchor | TriviaKind::Tag | TriviaKind::BlockScalarHeader => {
                let (lead, slot, name): (&[char], _, _) = match kind {
                    TriviaKind::Anchor => (&['&'], &mut self.pending_anchor, "&"),
                    TriviaKind::Tag => (&['!'], &mut self.pending_tag, "!"),
                    _ => (&['|', '>'], &mut self.pending_header, "|"),
                };
                if !text.starts_with(lead) {
                    return bad("a property or header with its indicator");
                }
                if slot.is_some() {
                    return Err(Violation::DuplicateProperty);
                }
                *slot = Some(span);
                if kind == TriviaKind::BlockScalarHeader {
                    self.breaks_since_header = 0;
                }
                if in_stream {
                    return Err(Violation::Misplaced {
                        found: name,
                        open: self.open(),
                    });
                }
            }
            TriviaKind::DocumentEndMarker => {
                if text != "..." {
                    return bad("...");
                }
                // After an implicitly ended document, `...` *is* that
                // document's explicit end, never an orphan marker, unless
                // a document prefix (a byte order mark) came between them:
                // then it is `l-document-suffix` after the
                // prefix (production 211), between documents.
                let closes_open_doc = self.docs_seen
                    && !self.last_end_explicit
                    && !self.error_since_doc_end
                    && !self.prefix_since_doc_end;
                if !in_stream || closes_open_doc {
                    return Err(self.misplaced("..."));
                }
                self.last_end_explicit = true;
            }
            TriviaKind::Skipped => {
                if !self.errors_seen {
                    return Err(Violation::SkippedWithoutError);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
