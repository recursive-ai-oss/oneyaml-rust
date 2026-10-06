//! What each arena knows about its stream-level elements (cheap to keep, and
//! read the same by every edit), computed once when the arena enters the tree:
//! at build time for the parsed arrays, at insertion for a fragment inserted
//! into the stream (O(its roots + its diagnostics), which building it already
//! cost). Arenas are immutable, so the facts never go stale; a tree without
//! long trivia runs, directive errors, or stream-level content outside
//! documents keeps two empty `Vec`s and no document list (no allocation: an
//! unedited tree does not grow).
//!
//! * **Long trivia runs**: maximal runs of consecutive stream-level trivia
//!   tokens longer than [`LONG_RUN`]. A walk back over stream children
//!   (`SyntaxTree::after_document`) jumps over such a run in O(log) instead
//!   of stepping over each comment.
//! * **Directive errors**: documents with an error among their directives
//!   (`%YAML 2.0`, a duplicate `%YAML` or `%TAG`: the parser's soft errors,
//!   which are not followed by an error region, so warnings of later
//!   directives come after them). Every other error ends its document's
//!   content (C6), which is what lets `SyntaxTree::has_errors_within` read
//!   only a subtree's last diagnostic.
//! * **Documents**: the comment attachment asks for the
//!   documents around a stray `...` or a stream-level error region
//!   (`runs.rs`). An arena holding such stream-level content records its
//!   documents (one exactly sized list), so they are found in O(log)
//!   however many stray elements lie between. Any other arena records
//!   nothing (the allocation gate: an ordinary multi-document
//!   stream allocates nothing more than before): between two of its
//!   documents lie only trivia tokens, so the nearest document is found
//!   by a walk over at most [`LONG_RUN`] of them, the long runs jumped
//!   (O(depth + `LONG_RUN` + log)).

use oneyaml_core::Severity;

use super::SyntaxKind as K;
use super::arena::Flat;

/// Trivia runs up to this many elements are walked step by step (the
/// validation's own `TRIVIA_WALK`); longer ones are recorded.
pub(super) const LONG_RUN: usize = 64;

/// See the module docs. Local indices of one arena.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct StreamFacts {
    /// `(a, b)`: local indices `a..b` are consecutive stream-level trivia
    /// tokens, `b - a > LONG_RUN`, and the run is maximal. Sorted.
    trivia_runs: Vec<(u32, u32)>,
    /// Documents with an error among their directives. Sorted.
    directive_errors: Vec<u32>,
    /// The documents where an error without a
    /// directive code (`validate::is_directive_error`) comes before one
    /// with it (a failed directive block, `Y0001`, kept with the next
    /// document's directives). Sorted; empty for ordinary input.
    mixed_directive_errors: Vec<u32>,
    /// The stream-level documents (sorted; see `runs.rs`),
    /// recorded only when a stream-level element is neither a document
    /// nor trivia (a stray `...`, an error region); `None` otherwise (the
    /// documents are then found by a walk, see the module docs).
    docs: Option<Vec<u32>>,
}

const fn is_directive(k: K) -> bool {
    matches!(k, K::YamlDirective | K::TagDirective | K::ReservedDirective)
}

impl StreamFacts {
    /// Heap bytes of the tables, by capacity (for the overlay's estimate,
    /// `Edits::heap_bytes`).
    #[cfg(test)]
    pub(super) fn heap_bytes(&self) -> usize {
        self.trivia_runs
            .capacity()
            .saturating_mul(size_of::<(u32, u32)>())
            .saturating_add(
                self.directive_errors
                    .capacity()
                    .saturating_mul(size_of::<u32>()),
            )
            .saturating_add(
                self.mixed_directive_errors
                    .capacity()
                    .saturating_mul(size_of::<u32>()),
            )
            .saturating_add(
                self.docs
                    .as_ref()
                    .map_or(0, |d| d.capacity().saturating_mul(size_of::<u32>())),
            )
    }

    /// The facts of `flat`, whose stream-level elements are `children`
    /// (in order): the stream's children for parsed arrays, the roots of
    /// a fragment inserted into the stream; `errors`: `flat` has an error
    /// diagnostic. O(children + diagnostics of documents, when `errors`);
    /// Loops only.
    pub(super) fn of(
        flat: Flat<'_>,
        children: impl Iterator<Item = usize> + Clone,
        errors: bool,
    ) -> Self {
        let mut facts = Self::default();
        // Allocations: the document list is kept
        // only next to stream-level content outside documents; the
        // documents are counted on the way and collected afterwards, so it
        // is one exactly sized allocation, and other arenas are read once.
        let (mut docs, mut stray) = (0usize, false);
        let all = children.clone();
        let mut run: Option<(usize, usize)> = None;
        let close = |run: Option<(usize, usize)>, out: &mut Vec<(u32, u32)>| {
            if let Some((a, b)) = run {
                if b.saturating_sub(a) > LONG_RUN {
                    if let (Ok(a), Ok(b)) = (u32::try_from(a), u32::try_from(b)) {
                        out.push((a, b));
                    }
                }
            }
        };
        for i in children {
            let Some(k) = flat.kind(i) else { break };
            if k.is_trivia() {
                run = match run {
                    Some((a, b)) if b == i => Some((a, i.saturating_add(1))),
                    other => {
                        close(other, &mut facts.trivia_runs);
                        Some((i, i.saturating_add(1)))
                    }
                };
                continue;
            }
            close(run.take(), &mut facts.trivia_runs);
            if k == K::Document {
                docs = docs.saturating_add(1);
            } else {
                stray = true;
            }
            if errors && k == K::Document {
                if let Ok(j) = u32::try_from(i) {
                    if Self::has_directive_error(flat, i) {
                        facts.directive_errors.push(j);
                    }
                    if Self::mixed(flat, i) {
                        facts.mixed_directive_errors.push(j);
                    }
                }
            }
        }
        close(run, &mut facts.trivia_runs);
        if stray {
            let mut list = Vec::with_capacity(docs);
            list.extend(
                all.filter(|i| flat.kind(*i) == Some(K::Document))
                    .filter_map(|i| u32::try_from(i).ok()),
            );
            facts.docs = Some(list);
        }
        facts
    }

    /// Document `doc` has an error attached to one of its children that a
    /// `---` marker follows (only directives, trivia, and errors between).
    fn has_directive_error(flat: Flat<'_>, doc: usize) -> bool {
        let end = flat.end(doc);
        let list = flat.diagnostics;
        let lo = list.partition_point(|(n, _)| (*n as usize) < doc);
        let hi = list.partition_point(|(n, _)| (*n as usize) < end).max(lo);
        for (e, d) in list.get(lo..hi).unwrap_or_default() {
            let e = *e as usize;
            if d.severity() != Severity::Error || flat.parent(e) != Some(doc) {
                continue;
            }
            let mut at = Some(e);
            while let Some(x) = at {
                match flat.kind(x) {
                    Some(K::DocumentStartMarker) => return true,
                    Some(k) if is_directive(k) || k.is_trivia() || k == K::Error => {}
                    _ => break,
                }
                at = flat.next_sibling(x);
            }
        }
        false
    }

    /// In document `doc`'s diagnostics (in order), an error
    /// without a directive code comes before one with it. O(diagnostics of
    /// `doc`).
    fn mixed(flat: Flat<'_>, doc: usize) -> bool {
        let end = flat.end(doc);
        let list = flat.diagnostics;
        let lo = list.partition_point(|(n, _)| (*n as usize) < doc);
        let hi = list.partition_point(|(n, _)| (*n as usize) < end).max(lo);
        let mut other = false;
        for (_, d) in list.get(lo..hi).unwrap_or_default() {
            if d.severity() != Severity::Error {
                continue;
            }
            if super::validate::is_directive_error(d.code()) {
                if other {
                    return true;
                }
            } else {
                other = true;
            }
        }
        false
    }

    /// The first local index of the long trivia run holding `local`.
    pub(super) fn run_start(&self, local: usize) -> Option<usize> {
        let k = self
            .trivia_runs
            .partition_point(|(a, _)| (*a as usize) <= local);
        let (a, b) = *self.trivia_runs.get(k.checked_sub(1)?)?;
        (local < b as usize).then_some(a as usize)
    }

    /// The last local index of the long trivia run holding `local`
    /// (the forward walk jumps over it).
    pub(super) fn run_end(&self, local: usize) -> Option<usize> {
        let k = self
            .trivia_runs
            .partition_point(|(a, _)| (*a as usize) <= local);
        let (_, b) = *self.trivia_runs.get(k.checked_sub(1)?)?;
        let b = (b as usize).checked_sub(1)?;
        (local <= b).then_some(b)
    }

    /// The stream-level element of `flat` holding local `x` (`x` itself at
    /// stream level); `None` for the stream. O(depth).
    fn top(flat: Flat<'_>, mut x: usize) -> Option<usize> {
        // The stream's children for parsed arrays, the roots otherwise.
        let stream = (flat.kind(0) == Some(K::Stream)).then_some(0usize);
        let mut guard = flat.len();
        // A climb.
        while guard > 0 {
            guard = guard.saturating_sub(1);
            match flat.parent(x) {
                p if p == stream => return (Some(x) != stream).then_some(x),
                Some(p) => x = p,
                None => return None,
            }
        }
        None
    }

    /// The arena keeps a document list (tests).
    #[cfg(test)]
    pub(super) const fn has_doc_list(&self) -> bool {
        self.docs.is_some()
    }

    /// A stream-level document of `flat` starts at local indices `a..b`.
    pub(super) fn holds_doc(&self, flat: Flat<'_>, a: u32, b: u32) -> bool {
        self.first_doc_in(flat, a, b).is_some()
    }

    /// The last stream-level document of `flat` starting at local indices
    /// `a..b`. O(log D) from the list; without one, O(depth + `LONG_RUN` +
    /// log): a walk back over the trivia tokens between documents.
    pub(super) fn last_doc_in(&self, flat: Flat<'_>, a: u32, b: u32) -> Option<u32> {
        if let Some(list) = &self.docs {
            let hi = list.partition_point(|d| *d < b);
            return list.get(hi.checked_sub(1)?).copied().filter(|d| *d >= a);
        }
        let a = a as usize;
        let b = (b as usize).min(flat.len());
        if b <= a {
            return None;
        }
        let mut t = Self::top(flat, b.checked_sub(1)?)?;
        let mut guard = flat.len();
        // A loop back over stream-level elements (trivia only between
        // documents in an arena without a list; long runs jumped).
        while guard > 0 && t >= a {
            guard = guard.saturating_sub(1);
            let k = flat.kind(t)?;
            if k == K::Document {
                return u32::try_from(t).ok();
            }
            let first = if k.is_trivia() {
                self.run_start(t).unwrap_or(t)
            } else {
                t
            };
            t = Self::top(flat, first.checked_sub(1)?)?;
        }
        None
    }

    /// The first stream-level document of `flat` starting at local indices
    /// `a..b`. Costs as [`StreamFacts::last_doc_in`].
    pub(super) fn first_doc_in(&self, flat: Flat<'_>, a: u32, b: u32) -> Option<u32> {
        if let Some(list) = &self.docs {
            let lo = list.partition_point(|d| *d < a);
            return list.get(lo).copied().filter(|d| *d < b);
        }
        let a = a as usize;
        let b = (b as usize).min(flat.len());
        if b <= a {
            return None;
        }
        // The first stream-level element starting at `a` or later.
        let mut c = match Self::top(flat, a) {
            Some(t) if t == a => a,
            Some(t) => flat.end(t),
            // `a` is the stream: its first child.
            None => a.saturating_add(1),
        };
        let mut guard = flat.len();
        // A loop over stream-level elements.
        while guard > 0 && c < b {
            guard = guard.saturating_sub(1);
            let k = flat.kind(c)?;
            if k == K::Document {
                return u32::try_from(c).ok();
            }
            let last = if k.is_trivia() {
                self.run_end(c).unwrap_or(c)
            } else {
                c
            };
            c = flat.end(last).max(last.saturating_add(1));
        }
        None
    }

    /// Document `local` has an error without a directive code
    /// before one with it (see `mixed_directive_errors`).
    pub(super) fn mixed_directive_error(&self, local: usize) -> bool {
        u32::try_from(local).is_ok_and(|l| self.mixed_directive_errors.binary_search(&l).is_ok())
    }

    /// Document `local` has an error among its directives.
    pub(super) fn directive_error(&self, local: usize) -> bool {
        u32::try_from(local).is_ok_and(|l| self.directive_errors.binary_search(&l).is_ok())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::{K, StreamFacts};
    use crate::tree::SyntaxTree;

    #[test]
    fn long_runs_and_directive_errors_are_recorded() {
        let src = format!("a: 1\n{}--- b\n", "# c\n".repeat(40));
        let t = SyntaxTree::parse(src);
        // 80 trivia tokens (a comment and a break per line) and the break
        // after `a: 1`: one run.
        assert_eq!(t.facts.trivia_runs.len(), 1);
        let (a, b) = t.facts.trivia_runs[0];
        assert_eq!(b - a, 81);
        assert_eq!(t.facts.run_start(b as usize - 1), Some(a as usize));
        assert_eq!(t.facts.run_start(b as usize), None);
        assert_eq!(t.facts.run_end(a as usize), Some(b as usize - 1));
        assert_eq!(t.facts.run_end(b as usize - 1), Some(b as usize - 1));
        assert_eq!(t.facts.run_end(b as usize), None);
        assert_eq!(t.facts.run_end(a as usize - 1), None);
        assert!(t.facts.directive_errors.is_empty());
        let short = SyntaxTree::parse("a: 1\n# c\n--- b\n");
        assert!(short.facts.trivia_runs.is_empty());
        for (src, n) in [
            ("%YAML 2.0\n%FOO bar\n---\na: 1\n", 1),
            ("%TAG !a! x\n%TAG !a! y\n%FOO\n--- a\n", 1),
            ("%YAML 1.2\n---\na: [1\n", 0),
            ("--- a\n...\n%YAML 2.0\n--- b\n", 1),
        ] {
            let t = SyntaxTree::parse(src);
            assert_eq!(t.facts.directive_errors.len(), n, "{src:?}");
        }
    }

    /// The allocation gate: an ordinary multi-document
    /// stream keeps no document list; the walk that stands in for it finds
    /// the same documents as the list and as a scan over the stream's
    /// children, for every range, also next to stray `...` markers and
    /// stream-level error regions (where the list is kept).
    #[test]
    fn documents_are_found_with_and_without_a_list() {
        let long = "# between\n".repeat(80);
        let inputs = [
            String::new(),
            "# only a comment\n".to_owned(),
            "a: 1\n".to_owned(),
            "a: 1\n---\nb: 2\n--- c\n".to_owned(),
            format!("%YAML 1.2\n--- a\n...\n{long}--- b\n...\n# t\n"),
            format!("\u{feff}--- a\n{long}\n---\n- b\n...\n{long}"),
            format!("a\n...\n...\n# x\n{long}...\n--- b\n...\n"),
            "a: 1\n...\n]\n# c\n--- b\n".to_owned(),
        ];
        let (mut with_list, mut multi_without) = (0, 0);
        for src in &inputs {
            let t = SyntaxTree::parse(src.as_str());
            let flat = t.base_flat();
            let n = flat.len();
            let kids: Vec<usize> = flat
                .roots()
                .flat_map(|r| {
                    let mut out = Vec::new();
                    let mut c = flat.first_child(r);
                    while let Some(x) = c {
                        out.push(x);
                        c = flat.next_sibling(x);
                    }
                    out
                })
                .collect();
            let docs: Vec<u32> = kids
                .iter()
                .filter(|c| flat.kind(**c) == Some(K::Document))
                .map(|c| *c as u32)
                .collect();
            let stray = kids.iter().any(|c| {
                flat.kind(*c)
                    .is_some_and(|k| k != K::Document && !k.is_trivia())
            });
            assert_eq!(t.facts.docs.is_some(), stray, "{src:?}");
            with_list += usize::from(stray);
            multi_without += usize::from(!stray && docs.len() > 1);
            if !stray {
                assert_eq!(t.facts.heap_bytes() - t.facts.trivia_runs.capacity() * 8, 0);
            }
            // The walk alone, also where the list is kept.
            let walk = StreamFacts {
                docs: None,
                ..t.facts.clone()
            };
            let n = u32::try_from(n).unwrap();
            for a in 0..=n {
                for b in (a..=n).step_by(((n as usize) / 50).max(1)).chain([n]) {
                    let first = docs.iter().copied().find(|d| a <= *d && *d < b);
                    let last = docs.iter().copied().rev().find(|d| a <= *d && *d < b);
                    for f in [&t.facts, &walk] {
                        assert_eq!(f.first_doc_in(flat, a, b), first, "{src:?} {a}..{b}");
                        assert_eq!(f.last_doc_in(flat, a, b), last, "{src:?} {a}..{b}");
                        assert_eq!(f.holds_doc(flat, a, b), first.is_some());
                    }
                }
            }
        }
        assert!(
            with_list >= 2 && multi_without >= 3,
            "{with_list} {multi_without}"
        );
    }
}
