//! Flow collections (YAML 1.2.2 §7.4): flow sequences, flow mappings, and
//! single-pair mappings inside flow sequences ([`CollectionStyle::FlowPair`],
//! productions 150-153).
//!
//! A flow collection opened by a block node head is parsed by a [`Flow`]
//! sub-machine with its own explicit frame stack, one token per
//! step, while the main machine is in `State::Flow`. Its state lives in
//! [`FlowState`] between steps.
//!
//! # Bounded lookahead
//!
//! Two decisions are made only after events that precede them in the
//! stream have been produced, so those events are **held** until the
//! decision:
//!
//! * **Pair starts.** A flow sequence entry may turn out to be the key of
//!   a single-pair mapping (`[a: b]`); its zero-width `FlowPair` start goes
//!   before the key (C4) but is known only at the `:`. A *reserved* slot
//!   is held in its place.
//! * **The root.** The whole collection may turn out to be a block
//!   mapping's implicit key (`[a, b]: c`, production 155), which changes
//!   the events before it (a block `MappingStart`, where earlier
//!   properties go) or turns it into an error reported before it.
//!
//! Both are implicit keys, so both are decided within one line and 1024
//! characters (productions 153-155): a reservation still open when a line
//! break is crossed, or when the scan is more than [`KEY_WINDOW`] bytes
//! past the key's start (1024 characters of up to 4 bytes each), can no
//! longer become a (valid) key, and is resolved as "not a key" at once.
//! The hold therefore never covers more than one line or 4096 bytes of
//! source, whatever the collection's size, and events past it stream
//! through. A single-line key between 1025 and 4096 bytes is still held
//! and reported exactly as before; one longer than that has already been
//! emitted as a node when its `:` is seen, and the `:` is reported after
//! it (timing convention T3 in the parser module docs).

use std::collections::{BTreeMap, VecDeque};

use oneyaml_core::code::{
    DEPTH_LIMIT_EXCEEDED, IMPLICIT_KEY_TOO_LONG, INVALID_INDENTATION, MISSING_FLOW_SEPARATOR,
    MULTILINE_IMPLICIT_KEY, UNEXPECTED_FLOW_INDICATOR, UNTERMINATED_FLOW_COLLECTION,
};
use oneyaml_core::{Code, Span};

use super::block::HeadMode;
use super::scan::{
    IMPLICIT_KEY_MAX_CHARS, TagPrefix, any_marker_at, blank_or_end, break_len, byte, char_at,
    comment_at, is_flow_indicator, key_too_long, line_end, skip_white,
};
use super::{Item, Out, Parser};
use crate::event::{CollectionStyle, Event, EventKind, Indicator, Properties, TriviaKind};

/// How far past an implicit key's start, in bytes, the key may still be
/// valid: 1024 characters (productions 154-155) of at most 4 bytes each.
/// Beyond it `key_too_long` is certainly true, so a held decision can be
/// made without counting characters.
pub(crate) const KEY_WINDOW: usize = IMPLICIT_KEY_MAX_CHARS.saturating_mul(4);

/// Which collection a frame is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Seq,
    Map,
    /// A single-pair mapping inside a flow sequence.
    Pair,
}

/// The node slot a frame owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slot {
    /// A flow sequence entry; `line` is the line it starts on and `at` the
    /// position of its reserved pair start in the buffer.
    Entry { at: usize, pos: usize, line: usize },
    /// A mapping key (after `?` when `explicit`).
    Key,
    /// A mapping value.
    Value,
}

/// Where a frame is (`ns-s-flow-seq-entries`, production 138;
/// `ns-s-flow-map-entries`, production 141).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    /// After `[`, `{`, or `,`: an entry or the closing bracket.
    Entry,
    /// A node is owed (or is open, as the frame above).
    Node(Slot),
    /// A sequence entry is complete; a `:` on its line makes it a pair key.
    /// `peak`: the depth of the entry's deepest collection (0 for none),
    /// which grows by one if it becomes a pair key.
    EntryDone {
        at: usize,
        pos: usize,
        line: usize,
        json: bool,
        peak: usize,
    },
    /// A key is complete: `:` or the end of the entry.
    AfterKey { json: bool },
    /// An entry is complete: `,` or the closing bracket.
    AfterEntry,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Frame {
    pub(super) kind: Kind,
    pub(super) state: State,
    /// The depth (block and flow) of the deepest collection in this
    /// frame's subtree so far, itself included.
    pub(super) peak: usize,
    /// Where the frame opened: its `[` or `{` (a pair: its key's start),
    /// for the diagnostics of an unclosed collection.
    pub(super) open: usize,
}

/// A slot of the hold (see the module docs).
#[derive(Clone, Debug)]
enum Held<'a> {
    Item(Item<'a>),
    /// A pair start not yet decided.
    Reserved,
    /// A reservation decided against, or a released slot.
    Empty,
}

/// The open flow collection, kept on the [`Parser`] between steps. Its
/// buffers keep their capacity from one collection to the next.
#[derive(Debug, Default)]
pub(super) struct FlowState<'a> {
    /// Events not yet released to the output queue, oldest first.
    hold: VecDeque<Held<'a>>,
    /// The absolute index of `hold[0]`; a reservation is named by its
    /// absolute index.
    hold_base: usize,
    /// Open pair reservations, oldest first: absolute index and the start
    /// of the entry (the would-be key).
    pairs: VecDeque<(usize, usize)>,
    /// End of the last event produced.
    pub(super) out_pos: usize,
    /// Scan position; `out_pos..pos` is pending trivia.
    pub(super) pos: usize,
    pub(super) line_start: usize,
    pub(super) line_indent: usize,
    /// Lines need more leading spaces than this (`s-flow-line-prefix(n)`,
    /// production 69); `None` at the document level.
    pub(super) min: Option<usize>,
    pub(super) stack: Vec<Frame>,
    /// Properties emitted but not yet attached to a node.
    pub(super) props: Properties,
    /// A node is done at the top level: the collection is complete.
    pub(super) finished: bool,
    /// Start of the line the collection's opening bracket is on.
    pub(super) first_line: usize,
    /// The collection's opening bracket.
    pub(super) open: usize,
    /// Set when the collection failed as a multi-line implicit key (the
    /// C2SP rule in `close`): the diagnostic's secondary label points at
    /// its opening bracket.
    pub(super) multiline_key: bool,
    /// The depth of the deepest collection opened so far.
    pub(super) peak: usize,
    /// The head this collection is the content of.
    pub(super) mode: Option<HeadMode>,
    /// The head's property events, emitted before the collection.
    pub(super) head: Vec<Event<'a>>,
    /// Where the head starts (the main parser's position).
    pub(super) head_start: usize,
    /// The head's column, the indentation of a mapping it may start.
    pub(super) head_col: usize,
    /// Properties from earlier lines conflict with the head's own; an
    /// error unless the collection is a key.
    pub(super) conflicting: bool,
    /// The root decision was made early ("not a key"): events stream.
    pub(super) released: bool,
}

impl<'a> FlowState<'a> {
    /// Prepares for the collection whose bracket is at `start`, keeping the
    /// buffers' capacity.
    pub(super) fn reset(&mut self, start: usize, line_start: usize, line_indent: usize) {
        self.hold.clear();
        self.pairs.clear();
        self.stack.clear();
        self.out_pos = start;
        self.pos = start;
        self.line_start = line_start;
        self.line_indent = line_indent;
        self.first_line = line_start;
        self.open = start;
        self.multiline_key = false;
        self.finished = false;
        self.peak = 0;
        self.released = false;
    }

    /// Total buffer capacity, in elements (see `Events::retained_capacity`).
    pub(super) fn capacity(&self) -> usize {
        self.hold
            .capacity()
            .saturating_add(self.pairs.capacity())
            .saturating_add(self.stack.capacity())
            .saturating_add(self.head.capacity())
    }

    /// Drops everything held (the collection is abandoned).
    pub(super) fn abandon(&mut self) {
        self.hold.clear();
        self.pairs.clear();
        self.stack.clear();
        self.mode = None;
    }

    /// The collection's own start event, while it is still held.
    pub(super) fn first_event(&mut self) -> Option<&mut Event<'a>> {
        self.hold.iter_mut().find_map(|h| match h {
            Held::Item(Item::Event(e)) => Some(e),
            _ => None,
        })
    }

    /// Whether any event is held.
    pub(super) fn holds_events(&self) -> bool {
        self.hold
            .iter()
            .any(|h| matches!(h, Held::Item(Item::Event(_))))
    }

    /// Decides the reservation `at`: a pair start, or none.
    fn resolve(&mut self, at: usize, event: Option<Event<'a>>) {
        let slot = at
            .checked_sub(self.hold_base)
            .and_then(|k| self.hold.get_mut(k));
        if let Some(slot @ Held::Reserved) = slot {
            *slot = event.map_or(Held::Empty, |e| Held::Item(Item::Event(e)));
        }
        if self.pairs.back().map(|p| p.0) == Some(at) {
            self.pairs.pop_back();
        }
    }

    /// A line break was crossed: no open reservation can become a pair
    /// (`ns-flow-pair-entry` keys are single-line, production 151).
    fn new_line(&mut self) {
        while let Some((at, _)) = self.pairs.pop_front() {
            self.resolve(at, None);
        }
    }

    /// Resolves the reservations whose key would be too long already
    /// ([`KEY_WINDOW`]).
    pub(super) fn expire(&mut self) {
        while let Some(&(at, key)) = self.pairs.front() {
            if self.pos.saturating_sub(key) <= KEY_WINDOW {
                break;
            }
            self.pairs.pop_front();
            self.resolve(at, None);
        }
    }

    /// Releases the decided prefix of the hold to `out`.
    pub(super) fn drain(&mut self, out: &mut Out<'a>) {
        while let Some(front) = self.hold.front() {
            if matches!(front, Held::Reserved) {
                break;
            }
            if let Some(Held::Item(item)) = self.hold.pop_front() {
                out.item(item);
            }
            self.hold_base = self.hold_base.saturating_add(1);
        }
    }

    /// Releases everything held, dropping undecided reservations.
    pub(super) fn drain_all(&mut self, out: &mut Out<'a>) {
        self.new_line();
        while let Some(held) = self.hold.pop_front() {
            if let Held::Item(item) = held {
                out.item(item);
            }
            self.hold_base = self.hold_base.saturating_add(1);
        }
    }
}

pub(super) type Stop = (Code, &'static str);

/// Why [`Flow::run`] stopped.
pub(super) enum Pause {
    /// A syntax error.
    Failed(Stop),
    /// The collection is complete.
    Finished,
    /// The collection cannot be a (valid) implicit key: the root can be
    /// released (see the module docs).
    NotAKey,
    /// The output queue is full.
    Full,
}

/// One step's view of the flow sub-machine: the parser's configuration,
/// the flow state, and the output queue.
pub(super) struct Flow<'s, 'a> {
    pub(super) src: &'a str,
    /// `%TAG` handles of the current document.
    pub(super) tags: &'s BTreeMap<&'a str, TagPrefix>,
    /// Open block collections around the flow collection.
    block_depth: usize,
    max_depth: usize,
    pub(super) max_scalar_len: usize,
    st: &'s mut FlowState<'a>,
    out: &'s mut Out<'a>,
}

impl<'a> core::ops::Deref for Flow<'_, 'a> {
    type Target = FlowState<'a>;

    fn deref(&self) -> &FlowState<'a> {
        self.st
    }
}

impl<'a> core::ops::DerefMut for Flow<'_, 'a> {
    fn deref_mut(&mut self) -> &mut FlowState<'a> {
        self.st
    }
}

/// A `:`, `?` followed by white space, a break, the end, or a flow
/// indicator.
pub(super) fn separated(src: &str, i: usize) -> bool {
    blank_or_end(src, i) || char_at(src, i).is_some_and(is_flow_indicator)
}

impl<'s, 'a> Flow<'s, 'a> {
    /// The flow sub-machine of `p` for one step.
    pub(super) fn new(p: &'s mut Parser<'a>) -> Self {
        Flow {
            src: p.src,
            tags: &p.tags,
            block_depth: p.stack.len().saturating_sub(1),
            max_depth: p.max_depth,
            max_scalar_len: p.max_scalar_len,
            st: &mut p.flow,
            out: &mut p.out,
        }
    }

    // ------------------------------------------------------------ emission

    /// Runs the machine until it pauses (one loop over the explicit
    /// stack).
    pub(super) fn run(&mut self) -> Pause {
        loop {
            if let Err(stop) = self.step() {
                return Pause::Failed(stop);
            }
            let st = &mut *self.st;
            if st.finished {
                return Pause::Finished;
            }
            if !st.released
                && (st.line_start != st.first_line
                    || st.pos.saturating_sub(st.head_start) > KEY_WINDOW)
            {
                return Pause::NotAKey;
            }
            st.expire();
            if st.released {
                st.drain(self.out);
            }
            if self.out.is_full() {
                return Pause::Full;
            }
        }
    }

    /// Queues an item: straight to the output once the root is decided and
    /// nothing is held, else into the hold.
    fn emit(&mut self, item: Item<'a>) {
        if self.st.released && self.st.hold.is_empty() {
            self.out.item(item);
        } else {
            self.st.hold.push_back(Held::Item(item));
        }
    }

    pub(super) fn push(&mut self, kind: EventKind, start: usize, end: usize) {
        self.emit(Item::Event(Event::new(kind, Span::new(start, end))));
        self.out_pos = end;
    }

    /// A zero-width event before the pending trivia.
    pub(super) fn zero(&mut self, kind: EventKind) {
        let at = self.out_pos;
        self.push(kind, at, at);
    }

    /// Queues the pending trivia.
    pub(super) fn flush(&mut self) {
        let (from, to) = (self.out_pos, self.pos);
        if from < to {
            self.emit(Item::Trivia(from, to));
        }
        self.out_pos = to;
    }

    /// Reserves the slot of a possible pair start; returns its name.
    fn reserve(&mut self) -> usize {
        let st = &mut *self.st;
        let at = st.hold_base.saturating_add(st.hold.len());
        st.hold.push_back(Held::Reserved);
        st.pairs.push_back((at, st.pos));
        at
    }

    /// The line bookkeeping moved to a new line.
    pub(super) fn crossed_line(&mut self) {
        self.st.new_line();
    }

    /// Flushes, then emits an event covering `pos..end` and consumes it.
    pub(super) fn token(&mut self, kind: EventKind, end: usize) {
        self.flush();
        let start = self.pos;
        self.push(kind, start, end);
        self.pos = end;
    }

    fn indicator(&mut self, indicator: Indicator) {
        let end = self.pos.saturating_add(1);
        self.token(EventKind::Trivia(TriviaKind::Indicator(indicator)), end);
    }

    // ------------------------------------------------------------ trivia

    /// Scans separation (`s-separate(n,c)` in flow context, production 80)
    /// as pending trivia and checks the line prefix of a new line.
    fn trivia(&mut self) -> Result<(), Stop> {
        let src = self.src;
        let mut new_line = false;
        // Bytes of misplaced byte order marks among the line's leading
        // spaces: skipped as if absent, so spaces after them
        // still count as indentation.
        let mut lead_marks = 0usize;
        loop {
            let i = self.pos;
            let lead = self
                .line_start
                .saturating_add(self.line_indent)
                .saturating_add(lead_marks);
            let end = match byte(src, i) {
                Some(b' ' | b'\t') => {
                    let end = skip_white(src, i);
                    if i == lead {
                        let spaces = src
                            .get(i..end)
                            .map_or(0, |r| r.bytes().take_while(|b| *b == b' ').count());
                        self.line_indent = self.line_indent.saturating_add(spaces);
                    }
                    end
                }
                Some(b'\n' | b'\r') => {
                    let end = i.saturating_add(break_len(src, i));
                    self.line_start = end;
                    self.line_indent = 0;
                    lead_marks = 0;
                    new_line = true;
                    end
                }
                Some(b'#') if comment_at(src, i) => {
                    // A mark inside the comment (no `nb-char`,
                    // production 27) is reported right after it.
                    let end = line_end(src, i);
                    let text = src.get(i..end).unwrap_or("");
                    if text.contains('\u{feff}') {
                        self.pos = end;
                        self.flush();
                        for (k, _) in text.match_indices('\u{feff}') {
                            let at = i.saturating_add(k);
                            let d = super::misplaced_bom_diagnostic(
                                at,
                                at.saturating_add('\u{feff}'.len_utf8()),
                            );
                            self.zero(EventKind::Error(d));
                        }
                    }
                    end
                }
                _ if src.get(i..).is_some_and(|r| r.starts_with('\u{feff}')) => {
                    // §5.2, production 202: never a document prefix here.
                    let end = i.saturating_add('\u{feff}'.len_utf8());
                    if i == lead {
                        lead_marks = lead_marks.saturating_add(end.saturating_sub(i));
                    }
                    self.flush();
                    self.push(
                        EventKind::Error(super::misplaced_bom_diagnostic(i, end)),
                        i,
                        end,
                    );
                    self.pos = end;
                    continue;
                }
                _ => break,
            };
            // Pending trivia is queued later as one lazy run (`flush`).
            self.pos = end;
        }
        if new_line {
            self.crossed_line();
        }
        if self.pos >= src.len() {
            return Err((
                UNTERMINATED_FLOW_COLLECTION,
                "flow collection is not closed",
            ));
        }
        if new_line {
            if self.pos == self.line_start && any_marker_at(src, self.pos) {
                return Err((
                    UNTERMINATED_FLOW_COLLECTION,
                    "document marker inside a flow collection",
                ));
            }
            if self.min.is_some_and(|n| self.line_indent <= n) {
                return Err((
                    INVALID_INDENTATION,
                    "flow content must be indented more than its block parent",
                ));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ dispatch

    fn top(&self) -> Option<Frame> {
        self.stack.last().copied()
    }

    /// The depth a frame pushed now would have: the open block collections
    /// plus the open flow frames plus itself.
    fn next_depth(&self) -> usize {
        self.block_depth
            .saturating_add(self.stack.len())
            .saturating_add(1)
    }

    /// `Limits::max_depth` allows a collection at `depth`.
    pub(super) fn check_depth(&self, depth: usize) -> Result<(), Stop> {
        if depth <= self.max_depth {
            Ok(())
        } else {
            Err((DEPTH_LIMIT_EXCEEDED, "collections are nested too deeply"))
        }
    }

    /// Pushes a frame after checking its depth; call before emitting its
    /// start event.
    pub(super) fn push_frame(&mut self, kind: Kind, state: State) -> Result<(), Stop> {
        let depth = self.next_depth();
        self.check_depth(depth)?;
        let open = self.pos;
        self.stack.push(Frame {
            kind,
            state,
            peak: depth,
            open,
        });
        self.peak = self.peak.max(depth);
        Ok(())
    }

    /// Pops the top frame, folding its peak into its parent's; returns it.
    pub(super) fn pop_frame(&mut self) -> Option<Frame> {
        let frame = self.stack.pop()?;
        if let Some(parent) = self.stack.last_mut() {
            parent.peak = parent.peak.max(frame.peak);
        }
        Some(frame)
    }

    fn set(&mut self, state: State) {
        if let Some(top) = self.stack.last_mut() {
            top.state = state;
        }
    }

    /// One step of the machine.
    pub(super) fn step(&mut self) -> Result<(), Stop> {
        let Some(top) = self.top() else {
            // The opening bracket of the whole collection.
            return self.node();
        };
        self.trivia()?;
        let src = self.src;
        let i = self.pos;
        let c = byte(src, i);
        let sep = separated(src, i.saturating_add(1));
        let closer = if top.kind == Kind::Map { b'}' } else { b']' };
        match (top.kind, top.state) {
            (_, State::Node(_)) => self.node(),
            (Kind::Seq | Kind::Map, State::Entry) if c == Some(closer) => self.close(),
            (_, State::Entry) if matches!(c, Some(b',' | b']' | b'}')) => {
                Err((UNEXPECTED_FLOW_INDICATOR, "expected an entry"))
            }
            // ns-flow-pair (production 150) / ns-flow-map-explicit-entry (143).
            (kind, State::Entry) if c == Some(b'?') && sep => {
                self.flush();
                if kind == Kind::Seq {
                    self.push_frame(Kind::Pair, State::Node(Slot::Key))?;
                    self.zero(EventKind::mapping_start(CollectionStyle::FlowPair));
                } else {
                    self.set(State::Node(Slot::Key));
                }
                self.indicator(Indicator::MappingKey);
                Ok(())
            }
            // An empty key before `:` (c-ns-flow-map-empty-key-entry, 146).
            (kind, State::Entry) if c == Some(b':') && sep => {
                self.flush();
                if kind == Kind::Seq {
                    self.push_frame(Kind::Pair, State::Node(Slot::Value))?;
                    self.zero(EventKind::mapping_start(CollectionStyle::FlowPair));
                } else {
                    self.set(State::Node(Slot::Value));
                }
                self.empty();
                self.indicator(Indicator::MappingValue);
                Ok(())
            }
            (Kind::Seq, State::Entry) => {
                // Reserve the start of a possible pair (C4: it starts late,
                // right before the key's first event).
                self.flush();
                let at = self.reserve();
                let (pos, line) = (self.pos, self.line_start);
                self.set(State::Node(Slot::Entry { at, pos, line }));
                Ok(())
            }
            (_, State::Entry) => {
                self.set(State::Node(Slot::Key));
                Ok(())
            }
            (
                _,
                State::EntryDone {
                    at,
                    pos,
                    line,
                    json,
                    peak,
                },
            ) => {
                let pair = c == Some(b':') && self.line_start == line && (json || sep);
                if pair && key_too_long(src, pos, i) {
                    // c-s-implicit-json-key / ns-s-implicit-yaml-key (154-155).
                    return Err((
                        IMPLICIT_KEY_TOO_LONG,
                        "an implicit key must be at most 1024 characters",
                    ));
                }
                if pair {
                    // The pair nests its key one level deeper.
                    if peak > 0 {
                        self.check_depth(peak.saturating_add(1))?;
                    }
                    self.push_frame(Kind::Pair, State::Node(Slot::Value))?;
                    let key_peak = peak.saturating_add(1);
                    if let Some(top) = self.stack.last_mut() {
                        top.peak = top.peak.max(key_peak);
                    }
                    self.peak = self.peak.max(key_peak);
                    // ns-flow-pair-entry (production 151): single line.
                    let start = EventKind::mapping_start(CollectionStyle::FlowPair);
                    self.st
                        .resolve(at, Some(Event::new(start, Span::empty(pos))));
                    self.indicator(Indicator::MappingValue);
                } else {
                    self.st.resolve(at, None);
                    self.set(State::AfterEntry);
                }
                Ok(())
            }
            (kind, State::AfterKey { json }) => {
                if c == Some(b':') && (json || sep) {
                    self.set(State::Node(Slot::Value));
                    self.indicator(Indicator::MappingValue);
                    return Ok(());
                }
                if !matches!(c, Some(b',' | b']' | b'}')) {
                    return Err((
                        MISSING_FLOW_SEPARATOR,
                        "expected `:`, `,`, or the end of the collection",
                    ));
                }
                // No value (placement rule 3: right after the key).
                self.empty();
                if kind == Kind::Pair {
                    self.close_pair();
                } else {
                    self.set(State::AfterEntry);
                }
                Ok(())
            }
            (_, State::AfterEntry) => match c {
                Some(b',') => {
                    self.set(State::Entry);
                    self.indicator(Indicator::FlowEntry);
                    Ok(())
                }
                Some(x) if x == closer => self.close(),
                Some(b']' | b'}') => Err((UNEXPECTED_FLOW_INDICATOR, "mismatched closing bracket")),
                Some(b':') if top.kind == Kind::Seq => Err((
                    MULTILINE_IMPLICIT_KEY,
                    "an implicit key must be on one line with its `:`",
                )),
                _ => Err((
                    MISSING_FLOW_SEPARATOR,
                    "flow entries must be separated by `,`",
                )),
            },
        }
    }
}
