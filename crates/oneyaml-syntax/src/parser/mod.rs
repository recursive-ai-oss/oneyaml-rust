//! The YAML 1.2.2 parser: source text to the lossless event stream.
//!
//! # Architecture
//!
//! * `scan` — the lexical layer: character classes (§5) and single-line
//!   scanners for indicators, properties, and plain scalar lines.
//! * `scalar` — multi-line flow scalars (plain, single- and double-quoted)
//!   with folding and escapes.
//! * `stream` — stream and document level: BOMs, directives, document
//!   markers (§9).
//! * `block`, `head` — block collections and node heads (§8.2, §6.9).
//! * `block_scalar` — literal and folded block scalars (§8.1).
//! * `flow`, `flow_node`, `flow_block` — flow collections (§7.4), parsed
//!   by a sub-machine with its own explicit stack into a local buffer so a
//!   single-line flow collection can still become a block implicit key.
//!
//! The parser is an **explicit-stack state machine**: `State` says
//! what the machine expects next and `Frame`s record the open documents and
//! block collections. Nothing recurses, so arbitrarily deep input cannot
//! overflow the call stack.
//!
//! **Placement (event contract C4).** Trivia (white space, comments, line
//! breaks) is scanned into a *pending* buffer. Zero-width closing events
//! (collection ends, implicit document ends, empty nodes) are pushed before
//! the buffer is flushed, so they "end early"; zero-width openings are pushed
//! after the flush, immediately before their first child, so they "start
//! late".
//!
//! **Errors (C6).** A syntax error emits an [`EventKind::Error`] covering the
//! rest of the offending line, completes the open structure with zero-width
//! events (R2), and then skips whole lines as [`TriviaKind::Skipped`] until
//! the next document marker, where parsing resumes (R4).
//!
//! # Streaming and buffering
//!
//! [`events_with`] is the parser; [`parse_with`] only collects it.
//! What the parser holds between two pulls, for a consumer that drops
//! each event (the bound is derived here; `tests/streaming.rs` checks it
//! with `Events::retained_capacity`, hidden test instrumentation):
//!
//! * the flow hold (`flow.rs`): one line and at most the implicit-key
//!   window of productions 154-155, 1024 characters or `W` = 4096 bytes,
//!   whatever the collection's size. A byte yields at most 3 held items:
//!   an entry of `k` bytes plus its `,` yields at most `k + 1` items that
//!   cover bytes and 4 zero-width ones (a flow pair's start and end, an
//!   empty key and value), and `(k + 5) / (k + 1) <= 3`, reached by
//!   `[:,:,…]` and `[?,?,…]`. So the hold holds at most `3W` = 12,288
//!   items, and its open pair reservations at most one per entry, `W / 2`;
//! * the output queue: a batch of 64 items plus the zero-width ends one
//!   token confirms (bounded by depth), **or**, once, the whole flow hold
//!   when the root is decided (or an error ends it) and the hold drains
//!   into it: at most `3W` items plus the batch and the depth-bounded
//!   ends. Both buffers keep their capacity, so for a single-line flow
//!   collection longer than the window both reach 12,288 items, which
//!   `std`'s power-of-two `VecDeque` rounds to a capacity of 16,384 each;
//! * frame stacks, `deferred` ends and empty nodes, `flow_repair`, and the
//!   head buffer: bounded by depth;
//! * trivia runs cost one queue item each and are lexed as they are
//!   pulled, including the white lines before a block scalar error;
//!   scalar values are decoded only by `Sourced::value`;
//! * **not** bounded by depth or the key window: the `%TAG` handles of the
//!   current and the next document (one entry per directive).
//!
//! So, `%TAG` handles aside, the parser holds `O(depth)` plus at most
//! `2 × 3W + W / 2` items, independent of the input's length; with
//! amortised growth (capacity below twice the peak length) that is below
//! `4 × 3W + W` = 53,248 slots plus `O(depth)`. Measured peak: ≈32,784.
//!
//! # Prefix timing conventions (not contract requirements)
//!
//! The event contract fixes *where* each event goes (C1-C6), not *when*,
//! relative to a later error, the parser commits to it. yaml-test-suite
//! records, for every invalid input, exactly the events its reference
//! parser emitted before failing, and our harness compares that prefix
//! byte for byte. The rules below reproduce that timing. They are
//! conventions for matching the suite, justified by R5 ("the events before
//! the first `Error` are what the parser was sure of"); they are **not**
//! part of the contract, `check_contract` does not test them, and another
//! choice would be equally valid structurally. Code comments cite them as
//! T1-T5.
//!
//! * **T1: deferred implicit document start.** An implicit document's
//!   `DocumentStart` is emitted when its root node's content token is
//!   recognised, not when its properties are, so an error in the first
//!   head (e.g. `&a - b`) yields no document (`doc_start_deferred`).
//! * **T2: deferred ends and empty nodes.** A block collection end caused
//!   by a strict dedent, and an empty node with properties, are emitted at
//!   once. A sequence end caused by a same-column non-`-` line, and an
//!   empty node without properties, are deferred until the next token is
//!   accepted; if it is not, they follow the `Error` as its repair
//!   (`deferred`).
//! * **T3: key errors.** A node that turns out to be a multi-line implicit
//!   key never gets its completing event: a multi-line scalar key is
//!   rejected before it is emitted, and a multi-line flow collection key
//!   is rejected at its closing bracket (its contents are emitted, its end
//!   is not). A complete single-line quoted scalar or flow collection
//!   followed by a `:` that cannot start a mapping is emitted first, and
//!   the `:` is reported after it. A multi-line flow collection in a block
//!   key slot that no `:` follows (`'k' : ---\n[\n  'x' : value]`) is
//!   emitted whole; the missing `:` (`Y0018`) is reported where the scan
//!   stopped, which may be lines later or the end of input, so its
//!   diagnostic carries a secondary label covering the key.
//! * **T4: the document stays open at content after the root.** A document
//!   ends only at its end marker, at the next document start, or at the end
//!   of the stream, so content after a complete root (whatever its style) is
//!   reported before the implicit `DocumentEnd`, which comes with the repair.
//! * **T5: flow prefixes.** A flow collection with an error commits every
//!   event before the error, then the repairs.
//!
//! # Settings and limits
//!
//! [`parse_with`] takes the [`Settings`]. The parser enforces four of the
//! [`Limits`](oneyaml_core::Limits); each is checked **before** the
//! offending event is emitted, so the stream stays contract-valid and the
//! error is repaired like any other (C6):
//!
//! * `max_depth` (`Y2000`): block and flow collections counted together.
//!   Checked when a collection opens, where a block collection that may
//!   not start there would be reported; for a flow collection that turns
//!   out to be an implicit key (whose depth grows by the mapping or flow
//!   pair it starts), when the key's `:` is seen.
//! * `max_scalar_len` (`Y2001`): a scalar's value in bytes, checked before
//!   the scalar is emitted.
//! * `max_document_size` (`Y2002`): the input length in bytes, checked
//!   first; the whole input is then skipped.
//! * `max_documents` (`Y2003`): checked when a document would start; the
//!   rest of the input is then skipped.
//!
//! The help text of each limit diagnostic names the setting and its current
//! value. The one leniency, `lenient_inline_colons`, is
//! described in `head.rs`.

mod block;
mod block_scalar;
mod flow;
mod flow_block;
mod flow_node;
mod head;
mod polish;
mod read;
mod run;
mod scalar;
mod scan;
mod stream;

/// How far past an implicit key's start the parser looks for its `:`
/// (bounds how far a validation window reads past a peeked
/// element's start).
pub(crate) use flow::KEY_WINDOW;

use std::collections::{BTreeMap, VecDeque};

use oneyaml_core::code::{
    DEPTH_LIMIT_EXCEEDED, DOCUMENT_TOO_LARGE, INTERNAL_ERROR, INVALID_COMMENT_CHARACTER,
    INVALID_ESCAPE, MISPLACED_BYTE_ORDER_MARK, MISSING_MAPPING_COLON, SCALAR_TOO_LONG,
    TAB_INDENTATION, TOO_MANY_DOCUMENTS, UNTERMINATED_FLOW_COLLECTION, UNTERMINATED_QUOTED_SCALAR,
};
use oneyaml_core::{Code, Diagnostic, Label, Settings, Span};

use crate::event::{Event, EventKind, EventList, NodeInfo, Properties, Sourced, TriviaKind};

pub(crate) use read::{
    anchor_at, decoded_suffix, directive_at, directive_prefix, resolve_tag, scalar_value,
    tag_source_at,
};

use scan::{break_len, byte, line_end, skip_white};

/// Parses `source` into the lossless event stream with the default
/// [`Settings`] (including [`Limits::default()`](oneyaml_core::Limits)).
///
/// Collects [`events`] into an [`EventList`]: the same parser, the same
/// events. Never fails and never panics: problems are reported in-band as
/// [`EventKind::Error`] and [`EventKind::Warning`] events, and the stream
/// always satisfies the event contract (C1–C6). Runs in O(n) time with no
/// recursion.
#[must_use]
pub fn parse(source: &str) -> EventList<'_> {
    parse_with(source, &Settings::new())
}

/// Parses `source` into the lossless event stream under `settings`,
/// collected into an [`EventList`] that keeps the source for reading (see
/// [`events_with`]).
///
/// The parser reads the limits `max_depth`, `max_scalar_len`,
/// `max_document_size`, and `max_documents`, the `lenient_inline_colons`
/// leniency, and `honor_version_directive` (for the wording of the `%YAML
/// 1.0` and `%YAML 1.1` warnings); every other setting belongs to a later layer. Same
/// guarantees as [`parse`].
#[must_use]
pub fn parse_with<'a>(source: &'a str, settings: &Settings) -> EventList<'a> {
    let events = events_with(source, settings)
        .map(Sourced::into_event)
        .collect();
    EventList::new(source, events)
}

/// The lossless event stream of `source` under the default [`Settings`],
/// produced incrementally (see [`Events`]).
#[must_use]
pub fn events(source: &str) -> Events<'_> {
    events_with(source, &Settings::new())
}

/// The lossless event stream of `source` under `settings`, produced
/// incrementally (see [`Events`]). The settings are read once; the
/// iterator does not borrow them.
#[must_use]
pub fn events_with<'a>(source: &'a str, settings: &Settings) -> Events<'a> {
    Events {
        parser: Parser::new(source, settings),
    }
}

/// A pull parser: an iterator over the lossless event stream of one source.
///
/// Each call to `next` runs the state machine only until the next event is
/// known, so a consumer that drops events as it reads them (the serde
/// layer) holds memory bounded by the nesting depth plus the grammar's
/// lookahead, independent of the input's length. See the module
/// docs for the bounds.
///
/// The stream is the one [`parse`] collects, and satisfies the event
/// contract (C1–C6). Each event comes paired with the source as a
/// [`Sourced`], so names and values are read from the right text by
/// construction. The iterator is fused: after `StreamEnd` it returns
/// `None` forever.
#[derive(Debug)]
pub struct Events<'a> {
    parser: Parser<'a>,
}

impl<'a> Iterator for Events<'a> {
    type Item = Sourced<'a>;

    fn next(&mut self) -> Option<Sourced<'a>> {
        let p = &mut self.parser;
        if let Some(event) = p.out.pop(p.src) {
            return Some(event);
        }
        p.fill();
        p.out.pop(p.src)
    }
}

impl core::iter::FusedIterator for Events<'_> {}

impl<'a> Events<'a> {
    /// The source text being parsed.
    #[must_use]
    pub const fn source(&self) -> &'a str {
        self.parser.src
    }

    /// Test instrumentation, not part of the API: the total size, in
    /// elements, of what the parser holds right now: the capacity of the
    /// output queue, the flow-collection hold, and the stacks and owed
    /// events that grow with nesting depth, plus the number of `%TAG`
    /// handles held for the current and the next document.
    ///
    /// For a consumer that drops each event, everything but the `%TAG`
    /// handles is bounded by the nesting depth plus the implicit-key
    /// lookahead (see *Streaming and buffering* in the module docs), never
    /// by the input's length. The handles grow with the number of `%TAG`
    /// directives of one document, which the input chooses; they are
    /// counted so that tests see them, and the bound is stated without
    /// them. Hidden from the docs: `tests/streaming.rs` checks the bound
    /// through it.
    #[doc(hidden)]
    #[must_use]
    pub fn retained_capacity(&self) -> usize {
        let p = &self.parser;
        p.out
            .q
            .capacity()
            .saturating_add(p.deferred.capacity())
            .saturating_add(p.stack.capacity())
            .saturating_add(p.flow_repair.capacity())
            .saturating_add(p.scratch.capacity())
            .saturating_add(p.flow.capacity())
            .saturating_add(p.tags.len())
            .saturating_add(p.pending_tags.len())
    }
}

/// How many items the parser queues ahead before handing events out: a
/// constant, so the queue stays bounded, and large enough that the
/// state machine runs in batches rather than once per event.
const BATCH: usize = 64;

/// An entry of the output queue: an event, or a run of trivia (white
/// space, line breaks, comments, byte order marks) that is lexed into
/// events only as the consumer pulls them, so any amount of trivia costs
/// O(1) memory.
#[derive(Clone, Debug)]
pub(super) enum Item<'a> {
    Event(Event<'a>),
    /// Trivia covering `start..end`.
    Trivia(usize, usize),
}

/// The output queue, in stream order.
#[derive(Debug, Default)]
pub(super) struct Out<'a> {
    q: VecDeque<Item<'a>>,
    /// What is left of the trivia run being lexed (empty when `.0 >= .1`);
    /// it comes before everything in `q`. Held here, not pushed back on the
    /// queue, so a run of several pieces (a line break and the next line's
    /// indentation, a comment) costs no queue traffic between them.
    run: (usize, usize),
}

impl<'a> Out<'a> {
    pub(super) fn event(&mut self, event: Event<'a>) {
        self.q.push_back(Item::Event(event));
    }

    /// Queues the trivia covering `start..end` (nothing when empty).
    pub(super) fn trivia(&mut self, start: usize, end: usize) {
        if start < end {
            self.q.push_back(Item::Trivia(start, end));
        }
    }

    pub(super) fn item(&mut self, item: Item<'a>) {
        match item {
            Item::Event(event) => self.event(event),
            Item::Trivia(start, end) => self.trivia(start, end),
        }
    }

    /// Enough is queued to hand out (see [`BATCH`]).
    fn is_full(&self) -> bool {
        self.q.len() >= BATCH
    }

    /// The next event, lexing the front trivia run if needed.
    #[inline]
    fn pop(&mut self, src: &'a str) -> Option<Sourced<'a>> {
        // The `Sourced` is built at each return site, so the item is
        // written once, straight into the caller's slot.
        let (from, to) = if self.run.0 < self.run.1 {
            self.run
        } else {
            match self.q.pop_front()? {
                Item::Event(event) => return Some(Sourced::new(event, src)),
                Item::Trivia(from, to) => (from, to),
            }
        };
        match scan::lex_trivia(src, from, to) {
            Some((kind, stop)) => {
                self.run = (stop, to);
                let span = Span::new(from, stop);
                Some(Sourced::new(Event::new(EventKind::Trivia(kind), span), src))
            }
            None => {
                self.run = (to, to);
                Some(Sourced::new(broken_trivia_run(from, to), src))
            }
        }
    }
}

/// Only trivia is ever queued as a run. In release the rest of the
/// run becomes an in-band Y9000 error, never a silently mislabelled trivia
/// event.
#[cold]
#[inline(never)]
fn broken_trivia_run<'a>(from: usize, to: usize) -> Event<'a> {
    debug_assert!(false, "non-trivia byte in a trivia run at {from}");
    let span = Span::new(from, to);
    Event::new(
        EventKind::Error(internal("non-trivia bytes in a trivia run", span)),
        span,
    )
}

/// Where a block mapping is within its current entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MapPhase {
    /// Expecting a key or the end of the mapping: `ns-l-block-map-entry`
    /// (production 188) via `?` (190), `:` after an empty key, or an
    /// implicit key (192-194).
    Key,
    /// The key after `?` is being parsed (`c-l-block-map-explicit-key`,
    /// production 190).
    ExplicitKey,
    /// An explicit key is complete; expecting `l-block-map-explicit-value`
    /// (production 191) or the next entry.
    AfterExplicitKey,
    /// The value after `:` is being parsed (productions 191 and 194).
    Value,
}

/// An open document or block collection.
#[derive(Clone, Copy, Debug)]
enum Frame {
    /// `l-any-document` (production 210); the root node is complete.
    Document { root_done: bool },
    /// `l+block-sequence(n)` (production 183) with entries at `indent`.
    Seq { indent: usize },
    /// `l+block-mapping(n)` (production 187) with keys at `indent`.
    Map { indent: usize, phase: MapPhase },
}

/// Context for parsing one node (`s-l+block-node(n,c)`, production 196).
#[derive(Clone, Copy, Debug)]
struct NodeCtx {
    /// Indentation `n` of the parent collection; `None` is the document
    /// level (indentation -1).
    parent: Option<usize>,
    /// A compact block collection may start on the current line
    /// (`s-l+block-indented`, production 185: after `-`, `?`, or an explicit
    /// `:`).
    compact: bool,
    /// A block sequence may sit at the parent's own indentation
    /// (`seq-space(n, block-out)`, production 201).
    seq_at_parent: bool,
}

/// What the machine does next.
#[derive(Clone, Copy, Debug)]
enum State {
    /// Between documents (`l-yaml-stream`, production 211).
    Stream,
    /// Expecting a node in the context given.
    Node(NodeCtx),
    /// A node or indicator is done; the innermost frame decides: another
    /// entry (productions 183-184, 187-194), the collection's end, or the
    /// document's end (`l-document-suffix`, production 205).
    Next,
    /// Inside a flow collection opened by a block node head (§7.4); the
    /// flow sub-machine (`flow.rs`) holds its own explicit stack.
    Flow,
    /// Resynchronising after an error (C6 R4): whole lines are skipped up
    /// to the next `c-directives-end`/`c-document-end` (203-204).
    Skip,
    /// `StreamEnd` emitted.
    Done,
}

#[derive(Debug)]
struct Parser<'a> {
    src: &'a str,
    /// Events ready for the consumer (bounded by one step's output).
    out: Out<'a>,
    /// End of the last emitted event.
    emit_pos: usize,
    /// Zero-width events owed at `emit_pos` (block collection ends and
    /// empty nodes), emitted lazily: before the pending trivia once the next
    /// token is accepted, or after the `Error` if it is not. Timing
    /// convention T2 (module docs), not a contract rule. Bounded by depth.
    deferred: Vec<EventKind>,
    /// Scan position. The bytes `emit_pos..pos` are trivia scanned but not
    /// yet emitted ("pending"); they are queued as one lazily lexed run.
    pos: usize,
    /// Start of the current line.
    line_start: usize,
    /// Leading spaces of the current line.
    line_indent: usize,
    /// No content has been consumed on the current line yet.
    fresh: bool,
    /// A tab occurred in white space since the last line break (or since the
    /// last consumed token on this line).
    tab_seen: bool,
    stack: Vec<Frame>,
    state: State,
    /// Properties emitted as trivia but not yet attached to a node.
    props: Properties,
    /// An implicit document was entered but its `DocumentStart` is deferred
    /// until its first node event (so an error in the first node's head
    /// yields no document at all). Timing convention T1 (module docs).
    doc_start_deferred: bool,
    /// Directives seen since the last document.
    directives_pending: bool,
    /// A `%YAML` directive was seen for the next document.
    yaml_seen: bool,
    /// `%TAG` handles declared for the next document, with their prefixes
    /// (read once).
    pending_tags: BTreeMap<&'a str, scan::TagPrefix>,
    /// `%TAG` handles of the current document.
    tags: BTreeMap<&'a str, scan::TagPrefix>,
    /// A `...` was just consumed; the rest of its line must be trivia.
    doc_end_line: bool,
    /// A document was ended by a byte order mark (`prefix_mark_at`), not
    /// by `...`: until the next `---`, only a document prefix may follow
    /// (production 211's second branch).
    /// Where that mark starts (for the diagnostic).
    prefix_without_end: Option<usize>,
    /// A byte order mark that caused the error about to be reported
    /// (`mark_then_fail`): its `MISPLACED_BYTE_ORDER_MARK` is reported with
    /// the error, in the order of their labels. Consumed
    /// by `fail_with`.
    mark_before_error: Option<usize>,
    /// Where the first character that cannot belong to any valid document
    /// is, when that is later than the scan position the failing construct
    /// started at: the error is reported there (consumed by `fail_with`).
    /// A cell, so that scanners holding only `&self` can name it.
    blame: core::cell::Cell<Option<usize>>,
    /// Zero-width events completing an erroneous flow collection, emitted
    /// right after its `Error` (C6 R2).
    flow_repair: Vec<Owed>,
    /// Reused buffer for a node head's events (no allocation per
    /// node on the hot path). At most a few events: one line's properties.
    scratch: Vec<Event<'a>>,
    /// The open flow collection, if any (`State::Flow`).
    flow: flow::FlowState<'a>,
    /// `Limits::max_depth`.
    max_depth: usize,
    /// `Limits::max_scalar_len`.
    max_scalar_len: usize,
    /// `Limits::max_document_size`.
    max_document_size: usize,
    /// `Limits::max_documents`.
    max_documents: usize,
    /// `Settings::lenient_inline_colons`.
    lenient_inline_colons: bool,
    /// `Settings::honor_version_directive`, for the `%YAML 1.0`
    /// and `%YAML 1.1` warnings' wording only.
    honor_version_directive: bool,
    /// Documents started so far.
    documents: usize,
    /// A stream-wide limit was exceeded: skip to the end of the input, not
    /// just to the next document marker.
    halted: bool,
    /// Start and end of the last quoted scalar emitted as a block node, so
    /// content glued to its closing quote (`'it's'`) gets a targeted
    /// diagnostic.
    last_quoted: (usize, usize),
}

/// A zero-width event owed after an error (C6 R2), packed once its position
/// is known.
#[derive(Clone, Debug)]
pub(super) enum Owed {
    /// A collection end.
    Kind(EventKind),
    /// An empty plain scalar with these properties.
    Empty(Properties),
}

/// `kind` with its node information set (for a node start or scalar).
pub(super) fn with_info(mut kind: EventKind, new: NodeInfo) -> EventKind {
    if let EventKind::Scalar { info, .. }
    | EventKind::MappingStart { info, .. }
    | EventKind::SequenceStart { info, .. } = &mut kind
    {
        *info = new;
    }
    kind
}

/// The `Y9000` diagnostic for a broken internal invariant: the release-build
/// outcome of a `debug_assert!`.
pub(super) fn internal(what: &str, span: Span) -> Box<Diagnostic> {
    Box::new(
        Diagnostic::error(
            INTERNAL_ERROR,
            format!("internal error: {what}"),
            Label::new(span, ""),
        )
        .with_help("this is a bug in oneyaml; please report it with this input"),
    )
}

/// The help every diagnostic with `code` gets unless its producer
/// refines it (the diagnostic polish list: what to write instead).
fn default_help(code: Code) -> Option<&'static str> {
    Some(match code {
        TAB_INDENTATION => {
            "YAML indents with spaces only; replace the tabs used for indentation here with spaces"
        }
        MISSING_MAPPING_COLON => {
            "each entry of a block mapping is written `key: value`, with a space after the `:`"
        }
        UNTERMINATED_QUOTED_SCALAR => {
            "add the closing quote; inside '...' write '' for a literal ', inside \"...\" write \\\" for a literal \""
        }
        INVALID_ESCAPE => {
            "in double quotes, `\\` starts an escape such as `\\n`, `\\t`, `\\\"` or `\\\\`; write `\\\\` for a literal backslash, or use single quotes, where `\\` has no special meaning"
        }
        UNTERMINATED_FLOW_COLLECTION => "close every `[` with `]` and every `{` with `}`",
        _ => return None,
    })
}

/// The `MISPLACED_BYTE_ORDER_MARK` diagnostic for the mark at `start..end`
/// (§5.2, production 202).
pub(super) fn misplaced_bom_diagnostic(start: usize, end: usize) -> Box<Diagnostic> {
    Box::new(
        Diagnostic::error(
            MISPLACED_BYTE_ORDER_MARK,
            "a byte order mark may appear only at the start of a document prefix",
            Label::new(Span::new(start, end), ""),
        )
        .with_help(
            "remove this byte order mark (U+FEFF); a mark is allowed only at the very start of the stream, or at the start of a line before a document",
        ),
    )
}

/// Whether `bytes` may hold a character that is not printable: false only
/// for bytes that are all ASCII, none a control character (a tab counts as
/// one here) or DEL. Eight bytes at a time.
#[inline(never)]
fn may_hold_unprintable(bytes: &[u8]) -> bool {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let mut words = bytes.chunks_exact(8);
    for chunk in words.by_ref() {
        let Ok(chunk) = <[u8; 8]>::try_from(chunk) else {
            return true;
        };
        let w = u64::from_le_bytes(chunk);
        // A byte with the high bit set, below 0x20, or equal to 0x7f.
        let del = w ^ (ONES * 0x7f);
        let found = w | (w.wrapping_sub(ONES * 0x20) & !w) | (del.wrapping_sub(ONES) & !del);
        if found & HIGH != 0 {
            return true;
        }
    }
    words.remainder().iter().any(|b| *b < 0x20 || *b >= 0x7f)
}

/// The error for the first character of the comment `start..end` that is
/// not printable, if any.
#[inline(never)]
pub(super) fn comment_char_diagnostic(
    src: &str,
    start: usize,
    end: usize,
) -> Option<Box<Diagnostic>> {
    let text = src.get(start..end)?;
    // Nearly every comment is plain printable ASCII: a cheap pass over the
    // bytes rules that out before the slower pass over characters.
    if !may_hold_unprintable(text.as_bytes()) {
        return None;
    }
    let (k, c) = text.char_indices().find(|(_, c)| !scan::is_printable(*c))?;
    let at = start.saturating_add(k);
    Some(Box::new(
        Diagnostic::error(
            INVALID_COMMENT_CHARACTER,
            "a comment may hold only printable characters",
            Label::new(Span::new(at, at.saturating_add(c.len_utf8())), ""),
        )
        .with_help("remove this character; control characters (U+0000 to U+001F except tab, U+007F, and U+0080 to U+009F except U+0085) cannot appear in a comment"),
    ))
}

fn diagnostic(code: Code, message: &str, span: Span) -> Box<Diagnostic> {
    Box::new(Diagnostic::error(code, message, Label::new(span, "")))
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, settings: &Settings) -> Self {
        let limits = settings.get_limits();
        let mut p = Self {
            src,
            out: Out::default(),
            emit_pos: 0,
            deferred: Vec::new(),
            pos: 0,
            line_start: 0,
            line_indent: 0,
            fresh: true,
            tab_seen: false,
            stack: Vec::new(),
            state: State::Stream,
            props: Properties::none(),
            doc_start_deferred: false,
            directives_pending: false,
            yaml_seen: false,
            pending_tags: BTreeMap::new(),
            tags: BTreeMap::new(),
            doc_end_line: false,
            prefix_without_end: None,
            mark_before_error: None,
            blame: core::cell::Cell::new(None),
            flow_repair: Vec::new(),
            scratch: Vec::new(),
            flow: flow::FlowState::default(),
            max_depth: limits.get_max_depth(),
            max_scalar_len: limits.get_max_scalar_len(),
            max_document_size: limits.get_max_document_size(),
            max_documents: limits.get_max_documents(),
            lenient_inline_colons: settings.get_lenient_inline_colons(),
            honor_version_directive: settings.get_honor_version_directive(),
            documents: 0,
            halted: false,
            last_quoted: (0, 0),
        };
        p.push(EventKind::StreamStart, 0, 0);
        if src.len() > p.max_document_size {
            // Refuse oversized input before parsing any of it.
            p.halt(DOCUMENT_TOO_LARGE, "the input is too large");
        }
        p
    }

    /// Runs the state machine until [`BATCH`] items are queued or the
    /// stream is complete. Not inlined, so that `Events::next`, which
    /// mostly just pops the queue, stays small (per-event overhead).
    #[inline(never)]
    fn fill(&mut self) {
        // One loop over an explicit state; no recursion on input.
        while !self.out.is_full() && !matches!(self.state, State::Done) {
            self.step();
        }
    }

    /// One transition of the state machine. Each queues a bounded number
    /// of items: one token or line and the zero-width events it
    /// confirms, which are bounded by the nesting depth.
    ///
    fn step(&mut self) {
        match self.state {
            State::Stream => self.stream(),
            State::Node(ctx) => self.node(ctx),
            State::Next => self.next(),
            State::Flow => self.flow_step(),
            State::Skip => self.skip(),
            State::Done => {}
        }
    }

    // ------------------------------------------------------------ emission

    /// Emits an event covering `start..end` (`start` must be `emit_pos`).
    fn push(&mut self, kind: EventKind, start: usize, end: usize) {
        self.push_event(Event::new(kind, Span::new(start, end)));
    }

    /// Emits a built event (its start must be `emit_pos`).
    fn push_event(&mut self, event: Event<'a>) {
        self.emit_pos = event.span().end;
        self.out.event(event);
    }

    /// Emits a zero-width event at `emit_pos` (before any pending trivia),
    /// after any deferred ones.
    fn zero(&mut self, kind: EventKind) {
        self.materialize();
        let at = self.emit_pos;
        self.push(kind, at, at);
    }

    /// [`Parser::zero`] for a node event with properties (packed relative
    /// to its position, `emit_pos`).
    fn zero_node(&mut self, kind: EventKind, props: Properties) {
        let info = NodeInfo::new(self.emit_pos, props, None);
        self.zero(with_info(kind, info));
    }

    /// Owes a zero-width event at `emit_pos` (see `deferred`).
    fn defer(&mut self, kind: EventKind) {
        self.deferred.push(kind);
    }

    /// Emits the deferred zero-width events.
    fn materialize(&mut self) {
        if self.deferred.is_empty() {
            return;
        }
        let at = self.emit_pos;
        for kind in self.deferred.drain(..) {
            self.out.event(Event::new(kind, Span::empty(at)));
        }
    }

    /// Emits the deferred events, then the pending trivia.
    fn flush(&mut self) {
        self.materialize();
        self.out.trivia(self.emit_pos, self.pos);
        self.emit_pos = self.pos;
    }

    /// Flushes and, for a deferred implicit document, emits its start: call
    /// before the first event of any node.
    fn begin_content(&mut self) {
        self.flush();
        if self.doc_start_deferred {
            self.doc_start_deferred = false;
            self.zero(EventKind::document_start(false));
        }
    }

    /// Emits an event at the scan position after flushing, and consumes it.
    fn token(&mut self, kind: EventKind, end: usize) {
        let start = self.pos;
        self.push(kind, start, end);
        self.pos = end;
        self.fresh = false;
        self.tab_seen = false;
    }

    /// The reusable head buffer, empty.
    fn take_scratch(&mut self) -> Vec<Event<'a>> {
        let mut events = core::mem::take(&mut self.scratch);
        events.clear();
        events
    }

    /// Commits a head buffer and keeps its capacity for the next head.
    fn commit_scratch(&mut self, mut events: Vec<Event<'a>>) {
        self.commit(events.drain(..));
        self.scratch = events;
    }

    /// An empty plain scalar at `emit_pos` carrying the pending properties.
    /// With properties the node is certain and is emitted at once; without,
    /// it is deferred until the next token confirms it (timing convention
    /// T2).
    fn empty(&mut self) {
        let props = core::mem::take(&mut self.props);
        if props.is_empty() {
            self.defer(EventKind::plain());
        } else {
            self.zero_node(EventKind::plain(), props);
        }
    }

    fn col(&self) -> usize {
        self.pos.saturating_sub(self.line_start)
    }

    // ------------------------------------------------------------ limits

    /// The number of open block collections. The stack is a `Document`
    /// frame followed by the block collections inside it.
    fn block_depth(&self) -> usize {
        self.stack.len().saturating_sub(1)
    }

    /// `Limits::max_depth` allows a collection at `depth`.
    fn depth_ok(&self, depth: usize) -> bool {
        depth <= self.max_depth
    }

    /// `Limits::max_scalar_len` allows a scalar value of `len` bytes.
    fn scalar_len_ok(&self, len: usize) -> bool {
        len <= self.max_scalar_len
    }

    /// The help of a limit diagnostic names the setting to raise and
    /// its current value.
    fn limit_help(&self, code: Code) -> Option<String> {
        let (setting, value) = match code {
            DEPTH_LIMIT_EXCEEDED => ("max_depth", self.max_depth),
            SCALAR_TOO_LONG => ("max_scalar_len", self.max_scalar_len),
            DOCUMENT_TOO_LARGE => ("max_document_size", self.max_document_size),
            TOO_MANY_DOCUMENTS => ("max_documents", self.max_documents),
            _ => return None,
        };
        Some(format!("raise Limits::{setting} (currently {value})"))
    }

    /// Reports a stream-wide limit and skips the rest of the input.
    fn halt(&mut self, code: Code, message: &str) {
        self.fail(code, message, false);
        self.halted = true;
    }

    // ------------------------------------------------------------ trivia

    /// `#` at `i` starts a comment (production 75).
    fn comment_at(&self, i: usize) -> bool {
        scan::comment_at(self.src, i)
    }

    /// Scans white space, comments, and line breaks as pending trivia
    /// (`s-l-comments`, production 79, and `s-separate`, production 80).
    ///
    /// Byte order marks (§5.2, productions 202 and 211): `bom` says the
    /// scan is between documents, where `l-document-prefix` may start.
    /// There a mark at the very start of a line (nothing before it on the
    /// line, no directives pending) is `ByteOrderMark` trivia and the
    /// line's columns count after it. Inside a document such a mark
    /// ([`Parser::prefix_mark_at`]) ends the document like `---` does
    /// too: the scan stops before it, and the structure closes
    /// around it. Anywhere else a mark is a recoverable
    /// `MISPLACED_BYTE_ORDER_MARK` error, and the parse goes on
    /// as if it were absent (`misplaced_bom`). A mark inside a comment
    /// stays in the comment token and is reported after it
    /// (`marks_in_token`).
    fn scan_trivia(&mut self, bom: bool) {
        loop {
            let i = self.pos;
            let kind_end = match byte(self.src, i) {
                Some(b' ' | b'\t') => {
                    // Leading spaces of the run, then (rarely) tabs.
                    let spaces = self
                        .src
                        .as_bytes()
                        .get(i..)
                        .map_or(0, |r| r.iter().take_while(|b| **b == b' ').count());
                    let mut end = i.saturating_add(spaces);
                    if byte(self.src, end) == Some(b'\t') {
                        self.tab_seen = true;
                        end = skip_white(self.src, end);
                    }
                    // Leading spaces of the line: the run starts where the
                    // line's spaces so far end, which is the line start
                    // unless a skipped misplaced mark sits in between (its
                    // bytes were moved out of the line by `misplaced_bom`).
                    if i == self.line_start.saturating_add(self.line_indent) {
                        self.line_indent = self.line_indent.saturating_add(spaces);
                    }
                    (TriviaKind::Whitespace, end)
                }
                Some(b'\n' | b'\r') => {
                    let end = i.saturating_add(break_len(self.src, i));
                    self.line_start = end;
                    self.line_indent = 0;
                    self.fresh = true;
                    self.tab_seen = false;
                    (TriviaKind::LineBreak, end)
                }
                Some(b'#') if self.comment_at(i) => {
                    // c-nb-comment-text (production 75) is `nb-char`s,
                    // which exclude the mark (production 27).
                    let (end, clean) = scan::comment_extent(self.src, i);
                    self.pos = end;
                    // Printable ASCII holds no byte order mark and no
                    // character to report.
                    if !clean {
                        let bad = comment_char_diagnostic(self.src, i, end);
                        self.marks_in_token(i, end, bad);
                    }
                    continue;
                }
                _ if !bom && self.prefix_mark_at(i) => break,
                _ if scan::mark_at(self.src, i) => {
                    let end = i.saturating_add('\u{feff}'.len_utf8());
                    let prefix_start = bom
                        && self.fresh
                        && i == self.line_start
                        && self.line_indent == 0
                        && !self.directives_pending;
                    if !prefix_start {
                        self.misplaced_bom(i, end);
                        continue;
                    }
                    // §5.2: the mark is no column; the line starts after it.
                    self.line_start = end;
                    self.line_indent = 0;
                    (TriviaKind::ByteOrderMark, end)
                }
                _ => break,
            };
            // Pending trivia is queued later as one lazy run (`flush`).
            let (_, end) = kind_end;
            self.pos = end;
        }
    }

    /// A byte order mark at `i` at the very start of a fresh line inside a
    /// document: it can continue nothing (it is no `ns-char`), so it ends
    /// the document implicitly and starts an `l-document-prefix`
    /// (production 211, §9.1.1). What may follow the prefix is
    /// checked in `stream` (`prefix_without_end`).
    pub(super) fn prefix_mark_at(&self, i: usize) -> bool {
        self.fresh
            && i == self.line_start
            && self.line_indent == 0
            && !self.stack.is_empty()
            && scan::mark_at(self.src, i)
    }

    /// Reports each byte order mark inside the token `start..end` (a
    /// comment, a block scalar), which keeps its bytes: one
    /// zero-width `MISPLACED_BYTE_ORDER_MARK` error per mark right after
    /// the token, whose label covers the mark. The pending
    /// trivia up to the scan position is flushed first, so the error
    /// directly follows the token and nothing lies between them.
    pub(super) fn marks_in_token(
        &mut self,
        start: usize,
        end: usize,
        bad: Option<Box<Diagnostic>>,
    ) {
        let text = self.src.get(start..end).unwrap_or_default();
        if bad.is_none() && !text.contains('\u{feff}') {
            return;
        }
        self.flush();
        // The errors keep the order of the characters they report.
        let mut bad = bad;
        for (k, _) in text.match_indices('\u{feff}') {
            let at = start.saturating_add(k);
            if bad.as_ref().is_some_and(|d| d.primary.span.start < at) {
                self.comment_error(bad.take());
            }
            let diag = misplaced_bom_diagnostic(at, at.saturating_add('\u{feff}'.len_utf8()));
            let here = self.emit_pos;
            self.push(EventKind::Error(diag), here, here);
        }
        self.comment_error(bad);
    }

    /// Pushes the error for a character that cannot be in a comment, if
    /// there is one, right after the comment (production 1; comment text
    /// is `nb-char`s, productions 75 and 27): one zero-width
    /// `INVALID_COMMENT_CHARACTER` error whose label covers the character.
    /// The comment itself is kept as written, and the parse goes on.
    fn comment_error(&mut self, bad: Option<Box<Diagnostic>>) {
        if let Some(diag) = bad {
            let here = self.emit_pos;
            self.push(EventKind::Error(diag), here, here);
        }
    }

    /// A byte order mark at `start..end` where no document prefix may
    /// start (§5.2, production 202): a recoverable
    /// `MISPLACED_BYTE_ORDER_MARK` error covering the mark, after the
    /// pending trivia (deferred zero-width events go first, at their own
    /// position). The parse then continues as if the mark were absent
    /// between tokens: the line start moves by the mark's length, so
    /// columns and the line's leading spaces are counted without it, and
    /// `fresh` is unchanged. (A mark right after a plain scalar's text ends
    /// that scalar, as any non-`ns-char` does; what follows is new
    /// content. Marks inside comments and block scalars: `marks_in_token`.)
    fn misplaced_bom(&mut self, start: usize, end: usize) {
        self.flush();
        self.push(
            EventKind::Error(misplaced_bom_diagnostic(start, end)),
            start,
            end,
        );
        self.pos = end;
        self.line_start = self
            .line_start
            .saturating_add(end.saturating_sub(start))
            .min(end);
    }

    // ------------------------------------------------------------ structure

    /// A child node of the innermost frame is complete.
    fn child_done(&mut self) {
        match self.stack.last_mut() {
            Some(Frame::Document { root_done }) => *root_done = true,
            Some(Frame::Map { phase, .. }) => {
                *phase = match *phase {
                    MapPhase::ExplicitKey => MapPhase::AfterExplicitKey,
                    MapPhase::AfterExplicitKey | MapPhase::Value | MapPhase::Key => MapPhase::Key,
                };
            }
            Some(Frame::Seq { .. }) | None => {}
        }
        self.state = State::Next;
    }

    /// Closes the innermost block collection with a zero-width end. `certain`:
    /// the end is confirmed (a strict dedent or the end of the document) and
    /// is emitted at once; otherwise it is deferred until the next token is
    /// accepted by an outer collection (timing convention T2).
    fn close_block(&mut self, certain: bool) {
        let end = match self.stack.pop() {
            Some(Frame::Map { .. }) => EventKind::MappingEnd,
            Some(Frame::Seq { .. }) => EventKind::SequenceEnd,
            Some(frame @ Frame::Document { .. }) => {
                self.stack.push(frame);
                self.child_done();
                return;
            }
            None => {
                self.child_done();
                return;
            }
        };
        if certain {
            self.zero(end);
        } else {
            self.defer(end);
        }
        self.child_done();
    }

    // ------------------------------------------------------------ errors

    /// Reports a syntax error at the scan position covering the rest of the
    /// line, repairs the structure (C6 R2), and starts resynchronising (R4).
    /// `slot_open`: a node is owed to the innermost frame.
    fn fail(&mut self, code: Code, message: &str, slot_open: bool) {
        self.fail_with(code, message, slot_open, |d| d);
    }

    /// [`Parser::fail`] with a diagnostic refined by `refine` (secondary
    /// labels, help, suggestions). Limit help is added automatically.
    fn fail_with(
        &mut self,
        code: Code,
        message: &str,
        slot_open: bool,
        refine: impl FnOnce(Diagnostic) -> Diagnostic,
    ) {
        let mark = self.mark_before_error.take();
        let blame = self.blame.take();
        if matches!(self.state, State::Skip) {
            // Already failed in this step: one error per problem, and never
            // a second error that could swallow the next line (e.g. `---`).
            // Emit anything scanned since, so the stream keeps tiling (C1)
            // when `skip` resumes at `pos`.
            self.flush();
            if self.emit_pos != self.pos || !self.deferred.is_empty() {
                // `flush` guarantees this; never a panic: a Y9000 event in
                // release.
                debug_assert!(false, "flush left owed events or pending trivia");
                self.internal_error("`flush` left owed events or pending trivia");
            }
            return;
        }
        // Deferred events were not confirmed by an accepted token: they go
        // after the error, as part of its repair (R2, R5).
        let deferred = core::mem::take(&mut self.deferred);
        self.flush();
        self.deferred = deferred;
        let start = self.pos;
        // Without a mark named by the failing construct, a mark
        // on the rest of the offending line that no quote precedes (so it
        // is not inside a quoted scalar, whose `nb-json` allows it) is
        // reported too. Limit and internal errors are about something
        // else.
        let mark = mark.or_else(|| {
            let about_syntax = code.get() < 1000;
            let text = self.src.get(start..line_end(self.src, start))?;
            let unquoted = text.find(['\'', '"']).unwrap_or(text.len());
            let k = text.get(..unquoted)?.find('\u{feff}')?;
            about_syntax.then_some(start.saturating_add(k))
        });
        // The mark that made this construct fail is reported
        // too, with its own span, as a zero-width event right after the
        // error (the first error event is the first diagnostic by
        // position, the one a strict parse fails with).
        //
        // A mark is never before `start` (= `self.pos`), so the error always
        // comes first. The fallback above searches from `start`;
        // `mark_then_fail` callers pass a mark found from the scan position on
        // (`directive`) or in a block scalar header that the scan position has
        // not passed (`block_scalar`); `block_scalar_error` places `pos` at the
        // offending line's start, or past its leading spaces, which hold no
        // mark; and a node head's mark (`head`, the mark after its content) is
        // used only by a failure at the head's start, or cleared (`commit_key`,
        // `emit_node`, `inline_colon`, a head that is not a key) before the
        // scan position moves past it. Never a panic: should that ever break,
        // an internal error in release, never a misordered report.
        let mark_diag =
            |m: usize| misplaced_bom_diagnostic(m, m.saturating_add('\u{feff}'.len_utf8()));
        if mark.is_some_and(|m| m < start) {
            debug_assert!(false, "a byte order mark before the error's start");
            self.internal_error("a byte order mark before the error's start");
        }
        // At a document marker the error is zero-width, so resynchronisation
        // resumes at that marker (R4).
        let at_marker = start == self.line_start
            && (scan::any_marker_at(self.src, start) || self.prefix_mark_at(start));
        let end = if at_marker {
            start
        } else {
            line_end(self.src, start)
        };
        // The report points at the first character that cannot belong, when
        // the producer named one; the event keeps covering the line.
        let blame = blame.or_else(|| scalar::quoted_fault(self.src, start, code));
        let at = blame.filter(|b| *b >= start).unwrap_or(start);
        // A byte order mark that made the construct fail is reported right
        // after it, so the error never starts after that mark.
        let at = mark.filter(|m| *m >= start).map_or(at, |m| at.min(m));
        let mut diag = Diagnostic::error(code, message, Label::new(Span::new(at, end.max(at)), ""));
        if let Some(help) = self.limit_help(code) {
            diag = diag.with_help(help);
        } else if let Some(help) = default_help(code) {
            diag = diag.with_help(help);
        }
        let diag = refine(diag);
        self.push(EventKind::Error(Box::new(diag)), start, end);
        self.pos = end;
        if let Some(m) = mark.filter(|m| *m >= start) {
            self.push(EventKind::Error(mark_diag(m)), end, end);
        }
        let mut repairs = core::mem::take(&mut self.flow_repair);
        for owed in repairs.drain(..) {
            match owed {
                Owed::Kind(kind) => self.zero(kind),
                Owed::Empty(props) => self.zero_node(EventKind::plain(), props),
            }
        }
        self.flow_repair = repairs;
        self.repair(slot_open);
        self.materialize();
        self.state = State::Skip;
    }

    /// [`Parser::fail`], also reporting the byte order mark at `mark`, if
    /// any, that made the construct fail (after the error
    /// when the mark lies inside the offending line).
    pub(super) fn mark_then_fail(
        &mut self,
        mark: Option<usize>,
        code: Code,
        message: &str,
        slot_open: bool,
    ) {
        self.mark_before_error = mark;
        self.fail(code, message, slot_open);
    }

    /// [`Parser::fail`] for a broken internal invariant: a `Y9000` error
    /// with the usual repair and resynchronisation. Callers
    /// `debug_assert!` first, so this runs only in release builds.
    fn fail_internal(&mut self, what: &str, slot_open: bool) {
        let message = format!("internal error: {what}");
        self.fail_with(INTERNAL_ERROR, &message, slot_open, |d| {
            d.with_help("this is a bug in oneyaml; please report it with this input")
        });
    }

    /// Reports a broken internal invariant where no repair is possible: a
    /// zero-width `Y9000` error at `emit_pos` after any owed events, then
    /// the pending bytes as skipped trivia, so the stream still tiles (C1).
    /// Callers `debug_assert!` first.
    fn internal_error(&mut self, what: &str) {
        self.materialize();
        let at = self.emit_pos;
        self.push(EventKind::Error(internal(what, Span::empty(at))), at, at);
        if at < self.pos {
            let end = self.pos;
            self.push(EventKind::Trivia(TriviaKind::Skipped), at, end);
        }
    }

    /// A zero-width error at `emit_pos` that needs no repair (the parse
    /// continues as if the problem were absent).
    fn soft_error(&mut self, code: Code, message: &str) {
        let at = self.emit_pos;
        self.soft_error_at(code, message, at);
    }

    /// [`Parser::soft_error`] whose diagnostic points at `at`, an earlier
    /// position than the event (which stays zero-width at the scan end).
    fn soft_error_at(&mut self, code: Code, message: &str, at: usize) {
        self.zero(EventKind::Error(diagnostic(code, message, Span::empty(at))));
    }

    /// Completes every open block collection with zero-width events after an
    /// error (C6 R2). The document stays open until resynchronisation.
    fn repair(&mut self, slot_open: bool) {
        // A failed directive block declares nothing (§6.8): its `%TAG`
        // handles and `%YAML` must not reach a later document.
        self.directives_pending = false;
        self.pending_tags.clear();
        self.yaml_seen = false;
        self.prefix_without_end = None;
        if self.doc_start_deferred {
            // Nothing of this document was emitted: drop it entirely.
            self.doc_start_deferred = false;
            self.deferred.clear();
            self.props = Properties::none();
            self.stack.clear();
            return;
        }
        if slot_open || !self.props.is_empty() {
            self.empty();
            self.child_done();
        }
        loop {
            match self.stack.last().copied() {
                Some(Frame::Map { phase, .. }) if phase != MapPhase::Key => {
                    self.empty();
                    self.child_done();
                }
                Some(Frame::Map { .. } | Frame::Seq { .. }) => self.close_block(false),
                Some(Frame::Document { root_done }) => {
                    if !root_done {
                        self.empty();
                        self.child_done();
                    }
                    break;
                }
                None => break,
            }
        }
    }

    /// Skips one line (or line break) after an error, until a document
    /// marker at the start of a line or the end of input (C6 R4). One line
    /// per step, so skipping never buffers.
    fn skip(&mut self) {
        let i = self.pos;
        let blen = break_len(self.src, i);
        if blen > 0 {
            let end = i.saturating_add(blen);
            self.push(EventKind::Trivia(TriviaKind::LineBreak), i, end);
            self.pos = end;
            self.line_start = end;
            self.line_indent = 0;
            return;
        }
        // A mark at the start of a line may start a document
        // prefix (production 211), so recovery resumes there too.
        let resync = i >= self.src.len()
            || !self.halted
                && i == self.line_start
                && (scan::any_marker_at(self.src, i) || scan::mark_at(self.src, i));
        if !resync {
            let end = line_end(self.src, i);
            self.push(EventKind::Trivia(TriviaKind::Skipped), i, end);
            self.pos = end;
            return;
        }
        self.fresh = true;
        self.line_indent = 0;
        self.tab_seen = false;
        self.state = if self.stack.is_empty() {
            State::Stream
        } else {
            State::Next
        };
    }
}

#[cfg(test)]
mod equivalence_tests;
#[cfg(test)]
mod tests;
