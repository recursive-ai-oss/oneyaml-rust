//! A flow collection as a block node or a block mapping's implicit key
//! (`c-s-implicit-json-key`, production 155, e.g. `[a, b]: c`): starting
//! the flow sub-machine, stepping it, and deciding the root (see the
//! bounded-lookahead notes in `flow.rs`).

use oneyaml_core::code::{
    DUPLICATE_PROPERTY, IMPLICIT_KEY_TOO_LONG, INVALID_INDENTATION, MISSING_MAPPING_COLON,
    UNTERMINATED_FLOW_COLLECTION,
};
use oneyaml_core::{Applicability, Code, Label, Replacement, Span, Suggestion};

use super::block::HeadMode;
use super::flow::{Flow, Kind, Pause, Slot, State as FlowStep};
use super::flow_node::attach_props;
use super::polish::{self, Place};
use super::scan::{byte, indicator_at, key_too_long, line_end, skip_white};
use super::{Frame, MapPhase, Parser, State};
use crate::event::{CollectionStyle, Event, EventKind, Indicator, Properties, TriviaKind};

fn conflict(a: &Properties, b: &Properties) -> bool {
    a.anchor.is_some() && b.anchor.is_some() || a.tag.is_some() && b.tag.is_some()
}

const TOO_LONG: &str = "an implicit key must be at most 1024 characters";

impl<'a> Parser<'a> {
    /// A head whose content is the flow collection at `start`; `head` holds
    /// the head's property events and `props` their meaning (the
    /// collection's own properties).
    pub(super) fn flow_head(
        &mut self,
        mode: HeadMode,
        head: Vec<Event<'a>>,
        props: Properties,
        start: usize,
    ) {
        let slot_open = matches!(mode, HeadMode::Node { .. });
        let min = match mode {
            HeadMode::Node { ctx, .. } => ctx.parent,
            HeadMode::Key { indent } => Some(indent),
        };
        // Properties from earlier lines go to the collection unless it turns
        // out to be a key; a conflict is only an error in that case.
        let conflicting = slot_open && conflict(&self.props, &props);
        let head_col = if self.fresh {
            self.line_indent
        } else {
            self.col()
        };
        let st = &mut self.flow;
        st.reset(start, self.line_start, self.line_indent);
        st.min = min;
        st.props = props;
        st.mode = Some(mode);
        st.head = head;
        st.head_start = self.pos;
        st.head_col = head_col;
        st.conflicting = conflicting;
        self.state = State::Flow;
    }

    /// Runs the flow sub-machine until the output queue is full, or it
    /// finishes or fails.
    pub(super) fn flow_step(&mut self) {
        // One loop over the flow machine's explicit stack.
        loop {
            match Flow::new(self).run() {
                Pause::Failed((code, message)) => return self.flow_failed(code, message),
                Pause::Finished => return self.flow_finished(),
                Pause::NotAKey => {
                    if !self.release_root() {
                        return;
                    }
                }
                Pause::Full => return,
            }
        }
    }

    /// Gives the head buffer back for reuse and drops the flow state.
    fn abandon_flow(&mut self) {
        self.flow.abandon();
        let head = core::mem::take(&mut self.flow.head);
        self.scratch = head;
    }

    /// Emits the head's events after the content before them (C4).
    fn commit_head(&mut self) {
        self.begin_content();
        let mut head = core::mem::take(&mut self.flow.head);
        self.commit(head.drain(..));
        self.flow.head = head;
    }

    /// The root is decided early: the collection is not a key (it spans
    /// lines or is too long for one). In a node slot, properties from
    /// earlier lines join the collection's own. `false` when that is an
    /// error (reported, flow abandoned).
    fn release_root(&mut self) -> bool {
        let node = matches!(self.flow.mode, Some(HeadMode::Node { .. }));
        if self.flow.conflicting {
            self.abandon_flow();
            self.fail(DUPLICATE_PROPERTY, "duplicate anchor or tag", true);
            return false;
        }
        if node {
            let extra = core::mem::take(&mut self.props);
            if let Some(first) = self.flow.first_event() {
                attach_props(first, extra);
            }
        }
        self.commit_head();
        self.flow.released = true;
        true
    }

    /// Takes over the flow machine's position once the collection is done.
    fn sync_after_flow(&mut self, end: usize) {
        let st = &mut self.flow;
        st.drain_all(&mut self.out);
        self.pos = end;
        self.emit_pos = end;
        self.fresh = false;
        self.tab_seen = false;
        self.line_start = self.flow.line_start;
        self.line_indent = self.flow.line_indent;
        let head = core::mem::take(&mut self.flow.head);
        self.scratch = head;
        self.flow.mode = None;
    }

    /// The collection closed without error.
    fn flow_finished(&mut self) {
        let src = self.src;
        let st = &self.flow;
        let Some(mode) = st.mode else {
            return self.abandon_flow();
        };
        let (end, peak, head_col) = (st.out_pos, st.peak, st.head_col);
        let colon = skip_white(src, end);
        let is_key = indicator_at(src, colon, b':');
        let slot_open = matches!(mode, HeadMode::Node { .. });
        if st.released {
            return self.finish_released(mode, end, is_key);
        }
        // A multi-line collection followed by `:` is rejected at its closing
        // bracket (`Flow::close`, the C2SP rule), so a key here is always on
        // one line; this checks that the two places cannot drift apart.
        // In release a drift is an in-band Y9000 error, not a key.
        if is_key && st.line_start != st.first_line {
            debug_assert!(false, "multi-line flow key passed Flow::close");
            self.abandon_flow();
            return self.fail_internal("a multi-line flow key passed `Flow::close`", slot_open);
        }
        if is_key && key_too_long(src, self.pos, colon) {
            self.abandon_flow();
            self.blame.set(Some(colon.saturating_add(1)));
            return self.fail(IMPLICIT_KEY_TOO_LONG, TOO_LONG, slot_open);
        }
        match mode {
            HeadMode::Key { indent } => {
                if !is_key {
                    self.abandon_flow();
                    return self.fail(MISSING_MAPPING_COLON, "a mapping key needs `:`", false);
                }
                self.commit_head();
                self.sync_after_flow(end);
                self.colon_tail(end, colon);
                self.set_key_value(indent);
            }
            HeadMode::Node { can_open, .. } if is_key && can_open => {
                // A new block mapping with this flow collection as its first
                // key; properties from earlier lines belong to the mapping.
                // The mapping nests the key one level deeper.
                if !self.depth_ok(peak.saturating_add(1)) {
                    self.abandon_flow();
                    self.fail_if_too_deep(peak.saturating_add(1));
                    return;
                }
                self.begin_content();
                let map_props = core::mem::take(&mut self.props);
                self.zero_node(EventKind::mapping_start(CollectionStyle::Block), map_props);
                self.stack.push(Frame::Map {
                    indent: head_col,
                    phase: MapPhase::Value,
                });
                self.commit_head();
                self.sync_after_flow(end);
                self.colon_tail(end, colon);
                self.set_key_value(head_col);
            }
            HeadMode::Node { .. } => {
                if !self.release_root() {
                    return;
                }
                self.sync_after_flow(end);
                self.child_done();
                if is_key {
                    // A complete flow node followed by a `:` that cannot
                    // start a mapping here: report the `:` (timing
                    // convention T3).
                    self.scan_trivia(false);
                    let (code, message) = self.collection_error();
                    self.fail(code, message, false);
                }
            }
        }
    }

    /// The collection closed after its root was released as "not a key":
    /// it is already emitted as a node (or, in a key slot, as the key), so
    /// a `:` after it (necessarily more than [`KEY_WINDOW`](super::flow::KEY_WINDOW) bytes after the
    /// head, as a multi-line key fails at its bracket) or a missing `:` is
    /// reported after it (timing convention T3).
    fn finish_released(&mut self, mode: HeadMode, end: usize, is_key: bool) {
        let key = Span::new(self.flow.head_start, end);
        self.sync_after_flow(end);
        match mode {
            HeadMode::Node { .. } => {
                self.child_done();
                if is_key {
                    self.scan_trivia(false);
                    self.fail(IMPLICIT_KEY_TOO_LONG, TOO_LONG, false);
                }
            }
            HeadMode::Key { .. } => {
                self.set_phase_value();
                self.scan_trivia(false);
                if is_key {
                    self.fail(IMPLICIT_KEY_TOO_LONG, TOO_LONG, true);
                } else {
                    // Timing convention T3: the error comes after the
                    // released collection, possibly lines later (even at the
                    // end of input), so a label marks the key itself.
                    self.fail_with(
                        MISSING_MAPPING_COLON,
                        "a mapping key needs `:`",
                        true,
                        |d| d.with_secondary(Label::new(key, "this key has no `:`")),
                    );
                }
            }
        }
    }

    /// White space before a key's `:` and the `:` itself.
    fn colon_tail(&mut self, end: usize, colon: usize) {
        if colon > end {
            self.push(EventKind::Trivia(TriviaKind::Whitespace), end, colon);
        }
        let c = EventKind::Trivia(TriviaKind::Indicator(Indicator::MappingValue));
        let after = colon.saturating_add(1);
        self.out.event(Event::new(c, Span::new(colon, after)));
        self.emit_pos = after;
        self.pos = after;
    }

    /// The flow machine stopped with an error: the valid prefix, then the
    /// error and its repairs (timing convention T5).
    fn flow_failed(&mut self, code: Code, message: &'static str) {
        let blame = self.flow.blame.take();
        self.blame.set(blame);
        // The C2SP rule: point at where the multi-line key starts.
        let key_open = self.flow.multiline_key.then_some(self.flow.open);
        let dedent = self.dedent(code);
        let unclosed = self.unclosed_flow(code, dedent);
        let message = dedent.map_or(message, |d| self.dedent_message(d));
        // Polish of a failed flow node at the scan position (a key's
        // text may end at its `:`).
        // A key position (a mapping key, or a sequence entry that may be a
        // pair's key) may end at its `:`; a value may not.
        let key = !matches!(
            self.flow.stack.last().map(|f| f.state),
            Some(FlowStep::Node(Slot::Value))
        );
        let place = Place {
            key,
            key_only: false,
            parent: None,
            flow: true,
            // A collection in a key position still needs its `:` after
            // the closing bracket, which is not read yet.
            sound: !matches!(self.flow.mode, Some(HeadMode::Key { .. })),
        };
        let polished = polish::content(self.src, code, self.flow.pos, place);
        let (at, line_start, repair) = {
            let mut f = Flow::new(self);
            f.flush();
            let repair = f.repairs();
            (f.pos, f.line_start, repair)
        };
        let Some(mode) = self.flow.mode else {
            return self.abandon_flow();
        };
        let slot_open = matches!(mode, HeadMode::Node { .. });
        if !self.flow.released {
            if self.flow.conflicting {
                self.abandon_flow();
                return self.fail(DUPLICATE_PROPERTY, "duplicate anchor or tag", true);
            }
            if !self.flow.holds_events() {
                // The collection itself could not open (its `[` or
                // `{` exceeds `max_depth`): nothing of it exists, so this is
                // a failed head like any other.
                self.abandon_flow();
                return self.fail(code, message, slot_open);
            }
            if !self.release_root() {
                return;
            }
        }
        self.sync_after_flow(at);
        self.line_start = line_start;
        self.flow_repair = repair;
        if slot_open {
            self.child_done();
        } else {
            self.set_phase_value();
        }
        self.fail_with(code, message, !slot_open, |d| {
            let d = match key_open {
                Some(open) => d.with_secondary(Label::new(
                    Span::new(open, open.saturating_add(1)),
                    "the multi-line key starts here",
                )),
                None => d,
            };
            let d = match polished {
                Some(p) => p.apply(d),
                None => d,
            };
            match unclosed {
                Some((label, fix, Some(help))) => {
                    d.with_secondary(label).with_suggestion(fix).with_help(help)
                }
                Some((label, fix, None)) => d.with_secondary(label).with_suggestion(fix),
                None => d,
            }
        });
    }

    /// How the less-indented line that stopped a flow collection
    /// (`INVALID_INDENTATION`) relates to it; `None` for other errors.
    fn dedent(&self, code: Code) -> Option<Dedent> {
        if code != INVALID_INDENTATION {
            return None;
        }
        let st = &self.flow;
        let open = st.stack.iter().filter(|f| f.kind != Kind::Pair).count();
        let own = match st.stack.iter().rev().find(|f| f.kind != Kind::Pair)?.kind {
            Kind::Seq => b']',
            _ => b'}',
        };
        Some(match byte(self.src, st.pos) {
            Some(c) if c == own => Dedent::Closer(c),
            // A bracket of the other kind closes nothing here: the
            // collection is still open at this line.
            Some(b']' | b'}') => Dedent::Open,
            _ if closes_on_line(self.src, st.pos, open) => Dedent::Closes,
            _ => Dedent::Open,
        })
    }

    /// The message for a less-indented line inside a flow collection. The
    /// plain message is all many readers see, so it tells a closing
    /// bracket or a line that is only short of indentation (common in
    /// JSON-style YAML) from a collection still open there (often a
    /// forgotten bracket).
    fn dedent_message(&self, dedent: Dedent) -> &'static str {
        let seq = self
            .flow
            .stack
            .iter()
            .rev()
            .find(|f| f.kind != Kind::Pair)
            .is_some_and(|f| f.kind == Kind::Seq);
        match (dedent, seq) {
            (Dedent::Closer(b']'), _) => {
                "the closing `]` of a flow sequence is not indented more than the sequence's block parent"
            }
            (Dedent::Closer(_), _) => {
                "the closing `}` of a flow mapping is not indented more than the mapping's block parent"
            }
            (Dedent::Closes, true) => {
                "this line of a flow sequence is not indented more than the sequence's block parent"
            }
            (Dedent::Closes, false) => {
                "this line of a flow mapping is not indented more than the mapping's block parent"
            }
            (Dedent::Open, true) => {
                "a flow sequence is still open at this line, which is not indented more than its block parent; a `]` may be missing before it"
            }
            (Dedent::Open, false) => {
                "a flow mapping is still open at this line, which is not indented more than its block parent; a `}` may be missing before it"
            }
        }
    }

    /// Polish for a flow collection that runs into the end of the
    /// input, a document marker, or a dedented line: a label at the
    /// innermost open bracket, and the closing brackets to insert after
    /// the last token (machine-applicable at the end of the input, where
    /// nothing else can close them). A dedented line that closes the
    /// collection itself is offered the indentation it lacks instead.
    fn unclosed_flow(
        &self,
        code: Code,
        dedent: Option<Dedent>,
    ) -> Option<(Label, Suggestion, Option<&'static str>)> {
        if code != UNTERMINATED_FLOW_COLLECTION && code != INVALID_INDENTATION {
            return None;
        }
        let st = &self.flow;
        let (open, kind) = st
            .stack
            .iter()
            .rev()
            .find(|f| f.kind != Kind::Pair)
            .map(|f| (f.open, f.kind))?;
        let (bracket, what) = if kind == Kind::Seq {
            ("`[`", "sequence")
        } else {
            ("`{`", "mapping")
        };
        if let (Some(Dedent::Closer(_) | Dedent::Closes), Some(min)) = (dedent, st.min) {
            // The collection is closed on this line: only its indentation
            // is short, by `min + 1 - line_indent` spaces.
            let missing = min.saturating_add(1).saturating_sub(st.line_indent).max(1);
            let fix = Suggestion::new(
                "indent the line",
                vec![Replacement::new(
                    Span::empty(st.line_start),
                    " ".repeat(missing),
                )],
                Applicability::MaybeIncorrect,
            );
            let label = Label::new(
                Span::new(open, open.saturating_add(1)),
                format!("the flow {what} opened here"),
            );
            let help = "every line of a flow collection, its closing bracket included, \
                        must be indented more than the block collection it is in";
            return Some((label, fix, Some(help)));
        }
        let closers: String = st
            .stack
            .iter()
            .rev()
            .filter_map(|f| match f.kind {
                Kind::Seq => Some(']'),
                Kind::Map => Some('}'),
                Kind::Pair => None,
            })
            .collect();
        let at_end = st.pos >= self.src.len();
        let insert = st.out_pos;
        let label = if code == INVALID_INDENTATION {
            format!("the flow {what} opened here is still open")
        } else {
            format!("this {bracket} is never closed")
        };
        let help = (code == INVALID_INDENTATION).then_some(
            "the lines of a flow collection must be indented more than its parent; \
             if the collection should end before this line, close it there",
        );
        // In a key position the closed collection still needs its `:`.
        let key_only = matches!(st.mode, Some(HeadMode::Key { .. }));
        let applicability = if at_end && !key_only {
            Applicability::MachineApplicable
        } else {
            Applicability::MaybeIncorrect
        };
        let fix = Suggestion::new(
            "close the collection",
            vec![Replacement::new(Span::empty(insert), closers)],
            applicability,
        );
        let label = Label::new(Span::new(open, open.saturating_add(1)), label);
        Some((label, fix, help))
    }

    fn set_phase_value(&mut self) {
        if let Some(Frame::Map { phase, .. }) = self.stack.last_mut() {
            *phase = MapPhase::Value;
        }
    }
}

/// How a less-indented line relates to the flow collection it stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dedent {
    /// The line starts with this closing bracket: the collection ends
    /// here, but its closer is short of indentation.
    Closer(u8),
    /// The line continues the collection and closes it: only its
    /// indentation is short.
    Closes,
    /// The collection is still open at the end of the line.
    Open,
}

/// Whether the line from `from` closes all `open` flow collections. Quoted
/// scalars are skipped and a comment ends the line; the scan never leaves
/// the line, so a diagnostic costs O(its line).
fn closes_on_line(src: &str, from: usize, mut open: usize) -> bool {
    let end = line_end(src, from);
    let mut i = from;
    // The start of the content counts as white space before it.
    let mut prev = b' ';
    while i < end {
        let Some(b) = byte(src, i) else {
            return false;
        };
        match b {
            b'\'' | b'"' if matches!(prev, b' ' | b'\t' | b'[' | b'{' | b',' | b':') => {
                // A quoted scalar: skip to its closing quote on this line.
                let mut j = i.saturating_add(1);
                loop {
                    if j >= end {
                        return false;
                    }
                    match byte(src, j) {
                        Some(b'\\') if b == b'"' => j = j.saturating_add(2),
                        Some(c)
                            if c == b
                                && b == b'\''
                                && byte(src, j.saturating_add(1)) == Some(b'\'') =>
                        {
                            j = j.saturating_add(2);
                        }
                        Some(c) if c == b => break,
                        Some(_) => j = j.saturating_add(1),
                        None => return false,
                    }
                }
                i = j;
            }
            b'#' if matches!(prev, b' ' | b'\t') => return false,
            b'[' | b'{' => open = open.saturating_add(1),
            b']' | b'}' => {
                open = open.saturating_sub(1);
                if open == 0 {
                    return true;
                }
            }
            _ => {}
        }
        prev = byte(src, i).unwrap_or(b' ');
        i = i.saturating_add(1);
    }
    false
}
