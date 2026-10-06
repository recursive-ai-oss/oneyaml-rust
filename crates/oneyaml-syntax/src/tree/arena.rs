//! Flat preorder arenas: the base tree's arrays and the owned arenas of
//! fragments, read through one borrowed view (index loops only).

use super::stored::Stored;
use oneyaml_core::{Diagnostic, Span};

use super::{Extra, NONE, SyntaxKind, WRAP};

/// A borrowed view of one flat preorder arena (the base tree's arrays, or
/// an inserted fragment's). Every index is local to the arena.
#[derive(Clone, Copy)]
pub(super) struct Flat<'a> {
    pub(super) source: &'a str,
    /// Length of the prefix of `source` that the tokens tile; bytes past it
    /// hold copied `%TAG` prefixes (see [`Arena`]).
    pub(super) tiled: usize,
    pub(super) kinds: &'a [SyntaxKind],
    pub(super) parents: &'a [u32],
    pub(super) words: &'a [u32],
    pub(super) wraps: &'a [u32],
    pub(super) extras: &'a [(u32, Extra)],
    pub(super) diagnostics: &'a [(u32, Stored)],
}

impl<'a> Flat<'a> {
    pub(super) fn len(&self) -> usize {
        self.kinds.len()
    }

    pub(super) fn kind(&self, i: usize) -> Option<SyntaxKind> {
        self.kinds.get(i).copied()
    }

    /// The local parent; `None` for a root (the stream, or a fragment's
    /// top-level element).
    pub(super) fn parent(&self, i: usize) -> Option<usize> {
        match self.parents.get(i) {
            Some(&p) if p != NONE => Some(p as usize),
            _ => None,
        }
    }

    /// One past the last local index of `i`'s subtree.
    pub(super) fn end(&self, i: usize) -> usize {
        match self.kind(i) {
            Some(k) if !k.is_token() => self.words.get(i).map_or(i, |w| *w as usize),
            _ => i.saturating_add(1),
        }
    }

    pub(super) fn token_start(&self, i: usize) -> usize {
        let low = u64::from(self.words.get(i).copied().unwrap_or(0));
        if self.wraps.is_empty() {
            return usize::try_from(low).unwrap_or(usize::MAX);
        }
        let crossings = self.wraps.partition_point(|w| *w as usize <= i);
        let high = u64::try_from(crossings)
            .ok()
            .and_then(|c| c.checked_mul(WRAP));
        high.and_then(|h| h.checked_add(low))
            .and_then(|at| usize::try_from(at).ok())
            .unwrap_or(usize::MAX)
    }

    /// Start of the first token at local index `i` or later, else the end
    /// of the tiled bytes. The bytes of the tokens in `a..b` are exactly
    /// `start(a)..start(b)` (tokens tile the arena, C1).
    pub(super) fn start(&self, mut i: usize) -> usize {
        loop {
            match self.kind(i) {
                None => return self.tiled,
                Some(k) if k.is_token() => return self.token_start(i).min(self.tiled),
                Some(_) => i = i.saturating_add(1),
            }
        }
    }

    /// The local span of a token.
    pub(super) fn token_span(&self, i: usize) -> Span {
        let start = self.start(i);
        Span::new(start, self.start(i.saturating_add(1)).max(start))
    }

    /// The token text of local token `i`.
    pub(super) fn token_text(&self, i: usize) -> &'a str {
        let s = self.token_span(i);
        self.source.get(s.start..s.end).unwrap_or("")
    }

    pub(super) fn extra(&self, i: usize) -> Option<&'a Extra> {
        let at = self
            .extras
            .binary_search_by_key(&i, |(n, _)| *n as usize)
            .ok()?;
        self.extras.get(at).map(|(_, e)| e)
    }

    /// The diagnostics attached to local index `i`, in order.
    pub(super) fn diagnostics_at(&self, i: usize) -> &'a [(u32, Stored)] {
        let lo = self.diagnostics.partition_point(|(n, _)| (*n as usize) < i);
        let hi = self
            .diagnostics
            .partition_point(|(n, _)| (*n as usize) <= i)
            .max(lo);
        self.diagnostics.get(lo..hi).unwrap_or_default()
    }

    /// Number of diagnostics attached to local indices `a..b`. O(log d).
    pub(super) fn diagnostics_in(&self, a: u32, b: u32) -> u64 {
        let lo = self.diagnostics.partition_point(|(n, _)| *n < a);
        let hi = self.diagnostics.partition_point(|(n, _)| *n < b);
        u64::try_from(hi.saturating_sub(lo)).unwrap_or(u64::MAX)
    }

    // ------------------------------------------------------- local links

    pub(super) fn first_child(&self, i: usize) -> Option<usize> {
        let next = i.checked_add(1)?;
        (next < self.end(i)).then_some(next)
    }

    /// The last child. O(depth).
    pub(super) fn last_child(&self, i: usize) -> Option<usize> {
        let end = self.end(i);
        if end <= i.saturating_add(1) {
            return None;
        }
        let mut j = end.checked_sub(1)?;
        while let Some(p) = self.parent(j) {
            if p == i {
                return Some(j);
            }
            j = p;
        }
        None
    }

    /// The next sibling; for a root, the next root of the arena.
    pub(super) fn next_sibling(&self, i: usize) -> Option<usize> {
        let next = self.end(i);
        let limit = self.parent(i).map_or(self.len(), |p| self.end(p));
        (next < limit && next > i).then_some(next)
    }

    /// The previous sibling (for a root, the previous root). O(depth).
    pub(super) fn prev_sibling(&self, i: usize) -> Option<usize> {
        let parent = self.parent(i);
        let mut j = i.checked_sub(1)?;
        if Some(j) == parent {
            return None;
        }
        loop {
            let p = self.parent(j);
            if p == parent {
                return Some(j);
            }
            j = p?;
        }
    }

    /// The local roots, in order.
    pub(super) fn roots(self) -> impl Iterator<Item = usize> + Clone + 'a {
        let mut next = (!self.kinds.is_empty()).then_some(0usize);
        core::iter::from_fn(move || {
            let i = next?;
            next = self.next_sibling(i);
            Some(i)
        })
    }
}

/// An owned flat arena: a fragment's forest. Its roots have
/// no local parent. `source` holds the forest's bytes (tiled by its tokens)
/// followed by a copy of each `%TAG` prefix its tags were resolved with,
/// so that tags read with the one reader resolve as they did where
/// the forest came from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) struct Arena {
    pub(super) source: String,
    pub(super) tiled: usize,
    pub(super) kinds: Vec<SyntaxKind>,
    pub(super) parents: Vec<u32>,
    pub(super) words: Vec<u32>,
    pub(super) extras: Vec<(u32, Extra)>,
    pub(super) diagnostics: Vec<(u32, Stored)>,
}

impl Arena {
    pub(super) fn flat(&self) -> Flat<'_> {
        Flat {
            source: &self.source,
            tiled: self.tiled,
            kinds: &self.kinds,
            parents: &self.parents,
            words: &self.words,
            wraps: &[],
            extras: &self.extras,
            diagnostics: &self.diagnostics,
        }
    }
}

/// Moves every span of `d` by `to - from` (saturating at 0).
pub(super) fn shift_diagnostic(d: &mut Diagnostic, from: usize, to: usize) {
    let mv = |s: &mut Span| *s = shift_span(*s, from, to);
    mv(&mut d.primary.span);
    for l in &mut d.secondary {
        mv(&mut l.span);
    }
    for sg in &mut d.suggestions {
        for e in &mut sg.edits {
            mv(&mut e.span);
        }
    }
}

/// `s` moved by `to - from`.
pub(super) fn shift_span(s: Span, from: usize, to: usize) -> Span {
    let mv = |x: usize| {
        if to >= from {
            x.saturating_add(to.saturating_sub(from))
        } else {
            x.saturating_sub(from.saturating_sub(to))
        }
    };
    Span::new(mv(s.start), mv(s.end))
}
