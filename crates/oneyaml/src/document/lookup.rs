//! Key and position lookups in **wide** collections in O(log width), never
//! O(width), once the collection is read more than a few times.
//!
//! A mapping or sequence with at least [`WIDE`] YAML nodes gets, on a
//! key- or position-addressed lookup after [`LINEAR_READS`] earlier ones, a
//! **lookup index** ([`CollIndex`]); smaller collections are read linearly,
//! as before (the scan is bounded by the constant), and so is a wide one
//! for its first reads: a scan of w entries costs a fraction of the index
//! build, so a collection read once or twice (one edit does a few lookups)
//! never pays for an index, and one read often pays for it after at most
//! [`LINEAR_READS`] scans. The count of those reads is one small entry per
//! wide collection, dropped with its collection. The index holds:
//!
//! * the collection's YAML nodes in order, in an implicit treap (an
//!   order-statistics tree: k-th item and insertion/removal at a position
//!   in O(log width); a mapping holds keys and values, so entry `i` is
//!   items `2i` and `2i + 1`, exactly as [`MappingRef::iter`] pairs them);
//! * for a mapping, its scalar keys by **meaning**: a keyed digest
//!   (`RandomState`: hash flooding resistance) of the key's resolved
//!   [`Value`], the value the resolver gives it (the same meaning-level
//!   equality the duplicate-key check uses: tags, `!!str` vs plain, numbers
//!   in other radixes). A digest only selects candidates: every lookup
//!   confirms each candidate with the very comparison the linear scan makes,
//!   so a collision costs time, never a wrong answer;
//! * merge keys (`<<` with `merge_keys` on) and **alias keys** apart: an
//!   alias key's meaning is its anchored node's, which edits elsewhere can
//!   change, so alias keys are read live on every lookup (O(alias keys of
//!   that mapping), a documented bound).
//!
//! **Never stale.** Every structural edit of a `Document` goes
//! through `t_remove`/`t_insert`/`t_replace`/`t_remove_to_line_end`
//! (`edit.rs`), which report the siblings that leave and join a collection
//! here: the index of their parent is updated by position (found from the
//! syntax tree's preorder ranks, O(log width · log R)) and checked against
//! the tree (the items at those positions must be the removed ones),
//! anything unexpected drops the index (rebuilt on the next lookup,
//! correct). Token rewrites (anchor renames, comments) change no key's
//! meaning but an alias key's, which is never cached. An edit at document
//! level that touches a directive (which decides how keys resolve) drops
//! every index. An edit group that is rolled back (`Document::atomic`)
//! undoes its index changes from a log, exactly as the syntax layer undoes
//! the tree; indexes built during the group are dropped. Pinned by the
//! edit fuzzes, which compare every collection's index lookups with linear
//! scans after every call (`Document::check_lookup_indexes`).
//!
//! **Memory.** Built only for a wide collection that is looked up, kept in
//! the document's shared state (a fork copies it with the tree, whose
//! handles it keeps), and dropped when its collection is gone (an
//! amortised sweep). Items and keys are held as 4-byte
//! syntax-tree slots ([`Slot`]), not 16-byte handles, and a treap node's
//! priority is derived from its seed and position, not stored: 16 bytes
//! per item plus about 17 per scalar key (a 4-byte digest and an 8-byte
//! list in the hash table); a list of repeats lives apart, in [`Lists`].
//!
//! Every operation is a loop (the treap's splits and merges keep an
//! explicit path).

use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, RandomState};
use std::sync::PoisonError;

use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind, SyntaxTree};

use super::place::Coll;
use super::{Document, NodeRef};
use crate::value::Value;

/// A syntax-tree handle (never in a public signature).
type TreeId = oneyaml_syntax::tree::NodeId;

/// An element of the document's tree by its slot (`Node::slot`,
/// kept small): 4 bytes, stable for the tree's life (edits never reuse a slot),
/// and shared with a fork for the elements before it, as the index is
/// (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Slot(u32);

impl Slot {
    pub(super) fn of(n: TreeNode<'_>) -> Self {
        Self(n.slot())
    }

    /// The live element, or `None` once it is gone.
    pub(super) fn node(self, tree: &SyntaxTree) -> Option<TreeNode<'_>> {
        tree.node_at_slot(self.0)
    }
}

/// The number of YAML nodes from which a collection is indexed (a
/// mapping of 32 entries, a sequence of 64 items): a linear scan of fewer
/// costs a constant.
pub(super) const WIDE: usize = 64;

/// How many reads of a wide collection are answered by the linear scan
/// before its index is built, at most. A scan of small entries costs about
/// a fifth of the build (measured: 8 lookups' scans of 3,700 short keys
/// against one build), so a collection read a few times (one edit does a
/// handful of lookups) never pays for an index. The price of waiting is paid
/// by a collection read just over this many times: its scans and the build
/// together cost about 2.6 times the build alone (measured, short keys);
/// fewer reads cost less than the build, more reads amortise both. Large
/// entries make a scan about as dear as the build, so they get fewer scans
/// ([`linear_reads`]): one for entries of 1 KiB, never fewer than one.
pub(super) const LINEAR_READS: u8 = 8;

/// The scans a wide collection gets before its index, from the source
/// bytes `first` of its first [`WIDE`] YAML nodes: [`LINEAR_READS`] while
/// they average at most 64 bytes, fewer as they grow (one scan reads the
/// text, as the build does), never none.
fn linear_reads(first: usize) -> u8 {
    let allowed = usize::from(LINEAR_READS)
        .saturating_mul(64)
        .saturating_mul(WIDE)
        .checked_div(first.max(1))
        .unwrap_or(1);
    u8::try_from(allowed.clamp(1, usize::from(LINEAR_READS))).unwrap_or(1)
}

// ============================================================== treap

/// "No node" in the treap's arena.
const NIL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
struct TNode {
    item: Slot,
    left: u32,
    right: u32,
    /// Items in this subtree.
    size: u32,
}

/// An implicit treap: a sequence of handles with O(log n) expected k-th
/// item, insertion and removal at a position (priorities are random, from
/// a per-process seeded generator: never chosen by the input). No
/// recursion.
#[derive(Clone, Debug)]
pub(super) struct Treap {
    nodes: Vec<TNode>,
    free: Vec<u32>,
    root: u32,
    /// Node `n`'s max-heap priority is a hash of this seed and `n`
    /// (derived, not stored; random per treap, never chosen by the
    /// input).
    seed: u64,
}

/// A small generator for the treap's priorities (xorshift64), seeded from
/// `RandomState`.
#[derive(Clone, Debug)]
struct Rng(u64);

impl Rng {
    fn seeded() -> Self {
        // The step-count tests compare counts between runs, so with the
        // internal checks on (test tooling, never a release) every index
        // gets the same shape.
        #[cfg(any(test, feature = "__internal-checks"))]
        {
            Self(0x9e37_79b9_7f4a_7c15)
        }
        #[cfg(not(any(test, feature = "__internal-checks")))]
        {
            Self(RandomState::new().hash_one(0x44b_u64) | 1)
        }
    }

    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x.wrapping_shl(13);
        x ^= x.wrapping_shr(7);
        x ^= x.wrapping_shl(17);
        self.0 = x;
        u32::try_from(x.wrapping_shr(32)).unwrap_or(0)
    }

    fn seed(&mut self) -> u64 {
        u64::from(self.next()).wrapping_shl(32) | u64::from(self.next())
    }
}

/// A 64-bit mix (splitmix64's finaliser).
const fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Treap {
    fn new(seed: u64) -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NIL,
            seed,
        }
    }

    /// Node `n`'s priority (see [`Treap::seed`]).
    fn prio(&self, n: u32) -> u32 {
        u32::try_from(mix(self.seed ^ u64::from(n)) >> 32).unwrap_or(0)
    }

    fn at(&self, n: u32) -> Option<&TNode> {
        self.nodes.get(usize::try_from(n).ok()?)
    }

    fn at_mut(&mut self, n: u32) -> Option<&mut TNode> {
        self.nodes.get_mut(usize::try_from(n).ok()?)
    }

    fn size_of(&self, n: u32) -> u32 {
        self.at(n).map_or(0, |x| x.size)
    }

    fn len(&self) -> usize {
        usize::try_from(self.size_of(self.root)).unwrap_or(usize::MAX)
    }

    /// Recomputes `n`'s size from its children.
    fn fix(&mut self, n: u32) {
        let Some(&TNode { left, right, .. }) = self.at(n) else {
            return;
        };
        let size = 1u32
            .saturating_add(self.size_of(left))
            .saturating_add(self.size_of(right));
        if let Some(x) = self.at_mut(n) {
            x.size = size;
        }
    }

    fn alloc(&mut self, item: Slot) -> Option<u32> {
        let node = TNode {
            item,
            left: NIL,
            right: NIL,
            size: 1,
        };
        if let Some(i) = self.free.pop() {
            *self.at_mut(i)? = node;
            return Some(i);
        }
        let i = u32::try_from(self.nodes.len()).ok().filter(|i| *i != NIL)?;
        self.nodes.push(node);
        Some(i)
    }

    /// The treap of `items` in order, in O(n): a Cartesian tree built
    /// along its right spine. A node's subtree is the
    /// items from the first one its left spine took in to the one before
    /// the item that takes it in (or the last), so its size is set when it
    /// leaves the spine, in the same pass (a post-order pass over the tree
    /// cost as much as building it).
    fn build(items: &[Slot], rng: &mut Rng) -> Option<Self> {
        let mut t = Self::new(rng.seed());
        t.nodes.reserve_exact(items.len());
        // (Node, position of the first item of its subtree.)
        let mut spine: Vec<(u32, u32)> = Vec::new();
        for (at, item) in items.iter().enumerate() {
            let at = u32::try_from(at).ok()?;
            let x = t.alloc(*item)?;
            let px = t.prio(x);
            let mut last = NIL;
            let mut first = at;
            while let Some(&(top, top_first)) = spine.last() {
                if t.prio(top) >= px {
                    break;
                }
                t.at_mut(top)?.size = at.checked_sub(top_first)?;
                last = top;
                first = top_first;
                spine.pop();
            }
            t.at_mut(x)?.left = last;
            if let Some(&(top, _)) = spine.last() {
                t.at_mut(top)?.right = x;
            }
            spine.push((x, first));
        }
        let total = u32::try_from(items.len()).ok()?;
        for &(n, first) in &spine {
            t.at_mut(n)?.size = total.checked_sub(first)?;
        }
        t.root = spine.first().map_or(NIL, |s| s.0);
        Some(t)
    }

    /// Item `k` (0-based).
    fn kth(&self, k: usize) -> Option<Slot> {
        let mut k = u32::try_from(k).ok()?;
        let mut t = self.root;
        loop {
            let x = self.at(t)?;
            let ls = self.size_of(x.left);
            match k.cmp(&ls) {
                core::cmp::Ordering::Less => t = x.left,
                core::cmp::Ordering::Equal => return Some(x.item),
                core::cmp::Ordering::Greater => {
                    k = k.checked_sub(ls)?.checked_sub(1)?;
                    t = x.right;
                }
            }
        }
    }

    /// How many items `before` holds for, when it holds for a prefix of
    /// the items (`None` from `before`: unknown, the answer is `None`).
    fn count_before(&self, mut before: impl FnMut(Slot) -> Option<bool>) -> Option<usize> {
        let mut acc = 0u32;
        let mut t = self.root;
        while let Some(x) = self.at(t) {
            if before(x.item)? {
                acc = acc.checked_add(self.size_of(x.left))?.checked_add(1)?;
                t = x.right;
            } else {
                t = x.left;
            }
        }
        usize::try_from(acc).ok()
    }

    /// Splits `t` into its first `k` items and the rest.
    fn split(&mut self, t: u32, k: u32) -> (u32, u32) {
        let (mut lroot, mut rroot) = (NIL, NIL);
        let (mut llast, mut rlast) = (NIL, NIL);
        let mut path = Vec::new();
        let (mut t, mut k) = (t, k);
        while let Some(&x) = self.at(t) {
            path.push(t);
            let ls = self.size_of(x.left);
            if k > ls {
                // `t` and its left subtree go left; its right child is
                // decided further down.
                match self.at_mut(llast) {
                    Some(p) => p.right = t,
                    None => lroot = t,
                }
                llast = t;
                k = k.saturating_sub(ls).saturating_sub(1);
                t = x.right;
            } else {
                match self.at_mut(rlast) {
                    Some(p) => p.left = t,
                    None => rroot = t,
                }
                rlast = t;
                t = x.left;
            }
        }
        if let Some(p) = self.at_mut(llast) {
            p.right = NIL;
        }
        if let Some(p) = self.at_mut(rlast) {
            p.left = NIL;
        }
        // Children are untouched subtrees or nodes later on the path.
        for n in path.into_iter().rev() {
            self.fix(n);
        }
        (lroot, rroot)
    }

    /// `a` then `b`, as one treap.
    fn merge(&mut self, a: u32, b: u32) -> u32 {
        let mut root = NIL;
        // Where the next subtree goes: the root, or a node's right (true)
        // or left (false) child.
        let mut hole: Option<(u32, bool)> = None;
        let mut path = Vec::new();
        let (mut a, mut b) = (a, b);
        loop {
            let (pa, pb) = (
                self.at(a).map(|_| self.prio(a)),
                self.at(b).map(|_| self.prio(b)),
            );
            let (node, next) = match (pa, pb) {
                (None, _) => {
                    self.place(&mut root, hole, b);
                    break;
                }
                (_, None) => {
                    self.place(&mut root, hole, a);
                    break;
                }
                (Some(x), Some(y)) if x > y => {
                    let n = a;
                    a = self.at(a).map_or(NIL, |x| x.right);
                    (n, (n, true))
                }
                _ => {
                    let n = b;
                    b = self.at(b).map_or(NIL, |x| x.left);
                    (n, (n, false))
                }
            };
            self.place(&mut root, hole, node);
            path.push(node);
            hole = Some(next);
        }
        for n in path.into_iter().rev() {
            self.fix(n);
        }
        root
    }

    fn place(&mut self, root: &mut u32, hole: Option<(u32, bool)>, n: u32) {
        match hole {
            None => *root = n,
            Some((p, right)) => {
                if let Some(x) = self.at_mut(p) {
                    if right {
                        x.right = n;
                    } else {
                        x.left = n;
                    }
                }
            }
        }
    }

    /// Inserts `items` (in order) before item `pos`.
    fn insert_at(&mut self, pos: usize, items: &[Slot]) -> Option<()> {
        let pos = u32::try_from(pos).ok()?;
        if pos > self.size_of(self.root) {
            return None;
        }
        let mut mid = NIL;
        for item in items {
            let n = self.alloc(*item)?;
            mid = self.merge(mid, n);
        }
        let (l, r) = self.split(self.root, pos);
        let lm = self.merge(l, mid);
        self.root = self.merge(lm, r);
        Some(())
    }

    /// Removes items `pos..pos + count`, returning them in order.
    fn remove_at(&mut self, pos: usize, count: usize) -> Option<Vec<Slot>> {
        let (p, c) = (u32::try_from(pos).ok()?, u32::try_from(count).ok()?);
        if p.checked_add(c)? > self.size_of(self.root) {
            return None;
        }
        let (l, r) = self.split(self.root, p);
        let (m, r2) = self.split(r, c);
        let out = self.drain(m);
        self.root = self.merge(l, r2);
        Some(out)
    }

    /// The items of subtree `t` in order; its nodes are freed.
    fn drain(&mut self, t: u32) -> Vec<Slot> {
        let out = self.in_order(t);
        let mut stack = vec![t];
        while let Some(n) = stack.pop() {
            if let Some(&TNode { left, right, .. }) = self.at(n) {
                self.free.push(n);
                stack.push(left);
                stack.push(right);
            }
        }
        out
    }

    /// The items of subtree `t` in order (an explicit stack).
    fn in_order(&self, t: u32) -> Vec<Slot> {
        let mut out = Vec::new();
        let mut stack = Vec::new();
        let mut at = t;
        loop {
            while let Some(x) = self.at(at) {
                stack.push(at);
                at = x.left;
            }
            let Some(n) = stack.pop() else { break };
            let Some(x) = self.at(n) else { break };
            out.push(x.item);
            at = x.right;
        }
        out
    }
}

// ======================================================= one collection

/// How a mapping key is read by the lookups (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Class {
    /// Never equal to a `&str` or a scalar `Value` (a collection key, or a
    /// scalar that does not resolve).
    Never,
    /// A merge key (`<<` with `merge_keys` on).
    Merge,
    /// Read live on every lookup (an alias key).
    Live,
    /// A scalar key whose resolved value has this digest.
    Digest(u32),
}

/// What a key is, from `view.rs` (the meaning the resolver gives it).
pub(super) enum KeyKind {
    Never,
    Merge,
    /// An alias key: read live.
    Live,
    Value(Value),
}

/// The keys of one list, **in document order**: a digest's bucket (one
/// key, or many repeats of it under `DuplicateKeys::LastWins`), the merge
/// keys, the alias keys. The governing member of repeats
/// is the last, found in O(log k); a member goes in or out at a position
/// in O(log k) (a treap once there are two). 8 bytes; a list of
/// two or more is a treap in [`Lists`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Members {
    #[default]
    Empty,
    One(Slot),
    /// The list's treap in [`Lists::treaps`].
    Many(u32),
}

/// The lists of two or more members of one index (see [`Members`]), with
/// the room of lists that shrank back to one member, for reuse.
#[derive(Clone, Debug, Default)]
pub(super) struct Lists {
    treaps: Vec<Treap>,
    free: Vec<u32>,
}

impl Lists {
    fn get(&self, i: u32) -> Option<&Treap> {
        self.treaps.get(usize::try_from(i).ok()?)
    }

    fn get_mut(&mut self, i: u32) -> Option<&mut Treap> {
        self.treaps.get_mut(usize::try_from(i).ok()?)
    }

    /// Keeps `t`, returning its place.
    fn add(&mut self, t: Treap) -> Option<u32> {
        if let Some(i) = self.free.pop() {
            *self.get_mut(i)? = t;
            return Some(i);
        }
        let i = u32::try_from(self.treaps.len()).ok()?;
        self.treaps.push(t);
        Some(i)
    }

    /// Frees list `i` (its memory too).
    fn release(&mut self, i: u32) {
        if let Some(t) = self.get_mut(i) {
            *t = Treap::new(0);
            self.free.push(i);
        }
    }
}

impl Members {
    pub(super) fn len(self, lists: &Lists) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Many(i) => lists.get(i).map_or(0, Treap::len),
        }
    }

    pub(super) fn is_empty(self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Member `k` in document order.
    pub(super) fn kth(self, lists: &Lists, k: usize) -> Option<Slot> {
        match self {
            Self::Empty => None,
            Self::One(x) => (k == 0).then_some(x),
            Self::Many(i) => lists.get(i)?.kth(k),
        }
    }

    /// How many members `before` holds for (a prefix).
    fn count_before(
        self,
        lists: &Lists,
        mut before: impl FnMut(Slot) -> Option<bool>,
    ) -> Option<usize> {
        match self {
            Self::Empty => Some(0),
            Self::One(x) => Some(usize::from(before(x)?)),
            Self::Many(i) => lists.get(i)?.count_before(before),
        }
    }

    fn insert(&mut self, lists: &mut Lists, pos: usize, id: Slot, rng: &mut Rng) -> Option<()> {
        match *self {
            Self::Empty if pos == 0 => *self = Self::One(id),
            Self::One(x) => {
                let both = match pos {
                    0 => [id, x],
                    1 => [x, id],
                    _ => return None,
                };
                *self = Self::Many(lists.add(Treap::build(&both, rng)?)?);
            }
            Self::Many(i) => lists.get_mut(i)?.insert_at(pos, &[id])?,
            Self::Empty => return None,
        }
        Some(())
    }

    /// Removes member `pos`, which must be `id`.
    fn remove(&mut self, lists: &mut Lists, pos: usize, id: Slot) -> Option<()> {
        match *self {
            Self::One(x) if pos == 0 && x == id => *self = Self::Empty,
            Self::Many(i) => {
                let t = lists.get_mut(i)?;
                // (Checked first: a wrong position changes nothing.)
                if t.kth(pos)? != id {
                    return None;
                }
                t.remove_at(pos, 1)?;
                if t.len() == 1 {
                    let last = t.kth(0)?;
                    lists.release(i);
                    *self = Self::One(last);
                }
            }
            _ => return None,
        }
        Some(())
    }
}

/// A key that left or joined an index: its handle, class, and position
/// in its list ([`Members`]).
type KeyRec = (Slot, Class, usize);

/// The lookup index of one collection (see the module docs).
#[derive(Clone, Debug)]
pub(super) struct CollIndex {
    /// Which index this is (for the rollback log).
    generation: u64,
    mapping: bool,
    items: Treap,
    hasher: RandomState,
    keys: HashMap<u32, Members>,
    merges: Members,
    live: Members,
    /// The lists of more than one member (apart, so a key's
    /// entry in `keys` is 8 bytes).
    lists: Lists,
}

impl CollIndex {
    /// Entries (mapping) or items (sequence).
    pub(super) fn len(&self) -> usize {
        if self.mapping {
            self.items.len() / 2
        } else {
            self.items.len()
        }
    }

    /// Item `k` of the collection's YAML nodes (keys and values).
    pub(super) fn item(&self, k: usize) -> Option<Slot> {
        self.items.kth(k)
    }

    /// The digest of a key's meaning.
    /// 32 bits of the keyed hash (a digest only picks candidates,
    /// each confirmed; the table entry is then 12 bytes).
    pub(super) fn digest(&self, v: &Value) -> u32 {
        u32::try_from(self.hasher.hash_one(v) >> 32).unwrap_or(0)
    }

    /// The merge keys' list and the alias keys' list.
    pub(super) const fn merge_lists(&self) -> [Members; 2] {
        [self.merges, self.live]
    }

    /// The members of `m`, last first (O(log k) each).
    pub(super) fn rev(&self, m: Members) -> impl Iterator<Item = Slot> + '_ {
        let lists = &self.lists;
        (0..m.len(lists)).rev().filter_map(move |k| m.kth(lists, k))
    }

    /// The scalar keys whose meaning may be `digest`'s, last first.
    pub(super) fn bucket_rev(&self, digest: u32) -> impl Iterator<Item = Slot> + '_ {
        self.rev(self.keys.get(&digest).copied().unwrap_or_default())
    }

    /// Whether the mapping has a merge key (not an alias).
    pub(super) fn has_merges(&self) -> bool {
        !self.merges.is_empty()
    }

    /// The alias keys, last first.
    pub(super) fn live_rev(&self) -> impl Iterator<Item = Slot> + '_ {
        self.rev(self.live)
    }

    fn class(&self, kind: KeyKind) -> Class {
        match kind {
            KeyKind::Never => Class::Never,
            KeyKind::Merge => Class::Merge,
            KeyKind::Live => Class::Live,
            KeyKind::Value(v) => Class::Digest(self.digest(&v)),
        }
    }

    /// The list a key of `class` belongs to (`None`: none; an empty list
    /// for a digest no key has yet).
    fn list(&self, class: Class) -> Option<Members> {
        match class {
            Class::Never => None,
            Class::Merge => Some(self.merges),
            Class::Live => Some(self.live),
            Class::Digest(d) => Some(self.keys.get(&d).copied().unwrap_or_default()),
        }
    }

    /// How many members of `class`'s list `before` holds for.
    fn count_before(
        &self,
        class: Class,
        before: impl FnMut(Slot) -> Option<bool>,
    ) -> Option<usize> {
        match self.list(class) {
            Some(m) => m.count_before(&self.lists, before),
            None => Some(0),
        }
    }

    fn add_key(&mut self, (id, class, pos): KeyRec, rng: &mut Rng) -> Option<()> {
        let lists = &mut self.lists;
        match class {
            Class::Never => Some(()),
            Class::Merge => self.merges.insert(lists, pos, id, rng),
            Class::Live => self.live.insert(lists, pos, id, rng),
            Class::Digest(d) => self.keys.entry(d).or_default().insert(lists, pos, id, rng),
        }
    }

    fn remove_key(&mut self, (id, class, pos): KeyRec) -> Option<()> {
        let lists = &mut self.lists;
        match class {
            Class::Never => Some(()),
            Class::Merge => self.merges.remove(lists, pos, id),
            Class::Live => self.live.remove(lists, pos, id),
            Class::Digest(d) => {
                let list = self.keys.get_mut(&d)?;
                list.remove(lists, pos, id)?;
                if list.is_empty() {
                    self.keys.remove(&d);
                }
                Some(())
            }
        }
    }
}

// ======================================================= the documents'

/// A change of an index inside an open edit group, to undo with it.
#[derive(Clone, Debug)]
enum Op {
    Removed {
        coll: Slot,
        generation: u64,
        pos: usize,
        ids: Vec<Slot>,
        keys: Vec<KeyRec>,
    },
    Inserted {
        coll: Slot,
        generation: u64,
        pos: usize,
        count: usize,
        keys: Vec<KeyRec>,
    },
}

/// Where an edit group started (see [`Indexes::undo`]).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Mark {
    log: usize,
    generation: u64,
}

/// The lookup indexes of one tree's collections (in `Shared`). A `BTreeMap` by
/// handle (for a later `no_std` flag); content-keyed maps use `RandomState`.
#[derive(Clone, Debug)]
pub(super) struct Indexes {
    map: BTreeMap<Slot, CollIndex>,
    hasher: RandomState,
    rng: Rng,
    next_generation: u64,
    log: Vec<Op>,
    /// Edits reported since the last sweep of dead collections.
    touches: usize,
    /// Reads of wide collections that have no index yet (see
    /// [`LINEAR_READS`]): one small entry per such collection, dropped when
    /// its index is built or its collection is gone.
    reads: BTreeMap<Slot, (u8, u8)>,
}

impl Default for Indexes {
    fn default() -> Self {
        Self {
            map: BTreeMap::new(),
            hasher: RandomState::new(),
            rng: Rng::seeded(),
            next_generation: 0,
            log: Vec::new(),
            touches: 0,
            reads: BTreeMap::new(),
        }
    }
}

/// What an edit's removal does to an index (computed before the edit,
/// applied once the syntax layer accepted it).
#[derive(Debug, Default)]
pub(super) enum Pending {
    /// Nothing indexed changes.
    #[default]
    None,
    /// Every index goes (a directive changed).
    DropAll,
    /// This collection's index goes (something unexpected).
    Drop(Slot),
    /// Items `pos..pos + ids.len()` leave `coll`; `keys`: the keys among
    /// them (in document order), as indexed.
    Remove {
        coll: Slot,
        pos: usize,
        ids: Vec<Slot>,
        keys: Vec<KeyRec>,
    },
}

/// The keys and their lists' positions for an insertion (see
/// [`Document::lookup_inserted`]).
type Plan = Result<(Slot, usize, Vec<Slot>, Vec<KeyRec>), Slot>;

impl Indexes {
    pub(super) fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub(super) fn get(&self, coll: Slot) -> Option<&CollIndex> {
        self.map.get(&coll)
    }

    fn drop_all(&mut self) {
        self.map.clear();
        self.reads.clear();
    }

    /// Counts a read of the wide collection `coll`, which has no index:
    /// `true` once it has been read as many times as `limit` allows (set by
    /// its first read), from when its index is worth building (and its count
    /// is dropped).
    fn worth_building(&mut self, coll: Slot, limit: u8) -> bool {
        let (n, allowed) = self.reads.entry(coll).or_insert((0, limit));
        if *n >= *allowed {
            self.reads.remove(&coll);
            return true;
        }
        *n = n.saturating_add(1);
        false
    }

    pub(super) fn drop_one(&mut self, coll: Slot) {
        self.map.remove(&coll);
    }

    /// Builds and keeps the index of `coll` from its YAML nodes `items`
    /// and the kinds of its keys (`keys`, a mapping's, in document order).
    fn install(
        &mut self,
        coll: Slot,
        items: &[Slot],
        keys: Option<Vec<(Slot, KeyKind)>>,
    ) -> Option<()> {
        let mapping = keys.is_some();
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let mut ix = CollIndex {
            generation,
            mapping,
            items: Treap::build(items, &mut self.rng)?,
            hasher: self.hasher.clone(),
            keys: HashMap::new(),
            merges: Members::Empty,
            live: Members::Empty,
            lists: Lists::default(),
        };
        // (Sized once: at most one entry per key.)
        ix.keys.reserve(keys.as_ref().map_or(0, Vec::len));
        for (id, kind) in keys.into_iter().flatten() {
            let class = ix.class(kind);
            // In document order: each joins the end of its list.
            let pos = ix.list(class).map_or(0, |m| m.len(&ix.lists));
            ix.add_key((id, class, pos), &mut self.rng)?;
        }
        self.map.insert(coll, ix);
        Some(())
    }

    /// Drops `coll`'s index, which disagreed with its tree (never a
    /// stale answer). A bug: caught by `debug_assert!` once the index is
    /// gone (never with a lock held).
    fn stale(&mut self, coll: Slot, what: &str) {
        self.drop_one(coll);
        debug_assert!(false, "a lookup index {what}");
    }

    /// Applies `p` after the edit it was planned for; `logging`: inside
    /// an edit group.
    fn removed(&mut self, p: Pending, logging: bool) {
        match p {
            Pending::None => {}
            Pending::DropAll => self.drop_all(),
            Pending::Drop(c) => self.drop_one(c),
            Pending::Remove {
                coll,
                pos,
                ids,
                keys,
            } => {
                let Some(ix) = self.map.get_mut(&coll) else {
                    return;
                };
                let generation = ix.generation;
                // (A replacement's removal may leave a mapping odd until its
                // insertion: the parity is checked by readers.)
                let mut ok = ix
                    .items
                    .remove_at(pos, ids.len())
                    .is_some_and(|out| out == ids);
                // Last first: the earlier positions stay valid.
                for k in keys.iter().rev() {
                    ok = ok && ix.remove_key(*k).is_some();
                }
                if !ok {
                    return self.stale(coll, "lost track of its items");
                }
                if logging {
                    self.log.push(Op::Removed {
                        coll,
                        generation,
                        pos,
                        ids,
                        keys,
                    });
                }
            }
        }
    }

    /// Applies a [`Plan`] after the insertion it was planned for.
    fn inserted(&mut self, plan: Plan, logging: bool) {
        let (coll, pos, ids, keys) = match plan {
            Ok(p) => p,
            Err(coll) => return self.drop_one(coll),
        };
        let Some(ix) = self.map.get_mut(&coll) else {
            return;
        };
        let generation = ix.generation;
        let mut ok = ix.items.insert_at(pos, &ids).is_some();
        for k in &keys {
            ok = ok && ix.add_key(*k, &mut self.rng).is_some();
        }
        if !ok {
            return self.stale(coll, "could not take its new items");
        }
        if logging {
            self.log.push(Op::Inserted {
                coll,
                generation,
                pos,
                count: ids.len(),
                keys,
            });
        }
    }

    /// Where an edit group starts.
    pub(super) fn mark(&self) -> Mark {
        Mark {
            log: self.log.len(),
            generation: self.next_generation,
        }
    }

    /// Undoes the index changes since `m` (a rolled-back edit group: the
    /// syntax layer puts back the same elements, so the positions and
    /// handles of the log are exact), newest first; indexes built since
    /// `m` describe a state the rollback left, and go.
    pub(super) fn undo(&mut self, m: Mark) {
        let ops = self.log.split_off(m.log.min(self.log.len()));
        for op in ops.into_iter().rev() {
            let (coll, generation) = match &op {
                Op::Removed {
                    coll, generation, ..
                }
                | Op::Inserted {
                    coll, generation, ..
                } => (*coll, *generation),
            };
            let Some(ix) = self
                .map
                .get_mut(&coll)
                .filter(|ix| ix.generation == generation)
            else {
                continue;
            };
            let ok = match op {
                Op::Removed { pos, ids, keys, .. } => {
                    let mut ok = ix.items.insert_at(pos, &ids).is_some();
                    for k in keys {
                        ok = ok && ix.add_key(k, &mut self.rng).is_some();
                    }
                    ok
                }
                Op::Inserted {
                    pos, count, keys, ..
                } => {
                    let mut ok = ix.items.remove_at(pos, count).is_some();
                    for k in keys.into_iter().rev() {
                        ok = ok && ix.remove_key(k).is_some();
                    }
                    ok
                }
            };
            if !ok {
                self.map.remove(&coll);
            }
        }
        self.map.retain(|_, ix| ix.generation < m.generation);
    }

    /// The outermost edit group ended: its log is done with; every so
    /// often (amortised O(1) per edit) the indexes of collections that are
    /// gone are dropped.
    pub(super) fn settle(&mut self, live: impl Fn(Slot) -> bool) {
        self.log.clear();
        self.touches = self.touches.saturating_add(1);
        if self.touches > self.map.len().saturating_mul(2).saturating_add(16) {
            self.touches = 0;
            self.map.retain(|id, _| live(*id));
            self.reads.retain(|id, _| live(*id));
        }
    }
}

/// The YAML nodes among the siblings `first..=last` (`None`: `last` was
/// not found after `first`), and whether one of them is a directive.
fn siblings<'t>(first: TreeNode<'t>, last: TreeNode<'t>) -> Option<(Vec<TreeNode<'t>>, bool)> {
    let mut items = Vec::new();
    let mut directive = false;
    let mut at = Some(first);
    while let Some(x) = at {
        directive |= matches!(
            x.kind(),
            SyntaxKind::YamlDirective | SyntaxKind::TagDirective | SyntaxKind::ReservedDirective
        );
        if x.kind().is_yaml_node() {
            items.push(x);
        }
        if x == last {
            return Some((items, directive));
        }
        at = x.next_sibling();
    }
    None
}

/// Whether `coll` is a collection the lookups index, and a mapping.
fn indexable(coll: TreeNode<'_>) -> Option<bool> {
    match Coll::of(coll)? {
        Coll::BlockMap | Coll::FlowMap => Some(true),
        Coll::BlockSeq | Coll::FlowSeq => Some(false),
        Coll::Pair => None,
    }
}

impl Document {
    /// What mapping key `n` is to the index: read as the linear scans
    /// read it, through the node presented in its place (an empty
    /// key right after an error region is that region, which no lookup
    /// matches).
    fn key_kind_of(&self, n: TreeNode<'_>, r: &crate::resolve::Resolver) -> KeyKind {
        super::view::key_kind(NodeRef::new(self, super::view::presented(n)), r)
    }

    /// Where key `n` (live, of `class`) goes in its list of `ix`: after
    /// the members before it in document order. O(log k · log R).
    fn key_position(ix: &CollIndex, n: TreeNode<'_>, class: Class) -> Option<usize> {
        let tree = n.tree();
        let at = n.preorder_index();
        ix.count_before(class, |m| m.node(tree).map(|x| x.preorder_index() < at))
    }

    /// Runs `f` on the lookup index of collection `coll`, building it
    /// first when `coll` is wide ([`WIDE`]) and has been read more than
    /// [`LINEAR_READS`] times (any width, at once, when `force`).
    /// `None`: no index (a narrow collection: read linearly), or `f` found
    /// the index inconsistent with the tree (it is dropped: never a
    /// stale answer; a bug, caught by `debug_assert!`).
    ///
    /// Cost, for a collection of w YAML nodes: O(1) with an index to find
    /// (O(log c) for c indexed collections, plus the lock). Without one:
    /// O(min(w, [`WIDE`])) to see whether it is wide, and for a wide one
    /// the caller's scan, O(w), for its first [`LINEAR_READS`] reads, then
    /// the build, once: O(w) times the work to class a key (O(its text) for
    /// a key whose text settles its meaning, a resolution otherwise)
    /// and O(1) expected per node for the position tree and the digest
    /// table. Retained: 16 bytes per node and about 17 per scalar key for an
    /// index; before it, one 9-byte entry for the collection in the read
    /// count (at most one per wide collection, so one per 64 nodes of text at
    /// most), dropped when the index is built or the collection is gone.
    ///
    /// Also in a document with errors. Its reads go
    /// through the nodes presented in place of the error regions, as the
    /// linear scans do; a collection holding an error region is never
    /// edited (restricted mode refuses edits next to one), so its index
    /// stays as built, and other collections' change as usual.
    pub(super) fn with_lookup<R>(
        &self,
        coll: TreeNode<'_>,
        force: bool,
        f: impl FnOnce(&CollIndex) -> Option<R>,
    ) -> Option<R> {
        let mapping = indexable(coll)?;
        let mut g = self
            .shared
            .lookups
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = Slot::of(coll);
        if g.get(id).is_none() {
            if !force {
                let mut n = 0usize;
                let mut bytes = 0usize;
                let start = coll.span().start;
                for c in coll.children().filter(|c| c.kind().is_yaml_node()) {
                    n = n.saturating_add(1);
                    if n >= WIDE {
                        bytes = c.span().end.saturating_sub(start);
                        break;
                    }
                }
                if n < WIDE || !g.worth_building(id, linear_reads(bytes)) {
                    return None;
                }
            }
            // (a sequence's items are listed as slots only.)
            let items: Vec<TreeNode<'_>> = if mapping {
                coll.children()
                    .filter(|c| c.kind().is_yaml_node())
                    .collect()
            } else {
                Vec::new()
            };
            if mapping && items.len() % 2 != 0 {
                return None;
            }
            let keys = mapping.then(|| {
                let r = NodeRef::new(self, coll).resolver();
                items
                    .iter()
                    .step_by(2)
                    .map(|k| (Slot::of(*k), self.key_kind_of(*k, &r)))
                    .collect()
            });
            let ids: Vec<Slot> = if mapping {
                items.iter().map(|n| Slot::of(*n)).collect()
            } else {
                coll.children()
                    .filter(|c| c.kind().is_yaml_node())
                    .map(Slot::of)
                    .collect()
            };
            g.install(id, &ids, keys)?;
        }
        // A mapping's items pair up (keys and values) whenever it is read.
        let r = g
            .get(id)
            .filter(|ix| !ix.mapping || ix.items.len() % 2 == 0)
            .and_then(f);
        if r.is_none() {
            // (Dropped before the assertion, with the lock released: a
            // debug build never leaves a poisoned lock or a stale index.)
            g.drop_one(id);
            drop(g);
            debug_assert!(false, "a lookup index disagrees with its tree");
        }
        r
    }

    /// Whether collection `coll` has its lookup index now (it is never
    /// built here). O(log c) for c indexed collections, plus the lock.
    pub(super) fn has_lookup_index(&self, coll: TreeNode<'_>) -> bool {
        let g = self
            .shared
            .lookups
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        g.get(Slot::of(coll)).is_some()
    }

    /// Whether `n`, a YAML node child of wide mapping `map`, is at an even
    /// position among its items (a key): from the lookup index, O(log
    /// width · log R). `None` without an index.
    pub(super) fn item_parity(&self, map: TreeNode<'_>, n: TreeNode<'_>) -> Option<bool> {
        if !n.kind().is_yaml_node() || n.parent() != Some(map) {
            return None;
        }
        let tree = n.tree();
        let at = n.preorder_index();
        self.with_lookup(map, false, |ix| {
            let pos = ix
                .items
                .count_before(|it| it.node(tree).map(|x| x.preorder_index() < at))?;
            // (`n` is the item there: a YAML node child of `map`.)
            Some((ix.item(pos) == Some(Slot::of(n))).then_some(pos % 2 == 0))
        })
        .flatten()
    }

    /// Before removing the siblings `a..=b`: what the lookup indexes lose
    /// (O(1) when nothing is indexed; else O(the range's siblings + log
    /// width · log R)).
    pub(super) fn lookup_before(&self, a: TreeId, b: TreeId) -> Pending {
        let g = self
            .shared
            .lookups
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if g.is_empty() {
            return Pending::None;
        }
        let tree = self.tree();
        let (Ok(na), Ok(nb)) = (tree.get(a), tree.get(b)) else {
            return Pending::DropAll;
        };
        let Some(parent) = na.parent() else {
            return Pending::None;
        };
        let indexed = g.get(Slot::of(parent));
        // Directives are children of a document: only there can a range
        // hold one.
        if indexed.is_none() && parent.kind() != SyntaxKind::Document {
            return Pending::None;
        }
        let Some((items, directive)) = siblings(na, nb) else {
            return Pending::DropAll;
        };
        if directive {
            return Pending::DropAll;
        }
        let (Some(ix), Some(first), Some(last)) = (indexed, items.first(), items.last()) else {
            return Pending::None;
        };
        let coll = Slot::of(parent);
        let at = first.preorder_index();
        let pos = ix
            .items
            .count_before(|it| it.node(tree).map(|n| n.preorder_index() < at));
        let Some(pos) = pos else {
            return Pending::Drop(coll);
        };
        // The index must hold these items there (a cheap consistency check).
        let end = pos.saturating_add(items.len()).saturating_sub(1);
        if ix.item(pos) != Some(Slot::of(*first)) || ix.item(end) != Some(Slot::of(*last)) {
            return Pending::Drop(coll);
        }
        let mut keys = Vec::new();
        if ix.mapping {
            let r = NodeRef::new(self, parent).resolver();
            for (j, n) in items.iter().enumerate() {
                if pos.saturating_add(j) % 2 == 0 {
                    let class = ix.class(self.key_kind_of(*n, &r));
                    // (Each against the list before the removal: they go
                    // last first.)
                    let Some(kp) = Self::key_position(ix, *n, class) else {
                        return Pending::Drop(coll);
                    };
                    keys.push((Slot::of(*n), class, kp));
                }
            }
        }
        Pending::Remove {
            coll,
            pos,
            ids: items.iter().map(|n| Slot::of(*n)).collect(),
            keys,
        }
    }

    /// After an accepted removal: applies `p` (from
    /// [`Document::lookup_before`]).
    pub(super) fn lookup_removed(&mut self, p: Pending) {
        if matches!(p, Pending::None) {
            return;
        }
        if let Some(s) = std::sync::Arc::get_mut(&mut self.shared) {
            let logging = s.tree.in_group();
            let g = s.lookups.get_mut().unwrap_or_else(PoisonError::into_inner);
            g.touches = g.touches.saturating_add(1);
            g.removed(p, logging);
        }
    }

    /// After an accepted insertion of the siblings `first..=last`.
    pub(super) fn lookup_inserted(&mut self, first: TreeId, last: TreeId) {
        let plan: Plan = {
            let g = self
                .shared
                .lookups
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if g.is_empty() {
                return;
            }
            let tree = self.tree();
            let (Ok(na), Ok(nb)) = (tree.get(first), tree.get(last)) else {
                drop(g);
                return self.lookup_removed(Pending::DropAll);
            };
            let Some(parent) = na.parent() else { return };
            let indexed = g.get(Slot::of(parent));
            if indexed.is_none() && parent.kind() != SyntaxKind::Document {
                return;
            }
            let Some((items, directive)) = siblings(na, nb) else {
                drop(g);
                return self.lookup_removed(Pending::DropAll);
            };
            if directive {
                drop(g);
                return self.lookup_removed(Pending::DropAll);
            }
            let (Some(ix), Some(first)) = (indexed, items.first()) else {
                return;
            };
            let coll = Slot::of(parent);
            let at = first.preorder_index();
            let pos = ix
                .items
                .count_before(|it| it.node(tree).map(|n| n.preorder_index() < at));
            let planned = pos.and_then(|pos| {
                let mut keys: Vec<KeyRec> = Vec::new();
                if ix.mapping {
                    let r = NodeRef::new(self, parent).resolver();
                    for (j, n) in items.iter().enumerate() {
                        if pos.saturating_add(j) % 2 == 0 {
                            let class = ix.class(self.key_kind_of(*n, &r));
                            // Against the list before the insertion, plus
                            // the new keys of that list before it.
                            let base = Self::key_position(ix, *n, class)?;
                            let earlier = keys.iter().filter(|k| k.1 == class).count();
                            keys.push((Slot::of(*n), class, base.saturating_add(earlier)));
                        }
                    }
                }
                let ids: Vec<Slot> = items.iter().map(|n| Slot::of(*n)).collect();
                Some((coll, pos, ids, keys))
            });
            planned.ok_or(coll)
        };
        if let Some(s) = std::sync::Arc::get_mut(&mut self.shared) {
            let logging = s.tree.in_group();
            let g = s.lookups.get_mut().unwrap_or_else(PoisonError::into_inner);
            g.touches = g.touches.saturating_add(1);
            g.inserted(plan, logging);
        }
    }

    /// Where an edit group starts, for [`Document::lookup_undo`].
    pub(super) fn lookup_mark(&mut self) -> Mark {
        std::sync::Arc::get_mut(&mut self.shared).map_or_else(Mark::default, |s| {
            s.lookups
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .mark()
        })
    }

    /// A rolled-back edit group: its index changes are undone (`ok`: the
    /// syntax layer put the tree back; otherwise every index goes).
    pub(super) fn lookup_undo(&mut self, m: Mark, ok: bool) {
        if let Some(s) = std::sync::Arc::get_mut(&mut self.shared) {
            let g = s.lookups.get_mut().unwrap_or_else(PoisonError::into_inner);
            if ok {
                g.undo(m);
            } else {
                g.log.clear();
                g.drop_all();
            }
        }
    }

    /// The outermost edit group ended (see [`Indexes::settle`]).
    pub(super) fn lookup_settle(&mut self) {
        if let Some(s) = std::sync::Arc::get_mut(&mut self.shared) {
            let tree = &s.tree;
            s.lookups
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .settle(|id| id.node(tree).is_some());
        }
    }

    /// **Test support, not part of the stable API**: only with the
    /// unstable `__internal-checks` feature (semver-exempt, may
    /// change or go in any release). The bytes of key text that key
    /// lookups compared against the strings asked for on this thread since
    /// the last call (decoding is counted apart, by the syntax tree).
    ///
    /// ```
    /// let doc = oneyaml::Document::parse("ab: 1\ncd: 2\n").unwrap();
    /// let _ = oneyaml::Document::take_key_compared_bytes();
    /// assert!(doc.get("cd").is_some());
    /// assert!(oneyaml::Document::take_key_compared_bytes() > 0);
    /// ```
    #[cfg(feature = "__internal-checks")]
    #[doc(hidden)]
    #[must_use]
    pub fn take_key_compared_bytes() -> u64 {
        super::view::take_key_compared()
    }

    /// **Test support, not part of the stable API**: only with the
    /// unstable `__internal-checks` feature (semver-exempt, may
    /// change or go in any release). For the edit fuzzes, which
    /// check that both readers agree: every collection of this document gets a
    /// lookup index (whatever its width), and every lookup through it —
    /// length, each entry or item, each key's own entry and
    /// [`MappingRef::get`](super::MappingRef::get), equal keys by
    /// meaning, merge keys — is compared with the linear scan. The indexes
    /// stay, so the edits that follow maintain them. Returns how many
    /// collections were checked.
    ///
    /// Cost: **O(width²)** per collection (each key's lookup is compared
    /// with a linear scan) and an index for every collection: never call
    /// it on untrusted or large input.
    ///
    /// ```
    /// let doc = oneyaml::Document::parse("a: [1, 2]\nb: {c: 3}\n").unwrap();
    /// assert_eq!(doc.check_lookup_indexes(), Ok(3));
    /// ```
    ///
    /// # Errors
    ///
    /// The first disagreement, described (a bug: the index went stale).
    #[cfg(feature = "__internal-checks")]
    #[doc(hidden)]
    pub fn check_lookup_indexes(&self) -> Result<usize, String> {
        let top = if self.index.is_none() {
            Some(self.tree().root())
        } else {
            self.doc_node()
        };
        let mut checked = 0usize;
        for n in top.into_iter().flat_map(TreeNode::descendants) {
            if indexable(n).is_none() {
                continue;
            }
            self.with_lookup(n, true, |_| Some(()));
            super::view::check_against_linear(NodeRef::new(self, n))
                .map_err(|e| format!("{e} at {:?}: {:?}", n.span(), self.to_string()))?;
            checked = checked.saturating_add(1);
        }
        Ok(checked)
    }
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

    /// Handles to put in a treap (any distinct ones).
    fn ids(n: usize) -> Vec<Slot> {
        let src: String = (0..n).map(|i| format!("- {i}\n")).collect();
        let doc = Document::parse(src).unwrap();
        doc.root()
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .map(|n| Slot::of(n.tree_node()))
            .collect()
    }

    /// The treap against a `Vec` model: build, k-th, insertion and
    /// removal ranges at random positions, prefix counts.
    #[test]
    fn treap_matches_a_vec() {
        let pool = ids(400);
        let mut rng = Rng(0x1234_5678_9abc_def1);
        let mut model: Vec<Slot> = pool[..150].to_vec();
        let mut t = Treap::build(&model, &mut rng).unwrap();
        let mut spare: Vec<Slot> = pool[150..].to_vec();
        for step in 0..2_000u32 {
            assert_eq!(t.len(), model.len(), "{step}");
            let r = rng.next() as usize;
            if r % 3 == 0 && !spare.is_empty() {
                let pos = (rng.next() as usize) % (model.len() + 1);
                let k = 1 + (rng.next() as usize) % 3.min(spare.len());
                let new: Vec<Slot> = spare.drain(..k).collect();
                t.insert_at(pos, &new).unwrap();
                model.splice(pos..pos, new);
            } else if !model.is_empty() {
                let pos = (rng.next() as usize) % model.len();
                let k = 1 + (rng.next() as usize) % 3.min(model.len() - pos);
                let out = t.remove_at(pos, k).unwrap();
                let want: Vec<Slot> = model.drain(pos..pos + k).collect();
                assert_eq!(out, want, "{step}");
                spare.extend(out);
            }
            if step % 97 == 0 {
                assert_eq!(t.in_order(t.root), model, "{step}");
                for (i, id) in model.iter().enumerate() {
                    assert_eq!(t.kth(i), Some(*id));
                    let at = |x: Slot| model.iter().position(|m| *m == x).unwrap();
                    assert_eq!(t.count_before(|x| Some(at(x) < i)), Some(i));
                }
            }
        }
        assert_eq!(t.kth(model.len()), None);
        assert!(t.remove_at(model.len(), 1).is_none());
        assert!(t.insert_at(model.len() + 1, &pool[..1]).is_none());
        assert_eq!(
            t.count_before(|_| None),
            if model.is_empty() { Some(0) } else { None }
        );
        // The arena reuses freed nodes.
        assert!(t.nodes.len() <= 400);
        let empty = Treap::build(&[], &mut rng).unwrap();
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.kth(0), None);
    }

    /// The one-pass build gives every node the size a post-order
    /// pass gives it (1 + its children's), keeps the heap order, and holds
    /// the items in order, for every length up to 300 and long ones.
    #[test]
    fn built_sizes_are_subtree_sizes() {
        let pool = ids(3_000);
        let mut rng = Rng(0x0bad_cafe_f00d_d00d);
        for n in (0..300).chain([1_000, 3_000]) {
            let items = &pool[..n];
            let t = Treap::build(items, &mut rng).unwrap();
            assert_eq!(t.in_order(t.root), items, "{n}");
            assert_eq!(t.len(), n);
            for x in 0..t.nodes.len() as u32 {
                let node = *t.at(x).unwrap();
                let want = 1 + t.size_of(node.left) + t.size_of(node.right);
                assert_eq!(node.size, want, "{n} {x}");
                for c in [node.left, node.right] {
                    if c != NIL {
                        assert!(t.prio(c) <= t.prio(x), "{n} {x}");
                    }
                }
            }
        }
    }

    /// Key lists in document order: one member, many, back to
    /// one, gone; positions are checked (a wrong one is refused); merge
    /// and alias lists; the governing (last) member.
    #[test]
    fn key_lists() {
        let pool = ids(6);
        let mut rng = Rng(99);
        let mut ix = CollIndex {
            generation: 0,
            mapping: true,
            items: Treap::new(1),
            hasher: RandomState::new(),
            keys: HashMap::new(),
            merges: Members::Empty,
            live: Members::Empty,
            lists: Lists::default(),
        };
        let d = ix.digest(&Value::from("a"));
        assert_eq!(ix.class(KeyKind::Value(Value::from("a"))), Class::Digest(d));
        let c = Class::Digest(d);
        let list = |ix: &CollIndex| -> Vec<Slot> { ix.bucket_rev(d).collect() };
        ix.add_key((pool[1], c, 0), &mut rng).unwrap();
        ix.add_key((pool[0], c, 0), &mut rng).unwrap();
        ix.add_key((pool[3], c, 2), &mut rng).unwrap();
        ix.add_key((pool[2], c, 2), &mut rng).unwrap();
        assert_eq!(list(&ix), [pool[3], pool[2], pool[1], pool[0]]);
        assert!(ix.add_key((pool[4], c, 9), &mut rng).is_none());
        assert!(ix.remove_key((pool[1], c, 0)).is_none(), "not there");
        ix.remove_key((pool[1], c, 1)).unwrap();
        ix.remove_key((pool[3], c, 2)).unwrap();
        ix.remove_key((pool[0], c, 0)).unwrap();
        assert_eq!(ix.keys.get(&d), Some(&Members::One(pool[2])));
        assert!(ix.remove_key((pool[0], c, 0)).is_none());
        ix.remove_key((pool[2], c, 0)).unwrap();
        assert!(!ix.keys.contains_key(&d));
        assert!(ix.remove_key((pool[2], c, 0)).is_none());
        let mut e = Members::Empty;
        let mut l = Lists::default();
        assert!(e.insert(&mut l, 1, pool[0], &mut rng).is_none());
        assert!(e.remove(&mut l, 0, pool[0]).is_none());
        e.insert(&mut l, 0, pool[0], &mut rng).unwrap();
        assert!(e.insert(&mut l, 2, pool[1], &mut rng).is_none());
        assert_eq!(e.count_before(&l, |_| Some(true)), Some(1));
        assert_eq!(e.kth(&l, 1), None);
        assert_eq!(Members::Empty.count_before(&l, |_| None), Some(0));
        // A list of repeats is kept apart and its room reused.
        e.insert(&mut l, 1, pool[1], &mut rng).unwrap();
        assert_eq!(l.treaps.len(), 1);
        e.remove(&mut l, 0, pool[0]).unwrap();
        assert_eq!(
            (e, l.free.as_slice()),
            (Members::One(pool[1]), [0].as_slice())
        );
        e.insert(&mut l, 0, pool[0], &mut rng).unwrap();
        assert_eq!((l.treaps.len(), l.free.len()), (1, 0));
        assert_eq!(e.kth(&l, 0), Some(pool[0]));
        for (class, k) in [(Class::Merge, 0), (Class::Live, 1), (Class::Never, 2)] {
            ix.add_key((pool[k], class, 0), &mut rng).unwrap();
        }
        assert_eq!(ix.rev(ix.merge_lists()[0]).collect::<Vec<_>>(), [pool[0]]);
        assert_eq!(ix.live_rev().collect::<Vec<_>>(), [pool[1]]);
        assert_eq!(ix.list(Class::Never), None);
        ix.remove_key((pool[0], Class::Merge, 0)).unwrap();
        ix.remove_key((pool[1], Class::Live, 0)).unwrap();
        ix.remove_key((pool[2], Class::Never, 0)).unwrap();
        assert!(!ix.has_merges() && ix.live_rev().next().is_none());
    }

    /// The check's error arm: an index that went stale (corrupted here by
    /// hand) is reported, never trusted silently.
    #[test]
    fn a_stale_index_is_reported() {
        let src: String = (0..80).map(|i| format!("k{i}: {i}\n")).collect();
        let doc = Document::parse(src).unwrap();
        assert_eq!(doc.check_lookup_indexes(), Ok(1));
        let root = Slot::of(doc.root().unwrap().tree_node());
        {
            let mut g = doc.shared.lookups.lock().unwrap();
            let ix = g.map.get_mut(&root).unwrap();
            // Two items swapped: entry 0's key is now entry 1's.
            let out = ix.items.remove_at(0, 1).unwrap();
            ix.items.insert_at(2, &out).unwrap();
        }
        assert!(doc.check_lookup_indexes().is_err());
    }

    /// The scans a wide collection gets fall with the size of its entries.
    #[test]
    fn linear_reads_fall_with_entry_size() {
        assert_eq!(linear_reads(0), LINEAR_READS);
        assert_eq!(linear_reads(64 * WIDE), LINEAR_READS);
        assert_eq!(linear_reads(128 * WIDE), 4);
        assert_eq!(linear_reads(1_100 * WIDE), 1);
        assert_eq!(linear_reads(usize::MAX), 1);
    }

    /// Reads the root of `doc` until its lookup index stands.
    fn build_index(doc: &Document) {
        for _ in 0..=LINEAR_READS {
            assert!(doc.get("k79").is_some());
        }
        assert!(!doc.shared.lookups.lock().unwrap().is_empty());
    }

    /// An unknown range (handles of another tree) drops every index;
    /// rollback marks of a shared tree are empty.
    #[test]
    fn unknown_ranges_drop_the_indexes() {
        let src: String = (0..80).map(|i| format!("k{i}: {i}\n")).collect();
        let mut doc = Document::parse(src.as_str()).unwrap();
        build_index(&doc);
        let other = Document::parse("a: 1\n").unwrap();
        let foreign = other.root().unwrap().tree_node().id();
        let p = doc.lookup_before(foreign, foreign);
        assert!(matches!(p, Pending::DropAll), "{p:?}");
        doc.lookup_inserted(foreign, foreign);
        assert!(doc.shared.lookups.lock().unwrap().is_empty());
        let copy = doc.clone();
        assert_eq!(doc.lookup_mark().log, 0);
        drop(copy);
        // Undo after a failed rollback drops everything.
        build_index(&doc);
        doc.lookup_undo(Mark::default(), false);
        assert!(doc.shared.lookups.lock().unwrap().is_empty());
    }
}
