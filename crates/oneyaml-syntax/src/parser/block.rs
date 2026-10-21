//! Block collections (YAML 1.2.2 §8.2) and node heads: properties (§6.9),
//! aliases, flow scalars, and implicit keys (productions 192-194).

use std::collections::BTreeMap;

use oneyaml_core::Span;
use oneyaml_core::code::{
    BLOCK_COLLECTION_NOT_ALLOWED, DEPTH_LIMIT_EXCEEDED, DIRECTIVE_IN_DOCUMENT, DUPLICATE_PROPERTY,
    INVALID_ANCHOR, INVALID_INDENTATION, INVALID_TAG, SCALAR_TOO_LONG, TAB_INDENTATION,
    UNDEFINED_TAG_HANDLE,
};

use super::scan::{
    TagPrefix, TagScan, UriRead, anchor_end, any_marker_at, blank_or_end, byte, indicator_at,
    scan_tag, skip_white, tag_blame,
};
use super::stream::ROOT;
use super::{Frame, MapPhase, NodeCtx, Parser, State};
use crate::event::{CollectionStyle, Event, EventKind, Indicator, Properties, TagForm, TriviaKind};

/// Where a node head is being parsed.
#[derive(Clone, Copy, Debug)]
pub(super) enum HeadMode {
    /// Any node; `can_open`: a block mapping may start at the head.
    Node { ctx: NodeCtx, can_open: bool },
    /// An implicit key of the open mapping with keys at `indent`.
    Key { indent: usize },
}

type Failure = (oneyaml_core::Code, &'static str);

fn trivia<'a>(kind: TriviaKind, start: usize, end: usize) -> Event<'a> {
    Event::new(EventKind::Trivia(kind), Span::new(start, end))
}

impl<'a> Parser<'a> {
    fn set_phase(&mut self, to: MapPhase) {
        if let Some(Frame::Map { phase, .. }) = self.stack.last_mut() {
            *phase = to;
        }
    }

    /// Emits an indicator at the scan position (after flushing trivia).
    fn indicator(&mut self, indicator: Indicator) {
        self.flush();
        let i = self.pos;
        self.token(
            EventKind::Trivia(TriviaKind::Indicator(indicator)),
            i.saturating_add(1),
        );
    }

    /// Emits already-built head events and consumes their bytes.
    pub(super) fn commit(&mut self, events: impl IntoIterator<Item = Event<'a>>) {
        for event in events {
            self.emit_pos = event.span().end;
            self.out.event(event);
        }
        self.pos = self.emit_pos;
        self.fresh = false;
        self.tab_seen = false;
    }

    /// The error for a block collection that may not start here.
    pub(super) fn collection_error(&self) -> Failure {
        if self.tab_seen {
            (TAB_INDENTATION, "tabs cannot be used for block indentation")
        } else {
            (
                BLOCK_COLLECTION_NOT_ALLOWED,
                "a block collection cannot start on this line",
            )
        }
    }

    /// Dispatch after a node or indicator, by the innermost frame.
    pub(super) fn next(&mut self) {
        self.scan_trivia(false);
        let Some(top) = self.stack.last().copied() else {
            self.state = State::Stream;
            return;
        };
        let src = self.src;
        let i = self.pos;
        let fresh = self.fresh;
        // A mark starting a document prefix ends the document
        // like a marker (`prefix_mark_at`).
        let ended = i >= src.len()
            || fresh && i == self.line_start && any_marker_at(src, i)
            || self.prefix_mark_at(i);
        let col = if fresh { self.line_indent } else { self.col() };
        let dedent = ended
            || fresh
                && match top {
                    Frame::Seq { indent } | Frame::Map { indent, .. } => col < indent,
                    Frame::Document { .. } => false,
                };
        match top {
            Frame::Document { root_done: true } => self.after_root(),
            Frame::Document { root_done: false } => self.state = State::Node(ROOT),
            // l+block-sequence (production 183), c-l-block-seq-entry (184).
            Frame::Seq { indent } => {
                if dedent {
                    self.close_block(true);
                } else if fresh && col == indent && !indicator_at(src, i, b'-') {
                    // Only an outer mapping (`seq-space`, production 201) can accept this.
                    self.close_block(false);
                } else if fresh && col == indent && self.tab_seen {
                    self.fail(
                        TAB_INDENTATION,
                        "tabs cannot be used for block indentation",
                        false,
                    );
                } else if fresh && col == indent {
                    self.indicator(Indicator::SequenceEntry);
                    self.state = State::Node(NodeCtx {
                        parent: Some(indent),
                        compact: true,
                        seq_at_parent: false,
                    });
                } else {
                    self.misplaced(fresh);
                }
            }
            // l+block-mapping (production 187), ns-l-block-map-entry (188).
            Frame::Map { indent, phase } => {
                if dedent && phase != MapPhase::Key {
                    // A missing value: e.g. `? a` at the end of a mapping
                    // (placement rule 3, after the key).
                    self.empty();
                    self.child_done();
                } else if dedent {
                    self.close_block(true);
                } else if fresh && i == self.line_start && byte(src, i) == Some(b'%') {
                    // A `%` line in a document not ended with `...`: a
                    // misplaced directive (production 211), as at the root.
                    self.fail(
                        DIRECTIVE_IN_DOCUMENT,
                        "a directive must follow a document ended with `...`",
                        false,
                    );
                } else if fresh && col == indent && self.tab_seen {
                    self.fail(
                        TAB_INDENTATION,
                        "tabs cannot be used for block indentation",
                        false,
                    );
                } else if fresh && col == indent {
                    if phase == MapPhase::AfterExplicitKey {
                        self.explicit_value(indent);
                    } else {
                        self.map_key(indent);
                    }
                } else {
                    self.misplaced(fresh);
                }
            }
        }
    }

    fn misplaced(&mut self, fresh: bool) {
        if fresh {
            self.fail(
                INVALID_INDENTATION,
                "content at an invalid indentation",
                false,
            );
        } else {
            self.unexpected_content();
        }
    }

    /// `l-block-map-explicit-value` (production 191), or an empty value when
    /// the next entry starts instead.
    fn explicit_value(&mut self, indent: usize) {
        if indicator_at(self.src, self.pos, b':') {
            self.indicator(Indicator::MappingValue);
            self.set_phase(MapPhase::Value);
            self.state = State::Node(NodeCtx {
                parent: Some(indent),
                compact: true,
                seq_at_parent: true,
            });
        } else {
            self.empty();
            self.child_done();
        }
    }

    /// A key of the open mapping: `?` (production 190), an empty key before
    /// `:`, or an implicit key (production 192).
    fn map_key(&mut self, indent: usize) {
        let (src, i) = (self.src, self.pos);
        if indicator_at(src, i, b'?') {
            self.indicator(Indicator::MappingKey);
            self.set_phase(MapPhase::ExplicitKey);
            self.state = State::Node(NodeCtx {
                parent: Some(indent),
                compact: true,
                seq_at_parent: true,
            });
        } else if indicator_at(src, i, b':') {
            self.flush();
            self.empty();
            self.indicator(Indicator::MappingValue);
            self.set_phase(MapPhase::Value);
            self.state = State::Node(NodeCtx {
                parent: Some(indent),
                compact: false,
                seq_at_parent: true,
            });
        } else if indicator_at(src, i, b'-') {
            // The dash may still start a key (`-x`); the white space after
            // it is the first character that cannot belong.
            self.blame.set(Some(i.saturating_add(1)));
            self.fail(
                INVALID_INDENTATION,
                "a sequence entry cannot start at the indentation of mapping keys",
                false,
            );
        } else {
            self.head(HeadMode::Key { indent });
        }
    }

    /// `s-l+block-node(n,c)` (production 196): an empty node, a block
    /// collection, or a node head.
    pub(super) fn node(&mut self, ctx: NodeCtx) {
        self.scan_trivia(false);
        let src = self.src;
        let i = self.pos;
        let fresh = self.fresh;
        let line_head = fresh && i == self.line_start;
        let col = if fresh { self.line_indent } else { self.col() };
        let dash = indicator_at(src, i, b'-');
        let seq_here = fresh && ctx.seq_at_parent && ctx.parent == Some(col) && dash;
        let indented = !fresh || ctx.parent.is_none_or(|n| col > n);
        let ends_document = line_head
            && (any_marker_at(src, i) || ctx.parent.is_none() && byte(src, i) == Some(b'%'))
            || self.prefix_mark_at(i);
        if i >= src.len() || ends_document || !indented && !seq_here {
            // e-node (production 106), placed by C4 rules 1-2.
            self.empty();
            self.child_done();
            return;
        }
        let can_open = (fresh || ctx.compact) && !self.tab_seen;
        let opener = if dash {
            Some(b'-')
        } else if indicator_at(src, i, b'?') {
            Some(b'?')
        } else if indicator_at(src, i, b':') {
            Some(b':')
        } else {
            None
        };
        match (opener, byte(src, i)) {
            (Some(_), _) if !can_open => {
                let (code, message) = self.collection_error();
                self.fail(code, message, true);
            }
            (Some(which), _) => self.open_collection(col, which),
            (None, Some(b'|' | b'>')) => {
                let events = self.take_scratch();
                self.block_scalar(ctx, events, Properties::none());
            }
            (None, _) => self.head(HeadMode::Node { ctx, can_open }),
        }
    }

    /// Opens a block sequence (`-`) or mapping (`?`, or `:` after an empty
    /// key) whose entries sit at `indent`, taking the pending properties.
    fn open_collection(&mut self, indent: usize, which: u8) {
        if self.fail_if_too_deep(self.block_depth().saturating_add(1)) {
            return;
        }
        self.begin_content();
        let props = core::mem::take(&mut self.props);
        let ctx = |compact, seq_at_parent| NodeCtx {
            parent: Some(indent),
            compact,
            seq_at_parent,
        };
        if which == b'-' {
            self.zero_node(EventKind::sequence_start(CollectionStyle::Block), props);
            self.stack.push(Frame::Seq { indent });
            self.indicator(Indicator::SequenceEntry);
            self.state = State::Node(ctx(true, false));
            return;
        }
        self.zero_node(EventKind::mapping_start(CollectionStyle::Block), props);
        if which == b'?' {
            self.stack.push(Frame::Map {
                indent,
                phase: MapPhase::ExplicitKey,
            });
            self.indicator(Indicator::MappingKey);
            self.state = State::Node(ctx(true, true));
        } else {
            self.stack.push(Frame::Map {
                indent,
                phase: MapPhase::Value,
            });
            self.empty();
            self.indicator(Indicator::MappingValue);
            self.state = State::Node(ctx(false, true));
        }
    }

    /// Reports `Limits::max_depth` for a block collection that would
    /// open at `depth`, at the position where it would start (like a
    /// collection that may not start there, the owed node is repaired).
    /// `true` when it failed.
    pub(super) fn fail_if_too_deep(&mut self, depth: usize) -> bool {
        if self.depth_ok(depth) {
            return false;
        }
        self.fail(
            DEPTH_LIMIT_EXCEEDED,
            "collections are nested too deeply",
            true,
        );
        true
    }

    /// Adds `new` to the pending properties; `false` on a duplicate.
    pub(super) fn merge_props(&mut self, new: Properties) -> bool {
        let p = &mut self.props;
        if p.anchor.is_some() && new.anchor.is_some() || p.tag.is_some() && new.tag.is_some() {
            return false;
        }
        p.anchor = p.anchor.or(new.anchor);
        if new.tag.is_some() {
            p.tag = new.tag;
            p.tag_prefix = new.tag_prefix;
        }
        true
    }

    /// Scans `c-ns-properties` (production 96) at the scan position into
    /// `events`; returns them, the position after the last one, and the
    /// position after any following white space.
    pub(super) fn scan_props(
        &self,
        events: &mut Vec<Event<'a>>,
    ) -> Result<(Properties, usize, usize), Failure> {
        let src = self.src;
        let mut props = Properties::none();
        let mut i = self.pos;
        let mut props_end = i;
        loop {
            match byte(src, i) {
                Some(b'&') => {
                    let from = i.saturating_add(1);
                    let end = anchor_end(src, from);
                    if end == from || !blank_or_end(src, end) {
                        self.blame.set(Some(end));
                        return Err((INVALID_ANCHOR, "invalid anchor name"));
                    }
                    if props.anchor.is_some() {
                        self.blame.set(Some(i));
                        return Err((DUPLICATE_PROPERTY, "a node can have only one anchor"));
                    }
                    props.anchor = Some(i);
                    events.push(trivia(TriviaKind::Anchor, i, end));
                    i = end;
                }
                Some(b'!') => {
                    let scanned = scan_tag(src, i);
                    let scan = scanned.filter(|t| blank_or_end(src, t.end));
                    let Some(scan) = scan else {
                        self.blame.set(Some(tag_blame(src, i, scanned)));
                        return Err((INVALID_TAG, "malformed tag"));
                    };
                    let prefix = validate_tag(src, &self.tags, scan, self.max_scalar_len)?;
                    if props.tag.is_some() {
                        self.blame.set(Some(i));
                        return Err((DUPLICATE_PROPERTY, "a node can have only one tag"));
                    }
                    props.tag = Some(i);
                    props.tag_prefix = prefix;
                    events.push(trivia(TriviaKind::Tag, i, scan.end));
                    i = scan.end;
                }
                _ => return Ok((props, props_end, i)),
            }
            props_end = i;
            let w = skip_white(src, i);
            if w > i {
                events.push(trivia(TriviaKind::Whitespace, i, w));
                i = w;
            }
            // §5.2: a byte order mark after a property is a
            // recoverable error, skipped as if absent. It counts as part of
            // the properties' run, so what follows is placed after it.
            while src.get(i..).is_some_and(|r| r.starts_with('\u{feff}')) {
                let end = i.saturating_add('\u{feff}'.len_utf8());
                events.push(Event::new(
                    EventKind::Error(super::misplaced_bom_diagnostic(i, end)),
                    Span::new(i, end),
                ));
                props_end = end;
                i = skip_white(src, end);
                if i > end {
                    events.push(trivia(TriviaKind::Whitespace, end, i));
                }
            }
        }
    }
}

/// Checks a scanned tag against the document's `%TAG` handles (§6.8.2) and
/// the defaults `!` and `!!`, and that its `%XX` escapes decode to UTF-8;
/// returns where the declaring `%TAG` prefix starts, if any. The resolved
/// tag is built on demand (`Sourced::tag`); this allocates only for a suffix
/// with escapes.
pub(super) fn validate_tag(
    src: &str,
    tags: &BTreeMap<&str, TagPrefix>,
    t: TagScan,
    max_len: usize,
) -> Result<Option<usize>, Failure> {
    /// The default handles' prefixes (§6.8.2.2), read like a `%TAG` one.
    const PRIMARY: UriRead = UriRead { len: 1 };
    const SECONDARY: UriRead = UriRead {
        len: "tag:yaml.org,2002:".len(),
    };
    if t.form == TagForm::NonSpecific {
        return Ok(None);
    }
    let handle = src.get(t.handle.0..t.handle.1).unwrap_or("");
    let suffix = src.get(t.suffix.0..t.suffix.1).unwrap_or("");
    let (prefix, read) = match t.form {
        TagForm::Shorthand => match (tags.get(handle), handle) {
            (Some(p), _) => (Some(p.start), p.read),
            (None, "!") => (None, PRIMARY),
            (None, "!!") => (None, SECONDARY),
            (None, _) => {
                return Err((UNDEFINED_TAG_HANDLE, "tag handle is not declared by %TAG"));
            }
        },
        _ => (None, UriRead::EMPTY),
    };
    // §6.8.2.2: the tag is the URI prefix + suffix, decoded as one
    // string (what `read::tag_at` resolves), so a prefix escape counts. The
    // prefix was read once at its directive; this continues from its end,
    // O(suffix) per tag.
    let whole = read.then(suffix);
    // A resolved tag is bounded like a scalar.
    if whole.len > max_len {
        return Err((SCALAR_TOO_LONG, "the resolved tag is too long"));
    }
    Ok(prefix)
}
