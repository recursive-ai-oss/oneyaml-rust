//! The comment-run index (comment rule 13): what the
//! rulebook needs to decide one comment of a long run of comment lines
//! without walking the run.
//!
//! A comment's owner depends on its **run** (the trivia between the
//! content token P before it and the content token N after it) and, by
//! rule 13, on the comments of the run before it: their deepest owner is
//! the owner of the one with the **smallest column** (a comment's rank is
//! a non-increasing function of its column, see `attach.rs`). So the
//! rulebook needs P, N, and "the first own-line comment after a point
//! whose column is below a bound" — each of which a walk over the run
//! answers in O(run). This module answers them in O(log) instead:
//!
//! * **Per arena** (computed once when the arena enters the tree, like the
//!   stream facts of `facts.rs`; arenas are immutable, so this never goes
//!   stale under edits, rollbacks, clones, or forks): the maximal ranges
//!   of elements without a content token that are longer than
//!   [`LONG_RUN`] (*segments*), and, for each comment in them, its column
//!   as far as its arena tells (*notes*), with a min-tree over the
//!   columns. Shorter ranges are read element by element (O(`LONG_RUN`)).
//!   A tree without long comment runs keeps no table (`None`, no
//!   allocation).
//! * **Per overlay run** (the treap of `seq.rs`, maintained with every cut,
//!   merge and insertion in O(log)): whether the run holds a content
//!   token, whether it holds a stream-level document, and its **line
//!   summary** ([`Marks`]): how it moves the column and the "only white
//!   space and comments so far on this line" state of the text before it,
//!   and the smallest column of its own-line comments as a function of
//!   that state. Summaries compose (the summary of two adjacent runs is a
//!   function of theirs), so the treap keeps them as subtree sums, and a
//!   search carries the state of the text before each subtree: a
//!   comment's column is exact whichever run its line began in (a
//!   bound read from the run's own elements alone was
//!   "at least 0" for a comment whose line began in the run before, so a
//!   search visited every such run — after rewriting each comment of a
//!   long footer, O(footer) per question).
//!
//! A note's column is exact in the tree when the token ending its previous
//! line lies in the same overlay run (the elements of one run are
//! contiguous in the tree's order); otherwise — at most the first comment
//! of each run — it is decided from the tree's own tokens (O(its line)).
//! Every answer is checked against the walk it replaces: the attachment
//! tests over the yaml-test-suite and the fuzz corpora compare the fast
//! decisions with the one-pass decisions of `comment_attachments`, on
//! parsed and on edited trees (`tests/comment_index.rs`).
//!
//! **Cost.** Memory, exactly sized:
//! an 8-byte note per comment of a long run plus its min-tree slot (4
//! bytes per note rounded up to a power of two: 4 to 8), 4 per other
//! non-trivia element of a long run, 8 per segment (in a `Vec` grown by
//! doubling), one 8-byte pointer per arena (no table, no allocation,
//! without a long run), 4 per stream-level document of an arena with
//! stream-level content outside documents (a stray `...`, an error region;
//! none otherwise, `facts.rs`); per overlay run, a 20-byte summary in the run and a 20-byte
//! subtree sum in its treap item. Parsing reads a few elements per
//! [`LONG_RUN`] where content is dense ([`RunFacts::of`]). Queries: O(log
//! R + log C + `LONG_RUN` + the lines of the comments they decide
//! exactly), whatever the edit history (`tests/comment_run_scaling.rs`).
//!
//! Loops only.

use super::SyntaxKind as K;
use super::arena::Flat;
use super::facts::{LONG_RUN, StreamFacts};
use super::{NONE, Node, SyntaxTree};

/// A comment of a segment, as its arena tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Note {
    /// Local index of the comment.
    slot: u32,
    /// Its key in the min-tree: its column (characters from its line's
    /// start, saturating at `u32::MAX - 1`) when it is first on its line,
    /// characters from the arena's start (a lower bound) when its arena
    /// cannot tell (its line starts before the arena), and `u32::MAX` after
    /// content on its line. Exact in the tree for every note of a piece
    /// but the first (whose line may begin in the piece before: it is
    /// read from the tree).
    col: u32,
}

/// The tables of an arena with a long run (see the module docs). Local
/// indices.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Tables {
    /// `(a, b)`: local elements `a..b` hold no content token, the range is
    /// maximal and longer than [`LONG_RUN`]. Sorted.
    segs: Vec<(u32, u32)>,
    /// The comments of `segs`, in order (exactly sized).
    notes: Vec<Note>,
    /// The elements of `segs` that are not trivia tokens (nodes, empty
    /// tokens), in order (exactly sized): what a walk over siblings that
    /// skips trivia stops at.
    solids: Vec<u32>,
    /// The inner nodes of a min-tree over the notes' keys: `cap` is
    /// `notes.len()` rounded up to a power of two, node `x` (`1 <= x <
    /// cap`) has children `2x` and `2x + 1`, and node `cap + k` is note
    /// `k` itself (not stored). Empty when there are no notes.
    mins: Vec<u32>,
}

/// The per-arena part of the index (see the module docs): `None` (no
/// allocation) for an arena without a long run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct RunFacts(Option<Box<Tables>>);

/// `text`'s length in characters (saturating at `u32::MAX`).
fn chars(text: &str) -> u32 {
    u32::try_from(text.chars().count()).unwrap_or(u32::MAX)
}

/// Local element `i` is a content token: a token with bytes that is not
/// trivia (the rulebook's "visible").
pub(super) fn visible(flat: Flat<'_>, i: usize) -> bool {
    flat.kind(i)
        .is_some_and(|k| k.is_token() && !k.is_trivia() && !flat.token_span(i).is_empty())
}

/// A token of kind `k` with text `text` ends its line (`attach.rs`'s
/// `ends_line`: a line break, or a block scalar's text).
fn ends_line(k: K, text: &str) -> bool {
    k == K::LineBreak || (!text.is_empty() && text.ends_with(['\n', '\r']))
}

/// A token of kind `k` with text `text` neither ends a line nor is
/// content (`attach.rs`'s `blankish`, comments included: what a walk back
/// to a comment's line start skips).
fn skipped(k: K, text: &str) -> bool {
    matches!(k, K::Whitespace | K::ByteOrderMark | K::Comment)
        || (k != K::LineBreak && text.is_empty())
}

/// Where comment `c`'s line starts, read back over the tokens of `flat`
/// from `c` down to (not below) `floor`: (column, the deciding token,
/// first on its line). Exactly the attachment code's `line_of` and
/// `col_of` when the deciding token is found; `(chars from floor, NONE,
/// false)` otherwise. O(the tokens of its line).
fn back(flat: Flat<'_>, c: usize, floor: usize) -> (u32, u32, bool) {
    let mut acc = 0u32;
    let mut j = c;
    while j > floor {
        j = j.saturating_sub(1);
        let Some(k) = flat.kind(j) else { break };
        if !k.is_token() || k == K::ByteOrderMark {
            continue;
        }
        let text = flat.token_text(j);
        let at = u32::try_from(j).unwrap_or(NONE);
        if let Some(p) = text.rfind(['\n', '\r']) {
            let tail = chars(text.get(p.saturating_add(1)..).unwrap_or(""));
            return (acc.saturating_add(tail), at, ends_line(k, text));
        }
        if visible(flat, j) {
            // After content on its line: a same-line comment.
            return (acc, at, false);
        }
        acc = acc.saturating_add(chars(text));
    }
    (acc, NONE, false)
}

impl RunFacts {
    /// The index of arena `flat`. O(its elements), plus the line of each
    /// comment of a long run; no allocation without a long run.
    pub(super) fn of(flat: Flat<'_>) -> Self {
        let n = flat.len();
        let mut segs = Vec::new();
        // Parse cost: a long run must be clear of content
        // over its first `LONG_RUN + 1` elements, so the scan looks that
        // far ahead and back to the last content token before it, never
        // reading again what it knows is clear (`start..clear`): a document
        // with content on most lines costs a few reads per `LONG_RUN`
        // elements, and no element is read more than twice.
        let mut start = 0usize;
        let mut clear = 0usize;
        // One loop over the windows.
        while let Some(probe) = start.checked_add(LONG_RUN).filter(|p| *p < n) {
            let mut j = probe.saturating_add(1);
            let mut found = None;
            while j > clear.max(start) {
                j = j.saturating_sub(1);
                if visible(flat, j) {
                    found = Some(j);
                    break;
                }
            }
            if let Some(j) = found {
                start = j.saturating_add(1);
                clear = probe.saturating_add(1).max(start);
                continue;
            }
            let mut e = probe.saturating_add(1);
            while e < n && !visible(flat, e) {
                e = e.saturating_add(1);
            }
            if let (Ok(a), Ok(b)) = (u32::try_from(start), u32::try_from(e)) {
                segs.push((a, b));
            }
            start = e.saturating_add(1);
            clear = start;
        }
        if segs.is_empty() {
            return Self(None);
        }
        // The notes and solids are counted
        // first, so their tables are exactly sized.
        let (mut nc, mut sc) = (0usize, 0usize);
        for &(a, b) in &segs {
            for c in a as usize..b as usize {
                match flat.kind(c) {
                    Some(K::Comment) => nc = nc.saturating_add(1),
                    Some(k) if !k.is_trivia() => sc = sc.saturating_add(1),
                    _ => {}
                }
            }
        }
        let mut notes = Vec::with_capacity(nc);
        let mut solids = Vec::with_capacity(sc);
        for &(a, b) in &segs {
            // Parse cost: the line state is carried
            // forward from the segment's first line break on (a segment
            // holds only trivia, zero-width tokens and nodes); before it, a
            // comment's line starts before the segment and is read back.
            let mut state: Option<u32> = None;
            for c in a as usize..b as usize {
                let Some(k) = flat.kind(c) else { continue };
                match k {
                    K::Comment => {
                        // (A column saturates below `u32::MAX`, which marks
                        // a comment after content.)
                        let own = |col: u32| col.min(u32::MAX.saturating_sub(1));
                        let col = match state {
                            Some(col) => own(col),
                            None => match back(flat, c, 0) {
                                (col, NONE, _) | (col, _, true) => own(col),
                                (_, _, false) => u32::MAX,
                            },
                        };
                        notes.push(Note {
                            slot: u32::try_from(c).unwrap_or(NONE),
                            col,
                        });
                    }
                    K::LineBreak => state = Some(0),
                    K::Whitespace => {
                        if let Some(col) = state.as_mut() {
                            *col = col.saturating_add(chars(flat.token_text(c)));
                        }
                    }
                    // (Zero-width tokens and byte order marks add no
                    // column, `col_of`; nodes are no tokens.)
                    _ if !k.is_trivia() => solids.push(u32::try_from(c).unwrap_or(NONE)),
                    _ => {}
                }
            }
        }
        let mut t = Tables {
            segs,
            notes,
            solids,
            mins: Vec::new(),
        };
        if !t.notes.is_empty() {
            let cap = t.notes.len().next_power_of_two();
            t.mins = vec![u32::MAX; cap];
            let mut x = cap;
            while x > 1 {
                x = x.saturating_sub(1);
                let l = t.val(x.saturating_mul(2));
                let r = t.val(x.saturating_mul(2).saturating_add(1));
                if let Some(m) = t.mins.get_mut(x) {
                    *m = l.min(r);
                }
            }
        }
        Self(Some(Box::new(t)))
    }

    /// Heap bytes of the tables, by capacity (`Edits::heap_bytes`).
    #[cfg(test)]
    pub(super) fn heap_bytes(&self) -> usize {
        self.0.as_deref().map_or(0, |t| {
            size_of::<Tables>()
                .saturating_add(t.segs.capacity().saturating_mul(size_of::<(u32, u32)>()))
                .saturating_add(t.notes.capacity().saturating_mul(size_of::<Note>()))
                .saturating_add(t.mins.capacity().saturating_mul(4))
                .saturating_add(t.solids.capacity().saturating_mul(4))
        })
    }

    /// The comments of the arena's long runs (tests).
    #[cfg(test)]
    fn notes_len(&self) -> usize {
        self.0.as_deref().map_or(0, |t| t.notes.len())
    }

    /// The last element of a segment at local `a..b` that is not a trivia
    /// token.
    fn last_solid_in(&self, a: usize, b: usize) -> Option<usize> {
        let solids = &self.0.as_deref()?.solids;
        let hi = solids.partition_point(|s| (*s as usize) < b);
        let s = *solids.get(hi.checked_sub(1)?)? as usize;
        (s >= a).then_some(s)
    }

    /// The first element of a segment at local `a..b` that is not a
    /// trivia token.
    fn first_solid_in(&self, a: usize, b: usize) -> Option<usize> {
        let solids = &self.0.as_deref()?.solids;
        let lo = solids.partition_point(|s| (*s as usize) < a);
        let s = *solids.get(lo)? as usize;
        (s < b).then_some(s)
    }

    /// The long segment holding local element `i`.
    fn segment_at(&self, i: usize) -> Option<(usize, usize)> {
        let segs = &self.0.as_deref()?.segs;
        let k = segs.partition_point(|(a, _)| (*a as usize) <= i);
        let (a, b) = *segs.get(k.checked_sub(1)?)?;
        (i < b as usize).then_some((a as usize, b as usize))
    }

    /// The first note at local index `i` or later.
    fn notes_from(&self, i: usize) -> usize {
        self.0
            .as_deref()
            .map_or(0, |t| t.notes.partition_point(|n| (n.slot as usize) < i))
    }

    /// Note `k`.
    fn note(&self, k: usize) -> Option<Note> {
        self.0.as_deref()?.notes.get(k).copied()
    }

    /// The first note of `lo..hi` whose key is below `v`. O(log notes),
    /// no allocation.
    fn first_below(&self, lo: usize, hi: usize, v: u32) -> Option<usize> {
        let t = self.0.as_deref()?;
        let cap = t.cap();
        let (mut l, mut r) = (
            lo.saturating_add(cap),
            hi.min(t.notes.len()).saturating_add(cap),
        );
        // The covering nodes come left to right on the left side and
        // right to left on the right side: the first hit on the left is
        // the first of all; on the right, the last one met.
        let mut right_hit = None;
        while l < r {
            if l % 2 == 1 {
                if t.val(l) < v {
                    return t.descend(l, v);
                }
                l = l.saturating_add(1);
            }
            if r % 2 == 1 {
                r = r.saturating_sub(1);
                if t.val(r) < v {
                    right_hit = Some(r);
                }
            }
            l /= 2;
            r /= 2;
        }
        t.descend(right_hit?, v)
    }

    /// The smallest key of notes `lo..hi`. O(log notes), no allocation.
    fn min_in(&self, lo: usize, hi: usize) -> u32 {
        let Some(t) = self.0.as_deref() else {
            return u32::MAX;
        };
        let cap = t.cap();
        let (mut l, mut r) = (
            lo.saturating_add(cap),
            hi.min(t.notes.len()).saturating_add(cap),
        );
        let mut m = u32::MAX;
        while l < r {
            if l % 2 == 1 {
                m = m.min(t.val(l));
                l = l.saturating_add(1);
            }
            if r % 2 == 1 {
                r = r.saturating_sub(1);
                m = m.min(t.val(r));
            }
            l /= 2;
            r /= 2;
        }
        m
    }
}

impl Tables {
    /// The min-tree's leaf offset (`notes.len()` rounded up to a power of
    /// two; `mins.len()`).
    fn cap(&self) -> usize {
        self.mins.len()
    }

    /// Min-tree node `x`'s key: an inner node's minimum, or a note's key.
    fn val(&self, x: usize) -> u32 {
        let cap = self.cap();
        if x >= cap {
            self.notes
                .get(x.saturating_sub(cap))
                .map_or(u32::MAX, |n| n.col)
        } else {
            self.mins.get(x).copied().unwrap_or(u32::MAX)
        }
    }

    /// The first note under min-tree node `x` whose key is below `v` (`x`'s
    /// own key is).
    fn descend(&self, mut x: usize, v: u32) -> Option<usize> {
        let cap = self.cap();
        while x < cap {
            let l = x.saturating_mul(2);
            x = if self.val(l) < v {
                l
            } else {
                l.saturating_add(1)
            };
        }
        x.checked_sub(cap)
    }
}

/// The state of a line at a point of the text: its column there, and
/// whether only white space and comments came before it on its line (a
/// comment there is first on its line, `attach.rs`'s `line_of`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LineState {
    col: u32,
    clean: bool,
}

impl LineState {
    /// The start of the text.
    pub(super) const START: Self = Self {
        col: 0,
        clean: true,
    };
}

/// What one overlay run (local elements `a..b` of one arena), or a
/// stretch of consecutive runs (a treap subtree's sum), holds: content,
/// documents, firm elements, and its **line summary** (see the module
/// docs). Summaries compose with [`Marks::then`].
///
/// The line summary is exact for the trees the parser and the validated
/// edits build (a comment ends its line); for any other token sequence its
/// minimum is still a lower bound of the true columns (every search then
/// stays correct and only looks closer, never skips a hit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Marks {
    /// It holds a content token.
    pub(super) vis: bool,
    /// It holds an element that is not a trivia token (a node, a token
    /// that is not trivia): what a walk over siblings skipping trivia
    /// stops at.
    pub(super) firm: bool,
    /// It holds a stream-level document (a flag, not a count;
    /// the searches only skip runs without one).
    pub(super) docs: bool,
    /// It holds a line end or a content token (a column reset, see
    /// `line_moves`): `w` is then the column after the last one, else the
    /// width it adds to the column.
    brk: bool,
    /// What its last line-state setter makes of the state: `Some(true)` a
    /// line end, `Some(false)` content; `None` it keeps it.
    clean: Option<bool>,
    w: u32,
    /// Without content: the smallest column of its own-line comments that
    /// it decides itself (`u32::MAX` for none).
    exact: u32,
    /// Without content: the smallest offset of a comment before its first
    /// line break, from its start (`u32::MAX` for none): such a comment is
    /// first on its line, at the incoming column plus that offset, when
    /// the incoming state is clean.
    head: u32,
}

impl Marks {
    /// Nothing: no content token, no comment, no document; the identity of
    /// [`Marks::then`].
    pub(super) const NONE: Self = Self {
        vis: false,
        firm: false,
        docs: false,
        brk: false,
        clean: None,
        w: 0,
        exact: u32::MAX,
        head: u32::MAX,
    };

    /// Marks that claim content: every search looks inside (the
    /// conservative marks of an arena that cannot be read).
    pub(super) const CONTENT: Self = Self {
        vis: true,
        firm: true,
        docs: false,
        brk: false,
        clean: None,
        w: 0,
        exact: 0,
        head: 0,
    };

    /// Marks with these fields (`seq.rs`'s tests): `vis`, `firm`, `docs`,
    /// `(brk, clean, w)`, `(exact, head)`.
    #[cfg(test)]
    pub(super) const fn for_test(
        vis: bool,
        firm: bool,
        docs: bool,
        (brk, clean, w): (bool, Option<bool>, u32),
        (exact, head): (u32, u32),
    ) -> Self {
        Self {
            vis,
            firm,
            docs,
            brk,
            clean,
            w,
            exact,
            head,
        }
    }

    /// The marks of `self` followed by `r` (associative; [`Marks::NONE`]
    /// is its identity).
    pub(super) fn then(self, r: Self) -> Self {
        // `r`'s head comment after `self`: decided by `self`'s line state.
        let carried = self.w.saturating_add(r.head);
        let (to_exact, to_head) = match (self.clean, self.brk) {
            (Some(false), _) => (u32::MAX, u32::MAX),
            // (A line end comes with a break. A break without a setter, or
            // a setter without a break, takes no token the lexer makes;
            // counting it as decided keeps a lower bound.)
            (Some(true), _) | (None, true) => (carried, u32::MAX),
            (None, false) => (u32::MAX, carried),
        };
        Self {
            vis: self.vis || r.vis,
            firm: self.firm || r.firm,
            docs: self.docs || r.docs,
            brk: self.brk || r.brk,
            clean: r.clean.or(self.clean),
            w: if r.brk {
                r.w
            } else {
                self.w.saturating_add(r.w)
            },
            exact: self.exact.min(r.exact).min(to_exact),
            head: self.head.min(to_head),
        }
    }

    /// The line state after `self`, given the one before it.
    pub(super) fn after(&self, s: LineState) -> LineState {
        LineState {
            col: if self.brk {
                self.w
            } else {
                s.col.saturating_add(self.w)
            },
            clean: self.clean.unwrap_or(s.clean),
        }
    }

    /// `self`, after a text ending in state `s`, holds a content token or
    /// an own-line comment at a column below `v` (or, past content, may).
    pub(super) fn hits(&self, s: LineState, v: u32) -> bool {
        self.vis || self.exact < v || (s.clean && s.col.saturating_add(self.head) < v)
    }
}

/// How local elements `a..b` of `flat` move the line state: (a line end
/// or content among them, the column after the last one or the width they
/// add, the last state setter). Read back from `b` to the last line end
/// or content token: only white space, comments and zero-width tokens are
/// read (O(their text)), never a line of content (an edit next
/// to a long flow line stays local).
///
/// After content that does not end its line, the column is taken as 0:
/// a comment after it on its line is not first on its line whatever its
/// column, and the next line end sets the column again, so the column is
/// never read before it is set (and the summary stays associative: the
/// content counts as a reset everywhere).
fn line_moves(flat: Flat<'_>, a: usize, b: usize) -> (bool, u32, Option<bool>) {
    let mut acc = 0u32;
    let mut i = b;
    // A loop back over the run's trailing trivia.
    while i > a {
        i = i.saturating_sub(1);
        let Some(k) = flat.kind(i) else { break };
        if !k.is_token() || k == K::ByteOrderMark {
            continue;
        }
        let text = flat.token_text(i);
        if ends_line(k, text) {
            return (true, acc, Some(true));
        }
        if !skipped(k, text) {
            return (true, acc, Some(false));
        }
        // (White space, a comment or a zero-width token: none holds a
        // line break.)
        acc = acc.saturating_add(chars(text));
    }
    (false, acc, None)
}

/// The marks of local elements `a..b` of `flat` (`stream`: `sf` describes
/// its stream-level elements). O(log + `LONG_RUN` + a line).
pub(super) fn marks(
    flat: Flat<'_>,
    rf: &RunFacts,
    sf: Option<&StreamFacts>,
    a: u32,
    b: u32,
) -> Marks {
    let docs = sf.is_some_and(|s| s.holds_doc(flat, a, b));
    let (a, b) = (a as usize, b as usize);
    let (brk, w, clean) = line_moves(flat, a, b);
    let mut m = Marks {
        docs,
        brk,
        clean,
        w,
        ..Marks::NONE
    };
    let content = Marks {
        vis: true,
        firm: true,
        exact: u32::MAX,
        head: u32::MAX,
        ..m
    };
    if b.saturating_sub(a) > LONG_RUN {
        // A longer range without content lies inside a recorded segment.
        if !rf.segment_at(a).is_some_and(|(_, e)| e >= b) {
            return content;
        }
        m.firm = rf.first_solid_in(a, b).is_some();
        let (lo, hi) = (rf.notes_from(a), rf.notes_from(b));
        if let Some(first) = rf.note(lo).filter(|_| lo < hi) {
            // Its line may start before the run (O(its line)).
            match back(flat, first.slot as usize, a) {
                (off, NONE, _) => m.head = off,
                (col, _, true) => m.exact = col,
                _ => {}
            }
            // The later notes' lines start after the first one.
            m.exact = m.exact.min(rf.min_in(lo.saturating_add(1), hi));
        }
        return m;
    }
    // A loop over at most `LONG_RUN` elements. Columns from the
    // run's start (`brk`: from its last line break so far); a comment's
    // own width is not added (it ends its line), which keeps `col` a lower
    // bound for any other sequence.
    let mut col = 0u32;
    let mut seen_brk = false;
    let mut state: Option<bool> = None;
    for i in a..b {
        if visible(flat, i) {
            return content;
        }
        let Some(k) = flat.kind(i) else { continue };
        m.firm |= !k.is_trivia();
        if !k.is_token() || k == K::ByteOrderMark {
            continue;
        }
        if k == K::Comment {
            match (state, seen_brk) {
                (Some(false), _) => {}
                (None, false) => m.head = m.head.min(col),
                _ => m.exact = m.exact.min(col),
            }
            continue;
        }
        let text = flat.token_text(i);
        if let Some(p) = text.rfind(['\n', '\r']) {
            col = chars(text.get(p.saturating_add(1)..).unwrap_or(""));
            seen_brk = true;
        } else {
            col = col.saturating_add(chars(text));
        }
        if ends_line(k, text) {
            state = Some(true);
        } else if !skipped(k, text) {
            state = Some(false);
        }
    }
    m
}
/// A contiguous piece of the tree's order: local elements `a..b` of the
/// arena whose first slot is `arena`, and its overlay run (`None` in a
/// tree that was never edited).
#[derive(Clone, Copy)]
pub(super) struct Piece {
    pub(super) arena: u32,
    pub(super) a: u32,
    pub(super) b: u32,
    pub(super) item: Option<u32>,
}

impl SyntaxTree {
    /// The slot of local element `i` of `p`'s arena.
    fn piece_slot(p: Piece, i: usize) -> Option<u32> {
        u32::try_from(i).ok().and_then(|i| p.arena.checked_add(i))
    }

    /// Own-line comment `slot`'s column as the tree's tokens give it
    /// (`None`: it follows content on its line). O(its line).
    fn exact_col(&self, slot: u32) -> Option<usize> {
        let n = self.node_at_slot(slot)?;
        super::attach::own_col(n)
    }

    /// The first slot after `slot` in the tree's order that is a content
    /// token, or an own-line comment whose column is below `v`. O(log R +
    /// log C + `LONG_RUN` + the lines of the comments decided exactly):
    /// the line summaries skip every run without a hit, whatever the edit
    /// history cut the run into, so at most the piece holding
    /// `slot` and the piece holding the hit are read, each deciding at most
    /// its first comment from the tree.
    pub(super) fn run_hit(&self, slot: u32, v: u32) -> Option<u32> {
        let (mut p, local) = self.piece_at(slot)?;
        let mut from = local.checked_add(1)?;
        // A loop over the pieces that can hold a hit.
        loop {
            if let Some(h) = self.scan_piece(p, from as usize, v) {
                return Some(h);
            }
            let item = p.item?;
            p = self.piece_after(item, v)?;
            from = p.a;
        }
    }

    /// [`SyntaxTree::run_hit`] within piece `p`, from local `from` on.
    fn scan_piece(&self, p: Piece, from: usize, v: u32) -> Option<u32> {
        let (flat, rf) = self.arena_index(p.arena)?;
        let (a, b) = (p.a as usize, p.b as usize);
        let exact_hit = |i: usize| {
            if v == 0 {
                // No column is below 0: only content tokens are hits.
                return None;
            }
            let s = Self::piece_slot(p, i)?;
            self.exact_col(s)
                .is_some_and(|c| c < v as usize)
                .then_some(s)
        };
        let mut i = from;
        while i < b {
            if let Some((_, e)) = rf.segment_at(i) {
                let end = e.min(b);
                let hi = rf.notes_from(end);
                let mut k = rf.notes_from(i);
                // The piece's first note is the only one whose line may
                // start before the piece (every later one starts after the
                // note before it): it is then read from the tree (O(its
                // line)); the others' columns are exact.
                if k < hi && k == rf.notes_from(a) {
                    if let Some(note) = rf.note(k) {
                        if back(flat, note.slot as usize, a).1 == NONE {
                            if let Some(h) = exact_hit(note.slot as usize) {
                                return Some(h);
                            }
                            k = k.saturating_add(1);
                        }
                    }
                }
                if let Some(j) = rf.first_below(k, hi, v) {
                    return rf
                        .note(j)
                        .and_then(|hit| Self::piece_slot(p, hit.slot as usize));
                }
                if end < b {
                    // A segment ends at a content token.
                    return Self::piece_slot(p, end);
                }
                return None;
            }
            if visible(flat, i) {
                return Self::piece_slot(p, i);
            }
            if flat.kind(i) == Some(K::Comment) {
                if let Some(h) = exact_hit(i) {
                    return Some(h);
                }
            }
            i = i.saturating_add(1);
        }
        None
    }

    /// The last content token before `slot` in the tree's order. O(log R +
    /// log + `LONG_RUN`).
    pub(super) fn visible_before(&self, slot: u32) -> Option<u32> {
        let (mut p, local) = self.piece_at(slot)?;
        let mut to = local as usize;
        loop {
            if let Some(h) = self.scan_back(p, to) {
                return Some(h);
            }
            let item = p.item?;
            p = self.piece_visible_before(item)?;
            to = p.b as usize;
        }
    }

    /// The last content token of piece `p` before local `to`.
    fn scan_back(&self, p: Piece, to: usize) -> Option<u32> {
        let (flat, rf) = self.arena_index(p.arena)?;
        let a = p.a as usize;
        let mut i = to;
        while i > a {
            i = i.saturating_sub(1);
            if let Some((s, _)) = rf.segment_at(i) {
                if s > a {
                    // The element before a segment is a content token.
                    return Self::piece_slot(p, s.saturating_sub(1));
                }
                return None;
            }
            if visible(flat, i) {
                return Self::piece_slot(p, i);
            }
        }
        None
    }

    /// The last element before `slot` in the tree's order that is not a
    /// trivia token. O(log R + log + `LONG_RUN`).
    pub(super) fn solid_before(&self, slot: u32) -> Option<u32> {
        let (mut p, local) = self.piece_at(slot)?;
        let mut to = local as usize;
        loop {
            if let Some(h) = self.solid_back(p, to) {
                return Some(h);
            }
            let item = p.item?;
            p = self.piece_firm(item, false)?;
            to = p.b as usize;
        }
    }

    /// The last element of piece `p` before local `to` that is not a
    /// trivia token.
    fn solid_back(&self, p: Piece, to: usize) -> Option<u32> {
        let (flat, rf) = self.arena_index(p.arena)?;
        let a = p.a as usize;
        let mut i = to;
        while i > a {
            i = i.saturating_sub(1);
            if let Some((s, _)) = rf.segment_at(i) {
                if let Some(x) = rf.last_solid_in(s.max(a), i.saturating_add(1)) {
                    return Self::piece_slot(p, x);
                }
                if s > a {
                    return Self::piece_slot(p, s.saturating_sub(1));
                }
                return None;
            }
            if flat.kind(i).is_some_and(|k| !k.is_trivia()) {
                return Self::piece_slot(p, i);
            }
        }
        None
    }

    /// The first element after `slot` in the tree's order that is not a
    /// trivia token. O(log R + log + `LONG_RUN`).
    pub(super) fn solid_after(&self, slot: u32) -> Option<u32> {
        let (mut p, local) = self.piece_at(slot)?;
        let mut from = local.checked_add(1)? as usize;
        loop {
            if let Some(h) = self.solid_forward(p, from) {
                return Some(h);
            }
            let item = p.item?;
            p = self.piece_firm(item, true)?;
            from = p.a as usize;
        }
    }

    /// The first element of piece `p` from local `from` on that is not a
    /// trivia token.
    fn solid_forward(&self, p: Piece, from: usize) -> Option<u32> {
        let (flat, rf) = self.arena_index(p.arena)?;
        let b = p.b as usize;
        let mut i = from;
        while i < b {
            if let Some((_, e)) = rf.segment_at(i) {
                let end = e.min(b);
                if let Some(x) = rf.first_solid_in(i, end) {
                    return Self::piece_slot(p, x);
                }
                if end < b {
                    return Self::piece_slot(p, end);
                }
                return None;
            }
            if flat.kind(i).is_some_and(|k| !k.is_trivia()) {
                return Self::piece_slot(p, i);
            }
            i = i.saturating_add(1);
        }
        None
    }

    /// The last stream-level document that starts before `slot`: O(log R
    /// plus log D), or O(log R plus depth plus `LONG_RUN`) in an arena
    /// without a document list (`facts.rs`).
    pub(super) fn document_before(&self, slot: u32) -> Option<Node<'_>> {
        let (mut p, local) = self.piece_at(slot)?;
        let mut to = local;
        loop {
            if let (Some(sf), Some((flat, _))) =
                (self.stream_facts_of(p.arena), self.arena_index(p.arena))
            {
                if let Some(d) = sf.last_doc_in(flat, p.a, to) {
                    return self.node_at_slot(p.arena.checked_add(d)?);
                }
            }
            let item = p.item?;
            p = self.piece_docs(item, false)?;
            to = p.b;
        }
    }

    /// The first stream-level document that starts after `slot`. Costs as
    /// [`SyntaxTree::document_before`].
    pub(super) fn document_after(&self, slot: u32) -> Option<Node<'_>> {
        let (mut p, local) = self.piece_at(slot)?;
        let mut from = local.checked_add(1)?;
        loop {
            if let (Some(sf), Some((flat, _))) =
                (self.stream_facts_of(p.arena), self.arena_index(p.arena))
            {
                if let Some(d) = sf.first_doc_in(flat, from, p.b) {
                    return self.node_at_slot(p.arena.checked_add(d)?);
                }
            }
            let item = p.item?;
            p = self.piece_docs(item, true)?;
            from = p.a;
        }
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
    use crate::tree::{NodeId, SyntaxTree};

    /// The min-tree answers as a scan over the notes does.
    #[test]
    fn min_tree_agrees_with_a_scan() {
        for count in [1usize, 2, 3, 77, 128, 300] {
            let mut src = String::from("a:\n  b: 1\n");
            // Enough elements for a long run even with one comment.
            src.push_str(&"\n".repeat(70));
            for i in 0..count {
                src.push_str(&" ".repeat(i % 7));
                src.push_str("# c\n");
            }
            src.push_str("z: 1\n");
            let t = SyntaxTree::parse(src);
            let rf = RunFacts::of(t.base_flat());
            let tables = rf.0.as_deref().unwrap();
            assert_eq!(tables.segs.len(), 1);
            assert_eq!(tables.notes.len(), count);
            // Exactly sized.
            assert_eq!(tables.notes.capacity(), count);
            assert_eq!(tables.mins.len(), count.next_power_of_two());
            let keys: Vec<u32> = tables.notes.iter().map(|n| n.col).collect();
            let mut ranges = vec![(0, count), (0, 0), (count / 2, count)];
            for lo in 0..count.min(9) {
                for hi in lo..count.min(13) {
                    ranges.push((lo, hi));
                }
            }
            for (lo, hi) in ranges {
                for v in 0..9 {
                    let want = (lo..hi).find(|k| keys[*k] < v);
                    assert_eq!(rf.first_below(lo, hi, v), want, "{lo} {hi} {v}");
                }
                let want = keys[lo..hi].iter().copied().min().unwrap_or(u32::MAX);
                assert_eq!(rf.min_in(lo, hi), want);
            }
            // Columns as the attachment code reads them.
            for n in &tables.notes {
                let node = t.node_at_slot(n.slot).unwrap();
                assert_eq!(super::super::attach::own_col(node), Some(n.col as usize));
            }
        }
    }

    /// Inputs for the line summaries: comments at every column, blank and
    /// white-space lines, block scalars (whose text ends its line), a byte
    /// order mark, CRLF and CR line ends, tabs, flow collections, comments
    /// after content, several documents, long runs.
    fn line_inputs() -> Vec<String> {
        let mut out = vec![
            "\u{feff}# a\nk: v # t\n  # b\n# c\n".to_owned(),
            "a: |+\n  x\n\n# c\n   # d\nb: [1, # e\n  2]\n".to_owned(),
            "- a\r\n  # b\r\n# c\r\n- d\r\n".to_owned(),
            "a: 1\r# b\r  # c\rz: 2\r".to_owned(),
            "--- # h\n# a\n...\n# s\n--- x\n\t# t\n".to_owned(),
            "k: >\n  folded\n  # not a comment\n # c\n".to_owned(),
            "\u{feff}  # bom then indent\nx: 1\n".to_owned(),
        ];
        let mut long = String::from("a:\n  b:\n    - 1\n");
        for i in 0..150 {
            long.push_str(&" ".repeat((i * 5) % 9));
            long.push_str(&format!("# c{i}\n"));
            if i % 11 == 3 {
                long.push_str("  \n\n");
            }
            if i % 37 == 20 {
                long.push_str("   - q # t\n");
            }
        }
        long.push_str("z: 1\n...\n");
        for i in 0..90 {
            long.push_str(if i % 13 == 0 { "...\n" } else { "  # s\n" });
        }
        out.push(long);
        out
    }

    /// The tree's elements in order, as slots.
    fn order(t: &SyntaxTree) -> Vec<u32> {
        t.root().descendants().map(Node::slot).collect()
    }

    /// The line state before every element of `t`'s order, folded from the
    /// marks of each element (as one-element runs) — for every comment,
    /// first on its line exactly when the state is clean, at the state's
    /// column — and the marks of every range are the composition of the
    /// marks of its parts (summaries compose
    /// exactly).
    #[test]
    fn line_summaries_compose_as_the_text_reads() {
        for src in line_inputs() {
            let t = SyntaxTree::parse(src.as_str());
            let flat = t.base_flat();
            let rf = RunFacts::of(flat);
            let n = u32::try_from(flat.len()).unwrap();
            let m = |a: u32, b: u32| marks(flat, &rf, Some(&t.facts), a, b);
            let mut s = LineState::START;
            for i in 0..n {
                let node = t.node_at_slot(i).unwrap();
                if node.kind() == K::Comment {
                    let own = super::super::attach::own_col(node);
                    if s.clean {
                        assert_eq!(own, Some(s.col as usize), "{src:?} at {i}");
                    } else {
                        assert_eq!(own, None, "{src:?} at {i}");
                    }
                }
                s = m(i, i + 1).after(s);
            }
            // Composition: every split of a set of ranges.
            let step = (n / 40).max(1);
            let points: Vec<u32> = (0..=n).step_by(step as usize).chain([n]).collect();
            for (x, &a) in points.iter().enumerate() {
                for &b in &points[x..] {
                    let whole = m(a, b);
                    for &c in points.iter().filter(|c| **c >= a && **c <= b) {
                        let parts = m(a, c).then(m(c, b));
                        assert_eq!(
                            (whole.vis, whole.firm, whole.docs),
                            (parts.vis, parts.firm, parts.docs)
                        );
                        assert_eq!(
                            (whole.brk, whole.w, whole.clean),
                            (parts.brk, parts.w, parts.clean),
                            "{src:?} {a}..{c}..{b}"
                        );
                        if !whole.vis {
                            assert_eq!(
                                (whole.exact, whole.head),
                                (parts.exact, parts.head),
                                "{src:?} {a}..{c}..{b}"
                            );
                        }
                    }
                }
            }
            // The identity.
            let all = m(0, n);
            assert_eq!(Marks::NONE.then(all), all);
            assert_eq!(all.then(Marks::NONE), all);
        }
    }

    /// The pieces of `t` with a long run in a fragment's arena (the
    /// fragment index path).
    fn fragment_long_pieces(t: &SyntaxTree) -> usize {
        let mut out = std::collections::BTreeSet::new();
        for s in order(t) {
            let (p, _) = t.piece_at(s).unwrap();
            if p.arena != 0 && t.arena_index(p.arena).unwrap().1.notes_len() > 0 {
                out.insert((p.arena, p.a));
            }
        }
        out.len()
    }

    /// Every primitive answers as a walk over the elements does.
    fn check_primitives(t: &SyntaxTree, what: &str) {
        let slots = order(t);
        let at = |s: u32| t.node_at_slot(s).unwrap();
        let vis = |s: u32| {
            let n = at(s);
            n.is_token() && !n.kind().is_trivia() && !n.span().is_empty()
        };
        let own_col = |s: u32| {
            let n = at(s);
            if n.kind() == K::Comment {
                super::super::attach::own_col(n)
            } else {
                None
            }
        };
        let is_doc = |s: u32| at(s).kind() == K::Document;
        let solid = |s: u32| !at(s).kind().is_trivia();
        for (k, s) in slots.iter().enumerate() {
            let before = slots[..k].iter().rev().find(|x| vis(**x)).copied();
            assert_eq!(t.visible_before(*s), before, "{what} slot {s}");
            for v in [0u32, 1, 3, 5, 9] {
                let hit = slots[k + 1..]
                    .iter()
                    .find(|x| vis(**x) || own_col(**x).is_some_and(|c| c < v as usize))
                    .copied();
                assert_eq!(t.run_hit(*s, v), hit, "{what} slot {s} v {v}");
            }
            let sb = slots[..k].iter().rev().find(|x| solid(**x)).copied();
            assert_eq!(t.solid_before(*s), sb, "{what} slot {s}");
            let sa = slots[k + 1..].iter().find(|x| solid(**x)).copied();
            assert_eq!(t.solid_after(*s), sa, "{what} slot {s}");
            let db = slots[..k].iter().rev().find(|x| is_doc(**x)).copied();
            assert_eq!(t.document_before(*s).map(Node::slot), db, "{what}");
            let da = slots[k + 1..].iter().find(|x| is_doc(**x)).copied();
            assert_eq!(t.document_after(*s).map(Node::slot), da, "{what}");
        }
    }

    /// The fragment of `text`'s stream children.
    fn fragment(text: &str) -> crate::tree::Fragment {
        let f = SyntaxTree::parse(text);
        let kids: Vec<_> = f.root().children().collect();
        f.extract(kids[0].id(), kids[kids.len() - 1].id()).unwrap()
    }

    /// `k` comment lines at varying columns (`3k` elements or more: a long
    /// run of the fragment's own arena when `k > 21`).
    fn long_lines(k: usize, tag: &str) -> String {
        let mut s = String::new();
        for i in 0..k {
            s.push_str(&" ".repeat((i * 3) % 7));
            s.push_str(&format!("# {tag}{i}\n"));
        }
        s
    }

    fn ids(t: &SyntaxTree, kind: K) -> Vec<NodeId> {
        t.root()
            .descendants()
            .filter(|n| n.kind() == kind)
            .map(Node::id)
            .collect()
    }

    /// Every primitive answers as a walk over the elements does, on a parsed
    /// tree and after edits that cut its long runs: short and long
    /// (fragment-arena) comment lines inserted at every column, cuts inside the
    /// inserted runs (indentation and comments rewritten, lines removed),
    /// multi-piece removals, every comment of a stretch rewritten (the shape
    /// that once missed this), rolled-back groups, forks and clones; with stray
    /// `...` stretches and several documents. Every state also equals a fresh
    /// parse of its text.
    #[test]
    fn primitives_agree_with_walks() {
        let mut src = String::from("a:\n  b:\n    - 1\n");
        for i in 0..90 {
            src.push_str(&" ".repeat((i * 7) % 6));
            src.push_str(&format!("# c{i}\n"));
            if i % 13 == 5 {
                src.push('\n');
            }
        }
        src.push_str("z: 1\n...\n");
        for i in 0..80 {
            src.push_str(if i % 9 == 0 { "...\n" } else { "# s\n" });
        }
        src.push_str("--- x\n...\n# t\n--- y\n");
        let mut t = SyntaxTree::parse(src);
        let pick = |v: &Vec<NodeId>, k: usize| v[(k * 37 + 11) % v.len()];
        let mut long_seen = 0usize;
        let mut removed = 0usize;
        for step in 0..16usize {
            check_primitives(&t, &format!("step {step}"));
            long_seen = long_seen.max(fragment_long_pieces(&t));
            if step % 5 == 2 {
                check_primitives(&t.fork(), &format!("fork {step}"));
                check_primitives(&t.clone(), &format!("clone {step}"));
            }
            let comments = ids(&t, K::Comment);
            let breaks = ids(&t, K::LineBreak);
            match step % 8 {
                0 => {
                    t.replace_token(pick(&comments, step), "# rewritten")
                        .unwrap();
                }
                1 => {
                    let frag = fragment(&format!("{}# new\n", " ".repeat(step % 5)));
                    let mark = t.begin_group();
                    let _ = t.insert_after(pick(&breaks, step), frag.clone());
                    if step % 3 == 0 {
                        t.rollback_group(mark).unwrap();
                    } else {
                        t.end_group(mark).unwrap();
                    }
                    let _ = t.insert_after(pick(&breaks, step + 5), frag);
                }
                2 | 6 => {
                    // A long fragment: its own arena's long run.
                    let frag = fragment(&long_lines(40, &format!("f{step}-")));
                    let mark = t.begin_group();
                    let _ = t.insert_after(pick(&breaks, step + 1), frag.clone());
                    if step == 6 {
                        // Rolled back, then inserted elsewhere.
                        t.rollback_group(mark).unwrap();
                        let _ = t.insert_after(pick(&breaks, step + 9), frag);
                    } else {
                        t.end_group(mark).unwrap();
                    }
                }
                3 => {
                    // Cuts inside the inserted long runs: indentation (to
                    // another width) and comments rewritten (a piece then
                    // starts at a comment whose line began in the piece
                    // before).
                    let inserted: Vec<NodeId> = t
                        .root()
                        .descendants()
                        .filter(|n| n.kind() == K::Comment && n.text().contains("# f"))
                        .map(Node::id)
                        .collect();
                    for (k, c) in inserted.iter().enumerate().filter(|(k, _)| k % 6 == 2) {
                        let n = t.get(*c).unwrap();
                        match n.prev_sibling().filter(|w| w.kind() == K::Whitespace) {
                            Some(w) if k % 4 == 0 => {
                                // Another width: the arena's column of the
                                // comment is stale, the tree's is read.
                                let (w, len) = (w.id(), w.text().len());
                                t.replace_token(w, &" ".repeat(len % 5 + 1 + k % 3))
                                    .unwrap();
                            }
                            _ => {
                                t.replace_token(*c, "# cut").unwrap();
                            }
                        }
                    }
                }
                4 => {
                    // A multi-piece removal: whole lines across the cuts.
                    let c = t
                        .root()
                        .descendants()
                        .filter(|n| n.kind() == K::Comment && n.text() == "# cut")
                        .nth(1)
                        .unwrap();
                    let first = c
                        .prev_sibling()
                        .filter(|w| w.kind() == K::Whitespace)
                        .unwrap_or(c)
                        .id();
                    let mut last = c;
                    let mut lines = 0;
                    while lines < 5 {
                        let Some(x) = last.next_sibling() else { break };
                        last = x;
                        if last.kind() == K::LineBreak {
                            lines += 1;
                        }
                    }
                    let last = last.id();
                    let mark = t.begin_group();
                    if t.remove_range(first, last).is_ok() && lines == 5 {
                        removed += 1;
                    }
                    t.end_group(mark).unwrap();
                }
                5 => {
                    // Every comment of the first stretch rewritten.
                    for c in comments.iter().take(60) {
                        let text = t.get(*c).unwrap().text().into_owned();
                        let _ = t.replace_token(*c, &text);
                    }
                }
                _ => {
                    let c = t.get(pick(&comments, step + 2)).unwrap();
                    let b = c.next_sibling().unwrap().id();
                    let c = c.id();
                    let _ = t.remove_range(c, b);
                }
            }
            assert!(t == SyntaxTree::parse(t.emit()), "step {step}");
        }
        assert!(long_seen >= 2, "{long_seen} long fragment pieces");
        assert!(removed >= 1, "no multi-piece removal");
    }

    /// The segments are the maximal ranges of more than [`LONG_RUN`]
    /// elements without a content token, as a scan over every element
    /// finds them (the index's scan skips ahead): around the
    /// threshold, at the arena's ends, and over the inputs above.
    #[test]
    fn segments_are_the_maximal_long_runs() {
        let mut inputs = line_inputs();
        for gap in [20usize, 21, 22, 23, 43, 44, 45] {
            // `gap` comment lines (3 elements each) between content.
            for lead in [0usize, 1, 2] {
                let mut s = "# x\n".repeat(lead * gap);
                for k in 0..4 {
                    s.push_str(&format!("k{k}: v\n"));
                    s.push_str(&"  # c\n".repeat(gap + k % 2));
                }
                inputs.push(s);
            }
        }
        for src in inputs {
            let t = SyntaxTree::parse(src.as_str());
            let flat = t.base_flat();
            let mut want = Vec::new();
            let mut start = None;
            for i in 0..=flat.len() {
                if i == flat.len() || visible(flat, i) {
                    if let Some(a) = start.take() {
                        if i - a > LONG_RUN {
                            want.push((a as u32, i as u32));
                        }
                    }
                } else if start.is_none() {
                    start = Some(i);
                }
            }
            let rf = RunFacts::of(flat);
            let got = rf.0.as_deref().map_or(Vec::new(), |t| t.segs.clone());
            assert_eq!(got, want, "{src:?}");
        }
    }

    /// The same on multi-document trees without stray content
    /// (no document list: the documents are found by a walk), edited:
    /// documents and stray `...` markers inserted (a fragment arena with
    /// and without a list), comment lines inserted and removed between
    /// documents.
    #[test]
    fn primitives_agree_without_a_document_list() {
        let mut src = String::from("--- a\n");
        src.push_str(&long_lines(30, "h"));
        src.push_str("...\n");
        src.push_str(&long_lines(30, "s"));
        src.push_str("--- b: 1\n...\n# t\n---\n- c\n");
        let mut t = SyntaxTree::parse(src);
        assert!(!t.facts.has_doc_list());
        let pick = |v: &Vec<NodeId>, k: usize| v[(k * 37 + 11) % v.len()];
        let mut inserted = 0;
        for step in 0..8usize {
            check_primitives(&t, &format!("no list, step {step}"));
            let breaks = ids(&t, K::LineBreak);
            let comments = ids(&t, K::Comment);
            let text = match step % 4 {
                0 => "--- z\n".to_owned(),
                1 => "...\n# stray\n".to_owned(),
                2 => long_lines(25, &format!("n{step}-")),
                _ => {
                    let c = t.get(pick(&comments, step)).unwrap();
                    let b = c.next_sibling().unwrap().id();
                    let c = c.id();
                    let _ = t.remove_range(c, b);
                    continue;
                }
            };
            // After a stream-level line break (between documents).
            let at = breaks
                .iter()
                .copied()
                .filter(|b| {
                    t.get(*b)
                        .unwrap()
                        .parent()
                        .is_some_and(|p| p.kind() == K::Stream)
                })
                .nth(step % 3);
            if let Some(at) = at {
                inserted += usize::from(t.insert_after(at, fragment(&text)).is_ok());
            }
            assert!(t == SyntaxTree::parse(t.emit()), "step {step}");
        }
        check_primitives(&t, "no list, end");
        assert!(inserted >= 3, "{inserted} insertions");
    }

    /// No long run: nothing allocated; the per-arena index is a pointer.
    #[test]
    fn short_runs_allocate_nothing() {
        let t = SyntaxTree::parse("a: 1 # c\n# d\nb: 2\n");
        let rf = RunFacts::of(t.base_flat());
        assert_eq!(rf, RunFacts::default());
        assert_eq!(rf.heap_bytes(), 0);
        assert_eq!(size_of::<RunFacts>(), 8);
        assert_eq!(size_of::<Marks>(), 20);
    }

    /// The tables are exactly sized; per comment of a
    /// long run, an 8-byte note and 4 to 8 bytes of min-tree.
    #[test]
    fn long_run_tables_are_exactly_sized() {
        for n in [100usize, 1000, 1025, 75_000] {
            let mut src = String::from("a:\n  b: 1\n");
            for i in 0..n {
                src.push_str(&format!("  # comment line {i}\n"));
            }
            src.push_str("z: 1\n");
            let t = SyntaxTree::parse(src);
            let rf = RunFacts::of(t.base_flat());
            let fixed = size_of::<Tables>() + 4 * 8;
            let bytes = rf.heap_bytes();
            assert!(bytes <= fixed + 16 * n, "{n} comments: {bytes} bytes");
            assert!(bytes >= 12 * n, "{n} comments: {bytes} bytes");
        }
    }
}
