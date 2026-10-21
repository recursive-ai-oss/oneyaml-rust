//! Edits at a line's end, with a
//! byte-identical round trip.
//!
//! A block scalar's text ends with the line break that ends its last line
//! (and, kept with `|+`/`>+`, the blank lines after it): that break is
//! part of its token. Any other content ends before its line break, which
//! is a [`SyntaxKind::LineBreak`] token of its own, placed at the
//! outermost level that continues after it (C4). So replacing a block
//! scalar by other content, or other content by a block scalar, moves a
//! line break across levels, which one splice under one parent cannot do.
//!
//! The two primitives here take text that **ends its line** (a fragment
//! whose text ends with a line break: a block scalar's, or a final
//! [`SyntaxKind::LineBreak`] element) and put it in place of, or after, an
//! element range that ends its line, placing the break where the parser
//! does. They are one or two splices applied as one edit, each checked
//! for well-formedness on the result and undone together when refused
//! (see [`SyntaxTree::splice_pair`](super::SyntaxTree)).

use super::edit::{Replaced, SpliceArgs, token_fragment};
use super::{Fragment, Inserted, NodeId, SyntaxKind, SyntaxTree, TreeEditError};

/// Where the line an element range ends on ends.
#[derive(Clone, Copy)]
enum LineEnd {
    /// In the range's last token (a block scalar's text).
    Inner,
    /// A line break token right after the range, at any level.
    Break(u32),
    /// The end of the text (no line break).
    Eof,
}

/// `with` holds only white space, comments, and line breaks (that is,
/// lines of trivia, which the parser places at the outermost level that
/// continues after the content before them, C4).
fn trivia_lines(with: &Fragment) -> bool {
    with.root_kinds().all(|k| {
        matches!(
            k,
            SyntaxKind::Whitespace | SyntaxKind::Comment | SyntaxKind::LineBreak
        )
    })
}

impl SyntaxTree {
    /// How the line that element `last` ends on ends.
    fn line_end_after(&self, last: u32) -> Result<LineEnd, TreeEditError> {
        // The range's text ends with a line break (a block scalar's, even
        // an empty one whose last token has no bytes).
        let (start, end) = self.nav_bytes(last).ok_or(TreeEditError::Internal)?;
        if end > start {
            let b = end.checked_sub(1).and_then(|i| self.byte_at(i));
            if matches!(b, Some(b'\n' | b'\r')) {
                return Ok(LineEnd::Inner);
            }
        }
        match self.nav_after(last) {
            None => Ok(LineEnd::Eof),
            Some(c) => {
                let at = self.nav_bytes(c).ok_or(TreeEditError::Internal)?.0;
                if self.kind_of(c) == Some(SyntaxKind::LineBreak) && at == end {
                    Ok(LineEnd::Break(c))
                } else {
                    Err(TreeEditError::NotWellFormed)
                }
            }
        }
    }

    /// Where a line break ending content that ends right before `cut`
    /// goes: before `cut` under its parent, or at the end of the stream.
    fn outer_place(&self, cut: Option<u32>) -> Result<(u32, Option<u32>), TreeEditError> {
        match cut {
            Some(c) => Ok((
                self.nav_parent(c).ok_or(TreeEditError::Internal)?,
                self.nav_prev(c),
            )),
            None => Ok((0, self.nav_last(0))),
        }
    }

    /// The inserted span without its last element when `dropped` (a line
    /// break that went in with it).
    fn body_span(
        &self,
        span: Option<(u32, u32)>,
        dropped: bool,
    ) -> Result<Inserted, TreeEditError> {
        let (f, l) = span.ok_or(TreeEditError::Internal)?;
        let l = if dropped && f != l {
            self.nav_prev(l).ok_or(TreeEditError::Internal)?
        } else {
            l
        };
        Ok(Inserted {
            first: self.id_of(f),
            last: self.id_of(l),
        })
    }

    /// Replaces the siblings `first..=last`, **through the end of their
    /// line**, by `with`, whose text must end with a line break: the text
    /// becomes the text before `first`, then `with`'s text, then the text
    /// after the line break that ended the range's line (block scalars replaced or inserted at the end of a
    /// collection or document).
    ///
    /// The range's line ends inside its last token (a block scalar's text
    /// ends with its line break), at a [`SyntaxKind::LineBreak`] token
    /// right after it (at whatever level the parser put it), or at the end
    /// of the text. `with` ends its line inside its last token (a block
    /// scalar), or with a final [`SyntaxKind::LineBreak`] element, which
    /// goes where the parser puts it: after the collections the new
    /// content ends, before what followed the old line. So `a: |\n  x\n`
    /// with the scalar replaced by `1\n` gives `a: 1\n`, and `a: 1\n`
    /// with `1` replaced by `|\n  x\n` gives `a: |\n  x\n`.
    ///
    /// One or two splices, applied and checked for well-formedness as one
    /// edit (refused, the tree is unchanged). O(depth + log R +
    /// removed + inserted roots + windows). `inserted` names the
    /// fragment's elements without a final line break element.
    ///
    /// # Errors
    ///
    /// Those of [`SyntaxTree::replace_range`];
    /// [`TreeEditError::NotWellFormed`] also when the range's line goes on
    /// after it (e.g. a comment follows) or `with`'s text does not end
    /// with a line break; [`TreeEditError::EmptyFragment`] for a fragment
    /// that is only a line break.
    pub fn replace_to_line_end(
        &mut self,
        first: NodeId,
        last: NodeId,
        with: Fragment,
    ) -> Result<Replaced, TreeEditError> {
        let (f, l) = (self.live_slot(first)?, self.live_slot(last)?);
        let (parent, roots) = self.sibling_range(f, l)?;
        let pk = self.check_parent(parent)?;
        self.check_removable(&roots)?;
        if !with.ends_line() {
            return Err(TreeEditError::NotWellFormed);
        }
        let (body, brk) = with.split_last_break()?;
        if body.is_empty() || body.root_kinds().all(|k| k == SyntaxKind::LineBreak) {
            return Err(TreeEditError::EmptyFragment);
        }
        self.check_insertable(parent, pk, &body)?;
        let end = self.line_end_after(l)?;
        let prev = self.nav_prev(f);
        let cut = self.nav_after(l);
        let had_break = brk.is_some();
        // The old line end, when it is a token of its own, goes; the new
        // one (a final break element) goes where content ending before
        // `cut` puts it.
        let (removed_break, outer_parent) = match end {
            LineEnd::Break(b) => (Some(b), self.nav_parent(b)),
            LineEnd::Inner | LineEnd::Eof => (None, cut.map_or(Some(0), |c| self.nav_parent(c))),
        };
        if outer_parent == Some(parent) || (removed_break.is_none() && brk.is_none()) {
            // One splice: the break (if any) is a sibling of the range, or
            // nothing moves across levels.
            let mut removed = roots;
            removed.extend(removed_break);
            let fragment = match brk {
                Some(b) => body.concat(b)?,
                None => body,
            };
            let (taken, span) = self.splice(parent, prev, &removed, Some(fragment), true)?;
            let mut taken = taken.ok_or(TreeEditError::Internal)?;
            if removed_break.is_some() {
                taken = taken.split_last_break()?.0;
            }
            return Ok(Replaced {
                removed: taken,
                inserted: self.body_span(span, had_break)?,
            });
        }
        let first_args = SpliceArgs {
            parent,
            prev,
            removed: roots,
            fragment: Some(body),
        };
        let ((taken, span), _) = self.splice_pair(first_args, move |t, _| {
            Ok(Some(match removed_break {
                Some(b) => SpliceArgs {
                    parent: t.nav_parent(b).ok_or(TreeEditError::Internal)?,
                    prev: t.nav_prev(b),
                    removed: vec![b],
                    fragment: brk,
                },
                None => {
                    let (p, before) = t.outer_place(cut)?;
                    SpliceArgs {
                        parent: p,
                        prev: before,
                        removed: Vec::new(),
                        fragment: brk,
                    }
                }
            }))
        })?;
        Ok(Replaced {
            removed: taken.ok_or(TreeEditError::Internal)?,
            inserted: self.body_span(span, false)?,
        })
    }

    /// Removes the siblings `first..=last`, **through the end of their
    /// line**, and puts `trivia` (white space, comments, and line breaks
    /// only, ending with a line break; `None`: nothing) where the parser
    /// puts trivia after
    /// the content before `first` (freeze gate:
    /// removing the last entry of a block collection whose value is a
    /// block scalar, as in a GitHub Actions `run: |` step or a ConfigMap's
    /// `data`).
    ///
    /// The range's line ends inside its last token (a block scalar's text)
    /// or at a [`SyntaxKind::LineBreak`] token right after it, which goes
    /// too. `trivia` is typically the line end of the content before the
    /// range (its same-line comment and line break): when the range is the
    /// end of a collection, that trivia cannot stay inside it (the parser
    /// never ends a node with trivia, C4) and goes after the collections
    /// the content before `first` ends, before what followed the range's
    /// line (or at the end of the stream). So in `a: 1\nb: |\n  x\n` the
    /// siblings from the line break after `1` through the block scalar,
    /// removed with `trivia` = `\n`, give `a: 1\n` with the break after the
    /// mapping; and in `o:\n  a: |+\n    x\n\n  b: 1\nz: 2\n` the siblings
    /// from the indentation of `b` through `1`, removed with `None`, give
    /// `o:\n  a: |+\n    x\n\nz: 2\n` (the break after `1`, outside the
    /// inner mapping, goes too: left alone, the kept scalar would read it).
    ///
    /// One or two splices, applied and checked for well-formedness as one
    /// edit (refused, the tree is unchanged). Returns the removed
    /// siblings (without the old line break token).
    ///
    /// # Errors
    ///
    /// Those of [`SyntaxTree::remove_range`];
    /// [`TreeEditError::NotWellFormed`] also when the range's line goes on
    /// after it or ends the text without a line break, or `trivia` holds
    /// anything but white space, comments, and line breaks, or does not
    /// end with a line break.
    pub fn remove_to_line_end(
        &mut self,
        first: NodeId,
        last: NodeId,
        trivia: Option<Fragment>,
    ) -> Result<Fragment, TreeEditError> {
        let (f, l) = (self.live_slot(first)?, self.live_slot(last)?);
        let (parent, roots) = self.sibling_range(f, l)?;
        let parent_kind = self.check_parent(parent)?;
        self.check_removable(&roots)?;
        if trivia
            .as_ref()
            .is_some_and(|t| !t.ends_line() || !trivia_lines(t))
        {
            return Err(TreeEditError::NotWellFormed);
        }
        let with = trivia;
        let prev = self.nav_prev(f);
        // The old line end: inside the range (`None`), or a line break
        // token right after it, which `with` replaces.
        let (removed_break, cut) = match self.line_end_after(l)? {
            LineEnd::Inner | LineEnd::Eof => (None, self.nav_after(l)),
            LineEnd::Break(b) => (Some(b), Some(b)),
        };
        let outer = cut.map_or(Some(0), |c| self.nav_parent(c));
        // One splice: the break (if any) and the trivia's place are under
        // the range's parent, or there is neither.
        if outer == Some(parent) || (removed_break.is_none() && with.is_none()) {
            if let Some(w) = &with {
                self.check_insertable(parent, parent_kind, w)?;
            }
            let mut removed = roots;
            removed.extend(removed_break.filter(|_| outer == Some(parent)));
            let (taken, _) = self.splice(parent, prev, &removed, with, true)?;
            let mut taken = taken.ok_or(TreeEditError::Internal)?;
            if removed_break.is_some() {
                taken = taken.split_last_break()?.0;
            }
            return Ok(taken);
        }
        if let Some(w) = &with {
            let o = outer.ok_or(TreeEditError::Internal)?;
            let outer_kind = self.kind_of(o).ok_or(TreeEditError::Internal)?;
            self.check_insertable(o, outer_kind, w)?;
        }
        let first_args = SpliceArgs {
            parent,
            prev,
            removed: roots,
            fragment: None,
        };
        let ((taken, _), _) = self.splice_pair(first_args, move |t, _| {
            Ok(Some(match removed_break {
                Some(b) => SpliceArgs {
                    parent: t.nav_parent(b).ok_or(TreeEditError::Internal)?,
                    prev: t.nav_prev(b),
                    removed: vec![b],
                    fragment: with,
                },
                None => {
                    let (p, before) = t.outer_place(cut)?;
                    SpliceArgs {
                        parent: p,
                        prev: before,
                        removed: Vec::new(),
                        fragment: with,
                    }
                }
            }))
        })?;
        taken.ok_or(TreeEditError::Internal)
    }

    /// Gives the block scalar that ends the text its final line break: the
    /// scalar `sibling` ends (`a: |-\n  x` at the end of the input) has a
    /// last line that does not end, so nothing can follow it. Its text gets
    /// `brk` (a line break: `\n`, `\r\n` or `\r`), after the white space
    /// that ends the input behind it, if any (that white space is the text
    /// of one more line of the scalar then: `- |+\n   ` becomes
    /// `- |+\n   \n`). Lines can then go after it with
    /// [`SyntaxTree::insert_line_after`].
    ///
    /// One or two splices applied and checked as one edit.
    ///
    /// # Errors
    ///
    /// [`TreeEditError::NotWellFormed`] when `sibling` does not end the text
    /// in a block scalar without a final line break, or `brk` is not a line
    /// break; [`TreeEditError::Stale`].
    pub fn close_block_scalar(&mut self, sibling: NodeId, brk: &str) -> Result<(), TreeEditError> {
        if !matches!(brk, "\n" | "\r\n" | "\r") {
            return Err(TreeEditError::NotWellFormed);
        }
        let s = self.live_slot(sibling)?;
        // White space ends the input behind the scalar, or nothing does.
        let ws = match self.nav_after(s) {
            None => None,
            Some(w)
                if self.kind_of(w) == Some(SyntaxKind::Whitespace)
                    && self.nav_after(w).is_none() =>
            {
                Some(w)
            }
            Some(_) => return Err(TreeEditError::NotWellFormed),
        };
        let tok = self.nav_last_leaf(s).ok_or(TreeEditError::NotWellFormed)?;
        let kind = self.kind_of(tok).ok_or(TreeEditError::Internal)?;
        if !matches!(kind, SyntaxKind::LiteralScalar | SyntaxKind::FoldedScalar) {
            return Err(TreeEditError::NotWellFormed);
        }
        let mut text = super::Node::new(self, tok).text().into_owned();
        if text.ends_with(['\n', '\r']) {
            return Err(TreeEditError::NotWellFormed);
        }
        if let Some(w) = ws {
            text.push_str(&super::Node::new(self, w).text());
        }
        // The block scalar's header, which the content token is read after.
        let scalar = self.nav_parent(tok).ok_or(TreeEditError::Internal)?;
        let mut header = None;
        let mut at = self.nav_first(scalar);
        while let Some(x) = at {
            if self.kind_of(x) == Some(SyntaxKind::BlockScalarHeader) {
                header = Some(super::Node::new(self, x).text().into_owned());
                break;
            }
            at = self.nav_next(x);
        }
        let header = header.ok_or(TreeEditError::NotWellFormed)?;
        let closed = block_token_fragment(kind, &header, &text, brk, &self.settings)?;
        let first_args = SpliceArgs {
            parent: scalar,
            prev: self.nav_prev(tok),
            removed: vec![tok],
            fragment: Some(closed),
        };
        self.splice_pair(first_args, move |t, _| {
            Ok(match ws {
                Some(w) => Some(SpliceArgs {
                    parent: t.nav_parent(w).ok_or(TreeEditError::Internal)?,
                    prev: t.nav_prev(w),
                    removed: vec![w],
                    fragment: None,
                }),
                None => None,
            })
        })?;
        Ok(())
    }

    /// Inserts `with`, whose text must end with a line break, as the lines
    /// right after the line that `sibling` ends (see
    /// [`SyntaxTree::replace_to_line_end`]): the text becomes the text
    /// through that line's break, then `with`'s text, then the rest. The
    /// new elements go under `sibling`'s parent, after it; the line
    /// breaks go where the parser puts them. So in `a: |\n  x\n` an entry
    /// `b: 1\n` inserted after the scalar gives `a: |\n  x\nb: 1\n`, and in
    /// `a: 1\n` an entry `b: |\n  y\n` inserted after `1` gives
    /// `a: 1\nb: |\n  y\n`.
    ///
    /// One or two splices, applied and checked as one edit.
    /// `inserted` names what went in under the parent (a copy of
    /// `sibling`'s line break first, when that break lies outside the
    /// parent; without `with`'s final line break element).
    ///
    /// **Trivia**: `with` of white space, comments, and line
    /// breaks only (comment lines) goes wholly where the parser puts trivia
    /// after `sibling`'s line: right after `sibling`'s line break, under
    /// that break's parent, or, when `sibling`'s line ends inside its own
    /// text, before the element after it, under that element's parent (or
    /// at the end of the stream). So comment lines after a root collection
    /// that ends with a block scalar go at stream level. `inserted` names
    /// all of `with`'s elements.
    ///
    /// # Errors
    ///
    /// Those of [`SyntaxTree::insert_after`];
    /// [`TreeEditError::NotWellFormed`] also when `sibling`'s line goes on
    /// after it or ends the text without a line break, or `with`'s text
    /// does not end with one; [`TreeEditError::EmptyFragment`].
    pub fn insert_line_after(
        &mut self,
        sibling: NodeId,
        with: Fragment,
    ) -> Result<Inserted, TreeEditError> {
        let s = self.live_slot(sibling)?;
        let parent = self.nav_parent(s).ok_or(TreeEditError::NoParent)?;
        let pk = self.check_parent(parent)?;
        if !with.ends_line() {
            return Err(TreeEditError::NotWellFormed);
        }
        if trivia_lines(&with) {
            let (p, before) = match self.line_end_after(s)? {
                LineEnd::Eof => return Err(TreeEditError::NotWellFormed),
                LineEnd::Break(b) => (self.nav_parent(b).ok_or(TreeEditError::Internal)?, Some(b)),
                LineEnd::Inner => self.outer_place(self.nav_after(s))?,
            };
            let p_kind = self.check_parent(p)?;
            self.check_insertable(p, p_kind, &with)?;
            let (_, span) = self.splice(p, before, &[], Some(with), true)?;
            return self.body_span(span, false);
        }
        let (body, brk) = with.split_last_break()?;
        if body.is_empty() || body.root_kinds().all(|k| k == SyntaxKind::LineBreak) {
            return Err(TreeEditError::EmptyFragment);
        }
        self.check_insertable(parent, pk, &body)?;
        let end = self.line_end_after(s)?;
        let had_break = brk.is_some();
        match end {
            LineEnd::Eof => Err(TreeEditError::NotWellFormed),
            LineEnd::Break(b) if self.nav_parent(b) == Some(parent) => {
                // The collection goes on after the break: the new lines go
                // right after it, their breaks with them.
                let fragment = match brk {
                    Some(x) => body.concat(x)?,
                    None => body,
                };
                let (_, span) = self.splice(parent, Some(b), &[], Some(fragment), true)?;
                self.body_span(span, had_break)
            }
            LineEnd::Break(b) => {
                // `sibling`'s break lies outside the parent: a copy of it
                // goes in after `sibling`, then the new lines; the break
                // itself goes, or becomes the new lines' final break.
                let text = super::Node::new(self, b).text().into_owned();
                let copy = token_fragment(SyntaxKind::LineBreak, &text, false, &self.settings)?;
                let fragment = copy.concat(body)?;
                let first_args = SpliceArgs {
                    parent,
                    prev: Some(s),
                    removed: Vec::new(),
                    fragment: Some(fragment),
                };
                let ((_, span), _) = self.splice_pair(first_args, move |t, _| {
                    Ok(Some(SpliceArgs {
                        parent: t.nav_parent(b).ok_or(TreeEditError::Internal)?,
                        prev: t.nav_prev(b),
                        removed: vec![b],
                        fragment: brk,
                    }))
                })?;
                self.body_span(span, false)
            }
            LineEnd::Inner => {
                let cut = self.nav_after(s);
                let outer = cut.map_or(Some(0), |c| self.nav_parent(c));
                if brk.is_none() || outer == Some(parent) {
                    let fragment = match brk {
                        Some(x) => body.concat(x)?,
                        None => body,
                    };
                    let (_, span) = self.splice(parent, Some(s), &[], Some(fragment), true)?;
                    return self.body_span(span, had_break);
                }
                let first_args = SpliceArgs {
                    parent,
                    prev: Some(s),
                    removed: Vec::new(),
                    fragment: Some(body),
                };
                let ((_, span), _) = self.splice_pair(first_args, move |t, _| {
                    let (p, before) = t.outer_place(cut)?;
                    Ok(Some(SpliceArgs {
                        parent: p,
                        prev: before,
                        removed: Vec::new(),
                        fragment: brk,
                    }))
                })?;
                self.body_span(span, false)
            }
        }
    }
}

/// The block scalar token of `kind` holding `text` and then `brk`, read
/// through the parser after the scalar's `header` (the scalar's content
/// token, copied out).
fn block_token_fragment(
    kind: SyntaxKind,
    header: &str,
    text: &str,
    brk: &str,
    settings: &oneyaml_core::Settings,
) -> Result<Fragment, TreeEditError> {
    let bad = TreeEditError::InvalidToken { kind };
    let want = format!("{text}{brk}");
    let tree = SyntaxTree::parse_with(format!("{header}{brk}{want}"), settings);
    if tree.has_errors() {
        return Err(bad);
    }
    let mut found = None;
    for n in tree.root().descendants().filter(|n| n.is_token()) {
        if n.kind() == kind {
            if found.is_some() {
                return Err(bad);
            }
            found = Some(n);
        }
    }
    let token = found.ok_or(bad)?;
    if token.text() != want {
        return Err(TreeEditError::InvalidToken { kind });
    }
    let (arena, _) = tree.copy_forest(&[token.slot()])?;
    Ok(Fragment {
        arena,
        context: String::new(),
        has_tags: false,
    })
}
