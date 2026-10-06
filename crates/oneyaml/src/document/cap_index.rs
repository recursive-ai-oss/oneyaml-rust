//! The `max_diagnostics` index:
//! the per-document diagnostic counts of an edited stream whose
//! diagnostics exceed the cap, so that the capped list is found in
//! O(log d) plus the documents that hold what it shows, never by reading
//! every document of the stream; and
//! what the last capped list showed of each document, so that a later
//! read takes it again instead of reading untouched documents.
//!
//! **What is counted.** For document `i`, what
//! `Shared::edited_doc_events(i)` gives: how many diagnostics it lists
//! (`n`, skeletons included), how many of them are errors (`errors`), and
//! what its meaning check counted without keeping ([`Dropped`]: how many,
//! and the least start). Its meaning checks' share (`Shared::doc_meaning`,
//! its own node only) is kept apart from its syntax diagnostics' (those
//! its region owns, `Shared::region_tree_diagnostics`), so that either
//! can be read again alone. An edit of document `i` makes both stale.
//!
//! **The neighbours' syntax parts are refreshed too, defensively.** A
//! region's syntax part depends on where its region and the next one
//! start (`Shared::split_between`), so an edit of `i` could only change
//! `i - 1`'s or `i + 1`'s count by moving a boundary across a diagnostic.
//! None is known to: a boundary lies in the stretch of stream-level text
//! between two documents, after the earlier one's node and the comment
//! lines it owns, and never past the later one's first visible token
//! (directive, stray `...`, error region, `---`, content); the syntax
//! layer locates no diagnostic on the comment lines, blank lines and
//! white space in between (its warnings are at directives and lenient
//! `: ` inside content, its errors at visible tokens), and an edit
//! never changes a stretch comment's owner (refused with
//! `CommentReattaches`). So no test can tell the refresh's absence; it
//! is kept so that a future diagnostic located on trivia cannot make a
//! count stale, at the cost of reading the two neighbouring regions'
//! syntax diagnostics per edited document.
//!
//! **Layout** (small per document). Two Fenwick trees of
//! `u32` (listed diagnostics, errors) and the meaning checks' share as two
//! `u32`s: 16 bytes per document, built in place (no per-document
//! temporaries). What the meaning checks counted without keeping is kept
//! only for the documents that have some (a map, and the least start in
//! an ordered set). A stream whose counts do not fit `u32` (more than 4G
//! diagnostics) has no index: its reads scan every document, as before
//! this index. Measured: about 16 bytes per document retained (2.1 MB for
//! 131,093 documents).
//!
//! **What the last list showed** ([`Shown`]): the list itself (shared with
//! `Shared::cap_view`, never a copy), and per document that list read,
//! where its first entries are in it, how many, whether they are all of
//! its entries, where its first error is, and where its region started
//! then (a later read moves the spans by how far it moved: an edit before
//! it shifts its text, never its diagnostics' relative places). The
//! entries an edit can change are dropped with the counts in `refresh`;
//! every other one stays exact. A read takes a document's entries from it
//! when it holds as many as are shown now (or all of them), and its first
//! error when needed; otherwise it reads the document.
//!
//! **Cost.**
//!
//! * Built once, O(stream), on the first capped read of an edited stream,
//!   and built again after an edit that changes the tree's error count
//!   (the meaning checks' reach moves) or the documents
//!   (`documents_changed`: the first document of an empty input); nothing
//!   is built for a stream under the cap, nor before its first capped
//!   read.
//! * Per edited document, on the next read: O(log d), plus its meaning
//!   check (O(the document)) and the syntax diagnostics of its region and
//!   its two neighbouring regions (counted, not listed: a neighbour whose
//!   counts did not change keeps what the last list showed of it, so a
//!   huge neighbour costs a count of its syntax diagnostics, never a
//!   meaning check or a re-read of its text).
//! * Editing a document the list shows costs O(that document) on the next
//!   read: its meaning check, its entries, and (past the cap on syntax
//!   diagnostics) its text read again for their texts. Stream edits stay local
//!   (the edited document itself); a document that is most of the stream
//!   costs about as much as parsing the stream again (measured 1.8x the
//!   re-parse of the whole text for a 200,000-line document holding every shown
//!   skeleton, which a one-document stream avoids by parsing again).
//! * Per read: O(log d) per document holding an entry of the capped list
//!   or its first error, plus copying what it shows; a document is read
//!   only when it was edited (or is a neighbour of an edited one), or
//!   when the list now shows more of it than the last list did (earlier
//!   entries went away), or its first error, not shown before, is now
//!   needed. Past the cap on syntax diagnostics, a document read for a
//!   shown skeleton also has its text read again (`Shared::doc_head_filled`).
//!
//! **Memory**. Kept: 16 bytes per document, the
//! last list (shared with `Shared::cap_view`), one spare entry, and a
//! small record per document it shows. A read holds besides at most the
//! last list and the new one, the meaning checks the counts read when
//! non-empty (at most `max_diagnostics + 1` diagnostics in all), and the
//! document it reads: a document is read for its first entries and its
//! first error only (`Shared::doc_head`, never its whole list), its counts
//! are taken without listing it, and a document's text read again for
//! skeleton texts is parsed under the stream's own cap. A stream of one
//! document parses its text again instead (its region is the whole
//! text). Measured: a read's peak stays below a parse's of the edited
//! text (`oracles/tests/capped_read_memory.rs`: 2.1 MB against 21.8 MB
//! for 131,093 empty flow mappings).
//!
//! Every walk is a loop, never recursion.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use oneyaml_core::Diagnostic;

use crate::diag_cap::Dropped;

/// The counts of one document's part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Agg {
    /// Diagnostics listed.
    pub(super) n: usize,
    /// Errors among them.
    pub(super) errors: usize,
    /// Counted by the meaning checks but not kept.
    pub(super) dropped: Dropped,
}

/// A Fenwick tree of `u32` counts over the documents (node `k` at index
/// `k - 1`). Every partial sum fits a `u32`: [`CapIndex`] keeps the total
/// within it.
#[derive(Clone, Debug)]
struct Fenwick(Vec<u32>);

impl Fenwick {
    /// The tree over `leaves`, built in place. O(d).
    fn from_leaves(mut t: Vec<u32>) -> Self {
        let len = t.len();
        for k in 1..=len {
            let parent = k.wrapping_add(k & k.wrapping_neg());
            if parent <= len {
                let v = t.get(k.wrapping_sub(1)).copied().unwrap_or(0);
                if let Some(p) = t.get_mut(parent.wrapping_sub(1)) {
                    *p = p.wrapping_add(v);
                }
            }
        }
        Self(t)
    }

    /// Adds `delta` (two's complement: a subtraction wraps) to leaf `i`.
    fn add(&mut self, i: usize, delta: u32) {
        let len = self.0.len();
        let mut k = i.wrapping_add(1);
        while k <= len {
            if let Some(v) = self.0.get_mut(k.wrapping_sub(1)) {
                *v = v.wrapping_add(delta);
            }
            k = k.wrapping_add(k & k.wrapping_neg());
        }
    }

    /// The sum over the leaves before `j`. O(log d).
    fn prefix(&self, j: usize) -> u32 {
        let mut k = j.min(self.0.len());
        let mut sum = 0u32;
        while k > 0 {
            sum = sum.wrapping_add(self.0.get(k.wrapping_sub(1)).copied().unwrap_or(0));
            k &= k.wrapping_sub(1);
        }
        sum
    }

    /// Leaf `i`. O(log d).
    fn leaf(&self, i: usize) -> u32 {
        self.prefix(i.saturating_add(1))
            .wrapping_sub(self.prefix(i))
    }

    /// The leaf holding item `t` (0-based) counted from the first leaf,
    /// with how many items of it come before `t`. O(log d).
    fn find(&self, t: u32, total: u32) -> Option<(usize, u32)> {
        if total <= t {
            return None;
        }
        let len = self.0.len();
        // The highest power of two not past `len` (0 for none).
        let mut step = usize::from(len > 0);
        while step.checked_mul(2).is_some_and(|s| s <= len) {
            step = step.saturating_mul(2);
        }
        let mut at = 0usize;
        let mut rest = t;
        while step > 0 {
            let next = at.wrapping_add(step);
            if next <= len {
                let v = self.0.get(next.wrapping_sub(1)).copied().unwrap_or(0);
                if v <= rest {
                    at = next;
                    rest = rest.wrapping_sub(v);
                }
            }
            step /= 2;
        }
        (at < len).then_some((at, rest))
    }
}

/// Where a document's first error is in the last list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ErrorAt {
    /// At this index.
    At(usize),
    /// Not in it (or not known).
    Unknown,
}

/// What the last capped list showed of one document (see the module
/// docs).
#[derive(Clone, Copy, Debug)]
pub(super) struct Seen {
    /// Where its region started when the list was built.
    pub(super) start: usize,
    /// Where its first entries are in the list.
    pub(super) at: usize,
    /// How many of them are there; and, when `spare`, the next one is
    /// [`Shown::spare`] (the entry the cap replaced with its summary).
    pub(super) len: usize,
    pub(super) spare: bool,
    /// Where its first error is.
    pub(super) error: ErrorAt,
}

/// The last capped list and what it showed of each document.
#[derive(Clone, Debug, Default)]
pub(super) struct Shown {
    pub(super) list: Option<Arc<[Diagnostic]>>,
    pub(super) spare: Option<Diagnostic>,
    pub(super) docs: BTreeMap<usize, Seen>,
}

/// The diagnostic counts of every document of an edited stream (see the
/// module docs).
#[derive(Clone, Debug)]
pub(super) struct CapIndex {
    /// Listed diagnostics and errors per document.
    n: Fenwick,
    errors: Fenwick,
    total_n: u32,
    total_errors: u32,
    /// Per document: its meaning checks' listed diagnostics and errors.
    meaning: Vec<[u32; 2]>,
    /// The documents whose meaning checks counted some without keeping
    /// them, and those counts' least starts.
    dropped: BTreeMap<usize, Dropped>,
    firsts: BTreeSet<(usize, usize)>,
    dropped_count: usize,
    /// Documents edited since the last refresh.
    stale: BTreeSet<usize>,
    /// What the last list showed.
    pub(super) shown: Shown,
}

/// The two parts of document `i`'s counts.
pub(super) trait Parts {
    /// The syntax diagnostics document `i`'s region owns (`dropped` none).
    fn syntax_part(&self, i: usize) -> Agg;
    /// Document `i`'s meaning checks.
    fn meaning_part(&self, i: usize) -> Agg;
}

/// The totals of the counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Total {
    pub(super) n: usize,
    pub(super) errors: usize,
    pub(super) dropped: Dropped,
}

fn small(v: usize) -> Option<u32> {
    u32::try_from(v).ok()
}

fn wide(v: u32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

impl CapIndex {
    /// The counts of `docs` documents. O(stream). `None` when a count
    /// does not fit (the caller scans).
    pub(super) fn build(docs: usize, parts: &impl Parts) -> Option<Self> {
        let mut n = Vec::with_capacity(docs);
        let mut errors = Vec::with_capacity(docs);
        let mut meaning = Vec::with_capacity(docs);
        let mut ix = Self {
            n: Fenwick(Vec::new()),
            errors: Fenwick(Vec::new()),
            total_n: 0,
            total_errors: 0,
            meaning: Vec::new(),
            dropped: BTreeMap::new(),
            firsts: BTreeSet::new(),
            dropped_count: 0,
            stale: BTreeSet::new(),
            shown: Shown::default(),
        };
        let (mut total_n, mut total_errors) = (0u32, 0u32);
        for i in 0..docs {
            let s = parts.syntax_part(i);
            let m = parts.meaning_part(i);
            let mn = [small(m.n)?, small(m.errors)?];
            let dn = small(s.n)?.checked_add(mn[0])?;
            let de = small(s.errors)?.checked_add(mn[1])?;
            total_n = total_n.checked_add(dn)?;
            total_errors = total_errors.checked_add(de)?;
            n.push(dn);
            errors.push(de);
            meaning.push(mn);
            ix.set_dropped(i, m.dropped);
        }
        ix.n = Fenwick::from_leaves(n);
        ix.errors = Fenwick::from_leaves(errors);
        ix.total_n = total_n;
        ix.total_errors = total_errors;
        ix.meaning = meaning;
        Some(ix)
    }

    /// Document `i`'s meaning checks' uncounted part is now `d`.
    fn set_dropped(&mut self, i: usize, d: Dropped) {
        if let Some(old) = self.dropped.remove(&i) {
            self.dropped_count = self.dropped_count.saturating_sub(old.count);
            if let Some(f) = old.first {
                self.firsts.remove(&(f, i));
            }
        }
        if d != Dropped::default() {
            self.dropped_count = self.dropped_count.saturating_add(d.count);
            if let Some(f) = d.first {
                self.firsts.insert((f, i));
            }
            self.dropped.insert(i, d);
        }
    }

    /// Document `i` was edited. O(log edited).
    pub(super) fn mark(&mut self, i: usize) {
        self.stale.insert(i);
    }

    /// The counts of the documents edited since the last refresh read
    /// again, and what the last list showed of them and their
    /// neighbours dropped. O(those documents and their neighbours'
    /// diagnostics). `false` when a count no longer fits (the caller
    /// drops the index).
    pub(super) fn refresh(&mut self, parts: &impl Parts) -> bool {
        let stale = std::mem::take(&mut self.stale);
        let len = self.meaning.len();
        let mut syntax = BTreeSet::new();
        for &i in stale.iter().filter(|i| **i < len) {
            syntax.extend(i.checked_sub(1));
            syntax.insert(i);
            syntax.extend(i.checked_add(1).filter(|j| *j < len));
        }
        for &i in &syntax {
            let old_m = self.meaning.get(i).copied().unwrap_or([0, 0]);
            let new_m = if stale.contains(&i) {
                let m = parts.meaning_part(i);
                self.set_dropped(i, m.dropped);
                match (small(m.n), small(m.errors)) {
                    (Some(a), Some(b)) => [a, b],
                    _ => return false,
                }
            } else {
                old_m
            };
            let s = parts.syntax_part(i);
            let (Some(sn), Some(se)) = (small(s.n), small(s.errors)) else {
                return false;
            };
            let (Some(dn), Some(de)) = (sn.checked_add(new_m[0]), se.checked_add(new_m[1])) else {
                return false;
            };
            let (was_n, was_e) = (self.n.leaf(i), self.errors.leaf(i));
            // What the last list showed of it stays exact for a neighbour
            // of one edited document whose counts did not change: only its
            // boundary with that document can have moved, and moving it
            // across one of its diagnostics changes its count.
            let both = i.checked_sub(1).is_some_and(|p| stale.contains(&p))
                && stale.contains(&i.saturating_add(1));
            if stale.contains(&i) || both || (dn, de) != (was_n, was_e) {
                self.shown.docs.remove(&i);
            }
            let moved = |total: u32, now: u32, was: u32| {
                u64::from(total)
                    .checked_add(u64::from(now))
                    .and_then(|v| v.checked_sub(u64::from(was)))
                    .and_then(|v| u32::try_from(v).ok())
            };
            let (Some(total_n), Some(total_e)) = (
                moved(self.total_n, dn, was_n),
                moved(self.total_errors, de, was_e),
            ) else {
                return false;
            };
            self.n.add(i, dn.wrapping_sub(was_n));
            self.errors.add(i, de.wrapping_sub(was_e));
            self.total_n = total_n;
            self.total_errors = total_e;
            if let Some(m) = self.meaning.get_mut(i) {
                *m = new_m;
            }
        }
        true
    }

    /// Every document's counts together. O(1).
    pub(super) fn total(&self) -> Total {
        Total {
            n: wide(self.total_n),
            errors: wide(self.total_errors),
            dropped: Dropped {
                count: self.dropped_count,
                first: self.firsts.first().map(|(f, _)| *f),
            },
        }
    }

    /// How many diagnostics document `i` lists. O(log d).
    pub(super) fn leaf_n(&self, i: usize) -> usize {
        wide(self.n.leaf(i))
    }

    /// How many errors document `i` lists. O(log d).
    pub(super) fn leaf_errors(&self, i: usize) -> usize {
        wide(self.errors.leaf(i))
    }

    /// Whether document `i`'s meaning checks found nothing (so they need
    /// not be run to list it). O(log).
    pub(super) fn meaning_empty(&self, i: usize) -> bool {
        self.meaning.get(i).is_some_and(|m| m[0] == 0) && !self.dropped.contains_key(&i)
    }

    /// The document holding diagnostic `t` (0-based, in stream order),
    /// and how many of its own come before it.
    pub(super) fn nth_diagnostic(&self, t: usize) -> Option<(usize, usize)> {
        let t = small(t)?;
        self.n
            .find(t, self.total_n)
            .map(|(d, before)| (d, wide(before)))
    }

    /// The first document from `j` on with an error.
    pub(super) fn first_error_from(&self, j: usize) -> Option<usize> {
        let before = self.errors.prefix(j);
        self.errors.find(before, self.total_errors).map(|(d, _)| d)
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

    struct Fixed(Vec<(usize, usize)>);
    impl Parts for Fixed {
        fn syntax_part(&self, i: usize) -> Agg {
            // A syntax share too, so that both parts are summed.
            Agg {
                n: i % 2,
                errors: 0,
                dropped: Dropped::default(),
            }
        }
        fn meaning_part(&self, i: usize) -> Agg {
            let (n, errors) = self.0[i];
            Agg {
                n,
                errors,
                dropped: if n > 3 {
                    Dropped::one(i * 10)
                } else {
                    Dropped::default()
                },
            }
        }
    }

    /// The counts' answers equal a scan of the documents, for every size
    /// (powers of two and not) and after updates.
    #[test]
    fn answers_equal_a_scan() {
        for d in 0..40usize {
            let mut f = Fixed((0..d).map(|i| ((i * 7) % 5, (i * 3) % 2)).collect());
            let mut ix = CapIndex::build(d, &f).unwrap();
            for round in 0..3 {
                let each = |i: usize| f.0[i].0 + i % 2;
                let total: usize = (0..d).map(each).sum();
                assert_eq!(ix.total().n, total);
                assert_eq!(ix.total().errors, f.0.iter().map(|x| x.1).sum::<usize>());
                let first = f.0.iter().position(|x| x.0 > 3).map(|i| i * 10);
                assert_eq!(ix.total().dropped.first, first);
                let count = f.0.iter().filter(|x| x.0 > 3).count();
                assert_eq!(ix.total().dropped.count, count);
                let mut t = 0;
                for i in 0..d {
                    assert_eq!(ix.leaf_n(i), each(i));
                    assert_eq!(ix.leaf_errors(i), f.0[i].1);
                    assert_eq!(ix.meaning_empty(i), f.0[i].0 == 0);
                    for k in 0..each(i) {
                        assert_eq!(ix.nth_diagnostic(t), Some((i, k)));
                        t += 1;
                    }
                }
                assert_eq!(ix.nth_diagnostic(t), None);
                for j in 0..=d {
                    let want = (j..d).find(|i| f.0[*i].1 > 0);
                    assert_eq!(ix.first_error_from(j), want, "d {d} j {j}");
                }
                // Edit some documents.
                for i in (round..d).step_by(3) {
                    f.0[i] = ((f.0[i].0 + round + 2) % 6, (f.0[i].1 + 1) % 2);
                    ix.mark(i);
                }
                assert!(ix.refresh(&f));
            }
        }
    }

    /// A count past `u32` gives no index.
    #[test]
    fn counts_past_u32_give_no_index() {
        struct Big;
        impl Parts for Big {
            fn syntax_part(&self, _: usize) -> Agg {
                Agg {
                    n: u32::MAX as usize,
                    errors: 0,
                    dropped: Dropped::default(),
                }
            }
            fn meaning_part(&self, _: usize) -> Agg {
                Agg::default()
            }
        }
        assert!(CapIndex::build(1, &Big).is_some());
        assert!(CapIndex::build(2, &Big).is_none());
    }
}
