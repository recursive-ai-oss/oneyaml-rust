//! Comment attachment: the one owner of every comment, decided by
//! the numbered rules of `docs/comment-attachment.md` (the normative
//! rulebook; each [`AttachRule`] names one of its rules).
//!
//! **One tree.** Owners are read from the syntax tree's own
//! tokens: comments are [`SyntaxKind::Comment`](super::SyntaxKind::Comment) tokens, lines end at
//! [`SyntaxKind::LineBreak`](super::SyntaxKind::LineBreak) tokens or at a block scalar's text (which
//! ends with its line break), and columns are counted over the tokens of
//! a line. There is no second scanner over the source text, so an edited
//! tree is read exactly as a parsed one.
//!
//! **On demand.** No owner is computed or stored while parsing:
//! [`SyntaxTree::comment_attachment`] decides one comment in
//! O(depth + log + its line) from the comment-run index (`runs.rs`:
//! per-arena facts computed with the arena, and treap sums; P and N,
//! the documents around a stray `...`, siblings past comment lines, and
//! rule 13's floor without walking the run), plus, for a header owner, the
//! blank lines after it in its run; [`SyntaxTree::comment_attachments`]
//! and [`SyntaxTree::document_comment_attachments`] decide every comment
//! in one O(n) pass that computes each run's context once (the run's
//! neighbours, the block collections that close in it, its document) and
//! shares it among the run's comments. No decision scans a document's
//! children: it reads the kinds of the neighbouring tokens and of the
//! document children holding them. The passes never scan the
//! stream's elements per comment either (they table the documents around
//! each stream-level element once). Both paths call the same decision
//! code, so they always agree (pinned by tests over the yaml-test-suite
//! corpus, a differential fuzz, and `tests/comment_index.rs` for long
//! runs and edited trees).
//!
//! **No recursion.** Every walk here is a loop over tokens or ancestors;
//! nothing recurses.

use super::{Node, SyntaxKind as K, SyntaxTree};

/// The numbered rule of `docs/comment-attachment.md` that decided a
/// comment's owner. Rule numbers are stable once published (stability policy);
/// rule 8 (blank-line counts) decides no owner and has
/// no variant.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AttachRule {
    /// Rule 1: an own-line comment before the document's `---` (directives
    /// included) is document header.
    HeaderBeforeStart,
    /// Rule 2: an own-line comment before the root with a blank line after
    /// it (before the root), or in a document without visible content, is
    /// document header.
    HeaderBlankLine,
    /// Rule 3: an own-line comment before the root with no blank line
    /// after it leads the first entry, item, or node the root opens with.
    FirstNodeLeading,
    /// Rule 4: an own-line comment above a node (blank lines between
    /// allowed), at or right of its column, leads that node (for a key or
    /// `-`, the entry or item).
    Leading,
    /// Rule 5: a comment after content on its line trails the entry or
    /// item that content belongs to (or the root, or the document marker's
    /// header or footer).
    Trailing,
    /// Rule 6: an own-line comment after a block collection's last child,
    /// right of the column of what follows, is that collection's footer.
    CollectionFooter,
    /// Rule 7: an own-line comment after the root at root column (or after
    /// the last document) is document footer.
    DocumentFooter,
    /// Rule 9: an own-line comment inside a flow collection leads the next
    /// item, or is the collection's footer before its closing bracket.
    Flow,
    /// Rule 10: an own-line comment left of the column of the block node
    /// that follows it still leads that node.
    ShallowLeading,
    /// Rule 11: an own-line comment directly above the `:` of an explicit
    /// (`? `) entry leads that entry's value.
    ExplicitValue,
    /// Rule 12: an own-line comment followed by a property, a block scalar
    /// header, or error bytes inside a node leads the innermost node
    /// holding them.
    Inside,
    /// Rule 13: within one run of comment lines owners only move outward;
    /// a comment whose other rules would give it a deeper owner than an
    /// earlier comment of its run takes the owner of the comment before it.
    RunOrder,
}

impl AttachRule {
    /// The rule's number in `docs/comment-attachment.md` (stable).
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::HeaderBeforeStart => 1,
            Self::HeaderBlankLine => 2,
            Self::FirstNodeLeading => 3,
            Self::Leading => 4,
            Self::Trailing => 5,
            Self::CollectionFooter => 6,
            Self::DocumentFooter => 7,
            Self::Flow => 9,
            Self::ShallowLeading => 10,
            Self::ExplicitValue => 11,
            Self::Inside => 12,
            Self::RunOrder => 13,
        }
    }
}

/// Who owns a comment.
///
/// A mapping **entry** is named by its key node, a sequence **item** by
/// its node; a key's leading and trailing comments are its entry's.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommentOwner<'t> {
    /// The header of this [`SyntaxKind::Document`](super::SyntaxKind::Document) (the
    /// [`SyntaxKind::Stream`](super::SyntaxKind::Stream) when the input has no document).
    DocumentHeader(Node<'t>),
    /// The footer of this [`SyntaxKind::Document`](super::SyntaxKind::Document).
    DocumentFooter(Node<'t>),
    /// A leading comment of this entry (key), item, or node.
    Leading(Node<'t>),
    /// The trailing (same-line) comment of this entry (key), item, or
    /// node.
    Trailing(Node<'t>),
    /// The footer of this collection.
    Footer(Node<'t>),
}

/// A comment's owner, the rule that decided it, and the blank lines above
/// it (rule 8 of the comment rulebook).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Attachment<'t> {
    owner: CommentOwner<'t>,
    rule: AttachRule,
    blank_lines_before: usize,
    document: Option<Node<'t>>,
}

impl<'t> Attachment<'t> {
    /// The owner.
    #[must_use]
    pub const fn owner(&self) -> CommentOwner<'t> {
        self.owner
    }

    /// The rule that decided the owner.
    #[must_use]
    pub const fn rule(&self) -> AttachRule {
        self.rule
    }

    /// Blank lines between the previous non-blank line and the comment's
    /// line (rule 8); 0 for a same-line comment. Blank lines that are a
    /// block scalar's content (kept by `|+`) are not counted.
    #[must_use]
    pub const fn blank_lines_before(&self) -> usize {
        self.blank_lines_before
    }

    /// The [`SyntaxKind::Document`](super::SyntaxKind::Document) the comment belongs to; `None` only
    /// when the input has no document.
    #[must_use]
    pub const fn document(&self) -> Option<Node<'t>> {
        self.document
    }
}

impl SyntaxTree {
    /// The attachment of `comment`, a [`SyntaxKind::Comment`](super::SyntaxKind::Comment)
    /// token of this tree; `None` for any other element.
    ///
    /// Cost: O(depth + log + its line) from the comment-run
    /// index, plus, for a comment whose owner is a document header or the
    /// first node (rules 2/3), the blank lines and comments after it in its
    /// run. Columns beyond `u32` and recovered input with both header and
    /// footer owners in one run replay the run (O(the run)).
    #[must_use]
    pub fn comment_attachment<'t>(&'t self, comment: Node<'t>) -> Option<Attachment<'t>> {
        if comment.kind() != K::Comment || !core::ptr::eq(comment.tree(), self) {
            return None;
        }
        let around = Around::Scan;
        let a = match line_of(comment) {
            Line::Same(p) => same_line(locate(p), &around),
            Line::Own(end) => match self.decide_indexed(comment, end, Want::Attachment) {
                Some(Decided::Attachment(a)) => a,
                _ => self.replay(comment)?,
            },
        };
        Some(a)
    }

    /// The document [`SyntaxTree::comment_attachment`] gives `comment`
    /// ([`Attachment::document`]): the outer `None` when `comment` is not a
    /// [`SyntaxKind::Comment`](super::SyntaxKind::Comment) token of this
    /// tree. O(depth + log) with the comment-run index,
    /// never the run around it or the stream-level elements between it
    /// and its neighbouring documents (the rules' document never depends
    /// on the blank lines after a comment, which its header owner may).
    #[must_use]
    pub fn comment_document<'t>(&'t self, comment: Node<'t>) -> Option<Option<Node<'t>>> {
        if comment.kind() != K::Comment || !core::ptr::eq(comment.tree(), self) {
            return None;
        }
        match line_of(comment) {
            Line::Same(p) => Some(same_line(locate(p), &Around::Scan).document),
            Line::Own(end) => match self.decide_indexed(comment, end, Want::Document) {
                Some(Decided::Document(d)) => Some(d),
                Some(Decided::Attachment(a)) => Some(a.document),
                None => self.replay(comment).map(|a| a.document),
            },
        }
    }

    /// The last content token before `n` in source order (a token with
    /// bytes that is not trivia: what the rulebook calls content; comments,
    /// white space, line breaks and zero-width tokens are skipped); `None`
    /// at the start of the text or for another tree's element.
    /// O(log) with the comment-run index, never the comment lines
    /// before `n`.
    #[must_use]
    pub fn prev_content_token<'t>(&'t self, n: Node<'t>) -> Option<Node<'t>> {
        core::ptr::eq(n.tree(), self)
            .then(|| prev_visible(n))
            .flatten()
    }

    /// The first content token after `n` in source order (inside `n` for a
    /// node with content), as [`SyntaxTree::prev_content_token`].
    #[must_use]
    pub fn next_content_token<'t>(&'t self, n: Node<'t>) -> Option<Node<'t>> {
        core::ptr::eq(n.tree(), self)
            .then(|| next_visible(n))
            .flatten()
    }

    /// The last own-line comment of the run after block collection
    /// `collection` (the comment lines between its last content token and
    /// the next one) whose owner is the footer of `collection` or of a
    /// collection whose span lies within its span (rules 6 and 13: those
    /// comments open the run, each owner's together); `None` when there is
    /// none, or `collection` is not a block collection of this tree.
    ///
    /// O(depth + log) with the comment-run index, plus the
    /// blank lines before the next comment, never the footer's length:
    /// what `oneyaml`'s `Document::push_footer` appends after.
    #[must_use]
    pub fn last_footer_comment<'t>(&'t self, collection: Node<'t>) -> Option<Node<'t>> {
        if !is_block(collection.kind()) || !core::ptr::eq(collection.tree(), self) {
            return None;
        }
        let p = last_visible_in(collection)?;
        let n = next_visible(p);
        let around = Around::Scan;
        let ahead = Ahead {
            next: n,
            last_blank: None,
        };
        let plan = make_plan(self, Some(locate(p)), &ahead, &around);
        if !cols_fit(&plan) {
            return self.last_footer_comment_walk(collection, p, &plan);
        }
        let i = plan.footer.iter().position(|(k, _)| *k == collection)?;
        let span = collection.span();
        let within = |y: Node<'_>| {
            let s = y.span();
            span.start <= s.start && s.end <= span.end
        };
        // The deepest rank whose owner lies within `collection` (ranks up
        // to `i` are collections inside it).
        let mut j = i;
        while plan
            .footer
            .get(j.saturating_add(1))
            .is_some_and(|(k, _)| within(*k))
        {
            j = j.saturating_add(1);
        }
        // The first own-line comment ranked deeper than `j` ends them.
        let cut = cut_below(&plan, j.saturating_add(1))?;
        let stop = self.run_hit(p.slot(), cut);
        let stop = stop.and_then(|s| self.node_at_slot(s));
        // Back over the trivia before it (no comment lies between).
        let mut at = match stop {
            Some(s) => prev_token(s),
            None => last_token(self),
        };
        while let Some(t) = at {
            if t == p || visible(t) {
                return None;
            }
            if t.kind() == K::Comment {
                return matches!(line_of(t), Line::Own(_)).then_some(t);
            }
            at = prev_token(t);
        }
        None
    }

    /// [`SyntaxTree::last_footer_comment`] by deciding the run (columns
    /// beyond the index's range).
    fn last_footer_comment_walk<'t>(
        &'t self,
        collection: Node<'t>,
        p: Node<'t>,
        plan: &Plan<'t>,
    ) -> Option<Node<'t>> {
        let span = collection.span();
        let within = |y: Node<'_>| {
            let s = y.span();
            span.start <= s.start && s.end <= span.end
        };
        let mut floor = None;
        let mut last = None;
        let mut at = next_token(p);
        while let Some(t) = at {
            if visible(t) {
                break;
            }
            if t.kind() == K::Comment {
                if let Line::Own(end) = line_of(t) {
                    let a = decide(plan, t, col_of(t), blank_before(end), &mut floor);
                    match a.owner {
                        CommentOwner::Footer(y) if within(y) => last = Some(t),
                        _ => break,
                    }
                }
            }
            at = next_token(t);
        }
        last
    }

    /// The attachment (or only the document, `want`) of own-line comment
    /// `c` (the previous line ending with `end`) from the comment-run
    /// index: P and N found in O(log), and rule 13's floor —
    /// the deepest rank among the run's comments before `c`, which is the
    /// rank of the smallest column among them — found by asking for the
    /// first comment below each rank's column (O(log footer) questions).
    /// `None` where the index does not decide: a header owner whose run
    /// also has footer ranks (only recovered input has them), columns
    /// beyond `u32`.
    fn decide_indexed<'t>(
        &'t self,
        c: Node<'t>,
        end: Option<Node<'t>>,
        want: Want,
    ) -> Option<Decided<'t>> {
        let p = prev_visible(c);
        let n = next_visible(c);
        let around = Around::Scan;
        let ahead = Ahead {
            next: n,
            last_blank: None,
        };
        let mut plan = make_plan(self, p.map(locate), &ahead, &around);
        if !cols_fit(&plan) {
            return None;
        }
        let outer = plan.footer.len();
        let header = matches!(plan.otherwise, Otherwise::Header { .. });
        let col = col_of(c);
        let floor = self.floor_rank(&plan, p, c)?;
        if let Want::Document = want {
            let rank = floor.max(own_rank(&plan, col));
            let doc = if rank < outer {
                plan.footer_document
            } else {
                plan.document
            };
            return Some(Decided::Document(doc));
        }
        if header && outer > 0 {
            return None;
        }
        if let Otherwise::Header { last_blank, .. } = &mut plan.otherwise {
            // Only the blank lines after `c` matter to it (O(the rest of
            // the run)); the run's own decision reads them from its first
            // comment on, which finds the same ones after `c`.
            *last_blank = scan_ahead(c).last_blank;
        }
        let blank = blank_before(end);
        let (a, rank) = decide_one(&plan, c, col, blank);
        if floor <= rank {
            return Some(Decided::Attachment(a));
        }
        // Rule 13: the owner of the comment that set the floor.
        let (owner, document) = match plan.footer.get(floor) {
            Some((k, _)) => (CommentOwner::Footer(*k), plan.footer_document),
            None => match plan.otherwise {
                Otherwise::Fixed(o, _) => (o, plan.document),
                Otherwise::Leading { target, .. } => (CommentOwner::Leading(target), plan.document),
                Otherwise::Header { .. } => {
                    // An invariant, never a panic: not
                    // reached. A header plan with footer ranks returned
                    // `None` above (`header && outer > 0`); without them
                    // (`outer == 0`) `floor_rank` is 0, never above `rank`.
                    // In release the run is replayed (`None`), the decision
                    // the index stands in for, never a guess.
                    debug_assert!(false, "rule 13 above a header owner");
                    return None;
                }
            },
        };
        Some(Decided::Attachment(Attachment {
            owner,
            rule: AttachRule::RunOrder,
            blank_lines_before: blank,
            document,
        }))
    }

    /// Rule 13's floor at own-line comment `c` after content token `p`:
    /// the deepest rank of the run's own-line comments before `c` (0 for
    /// none). Ranks are non-increasing in the column, so "rank at least
    /// k" is "column below `cut_below(k)`", and the floor is the largest
    /// k for which the index finds such a comment before `c`. O(log
    /// footer) index questions.
    fn floor_rank(&self, plan: &Plan<'_>, p: Option<Node<'_>>, c: Node<'_>) -> Option<usize> {
        let outer = plan.footer.len();
        if outer == 0 {
            return Some(0);
        }
        let from = p.map_or(0, Node::slot);
        let at = c.preorder_index();
        let exists = |k: usize| -> Option<bool> {
            let cut = cut_below(plan, k)?;
            Some(
                self.run_hit(from, cut)
                    .and_then(|s| self.node_at_slot(s))
                    .is_some_and(|h| h.preorder_index() < at),
            )
        };
        // The largest k in 0..=outer with `exists(k)` (true at 0).
        let (mut lo, mut hi) = (0usize, outer);
        while lo < hi {
            let mid = lo.saturating_add(hi.saturating_sub(lo).saturating_add(1) / 2);
            if exists(mid)? {
                lo = mid;
            } else {
                hi = mid.saturating_sub(1);
            }
        }
        Some(lo)
    }

    /// [`SyntaxTree::comment_attachment`] of own-line `comment` by
    /// replaying its run (the decision the index stands in for).
    fn replay<'t>(&'t self, comment: Node<'t>) -> Option<Attachment<'t>> {
        let around = Around::Scan;
        // The run's comments up to this one, and P before them: the
        // run is decided in order, as the pass decides it (rule 13).
        let mut run = Vec::new();
        let mut p = None;
        let mut at = prev_token(comment);
        while let Some(t) = at {
            if visible(t) {
                p = Some(t);
                break;
            }
            if t.kind() == K::Comment {
                run.push(t);
            }
            at = prev_token(t);
        }
        run.reverse();
        run.push(comment);
        let own: Vec<(Node<'t>, Option<Node<'t>>)> = run
            .into_iter()
            .filter_map(|r| match line_of(r) {
                Line::Own(end) => Some((r, end)),
                Line::Same(_) => None,
            })
            .collect();
        let first = own.first().map_or(comment, |(r, _)| *r);
        let plan = make_plan(self, p.map(locate), &scan_ahead(first), &around);
        let mut floor = None;
        let mut last = None;
        for (r, end) in own {
            last = Some(decide(&plan, r, col_of(r), blank_before(end), &mut floor));
        }
        last
    }

    /// The attachments of every comment of the **run** holding `comment` (a
    /// [`SyntaxKind::Comment`](super::SyntaxKind::Comment) token of this
    /// tree): the comments between the content token before it and the
    /// content token after it, in source order, decided as
    /// [`SyntaxTree::comment_attachments`] decides them (the same code).
    /// Empty for any other element.
    ///
    /// The comments of one owner are
    /// read, and a comment edit is checked, one run at a time, in
    /// O(depth + the run + its lines); next to stray `...` markers between
    /// documents, the documents around the run are found in O(log) with
    /// the comment-run index, never by walking the stream-level
    /// elements between them.
    #[must_use]
    pub fn comment_run_attachments<'t>(
        &'t self,
        comment: Node<'t>,
    ) -> Vec<(Node<'t>, Attachment<'t>)> {
        if comment.kind() != K::Comment || !core::ptr::eq(comment.tree(), self) {
            return Vec::new();
        }
        // The run: back to P, then on to N (loops over tokens).
        let mut run = Vec::new();
        let mut p = None;
        let mut at = prev_token(comment);
        while let Some(t) = at {
            if visible(t) {
                p = Some(t);
                break;
            }
            if t.kind() == K::Comment {
                run.push(t);
            }
            at = prev_token(t);
        }
        run.reverse();
        run.push(comment);
        let mut at = next_token(comment);
        while let Some(t) = at {
            if visible(t) {
                break;
            }
            if t.kind() == K::Comment {
                run.push(t);
            }
            at = next_token(t);
        }
        let around = Around::Scan;
        let from = p.map(locate);
        let mut plan: Option<Plan<'t>> = None;
        let mut floor = None;
        let mut out = Vec::with_capacity(run.len());
        for r in run {
            let a = match line_of(r) {
                Line::Same(q) => same_line(locate(q), &around),
                Line::Own(end) => {
                    // The plan is made at the run's first own-line comment,
                    // as the pass makes it.
                    let plan =
                        plan.get_or_insert_with(|| make_plan(self, from, &scan_ahead(r), &around));
                    decide(plan, r, col_of(r), blank_before(end), &mut floor)
                }
            };
            out.push((r, a));
        }
        out
    }

    /// Every comment of the stream-level elements strictly between
    /// documents `before` and `after` (`None`: the start, the end of the
    /// stream), with their attachments, in source order, decided as
    /// [`SyntaxTree::comment_attachments`] decides them (the same pass).
    /// Empty when `before` or `after` is not a document of this tree, or
    /// `after` does not follow `before`.
    ///
    /// The documents around the stretch are found once
    /// for all of its runs, so a stretch of N stray `...` markers and
    /// comments costs O(N), where deciding its N runs one at a time
    /// ([`SyntaxTree::comment_run_attachments`]) walks the stretch for
    /// each, O(N²).
    #[must_use]
    pub fn comment_attachments_between<'t>(
        &'t self,
        before: Option<Node<'t>>,
        after: Option<Node<'t>>,
    ) -> Vec<(Node<'t>, Attachment<'t>)> {
        let is_doc = |d: Option<Node<'t>>| {
            d.is_none_or(|d| {
                d.kind() == K::Document
                    && core::ptr::eq(d.tree(), self)
                    && d.parent().is_some_and(|p| p.kind() == K::Stream)
            })
        };
        if !is_doc(before) || !is_doc(after) {
            return Vec::new();
        }
        let mut tops = Vec::new();
        let mut at = match before {
            Some(b) => b.next_sibling(),
            None => self.root().first_child(),
        };
        // A loop over the stream's children.
        while let Some(n) = at {
            if Some(n) == after {
                break;
            }
            tops.push(n);
            at = n.next_sibling();
        }
        if at != after {
            return Vec::new();
        }
        let start = tops.first().and_then(|f| prev_visible(*f)).map(locate);
        pass(self, &tops, start, before, after, None)
    }

    /// Every comment of the tree with its attachment, in source order. One
    /// O(n) pass (see the module docs); nothing is kept afterwards.
    #[must_use]
    pub fn comment_attachments(&self) -> Vec<(Node<'_>, Attachment<'_>)> {
        let tops: Vec<Node<'_>> = self.root().children().collect();
        pass(self, &tops, None, None, None, None)
    }

    /// The comments that belong to `document` (a
    /// [`SyntaxKind::Document`](super::SyntaxKind::Document) of this tree),
    /// with their attachments, in source order: its own and those between
    /// it and its neighbouring documents that the rules give it. O(the
    /// document and the stream-level elements around it), never the rest
    /// of the stream; empty for any other element.
    #[must_use]
    pub fn document_comment_attachments<'t>(
        &'t self,
        document: Node<'t>,
    ) -> Vec<(Node<'t>, Attachment<'t>)> {
        if document.kind() != K::Document || !core::ptr::eq(document.tree(), self) {
            return Vec::new();
        }
        // From the element after the previous document to the element
        // before the next one: a comment outside that range belongs to
        // another document.
        let mut first = document;
        let mut before = None;
        while let Some(p) = first.prev_sibling() {
            if p.kind() == K::Document {
                before = Some(p);
                break;
            }
            first = p;
        }
        let start = prev_visible(first).map(locate);
        let mut tops = Vec::new();
        let mut at = Some(first);
        let mut after = None;
        while let Some(n) = at {
            if n.kind() == K::Document && n != document {
                after = Some(n);
                break;
            }
            tops.push(n);
            at = n.next_sibling();
        }
        pass(self, &tops, start, before, after, Some(document))
    }
}

/// The trivia tokens of document `doc` after its content token `p` (its
/// last one), or before its first content token when `p` is `None`: in
/// recovered input a document can end or begin with them, and their
/// comments can belong to the neighbouring document. O(those
/// tokens).
fn edge_tokens<'t>(doc: Node<'t>, p: Option<Node<'t>>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut at = match p {
        Some(p) => next_token(p),
        None => Some(first_token(doc)).filter(|t| t.is_token()),
    };
    while let Some(t) = at {
        if visible(t) || !contains(doc, t) {
            break;
        }
        out.push(t);
        at = next_token(t);
    }
    out
}

/// A content token and where it sits: its document and the document's
/// child that holds it (`None`, `None` at stream level).
#[derive(Clone, Copy)]
struct At<'t> {
    tok: Node<'t>,
    doc: Option<Node<'t>>,
    child: Option<Node<'t>>,
}

/// Where `tok` sits (O(depth): a climb to its document).
fn locate(tok: Node<'_>) -> At<'_> {
    let mut below = tok;
    for a in tok.ancestors() {
        if a.kind() == K::Document {
            return At {
                tok,
                doc: Some(a),
                child: Some(below),
            };
        }
        below = a;
    }
    At {
        tok,
        doc: None,
        child: None,
    }
}

/// The documents around a stream-level element (a stray `...` or an
/// error region between documents).
enum Around<'t> {
    /// Found for one comment from the comment-run index (`runs.rs`:
    /// O(log), never a walk over the stream's elements).
    Scan,
    /// Known to the pass for the stream-level element it is in (a
    /// pass never walks siblings per comment).
    Known {
        prev: Option<Node<'t>>,
        next: Option<Node<'t>>,
    },
}

impl<'t> Around<'t> {
    /// The last document before the stream-level element holding `x` (a
    /// token outside every document, possibly inside a stream-level error
    /// region).
    fn prev_doc(&self, x: Node<'t>) -> Option<Node<'t>> {
        match self {
            Self::Known { prev, .. } => *prev,
            // From the index (O(log)), never a walk over the
            // stream's elements; the last document before `x` is the last
            // one before the stream-level element holding it.
            Self::Scan => x.tree().document_before(x.slot()),
        }
    }

    /// The first document after the stream-level element holding `x`.
    fn next_doc(&self, x: Node<'t>) -> Option<Node<'t>> {
        match self {
            Self::Known { next, .. } => *next,
            Self::Scan => x.tree().document_after(x.slot()),
        }
    }
}

/// The attachments of the comments under `tops` (consecutive
/// stream-level elements), from content token `start` on; `before` and
/// `after` are the documents right outside `tops`; only those of document
/// `want` when given. One pass (O(n)): each content token's
/// document and document child are tracked, not climbed to; a run's
/// [`Plan`] is made at its first own-line comment and shared by the rest;
/// the documents around each stream-level element are tabled once.
fn pass<'t>(
    tree: &'t SyntaxTree,
    tops: &[Node<'t>],
    start: Option<At<'t>>,
    before: Option<Node<'t>>,
    after: Option<Node<'t>>,
    want: Option<Node<'t>>,
) -> Vec<(Node<'t>, Attachment<'t>)> {
    let is_doc = |n: &Node<'t>| n.kind() == K::Document;
    // The documents strictly before and after each top.
    let mut prevs = Vec::with_capacity(tops.len());
    let mut last = before;
    for t in tops {
        prevs.push(last);
        if is_doc(t) {
            last = Some(*t);
        }
    }
    let mut nexts = vec![None; tops.len()];
    let mut next = after;
    for (t, slot) in tops.iter().zip(nexts.iter_mut()).rev() {
        *slot = next;
        if is_doc(t) {
            next = Some(*t);
        }
    }
    // Only for one document's pass (`want`): the trivia at the end of the
    // document before and at the start of the document after the tops.
    let tail = match (want, before, start) {
        (Some(_), Some(b), Some(p)) if p.doc == Some(b) => edge_tokens(b, Some(p.tok)),
        _ => Vec::new(),
    };
    let head = match (want, after) {
        (Some(_), Some(a)) => edge_tokens(a, None),
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    let mut p = start;
    let mut plan: Option<Plan<'t>> = None;
    let mut floor: Option<Floor<'t>> = None;
    let mut step =
        |t: Node<'t>, doc: Option<Node<'t>>, child: Option<Node<'t>>, around: &Around<'t>| {
            if t.kind() == K::Comment {
                let a = match line_of(t) {
                    Line::Same(q) => {
                        let at = match p {
                            Some(x) if x.tok == q => x,
                            _ => locate(q),
                        };
                        same_line(at, around)
                    }
                    Line::Own(end) => {
                        let plan =
                            plan.get_or_insert_with(|| make_plan(tree, p, &scan_ahead(t), around));
                        decide(plan, t, col_of(t), blank_before(end), &mut floor)
                    }
                };
                if want.is_none_or(|w| a.document == Some(w)) {
                    out.push((t, a));
                }
            } else if visible(t) {
                p = Some(At { tok: t, doc, child });
                plan = None;
                floor = None;
            }
        };
    // The tail of `before`: P is its last content token, and the first
    // document after it is the first top that is one (or `after`).
    let first_next = tops.iter().copied().find(is_doc).or(after);
    let tail_around = Around::Known {
        prev: None,
        next: first_next,
    };
    for t in tail {
        step(t, before, None, &tail_around);
    }
    for ((top, prev), next) in tops.iter().zip(prevs).zip(nexts) {
        let around = Around::Known { prev, next };
        if is_doc(top) {
            // Flat preorder loops, one per child of the document.
            for ch in top.children() {
                for t in ch.descendants() {
                    step(t, Some(*top), Some(ch), &around);
                }
            }
        } else {
            for t in top.descendants() {
                step(t, None, None, &around);
            }
        }
    }
    let last_doc = tops.iter().copied().rev().find(is_doc).or(before);
    let head_around = Around::Known {
        prev: last_doc,
        next: after,
    };
    for t in head {
        step(t, after, None, &head_around);
    }
    out
}

// ================================================================ kinds

fn is_item(k: K) -> bool {
    k.is_yaml_node() || k == K::Error
}

fn is_block(k: K) -> bool {
    matches!(k, K::BlockMapping | K::BlockSequence)
}

fn is_map(k: K) -> bool {
    matches!(k, K::BlockMapping | K::FlowMapping | K::FlowPair)
}

fn is_seq(k: K) -> bool {
    matches!(k, K::BlockSequence | K::FlowSequence)
}

fn is_flow(k: K) -> bool {
    matches!(k, K::FlowMapping | K::FlowSequence | K::FlowPair)
}

fn empty(n: Node<'_>) -> bool {
    n.span().is_empty()
}

/// A token with bytes that is not trivia: what a line's content is made
/// of.
fn visible(t: Node<'_>) -> bool {
    t.is_token() && !t.kind().is_trivia() && !empty(t)
}

/// A token that neither ends a line nor is content: white space, a byte
/// order mark, a zero-width token.
fn blankish(t: Node<'_>) -> bool {
    matches!(t.kind(), K::Whitespace | K::ByteOrderMark) || (t.kind() != K::LineBreak && empty(t))
}

/// The token ends its line (a line break, or a block scalar's text).
fn ends_line(t: Node<'_>) -> bool {
    t.kind() == K::LineBreak || (!empty(t) && t.text().ends_with(['\n', '\r']))
}

// =========================================================== navigation

fn prev_token(n: Node<'_>) -> Option<Node<'_>> {
    let mut at = n.pred();
    while let Some(x) = at {
        if x.is_token() {
            return Some(x);
        }
        at = x.pred();
    }
    None
}

fn next_token(n: Node<'_>) -> Option<Node<'_>> {
    let mut at = n.succ();
    while let Some(x) = at {
        if x.is_token() {
            return Some(x);
        }
        at = x.succ();
    }
    None
}

/// The last content token before `n` (comments, white space, line breaks,
/// zero-width tokens skipped). From the comment-run index
/// (`runs.rs`), O(log + `LONG_RUN`), never the run before `n`.
fn prev_visible(n: Node<'_>) -> Option<Node<'_>> {
    let t = n.tree();
    t.visible_before(n.slot()).and_then(|s| t.node_at_slot(s))
}

/// The first content token after `n` in preorder (inside `n` for a node).
/// From the comment-run index.
fn next_visible(n: Node<'_>) -> Option<Node<'_>> {
    let t = n.tree();
    t.run_hit(n.slot(), 0).and_then(|s| t.node_at_slot(s))
}

/// The last content token of `n`'s subtree.
fn last_visible_in(n: Node<'_>) -> Option<Node<'_>> {
    let t = n.tree();
    let leaf = t.node_at_slot(t.nav_last_leaf(n.slot())?)?;
    let last = if visible(leaf) {
        leaf
    } else {
        prev_visible(leaf)?
    };
    contains(n, last).then_some(last)
}

/// The tree's last token.
fn last_token(tree: &SyntaxTree) -> Option<Node<'_>> {
    let root = tree.root();
    let leaf = tree.node_at_slot(tree.nav_last_leaf(root.slot())?)?;
    if leaf.is_token() {
        Some(leaf)
    } else {
        prev_token(leaf)
    }
}

/// Own-line comment `c`'s column, `None` for a comment after content on
/// its line (`runs.rs` reads what its arena cannot tell this way).
pub(super) fn own_col(c: Node<'_>) -> Option<usize> {
    match line_of(c) {
        Line::Own(_) => Some(col_of(c)),
        Line::Same(_) => None,
    }
}

/// `n` lies in `a`'s subtree (or is `a`).
fn contains(a: Node<'_>, n: Node<'_>) -> bool {
    let ai = a.preorder_index();
    let ni = n.preorder_index();
    ai <= ni && ni < ai.saturating_add(a.descendants().len())
}

/// The column (characters from the line start) where token `t` starts. A
/// byte order mark is not a column (§5.2): the parser counts columns
/// after it, and so do the rules (a fuzz finding: rule 6 read the
/// root of `\u{feff}k: v` as indented by one). White space before a mark
/// still counts (the parser keeps that indentation and reports such a
/// mark as an error).
fn col_of(t: Node<'_>) -> usize {
    let mut col = 0usize;
    let mut at = prev_token(t);
    while let Some(x) = at {
        if x.kind() == K::ByteOrderMark {
            at = prev_token(x);
            continue;
        }
        let text = x.text();
        if let Some(i) = text.rfind(['\n', '\r']) {
            let tail = text.get(i.saturating_add(1)..).unwrap_or("");
            return col.saturating_add(tail.chars().count());
        }
        col = col.saturating_add(text.chars().count());
        at = prev_token(x);
    }
    col
}

/// The first token of `n`'s subtree (`n` itself for a token).
fn first_token(n: Node<'_>) -> Node<'_> {
    let mut at = n;
    while let Some(c) = at.first_child() {
        at = c;
    }
    at
}

/// The token where a block collection's first entry starts (its own
/// properties skipped): what its column is read from.
fn coll_start(c: Node<'_>) -> Node<'_> {
    let first = c
        .children()
        .find(|x| !x.kind().is_trivia() && !matches!(x.kind(), K::Anchor | K::Tag))
        .unwrap_or(c);
    first_token(first)
}

/// The columns of `tokens`, given latest first (as the starts of nested
/// collections are, innermost first). Tokens on one line share one
/// backward scan of it (`- - - … x` nests a collection per `- `
/// on one line; a scan per collection would be quadratic).
fn cols_of(tokens: &[Node<'_>]) -> Vec<usize> {
    let mut out = vec![0usize; tokens.len()];
    let mut i = 0usize;
    while let Some(&t) = tokens.get(i) {
        // Chars from the scan position to `t`'s start, and, for each
        // token met on the way, the count when it was met.
        let mut acc = 0usize;
        let mut met = vec![(i, 0usize)];
        let mut j = i.saturating_add(1);
        let mut at = prev_token(t);
        while let Some(x) = at {
            let text = x.text();
            // (A byte order mark is no column, as in `col_of`.)
            let chars = if x.kind() == K::ByteOrderMark {
                0
            } else {
                text.chars().count()
            };
            let breaks = text.contains(['\n', '\r']);
            while tokens.get(j) == Some(&x) {
                if !breaks {
                    met.push((j, acc.saturating_add(chars)));
                    j = j.saturating_add(1);
                } else {
                    break;
                }
            }
            if let Some(k) = text.rfind(['\n', '\r']) {
                let tail = text.get(k.saturating_add(1)..).unwrap_or("");
                acc = acc.saturating_add(tail.chars().count());
                break;
            }
            acc = acc.saturating_add(chars);
            at = prev_token(x);
        }
        for (k, before) in met {
            if let Some(o) = out.get_mut(k) {
                *o = acc.saturating_sub(before);
            }
        }
        i = j.max(i.saturating_add(1));
    }
    out
}

/// The next sibling of `x` that is not a trivia token. The
/// first such element after `x`'s subtree from the comment-run index
/// (O(log)), never a walk over the comment lines between siblings: it is
/// that sibling, or lies past the parent.
fn next_sig(x: Node<'_>) -> Option<Node<'_>> {
    let parent = x.parent()?;
    let t = x.tree();
    let last = t.nav_last_leaf(x.slot())?;
    let s = t.node_at_slot(t.solid_after(last)?)?;
    (s.parent() == Some(parent)).then_some(s)
}

/// The previous sibling of `x` that is not a trivia token. The
/// last such element before `x` (from the comment-run index) lies in that
/// sibling's subtree, or is the parent.
fn prev_sig(x: Node<'_>) -> Option<Node<'_>> {
    let parent = x.parent()?;
    let t = x.tree();
    let mut s = t.node_at_slot(t.solid_before(x.slot())?)?;
    // A climb to the parent's child.
    loop {
        if s == parent {
            return None;
        }
        match s.parent() {
            Some(p) if p == parent => return Some(s),
            Some(p) => s = p,
            None => return None,
        }
    }
}

/// The next sibling of `x` that is an item (trivia skipped in O(log),
/// from the comment-run index).
fn next_item(x: Node<'_>) -> Option<Node<'_>> {
    let mut at = next_sig(x);
    while let Some(s) = at {
        if is_item(s.kind()) {
            return Some(s);
        }
        at = next_sig(s);
    }
    None
}

/// The previous sibling of `x` that is an item (trivia skipped in
/// O(log)).
fn prev_item(x: Node<'_>) -> Option<Node<'_>> {
    let mut at = prev_sig(x);
    while let Some(s) = at {
        if is_item(s.kind()) {
            return Some(s);
        }
        at = prev_sig(s);
    }
    None
}

// ============================================================ entries

/// Whether item `x` is a key of its mapping, read from its neighbours;
/// `None` when they cannot tell.
fn local_key(x: Node<'_>) -> Option<bool> {
    match prev_sig(x).map(Node::kind) {
        None | Some(K::MappingKey | K::FlowMappingOpen | K::FlowSequenceOpen | K::FlowEntry) => {
            return Some(true);
        }
        Some(K::MappingValue) => return Some(false),
        _ => {}
    }
    if next_sig(x).is_some_and(|s| s.kind() == K::MappingValue) {
        return Some(true);
    }
    match prev_sig(x) {
        Some(p) if is_item(p.kind()) => None,
        // The mapping's own properties or other tokens before a first key.
        _ => Some(true),
    }
}

/// Whether item `x` is a key of its mapping: from its neighbours, then
/// from its previous item's (an empty value right after its key). Only
/// error recovery builds shapes neither can tell; `x` is then read as a key
/// (rulebook section 5, best effort), never by counting the mapping's items
/// (that would cost O(width) per comment).
fn is_key(x: Node<'_>) -> bool {
    if let Some(k) = local_key(x) {
        return k;
    }
    prev_item(x).and_then(local_key).is_none_or(|k| !k)
}

/// The key naming the entry that item `x` of a mapping belongs to.
fn key_of(x: Node<'_>) -> Node<'_> {
    if is_key(x) {
        x
    } else {
        prev_item(x).unwrap_or(x)
    }
}

/// The entry (its key) or item of collection `c` that its child `x` (a
/// content element) belongs to; `None` for the collection's own brackets
/// and properties.
fn entry_owner<'t>(c: Node<'t>, x: Node<'t>) -> Option<Node<'t>> {
    let k = x.kind();
    if matches!(
        k,
        K::FlowSequenceOpen
            | K::FlowSequenceClose
            | K::FlowMappingOpen
            | K::FlowMappingClose
            | K::Anchor
            | K::Tag
    ) {
        return None;
    }
    if is_seq(c.kind()) {
        return match k {
            _ if is_item(k) => Some(x),
            K::SequenceEntry => next_item(x),
            _ => prev_item(x),
        };
    }
    match k {
        _ if is_item(k) => Some(key_of(x)),
        K::MappingKey => next_item(x),
        K::MappingValue => prev_item(x),
        _ => prev_item(x).map(key_of),
    }
}

/// The entry (its key) or item of collection `c` that its child `x`
/// **starts**: `x` is the `-`, the `?`, a key written without `?`, or an
/// item of a flow sequence.
fn entry_opened<'t>(c: Node<'t>, x: Node<'t>) -> Option<Node<'t>> {
    let k = x.kind();
    match c.kind() {
        K::BlockSequence => (k == K::SequenceEntry).then(|| next_item(x)).flatten(),
        K::FlowSequence => is_item(k).then_some(x),
        _ if is_map(c.kind()) => {
            if k == K::MappingKey {
                return next_item(x);
            }
            let after_q = prev_sig(x).is_some_and(|p| p.kind() == K::MappingKey);
            (is_item(k) && !after_q && is_key(x)).then_some(x)
        }
        _ => None,
    }
}

/// What the next content token `n` opens.
#[derive(Clone, Copy, Debug)]
enum Opens {
    /// An entry (its key) or item.
    Entry,
    /// A node (a value, or a collection with properties).
    Node,
    /// The `:` of an explicit entry: the target is its value.
    ExplicitValue,
    /// Nothing: `n` is inside a node (a property after its first one, a
    /// block scalar header, error bytes); the target is that node.
    Inside,
}

/// The target a comment right before content token `n` leads (rules 3,
/// 4, 9, 10, 11, 12): the innermost entry or item `n` starts, else the
/// outermost node it starts, else (for `:` of an explicit entry) that
/// entry's value, else the innermost node holding `n`; `fallback` when
/// nothing holds it.
fn target_of<'t>(n: Node<'t>, fallback: Node<'t>) -> (Node<'t>, Opens) {
    let s = n.span().start;
    let mut x = n;
    // A loop up the ancestors that start where `n` does.
    while let Some(a) = x.parent() {
        if is_map(a.kind()) || is_seq(a.kind()) {
            if let Some(e) = entry_opened(a, x) {
                return (e, Opens::Entry);
            }
        }
        if !is_item(a.kind()) || a.span().start != s {
            break;
        }
        x = a;
    }
    if is_item(x.kind()) {
        return (x, Opens::Node);
    }
    if x.kind() == K::MappingValue {
        if let Some(v) = next_item(x) {
            return (v, Opens::ExplicitValue);
        }
    }
    let holder = x
        .ancestors()
        .find(|a| is_item(a.kind()))
        .unwrap_or(fallback);
    (holder, Opens::Inside)
}

// ======================================================= lines, blanks

/// Where a comment's line puts it.
enum Line<'t> {
    /// First on its line; the token ending the previous line (`None` at
    /// the start of the text).
    Own(Option<Node<'t>>),
    /// After content: the last content token before it.
    Same(Node<'t>),
}

fn line_of(c: Node<'_>) -> Line<'_> {
    let mut at = prev_token(c);
    while let Some(t) = at {
        if ends_line(t) {
            return Line::Own(Some(t));
        }
        if blankish(t) || t.kind() == K::Comment {
            at = prev_token(t);
            continue;
        }
        return Line::Same(t);
    }
    Line::Own(None)
}

/// Blank lines above a line whose previous line ends with `end` (rule 8).
fn blank_before(end: Option<Node<'_>>) -> usize {
    let Some(mut at) = end else { return 0 };
    let mut count = 0usize;
    loop {
        if at.kind() != K::LineBreak {
            // A block scalar's text: its lines are content.
            return count;
        }
        let mut u = prev_token(at);
        while let Some(t) = u.filter(|t| blankish(*t)) {
            u = prev_token(t);
        }
        match u {
            None => return count.saturating_add(1),
            Some(t) if t.kind() == K::LineBreak => {
                count = count.saturating_add(1);
                at = t;
            }
            Some(t) if ends_line(t) => return count.saturating_add(1),
            Some(_) => return count,
        }
    }
}

/// What follows an own-line comment up to the next content token.
struct Ahead<'t> {
    /// The next content token.
    next: Option<Node<'t>>,
    /// Where the last blank line before it starts.
    last_blank: Option<usize>,
}

fn scan_ahead(c: Node<'_>) -> Ahead<'_> {
    let mut line_empty = false;
    let mut last_blank = None;
    let mut at = next_token(c);
    while let Some(t) = at {
        match t.kind() {
            K::LineBreak => {
                if line_empty {
                    last_blank = Some(t.span().start);
                }
                line_empty = true;
            }
            K::Comment => line_empty = false,
            _ if blankish(t) => {}
            _ => {
                return Ahead {
                    next: Some(t),
                    last_blank,
                };
            }
        }
        at = next_token(t);
    }
    Ahead {
        next: None,
        last_blank,
    }
}

// ============================================================ decisions

/// A document's child that comes before its content: a directive or the
/// `---` marker.
fn is_start(n: Node<'_>) -> bool {
    matches!(
        n.kind(),
        K::DocumentStartMarker | K::YamlDirective | K::TagDirective | K::ReservedDirective
    )
}

/// The document begins with a directive or `---` (rule 1 applies before
/// it). O(1): a document never begins with trivia.
fn has_start(doc: Node<'_>) -> bool {
    doc.first_child().is_some_and(is_start)
}

/// Rule 5: a comment after content token `p` on its line.
fn same_line<'t>(p: At<'t>, around: &Around<'t>) -> Attachment<'t> {
    let done = |owner, document| Attachment {
        owner,
        rule: AttachRule::Trailing,
        blank_lines_before: 0,
        document,
    };
    let mut x = p.tok;
    // A loop up the ancestors, to the innermost entry or item. The
    // loop ends at the stream, the only element without a parent.
    loop {
        let Some(a) = x.parent() else {
            // `x` is the stream: an input without documents.
            return done(CommentOwner::DocumentHeader(x), None);
        };
        let k = a.kind();
        if is_map(k) || is_seq(k) {
            if let Some(o) = entry_owner(a, x) {
                return done(CommentOwner::Trailing(o), p.doc);
            }
        } else if k == K::Document {
            // `x` is a child of the document: O(1) by its kind.
            return if is_start(x) {
                done(CommentOwner::DocumentHeader(a), Some(a))
            } else if is_item(x.kind()) {
                done(CommentOwner::Trailing(x), Some(a))
            } else {
                // `...`, or bytes skipped at document level (error
                // recovery, best effort).
                done(CommentOwner::DocumentFooter(a), Some(a))
            };
        } else if k == K::Stream {
            // A stray `...` or an error region between documents.
            if let Some(d) = around.next_doc(x) {
                return done(CommentOwner::DocumentHeader(d), Some(d));
            }
            return match around.prev_doc(x) {
                Some(d) => done(CommentOwner::DocumentFooter(d), Some(d)),
                None => done(CommentOwner::DocumentHeader(a), None),
            };
        }
        x = a;
    }
}

/// What an own-line comment's owner is when it does not fall to rule 6.
enum Otherwise<'t> {
    Fixed(CommentOwner<'t>, AttachRule),
    /// Rules 2/3 for this document.
    Header {
        doc: Node<'t>,
        target: Node<'t>,
        last_blank: Option<usize>,
    },
    /// Rules 4/10/11/12.
    Leading {
        target: Node<'t>,
        opens: Opens,
        col: usize,
    },
}

/// The decision context of one run of trivia (shared by its comments).
struct Plan<'t> {
    /// The block collections that close in the run, innermost first, with
    /// their columns (rule 6).
    footer: Vec<(Node<'t>, usize)>,
    /// Rule 6 needs a column right of this.
    threshold: usize,
    otherwise: Otherwise<'t>,
    /// The document of `otherwise`'s owner.
    document: Option<Node<'t>>,
    /// The document of the `footer` collections.
    footer_document: Option<Node<'t>>,
}

impl<'t> Plan<'t> {
    fn fixed(owner: CommentOwner<'t>, rule: AttachRule, document: Option<Node<'t>>) -> Self {
        Self {
            footer: Vec::new(),
            threshold: 0,
            otherwise: Otherwise::Fixed(owner, rule),
            document,
            footer_document: document,
        }
    }

    fn header(doc: Node<'t>, rule: AttachRule) -> Self {
        Self::fixed(CommentOwner::DocumentHeader(doc), rule, Some(doc))
    }
}

/// The block collections among `p`'s ancestors below the first one
/// `stop` accepts (or below the document), innermost first, with their
/// columns: what closes between `p` and what follows.
fn closing<'t>(
    p: Node<'t>,
    stop: impl Fn(Node<'t>) -> bool,
) -> (Vec<(Node<'t>, usize)>, Option<Node<'t>>) {
    let mut colls = Vec::new();
    let mut at = p.parent();
    let mut lca = None;
    while let Some(a) = at {
        if stop(a) || matches!(a.kind(), K::Document | K::Stream) {
            lca = Some(a);
            break;
        }
        if is_block(a.kind()) {
            colls.push(a);
        }
        at = a.parent();
    }
    let starts: Vec<Node<'t>> = colls.iter().map(|c| coll_start(*c)).collect();
    let cols = cols_of(&starts);
    (colls.into_iter().zip(cols).collect(), lca)
}

/// Rules 2/3 for document `d` whose root `r` starts with content token
/// `n`.
fn header_plan<'t>(d: Node<'t>, r: Node<'t>, n: Node<'t>, ahead: &Ahead<'t>) -> Plan<'t> {
    Plan {
        footer: Vec::new(),
        threshold: 0,
        otherwise: Otherwise::Header {
            doc: d,
            target: target_of(n, r).0,
            last_blank: ahead.last_blank,
        },
        document: Some(d),
        footer_document: Some(d),
    }
}

/// The context of the run between content token `p` and `ahead.next`.
fn make_plan<'t>(
    tree: &'t SyntaxTree,
    p: Option<At<'t>>,
    ahead: &Ahead<'t>,
    around: &Around<'t>,
) -> Plan<'t> {
    let n = ahead.next.map(|n| match p {
        // In P's document: its child is P's, or found by a climb through
        // the nodes `n` opens (amortised O(1) per node over a pass).
        Some(At {
            doc: Some(d),
            child,
            ..
        }) if contains(d, n) => {
            let child = match child {
                Some(c) if contains(c, n) => Some(c),
                _ => {
                    let mut below = n;
                    while let Some(a) = below.parent().filter(|a| *a != d) {
                        below = a;
                    }
                    Some(below)
                }
            };
            At {
                tok: n,
                doc: Some(d),
                child,
            }
        }
        _ => locate(n),
    });
    match (p, n) {
        (
            Some(At {
                tok: pt,
                doc: Some(d),
                child: Some(xp),
            }),
            Some(At {
                tok: nt,
                doc: Some(e),
                child: Some(xn),
            }),
        ) if d == e => inside(d, pt, xp, nt, xn, ahead),
        _ => between(tree, p, n, ahead, around),
    }
}

/// A run inside document `d`, between content token `p` (in `d`'s child
/// `xp`) and `n` (in `xn`).
fn inside<'t>(
    d: Node<'t>,
    p: Node<'t>,
    xp: Node<'t>,
    n: Node<'t>,
    xn: Node<'t>,
    ahead: &Ahead<'t>,
) -> Plan<'t> {
    // Rule 1: before `---` (or a directive before it).
    if is_start(xn) {
        return Plan::header(d, AttachRule::HeaderBeforeStart);
    }
    // After `---` or a directive, before any content: rules 2/3.
    if is_start(xp) {
        return if is_item(xn.kind()) && !empty(xn) {
            header_plan(d, xn, n, ahead)
        } else {
            Plan::header(d, AttachRule::HeaderBlankLine)
        };
    }
    if xp != xn || !is_item(xn.kind()) {
        // After the root, before `...` (or an error region after it):
        // rules 6/7.
        let (footer, _) = closing(p, |_| false);
        let threshold = footer.last().map_or(0, |(_, c)| *c);
        return Plan {
            footer,
            threshold,
            otherwise: Otherwise::Fixed(
                CommentOwner::DocumentFooter(d),
                AttachRule::DocumentFooter,
            ),
            document: Some(d),
            footer_document: Some(d),
        };
    }
    // Inside the root `xn`: rules 4, 6, 9, 10, 11, 12.
    let r = xn;
    let (footer, lca) = closing(p, |a| contains(a, n));
    let flow = lca
        .into_iter()
        .flat_map(|l| core::iter::once(l).chain(l.ancestors()))
        .take_while(|a| !matches!(a.kind(), K::Document | K::Stream))
        .find(|a| is_map(a.kind()) || is_seq(a.kind()))
        .is_some_and(|c| is_flow(c.kind()));
    if flow {
        return Plan::fixed(flow_owner(n, r), AttachRule::Flow, Some(d));
    }
    let col = col_of(n);
    let (target, opens) = target_of(n, r);
    Plan {
        footer,
        threshold: col,
        otherwise: Otherwise::Leading { target, opens, col },
        document: Some(d),
        footer_document: Some(d),
    }
}

/// Rule 9: the item after the comment (commas skipped) or the footer of
/// the flow collection whose closing bracket follows.
fn flow_owner<'t>(n: Node<'t>, root: Node<'t>) -> CommentOwner<'t> {
    let mut t = n;
    while t.kind() == K::FlowEntry {
        match next_visible(t) {
            Some(x) => t = x,
            None => break,
        }
    }
    if matches!(t.kind(), K::FlowSequenceClose | K::FlowMappingClose) {
        if let Some(c) = t.parent() {
            return CommentOwner::Footer(c);
        }
    }
    CommentOwner::Leading(target_of(t, root).0)
}

/// A run at stream level (outside every document), between content
/// tokens `p` and `n` (either may be missing, or a stray token).
fn between<'t>(
    tree: &'t SyntaxTree,
    p: Option<At<'t>>,
    n: Option<At<'t>>,
    ahead: &Ahead<'t>,
    around: &Around<'t>,
) -> Plan<'t> {
    // The documents before and after the run.
    let dp = p.and_then(|p| p.doc.or_else(|| around.prev_doc(p.tok)));
    let dn = n.and_then(|n| n.doc.or_else(|| around.next_doc(n.tok)));
    // P is in `dp` (which then ended without `...`), after its content.
    let content = p
        .and_then(|p| p.doc.and(p.child))
        .is_some_and(|xp| !is_start(xp) && xp.kind() != K::DocumentEndMarker);
    // P is `dp`'s `---` or a directive: `dp` has no content.
    let blank = p.and_then(|p| p.doc.and(p.child)).is_some_and(is_start);
    let dn_start = dn.is_some_and(has_start);
    let mut plan = match (dn, dp) {
        // Rule 1: a comment before a `---` is always its header.
        (Some(e), _) if dn_start => Plan::header(e, AttachRule::HeaderBeforeStart),
        (Some(e), _) => match n {
            Some(At {
                tok,
                doc: Some(_),
                child: Some(xn),
            }) if is_item(xn.kind()) && !empty(xn) => header_plan(e, xn, tok, ahead),
            _ => Plan::header(e, AttachRule::HeaderBlankLine),
        },
        // After a document with no content that ended without `...`: still
        // its header (rule 2).
        (None, Some(i)) if blank => Plan::header(i, AttachRule::HeaderBlankLine),
        (None, Some(i)) => Plan::fixed(
            CommentOwner::DocumentFooter(i),
            AttachRule::DocumentFooter,
            Some(i),
        ),
        (None, None) => Plan::fixed(
            CommentOwner::DocumentHeader(tree.root()),
            AttachRule::HeaderBlankLine,
            None,
        ),
    };
    // Rule 6 past an implicitly ended document's end, unless a `---`
    // follows (rule 1 wins).
    if content && !dn_start {
        if let Some(p) = p {
            let (footer, _) = closing(p.tok, |_| false);
            plan.threshold = footer.last().map_or(0, |(_, c)| *c);
            plan.footer = footer;
            plan.footer_document = dp;
        }
    }
    plan
}

/// The deepest owner a run has reached so far (rule 13): its rank and the
/// attachment that set it.
#[derive(Clone, Copy)]
struct Floor<'t> {
    rank: usize,
    attachment: Attachment<'t>,
}

/// An own-line comment `c` at column `col` with `blank` blank lines above
/// it, in the run `plan` describes, after the run's earlier comments left
/// `floor` (rule 13).
///
/// Ranks, deepest first: the closing collections innermost first (rule
/// 6), then everything past them (`plan.footer.len()`: the next node's
/// leading comment, the document footer or header, a flow owner). A
/// comment whose own rules give a lower rank than `floor` takes the owner
/// of the comment before it, so each owner's comments are contiguous in
/// the run.
fn decide<'t>(
    plan: &Plan<'t>,
    c: Node<'t>,
    col: usize,
    blank: usize,
    floor: &mut Option<Floor<'t>>,
) -> Attachment<'t> {
    let (a, rank) = decide_one(plan, c, col, blank);
    if let Some(f) = *floor {
        if rank < f.rank {
            return Attachment {
                owner: f.attachment.owner,
                rule: AttachRule::RunOrder,
                blank_lines_before: blank,
                document: f.attachment.document,
            };
        }
    }
    *floor = Some(Floor {
        rank,
        attachment: a,
    });
    a
}

/// Rules 2–4, 6, 7, 9–12 for one own-line comment, with the owner's rank
/// (see [`decide`]).
fn decide_one<'t>(
    plan: &Plan<'t>,
    c: Node<'t>,
    col: usize,
    blank: usize,
) -> (Attachment<'t>, usize) {
    let done = |owner, rule, document| Attachment {
        owner,
        rule,
        blank_lines_before: blank,
        document,
    };
    let outer = plan.footer.len();
    let i = own_rank(plan, col);
    if i < outer {
        if let Some((k, _)) = plan.footer.get(i) {
            let a = done(
                CommentOwner::Footer(*k),
                AttachRule::CollectionFooter,
                plan.footer_document,
            );
            return (a, i);
        }
    }
    let a = match plan.otherwise {
        Otherwise::Fixed(owner, rule) => done(owner, rule, plan.document),
        Otherwise::Header {
            doc,
            target,
            last_blank,
        } => {
            if last_blank.is_some_and(|b| b > c.span().start) {
                done(
                    CommentOwner::DocumentHeader(doc),
                    AttachRule::HeaderBlankLine,
                    Some(doc),
                )
            } else {
                done(
                    CommentOwner::Leading(target),
                    AttachRule::FirstNodeLeading,
                    Some(doc),
                )
            }
        }
        Otherwise::Leading {
            target,
            opens,
            col: n,
        } => {
            let rule = if col < n {
                AttachRule::ShallowLeading
            } else {
                match opens {
                    Opens::Entry | Opens::Node => AttachRule::Leading,
                    Opens::ExplicitValue => AttachRule::ExplicitValue,
                    Opens::Inside => AttachRule::Inside,
                }
            };
            done(CommentOwner::Leading(target), rule, plan.document)
        }
    };
    (a, outer)
}

/// The rank rules 2–4, 6, 7, 9–12 give an own-line comment at column `col` in
/// the run `plan` describes (see [`decide`]): the closing collection it is
/// right of, innermost first, else past them all (`plan.footer.len()`).
/// Non-increasing in `col` (the comment-run index relies on it: rule 13's floor
/// is the rank of the run's smallest column).
fn own_rank(plan: &Plan<'_>, col: usize) -> usize {
    let outer = plan.footer.len();
    if col > plan.threshold && outer > 0 {
        plan.footer
            .partition_point(|(_, kc)| *kc > col)
            .min(outer.saturating_sub(1))
    } else {
        outer
    }
}

/// The smallest column whose rank is below `k` (`k >= 1`): a comment
/// ranks at least `k` exactly when its column is below it. `None` when it
/// does not fit the index's `u32` columns.
fn cut_below(plan: &Plan<'_>, k: usize) -> Option<u32> {
    // `big` ranks 0: right of the threshold and of every footer column.
    let big = plan
        .footer
        .iter()
        .map(|(_, c)| *c)
        .chain(core::iter::once(plan.threshold))
        .max()
        .unwrap_or(0)
        .checked_add(1)?;
    let (mut lo, mut hi) = (0usize, big);
    // A binary search for the first column ranked below `k`.
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if own_rank(plan, mid) < k {
            hi = mid;
        } else {
            lo = mid.saturating_add(1);
        }
    }
    u32::try_from(lo).ok()
}

/// The plan's columns fit the index's `u32` columns (with room for the
/// cut past them).
fn cols_fit(plan: &Plan<'_>) -> bool {
    let limit = col_limit();
    let fits = |c: usize| c < limit;
    fits(plan.threshold) && plan.footer.iter().all(|(_, c)| fits(*c))
}

/// The columns the index decides: below `u32::MAX / 2`.
#[cfg(not(test))]
const fn col_limit() -> usize {
    (u32::MAX / 2) as usize
}

/// The columns the index decides, lowered by tests: the
/// fallbacks for wider columns need lines of 2 GiB
/// otherwise).
#[cfg(test)]
fn col_limit() -> usize {
    tests::COL_LIMIT.with(core::cell::Cell::get)
}

/// What [`SyntaxTree::decide_indexed`] is asked for.
#[derive(Clone, Copy)]
enum Want {
    Attachment,
    Document,
}

/// What [`SyntaxTree::decide_indexed`] decided.
enum Decided<'t> {
    Attachment(Attachment<'t>),
    Document(Option<Node<'t>>),
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use core::cell::Cell;

    std::thread_local! {
        /// [`col_limit`] on this thread.
        pub(super) static COL_LIMIT: Cell<usize> = const { Cell::new((u32::MAX / 2) as usize) };
    }

    /// Long runs of comment lines below nested collections at every
    /// column, stray `...`, a document after them; and the same trees
    /// after edits that cut the runs.
    fn trees() -> Vec<SyntaxTree> {
        let mut out = Vec::new();
        for depth in 1..5usize {
            let mut s = String::new();
            for i in 0..depth {
                s.push_str(&format!("{}k{i}:\n", " ".repeat(2 * i)));
            }
            s.push_str(&format!("{}leaf: 1\n", " ".repeat(2 * depth)));
            for i in 0..90 {
                // Deep first, shallower later (footers, then outer owners).
                s.push_str(&" ".repeat((2 * depth + 1).saturating_sub((i * 7) % 5 + i / 30)));
                s.push_str(&format!("# c{i}\n"));
                if i % 17 == 4 {
                    s.push('\n');
                }
            }
            if depth % 2 == 0 {
                s.push_str("z: 1\n");
            } else {
                s.push_str("...\n# s\n...\n# h\n--- y\n");
            }
            let mut t = SyntaxTree::parse(s.as_str());
            out.push(t.clone());
            let comments: Vec<_> = t
                .root()
                .descendants()
                .filter(|n| n.kind() == K::Comment)
                .map(Node::id)
                .collect();
            for (k, c) in comments.iter().enumerate().filter(|(k, _)| k % 5 == 1) {
                let _ = t.replace_token(*c, &format!("# r{k}"));
            }
            out.push(t);
        }
        out
    }

    /// With the index's column range lowered
    /// (to none, and to a few columns), the fallbacks for columns beyond
    /// it — the run replay of [`SyntaxTree::comment_attachment`] and
    /// [`SyntaxTree::comment_document`], and
    /// [`SyntaxTree::last_footer_comment_walk`] — decide as the pass and
    /// as the index do.
    #[test]
    fn wide_column_fallbacks_decide_as_the_index() {
        let mut with_footers = 0;
        for t in trees() {
            let all = t.comment_attachments();
            let blocks: Vec<_> = t
                .root()
                .descendants()
                .filter(|x| is_block(x.kind()))
                .collect();
            let footers: Vec<_> = blocks.iter().map(|b| t.last_footer_comment(*b)).collect();
            with_footers += usize::from(footers.iter().any(Option::is_some));
            for limit in [0usize, 1, 2, 3, 5, 8] {
                COL_LIMIT.with(|c| c.set(limit));
                for (c, a) in &all {
                    assert_eq!(t.comment_attachment(*c).as_ref(), Some(a), "{limit}");
                    assert_eq!(t.comment_document(*c), Some(a.document()), "{limit}");
                }
                for (b, want) in blocks.iter().zip(&footers) {
                    assert_eq!(t.last_footer_comment(*b), *want, "{limit}");
                }
            }
            COL_LIMIT.with(|c| c.set((u32::MAX / 2) as usize));
        }
        assert!(with_footers >= 4, "{with_footers}");
    }
}
