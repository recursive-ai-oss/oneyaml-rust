//! Nodes inside flow collections (`ns-flow-node`, production 161): properties,
//! nested collections, aliases, flow scalars, and empty nodes; plus closing
//! frames and the repair after an error (C6 R2).

use oneyaml_core::code::{
    ALIAS_WITH_PROPERTIES, DUPLICATE_PROPERTY, INVALID_ANCHOR, INVALID_NODE_START, INVALID_TAG,
    MISSING_FLOW_SEPARATOR, MULTILINE_IMPLICIT_KEY, QUOTED_SCALAR_INDENTATION, SCALAR_TOO_LONG,
    UNEXPECTED_FLOW_INDICATOR,
};

use super::Owed;
use super::block::validate_tag;
use super::flow::{Flow, Frame, Kind, Slot, State, Stop, separated};
use super::scalar::{self, Count};
use super::scan::{
    anchor_end, byte, indicator_at, is_plain_first, plain_line_end, scan_tag, skip_white, tag_blame,
};
use crate::event::{
    CollectionStyle, Event, EventKind, NodeInfo, Properties, ScalarStyle, TriviaKind,
};

/// What may follow a property (an anchor or a tag) of a flow node: white
/// space, the end, or a flow indicator that ends the node, which then has
/// no content. An opening bracket is not one: a collection cannot follow
/// properties without separation (production 161).
fn property_separated(src: &str, i: usize) -> bool {
    separated(src, i) && !matches!(byte(src, i), Some(b'[' | b'{'))
}

impl<'a> Flow<'_, 'a> {
    /// The node owed by the top frame (or the collection itself when the
    /// stack is empty).
    pub(super) fn node(&mut self) -> Result<(), Stop> {
        let src = self.src;
        let i = self.pos;
        let slot = match self.stack.last() {
            Some(Frame {
                state: State::Node(slot),
                ..
            }) => Some(*slot),
            _ => None,
        };
        match byte(src, i) {
            Some(b'&') => {
                let from = i.saturating_add(1);
                let end = anchor_end(src, from);
                if end == from || !property_separated(src, end) {
                    self.blame = Some(end);
                    return Err((INVALID_ANCHOR, "invalid anchor name"));
                }
                if self.props.anchor.is_some() {
                    self.blame = Some(i);
                    return Err((DUPLICATE_PROPERTY, "a node can have only one anchor"));
                }
                self.props.anchor = Some(i);
                self.token(EventKind::Trivia(TriviaKind::Anchor), end);
                Ok(())
            }
            Some(b'!') => {
                let scanned = scan_tag(src, i);
                let scan = scanned.filter(|t| property_separated(src, t.end));
                let Some(scan) = scan else {
                    self.blame = Some(tag_blame(src, i, scanned));
                    return Err((INVALID_TAG, "malformed tag"));
                };
                let prefix = validate_tag(src, self.tags, scan, self.max_scalar_len)?;
                if self.props.tag.is_some() {
                    self.blame = Some(i);
                    return Err((DUPLICATE_PROPERTY, "a node can have only one tag"));
                }
                self.props.tag = Some(i);
                self.props.tag_prefix = prefix;
                self.token(EventKind::Trivia(TriviaKind::Tag), scan.end);
                Ok(())
            }
            Some(b'[') => self.open(Kind::Seq),
            Some(b'{') => self.open(Kind::Map),
            Some(b'*') => {
                if !self.props.is_empty() {
                    return Err((ALIAS_WITH_PROPERTIES, "an alias cannot have properties"));
                }
                let from = i.saturating_add(1);
                let end = anchor_end(src, from);
                if end == from {
                    return Err((INVALID_ANCHOR, "empty alias name"));
                }
                self.token(EventKind::Alias, end);
                self.node_done(false, 0);
                Ok(())
            }
            Some(q @ (b'"' | b'\'')) => {
                let double = q == b'"';
                let f = scalar::quoted::<Count>(src, i, self.min, double).inspect_err(|e| {
                    if e.0 == QUOTED_SCALAR_INDENTATION {
                        self.blame = scalar::indentation_fault(src, i, self.min, double);
                    }
                })?;
                self.check_len(f.len)?;
                let style = if double {
                    ScalarStyle::DoubleQuoted
                } else {
                    ScalarStyle::SingleQuoted
                };
                self.scalar(style, f.end);
                self.node_done(true, 0);
                Ok(())
            }
            Some(b',' | b']' | b'}') => {
                if matches!(slot, Some(Slot::Entry { .. })) && self.props.is_empty() {
                    return Err((UNEXPECTED_FLOW_INDICATOR, "expected an entry"));
                }
                self.empty();
                self.node_done(false, 0);
                Ok(())
            }
            Some(b':') if separated(src, i.saturating_add(1)) => {
                let empty_ok = matches!(slot, Some(Slot::Key))
                    || matches!(slot, Some(Slot::Entry { .. })) && !self.props.is_empty();
                if !empty_ok {
                    return Err((INVALID_NODE_START, "expected a node before `:`"));
                }
                self.empty();
                self.node_done(false, 0);
                Ok(())
            }
            _ if is_plain_first(src, i, true) => {
                let first_end = plain_line_end(src, i, true);
                let f = scalar::plain(src, i, first_end, self.min, true);
                // Only a flow mapping key may span lines (production 144);
                // elsewhere a multi-line plain before `:` is a bad key, reported
                // before it is emitted (timing convention T3).
                let colon = skip_white(src, f.end);
                if f.multiline
                    && !matches!(slot, Some(Slot::Key))
                    && byte(src, colon) == Some(b':')
                    && separated(src, colon.saturating_add(1))
                {
                    // In a mapping value slot the `:` belongs to a next
                    // entry whose `,` is missing (production 141).
                    return Err(if matches!(slot, Some(Slot::Value)) {
                        (
                            MISSING_FLOW_SEPARATOR,
                            "flow entries must be separated by `,`",
                        )
                    } else {
                        (
                            MULTILINE_IMPLICIT_KEY,
                            "an implicit key must be on one line",
                        )
                    });
                }
                self.check_len(f.len)?;
                self.scalar(ScalarStyle::Plain, f.end);
                self.node_done(false, 0);
                Ok(())
            }
            _ => Err((INVALID_NODE_START, "this character cannot start a node")),
        }
    }

    /// `Limits::max_scalar_len` allows a value of `len` bytes.
    fn check_len(&self, len: usize) -> Result<(), Stop> {
        if len <= self.max_scalar_len {
            Ok(())
        } else {
            Err((SCALAR_TOO_LONG, "the scalar is too long"))
        }
    }

    /// Emits a flow scalar covering `pos..end` and tracks the lines it spans.
    fn scalar(&mut self, style: ScalarStyle, end: usize) {
        let props = core::mem::take(&mut self.props);
        let start = self.pos;
        let info = NodeInfo::new(start, props, None);
        self.token(EventKind::Scalar { style, info }, end);
        self.track_lines(start, end);
    }

    /// Updates the line bookkeeping after consuming `start..end`.
    fn track_lines(&mut self, start: usize, end: usize) {
        let src = self.src;
        let Some(text) = src.get(start..end) else {
            return;
        };
        if let Some(k) = text.rfind(['\n', '\r']) {
            let line = start.saturating_add(k).saturating_add(1);
            self.line_start = line;
            self.line_indent = src
                .as_bytes()
                .get(line..end)
                .map_or(0, |r| r.iter().take_while(|b| **b == b' ').count());
            self.crossed_line();
        }
    }

    /// An empty plain scalar before the pending trivia (C4 rules 1-3),
    /// carrying the pending properties.
    pub(super) fn empty(&mut self) {
        let props = core::mem::take(&mut self.props);
        let info = NodeInfo::new(self.out_pos, props, None);
        self.zero(super::with_info(EventKind::plain(), info));
    }

    /// Opens a flow sequence or mapping at the scan position, if
    /// `Limits::max_depth` allows it (checked before anything is emitted).
    fn open(&mut self, kind: Kind) -> Result<(), Stop> {
        self.push_frame(kind, State::Entry)?;
        let props = core::mem::take(&mut self.props);
        let end = self.pos.saturating_add(1);
        let event = if kind == Kind::Seq {
            EventKind::sequence_start(CollectionStyle::Flow)
        } else {
            EventKind::mapping_start(CollectionStyle::Flow)
        };
        let event = super::with_info(event, NodeInfo::new(self.pos, props, None));
        self.token(event, end);
        Ok(())
    }

    /// Closes the top flow sequence or mapping with its bracket.
    pub(super) fn close(&mut self) -> Result<(), Stop> {
        // Timing convention T3 (a multi-line implicit key never gets its
        // completing event): an outermost collection that spans lines and
        // is followed by `:` is a multi-line key (production 155); it is
        // rejected at its closing bracket, which is not emitted, just as a
        // multi-line scalar key is rejected without emitting the scalar.
        // The white space after the bracket is scanned only for an
        // outermost collection that spans lines: O(white space after the
        // bracket), and no work for any other closing bracket.
        if self.stack.len() == 1
            && self.line_start != self.first_line
            && indicator_at(
                self.src,
                skip_white(self.src, self.pos.saturating_add(1)),
                b':',
            )
        {
            // `flow_finished` relies on this rule: it never sees a
            // multi-line collection followed by `:`.
            self.multiline_key = true;
            return Err((
                MULTILINE_IMPLICIT_KEY,
                "an implicit key must be on one line",
            ));
        }
        let popped = self.pop_frame();
        let event = match popped {
            Some(Frame {
                kind: Kind::Seq, ..
            }) => EventKind::SequenceEnd,
            _ => EventKind::MappingEnd,
        };
        let end = self.pos.saturating_add(1);
        self.token(event, end);
        self.node_done(true, popped.map_or(0, |f| f.peak));
        Ok(())
    }

    /// Closes a flow pair right after its value (C4 "ends early").
    pub(super) fn close_pair(&mut self) {
        self.zero(EventKind::MappingEnd);
        self.pop_frame();
        if let Some(top) = self.stack.last_mut() {
            top.state = State::AfterEntry;
        }
    }

    /// A node of the top frame's slot is complete; `json`: it was a quoted
    /// scalar or a flow collection (`c-flow-json-node`, production 160);
    /// `peak`: the depth of its deepest collection (0 for a scalar).
    fn node_done(&mut self, json: bool, peak: usize) {
        let Some(top) = self.stack.last_mut() else {
            self.finished = true;
            return;
        };
        match (top.kind, top.state) {
            (Kind::Seq, State::Node(Slot::Entry { at, pos, line })) => {
                top.state = State::EntryDone {
                    at,
                    pos,
                    line,
                    json,
                    peak,
                };
            }
            (_, State::Node(Slot::Key)) => top.state = State::AfterKey { json },
            (Kind::Pair, State::Node(Slot::Value)) => self.close_pair(),
            _ => top.state = State::AfterEntry,
        }
    }

    /// Zero-width events completing every open frame after an error.
    pub(super) fn repairs(&mut self) -> Vec<Owed> {
        let mut out = Vec::new();
        let mut props = core::mem::take(&mut self.props);
        for (depth, frame) in self.stack.iter().rev().enumerate() {
            let top = depth == 0;
            let owed = match frame.state {
                State::Node(Slot::Key) if top => 2,
                State::Node(Slot::Key) | State::AfterKey { .. } => 1,
                State::Node(Slot::Value) if top => 1,
                State::Node(Slot::Entry { .. }) if top && !props.is_empty() => 1,
                _ => 0,
            };
            for _ in 0..owed {
                out.push(Owed::Empty(core::mem::take(&mut props)));
            }
            out.push(Owed::Kind(match frame.kind {
                Kind::Seq => EventKind::SequenceEnd,
                Kind::Map | Kind::Pair => EventKind::MappingEnd,
            }));
        }
        out
    }
}

/// Adds `extra` to the properties of a collection start event; `false` on a
/// duplicate anchor or tag.
pub(super) fn attach_props(event: &mut Event<'_>, extra: Properties) -> bool {
    let Some(mut props) = event.properties() else {
        return extra.is_empty();
    };
    if props.anchor.is_some() && extra.anchor.is_some()
        || props.tag.is_some() && extra.tag.is_some()
    {
        return false;
    }
    props.anchor = props.anchor.or(extra.anchor);
    if extra.tag.is_some() {
        props.tag = extra.tag;
        props.tag_prefix = extra.tag_prefix;
    }
    let info = NodeInfo::new(event.span().start, props, None);
    event.set_info(info);
    true
}
