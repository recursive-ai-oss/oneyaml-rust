//! The comment API's writes: leading, trailing and footer
//! comments of a node, the document header and footer, each set through
//! an explicit call (the header and footer change only this way).
//!
//! Every write takes **view** text (`# ` is added; storage is byte-exact:
//! [`Comment::raw`](crate::Comment::raw) is what goes into the text),
//! checked through the one parser, and refused with
//! [`EditError::InvalidComment`] when it cannot be a comment. New comment
//! lines are written in the owner's own indentation and the document's
//! line break (style inference), where the parser puts trivia
//! (the syntax layer refuses anything else: an edited tree always
//! agrees with its own text").
//!
//! **Owners are checked, not assumed.** After a write, the runs of
//! comment lines it touched are decided again by the rulebook: every new
//! comment must belong to the owner it was written for, and every other
//! comment of those runs must keep its owner; otherwise the write is
//! undone and refused with [`EditError::CommentReattaches`] (the choice
//! made over re-owning such comments explicitly, which the text
//! could not carry). So the text of an edited document, parsed again, has
//! the owners the document reports, except for explicit owners
//! ([`Document::attach_comment`]), which live in the document only.
//!
//! **Meaning is checked too.** A comment write changes only
//! trivia, and the only values that read trivia are those of the scalars
//! right before a run of it (a kept `|+` block scalar at the end of the
//! text reads an unterminated last line of spaces as one more line). So
//! before a write the values of the scalars next to every run it touches
//! are recorded (the runs of the comments it removes and of every place
//! it tries to insert at), and after it they must be the same; the new
//! comments' run must lie between recorded neighbours. Otherwise the
//! write is undone and refused with [`EditError::Unsupported`].
//!
//! Each write is one edit group: refused, nothing changed.
//! Cost: the runs of comment lines around the owner (each decided
//! once, and removed in one splice per block of consecutive lines, each
//! checked locally by the syntax layer: O(the comments removed), whatever
//! the selection) and the
//! syntax layer's local edits (which check an insertion into a run of
//! comment lines over that run); never the document, never the number of
//! explicit owners (O(log k)). Loops only, no recursion.

use std::collections::{BTreeMap, BTreeSet};

use oneyaml_core::{Path, Span};
use oneyaml_syntax::tree::{Fragment, Inserted, Node as TreeNode, SyntaxKind};

use super::comments::{self as cm, Key};

/// What a comment write must leave as it is, recorded before it
/// (see [`Document::check_after`]).
#[derive(Default)]
struct Before {
    /// The rulebook owners of the comments of the runs read (explicit
    /// ones aside).
    owners: BTreeMap<TreeId, Key>,
    /// The values of the scalars next to every run the write touches.
    values: BTreeMap<TreeId, Option<String>>,
    /// The comments walked past while recording `values`.
    walked: BTreeSet<TreeId>,
}
use super::edit::{Put, map_tree};
use super::place::{self, Coll};
use super::{CommentId, CommentOwner, CommentRule, Document, EditError, NodeId, TreeId};

fn unsupported(reason: &'static str) -> EditError {
    EditError::Unsupported {
        at: Path::new(),
        reason,
    }
}

/// An internal invariant failed (`debug_assert!` plus an error in
/// release).
fn internal(what: &str) -> EditError {
    debug_assert!(false, "comment edit: {what}");
    EditError::Internal
}

/// `t`, or the scalar, alias, or error region holding it: the syntax
/// layer keeps the inside of those read-only, so text goes next to them.
fn climb(t: TreeNode<'_>) -> TreeNode<'_> {
    let mut at = t;
    while let Some(p) = at.parent().filter(|p| {
        matches!(
            p.kind(),
            SyntaxKind::Scalar | SyntaxKind::Alias | SyntaxKind::Error
        )
    }) {
        at = p;
    }
    at
}

/// Places after `t` (climbed), then after each ancestor `t` ends, below
/// the stream: where a same-line comment after `t` may go (the parser puts
/// trivia after the outermost node that ends there, C4; the syntax layer
/// refuses the others).
pub(super) fn after_chain(t: TreeNode<'_>) -> Vec<Put> {
    let mut x = climb(t);
    let mut out = vec![Put::After(x.id())];
    while x.next_sibling().is_none() {
        match x.parent() {
            Some(p) if p.kind() != SyntaxKind::Stream => {
                x = p;
                out.push(Put::After(x.id()));
            }
            _ => break,
        }
    }
    out
}

/// Places for whole lines right after the line that `e` ends (`e`: a line
/// break or a block scalar's text; `None`: before the text's first line).
pub(super) fn after_line(root: TreeNode<'_>, e: Option<TreeNode<'_>>) -> Vec<Put> {
    match e {
        None => match root.first_child() {
            Some(b) if b.kind() == SyntaxKind::ByteOrderMark => vec![Put::After(b.id())],
            Some(f) => vec![Put::Before(f.id())],
            None => vec![Put::Append(root.id())],
        },
        Some(e) if e.kind() == SyntaxKind::LineBreak => {
            let mut out = Vec::new();
            // After the line's last token (the syntax layer puts the lines
            // after its break, wherever the parser put that).
            if let Some(q) = cm::before_indent(e).filter(|q| !cm::ends_line(*q)) {
                out.push(Put::LineAfter(climb(q).id()));
            }
            out.push(Put::LineAfter(e.id()));
            out
        }
        Some(e) => vec![Put::LineAfter(climb(e).id())],
    }
}

/// The line after the one `e` ends is blank (`None`: the text's first
/// line).
fn blank_follows(root: TreeNode<'_>, e: Option<TreeNode<'_>>) -> bool {
    let mut at = match e {
        Some(e) => place::next_token(e),
        None => cm::first_token(root),
    };
    while let Some(t) = at.filter(|t| cm::blankish(*t)) {
        at = place::next_token(t);
    }
    at.is_some_and(|t| t.kind() == SyntaxKind::LineBreak)
}

/// Only white space follows `t` on its line.
fn rest_blank(t: TreeNode<'_>) -> bool {
    let mut at = place::next_token(t);
    while let Some(x) = at.filter(|x| cm::blankish(*x)) {
        at = place::next_token(x);
    }
    at.is_none_or(|x| x.kind() == SyntaxKind::LineBreak)
}

/// A same-line comment can go right after `t`: it does not end its line
/// inside its text (a block scalar's), and it is not a block scalar
/// header (inside the scalar, read-only).
fn writable(t: TreeNode<'_>) -> bool {
    !cm::ends_line(t) && t.kind() != SyntaxKind::BlockScalarHeader
}

/// The last content token on the line starting at `s`, when no comment
/// is on that line and the line does not end inside a block scalar.
fn line_last(s: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = cm::first_token(s);
    let mut last = None;
    while let Some(t) = at {
        match t.kind() {
            SyntaxKind::LineBreak => break,
            SyntaxKind::Comment => return None,
            _ if cm::visible(t) => {
                if cm::ends_line(t) {
                    return None;
                }
                last = Some(t);
            }
            _ => {}
        }
        at = place::next_token(t);
    }
    last.filter(|t| writable(*t))
}

/// The line starting at `s` already holds a comment (whoever owns it).
fn line_has_comment(s: TreeNode<'_>) -> bool {
    let mut at = cm::first_token(s);
    while let Some(t) = at {
        match t.kind() {
            SyntaxKind::LineBreak => return false,
            SyntaxKind::Comment => return true,
            _ if cm::ends_line(t) => return false,
            _ => {}
        }
        at = place::next_token(t);
    }
    false
}

/// Where a new same-line comment of entry, item, or node `x` may go, in
/// order: the end of its first line, the end of its last line (after a
/// flow value spanning lines). The rulebook decides which one gives it to
/// `x` (rule 5); the caller checks.
fn trailing_points(x: TreeNode<'_>) -> Vec<TreeId> {
    let start = cm::entry_start(x);
    let mut out = Vec::new();
    if let Some(p) = line_last(start) {
        out.push(p.id());
    }
    let key = !cm::is_value(x) && x.parent().and_then(Coll::of).is_some_and(Coll::is_map);
    let end = if key { cm::value_of(x) } else { Some(x) };
    let last = end
        .and_then(cm::last_token)
        .filter(|t| writable(*t) && rest_blank(*t));
    if let Some(t) = last {
        if !out.contains(&t.id()) {
            out.push(t.id());
        }
    }
    out
}

/// The comment lines `raws` as text: each after `pad`, each but the last
/// followed by `nl` (the last too when `last_break`); `lead`: a line break
/// first; `blank`: an empty line after.
fn lines_text(
    pad: &str,
    raws: &[String],
    nl: &str,
    lead: bool,
    last_break: bool,
    blank: bool,
) -> String {
    let mut text = String::new();
    if lead {
        text.push_str(nl);
    }
    let n = raws.len();
    for (i, r) in raws.iter().enumerate() {
        text.push_str(pad);
        text.push_str(r);
        if last_break || i.saturating_add(1) < n {
            text.push_str(nl);
        }
    }
    if blank {
        text.push_str(nl);
    }
    text
}

impl Document {
    /// The stored form of view text `text`, read back through the one
    /// parser as exactly one comment with exactly those bytes.
    fn checked_raw(&self, text: &str) -> Result<String, EditError> {
        let raw = cm::raw_of(text)?;
        let invalid = || EditError::InvalidComment {
            text: text.to_owned(),
        };
        let t = self.synth(&raw).map_err(|_| invalid())?;
        let mut kids = t.root().children();
        match (kids.next(), kids.next()) {
            (Some(k), None) if k.kind() == SyntaxKind::Comment && k.text() == raw.as_str() => {
                Ok(raw)
            }
            _ => Err(invalid()),
        }
    }

    fn checked_raws<I>(&self, lines: I) -> Result<Vec<String>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        lines
            .into_iter()
            .map(|l| self.checked_raw(l.as_ref()))
            .collect()
    }

    /// Trivia text (white space, comments, line breaks) as a fragment,
    /// through the one parser: text this layer checked or wrote (comments
    /// through `checked_raw`, white space, line breaks), which always
    /// reads as trivia (`Internal` otherwise, an invariant).
    pub(super) fn trivia_fragment(&self, text: &str) -> Result<Fragment, EditError> {
        self.read_trivia(text)?
            .ok_or_else(|| internal("comment lines did not read as trivia"))
    }

    /// [`Document::trivia_fragment`] of comment lines carried from the
    /// document as they are (with the entry they belong to). Fuzz
    /// finding E11: those need not read
    /// as trivia again (a comment line of recovered input holding a
    /// character the parser refuses, `# c\0x`): such text is refused,
    /// `NotWellFormed`, nothing changed.
    pub(super) fn carried_fragment(&self, text: &str) -> Result<Fragment, EditError> {
        self.read_trivia(text)?
            .ok_or(EditError::NotWellFormed { at: Path::new() })
    }

    /// `text` as a trivia fragment, `None` when it does not read as trivia.
    fn read_trivia(&self, text: &str) -> Result<Option<Fragment>, EditError> {
        let t = self.synth(text)?;
        let kids: Vec<TreeNode<'_>> = t.root().children().collect();
        let trivia = kids.iter().all(|k| {
            matches!(
                k.kind(),
                SyntaxKind::Whitespace | SyntaxKind::Comment | SyntaxKind::LineBreak
            )
        });
        match (kids.first(), kids.last()) {
            (Some(a), Some(b)) if trivia => t
                .extract(a.id(), b.id())
                .map(Some)
                .map_err(|e| map_tree(e, Span::empty(0))),
            _ => Ok(None),
        }
    }

    /// The first place of `puts` the syntax layer accepts `frag` at. Each
    /// place's neighbours are recorded in `b` before it is tried
    /// ([`Document::check_after`] checks the run the lines land in).
    fn try_puts(
        &mut self,
        puts: &[Put],
        frag: &Fragment,
        b: &mut Before,
    ) -> Result<Inserted, EditError> {
        let mut last = EditError::NotWellFormed { at: Path::new() };
        for p in puts {
            self.border_put(b, *p);
            match self.t_insert(*p, frag.clone()) {
                Ok(i) => return Ok(i),
                Err(e @ (EditError::NotWellFormed { .. } | EditError::ReadOnly { .. })) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// [`Document::border`] of the place `put` names.
    fn border_put(&self, b: &mut Before, put: Put) {
        let at = match put {
            Put::After(x) | Put::Before(x) | Put::LineAfter(x) => self.tree().get(x).ok(),
            Put::Append(p) => self.tree().get(p).ok().and_then(cm::last_token),
        };
        if let Some(n) = at {
            self.border(b, n);
        }
    }

    /// The comments among the inserted siblings.
    fn comments_in(&self, ins: Inserted) -> Vec<TreeId> {
        let mut out = Vec::new();
        let mut at = self.tree().get(ins.first).ok();
        while let Some(n) = at {
            if n.kind() == SyntaxKind::Comment {
                out.push(n.id());
            }
            if n.id() == ins.last {
                break;
            }
            at = n.next_sibling();
        }
        out
    }

    /// Comment lines `raws` at `pad` as the lines after the one `e` ends
    /// (see [`after_line`]), with an empty line after them when `blank`.
    fn insert_after_line(
        &mut self,
        e: Option<TreeId>,
        pad: &str,
        raws: &[String],
        blank: bool,
        b: &mut Before,
    ) -> Result<Vec<TreeId>, EditError> {
        let puts = {
            let e = match e {
                Some(e) => Some(self.tree().get(e).map_err(|_| EditError::StaleNode)?),
                None => None,
            };
            after_line(self.tree().root(), e)
        };
        let frag = self.trivia_fragment(&lines_text(pad, raws, self.nl(), false, true, blank))?;
        let ins = self.try_puts(&puts, &frag, b)?;
        Ok(self.comments_in(ins))
    }

    /// Comment lines `raws` at `pad` right above the line `start` starts.
    fn insert_lines_above(
        &mut self,
        start: TreeId,
        pad: &str,
        raws: &[String],
        b: &mut Before,
    ) -> Result<Vec<TreeId>, EditError> {
        let e = {
            let s = self.tree().get(start).map_err(|_| EditError::StaleNode)?;
            cm::before_indent(s).map(|e| e.id())
        };
        self.insert_after_line(e, pad, raws, false, b)
    }

    /// Comment lines `raws` at `pad` right after the line that `sibling`
    /// ends (after it on a line without a final line break).
    fn insert_lines_below(
        &mut self,
        sibling: TreeId,
        pad: &str,
        raws: &[String],
        b: &mut Before,
    ) -> Result<Vec<TreeId>, EditError> {
        let nl = self.nl();
        let frag = self.trivia_fragment(&lines_text(pad, raws, nl, false, true, false))?;
        self.border_put(b, Put::LineAfter(sibling));
        match self.t_insert(Put::LineAfter(sibling), frag) {
            Ok(i) => return Ok(self.comments_in(i)),
            Err(EditError::NotWellFormed { .. } | EditError::ReadOnly { .. }) => {}
            Err(e) => return Err(e),
        }
        // The line ends the text without a line break: each line after one.
        let puts = {
            let s = self.tree().get(sibling).map_err(|_| EditError::StaleNode)?;
            after_chain(s)
        };
        let frag = self.trivia_fragment(&lines_text(pad, raws, nl, true, false, false))?;
        let ins = self.try_puts(&puts, &frag, b)?;
        Ok(self.comments_in(ins))
    }

    /// A same-line comment `raw` right after token `p`.
    fn insert_trailing(
        &mut self,
        p: TreeId,
        raw: &str,
        b: &mut Before,
    ) -> Result<TreeId, EditError> {
        let puts = {
            let t = self.tree().get(p).map_err(|_| EditError::StaleNode)?;
            after_chain(t)
        };
        let frag = self.trivia_fragment(&format!(" {raw}"))?;
        let ins = self.try_puts(&puts, &frag, b)?;
        self.comments_in(ins)
            .first()
            .copied()
            .ok_or_else(|| internal("no comment inserted"))
    }

    /// Removes comments `ids`: each with the white space before it, and
    /// its whole line when it is alone on it; blank lines around and
    /// between them are kept byte for byte.
    ///
    /// Consecutive own-line comments (blank lines between
    /// allowed) go in **one** splice, so removing k comment lines costs
    /// O(k) plus the syntax layer's local check, never k checks of the
    /// run.
    fn drop_comments(&mut self, ids: &[TreeId]) -> Result<(), EditError> {
        /// One splice of a removal.
        enum Cut {
            /// Siblings `first..=last` (a same-line comment and the white
            /// space before it).
            Range(TreeId, TreeId),
            /// Siblings through the end of their line (a comment line whose
            /// line break lies elsewhere).
            ToLineEnd(TreeId, TreeId),
            /// Comment lines `first..=end` (end: the last one's line
            /// break), the blank lines between them written back as `kept`.
            Lines(TreeId, TreeId, String),
            /// Stream-level comment lines, cut from the line break before
            /// the first through the last comment (their last line break
            /// stays), the lines kept between them written back as the
            /// text (see below).
            Shifted(TreeId, TreeId, String),
        }
        if ids.is_empty() {
            return Ok(());
        }
        let cuts = {
            let tree = self.tree();
            let set: BTreeSet<TreeId> = ids.iter().copied().collect();
            let mut nodes = Vec::with_capacity(ids.len());
            for c in &set {
                nodes.push(tree.get(*c).map_err(|_| EditError::StaleNode)?);
            }
            nodes.sort_by_key(|n| n.preorder_index());
            let is_break = |x: &TreeNode<'_>| x.kind() == SyntaxKind::LineBreak;
            let mut used = BTreeSet::new();
            let mut cuts = Vec::new();
            for n in nodes {
                if !used.insert(n.id()) {
                    continue;
                }
                let ws = n
                    .prev_sibling()
                    .filter(|w| w.kind() == SyntaxKind::Whitespace);
                if !cm::line_start(n) {
                    cuts.push(Cut::Range(ws.unwrap_or(n).id(), n.id()));
                    continue;
                }
                let first = match ws {
                    Some(w) if cm::line_start(w) => w,
                    _ => n,
                };
                let Some(mut end) = n.next_sibling().filter(is_break) else {
                    cuts.push(Cut::ToLineEnd(first.id(), n.id()));
                    continue;
                };
                // The next lines: blank ones (kept, pending until another
                // comment line of `ids` follows) and comment lines of `ids`
                // whose line break is their sibling. A loop, not recursion.
                let (mut kept, mut pending) = (String::new(), String::new());
                let mut last_c = n;
                let mut at = end.next_sibling();
                loop {
                    let mut line = String::new();
                    while let Some(w) = at.filter(|w| w.kind() == SyntaxKind::Whitespace) {
                        line.push_str(&w.text());
                        at = w.next_sibling();
                    }
                    match at {
                        Some(b) if is_break(&b) => {
                            pending.push_str(&line);
                            pending.push_str(&b.text());
                            at = b.next_sibling();
                        }
                        Some(c)
                            if c.kind() == SyntaxKind::Comment
                                && set.contains(&c.id())
                                && !used.contains(&c.id()) =>
                        {
                            let Some(b) = c.next_sibling().filter(is_break) else {
                                break;
                            };
                            used.insert(c.id());
                            kept.push_str(&pending);
                            pending.clear();
                            last_c = c;
                            end = b;
                            at = b.next_sibling();
                        }
                        _ => break,
                    }
                }
                // At stream level, the syntax layer checks
                // a cut that ends right before content (a stray `...`, a
                // directive) by re-reading up to the next document; cut
                // from the line break before instead, so the edit ends at a
                // line break and is checked locally, whatever is removed
                // around it. Same text: the previous line's break and the
                // kept lines, less the break that stays.
                let at_stream = n.parent().is_some_and(|p| p.kind() == SyntaxKind::Stream);
                let before = first.prev_sibling().filter(is_break);
                if let (true, Some(lb)) = (at_stream, before) {
                    let joined = format!("{}{kept}", lb.text());
                    let stays = end.text();
                    if let Some(text) = joined.strip_suffix(stays.as_ref()) {
                        cuts.push(Cut::Shifted(lb.id(), last_c.id(), text.to_owned()));
                        continue;
                    }
                }
                cuts.push(Cut::Lines(first.id(), end.id(), kept));
            }
            cuts
        };
        for cut in cuts.into_iter().rev() {
            match cut {
                Cut::Shifted(a, b, text) if text.is_empty() => self.t_remove(a, b)?,
                Cut::Shifted(a, b, text) => {
                    let frag = self.trivia_fragment(&text)?;
                    self.t_replace(a, b, frag, false)?;
                }
                Cut::Range(a, b) => self.t_remove(a, b)?,
                Cut::ToLineEnd(a, b) => self.t_remove_to_line_end(a, b, None)?,
                Cut::Lines(a, b, kept) if kept.is_empty() => self.t_remove(a, b)?,
                Cut::Lines(a, b, kept) => {
                    let frag = self.trivia_fragment(&kept)?;
                    self.t_replace(a, b, frag, false)?;
                }
            }
        }
        Ok(())
    }

    /// What a write must leave as it is, read before it: the rulebook
    /// owners of the comments `seen` (explicit ones and `except` aside).
    fn before(&self, seen: &[(TreeNode<'_>, Option<Key>)], except: &BTreeSet<TreeId>) -> Before {
        Before {
            owners: seen
                .iter()
                .filter(|(c, _)| !self.has_explicit(c.id()) && !except.contains(&c.id()))
                .filter_map(|(c, k)| Some((c.id(), (*k)?)))
                .collect(),
            ..Before::default()
        }
    }

    /// Records the values of the scalars next to the comment run(s) at
    /// `t` (a comment, the place new lines go, or content next to it): the
    /// content tokens before and after it, and `t`'s own first and last
    /// tokens when they are content. Each run is walked once.
    fn border(&self, b: &mut Before, t: TreeNode<'_>) {
        if b.walked.contains(&t.id()) {
            return;
        }
        let (first, last) = (cm::first_token(t), cm::last_token(t));
        let mut toks: Vec<TreeNode<'_>> = first.into_iter().chain(last).collect();
        if t.kind() == SyntaxKind::Comment {
            b.walked.insert(t.id());
        }
        // Loops over tokens, each side up to its content token, or up
        // to a comment walked before (whose walk reached, and recorded, the
        // content token on that side): each run is walked once.
        let mut at = first.and_then(place::prev_token);
        while let Some(x) = at {
            if cm::visible(x) {
                toks.push(x);
                break;
            }
            if x.kind() == SyntaxKind::Comment && !b.walked.insert(x.id()) {
                break;
            }
            at = place::prev_token(x);
        }
        let mut at = last.and_then(place::next_token);
        while let Some(x) = at {
            if cm::visible(x) {
                toks.push(x);
                break;
            }
            if x.kind() == SyntaxKind::Comment && !b.walked.insert(x.id()) {
                break;
            }
            at = place::next_token(x);
        }
        for tok in toks.into_iter().filter(|x| cm::visible(*x)) {
            if let Some(s) = tok.parent().filter(|p| p.kind() == SyntaxKind::Scalar) {
                b.values
                    .entry(s.id())
                    .or_insert_with(|| s.value().map(std::borrow::Cow::into_owned));
            }
        }
    }

    /// [`Document::border`] of every comment of `ids`.
    fn border_all(&self, b: &mut Before, ids: &[TreeId]) {
        for c in ids {
            if let Ok(n) = self.tree().get(*c) {
                self.border(b, n);
            }
        }
    }

    /// After a write: nothing but comments changed.
    ///
    /// * **Meaning.** Every scalar next to a run the write touched has the
    ///   value it had (a kept `|+` scalar reads the lines after it: an
    ///   unterminated last line of spaces is one more line of it, so a
    ///   comment line added or removed there would change it), and the
    ///   new comments' run lies between recorded neighbours. Trivia edits
    ///   change no token, so no other value can move.
    /// * **Owners.** The runs holding `new` and the recorded comments,
    ///   decided again (one pass per run), give each new comment `want`
    ///   and every recorded comment its owner.
    fn check_after(&self, b: &Before, new: &[TreeId], want: Option<Key>) -> Result<(), EditError> {
        let tree = self.tree();
        for (id, was) in &b.values {
            let now = tree
                .get(*id)
                .ok()
                .and_then(|n| n.value().map(std::borrow::Cow::into_owned));
            if now != *was {
                return Err(unsupported(
                    "a comment edit that would change the value of the scalar next to it \
                     (a kept `|+` block scalar reads the lines after it)",
                ));
            }
        }
        if let Some(n) = new.first().and_then(|c| tree.get(*c).ok()) {
            // The new lines' run, walked each way up to a recorded comment
            // (O(1) next to the place they went) or to a content token,
            // whose scalar must be recorded.
            let recorded = |x: TreeNode<'_>| {
                x.parent()
                    .filter(|p| p.kind() == SyntaxKind::Scalar)
                    .is_none_or(|p| b.values.contains_key(&p.id()))
            };
            let ok = |step: fn(TreeNode<'_>) -> Option<TreeNode<'_>>| {
                let mut at = step(n);
                while let Some(x) = at {
                    if cm::visible(x) {
                        return recorded(x);
                    }
                    if x.kind() == SyntaxKind::Comment && b.walked.contains(&x.id()) {
                        return true;
                    }
                    at = step(x);
                }
                true
            };
            if !ok(place::prev_token) || !ok(place::next_token) {
                return Err(internal("new comment lines next to an unrecorded scalar"));
            }
        }
        let fresh: BTreeSet<TreeId> = new.iter().copied().collect();
        // Each run decided once; the runs between documents a stretch at a
        // time.
        let seeds = new
            .iter()
            .chain(b.owners.keys())
            .filter_map(|id| tree.get(*id).ok());
        for (c, a) in self.decide_runs(seeds) {
            let expect = if fresh.contains(&c.id()) {
                want
            } else {
                b.owners.get(&c.id()).copied()
            };
            let Some(expect) = expect else { continue };
            let got = cm::owner_of(&a).map(|(o, _)| (o, a.document().map(TreeNode::id)));
            if got != Some(expect) {
                let rule = cm::rule_of(a.rule()).ok_or_else(|| internal("unknown rule"))?;
                return Err(EditError::CommentReattaches { rule });
            }
        }
        Ok(())
    }

    /// After a write: explicit owners of the removed comments go
    /// (O(removed · log k)), the edit is committed.
    fn finish_comment_edit(&mut self, removed: &[TreeId]) {
        self.forget_explicit(removed);
        self.commit();
    }

    /// Whether [`Document::clear_comments`] of `id` would find anything to
    /// clear (its errors when it would refuse first). Fuzz
    /// finding H1): a value handle's `clear_comments` is a no-op exactly
    /// when both of its base-layer calls are.
    pub(super) fn has_comments_to_clear(&self, id: NodeId) -> Result<bool, EditError> {
        let x = self.comment_target(id)?;
        let f = self.find_comments(x)?;
        Ok(!(f.leading.is_empty() && f.trailing.is_empty() && f.footer.is_empty()))
    }

    /// The node a comment write is for: a YAML node of this document (an
    /// error region is read-only), editable in restricted mode.
    fn comment_target(&self, id: NodeId) -> Result<TreeNode<'_>, EditError> {
        let x = self.node(id)?.tree_node();
        if !x.kind().is_yaml_node() {
            return Err(EditError::ReadOnly { at: Path::new() });
        }
        self.check_region(&[x], x.parent())?;
        Ok(x)
    }

    /// The root node, as the tree has it.
    fn root_tree(&self) -> Option<TreeNode<'_>> {
        self.doc_node()?
            .children()
            .find(|c| c.kind().is_yaml_node())
    }

    /// The rulebook key of `owner` in this document.
    fn key_of(&self, owner: CommentOwner) -> Key {
        (owner, self.doc)
    }

    /// Replaces the leading comments of node `id` (an entry is named by
    /// its key, see [`NodeRef::key`](crate::NodeRef::key)) with one
    /// comment line per item of `lines` (view text: `# ` is added), right
    /// above the entry, item, or node, in its indentation and the
    /// document's line break. The old leading comments go (their blank
    /// lines stay); [`NO_COMMENTS`](crate::NO_COMMENTS) removes them. `lines` is any
    /// iterable of strings (`["a"]`, `&["a"]`, a `Vec<String>`). Returns the
    /// new comments' handles. [`NodeMut::set_leading`](crate::NodeMut::set_leading)
    /// does the same through a handle.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let mut doc = Document::parse("a: 1\nb:\n  c: 2 # two\n").unwrap();
    /// let c = doc.get_path(&path!["b", "c"]).unwrap().key().unwrap().id();
    /// doc.set_leading(c, &["about c"]).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1\nb:\n  # about c\n  c: 2 # two\n");
    /// assert_eq!(doc.comments(c).unwrap().leading()[0].text(), "about c");
    /// ```
    ///
    /// The base layer: acts on exactly node `id` (a mapping value's own
    /// comments for a value's id); [`NodeMut`](crate::NodeMut) on a mapping
    /// value acts on its entry instead.
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidComment`] for text that cannot be a comment;
    /// [`EditError::StaleNode`]; [`EditError::ReadOnly`] (an error region,
    /// or next to one); [`EditError::Unsupported`] when the entry does not
    /// start its line (a value on its key's line: name the entry by its
    /// key); [`EditError::CommentReattaches`] when the rulebook would give
    /// the lines to another owner there (e.g. above a root mapping, rule 3
    /// gives them to its first entry); [`EditError::NotWellFormed`] should
    /// the syntax layer refuse the lines. Nothing changes on error.
    pub fn set_leading<I>(&mut self, id: NodeId, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.prepare()?;
        let raws = self.checked_raws(lines)?;
        self.set_leading_raws(id, raws)
    }

    fn set_leading_raws(
        &mut self,
        id: NodeId,
        raws: Vec<String>,
    ) -> Result<Vec<CommentId>, EditError> {
        let (start, pad, old, mut before) = {
            let x = self.comment_target(id)?;
            let start = cm::entry_start(x);
            if !raws.is_empty() && !cm::line_start(start) {
                return Err(unsupported(
                    "a leading comment of an entry or node that does not start its line",
                ));
            }
            let f = self.find_comments(x)?;
            let old: Vec<TreeId> = f.leading.iter().map(|h| h.node.id()).collect();
            let mut before = self.before(&f.seen, &old.iter().copied().collect());
            self.border(&mut before, start);
            self.border_all(&mut before, &old);
            (start.id(), cm::line_prefix(start), old, before)
        };
        if old.is_empty() && raws.is_empty() {
            return Ok(Vec::new());
        }
        let want = Some(self.key_of(CommentOwner::Leading(id)));
        let new = self.atomic(|d| {
            d.drop_comments(&old)?;
            let new = if raws.is_empty() {
                Vec::new()
            } else {
                d.insert_lines_above(start, &pad, &raws, &mut before)?
            };
            d.check_after(&before, &new, want)?;
            Ok(new)
        })?;
        self.finish_comment_edit(&old);
        Ok(new.into_iter().map(CommentId::of).collect())
    }

    /// Sets the trailing (same-line) comment of entry, item, or node `id`
    /// (an entry is named by its key) to `text` (view text), or removes it
    /// (`None`). An existing trailing comment keeps its place and the white
    /// space before it (its text is replaced; others the owner has go); a
    /// new one goes at the end of the entry's first line, one space after
    /// its content (or at the end of its last line, for a flow value
    /// spanning lines). Returns the comment's handle.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let mut doc = Document::parse("a: 1\nb: 2 # old\n").unwrap();
    /// let a = doc.get_path(&path!["a"]).unwrap().key().unwrap().id();
    /// doc.set_trailing(a, Some("one")).unwrap();
    /// let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    /// doc.set_trailing(b, Some("two")).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1 # one\nb: 2 # two\n");
    /// ```
    ///
    /// The base layer: acts on exactly node `id` (a mapping value's own
    /// comments for a value's id); [`NodeMut`](crate::NodeMut) on a mapping
    /// value acts on its entry instead.
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidComment`]; [`EditError::StaleNode`];
    /// [`EditError::ReadOnly`]; [`EditError::Unsupported`] for a mapping
    /// value (its entry owns its line's comment: use the key), and when no
    /// line of the entry ends with its own content (e.g. an item whose
    /// line goes on with a nested entry, `- a: 1`, or a block scalar's
    /// header line); [`EditError::CommentReattaches`] when a comment there
    /// would belong to another owner. Nothing changes on error.
    pub fn set_trailing(
        &mut self,
        id: NodeId,
        text: Option<&str>,
    ) -> Result<Option<CommentId>, EditError> {
        self.prepare()?;
        let raw = text.map(|t| self.checked_raw(t)).transpose()?;
        let (keep, drop, mut before) = {
            let x = self.comment_target(id)?;
            if cm::is_value(x) {
                return Err(unsupported(
                    "a same-line comment of a mapping value (its entry owns it: use the key)",
                ));
            }
            let f = self.find_comments(x)?;
            let keep = match raw {
                Some(_) => f
                    .trailing
                    .iter()
                    .find(|h| !h.explicit)
                    .map(|h| (h.node.id(), h.node.span())),
                None => None,
            };
            let drop: Vec<TreeId> = f
                .trailing
                .iter()
                .map(|h| h.node.id())
                .filter(|c| keep.is_none_or(|(k, _)| k != *c))
                .collect();
            let mut except: BTreeSet<TreeId> = drop.iter().copied().collect();
            except.extend(keep.map(|(k, _)| k));
            let mut before = self.before(&f.seen, &except);
            self.border_all(&mut before, &drop);
            if let Some((k, _)) = keep {
                self.border_all(&mut before, &[k]);
            }
            (keep, drop, before)
        };
        let Some(raw) = raw else {
            if drop.is_empty() {
                return Ok(None);
            }
            self.atomic(|d| {
                d.drop_comments(&drop)?;
                d.check_after(&before, &[], None)
            })?;
            self.finish_comment_edit(&drop);
            return Ok(None);
        };
        let want = Some(self.key_of(CommentOwner::Trailing(id)));
        let new = self.atomic(|d| {
            d.drop_comments(&drop)?;
            let new = match keep {
                // Another comment in its place is always well-formed; its
                // owner is decided by the content before it, unchanged.
                Some((k, span)) => d
                    .tree_mut()?
                    .replace_token(k, &raw)
                    .map_err(|e| map_tree(e, span))?,
                None => d.new_trailing(id, &raw, &mut before, want)?,
            };
            d.check_after(&before, &[new], want)?;
            Ok(new)
        })?;
        let mut gone = drop;
        gone.extend(keep.map(|(k, _)| k));
        self.finish_comment_edit(&gone);
        Ok(Some(CommentId::of(new)))
    }

    /// A new same-line comment of `id` at the first line end of the entry
    /// where the rulebook gives it to `id` (each attempt its own group,
    /// undone when it does not).
    fn new_trailing(
        &mut self,
        id: NodeId,
        raw: &str,
        before: &mut Before,
        want: Option<Key>,
    ) -> Result<TreeId, EditError> {
        let (points, taken) = {
            let x = self.tree_node(id)?;
            (trailing_points(x), line_has_comment(cm::entry_start(x)))
        };
        let mut last = None;
        for p in points {
            // (Each place is recorded when tried, after the removals: a
            // scalar their runs touch was recorded before them, and no
            // other value moved.)
            let r = self.atomic(|d| {
                let c = d.insert_trailing(p, raw, before)?;
                d.check_after(before, &[c], want)?;
                Ok(c)
            });
            match r {
                Ok(c) => return Ok(c),
                Err(
                    e @ (EditError::CommentReattaches { .. }
                    | EditError::NotWellFormed { .. }
                    | EditError::ReadOnly { .. }),
                ) => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            if taken {
                // e.g. a comment on the line re-owned by
                // `attach_comment` to another owner.
                unsupported(
                    "the entry's line already ends with a comment another owner has \
                     (re-own it with attach_comment, or clear it first)",
                )
            } else {
                unsupported("no line of the entry ends with its own content (a same-line comment)")
            }
        }))
    }

    /// Appends one comment line (view text) to the footer of collection
    /// `id` (rules 6 and 9): for a block collection, a line in its
    /// entries' indentation after its last line and its footer (and the
    /// footers of collections inside it); for a flow collection, a line
    /// right above its closing bracket. Returns the comment's handle.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("list:\n  - a\nnext: 1\n").unwrap();
    /// let list = doc.get("list").unwrap().id();
    /// doc.push_footer(list, "- b").unwrap();
    /// assert_eq!(doc.to_string(), "list:\n  - a\n  # - b\nnext: 1\n");
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidComment`]; [`EditError::StaleNode`];
    /// [`EditError::ReadOnly`]; [`EditError::Unsupported`] for a node that
    /// is not a collection, a single-pair flow mapping or unclosed flow
    /// collection, or a flow collection whose closing bracket does not
    /// start its line; [`EditError::CommentReattaches`] when the line would
    /// not be the footer there (rule 6 needs it right of what follows: a
    /// root collection at column 0 has none, rule 7 makes such a line the
    /// document footer). Nothing changes on error.
    ///
    /// Cost: for a block collection, O(depth + log) with
    /// the syntax layer's comment-run index (the line goes after the last
    /// comment of the run the rules give to it or to a collection inside
    /// it, found without reading the run, and only the new line's owner is
    /// decided: a line added there changes no other rulebook-decided owner
    /// of its run, rule 13), plus its explicitly owned footer comments;
    /// independent of the footer's length, so n pushes cost O(n log n). A
    /// flow collection, and a document holding an error region, read the
    /// comment runs around the collection (O(the runs)).
    ///
    /// Known limitation: explicitly owned comments at the end
    /// of the footer stay after the new line. They keep their explicit
    /// owners, but the owner the rules give them can move outward to the
    /// new line's owner (rule 13), which shows once they are detached: in
    /// `a:\n  b:\n    c: 1\n    # f1\n    # f2\nz: 1\n`, with `# f2`
    /// attached to the document footer, a push onto `a`'s mapping and a
    /// [`Document::detach_comment`] of `# f2` give it `a`'s mapping's
    /// footer, not `b`'s.
    pub fn push_footer(&mut self, id: NodeId, text: &str) -> Result<CommentId, EditError> {
        self.prepare()?;
        let raw = self.checked_raw(text)?;
        if let Some(r) = self.push_footer_indexed(id, &raw)? {
            return Ok(r);
        }
        let (above, at, pad, mut before) = {
            let x = self.comment_target(id)?;
            let kind = Coll::of(x)
                .ok_or_else(|| unsupported("a footer of a node that is not a collection"))?;
            let f = self.find_comments(x)?;
            let mut before = self.before(&f.seen, &BTreeSet::new());
            if kind.is_flow() {
                let close = x.last_child().filter(|l| {
                    matches!(
                        l.kind(),
                        SyntaxKind::FlowSequenceClose | SyntaxKind::FlowMappingClose
                    )
                });
                let close = close.ok_or_else(|| {
                    unsupported("a footer of a single-pair or unclosed flow collection")
                })?;
                if !cm::line_start(close) {
                    return Err(unsupported(
                        "a flow collection's footer before a closing bracket that does not start its line",
                    ));
                }
                self.border(&mut before, close);
                (true, close.id(), cm::line_prefix(close), before)
            } else {
                let first =
                    place::first_entry(x)?.ok_or_else(|| internal("an empty block collection"))?;
                let pad = " ".repeat(place::col_of(first.first));
                // Rule 13 (owners only move outward): after the footer
                // comments of the run after it, its own and those of the
                // collections inside it.
                let end = x.span().end;
                let (from, to) = (x.span().start, end);
                let inside = |o: &CommentOwner| match o {
                    CommentOwner::Footer(y) if *y == id => true,
                    CommentOwner::Footer(y) => self.node(*y).is_ok_and(|n| {
                        let s = n.span();
                        from <= s.start && s.end <= to
                    }),
                    _ => false,
                };
                let deep = f
                    .seen
                    .iter()
                    .filter(|(c, k)| {
                        k.is_some_and(|(o, _)| inside(&o))
                            && !self.has_explicit(c.id())
                            && c.span().start >= end
                    })
                    .map(|(c, _)| *c)
                    .chain(f.footer.iter().map(|h| h.node))
                    .filter(|c| c.span().start >= end)
                    .max_by_key(|c| c.preorder_index());
                let sibling = match deep {
                    Some(c) => c,
                    None => {
                        let t = cm::last_token(x)
                            .ok_or_else(|| internal("a collection without tokens"))?;
                        if cm::ends_line(t) {
                            climb(t)
                        } else {
                            cm::comment_after(t).unwrap_or_else(|| climb(t))
                        }
                    }
                };
                self.border(&mut before, sibling);
                (false, sibling.id(), pad, before)
            }
        };
        let want = Some(self.key_of(CommentOwner::Footer(id)));
        let raws = [raw];
        let new = self.atomic(|d| {
            let new = if above {
                d.insert_lines_above(at, &pad, &raws, &mut before)?
            } else {
                d.insert_lines_below(at, &pad, &raws, &mut before)?
            };
            d.check_after(&before, &new, want)?;
            new.first()
                .copied()
                .ok_or_else(|| internal("no comment inserted"))
        })?;
        self.finish_comment_edit(&[]);
        Ok(CommentId::of(new))
    }

    /// [`Document::push_footer`] of a block collection through the syntax
    /// layer's comment-run index: the same line in the same
    /// place, the same checks, decided locally. `Ok(None)`, before
    /// anything changed, where it does not apply (a flow collection, a
    /// document with an error region, an error the run-reading path
    /// reports itself); that path then decides.
    ///
    /// Why the new line's owner is all there is to check (comment rule 13): the
    /// line goes right after the last comment of its run whose rulebook owner
    /// lies within the collection and that is not explicitly owned elsewhere,
    /// so every comment before it keeps its decision (the same P, N, columns,
    /// and deepest owner before it). Every comment after it that the rules own
    /// (no explicit owner) already had an owner outside the collection, ranked
    /// deeper than anything the new line can bring (owners only move outward
    /// within a run), so it keeps it. Explicitly owned comments after it keep
    /// their explicit owners, which are what reads report; the rulebook owner
    /// under one of them is not checked and can change (an explicitly owned
    /// comment at the footer's end, skipped by the loop below, can fall to the
    /// new line's owner by rule 13 once detached; a known limitation,
    /// documented on [`Document::push_footer`]; earlier versions behaves the
    /// same). No other run is touched (whole comment lines go in). The comment
    /// fuzzes pin the rulebook-owned comments against deciding every recorded
    /// comment again.
    fn push_footer_indexed(
        &mut self,
        id: NodeId,
        raw: &str,
    ) -> Result<Option<CommentId>, EditError> {
        let (at, pad, mut before) = {
            let Ok(x) = self.comment_target(id) else {
                return Ok(None);
            };
            let Some(kind) = Coll::of(x) else {
                return Ok(None);
            };
            if kind.is_flow() || self.holds_error_regions() {
                return Ok(None);
            }
            let Ok(Some(first)) = place::first_entry(x) else {
                return Ok(None);
            };
            let pad = " ".repeat(place::col_of(first.first));
            let tree = self.tree();
            let own = self.doc_node();
            let end = x.span().end;
            let me = CommentOwner::Footer(id);
            // The rulebook's footer comments within the collection, last
            // first, explicitly owned ones aside (unless their explicit
            // owner lapsed and the rulebook gives them to `id` itself).
            let mut deep = None;
            let mut at = tree
                .last_footer_comment(x)
                .filter(|c| c.span().start >= end);
            while let Some(c) = at {
                if !self.has_explicit(c.id()) {
                    deep = Some(c);
                    break;
                }
                if self.explicit_owner(c.id()).is_none() {
                    let mine = tree.comment_attachment(c).is_some_and(|a| {
                        a.document() == own && cm::owner_of(&a).is_some_and(|(o, _)| o == me)
                    });
                    if mine {
                        deep = Some(c);
                        break;
                    }
                }
                at = cm::prev_comment_in_run(c).filter(|c| c.span().start >= end);
            }
            // Comments explicitly owned by its footer, wherever they are.
            for (c, o) in self.explicit_owned_by(&[x.id()]) {
                let Ok(c) = tree.get(c) else { continue };
                if o != me
                    || self.explicit_owner(c.id()) != Some(me)
                    || c.span().start < end
                    || tree.comment_document(c) != Some(own)
                {
                    continue;
                }
                if deep.is_none_or(|d: TreeNode<'_>| d.span().start < c.span().start) {
                    deep = Some(c);
                }
            }
            let sibling = match deep {
                Some(c) => c,
                None => {
                    let Some(t) = cm::last_token(x) else {
                        return Ok(None);
                    };
                    if cm::ends_line(t) {
                        climb(t)
                    } else {
                        cm::comment_after(t).unwrap_or_else(|| climb(t))
                    }
                }
            };
            let mut before = Before::default();
            self.border_indexed(&mut before, sibling);
            (sibling.id(), pad, before)
        };
        let want = self.key_of(CommentOwner::Footer(id));
        let raws = [raw.to_owned()];
        let new = self.atomic(|d| {
            let new = d.insert_lines_below(at, &pad, &raws, &mut before)?;
            d.check_after_indexed(&before, &new, want)?;
            new.first()
                .copied()
                .ok_or_else(|| internal("no comment inserted"))
        })?;
        self.finish_comment_edit(&[]);
        Ok(Some(CommentId::of(new)))
    }

    /// [`Document::border`] through the syntax layer's comment-run index:
    /// the content tokens around `t` found in O(log), not by
    /// walking its run; `t` itself is recorded as walked (the places tried
    /// next to it are not recorded again).
    fn border_indexed(&self, b: &mut Before, t: TreeNode<'_>) {
        let tree = self.tree();
        let (first, last) = (cm::first_token(t), cm::last_token(t));
        let mut toks: Vec<TreeNode<'_>> = first.into_iter().chain(last).collect();
        if t.kind() == SyntaxKind::Comment {
            b.walked.insert(t.id());
        }
        toks.extend(first.and_then(|f| tree.prev_content_token(f)));
        toks.extend(last.and_then(|l| tree.next_content_token(l)));
        for tok in toks.into_iter().filter(|x| cm::visible(*x)) {
            if let Some(s) = tok.parent().filter(|p| p.kind() == SyntaxKind::Scalar) {
                b.values
                    .entry(s.id())
                    .or_insert_with(|| s.value().map(std::borrow::Cow::into_owned));
            }
        }
    }

    /// [`Document::check_after`] of [`Document::push_footer_indexed`]:
    /// the recorded values; the new lines between recorded
    /// content tokens (found by the index); the new lines' owner, decided
    /// by the index — the only owner such a write can change.
    fn check_after_indexed(&self, b: &Before, new: &[TreeId], want: Key) -> Result<(), EditError> {
        let tree = self.tree();
        for (id, was) in &b.values {
            let now = tree
                .get(*id)
                .ok()
                .and_then(|n| n.value().map(std::borrow::Cow::into_owned));
            if now != *was {
                return Err(unsupported(
                    "a comment edit that would change the value of the scalar next to it \
                     (a kept `|+` block scalar reads the lines after it)",
                ));
            }
        }
        let recorded = |x: Option<TreeNode<'_>>| {
            x.and_then(|x| x.parent())
                .filter(|p| p.kind() == SyntaxKind::Scalar)
                .is_none_or(|p| b.values.contains_key(&p.id()))
        };
        for c in new.iter().filter_map(|c| tree.get(*c).ok()) {
            if !recorded(tree.prev_content_token(c)) || !recorded(tree.next_content_token(c)) {
                return Err(internal("new comment lines next to an unrecorded scalar"));
            }
            let a = tree
                .comment_attachment(c)
                .ok_or_else(|| internal("a new comment without an owner"))?;
            let got = cm::owner_of(&a).map(|(o, _)| (o, a.document().map(TreeNode::id)));
            if got != Some(want) {
                let rule = cm::rule_of(a.rule()).ok_or_else(|| internal("unknown rule"))?;
                return Err(EditError::CommentReattaches { rule });
            }
        }
        Ok(())
    }

    /// Removes every comment node `id` owns — leading, trailing, and
    /// footer ([`Document::comments`]) — and returns their stored
    /// text, in source order ("nothing destroyed silently": what is
    /// removed is handed back). Own-line comments go with their lines;
    /// blank lines stay. O(the comments removed + their runs).
    ///
    // Naming: the rulebook's word for this is
    // `clear`; it is `clear_comments` so that it does not read as clearing
    // the document.
    ///
    /// The base layer: acts on exactly node `id` (a mapping value's own
    /// comments for a value's id); [`NodeMut`](crate::NodeMut) on a mapping
    /// value acts on its entry instead.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`]; [`EditError::ReadOnly`];
    /// [`EditError::CommentReattaches`] should another comment change
    /// owner (the rulebook never does that when one owner's comments go
    /// together; checked anyway). Nothing changes on error.
    pub fn clear_comments(&mut self, id: NodeId) -> Result<Vec<String>, EditError> {
        self.prepare()?;
        let (all, texts, before) = {
            let x = self.comment_target(id)?;
            let f = self.find_comments(x)?;
            let mut hits: Vec<TreeNode<'_>> = f
                .leading
                .iter()
                .chain(&f.trailing)
                .chain(&f.footer)
                .map(|h| h.node)
                .collect();
            hits.sort_by_key(|h| h.preorder_index());
            let ids: Vec<TreeId> = hits.iter().map(|h| h.id()).collect();
            let texts: Vec<String> = hits.iter().map(|h| h.text().into_owned()).collect();
            let mut before = self.before(&f.seen, &ids.iter().copied().collect());
            self.border_all(&mut before, &ids);
            (ids, texts, before)
        };
        if all.is_empty() {
            return Ok(Vec::new());
        }
        self.atomic(|d| {
            d.drop_comments(&all)?;
            d.check_after(&before, &[], None)
        })?;
        self.finish_comment_edit(&all);
        Ok(texts)
    }

    /// Sets the blank lines right above the line where entry, item, or
    /// node `id` starts (an entry is named by its key) to `n` (rule
    /// 8): the count [`Comments::blank_lines_before`](crate::Comments::blank_lines_before)
    /// reads, below the node's leading comments. Blank lines are written
    /// as bare line breaks in the document's line break; removed ones go
    /// with the white space on them. To change the blank lines above a
    /// leading comment, use [`Document::set_comment_blank_lines`].
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let mut doc = Document::parse("a: 1\nb: 2\n\n\nc: 3\n").unwrap();
    /// let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    /// doc.set_blank_lines_before(b, 1).unwrap();
    /// let c = doc.get_path(&path!["c"]).unwrap().key().unwrap().id();
    /// assert_eq!(doc.comments(c).unwrap().blank_lines_before(), 2);
    /// doc.set_blank_lines_before(c, 0).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1\n\nb: 2\nc: 3\n");
    /// ```
    ///
    /// Checked like every comment write: every comment around keeps
    /// its owner (e.g. removing the blank line below a header comment
    /// would make it the first entry's leading comment, rule 3: refused
    /// with [`EditError::CommentReattaches`]), and every value keeps its
    /// meaning (blank lines right after a *kept* block scalar, `|+` or
    /// `>+`, are its content: refused with [`EditError::Unsupported`]).
    /// O(the blank lines and the comment runs next to them).
    ///
    /// The base layer: acts on exactly node `id` (a mapping value's own
    /// comments for a value's id); [`NodeMut`](crate::NodeMut) on a mapping
    /// value acts on its entry instead.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`]; [`EditError::ReadOnly`];
    /// [`EditError::Unsupported`] for `n > 0` when the entry does not start
    /// its line (a value on its key's line: name the entry by its key),
    /// and for lines right after a kept (`|+`/`>+`) block scalar or that
    /// would change a value; [`EditError::Rejected`] with `Y2002`
    /// (`Limits::max_document_size`) when the text would grow past the
    /// limit (checked before anything is allocated: any `n` is safe);
    /// [`EditError::CommentReattaches`]; [`EditError::NotWellFormed`]
    /// should the syntax layer refuse the lines. Nothing changes on error.
    pub fn set_blank_lines_before(&mut self, id: NodeId, n: usize) -> Result<(), EditError> {
        self.prepare()?;
        let (start, before) = {
            let x = self.comment_target(id)?;
            let f = self.find_comments(x)?;
            (
                cm::entry_start(x).id(),
                self.before(&f.seen, &BTreeSet::new()),
            )
        };
        self.set_blank_above(start, n, before)
    }

    /// Sets the blank lines right above own-line comment `id` to `n` (rule
    /// 8 of the rulebook: the count [`Comment::blank_lines_before`](crate::Comment::blank_lines_before)
    /// reads), checked as [`Document::set_blank_lines_before`]: the owners
    /// of the comments around, the comment's own included, and the values
    /// next to them do not change.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("a: 1\n# about b\nb: 2\n").unwrap();
    /// let about_b = doc.comment_ids()[0];
    /// doc.set_comment_blank_lines(about_b, 1).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1\n\n# about b\nb: 2\n");
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no comment of this
    /// document; [`EditError::Unsupported`] for `n > 0` and a same-line
    /// comment, for lines right after a kept (`|+`/`>+`) block scalar, or
    /// when the lines
    /// would change a value; [`EditError::Rejected`] (`Y2002`) past
    /// `Limits::max_document_size`;
    /// [`EditError::ReadOnly`]; [`EditError::CommentReattaches`] (e.g. the
    /// blank line that makes a header comment the header, rule 2);
    /// [`EditError::NotWellFormed`]. Nothing changes on error.
    pub fn set_comment_blank_lines(&mut self, id: CommentId, n: usize) -> Result<(), EditError> {
        self.prepare()?;
        self.check_own_comment(id)?;
        let c = id.tree_id();
        let before = {
            let tree = self.tree();
            let node = tree.get(c).map_err(|_| EditError::StaleNode)?;
            // Restricted mode: not next to an error region.
            let holder = node.parent();
            self.check_region(&[], holder)?;
            let seen: Vec<(TreeNode<'_>, Option<Key>)> = self
                .decide_runs([node])
                .into_iter()
                .map(|(n, a)| {
                    let key = cm::owner_of(&a).map(|(o, _)| (o, a.document().map(TreeNode::id)));
                    (n, key)
                })
                .collect();
            self.before(&seen, &BTreeSet::new())
        };
        self.set_blank_above(c, n, before)
    }

    /// Sets the blank lines right above the line token (or node) `s`
    /// starts to `n`, then checks what `before` recorded (see
    /// [`Document::check_after`]).
    fn set_blank_above(
        &mut self,
        s: TreeId,
        n: usize,
        mut before: Before,
    ) -> Result<(), EditError> {
        /// One blank line: its white space and its line break.
        type Line = (Vec<TreeId>, TreeId);
        let (lines, end, after_scalar): (Vec<Line>, Option<TreeId>, bool) = {
            let sn = self.tree().get(s).map_err(|_| EditError::StaleNode)?;
            if !cm::line_start(sn) {
                if n == 0 {
                    return Ok(());
                }
                return Err(unsupported(
                    "blank lines above an entry, node, or comment that does not start its line",
                ));
            }
            self.border(&mut before, sn);
            let e = cm::before_indent(sn);
            // The line above the blank lines ends with a block scalar's
            // text: blank lines there are the scalar's to read.
            let scalar_end =
                |t: TreeNode<'_>| t.kind() != SyntaxKind::LineBreak && cm::ends_line(t);
            let mut after_scalar = e.is_some_and(scalar_end);
            // The blank lines above, nearest first (rule 8 counts them the
            // same way). A loop back over the tokens, not recursion.
            let mut lines = Vec::new();
            let mut at = e;
            while let Some(b) = at.filter(|b| b.kind() == SyntaxKind::LineBreak) {
                let mut ws = Vec::new();
                let mut u = place::prev_token(b);
                while let Some(t) = u.filter(|t| cm::blankish(*t)) {
                    // (Zero-width tokens stay: they are the parser's.)
                    if t.kind() == SyntaxKind::Whitespace {
                        ws.push(t.id());
                    }
                    u = place::prev_token(t);
                }
                match u {
                    Some(t) if t.kind() == SyntaxKind::LineBreak => {
                        lines.push((ws, b.id()));
                        at = Some(t);
                    }
                    None => {
                        lines.push((ws, b.id()));
                        break;
                    }
                    Some(t) if cm::ends_line(t) => {
                        lines.push((ws, b.id()));
                        after_scalar = scalar_end(t);
                        break;
                    }
                    Some(_) => break,
                }
            }
            (lines, e.map(|e| e.id()), after_scalar)
        };
        let current = lines.len();
        if current == n {
            return Ok(());
        }
        let scalar_content = || {
            unsupported(
                "blank lines right after a block scalar (a kept `|+` scalar reads them as its \
                 content)",
            )
        };
        if n > current {
            // The new text is bounded before it is
            // allocated: checked arithmetic, `isize::MAX` (what a `String`
            // can hold), and `max_document_size` (`Y2002`, as the syntax
            // layer reports it for every other edit).
            let add = n.saturating_sub(current);
            let total = self.tree().root().span().end;
            let max = self.settings().get_limits().get_max_document_size();
            let fits = add
                .checked_mul(self.nl().len())
                .and_then(|b| total.checked_add(b))
                .is_some_and(|t| t <= max && isize::try_from(t).is_ok());
            if !fits {
                return Err(super::edit::too_large(Span::new(total, total)));
            }
        }
        let r = self.atomic(|d| {
            if n < current {
                let drop = current.saturating_sub(n);
                for (ws, b) in lines.iter().take(drop) {
                    // The line's white space and its line break, removed
                    // as siblings where they are (else one by one).
                    let first = ws.last().copied();
                    let siblings = first.is_some_and(|f| {
                        let t = d.tree();
                        let mut at = t.get(f).ok();
                        while let Some(x) = at {
                            if x.id() == *b {
                                return true;
                            }
                            if !ws.contains(&x.id()) {
                                return false;
                            }
                            at = x.next_sibling();
                        }
                        false
                    });
                    match first {
                        Some(f) if siblings => d.t_remove(f, *b)?,
                        _ => {
                            for w in ws {
                                d.t_remove(*w, *w)?;
                            }
                            d.t_remove(*b, *b)?;
                        }
                    }
                }
            } else {
                let add = n.saturating_sub(current);
                let puts = {
                    let e = match end {
                        Some(e) => Some(d.tree().get(e).map_err(|_| EditError::StaleNode)?),
                        None => None,
                    };
                    after_line(d.tree().root(), e)
                };
                let frag = d.trivia_fragment(&d.nl().repeat(add))?;
                d.try_puts(&puts, &frag, &mut before)?;
            }
            d.check_after(&before, &[], None)
        });
        match r {
            // The syntax layer refuses lines a block scalar
            // would read; named, as the value check names them.
            Err(EditError::NotWellFormed { .. }) if after_scalar => return Err(scalar_content()),
            Err(e) => return Err(e),
            Ok(()) => {}
        }
        self.finish_comment_edit(&[]);
        Ok(())
    }

    /// Replaces the document header ([`Document::header`]) with one
    /// comment line per item of `lines` (view text), at column 0: before
    /// the directives and `---` when the document has them (rule 1),
    /// otherwise at the top of the document followed by a blank line
    /// (rule 2; an existing blank line is reused). [`NO_COMMENTS`](crate::NO_COMMENTS) removes the
    /// header. The header changes only through this call. Returns
    /// the new comments' handles.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("# lead a\na: 1\n").unwrap();
    /// doc.set_header(&["config", "v2"]).unwrap();
    /// assert_eq!(doc.to_string(), "# config\n# v2\n\n# lead a\na: 1\n");
    /// assert_eq!(doc.header().unwrap().len(), 2);
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidComment`]; [`EditError::ReadOnly`] (the root is
    /// an error region or next to one);
    /// [`EditError::CommentReattaches`] should the lines not read as the
    /// header there. Nothing changes on error.
    pub fn set_header<I>(&mut self, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.prepare()?;
        let raws = self.checked_raws(lines)?;
        self.set_header_raws(raws)
    }

    fn set_header_raws(&mut self, raws: Vec<String>) -> Result<Vec<CommentId>, EditError> {
        let (old, mut before) = {
            if let Some(r) = self.root_tree() {
                self.check_region(&[r], r.parent())?;
            }
            let f = self.find_doc_comments()?;
            let old: Vec<TreeId> = f.leading.iter().map(|h| h.node.id()).collect();
            let mut before = self.before(&f.seen, &old.iter().copied().collect());
            self.border_all(&mut before, &old);
            (old, before)
        };
        if old.is_empty() && raws.is_empty() {
            return Ok(Vec::new());
        }
        let want = Some(self.key_of(CommentOwner::DocumentHeader));
        let new = self.atomic(|d| {
            d.drop_comments(&old)?;
            let new = if raws.is_empty() {
                Vec::new()
            } else {
                d.insert_header(&raws, &mut before)?
            };
            d.check_after(&before, &new, want)?;
            Ok(new)
        })?;
        self.finish_comment_edit(&old);
        Ok(new.into_iter().map(CommentId::of).collect())
    }

    /// Writes the header lines `raws` (see [`Document::set_header`]).
    fn insert_header(
        &mut self,
        raws: &[String],
        before: &mut Before,
    ) -> Result<Vec<TreeId>, EditError> {
        /// Where the lines go.
        enum At {
            /// At the end of an input without documents.
            End(bool),
            /// Right above this token's line (rule 1).
            Above(TreeId),
            /// After the line this token ends (`None`: the first line),
            /// with an empty line after them unless one follows.
            Top(Option<TreeId>, bool),
        }
        let at = match self.doc_node() {
            None => {
                let text = self.tree().root().text();
                At::End(!text.is_empty() && !text.ends_with(['\n', '\r']))
            }
            Some(d) => {
                let ft = cm::first_token(d).ok_or_else(|| internal("a document without tokens"))?;
                let started = d.first_child().is_some_and(|f| {
                    matches!(
                        f.kind(),
                        SyntaxKind::DocumentStartMarker
                            | SyntaxKind::YamlDirective
                            | SyntaxKind::TagDirective
                            | SyntaxKind::ReservedDirective
                    )
                });
                if started {
                    At::Above(ft.id())
                } else {
                    // The first line after the content before the document.
                    let mut ignored = Vec::new();
                    let e = match cm::comments_back(ft, false, &mut ignored) {
                        None => None,
                        Some(v) => {
                            let mut at = Some(v);
                            while let Some(t) = at.filter(|t| !cm::ends_line(*t)) {
                                at = place::next_token(t);
                            }
                            Some(at.ok_or_else(|| {
                                internal("content before a document ends no line")
                            })?)
                        }
                    };
                    let blank = blank_follows(self.tree().root(), e);
                    At::Top(e.map(|e| e.id()), !blank)
                }
            }
        };
        // (Each place tried is recorded before it is tried, after the
        // removals: a scalar their runs touch was recorded before them,
        // and no other value moved.)
        match at {
            At::End(lead) => {
                let root = self.tree().root().id();
                let text = lines_text("", raws, self.nl(), lead, true, false);
                let frag = self.trivia_fragment(&text)?;
                self.border_put(before, Put::Append(root));
                let ins = self.t_insert(Put::Append(root), frag)?;
                Ok(self.comments_in(ins))
            }
            At::Above(t) => self.insert_lines_above(t, "", raws, before),
            At::Top(e, blank) => self.insert_after_line(e, "", raws, blank, before),
        }
    }

    /// Replaces the document footer ([`Document::footer`]) with one
    /// comment line per item of `lines` (view text), at column 0 after the
    /// root's last line and whatever comment lines follow it, before the
    /// document's `...` (rule 7). [`NO_COMMENTS`](crate::NO_COMMENTS) removes the footer. The footer
    /// changes only through this call. Returns the new comments'
    /// handles.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("a: 1\n").unwrap();
    /// doc.set_footer(&["end"]).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1\n# end\n");
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidComment`]; [`EditError::ReadOnly`];
    /// [`EditError::CommentReattaches`] when a footer cannot be written:
    /// the document has no root (rule 2 makes its comments header), or a
    /// next document starts with `---` right after it without `...`
    /// between (rule 1 makes the lines its header). Nothing changes on
    /// error.
    pub fn set_footer<I>(&mut self, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.prepare()?;
        let raws = self.checked_raws(lines)?;
        self.set_footer_raws(raws)
    }

    fn set_footer_raws(&mut self, raws: Vec<String>) -> Result<Vec<CommentId>, EditError> {
        let (old, mut before) = {
            if let Some(r) = self.root_tree() {
                self.check_region(&[r], r.parent())?;
            }
            let f = self.find_doc_comments()?;
            let old: Vec<TreeId> = f.footer.iter().map(|h| h.node.id()).collect();
            let mut before = self.before(&f.seen, &old.iter().copied().collect());
            self.border_all(&mut before, &old);
            (old, before)
        };
        if old.is_empty() && raws.is_empty() {
            return Ok(Vec::new());
        }
        if !raws.is_empty() && self.root_tree().is_none() {
            return Err(EditError::CommentReattaches {
                rule: CommentRule::HeaderBlankLine,
            });
        }
        let want = Some(self.key_of(CommentOwner::DocumentFooter));
        let new = self.atomic(|d| {
            d.drop_comments(&old)?;
            let new = if raws.is_empty() {
                Vec::new()
            } else {
                let s = d.footer_sibling()?;
                d.insert_lines_below(s, "", &raws, &mut before)?
            };
            d.check_after(&before, &new, want)?;
            Ok(new)
        })?;
        self.finish_comment_edit(&old);
        Ok(new.into_iter().map(CommentId::of).collect())
    }

    /// The last line of the document before its `...` (or the next
    /// document): the token the footer lines go after.
    fn footer_sibling(&self) -> Result<TreeId, EditError> {
        let d = self
            .doc_node()
            .ok_or_else(|| internal("a footer without a document"))?;
        let r = self
            .root_tree()
            .ok_or_else(|| internal("a footer without a root"))?;
        let mut last = cm::last_token(r).ok_or_else(|| internal("a root without tokens"))?;
        let mut at = place::next_token(r);
        while let Some(t) = at {
            if cm::visible(t) || cm::in_other_document(t, d) {
                break;
            }
            if t.kind() != SyntaxKind::Whitespace {
                last = t;
            }
            at = place::next_token(t);
        }
        Ok(climb(last).id())
    }
}
