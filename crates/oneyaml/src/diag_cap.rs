//! The diagnostics cap (`Limits::max_diagnostics`).
//!
//! **The rule.** One budget per parse or read (a whole stream), shared by
//! errors and warnings of every kind. A list keeps its first
//! `max_diagnostics` entries in the order it reports them; when there are
//! more, it also keeps the first error (the one a strict parse fails with,
//! so `has_errors` and the strict outcome never change), and one summary
//! warning, [`code::DIAGNOSTICS_SUPPRESSED`], takes the place of the first
//! suppressed entry: "N more diagnostics suppressed", N exact (saturating at
//! `usize::MAX`), its span empty at the first suppressed entry's start. The
//! summary does not count toward the limit.
//!
//! **Collecting.** A [`Capped`] collector keeps the `max_diagnostics + 1`
//! entries that **start first** (not the first to arrive: alias replay
//! reports duplicates late with early spans) in a bounded max-heap, and
//! the earliest-starting error among the others, and counts the rest with
//! where the first of them starts ([`Dropped`]): O(log limit) per
//! diagnostic, memory O(limit). The syntax tree's builder in
//! `-syntax`, whose parser reports in source order, keeps its first
//! `limit + 1` in full and the rest as fixed-size skeletons. So
//! the first `limit + 1` entries of the merged list, in source order, lie
//! within what the collectors kept, and [`cap`] then decides exactly as if
//! nothing had been dropped.

use oneyaml_core::{Diagnostic, DuplicateKeys, Label, Limits, Severity, Span, code};

use crate::error::Error;

/// What a collector counted but did not keep: how many, and where the
/// first of them starts (where a summary goes when nothing kept is past
/// the limit).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub(crate) count: usize,
    pub(crate) first: Option<usize>,
}

impl Dropped {
    /// One dropped diagnostic starting at `at`.
    pub(crate) fn one(at: usize) -> Self {
        Self {
            count: 1,
            first: Some(at),
        }
    }

    /// Both.
    #[must_use]
    pub(crate) fn and(self, other: Self) -> Self {
        let first = match (self.first, other.first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Self {
            count: self.count.saturating_add(other.count),
            first,
        }
    }
}

/// A kept entry, ordered by where it starts (then by arrival).
#[derive(Debug)]
struct Kept {
    start: usize,
    seq: u64,
    d: Diagnostic,
}

impl PartialEq for Kept {
    fn eq(&self, other: &Self) -> bool {
        (self.start, self.seq) == (other.start, other.seq)
    }
}
impl Eq for Kept {}
impl PartialOrd for Kept {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Kept {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.start, self.seq).cmp(&(other.start, other.seq))
    }
}

/// A collector of diagnostics under the cap (see the module docs).
///
/// It keeps the `keep` entries that **start first**, not the
/// first to arrive (a bounded max-heap: alias replay reports duplicates
/// late with early spans), and the earliest-starting error among the
/// others. O(log keep) per entry, O(keep) memory.
#[derive(Debug, Default)]
pub(crate) struct Capped {
    heap: std::collections::BinaryHeap<Kept>,
    /// How many to keep (`limit + 1`).
    keep: usize,
    seq: u64,
    /// The earliest-starting error not in `heap`.
    error: Option<Kept>,
    dropped: Dropped,
}

impl Capped {
    /// A collector for `limits`.
    pub(crate) fn new(limits: &Limits) -> Self {
        Self::with_keep(limits.get_max_diagnostics().saturating_add(1))
    }

    /// A collector that keeps the `keep` entries that start first.
    pub(crate) fn with_keep(keep: usize) -> Self {
        Self {
            heap: std::collections::BinaryHeap::new(),
            keep,
            seq: 0,
            error: None,
            dropped: Dropped::default(),
        }
    }

    /// Whether a warning starting at `at` pushed now would be kept
    /// (otherwise [`Capped::count`] it without building it).
    pub(crate) fn wants_warning(&self, at: usize) -> bool {
        self.heap.len() < self.keep || self.heap.peek().is_some_and(|m| at < m.start)
    }

    /// Counts a warning starting at `at` that is not kept.
    pub(crate) fn count(&mut self, at: usize) {
        self.dropped = self.dropped.and(Dropped::one(at));
    }

    /// Records `d`. O(log keep).
    pub(crate) fn push(&mut self, d: Diagnostic) {
        self.seq = self.seq.wrapping_add(1);
        let k = Kept {
            start: d.primary.span.start,
            seq: self.seq,
            d,
        };
        let out = if self.heap.len() < self.keep {
            self.heap.push(k);
            return;
        } else if self.heap.peek().is_some_and(|m| k < *m) {
            let out = self.heap.pop();
            self.heap.push(k);
            out
        } else {
            Some(k)
        };
        if let Some(k) = out {
            self.drop_entry(k);
        }
    }

    /// An entry not kept in the heap: the earliest error stays aside, the
    /// rest is counted.
    fn drop_entry(&mut self, k: Kept) {
        let error = k.d.severity == Severity::Error && self.keep > 0;
        if error && self.error.as_ref().is_none_or(|e| k < *e) {
            if let Some(old) = self.error.replace(k) {
                self.count(old.start);
            }
        } else {
            self.count(k.start);
        }
    }

    /// A duplicate key under `policy`: the `LastWins` warning (built only
    /// when it is kept), or the error.
    pub(crate) fn duplicate(
        &mut self,
        policy: DuplicateKeys,
        key: impl FnOnce() -> String,
        first: Span,
        dup: Span,
    ) -> Result<(), Error> {
        if policy == DuplicateKeys::LastWins && !self.wants_warning(dup.start) {
            self.count(dup.start);
            return Ok(());
        }
        let d = crate::construct::keys::on_duplicate(policy, &key(), first, dup)?;
        self.push(d);
        Ok(())
    }

    /// Every diagnostic counted (kept or not).
    #[cfg(test)]
    pub(crate) fn total(&self) -> usize {
        self.heap
            .len()
            .saturating_add(usize::from(self.error.is_some()))
            .saturating_add(self.dropped.count)
    }

    /// Moves the kept diagnostics out, ordered by start, with what was not
    /// kept; the collector starts over.
    pub(crate) fn take(&mut self) -> (Vec<Diagnostic>, Dropped) {
        let heap = std::mem::take(&mut self.heap);
        let mut kept: Vec<Diagnostic> = heap.into_sorted_vec().into_iter().map(|k| k.d).collect();
        if let Some(e) = self.error.take() {
            let at = kept.partition_point(|d| d.primary.span.start <= e.start);
            kept.insert(at, e.d);
        }
        (kept, std::mem::take(&mut self.dropped))
    }

    /// The kept diagnostics and what was not kept.
    pub(crate) fn into_parts(mut self) -> (Vec<Diagnostic>, Dropped) {
        self.take()
    }
}

/// A skeleton diagnostic (past `max_diagnostics` the syntax tree
/// keeps code, severity, and spans only) with texts to show: its code's
/// summary and a help naming the limit. Other diagnostics unchanged.
pub(crate) fn filled(mut d: Diagnostic, limit: usize) -> Diagnostic {
    if !oneyaml_syntax::tree::SyntaxTree::is_skeleton(&d) {
        return d;
    }
    d.message = d
        .code
        .info()
        .map_or_else(|| d.code.to_string(), |i| i.summary.to_owned());
    d.help = Some(format!(
        "its full message was not kept: the input has more diagnostics than \
         Limits::max_diagnostics (currently {limit}); raise it to see it"
    ));
    d
}

/// The summary that ends a capped list.
pub(crate) fn summary(count: usize, limit: usize, at: usize) -> Diagnostic {
    let noun = if count == 1 {
        "diagnostic"
    } else {
        "diagnostics"
    };
    Diagnostic::warning(
        code::DIAGNOSTICS_SUPPRESSED,
        format!("{count} more {noun} suppressed"),
        Label::new(Span::empty(at), "suppressed from here on"),
    )
    .with_help(format!(
        "raise Limits::max_diagnostics (currently {limit}) to see them"
    ))
}

/// Where [`cap`] put what it kept: the list's entries before it are
/// `list[..limit]`, then the summary (in the place of `list[limit]`), then
/// `list[error]` if the first error came later.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cut {
    pub(crate) limit: usize,
    pub(crate) error: Option<usize>,
}

impl Cut {
    /// How many entries of the capped list come from `list[..b]` (the
    /// summary counting as `list[limit]`'s).
    pub(crate) fn before(self, b: usize) -> usize {
        let mut n = b.min(self.limit);
        if self.limit < b {
            n = n.saturating_add(1);
        }
        if self.error.is_some_and(|e| e < b) {
            n = n.saturating_add(1);
        }
        n
    }
}

/// Applies the cap to `list` (every collected diagnostic, in the order the
/// list reports them) when `extra` more were counted by the collectors
/// but not kept. Unchanged when the total is within `limit`.
///
/// The summary goes where the first entry past the limit is: `list[limit]`
/// (the collectors keep `limit + 1` each, so the merged list has the true
/// one there), else, when the collectors dropped entries but the list is
/// short (a list of one document, say, all of whose entries the tree kept
/// as skeletons), where the first dropped one starts.
pub(crate) fn cap(list: &mut Vec<Diagnostic>, extra: Dropped, limit: usize) -> Option<Cut> {
    let total = list.len().saturating_add(extra.count);
    if total <= limit {
        return None;
    }
    let error = list
        .iter()
        .position(|d| d.severity == Severity::Error)
        .filter(|e| *e >= limit);
    let (at, internal) = match (list.get(limit), extra.first) {
        (Some(d), _) => (d.primary.span.start, None),
        (None, Some(f)) => (f, None),
        (None, None) => {
            // Unreachable: past the limit, either the list or a collector
            // holds the entry past it (`Dropped` counts only with a first
            // offset). Never a panic: in release, an in-band `Y9000` error
            // says the summary's place is not known, never a silent guess.
            debug_assert!(false, "capped list without its first suppressed entry");
            let at = list.last().map_or(0, |d| d.primary.span.end);
            let e = Error::internal(
                "capped diagnostics without their first suppressed entry",
                Some(Span::empty(at)),
            );
            (at, Some(e.diagnostic().clone()))
        }
    };
    let first_error = error.and_then(|e| list.get(e).cloned());
    list.truncate(limit);
    let kept = list
        .len()
        .saturating_add(usize::from(first_error.is_some()));
    list.push(summary(total.saturating_sub(kept), limit, at));
    list.extend(first_error);
    list.extend(internal);
    Some(Cut { limit, error })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn w(at: usize) -> Diagnostic {
        Diagnostic::warning(
            code::LENIENT_INLINE_COLON,
            "w",
            Label::new(Span::empty(at), ""),
        )
    }

    /// The unreachable case (more counted than kept, but no first
    /// offset) is an in-band `Y9000` in release, a debug assertion in
    /// debug builds.
    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "capped list without its first suppressed entry")
    )]
    fn missing_first_offset_is_internal() {
        let mut l = vec![w(1)];
        let extra = Dropped {
            count: 3,
            first: None,
        };
        cap(&mut l, extra, 1).unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[1].code, code::DIAGNOSTICS_SUPPRESSED);
        assert_eq!(l[2].code, code::INTERNAL_ERROR);
        assert_eq!(l[2].severity, Severity::Error);
    }

    fn e(at: usize) -> Diagnostic {
        Diagnostic::error(code::INVALID_ESCAPE, "e", Label::new(Span::empty(at), ""))
    }

    #[test]
    fn within_the_limit_nothing_changes() {
        let mut l = vec![w(0), w(1)];
        assert!(cap(&mut l, Dropped::default(), 2).is_none());
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn one_over_keeps_the_limit_and_a_summary() {
        let mut l = vec![w(0), w(1), w(2)];
        let cut = cap(&mut l, Dropped::default(), 2).unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[2].code, code::DIAGNOSTICS_SUPPRESSED);
        assert_eq!(l[2].message, "1 more diagnostic suppressed");
        assert_eq!(l[2].primary.span, Span::empty(2));
        assert_eq!(l[2].severity, Severity::Warning);
        assert_eq!(cut.before(3), 3);
        assert_eq!(cut.before(1), 1);
    }

    #[test]
    fn the_first_error_is_kept_and_counts_are_exact() {
        let mut l = vec![w(0), w(1), w(2), e(3), e(4)];
        let cut = cap(
            &mut l,
            Dropped {
                count: 7,
                first: Some(9),
            },
            1,
        )
        .unwrap();
        assert_eq!(l.len(), 3);
        assert_eq!(l[1].message, "10 more diagnostics suppressed");
        assert_eq!(l[2].primary.span, Span::empty(3));
        assert_eq!(cut.before(5), 3);
        assert_eq!(cut.before(3), 2);
    }

    #[test]
    fn a_short_list_places_the_summary_at_the_first_dropped_entry() {
        let mut l = vec![w(0)];
        cap(
            &mut l,
            Dropped {
                count: 3,
                first: Some(40),
            },
            2,
        )
        .unwrap();
        assert_eq!(l[1].primary.span, Span::empty(40));
        assert_eq!(l[1].message, "3 more diagnostics suppressed");
    }

    #[test]
    fn skeletons_are_filled_with_their_summary() {
        let mut d = e(3);
        d.message = String::new();
        let f = filled(d, 7);
        assert_eq!(
            f.message,
            "invalid escape sequence in a double-quoted scalar"
        );
        assert!(f.help.unwrap().contains("currently 7"));
        assert_eq!(filled(w(1), 7).message, "w");
    }

    #[test]
    fn a_limit_of_zero_keeps_only_the_summary_and_the_first_error() {
        let mut l = vec![w(0), e(1)];
        cap(&mut l, Dropped::default(), 0).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].code, code::DIAGNOSTICS_SUPPRESSED);
        assert_eq!(l[1].severity, Severity::Error);
    }

    #[test]
    fn collectors_keep_limit_plus_one_and_the_first_error() {
        let mut c = Capped::new(&Limits::default().max_diagnostics(1));
        for i in 0..5 {
            c.push(w(i));
        }
        c.push(e(5));
        c.push(e(6));
        let (items, dropped) = c.into_parts();
        assert_eq!(items.len(), 3);
        assert_eq!(
            dropped,
            Dropped {
                count: 4,
                first: Some(2)
            }
        );
        let mut n = Capped::with_keep(0);
        n.push(e(0));
        n.push(w(1));
        assert_eq!(n.total(), 2);
        assert_eq!(n.into_parts().0.len(), 0);
    }
}
