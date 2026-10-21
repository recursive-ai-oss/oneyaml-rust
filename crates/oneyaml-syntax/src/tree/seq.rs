//! The order of an edited tree's elements: a treap of **runs**, each a
//! contiguous preorder range of one arena, with subtree sums of element
//! counts and token bytes (rank and offset of any run in O(log R)
//! expected, R = number of runs, which grows with the number of edits and
//! never with the document). Every operation is a loop; nothing
//! recurses.

use super::runs::{LineState, Marks};
use super::{NONE, cost};

/// A contiguous local range `a..b` of arena `arena`, in the tree's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Run {
    /// 0 for the base tree, else the inserted arena's first slot (its key
    /// in the overlay: arenas are dropped when no run covers
    /// them, so they are not numbered by position).
    pub(super) arena: u32,
    /// The arena's index in the overlay's slab (`NONE`
    /// for the base), so a read reaches the arena in O(1), never through
    /// a map. (Held in what was padding: the item stays 128 bytes.)
    pub(super) graft: u32,
    pub(super) a: u32,
    pub(super) b: u32,
    /// Bytes of the run's tokens.
    pub(super) bytes: u64,
    /// Diagnostics of its arena attached to elements `a..b`.
    /// Scans for diagnostics skip the runs without any in
    /// O(log R), so their cost does not grow with the edit history).
    pub(super) diags: u64,
    /// Every element of the run is a stream-level trivia token:
    /// the runs that are not are counted in the
    /// treap, so a walk over stream-level trivia jumps over any number of
    /// such runs in O(log R), whatever edits cut them into.
    pub(super) trivia: bool,
    /// What the run holds for the comment-run (`runs.rs`)
    /// index — a content token, its stream-level documents, its line
    /// summary (how it moves the column, and its own-line
    /// comments' columns as a function of the text before it).
    pub(super) marks: Marks,
}

/// What a subtree sum of the treap counts.
#[derive(Clone, Copy)]
enum Mark {
    /// Runs holding diagnostics (their count).
    Diags,
    /// Runs that are not all stream-level trivia.
    Solid,
    /// Runs holding a content token.
    Visible,
    /// Runs holding a stream-level document.
    Docs,
    /// Runs holding an element that is not a trivia token.
    Firm,
}

/// A run as the reads see it: its treap item, its arena (first
/// slot and slab index) and its local range `a..b`.
#[derive(Clone, Copy, Debug)]
pub(super) struct RunAt {
    pub(super) item: u32,
    pub(super) arena: u32,
    pub(super) graft: u32,
    pub(super) a: u32,
    pub(super) b: u32,
}

impl Run {
    fn count(&self) -> u64 {
        u64::from(self.b.saturating_sub(self.a))
    }
}

#[derive(Clone, Debug)]
struct Item {
    left: u32,
    right: u32,
    parent: u32,
    prio: u32,
    run: Run,
    /// Sums over the item's subtree.
    count: u64,
    bytes: u64,
    diags: u64,
    /// Runs of the subtree that are not all stream-level trivia.
    solid: u64,
    /// The marks of the subtree's runs in order, composed (see
    /// `runs.rs`): whether one holds a content token or a firm element,
    /// the documents they hold, their line summary.
    agg: Marks,
}

/// The runs in order (an implicit treap with parent links).
#[derive(Clone, Debug)]
pub(super) struct Seq {
    items: Vec<Item>,
    root: u32,
    free: Vec<u32>,
    rng: u64,
    /// Most items ever allocated (`NONE` in use; lowered by tests to
    /// exercise running out).
    cap: u32,
}

/// A per-tree seed for the priorities (xorshift needs it non-zero).
fn seed() -> u64 {
    // The step-count tests compare counts between runs, so with the
    // internal checks on (test tooling, never a release) every tree gets
    // the same shape.
    #[cfg(any(test, feature = "__internal-checks"))]
    {
        0x9e37_79b9_7f4a_7c15
    }
    #[cfg(not(any(test, feature = "__internal-checks")))]
    {
        use std::hash::{BuildHasher, RandomState};
        RandomState::new().hash_one(0x5eed_u64) | 1
    }
}

impl Seq {
    /// A sequence holding one run.
    pub(super) fn new(run: Run) -> Self {
        let mut s = Self {
            items: Vec::new(),
            root: NONE,
            free: Vec::new(),
            // Seeded per tree from `RandomState`, never a
            // fixed constant input could be shaped against. The priorities
            // only balance the treap: text, events and diagnostics are the
            // runs' order, whatever the shape.
            rng: seed(),
            cap: NONE,
        };
        let _ = s.insert_after(None, run);
        s
    }

    /// `true` when `n` more runs can be inserted.
    pub(super) fn can_insert(&self, n: usize) -> bool {
        let fresh = (self.cap as usize).saturating_sub(self.items.len());
        self.free.len().saturating_add(fresh) >= n
    }

    /// Items allocated so far (live or free): what
    /// [`Seq::remap_grafts`] visits.
    pub(super) fn items_len(&self) -> usize {
        self.items.len()
    }

    /// Heap bytes the sequence holds (for the retention tests).
    #[cfg(test)]
    pub(super) fn heap_bytes(&self) -> usize {
        self.items
            .capacity()
            .saturating_mul(core::mem::size_of::<Item>())
            .saturating_add(self.free.capacity().saturating_mul(4))
    }

    /// Lowers the item space to `cap` items.
    #[cfg(test)]
    pub(super) fn limit_items(&mut self, cap: usize) {
        self.cap = u32::try_from(cap).unwrap_or(NONE);
    }

    fn it(&self, x: u32) -> Option<&Item> {
        self.items.get(x as usize)
    }

    fn it_mut(&mut self, x: u32) -> Option<&mut Item> {
        self.items.get_mut(x as usize)
    }

    fn left(&self, x: u32) -> u32 {
        self.it(x).map_or(NONE, |i| i.left)
    }

    fn right(&self, x: u32) -> u32 {
        self.it(x).map_or(NONE, |i| i.right)
    }

    fn parent(&self, x: u32) -> u32 {
        self.it(x).map_or(NONE, |i| i.parent)
    }

    fn sums(&self, x: u32) -> (u64, u64) {
        self.it(x).map_or((0, 0), |i| (i.count, i.bytes))
    }

    /// The subtree sum `mark` counts for item `x` (0 for none).
    fn mark_sum(&self, x: u32, mark: Mark) -> u64 {
        self.it(x).map_or(0, |i| match mark {
            Mark::Diags => i.diags,
            Mark::Solid => i.solid,
            Mark::Visible => u64::from(i.agg.vis),
            Mark::Docs => u64::from(i.agg.docs),
            Mark::Firm => u64::from(i.agg.firm),
        })
    }

    /// `mark` counts item `x`'s own run.
    fn own_mark(&self, x: u32, mark: Mark) -> bool {
        self.it(x).is_some_and(|i| match mark {
            Mark::Diags => i.run.diags > 0,
            Mark::Solid => !i.run.trivia,
            Mark::Visible => i.run.marks.vis,
            Mark::Docs => i.run.marks.docs,
            Mark::Firm => i.run.marks.firm,
        })
    }

    /// The first run of `x`'s subtree that `mark` counts. O(height).
    fn first_marked(&self, mut x: u32, mark: Mark) -> Option<u32> {
        let mut guard = self.items.len();
        while x != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.mark_sum(self.left(x), mark) > 0 {
                x = self.left(x);
            } else if self.own_mark(x, mark) {
                return Some(x);
            } else if self.mark_sum(self.right(x), mark) > 0 {
                x = self.right(x);
            } else {
                return None;
            }
        }
        None
    }

    /// The last run of `x`'s subtree that `mark` counts. O(height).
    fn last_marked(&self, mut x: u32, mark: Mark) -> Option<u32> {
        let mut guard = self.items.len();
        while x != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.mark_sum(self.right(x), mark) > 0 {
                x = self.right(x);
            } else if self.own_mark(x, mark) {
                return Some(x);
            } else if self.mark_sum(self.left(x), mark) > 0 {
                x = self.left(x);
            } else {
                return None;
            }
        }
        None
    }

    /// The first run after `x` that `mark` counts: the runs it does not
    /// count are skipped by their subtree sums, O(height) rather than
    /// O(runs passed).
    fn next_marked(&self, x: u32, mark: Mark) -> Option<u32> {
        if let Some(found) = self.first_marked(self.right(x), mark) {
            return Some(found);
        }
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.left(p) == at {
                if self.own_mark(p, mark) {
                    return Some(p);
                }
                if let Some(found) = self.first_marked(self.right(p), mark) {
                    return Some(found);
                }
            }
            at = p;
            p = self.parent(at);
        }
        None
    }

    /// The last run before `x` that `mark` counts. O(height).
    fn prev_marked(&self, x: u32, mark: Mark) -> Option<u32> {
        if let Some(found) = self.last_marked(self.left(x), mark) {
            return Some(found);
        }
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.right(p) == at {
                if self.own_mark(p, mark) {
                    return Some(p);
                }
                if let Some(found) = self.last_marked(self.left(p), mark) {
                    return Some(found);
                }
            }
            at = p;
            p = self.parent(at);
        }
        None
    }

    /// The first run after `x` holding a diagnostic (the
    /// runs without any are skipped by their subtree sums, O(height)
    /// rather than O(runs passed)).
    pub(super) fn next_with_diags(&self, x: u32) -> Option<u32> {
        self.next_marked(x, Mark::Diags)
    }

    /// The last run before `x` holding a diagnostic. O(height).
    pub(super) fn prev_with_diags(&self, x: u32) -> Option<u32> {
        self.prev_marked(x, Mark::Diags)
    }

    /// The first run after `x` that is not all stream-level trivia.
    /// O(height).
    pub(super) fn next_solid(&self, x: u32) -> Option<u32> {
        self.next_marked(x, Mark::Solid)
    }

    /// The last run before `x` that is not all stream-level trivia.
    /// O(height).
    pub(super) fn prev_solid(&self, x: u32) -> Option<u32> {
        self.prev_marked(x, Mark::Solid)
    }

    /// The last run before `x` holding a content token.
    /// O(height).
    pub(super) fn prev_visible(&self, x: u32) -> Option<u32> {
        self.prev_marked(x, Mark::Visible)
    }

    /// The nearest run after (`forward`) or before `x` holding an element
    /// that is not a trivia token. O(height).
    pub(super) fn near_firm(&self, x: u32, forward: bool) -> Option<u32> {
        if forward {
            self.next_marked(x, Mark::Firm)
        } else {
            self.prev_marked(x, Mark::Firm)
        }
    }

    /// The last run before `x` holding a stream-level document.
    /// O(height).
    pub(super) fn prev_docs(&self, x: u32) -> Option<u32> {
        self.prev_marked(x, Mark::Docs)
    }

    /// The first run after `x` holding a stream-level document.
    /// O(height).
    pub(super) fn next_docs(&self, x: u32) -> Option<u32> {
        self.next_marked(x, Mark::Docs)
    }

    /// The composed marks of `x`'s subtree ([`Marks::NONE`] for none).
    fn agg(&self, x: u32) -> Marks {
        self.it(x).map_or(Marks::NONE, |i| i.agg)
    }

    /// `x`'s own run's marks.
    fn own(&self, x: u32) -> Marks {
        self.it(x).map_or(Marks::NONE, |i| i.run.marks)
    }

    /// The line state at the end of `x`'s run: the marks of
    /// every run up to it, composed along the path to the root. O(height).
    fn state_after(&self, x: u32) -> LineState {
        let mut m = self.agg(self.left(x)).then(self.own(x));
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.right(p) == at {
                m = self.agg(self.left(p)).then(self.own(p)).then(m);
            }
            at = p;
            p = self.parent(at);
        }
        m.after(LineState::START)
    }

    /// The first run of `x`'s subtree that, after a text ending in state
    /// `s`, holds a content token or an own-line comment at a column below
    /// `v`; else the state after the subtree. O(height).
    fn first_hit(&self, mut x: u32, mut s: LineState, v: u32) -> Result<u32, LineState> {
        if x == NONE || !self.agg(x).hits(s, v) {
            return Err(self.agg(x).after(s));
        }
        let mut guard = self.items.len();
        // A loop down the treap. (The sums are exact for the trees
        // the parser and validated edits build; for any other token
        // sequence they are lower bounds, and a run they lead to without
        // a hit is a candidate the caller reads and passes.)
        while guard > 0 {
            guard = guard.saturating_sub(1);
            let l = self.left(x);
            if l != NONE && self.agg(l).hits(s, v) {
                x = l;
                continue;
            }
            s = self.agg(l).after(s);
            if self.own(x).hits(s, v) {
                return Ok(x);
            }
            s = self.own(x).after(s);
            let r = self.right(x);
            if r == NONE {
                return Ok(x);
            }
            x = r;
        }
        Ok(x)
    }

    /// The first run after `x` holding a content token or an own-line
    /// comment whose column is below `v` (the columns read from
    /// the line summaries with the state of the text before each run, so
    /// the runs in between are skipped by the subtree sums whatever the
    /// edit history cut them into). O(height).
    pub(super) fn next_low(&self, x: u32, v: u32) -> Option<u32> {
        let mut s = self.state_after(x);
        match self.first_hit(self.right(x), s, v) {
            Ok(found) => return Some(found),
            Err(after) => s = after,
        }
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.left(p) == at {
                if self.own(p).hits(s, v) {
                    return Some(p);
                }
                s = self.own(p).after(s);
                match self.first_hit(self.right(p), s, v) {
                    Ok(found) => return Some(found),
                    Err(after) => s = after,
                }
            }
            at = p;
            p = self.parent(at);
        }
        None
    }

    pub(super) fn run(&self, x: u32) -> Option<Run> {
        self.it(x).map(|i| i.run)
    }

    /// `(arena, a, b)` of item `x`'s run (what a cursor needs,
    /// without copying the run).
    #[inline]
    pub(super) fn bounds(&self, x: u32) -> Option<(u32, u32, u32)> {
        self.it(x).map(|i| (i.run.arena, i.run.a, i.run.b))
    }

    /// What a read needs of item `x`'s run, without copying its
    /// sums.
    #[inline]
    pub(super) fn at(&self, x: u32) -> Option<RunAt> {
        self.it(x).map(|i| RunAt {
            item: x,
            arena: i.run.arena,
            graft: i.run.graft,
            a: i.run.a,
            b: i.run.b,
        })
    }

    /// Renumbers every run's arena index through `map` (old
    /// index → new) after the overlay compacted its slab of arenas.
    /// O(items), paid for by the frees that trigger a compaction (see
    /// `Edits::drop_graft`); the order and the sums are unchanged (the
    /// index is not summed).
    pub(super) fn remap_grafts(&mut self, map: &[u32]) {
        for i in &mut self.items {
            cost::count(cost::Step::Visit);
            if let Some(&n) = map.get(i.run.graft as usize) {
                i.run.graft = n;
            }
        }
    }

    /// Element count and token bytes of the whole sequence.
    pub(super) fn total(&self) -> (u64, u64) {
        self.sums(self.root)
    }

    /// Element count and token bytes of the runs before `x`. O(height).
    pub(super) fn prefix(&self, x: u32) -> (u64, u64) {
        cost::count(cost::Step::Lookup);
        let (mut c, mut b) = self.sums(self.left(x));
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            if self.right(p) == at {
                let (lc, lb) = self.sums(self.left(p));
                let own = self.it(p).map_or((0, 0), |i| (i.run.count(), i.run.bytes));
                c = c.saturating_add(lc).saturating_add(own.0);
                b = b.saturating_add(lb).saturating_add(own.1);
            }
            at = p;
            p = self.parent(at);
        }
        (c, b)
    }

    /// The run holding byte `off` of the whole text, and the offset within
    /// that run's bytes (runs without bytes are skipped). O(height).
    pub(super) fn find_byte(&self, mut off: u64) -> Option<(u32, u64)> {
        cost::count(cost::Step::Lookup);
        let mut x = self.root;
        let mut guard = self.items.len();
        while x != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            let (_, lb) = self.sums(self.left(x));
            if off < lb {
                x = self.left(x);
                continue;
            }
            off = off.saturating_sub(lb);
            let own = self.it(x)?.run.bytes;
            if off < own {
                return Some((x, off));
            }
            off = off.saturating_sub(own);
            x = self.right(x);
        }
        None
    }

    fn leftmost(&self, mut x: u32) -> u32 {
        let mut guard = self.items.len();
        while self.left(x) != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            cost::count(cost::Step::Edge);
            x = self.left(x);
        }
        x
    }

    fn rightmost(&self, mut x: u32) -> u32 {
        let mut guard = self.items.len();
        while self.right(x) != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            cost::count(cost::Step::Edge);
            x = self.right(x);
        }
        x
    }

    pub(super) fn first(&self) -> Option<u32> {
        (self.root != NONE).then(|| self.leftmost(self.root))
    }

    pub(super) fn last(&self) -> Option<u32> {
        (self.root != NONE).then(|| self.rightmost(self.root))
    }

    /// The run after `x`.
    pub(super) fn next(&self, x: u32) -> Option<u32> {
        cost::count(cost::Step::Edge);
        if self.right(x) != NONE {
            return Some(self.leftmost(self.right(x)));
        }
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            cost::count(cost::Step::Edge);
            if self.left(p) == at {
                return Some(p);
            }
            at = p;
            p = self.parent(at);
        }
        None
    }

    /// The run before `x`.
    pub(super) fn prev(&self, x: u32) -> Option<u32> {
        cost::count(cost::Step::Edge);
        if self.left(x) != NONE {
            return Some(self.rightmost(self.left(x)));
        }
        let mut at = x;
        let mut p = self.parent(at);
        let mut guard = self.items.len();
        while p != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            cost::count(cost::Step::Edge);
            if self.right(p) == at {
                return Some(p);
            }
            at = p;
            p = self.parent(at);
        }
        None
    }

    /// Recomputes `x`'s sums from its children.
    fn pull(&mut self, x: u32) {
        let (lc, lb) = self.sums(self.left(x));
        let (rc, rb) = self.sums(self.right(x));
        let ld = self.mark_sum(self.left(x), Mark::Diags);
        let rd = self.mark_sum(self.right(x), Mark::Diags);
        let ls = self.mark_sum(self.left(x), Mark::Solid);
        let rs = self.mark_sum(self.right(x), Mark::Solid);
        let (la, ra) = (self.agg(self.left(x)), self.agg(self.right(x)));
        if let Some(i) = self.it_mut(x) {
            i.agg = la.then(i.run.marks).then(ra);
            i.count = lc.saturating_add(rc).saturating_add(i.run.count());
            i.bytes = lb.saturating_add(rb).saturating_add(i.run.bytes);
            i.diags = ld.saturating_add(rd).saturating_add(i.run.diags);
            i.solid = ls
                .saturating_add(rs)
                .saturating_add(u64::from(!i.run.trivia));
        }
    }

    /// Recomputes the sums of `x` and every ancestor.
    fn pull_up(&mut self, mut x: u32) {
        let mut guard = self.items.len();
        while x != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            self.pull(x);
            x = self.parent(x);
        }
    }

    fn set_left(&mut self, x: u32, c: u32) {
        if let Some(i) = self.it_mut(x) {
            i.left = c;
        }
        if let Some(i) = self.it_mut(c) {
            i.parent = x;
        }
    }

    fn set_right(&mut self, x: u32, c: u32) {
        if let Some(i) = self.it_mut(x) {
            i.right = c;
        }
        if let Some(i) = self.it_mut(c) {
            i.parent = x;
        }
    }

    /// Puts `c` where `old` hangs under `p` (or at the root).
    fn replace_child(&mut self, p: u32, old: u32, c: u32) {
        if p == NONE {
            self.root = c;
            if let Some(i) = self.it_mut(c) {
                i.parent = NONE;
            }
        } else if self.left(p) == old {
            self.set_left(p, c);
        } else {
            self.set_right(p, c);
        }
    }

    /// Rotates `x` above its parent.
    fn rotate_up(&mut self, x: u32) {
        let p = self.parent(x);
        if p == NONE {
            return;
        }
        let g = self.parent(p);
        if self.left(p) == x {
            let b = self.right(x);
            self.set_left(p, b);
            if b == NONE {
                if let Some(i) = self.it_mut(p) {
                    i.left = NONE;
                }
            }
            self.replace_child(g, p, x);
            self.set_right(x, p);
        } else {
            let b = self.left(x);
            self.set_right(p, b);
            if b == NONE {
                if let Some(i) = self.it_mut(p) {
                    i.right = NONE;
                }
            }
            self.replace_child(g, p, x);
            self.set_left(x, p);
        }
        self.pull(p);
        self.pull(x);
    }

    fn priority(&mut self) -> u32 {
        // xorshift64*: deterministic, so runs are reproducible.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }

    /// Inserts `run` right after `after` (at the front for `None`) and
    /// returns its item. `None` when the item space is exhausted.
    pub(super) fn insert_after(&mut self, after: Option<u32>, run: Run) -> Option<u32> {
        let prio = self.priority();
        let item = Item {
            left: NONE,
            right: NONE,
            parent: NONE,
            prio,
            run,
            count: run.count(),
            bytes: run.bytes,
            diags: run.diags,
            solid: u64::from(!run.trivia),
            agg: run.marks,
        };
        let n = match self.free.pop() {
            Some(n) => {
                *self.it_mut(n)? = item;
                n
            }
            None => {
                let n = u32::try_from(self.items.len())
                    .ok()
                    .filter(|n| *n != NONE && *n < self.cap)?;
                self.items.push(item);
                n
            }
        };
        if self.root == NONE {
            self.root = n;
            return Some(n);
        }
        match after {
            None => {
                let y = self.leftmost(self.root);
                self.set_left(y, n);
            }
            Some(x) if self.right(x) == NONE => self.set_right(x, n),
            Some(x) => {
                let y = self.leftmost(self.right(x));
                self.set_left(y, n);
            }
        }
        self.pull_up(self.parent(n));
        let mut guard = self.items.len();
        while guard > 0 {
            guard = guard.saturating_sub(1);
            let p = self.parent(n);
            if p == NONE || self.it(p).is_none_or(|i| i.prio >= prio) {
                break;
            }
            self.rotate_up(n);
        }
        Some(n)
    }

    /// Removes item `x`.
    pub(super) fn remove(&mut self, x: u32) {
        let mut guard = self.items.len();
        while self.left(x) != NONE && self.right(x) != NONE && guard > 0 {
            guard = guard.saturating_sub(1);
            let (l, r) = (self.left(x), self.right(x));
            let lp = self.it(l).map_or(0, |i| i.prio);
            let rp = self.it(r).map_or(0, |i| i.prio);
            self.rotate_up(if lp >= rp { l } else { r });
        }
        let child = if self.left(x) != NONE {
            self.left(x)
        } else {
            self.right(x)
        };
        let p = self.parent(x);
        self.replace_child(p, x, child);
        if child == NONE && p != NONE {
            if let Some(i) = self.it_mut(p) {
                if i.left == x {
                    i.left = NONE;
                } else if i.right == x {
                    i.right = NONE;
                }
            }
        }
        self.pull_up(p);
        if let Some(i) = self.it_mut(x) {
            *i = Item {
                left: NONE,
                right: NONE,
                parent: NONE,
                prio: 0,
                run: Run {
                    arena: 0,
                    graft: NONE,
                    a: 0,
                    b: 0,
                    bytes: 0,
                    diags: 0,
                    trivia: true,
                    marks: Marks::NONE,
                },
                count: 0,
                bytes: 0,
                diags: 0,
                solid: 0,
                agg: Marks::NONE,
            };
        }
        self.free.push(x);
    }

    /// Replaces the run of item `x` (same place in the order).
    pub(super) fn set_run(&mut self, x: u32, run: Run) {
        if let Some(i) = self.it_mut(x) {
            i.run = run;
        }
        self.pull_up(x);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]
    use super::*;

    fn run(a: u32, b: u32) -> Run {
        Run {
            arena: 0,
            graft: NONE,
            a,
            b,
            bytes: u64::from(b - a) * 10,
            diags: u64::from(a % 3 == 0) * u64::from(a % 7),
            trivia: a % 4 != 1,
            marks: marks(a),
        }
    }

    /// Marks of the shapes a run can have: content (its line
    /// state set by it, with or without a break), or none (a break sets a
    /// clean line; without one, comments only before it, as `head`).
    fn marks(a: u32) -> Marks {
        let vis = a % 5 == 2;
        let brk = a % 4 != 3;
        let clean = if vis {
            Some(brk && a % 2 == 0)
        } else {
            brk.then_some(true)
        };
        let exact = if !vis && brk { a % 11 + 2 } else { u32::MAX };
        let head = if vis || a % 3 == 0 { u32::MAX } else { a % 5 };
        Marks::for_test(
            vis,
            a % 3 != 0,
            a % 9 == 4,
            (brk, clean, a % 6),
            (exact, head),
        )
    }

    fn order(s: &Seq) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = s.first();
        while let Some(x) = at {
            out.push(s.run(x).unwrap().a);
            at = s.next(x);
        }
        out
    }

    /// The run's arena index lives in what was padding; a treap
    /// item stays 128 bytes.
    #[test]
    fn the_arena_index_costs_no_item_bytes() {
        assert_eq!(core::mem::size_of::<Run>(), 56);
        assert_eq!(core::mem::size_of::<Item>(), 128);
    }

    #[test]
    fn inserts_removes_and_prefix_sums_agree_with_a_vec() {
        let mut s = Seq::new(run(0, 1));
        let mut model: Vec<(u32, u32)> = vec![(0, 0)];
        let mut ids = vec![s.first().unwrap()];
        let mut rng = 7u64;
        for step in 1..600u32 {
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let pick = (rng >> 33) as usize % ids.len();
            if rng % 5 == 0 && ids.len() > 1 {
                s.remove(ids[pick]);
                ids.remove(pick);
                model.remove(pick);
            } else {
                let id = s
                    .insert_after(Some(ids[pick]), run(step, step + 1 + step % 3))
                    .unwrap();
                ids.insert(pick + 1, id);
                model.insert(pick + 1, (step, 0));
            }
            let want: Vec<u32> = model.iter().map(|m| m.0).collect();
            assert_eq!(order(&s), want);
            let mut c = 0u64;
            for (k, id) in ids.iter().enumerate() {
                assert_eq!(s.prefix(*id).0, c, "{k}");
                assert_eq!(s.prefix(*id).1, c * 10);
                c += s.run(*id).unwrap().count();
                if k > 0 {
                    assert_eq!(s.prev(*id), Some(ids[k - 1]));
                }
            }
            assert_eq!(s.total().0, c);
            // The runs holding diagnostics are found by the sums.
            for (k, id) in ids.iter().enumerate() {
                let has = |x: &u32| s.run(*x).unwrap().diags > 0;
                let next = ids.get(k + 1..).unwrap().iter().find(|x| has(x)).copied();
                let prev = ids.get(..k).unwrap().iter().rev().find(|x| has(x)).copied();
                assert_eq!(s.next_with_diags(*id), next, "{k}");
                assert_eq!(s.prev_with_diags(*id), prev, "{k}");
                // And the runs that are not all trivia.
                let solid = |x: &u32| !s.run(*x).unwrap().trivia;
                let next = ids.get(k + 1..).unwrap().iter().find(|x| solid(x)).copied();
                let prev = ids
                    .get(..k)
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|x| solid(x))
                    .copied();
                assert_eq!(s.next_solid(*id), next, "{k}");
                assert_eq!(s.prev_solid(*id), prev, "{k}");
                // Content runs, low columns, documents.
                let m = |x: &u32| s.run(*x).unwrap().marks;
                let prev = ids.get(..k).unwrap().iter().rev().find(|x| m(x).vis);
                assert_eq!(s.prev_visible(*id), prev.copied(), "{k}");
                let docs = |x: &&u32| m(x).docs;
                let prev = ids.get(..k).unwrap().iter().rev().find(docs);
                assert_eq!(s.prev_docs(*id), prev.copied(), "{k}");
                let next = ids.get(k + 1..).unwrap().iter().find(docs);
                assert_eq!(s.next_docs(*id), next.copied(), "{k}");
                let firm = |x: &&u32| m(x).firm;
                let next = ids.get(k + 1..).unwrap().iter().find(firm);
                assert_eq!(s.near_firm(*id, true), next.copied(), "{k}");
                let prev = ids.get(..k).unwrap().iter().rev().find(firm);
                assert_eq!(s.near_firm(*id, false), prev.copied(), "{k}");
                // Hits read with the line state before each run.
                let mut states = Vec::new();
                let mut st = LineState::START;
                for x in &ids {
                    states.push(st);
                    st = m(x).after(st);
                }
                for v in [0, 1, 3, 7, 11, 15] {
                    let next = (k + 1..ids.len())
                        .find(|j| m(&ids[*j]).hits(states[*j], v))
                        .map(|j| ids[j]);
                    assert_eq!(s.next_low(*id, v), next, "{k} {v}");
                }
            }
            // Every byte is found in its run.
            let mut b = 0u64;
            for id in &ids {
                let r = s.run(*id).unwrap();
                for k in [0, r.bytes.saturating_sub(1)] {
                    if r.bytes > 0 {
                        assert_eq!(s.find_byte(b + k), Some((*id, k)));
                    }
                }
                b += r.bytes;
            }
            assert_eq!(s.find_byte(b), None);
        }
        // Insert at the front and update a run in place.
        let f = s.insert_after(None, run(1000, 1002)).unwrap();
        assert_eq!(s.first(), Some(f));
        assert_eq!(s.prefix(f), (0, 0));
        let total = s.total();
        s.set_run(f, run(1000, 1001));
        assert_eq!(s.total().0, total.0 - 1);
        assert_eq!(s.last(), ids.last().copied());
    }
}
