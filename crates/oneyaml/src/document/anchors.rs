//! The anchor and alias index of a [`Document`](super::Document)'s tree
//! (anchors and aliases are edited by token, never by text search; anchor
//! bookkeeping per edit is O(local) with set-based membership; the index
//! is maintained incrementally, not rebuilt per edit).
//!
//! Per name, the anchored nodes and the aliases, each list in document
//! order. Positions are read live (`preorder_index`, O(log R) in an edited
//! tree): edits insert and remove elements but never reorder the ones
//! they keep, so a list sorted once stays sorted, new ids go in at a
//! searched position, and ids an edit removed are cut from the lists of
//! the names it touched. Every query is a search over one name's list.
//!
//! Anchors are per document (§3.2.2.2): queries take the document's
//! position range, so a name used in another document of a stream never
//! matters.
//!
//! **Bound**: a name's list is a
//! sorted `Vec` until it holds more than [`BIG`] (256) ids (one splice is
//! then a memmove of at most 4 KiB), and a [`Chunks`] list from then on
//! until it falls to [`SMALL`] (64) ids, when it is a `Vec` again
//! (hysteresis; the build makes any list of more than [`CAP`] ids a
//! chunked one). A chunked list is a treap of chunks of at most [`CAP`]
//! (128) ids, ordered by rank (in-order position) and searched by live
//! position. An edit touching a name then costs O(log k) positions read
//! and O(log k) treap steps per touched list plus O(the ids it adds or
//! cuts), whatever the name's frequency k: a cut or insertion splices one
//! chunk (≤ 2 KiB) and adds or deletes O(1 + run / [`CHUNK`]) treap nodes.
//! Chunks shorter than [`MIN`] merge with a neighbour when the two fit in
//! one, so a list of k ids holds at most k / 16 + 1 chunks. Measured
//! (release, one alias pushed and removed next to k aliases of one name):
//! 0.015 / 0.017 / 0.021 ms at k = 1k / 100k / 1M, against 0.016 / 0.055
//! / 0.295 ms with the `Vec` splice (`document_scaling.rs`,
//! `many_aliases_of_one_name_cost_log_time`; `tests` below). The anchor
//! checks of a move (`edit.rs`, `check_defines` with a moved entry's
//! runs) read ranks: O(log k) per name the entry defines; recording the
//! j aliases that referred to its anchors in the `Removed` stays O(j).
//!
//! Memory: a `Vec` list holds 16 bytes per id plus `Vec` growth (at most
//! as much again). A chunked list holds, per chunk, a buffer of at most
//! max(2 × its ids, 128) slots of 16 bytes (a grown buffer at most
//! doubled, a cut shrinks one past max(2 × its ids, 64), the last chunk
//! of a build has 128) and one allocation, plus 48 bytes in the arena
//! (`nodes`) and 4 in its free list per chunk slot: the arena keeps the
//! most chunks the list ever had at once (a deleted chunk's slot is
//! reused), and shrinks only when the list falls back to a `Vec`. The
//! build walks the tree once, straight into each name's
//! list: a `Vec` whose buffer becomes the first chunk past 128 ids, then
//! chunks filled in place; one key per name, nothing copied. Measured
//! (bytes per anchor or alias held after the build, and allocated during
//! it; with a plain `Vec` in parentheses): one name used 200,000 times 16.5
//! and 17.0 (21.0 and 43.9), 1M times 16.4 and 16.8 (16.8 and 35.6); 1,000
//! times 18.0 and 20.3 (17.6 and 35.9); 1,000 names used 100 times each
//! 22.7 and 42.3 (22.7 and 46.2). The peak of the build is what it holds
//! plus at most one `nodes` reallocation; it can pass the `Vec`'s by up
//! to about 0.4 bytes per id plus 2 KiB per list when k is just under a
//! power of two (the `Vec` then has no spare capacity, the chunks keep
//! their nodes): 18.1 against 17.6 KB at k = 1,000
//! (`oracles/tests/anchor_index_memory.rs` gates it).
//!
//! Every walk of the treap is a loop over an explicit path; the
//! arena (`Vec` of nodes linked by `u32` slots) makes `Clone`, `Drop` and
//! `Debug` flat. Treap priorities come from a generator seeded by
//! `RandomState` (per process), so an input cannot choose the shape.

use std::collections::{BTreeMap, HashSet};
use std::hash::BuildHasher;

use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind, SyntaxTree};

/// A syntax-tree handle.
type TreeId = oneyaml_syntax::tree::NodeId;

/// Anchor and alias names in a range of siblings (and their subtrees),
/// borrowed from the tree.
#[derive(Debug, Default)]
pub(super) struct Names<'t> {
    /// (name, anchored node), in document order.
    pub(super) anchors: Vec<(&'t str, TreeId)>,
    /// (name, alias node), in document order.
    pub(super) aliases: Vec<(&'t str, TreeId)>,
}

impl<'t> Names<'t> {
    /// The names in the siblings `first..=last`. O(their size); a
    /// flat walk.
    ///
    /// Constant factor: the walk reads each element's
    /// kind only; a node's anchor is read at its `Anchor` token (its
    /// properties come before its content, so the anchored nodes are met
    /// in document order), never by scanning every node's children. A
    /// token among `first..=last` itself belongs to their parent, which
    /// is not in the range.
    pub(super) fn of(first: TreeNode<'t>, last: TreeNode<'t>) -> Self {
        let mut out = Self::default();
        Self::visit(first, last, |alias, name, id| {
            if alias {
                out.aliases.push((name, id));
            } else {
                out.anchors.push((name, id));
            }
        });
        out
    }

    /// The walk of [`Names::of`], handing each name to `f` (`true` for an
    /// alias) instead of collecting them (the index
    /// build keeps no whole-tree list).
    fn visit(first: TreeNode<'t>, last: TreeNode<'t>, mut f: impl FnMut(bool, &'t str, TreeId)) {
        let mut at = Some(first);
        while let Some(s) = at {
            for n in s.descendants() {
                match n.kind() {
                    SyntaxKind::Anchor if n != s => {
                        // What `Node::anchor` reads: the node's first
                        // `Anchor` among its properties.
                        let Some(p) = n.parent() else { continue };
                        let Some(a) = p.anchor() else { continue };
                        if a.span.start == n.span().start {
                            f(false, a.name, p.id());
                        }
                    }
                    SyntaxKind::Alias => {
                        if let Some(a) = n.alias_name() {
                            f(true, a, n.id());
                        }
                    }
                    _ => {}
                }
            }
            if s == last {
                break;
            }
            at = s.next_sibling();
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.anchors.is_empty() && self.aliases.is_empty()
    }
}

// ================================================================ lists

/// A sorted `Vec` longer than this becomes a [`Chunks`] list.
const BIG: usize = 256;
/// A [`Chunks`] list this short becomes a `Vec` again (hysteresis).
const SMALL: usize = 64;
/// The size of the chunks a run of ids is cut into.
const CHUNK: usize = 64;
/// The most ids a chunk holds (an insertion past it splits the chunk).
const CAP: usize = 128;
/// A chunk shorter than this merges with a neighbour when they fit in one.
const MIN: usize = 32;
/// No node.
const NIL: u32 = u32::MAX;

/// A treap node: a chunk of consecutive ids of the list.
#[derive(Clone, Debug)]
struct Chunk {
    ids: Vec<TreeId>,
    left: u32,
    right: u32,
    prio: u32,
    /// Ids in this subtree.
    size: usize,
}

/// A long list (see the module docs): a treap of chunks in an arena,
/// ordered by rank.
#[derive(Clone, Debug)]
struct Chunks {
    nodes: Vec<Chunk>,
    free: Vec<u32>,
    root: u32,
    rng: u64,
    /// An invariant broke (a bug): the owning [`Index`] is poisoned and
    /// the document drops it, to be rebuilt (never a stale answer;
    /// as `lookup.rs` drops a lookup index).
    broken: bool,
}

impl Chunks {
    /// A balanced list of `ids` (in order), in chunks of [`CAP`].
    fn build(ids: &[TreeId]) -> Self {
        let mut out = Self::empty();
        for id in ids {
            out.push_back(*id);
        }
        out.link();
        out
    }

    /// An empty list, to fill with [`Chunks::push_back`] and then
    /// [`Chunks::link`].
    fn empty() -> Self {
        let seed = std::collections::hash_map::RandomState::new().hash_one(0u8);
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NIL,
            rng: seed | 1,
            broken: false,
        }
    }

    /// While building (before [`Chunks::link`]): `id` after the others,
    /// in the last chunk until it holds [`CAP`] (the ids
    /// go straight into their chunks, nothing is copied).
    fn push_back(&mut self, id: TreeId) {
        match self.nodes.last_mut() {
            Some(c) if c.ids.len() < CAP => c.ids.push(id),
            _ => {
                let mut ids = Vec::with_capacity(CAP);
                ids.push(id);
                if self.fresh(ids).is_none() {
                    self.broken = true;
                }
            }
        }
    }

    /// Links the chunks pushed, in order, into a treap. O(chunks);
    /// a Cartesian-tree build with an explicit stack, no recursion.
    fn link(&mut self) {
        let mut stack: Vec<u32> = Vec::new();
        let count = u32::try_from(self.nodes.len()).unwrap_or(NIL);
        for n in 0..count {
            let prio = self.prio(n);
            let mut last = NIL;
            while let Some(&t) = stack.last() {
                if self.prio(t) < prio {
                    last = t;
                    stack.pop();
                } else {
                    break;
                }
            }
            if let Some(c) = self.at_mut(n) {
                c.left = last;
                c.right = NIL;
            }
            if let Some(&t) = stack.last() {
                if let Some(c) = self.at_mut(t) {
                    c.right = n;
                }
            }
            stack.push(n);
        }
        self.root = stack.first().copied().unwrap_or(NIL);
        // Sizes, children first: the reverse of a preorder.
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut todo = vec![self.root];
        while let Some(n) = todo.pop() {
            if let Some(c) = self.at(n) {
                order.push(n);
                todo.push(c.left);
                todo.push(c.right);
            }
        }
        for n in order.into_iter().rev() {
            self.fix(n);
        }
    }

    fn at(&self, n: u32) -> Option<&Chunk> {
        self.nodes.get(usize::try_from(n).ok()?)
    }

    fn at_mut(&mut self, n: u32) -> Option<&mut Chunk> {
        self.nodes.get_mut(usize::try_from(n).ok()?)
    }

    fn size(&self, n: u32) -> usize {
        self.at(n).map_or(0, |c| c.size)
    }

    fn prio(&self, n: u32) -> u32 {
        self.at(n).map_or(0, |c| c.prio)
    }

    fn len(&self) -> usize {
        self.size(self.root)
    }

    /// Recomputes `n`'s subtree size from its children.
    fn fix(&mut self, n: u32) {
        let Some(c) = self.at(n) else { return };
        let (l, r, own) = (c.left, c.right, c.ids.len());
        let size = self
            .size(l)
            .saturating_add(self.size(r))
            .saturating_add(own);
        if let Some(c) = self.at_mut(n) {
            c.size = size;
        }
    }

    /// A new (detached) node holding `ids`, with a fresh priority.
    fn fresh(&mut self, ids: Vec<TreeId>) -> Option<u32> {
        // xorshift64.
        let mut x = self.rng;
        x ^= x.wrapping_shl(13);
        x ^= x.wrapping_shr(7);
        x ^= x.wrapping_shl(17);
        self.rng = x;
        let chunk = Chunk {
            size: ids.len(),
            ids,
            left: NIL,
            right: NIL,
            prio: x.wrapping_shr(32) as u32,
        };
        if let Some(n) = self.free.pop() {
            if let Some(c) = self.at_mut(n) {
                *c = chunk;
                return Some(n);
            }
            return None;
        }
        let n = u32::try_from(self.nodes.len()).ok().filter(|n| *n != NIL)?;
        self.nodes.push(chunk);
        Some(n)
    }

    /// The path from the root to the chunk holding rank `r`, and `r`'s
    /// offset in it. O(log chunks).
    fn find(&self, mut r: usize) -> Option<(Vec<u32>, usize)> {
        let mut path = Vec::new();
        let mut cur = self.root;
        while let Some(c) = self.at(cur) {
            path.push(cur);
            let ls = self.size(c.left);
            if r < ls {
                cur = c.left;
                continue;
            }
            let r2 = r.saturating_sub(ls);
            if r2 < c.ids.len() {
                return Some((path, r2));
            }
            r = r2.saturating_sub(c.ids.len());
            cur = c.right;
        }
        None
    }

    /// Adds `delta` ids to (or, `sub`, takes them from) the sizes along
    /// `path`.
    fn resize(&mut self, path: &[u32], delta: usize, sub: bool) {
        for &n in path {
            if let Some(c) = self.at_mut(n) {
                c.size = if sub {
                    c.size.saturating_sub(delta)
                } else {
                    c.size.saturating_add(delta)
                };
            }
        }
    }

    /// Replaces child `old` of `parent` (or the root) by `new`.
    fn relink(&mut self, parent: Option<u32>, old: u32, new: u32) {
        match parent.and_then(|p| self.at_mut(p)) {
            Some(p) if p.left == old => p.left = new,
            Some(p) => p.right = new,
            None => self.root = new,
        }
    }

    /// Rotates child `x` of `p` above it (its parent's link is the
    /// caller's to update).
    fn rotate(&mut self, p: u32, x: u32) {
        let left = self.at(p).is_some_and(|c| c.left == x);
        if left {
            let b = self.at(x).map_or(NIL, |c| c.right);
            if let Some(c) = self.at_mut(p) {
                c.left = b;
            }
            if let Some(c) = self.at_mut(x) {
                c.right = p;
            }
        } else {
            let b = self.at(x).map_or(NIL, |c| c.left);
            if let Some(c) = self.at_mut(p) {
                c.right = b;
            }
            if let Some(c) = self.at_mut(x) {
                c.left = p;
            }
        }
        self.fix(p);
        self.fix(x);
    }

    /// Inserts a chunk of `ids` so that its first id gets rank `r`, which
    /// must be a chunk boundary. O(log chunks) expected.
    fn insert_chunk(&mut self, mut r: usize, ids: Vec<TreeId>) {
        let len = ids.len();
        let Some(n) = self.fresh(ids) else {
            self.broken = true;
            return;
        };
        let mut path = Vec::new();
        let mut cur = self.root;
        let mut went_left = false;
        while let Some(c) = self.at(cur) {
            path.push(cur);
            let (l, rt, ls, own) = (c.left, c.right, self.size(c.left), c.ids.len());
            if r <= ls {
                went_left = true;
                cur = l;
            } else {
                if r < ls.saturating_add(own) {
                    // Not a chunk boundary: the caller's bug.
                    self.broken = true;
                }
                r = r.saturating_sub(ls.saturating_add(own));
                went_left = false;
                cur = rt;
            }
        }
        self.resize(&path, len, false);
        match path.last().copied().and_then(|p| self.at_mut(p)) {
            Some(p) if went_left => p.left = n,
            Some(p) => p.right = n,
            None => self.root = n,
        }
        // Up while its priority is higher than its parent's.
        let prio = self.prio(n);
        while let Some(&p) = path.last() {
            if self.prio(p) >= prio {
                break;
            }
            path.pop();
            self.rotate(p, n);
            self.relink(path.last().copied(), p, n);
        }
    }

    /// Deletes the last node of `path` (a path from the root) and returns
    /// its ids. O(log chunks) expected.
    fn delete(&mut self, mut path: Vec<u32>) -> Vec<TreeId> {
        let Some(&x) = path.last() else {
            return Vec::new();
        };
        let ids = self
            .at_mut(x)
            .map(|c| std::mem::take(&mut c.ids))
            .unwrap_or_default();
        self.resize(&path, ids.len(), true);
        // Down to a leaf, the child with the higher priority going up.
        while let Some(c) = self.at(x) {
            let (l, r) = (c.left, c.right);
            let up = match (l == NIL, r == NIL) {
                (true, true) => break,
                (true, false) => r,
                (false, true) => l,
                (false, false) if self.prio(l) > self.prio(r) => l,
                (false, false) => r,
            };
            let parent = path.len().checked_sub(2).and_then(|i| path.get(i)).copied();
            self.rotate(x, up);
            self.relink(parent, x, up);
            let at = path.len().saturating_sub(1);
            path.insert(at, up);
        }
        path.pop();
        self.relink(path.last().copied(), x, NIL);
        self.free.push(x);
        ids
    }

    /// The number of ids whose position is below `p`. O(log chunks +
    /// log [`CAP`]) positions read.
    fn lower_bound(&self, tree: &SyntaxTree, p: usize) -> usize {
        let mut acc = 0usize;
        let mut cur = self.root;
        while let Some(c) = self.at(cur) {
            let (Some(first), Some(last)) = (c.ids.first(), c.ids.last()) else {
                // Never: a mutation that would leave an empty chunk deletes
                // it, or poisons the index (`broken`), which the document
                // then drops before any read.
                debug_assert!(false, "an empty chunk");
                return acc;
            };
            let ls = self.size(c.left);
            if pos(tree, *last) < p {
                acc = acc.saturating_add(ls).saturating_add(c.ids.len());
                cur = c.right;
            } else if pos(tree, *first) >= p {
                cur = c.left;
            } else {
                let i = c.ids.partition_point(|x| pos(tree, *x) < p);
                return acc.saturating_add(ls).saturating_add(i);
            }
        }
        acc
    }

    /// The id at rank `r`. O(log chunks), no allocation.
    fn get(&self, mut r: usize) -> Option<TreeId> {
        let mut cur = self.root;
        while let Some(c) = self.at(cur) {
            let ls = self.size(c.left);
            if r < ls {
                cur = c.left;
                continue;
            }
            let r2 = r.saturating_sub(ls);
            if r2 < c.ids.len() {
                return c.ids.get(r2).copied();
            }
            r = r2.saturating_sub(c.ids.len());
            cur = c.right;
        }
        None
    }

    /// The ids at ranks `a..b`, in order. O(log chunks + (b - a));
    /// an in-order walk with an explicit stack, no recursion.
    fn collect(&self, a: usize, b: usize) -> Vec<TreeId> {
        let need = b.saturating_sub(a);
        let mut out = Vec::with_capacity(need.min(self.len()));
        if need == 0 {
            return out;
        }
        // Down to rank `a`, keeping the nodes still to visit.
        let mut pending = Vec::new();
        let mut cur = self.root;
        let mut r = a;
        let mut start = None;
        while let Some(c) = self.at(cur) {
            let ls = self.size(c.left);
            if r < ls {
                pending.push(cur);
                cur = c.left;
                continue;
            }
            let r2 = r.saturating_sub(ls);
            if r2 < c.ids.len() {
                start = Some((cur, r2));
                break;
            }
            r = r2.saturating_sub(c.ids.len());
            cur = c.right;
        }
        let Some((mut node, mut off)) = start else {
            return out;
        };
        while let Some(c) = self.at(node) {
            let take = need.saturating_sub(out.len());
            let end = off.saturating_add(take).min(c.ids.len());
            out.extend_from_slice(c.ids.get(off..end).unwrap_or_default());
            if out.len() >= need {
                break;
            }
            let mut next = c.right;
            while let Some(d) = self.at(next) {
                pending.push(next);
                next = d.left;
            }
            match pending.pop() {
                Some(n) => (node, off) = (n, 0),
                None => break,
            }
        }
        out
    }

    /// Inserts the ids `run` at rank `at` (`at` ≤ len). O(log chunks +
    /// [`CAP`] + run) expected.
    fn insert_run(&mut self, at: usize, run: &[TreeId]) {
        if run.is_empty() {
            return;
        }
        // The chunk of the id before `at` (its end), else of the first.
        let target = match at.checked_sub(1) {
            Some(before) => self.find(before).map(|(p, o)| (p, o.saturating_add(1))),
            None => self.find(0),
        };
        let Some((path, off)) = target else {
            // An empty list.
            for piece in run.chunks(CHUNK) {
                let r = self.len();
                self.insert_chunk(r, piece.to_vec());
            }
            return;
        };
        let Some(&x) = path.last() else { return };
        let old = self.at(x).map_or(0, |c| c.ids.len());
        if old.saturating_add(run.len()) <= CAP {
            if let Some(c) = self.at_mut(x) {
                let off = off.min(c.ids.len());
                c.ids.splice(off..off, run.iter().copied());
            }
            self.resize(&path, run.len(), false);
            return;
        }
        // Too long: the chunk and the run, cut into chunks of at most
        // `CHUNK` (at least `CHUNK / 2`) ids.
        let start = at.saturating_sub(off);
        let mut all = self
            .at_mut(x)
            .map(|c| std::mem::take(&mut c.ids))
            .unwrap_or_default();
        let tail = all.split_off(off.min(all.len()));
        all.extend_from_slice(run);
        all.extend(tail);
        let total = all.len();
        let m = total.div_ceil(CHUNK).max(1);
        let (base, extra) = (
            total.checked_div(m).unwrap_or(0),
            total.checked_rem(m).unwrap_or(0),
        );
        let mut pieces = Vec::with_capacity(m);
        let mut from = 0usize;
        for i in 0..m {
            let n = if i < extra {
                base.saturating_add(1)
            } else {
                base
            };
            let to = from.saturating_add(n).min(total);
            pieces.push(all.get(from..to).unwrap_or_default().to_vec());
            from = to;
        }
        let mut pieces = pieces.into_iter();
        let first = pieces.next().unwrap_or_default();
        let first_len = first.len();
        if let Some(c) = self.at_mut(x) {
            c.ids = first;
        }
        // The path's sizes: `old` ids out, `first_len` in.
        self.resize(&path, old, true);
        self.resize(&path, first_len, false);
        let mut r = start.saturating_add(first_len);
        for p in pieces {
            let len = p.len();
            self.insert_chunk(r, p);
            r = r.saturating_add(len);
        }
    }

    /// Removes the ids at ranks `a..b`. O(log chunks × chunks touched +
    /// (b - a)) expected.
    fn remove_range(&mut self, a: usize, b: usize) {
        let mut left = b.saturating_sub(a);
        while left > 0 {
            let Some((path, off)) = self.find(a) else {
                // Ranks past the end: the index is wrong.
                self.broken = true;
                break;
            };
            let Some(&x) = path.last() else { break };
            let len = self.at(x).map_or(0, |c| c.ids.len());
            let take = left.min(len.saturating_sub(off));
            if take == 0 {
                // An empty chunk, or ranks past the end: the index is wrong.
                self.broken = true;
                break;
            }
            if off == 0 && take == len {
                self.delete(path);
            } else {
                if let Some(c) = self.at_mut(x) {
                    c.ids.drain(off..off.saturating_add(take));
                    // At most twice the ids in capacity, as a `Vec` that
                    // grew.
                    if c.ids.capacity() > c.ids.len().saturating_mul(2).max(CHUNK) {
                        c.ids.shrink_to_fit();
                    }
                }
                self.resize(&path, take, true);
            }
            left = left.saturating_sub(take);
        }
        // The chunks now meeting at `a` may be short.
        self.settle(a);
        if let Some(before) = a.checked_sub(1) {
            self.settle(before);
        }
    }

    /// When the chunk holding rank `r` is shorter than [`MIN`], merges it
    /// with its successor or predecessor if the two fit in one chunk.
    fn settle(&mut self, r: usize) {
        let Some((path, off)) = self.find(r) else {
            return;
        };
        let Some(&x) = path.last() else { return };
        let len = self.at(x).map_or(0, |c| c.ids.len());
        if len >= MIN {
            return;
        }
        let start = r.saturating_sub(off);
        let end = start.saturating_add(len);
        // The successor's ids, appended to this chunk.
        if let Some((spath, _)) = self.find(end) {
            let slen = spath
                .last()
                .and_then(|s| self.at(*s))
                .map_or(0, |c| c.ids.len());
            if len.saturating_add(slen) <= CAP {
                let ids = self.delete(spath);
                if let Some((xpath, _)) = self.find(start) {
                    if let Some(c) = xpath.last().and_then(|x| self.at_mut(*x)) {
                        c.ids.extend_from_slice(&ids);
                    }
                    self.resize(&xpath, ids.len(), false);
                }
                return;
            }
        }
        // Else this chunk's ids, appended to the predecessor.
        if let Some(prev) = start.checked_sub(1) {
            if let Some((ppath, _)) = self.find(prev) {
                let plen = ppath
                    .last()
                    .and_then(|p| self.at(*p))
                    .map_or(0, |c| c.ids.len());
                if plen.saturating_add(len) <= CAP {
                    let ids = self.delete(path);
                    if let Some((ppath, _)) = self.find(prev) {
                        if let Some(c) = ppath.last().and_then(|p| self.at_mut(*p)) {
                            c.ids.extend_from_slice(&ids);
                        }
                        self.resize(&ppath, ids.len(), false);
                    }
                }
            }
        }
    }
}

/// One name's ids, in document order (see the module docs).
#[derive(Clone, Debug)]
enum List {
    /// At most [`BIG`] ids (after a [`Chunks`] list shrank: at most
    /// [`SMALL`]).
    Small(Vec<TreeId>),
    /// More (boxed: a short list's map entry stays a `Vec`'s size).
    Big(Box<Chunks>),
}

impl Default for List {
    fn default() -> Self {
        List::Small(Vec::new())
    }
}

impl List {
    /// While building the index: `id` after the others; [`List::seal`]
    /// then. A `Vec` up to [`CAP`] ids; past that, its buffer becomes the
    /// first chunk (no copy) and the next ids fill chunks in place
    /// (the build holds nothing but the lists it makes).
    fn push_built(&mut self, id: TreeId) {
        match self {
            List::Small(v) if v.len() < CAP => v.push(id),
            List::Small(v) => {
                let mut c = Chunks::empty();
                let first = std::mem::take(v);
                if c.fresh(first).is_none() {
                    c.broken = true;
                }
                c.push_back(id);
                *self = List::Big(Box::new(c));
            }
            List::Big(c) => c.push_back(id),
        }
    }

    /// After the last [`List::push_built`].
    fn seal(&mut self) {
        if let List::Big(c) = self {
            c.link();
        }
    }

    fn from_vec(ids: Vec<TreeId>) -> Self {
        if ids.len() > BIG {
            List::Big(Box::new(Chunks::build(&ids)))
        } else {
            List::Small(ids)
        }
    }

    fn len(&self) -> usize {
        match self {
            List::Small(v) => v.len(),
            List::Big(c) => c.len(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// An invariant of a chunked list broke (see [`Chunks::broken`]).
    fn broken(&self) -> bool {
        matches!(self, List::Big(c) if c.broken)
    }

    /// The number of ids whose position is below `p`.
    fn lower_bound(&self, tree: &SyntaxTree, p: usize) -> usize {
        match self {
            List::Small(v) => v.partition_point(|x| pos(tree, *x) < p),
            List::Big(c) => c.lower_bound(tree, p),
        }
    }

    /// The ids at ranks `a..b`.
    fn collect(&self, a: usize, b: usize) -> Vec<TreeId> {
        match self {
            List::Small(v) => v.get(a..b.max(a)).unwrap_or_default().to_vec(),
            List::Big(c) => c.collect(a, b),
        }
    }

    /// The id at rank `i`.
    fn get(&self, i: usize) -> Option<TreeId> {
        match self {
            List::Small(v) => v.get(i).copied(),
            List::Big(c) => c.get(i),
        }
    }

    fn to_vec(&self) -> Vec<TreeId> {
        self.collect(0, self.len())
    }

    /// Whether the ids at ranks `lo..` are exactly `run`.
    fn holds_at(&self, lo: usize, run: &[TreeId]) -> bool {
        let hi = lo.saturating_add(run.len());
        match self {
            List::Small(v) => v.get(lo..hi) == Some(run),
            List::Big(c) => hi <= c.len() && c.collect(lo, hi) == run,
        }
    }

    fn insert_run(&mut self, at: usize, run: &[TreeId]) {
        match self {
            List::Small(v) => {
                let at = at.min(v.len());
                v.splice(at..at, run.iter().copied());
                if v.len() > BIG {
                    *self = List::Big(Box::new(Chunks::build(v)));
                }
            }
            List::Big(c) => c.insert_run(at, run),
        }
    }

    fn remove_range(&mut self, a: usize, b: usize) {
        match self {
            List::Small(v) => {
                let b = b.min(v.len());
                v.drain(a.min(b)..b);
            }
            List::Big(c) => {
                c.remove_range(a, b);
                if c.len() <= SMALL {
                    *self = List::Small(c.collect(0, c.len()));
                }
            }
        }
    }
}

/// Per name, the ids in document order.
type Lists = BTreeMap<String, List>;

/// The index (see the module docs). `BTreeMap`s, for a later `no_std` flag.
/// `Clone` for a forked tree: its handles are the fork's too
/// (`SyntaxTree::fork`).
#[derive(Clone, Debug, Default)]
pub(super) struct Index {
    anchors: Lists,
    aliases: Lists,
    /// A list's invariant broke (a bug): the document drops the index
    /// (`Document::index_check`), to be rebuilt on next use.
    poisoned: bool,
}

/// The live position of `id` (`usize::MAX` for an id no longer in the
/// tree, which exact maintenance never leaves behind).
fn pos(tree: &SyntaxTree, id: TreeId) -> usize {
    tree.get(id).map_or(usize::MAX, TreeNode::preorder_index)
}

/// `list`'s entries grouped by name, each group in document order (the
/// order of `list`): every touched name is handled once per edit,
/// however often it occurs (duplicate names are valid YAML, section
/// 3.2.2.2).
fn by_name<'n>(list: &[(&'n str, TreeId)]) -> BTreeMap<&'n str, Vec<TreeId>> {
    let mut out: BTreeMap<&str, Vec<TreeId>> = BTreeMap::new();
    for (name, id) in list {
        out.entry(name).or_default().push(*id);
    }
    out
}

/// Where the ids `run` (in document order, a contiguous range of the
/// tree) sit in `list`: the rank of the first, when they are exactly the
/// ids from there (always, for exact maintenance). O(log k positions read
/// + run).
fn slice_of(tree: &SyntaxTree, list: &List, run: &[TreeId]) -> Option<usize> {
    let first = *run.first()?;
    let lo = list.lower_bound(tree, pos(tree, first));
    list.holds_at(lo, run).then_some(lo)
}

/// Adds the ids `run` (document order; ids that are one contiguous range
/// of the tree, such as the elements an edit inserted) to `list`, keeping
/// it sorted: one insertion at one searched rank (O(log k
/// positions read + run), see the module docs). Ids already there are not
/// added twice.
fn merge_run(tree: &SyntaxTree, list: &mut List, run: &[TreeId]) {
    let (Some(first), Some(last)) = (run.first(), run.last()) else {
        return;
    };
    let (p0, p1) = (pos(tree, *first), pos(tree, *last));
    let a = list.lower_bound(tree, p0);
    let b = list.lower_bound(tree, p1.saturating_add(1));
    if a == b {
        list.insert_run(a, run);
        return;
    }
    // The run interleaves with (or repeats) ids already listed (never
    // produced by an edit): a linear merge by position, without
    // duplicates.
    let mut old = list.to_vec().into_iter().peekable();
    let mut new = run.iter().copied().peekable();
    let mut out: Vec<TreeId> = Vec::with_capacity(old.len().saturating_add(run.len()));
    loop {
        let next = match (old.peek(), new.peek()) {
            (Some(o), Some(n)) if pos(tree, *n) < pos(tree, *o) => new.next(),
            (Some(_), _) => old.next(),
            (None, Some(_)) => new.next(),
            (None, None) => break,
        };
        if let Some(id) = next {
            if out.last() != Some(&id) {
                out.push(id);
            }
        }
    }
    *list = List::from_vec(out);
}

/// Removes the ids `gone` (document order) from `list`: one range cut
/// when they are the ids at rank `at` (read with the positions of a tree
/// that still holds them), else one pass with set membership (never
/// for exact maintenance).
fn cut(list: &mut List, at: Option<usize>, gone: &[TreeId]) {
    if let Some(lo) = at {
        if list.holds_at(lo, gone) {
            list.remove_range(lo, lo.saturating_add(gone.len()));
            return;
        }
    }
    let set: HashSet<TreeId> = gone.iter().copied().collect();
    let mut ids = list.to_vec();
    ids.retain(|x| !set.contains(x));
    *list = List::from_vec(ids);
}

/// What an edit removes from the index, read from the tree **before** the
/// edit (the removed ids' positions are gone afterwards): per touched
/// name, the ids and where they sit (see [`Index::plan_removal`]).
#[derive(Debug, Default)]
pub(super) struct Removal {
    /// (alias list?, name, ids in document order, their first rank).
    cuts: Vec<(bool, String, Vec<TreeId>, Option<usize>)>,
}

impl Index {
    /// Indexes the whole tree once (O(n)).
    ///
    /// Names are borrowed from the tree while grouping, so the
    /// build allocates per name and per list, never per occurrence.
    ///
    /// One walk straight into each name's list, so the
    /// build holds nothing but the lists it makes (no whole-tree name
    /// list, no per-name `Vec` copied into chunks).
    pub(super) fn build(tree: &SyntaxTree) -> Self {
        let mut anchors = Lists::new();
        let mut aliases = Lists::new();
        Names::visit(tree.root(), tree.root(), |alias, name, id| {
            let m = if alias { &mut aliases } else { &mut anchors };
            // (Looked up by `&str`: a key is allocated once per name.)
            match m.get_mut(name) {
                Some(l) => l.push_built(id),
                None => {
                    let mut l = List::default();
                    l.push_built(id);
                    m.insert(name.to_owned(), l);
                }
            }
        });
        for l in anchors.values_mut().chain(aliases.values_mut()) {
            l.seal();
        }
        let poisoned = anchors.values().chain(aliases.values()).any(List::broken);
        Self {
            anchors,
            aliases,
            poisoned,
        }
    }

    /// Whether a list's invariant broke: the document must drop the
    /// index.
    pub(super) fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// Before an edit that removes the elements holding `names` (a
    /// contiguous range of `tree`): where their ids sit in the lists, one
    /// entry per touched name (names are
    /// deduplicated, O(log k + ids) each).
    pub(super) fn plan_removal(&self, tree: &SyntaxTree, names: &Names<'_>) -> Removal {
        let mut cuts = Vec::new();
        for (alias, lists, of) in [
            (false, &self.anchors, &names.anchors),
            (true, &self.aliases, &names.aliases),
        ] {
            for (name, ids) in by_name(of) {
                let at = lists.get(name).and_then(|l| slice_of(tree, l, &ids));
                cuts.push((alias, name.to_owned(), ids, at));
            }
        }
        Removal { cuts }
    }

    /// After the edit [`Index::plan_removal`] planned was applied: drops
    /// the removed ids (one range cut per touched name).
    pub(super) fn apply_removal(&mut self, removal: Removal) {
        for (alias, name, ids, at) in removal.cuts {
            let lists = if alias {
                &mut self.aliases
            } else {
                &mut self.anchors
            };
            if let Some(list) = lists.get_mut(&name) {
                cut(list, at, &ids);
                self.poisoned |= list.broken();
                if list.is_empty() {
                    lists.remove(&name);
                }
            }
        }
    }

    /// After an edit inserted elements holding `names` (a contiguous range
    /// of `tree`): adds them at their positions, one insertion per touched
    /// name.
    pub(super) fn inserted(&mut self, tree: &SyntaxTree, names: &Names<'_>) {
        for (name, ids) in by_name(&names.anchors) {
            let list = self.anchors.entry(name.to_owned()).or_default();
            merge_run(tree, list, &ids);
            self.poisoned |= list.broken();
        }
        for (name, ids) in by_name(&names.aliases) {
            let list = self.aliases.entry(name.to_owned()).or_default();
            merge_run(tree, list, &ids);
            self.poisoned |= list.broken();
        }
    }

    /// A rename of anchored node `id` from `old` to `new`, with its
    /// aliases `aliases` (document order: the ids of `old`'s alias list
    /// between the node and the next anchor of that name). O(log k +
    /// aliases) positions read, one cut and one insertion per list
    /// (never one list operation per alias).
    pub(super) fn renamed(
        &mut self,
        tree: &SyntaxTree,
        old: &str,
        new: &str,
        id: TreeId,
        aliases: &[TreeId],
    ) {
        self.forget(tree, old, id);
        let list = self.anchors.entry(new.to_owned()).or_default();
        merge_run(tree, list, &[id]);
        self.poisoned |= list.broken();
        if aliases.is_empty() {
            return;
        }
        if let Some(list) = self.aliases.get_mut(old) {
            let at = slice_of(tree, list, aliases);
            cut(list, at, aliases);
            self.poisoned |= list.broken();
            if list.is_empty() {
                self.aliases.remove(old);
            }
        }
        let list = self.aliases.entry(new.to_owned()).or_default();
        merge_run(tree, list, aliases);
        self.poisoned |= list.broken();
    }

    /// The ranks of `list`'s ids with positions in `lo..hi`.
    fn range(tree: &SyntaxTree, list: &List, lo: usize, hi: usize) -> (usize, usize) {
        let a = list.lower_bound(tree, lo);
        let b = list.lower_bound(tree, hi).max(a);
        (a, b)
    }

    /// The first anchored node named `name` at positions `lo..hi`.
    pub(super) fn first_anchor_in(
        &self,
        tree: &SyntaxTree,
        name: &str,
        lo: usize,
        hi: usize,
    ) -> Option<TreeId> {
        let list = self.anchors.get(name)?;
        let (a, b) = Self::range(tree, list, lo, hi);
        (a < b).then(|| list.get(a)).flatten()
    }

    /// The aliases named `name` at positions `lo..hi`, in document order.
    pub(super) fn aliases_in(
        &self,
        tree: &SyntaxTree,
        name: &str,
        lo: usize,
        hi: usize,
    ) -> Vec<TreeId> {
        self.aliases.get(name).map_or_else(Vec::new, |l| {
            let (a, b) = Self::range(tree, l, lo, hi);
            l.collect(a, b)
        })
    }

    /// Whether a move keeps every alias after its anchor, in O(log k): whether
    /// the aliases named `name` at positions `point..end` are exactly the
    /// `count` ids of the list from `run.0` to `run.1` (none when `None`), all
    /// before the first anchor of that name there. Holds for a run of the
    /// list's live ids in document order (what a move records), so first, last
    /// and count decide it.
    pub(super) fn aliases_exactly(
        &self,
        tree: &SyntaxTree,
        name: &str,
        point: usize,
        end: usize,
        run: Option<(TreeId, TreeId)>,
        count: usize,
    ) -> bool {
        let next = self
            .first_anchor_in(tree, name, point, end)
            .map_or(end, |id| pos(tree, id));
        let Some(list) = self.aliases.get(name) else {
            return count == 0 && run.is_none();
        };
        let (a, e) = Self::range(tree, list, point, end);
        let n = list.lower_bound(tree, next).clamp(a, e);
        if n != e || e.saturating_sub(a) != count {
            return false;
        }
        match run {
            None => count == 0,
            Some((f, l)) => {
                list.get(a) == Some(f) && e.checked_sub(1).and_then(|i| list.get(i)) == Some(l)
            }
        }
    }

    /// The anchor an alias at position `at` would refer to: the last one
    /// named `name` before `at`, at or after `from` (the document's start)
    /// (§3.2.2.2).
    pub(super) fn nearest_before(
        &self,
        tree: &SyntaxTree,
        name: &str,
        from: usize,
        at: usize,
    ) -> Option<TreeId> {
        let list = self.anchors.get(name)?;
        let (a, b) = Self::range(tree, list, from, at);
        (a < b).then(|| list.get(b.saturating_sub(1))).flatten()
    }

    /// The position of the first anchor named `name` after position `at`,
    /// or `end`.
    pub(super) fn next_after(&self, tree: &SyntaxTree, name: &str, at: usize, end: usize) -> usize {
        self.first_anchor_in(tree, name, at.saturating_add(1), end)
            .map_or(end, |id| pos(tree, id))
    }

    /// The aliases that refer to anchored node `id` named `name` (at
    /// `at`), in document order, up to position `end` (the document's
    /// end): those after it and before the next anchor of that name.
    pub(super) fn aliases_of(
        &self,
        tree: &SyntaxTree,
        name: &str,
        at: usize,
        end: usize,
    ) -> Vec<TreeId> {
        let next = self.next_after(tree, name, at, end);
        self.aliases_in(tree, name, at.saturating_add(1), next)
    }

    /// An anchored node that keeps its handle (and its place in the tree)
    /// but loses its anchor `name`: O(log k) positions read.
    pub(super) fn forget(&mut self, tree: &SyntaxTree, name: &str, id: TreeId) {
        if let Some(list) = self.anchors.get_mut(name) {
            let at = slice_of(tree, list, &[id]);
            cut(list, at, &[id]);
            self.poisoned |= list.broken();
            if list.is_empty() {
                self.anchors.remove(name);
            }
        }
    }

    /// Whether any anchor named `name` lies at positions `lo..hi`.
    pub(super) fn has_anchor(&self, tree: &SyntaxTree, name: &str, lo: usize, hi: usize) -> bool {
        self.first_anchor_in(tree, name, lo, hi).is_some()
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

    use std::time::{Duration, Instant};

    use super::*;

    /// Chunks in the arena (in use), for tests and the bounds.
    fn chunks(c: &Chunks) -> usize {
        c.nodes.len() - c.free.len()
    }

    /// Checks the treap's invariants (tests).
    fn check(this: &Chunks) {
        let mut todo = vec![this.root];
        let mut seen = 0usize;
        while let Some(n) = todo.pop() {
            let Some(c) = this.at(n) else { continue };
            seen += 1;
            assert!(!c.ids.is_empty(), "empty chunk");
            assert!(c.ids.len() <= CAP, "chunk over CAP");
            assert_eq!(
                c.size,
                this.size(c.left) + this.size(c.right) + c.ids.len(),
                "size"
            );
            for ch in [c.left, c.right] {
                if ch != NIL {
                    assert!(this.prio(ch) <= c.prio, "heap order");
                }
            }
            todo.push(c.left);
            todo.push(c.right);
        }
        assert_eq!(seen, chunks(this), "arena leak");
    }

    /// `anchor: &x 1`, then `k` items `&x i` and `k` aliases `*x`.
    fn tree(k: usize) -> SyntaxTree {
        let mut s = String::from("anchor: &x 0\nsub:\n");
        for i in 0..k {
            s.push_str(&format!("  - &x {i}\n  - *x\n"));
        }
        s.push_str("list:\n");
        for _ in 0..k {
            s.push_str("  - *x\n");
        }
        SyntaxTree::parse(s)
    }

    type Flat = Vec<(String, Vec<TreeId>)>;

    fn lists(ix: &Index) -> (Flat, Flat) {
        let flat = |l: &Lists| {
            l.iter()
                .map(|(k, v)| (k.clone(), v.to_vec()))
                .collect::<Vec<_>>()
        };
        (flat(&ix.anchors), flat(&ix.aliases))
    }

    /// The index operations of one rename with `k` aliases and one
    /// removal + re-insertion of a subtree with `2k` names, back and forth
    /// (the tree is not edited: the index round-trips).
    fn round_trip(t: &SyntaxTree) -> Duration {
        let mut ix = Index::build(t);
        let anchor = ix.anchors.get("x").unwrap().get(0).unwrap();
        let sub = t
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::BlockSequence)
            .unwrap();
        let names = Names::of(sub, sub);
        let list = ix.aliases.get("x").unwrap();
        let aliases: Vec<TreeId> = list.collect(list.len() / 2, list.len());
        let before = lists(&ix);
        let start = Instant::now();
        ix.renamed(t, "x", "y", anchor, &aliases);
        ix.renamed(t, "y", "x", anchor, &aliases);
        let plan = ix.plan_removal(t, &names);
        ix.apply_removal(plan);
        ix.inserted(t, &names);
        let took = start.elapsed();
        assert_eq!(lists(&ix), before);
        took
    }

    /// With one name repeated `k` times, the
    /// index work of a rename and of a subtree removal is linear in `k`
    /// (one cut and one insertion per touched name), never one list
    /// operation per id.
    #[test]
    fn repeated_names_cost_linear_time() {
        let (small, large) = (tree(2_000), tree(20_000));
        let best = |t: &SyntaxTree| (0..3).map(|_| round_trip(t)).min().unwrap();
        let (a, b) = (best(&small), best(&large));
        let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        assert!(
            r < 30.0,
            "index work is not linear: {a:?} vs {b:?} ({r:.1})"
        );
    }

    /// One alias in the middle of `k` aliases of one name,
    /// cut from the index and added back (`plan_removal`, `apply_removal`,
    /// `inserted`), 200 times. With a plain `Vec` each was an O(k)
    /// splice (a memmove of 4 MB at k = 500,000: 204× the cost at k =
    /// 1,000, measured in release); now O(log k) (the module docs; 1.5×).
    #[test]
    fn one_alias_in_a_long_list_costs_log_time() {
        let alias_tree = |k: usize| {
            let mut s = String::from("anchor: &x 0\nlist:\n");
            for _ in 0..k {
                s.push_str("  - *x\n");
            }
            SyntaxTree::parse(s)
        };
        let timed = |t: &SyntaxTree| {
            let mut ix = Index::build(t);
            let list = ix.aliases.get("x").unwrap();
            let mid = list.get(list.len() / 2).unwrap();
            let node = t.get(mid).unwrap();
            let names = Names::of(node, node);
            let before = lists(&ix);
            let mut best = Duration::MAX;
            for _ in 0..3 {
                let start = Instant::now();
                for _ in 0..200 {
                    let plan = ix.plan_removal(t, &names);
                    ix.apply_removal(plan);
                    ix.inserted(t, &names);
                }
                best = best.min(start.elapsed());
            }
            assert_eq!(lists(&ix), before);
            best
        };
        let (small, large) = (alias_tree(1_000), alias_tree(500_000));
        let (a, b) = (timed(&small), timed(&large));
        let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        eprintln!("one alias, 1k vs 500k: {a:?} vs {b:?} (ratio {r:.2})");
        assert!(r < 8.0, "an edit costs more than log k: {r:.2}");
    }

    /// A run that interleaves with listed ids (never produced by an edit)
    /// is merged by position, without duplicates.
    #[test]
    fn merge_run_interleaved_and_repeated() {
        let t = tree(3);
        let ix = Index::build(&t);
        let all = ix.aliases.get("x").unwrap().to_vec();
        assert_eq!(all.len(), 6);
        let mut list = List::from_vec(vec![all[0], all[2], all[4]]);
        merge_run(&t, &mut list, &[all[1], all[2], all[3], all[5]]);
        assert_eq!(list.to_vec(), all);
        let mut list = List::from_vec(all.clone());
        cut(&mut list, None, &[all[1], all[4]]);
        assert_eq!(list.to_vec(), vec![all[0], all[2], all[3], all[5]]);
        cut(&mut list, Some(0), &[all[3]]);
        assert_eq!(list.to_vec(), vec![all[0], all[2], all[5]]);
        let all_list = List::from_vec(all.clone());
        assert_eq!(slice_of(&t, &all_list, &[all[2], all[3]]), Some(2));
        assert_eq!(slice_of(&t, &all_list, &[all[2], all[4]]), None);
    }

    /// The chunked list against a `Vec` model, under random insertions
    /// and range removals of every size (ranks only: the ids are
    /// arbitrary handles), with the treap's invariants checked after each
    /// step; chunks stay at most `len / 16 + 1`.
    #[test]
    fn chunks_match_a_vec_model() {
        let t = tree(1_500);
        let pool: Vec<TreeId> = t.root().descendants().map(|n| n.id()).collect();
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = |m: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % (m as u64).max(1)) as usize
        };
        let mut model: Vec<TreeId> = pool[..600].to_vec();
        let mut c = Chunks::build(&model);
        check(&c);
        let mut fresh = 600usize;
        for step in 0..4_000 {
            let len = model.len();
            if next(3) != 0 || len < 10 {
                // An insertion of 1..=300 ids (runs past CAP included).
                let n = 1 + next(if step % 50 == 0 { 300 } else { 6 });
                let at = next(len + 1);
                let run: Vec<TreeId> = (0..n).map(|i| pool[(fresh + i) % pool.len()]).collect();
                fresh += n;
                model.splice(at..at, run.iter().copied());
                c.insert_run(at, &run);
            } else {
                let a = next(len);
                let n = 1 + next(if step % 40 == 0 { len } else { 8 });
                let b = (a + n).min(len);
                model.drain(a..b);
                c.remove_range(a, b);
            }
            check(&c);
            assert_eq!(c.len(), model.len());
            assert!(chunks(&c) <= model.len() / 16 + 1, "{} chunks", chunks(&c));
            if step % 97 == 0 {
                assert_eq!(c.collect(0, c.len()), model);
                let a = next(model.len() + 1);
                let b = a + next(model.len() + 1 - a);
                assert_eq!(c.collect(a, b), model[a..b]);
            }
        }
        assert_eq!(c.collect(0, c.len()), model);
    }

    /// `lower_bound` of the chunked list is the `Vec`'s partition point,
    /// and the small/big representations switch with hysteresis.
    #[test]
    fn chunked_search_and_representation() {
        let t = tree(800);
        let ix = Index::build(&t);
        let list = ix.aliases.get("x").unwrap();
        assert!(matches!(list, List::Big(_)), "1,600 aliases: chunked");
        let ids = list.to_vec();
        for (i, id) in ids.iter().enumerate().step_by(37) {
            let p = pos(&t, *id);
            assert_eq!(list.lower_bound(&t, p), i);
            assert_eq!(list.lower_bound(&t, p + 1), i + 1);
            assert_eq!(list.get(i), Some(*id));
        }
        assert_eq!(list.lower_bound(&t, 0), 0);
        assert_eq!(list.lower_bound(&t, usize::MAX), ids.len());
        let mut l = List::from_vec(ids[..300].to_vec());
        assert!(matches!(l, List::Big(_)));
        l.remove_range(0, 200);
        assert!(matches!(l, List::Big(_)), "100 ids: still chunked");
        l.remove_range(0, 40);
        assert!(matches!(l, List::Small(_)), "60 ids: a Vec again");
        assert_eq!(l.to_vec(), ids[240..300]);
        l.insert_run(0, &ids[..240]);
        assert_eq!(l.to_vec(), ids[..300]);
        assert!(matches!(l, List::Big(_)));
    }

    /// `Names::of` reads the anchors the nodes report (an anchor token
    /// given as the range itself belongs to its parent, outside it).
    #[test]
    fn names_of_a_range() {
        let t = SyntaxTree::parse("a: &x [&y 1, *x, *y]\nb: &z {c: *z}\n");
        let n = Names::of(t.root(), t.root());
        let a: Vec<&str> = n.anchors.iter().map(|x| x.0).collect();
        let b: Vec<&str> = n.aliases.iter().map(|x| x.0).collect();
        assert_eq!((a, b), (vec!["x", "y", "z"], vec!["x", "y", "z"]));
        for (_, id) in &n.anchors {
            assert!(t.get(*id).unwrap().anchor().is_some());
        }
        let anchor = t
            .root()
            .descendants()
            .find(|x| x.kind() == SyntaxKind::Anchor)
            .unwrap();
        assert!(Names::of(anchor, anchor).is_empty());
    }
}
