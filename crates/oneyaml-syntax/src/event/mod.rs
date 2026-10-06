//! The public lossless event stream.
//!
//! The parser turns source text into a flat sequence of [`Event`]s. Two
//! consumers read the *same* events: the tree builder, which keeps every
//! byte, and the serde deserializer, which skips trivia and reads only the
//! semantic events. [`check_contract`] validates every rule on this page
//! except those listed under *Not machine-checked* below, and is run by the
//! conformance harness on every stream, valid or not.
//!
//! **Uniqueness goal.** For error-free input, the rules below leave exactly one
//! placement for every event, so one input has exactly one valid stream (up to
//! the parse itself, which the conformance suite checks). Recovery streams are
//! constrained by C6 but not unique: where to resynchronise is the parser's
//! choice.
//!
//! # C1 — Tiling
//!
//! Every event carries a [`Span`] (a UTF-8 byte range into the source text).
//! In stream order the spans **tile** the source: the first starts at `0`, each
//! starts where the previous ended, the last ends at `source.len()`, and every
//! boundary is a `char` boundary. Concatenating the covered slices reproduces
//! the input byte for byte. Trivia is never zero-width.
//!
//! # C2 — Bytes agree with kinds
//!
//! | Event                                   | Bytes                                        |
//! |-----------------------------------------|----------------------------------------------|
//! | `StreamStart`, `StreamEnd`, `Warning`   | always zero-width                            |
//! | `DocumentStart`/`DocumentEnd`           | `---` / `...` when explicit, else zero-width |
//! | `Directive`                             | `%` … end of last parameter, one line        |
//! | `MappingStart`/`MappingEnd` (flow)      | `{` / `}`                                    |
//! | `SequenceStart`/`SequenceEnd` (flow)    | `[` / `]`                                    |
//! | block and [`CollectionStyle::FlowPair`] starts/ends | zero-width                       |
//! | `Scalar`                                | the scalar's text, including quotes; see C5  |
//! | `Alias`                                 | `*name`                                      |
//! | `Error`                                 | offending bytes on one line (may be empty)   |
//! | [`TriviaKind`]                          | as documented on each variant                |
//!
//! Every **line break** in the source is covered either by a
//! [`TriviaKind::LineBreak`] event or lies inside a `Scalar` span (multi-line
//! flow scalars and block scalar content). No other event contains one.
//!
//! Payload meaning agrees with the covered bytes. Names, tags, directive
//! parameters, and scalar values are *read from* those bytes by the
//! accessors ([`Sourced::anchor`], [`Sourced::tag`], [`Sourced::alias_name`],
//! [`Sourced::directive`], [`Sourced::value`]), so they agree by construction;
//! what an event stores (property positions, a block header's chomping,
//! indentation indicator, and span) must match the bytes it points at.
//! A shorthand tag's suffix contains no `!` and no flow indicator (`ns-tag-char`,
//! production 40), so its handle/suffix split is unique; anchor and alias names
//! are `ns-anchor-char+` (production 102): printable, no space, tab, line
//! break, byte order mark, or flow indicator.
//!
//! # C3 — Structure
//!
//! `StreamStart` is first, `StreamEnd` last. Documents nest in the stream;
//! nodes (`Scalar`, `Alias`, `MappingStart`, `SequenceStart`) nest in documents
//! and collections. A document holds **exactly one** node (an empty document
//! holds a zero-width empty plain scalar), a mapping an even number of nodes
//! (key, value, …), a [`CollectionStyle::FlowPair`] mapping exactly two.
//! Directives appear only between documents, only before the first document or
//! after one ended with `...` (productions 210-211), and must be followed by
//! an explicit `---`. An implicit document start is allowed only for the first
//! document or after a document ended with `...`.
//!
//! **Properties.** An anchor or tag is covered by a [`TriviaKind::Anchor`] /
//! [`TriviaKind::Tag`] event, and its meaning is carried **only** by the
//! [`Properties`] of the next node event ([`Event::properties`],
//! [`Sourced::anchor`], [`Sourced::tag`]), whose property spans must equal the
//! trivia spans. Consumers read meaning from the node event, never from
//! trivia. Aliases carry no properties.
//!
//! # C4 — Placement: "hug the content"
//!
//! Zero-width structural events sit as close to content as possible, so that
//! **no node's range begins or ends with trivia**:
//!
//! * **Starts late.** A zero-width start (implicit `DocumentStart`, block
//!   collection start, `FlowPair` start) sits immediately before the first byte
//!   of its first child: the next event is a semantic event or an
//!   anchor/tag/indicator/block-header trivia event, never whitespace, a line
//!   break, or a comment.
//! * **Ends early.** A zero-width end (implicit `DocumentEnd`, block or
//!   `FlowPair` collection end) sits immediately after the last byte of its last
//!   child: the previous event is never trivia.
//! * **Empty nodes** are zero-width plain scalars. Exactly one position is
//!   valid, the first of these that applies:
//!   1. immediately after the node's **last property** (`a: !!null`, `- &x`);
//!   2. immediately after its **introducing indicator**: `---` for a document,
//!      `-` for a block sequence entry, `?` for an explicit key, `:` for a
//!      value;
//!   3. for a **value with no `:`** (`? a`, `{a, b: c}`, `[ ? a ]`),
//!      immediately after its key node's last event;
//!   4. for an **empty key without `?`** (`: v`, `{ : v}`), immediately before
//!      its `:`.
//!
//!   No other empty node exists in error-free input (an implicit document or
//!   a flow sequence entry is never empty without properties). A repair node
//!   (C6 R2) sits immediately after its `Error` instead.
//!
//! *Why:* every run of comments, blank lines, and indentation then lies
//! *between* nodes at the outermost level it can, preceded by the complete set
//! of collections it closes and followed by the complete set it opens. Each
//! comment-attachment rule (document header vs. first-node leading comment,
//! same-line trailing comment, collection footer vs. next sibling's leading
//! comment, document footer) becomes a decision over one flat gap, using the
//! columns of the closing and opening nodes, instead of hunting trivia at
//! several nesting depths. Header rule 1 (comments before `---`) and rule 2/3
//! (blank line before the first node) are directly visible because the
//! implicit `DocumentStart` comes *after* leading comments.
//!
//! Warnings are ignored when judging placement.
//!
//! # C5 — Block scalars
//!
//! The header (`|`, `>-`, `|2+`, …) is a [`TriviaKind::BlockScalarHeader`]
//! event; its meaning is carried by the scalar event as a [`BlockHeader`]
//! ([`Event::block_header`]) whose span equals the trivia span. After the header come optional
//! whitespace and comment, then the header line's `LineBreak`; the scalar span
//! starts right after that break and covers the content lines, including
//! their indentation and line breaks:
//!
//! * [`Chomping::Keep`]: through **all** trailing empty lines (no empty line
//!   may directly follow the span);
//! * [`Chomping::Strip`] / [`Chomping::Clip`]: through the line break of the
//!   last non-empty line; trailing empty lines are trivia after the scalar.
//!
//! With content indentation `n`, an *empty line* is at most `n` spaces followed
//! by a break (`l-empty`, production 70); a *content line* has at least `n`
//! spaces followed by more text (at `n = 0`, a `---`/`...` marker is not
//! content). The span never stops before a content line that follows it
//! (after any empty lines), and never contains a less-indented non-empty
//! line. All three break styles (`\n`, `\r\n`, `\r`) separate lines.
//!
//! **End of input.** The span ends at a line break, or at end of input on a
//! content line. A final unterminated line of at most `n` spaces (e.g. the
//! trailing space in `|+\n a\n `) is *not* part of the scalar: it is
//! `Whitespace` trivia after it, whatever the chomping. A final unterminated
//! line of more than `n` spaces is content.
//!
//! Trailing comment lines are always trivia after the scalar. A block scalar
//! with no content is zero-width at the start of the line after the header
//! (or at end of input).
//!
//! # C6 — Diagnostics and recovery
//!
//! [`EventKind::Warning`] is zero-width, carries a [`Severity::Warning`]
//! diagnostic, and follows the event it concerns. [`EventKind::Error`] carries
//! a [`Severity::Error`] diagnostic. Recovery obeys:
//!
//! * **R1 — Same structure.** Error streams satisfy every C1–C5 rule; consumers
//!   need no special cases for well-nesting.
//! * **R2 — Repair by synthesis.** The parser completes the structure with
//!   zero-width events: a missing node becomes an empty plain scalar (carrying
//!   any pending properties) placed immediately after its `Error`; an
//!   unclosed collection gets a zero-width end (for flow collections, allowed
//!   only after an error inside it); a document gets an implicit end. An
//!   `Error` after a block scalar header **abandons** the header (e.g. a bad
//!   content line, suite 5LLU): `Skipped` trivia may follow, the repair is an
//!   empty plain scalar, and no block scalar may claim that header.
//! * **R3 — Errors are not nodes.** An `Error` event never fills a node slot;
//!   it annotates the position where the problem was detected, before the
//!   synthesized events that repair it. The tree builder turns it plus the
//!   skipped bytes into a read-only error region.
//! * **R4 — Line-granular skipping.** Bytes skipped during resynchronisation
//!   are [`TriviaKind::Skipped`] events (only after an error), one per line,
//!   with the line breaks as `LineBreak` trivia. `Error` and `Skipped` spans
//!   never contain a line break.
//! * **R5 — Honest prefix.** The semantic events before the first `Error` are
//!   what the parser was sure of; a strict consumer stops at the first `Error`.
//!   Placement rule "ends early" does not apply to collections containing an error.
//!
//! # Not machine-checked
//!
//! [`check_contract`] validates structural consistency, not parse
//! correctness. It does **not** check:
//!
//! * that an `Error` sits where the problem actually is (R3), or that a
//!   `Warning` follows the event it concerns (C6); only their widths,
//!   severities, and the placement of repairs are checked;
//! * multi-line scalar values (folding, escapes) and double-quoted values;
//!   tag resolution (`Tag::resolved` vs `%TAG`); block scalar
//!   `content_indent` detection and the indentation of the parent node;
//! * lexical classification that needs the grammar, e.g. that a plain
//!   scalar contains no `: ` or ` #`, or that a key is a legal implicit key.
//!
//! The conformance harness's tree comparison covers these.
//!
//! # Representation
//!
//! An [`Event`] is 48 bytes on 64-bit targets and owns no heap memory
//! except a diagnostic. Everything that can be read from the source is
//! read from it on demand, so events are cheap to produce, move, and
//! store:
//!
//! * **Values are lazy.** A scalar event stores its style (and, for a block
//!   scalar, its header), not its value; [`Sourced::value`] decodes the
//!   covered bytes (folding, escapes, chomping) when asked, borrowing from
//!   the source whenever the value is a slice of it. The parser has
//!   already validated the bytes, so decoding a stream's own scalar
//!   against its own source always succeeds.
//! * **Names are lazy.** Anchor, tag, alias, and directive text is sliced
//!   from the source by the accessors; a tag is resolved (`%TAG` prefix,
//!   `%XX` escapes) by [`Sourced::tag`].
//! * **Positions are relative.** A node event stores its properties' and
//!   block header's positions as 32-bit offsets back from its own start;
//!   an input where such an offset exceeds `u32::MAX` (only possible past
//!   4 GiB) falls back to a boxed absolute form, with the same meaning.
//!   The span is therefore private: [`Event::span`] reads it.
//!
//! # Reading names and values
//!
//! Reading needs the source text, and pairing an event with the wrong text
//! would quietly read the wrong bytes. So `Event` itself has no public
//! reader: every read goes through [`Sourced`], an event together with the
//! source it was parsed from, and only the parser makes one. The pull
//! parser ([`crate::parser::Events`]) yields `Sourced` events; the
//! collected stream ([`EventList`]) keeps its source and hands out
//! `Sourced` views of its events. A mismatched pair cannot be built
//! (the tree builder and the deserializer read the same bytes).
//! Values still borrow from the source.
//!
//! # Recursion
//!
//! The stream is flat: nesting is expressed by start/end pairs, never by nested
//! values, so derived `Debug`/`PartialEq`/`Clone`/`Hash` cannot recurse on input.
//! Streams are produced by [`crate::parser::events`] (pulled one
//! event at a time) or collected by [`crate::parser::parse`].

mod contract;
mod node;
mod sourced;

use std::borrow::Cow;
use std::marker::PhantomData;

use oneyaml_core::{Diagnostic, Span};

#[cfg(doc)]
use oneyaml_core::Severity;

pub use contract::{ContractError, Violation, check_contract, check_tiling};
pub use node::NodeInfo;
pub use sourced::{EventList, IntoSourced, Sourced, SourcedIter};

/// One event of the lossless event stream.
///
/// `'a` is the lifetime of the source text the event was parsed from. The
/// event itself holds no reference (see *Representation* in the module
/// docs); names and values are read through [`Sourced`], which pairs it
/// with that text ([`Sourced::value`], [`Sourced::tag`], …).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Event<'a> {
    kind: EventKind,
    span: Span,
    source: PhantomData<&'a str>,
}

impl<'a> Event<'a> {
    /// Creates an event.
    ///
    /// A node event's [`NodeInfo`] stores positions relative to the start
    /// of the span it was built for; build node events with
    /// [`Event::scalar`], [`Event::mapping_start`], and
    /// [`Event::sequence_start`], or with a `NodeInfo` made for this span.
    #[must_use]
    pub const fn new(kind: EventKind, span: Span) -> Self {
        Self {
            kind,
            span,
            source: PhantomData,
        }
    }

    /// A scalar event covering `span`, with properties at absolute
    /// positions and, for `Literal`/`Folded`, its header.
    #[must_use]
    pub fn scalar(
        span: Span,
        style: ScalarStyle,
        props: Properties,
        block: Option<BlockHeader>,
    ) -> Self {
        let info = NodeInfo::new(span.start, props, block);
        Self::new(EventKind::Scalar { style, info }, span)
    }

    /// A mapping start event covering `span`, with properties at absolute
    /// positions.
    #[must_use]
    pub fn mapping_start(span: Span, style: CollectionStyle, props: Properties) -> Self {
        let info = NodeInfo::new(span.start, props, None);
        Self::new(EventKind::MappingStart { style, info }, span)
    }

    /// A sequence start event covering `span`, with properties at absolute
    /// positions.
    #[must_use]
    pub fn sequence_start(span: Span, style: CollectionStyle, props: Properties) -> Self {
        let info = NodeInfo::new(span.start, props, None);
        Self::new(EventKind::SequenceStart { style, info }, span)
    }

    /// What happened.
    #[must_use]
    pub const fn kind(&self) -> &EventKind {
        &self.kind
    }

    /// The event's kind, consuming it (e.g. to take a diagnostic).
    #[must_use]
    pub fn into_kind(self) -> EventKind {
        self.kind
    }

    /// The source bytes this event covers (see C1).
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The source text this event covers, or `None` if the span is out of
    /// bounds or not on `char` boundaries for `source`. Crate-private: the
    /// public reads go through [`Sourced`], which pairs an event with its
    /// own source (see *Reading* in the module docs).
    #[must_use]
    pub(crate) fn source<'s>(&self, source: &'s str) -> Option<&'s str> {
        source.get(self.span.start..self.span.end)
    }

    /// `true` for [`EventKind::Trivia`]: presentation-only bytes that a
    /// meaning-only consumer (serde) skips.
    #[must_use]
    pub const fn is_trivia(&self) -> bool {
        matches!(self.kind, EventKind::Trivia(_))
    }

    /// `true` for [`EventKind::Error`].
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self.kind, EventKind::Error(_))
    }

    /// `true` for [`EventKind::Warning`].
    #[must_use]
    pub const fn is_warning(&self) -> bool {
        matches!(self.kind, EventKind::Warning(_))
    }

    /// The diagnostic of an `Error` or `Warning` event.
    #[must_use]
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match &self.kind {
            EventKind::Error(d) | EventKind::Warning(d) => Some(d),
            _ => None,
        }
    }

    /// Replaces a node event's information (for the parser, which merges
    /// properties into a collection start it already built).
    pub(crate) fn set_info(&mut self, new: NodeInfo) {
        if let EventKind::Scalar { info, .. }
        | EventKind::MappingStart { info, .. }
        | EventKind::SequenceStart { info, .. } = &mut self.kind
        {
            *info = new;
        }
    }

    /// The node information of a `Scalar`, `MappingStart`, or
    /// `SequenceStart` event.
    #[must_use]
    pub const fn node_info(&self) -> Option<&NodeInfo> {
        match &self.kind {
            EventKind::Scalar { info, .. }
            | EventKind::MappingStart { info, .. }
            | EventKind::SequenceStart { info, .. } => Some(info),
            _ => None,
        }
    }

    /// Where the node's properties are (C3); `None` for events that are not
    /// `Scalar`, `MappingStart`, or `SequenceStart`.
    #[must_use]
    pub fn properties(&self) -> Option<Properties> {
        self.node_info().map(|i| i.properties(self.span.start))
    }

    /// The node's anchor, read from `source`.
    #[must_use]
    pub(crate) fn anchor(&self, source: &'a str) -> Option<Anchor<'a>> {
        crate::parser::anchor_at(source, self.properties()?.anchor?)
    }

    /// The node's tag, read and resolved from `source` (`%TAG` prefix and
    /// `%XX` escapes, §6.9.1).
    #[must_use]
    pub(crate) fn tag(&self, source: &'a str) -> Option<Tag<'a>> {
        self.tag_source(source)?.resolve()
    }

    /// The node's tag as written, not yet resolved.
    #[must_use]
    pub(crate) fn tag_source(&self, source: &'a str) -> Option<TagSource<'a>> {
        let props = self.properties()?;
        crate::parser::tag_source_at(source, props.tag?, props.tag_prefix)
    }

    /// A block scalar's header (C5).
    #[must_use]
    pub fn block_header(&self) -> Option<BlockHeader> {
        match &self.kind {
            EventKind::Scalar { info, .. } => info.block_header(self.span.start),
            _ => None,
        }
    }

    /// A scalar's value after escape processing, line folding, and
    /// chomping, decoded from `source`. Borrowed from the source when the
    /// value is a slice of it.
    #[must_use]
    pub(crate) fn value(&self, source: &'a str) -> Option<Cow<'a, str>> {
        let EventKind::Scalar { style, .. } = self.kind else {
            return None;
        };
        crate::parser::scalar_value(source, self.span, style, self.block_header())
    }

    /// An alias's anchor name, without `*`.
    #[must_use]
    pub(crate) fn alias_name(&self, source: &'a str) -> Option<&'a str> {
        match self.kind {
            EventKind::Alias => self.source(source)?.strip_prefix('*'),
            _ => None,
        }
    }

    /// A directive's meaning, read from `source`.
    #[must_use]
    pub(crate) fn directive(&self, source: &'a str) -> Option<DirectiveKind<'a>> {
        match self.kind {
            EventKind::Directive(_) => crate::parser::directive_at(source, self.span),
            _ => None,
        }
    }
}

/// The kind of an [`Event`], with its payload.
///
/// Semantic events correspond one-to-one to the yaml-test-suite event tree
/// (`+STR`, `+DOC`, `+MAP`, `+SEQ`, `=VAL`, `=ALI` and their ends). Values
/// and names are not stored: [`Event`]'s accessors read them from the
/// source. Struct-like variants are `#[non_exhaustive]`; build them with
/// the constructor functions on this type and on [`Event`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// Start of the stream; first event, zero-width at `0`.
    StreamStart,
    /// End of the stream; last event, zero-width at `source.len()`.
    StreamEnd,
    /// A directive (§6.8), from `%` to the end of its last parameter
    /// ([`Sourced::directive`] reads its parameters).
    Directive(Directive),
    /// Start of a document (§9.1). Explicit: covers `---`; else zero-width.
    #[non_exhaustive]
    DocumentStart {
        /// Whether the document began with `---`.
        explicit: bool,
    },
    /// End of a document. Explicit: covers `...`; else zero-width.
    #[non_exhaustive]
    DocumentEnd {
        /// Whether the document ended with `...`.
        explicit: bool,
    },
    /// Start of a mapping. Flow: covers `{`; block and flow-pair: zero-width.
    #[non_exhaustive]
    MappingStart {
        /// Layout.
        style: CollectionStyle,
        /// Where the node's properties are ([`Event::properties`]).
        info: NodeInfo,
    },
    /// End of a mapping. Flow: covers `}`; block and flow-pair: zero-width.
    MappingEnd,
    /// Start of a sequence. Flow: covers `[`; block: zero-width.
    #[non_exhaustive]
    SequenceStart {
        /// Layout ([`CollectionStyle::FlowPair`] is not valid here).
        style: CollectionStyle,
        /// Where the node's properties are ([`Event::properties`]).
        info: NodeInfo,
    },
    /// End of a sequence. Flow: covers `]`; block: zero-width.
    SequenceEnd,
    /// A scalar node (see C5 for block scalars; [`Sourced::value`] decodes
    /// its value).
    #[non_exhaustive]
    Scalar {
        /// Presentation style.
        style: ScalarStyle,
        /// Properties and, for `Literal`/`Folded`, the header.
        info: NodeInfo,
    },
    /// An alias node, covering `*name` ([`Sourced::alias_name`]).
    Alias,
    /// Presentation-only bytes; never zero-width.
    Trivia(TriviaKind),
    /// A warning (zero-width). The input is still valid.
    Warning(Box<Diagnostic>),
    /// A syntax error (see C6). Boxed so error-free events stay small.
    Error(Box<Diagnostic>),
}

impl EventKind {
    /// [`EventKind::DocumentStart`].
    #[must_use]
    pub const fn document_start(explicit: bool) -> Self {
        Self::DocumentStart { explicit }
    }

    /// [`EventKind::DocumentEnd`].
    #[must_use]
    pub const fn document_end(explicit: bool) -> Self {
        Self::DocumentEnd { explicit }
    }

    /// [`EventKind::MappingStart`] without properties.
    #[must_use]
    pub const fn mapping_start(style: CollectionStyle) -> Self {
        Self::MappingStart {
            style,
            info: NodeInfo::none(),
        }
    }

    /// [`EventKind::SequenceStart`] without properties.
    #[must_use]
    pub const fn sequence_start(style: CollectionStyle) -> Self {
        Self::SequenceStart {
            style,
            info: NodeInfo::none(),
        }
    }

    /// [`EventKind::Scalar`] without properties or header (a flow scalar).
    #[must_use]
    pub const fn scalar(style: ScalarStyle) -> Self {
        Self::Scalar {
            style,
            info: NodeInfo::none(),
        }
    }

    /// A plain [`EventKind::Scalar`] without properties.
    #[must_use]
    pub const fn plain() -> Self {
        Self::scalar(ScalarStyle::Plain)
    }
}

/// Layout of a collection.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CollectionStyle {
    /// Indentation-based layout (§8.2).
    Block,
    /// `{ }` / `[ ]` layout (§7.4).
    Flow,
    /// Mappings only: an implicit single-pair mapping written as an entry of a
    /// flow sequence, e.g. `[ a: b ]` or `[ ? a : b ]` (§7.4.1, productions
    /// 150 `ns-flow-pair` and 151 `ns-flow-pair-entry`). It has no brackets,
    /// so its start and end are zero-width; its parent is always a flow
    /// sequence; it holds exactly one key and one value.
    FlowPair,
}

/// Presentation style of a scalar.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScalarStyle {
    /// Unquoted.
    Plain,
    /// `'…'`.
    SingleQuoted,
    /// `"…"`.
    DoubleQuoted,
    /// `|` block scalar.
    Literal,
    /// `>` block scalar.
    Folded,
}

/// Block scalar chomping (§8.1.1.2).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chomping {
    /// `-`: final line break and trailing empty lines removed.
    Strip,
    /// No indicator: final line break kept, trailing empty lines removed.
    Clip,
    /// `+`: final line break and trailing empty lines kept.
    Keep,
}

/// The meaning of a block scalar header, so no consumer re-parses it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockHeader {
    /// Chomping indicator.
    pub chomping: Chomping,
    /// Explicit indentation indicator `1`–`9`, if written.
    pub indent_indicator: Option<u8>,
    /// Content indentation in spaces (explicit or auto-detected, §8.1.1.1).
    pub content_indent: usize,
    /// Span of the header text (equal to the header trivia event's span).
    pub span: Span,
}

impl BlockHeader {
    /// Creates a header payload.
    #[must_use]
    pub const fn new(
        chomping: Chomping,
        indent_indicator: Option<u8>,
        content_indent: usize,
        span: Span,
    ) -> Self {
        Self {
            chomping,
            indent_indicator,
            content_indent,
            span,
        }
    }
}

/// Where a node's properties (§6.9) are, as byte offsets into the source.
/// With the source, [`Sourced::anchor`] and [`Sourced::tag`] read their
/// meaning; this is the **only** source of it, the property trivia events
/// carry bytes only (C3).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Properties {
    /// Start of `&name`, if the node has an anchor.
    pub anchor: Option<usize>,
    /// Start of the tag property, if the node has a tag.
    pub tag: Option<usize>,
    /// For a shorthand tag whose handle a `%TAG` directive declares: the
    /// start of that directive's prefix. `None` for the default handles
    /// `!` and `!!`, verbatim tags, and the non-specific tag `!`.
    pub tag_prefix: Option<usize>,
}

impl Properties {
    /// Creates properties.
    #[must_use]
    pub const fn new(anchor: Option<usize>, tag: Option<usize>, tag_prefix: Option<usize>) -> Self {
        Self {
            anchor,
            tag,
            tag_prefix,
        }
    }

    /// No anchor and no tag.
    #[must_use]
    pub const fn none() -> Self {
        Self::new(None, None, None)
    }

    /// `true` when there is neither an anchor nor a tag.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.anchor.is_none() && self.tag.is_none()
    }
}

/// An anchor property, read from the source by [`Sourced::anchor`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor<'a> {
    /// The anchor name, without `&`.
    pub name: &'a str,
    /// Span of `&name`.
    pub span: Span,
}

impl<'a> Anchor<'a> {
    /// Creates an anchor.
    #[must_use]
    pub const fn new(name: &'a str, span: Span) -> Self {
        Self { name, span }
    }
}

/// How a tag was written (§6.9.1).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TagForm {
    /// `!<uri>`.
    Verbatim,
    /// `!suffix`, `!!suffix`, or `!handle!suffix`.
    Shorthand,
    /// The lone non-specific tag `!`.
    NonSpecific,
}

/// A tag property, read and resolved from the source by [`Sourced::tag`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tag<'a> {
    /// How the tag was written.
    pub form: TagForm,
    /// The handle as written (`!`, `!!`, `!e!`); empty for verbatim tags.
    pub handle: &'a str,
    /// The suffix as written; for verbatim tags, the text between `<` and `>`.
    pub suffix: &'a str,
    /// The full tag after `%TAG` expansion and URI unescaping (e.g.
    /// `tag:yaml.org,2002:str`), or `!` for the non-specific tag.
    pub resolved: Cow<'a, str>,
    /// Span of the whole property.
    pub span: Span,
}

impl<'a> Tag<'a> {
    /// Creates a tag.
    #[must_use]
    pub const fn new(
        form: TagForm,
        handle: &'a str,
        suffix: &'a str,
        resolved: Cow<'a, str>,
        span: Span,
    ) -> Self {
        Self {
            form,
            handle,
            suffix,
            resolved,
            span,
        }
    }
}

/// A tag property as written, before it is resolved ([`Sourced::tag_source`],
/// [`crate::tree::Node::tag_source`]).
///
/// Resolving builds the full tag text, whose `%TAG`
/// prefix may be long. Two tags of one source with equal [`TagSource::key`]s
/// resolve to the same text, so a consumer can resolve each distinct tag
/// once ([`TagSource::resolve`]) and share the result; reading a
/// `TagSource` costs only the property's own text.
#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct TagSource<'a> {
    /// How the tag was written.
    pub form: TagForm,
    /// The handle as written (`!`, `!!`, `!e!`); empty for verbatim tags.
    pub handle: &'a str,
    /// The suffix as written; for verbatim tags, the text between `<` and `>`.
    pub suffix: &'a str,
    /// Span of the whole property (in the text it was read from).
    pub span: Span,
    /// The source text holding the property, and its span there.
    src: &'a str,
    at: Span,
    /// Where the declaring `%TAG` prefix starts in `src`.
    prefix: Option<usize>,
}

/// Identifies what a tag resolves to within one source: its form, handle,
/// suffix, and the `%TAG` directive (by position) its handle names. Equal
/// keys resolve to equal text ([`TagSource::key`]).
///
/// The directive is identified by the **address** of the source text
/// holding it and its offset there, so a key is meaningful only while
/// that text is alive and unchanged: compare keys of tags read from the
/// same, living source (one event stream's text, or one syntax tree
/// between edits). After the text is dropped (or a tree is edited), its
/// address may be reused and a stale key could equal a new one. The
/// borrow `'a` keeps the handle and suffix alive, not the directive's
/// text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TagKey<'a> {
    form: TagForm,
    handle: &'a str,
    suffix: &'a str,
    /// The address of the source text and the prefix's offset in it.
    prefix: Option<(usize, usize)>,
}

impl<'a> TagSource<'a> {
    pub(crate) const fn new(
        form: TagForm,
        handle: &'a str,
        suffix: &'a str,
        span: Span,
        src: &'a str,
        prefix: Option<usize>,
    ) -> Self {
        Self {
            form,
            handle,
            suffix,
            span,
            src,
            at: span,
            prefix,
        }
    }

    /// What this tag resolves to, as a hashable key: O(1) to build, and
    /// O(handle + suffix) to hash, whatever the `%TAG` prefix's length.
    #[must_use]
    pub fn key(&self) -> TagKey<'a> {
        TagKey {
            form: self.form,
            handle: self.handle,
            suffix: self.suffix,
            prefix: self.prefix.map(|p| (self.src.as_ptr().addr(), p)),
        }
    }

    /// The tag, resolved (`%TAG` prefix and `%XX` escapes, §6.9.1): what
    /// [`Sourced::tag`] gives. O(prefix + suffix).
    #[must_use]
    pub fn resolve(&self) -> Option<Tag<'a>> {
        crate::parser::resolve_tag(self)
    }

    /// When the handle names a `%TAG` directive: the key of that
    /// directive's prefix, equal for every tag using it (to decode
    /// [`TagSource::directive_prefix`] once and share it).
    #[must_use]
    pub fn prefix_key(&self) -> Option<TagKey<'a>> {
        self.prefix.map(|p| TagKey {
            form: TagForm::Shorthand,
            handle: "",
            suffix: "",
            prefix: Some((self.src.as_ptr().addr(), p)),
        })
    }

    /// When the handle names a `%TAG` directive: its prefix with the
    /// `%XX` escapes decoded, as bytes (a character may continue in the
    /// suffix). O(prefix).
    #[must_use]
    pub fn directive_prefix(&self) -> Option<Cow<'a, [u8]>> {
        crate::parser::directive_prefix(self)
    }

    /// The prefix of a shorthand tag whose handle names no `%TAG`
    /// directive: `!` for the primary handle, `tag:yaml.org,2002:` for the
    /// secondary one (§6.8.2.2). `None` otherwise (a verbatim or
    /// non-specific tag, or one with a [`TagSource::directive_prefix`]).
    #[must_use]
    pub fn default_prefix(&self) -> Option<&'static str> {
        if self.form != TagForm::Shorthand || self.prefix.is_some() {
            return None;
        }
        match self.handle {
            "!" => Some("!"),
            "!!" => Some("tag:yaml.org,2002:"),
            _ => None,
        }
    }

    /// The suffix (for a verbatim tag, its whole text) with the `%XX`
    /// escapes decoded, as bytes (a character may begin in the prefix).
    /// O(suffix). The resolved tag is the prefix
    /// ([`TagSource::directive_prefix`] or [`TagSource::default_prefix`])
    /// followed by these bytes; together they are UTF-8.
    #[must_use]
    pub fn decoded_suffix(&self) -> Cow<'a, [u8]> {
        crate::parser::decoded_suffix(self)
    }

    pub(crate) const fn src(&self) -> &'a str {
        self.src
    }

    /// The property's span in [`TagSource::src`].
    pub(crate) const fn at(&self) -> Span {
        self.at
    }

    pub(crate) const fn prefix(&self) -> Option<usize> {
        self.prefix
    }
}

/// Which directive an [`EventKind::Directive`] is (§6.8); its parameters
/// are read by [`Sourced::directive`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Directive {
    /// `%YAML`.
    Yaml,
    /// `%TAG`.
    Tag,
    /// Any other (reserved) directive.
    Reserved,
}

/// The directives of §6.8, read from the source by [`Sourced::directive`].
/// All text fields borrow from the directive's span.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DirectiveKind<'a> {
    /// `%YAML <version>`; the version as written. The version policy (warnings
    /// for 1.1 and 1.3+, error for 2.x) is applied by the parser as
    /// [`EventKind::Warning`]/[`EventKind::Error`] events, not encoded here.
    #[non_exhaustive]
    Yaml {
        /// E.g. `1.2`.
        version: &'a str,
    },
    /// `%TAG <handle> <prefix>`.
    #[non_exhaustive]
    Tag {
        /// E.g. `!e!`.
        handle: &'a str,
        /// E.g. `tag:example.com,2000:`.
        prefix: &'a str,
    },
    /// Any other `%NAME …` directive; ignored with a warning.
    #[non_exhaustive]
    Reserved {
        /// The name, without `%`.
        name: &'a str,
        /// From the first byte of the first parameter to the last byte of the
        /// last parameter, **including** the whitespace between parameters and
        /// **excluding** the whitespace between the name and the first
        /// parameter. Empty when there are no parameters. E.g. `%FOO  bar baz`
        /// has `params == "bar baz"`.
        params: &'a str,
    },
}

impl<'a> DirectiveKind<'a> {
    /// [`DirectiveKind::Yaml`].
    #[must_use]
    pub const fn yaml(version: &'a str) -> Self {
        Self::Yaml { version }
    }

    /// [`DirectiveKind::Tag`].
    #[must_use]
    pub const fn tag(handle: &'a str, prefix: &'a str) -> Self {
        Self::Tag { handle, prefix }
    }

    /// [`DirectiveKind::Reserved`].
    #[must_use]
    pub const fn reserved(name: &'a str, params: &'a str) -> Self {
        Self::Reserved { name, params }
    }

    /// Which directive this is.
    #[must_use]
    pub const fn directive(&self) -> Directive {
        match self {
            Self::Yaml { .. } => Directive::Yaml,
            Self::Tag { .. } => Directive::Tag,
            Self::Reserved { .. } => Directive::Reserved,
        }
    }
}

/// Kinds of presentation-only bytes.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TriviaKind {
    /// U+FEFF, between documents only.
    ByteOrderMark,
    /// A run of spaces and/or tabs.
    Whitespace,
    /// Exactly one line break: `\n`, `\r\n`, or `\r` (a `\r` is never split
    /// from a following `\n`).
    LineBreak,
    /// `#` to the end of the line (excluding the break); preceded by
    /// whitespace, a line break, a byte order mark, or the start of input.
    Comment,
    /// A structural indicator.
    Indicator(Indicator),
    /// The bytes `&name` of an anchor property (meaning: [`Properties`]).
    Anchor,
    /// The bytes of a tag property (meaning: [`Properties`]).
    Tag,
    /// A block scalar header, e.g. `|`, `>-`, `|2+` (meaning: [`BlockHeader`]).
    BlockScalarHeader,
    /// A `...` marker that closes no document (e.g. at the start of a stream).
    DocumentEndMarker,
    /// Bytes skipped during error recovery: one line at most, only after an
    /// `Error` event (C6, R4).
    Skipped,
}

/// Structural indicators carried as trivia.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Indicator {
    /// `-` block sequence entry; only directly inside a block sequence.
    SequenceEntry,
    /// `?` mapping key; only directly inside a mapping.
    MappingKey,
    /// `:` mapping value; only directly inside a mapping.
    MappingValue,
    /// `,` entry separator; only directly inside a flow sequence or flow
    /// mapping (a `FlowPair` ends before its `,`).
    FlowEntry,
}

impl Indicator {
    /// The indicator's character.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SequenceEntry => "-",
            Self::MappingKey => "?",
            Self::MappingValue => ":",
            Self::FlowEntry => ",",
        }
    }
}

#[cfg(test)]
mod tests;
