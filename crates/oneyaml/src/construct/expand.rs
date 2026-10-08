//! Anchors and aliases (§7.1), the alias budget, and `max_depth` of the
//! expanded structure.
//!
//! **Recording.** While any anchored node is open, every event read from
//! the source is appended to one flat *tape*; an anchor is a range of it.
//! Nested anchors share the tape, so memory is O(anchored content).
//! An alias met while recording is taped as a reference to the anchor it
//! names *at that point*, so a later redefinition of the name cannot change
//! what an earlier alias means.
//!
//! **Replay.** An alias pushes a cursor over its anchor's range; a taped alias
//! inside it pushes another. Cursors live on an explicit stack (no recursion).
//! A taped alias always names an anchor that ended before it was taped, so
//! cursors only move to earlier, finished ranges: replay terminates, and a
//! recursive alias (`&a [*a]`) is an error, not a loop.
//!
//! **Budget.** Every node produced by replay counts, nested
//! expansions and merges through aliases included, checked as it is
//! produced against `Limits::get_alias_budget(own nodes so far)`. Own nodes
//! only grow, so checking early is never looser than checking at the end.
//! Both counts restart at each document start: the budget is per document.
//! The bytes expansion materialises are metered the same way, against
//! `Limits::get_alias_byte_budget` ([`super::budget`]).
//!
//! **Anchor sizes.** When an anchor's recording completes, its
//! expanded size (nodes and bytes, nested aliases included) is known: each
//! open recording accumulates what is taped into it, and a completed one
//! adds its total to the recording around it, O(1) per taped event
//! (saturating, so a size past `usize::MAX` stays `usize::MAX`, which no
//! budget but a disabled one admits). An alias whose whole expansion
//! would exceed a budget is refused at the alias, before anything is
//! replayed: replay produces exactly the anchor's taped events, and no own
//! node is read during it, so the budgets it is checked against cannot
//! change before it ends, and the refusal is the one the replay would
//! have reached (an over-budget read costs O(the input it read),
//! never O(budget)). The sizes live with the read's tape, so they are
//! always those of the text being read (edits need no invalidation).

use std::collections::HashMap;

use oneyaml_core::{Limits, Span, code};

use super::budget::{ReadShared, SharedRead};
use super::{Ev, Node, Source, Tag};
use crate::error::Error;

enum Taped<'a> {
    Ev(Ev<'a>),
    /// An alias, resolved to its anchor's index when taped.
    Alias {
        target: usize,
        span: Span,
    },
}

#[derive(Clone, Copy)]
struct Anchor {
    start: usize,
    /// `None` while the anchored node is still open.
    end: Option<usize>,
    /// What replaying it produces (set when it ends).
    size: Size,
}

/// What a replay produces: nodes and bytes (saturating).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Size {
    nodes: usize,
    bytes: usize,
}

impl Size {
    fn plus(self, o: Size) -> Size {
        Size {
            nodes: self.nodes.saturating_add(o.nodes),
            bytes: self.bytes.saturating_add(o.bytes),
        }
    }
}

/// A tag's logical length (prefix + suffix). O(1).
fn tag_len(tag: &Tag<'_>) -> usize {
    match tag {
        Tag::Named(t) => t.tag().len(),
        _ => 0,
    }
}

/// What replaying `ev` costs: a node, and the scalar text and tag it
/// copies.
fn replay_cost(ev: &Ev<'_>) -> Size {
    match &ev.node {
        Node::Scalar { value, tag, .. } => Size {
            nodes: 1,
            bytes: value.len().saturating_add(tag_len(tag)),
        },
        Node::SeqStart { tag } | Node::MapStart { tag } => Size {
            nodes: 1,
            bytes: tag_len(tag),
        },
        _ => Size::default(),
    }
}

/// The bytes an own (not replayed) event materialises beyond the input:
/// its tag's shared `%TAG` prefix ([`super::budget`]).
fn own_bytes(ev: &Ev<'_>) -> usize {
    match &ev.node {
        Node::Scalar {
            tag: Tag::Named(t), ..
        }
        | Node::SeqStart { tag: Tag::Named(t) }
        | Node::MapStart { tag: Tag::Named(t) } => {
            t.tag().shared_prefix().map_or(0, |p| p.text().len())
        }
        _ => 0,
    }
}

#[cfg(test)]
thread_local! {
    /// Events replayed on this thread (refusals do not replay).
    pub(crate) static REPLAYED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct Cursor {
    start: usize,
    pos: usize,
    end: usize,
    /// The alias this replay stands for. The replayed node spans the
    /// alias: its first event (the node's start) *and* its last event (the
    /// node's end, the same event for a scalar) carry the alias's span, so
    /// a duplicate or type error caused by an alias points at the alias,
    /// as strict checking does, and a span measured from the
    /// node's start to its end (`Spanned`) is the alias's, never an
    /// inverted mix of alias start and anchored-node end. Events inside
    /// the node keep their own (anchored) spans.
    alias: Span,
    /// The alias itself was replayed (an alias inside an anchored node):
    /// the edges then stand at a replayed position too ([`Ev::replayed`]).
    alias_replayed: bool,
    /// This read delivered the anchored node where it was defined (so its
    /// readers checked it): not anchored content from outside a per-node
    /// read ([`Source::take_preload`]), unless reached through a node this
    /// read delivered.
    delivered: bool,
}

/// An alias to no anchor defined before it (Y1015). Shared with
/// `Document`'s unexpanded check, so both report it alike.
pub(crate) fn undefined_alias(name: &str, span: Span) -> Error {
    // The name as a bounded segment (an anchor name may be
    // megabytes; message and help each echo it).
    let name = crate::error::segment(name);
    Error::new(
        code::UNDEFINED_ALIAS,
        format!("alias `*{name}` refers to no anchor defined before it in this document"),
        Some(span),
    )
    .with_help(format!("define `&{name}` on an earlier node"))
}

/// The `Y2004` limit error: `budget` for a document of `own` nodes.
/// Shared with the per-node read's scan of what its aliases reach.
pub(crate) fn alias_budget_exceeded(budget: usize, own: usize, span: Option<Span>) -> Error {
    Error::new(
        code::ALIAS_BUDGET_EXCEEDED,
        format!(
            "aliases expand to more than {budget} nodes (the document has {own} nodes of its own)"
        ),
        span,
    )
    .with_help(format!(
        "raise Limits::alias_budget (currently {budget} for this document), \
         or use Limits::unlimited() for fully trusted input"
    ))
}

/// An alias inside the node its anchor names (Y1015; shared as
/// [`undefined_alias`]).
pub(crate) fn recursive_alias(name: &str, span: Span) -> Error {
    let name = crate::error::segment(name);
    Error::new(
        code::UNDEFINED_ALIAS,
        format!("alias `*{name}` is inside the node anchored as `&{name}` (a recursive alias)"),
        Some(span),
    )
}

/// Expands aliases from a [`Source`]; the events it yields never contain
/// `Alias` nodes or anchors.
pub(crate) struct Expander<'a, S> {
    src: S,
    limits: Limits,
    tape: Vec<Taped<'a>>,
    anchors: Vec<Anchor>,
    names: HashMap<&'a str, usize>,
    /// Open recordings: (anchor index, source depth at its start, what is
    /// taped into it so far).
    recording: Vec<(usize, usize, Size)>,
    replay: Vec<Cursor>,
    /// Collection depth of the source (drives recording).
    src_depth: usize,
    /// Collection depth of the expanded structure (checked against
    /// `max_depth`).
    depth: usize,
    own_nodes: usize,
    expanded: usize,
    peeked: Option<Ev<'a>>,
    last_end: usize,
    /// [`Source::nodes_before`], once asked (per document).
    nodes_before: Option<usize>,
    /// The byte budget (shared with the read's variant naming).
    shared: SharedRead,
    /// Where the current document starts in the source.
    base: usize,
    /// The tape before this index holds anchored content from outside the
    /// read ([`Source::take_preload`]), never delivered by it.
    preloaded: usize,
}

impl<'a, S: Source<'a>> Expander<'a, S> {
    pub(crate) fn new(src: S, limits: &Limits) -> Self {
        Self {
            src,
            limits: limits.clone(),
            tape: Vec::new(),
            anchors: Vec::new(),
            names: HashMap::new(),
            recording: Vec::new(),
            replay: Vec::new(),
            src_depth: 0,
            depth: 0,
            own_nodes: 0,
            expanded: 0,
            peeked: None,
            last_end: 0,
            nodes_before: None,
            shared: ReadShared::new(limits),
            base: 0,
            preloaded: 0,
        }
    }

    /// What the read shares: its byte meter and variant memo.
    pub(crate) fn shared(&self) -> SharedRead {
        std::rc::Rc::clone(&self.shared)
    }

    /// The underlying source (for warnings).
    pub(crate) fn source_mut(&mut self) -> &mut S {
        &mut self.src
    }

    /// The end offset of the last event returned by [`Expander::next`].
    pub(crate) fn last_end(&self) -> usize {
        self.last_end
    }

    /// Whether the last event produced (the one peeked, or else the last
    /// returned) is the first event of an alias's replay whose anchored
    /// node this read delivered where it was defined, so every event of
    /// the replay was checked by this read's readers then.
    pub(crate) fn starts_delivered_replay(&self) -> bool {
        self.replay
            .last()
            .is_some_and(|c| c.delivered && c.pos == c.start.saturating_add(1))
    }

    /// The next event, without consuming it.
    pub(crate) fn peek(&mut self) -> Result<&Ev<'a>, Error> {
        if self.peeked.is_none() {
            self.peeked = Some(self.produce()?);
        }
        match &self.peeked {
            Some(e) => Ok(e),
            None => Err(Error::internal("peek without event", None)),
        }
    }

    /// The next event.
    pub(crate) fn next(&mut self) -> Result<Ev<'a>, Error> {
        let ev = match self.peeked.take() {
            Some(e) => e,
            None => self.produce()?,
        };
        self.last_end = ev.span.end;
        Ok(ev)
    }

    fn produce(&mut self) -> Result<Ev<'a>, Error> {
        loop {
            let Some(top) = self.replay.last_mut() else {
                return self.pull_source();
            };
            if top.pos >= top.end {
                self.replay.pop();
                continue;
            }
            let at = top.pos;
            top.pos = at.saturating_add(1);
            // The node's first or last event (a range is exactly one node,
            // and both ends are always taped events: an alias has no
            // anchor of its own, and a range ends with a completing event).
            let edge = at == top.start || top.pos >= top.end;
            let (alias, alias_replayed) = (top.alias, top.alias_replayed);
            match self.tape.get(at) {
                Some(Taped::Ev(e)) => {
                    let mut e = e.clone();
                    e.anchor = None;
                    // Inside the node, a position already
                    // read; at its edges, the alias's own position.
                    e.replayed = true;
                    if edge {
                        e.span = alias;
                        e.replayed = alias_replayed;
                    }
                    #[cfg(test)]
                    REPLAYED.with(|r| r.set(r.get().saturating_add(1)));
                    if e.starts_node() {
                        self.count_expanded(e.span)?;
                    }
                    self.shared
                        .meter
                        .charge(replay_cost(&e).bytes, Some(e.span))?;
                    self.track_depth(&e)?;
                    return Ok(e);
                }
                Some(Taped::Alias { target, span }) => {
                    let (target, span) = (*target, *span);
                    self.push_replay(target, span, true)?;
                }
                None => return Err(Error::internal("replay past the tape", None)),
            }
        }
    }

    fn pull_source(&mut self) -> Result<Ev<'a>, Error> {
        let mut ev = self.src.next_event()?;
        if matches!(ev.node, Node::DocStart { .. }) {
            // Anchors are scoped to their document (§7.1).
            self.tape.clear();
            self.anchors.clear();
            self.names.clear();
            self.recording.clear();
            // The alias budget is per document, like the anchors it
            // spends, so a stream of documents that each pass on their own
            // passes as a whole.
            self.own_nodes = 0;
            self.expanded = 0;
            self.nodes_before = None;
            // The byte budget is per document too, scaled by the
            // bytes of the document read so far, from the same start a
            // per-node read measures from.
            self.shared.meter.reset();
            self.base = self.src.document_start().unwrap_or(ev.span.start);
            for pre in self.src.take_preload()? {
                self.preload(pre)?;
            }
            self.preloaded = self.tape.len();
        }
        self.shared
            .meter
            .read_to(ev.span.end.saturating_sub(self.base));
        if ev.starts_node() {
            self.own_nodes = self.own_nodes.saturating_add(1);
        }
        if let Node::Alias { name } = ev.node {
            let target = self.resolve_alias(name, ev.span)?;
            self.tape_alias(target, ev.span);
            self.push_replay(target, ev.span, false)?;
            return self.produce();
        }
        self.record(&ev);
        self.shared.meter.charge(own_bytes(&ev), Some(ev.span))?;
        ev.anchor = None;
        self.track_depth(&ev)?;
        Ok(ev)
    }

    /// Tapes an alias to `target` if a recording is open: the recording
    /// grows by what the alias expands to.
    fn tape_alias(&mut self, target: usize, span: Span) {
        let Some(top) = self.recording.last_mut() else {
            return;
        };
        let size = self
            .anchors
            .get(target)
            .map_or_else(Size::default, |a| a.size);
        top.2 = top.2.plus(size);
        self.tape.push(Taped::Alias { target, span });
    }

    /// Records an event of anchored content from outside the read
    /// ([`Source::take_preload`]): taped like any anchored event (its
    /// aliases resolved to the anchors taped before it), never delivered,
    /// and not counted as an own node.
    fn preload(&mut self, ev: Ev<'a>) -> Result<(), Error> {
        if let Node::Alias { name } = ev.node {
            let target = self.resolve_alias(name, ev.span)?;
            self.tape_alias(target, ev.span);
            return Ok(());
        }
        self.record(&ev);
        Ok(())
    }

    /// Tapes `ev` if a recording is open or it opens one, and closes
    /// recordings it completes.
    fn record(&mut self, ev: &Ev<'a>) {
        if let Some(name) = ev.anchor {
            let index = self.anchors.len();
            self.anchors.push(Anchor {
                start: self.tape.len(),
                end: None,
                size: Size::default(),
            });
            self.names.insert(name, index);
            self.recording
                .push((index, self.src_depth, Size::default()));
        }
        match ev.node {
            Node::SeqStart { .. } | Node::MapStart { .. } => {
                self.src_depth = self.src_depth.saturating_add(1);
            }
            Node::SeqEnd | Node::MapEnd => self.src_depth = self.src_depth.saturating_sub(1),
            _ => {}
        }
        if self.recording.is_empty() {
            return;
        }
        self.tape.push(Taped::Ev(ev.clone()));
        if let Some(top) = self.recording.last_mut() {
            top.2 = top.2.plus(replay_cost(ev));
        }
        let end = self.tape.len();
        // A recording is complete when the source is back at its start
        // depth after a node-ending event (a scalar, or the matching end).
        let completes = matches!(ev.node, Node::Scalar { .. } | Node::SeqEnd | Node::MapEnd);
        if !completes {
            return;
        }
        while let Some(&(index, depth, size)) = self.recording.last() {
            if depth != self.src_depth {
                break;
            }
            if let Some(a) = self.anchors.get_mut(index) {
                a.end = Some(end);
                a.size = size;
            }
            self.recording.pop();
            // The anchored node is part of the recording around it.
            if let Some(outer) = self.recording.last_mut() {
                outer.2 = outer.2.plus(size);
            }
        }
    }

    fn resolve_alias(&self, name: &str, span: Span) -> Result<usize, Error> {
        let Some(&index) = self.names.get(name) else {
            return Err(undefined_alias(name, span));
        };
        match self.anchors.get(index) {
            Some(Anchor { end: Some(_), .. }) => Ok(index),
            _ => Err(recursive_alias(name, span)),
        }
    }

    fn push_replay(
        &mut self,
        target: usize,
        alias: Span,
        alias_replayed: bool,
    ) -> Result<(), Error> {
        match self.anchors.get(target) {
            Some(&Anchor {
                start,
                end: Some(end),
                size,
            }) => {
                // Refused here when the whole replay would be.
                self.check_nodes(self.expanded.saturating_add(size.nodes), alias)?;
                self.shared.meter.check(size.bytes, Some(alias))?;
                let delivered = start >= self.preloaded
                    || (alias_replayed && self.replay.last().is_some_and(|c| c.delivered));
                self.replay.push(Cursor {
                    start,
                    pos: start,
                    end,
                    alias,
                    alias_replayed,
                    delivered,
                });
                Ok(())
            }
            _ => Err(Error::internal(
                "alias to an unfinished anchor",
                Some(alias),
            )),
        }
    }

    fn count_expanded(&mut self, span: Span) -> Result<(), Error> {
        self.expanded = self.expanded.saturating_add(1);
        self.check_nodes(self.expanded, span)
    }

    /// Whether `total` expanded nodes fit the node budget here.
    fn check_nodes(&mut self, total: usize, span: Span) -> Result<(), Error> {
        let Some(budget) = self.limits.get_alias_budget(self.own_nodes) else {
            return Ok(());
        };
        if total <= budget {
            return Ok(());
        }
        // A per-node read (`NodeSource`) counts only the node's own nodes;
        // The budget counts incrementally from the document's start, so the
        // budget at this point is based on the own nodes before the node
        // too (asked once, O(log n), and only when the budget depends on
        // the count at all).
        let flat = self.limits.get_alias_budget(0) == self.limits.get_alias_budget(usize::MAX);
        if !flat && self.nodes_before.is_none() {
            self.nodes_before = Some(self.src.nodes_before().unwrap_or(0));
        }
        let own = self
            .own_nodes
            .saturating_add(self.nodes_before.unwrap_or(0));
        match self.limits.get_alias_budget(own) {
            Some(budget) if total > budget => Err(alias_budget_exceeded(budget, own, Some(span))),
            _ => Ok(()),
        }
    }

    fn track_depth(&mut self, ev: &Ev<'a>) -> Result<(), Error> {
        match ev.node {
            Node::SeqStart { .. } | Node::MapStart { .. } => {
                self.depth = self.depth.saturating_add(1);
                let limit = self.limits.get_max_depth();
                if self.depth > limit {
                    return Err(Error::new(
                        code::DEPTH_LIMIT_EXCEEDED,
                        format!("nesting deeper than {limit} levels after alias expansion"),
                        Some(ev.span),
                    )
                    .with_help(format!("raise Limits::max_depth (currently {limit})")));
                }
            }
            Node::SeqEnd | Node::MapEnd => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::{Document, Stream, Value};
    use oneyaml_core::Settings;

    /// Events replayed on this thread while `f` runs.
    fn replayed<R>(f: impl FnOnce() -> R) -> (R, usize) {
        let before = REPLAYED.with(std::cell::Cell::get);
        let r = f();
        (
            r,
            REPLAYED.with(std::cell::Cell::get).saturating_sub(before),
        )
    }

    /// A billion-laughs chain: `l6` expands to 10^7 scalars.
    fn chain() -> String {
        let mut s = String::from("l0: &l0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1usize..7 {
            let refs = vec![format!("*l{}", i.saturating_sub(1)); 10].join(", ");
            s.push_str(&format!("l{i}: &l{i} [{refs}]\n"));
        }
        s
    }

    fn untrusted() -> Settings {
        Settings::new().limits(Limits::untrusted())
    }

    #[test]
    fn anchor_sizes_count_nested_aliases_and_saturate() {
        // Sizes as replay would count them: `b` is itself (1) + two `a`s.
        let src = "a: &a [x, !t yy]\nb: &b [*a, *a]\nc: [*b]\n";
        let feed = super::super::feed::Feed::new(
            oneyaml_syntax::events_with(src, &Settings::new()),
            src.len(),
        );
        let mut e = Expander::new(feed, &Limits::default());
        let mut n = 0usize;
        loop {
            let ev = e.next().unwrap();
            if ev.node == Node::StreamEnd {
                break;
            }
            n = n.saturating_add(1);
        }
        assert!(n > 0);
        let a = e.anchors[0].size;
        assert_eq!(
            a,
            Size {
                nodes: 3,
                bytes: 1 + 2 + 2
            }
        );
        assert_eq!(
            e.anchors[1].size,
            Size {
                nodes: 7,
                bytes: 10
            }
        );
        // Doubling 80 times: the size saturates (no overflow), and
        // the read is refused at once.
        let mut s = String::from("a0: &a0 [x]\n");
        for i in 1usize..80 {
            s.push_str(&format!(
                "a{i}: &a{i} [*a{}, *a{}]\n",
                i.saturating_sub(1),
                i.saturating_sub(1)
            ));
        }
        let (r, n) = replayed(|| crate::from_str::<Value>(&s));
        assert_eq!(r.unwrap_err().code(), code::ALIAS_BUDGET_EXCEEDED);
        // Only the levels within the budget replay (2 + 4 + ... < 2^21).
        assert!(n < 1 << 21, "{n}");
    }

    /// An over-budget alias is refused before anything of it is
    /// replayed. A whole read still replays what fits the budget before
    /// it (the anchors' own definitions here); 300 refused node reads
    /// replay nothing (before: each replayed up to the budget).
    #[test]
    fn refusals_do_not_replay() {
        let mut src = chain();
        for i in 0..300 {
            src.push_str(&format!("n{i}: [*l6]\n"));
        }
        for (s, budget) in [(Settings::new(), 1_000_000usize), (untrusted(), 100_000)] {
            let (r, n) = replayed(|| crate::from_str_with::<Value>(&src, &s));
            assert_eq!(r.unwrap_err().code(), code::ALIAS_BUDGET_EXCEEDED);
            // At most the budget's nodes (each sequence two events).
            assert!(n <= budget.saturating_mul(2), "{n}");
            let doc = Document::parse_with(src.as_str(), &s).unwrap();
            let (refused, n) = replayed(|| {
                (0..300)
                    .filter(|i| {
                        let node = doc.get(format!("n{i}")).unwrap();
                        node.deserialize::<Value>().unwrap_err().code()
                            == code::ALIAS_BUDGET_EXCEEDED
                    })
                    .count()
            });
            assert_eq!((refused, n), (300, 0));
        }
        // The byte budget: a 64 KiB scalar, 1,000 aliases, budget 1 MiB:
        // 16 copies fit exactly (the anchored scalar itself is input, not
        // charged), the 17th is refused at its alias.
        let mut src = format!("a: &a {}\nb: [", "x".repeat(1 << 16));
        src.push_str(&vec!["*a"; 1000].join(", "));
        src.push_str("]\n");
        let s = Settings::new().limits(Limits::default().alias_byte_budget(1 << 20));
        let (r, n) = replayed(|| crate::from_str_with::<Value>(&src, &s));
        assert_eq!(r.unwrap_err().code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
        assert_eq!(n, 16);
        let doc = Document::parse_with(src.as_str(), &s).unwrap();
        let (r, n) = replayed(|| doc.get("b").unwrap().deserialize::<Value>());
        assert_eq!(r.unwrap_err().code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
        assert_eq!(n, 16);
        // A node holding only the refused alias: nothing replayed.
        let src = format!("a: &a {}\nb: [*a]\n", "x".repeat(1 << 16));
        let s = Settings::new().limits(Limits::default().alias_byte_budget(1 << 15));
        let doc = Document::parse_with(src.as_str(), &s).unwrap();
        let (r, n) = replayed(|| {
            (0..300)
                .filter(|_| doc.get("b").unwrap().deserialize::<Value>().is_err())
                .count()
        });
        assert_eq!((r, n), (300, 0));
    }

    /// 1,000 documents, each over its own budget: each is refused
    /// at its over-budget alias, having replayed at most its own budget
    /// (the bound for a stream is `max_documents` × the budget).
    #[test]
    fn a_thousand_refused_documents_stay_within_their_budgets() {
        let one = format!("{}n: [*l6]\n", chain());
        let stream_src = vec![one.as_str(); 1000].join("---\n");
        let budget = 1_000;
        let s = Settings::new().limits(Limits::untrusted().alias_budget(budget));
        let stream = Stream::parse_with(stream_src.as_str(), &s).unwrap();
        assert_eq!(stream.len(), 1000);
        let (refused, n) = replayed(|| {
            stream
                .documents()
                .filter(|d| d.to_value().unwrap_err().code() == code::ALIAS_BUDGET_EXCEEDED)
                .count()
        });
        assert_eq!(refused, 1000);
        assert!(n <= budget.saturating_mul(2000), "{n}");
        // The serde stream stops at the first refusal.
        let docs: Vec<_> = crate::Deserializer::from_str_with(&stream_src, &s)
            .map(<Value as serde::Deserialize>::deserialize)
            .collect();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0].as_ref().unwrap_err().code(),
            code::ALIAS_BUDGET_EXCEEDED
        );
    }
}
