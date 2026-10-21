//! Structural edits of a [`SyntaxTree`].
//!
//! # Representation of an edited tree
//!
//! The parsed arrays are never modified. The first edit adds an [`Edits`]
//! overlay (O(1)); from then on the tree is:
//!
//! * **arenas**: the base tree (arena 0) and one owned, immutable flat
//!   arena per inserted [`Fragment`]. Every element has a **slot**: its
//!   index in the base, or its arena's first slot plus its local index.
//!   Slots are never reused, so a [`NodeId`] of a removed or replaced
//!   element can never name another element. An inserted arena
//!   that no run covers any more (all its text was removed again) is
//!   dropped when that removal is accepted (memory stays bounded), so
//!   the overlay holds the text in the tree, not the edit history;
//! * **order**: the elements in preorder are a sequence of **runs**, each
//!   a contiguous local range of one arena, kept in a treap with sums of
//!   element counts and token bytes ([`Seq`]). A slot is live exactly when
//!   a run covers it. Rank ([`Node::preorder_index`]) and byte offset
//!   ([`Node::span`]) are a prefix sum: O(log R) for R runs, and R grows
//!   by at most 3 per accepted edit, never with the document; a refused
//!   edit merges back the runs it split, and an
//!   accepted one merges the runs it leaves continuing each other (an
//!   insertion and its removal leave no run behind). Each run
//!   also counts its arena's diagnostics, summed in the treap, so a scan
//!   for diagnostics jumps over the runs without any in O(log R); and
//!   says whether it holds only stream-level trivia tokens, summed too,
//!   so a walk over stream-level trivia jumps over any number of such runs
//!   in O(log R);
//!
//!   **Sequential reads**: a run names its arena's index in a
//!   slab (`slab.rs`, O(1), entries never move), and a [`Node`] carries
//!   its run's treap item as a hint, which the iterators keep with the
//!   run's bounds. Inside a run every step is the arena's arithmetic (see
//!   *hinted reads* below: parents are the arena's and the run's preorder
//!   is the arena's, so a link that stays inside a run is determined), and
//!   the next run is the treap successor, so a full
//!   read costs amortised O(1) per element and O(1) lookups per run;
//!
//!   **Edit cost is independent of edit history**:
//!   no step of an edit or a read visits the runs
//!   one by one outside the edit's own window, so the k-th edit costs
//!   what the first costs, up to the O(log R) of the treap;
//! * **links**: parent, first/last child, and next/previous sibling are
//!   read from the element's own arena (the same O(1)/O(depth) arithmetic
//!   as an unedited tree, one link at a time), except where an edit changed
//!   them: a sparse map of overrides, at most 4 entries per edit, those
//!   of removed slots dropped with them;
//! * **dropped warnings**: warnings attached to a live element whose
//!   labels were all in text an edit removed (a `Y0028` of a replaced
//!   lenient value, on the line break after it): skipped by
//!   every read, so the element and its handle stay.
//!
//! Nothing else is kept: no copies of text and no moved diagnostics
//! (no per-node caches, no interior mutability). A
//! node's text is read from the runs it spans (borrowed when one run holds
//! it), and diagnostics are read from each arena's own list, their spans
//! moved on the way out.
//!
//! # Cost of an edit
//!
//! An edit copies what it removes (O(removed)), links the fragment in
//! (O(fragment roots)), splits at most two runs, updates at most four link
//! overrides (O(depth)), and re-reads its validation window in context
//! (see the module docs of [`super`], *Well-formed edits*): O(depth +
//! log R + window), never O(document) below document level, and never a
//! neighbouring entry's whole subtree. A refused edit
//! is rolled back exactly (an undo record of the few changes made), so the
//! tree, its text, and every handle are as they were.

use super::stored::Stored;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use oneyaml_core::{Code, Diagnostic, Settings, Span};

use super::arena::{Arena, Flat};
use super::facts::{LONG_RUN, StreamFacts};
use super::fragment::Fragment;
use super::runs::{Marks, Piece, RunFacts, marks};
use super::seq::{Run, RunAt, Seq};
use super::slab::Slab;
use super::validate::Removed;
use super::{Extra, NONE, NodeId, StaleNode, SyntaxKind, SyntaxTree, cost};

/// Why an edit of a [`SyntaxTree`] was refused. The tree is unchanged
/// when an edit fails.
///
/// A distinct name from `oneyaml`'s own `EditError`: the
/// `oneyaml` crate maps this type and never re-exports it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeEditError {
    /// A handle names no live element of this tree.
    Stale(StaleNode),
    /// Children were to be added under a token.
    NotANode {
        /// The token's kind.
        kind: SyntaxKind,
    },
    /// A token operation was given a node.
    NotAToken {
        /// The node's kind.
        kind: SyntaxKind,
    },
    /// The element has no parent (the stream root cannot be removed,
    /// replaced, or given siblings).
    NoParent,
    /// The two ends of a range are not siblings, or the last comes before
    /// the first.
    NotSiblings,
    /// A child position past the end.
    Position {
        /// The position asked for.
        position: usize,
        /// Number of children (or roots).
        len: usize,
    },
    /// The element cannot be changed this way: the inside of a scalar,
    /// alias, or error node; directives (they change the meaning of the
    /// whole document); tag properties and block scalar headers (their
    /// meaning is carried by the node: replace the node instead); tokens
    /// whose text cannot be replaced in place. `kind: Error` also for an
    /// edit that could change how recovered input reads (see
    /// *Recovered input* in the [module docs](super)).
    ReadOnly {
        /// The kind refused.
        kind: SyntaxKind,
    },
    /// A fragment element may not sit under that parent (a stream anywhere,
    /// a document outside the stream, a YAML node directly in the stream).
    Misplaced {
        /// The fragment element's kind.
        kind: SyntaxKind,
        /// The parent's kind.
        parent: SyntaxKind,
    },
    /// The fragment has tags resolved under other `%TAG` directives than
    /// the document it would go into.
    TagContext,
    /// Replacement text does not read as exactly one token of the kind it
    /// replaces.
    InvalidToken {
        /// The token kind expected.
        kind: SyntaxKind,
    },
    /// Fragment text does not parse cleanly: the first error's code.
    Syntax {
        /// Code of the first error.
        code: Code,
    },
    /// An empty fragment cannot be inserted.
    EmptyFragment,
    /// The edit is not well-formed: the text around it, read again through
    /// the parser, would not give the tree the edit produces (it has
    /// errors, or another structure). See the *Well-formed edits* section
    /// of the [module docs](super) for the exact rule.
    NotWellFormed,
    /// The edit would exceed one of the tree's
    /// [`Limits`](oneyaml_core::Limits) (limits apply to edits): the
    /// code of the limit diagnostic the edited text would re-parse with
    /// (`Y2000` `max_depth`, counted from the tree's root; `Y2001`
    /// `max_scalar_len`; `Y2002` `max_document_size`; `Y2003`
    /// `max_documents`).
    Limit {
        /// The limit diagnostic's code.
        code: Code,
    },
    /// The slot space (2³² elements over the tree's lifetime) or a
    /// fragment's 4 GiB is exhausted.
    Capacity,
    /// An internal invariant failed (a bug; `Y9000`).
    Internal,
}

impl core::fmt::Display for TreeEditError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Stale(s) => write!(f, "{s}"),
            Self::NotANode { kind } => write!(f, "a {kind:?} token has no children"),
            Self::NotAToken { kind } => write!(f, "a {kind:?} node is not a token"),
            Self::NoParent => f.write_str("the stream root cannot be removed or given siblings"),
            Self::NotSiblings => f.write_str("the range ends are not siblings in order"),
            Self::Position { position, len } => {
                write!(f, "position {position} is past the end ({len})")
            }
            Self::ReadOnly { kind } => write!(f, "a {kind:?} cannot be edited this way"),
            Self::Misplaced { kind, parent } => {
                write!(f, "a {kind:?} cannot be a child of a {parent:?}")
            }
            Self::TagContext => f.write_str(
                "the fragment's tags were resolved under other %TAG directives than the target \
                 document's",
            ),
            Self::InvalidToken { kind } => {
                write!(f, "the text does not read as exactly one {kind:?} token")
            }
            Self::Syntax { code } => write!(f, "the fragment text does not parse: {code}"),
            Self::EmptyFragment => f.write_str("an empty fragment cannot be inserted"),
            Self::NotWellFormed => f.write_str(
                "the edit is not well-formed: its text would not read back as the edited tree",
            ),
            Self::Limit { code } => write!(f, "the edit would exceed a limit of the tree ({code})"),
            Self::Capacity => f.write_str("the tree's element or byte capacity is exhausted"),
            Self::Internal => f.write_str("internal error in the syntax tree (a bug)"),
        }
    }
}

impl std::error::Error for TreeEditError {}

impl From<StaleNode> for TreeEditError {
    fn from(s: StaleNode) -> Self {
        Self::Stale(s)
    }
}

/// The top-level elements an insertion added, first and last (the same
/// for a one-root fragment).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Inserted {
    /// The first inserted top-level element.
    pub first: NodeId,
    /// The last inserted top-level element.
    pub last: NodeId,
}

/// What a replacement removed and what it inserted.
#[non_exhaustive]
#[must_use = "the removed elements are only kept in `removed`"]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Replaced {
    /// The elements removed, as a detached fragment.
    pub removed: Fragment,
    /// The elements inserted in their place.
    pub inserted: Inserted,
}

/// Overridden structural links of one slot (`NONE` = none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Links {
    first: u32,
    last: u32,
    next: u32,
    prev: u32,
}

/// An inserted arena (its first slot is its key in [`Edits::arenas`]).
#[derive(Clone, Debug)]
pub(super) struct Grafted {
    /// Slot of the parent of the arena's roots.
    parent: u32,
    arena: Arena,
    /// Facts about its roots when they are stream children (`facts.rs`).
    facts: StreamFacts,
    /// Its comment-run index (`runs.rs`).
    runs: RunFacts,
}

/// The edit overlay (see the module docs). Plain data: reads never
/// change it.
#[derive(Clone, Debug)]
pub(super) struct Edits {
    /// The inserted arenas that some run still covers, by first slot (a
    /// run's `arena`). Memory retained after edits stays
    /// within budget: an arena no run covers any more
    /// is dropped once the edit that removed its last run is accepted, with
    /// the link overrides of every slot that edit removed, so what the
    /// overlay retains follows the text in the tree, not the edit history.
    /// A slot of a dropped arena resolves to nothing (stale), never to a
    /// stub; [`SyntaxTree::fork`] and `Clone` copy only these.
    ///
    /// The map names each arena's index in `grafts`, where it
    /// is kept; a run carries that index too (`Run::graft`), so a read
    /// reaches a run's arena in O(1), never through this map.
    arenas: BTreeMap<u32, u32>,
    /// The inserted arenas by index (`slab.rs`: O(1) access,
    /// entries never move; the first few inside the overlay's
    /// box, so a few arenas allocate nothing beyond their own data); free
    /// entries are reused, and the slab is compacted when they outnumber
    /// the live arenas and a quarter of the treap's items
    /// (`Edits::drop_graft`), so it holds what the tree holds, not the
    /// edit history.
    grafts: Slab<Grafted>,
    seq: Seq,
    /// First slot of each run → its item in `seq`.
    runs: BTreeMap<u32, u32>,
    /// Link overrides of live slots (those of removed slots are dropped
    /// when the removal is accepted).
    links: BTreeMap<u32, Links>,
    /// Warnings that left the tree with the text their labels are in
    /// while the element they are attached to stayed (finding
    /// 7: a `Y0028` of a replaced lenient value, attached to the line
    /// break after it): `(slot, n)`, the `n`-th diagnostic attached to
    /// live `slot`. Every diagnostic read skips them; empty (no
    /// allocation) unless such an edit was made, and entries of removed
    /// slots are dropped with them. Arenas stay immutable and every
    /// handle keeps resolving (edits never invalidate
    /// handles of untouched elements).
    dropped: BTreeSet<(u32, u32)>,
    next_slot: u32,
}

impl Edits {
    /// The first slot the next insertion takes.
    pub(super) const fn next_slot(&self) -> u32 {
        self.next_slot
    }

    /// The first slot of item `item`'s run (a run's `arena` is its arena's
    /// first slot, 0 for the base).
    fn run_start(&self, item: u32) -> Option<u32> {
        let run = self.seq.run(item)?;
        run.arena.checked_add(run.a)
    }

    /// An **estimate** of the heap bytes the overlay holds, computed from
    /// its containers' sizes and capacities. It is
    /// complete: the treap, the run and link maps,
    /// the dropped-warning set, and every inserted arena with the heap
    /// inside its diagnostics (messages, labels, help, path, suggestions).
    /// It is an estimate because B-tree nodes are counted at their
    /// entries' size plus 16 bytes of overhead each, and a stream facts
    /// table at its entries. An exact count needs a counting global
    /// allocator; our crates forbid `unsafe`, but a dev-dependency may
    /// provide one (as `allocation-counter` does for the `oracles`
    /// crate's `ser_allocations` test); this crate does not use one.
    #[cfg(test)]
    pub(super) fn heap_bytes(&self) -> usize {
        use core::mem::size_of;
        let entry = |k: usize, v: usize| k.saturating_add(v).saturating_add(16);
        let times = |n: usize, each: usize| n.saturating_mul(each);
        let mut parts = vec![
            self.seq.heap_bytes(),
            times(self.runs.len(), entry(4, 4)),
            times(self.links.len(), entry(4, size_of::<Links>())),
            times(self.dropped.len(), entry(8, 0)),
            times(self.arenas.len(), entry(4, 4)),
            self.grafts.heap_bytes(),
        ];
        for g in self.grafts.iter() {
            let a = &g.arena;
            parts.extend([
                a.source.capacity(),
                a.kinds.capacity(),
                times(a.parents.capacity(), 4),
                times(a.words.capacity(), 4),
                times(a.extras.capacity(), size_of::<(u32, Extra)>()),
                times(a.diagnostics.capacity(), size_of::<(u32, Stored)>()),
                g.facts.heap_bytes(),
                g.runs.heap_bytes(),
            ]);
            parts.extend(
                a.diagnostics
                    .iter()
                    .map(|(_, d)| d.heap_bytes(diagnostic_heap_bytes)),
            );
        }
        parts.into_iter().fold(0, usize::saturating_add)
    }

    /// Some run covers a slot in `lo..hi` (runs never overlap in slot
    /// space, so only the last run starting below `hi` can). O(log R).
    pub(super) fn live_in(&self, lo: u32, hi: u32) -> bool {
        let Some((&start, &item)) = self.runs.range(..hi).next_back() else {
            return false;
        };
        let count = self.seq.run(item).map_or(0, |r| r.b.saturating_sub(r.a));
        start.saturating_add(count) > lo
    }

    /// After an accepted edit, drops what only its
    /// removed slots used: their link overrides, and each inserted arena
    /// no run covers any more. O((k + 1) log L) per removed run, for the k
    /// overrides it drops (each dropped once), plus O(log R) per arena.
    fn release(&mut self, doomed: &[(u32, Run)]) {
        for &(key, run) in doomed {
            let end = key.saturating_add(run.b.saturating_sub(run.a));
            let dead: Vec<u32> = self.links.range(key..end).map(|(s, _)| *s).collect();
            for s in dead {
                self.links.remove(&s);
            }
            if !self.dropped.is_empty() {
                let gone: Vec<(u32, u32)> =
                    self.dropped.range((key, 0)..(end, 0)).copied().collect();
                for d in gone {
                    self.dropped.remove(&d);
                }
            }
            if run.arena == 0 {
                continue;
            }
            let len = self.graft(run.arena).map_or(0, |g| g.arena.kinds.len());
            let limit = u32::try_from(len)
                .ok()
                .and_then(|l| run.arena.checked_add(l))
                .unwrap_or(NONE);
            if self.runs.range(run.arena..limit).next().is_none() {
                self.drop_graft(run.arena, true);
            }
        }
    }

    /// The inserted arena whose first slot is `first`. O(log A).
    fn graft(&self, first: u32) -> Option<&Grafted> {
        cost::count(cost::Step::Lookup);
        let at = *self.arenas.get(&first)?;
        self.graft_at(at)
    }

    /// The inserted arena at index `at` of the slab (`Run::graft`). O(1).
    fn graft_at(&self, at: u32) -> Option<&Grafted> {
        self.grafts.get(at)
    }

    /// Keeps `g` as the arena whose first slot is `first`; returns its
    /// index (a free entry's, else a new one), or `None` (`g` dropped,
    /// nothing else changed) when the index space is exhausted.
    fn add_graft(&mut self, first: u32, g: Grafted) -> Option<u32> {
        let at = self.grafts.insert(g)?;
        self.arenas.insert(first, at);
        Some(at)
    }

    /// Drops the arena whose first slot is `first`. With `compact` (an
    /// accepted edit's release), a slab whose free entries outnumber
    /// `max(16, live arenas, treap items / 4)` is compacted and every run
    /// renumbered. Edit cost is independent of edit
    /// history): a compaction visits the slab's entries (live + free) and
    /// the treap's items, and since the last one at least as many entries
    /// were freed as a quarter of what it visits, each by an accepted edit
    /// (at most a few per edit), so it costs amortised O(1) per edit,
    /// however many runs base removals left. The free entries it leaves are
    /// bounded by the live arenas and runs, never by the history. An undo
    /// never compacts, since the runs its record puts back carry the
    /// indices they had.
    fn drop_graft(&mut self, first: u32, compact: bool) {
        let Some(at) = self.arenas.remove(&first) else {
            return;
        };
        let _ = self.grafts.remove(at);
        let free = self.grafts.free_len();
        let bound = self.arenas.len().max(16).max(self.seq.items_len() / 4);
        if compact && free > bound {
            let (grafts, map) = std::mem::take(&mut self.grafts).compacted();
            self.grafts = grafts;
            for at in self.arenas.values_mut() {
                if let Some(&n) = map.get(*at as usize) {
                    *at = n;
                }
            }
            self.seq.remap_grafts(&map);
        }
    }

    /// Merges the run starting at `slot` into the run before it when the
    /// two are one contiguous range of the same arena. O(log R).
    fn merge_at(&mut self, slot: u32) {
        let Some(&item) = self.runs.get(&slot) else {
            return;
        };
        let (Some(right), Some(left_item)) = (self.seq.run(item), self.seq.prev(item)) else {
            return;
        };
        let Some(left) = self.seq.run(left_item) else {
            return;
        };
        if left.arena == right.arena && left.b == right.a {
            // The halves' marks composed (exactly the marks of
            // the merged range: summaries compose, `runs.rs`).
            let marks = left.marks.then(right.marks);
            let merged = Run {
                b: right.b,
                bytes: left.bytes.saturating_add(right.bytes),
                diags: left.diags.saturating_add(right.diags),
                trivia: left.trivia && right.trivia,
                marks,
                ..left
            };
            self.seq.set_run(left_item, merged);
            self.seq.remove(item);
            self.runs.remove(&slot);
        }
    }
}

/// A step of a walk over stream-level trivia (see
/// [`SyntaxTree::jump_trivia`]).
pub(super) enum Jump {
    /// Go on one element at a time.
    Step,
    /// Every stream child up to this one (`None`: the end) is trivia.
    Past(Option<u32>),
}

/// What a splice changed, to put it back if the edit is refused (never a
/// panic: a refused edit changes nothing).
#[derive(Default)]
struct Undo {
    /// The overlay was created by this edit: drop it.
    created: bool,
    /// The run before the removed runs, by its first slot (`None`: the
    /// front). Runs are named by slot, never by treap item, so
    /// the record stays exact across later edits that were undone (their
    /// undo re-inserts runs as new items) inside an edit group.
    after: Option<u32>,
    /// The removed runs, in order, with their first slots.
    doomed: Vec<(u32, Run)>,
    /// The inserted fragment's first slot (its run's key).
    grafted: Option<u32>,
    arena_pushed: bool,
    next_slot: u32,
    /// Link overrides before the edit (`None`: there was none).
    links: Vec<(u32, Option<Links>)>,
    /// Slots at which this edit split a run in two (merged
    /// back when the edit is refused, so R does not grow with refusals).
    cuts: Vec<u32>,
    /// The document and error counts before the edit.
    documents: usize,
    errors: usize,
    /// Warnings this edit dropped.
    dropped: Vec<(u32, u32)>,
}

/// The open edit groups of a tree: their accepted edits, each
/// with the seams it left, oldest first.
#[derive(Default)]
pub(super) struct Group {
    depth: usize,
    log: Vec<(Undo, Vec<u32>)>,
}

/// The mark of an open edit group ([`SyntaxTree::begin_group`]): pass it
/// to [`SyntaxTree::end_group`] or [`SyntaxTree::rollback_group`].
#[must_use = "an edit group is closed with end_group or rollback_group"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupMark {
    /// The stamp of the tree the group was opened on (a mark of
    /// another tree is refused, whatever its depth).
    tree: u64,
    at: usize,
    depth: usize,
}

/// Where a slot lives.
#[derive(Clone, Copy)]
pub(super) struct Loc<'a> {
    pub(super) local: usize,
    pub(super) flat: Flat<'a>,
    /// Slot of local index 0.
    first: u32,
    /// Slot of the roots' parent (`NONE` for the base).
    attach: u32,
}

impl Loc<'_> {
    fn slot(&self, local: usize) -> Option<u32> {
        u32::try_from(local)
            .ok()
            .and_then(|l| self.first.checked_add(l))
    }
}

/// A diagnostic met by [`SyntaxTree::scan_diagnostics`]: its element's
/// start in the current text, and the shift (`from` → `to`) that
/// moves its spans there.
pub(super) struct DiagAt<'a> {
    /// The element's rank (preorder index) in the current tree.
    pub(super) rank: u64,
    pub(super) start: u64,
    pub(super) diagnostic: &'a Stored,
    pub(super) from: usize,
    pub(super) to: usize,
}

/// What [`SyntaxTree::splice`] removed, and the first and last inserted
/// top-level slots.
pub(super) type Spliced = (Option<Fragment>, Option<(u32, u32)>);

/// One splice of [`SyntaxTree::splice_pair`]: under `parent`, after
/// `prev`, remove the siblings `removed` and insert `fragment`.
pub(super) struct SpliceArgs {
    pub(super) parent: u32,
    pub(super) prev: Option<u32>,
    pub(super) removed: Vec<u32>,
    pub(super) fragment: Option<Fragment>,
}

/// What the well-formedness check of an applied splice needs.
struct Check {
    parent: u32,
    prev: Option<u32>,
    next: Option<u32>,
    what: Removed,
}

/// An applied, not yet validated splice.
struct Applied {
    taken: Option<Fragment>,
    span: Option<(u32, u32)>,
    undo: Undo,
    check: Check,
}

/// Where a splice goes, read before anything changes.
#[derive(Clone, Copy)]
struct Plan {
    parent: u32,
    prev: Option<u32>,
    next: Option<u32>,
    /// Insertion goes right before `cut` (`None`: at the very end).
    cut: Option<u32>,
    /// The first slot of the inserted arena.
    new_first: Option<u32>,
}

/// Token kinds whose text [`SyntaxTree::replace_token`] replaces.
const REPLACEABLE: &[SyntaxKind] = &[
    SyntaxKind::PlainScalar,
    SyntaxKind::SingleQuotedScalar,
    SyntaxKind::DoubleQuotedScalar,
    SyntaxKind::AliasName,
    SyntaxKind::Anchor,
    SyntaxKind::Comment,
    SyntaxKind::Whitespace,
    SyntaxKind::LineBreak,
];

const fn is_directive(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::YamlDirective | SyntaxKind::TagDirective | SyntaxKind::ReservedDirective
    )
}

/// Tokens whose meaning is carried by their node's event data (the
/// the extras), so they move only with their node.
const fn is_node_property(k: SyntaxKind) -> bool {
    matches!(k, SyntaxKind::Tag | SyntaxKind::BlockScalarHeader)
}

impl SyntaxTree {
    // ============================================================ access

    pub(super) fn base_flat(&self) -> Flat<'_> {
        Flat {
            source: &self.source,
            tiled: self.source.len(),
            kinds: &self.kinds,
            parents: &self.parents,
            words: &self.words,
            wraps: &self.wraps,
            extras: &self.extras,
            diagnostics: &self.diagnostics,
        }
    }

    /// The number of error-severity diagnostics in the tree, kept up to
    /// date by every edit (O(1)). Accepted edits never change it
    /// (they neither create nor remove an error region); a caller that
    /// derives state from the errors can check that here.
    #[must_use]
    pub const fn error_count(&self) -> usize {
        self.errors
    }

    /// `true` once an edit has been applied.
    #[must_use]
    pub fn is_edited(&self) -> bool {
        self.edits.is_some()
    }

    /// Where `slot` lives (live or not). Inline, so a caller reading one
    /// field of a base slot's place builds only that (out of
    /// line, every call built the whole `Loc`).
    #[inline]
    pub(super) fn locate(&self, slot: u32) -> Option<Loc<'_>> {
        if (slot as usize) < self.kinds.len() {
            return Some(Loc {
                local: slot as usize,
                flat: self.base_flat(),
                first: 0,
                attach: NONE,
            });
        }
        self.locate_grafted(slot)
    }

    /// [`SyntaxTree::locate`] for a slot past the base: through the arena
    /// map. O(log A).
    #[inline]
    fn locate_grafted(&self, slot: u32) -> Option<Loc<'_>> {
        let e = self.edits.as_deref()?;
        // A slot of a dropped arena falls in no arena's range (or
        // past the end of an earlier one): nothing, never a stub.
        cost::count(cost::Step::Lookup);
        let (&first, &at) = e.arenas.range(..=slot).next_back()?;
        let ins = e.graft_at(at)?;
        let local = slot.checked_sub(first)? as usize;
        (local < ins.arena.kinds.len()).then(|| Loc {
            local,
            flat: ins.arena.flat(),
            first,
            attach: ins.parent,
        })
    }

    /// The stream facts of `slot`'s arena, `slot`'s local index there, and
    /// the slot of local index 0 (`facts.rs`). O(log arenas).
    pub(super) fn facts_at(&self, slot: u32) -> Option<(&StreamFacts, usize, u32)> {
        if (slot as usize) < self.kinds.len() {
            return Some((&self.facts, slot as usize, 0));
        }
        let e = self.edits.as_deref()?;
        cost::count(cost::Step::Lookup);
        let (&first, &at) = e.arenas.range(..=slot).next_back()?;
        let ins = e.graft_at(at)?;
        let local = slot.checked_sub(first)? as usize;
        (local < ins.arena.kinds.len()).then_some((&ins.facts, local, first))
    }

    /// For a live stream-level trivia token `p` inside a long run of its
    /// arena (`facts.rs`): the first element of that run that is still
    /// contiguous with `p` in the tree's order (the run's start, or the
    /// start of the overlay run holding `p`); else `p`. Every element
    /// from there to `p` is a stream-level trivia token, and they are
    /// consecutive siblings (inside one overlay run the arena's order and
    /// links hold). O(log R + log runs).
    pub(super) fn trivia_run_start(&self, p: u32) -> u32 {
        let Some((facts, local, first)) = self.facts_at(p) else {
            return p;
        };
        let Some(a) = facts.run_start(local) else {
            return p;
        };
        let lo = match self.run_item(p) {
            Some((_, run, _)) => a.max(run.a as usize),
            None if self.edits.is_none() => a,
            None => return p,
        };
        u32::try_from(lo)
            .ok()
            .and_then(|l| first.checked_add(l))
            .filter(|s| *s <= p)
            .unwrap_or(p)
    }

    /// For a live stream-level trivia token `p` inside a long run of its
    /// arena: the last element of that run still contiguous with `p` in
    /// the tree's order (the run's end, or the end of the overlay run
    /// holding `p`); else `p`. The mirror of
    /// [`SyntaxTree::trivia_run_start`].
    pub(super) fn trivia_run_end(&self, p: u32) -> u32 {
        let Some((facts, local, first)) = self.facts_at(p) else {
            return p;
        };
        let Some(b) = facts.run_end(local) else {
            return p;
        };
        let hi = match self.run_item(p) {
            Some((_, run, _)) => b.min((run.b as usize).saturating_sub(1)),
            None if self.edits.is_none() => b,
            None => return p,
        };
        u32::try_from(hi)
            .ok()
            .and_then(|h| first.checked_add(h))
            .filter(|s| *s >= p)
            .unwrap_or(p)
    }

    /// For stream-level trivia token `s` at the start (`forward`: the end)
    /// of its overlay run, when the run before (after) it is all
    /// stream-level trivia: the stream child past every such run in that
    /// direction, found by the treap's sums in O(log R). The
    /// runs skipped hold only stream-level trivia tokens, so they are
    /// consecutive stream children, and the element next to them is the
    /// last element of a stream child (backwards: climbed to it, O(depth))
    /// or a stream child (forwards: the first element after a stream-level
    /// token).
    pub(super) fn jump_trivia(&self, s: u32, forward: bool) -> Result<Jump, TreeEditError> {
        let Some(e) = self.edits.as_deref() else {
            return Ok(Jump::Step);
        };
        let (item, run, start) = self.run_item(s).ok_or(TreeEditError::Internal)?;
        let at_edge = if forward {
            run.b
                .checked_sub(run.a)
                .and_then(|n| start.checked_add(n))
                .and_then(|end| end.checked_sub(1))
                == Some(s)
        } else {
            start == s
        };
        let neighbour = if forward {
            e.seq.next(item)
        } else {
            e.seq.prev(item)
        };
        let trivia_next = neighbour
            .and_then(|n| e.seq.run(n))
            .is_some_and(|r| r.trivia);
        if !at_edge || !trivia_next {
            return Ok(Jump::Step);
        }
        let solid = if forward {
            e.seq.next_solid(item)
        } else {
            e.seq.prev_solid(item)
        };
        let Some(j) = solid else {
            return Ok(Jump::Past(None));
        };
        let r = e.seq.run(j).ok_or(TreeEditError::Internal)?;
        if forward {
            let first = r.arena.checked_add(r.a).ok_or(TreeEditError::Internal)?;
            return Ok(Jump::Past(Some(first)));
        }
        let mut y =
            r.b.checked_sub(1)
                .and_then(|l| r.arena.checked_add(l))
                .ok_or(TreeEditError::Internal)?;
        // A loop up the parents. The stream root itself (a run that
        // ends with it): the trivia are the stream's first children.
        loop {
            match self.nav_parent(y) {
                Some(0) => return Ok(Jump::Past(Some(y))),
                Some(p) => y = p,
                None if y == 0 => return Ok(Jump::Past(None)),
                None => return Err(TreeEditError::Internal),
            }
        }
    }

    /// Local elements `a..b` of arena `arena` are all stream-level trivia
    /// tokens. O(`LONG_RUN` + log): a longer range is one only
    /// inside a long run its arena's facts record.
    fn stream_trivia(&self, arena: u32, a: u32, b: u32) -> bool {
        let Some((flat, _, attach)) = self.arena_flat(arena) else {
            return false;
        };
        let facts = if arena == 0 {
            &self.facts
        } else {
            match self.edits.as_deref().and_then(|e| e.graft(arena)) {
                Some(g) => &g.facts,
                None => return false,
            }
        };
        (arena == 0 || attach == 0) && all_stream_trivia(flat, facts, arena == 0, a, b)
    }

    /// The comment-run marks of local elements `a..b` of arena `arena`
    /// (`runs.rs`).
    fn run_marks(&self, arena: u32, a: u32, b: u32) -> Marks {
        match self.arena_index(arena) {
            Some((flat, rf)) => marks(flat, rf, self.stream_facts_of(arena), a, b),
            // (Unreachable: a run's arena is live. Marks that claim content
            // make every search look inside, never skip it.)
            None => Marks::CONTENT,
        }
    }

    /// Arena `arena`'s elements and comment-run index (`runs.rs`).
    pub(super) fn arena_index(&self, arena: u32) -> Option<(Flat<'_>, &RunFacts)> {
        if arena == 0 {
            return Some((self.base_flat(), &self.run_facts));
        }
        let g = self.edits.as_deref()?.graft(arena)?;
        Some((g.arena.flat(), &g.runs))
    }

    /// Arena `arena`'s stream facts, when its roots are stream-level (the
    /// base, or a fragment inserted into the stream).
    pub(super) fn stream_facts_of(&self, arena: u32) -> Option<&StreamFacts> {
        if arena == 0 {
            return Some(&self.facts);
        }
        let g = self.edits.as_deref()?.graft(arena)?;
        (g.parent == 0).then_some(&g.facts)
    }

    /// The piece of the tree's order holding `slot`, and `slot`'s local
    /// index there (`runs.rs`). O(log R).
    pub(super) fn piece_at(&self, slot: u32) -> Option<(Piece, u32)> {
        if self.edits.is_none() {
            let len = u32::try_from(self.kinds.len()).ok()?;
            let p = Piece {
                arena: 0,
                a: 0,
                b: len,
                item: None,
            };
            return (slot < len).then_some((p, slot));
        }
        let (item, run, start) = self.run_item(slot)?;
        let local = run.a.checked_add(slot.checked_sub(start)?)?;
        let p = Piece {
            arena: run.arena,
            a: run.a,
            b: run.b,
            item: Some(item),
        };
        Some((p, local))
    }

    /// The piece of overlay item `item`.
    fn piece_of_item(&self, item: u32) -> Option<Piece> {
        let run = self.edits.as_deref()?.seq.run(item)?;
        Some(Piece {
            arena: run.arena,
            a: run.a,
            b: run.b,
            item: Some(item),
        })
    }

    /// The first piece after item `item` that holds a content token or
    /// may hold an own-line comment at a column below `v`. O(log R).
    pub(super) fn piece_after(&self, item: u32, v: u32) -> Option<Piece> {
        let next = self.edits.as_deref()?.seq.next_low(item, v)?;
        self.piece_of_item(next)
    }

    /// The last piece before item `item` that holds a content token.
    /// O(log R).
    pub(super) fn piece_visible_before(&self, item: u32) -> Option<Piece> {
        let prev = self.edits.as_deref()?.seq.prev_visible(item)?;
        self.piece_of_item(prev)
    }

    /// The nearest piece after (`forward`) or before item `item` holding
    /// an element that is not a trivia token. O(log R).
    pub(super) fn piece_firm(&self, item: u32, forward: bool) -> Option<Piece> {
        let at = self.edits.as_deref()?.seq.near_firm(item, forward)?;
        self.piece_of_item(at)
    }

    /// The nearest piece after (`forward`) or before item `item` holding a
    /// stream-level document. O(log R).
    pub(super) fn piece_docs(&self, item: u32, forward: bool) -> Option<Piece> {
        let e = self.edits.as_deref()?;
        let at = if forward {
            e.seq.next_docs(item)?
        } else {
            e.seq.prev_docs(item)?
        };
        self.piece_of_item(at)
    }

    fn arena_flat(&self, id: u32) -> Option<(Flat<'_>, u32, u32)> {
        if id == 0 {
            return Some((self.base_flat(), 0, NONE));
        }
        let e = self.edits.as_deref()?;
        let ins = e.graft(id)?;
        Some((ins.arena.flat(), id, ins.parent))
    }

    /// The arena of `run` (O(1)): its elements, its first slot,
    /// and the slot of its roots' parent (`NONE` for the base).
    pub(super) fn run_flat(&self, run: &Run) -> Option<(Flat<'_>, u32, u32)> {
        self.graft_flat(run.arena, run.graft)
    }

    /// The arena whose first slot is `arena` and whose slab index is
    /// `graft` (O(1)): its elements, first slot, and roots' parent slot.
    #[inline(always)]
    fn graft_flat(&self, arena: u32, graft: u32) -> Option<(Flat<'_>, u32, u32)> {
        if arena == 0 {
            return Some((self.base_flat(), 0, NONE));
        }
        let ins = self.edits.as_deref()?.graft_at(graft)?;
        Some((ins.arena.flat(), arena, ins.parent))
    }

    /// The run covering `slot`: (item, run, slot of the run's first element).
    pub(super) fn run_item(&self, slot: u32) -> Option<(u32, Run, u32)> {
        let e = self.edits.as_deref()?;
        cost::count(cost::Step::Lookup);
        let (&start, &item) = e.runs.range(..=slot).next_back()?;
        let run = e.seq.run(item)?;
        let count = run.b.checked_sub(run.a)?;
        (slot < start.checked_add(count)?).then_some((item, run, start))
    }

    /// `true` when `slot` names a live element.
    pub(super) fn is_live(&self, slot: u32) -> bool {
        if self.edits.is_none() {
            return (slot as usize) < self.kinds.len();
        }
        self.run_item(slot).is_some()
    }

    pub(super) fn kind_of(&self, slot: u32) -> Option<SyntaxKind> {
        let loc = self.locate(slot)?;
        loc.flat.kind(loc.local)
    }

    // ======================================================== navigation

    /// All four links of `slot` as its arena has them. O(depth): only for
    /// edits, never on a read path.
    fn default_links(loc: &Loc<'_>) -> Links {
        let f = &loc.flat;
        let l = loc.local;
        let s = |x: Option<usize>| x.and_then(|x| loc.slot(x)).unwrap_or(NONE);
        Links {
            first: s(f.first_child(l)),
            last: s(f.last_child(l)),
            next: s(f.next_sibling(l)),
            prev: s(f.prev_sibling(l)),
        }
    }

    fn override_of(&self, slot: u32) -> Option<&Links> {
        let e = self.edits.as_deref()?;
        cost::count(cost::Step::Lookup);
        e.links.get(&slot)
    }

    /// All four links of `slot` (for an edit to change one of them).
    fn links_of(&self, slot: u32) -> Links {
        if let Some(l) = self.override_of(slot) {
            return *l;
        }
        self.locate(slot).map_or(
            Links {
                first: NONE,
                last: NONE,
                next: NONE,
                prev: NONE,
            },
            |loc| Self::default_links(&loc),
        )
    }

    fn some(x: u32) -> Option<u32> {
        (x != NONE).then_some(x)
    }

    /// One link: the override if an edit set one, else what the arena
    /// says. Only the link asked for is computed, so the O(1) links
    /// stay O(1) (finding: computing all four made every step O(depth)).
    fn nav_link(
        &self,
        slot: u32,
        pick: fn(&Links) -> u32,
        local: impl FnOnce(&Flat<'_>, usize) -> Option<usize>,
    ) -> Option<u32> {
        if let Some(l) = self.override_of(slot) {
            return Self::some(pick(l));
        }
        let loc = self.locate(slot)?;
        local(&loc.flat, loc.local).and_then(|x| loc.slot(x))
    }

    pub(super) fn nav_parent(&self, slot: u32) -> Option<u32> {
        if (slot as usize) < self.kinds.len() {
            // A base element's parent is its base parent.
            return self
                .parent_of(slot as usize)
                .and_then(|p| u32::try_from(p).ok());
        }
        let loc = self.locate(slot)?;
        match loc.flat.parent(loc.local) {
            Some(p) => loc.slot(p),
            None => Self::some(loc.attach),
        }
    }

    /// O(1) (plus O(log) to find an override).
    pub(super) fn nav_first(&self, slot: u32) -> Option<u32> {
        self.nav_link(slot, |l| l.first, |f, i| f.first_child(i))
    }

    /// O(depth), as in an unedited tree.
    pub(super) fn nav_last(&self, slot: u32) -> Option<u32> {
        self.nav_link(slot, |l| l.last, |f, i| f.last_child(i))
    }

    /// O(1) (plus O(log) to find an override).
    pub(super) fn nav_next(&self, slot: u32) -> Option<u32> {
        self.nav_link(slot, |l| l.next, |f, i| f.next_sibling(i))
    }

    /// O(depth), as in an unedited tree.
    pub(super) fn nav_prev(&self, slot: u32) -> Option<u32> {
        self.nav_link(slot, |l| l.prev, |f, i| f.prev_sibling(i))
    }

    /// The next element in preorder. O(1), O(log R) across runs.
    pub(super) fn nav_succ(&self, slot: u32) -> Option<u32> {
        let Some(e) = self.edits.as_deref() else {
            let n = slot.checked_add(1)?;
            return ((n as usize) < self.kinds.len()).then_some(n);
        };
        let (item, run, start) = self.run_item(slot)?;
        let n = slot.checked_add(1)?;
        if n < start.checked_add(run.b.checked_sub(run.a)?)? {
            return Some(n);
        }
        let next = e.seq.next(item)?;
        let r = e.seq.run(next)?;
        let first = r.arena;
        first.checked_add(r.a)
    }

    /// The previous element in preorder. O(1), O(log R) across runs.
    pub(super) fn nav_pred(&self, slot: u32) -> Option<u32> {
        let Some(e) = self.edits.as_deref() else {
            return slot.checked_sub(1);
        };
        let (item, _, start) = self.run_item(slot)?;
        if slot > start {
            return slot.checked_sub(1);
        }
        let prev = e.seq.prev(item)?;
        let r = e.seq.run(prev)?;
        let first = r.arena;
        first.checked_add(r.b.checked_sub(1)?)
    }

    // ================================================ hinted reads
    //
    // A `Node` of an edited tree carries the treap item of
    // its run as a **hint**. A `Node` borrows its tree immutably, so the
    // overlay cannot change while the hint exists; it is checked anyway
    // (O(1)), and a hint that does not cover the slot is looked up again.
    // With the run at hand, a step that stays inside the run is the
    // arena's own O(1) arithmetic (`Flat`), with no map lookup. Why this
    // is exact is structural: parents are always the arena's
    // (`Links` has no parent field, and an edit never moves an element, it
    // copies it), and inside one run the tree's preorder is the arena's.
    // A first child is the element right after its parent in preorder when
    // that one is a child of it, a next sibling the element right after
    // its subtree when that one has the same parent, and so on: when those
    // elements lie in the run, the tree's link is determined by the
    // arena's order and parents, whatever overrides exist. (An override
    // may sit inside a run, e.g. after an insertion and its removal merged
    // the runs back; it then equals the arena's link, as every override
    // equals the tree's.) Only a step that leaves the run reads the link
    // overrides, and the next run is the treap successor (amortised O(1)
    // over a traversal). Sequential reads thus cost
    // amortised O(1) per element and O(1) lookups per run, independent of
    // the number of elements; the `cost` counters state it in the tests.

    /// The run covering live `slot` and the slot of its first element:
    /// from `hint` in O(1) when it is that run's item, else looked up
    /// (O(log R)).
    #[inline(always)]
    pub(super) fn run_hinted(&self, slot: u32, hint: u32) -> Option<(RunAt, u32)> {
        if hint != NONE {
            if let Some(r) = self.edits.as_deref().and_then(|e| e.seq.at(hint)) {
                let start = r.arena.checked_add(r.a)?;
                let end = r.arena.checked_add(r.b)?;
                if start <= slot && slot < end {
                    return Some((r, start));
                }
            }
        }
        let (item, run, start) = self.run_item(slot)?;
        let r = RunAt {
            item,
            arena: run.arena,
            graft: run.graft,
            a: run.a,
            b: run.b,
        };
        Some((r, start))
    }

    /// The slots `start..end` of the run of treap item `item`, or `(0,
    /// 0)` for none. O(1).
    #[inline]
    pub(super) fn run_slots(&self, item: u32) -> (u32, u32) {
        let bounds = self.edits.as_deref().and_then(|e| e.seq.bounds(item));
        let Some((arena, a, b)) = bounds else {
            return (0, 0);
        };
        match (arena.checked_add(a), arena.checked_add(b)) {
            (Some(s), Some(t)) => (s, t),
            _ => (0, 0),
        }
    }

    /// The run after item `item`: its item and its slots `start..end`.
    /// Amortised O(1) over a traversal (a treap step).
    pub(super) fn run_after(&self, item: u32) -> Option<(u32, u32, u32)> {
        let next = self.edits.as_deref()?.seq.next(item)?;
        let (s, t) = self.run_slots(next);
        (t > s).then_some((next, s, t))
    }

    /// For a base `slot` whose `hint` is its run's item: the run's base
    /// indices `a..b` (a base run's slots are its base indices).
    /// O(1), nothing copied.
    #[inline]
    fn base_run(&self, slot: u32, hint: u32) -> Option<(usize, usize)> {
        if hint == NONE || (slot as usize) >= self.kinds.len() {
            return None;
        }
        let (arena, a, b) = self.edits.as_deref()?.seq.bounds(hint)?;
        (arena == 0 && a <= slot && slot < b).then_some((a as usize, b as usize))
    }

    /// The parent's slot (the base arrays for a base slot, the
    /// hint's arena for an inserted one; no run lookup with a hint).
    #[inline]
    pub(super) fn parent_slot(&self, slot: u32, hint: u32) -> Option<u32> {
        if (slot as usize) < self.kinds.len() {
            return self
                .parent_of(slot as usize)
                .and_then(|p| u32::try_from(p).ok());
        }
        self.nav_parent_hinted(slot, hint).map(|(p, _)| p)
    }

    /// Lowers the arena slab's index space to the entries it has now plus
    /// `spare` (for tests: running out).
    #[cfg(test)]
    pub(super) fn limit_arenas(&mut self, spare: usize) {
        if let Some(e) = self.edits.as_deref_mut() {
            let n = e.grafts.len().saturating_add(spare);
            e.grafts.limit_entries(n);
        }
    }

    /// Live inserted arenas, and free entries of the slab (for
    /// tests).
    #[cfg(test)]
    pub(super) fn arena_counts(&self) -> (usize, usize) {
        self.edits
            .as_deref()
            .map_or((0, 0), |e| (e.arenas.len(), e.grafts.free_len()))
    }

    /// The element count of each run, in order, and the number of runs
    /// the run map names (for tests).
    #[cfg(test)]
    pub(super) fn run_lengths(&self) -> (Vec<u32>, usize) {
        let Some(e) = self.edits.as_deref() else {
            return (Vec::new(), 0);
        };
        let mut out = Vec::new();
        let mut at = e.seq.first();
        while let Some(x) = at {
            out.extend(e.seq.run(x).map(|r| r.b.saturating_sub(r.a)));
            at = e.seq.next(x);
        }
        (out, e.runs.len())
    }

    /// The hint of live `slot`: its run's item (O(log R)); `NONE` for an
    /// unedited tree (no hint is needed there).
    pub(super) fn hint_of(&self, slot: u32) -> u32 {
        if self.edits.is_none() {
            return NONE;
        }
        self.run_item(slot).map_or(NONE, |(item, _, _)| item)
    }

    /// Where `slot` lives in `run`'s arena. O(1).
    #[inline(always)]
    fn loc_in_run(&self, run: &RunAt, slot: u32) -> Option<Loc<'_>> {
        let (flat, first, attach) = self.graft_flat(run.arena, run.graft)?;
        Some(Loc {
            local: slot.checked_sub(first)? as usize,
            flat,
            first,
            attach,
        })
    }

    /// [`SyntaxTree::locate`] through a hint: O(1) for a base slot (as
    /// always) and for an inserted slot whose hint holds it.
    #[inline(never)]
    pub(super) fn locate_hinted(&self, slot: u32, hint: u32) -> Option<Loc<'_>> {
        if (slot as usize) < self.kinds.len() || hint == NONE {
            return self.locate(slot);
        }
        match self.run_hinted(slot, hint) {
            Some((run, _)) => self.loc_in_run(&run, slot),
            None => self.locate(slot),
        }
    }

    /// The kind of live `slot` through a hint (a base slot's from the
    /// base arrays, O(1)).
    pub(super) fn kind_hinted(&self, slot: u32, hint: u32) -> Option<SyntaxKind> {
        if let Some(k) = self.kind_at(slot as usize) {
            return Some(k);
        }
        // An inserted slot: its arena's kinds, straight from the slab.
        let e = self.edits.as_deref()?;
        if let Some(r) = e.seq.at(hint) {
            let start = r.arena.checked_add(r.a)?;
            let end = r.arena.checked_add(r.b)?;
            if start <= slot && slot < end {
                let g = e.graft_at(r.graft)?;
                return g
                    .arena
                    .kinds
                    .get(slot.checked_sub(r.arena)? as usize)
                    .copied();
            }
        }
        let loc = self.locate(slot)?;
        loc.flat.kind(loc.local)
    }

    /// The run of live `slot` (from `hint`) with `slot`'s place in it.
    #[inline(always)]
    fn at_hinted(&self, slot: u32, hint: u32) -> Option<(u32, RunAt, Loc<'_>)> {
        let (run, _) = self.run_hinted(slot, hint)?;
        let loc = self.loc_in_run(&run, slot)?;
        Some((run.item, run, loc))
    }

    /// The hint of `slot`, the first element of the run after item
    /// `item`, when that run starts with it (else `NONE`). O(1) amortised
    /// over a traversal (a treap step).
    fn hint_after(&self, item: u32, slot: u32) -> u32 {
        let Some(e) = self.edits.as_deref() else {
            return NONE;
        };
        e.seq
            .next(item)
            .filter(|n| e.run_start(*n) == Some(slot))
            .unwrap_or(NONE)
    }

    /// The next element in preorder and its hint. O(1) inside a run; a
    /// treap step to the next run.
    pub(super) fn nav_succ_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        let Some(e) = self.edits.as_deref() else {
            return self.nav_succ(slot).map(|s| (s, NONE));
        };
        let (run, start) = self.run_hinted(slot, hint)?;
        let item = run.item;
        let n = slot.checked_add(1)?;
        if n < start.checked_add(run.b.checked_sub(run.a)?)? {
            return Some((n, item));
        }
        let next = e.seq.next(item)?;
        let r = e.seq.run(next)?;
        Some((r.arena.checked_add(r.a)?, next))
    }

    /// The previous element in preorder and its hint.
    pub(super) fn nav_pred_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        let Some(e) = self.edits.as_deref() else {
            return self.nav_pred(slot).map(|s| (s, NONE));
        };
        let (run, start) = self.run_hinted(slot, hint)?;
        let item = run.item;
        if slot > start {
            return Some((slot.checked_sub(1)?, item));
        }
        let prev = e.seq.prev(item)?;
        let r = e.seq.run(prev)?;
        Some((r.arena.checked_add(r.b.checked_sub(1)?)?, prev))
    }

    /// The parent and its hint (the same run when the parent lies in it).
    /// Without a hint, [`SyntaxTree::nav_parent`] (O(1) for a base slot).
    pub(super) fn nav_parent_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        if hint == NONE || self.edits.is_none() {
            return self.nav_parent(slot).map(|p| (p, NONE));
        }
        if let Some((a, _)) = self.base_run(slot, hint) {
            let p = self.parent_of(slot as usize)?;
            let h = if p >= a { hint } else { NONE };
            return Some((u32::try_from(p).ok()?, h));
        }
        let (item, run, loc) = self.at_hinted(slot, hint)?;
        match loc.flat.parent(loc.local) {
            // A live element's parent is live; inside the run when it lies
            // at or after the run's first element.
            Some(p) => Some((loc.slot(p)?, if p >= run.a as usize { item } else { NONE })),
            None => Self::some(loc.attach).map(|p| (p, NONE)),
        }
    }

    /// The first child and its hint. O(1) unless `slot` ends its run.
    pub(super) fn nav_first_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        // Without a hint, exactly the unhinted step (no run lookup on
        // top of the override lookup).
        if self.edits.is_none() || hint == NONE {
            return self.nav_first(slot).map(|s| (s, NONE));
        }
        if let Some((_, b)) = self.base_run(slot, hint) {
            let i = slot as usize;
            let c = i.checked_add(1)?;
            if c < b {
                // (As below, by the base arrays.)
                return (c < self.end_of(i))
                    .then(|| u32::try_from(c).ok())
                    .flatten()
                    .map(|c| (c, hint));
            }
            // `slot` ends its run: the fallback below, without re-reading
            // the run.
            let c = self.nav_first(slot)?;
            return Some((c, self.hint_after(hint, c)));
        }
        let (item, run, loc) = self.at_hinted(slot, hint)?;
        let next = loc.local.checked_add(1)?;
        if next < run.b as usize {
            // The element after `slot` is the arena's next one: a child
            // exactly when the arena says so.
            return loc
                .flat
                .first_child(loc.local)
                .and_then(|c| loc.slot(c))
                .map(|c| (c, item));
        }
        let c = self.nav_first(slot)?;
        Some((c, self.hint_after(item, c)))
    }

    /// The next sibling and its hint. O(1) unless `slot`'s subtree reaches
    /// the end of its run.
    pub(super) fn nav_next_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        // Without a hint, exactly the unhinted step (no run lookup on
        // top of the override lookup).
        if self.edits.is_none() || hint == NONE {
            return self.nav_next(slot).map(|s| (s, NONE));
        }
        if let Some((_, b)) = self.base_run(slot, hint) {
            let i = slot as usize;
            let after = self.end_of(i);
            if after < b {
                // (As below, by the base arrays.)
                let limit = self
                    .parent_of(i)
                    .map_or(self.kinds.len(), |p| self.end_of(p));
                return (after < limit && after > i)
                    .then(|| u32::try_from(after).ok())
                    .flatten()
                    .map(|s| (s, hint));
            }
            // The subtree reaches the run's end: the fallback below,
            // without re-reading the run.
            let s = self.nav_next(slot)?;
            let h = if after == b {
                self.hint_after(hint, s)
            } else {
                NONE
            };
            return Some((s, h));
        }
        let (item, run, loc) = self.at_hinted(slot, hint)?;
        let end = loc.flat.end(loc.local);
        if end < run.b as usize {
            // The subtree and the element after it lie in the run: the
            // arena's next sibling is the tree's.
            return loc
                .flat
                .next_sibling(loc.local)
                .and_then(|s| loc.slot(s))
                .map(|s| (s, item));
        }
        let s = self.nav_next(slot)?;
        // When the subtree ends the run, the next run may start with it.
        let h = if end == run.b as usize {
            self.hint_after(item, s)
        } else {
            NONE
        };
        Some((s, h))
    }

    /// The previous sibling and its hint. O(depth), as unedited; no
    /// override lookup unless `slot` starts its run.
    pub(super) fn nav_prev_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        // Without a hint, exactly the unhinted step (no run lookup on
        // top of the override lookup).
        if self.edits.is_none() || hint == NONE {
            return self.nav_prev(slot).map(|s| (s, NONE));
        }
        let (item, run, loc) = self.at_hinted(slot, hint)?;
        if loc.local > run.a as usize {
            // The element before `slot` is the arena's previous one, the
            // last of the previous sibling's subtree (or the parent).
            return loc.flat.prev_sibling(loc.local).and_then(|s| {
                let h = if s >= run.a as usize { item } else { NONE };
                Some((loc.slot(s)?, h))
            });
        }
        self.nav_prev(slot).map(|s| (s, NONE))
    }

    /// The last child and its hint. O(depth), as unedited; no override
    /// lookup unless `slot`'s subtree reaches the end of its run.
    pub(super) fn nav_last_hinted(&self, slot: u32, hint: u32) -> Option<(u32, u32)> {
        // Without a hint, exactly the unhinted step (no run lookup on
        // top of the override lookup).
        if self.edits.is_none() || hint == NONE {
            return self.nav_last(slot).map(|s| (s, NONE));
        }
        let (item, run, loc) = self.at_hinted(slot, hint)?;
        if loc.flat.end(loc.local) < run.b as usize {
            return loc
                .flat
                .last_child(loc.local)
                .and_then(|c| loc.slot(c))
                .map(|c| (c, item));
        }
        self.nav_last(slot).map(|s| (s, NONE))
    }

    /// (rank, byte offset) of live `slot` through a hint. O(log R) (the
    /// treap's prefix sums), without the run lookup.
    pub(super) fn nav_rank_hinted(&self, slot: u32, hint: u32) -> Option<(u64, u64)> {
        let Some(e) = self.edits.as_deref() else {
            return self.nav_rank(slot);
        };
        let (item, run, loc) = self.at_hinted(slot, hint)?;
        let (pc, pb) = e.seq.prefix(item);
        let within = u64::try_from(loc.local.checked_sub(run.a as usize)?).ok()?;
        let bytes = loc
            .flat
            .start(loc.local)
            .checked_sub(loc.flat.start(run.a as usize))?;
        Some((
            pc.checked_add(within)?,
            pb.checked_add(u64::try_from(bytes).ok()?)?,
        ))
    }

    /// The span of live `slot` in the edited text, through a hint.
    pub(super) fn nav_span_hinted(&self, slot: u32, hint: u32) -> Span {
        let Some((_, start)) = self.nav_rank_hinted(slot, hint) else {
            return Span::empty(0);
        };
        let end = self
            .nav_after(slot)
            .and_then(|a| self.nav_rank(a))
            .map_or(self.nav_total().1, |(_, b)| b);
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let end = usize::try_from(end).unwrap_or(usize::MAX).max(start);
        Span::new(start, end)
    }

    /// The text of live `slot` through a hint: a token's is borrowed from
    /// its arena in O(1).
    pub(super) fn edited_text_hinted(&self, slot: u32, hint: u32) -> Cow<'_, str> {
        let i = slot as usize;
        if self.kind_at(i).is_some_and(SyntaxKind::is_token) {
            // A base token's bytes are its bytes in the source.
            let s = self.span_of(i);
            return Cow::Borrowed(self.source.get(s.start..s.end).unwrap_or(""));
        }
        if let Some(loc) = self.locate_hinted(slot, hint) {
            if loc.flat.kind(loc.local).is_some_and(SyntaxKind::is_token) {
                return Cow::Borrowed(loc.flat.token_text(loc.local));
            }
        }
        self.edited_text(slot)
    }

    /// The last element of `slot`'s subtree in preorder (itself when it
    /// has no children): the end of its chain of last children.
    /// O(log R) plus [`SyntaxTree::nav_after`]'s climb, where
    /// following [`SyntaxTree::nav_last`] level by level costs O(depth)
    /// per level, O(depth²) in all.
    pub(super) fn nav_last_leaf(&self, slot: u32) -> Option<u32> {
        if self.nav_first(slot).is_none() {
            return self.is_live(slot).then_some(slot);
        }
        match self.nav_after(slot) {
            Some(a) => self.nav_pred(a),
            None => match self.edits.as_deref() {
                None => u32::try_from(self.kinds.len().checked_sub(1)?).ok(),
                Some(e) => {
                    let r = e.seq.run(e.seq.last()?)?;
                    let first = r.arena;
                    first.checked_add(r.b.checked_sub(1)?)
                }
            },
        }
    }

    /// The chain of last children from `slot` down: `slot`, its last
    /// child, that one's last child, … (O(depth + log R), see
    /// [`SyntaxTree::nav_last_leaf`]).
    pub(super) fn last_chain(&self, slot: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let Some(mut x) = self.nav_last_leaf(slot) else {
            chain.push(slot);
            return chain;
        };
        // A loop up the parents, then reversed.
        loop {
            chain.push(x);
            if x == slot {
                break;
            }
            match self.nav_parent(x) {
                Some(p) => x = p,
                None => {
                    // Not below `slot` (cannot happen for a live slot).
                    chain.clear();
                    chain.push(slot);
                    return chain;
                }
            }
        }
        chain.reverse();
        chain
    }

    /// (rank, byte offset) of a live slot. O(log R).
    pub(super) fn nav_rank(&self, slot: u32) -> Option<(u64, u64)> {
        let Some(e) = self.edits.as_deref() else {
            let start = self.start_of(slot as usize);
            return Some((u64::from(slot), u64::try_from(start).ok()?));
        };
        let (item, run, start) = self.run_item(slot)?;
        let loc = self.locate(slot)?;
        let (pc, pb) = e.seq.prefix(item);
        let within = u64::from(slot.checked_sub(start)?);
        let a = run.a as usize;
        let bytes = loc.flat.start(loc.local).checked_sub(loc.flat.start(a))?;
        Some((
            pc.checked_add(within)?,
            pb.checked_add(u64::try_from(bytes).ok()?)?,
        ))
    }

    /// Element count and token bytes of the whole tree.
    pub(super) fn nav_total(&self) -> (u64, u64) {
        match self.edits.as_deref() {
            Some(e) => e.seq.total(),
            None => (
                u64::try_from(self.kinds.len()).unwrap_or(u64::MAX),
                u64::try_from(self.source.len()).unwrap_or(u64::MAX),
            ),
        }
    }

    /// The first element after `slot`'s subtree in preorder; `None` at the
    /// end. O(log R) when the subtree was not edited, else O(depth): one
    /// O(1) link per level climbed (a loop, no recursion).
    pub(super) fn nav_after(&self, slot: u32) -> Option<u32> {
        if self.edits.is_none() {
            let end = self.end_of(slot as usize);
            return u32::try_from(end)
                .ok()
                .filter(|e| (*e as usize) < self.kinds.len());
        }
        let loc = self.locate(slot)?;
        if let Some((_, run, _)) = self.run_item(slot) {
            let end = loc.flat.end(loc.local);
            if end > loc.local && end < run.b as usize {
                return loc.slot(end);
            }
        }
        let mut x = slot;
        loop {
            if let Some(n) = self.nav_next(x) {
                return Some(n);
            }
            x = self.nav_parent(x)?;
        }
    }

    /// Elements in `slot`'s subtree, itself included.
    pub(super) fn nav_size(&self, slot: u32) -> u64 {
        let Some((r, _)) = self.nav_rank(slot) else {
            return 0;
        };
        let end = self
            .nav_after(slot)
            .and_then(|a| self.nav_rank(a))
            .map_or(self.nav_total().0, |(r, _)| r);
        end.saturating_sub(r)
    }

    /// Byte range of a live slot in the edited text.
    pub(super) fn nav_bytes(&self, slot: u32) -> Option<(u64, u64)> {
        let (_, start) = self.nav_rank(slot)?;
        let end = self
            .nav_after(slot)
            .and_then(|a| self.nav_rank(a))
            .map_or(self.nav_total().1, |(_, b)| b);
        Some((start, end.max(start)))
    }

    // ============================================================== text

    /// The text of a live slot in an edited tree: borrowed from its arena
    /// when one run holds it (tokens always), else built from the runs it
    /// spans (O(node size)). Nothing is cached.
    pub(super) fn edited_text(&self, slot: u32) -> Cow<'_, str> {
        if let Some(loc) = self.locate(slot) {
            if loc.flat.kind(loc.local).is_some_and(SyntaxKind::is_token) {
                return Cow::Borrowed(loc.flat.token_text(loc.local));
            }
        }
        match self.nav_bytes(slot) {
            // The fallback is exact, so a read
            // does not assert; `text_ranges_are_covered_or_an_error` checks
            // that the runs cover every element.
            Some((a, b)) => self
                .try_text_range(a, b)
                .unwrap_or_else(|_| self.text_from_tokens(slot)),
            None => Cow::Borrowed(""),
        }
    }

    /// Bytes `start..end` of the current text: borrowed when one run (or
    /// the unedited source) holds them, else joined from the runs.
    /// O(log R + bytes + runs crossed).
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Internal`] when the runs do not cover the range
    /// (the runs tile the text by construction, so this
    /// is a bug, reported rather than truncated).
    pub(super) fn try_text_range(
        &self,
        start: u64,
        end: u64,
    ) -> Result<Cow<'_, str>, TreeEditError> {
        let Some(e) = self.edits.as_deref() else {
            let a = usize::try_from(start).map_err(|_| TreeEditError::Internal)?;
            let b = usize::try_from(end).map_err(|_| TreeEditError::Internal)?;
            if b <= a {
                return Ok(Cow::Borrowed(""));
            }
            return self
                .source
                .get(a..b)
                .map(Cow::Borrowed)
                .ok_or(TreeEditError::Internal);
        };
        if end <= start {
            return Ok(Cow::Borrowed(""));
        }
        let (mut item, mut off) = e.seq.find_byte(start).ok_or(TreeEditError::Internal)?;
        let mut left = end.saturating_sub(start);
        let mut out: Option<String> = None;
        loop {
            let piece = e.seq.run(item).and_then(|run| {
                let (flat, _, _) = self.run_flat(&run)?;
                let take = left.min(run.bytes.saturating_sub(off));
                let a = flat
                    .start(run.a as usize)
                    .checked_add(usize::try_from(off).ok()?)?;
                let b = a.checked_add(usize::try_from(take).ok()?)?;
                Some((flat.source.get(a..b)?, take))
            });
            // The runs tile the text (seq.rs): a missing piece is a bug.
            let (piece, take) = piece.ok_or(TreeEditError::Internal)?;
            left = left.saturating_sub(take);
            if left == 0 && out.is_none() {
                return Ok(Cow::Borrowed(piece));
            }
            out.get_or_insert_with(String::new).push_str(piece);
            if left == 0 {
                break;
            }
            item = e.seq.next(item).ok_or(TreeEditError::Internal)?;
            off = 0;
        }
        Ok(Cow::Owned(out.unwrap_or_default()))
    }

    /// The text of live `slot` in an edited tree, joined from its tokens
    /// (each borrowed from its own arena): the read path's fallback when
    /// [`SyntaxTree::try_text_range`] reports a bug, so a read never
    /// returns truncated text. O(node size).
    fn text_from_tokens(&self, slot: u32) -> Cow<'_, str> {
        let mut out = String::new();
        let mut left = self.nav_size(slot);
        let mut at = Some(slot);
        while left > 0 {
            let Some(x) = at else { break };
            if let Some(loc) = self.locate(x) {
                if loc.flat.kind(loc.local).is_some_and(SyntaxKind::is_token) {
                    out.push_str(loc.flat.token_text(loc.local));
                }
            }
            left = left.saturating_sub(1);
            at = self.nav_succ(x);
        }
        Cow::Owned(out)
    }

    /// Byte `offset` of the current text. O(log R).
    pub(super) fn byte_at(&self, offset: u64) -> Option<u8> {
        let Some(e) = self.edits.as_deref() else {
            return self
                .source
                .as_bytes()
                .get(usize::try_from(offset).ok()?)
                .copied();
        };
        let (item, off) = e.seq.find_byte(offset)?;
        let run = e.seq.run(item)?;
        let (flat, _, _) = self.run_flat(&run)?;
        let at = flat
            .start(run.a as usize)
            .checked_add(usize::try_from(off).ok()?)?;
        flat.source.as_bytes().get(at).copied()
    }

    /// Bytes between the line break before byte `offset` of the current
    /// text (or its start) and `offset`: the column there, in the parser's
    /// own unit (it counts columns in bytes; a count of
    /// characters disagreed with it after non-ASCII text, and the context
    /// written for a re-read must put the parser where it was).
    /// O(column + runs crossed).
    ///
    /// Byte order marks at the start of the line are not counted: the
    /// parser starts a line after one (`scan_trivia`; a fuzz finding:
    /// counting them refused every block edit of a document behind a BOM).
    /// White space before a mark still counts, as the parser's indentation
    /// does (and the parser reports it as an error).
    pub(super) fn column_at(&self, offset: u64) -> Option<usize> {
        let mut col = self.line_bytes_before(offset)?;
        let mut at = offset.checked_sub(u64::try_from(col).ok()?)?;
        while col >= 3
            && self.byte_at(at) == Some(0xef)
            && self.byte_at(at.saturating_add(1)) == Some(0xbb)
            && self.byte_at(at.saturating_add(2)) == Some(0xbf)
        {
            col = col.saturating_sub(3);
            at = at.saturating_add(3);
        }
        Some(col)
    }

    /// Bytes between the line break before byte `offset` (or the start of
    /// the text) and `offset`. O(column + runs crossed).
    fn line_bytes_before(&self, offset: u64) -> Option<usize> {
        let breaks = ['\n', '\r'];
        let Some(e) = self.edits.as_deref() else {
            let before = self.source.get(..usize::try_from(offset).ok()?)?;
            let line = before
                .rfind(breaks)
                .and_then(|i| before.get(i.checked_add(1)?..))
                .unwrap_or(before);
            return Some(line.len());
        };
        if offset == 0 {
            return Some(0);
        }
        let (mut item, mut last) = e.seq.find_byte(offset.checked_sub(1)?)?;
        let mut count = 0usize;
        loop {
            let run = e.seq.run(item)?;
            let (flat, _, _) = self.run_flat(&run)?;
            let a = flat.start(run.a as usize);
            let b = a.checked_add(usize::try_from(last.checked_add(1)?).ok()?)?;
            let piece = flat.source.get(a..b)?;
            if let Some(i) = piece.rfind(breaks) {
                let tail = piece.get(i.checked_add(1)?..)?;
                return Some(count.saturating_add(tail.len()));
            }
            count = count.saturating_add(piece.len());
            let mut p = e.seq.prev(item);
            loop {
                let Some(x) = p else { return Some(count) };
                let bytes = e.seq.run(x)?.bytes;
                if bytes > 0 {
                    item = x;
                    last = bytes.checked_sub(1)?;
                    break;
                }
                p = e.seq.prev(x);
            }
        }
    }

    /// The text of the edited tree, run by run: O(1) per run beyond its
    /// bytes (the arena from the run's slab index, the next run
    /// by a treap step, amortised O(1); no map lookup), so O(n).
    pub(super) fn edited_emit(&self, e: &Edits) -> String {
        let (_, bytes) = e.seq.total();
        let mut out = String::with_capacity(usize::try_from(bytes).unwrap_or(0));
        let mut at = e.seq.first();
        while let Some(item) = at {
            if let Some(run) = e.seq.run(item) {
                if let Some((flat, _, _)) = self.run_flat(&run) {
                    let a = flat.start(run.a as usize);
                    let b = flat.start(run.b as usize).max(a);
                    out.push_str(flat.source.get(a..b).unwrap_or(""));
                }
            }
            at = e.seq.next(item);
        }
        out
    }

    /// Moves a local span of `slot`'s arena to the edited text.
    pub(super) fn to_global(&self, slot: u32, span: Span) -> Span {
        let (Some(loc), Some((_, off))) = (self.locate(slot), self.nav_rank(slot)) else {
            return span;
        };
        let from = loc.flat.start(loc.local);
        super::arena::shift_span(span, from, usize::try_from(off).unwrap_or(usize::MAX))
    }

    // ======================================================= diagnostics

    /// The diagnostics attached to live `slot`, in order, spans in the
    /// current text. O(log d) (+ O(log R) edited).
    ///
    /// A diagnostic's spans move with the element it is attached to. The
    /// only diagnostics whose primary span lies before their element are
    /// `Y0028` (`lenient_inline_colons`) warnings, whose span is inside the
    /// plain scalar right before the element: no other element lies
    /// between, so the move is exact unless that scalar itself is replaced
    /// (the warning then leaves with it: see
    /// [`SyntaxTree::is_dropped`], or the check refuses the edit).
    pub(super) fn diagnostics_of(&self, slot: u32) -> Vec<Cow<'_, Diagnostic>> {
        if self.edits.is_none() {
            let i = slot as usize;
            let lo = self.diagnostics.partition_point(|(n, _)| (*n as usize) < i);
            return self
                .diagnostics
                .get(lo..)
                .unwrap_or_default()
                .iter()
                .take_while(|(n, _)| *n as usize == i)
                .map(|(_, d)| d.view())
                .collect();
        }
        let (Some(loc), Some((_, off))) = (self.locate(slot), self.nav_rank(slot)) else {
            return Vec::new();
        };
        let from = loc.flat.start(loc.local);
        let to = usize::try_from(off).unwrap_or(usize::MAX);
        loc.flat
            .diagnostics_at(loc.local)
            .iter()
            .enumerate()
            .filter(|(n, _)| !self.is_dropped(slot, *n))
            .map(|(_, (_, d))| {
                let mut d = d.clone();
                d.shift(from, to);
                Cow::Owned(d.view().into_owned())
            })
            .collect()
    }

    /// Calls `f(diagnostic, from, to)` for each diagnostic attached to
    /// live `slot`, in order, where moving its spans by `to - from` puts
    /// them in the current text. No allocation, and
    /// nothing searched in an arena without diagnostics.
    pub(super) fn for_each_diagnostic(&self, slot: u32, mut f: impl FnMut(&Stored, usize, usize)) {
        let Some(loc) = self.locate(slot) else {
            return;
        };
        if loc.flat.diagnostics.is_empty() {
            return;
        }
        let list = loc.flat.diagnostics_at(loc.local);
        if list.is_empty() {
            return;
        }
        let (from, to) = if self.edits.is_none() {
            (0, 0)
        } else {
            let Some((_, off)) = self.nav_rank(slot) else {
                return;
            };
            (
                loc.flat.start(loc.local),
                usize::try_from(off).unwrap_or(usize::MAX),
            )
        };
        for (n, (_, d)) in list.iter().enumerate() {
            if !self.is_dropped(slot, n) {
                f(d, from, to);
            }
        }
    }

    /// The `n`-th diagnostic attached to `slot` left the tree (see
    /// `Edits::dropped`). O(1) while none has, else
    /// O(log).
    pub(super) fn is_dropped(&self, slot: u32, n: usize) -> bool {
        self.edits.as_deref().is_some_and(|e| {
            !e.dropped.is_empty() && u32::try_from(n).is_ok_and(|n| e.dropped.contains(&(slot, n)))
        })
    }

    /// `is_dropped` for entry `at` of `flat`'s diagnostic list, whose
    /// arena's first slot is `first`. O(1) while none was dropped.
    fn list_dropped(&self, flat: &Flat<'_>, first: u32, at: usize) -> bool {
        let Some(e) = self.edits.as_deref() else {
            return false;
        };
        if e.dropped.is_empty() {
            return false;
        }
        let Some(&(l, _)) = flat.diagnostics.get(at) else {
            return false;
        };
        let group = flat.diagnostics.partition_point(|(x, _)| *x < l);
        self.is_dropped(first.saturating_add(l), at.saturating_sub(group))
    }

    /// Calls `f` for the diagnostics attached to elements from live
    /// `slot` on, in element order (`forward`), or from `slot` back
    /// (`slot` included), until `f` returns `false` or the element rank
    /// leaves the bound (`forward`: ranks below `bound`; backwards: ranks
    /// at or above `bound`). Nothing is built or cloned,
    /// and only the runs inside the bound are visited, O(log d + log R)
    /// each, so a caller that stops early pays for what it reads.
    pub(super) fn scan_diagnostics(
        &self,
        slot: u32,
        forward: bool,
        bound: u64,
        mut f: impl FnMut(DiagAt<'_>) -> bool,
    ) {
        let Some(e) = self.edits.as_deref() else {
            let i = slot as usize;
            let len = self.kinds.len();
            let list = &self.diagnostics;
            let mut visit = |(n, d): &(u32, Stored)| {
                let n = *n as usize;
                let rank = u64::try_from(n).unwrap_or(u64::MAX);
                let inside = n < len && if forward { rank < bound } else { rank >= bound };
                inside
                    && f(DiagAt {
                        rank,
                        start: u64::try_from(self.start_of(n)).unwrap_or(u64::MAX),
                        diagnostic: d,
                        from: 0,
                        to: 0,
                    })
            };
            if forward {
                let lo = list.partition_point(|(n, _)| (*n as usize) < i);
                for x in list.get(lo..).unwrap_or_default() {
                    if !visit(x) {
                        return;
                    }
                }
            } else {
                let hi = list.partition_point(|(n, _)| (*n as usize) <= i);
                for x in list.get(..hi).unwrap_or_default().iter().rev() {
                    if !visit(x) {
                        return;
                    }
                }
            }
            return;
        };
        let Some((mut item, first_run, start)) = self.run_item(slot) else {
            return;
        };
        let (mut count_at, mut bytes_at) = e.seq.prefix(item);
        // The local index to scan from in the current run.
        let mut local = first_run.a.saturating_add(slot.saturating_sub(start));
        loop {
            let Some(run) = e.seq.run(item) else { return };
            // The runs visited are all inside the bound's side.
            let run_count = u64::from(run.b.saturating_sub(run.a));
            let outside = if forward {
                count_at >= bound
            } else {
                count_at.saturating_add(run_count) <= bound
            };
            if outside {
                return;
            }
            let flat = self.run_flat(&run).filter(|_| run_count > 0);
            if let Some((flat, first, _)) = flat {
                let run_from = flat.start(run.a as usize);
                let list = flat.diagnostics;
                let (lo, hi) = if forward {
                    (
                        list.partition_point(|(l, _)| *l < local),
                        list.partition_point(|(l, _)| *l < run.b),
                    )
                } else {
                    (
                        list.partition_point(|(l, _)| *l < run.a),
                        list.partition_point(|(l, _)| *l <= local),
                    )
                };
                let window = list.get(lo..hi.max(lo)).unwrap_or_default();
                let mut visit = |(l, d): &(u32, Stored)| {
                    let rank = count_at.saturating_add(u64::from(l.saturating_sub(run.a)));
                    let inside = if forward { rank < bound } else { rank >= bound };
                    let from = flat.start(*l as usize);
                    let start = u64::try_from(from.saturating_sub(run_from))
                        .unwrap_or(u64::MAX)
                        .saturating_add(bytes_at);
                    inside
                        && f(DiagAt {
                            rank,
                            start,
                            diagnostic: d,
                            from,
                            to: usize::try_from(start).unwrap_or(usize::MAX),
                        })
                };
                // Warnings that left the tree are passed over.
                let gone = |k: usize| self.list_dropped(&flat, first, lo.saturating_add(k));
                if forward {
                    for (k, x) in window.iter().enumerate() {
                        if !gone(k) && !visit(x) {
                            return;
                        }
                    }
                } else {
                    for (k, x) in window.iter().enumerate().rev() {
                        if !gone(k) && !visit(x) {
                            return;
                        }
                    }
                }
            }
            // Jump straight to the next run holding a
            // diagnostic (subtree sums, O(log R)); walking run by run made
            // every edit O(R), so edit cost grew with the edit history.
            let jump = if forward {
                e.seq.next_with_diags(item)
            } else {
                e.seq.prev_with_diags(item)
            };
            let Some(n) = jump else { return };
            let Some(nr) = e.seq.run(n) else { return };
            item = n;
            (count_at, bytes_at) = e.seq.prefix(n);
            local = if forward {
                nr.a
            } else {
                // (An empty run's window is empty.)
                nr.b.saturating_sub(1).max(nr.a)
            };
        }
    }

    /// An edited tree's diagnostics in element order, spans moved to the
    /// edited text, read from each arena's own sorted list (nothing
    /// is rebuilt per edit). `within`: only those attached inside that
    /// slot's subtree (O(D' (log d + log R) + k) for its D' runs holding
    /// diagnostics); `None`: all,
    /// with the base's after-the-last-element warnings (attached to
    /// `NONE`) at the end.
    pub(super) fn edited_diagnostics(&self, e: &Edits, within: Option<u32>) -> Vec<(u32, Stored)> {
        let mut out = Vec::new();
        let start = match within {
            Some(slot) => self
                .run_item(slot)
                .and_then(|(item, _, first)| Some((item, slot.checked_sub(first)?))),
            None => e.seq.first().map(|item| (item, 0)),
        };
        let size = match within {
            Some(slot) => self.nav_size(slot),
            None => e.seq.total().0,
        };
        if let Some((mut item, mut skip)) = start {
            let (run_rank, mut bytes_at) = e.seq.prefix(item);
            // The rank one past the last element read.
            let end = run_rank
                .saturating_add(u64::from(skip))
                .saturating_add(size);
            let mut rank_at = run_rank;
            while rank_at < end {
                let Some(run) = e.seq.run(item) else { break };
                let Some((flat, first, _)) = self.run_flat(&run) else {
                    break;
                };
                let a = run.a.saturating_add(skip);
                let n = u32::try_from(end.saturating_sub(rank_at))
                    .unwrap_or(u32::MAX)
                    .min(run.b.saturating_sub(run.a))
                    .saturating_sub(skip);
                let b = a.saturating_add(n);
                let run_from = flat.start(run.a as usize);
                let lo = flat.diagnostics.partition_point(|(l, _)| *l < a);
                let list = flat.diagnostics.get(lo..).unwrap_or_default();
                for (k, (l, d)) in list.iter().take_while(|(l, _)| *l < b).enumerate() {
                    // Warnings that left the tree are not listed.
                    if self.list_dropped(&flat, first, lo.saturating_add(k)) {
                        continue;
                    }
                    let from = flat.start(*l as usize);
                    let to = from
                        .checked_sub(run_from)
                        .and_then(|x| u64::try_from(x).ok())
                        .and_then(|x| x.checked_add(bytes_at))
                        .and_then(|x| usize::try_from(x).ok())
                        .unwrap_or(usize::MAX);
                    let mut d = d.clone();
                    d.shift(from, to);
                    out.push((first.saturating_add(*l), d));
                }
                skip = 0;
                // The runs without diagnostics are
                // skipped in O(log R), not visited one by one.
                match e.seq.next_with_diags(item) {
                    Some(x) => {
                        item = x;
                        (rank_at, bytes_at) = e.seq.prefix(x);
                    }
                    None => break,
                }
            }
        }
        if within.is_none() {
            // Warnings after the last element: their spans are at the end
            // of the base text, which is the end of the edited text.
            let base_len = self.kinds.len();
            let lo = self
                .diagnostics
                .partition_point(|(n, _)| (*n as usize) < base_len);
            let to = usize::try_from(e.seq.total().1).unwrap_or(usize::MAX);
            for (_, d) in self.diagnostics.get(lo..).unwrap_or_default() {
                let mut d = d.clone();
                d.shift(self.source.len(), to);
                out.push((NONE, d));
            }
        }
        out
    }

    // ============================================================ copying

    /// The document enclosing `slot` (or `slot` itself). O(depth).
    pub(super) fn document_of(&self, slot: u32) -> Option<u32> {
        let mut x = Some(slot);
        while let Some(s) = x {
            if self.kind_of(s) == Some(SyntaxKind::Document) {
                return Some(s);
            }
            x = self.nav_parent(s);
        }
        None
    }

    /// The `%TAG` directives (text, one per line) of the document enclosing
    /// `slot` (or `slot` itself); empty outside any document. Cheap:
    /// directives are a document's first children, so only those and the
    /// trivia between them are read, never the document's content.
    pub(super) fn tag_context(&self, slot: u32) -> String {
        let mut out = String::new();
        let Some(doc) = self.document_of(slot) else {
            return out;
        };
        let mut c = self.nav_first(doc);
        while let Some(ch) = c {
            let Some(loc) = self.locate(ch) else { break };
            match loc.flat.kind(loc.local) {
                Some(SyntaxKind::TagDirective) => {
                    out.push_str(loc.flat.token_text(loc.local));
                    out.push('\n');
                }
                Some(k) if is_directive(k) || k.is_trivia() => {}
                _ => break,
            }
            c = self.nav_next(ch);
        }
        out
    }

    /// A fragment of the subtrees of `roots` (live slots, in order): its
    /// `%TAG` context is that of `context`'s document, read only when a
    /// copied tag needs it.
    pub(super) fn fragment_of(
        &self,
        roots: &[u32],
        context: u32,
    ) -> Result<Fragment, TreeEditError> {
        let (arena, has_tags) = self.copy_forest(roots)?;
        let context = if has_tags {
            self.tag_context(context)
        } else {
            String::new()
        };
        Ok(Fragment {
            arena,
            context,
            has_tags,
        })
    }

    /// Copies the subtrees of `roots` (live slots, in order) into a new
    /// arena; also says whether a tag lies outside every copied document.
    /// O(copied elements), with an explicit stack: no recursion.
    pub(super) fn copy_forest(&self, roots: &[u32]) -> Result<(Arena, bool), TreeEditError> {
        self.copy_forest_with(roots, true)
    }

    /// [`SyntaxTree::copy_forest`]; `slices`: through
    /// [`SyntaxTree::copy_slice`] where it applies (`false`: the element
    /// walk only, which the tests compare it with).
    fn copy_forest_with(
        &self,
        roots: &[u32],
        slices: bool,
    ) -> Result<(Arena, bool), TreeEditError> {
        let cap = |n: usize| u32::try_from(n).ok().filter(|n| *n < NONE);
        let mut out = Arena::default();
        // (global slot, local index, is a document)
        let mut stack: Vec<(u32, u32, bool)> = Vec::new();
        // Open documents are counted, not searched for per tag.
        let mut open_docs = 0usize;
        let mut prefixes: Vec<(usize, String)> = Vec::new();
        let mut has_tags = false;
        for &root in roots {
            // A subtree that is one piece of one arena
            // (always in a parsed tree) is copied as slices.
            if slices && self.copy_slice(root, &mut out, &mut prefixes, &mut has_tags)? {
                continue;
            }
            let count = self.nav_size(root);
            let mut x = Some(root);
            let mut left = count;
            while left > 0 {
                left = left.saturating_sub(1);
                let slot = x.ok_or(TreeEditError::Internal)?;
                x = self.nav_succ(slot);
                let loc = self.locate(slot).ok_or(TreeEditError::Internal)?;
                let kind = loc.flat.kind(loc.local).ok_or(TreeEditError::Internal)?;
                let parent = if slot == root {
                    None
                } else {
                    self.nav_parent(slot)
                };
                while let Some(&(g, local, doc)) = stack.last() {
                    if Some(g) == parent {
                        break;
                    }
                    stack.pop();
                    if doc {
                        open_docs = open_docs.saturating_sub(1);
                    }
                    let end = cap(out.kinds.len()).ok_or(TreeEditError::Capacity)?;
                    *out.words
                        .get_mut(local as usize)
                        .ok_or(TreeEditError::Internal)? = end;
                }
                let index = cap(out.kinds.len()).ok_or(TreeEditError::Capacity)?;
                let at = out.source.len();
                out.kinds.push(kind);
                out.parents.push(stack.last().map_or(NONE, |(_, l, _)| *l));
                for (k, (_, d)) in loc.flat.diagnostics_at(loc.local).iter().enumerate() {
                    // A warning that left the tree is not copied.
                    if self.is_dropped(slot, k) {
                        continue;
                    }
                    let mut d = d.clone();
                    d.shift(loc.flat.start(loc.local), at);
                    out.diagnostics.push((index, d));
                }
                if kind.is_token() {
                    out.words.push(cap(at).ok_or(TreeEditError::Capacity)?);
                    let text = loc.flat.token_text(loc.local);
                    out.source.push_str(text);
                    if kind == SyntaxKind::Tag && open_docs == 0 {
                        has_tags = true;
                    }
                    if kind == SyntaxKind::BlockScalarHeader {
                        // The header span is the header token's (C5). The
                        // owner's extra is the last one pushed: only its
                        // property tokens came since (no search).
                        let owner = stack.last().map(|(_, l, _)| *l);
                        if let Some(e) = out.extras.last_mut().filter(|(i, _)| Some(*i) == owner) {
                            if let Some(h) = e.1.block.as_mut() {
                                h.span = Span::new(at, at.saturating_add(text.len()));
                            }
                        }
                    }
                } else {
                    let doc = kind == SyntaxKind::Document;
                    out.words.push(index);
                    stack.push((slot, index, doc));
                    if doc {
                        open_docs = open_docs.saturating_add(1);
                    }
                    if let Some(extra) = loc.flat.extra(loc.local) {
                        if let Some(p) = extra.tag_prefix {
                            let text = loc.flat.source.get(p..).unwrap_or("");
                            let end = text.find([' ', '\t', '\n', '\r']).unwrap_or(text.len());
                            prefixes
                                .push((out.extras.len(), text.get(..end).unwrap_or("").to_owned()));
                        }
                        out.extras.push((
                            index,
                            Extra {
                                tag_prefix: None,
                                block: extra.block,
                            },
                        ));
                    }
                }
            }
            while let Some((_, local, _)) = stack.pop() {
                let end = cap(out.kinds.len()).ok_or(TreeEditError::Capacity)?;
                *out.words
                    .get_mut(local as usize)
                    .ok_or(TreeEditError::Internal)? = end;
            }
            open_docs = 0;
        }
        out.tiled = out.source.len();
        for (e, text) in prefixes {
            let p = out.source.len();
            out.source.push_str(&text);
            out.source.push('\n');
            if let Some(x) = out.extras.get_mut(e) {
                x.1.tag_prefix = Some(p);
            }
        }
        cap(out.source.len()).ok_or(TreeEditError::Capacity)?;
        Ok((out, has_tags))
    }

    /// [`SyntaxTree::copy_forest`] of one root whose subtree is local
    /// indices `l0..l1` of one arena, all in one overlay run (every subtree
    /// of a parsed tree): the arena's arrays are copied as slices, their
    /// links moved by a constant, with no navigation per element (the
    /// per-element walk was 54% of the cost of removing a large entry).
    /// Writes exactly what the element walk writes, or nothing and
    /// returns `false` (a subtree holding a document, or one an edit cut
    /// into pieces: the walk copies it). O(copied elements).
    fn copy_slice(
        &self,
        root: u32,
        out: &mut Arena,
        prefixes: &mut Vec<(usize, String)>,
        has_tags: &mut bool,
    ) -> Result<bool, TreeEditError> {
        let Some(loc) = self.locate(root) else {
            return Ok(false);
        };
        let flat = loc.flat;
        let l0 = loc.local;
        let Ok(count) = usize::try_from(self.nav_size(root)) else {
            return Ok(false);
        };
        let Some(l1) = l0.checked_add(count) else {
            return Ok(false);
        };
        if count == 0 || l1 > flat.len() || flat.end(l0) != l1 {
            return Ok(false);
        }
        // One run: its slots are consecutive, in its arena's order.
        if self.edits.is_some() {
            let Some((_, run, start)) = self.run_item(root) else {
                return Ok(false);
            };
            let end = run
                .b
                .checked_sub(run.a)
                .and_then(|n| start.checked_add(n))
                .and_then(|e| usize::try_from(e).ok());
            let last = usize::try_from(root)
                .ok()
                .and_then(|r| r.checked_add(count));
            if !matches!((last, end), (Some(l), Some(e)) if l <= e) {
                return Ok(false);
            }
        }
        let Some(kinds) = flat.kinds.get(l0..l1) else {
            return Ok(false);
        };
        // (A document's tags are resolved inside it: the walk counts them.)
        if kinds.contains(&SyntaxKind::Document) {
            return Ok(false);
        }
        let tags = kinds.contains(&SyntaxKind::Tag);
        let cap = |n: usize| u32::try_from(n).ok().filter(|n| *n < NONE);
        let base = out.kinds.len();
        let at_base = out.source.len();
        let src_a = flat.start(l0);
        let src_b = flat.start(l1);
        let Some(text) = flat.source.get(src_a..src_b) else {
            return Ok(false);
        };
        cap(base.saturating_add(count)).ok_or(TreeEditError::Capacity)?;
        cap(at_base.saturating_add(text.len())).ok_or(TreeEditError::Capacity)?;
        let marks = (
            out.parents.len(),
            out.words.len(),
            out.extras.len(),
            out.diagnostics.len(),
            prefixes.len(),
        );
        // An element's index and a byte offset in the copy (in range: both
        // capped above).
        let index = |i: usize| -> u32 {
            let x = base.saturating_add(i.saturating_sub(l0));
            u32::try_from(x).unwrap_or(NONE)
        };
        let offset = |at: usize| -> usize { at_base.saturating_add(at.saturating_sub(src_a)) };
        // Links and words; the tokens must start in order (C1), so that
        // their texts, one after another as the walk copies them, are
        // `text`.
        let mut tiled = src_a;
        let mut ok = true;
        let mut headers: Vec<(usize, usize, usize)> = Vec::new();
        out.parents.reserve(count);
        out.words.reserve(count);
        out.kinds.reserve(count);
        for (i, kind) in (l0..l1).zip(kinds.iter().copied()) {
            let parent = if i == l0 {
                NONE
            } else {
                match flat.parent(i) {
                    Some(p) if p >= l0 && p < i => index(p),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            };
            out.parents.push(parent);
            if kind.is_token() {
                let start = flat.token_start(i).min(flat.tiled);
                if start < tiled || start > src_b {
                    ok = false;
                    break;
                }
                tiled = start;
                let at = offset(start);
                out.words.push(u32::try_from(at).unwrap_or(NONE));
                if kind == SyntaxKind::BlockScalarHeader && i != l0 {
                    headers.push((i, at, flat.token_span(i).len()));
                }
            } else {
                let end = flat.end(i);
                if end <= i || end > l1 {
                    ok = false;
                    break;
                }
                out.words.push(index(end));
            }
        }
        if ok {
            // Extras (nodes only), in order, as the walk pushes them.
            let lo = flat.extras.partition_point(|(n, _)| (*n as usize) < l0);
            let hi = flat.extras.partition_point(|(n, _)| (*n as usize) < l1);
            for (n, extra) in flat.extras.get(lo..hi).unwrap_or_default() {
                let n = *n as usize;
                if flat.kind(n).is_none_or(SyntaxKind::is_token) {
                    continue;
                }
                if let Some(p) = extra.tag_prefix {
                    let t = flat.source.get(p..).unwrap_or("");
                    let end = t.find([' ', '\t', '\n', '\r']).unwrap_or(t.len());
                    prefixes.push((out.extras.len(), t.get(..end).unwrap_or("").to_owned()));
                }
                out.extras.push((
                    index(n),
                    Extra {
                        tag_prefix: None,
                        block: extra.block,
                    },
                ));
            }
            // A block scalar header's span is its token's (C5), set where
            // the walk sets it: on the last extra pushed before the header
            // token, when that is its parent's.
            for (i, at, len) in headers {
                let (Some(p), here) = (flat.parent(i), index(i)) else {
                    continue;
                };
                let owner = index(p);
                let k = out.extras.partition_point(|(n, _)| *n < here);
                let Some(e) = k.checked_sub(1).and_then(|k| out.extras.get_mut(k)) else {
                    continue;
                };
                if e.0 == owner {
                    if let Some(h) = e.1.block.as_mut() {
                        h.span = Span::new(at, at.saturating_add(len));
                    }
                }
            }
            // Diagnostics, by element, in order (a dropped warning stays).
            let lo = flat
                .diagnostics
                .partition_point(|(n, _)| (*n as usize) < l0);
            let hi = flat
                .diagnostics
                .partition_point(|(n, _)| (*n as usize) < l1);
            let mut prev = usize::MAX;
            let mut k = 0usize;
            for (n, d) in flat.diagnostics.get(lo..hi).unwrap_or_default() {
                let n = *n as usize;
                k = if n == prev { k.saturating_add(1) } else { 0 };
                prev = n;
                let slot = u32::try_from(n.saturating_sub(l0))
                    .ok()
                    .and_then(|o| root.checked_add(o))
                    .ok_or(TreeEditError::Internal)?;
                if self.is_dropped(slot, k) {
                    continue;
                }
                let from = flat.start(n);
                let mut d = d.clone();
                d.shift(from, offset(from));
                out.diagnostics.push((index(n), d));
            }
        }
        if !ok {
            out.parents.truncate(marks.0);
            out.words.truncate(marks.1);
            out.extras.truncate(marks.2);
            out.diagnostics.truncate(marks.3);
            prefixes.truncate(marks.4);
            return Ok(false);
        }
        out.kinds.extend_from_slice(kinds);
        out.source.push_str(text);
        *has_tags |= tags;
        Ok(true)
    }

    // ========================================================= validation

    pub(super) fn live_slot(&self, id: NodeId) -> Result<u32, TreeEditError> {
        Ok(self.get(id)?.slot())
    }

    /// Children may be added under / removed from `parent`.
    pub(super) fn check_parent(&self, parent: u32) -> Result<SyntaxKind, TreeEditError> {
        let kind = self.kind_of(parent).ok_or(TreeEditError::Internal)?;
        if kind.is_token() {
            return Err(TreeEditError::NotANode { kind });
        }
        if matches!(
            kind,
            SyntaxKind::Scalar | SyntaxKind::Alias | SyntaxKind::Error
        ) {
            return Err(TreeEditError::ReadOnly { kind });
        }
        Ok(kind)
    }

    /// The siblings `first..=last`, with their parent.
    pub(super) fn sibling_range(
        &self,
        first: u32,
        last: u32,
    ) -> Result<(u32, Vec<u32>), TreeEditError> {
        let parent = self.nav_parent(first).ok_or(TreeEditError::NoParent)?;
        if self.nav_parent(last) != Some(parent) {
            return Err(TreeEditError::NotSiblings);
        }
        let mut roots = vec![first];
        let mut x = first;
        while x != last {
            x = self.nav_next(x).ok_or(TreeEditError::NotSiblings)?;
            roots.push(x);
        }
        Ok((parent, roots))
    }

    /// The range may be removed.
    pub(super) fn check_removable(&self, roots: &[u32]) -> Result<(), TreeEditError> {
        for &r in roots {
            let kind = self.kind_of(r).ok_or(TreeEditError::Internal)?;
            if is_directive(kind) || is_node_property(kind) {
                return Err(TreeEditError::ReadOnly { kind });
            }
        }
        // An error region reads the same after
        // edits, so no edit removes (or replaces) an `Error` node inside a
        // document; a whole document goes with its errors (the edited
        // text then has neither, as its re-parse). Only a tree with errors
        // pays for the walk, O(removed) (what a removal copies anyway);
        // A flat walk.
        if self.error_count() > 0 {
            for &r in roots {
                if self.kind_of(r) == Some(SyntaxKind::Document) {
                    continue;
                }
                let error = super::Node::new(self, r)
                    .descendants()
                    .any(|n| n.kind() == SyntaxKind::Error);
                if error {
                    return Err(TreeEditError::ReadOnly {
                        kind: SyntaxKind::Error,
                    });
                }
            }
        }
        Ok(())
    }

    /// `fragment` may go under `parent`.
    pub(super) fn check_insertable(
        &self,
        parent: u32,
        parent_kind: SyntaxKind,
        fragment: &Fragment,
    ) -> Result<(), TreeEditError> {
        if fragment.is_empty() {
            return Err(TreeEditError::EmptyFragment);
        }
        for kind in fragment.root_kinds() {
            if is_directive(kind) || is_node_property(kind) {
                return Err(TreeEditError::ReadOnly { kind });
            }
            let fits = match (parent_kind, kind) {
                (_, SyntaxKind::Stream) => false,
                (SyntaxKind::Stream, k) => k == SyntaxKind::Document || k.is_token(),
                (_, SyntaxKind::Document) => false,
                _ => true,
            };
            if !fits {
                return Err(TreeEditError::Misplaced {
                    kind,
                    parent: parent_kind,
                });
            }
        }
        if fragment.has_tags && fragment.context != self.tag_context(parent) {
            return Err(TreeEditError::TagContext);
        }
        Ok(())
    }

    // ============================================================ editing

    /// Creates the overlay on the first edit: one run over the base, O(1).
    fn ensure_edits(&mut self) -> Result<(), TreeEditError> {
        if self.edits.is_some() {
            return Ok(());
        }
        let len = u32::try_from(self.kinds.len()).map_err(|_| TreeEditError::Capacity)?;
        let bytes = u64::try_from(self.source.len()).map_err(|_| TreeEditError::Capacity)?;
        let diags = self.base_flat().diagnostics_in(0, len);
        let marks = marks(self.base_flat(), &self.run_facts, Some(&self.facts), 0, len);
        let seq = Seq::new(Run {
            arena: 0,
            graft: NONE,
            a: 0,
            b: len,
            bytes,
            diags,
            // The base holds the stream root.
            trivia: false,
            marks,
        });
        let mut runs = BTreeMap::new();
        runs.insert(0, seq.first().ok_or(TreeEditError::Internal)?);
        self.edits = Some(Box::new(Edits {
            arenas: BTreeMap::new(),
            grafts: Slab::default(),
            seq,
            runs,
            links: BTreeMap::new(),
            dropped: BTreeSet::new(),
            // Never below the slots a rolled-back group
            // issued before it dropped the overlay.
            next_slot: len.max(self.slot_floor),
        }));
        Ok(())
    }

    /// Makes `slot` the first element of a run; returns that run's item.
    /// Changes how the order is cut, never the order itself.
    #[cfg(test)]
    fn cut_before(&mut self, slot: u32) -> Result<u32, TreeEditError> {
        self.cut_logged(slot, None)
    }

    /// [`SyntaxTree::cut_before`], recording an actual split in `undo`.
    fn cut_logged(&mut self, slot: u32, undo: Option<&mut Undo>) -> Result<u32, TreeEditError> {
        let (item, run, start) = self.run_item(slot).ok_or(TreeEditError::Internal)?;
        let offset = slot.checked_sub(start).ok_or(TreeEditError::Internal)?;
        if offset == 0 {
            return Ok(item);
        }
        let (flat, _, _) = self.run_flat(&run).ok_or(TreeEditError::Internal)?;
        let mid = run.a.checked_add(offset).ok_or(TreeEditError::Internal)?;
        let left_bytes = flat
            .start(mid as usize)
            .checked_sub(flat.start(run.a as usize))
            .and_then(|b| u64::try_from(b).ok())
            .ok_or(TreeEditError::Internal)?;
        let left_diags = flat.diagnostics_in(run.a, mid);
        // Each half's own flag (O(LONG_RUN + log)).
        let (left_trivia, right_trivia) = if run.trivia {
            (true, true)
        } else {
            (
                self.stream_trivia(run.arena, run.a, mid),
                self.stream_trivia(run.arena, mid, run.b),
            )
        };
        // Each half's marks (O(log + LONG_RUN + a line)).
        let left_marks = self.run_marks(run.arena, run.a, mid);
        let right_marks = self.run_marks(run.arena, mid, run.b);
        let left = Run {
            b: mid,
            bytes: left_bytes,
            diags: left_diags,
            trivia: left_trivia,
            marks: left_marks,
            ..run
        };
        let right = Run {
            a: mid,
            bytes: run.bytes.saturating_sub(left_bytes),
            diags: run.diags.saturating_sub(left_diags),
            trivia: right_trivia,
            marks: right_marks,
            ..run
        };
        let e = self.edits.as_deref_mut().ok_or(TreeEditError::Internal)?;
        // Reserve first: a full item space leaves the run whole.
        if !e.seq.can_insert(1) {
            return Err(TreeEditError::Capacity);
        }
        let new = e
            .seq
            .insert_after(Some(item), right)
            .ok_or(TreeEditError::Capacity)?;
        e.seq.set_run(item, left);
        e.runs.insert(slot, new);
        if let Some(u) = undo {
            u.cuts.push(slot);
        }
        Ok(new)
    }

    fn set_links(
        &mut self,
        slot: u32,
        undo: &mut Undo,
        f: impl FnOnce(&mut Links),
    ) -> Result<(), TreeEditError> {
        let mut l = self.links_of(slot);
        f(&mut l);
        let e = self.edits.as_deref_mut().ok_or(TreeEditError::Internal)?;
        let old = e.links.insert(slot, l);
        undo.links.push((slot, old));
        Ok(())
    }

    /// The one structural edit: under `parent`, after sibling `prev`
    /// (`None`: at the front), remove the siblings `removed` (consecutive,
    /// starting right after `prev`) and insert `fragment`'s roots; then,
    /// when `validate`, check that the edit is well-formed. The caller has
    /// done the other checks. Refused or failed, the edit is undone
    /// exactly: the tree is unchanged on error.
    pub(super) fn splice(
        &mut self,
        parent: u32,
        prev: Option<u32>,
        removed: &[u32],
        fragment: Option<Fragment>,
        validate: bool,
    ) -> Result<Spliced, TreeEditError> {
        self.splice_dropping(parent, prev, removed, fragment, validate, &[])
    }

    /// [`SyntaxTree::splice`] that also drops warnings `drops` (`(slot,
    /// n)`: the `n`-th diagnostic attached to live `slot`) before the
    /// check (undone with the rest when refused).
    fn splice_dropping(
        &mut self,
        parent: u32,
        prev: Option<u32>,
        removed: &[u32],
        fragment: Option<Fragment>,
        validate: bool,
        drops: &[(u32, u32)],
    ) -> Result<Spliced, TreeEditError> {
        let mut a = self.splice_apply(parent, prev, removed, fragment, validate)?;
        if let Some(e) = self.edits.as_deref_mut() {
            for &d in drops {
                if e.dropped.insert(d) {
                    a.undo.dropped.push(d);
                }
            }
        }
        if validate {
            let c = a.check;
            if let Err(err) = self.check_well_formed(c.parent, c.prev, c.next, a.span, c.what) {
                self.undo(a.undo)?;
                return Err(err);
            }
        }
        self.accept(a.undo);
        Ok((a.taken, a.span))
    }

    /// After an accepted edit: its seams merged and what it removed
    /// released, or, inside an edit group, the edit kept to be undone.
    fn accept(&mut self, undo: Undo) {
        let seams = self.seams(&undo);
        self.accept_with(undo, seams);
    }

    /// [`SyntaxTree::accept`] with the seams read already.
    fn accept_with(&mut self, undo: Undo, seams: Vec<u32>) {
        if self.group.depth > 0 {
            // Nothing is merged or released while a group
            // is open, so every logged undo record stays exact.
            self.group.log.push((undo, seams));
            return;
        }
        self.coalesce(&seams);
        self.release(&undo);
    }

    /// Opens an **edit group** (a multi-step edit is all or
    /// nothing): the edits accepted until the matching
    /// [`SyntaxTree::end_group`] can be undone together by
    /// [`SyntaxTree::rollback_group`], exactly (the tree, its text, its
    /// diagnostics, and every handle are as they were at
    /// `begin_group`). Groups nest; each edit is still checked on its own.
    /// O(1).
    ///
    /// While a group is open, accepted edits keep their undo records and
    /// the runs they split (merged and released when the outermost group
    /// ends), so a group should be closed by one of the two calls: an
    /// unclosed group costs memory and run count, never correctness.
    pub fn begin_group(&mut self) -> GroupMark {
        self.group.depth = self.group.depth.saturating_add(1);
        GroupMark {
            tree: self.stamp,
            at: self.group.log.len(),
            depth: self.group.depth,
        }
    }

    /// `mark` is the innermost open group's mark of this tree.
    fn check_mark(&self, mark: GroupMark) -> Result<(), TreeEditError> {
        if mark.tree != self.stamp
            || mark.depth != self.group.depth
            || mark.at > self.group.log.len()
        {
            return Err(TreeEditError::Internal);
        }
        Ok(())
    }

    /// For a copy of this tree made while a group is open (`Clone`,
    /// [`SyntaxTree::fork`]): the copy has no group, so the group's edits
    /// are accepted edits there; they are merged and released in the copy
    /// as [`SyntaxTree::end_group`] would (a copy never
    /// keeps the runs or arenas the group held back).
    pub(super) fn settle_group_copy(&self, copy: &mut Self) {
        for (_, seams) in &self.group.log {
            copy.coalesce(seams);
        }
        for (undo, _) in &self.group.log {
            copy.release(undo);
        }
    }

    /// Closes the group `mark` opened, keeping its edits (for the
    /// outermost group: merging and releasing as each edit alone would
    /// have; O(edits of the group · log R)).
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Internal`] when `mark` is not the innermost open
    /// group's (the tree is unchanged).
    pub fn end_group(&mut self, mark: GroupMark) -> Result<(), TreeEditError> {
        self.check_mark(mark)?;
        self.group.depth = self.group.depth.saturating_sub(1);
        if self.group.depth == 0 {
            let log = std::mem::take(&mut self.group.log);
            for (_, seams) in &log {
                self.coalesce(seams);
            }
            for (undo, _) in &log {
                self.release(undo);
            }
        }
        Ok(())
    }

    /// Closes the group `mark` opened, undoing every edit accepted in it,
    /// latest first: the tree is exactly as at [`SyntaxTree::begin_group`].
    /// O(edits of the group · (log R + their size)).
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Internal`] when `mark` is not the innermost open
    /// group's (the tree is unchanged), or an undo record does not apply
    /// (an invariant of this crate).
    pub fn rollback_group(&mut self, mark: GroupMark) -> Result<(), TreeEditError> {
        self.check_mark(mark)?;
        // Slots are never reused: the slots the group
        // issued stay issued. Undoing its edits puts back everything else,
        // but the next slot stays past them, in the overlay or, when the
        // rollback drops the overlay the group created, in `slot_floor`.
        let high = self.next_slot();
        let undone = self.group.log.split_off(mark.at);
        self.group.depth = self.group.depth.saturating_sub(1);
        for (undo, _) in undone.into_iter().rev() {
            self.undo(undo)?;
        }
        match self.edits.as_deref_mut() {
            Some(e) => e.next_slot = e.next_slot.max(high),
            None => self.slot_floor = self.slot_floor.max(high),
        }
        if self.group.depth == 0 {
            // Only an outer group's edits can be left (none here, as the
            // outermost mark starts at 0); kept for symmetry.
            let log = std::mem::take(&mut self.group.log);
            for (_, seams) in &log {
                self.coalesce(seams);
            }
            for (undo, _) in &log {
                self.release(undo);
            }
        }
        Ok(())
    }

    /// `true` while an edit group is open.
    #[must_use]
    pub fn in_group(&self) -> bool {
        self.group.depth > 0
    }

    /// See [`Edits::release`] (after an accepted edit only: a
    /// refused one puts its removed runs back).
    fn release(&mut self, u: &Undo) {
        if let Some(e) = self.edits.as_deref_mut() {
            e.release(&u.doomed);
        }
    }

    /// Up to two splices applied as one edit: each
    /// is applied, then each is checked for well-formedness on the tree
    /// both produced (a window of one may read the other's text); refused
    /// or failed, both are undone, the later first. The second is planned
    /// by `second` once the first is applied (its places may name what
    /// the first inserted). Returns what each removed and inserted.
    pub(super) fn splice_pair(
        &mut self,
        first: SpliceArgs,
        second: impl FnOnce(&Self, Option<(u32, u32)>) -> Result<Option<SpliceArgs>, TreeEditError>,
    ) -> Result<(Spliced, Option<Spliced>), TreeEditError> {
        let a = self.splice_apply(
            first.parent,
            first.prev,
            &first.removed,
            first.fragment,
            true,
        )?;
        let plan = match second(self, a.span) {
            Ok(p) => p,
            Err(err) => {
                self.undo(a.undo)?;
                return Err(err);
            }
        };
        let b = match plan {
            Some(s) => match self.splice_apply(s.parent, s.prev, &s.removed, s.fragment, true) {
                Ok(b) => Some(b),
                Err(err) => {
                    self.undo(a.undo)?;
                    return Err(err);
                }
            },
            None => None,
        };
        // When the second splice replaced the element right after
        // the first (`splice_leaving_warnings`), the first's `next` is its
        // copy now.
        let mut a = a;
        if let (Some(n), Some(b)) = (a.check.next, &b) {
            if !self.is_live(n) {
                a.check.next = b.span.map(|s| s.0).or(b.check.next);
            }
        }
        // The first splice's labels are checked in the
        // text before it, so the elements after the second splice are
        // moved back by that one's bytes too, the others (the first's own
        // elements) not. Every caller puts the second after the first.
        let mut what = a.check.what;
        let mut ordered = true;
        if let Some(b) = &b {
            let from = self.rank_after(&b.check, b.span);
            // In every caller the second splice sits right after the first
            // in preorder (it moves the line break that ends the first's
            // line, or re-copies the element after the first without a
            // warning that left with it): no element lies between them, so
            // only the first's own elements were moved by the whole text's
            // delta. Checked in release too,
            // as part of `ordered` (an `Err(Internal)`, never a wrong
            // label check).
            let adjacent = Some(self.rank_after(&a.check, a.span))
                == match b.span {
                    Some((f, _)) => self.nav_rank(f).map(|r| r.0),
                    None => Some(from),
                };
            let head = a.span.map(|s| s.0).or(a.check.next);
            let head_rank = head.and_then(|h| self.nav_rank(h)).map(|r| r.0);
            ordered = adjacent && head_rank.is_none_or(|h| h <= from);
            let more =
                i128::from(b.check.what.added).saturating_sub(i128::from(b.check.what.removed));
            what.later = Some((from, more));
        }
        debug_assert!(
            ordered,
            "splice_pair: the second splice precedes the first or is not next to it"
        );
        let mut checked = if ordered {
            let c = &a.check;
            self.check_well_formed(c.parent, c.prev, c.next, a.span, what)
        } else {
            Err(TreeEditError::Internal)
        };
        if let (Ok(()), Some(b)) = (&checked, &b) {
            let c = &b.check;
            checked = self.check_well_formed(c.parent, c.prev, c.next, b.span, c.what);
        }
        if let Err(err) = checked {
            if let Some(b) = b {
                self.undo(b.undo)?;
            }
            self.undo(a.undo)?;
            return Err(err);
        }
        // Both seams are read before any merge (item ids are only valid
        // until then).
        let seams_a = self.seams(&a.undo);
        let spliced_a = (a.taken, a.span);
        let Some(b) = b else {
            self.accept_with(a.undo, seams_a);
            return Ok((spliced_a, None));
        };
        let seams_b = self.seams(&b.undo);
        self.accept_with(a.undo, seams_a);
        self.accept_with(b.undo, seams_b);
        Ok((spliced_a, Some((b.taken, b.span))))
    }

    /// The rank of the first element after an applied splice's change (the
    /// total element count at the end).
    fn rank_after(&self, check: &Check, span: Option<(u32, u32)>) -> u64 {
        let x = match (span, check.next, check.prev) {
            (Some((_, l)), _, _) => self.nav_after(l),
            (None, Some(n), _) => Some(n),
            (None, None, Some(p)) => self.nav_after(p),
            (None, None, None) => self.nav_after(check.parent),
        };
        x.and_then(|x| self.nav_rank(x))
            .map_or(self.nav_total().0, |r| r.0)
    }

    /// The applying part of [`SyntaxTree::splice`]: the checks that come
    /// before any change (limits, capacity), then the change. The caller
    /// validates and undoes.
    fn splice_apply(
        &mut self,
        parent: u32,
        prev: Option<u32>,
        removed: &[u32],
        fragment: Option<Fragment>,
        validate: bool,
    ) -> Result<Applied, TreeEditError> {
        // ---- everything read before anything changes
        let next = match removed.last() {
            Some(&l) => self.nav_next(l),
            None => match prev {
                Some(p) => self.nav_next(p),
                None => self.nav_first(parent),
            },
        };
        // Insertion goes right before `cut` (`None`: at the very end).
        let cut = match removed.last() {
            Some(&l) => self.nav_after(l),
            None => match next {
                Some(n) => Some(n),
                None => self.nav_after(parent),
            },
        };
        let taken = if removed.is_empty() {
            None
        } else {
            Some(self.fragment_of(removed, parent)?)
        };
        // Limits apply to edits. The whole text's size, and the
        // number of documents when documents are inserted (the window
        // reads only its own documents). The documents
        // are counted (`SyntaxTree::documents`, kept by every edit) and the
        // removed ones once, so the check is O(removed), never
        // O(documents).
        let limits = self.settings.get_limits();
        let removed_bytes = taken.as_ref().map_or(0, |t| t.arena.tiled);
        let added_bytes = fragment.as_ref().map_or(0, |f| f.arena.tiled);
        let total = usize::try_from(self.nav_total().1).map_err(|_| TreeEditError::Capacity)?;
        let new_total = total
            .saturating_sub(removed_bytes)
            .saturating_add(added_bytes);
        if validate && new_total > limits.get_max_document_size() && new_total > total {
            return Err(TreeEditError::Limit {
                code: oneyaml_core::code::DOCUMENT_TOO_LARGE,
            });
        }
        let (docs_removed, docs_added) = if parent == 0 {
            (
                removed
                    .iter()
                    .filter(|&&r| self.kind_of(r) == Some(SyntaxKind::Document))
                    .count(),
                fragment.as_ref().map_or(0, |f| {
                    f.root_kinds()
                        .filter(|k| *k == SyntaxKind::Document)
                        .count()
                }),
            )
        } else {
            (0, 0)
        };
        let documents = self
            .documents
            .saturating_sub(docs_removed)
            .saturating_add(docs_added);
        // The error count moves with the diagnostics removed and inserted
        // (O(their diagnostics), already copied or owned).
        let error_count = |a: &Arena| a.diagnostics.iter().filter(|(_, d)| d.is_error()).count();
        let errors = self
            .errors
            .saturating_sub(taken.as_ref().map_or(0, |t| error_count(&t.arena)))
            .saturating_add(fragment.as_ref().map_or(0, |f| error_count(&f.arena)));
        if validate && docs_added > 0 && documents > limits.get_max_documents() {
            return Err(TreeEditError::Limit {
                code: oneyaml_core::code::TOO_MANY_DOCUMENTS,
            });
        }
        let what = Removed {
            // A diagnostic leaving with what is removed is decided by the
            // window (the shortcut only reads the lines around).
            trivia_only: taken
                .as_ref()
                .is_none_or(|t| t.arena.diagnostics.is_empty())
                && removed.iter().all(|&r| {
                    self.kind_of(r).is_some_and(|k| {
                        matches!(
                            k,
                            SyntaxKind::Whitespace | SyntaxKind::LineBreak | SyntaxKind::Comment
                        )
                    })
                }),
            comment: removed
                .iter()
                .any(|&r| self.kind_of(r) == Some(SyntaxKind::Comment)),
            // Whether the element after the edit started a line before it.
            next_line_start: match removed.last() {
                Some(&r) => self.kind_of(r) == Some(SyntaxKind::LineBreak),
                // The start of the text starts a line.
                None => match prev {
                    Some(p) => self.kind_of(p) == Some(SyntaxKind::LineBreak),
                    None => parent == 0,
                },
            },
            added: u64::try_from(added_bytes).map_err(|_| TreeEditError::Capacity)?,
            removed: u64::try_from(removed_bytes).map_err(|_| TreeEditError::Capacity)?,
            later: None,
            // Read before the change (see `Removed::old_column`).
            old_column: (validate
                && prev.is_none()
                && self.errors == 0
                && self.kind_of(parent) == Some(SyntaxKind::BlockMapping))
            .then(|| self.entry_column(parent))
            .flatten(),
            // (O(removed), as copying it.)
            line_break: taken
                .as_ref()
                .is_some_and(|t| t.text().contains(['\n', '\r'])),
        };
        let mut new_first = None;
        if let Some(f) = &fragment {
            // (past the slots of a rolled-back group, too.)
            let next_slot = self.next_slot() as usize;
            let end = next_slot
                .checked_add(f.len())
                .ok_or(TreeEditError::Capacity)?;
            if u32::try_from(end).map_or(true, |e| e == NONE) {
                return Err(TreeEditError::Capacity);
            }
            new_first = Some(u32::try_from(next_slot).map_err(|_| TreeEditError::Capacity)?);
        }

        // ---- the change, recorded to be undone
        let created = self.edits.is_none();
        self.ensure_edits()?;
        let mut undo = Undo {
            created,
            next_slot: self.edits.as_deref().map_or(0, |e| e.next_slot),
            documents: self.documents,
            errors: self.errors,
            ..Undo::default()
        };
        let plan = Plan {
            parent,
            prev,
            next,
            cut,
            new_first,
        };
        match self.apply(plan, removed, fragment, &mut undo) {
            Ok(span) => {
                self.documents = documents;
                self.errors = errors;
                Ok(Applied {
                    taken,
                    span,
                    undo,
                    check: Check {
                        parent,
                        prev,
                        next,
                        what,
                    },
                })
            }
            Err(err) => {
                self.undo(undo)?;
                Err(err)
            }
        }
    }

    /// The mutating part of [`SyntaxTree::splice`].
    fn apply(
        &mut self,
        plan: Plan,
        removed: &[u32],
        fragment: Option<Fragment>,
        undo: &mut Undo,
    ) -> Result<Option<(u32, u32)>, TreeEditError> {
        let Plan {
            parent,
            prev,
            next,
            cut,
            new_first,
        } = plan;
        if let Some(&first) = removed.first() {
            let from = self.cut_logged(first, Some(undo))?;
            let until = cut.map(|c| self.cut_logged(c, Some(undo))).transpose()?;
            let e = self.edits.as_deref_mut().ok_or(TreeEditError::Internal)?;
            undo.after = match e.seq.prev(from) {
                Some(item) => Some(e.run_start(item).ok_or(TreeEditError::Internal)?),
                None => None,
            };
            let mut doomed = Vec::new();
            let mut at = Some(from);
            while let Some(item) = at {
                if Some(item) == until {
                    break;
                }
                doomed.push(item);
                at = e.seq.next(item);
            }
            for item in doomed {
                let run = e.seq.run(item).ok_or(TreeEditError::Internal)?;
                let key = run
                    .arena
                    .checked_add(run.a)
                    .ok_or(TreeEditError::Internal)?;
                e.runs.remove(&key);
                e.seq.remove(item);
                undo.doomed.push((key, run));
            }
        }
        let mut span = None;
        if let (Some(fragment), Some(first)) = (fragment, new_first) {
            let before = match cut {
                Some(c) => Some(self.cut_logged(c, Some(undo))?),
                None => None,
            };
            let e = self.edits.as_deref_mut().ok_or(TreeEditError::Internal)?;
            let after = match before {
                Some(b) => e.seq.prev(b),
                None => e.seq.last(),
            };
            let len = u32::try_from(fragment.len()).map_err(|_| TreeEditError::Capacity)?;
            let bytes = u64::try_from(fragment.arena.tiled).map_err(|_| TreeEditError::Capacity)?;
            // An arena is named by its first slot (never 0: the
            // base holds at least the stream root).
            let id = first;
            let last_root = fragment.arena.flat().roots().last().unwrap_or(0);
            let last = first
                .checked_add(u32::try_from(last_root).map_err(|_| TreeEditError::Internal)?)
                .ok_or(TreeEditError::Internal)?;
            let next_slot = first.checked_add(len).ok_or(TreeEditError::Capacity)?;
            // Reserve before mutating: the run's item.
            if !e.seq.can_insert(1) {
                return Err(TreeEditError::Capacity);
            }
            // O(roots + diagnostics), which
            // building the fragment already cost.
            let facts = if parent == 0 {
                let flat = fragment.arena.flat();
                let errors = flat.diagnostics.iter().any(|(_, d)| d.is_error());
                StreamFacts::of(flat, flat.roots(), errors)
            } else {
                StreamFacts::default()
            };
            // Whether the whole fragment is stream-level trivia.
            let trivia =
                parent == 0 && all_stream_trivia(fragment.arena.flat(), &facts, false, 0, len);
            // Its comment-run index, O(its elements), which
            // building it already cost.
            let runs = RunFacts::of(fragment.arena.flat());
            let marks = marks(
                fragment.arena.flat(),
                &runs,
                (parent == 0).then_some(&facts),
                0,
                len,
            );
            let diags = fragment.arena.flat().diagnostics_in(0, len);
            // The arena is kept first, so the run names its index.
            let graft = e
                .add_graft(
                    first,
                    Grafted {
                        parent,
                        arena: fragment.arena,
                        facts,
                        runs,
                    },
                )
                .ok_or(TreeEditError::Capacity)?;
            let run = Run {
                arena: id,
                graft,
                a: 0,
                b: len,
                bytes,
                diags,
                trivia,
                marks,
            };
            let Some(item) = e.seq.insert_after(after, run) else {
                e.drop_graft(first, false);
                return Err(TreeEditError::Capacity);
            };
            undo.grafted = Some(first);
            undo.arena_pushed = true;
            e.runs.insert(first, item);
            e.next_slot = next_slot;
            span = Some((first, last));
        }

        // ---- links (at most four overrides)
        let (head, tail) = match span {
            Some((f, l)) => (Some(f), Some(l)),
            None => (next, prev),
        };
        match prev {
            Some(p) => self.set_links(p, undo, |l| l.next = head.unwrap_or(NONE))?,
            None => self.set_links(parent, undo, |l| l.first = head.unwrap_or(NONE))?,
        }
        match next {
            Some(n) => self.set_links(n, undo, |l| l.prev = tail.unwrap_or(NONE))?,
            None => self.set_links(parent, undo, |l| l.last = tail.unwrap_or(NONE))?,
        }
        if let Some((f, l)) = span {
            self.set_links(f, undo, |x| x.prev = prev.unwrap_or(NONE))?;
            self.set_links(l, undo, |x| x.next = next.unwrap_or(NONE))?;
        }
        Ok(span)
    }

    /// Puts back what [`SyntaxTree::apply`] changed. The order of runs and
    /// every link is restored (the treap's shape may differ, which no read
    /// can see).
    fn undo(&mut self, u: Undo) -> Result<(), TreeEditError> {
        self.documents = u.documents;
        self.errors = u.errors;
        if u.created {
            self.edits = None;
            return Ok(());
        }
        let e = self.edits.as_deref_mut().ok_or(TreeEditError::Internal)?;
        for d in &u.dropped {
            e.dropped.remove(d);
        }
        for (slot, old) in u.links.into_iter().rev() {
            match old {
                Some(l) => e.links.insert(slot, l),
                None => e.links.remove(&slot),
            };
        }
        if let Some(first) = u.grafted {
            let item = e.runs.remove(&first).ok_or(TreeEditError::Internal)?;
            e.seq.remove(item);
            if u.arena_pushed {
                e.drop_graft(first, false);
            }
        }
        e.next_slot = u.next_slot;
        let mut at = match u.after {
            Some(key) => Some(*e.runs.get(&key).ok_or(TreeEditError::Internal)?),
            None => None,
        };
        for (key, run) in u.doomed {
            // The removals freed these items: re-inserting cannot fail.
            let item = e.seq.insert_after(at, run).ok_or(TreeEditError::Internal)?;
            e.runs.insert(key, item);
            at = Some(item);
        }
        // The splits this edit made are merged back, latest
        // first, so a refused edit leaves the runs as they were.
        for slot in u.cuts.into_iter().rev() {
            e.merge_at(slot);
        }
        Ok(())
    }

    /// The run starts where an accepted edit may have left two runs that
    /// continue each other: its cuts, and the place of its removal.
    /// Collected before any merge (item ids are only valid until then).
    fn seams(&self, u: &Undo) -> Vec<u32> {
        let mut out = u.cuts.clone();
        let Some(e) = self.edits.as_deref() else {
            return out;
        };
        if !u.doomed.is_empty() {
            let after = match u.after {
                Some(key) => e.runs.get(&key).and_then(|a| e.seq.next(*a)),
                None => e.seq.first(),
            };
            if let Some(key) = after.and_then(|item| e.run_start(item)) {
                out.push(key);
            }
        }
        out
    }

    /// Edit cost is independent of edit history: after an accepted edit,
    /// merges the runs it left continuing each other (a cut next to a
    /// removal of inserted text, e.g. an insertion and its removal), so R
    /// grows only with the edits whose text is still in the tree. O(k log
    /// R) for k seams (at most 4 per splice).
    fn coalesce(&mut self, seams: &[u32]) {
        if let Some(e) = self.edits.as_deref_mut() {
            for &slot in seams {
                e.merge_at(slot);
            }
        }
    }

    fn inserted(&self, span: Option<(u32, u32)>) -> Result<Inserted, TreeEditError> {
        let (f, l) = span.ok_or(TreeEditError::Internal)?;
        Ok(Inserted {
            first: self.id_of(f),
            last: self.id_of(l),
        })
    }

    // ========================================================= public API

    /// Copies the siblings `first..=last` and their subtrees into a
    /// detached [`Fragment`], leaving the tree as it is. O(copied).
    ///
    /// With [`SyntaxTree::parse_with`], this builds a fragment out of any
    /// text through the one parser: parse a snippet written for the
    /// target context, then extract the elements wanted.
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Stale`], [`TreeEditError::NoParent`] (the stream
    /// root), [`TreeEditError::NotSiblings`].
    pub fn extract(&self, first: NodeId, last: NodeId) -> Result<Fragment, TreeEditError> {
        let (f, l) = (self.live_slot(first)?, self.live_slot(last)?);
        let (parent, roots) = self.sibling_range(f, l)?;
        self.fragment_of(&roots, parent)
    }

    /// Removes element `id` with its subtree and returns it as a detached
    /// fragment (with its diagnostics). Handles into it become stale.
    ///
    /// # Errors
    ///
    /// See [`SyntaxTree::remove_range`].
    pub fn remove(&mut self, id: NodeId) -> Result<Fragment, TreeEditError> {
        self.remove_range(id, id)
    }

    /// Removes the siblings `first..=last` with their subtrees.
    /// O(depth + log R + removed + window).
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Stale`]; [`TreeEditError::NoParent`] for the stream
    /// root; [`TreeEditError::NotSiblings`]; [`TreeEditError::ReadOnly`]
    /// inside a scalar, alias, or error node, and for directives, tag
    /// properties, and block scalar headers; `ReadOnly { kind: Error }`
    /// for a range holding an [`SyntaxKind::Error`] node below document
    /// level (an error region is never removed or replaced by an
    /// edit; a whole document goes with its errors), and for an edit that
    /// could change how recovered input reads;
    /// [`TreeEditError::NotWellFormed`] when the text left would not read
    /// back as the tree left (e.g. removing the `:` of `a: 1`).
    pub fn remove_range(&mut self, first: NodeId, last: NodeId) -> Result<Fragment, TreeEditError> {
        let (f, l) = (self.live_slot(first)?, self.live_slot(last)?);
        let (parent, roots) = self.sibling_range(f, l)?;
        self.check_parent(parent)?;
        self.check_removable(&roots)?;
        let prev = self.nav_prev(f);
        // (a removal keeps its warnings, unlike a replacement:
        // the removed text could not be put back, since an insertion
        // cannot give the element after it the warning back.)
        let (taken, _) = self.splice(parent, prev, &roots, None, true)?;
        taken.ok_or(TreeEditError::Internal)
    }

    /// Replaces element `id` (with its subtree) by `with`'s elements.
    ///
    /// # Errors
    ///
    /// See [`SyntaxTree::replace_range`].
    pub fn replace(&mut self, id: NodeId, with: Fragment) -> Result<Replaced, TreeEditError> {
        self.replace_range(id, id, with)
    }

    /// Replaces the siblings `first..=last` by `with`'s elements. The
    /// fragment is dropped when the edit is refused (clone it first to keep
    /// it). O(depth + log R + removed + inserted roots + window).
    ///
    /// # Errors
    ///
    /// Those of [`SyntaxTree::remove_range`] and
    /// [`SyntaxTree::insert_before`].
    pub fn replace_range(
        &mut self,
        first: NodeId,
        last: NodeId,
        with: Fragment,
    ) -> Result<Replaced, TreeEditError> {
        let (f, l) = (self.live_slot(first)?, self.live_slot(last)?);
        let (parent, roots) = self.sibling_range(f, l)?;
        let pk = self.check_parent(parent)?;
        self.check_removable(&roots)?;
        self.check_insertable(parent, pk, &with)?;
        let prev = self.nav_prev(f);
        let (taken, span) = self.splice_leaving_warnings(parent, prev, roots, Some(with))?;
        Ok(Replaced {
            removed: taken.ok_or(TreeEditError::Internal)?,
            inserted: self.inserted(span)?,
        })
    }

    /// Inserts `fragment`'s elements right before `sibling`.
    /// O(depth + log R + inserted roots + window). The fragment is dropped
    /// when the edit is refused.
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Stale`]; [`TreeEditError::NoParent`];
    /// [`TreeEditError::EmptyFragment`]; [`TreeEditError::ReadOnly`]
    /// (inside a scalar, alias, or error node; directive, tag, or header
    /// elements); [`TreeEditError::Misplaced`];
    /// [`TreeEditError::TagContext`]; [`TreeEditError::NotWellFormed`]
    /// (e.g. `x` appended right after `a: 1`); [`TreeEditError::Capacity`].
    pub fn insert_before(
        &mut self,
        sibling: NodeId,
        fragment: Fragment,
    ) -> Result<Inserted, TreeEditError> {
        let s = self.live_slot(sibling)?;
        let parent = self.nav_parent(s).ok_or(TreeEditError::NoParent)?;
        let prev = self.nav_prev(s);
        self.insert_at(parent, prev, fragment)
    }

    /// Inserts `fragment`'s elements right after `sibling`.
    ///
    /// # Errors
    ///
    /// As [`SyntaxTree::insert_before`].
    pub fn insert_after(
        &mut self,
        sibling: NodeId,
        fragment: Fragment,
    ) -> Result<Inserted, TreeEditError> {
        let s = self.live_slot(sibling)?;
        let parent = self.nav_parent(s).ok_or(TreeEditError::NoParent)?;
        self.insert_at(parent, Some(s), fragment)
    }

    /// Inserts `fragment`'s elements as the last children of `parent`.
    ///
    /// # Errors
    ///
    /// As [`SyntaxTree::insert_before`], plus [`TreeEditError::NotANode`]
    /// when `parent` is a token.
    pub fn append_child(
        &mut self,
        parent: NodeId,
        fragment: Fragment,
    ) -> Result<Inserted, TreeEditError> {
        let p = self.live_slot(parent)?;
        self.check_parent(p)?;
        let prev = self.nav_last(p);
        self.insert_at(p, prev, fragment)
    }

    /// Inserts `fragment`'s elements so that the first becomes child
    /// number `position` of `parent` (`0..=children`). O(position) to find
    /// the place, then as [`SyntaxTree::insert_before`].
    ///
    /// # Errors
    ///
    /// As [`SyntaxTree::append_child`], plus [`TreeEditError::Position`].
    #[doc(hidden)]
    pub fn insert_child(
        &mut self,
        parent: NodeId,
        position: usize,
        fragment: Fragment,
    ) -> Result<Inserted, TreeEditError> {
        let p = self.live_slot(parent)?;
        self.check_parent(p)?;
        let mut prev = None;
        let mut seen = 0usize;
        while seen < position {
            let next = match prev {
                None => self.nav_first(p),
                Some(x) => self.nav_next(x),
            };
            match next {
                Some(n) => prev = Some(n),
                None => {
                    return Err(TreeEditError::Position {
                        position,
                        len: seen,
                    });
                }
            }
            seen = seen.saturating_add(1);
        }
        self.insert_at(p, prev, fragment)
    }

    fn insert_at(
        &mut self,
        parent: u32,
        prev: Option<u32>,
        fragment: Fragment,
    ) -> Result<Inserted, TreeEditError> {
        let pk = self.check_parent(parent)?;
        self.check_insertable(parent, pk, &fragment)?;
        let (_, span) = self.splice(parent, prev, &[], Some(fragment), true)?;
        self.inserted(span)
    }

    /// Replaces the text of token `id`, returning the new token's handle
    /// (the old one becomes stale). The text must read, through the one
    /// parser under the tree's own [`SyntaxTree::settings`], as
    /// exactly one token of the same kind: in flow context for a scalar,
    /// alias, or anchor inside a flow collection. Replaceable: plain,
    /// quoted, alias, anchor, comment, white space, and line break tokens.
    /// The caller supplies text valid in its context (e.g. a multi-line
    /// scalar's indentation); the edit must be well-formed (a comment
    /// replaced by a comment always is).
    ///
    /// # Errors
    ///
    /// [`TreeEditError::Stale`]; [`TreeEditError::NotAToken`];
    /// [`TreeEditError::ReadOnly`] for other token kinds;
    /// [`TreeEditError::InvalidToken`] when the text does not read as one
    /// such token; [`TreeEditError::NotWellFormed`] when it does, but not
    /// where it would go (e.g. a multi-line key, or indentation that moves
    /// an entry).
    pub fn replace_token(&mut self, id: NodeId, text: &str) -> Result<NodeId, TreeEditError> {
        let x = self.live_slot(id)?;
        let kind = self.kind_of(x).ok_or(TreeEditError::Internal)?;
        if !kind.is_token() {
            return Err(TreeEditError::NotAToken { kind });
        }
        if !REPLACEABLE.contains(&kind) {
            return Err(TreeEditError::ReadOnly { kind });
        }
        let parent = self.nav_parent(x).ok_or(TreeEditError::Internal)?;
        let flow = {
            let mut at = Some(parent);
            let mut flow = false;
            while let Some(a) = at {
                match self.kind_of(a) {
                    Some(
                        SyntaxKind::FlowMapping | SyntaxKind::FlowSequence | SyntaxKind::FlowPair,
                    ) => {
                        flow = true;
                        break;
                    }
                    Some(SyntaxKind::Document | SyntaxKind::Stream) | None => break,
                    _ => at = self.nav_parent(a),
                }
            }
            flow
        };
        let fragment = token_fragment(kind, text, flow, &self.settings)?;
        // A comment's text ends its line and means nothing: another comment
        // in its place is always well-formed (not re-read). A diagnostic
        // attached to it would leave with it, so then it is re-read.
        // In a tree with errors it is checked too, as any
        // edit (a diagnostic after it may have a label before it, which
        // would move with its element: an unclosed `[`).
        let validate = kind != SyntaxKind::Comment
            || self.error_count() > 0
            || !self.diagnostics_of(x).is_empty();
        let prev = self.nav_prev(x);
        let (_, span) = if validate {
            self.splice_leaving_warnings(parent, prev, vec![x], Some(fragment))?
        } else {
            self.splice(parent, prev, &[x], Some(fragment), false)?
        };
        Ok(self.inserted(span)?.first)
    }

    /// [`SyntaxTree::splice`] of a replacement of `roots`, validated.
    /// With `lenient_inline_colons`, a
    /// warning attached to the element right after `roots` whose labels
    /// all lie inside the removed bytes (a `Y0028` on the line break after
    /// a lenient `k: a: b` value) leaves with them when the edit is refused
    /// with it: the edit is then applied again with that warning dropped
    /// (`Edits::dropped`; the element and its handle stay), and the
    /// re-read decides. The one-splice edit, which keeps the warning, is
    /// tried first, so it stays when the new text reads with it again
    /// (e.g. a scalar replaced by a copy of itself); a refused edit
    /// changes nothing, so the retry starts from the same tree (the
    /// fragment is cloned for it only when such a warning exists).
    fn splice_leaving_warnings(
        &mut self,
        parent: u32,
        prev: Option<u32>,
        roots: Vec<u32>,
        fragment: Option<Fragment>,
    ) -> Result<Spliced, TreeEditError> {
        let Some((next, nths)) = self.orphaned_warnings(&roots)? else {
            return self.splice(parent, prev, &roots, fragment, true);
        };
        match self.splice(parent, prev, &roots, fragment.clone(), true) {
            Err(TreeEditError::NotWellFormed) => {}
            done => return done,
        }
        let drops: Vec<(u32, u32)> = nths.into_iter().map(|n| (next, n)).collect();
        self.splice_dropping(parent, prev, &roots, fragment, true, &drops)
    }

    /// For the removal of `roots`: the element right after them, and the
    /// positions (in its list) of the warnings attached to it whose labels
    /// all lie inside the removed bytes (see
    /// [`SyntaxTree::splice_leaving_warnings`]); `None` when there is no
    /// such warning. O(its diagnostics).
    fn orphaned_warnings(&self, roots: &[u32]) -> Result<Option<(u32, Vec<u32>)>, TreeEditError> {
        let (Some(&f), Some(&l)) = (roots.first(), roots.last()) else {
            return Ok(None);
        };
        let Some(next) = self.nav_after(l) else {
            return Ok(None);
        };
        // The common case (no diagnostic there) costs a lookup.
        let Some(loc) = self.locate(next) else {
            return Ok(None);
        };
        if loc.flat.diagnostics.is_empty() {
            return Ok(None);
        }
        let list = loc.flat.diagnostics_at(loc.local);
        if list.is_empty() {
            return Ok(None);
        }
        let lo = self.nav_bytes(f).ok_or(TreeEditError::Internal)?.0;
        let hi = self.nav_bytes(l).ok_or(TreeEditError::Internal)?.1;
        let mut nths = Vec::new();
        let from = loc.flat.start(loc.local);
        let to = self.nav_bytes(next).ok_or(TreeEditError::Internal)?.0;
        let to = usize::try_from(to).map_err(|_| TreeEditError::Internal)?;
        for (k, (_, d)) in list.iter().enumerate() {
            if self.is_dropped(next, k) {
                continue;
            }
            // Every label and fix inside: the earliest start and the
            // latest end (what a skeleton keeps).
            let s = super::arena::shift_span(Span::new(d.first_start(), d.last_end()), from, to);
            let inside = lo <= s.start as u64 && s.end as u64 <= hi;
            if d.severity() == oneyaml_core::Severity::Warning && inside {
                nths.push(u32::try_from(k).map_err(|_| TreeEditError::Capacity)?);
            }
        }
        Ok((!nths.is_empty()).then_some((next, nths)))
    }
}

/// The heap a diagnostic owns (its strings and vectors, by capacity), for
/// [`Edits::heap_bytes`].
#[cfg(test)]
fn diagnostic_heap_bytes(d: &Diagnostic) -> usize {
    use core::mem::size_of;
    use oneyaml_core::{Label, PathSegment, Replacement, Suggestion};
    let label = |l: &Label| l.message.capacity();
    let mut n = d.message.capacity();
    n = n.saturating_add(label(&d.primary));
    n = n.saturating_add(d.secondary.capacity().saturating_mul(size_of::<Label>()));
    n = d.secondary.iter().map(label).fold(n, usize::saturating_add);
    n = n.saturating_add(d.help.as_ref().map_or(0, String::capacity));
    if let Some(p) = &d.path {
        n = n.saturating_add(p.segments().len().saturating_mul(size_of::<PathSegment>()));
        for s in p.segments() {
            if let PathSegment::Key(k) = s {
                n = n.saturating_add(k.capacity());
            }
        }
    }
    n = n.saturating_add(
        d.suggestions
            .capacity()
            .saturating_mul(size_of::<Suggestion>()),
    );
    for s in &d.suggestions {
        n = n.saturating_add(s.message.capacity());
        n = n.saturating_add(s.edits.capacity().saturating_mul(size_of::<Replacement>()));
        n = s
            .edits
            .iter()
            .map(|e| e.text.capacity())
            .fold(n, usize::saturating_add);
    }
    n
}

/// Local elements `a..b` of `flat` are all stream-level trivia tokens:
/// children of the stream (`base`: local parent 0), or roots of an arena
/// inserted into the stream. A range longer than [`LONG_RUN`] is one only
/// inside a long run `facts` records; a shorter one is read element by
/// element. O(`LONG_RUN` + log).
fn all_stream_trivia(flat: Flat<'_>, facts: &StreamFacts, base: bool, a: u32, b: u32) -> bool {
    let (a, b) = (a as usize, b as usize);
    if b <= a {
        return true;
    }
    if b.saturating_sub(a) > LONG_RUN {
        return facts.run_start(b.saturating_sub(1)).is_some_and(|s| s <= a);
    }
    (a..b).all(|i| {
        let level = if base {
            flat.parent(i) == Some(0)
        } else {
            flat.parent(i).is_none()
        };
        level && flat.kind(i).is_some_and(|k| k.is_trivia() && k.is_token())
    })
}

/// Reads `text` as one token of `kind` through the parser, under
/// `settings`, and copies that token out.
pub(super) fn token_fragment(
    kind: SyntaxKind,
    text: &str,
    flow: bool,
    settings: &Settings,
) -> Result<Fragment, TreeEditError> {
    let bad = TreeEditError::InvalidToken { kind };
    let scalarish = matches!(
        kind,
        SyntaxKind::PlainScalar
            | SyntaxKind::SingleQuotedScalar
            | SyntaxKind::DoubleQuotedScalar
            | SyntaxKind::AliasName
            | SyntaxKind::Anchor
    );
    let empty_plain = kind == SyntaxKind::PlainScalar && text.is_empty();
    let src = if empty_plain {
        // An anchor on an empty node: its text token is the empty plain
        // scalar wanted.
        "&x".to_owned()
    } else if flow && scalarish {
        format!("[{text}]")
    } else {
        text.to_owned()
    };
    let tree = SyntaxTree::parse_with(src, settings);
    if tree.has_errors() {
        return Err(bad);
    }
    let wrapped = flow && scalarish && !empty_plain;
    let mut found = None;
    for n in tree.root().descendants().filter(|n| n.is_token()) {
        let k = n.kind();
        if wrapped
            && matches!(
                k,
                SyntaxKind::FlowSequenceOpen | SyntaxKind::FlowSequenceClose
            )
        {
            continue;
        }
        if empty_plain && k != SyntaxKind::PlainScalar {
            continue;
        }
        if n.text().is_empty() && !empty_plain {
            continue;
        }
        if found.is_some() {
            return Err(bad);
        }
        found = Some(n);
    }
    let token = found.ok_or(bad)?;
    if token.kind() != kind || token.text() != text {
        return Err(bad);
    }
    let (arena, _) = tree.copy_forest(&[token.slot()])?;
    Ok(Fragment {
        arena,
        context: String::new(),
        has_tags: false,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use crate::tree::Fragment;
    use crate::tree::fragment::tags_outside_documents;

    /// Memory, deterministically: after k edits the overlay holds at most
    /// 1 + 3k runs and 4k link overrides, whatever the document's size;
    /// the first edit costs one run.
    #[test]
    fn overlay_size_depends_on_edits_not_on_the_document() {
        for entries in [10usize, 10_000] {
            let src: String = (0..entries).map(|i| format!("k{i}: [a, b]\n")).collect();
            let mut tree = SyntaxTree::parse(src);
            let targets: Vec<NodeId> = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::FlowSequence)
                .map(|n| n.id())
                .step_by((entries / 10).max(1))
                .take(10)
                .collect();
            let mut k = 0usize;
            for id in targets {
                let first = tree.get(id).unwrap().children().nth(1).unwrap().id();
                let f = Fragment::parse("zz").unwrap();
                let _ = tree.replace(first, f).unwrap();
                k += 1;
                let e = tree.edits.as_deref().unwrap();
                assert!(e.runs.len() <= 1 + 3 * k, "{} runs after {k}", e.runs.len());
                assert!(e.links.len() <= 4 * k, "{} links after {k}", e.links.len());
            }
            // Cutting at an existing boundary adds no run.
            let slot = tree.root().first_child().unwrap().slot();
            let before = tree.edits.as_deref().unwrap().runs.len();
            let a = tree.cut_before(slot).unwrap();
            let b = tree.cut_before(slot).unwrap();
            assert_eq!(a, b);
            assert!(tree.edits.as_deref().unwrap().runs.len() <= before + 1);
            assert_eq!(tree.emit().matches("zz").count(), 10.min(entries));
        }
    }

    /// `n` toggles of the first plain scalar of `tree` between `x` and `y`.
    fn toggle(tree: &mut SyntaxTree, n: usize) {
        let mut id = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar)
            .unwrap()
            .id();
        for i in 0..n {
            id = tree
                .replace_token(id, if i % 2 == 0 { "x" } else { "y" })
                .unwrap();
        }
    }

    /// Memory retained after edits stays within the budget:
    /// 100k token toggles retain
    /// what 100 retain, on a 15-byte and on a 3 KB document (keeping
    /// every inserted arena and its link overrides would grow with the
    /// edits). Estimated from the overlay's containers (see
    /// [`Edits::heap_bytes`] on exact counting); the slots of
    /// dropped arenas resolve to nothing, and a fork copies what is live.
    /// (the estimate counts the dropped-warning set and the heap
    /// inside diagnostics too.)
    #[test]
    #[ignore = "slow: runs in CI"]
    fn toggles_retain_what_the_tree_holds_not_the_history() {
        let big: String = (0..300).map(|i| format!("key{i}: v\n")).collect();
        for src in ["a: [q, y]\nb: 1\n".to_owned(), format!("a: q\n{big}")] {
            let base = SyntaxTree::parse(src.as_str());
            let bytes = |t: &SyntaxTree| t.edits.as_deref().unwrap().heap_bytes();
            let mut few = base.clone();
            toggle(&mut few, 100);
            let mut many = base.clone();
            let first = many
                .root()
                .descendants()
                .find(|x| x.kind() == SyntaxKind::PlainScalar)
                .unwrap()
                .id();
            toggle(&mut many, 100_000);
            let (f, m) = (bytes(&few), bytes(&many));
            eprintln!("{} B: 100 toggles keep {f} B, 100k keep {m} B", src.len());
            assert!(
                m <= f + 256,
                "{} B: 100 toggles keep {f} B, 100k keep {m} B",
                src.len()
            );
            assert!(m < 2048, "{m} B retained");
            let e = many.edits.as_deref().unwrap();
            assert!(e.arenas.len() <= 1, "{} arenas", e.arenas.len());
            assert!(e.links.len() <= 4, "{} link overrides", e.links.len());
            // Slots of dropped arenas resolve to nothing (stale), never a
            // stub; the base slot the first toggle removed is stale too.
            assert!(many.get(first).is_err());
            let base_len = u32::try_from(many.kinds.len()).unwrap();
            let live: Vec<u32> = (base_len..e.next_slot())
                .filter(|s| many.kind_of(*s).is_some())
                .collect();
            assert_eq!(live.len(), 1, "{live:?}");
            assert!(many.is_live(live[0]));
            // The fork copies what is live.
            let fork = many.fork();
            assert!(bytes(&fork) <= m, "{} > {m}", bytes(&fork));
            assert_eq!(fork.emit(), many.emit());
            assert_eq!(many, SyntaxTree::parse(many.emit()));
        }
    }

    /// Reads keep nothing. Reading the text,
    /// spans, and diagnostics of every element leaves the overlay exactly
    /// as the edits left it (before the fix, every ancestor's text was
    /// copied into a per-node cache on first read).
    #[test]
    fn reads_leave_the_overlay_untouched() {
        let src = "a:\n  b:\n    - [x, y]\n    - z # c\nq: [1\n";
        let mut tree = SyntaxTree::parse(src);
        let x = tree
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::PlainScalar && n.text() == "x")
            .unwrap()
            .id();
        tree.replace_token(x, "xx").unwrap();
        let before = format!("{:?}", tree.edits);
        let mut total = 0usize;
        for n in tree.root().descendants() {
            total += n.text().len() + n.span().len();
            total += n.ancestors().map(|a| a.text().len()).sum::<usize>();
            total += tree.diagnostics_within(n).count();
            total += usize::from(n.diagnostic().is_some());
        }
        total += tree.diagnostics().count();
        assert!(total > 0);
        assert_eq!(format!("{:?}", tree.edits), before);
        // Split nodes' texts are built, tokens' are borrowed.
        let seq = tree
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::FlowSequence)
            .unwrap();
        assert!(matches!(seq.text(), Cow::Owned(_)));
        assert_eq!(seq.text(), "[xx, y]");
        let tok = seq.children().nth(1).unwrap().first_child().unwrap();
        assert!(matches!(tok.text(), Cow::Borrowed("xx")));
    }

    /// `copy_forest`'s open-document count says what the O(n)
    /// scan says, on every sibling range of every suite-like shape.
    #[test]
    fn copied_tag_flags_agree_with_a_scan() {
        for src in [
            "--- !t a\n--- [!t b]\n",
            "- !t [!u a, &x !v b]\n- c\n",
            "%TAG !e! tag:e,2000:\n--- !e!x\n- !e!y z\n",
            "!t\n",
        ] {
            let tree = SyntaxTree::parse(src);
            for n in tree.root().descendants() {
                let mut last = n;
                for _ in 0..3 {
                    let (arena, has_tags) = tree.copy_forest(&[last.slot()]).unwrap();
                    assert_eq!(has_tags, tags_outside_documents(&arena), "{src:?} {n:?}");
                    match last.next_sibling() {
                        Some(s) => last = s,
                        None => break,
                    }
                }
                let mut roots = vec![n.slot()];
                let mut at = n.next_sibling();
                while let Some(s) = at {
                    roots.push(s.slot());
                    at = s.next_sibling();
                }
                let (arena, has_tags) = tree.copy_forest(&roots).unwrap();
                assert_eq!(has_tags, tags_outside_documents(&arena), "{src:?} {n:?}");
            }
        }
    }

    /// Every copy through slices equals the element walk's, for every
    /// element and sibling range of `tree`.
    /// Returns how many elements were copied as slices.
    fn assert_slices_copy_as_the_walk(tree: &SyntaxTree, what: &str) -> usize {
        let mut sliced = 0;
        for n in tree.root().descendants() {
            let mut out = Arena::default();
            if tree
                .copy_slice(n.slot(), &mut out, &mut Vec::new(), &mut false)
                .unwrap()
            {
                sliced += 1;
            }
            let mut roots = vec![n.slot()];
            let mut at = n.next_sibling();
            for _ in 0..3 {
                let one = [*roots.last().unwrap()];
                for r in [&one[..], &roots[..]] {
                    let fast = tree.copy_forest_with(r, true);
                    let walk = tree.copy_forest_with(r, false);
                    assert_eq!(fast, walk, "{what}: {n:?} {r:?}");
                }
                match at {
                    Some(s) => {
                        roots.push(s.slot());
                        at = s.next_sibling();
                    }
                    None => break,
                }
            }
        }
        sliced
    }

    /// `copy_slice` writes exactly what the element walk
    /// writes (kinds, links, words, text, extras with their block headers
    /// and `%TAG` prefixes, diagnostics with dropped warnings left out, the
    /// tag flag), over the yaml-test-suite (strict and lenient) and shapes
    /// of its own, parsed and after edits that cut subtrees into pieces.
    #[test]
    fn slice_copies_equal_the_element_walk() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-suite/data/yaml-test-suite");
        let mut stack = vec![dir];
        let mut inputs = Vec::new();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                    inputs.push(std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        assert!(inputs.len() > 300);
        inputs.extend(
            [
                "k: |+ # c\n  x\n\n ",
                "- &a !t |2-\n   x\n- >\n  y\n- [!u a, b]\n",
                "%TAG !e! tag:e,2000:\n--- !e!x\n- !e!y z\n- {a: !e!w |\n  q\n}\n",
                "k: a: b\nm: 1\nn: [x: y: z]\n",
                "a: [1, 2\nb: }\n- c\n",
            ]
            .iter()
            .map(|s| (*s).to_owned()),
        );
        // (Edits at its start leave most of it in pieces of one run.)
        inputs.push((0..200).map(|i| format!("k{i}: [a, {{b: c}}]\n")).collect());
        let lenient = Settings::new().lenient_inline_colons(true);
        let (mut parsed, mut cut) = (0, 0);
        for src in &inputs {
            for settings in [&Settings::new(), &lenient] {
                let mut tree = SyntaxTree::parse_with(src.as_str(), settings);
                parsed += assert_slices_copy_as_the_walk(&tree, src);
                // Edits: removals and insertions cut subtrees into runs.
                let scalars: Vec<NodeId> = tree
                    .root()
                    .descendants()
                    .filter(|n| n.kind() == SyntaxKind::Scalar)
                    .map(|n| n.id())
                    .collect();
                let mut edited = false;
                for (i, id) in scalars.iter().enumerate().take(6) {
                    let ok = match i % 3 {
                        0 => tree.remove(*id).is_ok(),
                        1 => {
                            let f = Fragment::parse("x").unwrap();
                            tree.insert_after(*id, f).is_ok()
                        }
                        _ => {
                            let f = Fragment::parse("yy").unwrap();
                            tree.replace(*id, f).is_ok()
                        }
                    };
                    edited |= ok;
                }
                if edited {
                    cut += assert_slices_copy_as_the_walk(&tree, src);
                }
            }
        }
        // (Both paths ran: slices for most subtrees, parsed or edited.)
        assert!(parsed > 20_000 && cut > 5_000, "{parsed} {cut}");
    }

    /// Deterministically: copying a large subtree that an
    /// edit elsewhere left whole (one run) takes a constant number of
    /// overlay lookups, not one per element as the walk does; the copy is
    /// the walk's.
    #[test]
    fn copying_a_whole_subtree_looks_up_a_constant() {
        let mut lookups = Vec::new();
        for n in [100usize, 10_000] {
            let mut src = String::from("a: 1\nbig:\n");
            for i in 0..n {
                src.push_str(&format!("  k{i}: [v, {{w: {i}}}] # c\n"));
            }
            let mut tree = SyntaxTree::parse(src.as_str());
            let one = tree
                .root()
                .descendants()
                .find(|x| x.kind() == SyntaxKind::Scalar && x.text() == "1")
                .unwrap()
                .id();
            let _ = tree.replace(one, Fragment::parse("2").unwrap()).unwrap();
            assert!(tree.is_edited());
            let big = tree
                .root()
                .descendants()
                .filter(|x| x.kind() == SyntaxKind::BlockMapping)
                .nth(1)
                .unwrap()
                .slot();
            cost::take();
            let fast = tree.copy_forest(&[big]).unwrap();
            let (l, _) = cost::take();
            lookups.push(l);
            let walk = tree.copy_forest_with(&[big], false).unwrap();
            let (w, _) = cost::take();
            assert_eq!(fast, walk);
            assert!(w as usize > n, "{w}");
        }
        assert!(lookups.iter().all(|l| *l <= 8), "{lookups:?}");
    }

    /// An item space that runs out part-way through an
    /// edit leaves the tree, its text, and its handles as they were.
    #[test]
    fn capacity_failures_leave_the_tree_unchanged() {
        let src = "k: [a, b, c]\nv: {x: 1}\n";
        let mut tree = SyntaxTree::parse(src);
        let b = tree
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::Scalar && n.text() == "b")
            .unwrap()
            .id();
        // A first edit so the overlay exists, then a tight item space.
        let c = tree
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::Scalar && n.text() == "c")
            .unwrap()
            .id();
        let _ = tree.replace(c, Fragment::parse("cc").unwrap()).unwrap();
        let text = tree.emit();
        let all: Vec<(NodeId, Span)> = tree
            .root()
            .descendants()
            .map(|n| (n.id(), n.span()))
            .collect();
        let links = tree.edits.as_deref().unwrap().links.clone();
        // Two cuts need two items (the removed run frees one for the
        // insertion): with none or one to spare the edit fails part-way.
        for spare in [0, 1] {
            let seq = &mut tree.edits.as_deref_mut().unwrap().seq;
            seq.limit_items(seq.items_len() + spare);
            let r = tree.replace(b, Fragment::parse("[q]").unwrap());
            assert_eq!(r.err(), Some(TreeEditError::Capacity), "spare {spare}");
            assert_eq!(tree.emit(), text);
            for (id, span) in &all {
                assert_eq!(tree.get(*id).unwrap().span(), *span);
            }
            assert_eq!(tree.edits.as_deref().unwrap().links, links);
            assert!(tree.edits.as_deref().unwrap().arenas.len() == 1);
        }
        let seq = &mut tree.edits.as_deref_mut().unwrap().seq;
        seq.limit_items(seq.items_len() + 2);
        let _ = tree.replace(b, Fragment::parse("[q]").unwrap()).unwrap();
        assert_eq!(tree.emit(), "k: [a, [q], cc]\nv: {x: 1}\n");
    }

    /// (start, code) of the diagnostics `scan_diagnostics` visits from
    /// `slot` in one direction, up to `bound`.
    fn scanned(t: &SyntaxTree, slot: u32, forward: bool, bound: u64) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        t.scan_diagnostics(slot, forward, bound, |x| {
            let mut d = x.diagnostic.clone();
            d.shift(x.from, x.to);
            out.push((x.start, format!("{} {:?}", d.code(), d.span())));
            true
        });
        out
    }

    /// The diagnostic scan visits exactly the diagnostics the
    /// full list has on each side of every element, spans moved, edited
    /// or not; and a subtree's errors are found from its last diagnostic
    /// (errors end their document's content).
    #[test]
    fn diagnostic_scans_agree_with_the_full_list() {
        let lenient = Settings::new().lenient_inline_colons(true);
        let src = "%YAML 1.1\n--- \na: b: c\nd: [e, f]\ng: h: i\n--- \nx: y: z\nk: [1\n...\nq: r\n";
        let mut tree = SyntaxTree::parse_with(src, &lenient);
        let check = |t: &SyntaxTree| {
            let all: Vec<(u64, u64, String)> = t
                .diagnostics_by_element()
                .filter_map(|(n, d)| {
                    let n = n?;
                    Some((
                        n.preorder_index() as u64,
                        n.span().start as u64,
                        format!("{} {:?}", d.code, d.primary.span),
                    ))
                })
                .collect();
            let total = t.nav_total().0;
            for n in t.root().descendants() {
                let rank = n.preorder_index() as u64;
                let fwd: Vec<(u64, String)> = all
                    .iter()
                    .filter(|(r, _, _)| *r >= rank)
                    .map(|(_, s, d)| (*s, d.clone()))
                    .collect();
                assert_eq!(scanned(t, n.slot(), true, total), fwd, "{n:?}");
                let back: Vec<(u64, String)> = all
                    .iter()
                    .rev()
                    .filter(|(r, _, _)| *r <= rank)
                    .map(|(_, s, d)| (*s, d.clone()))
                    .collect();
                assert_eq!(scanned(t, n.slot(), false, 0), back, "{n:?}");
                // Bounded to the subtree.
                let end = rank + n.descendants().len() as u64;
                let inside: Vec<(u64, String)> = all
                    .iter()
                    .filter(|(r, _, _)| *r >= rank && *r < end)
                    .map(|(_, s, d)| (*s, d.clone()))
                    .collect();
                assert_eq!(scanned(t, n.slot(), true, end), inside, "{n:?}");
                let errors = t
                    .diagnostics_within(n)
                    .any(|(_, d)| d.severity == oneyaml_core::Severity::Error);
                assert_eq!(t.has_errors_within(n.slot()), errors, "{n:?}");
            }
        };
        check(&tree);
        let find = |t: &SyntaxTree, text: &str| {
            t.root()
                .descendants()
                .find(|x| x.kind() == SyntaxKind::PlainScalar && x.text() == text)
                .unwrap()
                .id()
        };
        let e = find(&tree, "e");
        let _ = tree.replace_token(e, "ee").unwrap();
        check(&tree);
        let a = find(&tree, "a");
        let _ = tree.replace_token(a, "aaa").unwrap();
        check(&tree);
        let r = find(&tree, "r");
        let _ = tree.replace_token(r, "rr").unwrap();
        check(&tree);
        assert_eq!(tree, SyntaxTree::parse_with(tree.emit(), &lenient));
    }

    /// The invariant `has_errors_within` and `labels_cross`
    /// rest on, over the whole yaml-test-suite (strict and lenient): in a
    /// document, no warning is attached after an error.
    #[test]
    fn errors_are_the_last_diagnostics_of_their_document() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-suite/data/yaml-test-suite");
        let mut stack = vec![dir];
        let mut inputs = Vec::new();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                    inputs.push(std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        assert!(inputs.len() > 300);
        // Directive errors are followed by warnings
        // of later directives (and, lenient, of the content).
        inputs.extend(DIRECTIVE_ERRORS.iter().map(|s| (*s).to_owned()));
        let mut docs_with_errors = 0;
        let mut directive_errors = 0;
        for settings in [Settings::new(), Settings::new().lenient_inline_colons(true)] {
            for src in &inputs {
                let tree = SyntaxTree::parse_with(src.as_str(), &settings);
                for doc in tree.root().children() {
                    // Content errors end the content: after the `---`
                    // (or from the start without one), no warning follows
                    // an error.
                    let marker = doc
                        .children()
                        .find(|c| c.kind() == SyntaxKind::DocumentStartMarker)
                        .map_or(0, |m| m.preorder_index());
                    let all: Vec<_> = tree
                        .diagnostics_within(doc)
                        .map(|(n, d)| (n.map_or(usize::MAX, |n| n.preorder_index()), d))
                        .collect();
                    let content: Vec<_> = all
                        .iter()
                        .filter(|(at, _)| *at > marker)
                        .map(|(_, d)| d.severity)
                        .collect();
                    if let Some(i) = content
                        .iter()
                        .position(|s| *s == oneyaml_core::Severity::Error)
                    {
                        assert!(
                            content[i..]
                                .iter()
                                .all(|s| *s == oneyaml_core::Severity::Error),
                            "{src:?}: {content:?}"
                        );
                    }
                    // Errors before it are directive errors, and recorded.
                    let early = all
                        .iter()
                        .any(|(at, d)| *at < marker && d.severity == oneyaml_core::Severity::Error);
                    let recorded = doc.kind() == SyntaxKind::Document
                        && tree
                            .facts_at(doc.slot())
                            .is_some_and(|(f, l, _)| f.directive_error(l));
                    assert_eq!(early, recorded, "{src:?}");
                    directive_errors += usize::from(early);
                    let any = all
                        .iter()
                        .any(|(_, d)| d.severity == oneyaml_core::Severity::Error);
                    docs_with_errors += usize::from(any);
                    assert_eq!(tree.has_errors_within(doc.slot()), any, "{src:?}");
                }
                assert_eq!(tree.has_errors_within(0), tree.has_errors());
            }
        }
        assert!(docs_with_errors > 50, "{docs_with_errors}");
        assert!(
            directive_errors >= 2 * DIRECTIVE_ERRORS.len(),
            "{directive_errors}"
        );
    }

    /// A line-end edit made of two splices checks the
    /// first one's labels with that splice's own shift (and the second's
    /// for the elements after the second). Swept over every range and
    /// line-end edit of inputs with `Y0028` warnings (labels before their
    /// element) next to block scalars and line breaks at other levels:
    /// accepted edits equal their re-parse, diagnostics and spans included
    /// (each on a fork of the tree, where its handles resolve).
    #[test]
    fn pair_edits_check_labels_in_each_splices_own_text() {
        let lenient = Settings::new().lenient_inline_colons(true);
        let inputs = [
            "a:\n  b: 1\nc: d: e\n",
            "a:\n  b: |\n    x\nc: d: e\nf: g\n",
            "- k: v\n- p: q: r\n",
            "a:\n  - x: y: z\n  - w\nb: c: d\n",
        ];
        let withs = ["|\n  new\n", "n: m: o\n", "9\n", ">-\n  f\n\n"];
        let (mut accepted, mut pairs) = (0usize, 0usize);
        for src in inputs {
            let tree = SyntaxTree::parse_with(src, &lenient);
            assert!(tree.diagnostics().len() > 0, "{src:?}");
            for n in tree.root().descendants() {
                for with in withs {
                    let Ok(f) = Fragment::parse_with(with, &lenient) else {
                        continue;
                    };
                    for insert in [false, true] {
                        let mut e = tree.fork();
                        let arenas =
                            |t: &SyntaxTree| t.edits.as_deref().map_or(0, |e| e.arenas.len());
                        let r = if insert {
                            e.insert_line_after(n.id(), f.clone()).map(|_| ())
                        } else {
                            e.replace_to_line_end(n.id(), n.id(), f.clone()).map(|_| ())
                        };
                        if r.is_ok() {
                            accepted += 1;
                            pairs += usize::from(arenas(&e) > 1);
                            let again = SyntaxTree::parse_with(e.emit(), &lenient);
                            assert_eq!(e, again, "{src:?} {n:?} {with:?} {insert}");
                        }
                    }
                }
            }
        }
        assert!(
            accepted > 20 && pairs > 5,
            "{accepted} accepted, {pairs} pairs"
        );
    }

    /// Documents whose directives hold an error that
    /// later warnings follow.
    const DIRECTIVE_ERRORS: &[&str] = &[
        "%YAML 2.0\n%FOO bar\n---\na: 1\n",
        "%TAG !a! x\n%TAG !a! y\n%FOO\n--- a\n",
        "%YAML 1.1\n%YAML 1.2\n%FOO\n---\nk: a: b\n",
        "a: 1\n...\n%YAML 2.0\n%FOO\n--- [b]\n",
    ];

    /// The backward pass of `labels_cross` may stop
    /// before a document's directive errors because they never cross: each
    /// is a zero-width error node whose labels are all at its own start.
    #[test]
    fn directive_errors_never_cross() {
        for settings in [Settings::new(), Settings::new().lenient_inline_colons(true)] {
            for src in DIRECTIVE_ERRORS {
                let tree = SyntaxTree::parse_with(*src, &settings);
                let mut seen = 0;
                for (n, d) in tree.diagnostics_by_element() {
                    let n = n.unwrap();
                    let in_directives = n.kind() == SyntaxKind::Error
                        && n.parent().is_some_and(|p| {
                            p.kind() == SyntaxKind::Document
                                && p.children().any(|c| {
                                    c.kind() == SyntaxKind::DocumentStartMarker
                                        && c.preorder_index() > n.preorder_index()
                                })
                        });
                    if !in_directives {
                        continue;
                    }
                    seen += 1;
                    let at = n.span().start;
                    assert!(n.span().is_empty(), "{src:?}");
                    assert_eq!(d.primary.span, Span::empty(at), "{src:?}");
                    assert!(d.secondary.iter().all(|l| l.span.start >= at));
                    assert!(
                        d.suggestions
                            .iter()
                            .all(|s| s.edits.iter().all(|e| e.span.start >= at))
                    );
                }
                assert!(seen >= 1, "{src:?}");
                // Edits after them are accepted exactly when the re-parse
                // agrees (the implication), errors included; each on a
                // fork, where the handle resolves.
                for n in tree.root().descendants() {
                    if n.kind() != SyntaxKind::PlainScalar {
                        continue;
                    }
                    let mut e = tree.fork();
                    if e.replace_token(n.id(), "zz").is_ok() {
                        assert_eq!(e, SyntaxTree::parse_with(e.emit(), &settings), "{src:?}");
                    }
                }
            }
        }
    }

    /// What `errors_reach` and
    /// `before_errors` rely on when they stop scanning a document's errors
    /// (backwards) at the first one with a directive code
    /// (`validate::is_directive_error`): every such diagnostic in a
    /// document is a directive error (a zero-width error node before its
    /// `---`), and every error before it in the document is one too, unless
    /// the document is recorded as mixed (`facts.rs`), which the scans
    /// take, in O(1), as reaching every edit next to it. Over
    /// the yaml-test-suite, every fuzz corpus and the directive shapes,
    /// strict and lenient.
    #[test]
    fn directive_codes_mark_directive_errors_only() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut stack = vec![
            root.join("test-suite/data/yaml-test-suite"),
            root.join("fuzz/corpus"),
            root.join("fuzz/regressions"),
        ];
        let mut inputs: Vec<String> = DIRECTIVE_ERRORS.iter().map(|s| (*s).to_owned()).collect();
        inputs.extend(
            [
                "%YAML 1.2\n%YAML 1.2\n---\na: x\nb: ]\n",
                "%TAG !t! a\n%TAG !t! b\n---\n- [x\n",
                "%YAML 2.0\n---\n'x\n\u{feff}y\n",
                "a: [x\n---\n%YAML 1.2\n%YAML 1.2\n",
                "%YAML 0.2\n----\nm\n...\n%YAML 1.2\n%YAML 1.2\n---\n",
            ]
            .map(str::to_owned),
        );
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_none_or(|x| x != "md") {
                    let b = std::fs::read(&p).unwrap();
                    inputs.push(String::from_utf8_lossy(&b).into_owned());
                }
            }
        }
        assert!(inputs.len() > 3000, "{}", inputs.len());
        let mut seen = 0usize;
        let mut mixed_docs = 0usize;
        for settings in [Settings::new(), Settings::new().lenient_inline_colons(true)] {
            for src in &inputs {
                let tree = SyntaxTree::parse_with(src.as_str(), &settings);
                for doc in tree.root().children() {
                    if doc.kind() != SyntaxKind::Document {
                        continue;
                    }
                    let marker = doc
                        .children()
                        .find(|c| c.kind() == SyntaxKind::DocumentStartMarker)
                        .map(|m| m.preorder_index());
                    let all: Vec<_> = tree.diagnostics_within(doc).collect();
                    // Read backwards, as the scans do: once a directive
                    // code is met, only directive-coded errors (and
                    // warnings) come before it.
                    let mut past_directive = false;
                    let mut mixed = false;
                    for (n, d) in all.iter().rev() {
                        let directive = super::super::validate::is_directive_error(d.code);
                        if d.severity != oneyaml_core::Severity::Error {
                            continue;
                        }
                        if directive {
                            seen += 1;
                            let n = n.as_ref().unwrap();
                            assert_eq!(n.kind(), SyntaxKind::Error, "{src:?}");
                            assert!(n.span().is_empty(), "{src:?}");
                            assert!(
                                marker.is_some_and(|m| m > n.preorder_index()),
                                "{src:?}: a directive code after the document's `---`"
                            );
                            past_directive = true;
                        } else {
                            mixed |= past_directive;
                        }
                    }
                    // Such an error before a directive error (a failed
                    // directive block kept with the next directives) is
                    // recorded; the scans take it as reaching (O(1)).
                    let recorded = tree
                        .facts_at(doc.slot())
                        .is_some_and(|(f, l, _)| f.mixed_directive_error(l));
                    assert_eq!(mixed, recorded, "{src:?}");
                    mixed_docs += usize::from(mixed);
                }
            }
        }
        assert!(seen > 20, "{seen}");
        assert!(mixed_docs >= 2, "{mixed_docs}");
    }

    /// A refused edit merges back the runs it split,
    /// so R grows only with accepted edits (at most 3 each), never with
    /// refusals.
    #[test]
    fn refused_edits_leave_the_runs_as_they_were() {
        let src = "a: 1\nb: [x, y, z]\nc: 3\nd: {k: v}\n";
        let mut tree = SyntaxTree::parse(src);
        let colon = |t: &SyntaxTree, n: usize| {
            t.root()
                .descendants()
                .filter(|x| x.kind() == SyntaxKind::MappingValue)
                .nth(n)
                .unwrap()
                .id()
        };
        // Refusals on an unedited tree leave no overlay.
        for n in 0..4 {
            assert_eq!(
                tree.remove(colon(&tree, n)),
                Err(TreeEditError::NotWellFormed)
            );
            assert!(!tree.is_edited());
        }
        let y = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar && x.text() == "y")
            .unwrap()
            .id();
        let _ = tree.replace_token(y, "yy").unwrap();
        let runs = tree.edits.as_deref().unwrap().runs.len();
        // The treap's storage reaches the peak of one attempt and reuses
        // freed items after that.
        let mut items = None;
        let text = tree.emit();
        for round in 0..20 {
            for n in 0..4 {
                let r = tree.remove(colon(&tree, n));
                assert_eq!(r, Err(TreeEditError::NotWellFormed), "{round} {n}");
            }
            let comma = tree
                .root()
                .descendants()
                .find(|x| x.kind() == SyntaxKind::FlowEntry)
                .unwrap()
                .id();
            assert_eq!(tree.remove(comma), Err(TreeEditError::NotWellFormed));
            let e = tree.edits.as_deref().unwrap();
            assert_eq!(e.runs.len(), runs, "runs grew after refusals");
            assert_eq!(*items.get_or_insert(e.seq.items_len()), e.seq.items_len());
        }
        assert_eq!(tree.emit(), text);
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
    }

    /// The runs cover every live element's bytes;
    /// the read path's fallback (joining tokens) gives the same text, and
    /// a range past the end is an `Internal` error, never a truncation.
    #[test]
    fn text_ranges_are_covered_or_an_error() {
        let mut tree = SyntaxTree::parse("a:\n  b: [1, 2]\n  c: |\n    x\n# end\n");
        let one = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar && x.text() == "1")
            .unwrap()
            .id();
        let _ = tree.replace_token(one, "11").unwrap();
        let two = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::Scalar && x.text() == "2")
            .unwrap()
            .id();
        let _ = tree
            .replace(two, Fragment::parse("{z: 2}").unwrap())
            .unwrap();
        for n in tree.root().descendants() {
            let (a, b) = tree.nav_bytes(n.slot()).unwrap();
            let runs = tree.try_text_range(a, b).unwrap();
            assert_eq!(runs, tree.text_from_tokens(n.slot()), "{n:?}");
            assert_eq!(runs, n.text());
        }
        let total = tree.nav_total().1;
        assert_eq!(
            tree.try_text_range(total, total + 5),
            Err(TreeEditError::Internal)
        );
        assert_eq!(tree.try_text_range(0, total).unwrap(), tree.emit());
        assert_eq!(tree.byte_at(0), Some(b'a'));
        assert_eq!(tree.byte_at(total), None);
    }

    /// Columns are counted in the parser's unit, bytes,
    /// edited or not (a count of characters put the re-read context in
    /// another place than the parser after non-ASCII text on a line).
    #[test]
    fn columns_are_bytes_as_the_parser_counts_them() {
        let src = "\u{e9}\u{1f600}: [a, b]\n";
        let mut tree = SyntaxTree::parse(src);
        let seq_at = src.find('[').unwrap() as u64;
        assert_eq!(tree.column_at(seq_at), Some(8));
        let a = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar && x.text() == "a")
            .unwrap()
            .id();
        let _ = tree.replace_token(a, "\u{2603}").unwrap();
        let b_at = tree.emit().find('b').unwrap() as u64;
        assert_eq!(tree.column_at(seq_at), Some(8));
        assert_eq!(tree.column_at(b_at), Some(14));
        assert_eq!(tree.column_at(0), Some(0));
    }

    /// A fuzz finding: a byte order mark starts the line, as for the
    /// parser (`scan_trivia`), edited or not.
    #[test]
    fn columns_skip_a_byte_order_mark() {
        let src = "\u{feff}a: [b]\n--- \u{feff}\n";
        let mut tree = SyntaxTree::parse(src);
        assert_eq!(tree.column_at(3), Some(0));
        assert_eq!(tree.column_at(src.find('[').unwrap() as u64), Some(3));
        let b = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar && x.text() == "b")
            .unwrap()
            .id();
        let _ = tree.replace_token(b, "cc").unwrap();
        assert_eq!(tree.column_at(src.find('[').unwrap() as u64), Some(3));
        assert_eq!(tree.column_at(0), Some(0));
    }

    /// The last leaf and the chain of last children agree
    /// with `nav_last` level by level, edited or not.
    #[test]
    fn last_chains_agree_with_last_children() {
        let mut tree = SyntaxTree::parse("a:\n  b:\n    - [x, {y: z}]\n    - q\nc: []\n");
        let check = |t: &SyntaxTree| {
            for n in t.root().descendants() {
                let mut want = vec![n.slot()];
                let mut at = n.slot();
                while let Some(l) = t.nav_last(at) {
                    want.push(l);
                    at = l;
                }
                assert_eq!(t.last_chain(n.slot()), want, "{n:?}");
                assert_eq!(t.nav_last_leaf(n.slot()), want.last().copied());
            }
        };
        check(&tree);
        let q = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::Scalar && x.text() == "q")
            .unwrap()
            .id();
        let _ = tree
            .replace(q, Fragment::parse("[r, [s]]").unwrap())
            .unwrap();
        check(&tree);
    }

    /// `is_empty` agrees with `len`, edited or not.
    #[test]
    fn is_empty_agrees_with_len() {
        let mut tree = SyntaxTree::parse("[a]");
        assert!(!tree.is_empty());
        assert_eq!(tree.is_empty(), matches!(tree.len(), 0));
        let a = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar)
            .unwrap()
            .id();
        let _ = tree.replace_token(a, "b").unwrap();
        assert!(!tree.is_empty());
        assert_eq!(tree.is_empty(), matches!(tree.len(), 0));
    }

    /// `unwrap_root` moves the element's warnings to
    /// its first child, and refuses an element without children that
    /// carries one (it would move to the next root or be dropped).
    #[test]
    fn unwrap_root_keeps_warnings() {
        let warning = Diagnostic::warning(
            oneyaml_core::code::YAML_VERSION_1_1,
            "w",
            oneyaml_core::Label::new(Span::new(0, 1), ""),
        );
        let mut f = Fragment::parse("[a, b]").unwrap();
        f.arena.diagnostics.push((0, Stored::full(warning.clone())));
        let g = f.unwrap_root(0).unwrap();
        assert_eq!(g.arena.diagnostics.len(), 1);
        let at = g.arena.diagnostics[0].0 as usize;
        assert_eq!(at, 0);
        assert_eq!(g.arena.flat().kind(at), Some(SyntaxKind::FlowSequenceOpen));
        // A hand-built childless root followed by another root.
        let mut f = Fragment::parse("x").unwrap();
        let arena = &mut f.arena;
        arena.kinds.insert(0, SyntaxKind::Alias);
        arena.parents.insert(0, NONE);
        arena.words.insert(0, 1);
        for p in arena.parents.iter_mut().skip(1) {
            if *p != NONE {
                *p += 1;
            }
        }
        for (i, w) in arena.words.iter_mut().enumerate().skip(1) {
            if !arena.kinds[i].is_token() {
                *w += 1;
            }
        }
        arena.diagnostics.push((0, Stored::full(warning)));
        assert_eq!(
            f.unwrap_root(0),
            Err(TreeEditError::ReadOnly {
                kind: SyntaxKind::Alias
            })
        );
    }

    /// A document's leading comments are the
    /// stream's (C4), so `tag_context` stops at the document's first child
    /// when it has no directive: O(1), whatever the comments before it.
    #[test]
    fn tag_context_reads_only_directives() {
        let src = format!("{}a: !t x\n", "# c\n".repeat(1000));
        let tree = SyntaxTree::parse(src);
        let doc = tree
            .root()
            .children()
            .find(|x| x.kind() == SyntaxKind::Document)
            .unwrap();
        assert_eq!(doc.first_child().unwrap().kind(), SyntaxKind::BlockMapping);
        assert_eq!(tree.tag_context(doc.slot()), "");
        let tree = SyntaxTree::parse("# c\n%TAG !e! tag:e,2000:\n# d\n--- !e!x a\n");
        let doc = tree
            .root()
            .children()
            .find(|x| x.kind() == SyntaxKind::Document)
            .unwrap();
        assert_eq!(doc.first_child().unwrap().kind(), SyntaxKind::TagDirective);
        assert_eq!(tree.tag_context(doc.slot()), "%TAG !e! tag:e,2000:\n");
    }

    /// A refused edit is undone exactly, including the removal of
    /// runs and the link overrides, and the tree then edits on normally.
    #[test]
    fn refused_edits_are_undone() {
        let src = "a: 1\nb: [x, y]\nc: 3\n";
        let mut tree = SyntaxTree::parse(src);
        let find = |t: &SyntaxTree, k: SyntaxKind, n: usize| {
            t.root()
                .descendants()
                .filter(|x| x.kind() == k)
                .nth(n)
                .unwrap()
                .id()
        };
        // Refused on an unedited tree: no overlay is left behind.
        let colon = find(&tree, SyntaxKind::MappingValue, 0);
        assert_eq!(tree.remove(colon), Err(TreeEditError::NotWellFormed));
        assert!(!tree.is_edited());
        let y = find(&tree, SyntaxKind::Scalar, 4);
        let _ = tree.replace(y, Fragment::parse("yy").unwrap()).unwrap();
        let text = tree.emit();
        let all: Vec<(NodeId, Span)> = tree
            .root()
            .descendants()
            .map(|n| (n.id(), n.span()))
            .collect();
        let links = tree.edits.as_deref().unwrap().links.clone();
        let arenas = tree.edits.as_deref().unwrap().arenas.len();
        // Removing the comma of `[x, yy]` gives `[x yy]`: one scalar.
        let comma = find(&tree, SyntaxKind::FlowEntry, 0);
        assert_eq!(tree.remove(comma), Err(TreeEditError::NotWellFormed));
        let map = find(&tree, SyntaxKind::BlockMapping, 0);
        assert_eq!(
            tree.append_child(map, Fragment::parse("x").unwrap()),
            Err(TreeEditError::NotWellFormed)
        );
        assert_eq!(tree.emit(), text);
        for (id, span) in &all {
            assert_eq!(tree.get(*id).unwrap().span(), *span);
        }
        let e = tree.edits.as_deref().unwrap();
        assert_eq!(e.links, links);
        assert_eq!(e.arenas.len(), arenas);
        // Still editable.
        let three = find(&tree, SyntaxKind::Scalar, 6);
        let _ = tree.replace(three, Fragment::parse("33").unwrap()).unwrap();
        assert_eq!(tree.emit(), "a: 1\nb: [x, yy]\nc: 33\n");
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
    }

    /// Every run's diagnostic count is its arena's count over its range,
    /// and the treap's sums add up (the scans skip runs by them).
    fn assert_diag_sums(tree: &SyntaxTree) {
        let e = tree.edits.as_deref().unwrap();
        let mut at = e.seq.first();
        let mut total = 0u64;
        while let Some(item) = at {
            let run = e.seq.run(item).unwrap();
            let (flat, _, _) = tree.arena_flat(run.arena).unwrap();
            assert_eq!(run.diags, flat.diagnostics_in(run.a, run.b), "{run:?}");
            total += run.diags;
            at = e.seq.next(item);
        }
        let listed = tree
            .diagnostics_by_element()
            .filter(|(n, _)| n.is_some())
            .count();
        // (warnings that left the tree stay counted in their
        // runs, and are skipped when read.)
        assert_eq!(total, (listed + e.dropped.len()) as u64);
    }

    /// A dropped warning is recorded by its element's
    /// slot, undone with a refused edit, and released with its element.
    #[test]
    fn dropped_warnings_are_undone_and_released() {
        let settings = Settings::new().lenient_inline_colons(true);
        let mut tree = SyntaxTree::parse_with("k: a: b\nm: 1\n", &settings);
        let find = |t: &SyntaxTree, k: SyntaxKind, text: &str| {
            t.root()
                .descendants()
                .find(|x| x.kind() == k && x.text() == text)
                .unwrap()
                .id()
        };
        let v = find(&tree, SyntaxKind::PlainScalar, "a: b");
        let _ = tree.replace_token(v, "x").unwrap();
        let e = tree.edits.as_deref().unwrap();
        assert_eq!(e.dropped.len(), 1);
        assert_diag_sums(&tree);
        // A refused edit adds none.
        let colon = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::MappingValue)
            .unwrap()
            .id();
        assert_eq!(tree.remove(colon), Err(TreeEditError::NotWellFormed));
        assert_eq!(tree.edits.as_deref().unwrap().dropped.len(), 1);
        // Its element replaced: released.
        let brk = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::LineBreak)
            .unwrap()
            .id();
        let _ = tree.replace_token(brk, "\n").unwrap();
        assert!(tree.edits.as_deref().unwrap().dropped.is_empty());
        assert_diag_sums(&tree);
        assert_eq!(tree.diagnostics().count(), 0);
        assert_eq!(tree, SyntaxTree::parse_with(tree.emit(), &settings));
        // Refused as a whole: a value that reads as a lenient value with a
        // warning of its own elsewhere is refused twice, nothing dropped.
        let mut tree = SyntaxTree::parse_with("k: a: b\nm: 1\n", &settings);
        let node = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::Scalar && x.text() == "a: b")
            .unwrap()
            .id();
        let map = Fragment::parse("p: q").unwrap();
        assert_eq!(tree.replace(node, map), Err(TreeEditError::NotWellFormed));
        assert!(tree.edits.as_deref().is_none_or(|e| e.dropped.is_empty()));
        assert_eq!(tree.diagnostics().count(), 1);
    }

    /// Every run's `trivia` flag says exactly whether all its elements
    /// are stream-level trivia tokens (the walks over
    /// stream-level trivia jump over the runs by it).
    fn assert_trivia_flags(tree: &SyntaxTree) {
        let Some(e) = tree.edits.as_deref() else {
            return;
        };
        let mut at = e.seq.first();
        while let Some(item) = at {
            let run = e.seq.run(item).unwrap();
            let all = (run.a..run.b).all(|l| {
                let slot = run.arena + l;
                tree.nav_parent(slot) == Some(0)
                    && tree.kind_of(slot).is_some_and(|k| k.is_trivia())
            });
            assert_eq!(run.trivia, all, "{run:?}");
            at = e.seq.next(item);
        }
    }

    /// The facts of an inserted arena. A fragment of
    /// more than 64 stream-level trivia elements records its long run (so
    /// walks jump over it), whole or cut by later edits; an extracted
    /// document with a directive error (`%YAML 2.0`) inserted at stream
    /// level records it (so `has_errors_within` sees it without a scan).
    #[test]
    fn inserted_arenas_record_their_stream_facts() {
        let mut tree = SyntaxTree::parse("a: 1\n--- b\n");
        let doc_b = tree
            .root()
            .children()
            .filter(|x| x.kind() == SyntaxKind::Document)
            .nth(1);
        let doc_b = doc_b.unwrap().id();
        let comments = Fragment::parse(&"# c\n".repeat(40)).unwrap();
        assert_eq!(comments.len(), 80);
        let ins = tree.insert_before(doc_b, comments).unwrap();
        let first = tree.get(ins.first).unwrap().slot();
        let last = tree.get(ins.last).unwrap().slot();
        let (facts, local, _) = tree.facts_at(last).unwrap();
        assert_eq!(facts.run_start(local), Some(0));
        assert_eq!(facts.run_end(0), Some(79));
        assert_eq!(tree.trivia_run_start(last), first);
        assert_eq!(tree.trivia_run_end(first), last);
        assert_trivia_flags(&tree);
        // Cut in the middle: each piece is bounded by its overlay run.
        let mid = tree
            .root()
            .children()
            .filter(|x| x.kind() == SyntaxKind::Comment)
            .nth(20)
            .unwrap();
        let (mid_slot, mid_id) = (mid.slot(), mid.id());
        let new = tree.replace_token(mid_id, "# d").unwrap();
        let new_slot = tree.get(new).unwrap().slot();
        assert_eq!(tree.trivia_run_start(last), mid_slot + 1);
        assert_eq!(tree.trivia_run_end(first), mid_slot - 1);
        assert_eq!(tree.trivia_run_start(new_slot), new_slot);
        assert_trivia_flags(&tree);
        // `b` is edited with the whole run between it and `a: 1` jumped.
        let b = tree
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::PlainScalar && x.text() == "b")
            .unwrap()
            .id();
        let _ = tree.replace_token(b, "bb").unwrap();
        assert_trivia_flags(&tree);
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
        // A document with a directive error, extracted and inserted.
        let donor = SyntaxTree::parse("%YAML 2.0\n--- x\n");
        let bad = donor.root().first_child().unwrap();
        assert_eq!(bad.kind(), SyntaxKind::Document);
        let f = donor.extract(bad.id(), bad.id()).unwrap();
        // (After a document ended by `...`: a directive after an open one
        // would be its content.)
        let mut tree = SyntaxTree::parse("--- a\n...\n");
        let root = tree.root().id();
        let ins = tree.append_child(root, f).unwrap();
        let slot = tree.get(ins.first).unwrap().slot();
        let (facts, local, _) = tree.facts_at(slot).unwrap();
        assert!(facts.directive_error(local));
        assert!(tree.has_errors_within(slot));
        assert!(tree.has_errors());
        let first_doc = tree.root().first_child().unwrap().slot();
        assert!(!tree.has_errors_within(first_doc));
        assert_trivia_flags(&tree);
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
    }

    /// The runs' trivia flags stay exact
    /// through insertions, cuts, removals, merges, and refusals of
    /// stream-level trivia and documents; and the walks over them find
    /// what a step-by-step walk finds.
    #[test]
    fn trivia_flags_follow_every_edit() {
        let mut tree = SyntaxTree::parse(format!("a: 1\n{}--- b\n# t\n", "# c\n".repeat(70)));
        let mut rng = 0x5eed_u64;
        let mut next = |n: usize| {
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (rng >> 33) as usize % n.max(1)
        };
        let pieces = ["# d\n", "\n", "--- z\n", "# e\n# f\n"];
        let mut accepted = 0usize;
        for round in 0..300 {
            let kids: Vec<NodeId> = tree.root().children().map(|x| x.id()).collect();
            let at = kids[next(kids.len())];
            match next(3) {
                0 => {
                    let f = Fragment::parse(pieces[next(pieces.len())]).unwrap();
                    accepted += usize::from(tree.insert_before(at, f).is_ok());
                }
                1 => accepted += usize::from(tree.remove(at).is_ok()),
                _ => {
                    let n = tree.get(at).unwrap();
                    if n.kind() == SyntaxKind::Comment {
                        let r = tree.replace_token(at, &format!("# r{round}"));
                        accepted += usize::from(r.is_ok());
                    }
                }
            }
            assert_trivia_flags(&tree);
            // Every stream child's content before and after, both ways.
            let kids: Vec<u32> = tree.root().children().map(|x| x.slot()).collect();
            for (i, &k) in kids.iter().enumerate() {
                let is_trivia = |s: &&u32| !tree.kind_of(**s).unwrap().is_trivia();
                let before = kids[..i].iter().rev().find(is_trivia).copied();
                let after = kids[i..].iter().find(is_trivia).copied();
                assert_eq!(tree.content_before(k).unwrap(), before, "round {round}");
                assert_eq!(tree.content_after(k).unwrap(), after, "round {round}");
            }
            assert_eq!(tree, SyntaxTree::parse(tree.emit()), "round {round}");
        }
        assert!(accepted > 150, "{accepted}");
        let runs = tree.edits.as_deref().unwrap().runs.len();
        assert!(runs > 20, "{runs} runs");
    }

    /// An insertion and its removal
    /// leave no run behind (the base runs they cut are merged again), so R
    /// grows only with the edits whose text is still in the tree.
    #[test]
    fn insert_remove_pairs_leave_no_runs_behind() {
        let src: String = (0..200).map(|i| format!("- e{i}\n")).collect();
        let mut tree = SyntaxTree::parse(src.as_str());
        let t = SyntaxTree::parse("- x\n- z");
        let seq = t.root().first_child().unwrap().first_child().unwrap();
        let kids: Vec<_> = seq.children().collect();
        let entry = t.extract(kids[3].id(), kids[6].id()).unwrap();
        let values: Vec<NodeId> = tree
            .root()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::Scalar)
            .map(|n| n.id())
            .collect();
        let mut stale = Vec::new();
        for (k, id) in values.iter().enumerate().step_by(7) {
            let ins = tree.insert_after(*id, entry.clone()).unwrap();
            let _ = tree.remove_range(ins.first, ins.last).unwrap();
            stale.push(ins.first);
            let e = tree.edits.as_deref().unwrap();
            assert_eq!(e.runs.len(), 1, "runs left behind after pair {k}");
            assert_eq!(e.seq.total().0 as usize, tree.len());
        }
        assert_eq!(tree.emit().as_bytes(), src.as_bytes());
        for id in &values {
            assert!(tree.get(*id).is_ok());
        }
        for id in stale {
            assert!(tree.get(id).is_err());
        }
        // A replacement's text stays: its runs stay too, bounded as before.
        let _ = tree
            .replace(values[3], Fragment::parse("zz").unwrap())
            .unwrap();
        assert!(tree.edits.as_deref().unwrap().runs.len() <= 3);
        assert_eq!(tree, SyntaxTree::parse(tree.emit()));
    }

    /// A copy made while a group is open
    /// (`Clone` or `fork`) merges and releases what the group held back,
    /// exactly as ending the group does; the original's group is intact.
    #[test]
    fn copies_made_inside_a_group_settle_its_edits() {
        let src: String = (0..20).map(|i| format!("k{i}: v\n")).collect();
        let mut tree = SyntaxTree::parse(src.as_str());
        let mut ended = tree.clone();
        let edit = |t: &mut SyntaxTree| {
            toggle(t, 40);
            let s = t
                .root()
                .descendants()
                .filter(|x| x.kind() == SyntaxKind::Scalar)
                .nth(5)
                .unwrap()
                .id();
            let ins = t.insert_after(s, Fragment::parse("# c").unwrap());
            if let Ok(ins) = ins {
                let _ = t.remove(ins.first).unwrap();
            }
        };
        let mark = tree.begin_group();
        edit(&mut tree);
        let m = ended.begin_group();
        edit(&mut ended);
        ended.end_group(m).unwrap();
        let shape = |t: &SyntaxTree| {
            let e = t.edits.as_deref().unwrap();
            (e.runs.len(), e.arenas.len(), e.links.len(), t.emit())
        };
        let held = shape(&tree);
        assert!(held.1 > shape(&ended).1, "{held:?}");
        for copy in [tree.clone(), tree.fork()] {
            assert!(!copy.in_group());
            assert_eq!(shape(&copy), shape(&ended));
            assert_eq!(copy, SyntaxTree::parse(copy.emit()));
        }
        assert_eq!(shape(&tree), held);
        tree.rollback_group(mark).unwrap();
        assert_eq!(tree.emit(), src);
    }

    /// The per-run diagnostic counts stay exact through cuts,
    /// insertions of fragments with diagnostics, removals, merges, and
    /// refused edits; the diagnostics read through them are the
    /// re-parse's.
    #[test]
    fn run_diagnostic_counts_follow_every_edit() {
        let settings = Settings::new().lenient_inline_colons(true);
        let src: String = (0..40)
            .map(|i| format!("k{i}: a: b\nv{i}: [x, y]\n"))
            .collect();
        let mut tree = SyntaxTree::parse_with(src.as_str(), &settings);
        assert!(tree.diagnostics().len() >= 40);
        // The warning of `w: e` is attached to the element after it, `r`:
        // inside the fragment.
        let donor = SyntaxTree::parse_with("a: 1\nq: w: e\nr: 1\n", &settings);
        let map = donor.root().first_child().unwrap().first_child().unwrap();
        let kids: Vec<_> = map.children().collect();
        let with_warning = donor
            .extract(kids[4].id(), kids.last().unwrap().id())
            .unwrap();
        assert_eq!(with_warning.text(), "\nq: w: e\nr: 1");
        let mut inserted = Vec::new();
        for round in 0..30usize {
            let seqs: Vec<NodeId> = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::FlowSequence)
                .map(|n| n.id())
                .collect();
            let s = seqs[round * 7 % seqs.len()];
            let x = tree.get(s).unwrap().children().nth(1).unwrap();
            let tok = x.first_child().unwrap().id();
            let _ = tree.replace_token(tok, &format!("x{round}")).unwrap();
            assert_diag_sums(&tree);
            // An entry with its own warning, from another arena.
            inserted.push(tree.insert_after(s, with_warning.clone()).unwrap());
            assert_diag_sums(&tree);
            if round % 3 == 2 {
                let ins = inserted.remove(round % inserted.len());
                let _ = tree.remove_range(ins.first, ins.last).unwrap();
                assert_diag_sums(&tree);
            }
            // A refused edit changes no count.
            let colon = tree
                .root()
                .descendants()
                .find(|n| n.kind() == SyntaxKind::MappingValue)
                .unwrap()
                .id();
            assert_eq!(tree.remove(colon), Err(TreeEditError::NotWellFormed));
            assert_diag_sums(&tree);
            assert_eq!(tree, SyntaxTree::parse_with(tree.emit(), &settings));
        }
        assert_eq!(tree.emit().matches("q: w: e").count(), 20);
    }
}
