//! Events to the flat arena (an explicit stack of open nodes, no
//! recursion; O(n) time).

use super::stored::Stored;
use oneyaml_core::code::{INTERNAL_ERROR, INVALID_EVENT_STREAM, TREE_CAPACITY_EXCEEDED};
use oneyaml_core::{Diagnostic, Label, Span};

use super::{Extra, NONE, SyntaxKind, WRAP};
use crate::event::{
    CollectionStyle, Directive, Event, EventKind, Indicator, ScalarStyle, TriviaKind,
};

/// Elements kept free below the index limit for the closing error node,
/// its text, and the skipped rest of the input.
const HEADROOM: usize = 4;

/// The largest arena the `u32` indices address.
const MAX_ELEMENTS: usize = (u32::MAX as usize).saturating_sub(HEADROOM);

/// The arrays of a built tree.
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) struct Parts {
    pub(super) kinds: Vec<SyntaxKind>,
    pub(super) parents: Vec<u32>,
    pub(super) words: Vec<u32>,
    pub(super) wraps: Vec<u32>,
    pub(super) extras: Vec<(u32, Extra)>,
    pub(super) diagnostics: Vec<(u32, Stored)>,
    /// `Document` children of the stream (`max_documents` on edits
    /// is checked against a count, never a walk over the documents).
    pub(super) documents: usize,
    /// Diagnostics counted but not kept (`max_diagnostics`).
    pub(super) suppressed: usize,
}

/// What a buffered run of events is waiting for (see [`Builder::push`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// Properties or a block scalar header: the node event they belong to.
    Node,
    /// Directives: the explicit `DocumentStart` they belong to.
    Document,
}

pub(super) struct Builder<'a> {
    kinds: Vec<SyntaxKind>,
    parents: Vec<u32>,
    words: Vec<u32>,
    wraps: Vec<u32>,
    extras: Vec<(u32, Extra)>,
    diagnostics: Vec<(u32, Stored)>,
    /// How many diagnostics are kept in arrival order
    /// (`max_diagnostics + 1`: the first one past the limit tells where
    /// suppression starts); the first error is kept whatever its place.
    keep: usize,
    error_kept: bool,
    /// Diagnostics admitted so far.
    admitted: usize,
    /// Diagnostics counted but not kept.
    suppressed: usize,
    /// `Document` nodes opened as children of the stream.
    documents: usize,
    /// Open nodes, innermost last; `stack[0]` is the stream.
    stack: Vec<u32>,
    /// Events that precede the node (or document) they belong to: a node's
    /// properties and block scalar header with the trivia, warnings, and
    /// errors between them (C3, C5), or a document's directives. Replayed
    /// as that node's first children once it opens.
    pending: Vec<Pend<'a>>,
    owner: Owner,
    /// Directives waiting for their `---` (and the trivia
    /// and diagnostics among them) are written into the arena as they
    /// come, not buffered: when the `---` adopts them, its `Document`
    /// element is inserted before them (each held element moves once,
    /// O(n) in all); otherwise they simply stay. Only what no longer fits
    /// below the arena's cap (one slot kept for the `Document`) waits in
    /// `pending`, so the tree is the one buffering would build.
    held: Option<Held>,
    /// Whether directives are held (`false`: buffered, the old way;
    /// tests compare the two).
    hold: bool,
    /// End of the last token (C1: where the next one must start).
    pos: usize,
    /// Start of the last token (starts are kept non-decreasing).
    last_start: usize,
    /// Crossings of 2³² recorded so far.
    high: u64,
    source_len: usize,
    /// Most elements this arena may hold (tests lower it).
    cap: usize,
    /// Where the index space ran out; everything from here is skipped.
    full_at: Option<usize>,
    /// The stream broke the event contract.
    malformed: bool,
    /// Where the stream came from, which decides the code of a contract
    /// violation.
    origin: Origin,
}

/// Where held directives start (see [`Builder::held`]): the lengths of the
/// arena's lists then, and the node they were added to.
#[derive(Clone, Copy, Debug)]
struct Held {
    at: usize,
    diagnostics: usize,
    wraps: usize,
    extras: usize,
    parent: u32,
}

/// A buffered event (see [`Builder::push`]). A
/// diagnostic past `max_diagnostics` waits as its fixed-size skeleton, its
/// texts already dropped, so nothing past the limit holds heap memory.
///
/// Every buffered event is a token (trivia, directives) or a diagnostic,
/// so it waits in that compact form (directives that wait for
/// a `---` hold their whole run until it comes).
#[derive(Debug)]
enum Pend<'a> {
    /// A token: trivia or a directive.
    Token(SyntaxKind, Span),
    /// A warning.
    Warn(Stored),
    /// An error, with its event's span (its error node's text).
    Err(Box<(Stored, Span)>),
    /// Any other event (none is buffered today).
    Event(Box<Event<'a>>),
}

impl<'a> Pend<'a> {
    fn start(&self) -> usize {
        match self {
            Self::Token(_, s) => s.start,
            Self::Event(e) => e.span().start,
            Self::Warn(s) => s.span().start,
            Self::Err(e) => e.1.start,
        }
    }

    /// `event` (admitted, with its texts) in its compact form.
    fn of(event: Event<'a>) -> Self {
        let span = event.span();
        match event.kind() {
            EventKind::Trivia(_)
            | EventKind::Directive(_)
            | EventKind::Warning(_)
            | EventKind::Error(_) => {}
            _ => return Self::Event(Box::new(event)),
        }
        match event.into_kind() {
            EventKind::Trivia(t) => Self::Token(trivia_kind(t), span),
            EventKind::Directive(d) => Self::Token(directive_kind(d), span),
            EventKind::Warning(d) => Self::Warn(Stored::Full(d)),
            EventKind::Error(d) => Self::Err(Box::new((Stored::Full(d), span))),
            // (Unreachable: matched above.)
            other => Self::Event(Box::new(Event::new(other, span))),
        }
    }
}

/// The token kind of a directive.
fn directive_kind(d: Directive) -> SyntaxKind {
    match d {
        Directive::Yaml => SyntaxKind::YamlDirective,
        Directive::Tag => SyntaxKind::TagDirective,
        Directive::Reserved => SyntaxKind::ReservedDirective,
    }
}

/// Who produced the events a [`Builder`] is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Origin {
    /// This crate's parser: a contract violation is a bug in oneyaml
    /// (`Y9000`).
    Parser,
    /// The caller (`SyntaxTree::from_events`): a contract violation is an
    /// invalid input stream (`Y9001`), not a bug.
    Caller,
}

impl<'a> Builder<'a> {
    pub(super) fn new(source_len: usize, origin: Origin) -> Self {
        Self::with_cap(source_len, MAX_ELEMENTS, origin)
    }

    pub(super) fn with_cap(source_len: usize, cap: usize, origin: Origin) -> Self {
        let mut b = Self {
            kinds: Vec::new(),
            parents: Vec::new(),
            words: Vec::new(),
            wraps: Vec::new(),
            extras: Vec::new(),
            diagnostics: Vec::new(),
            keep: usize::MAX,
            error_kept: false,
            admitted: 0,
            suppressed: 0,
            documents: 0,
            stack: Vec::new(),
            pending: Vec::new(),
            owner: Owner::Node,
            held: None,
            hold: true,
            pos: 0,
            last_start: 0,
            high: 0,
            source_len,
            cap: cap.max(1),
            full_at: None,
            malformed: false,
            origin,
        };
        b.open(SyntaxKind::Stream);
        b
    }

    /// Adds the next event of the stream.
    ///
    /// Anchors, tags, block scalar headers, and directives are buffered
    /// with what follows them until their owner arrives, which then adopts
    /// them as its first children. Anything else ends the buffer, which is
    /// then flushed into the current node unchanged.
    ///
    /// Whether a diagnostic keeps its texts is
    /// decided here, on arrival: one past `max_diagnostics` is kept as its
    /// fixed-size skeleton ([`Stored::skeleton`]), so the tree's record of
    /// what the parse found stays complete at 32 bytes each. O(1).
    pub(super) fn push(&mut self, event: Event<'a>) {
        if self.full_at.is_some() {
            return;
        }
        if (event.is_warning() || event.is_error()) && !self.admit(event.is_error()) {
            self.suppressed = self.suppressed.saturating_add(1);
            let span = event.span();
            let item = match event.diagnostic() {
                Some(d) if event.is_error() => Pend::Err(Box::new((Stored::skeleton(d), span))),
                Some(d) => Pend::Warn(Stored::skeleton(d)),
                None => return,
            };
            // Buffered like the event itself while buffering.
            if self.held.is_some() {
                self.hold(item);
            } else if self.pending.is_empty() {
                self.skeleton(item);
            } else {
                self.pending.push(item);
            }
            return;
        }
        if self.buffering() {
            if self.buffers(&event) {
                if self.held.is_some() {
                    self.hold(Pend::of(event));
                } else {
                    self.pending.push(Pend::of(event));
                }
                return;
            }
            if self.adopts(&event) {
                self.node_event(event, true);
                return;
            }
            self.replay();
        }
        if let Some(owner) = starts_buffer(&event) {
            self.owner = owner;
            if owner == Owner::Document && self.hold {
                self.held = Some(Held {
                    at: self.kinds.len(),
                    diagnostics: self.diagnostics.len(),
                    wraps: self.wraps.len(),
                    extras: self.extras.len(),
                    parent: self.stack.last().copied().unwrap_or(NONE),
                });
                self.hold(Pend::of(event));
            } else {
                self.pending.push(Pend::of(event));
            }
            return;
        }
        self.node_event(event, false);
    }

    /// Events are waiting for their owner (held or buffered).
    fn buffering(&self) -> bool {
        self.held.is_some() || !self.pending.is_empty()
    }

    /// A held item: into the arena now if it fits below the cap with a
    /// slot to spare for the `Document` that may adopt it (and nothing is
    /// buffered yet); else buffered, so the arena fills exactly as when
    /// everything was buffered (the same tree at the cap).
    fn hold(&mut self, item: Pend<'a>) {
        let needed: usize = match &item {
            Pend::Warn(_) => 0,
            Pend::Err(_) => 2,
            Pend::Token(..) => 1,
            Pend::Event(_) => usize::MAX,
        };
        let fits = self.pending.is_empty()
            && self.full_at.is_none()
            && self
                .kinds
                .len()
                .checked_add(needed)
                .and_then(|n| n.checked_add(1))
                .is_some_and(|n| n <= self.cap);
        if fits {
            self.skeleton(item);
        } else {
            self.pending.push(item);
        }
    }

    /// The `---` adopting held directives: its `Document` element goes
    /// before them, which become its first children (O(held elements)).
    fn adopt_held(&mut self, h: Held) {
        // As `open` would have, before the held elements were added: one
        // slot was kept free for it (see `hold`).
        if self.full_at.is_some() {
            return;
        }
        if self.kinds.len() >= self.cap {
            // (Nothing was held in the arena: it was full already.)
            self.full_at = Some(self.pos);
            return;
        }
        let (Ok(at32), Some(len)) = (u32::try_from(h.at), self.kinds.len().checked_add(1)) else {
            self.malformed = true;
            return;
        };
        if h.at > self.kinds.len() {
            debug_assert!(false, "held elements past the arena");
            self.malformed = true;
            return;
        }
        if self.stack.len() == 1 {
            self.documents = self.documents.saturating_add(1);
        }
        self.kinds.insert(h.at, SyntaxKind::Document);
        self.parents.insert(h.at, h.parent);
        self.words.insert(h.at, as_u32(len));
        let shift = |x: &mut u32| {
            if *x != NONE && *x >= at32 {
                *x = x.saturating_add(1);
            }
        };
        let from = h.at.saturating_add(1);
        let kinds = self.kinds.get(from..).unwrap_or_default();
        let parents = self.parents.get_mut(from..).unwrap_or_default();
        let words = self.words.get_mut(from..).unwrap_or_default();
        for ((kind, parent), word) in kinds.iter().zip(parents).zip(words) {
            if *parent == h.parent {
                *parent = at32;
            } else {
                shift(parent);
            }
            // A node's word is its subtree's end (a token's, its text's
            // start).
            if !kind.is_token() {
                shift(word);
            }
        }
        for (i, _) in self
            .diagnostics
            .get_mut(h.diagnostics..)
            .unwrap_or_default()
        {
            shift(i);
        }
        for i in self.wraps.get_mut(h.wraps..).unwrap_or_default() {
            shift(i);
        }
        for (i, _) in self.extras.get_mut(h.extras..).unwrap_or_default() {
            shift(i);
        }
        self.stack.push(at32);
    }

    /// Whether a diagnostic arriving now is kept: the first `keep`, and
    /// the first error wherever it is.
    fn admit(&mut self, error: bool) -> bool {
        if self.admitted < self.keep || (error && !self.error_kept) {
            self.admitted = self.admitted.saturating_add(1);
            self.error_kept |= error;
            return true;
        }
        false
    }

    /// While buffering: `event` joins the buffer.
    fn buffers(&self, event: &Event<'a>) -> bool {
        match event.kind() {
            EventKind::Trivia(_) | EventKind::Warning(_) | EventKind::Error(_) => true,
            EventKind::Directive(_) => self.owner == Owner::Document,
            _ => false,
        }
    }

    /// While buffering: `event` owns the buffer. A node owns it when its
    /// first property (or header) is the buffer's first event.
    fn adopts(&self, event: &Event<'a>) -> bool {
        let first = self.pending.first().map(Pend::start);
        match (self.owner, event.kind()) {
            (Owner::Document, EventKind::DocumentStart { explicit }) => *explicit,
            (
                Owner::Node,
                EventKind::Scalar { .. }
                | EventKind::MappingStart { .. }
                | EventKind::SequenceStart { .. },
            ) => {
                let props = event.properties().unwrap_or_default();
                let header = event.block_header().map(|h| h.span.start);
                [props.anchor, props.tag, header]
                    .into_iter()
                    .flatten()
                    .min()
                    .is_some_and(|at| Some(at) == first)
            }
            _ => false,
        }
    }

    /// Flushes the buffer into the current node (held elements are there
    /// already).
    fn replay(&mut self) {
        self.held = None;
        let mut pending = core::mem::take(&mut self.pending);
        for item in pending.drain(..) {
            self.skeleton(item);
        }
        self.pending = pending;
    }

    /// Adds a buffered item as [`Builder::node_event`] adds its event: a
    /// token, a warning to the next element, an error as its error node.
    fn skeleton(&mut self, item: Pend<'a>) {
        match item {
            Pend::Token(kind, span) => self.token(kind, span),
            Pend::Warn(s) => self.attach(s, false),
            Pend::Err(e) => {
                let (s, span) = *e;
                if self.open(SyntaxKind::Error) {
                    self.attach(s, true);
                    self.token(SyntaxKind::ErrorText, span);
                    self.close();
                }
            }
            Pend::Event(e) => self.node_event(*e, false),
        }
    }

    /// Adds one event; `adopt`: open its node, then replay the buffer into
    /// it before its own tokens.
    fn node_event(&mut self, event: Event<'a>, adopt: bool) {
        let span = event.span();
        let extra = extra_of(&event);
        match event.into_kind() {
            EventKind::StreamStart => {
                if self.kinds.len() != 1 {
                    self.malformed = true;
                }
            }
            EventKind::StreamEnd => {
                // (A full arena closes nothing: `finish` pops what is
                // open. `close` does nothing then, so looping on
                // it never ended when the arena filled inside an error
                // node.)
                while self.stack.len() > 1 && self.full_at.is_none() {
                    self.close();
                }
            }
            EventKind::DocumentStart { explicit } => {
                match self.held.take() {
                    Some(h) if adopt => self.adopt_held(h),
                    _ => {
                        self.open(SyntaxKind::Document);
                    }
                }
                if adopt {
                    self.replay();
                }
                if explicit {
                    self.token(SyntaxKind::DocumentStartMarker, span);
                }
            }
            EventKind::DocumentEnd { explicit } => {
                if explicit {
                    self.token(SyntaxKind::DocumentEndMarker, span);
                }
                self.close_kind(|k| k == SyntaxKind::Document);
            }
            EventKind::MappingStart { style, .. } => {
                let kind = match style {
                    CollectionStyle::Block => SyntaxKind::BlockMapping,
                    CollectionStyle::Flow => SyntaxKind::FlowMapping,
                    CollectionStyle::FlowPair => SyntaxKind::FlowPair,
                };
                self.open_node(kind, extra, adopt);
                if !span.is_empty() {
                    self.token(SyntaxKind::FlowMappingOpen, span);
                }
            }
            EventKind::SequenceStart { style, .. } => {
                let kind = match style {
                    CollectionStyle::Flow => SyntaxKind::FlowSequence,
                    CollectionStyle::Block | CollectionStyle::FlowPair => SyntaxKind::BlockSequence,
                };
                self.open_node(kind, extra, adopt);
                if !span.is_empty() {
                    self.token(SyntaxKind::FlowSequenceOpen, span);
                }
            }
            EventKind::MappingEnd => {
                if !span.is_empty() {
                    self.token(SyntaxKind::FlowMappingClose, span);
                }
                self.close_kind(|k| {
                    matches!(
                        k,
                        SyntaxKind::BlockMapping | SyntaxKind::FlowMapping | SyntaxKind::FlowPair
                    )
                });
            }
            EventKind::SequenceEnd => {
                if !span.is_empty() {
                    self.token(SyntaxKind::FlowSequenceClose, span);
                }
                self.close_kind(|k| {
                    matches!(k, SyntaxKind::BlockSequence | SyntaxKind::FlowSequence)
                });
            }
            EventKind::Scalar { style, .. } => {
                self.open_node(SyntaxKind::Scalar, extra, adopt);
                let kind = match style {
                    ScalarStyle::Plain => SyntaxKind::PlainScalar,
                    ScalarStyle::SingleQuoted => SyntaxKind::SingleQuotedScalar,
                    ScalarStyle::DoubleQuoted => SyntaxKind::DoubleQuotedScalar,
                    ScalarStyle::Literal => SyntaxKind::LiteralScalar,
                    ScalarStyle::Folded => SyntaxKind::FoldedScalar,
                };
                self.token(kind, span);
                self.close();
            }
            EventKind::Alias => {
                self.open(SyntaxKind::Alias);
                self.token(SyntaxKind::AliasName, span);
                self.close();
            }
            EventKind::Directive(d) => self.token(directive_kind(d), span),
            EventKind::Trivia(t) => self.token(trivia_kind(t), span),
            EventKind::Warning(d) => self.diagnose(*d),
            EventKind::Error(d) => {
                if self.open(SyntaxKind::Error) {
                    self.diagnose(*d);
                    self.token(SyntaxKind::ErrorText, span);
                    self.close();
                }
            }
        }
    }

    /// Keeps at most `max + 1` diagnostics (plus the first error), counting
    /// the rest (`Limits::max_diagnostics`).
    pub(super) const fn max_diagnostics(mut self, max: usize) -> Self {
        self.keep = max.saturating_add(1);
        self
    }

    /// Buffers held directives instead (the old way, for comparison).
    #[cfg(test)]
    pub(super) const fn buffered(mut self) -> Self {
        self.hold = false;
        self
    }

    /// Opens a YAML node, records its extra, and adopts the buffer.
    fn open_node(&mut self, kind: SyntaxKind, extra: Option<Extra>, adopt: bool) {
        let index = self.kinds.len();
        if !self.open(kind) {
            return;
        }
        if let Some(extra) = extra {
            self.extras.push((as_u32(index), extra));
        }
        if adopt {
            self.replay();
        }
    }

    /// Records a diagnostic for the next element (or, for an error, the
    /// `Error` node just opened).
    /// (admitted on arrival, [`Builder::push`].)
    fn diagnose(&mut self, d: Diagnostic) {
        let error = d.severity == oneyaml_core::Severity::Error;
        self.attach(Stored::full(d), error);
    }

    /// Records a stored diagnostic, as [`Builder::diagnose`].
    fn attach(&mut self, d: Stored, error: bool) {
        let at = if error {
            self.kinds.len().saturating_sub(1)
        } else {
            self.kinds.len()
        };
        self.diagnostics.push((as_u32(at), d));
    }

    /// Room for one more element; otherwise the build stops here.
    fn room(&mut self) -> bool {
        if self.full_at.is_some() {
            return false;
        }
        if self.kinds.len() >= self.cap {
            self.full_at = Some(self.pos);
            return false;
        }
        true
    }

    /// Opens a node; `false` when the arena is full.
    fn open(&mut self, kind: SyntaxKind) -> bool {
        if !self.room() {
            return false;
        }
        let index = as_u32(self.kinds.len());
        if kind == SyntaxKind::Document && self.stack.len() == 1 {
            self.documents = self.documents.saturating_add(1);
        }
        self.kinds.push(kind);
        self.parents
            .push(self.stack.last().copied().unwrap_or(NONE));
        self.words.push(index);
        self.stack.push(index);
        true
    }

    /// Closes the innermost node (never the stream while building).
    fn close(&mut self) {
        if self.full_at.is_some() || self.stack.len() <= 1 {
            if self.full_at.is_none() {
                self.malformed = true;
            }
            return;
        }
        self.pop();
    }

    fn pop(&mut self) {
        if let Some(index) = self.stack.pop() {
            let end = as_u32(self.kinds.len());
            if let Some(word) = self.words.get_mut(index as usize) {
                *word = end;
            }
        }
    }

    /// Closes the innermost node, which should be of a kind `fits`.
    fn close_kind(&mut self, fits: impl Fn(SyntaxKind) -> bool) {
        if self.full_at.is_some() {
            return;
        }
        let top = self.stack.last().and_then(|i| self.kinds.get(*i as usize));
        if !top.is_some_and(|k| fits(*k)) {
            self.malformed = true;
        }
        self.close();
    }

    /// Adds a token covering `span`.
    fn token(&mut self, kind: SyntaxKind, span: Span) {
        if span.start != self.pos || span.end < span.start {
            self.malformed = true;
        }
        if !self.room() {
            return;
        }
        // Starts never decrease, so spans derived from them never invert.
        let start = span.start.max(self.last_start);
        self.last_start = start;
        self.pos = self.pos.max(span.end);
        let wide = u64::try_from(start).unwrap_or(u64::MAX);
        let index = as_u32(self.kinds.len());
        while self.high < wide / WRAP {
            self.wraps.push(index);
            self.high = self.high.saturating_add(1);
        }
        self.kinds.push(kind);
        self.parents
            .push(self.stack.last().copied().unwrap_or(NONE));
        self.words.push(u32::try_from(wide % WRAP).unwrap_or(0));
    }

    /// Completes the tree.
    pub(super) fn finish(mut self) -> Parts {
        if self.buffering() {
            self.replay();
        }
        let stop = match self.full_at {
            Some(at) => Some((at, true)),
            None if self.pos < self.source_len => {
                self.malformed = true;
                Some((self.pos, false))
            }
            None => None,
        };
        // Past this point the headroom is used: no more `room` checks.
        self.cap = usize::MAX;
        self.full_at = None;
        while self.stack.len() > 1 {
            self.pop();
        }
        if let Some((at, full)) = stop {
            self.pos = at;
            self.last_start = self.last_start.min(at);
            if full {
                let d = Diagnostic::error(
                    TREE_CAPACITY_EXCEEDED,
                    "the input has more elements than a syntax tree can index",
                    Label::new(Span::new(at, self.source_len), ""),
                )
                .with_help(
                    "a syntax tree holds at most 4,294,967,291 elements (nodes, tokens, and \
                     trivia); the rest of the input is kept as skipped text. Split the input \
                     into smaller documents or streams",
                );
                self.open(SyntaxKind::Error);
                self.diagnose(d);
                self.token(SyntaxKind::ErrorText, Span::empty(at));
                self.pop();
            }
            if at < self.source_len {
                self.token(SyntaxKind::Skipped, Span::new(at, self.source_len));
            }
        }
        if self.malformed {
            let at = self.kinds.len();
            let label = Label::new(Span::empty(self.pos.min(self.source_len)), "");
            let d = match self.origin {
                Origin::Caller => Diagnostic::error(
                    INVALID_EVENT_STREAM,
                    "the event stream passed in violates the event contract",
                    label,
                )
                .with_help(
                    "the input stream was invalid (not a bug in oneyaml): pass the parser's \
                     events unmodified, in order, and all of them; the tree is built without \
                     panicking but may not round-trip",
                ),
                Origin::Parser => Diagnostic::error(
                    INTERNAL_ERROR,
                    "internal error: the parser's event stream violates the event contract",
                    label,
                )
                .with_help("this is a bug in oneyaml; please report it with the input"),
            };
            self.diagnostics.push((as_u32(at), Stored::full(d)));
        }
        self.pop();
        let mut parts = Parts {
            kinds: self.kinds,
            parents: self.parents,
            words: self.words,
            wraps: self.wraps,
            extras: self.extras,
            diagnostics: self.diagnostics,
            documents: self.documents,
            suppressed: self.suppressed,
        };
        // The tree keeps exactly what it holds.
        parts.kinds.shrink_to_fit();
        parts.parents.shrink_to_fit();
        parts.words.shrink_to_fit();
        parts.wraps.shrink_to_fit();
        parts.extras.shrink_to_fit();
        parts.diagnostics.shrink_to_fit();
        parts
    }
}

/// An index known to fit (the arena never exceeds `u32::MAX` elements).
fn as_u32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(NONE)
}

/// The owner an event that starts a buffer waits for.
fn starts_buffer(event: &Event<'_>) -> Option<Owner> {
    match event.kind() {
        EventKind::Trivia(TriviaKind::Anchor | TriviaKind::Tag | TriviaKind::BlockScalarHeader) => {
            Some(Owner::Node)
        }
        EventKind::Directive(_) => Some(Owner::Document),
        _ => None,
    }
}

/// What a node event carries beyond its tokens, if anything.
fn extra_of(event: &Event<'_>) -> Option<Extra> {
    let tag_prefix = event.properties().and_then(|p| p.tag_prefix);
    let block = event.block_header();
    (tag_prefix.is_some() || block.is_some()).then_some(Extra { tag_prefix, block })
}

fn trivia_kind(t: TriviaKind) -> SyntaxKind {
    match t {
        TriviaKind::ByteOrderMark => SyntaxKind::ByteOrderMark,
        TriviaKind::Whitespace => SyntaxKind::Whitespace,
        TriviaKind::LineBreak => SyntaxKind::LineBreak,
        TriviaKind::Comment => SyntaxKind::Comment,
        TriviaKind::Indicator(Indicator::SequenceEntry) => SyntaxKind::SequenceEntry,
        TriviaKind::Indicator(Indicator::MappingKey) => SyntaxKind::MappingKey,
        TriviaKind::Indicator(Indicator::MappingValue) => SyntaxKind::MappingValue,
        TriviaKind::Indicator(Indicator::FlowEntry) => SyntaxKind::FlowEntry,
        TriviaKind::Anchor => SyntaxKind::Anchor,
        TriviaKind::Tag => SyntaxKind::Tag,
        TriviaKind::BlockScalarHeader => SyntaxKind::BlockScalarHeader,
        TriviaKind::DocumentEndMarker => SyntaxKind::StrayDocumentEndMarker,
        TriviaKind::Skipped => SyntaxKind::Skipped,
    }
}
