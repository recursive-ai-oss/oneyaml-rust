//! Planning buffered mappings (merge keys, tolerated duplicates) and checking
//! skipped content, at the event level, in **one iterative pass** (no
//! recursion, linear time, and one interpretation shared with the `Value`
//! read).
//!
//! Both follow exactly the rules of the shared `Value` builder
//! ([`crate::construct::build`], [`crate::construct::keys`]), in the same
//! order, so a typed read accepts and rejects what `from_str::<Value>`
//! does:
//!
//! * every scalar and every collection tag is resolved (errors as the
//!   builder's);
//! * duplicate keys are detected when the entry's value completes, with the
//!   builder's policy, message, and "first defined here" label;
//! * a `<<` value must be a mapping or a sequence of mappings; a second
//!   `<<` is a duplicate key.
//!
//! [`plan`] rewrites a buffered mapping **and every mapping nested in it**
//! for the target: explicit entries in source order (a duplicate keeps the
//! first position and takes the last value), then merged entries whose keys
//! are not explicit, earlier merge sources first. Nodes are arena items;
//! merged entries reference the merge source's items instead of copying
//! them, so each event is read once and moved once: O(n) for any nesting
//! (linear). The target still sees every scalar's source text, style, and
//! span (string coercions and error locations keep working).
//!
//! Planning reads each event as it arrives, so its errors come in the
//! builder's order. A complex key's identity is built bottom-up **once**
//! as the walk completes its children (never rebuilt per enclosing key),
//! and merged entries are united with [`MergeAcc`] (small-to-large), so
//! nested keys and merge chains stay near-linear.
//!
//! [`check_node`] is the same walk without output, for content the target
//! ignores: memory is O(depth) frames plus the keys of each open mapping
//! only. [`check_unexpanded`] is the same walk over *unexpanded* events,
//! for `Document`'s strict parse (no alias expansion).

use std::borrow::Cow;
use std::collections::HashMap;

use oneyaml_core::{Diagnostic, DuplicateKeys, Settings, Span};
use oneyaml_syntax::tree::SyntaxTree;

use super::Src;
use crate::construct::build::{Cx, build_from};
use crate::construct::expand::{recursive_alias, undefined_alias};
use crate::construct::keys::{KeyMap, KeyRepr, Merge, duplicate_at, invalid_merge, key_text};
use crate::construct::merge::MergeAcc;
use crate::construct::scalar::{is_merge_key, resolve, resolve_collection, scalar_value};
use crate::construct::tree::{Replay, TreeSource};
use crate::construct::{Ev, Node, Pull, Source, Tag as EvTag};
use crate::error::Error;
use crate::resolve::{CollectionKind, Resolved, ResolvedCollection, Resolver};
use crate::value::walk::{OPAQUE_ALIAS, OPAQUE_MERGED};
use crate::value::{Mapping, Sequence, Tag, TaggedValue, Value};

#[cfg(test)]
thread_local! {
    /// Events read by [`plan`] on this thread (the linear-scaling test).
    pub(crate) static PLANNED_EVENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// `<<` values planned by a walk on this thread (the others were
    /// replayed source mappings, only buffered).
    pub(crate) static PLANNED_MERGE_VALUES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Where expansion counts saturate on this thread (a test
    /// sets `u32::MAX` to check a 64-bit build as a 32-bit target counts).
    pub(crate) static COUNT_CAP: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// `a + b` as expansion counts add: saturating at `usize::MAX` (tests may
/// lower the cap, [`COUNT_CAP`]).
fn count_add(a: usize, b: usize) -> usize {
    let sum = a.saturating_add(b);
    #[cfg(test)]
    let sum = sum.min(COUNT_CAP.with(std::cell::Cell::get));
    sum
}

/// One entry of a planned mapping: key item, value item, and (inside a
/// complex key) the value's `Value`.
struct Ent {
    k: usize,
    v: usize,
    val: Option<Value>,
}

/// A planned mapping's entries as a merge source (see
/// [`MergeAcc`], shared with the `Value` builder).
type Acc<'de> = MergeAcc<KeyRepr<'de>, Ent>;

/// What a completed node is, to a `<<` that takes it as its value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Scalar,
    /// An untagged (or `!!map`) mapping.
    Map,
    /// An untagged sequence whose items are all [`Shape::Map`].
    MapSeq,
    Other,
}

/// A node of the planned output (plan mode only).
enum Item {
    Leaf(usize),
    /// Start and end event indices (`None` for the buffered root, whose
    /// start and end are not buffered) and the children in output order.
    Coll {
        start: Option<usize>,
        end: Option<usize>,
        kids: Vec<usize>,
    },
}

/// A completed node, delivered to its parent.
struct Done<'de> {
    item: usize,
    /// A scalar's span, or a collection's start (for "first defined here").
    span: Span,
    /// `span` is a replayed position ([`Ev::replayed`]).
    replayed: bool,
    shape: Shape,
    /// The node's identity as a key (the `Value` builder's key equality,
    /// strings borrowed), when it completed in key position.
    key: Option<KeyRepr<'de>>,
    /// The node's `Value`, when it is part of a complex key (not the key
    /// itself: that is `key`). Built bottom-up, once.
    value: Option<Value>,
    /// Merge entries it provides (plan mode, merge values only).
    source: Option<Box<Acc<'de>>>,
    /// As a merge value, it provides entries that only alias expansion
    /// reveals (unexpanded checking): an alias of a collection, or a
    /// merge source that merges one.
    unknown: bool,
}

enum Pending<'de> {
    Key {
        key: KeyRepr<'de>,
        span: Span,
        /// `span` is a replayed position ([`Ev::replayed`]).
        replayed: bool,
        item: usize,
    },
    Merge(Span, bool),
}

#[derive(Default)]
struct MapState<'de> {
    /// Explicit keys and their pick index. Keys only, never values.
    seen: KeyMap<KeyRepr<'de>, usize>,
    /// Span of each explicit key's first definition, by pick index.
    first: Vec<Span>,
    /// (key item, value item, value inside a complex key) by pick index
    /// (plan mode).
    picks: Vec<(usize, usize, Option<Value>)>,
    /// Plan mode, and this mapping is a merge source.
    keep: bool,
    pending: Option<Pending<'de>>,
    /// Merged entries (earlier sources win).
    merge: Merge<KeyRepr<'de>, Ent>,
    /// The current `<<` merges entries only expansion reveals (see
    /// [`Done::unknown`]).
    unknown: bool,
}

enum Body<'de> {
    Seq {
        kids: Vec<usize>,
        all_maps: bool,
        source: Option<Box<Acc<'de>>>,
        /// Item values inside a complex key.
        vals: Vec<Value>,
        /// A merge value with an item only expansion reveals.
        unknown: bool,
        /// Items completed so far (the error path).
        len: usize,
    },
    Map(Box<MapState<'de>>),
}

struct Frame<'de> {
    start: Option<usize>,
    span: Span,
    /// The start event's [`Ev::replayed`].
    replayed: bool,
    custom: bool,
    /// This node is the value of a `<<`, or an item of one: keep what the
    /// merge needs.
    merge_part: bool,
    /// This node is a complex key or inside one (plan mode): its `Value`
    /// is built as it completes.
    in_key: bool,
    /// The nodes and scalar bytes of this node so far with aliases
    /// expanded (saturating), as its replay meters them: what an anchor on
    /// it records ([`Def::Node`]). Kept per node (never a panic,
    /// even on 32-bit targets) and added to the parent's when it closes, so a
    /// count saturates only when the node itself is that large (a true "at
    /// least"), never because of what came before it in the document (on
    /// 32-bit targets a small chain of aliases saturates a running total).
    count: (usize, usize),
    /// Rarely needed extras, boxed so frames stay small on the hot
    /// checking path.
    extra: Option<Box<Extra<'de>>>,
    body: Body<'de>,
}

/// A frame's rarely needed properties.
struct Extra<'de> {
    /// The custom tag, kept inside a complex key (its `Value` is built).
    tag: Option<Tag>,
    /// Its anchor (unexpanded checking).
    anchor: Option<AnchorAt<'de>>,
}

/// An open anchored collection (unexpanded checking).
struct AnchorAt<'de> {
    name: &'de str,
    /// The definition's serial.
    serial: usize,
    /// The node's slot, to read it again.
    slot: Option<u32>,
}

/// No slot: [`Anchored::prev`] when there is no earlier definition (the
/// tree never issues it).
const NO_SLOT: u32 = u32::MAX;

/// An anchored node, for the aliases to it (unexpanded checking).
struct Anchored<'de> {
    shape: Shape,
    /// Its identity as a key: a scalar's own; for a collection, an opaque
    /// identity of this anchor (equal only to other aliases of it). `None`
    /// for a `<<` that was a merge key where defined, so never resolved
    /// there (resolved when an alias needs its value, as the builder does).
    key: Option<KeyRepr<'de>>,
    /// A scalar that is a merge key wherever it stands as a key (`<<`
    /// under `merge_keys`): an alias of it in key position is a merge key
    /// too, as its expansion is.
    merge: Option<Box<RawScalar<'de>>>,
    /// Where the definition stands: open, or closed with what an alias key
    /// needs of it.
    def: Def,
    /// The slot of the name's previous closed definition, or [`NO_SLOT`].
    /// A name defined twice keeps it here, in what was
    /// padding, so it costs nothing; only older ones go to
    /// [`Walker::history`].
    prev: u32,
}

/// [`Anchored::def`]. No larger than the `Option<usize>`
/// it replaced, so an anchor no alias key uses costs what it cost before
/// the alias-key replay existed: the node to read again
/// is a 4-byte slot of the tree, and nothing else is kept per anchor.
enum Def {
    /// The anchored collection is open: an alias to it is recursive. The
    /// serial names this definition, so closing an outer node never
    /// finishes a redefinition made inside it.
    Open(usize),
    /// Closed; the slot of its node, to read it again ([`NO_SLOT`]: a
    /// source without replays), and the nodes and scalar bytes its replay
    /// produces with aliases expanded (each saturated at `u32::MAX`): an
    /// alias key whose value cannot fit the copy budget is refused without
    /// replaying it (nodes, and bytes). The slot is a bare
    /// `u32` so the byte count fits where `Option`'s tag was.
    Node { slot: u32, size: u32, bytes: u32 },
    /// An anchored collection's `Value` for its aliases as keys
    /// (built from a replay of the node when an alias of it
    /// first stands as a key or inside one, so an alias key equals an
    /// equal key written out (`{? &k [1] : 1, [1]: 2}`) as the `Value` read
    /// finds. Never built for an anchor no alias key uses.
    Built(Box<Built>),
    /// Not buildable (no replay, or past the copy budget): the identity is
    /// the anchor's opaque one, equal only to other aliases of it.
    Opaque { slot: u32, size: u32, bytes: u32 },
}

impl Def {
    /// The slot of a closed definition's node.
    fn slot(&self) -> Option<u32> {
        match self {
            Self::Open(_) => None,
            Self::Node { slot, .. } | Self::Opaque { slot, .. } => {
                (*slot != NO_SLOT).then_some(*slot)
            }
            Self::Built(b) => b.slot,
        }
    }

    /// The nodes an alias of it expands to (saturated; 0 while open: such
    /// an alias is recursive).
    fn size(&self) -> usize {
        match self {
            Self::Open(_) => 0,
            Self::Node { size, .. } | Self::Opaque { size, .. } => *size as usize,
            Self::Built(b) => b.nodes,
        }
    }

    /// The scalar bytes an alias of it expands to (saturated; 0 while
    /// open).
    fn bytes(&self) -> usize {
        match self {
            Self::Open(_) => 0,
            Self::Node { bytes, .. } | Self::Opaque { bytes, .. } => *bytes as usize,
            Self::Built(b) => b.bytes,
        }
    }
}

/// A node or byte count as kept in [`Def`]: saturated at `u32::MAX`,
/// which then compares as "at least that many".
fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// An invariant of the alias-key replay is broken (a bug; never a
/// silent wrong result).
fn broken(what: &'static str) -> Error {
    debug_assert!(false, "{what}");
    Error::internal(what, None)
}

/// Whether a replay produced the nodes and scalar bytes the walk counted
/// for it (`counted`, each saturated: `u32::MAX` matches any larger
/// count). Checked in release builds too; a drift is a
/// broken invariant (`Y9000`), never a value built from a different
/// expansion than the pre-check judged.
fn replay_counted(counted: (u32, u32), produced: (usize, usize)) -> Result<(), Error> {
    replay_counts(counted, produced)
        .inspect_err(|_| debug_assert!(false, "replay count drifted from the walk's"))
}

/// [`replay_counted`] without its `debug_assert!`: the release outcome,
/// which tests check in every build.
fn replay_counts(counted: (u32, u32), produced: (usize, usize)) -> Result<(), Error> {
    let agrees = |c: u32, n: usize| c == u32::MAX || n == c as usize;
    if agrees(counted.0, produced.0) && agrees(counted.1, produced.1) {
        Ok(())
    } else {
        Err(Error::internal(
            "replay count drifted from the walk's",
            None,
        ))
    }
}

/// [`Def::Built`]: the value, its node count and scalar bytes, and the
/// node's slot.
struct Built {
    value: Value,
    nodes: usize,
    bytes: usize,
    slot: Option<u32>,
}

/// What an alias inside a replayed node means (§3.2.2.2: the last
/// definition of its name before it): the current
/// definitions, their previous ones, and the older ones of names defined
/// three times or more.
struct Defs<'h, 'de> {
    tree: &'de SyntaxTree,
    anchors: Option<&'h HashMap<&'de str, Anchored<'de>>>,
    history: &'h HashMap<&'de str, Vec<u32>>,
}

impl<'de> Defs<'_, 'de> {
    /// The node in `slot`. The checked tree is never edited while it is
    /// checked, so every slot an anchor kept lives.
    fn replay(&self, slot: u32) -> Result<Replay<'de>, Error> {
        Replay::at(self.tree, slot).ok_or_else(|| broken("anchored node gone"))
    }

    /// The definition of `name` an alias at `at` means (`None`: none, a
    /// broken invariant for an alias the walk accepted).
    fn before(&self, name: &str, at: usize) -> Result<Option<Replay<'de>>, Error> {
        let Some(a) = self.anchors.and_then(|a| a.get(name)) else {
            return Ok(None);
        };
        // Newest first: the current definition (an open one never precedes
        // a valid alias), its previous one, then the older ones.
        if let Some(slot) = a.def.slot() {
            let r = self.replay(slot)?;
            if r.start() < at {
                return Ok(Some(r));
            }
        }
        if a.prev != NO_SLOT {
            let r = self.replay(a.prev)?;
            if r.start() < at {
                return Ok(Some(r));
            }
        }
        let Some(defs) = self.history.get(name) else {
            return Ok(None);
        };
        // Slots in document order: their starts ascend.
        let mut dead = false;
        let i = defs.partition_point(|s| match Replay::at(self.tree, *s) {
            Some(r) => r.start() < at,
            None => {
                dead = true;
                false
            }
        });
        if dead {
            return Err(broken("anchored node gone"));
        }
        match i.checked_sub(1).and_then(|i| defs.get(i)) {
            Some(s) => self.replay(*s).map(Some),
            None => Ok(None),
        }
    }
}

/// The copy budget of unexpanded checking (bounded cost and
/// memory): an alias as a key, or inside one, means its anchor's value,
/// built and copied per use within the alias node budget the `Value` read
/// has for the same settings (`Limits::get_alias_budget` of the
/// document's own nodes so far). The `Value` read charges every alias
/// use; this charges only uses as keys, so it is never refused here while
/// that read is within its budget, and past it that read has refused
/// (`Y2004`). Scalar bytes are metered the same way against the alias
/// byte budget (`Y2009`). An alias over a budget keeps its anchor's opaque
/// identity: never a false duplicate.
struct AliasMeter {
    limits: oneyaml_core::Limits,
    /// Own nodes of the document so far.
    own: usize,
    /// Bytes of the document read so far, and where it starts.
    input: usize,
    start: usize,
    /// Nodes and scalar bytes built or copied for alias keys.
    spent: usize,
    bytes: usize,
}

impl AliasMeter {
    fn new(limits: &oneyaml_core::Limits) -> Self {
        Self {
            limits: limits.clone(),
            own: 0,
            input: 0,
            start: 0,
            spent: 0,
            bytes: 0,
        }
    }

    /// A new document, starting at `start`.
    fn reset(&mut self, start: usize) {
        self.own = 0;
        self.input = 0;
        self.start = start;
        self.spent = 0;
        self.bytes = 0;
    }

    /// The document has been read up to `end`.
    fn read_to(&mut self, end: usize) {
        self.input = self.input.max(end.saturating_sub(self.start));
    }

    /// Nodes and scalar bytes that may still be built or copied.
    fn left(&self) -> (usize, usize) {
        let nodes = self
            .limits
            .get_alias_budget(self.own)
            .map_or(usize::MAX, |b| b.saturating_sub(self.spent));
        let bytes = self
            .limits
            .get_alias_byte_budget(self.input)
            .map_or(usize::MAX, |b| b.saturating_sub(self.bytes));
        (nodes, bytes)
    }

    /// Whether `nodes` and `bytes` more fit; charges them if so.
    fn take(&mut self, nodes: usize, bytes: usize) -> bool {
        let (n, b) = self.left();
        if nodes > n || bytes > b {
            return false;
        }
        self.charge(nodes, bytes);
        true
    }

    /// Charges work done (a replay refused at the budget still
    /// cost what it read, so all replays of a check together read at most
    /// the budget).
    fn charge(&mut self, nodes: usize, bytes: usize) {
        self.spent = self.spent.saturating_add(nodes);
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

/// An anchored `<<` scalar as written (unexpanded checking).
struct RawScalar<'de> {
    tag: EvTag<'de>,
    plain: bool,
    value: Cow<'de, str>,
}

/// A resolved scalar's identity as a key.
fn scalar_key<'de>(r: Resolved<'_>, value: &Cow<'de, str>, tag: &EvTag<'_>) -> KeyRepr<'de> {
    match r {
        Resolved::Str => KeyRepr::Str(value.clone()),
        other => KeyRepr::of_value(scalar_value(other, value.clone(), tag)),
    }
}

struct Walker<'de, 'w> {
    record: bool,
    /// Build complex keys inline, and read aliases without expanding them
    /// (`Document`'s strict check); otherwise keys are built by the
    /// shared builder from expanded events.
    inline: bool,
    /// Anchored nodes by name (unexpanded checking only).
    anchors: Option<HashMap<&'de str, Anchored<'de>>>,
    /// Anchors defined so far (opaque collection identities).
    serial: usize,
    /// The copy budget of alias keys (unexpanded checking).
    copies: AliasMeter,
    /// The replay of the node whose event is being stepped, when it is
    /// anchored (unexpanded checking of a tree).
    replay: Option<Replay<'de>>,
    /// The tree the replays read (anchors keep only slots).
    tree: Option<&'de SyntaxTree>,
    /// Closed definitions older than [`Anchored::prev`], slots in document
    /// order: with the current and previous ones, what an alias inside a
    /// replayed node means (§3.2.2.2). Only names
    /// defined three times or more have an entry.
    history: HashMap<&'de str, Vec<u32>>,
    resolver: Resolver,
    policy: DuplicateKeys,
    warnings: &'w mut crate::diag_cap::Capped,
    stack: Vec<Frame<'de>>,
    items: Vec<Item>,
    /// Cleared mapping states, reused so that checking allocates per
    /// nesting level, not per mapping. Boxed as the frames hold
    /// them, so reuse moves a pointer.
    #[allow(clippy::vec_box)]
    pool: Vec<Box<MapState<'de>>>,
}

/// Allocations [`check_node`] keeps between calls (one per deserializer).
#[derive(Default)]
pub(crate) struct Scratch<'de> {
    stack: Vec<Frame<'de>>,
    #[allow(clippy::vec_box)]
    pool: Vec<Box<MapState<'de>>>,
}

fn wrap(tag: Option<Tag>, value: Value) -> Value {
    match tag {
        None => value,
        Some(tag) => Value::Tagged(Box::new(TaggedValue { tag, value })),
    }
}

impl<'de> Walker<'de, '_> {
    /// The next completed node is a mapping key.
    fn key_position(&self) -> bool {
        matches!(self.stack.last(), Some(Frame { body: Body::Map(m), .. }) if m.pending.is_none())
    }

    /// The next completed node is inside a complex key (plan mode).
    fn in_key(&self) -> bool {
        self.stack.last().is_some_and(|f| f.in_key)
    }

    /// Adds expanded nodes and scalar bytes to the open node's count
    /// (none at the root, where no anchor encloses them.
    fn count(&mut self, nodes: usize, bytes: usize) {
        if let Some(f) = self.stack.last_mut() {
            f.count = (count_add(f.count.0, nodes), count_add(f.count.1, bytes));
        }
    }

    fn push_item(&mut self, item: Item) -> usize {
        if !self.record {
            return 0;
        }
        self.items.push(item);
        self.items.len().saturating_sub(1)
    }

    fn scalar(&mut self, idx: usize, ev: &Ev<'de>) -> Result<Option<Done<'de>>, Error> {
        let Node::Scalar { value, plain, tag } = &ev.node else {
            return Err(Error::internal("scalar expected", Some(ev.span)));
        };
        // As the replay meters them (`Replayer::pull`).
        self.count(1, value.len());
        let key_pos = self.key_position();
        let merge = is_merge_key(&self.resolver, tag, *plain, value);
        // An anchored `<<` is recorded before anything else, merge
        // key or not, so aliases of it mean what their expansion means.
        let anchor = ev.anchor.filter(|_| self.inline);
        let raw = || {
            Box::new(RawScalar {
                tag: tag.clone(),
                plain: *plain,
                value: value.clone(),
            })
        };
        if key_pos && merge {
            if let Some(name) = anchor {
                self.anchor(name, Shape::Scalar, None, Some(raw()), None, value.len());
            }
            self.pending_merge(ev.span, ev.replayed);
            return Ok(None);
        }
        let r = resolve(&self.resolver, tag, *plain, value, ev.span)?;
        if let Some(name) = anchor {
            let key = scalar_key(r.clone(), value, tag);
            self.anchor(
                name,
                Shape::Scalar,
                Some(key),
                merge.then(raw),
                None,
                value.len(),
            );
        }
        let (key, val) = match (key_pos, r) {
            (true, Resolved::Str) => (Some(KeyRepr::Str(value.clone())), None),
            (true, other) => (
                Some(KeyRepr::of_value(scalar_value(other, value.clone(), tag))),
                None,
            ),
            (false, other) if self.in_key() => {
                (None, Some(scalar_value(other, value.clone(), tag)))
            }
            (false, _) => (None, None),
        };
        Ok(Some(Done {
            item: self.push_item(Item::Leaf(idx)),
            span: ev.span,
            replayed: ev.replayed,
            shape: Shape::Scalar,
            key,
            value: val,
            source: None,
            unknown: false,
        }))
    }

    /// The value of the collection anchored as `name`, for an alias of it
    /// as a key or inside one: built from its replay on first
    /// use, then copied; each use charged to the copy budget. `None` (the
    /// opaque identity) when it cannot be built or past the budget.
    ///
    /// A value whose expanded size exceeds what is
    /// left of the budget is refused without replaying it, and a replay
    /// refused part-way is charged what it read, so the replays of one
    /// check read at most the budget in all, as the `Value` read does.
    ///
    /// The same holds for scalar bytes, so a value
    /// small in nodes but large in bytes is refused without copying any.
    fn alias_value(&mut self, name: &str) -> Result<Option<Value>, Error> {
        let left = self.copies.left();
        let Some(a) = self.anchors.as_ref().and_then(|a| a.get(name)) else {
            return Ok(None);
        };
        if let Def::Node { slot, size, bytes } = a.def {
            let opaque = Def::Opaque { slot, size, bytes };
            let new = match ((slot != NO_SLOT).then_some(slot), self.tree) {
                // More nodes than are left: the `Value` read has refused
                // (`Y2004`) by this alias.
                _ if size as usize > left.0 => opaque,
                // More scalar bytes than are left: the `Value` read, which
                // charges at least these bytes for this alias, has refused
                // (`Y2009`) by it. Charged as the replay refused at the
                // byte budget was (all bytes left are spent), so later
                // alias keys see the budget they saw before.
                _ if bytes as usize > left.1 => {
                    self.copies.charge(0, bytes as usize);
                    opaque
                }
                (Some(s), Some(tree)) => {
                    let defs = Defs {
                        tree,
                        anchors: self.anchors.as_ref(),
                        history: &self.history,
                    };
                    let replay = defs.replay(s)?;
                    match build_replayed(replay, &defs, self.resolver, self.policy, left)? {
                        Replayed::Built {
                            value,
                            nodes,
                            bytes: built_bytes,
                        } => {
                            // The walk counted what the replay produces
                            // (in release too, `Y9000`).
                            replay_counted((size, bytes), (nodes, built_bytes))?;
                            Def::Built(Box::new(Built {
                                value,
                                nodes,
                                bytes: built_bytes,
                                slot: Some(s),
                            }))
                        }
                        Replayed::Refused { nodes, bytes } => {
                            self.copies.charge(nodes, bytes);
                            opaque
                        }
                    }
                }
                // A slot names a node of the tree it came from.
                (Some(_), None) => return Err(broken("anchor slot without a tree")),
                // A source without replays.
                (None, _) => opaque,
            };
            if let Some(a) = self.anchors.as_mut().and_then(|a| a.get_mut(name)) {
                a.def = new;
            }
        }
        let Some(Def::Built(built)) = self
            .anchors
            .as_ref()
            .and_then(|a| a.get(name))
            .map(|a| &a.def)
        else {
            return Ok(None);
        };
        let (value, nodes, bytes) = (&built.value, built.nodes, built.bytes);
        if !self.copies.take(nodes, bytes) {
            return Ok(None);
        }
        Ok(Some(value.clone()))
    }

    /// The top mapping's next value is a `<<`'s.
    fn pending_merge(&mut self, span: Span, replayed: bool) {
        if let Some(Frame {
            body: Body::Map(m), ..
        }) = self.stack.last_mut()
        {
            m.pending = Some(Pending::Merge(span, replayed));
        }
    }

    /// Records an anchor (unexpanded checking); later definitions of a
    /// name replace earlier ones (§3.2.2.2).
    /// Returns the slot of the node, to read it again (a collection keeps
    /// it in its frame until it closes). `open`: a collection's serial;
    /// otherwise a scalar, one node of `bytes` scalar bytes.
    fn anchor(
        &mut self,
        name: &'de str,
        shape: Shape,
        key: Option<KeyRepr<'de>>,
        merge: Option<Box<RawScalar<'de>>>,
        open: Option<usize>,
        bytes: usize,
    ) -> Option<u32> {
        use std::collections::hash_map::Entry;
        let slot = self.replay.take().map(|r| {
            self.tree.get_or_insert(r.tree());
            r.slot()
        });
        let def = match open {
            Some(serial) => Def::Open(serial),
            None => Def::Node {
                slot: slot.unwrap_or(NO_SLOT),
                size: 1,
                bytes: saturate(bytes),
            },
        };
        let new = Anchored {
            shape,
            key,
            merge,
            def,
            prev: NO_SLOT,
        };
        match self.anchors.get_or_insert_with(HashMap::new).entry(name) {
            Entry::Vacant(v) => {
                v.insert(new);
            }
            Entry::Occupied(mut o) => {
                let old = o.insert(new);
                // A redefinition keeps the earlier
                // one only if it was closed (an open one is never what an
                // alias means: an alias inside it before the redefinition is
                // recursive), in `prev`; what `prev` held moves to the
                // history.
                let prev = match old.def.slot() {
                    Some(s) => {
                        if old.prev != NO_SLOT {
                            self.history.entry(name).or_default().push(old.prev);
                        }
                        s
                    }
                    None => old.prev,
                };
                o.get_mut().prev = prev;
            }
        }
        slot
    }

    /// An opaque key identity for anchor number `serial`: its value is
    /// not built (`Document` never expands aliases).
    /// The name follows the serial only so messages can show the alias
    /// (`*n`); the serial alone makes the identity unique.
    fn opaque(serial: usize, name: &str) -> KeyRepr<'de> {
        KeyRepr::Other(Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(format!("{OPAQUE_ALIAS}{serial}/{name}")),
            value: Value::Null,
        })))
    }

    /// An alias, read without expansion: the shape and key identity of
    /// its anchor. An undefined or recursive alias is Y1015, with the
    /// expander's message, at the point the expander meets it:
    /// neither needs expansion to detect.
    ///
    /// An alias of a `<<` scalar in key position is a merge key, at the
    /// alias, where the expander replays it; `None` then, as for a `<<`
    /// scalar.
    fn alias(&mut self, name: &str, span: Span) -> Result<Option<Done<'de>>, Error> {
        let a = match self.anchors.as_ref().and_then(|a| a.get(name)) {
            Some(a) if !matches!(a.def, Def::Open(_)) => a,
            Some(_) => return Err(recursive_alias(name, span)),
            None => return Err(undefined_alias(name, span)),
        };
        let shape = a.shape;
        // `Walker::count`, with `a` still borrowed.
        if let Some(f) = self.stack.last_mut() {
            f.count = (
                count_add(f.count.0, a.def.size()),
                count_add(f.count.1, a.def.bytes()),
            );
        }
        if let (true, Some(_)) = (self.key_position(), a.merge.as_ref()) {
            // Unexpanded checking reads no replays.
            self.pending_merge(span, false);
            return Ok(None);
        }
        let key = match (&a.key, &a.merge) {
            (Some(k), _) => k.clone(),
            // A `<<` defined as a merge key, now a value: resolved here,
            // where its expansion is (errors at the alias, where the
            // expander replays it).
            (None, Some(raw)) => {
                let r = resolve(&self.resolver, &raw.tag, raw.plain, &raw.value, span)?;
                scalar_key(r, &raw.value, &raw.tag)
            }
            (None, None) => return Err(Error::internal("anchored scalar", Some(span))),
        };
        // A collection alias as a key, or inside one, means
        // its anchor's value, built on first use and copied per use within
        // the copy budget (see `AliasMeter`).
        let wanted = self.key_position() || self.in_key();
        let full = if wanted && shape != Shape::Scalar {
            self.alias_value(name)?
        } else {
            None
        };
        // As a `<<` value (or an item of one) inside a
        // complex key, the anchor's entries are merged into the key's
        // identity, as the `Value` read merges them.
        let mergeable = match self.merging_in_key() {
            // A `<<` value: a mapping or a sequence of mappings.
            Some(true) => matches!(shape, Shape::Map | Shape::MapSeq),
            // An item of a `<<` sequence: a mapping.
            Some(false) => shape == Shape::Map,
            None => false,
        };
        let source = match full {
            Some(v) if mergeable => Some(merge_source(v)?),
            Some(v) => {
                return Ok(Some(self.alias_done(span, shape, KeyRepr::of_value(v))));
            }
            None => None,
        };
        if source.is_some() {
            return Ok(Some(Done {
                item: 0,
                span,
                replayed: false,
                shape,
                key: None,
                value: None,
                source,
                unknown: false,
            }));
        }
        Ok(Some(self.alias_done(span, shape, key)))
    }

    /// The next completed node is a `<<` value (`Some(true)`), or an item
    /// of one (`Some(false)`), inside a complex key (unexpanded checking).
    fn merging_in_key(&self) -> Option<bool> {
        let f = self.stack.last().filter(|f| self.inline && f.in_key)?;
        match &f.body {
            Body::Map(m) => matches!(m.pending, Some(Pending::Merge(..))).then_some(true),
            Body::Seq { .. } => f.merge_part.then_some(false),
        }
    }

    /// An alias delivered with `key` as its identity.
    fn alias_done(&self, span: Span, shape: Shape, key: KeyRepr<'de>) -> Done<'de> {
        let (key, value) = if self.key_position() {
            (Some(key), None)
        } else if self.in_key() {
            (None, Some(key.into_value()))
        } else {
            (None, None)
        };
        Done {
            item: 0,
            span,
            replayed: false,
            shape,
            key,
            value,
            source: None,
            // A collection's entries, as a merge source, need expansion.
            unknown: shape != Shape::Scalar,
        }
    }

    /// One node event of unexpanded checking.
    fn step(&mut self, ev: &Ev<'de>) -> Result<(), Error> {
        let done = match ev.node {
            Node::Scalar { .. } => self.scalar(0, ev)?,
            Node::SeqStart { .. } | Node::MapStart { .. } => {
                self.open(None, ev)?;
                None
            }
            Node::SeqEnd | Node::MapEnd => Some(self.close(None)?),
            Node::Alias { name } => self.alias(name, ev.span)?,
            _ => return Err(Error::internal("node expected", Some(ev.span))),
        };
        if let Some(d) = done {
            // The document's root completes with no parent: nothing to do.
            self.deliver(d)?;
        }
        Ok(())
    }

    fn open(&mut self, idx: Option<usize>, ev: &Ev<'de>) -> Result<(), Error> {
        let (kind, tag) = match &ev.node {
            Node::SeqStart { tag } => (CollectionKind::Sequence, tag),
            Node::MapStart { tag } => (CollectionKind::Mapping, tag),
            _ => return Err(Error::internal("collection expected", Some(ev.span))),
        };
        let in_key = (self.record || self.inline) && (self.key_position() || self.in_key());
        let (custom, tag) = match resolve_collection(&self.resolver, tag, kind, ev.span)? {
            ResolvedCollection::Custom(t) => (true, in_key.then(|| tag.value_tag(t))),
            ResolvedCollection::Plain => (false, None),
        };
        let merge_part = match self.stack.last() {
            Some(Frame {
                body: Body::Map(m), ..
            }) => matches!(m.pending, Some(Pending::Merge(..))),
            Some(f) => f.merge_part && kind == CollectionKind::Mapping,
            None => false,
        };
        let body = match kind {
            CollectionKind::Sequence => Body::Seq {
                kids: Vec::new(),
                all_maps: true,
                source: None,
                vals: Vec::new(),
                unknown: false,
                len: 0,
            },
            CollectionKind::Mapping => {
                let mut m = self.pool.pop().unwrap_or_default();
                // A merge source's entries are kept for the planned output,
                // and inside a complex key for its identity (the
                // key's `Value` has the merged entries).
                m.keep = merge_part && (self.record || in_key);
                Body::Map(m)
            }
        };
        let anchor = ev.anchor.filter(|_| self.inline).map(|name| {
            let serial = self.serial;
            self.serial = serial.saturating_add(1);
            let key = Some(Self::opaque(serial, name));
            let slot = self.anchor(name, Shape::Other, key, None, Some(serial), 0);
            AnchorAt { name, serial, slot }
        });
        self.stack.push(Frame {
            start: idx,
            span: ev.span,
            replayed: ev.replayed,
            custom,
            merge_part,
            in_key,
            // The collection node itself.
            count: (1, 0),
            extra: (tag.is_some() || anchor.is_some()).then(|| Box::new(Extra { tag, anchor })),
            body,
        });
        Ok(())
    }

    fn close(&mut self, end: Option<usize>) -> Result<Done<'de>, Error> {
        let frame = self
            .stack
            .pop()
            .ok_or_else(|| Error::internal("unbalanced collection end", None))?;
        let (tag, anchor) = match frame.extra {
            Some(x) => (x.tag, x.anchor),
            None => (None, None),
        };
        let (kids, shape, source, value, unknown) = match frame.body {
            Body::Seq {
                kids,
                all_maps,
                source,
                vals,
                unknown,
                ..
            } => {
                let shape = if frame.custom || !all_maps {
                    Shape::Other
                } else {
                    Shape::MapSeq
                };
                let value = frame
                    .in_key
                    .then(|| wrap(tag, Value::Sequence(Sequence::from(vals))));
                (kids, shape, source, value, unknown)
            }
            Body::Map(mut m) => {
                let shape = if frame.custom {
                    Shape::Other
                } else {
                    Shape::Map
                };
                let fin = finish_map(&mut m, frame.in_key).map_err(|e| e.at(frame.span))?;
                self.pool.push(m);
                let value = if fin.unknown && fin.value.is_some() {
                    // Inside a complex key, a mapping that merges an alias
                    // whose value is past the copy budget (where
                    // the `Value` read has refused, `Y2004`/`Y2009`): an
                    // identity equal to nothing else, so no false duplicate.
                    let serial = self.serial;
                    self.serial = serial.saturating_add(1);
                    Some(Self::opaque_merged(serial))
                } else {
                    fin.value.map(|v| wrap(tag, Value::Mapping(v)))
                };
                (fin.kids, shape, fin.source, value, fin.unknown)
            }
        };
        let (nodes, bytes) = frame.count;
        self.count(nodes, bytes);
        let (size, bytes) = (saturate(nodes), saturate(bytes));
        let anchors = self.anchors.as_mut();
        let found = anchor.and_then(|at| {
            anchors
                .and_then(|a| a.get_mut(at.name))
                .filter(|a| matches!(a.def, Def::Open(s) if s == at.serial))
                .map(|a| (a, at.slot))
        });
        // A redefinition inside this node replaced the name: that later
        // definition is what aliases after it mean (§3.2.2.2), untouched.
        if let Some((a, slot)) = found {
            a.shape = shape;
            a.def = Def::Node {
                slot: slot.unwrap_or(NO_SLOT),
                size,
                bytes,
            };
        }
        // A complex key (plan mode): its identity is the `Value` just
        // built from its children's, each built once.
        let (key, value) = if self.key_position() {
            (value.map(KeyRepr::of_value), None)
        } else {
            (None, value)
        };
        let item = self.push_item(Item::Coll {
            start: frame.start,
            end,
            kids,
        });
        Ok(Done {
            item,
            span: frame.span,
            replayed: frame.replayed,
            shape,
            key,
            value,
            source,
            unknown,
        })
    }

    /// Prepends the path of the open frames (innermost first) to an
    /// error, as the `Value` builder does (`build::with_path`): an
    /// item's index, a pending key's text, or `<<`.
    fn with_path(&self, mut e: Error) -> Error {
        for f in self.stack.iter().rev() {
            e = match &f.body {
                Body::Seq { len, .. } => e.in_index(*len),
                Body::Map(m) => match &m.pending {
                    Some(Pending::Key { key, .. }) => match key {
                        KeyRepr::Str(s) => e.in_key(s),
                        KeyRepr::Other(v) => e.in_key(&key_text(v)),
                    },
                    Some(Pending::Merge(..)) => e.in_key("<<"),
                    None => e,
                },
            };
        }
        e
    }

    /// A unique identity for a key mapping whose merged entries only
    /// expansion reveals (unexpanded checking).
    fn opaque_merged(serial: usize) -> Value {
        Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(format!("{OPAQUE_MERGED}{serial}")),
            value: Value::Null,
        }))
    }

    /// Hands a completed node to its parent; the node itself when it is
    /// the root.
    fn deliver(&mut self, done: Done<'de>) -> Result<Option<Done<'de>>, Error> {
        let record = self.record;
        let policy = self.policy;
        let Some(top) = self.stack.last_mut() else {
            return Ok(Some(done));
        };
        let merge_part = top.merge_part;
        let in_key = top.in_key;
        match &mut top.body {
            Body::Seq {
                kids,
                all_maps,
                source,
                vals,
                unknown,
                len,
            } => {
                *len = len.saturating_add(1);
                if record {
                    kids.push(done.item);
                }
                if done.shape != Shape::Map {
                    *all_maps = false;
                }
                if merge_part {
                    *unknown |= done.unknown;
                    if let Some(src) = done.source {
                        match source.as_mut() {
                            None => *source = Some(src),
                            Some(acc) => acc.absorb_later(*src),
                        }
                    }
                } else if in_key {
                    // A `<<` sequence has no value of its own (its items
                    // are merge sources, delivered as entries): only other
                    // sequences in a complex key collect item values.
                    let v = done
                        .value
                        .ok_or_else(|| Error::internal("key item value", Some(done.span)))?;
                    vals.push(v);
                }
            }
            Body::Map(m) => match m.pending.take() {
                None => {
                    let key = done
                        .key
                        .ok_or_else(|| Error::internal("key without value", Some(done.span)))?;
                    m.pending = Some(Pending::Key {
                        key,
                        span: done.span,
                        replayed: done.replayed,
                        item: done.item,
                    });
                }
                Some(Pending::Key {
                    key,
                    span,
                    replayed,
                    item,
                }) => {
                    if let Some(&i) = m.seen.get(&key) {
                        // First position, last value (keys.rs).
                        let first = m.first.get(i).copied().unwrap_or(span);
                        let text = || match &key {
                            KeyRepr::Str(s) => crate::error::segment(s),
                            KeyRepr::Other(v) => key_text(v),
                        };
                        duplicate_at(self.warnings, policy, replayed, text, first, span)?;
                        if let Some(p) = m.picks.get_mut(i) {
                            p.1 = done.item;
                            p.2 = done.value;
                        }
                    } else {
                        let i = m.first.len();
                        m.first.push(span);
                        if record || in_key {
                            m.picks.push((item, done.item, done.value));
                        }
                        m.seen.insert_new(key, i);
                    }
                }
                Some(Pending::Merge(span, replayed)) => {
                    // As the builder: a repeated `<<` first, then the shape.
                    m.merge.key(span, replayed, policy, self.warnings)?;
                    if !matches!(done.shape, Shape::Map | Shape::MapSeq) {
                        return Err(invalid_merge(span));
                    }
                    // A repeated `<<` (LastWins) replaced the earlier
                    // sources, and what they needed with them.
                    m.unknown = done.unknown;
                    if let Some(src) = done.source {
                        m.merge.absorb(src);
                    }
                }
            },
        }
        Ok(None)
    }
}

/// The entries an alias's value provides as a merge source inside a
/// complex key: a mapping's, or a sequence of mappings', earlier
/// ones first, with the rules the `Value` builder merges them by.
fn merge_source<'de>(value: Value) -> Result<Box<Acc<'de>>, Error> {
    let entries = |m: Mapping| {
        MergeAcc::from_entries(m.into_iter().map(|(k, v)| {
            let ent = Ent {
                k: 0,
                v: 0,
                val: Some(v),
            };
            (KeyRepr::of_value(k), ent)
        }))
    };
    match value {
        Value::Mapping(m) => Ok(Box::new(entries(m))),
        Value::Sequence(items) => {
            let mut acc: Acc<'de> = MergeAcc::default();
            for item in items {
                match item {
                    Value::Mapping(m) => acc.absorb_later(entries(m)),
                    _ => return Err(broken("merged alias item")),
                }
            }
            Ok(Box::new(acc))
        }
        // Its shape was checked: a mapping or a sequence of mappings.
        _ => Err(broken("merged alias value")),
    }
}

/// What a finished mapping gives its parent.
struct Finished<'de> {
    /// Output children (plan mode, not for a merge source).
    kids: Vec<usize>,
    /// Its entries, for a merge source.
    source: Option<Box<Acc<'de>>>,
    /// Its `Value`, inside a complex key.
    value: Option<Mapping>,
    /// It merges entries only expansion reveals.
    unknown: bool,
}

/// A mapping's output children (explicit entries, then merged entries
/// whose keys are not explicit) or, for a merge source, its entries; its
/// `Value` inside a complex key. Leaves `m` cleared for reuse.
fn finish_map<'de>(m: &mut MapState<'de>, in_key: bool) -> Result<Finished<'de>, Error> {
    if m.pending.is_some() {
        return Err(Error::internal("mapping ended after a key", None));
    }
    let merge = std::mem::take(&mut m.merge);
    let keep = std::mem::take(&mut m.keep);
    m.first.clear();
    let mut out = Finished {
        kids: Vec::new(),
        source: None,
        value: None,
        unknown: std::mem::take(&mut m.unknown),
    };
    if !keep && !in_key {
        // Explicit entries in order, then the merged ones whose keys are
        // not explicit (materialised once, keys never moved).
        for (k, v, _) in m.picks.drain(..) {
            out.kids.push(k);
            out.kids.push(v);
        }
        if let Some(merged) = merge.into_merged() {
            for (key, ent) in merged.into_entries() {
                if !m.seen.contains_key(&key) {
                    out.kids.push(ent.k);
                    out.kids.push(ent.v);
                }
            }
        }
        m.seen.clear();
        return Ok(out);
    }
    // The keys, by pick index, moved out of `seen` (never cloned: a
    // complex key can be large).
    let mut keys: Vec<Option<KeyRepr<'de>>> = Vec::new();
    keys.resize_with(m.picks.len(), || None);
    for (k, i) in m.seen.drain() {
        if let Some(slot) = keys.get_mut(i) {
            *slot = Some(k);
        }
    }
    let mut explicit = Vec::with_capacity(m.picks.len());
    for (key, (k, v, val)) in keys.into_iter().zip(m.picks.drain(..)) {
        let key = key.ok_or_else(|| Error::internal("planned key", None))?;
        explicit.push((key, Ent { k, v, val }));
    }
    let all: Box<dyn Iterator<Item = (KeyRepr<'de>, Ent)>> = if !keep && merge.is_empty() {
        // Inside a complex key, without merges: keys are unique already.
        Box::new(explicit.into_iter())
    } else {
        let all = merge.finish(MergeAcc::from_entries(explicit));
        if keep {
            out.source = Some(Box::new(all));
            return Ok(out);
        }
        Box::new(all.into_entries())
    };
    let mut value = in_key.then(Mapping::new);
    for (key, ent) in all {
        out.kids.push(ent.k);
        out.kids.push(ent.v);
        if let Some(map) = value.as_mut() {
            let v = ent
                .val
                .ok_or_else(|| Error::internal("key entry value", None))?;
            map.insert(key.into_value(), v);
        }
    }
    out.value = value;
    Ok(out)
}

/// Plans the entries of one mapping: reads its events from `p` up to its end
/// (left unread), including every mapping nested in them. Each event is checked
/// as it is read, so errors come in the `Value` builder's order (both readers
/// agree): a later syntax error never hides an earlier meaning error.
pub(crate) fn plan<'de>(
    p: &mut impl Pull<'de>,
    resolver: Resolver,
    policy: DuplicateKeys,
    warnings: &mut crate::diag_cap::Capped,
) -> Result<Vec<Ev<'de>>, Error> {
    let mut entries: Vec<Ev<'de>> = Vec::new();
    let root = {
        let mut w = Walker {
            record: true,
            inline: false,
            anchors: None,
            serial: 0,
            copies: AliasMeter::new(&oneyaml_core::Limits::default()),
            replay: None,
            tree: None,
            history: HashMap::new(),
            resolver,
            policy,
            warnings,
            stack: vec![Frame {
                start: None,
                span: p.peek()?.span,
                replayed: false,
                custom: false,
                merge_part: false,
                in_key: false,
                count: (0, 0),
                extra: None,
                body: Body::Map(Box::default()),
            }],
            items: Vec::new(),
            pool: Vec::new(),
        };
        loop {
            match plan_step(&mut w, p, &mut entries) {
                Ok(true) => break,
                Ok(false) => {}
                // The path the `Value` builder reports.
                Err(e) => return Err(w.with_path(e)),
            }
        }
        let done = w.close(None)?;
        if !w.stack.is_empty() {
            return Err(Error::internal("unbalanced buffered mapping", None));
        }
        (done.item, w.items)
    };
    #[cfg(test)]
    PLANNED_EVENTS.with(|c| c.set(c.get().saturating_add(entries.len())));
    flatten(root.1, root.0, entries)
}

/// Reads and plans one event of the buffered mapping; `true` at its end
/// (left unread).
fn plan_step<'de>(
    w: &mut Walker<'de, '_>,
    p: &mut impl Pull<'de>,
    entries: &mut Vec<Ev<'de>>,
) -> Result<bool, Error> {
    if w.stack.len() == 1 && matches!(p.peek()?.node, Node::MapEnd) {
        return Ok(true);
    }
    let ev = p.next()?;
    let i = entries.len();
    let done = match ev.node {
        Node::Scalar { .. } => w.scalar(i, &ev)?,
        Node::SeqStart { .. } | Node::MapStart { .. } => {
            w.open(Some(i), &ev)?;
            None
        }
        Node::SeqEnd | Node::MapEnd => Some(w.close(Some(i))?),
        _ => return Err(Error::internal("node expected", Some(ev.span))),
    };
    entries.push(ev);
    if let Some(d) = done {
        if w.deliver(d)?.is_some() {
            return Err(Error::internal("unbalanced buffered mapping", None));
        }
    }
    Ok(false)
}

/// The merge sources of one mapping that streams (merge keys on, duplicate
/// keys refused): its explicit entries go to the target as they are read,
/// and only the value of each `<<` is buffered here. The merged entries
/// whose keys are not explicit follow the explicit ones at the mapping's
/// end ([`MergeSources::merged`]), as the `Value` builder orders them.
///
/// A `<<` value is planned (nested merges and every mapping inside it
/// included), except the common `<<: *base`: the alias of a mapping this
/// read delivered where it was anchored, and so checked then, is only
/// buffered, its top-level keys resolved for their identity, and its
/// entries reach the target as written (nested mappings then stream as
/// any other). A per-node read replays anchors from outside the node,
/// which it never delivered: their aliases are planned.
pub(crate) struct MergeSources<'de> {
    /// The streaming mapping's state as a planned mapping's root: its
    /// merged entries, and whether a `<<` was seen (a repeated one is a
    /// duplicate key).
    root: Box<MapState<'de>>,
    items: Vec<Item>,
    events: Vec<Ev<'de>>,
    #[allow(clippy::vec_box)]
    pool: Vec<Box<MapState<'de>>>,
    /// The entries of a replayed source mapping read without planning:
    /// key identity, first event, and end (exclusive) in `events`.
    replayed: Vec<(KeyRepr<'de>, usize, usize)>,
}

impl<'de> MergeSources<'de> {
    pub(crate) fn new() -> Box<Self> {
        Box::new(Self {
            root: Box::default(),
            items: Vec::new(),
            events: Vec::new(),
            pool: Vec::new(),
            replayed: Vec::new(),
        })
    }

    /// Reads the value of the `<<` key at `span` (just read) from `p` as a
    /// merge source: checked as the `Value` builder checks it, in its
    /// order (the value's own errors, then a repeated `<<`, then the
    /// value's shape), with the builder's paths below `<<`.
    pub(crate) fn read_value<P: Pull<'de>>(
        &mut self,
        p: &mut Src<'de, P>,
        span: Span,
        replayed: bool,
        resolver: Resolver,
        warnings: &mut crate::diag_cap::Capped,
    ) -> Result<(), Error> {
        #[cfg(test)]
        let before = self.events.len();
        let result = match self.read_replayed(p, span, replayed, resolver, warnings) {
            Ok(true) => Ok(()),
            Ok(false) => self.plan_value(p, span, replayed, resolver, warnings),
            Err(e) => Err(e),
        };
        #[cfg(test)]
        PLANNED_EVENTS.with(|c| {
            c.set(
                c.get()
                    .saturating_add(self.events.len().saturating_sub(before)),
            );
        });
        result
    }

    /// `<<: *base`: an alias of an untagged mapping that this read
    /// delivered (and so checked) where it was anchored, every event of the
    /// replay at a replayed position, each top-level key a scalar other
    /// than `<<`. Its events are buffered and its top-level keys resolved;
    /// nothing else needs checking. Anything else (an inline value, an
    /// anchor from outside a per-node read, which this read never
    /// checked): what was read goes back to `p`, and `false` (the value is
    /// planned).
    fn read_replayed<P: Pull<'de>>(
        &mut self,
        p: &mut Src<'de, P>,
        span: Span,
        replayed: bool,
        resolver: Resolver,
        warnings: &mut crate::diag_cap::Capped,
    ) -> Result<bool, Error> {
        // The path the `Value` builder reports below `<<`.
        let first_ev = p.peek().map_err(|e| e.in_key("<<"))?;
        if !matches!(first_ev.node, Node::MapStart { tag: EvTag::Absent })
            || !p.peeked_delivered_replay()
        {
            return Ok(false);
        }
        let start = self.events.len();
        let entries = self.replayed.len();
        // Collections open inside the source mapping (0: at its top level,
        // where the next event is a key, or a value when `key` is set).
        let mut depth = 0usize;
        let mut key: Option<(KeyRepr<'de>, usize)> = None;
        let mut first = true;
        loop {
            let ev = match p.next() {
                Ok(ev) => ev,
                Err(e) => {
                    return Err(self.path_at(start, span, replayed, resolver, e));
                }
            };
            let at = self.events.len();
            let step = match &ev.node {
                // The source mapping's own start and end (its edges stand
                // at the alias).
                Node::MapStart { .. } if first => Scan::Go,
                Node::MapEnd if depth == 0 && key.is_none() => {
                    self.events.push(ev);
                    break;
                }
                _ if !ev.replayed => Scan::Plan,
                Node::Scalar { value, plain, tag } if depth == 0 && key.is_none() => {
                    if is_merge_key(&resolver, tag, *plain, value) {
                        Scan::Plan
                    } else {
                        let r = resolve(&resolver, tag, *plain, value, ev.span)
                            .map_err(|e| e.in_key("<<"))?;
                        key = Some((scalar_key(r, value, tag), at));
                        Scan::Go
                    }
                }
                // A complex key.
                Node::SeqStart { .. } | Node::MapStart { .. } if depth == 0 && key.is_none() => {
                    Scan::Plan
                }
                Node::SeqStart { .. } | Node::MapStart { .. } => {
                    depth = depth.saturating_add(1);
                    Scan::Go
                }
                Node::SeqEnd | Node::MapEnd => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        Scan::EntryEnds
                    } else {
                        Scan::Go
                    }
                }
                Node::Scalar { .. } if depth == 0 => Scan::EntryEnds,
                Node::Scalar { .. } => Scan::Go,
                _ => Scan::Plan,
            };
            first = false;
            self.events.push(ev);
            match step {
                Scan::Go => {}
                Scan::EntryEnds => {
                    if let Some((k, from)) = key.take() {
                        self.replayed.push((k, from, at.saturating_add(1)));
                    }
                }
                Scan::Plan => {
                    let read: Vec<Ev<'de>> = self.events.drain(start..).collect();
                    self.replayed.truncate(entries);
                    p.push_front_all(read, false);
                    return Ok(false);
                }
            }
        }
        // As the builder: a repeated `<<` (after the value, which has no
        // error of its own); the shape is a mapping.
        self.root
            .merge
            .key(span, replayed, DuplicateKeys::Error, warnings)?;
        Ok(true)
    }

    /// `e`, met reading a `<<` value whose events from `start` were
    /// buffered, with the path the `Value` builder reports there: the
    /// buffered events replayed through a walk.
    fn path_at(
        &self,
        start: usize,
        span: Span,
        replayed: bool,
        resolver: Resolver,
        e: Error,
    ) -> Error {
        let mut root: Box<MapState<'de>> = Box::default();
        root.pending = Some(Pending::Merge(span, replayed));
        // Its events were checked when they were first read: nothing is
        // reported again.
        let mut quiet = crate::diag_cap::Capped::with_keep(0);
        let mut w = Walker {
            record: true,
            inline: false,
            anchors: None,
            serial: 0,
            copies: AliasMeter::new(&oneyaml_core::Limits::default()),
            replay: None,
            tree: None,
            history: HashMap::new(),
            resolver,
            policy: DuplicateKeys::Error,
            warnings: &mut quiet,
            stack: vec![Frame {
                start: None,
                span,
                replayed,
                custom: false,
                merge_part: false,
                in_key: false,
                count: (0, 0),
                extra: None,
                body: Body::Map(root),
            }],
            items: Vec::new(),
            pool: Vec::new(),
        };
        for (i, ev) in self.events.iter().enumerate().skip(start) {
            let done = match ev.node {
                Node::Scalar { .. } => w.scalar(i, ev),
                Node::SeqStart { .. } | Node::MapStart { .. } => w.open(Some(i), ev).map(|()| None),
                Node::SeqEnd | Node::MapEnd => w.close(Some(i)).map(Some),
                _ => Ok(None),
            };
            let delivered = match done {
                Ok(Some(d)) => w.deliver(d).map(|_| ()),
                Ok(None) => Ok(()),
                Err(x) => Err(x),
            };
            if delivered.is_err() {
                // Checked content never fails the walk; should it, the
                // error keeps the segment it surely has.
                return e.in_key("<<");
            }
        }
        w.with_path(e)
    }

    /// Plans the value of the `<<` key at `span` from `p` (see
    /// [`MergeSources::read_value`]).
    fn plan_value(
        &mut self,
        p: &mut impl Pull<'de>,
        span: Span,
        replayed: bool,
        resolver: Resolver,
        warnings: &mut crate::diag_cap::Capped,
    ) -> Result<(), Error> {
        #[cfg(test)]
        PLANNED_MERGE_VALUES.with(|c| c.set(c.get().saturating_add(1)));
        let mut root = std::mem::take(&mut self.root);
        root.pending = Some(Pending::Merge(span, replayed));
        let mut w = Walker {
            record: true,
            inline: false,
            anchors: None,
            serial: 0,
            copies: AliasMeter::new(&oneyaml_core::Limits::default()),
            replay: None,
            tree: None,
            history: HashMap::new(),
            resolver,
            // Only mappings that refuse duplicates stream.
            policy: DuplicateKeys::Error,
            warnings,
            stack: vec![Frame {
                start: None,
                span,
                replayed,
                custom: false,
                merge_part: false,
                in_key: false,
                count: (0, 0),
                extra: None,
                body: Body::Map(root),
            }],
            items: std::mem::take(&mut self.items),
            pool: std::mem::take(&mut self.pool),
        };
        let result = loop {
            match merge_step(&mut w, p, &mut self.events) {
                Ok(true) => break Ok(()),
                Ok(false) => {}
                // The path the `Value` builder reports.
                Err(e) => break Err(w.with_path(e)),
            }
        };
        let root = match w.stack.drain(..).next() {
            Some(Frame {
                body: Body::Map(m), ..
            }) => m,
            _ => return Err(Error::internal("merge root", Some(span))),
        };
        self.root = root;
        self.items = std::mem::take(&mut w.items);
        self.pool = std::mem::take(&mut w.pool);
        result
    }

    /// The merged entries whose keys `explicit` does not hold, earlier
    /// sources first, and whether they are planned (no `<<`, no
    /// duplicates, nested mappings rewritten); a replayed source's entries
    /// are as written.
    pub(crate) fn merged(
        self,
        explicit: impl Fn(&KeyRepr<'de>) -> bool,
    ) -> Result<(Vec<Ev<'de>>, bool), Error> {
        let Self {
            mut root,
            mut items,
            events,
            replayed,
            ..
        } = self;
        if !replayed.is_empty() {
            // One source (a second `<<` is refused): its entries in order,
            // each a run of events.
            let mut kept = replayed
                .into_iter()
                .filter(|(k, _, _)| !explicit(k))
                .map(|(_, from, to)| (from, to))
                .peekable();
            let mut out = Vec::new();
            for (i, ev) in events.into_iter().enumerate() {
                while kept.peek().is_some_and(|&(_, to)| i >= to) {
                    kept.next();
                }
                match kept.peek() {
                    Some(&(from, _)) if from <= i => out.push(ev),
                    Some(_) => {}
                    None => break,
                }
            }
            return Ok((out, false));
        }
        let mut kids = Vec::new();
        if let Some(merged) = std::mem::take(&mut root.merge).into_merged() {
            for (key, ent) in merged.into_entries() {
                if !explicit(&key) {
                    kids.push(ent.k);
                    kids.push(ent.v);
                }
            }
        }
        if kids.is_empty() {
            return Ok((Vec::new(), true));
        }
        items.push(Item::Coll {
            start: None,
            end: None,
            kids,
        });
        let at = items.len().saturating_sub(1);
        flatten(items, at, events).map(|evs| (evs, true))
    }
}

/// What [`MergeSources::read_replayed`] does with an event.
enum Scan {
    Go,
    /// The value of a top-level entry completed with this event.
    EntryEnds,
    /// Not a replayed source mapping: the value is planned instead.
    Plan,
}

/// Reads and plans one event of a `<<` value; `true` once the value is
/// complete (delivered to the streaming mapping's root).
fn merge_step<'de>(
    w: &mut Walker<'de, '_>,
    p: &mut impl Pull<'de>,
    entries: &mut Vec<Ev<'de>>,
) -> Result<bool, Error> {
    let ev = p.next()?;
    let i = entries.len();
    let done = match ev.node {
        Node::Scalar { .. } => w.scalar(i, &ev)?,
        Node::SeqStart { .. } | Node::MapStart { .. } => {
            w.open(Some(i), &ev)?;
            None
        }
        Node::SeqEnd | Node::MapEnd => Some(w.close(Some(i))?),
        _ => return Err(Error::internal("node expected", Some(ev.span))),
    };
    entries.push(ev);
    if let Some(d) = done {
        if w.deliver(d)?.is_some() {
            return Err(Error::internal("unbalanced merge value", None));
        }
    }
    Ok(w.stack.len() == 1)
}

/// What [`check_unexpanded`] found.
pub(crate) struct Unexpanded {
    /// Warnings of the meaning checks (tolerated duplicates), the first
    /// `max_diagnostics + 1`.
    pub(crate) warnings: Vec<Diagnostic>,
    /// Errors in source order; at most one unless recovering, then the
    /// first `max_diagnostics + 1`.
    pub(crate) errors: Vec<Error>,
    /// Warnings and errors counted but not kept.
    pub(crate) suppressed: crate::diag_cap::Dropped,
    /// The code and span of every error not kept, so the record
    /// of errors (`has_errors`, restricted mode) never depends on the cap.
    pub(crate) dropped_errors: Vec<(oneyaml_core::Code, oneyaml_core::Span)>,
}

/// Checks the meaning of every document of `src` **without expanding
/// aliases** (`Document` stays alias-free): scalars and collection
/// tags resolved, duplicate keys (per the policy) and merge values
/// checked, all with the rules and messages the `Value` builder uses, in
/// source order. An alias stands for its anchor: its shape (for
/// `<<`) and, as a key, a scalar anchor's value or an identity equal only
/// to other aliases of the same collection anchor; an undefined or
/// recursive alias is Y1015, as the expander reports it. The alias budget
/// and the depth after expansion are not checked.
///
/// `single`: a second document is an error. `recover`: every document is
/// checked, and a meaning error skips the rest of its document; a syntax
/// error ends the check (its diagnostic is the tree's). Otherwise the
/// first error of any kind ends it.
pub(crate) fn check_unexpanded<'de>(
    src: &mut impl Source<'de>,
    settings: &Settings,
    single: bool,
    recover: bool,
) -> Unexpanded {
    let mut warnings = crate::diag_cap::Capped::new(settings.get_limits());
    let mut errors = Vec::new();
    // Errors past the cap are counted, not kept.
    let keep_errors = settings
        .get_limits()
        .get_max_diagnostics()
        .saturating_add(1);
    let mut dropped_errors = Vec::new();
    {
        let mut w = Walker {
            record: false,
            inline: true,
            anchors: None,
            serial: 0,
            copies: AliasMeter::new(settings.get_limits()),
            replay: None,
            tree: None,
            history: HashMap::new(),
            resolver: Resolver::new(settings),
            policy: settings.get_duplicate_keys(),
            warnings: &mut warnings,
            stack: Vec::new(),
            items: Vec::new(),
            pool: Vec::new(),
        };
        let mut docs = 0usize;
        let mut skip = false;
        loop {
            let ev = match src.next_event() {
                Ok(ev) => {
                    // An anchored node can be read again, for the
                    // value of an alias of it as a key; own nodes scale the
                    // copy budget, as the `Value` read's alias budget.
                    if ev.anchor.is_some() {
                        w.replay = src.replay_last();
                    }
                    if ev.starts_node() {
                        w.copies.own = w.copies.own.saturating_add(1);
                    }
                    w.copies.read_to(ev.span.end);
                    ev
                }
                Err(e) => {
                    if !recover && !skip {
                        errors.push(w.with_path(e));
                    } else if !recover {
                        errors.push(e);
                    }
                    break;
                }
            };
            let step = match ev.node {
                Node::StreamEnd => break,
                Node::DocStart { .. } if single && docs == 1 => {
                    errors.push(
                        Error::new(
                            oneyaml_core::code::MULTIPLE_DOCUMENTS,
                            "the input has more than one document",
                            Some(ev.span),
                        )
                        .with_help("use oneyaml::Stream to read each document"),
                    );
                    break;
                }
                Node::DocStart { yaml11 } => {
                    docs = docs.saturating_add(1);
                    w.resolver = Resolver::for_document(settings, yaml11);
                    w.anchors = None;
                    w.history.clear();
                    w.copies.reset(ev.span.start);
                    w.stack.clear();
                    skip = false;
                    Ok(())
                }
                Node::DocEnd => Ok(()),
                _ if skip => Ok(()),
                // The path the `Value` builder reports.
                _ => w.step(&ev).map_err(|e| w.with_path(e)),
            };
            if let Err(e) = step {
                if errors.len() < keep_errors {
                    errors.push(e);
                } else {
                    let d = e.diagnostic();
                    dropped_errors.push((d.code, d.primary.span));
                }
                if !recover {
                    break;
                }
                skip = true;
                w.stack.clear();
            }
        }
    }
    let (warnings, mut suppressed) = warnings.into_parts();
    for (_, span) in &dropped_errors {
        suppressed = suppressed.and(crate::diag_cap::Dropped::one(span.start));
    }
    Unexpanded {
        warnings,
        errors,
        suppressed,
        dropped_errors,
    }
}

/// The planned events, moved out of the buffer in output order (an
/// explicit stack). Every item is emitted at most once.
fn flatten<'de>(items: Vec<Item>, root: usize, evs: Vec<Ev<'de>>) -> Result<Vec<Ev<'de>>, Error> {
    enum Step {
        Node(usize),
        End(usize),
    }
    let mut evs: Vec<Option<Ev<'de>>> = evs.into_iter().map(Some).collect();
    let mut take = |i: usize| {
        evs.get_mut(i)
            .and_then(Option::take)
            .ok_or_else(|| Error::internal("planned event reused", None))
    };
    let mut out = Vec::new();
    // The root's own start and end are not buffered: emit its children.
    let mut todo: Vec<Step> = match items.get(root) {
        Some(Item::Coll { kids, .. }) => kids.iter().rev().map(|k| Step::Node(*k)).collect(),
        _ => return Err(Error::internal("planned root", None)),
    };
    while let Some(step) = todo.pop() {
        match step {
            Step::End(i) => out.push(take(i)?),
            Step::Node(n) => match items.get(n) {
                Some(Item::Leaf(i)) => out.push(take(*i)?),
                Some(Item::Coll { start, end, kids }) => {
                    if let Some(s) = start {
                        out.push(take(*s)?);
                    }
                    if let Some(e) = end {
                        todo.push(Step::End(*e));
                    }
                    todo.extend(kids.iter().rev().map(|k| Step::Node(*k)));
                }
                None => return Err(Error::internal("planned item", None)),
            },
        }
    }
    Ok(out)
}

/// Consumes the node that starts with `first` from `p`, checking it as the
/// `Value` builder would (ignored content is still resolved and
/// checked for duplicate and merge keys), without building it.
pub(crate) fn check_node<'de>(
    p: &mut impl Pull<'de>,
    first: Ev<'de>,
    resolver: Resolver,
    policy: DuplicateKeys,
    warnings: &mut crate::diag_cap::Capped,
    scratch: &mut Scratch<'de>,
) -> Result<(), Error> {
    // A lone scalar (the common ignored value): resolved, nothing else to
    // check.
    if let Node::Scalar { value, plain, tag } = &first.node {
        resolve(&resolver, tag, *plain, value, first.span)?;
        return Ok(());
    }
    let mut w = Walker {
        record: false,
        inline: false,
        anchors: None,
        serial: 0,
        copies: AliasMeter::new(&oneyaml_core::Limits::default()),
        replay: None,
        tree: None,
        history: HashMap::new(),
        resolver,
        policy,
        warnings,
        stack: std::mem::take(&mut scratch.stack),
        items: Vec::new(),
        pool: std::mem::take(&mut scratch.pool),
    };
    let result = check_with(&mut w, p, first);
    // Keep the allocations (cleared) for the next skip.
    w.stack.clear();
    scratch.stack = w.stack;
    scratch.pool = w.pool;
    result
}

fn check_with<'de>(
    w: &mut Walker<'de, '_>,
    p: &mut impl Pull<'de>,
    first: Ev<'de>,
) -> Result<(), Error> {
    let (resolver, policy) = (w.resolver, w.policy);
    let mut ev = first;
    loop {
        let done = match ev.node {
            Node::Scalar { .. } => w.scalar(0, &ev)?,
            Node::SeqStart { .. } | Node::MapStart { .. } if w.key_position() => {
                // A complex key: its identity (and its checks) come from
                // the shared builder; keys are held per open mapping.
                let (span, replayed) = (ev.span, ev.replayed);
                let mut cx = Cx {
                    resolver,
                    policy,
                    warnings: &mut *w.warnings,
                };
                let key = build_from(p, &mut cx, ev)?;
                Some(Done {
                    item: 0,
                    span,
                    replayed,
                    shape: Shape::Other,
                    key: Some(KeyRepr::of_value(key)),
                    value: None,
                    source: None,
                    unknown: false,
                })
            }
            Node::SeqStart { .. } | Node::MapStart { .. } => {
                w.open(None, &ev)?;
                None
            }
            Node::SeqEnd | Node::MapEnd => Some(w.close(None)?),
            _ => return Err(Error::internal("node expected", Some(ev.span))),
        };
        if let Some(d) = done {
            if w.deliver(d)?.is_some() {
                return Ok(());
            }
        }
        ev = p.next()?;
    }
}

/// What [`build_replayed`] found.
enum Replayed {
    /// The value, its node count and its scalar bytes.
    Built {
        value: Value,
        nodes: usize,
        bytes: usize,
    },
    /// Refused: past a cap (what it read so far), or an error the build
    /// found that only expansion reveals (the `Value` read has
    /// refused by this alias, as only an over-budget alias key leaves such
    /// an error unreported where it stands).
    Refused { nodes: usize, bytes: usize },
}

/// Builds the value of the node `replay` reads (`Document`'s
/// strict check): aliases inside it expanded through `defs` (each the
/// last definition of its name before it), producing at most `cap` nodes
/// and `byte_cap` scalar bytes.
///
/// # Errors
///
/// A broken invariant only (`Y9000`); a cap or what expansion
/// reveals is [`Replayed::Refused`].
fn build_replayed<'de>(
    replay: Replay<'de>,
    defs: &Defs<'_, 'de>,
    resolver: Resolver,
    policy: DuplicateKeys,
    (cap, byte_cap): (usize, usize),
) -> Result<Replayed, Error> {
    let mut pull = Replayer {
        stack: vec![replay.source()],
        defs,
        peeked: None,
        produced: 0,
        bytes: 0,
        cap,
        byte_cap,
        over: false,
        last_end: 0,
    };
    // Its meaning was checked where it was read: nothing is reported again.
    let mut warnings = crate::diag_cap::Capped::with_keep(0);
    let mut cx = Cx {
        resolver,
        policy,
        warnings: &mut warnings,
    };
    let built = pull
        .next()
        .and_then(|first| build_from(&mut pull, &mut cx, first));
    let (nodes, bytes) = (pull.produced, pull.bytes);
    match built {
        Ok(value) => Ok(Replayed::Built {
            value,
            nodes,
            bytes,
        }),
        Err(_) if pull.over => Ok(Replayed::Refused { nodes, bytes }),
        Err(e) if e.code() == oneyaml_core::code::INTERNAL_ERROR => Err(e),
        Err(_) => Ok(Replayed::Refused { nodes, bytes }),
    }
}

/// The events of a replayed node with its aliases expanded (an
/// explicit stack of replays, no recursion), counted against a cap.
struct Replayer<'h, 'de> {
    stack: Vec<TreeSource<'de>>,
    defs: &'h Defs<'h, 'de>,
    peeked: Option<Ev<'de>>,
    produced: usize,
    bytes: usize,
    cap: usize,
    byte_cap: usize,
    /// A cap was passed (the error that ends the build says so only).
    over: bool,
    last_end: usize,
}

impl<'de> Replayer<'_, 'de> {
    fn pull(&mut self) -> Result<Ev<'de>, Error> {
        loop {
            let Some(top) = self.stack.last_mut() else {
                return Ok(Ev::new(Node::StreamEnd, Span::empty(self.last_end)));
            };
            let ev = top.next_event()?;
            match ev.node {
                Node::StreamEnd => {
                    self.stack.pop();
                    continue;
                }
                Node::Alias { name } => {
                    // The walk accepted this alias: it has a definition.
                    let target = self
                        .defs
                        .before(name, ev.span.start)?
                        .ok_or_else(|| broken("replayed alias undefined"))?;
                    self.stack.push(target.source());
                    continue;
                }
                _ => {}
            }
            if let Node::Scalar { value, .. } = &ev.node {
                self.bytes = self.bytes.saturating_add(value.len());
            }
            if ev.starts_node() {
                self.produced = self.produced.saturating_add(1);
            }
            if self.produced > self.cap || self.bytes > self.byte_cap {
                self.over = true;
                return Err(Error::internal("past the copy budget", Some(ev.span)));
            }
            self.last_end = ev.span.end;
            return Ok(ev);
        }
    }
}

impl<'de> Pull<'de> for Replayer<'_, 'de> {
    fn next(&mut self) -> Result<Ev<'de>, Error> {
        match self.peeked.take() {
            Some(ev) => Ok(ev),
            None => self.pull(),
        }
    }

    fn peek(&mut self) -> Result<&Ev<'de>, Error> {
        if self.peeked.is_none() {
            self.peeked = Some(self.pull()?);
        }
        self.peeked
            .as_ref()
            .ok_or_else(|| Error::internal("peeked event", None))
    }

    fn last_end(&self) -> usize {
        self.last_end
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::PLANNED_EVENTS;
    use oneyaml_core::{Compat, Settings};

    /// `depth` mappings nested under key `n`, each with `width - 1` other
    /// entries; the innermost is `{}`.
    fn nested(width: usize, depth: usize) -> String {
        let entries: Vec<String> = (0..width.saturating_sub(1))
            .map(|i| format!("k{i}: {i}"))
            .collect();
        let open = format!("{{{}, n: ", entries.join(", "));
        format!("{}{{}}{}", open.repeat(depth), "}".repeat(depth))
    }

    /// Events of the planned (buffered) region: every event but the
    /// outermost mapping's start and end.
    fn planned(src: &str, settings: &Settings) -> usize {
        PLANNED_EVENTS.with(|c| c.set(0));
        let v: serde_json::Value = crate::from_str_with(src, settings).unwrap();
        assert!(v.is_object());
        PLANNED_EVENTS.with(std::cell::Cell::get)
    }

    /// Runs `f` on a thread with a large stack (debug builds).
    fn big_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap();
    }

    /// `{ {…{a: 1}: 1, k0: 0 … }: 1, k0: 0 … k18: 18 }`: complex keys
    /// nested `depth` deep, each mapping `width` entries wide.
    fn nested_keys(width: usize, depth: usize) -> String {
        let rest: Vec<String> = (0..width.saturating_sub(1))
            .map(|i| format!("k{i}: {i}"))
            .collect();
        let rest = rest.join(", ");
        let mut s = "{a: 1}".to_owned();
        for _ in 0..depth {
            s = format!("{{{s}: 1, {rest}}}");
        }
        s
    }

    #[test]
    fn nested_complex_keys_are_built_once() {
        // Under planning (LastWins), each complex key's identity is
        // built bottom-up once, not rebuilt per enclosing key (it was
        // O(n × key depth)). The builder then reads each event a bounded
        // number of times (the top-level key: its duplicate check and the
        // `Value` target), whatever the depth.
        use crate::construct::build::BUILT_EVENTS;
        use std::collections::HashMap;
        big_stack(|| {
            for settings in [Compat::serde_yaml(), Settings::new()] {
                for depth in [1usize, 10, 100, 400] {
                    let src = nested_keys(20, depth);
                    BUILT_EVENTS.with(|c| c.set(0));
                    let m: HashMap<crate::Value, crate::Value> =
                        crate::from_str_with(&src, &settings).unwrap();
                    assert_eq!(m.len(), 20);
                    let built = BUILT_EVENTS.with(std::cell::Cell::get);
                    let events = depth.saturating_mul(41).saturating_add(4);
                    assert!(built <= 3 * events, "depth {depth}: {built} > 3 × {events}");
                    // The same key as the `Value` path.
                    let v: crate::Value = crate::from_str_with(&src, &settings).unwrap();
                    let key = v.as_mapping().unwrap().get_index(0).unwrap().0;
                    assert!(m.contains_key(key));
                }
            }
        });
    }

    /// `{<<: {<<: … {k0: 0 … kN: N} …}}`, `depth` merges deep.
    fn merge_chain(keys: usize, depth: usize, side: bool) -> String {
        let inner: Vec<String> = (0..keys).map(|i| format!("k{i}: {i}")).collect();
        let mut s = format!("{{{}}}", inner.join(", "));
        for d in 0..depth {
            s = if side {
                // An earlier, smaller source and an explicit key per level.
                format!("{{x{d}: {d}, <<: [{{k{d}: -1, y{d}: {d}}}, {s}]}}")
            } else {
                format!("{{<<: {s}}}")
            };
        }
        s
    }

    #[test]
    fn merge_chains_are_linear_on_both_paths() {
        // A merge chain moves each entry O(1) (chain) or
        // O(log n) (small-to-large) times on the `Value` and typed paths
        // alike; it was O(entries × depth).
        use crate::construct::merge::MOVES;
        use std::collections::BTreeMap;
        big_stack(|| {
            let s = Settings::new().merge_keys(true);
            for side in [false, true] {
                // With a sequence per level, 400 levels nest 800 deep
                // (`max_depth` is 1024).
                let top = if side { 400usize } else { 1000 };
                for depth in [1usize, 10, 100, top] {
                    let keys = 2000usize;
                    let src = merge_chain(keys, depth, side);
                    MOVES.with(|c| c.set(0));
                    let v: crate::Value = crate::from_str_with(&src, &s).unwrap();
                    let value_moves = MOVES.with(std::cell::Cell::get);
                    MOVES.with(|c| c.set(0));
                    let t: BTreeMap<String, i64> = crate::from_str_with(&src, &s).unwrap();
                    let typed_moves = MOVES.with(std::cell::Cell::get);
                    let n = if side { keys + 2 * depth } else { keys };
                    assert_eq!(v.as_mapping().unwrap().len(), n);
                    assert_eq!(t.len(), n);
                    // Explicit keys win, then earlier sources.
                    if side && depth > 0 {
                        assert_eq!(t["k0"], -1);
                        assert_eq!(v["x0"], crate::Value::from(0));
                    }
                    let bound = if side { 4 * (keys + 4 * depth) } else { keys };
                    assert!(value_moves <= bound, "value {side} {depth}: {value_moves}");
                    assert!(typed_moves <= bound, "typed {side} {depth}: {typed_moves}");
                }
            }
        });
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn anchors_cost_what_they_did() {
        // The alias-key replay adds nothing per
        // anchor: `Def` is the size of the `Option<usize>` it replaced, and
        // `Anchored::prev` lives in what was padding (112 bytes before).
        // The byte count lives where the slot's `Option` tag was.
        assert_eq!(std::mem::size_of::<super::Def>(), 16);
        assert_eq!(std::mem::size_of::<super::Anchored<'static>>(), 112);
    }

    #[test]
    fn replay_counts_that_agree_pass() {
        assert!(super::replay_counted((3, 10), (3, 10)).is_ok());
        // Saturated counts mean "at least".
        let big = usize::MAX;
        assert!(super::replay_counted((u32::MAX, u32::MAX), (big, big)).is_ok());
        assert!(super::replay_counted((u32::MAX, 7), (big, 7)).is_ok());
    }

    /// A replay that produced other counts than the
    /// walk recorded is a broken invariant: `Y9000`, checked here on the
    /// release path in every build.
    #[test]
    fn replay_count_drift_is_an_internal_error() {
        for (counted, produced) in [
            ((3, 10), (4, 10)),
            ((3, 10), (3, 9)),
            ((u32::MAX, 1), (9, 2)),
            // An undercount is drift too, saturated or not.
            ((0, 0), (2, 1)),
        ] {
            let e = super::replay_counts(counted, produced).unwrap_err();
            assert_eq!(e.code(), oneyaml_core::code::INTERNAL_ERROR, "{counted:?}");
        }
    }

    /// In debug builds the `debug_assert!` fires first; in release the
    /// error above is returned.
    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "replay count drifted"))]
    fn replay_count_drift_asserts_in_debug() {
        let e = super::replay_counted((3, 10), (4, 10)).unwrap_err();
        assert_eq!(e.code(), oneyaml_core::code::INTERNAL_ERROR);
    }

    /// `l0` a 1,000-byte scalar (or ten empty ones), then `levels` levels
    /// of ten aliases of the previous one, then `tail`.
    fn laughs(levels: usize, l0: &str, tail: &str) -> String {
        let mut s = format!("l0: &l0 [{l0}]\n");
        for i in 1..=levels {
            let refs = vec![format!("*l{}", i.saturating_sub(1)); 10].join(", ");
            s.push_str(&format!("l{i}: &l{i} [{refs}]\n"));
        }
        s.push_str(tail);
        s
    }

    /// Expansion counts that saturate (on
    /// 32-bit targets, wasm32 among them, a small alias chain gets there)
    /// are a true "at least" for the node that is that large, and never
    /// an undercount for a node after it, which would pass the copy-budget
    /// pre-check and trip the replay drift check (`Y9000` on a valid
    /// document). Counts here saturate at `u32::MAX`, as `usize` does on a
    /// 32-bit target (the wasm32 repro shapes).
    #[test]
    fn saturated_counts_never_undercount() {
        let x = "x".repeat(1000);
        let empties = ["''"; 10].join(", ");
        let dup = Some(oneyaml_core::code::DUPLICATE_KEY);
        let cases = [
            // Bytes saturate (1e10 expanded), nodes do not.
            (laughs(7, &x, "k: &k [y]\nm: {? *k : 1}\n"), None),
            // Below saturation (1e9 bytes).
            (laughs(6, &x, "k: &k [y]\nm: {? *k : 1}\n"), None),
            // Saturated, no alias key.
            (laughs(7, &x, "k: &k [y]\nm: {[y]: 1}\n"), None),
            // A true duplicate the strict check must see.
            (laughs(7, &x, "k: &k [y]\nm: {? *k : 1, [y]: 2}\n"), dup),
            // Nodes saturate (1e10 expanded).
            (
                laughs(9, &empties, "k: &k ['']\nm: {? *k : 1, ['']: 2}\n"),
                dup,
            ),
            // An alias key of the saturated node itself: past every
            // budget, its opaque identity (no false duplicate, no drift).
            (laughs(7, &x, "m: {? *l7 : 1, ? *l6 : 2}\n"), None),
            (laughs(7, &x, "m: {? *l7 : 1, ? *l7 : 2}\n"), dup),
        ];
        for cap in [u32::MAX as usize, usize::MAX] {
            super::COUNT_CAP.with(|c| c.set(cap));
            for (src, want) in &cases {
                let got = crate::Document::parse_with(src.as_str(), &Settings::new())
                    .err()
                    .map(|e| e.code());
                assert_eq!(got, *want, "cap {cap}: {src:.80}");
            }
        }
        super::COUNT_CAP.with(|c| c.set(usize::MAX));
    }

    /// `depth` mappings nested under key `n`, each with `width - 1` other
    /// entries and a `<<: {m: d}` entry; the innermost is `{}`.
    fn nested_merging(width: usize, depth: usize) -> String {
        let mut s = "{}".to_owned();
        for d in 0..depth {
            let entries: Vec<String> = (0..width.saturating_sub(1))
                .map(|i| format!("k{i}: {i}"))
                .collect();
            s = format!("{{{}, <<: {{m: {d}}}, n: {s}}}", entries.join(", "));
        }
        s
    }

    #[test]
    fn replayed_source_mappings_are_buffered_not_planned() {
        use super::PLANNED_MERGE_VALUES;
        use std::collections::{BTreeMap, HashMap};
        let s = Settings::new().merge_keys(true);
        for (src, walks) in [
            // `<<: *base`: buffered as replayed.
            ("b: &b {x: 1, y: {z: 2}}\nm: {y: 0, <<: *b}\n", 0),
            ("b: &b {}\nm: {<<: *b}\n", 0),
            // A source whose entries hold merges of their own.
            ("b: &b {x: 1}\nc: &c {k: {<<: *b}}\nm: {<<: *c}\n", 0),
            // Planned: inline, a sequence, a tagged anchor, a `<<` or a
            // complex key at the source's top level.
            ("b: {}\nm: {<<: {x: 1}}\n", 1),
            ("b: {}\nm: {<<: {}}\n", 1),
            ("b: &b {x: 1}\nm: {<<: [*b]}\n", 1),
            ("b: &b !!map {x: 1}\nm: {<<: *b}\n", 1),
            ("c: &c {w: 3}\nb: &b {x: 1, <<: *c}\nm: {<<: *b}\n", 1),
            ("b: &b {[1]: 1}\nm: {<<: *b}\n", 1),
        ] {
            PLANNED_MERGE_VALUES.with(|c| c.set(0));
            type M = HashMap<crate::Value, crate::Value>;
            let t: BTreeMap<String, M> = crate::from_str_with(src, &s).unwrap();
            assert_eq!(
                PLANNED_MERGE_VALUES.with(std::cell::Cell::get),
                walks,
                "{src:?}"
            );
            let v: crate::Value = crate::from_str_with(src, &s).unwrap();
            let want: M = v["m"].as_mapping().unwrap().clone().into_iter().collect();
            assert_eq!(t["m"], want, "{src:?}");
        }
        // A node read replays the anchor from outside the node, which it
        // never read: planned (and so checked).
        let doc = crate::Document::parse_with("b: &b {x: 1}\nm: {y: 2, <<: *b}\n", &s).unwrap();
        PLANNED_MERGE_VALUES.with(|c| c.set(0));
        let t: HashMap<crate::Value, crate::Value> = doc.get("m").unwrap().deserialize().unwrap();
        assert_eq!(PLANNED_MERGE_VALUES.with(std::cell::Cell::get), 1);
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn planning_is_linear_in_nesting() {
        // Under LastWins each mapping is planned once, however deep, so a
        // typed target that visits every level reads each event through
        // the planner exactly once (it was O(n × depth)). With merge keys
        // alone mappings stream: only `<<` values are buffered, each
        // event at most once, so nothing is buffered without a `<<`.
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let width = 20usize;
                let lastwins = Compat::serde_yaml();
                let merge = Settings::new().merge_keys(true);
                for depth in [1usize, 10, 60, 120] {
                    let events = depth.saturating_mul(width.saturating_mul(2).saturating_add(1));
                    assert_eq!(planned(&nested(width, depth), &lastwins), events);
                    assert_eq!(planned(&nested(width, depth), &merge), 0);
                    // `{m: d}`: four events per level.
                    let src = nested_merging(width, depth);
                    assert_eq!(planned(&src, &merge), 4 * depth, "depth {depth}");
                    // A chain: the outer `<<` value holds every other
                    // level, each of its events buffered once.
                    let keys = 50usize;
                    let chain = merge_chain(keys, depth, false);
                    let events = 2 + 2 * keys + 3 * (depth - 1);
                    assert_eq!(planned(&chain, &merge), events, "chain {depth}");
                }
                // Without merge keys or LastWins, nothing is planned.
                assert_eq!(planned(&nested(width, 60), &Settings::new()), 0);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
