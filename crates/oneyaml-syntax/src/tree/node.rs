//! Borrowed views of tree elements and iterative traversals (every
//! traversal is an index loop over the flat arena; nothing recurses).

use std::borrow::Cow;

use oneyaml_core::{Diagnostic, Span};

use super::{NodeId, SyntaxKind, SyntaxTree};
use crate::event::{Anchor, BlockHeader, ScalarStyle, Tag, TagSource};

/// A node or token of a [`SyntaxTree`]: a cheap, copyable view.
///
/// Navigation is O(1) per step, except [`Node::last_child`] and
/// [`Node::prev_sibling`], which are O(depth) (the arena links forward).
/// In an edited tree too, as long as the step stays inside one of the
/// edits' runs, so sequential reads cost amortised O(1) per
/// element (see [`SyntaxTree::is_edited`]).
#[derive(Clone, Copy)]
pub struct Node<'t> {
    tree: &'t SyntaxTree,
    /// The element's slot (low 32 bits) and, its **hint**
    /// (high 32 bits): in an edited tree, the treap item of the run holding
    /// the element when known (`NONE` otherwise, and always for an
    /// unedited tree). The tree is borrowed immutably, so the overlay
    /// cannot change while the hint exists. The hint is not part of the
    /// element's identity (`PartialEq`, `Hash`). One `u64`, so a `Node`
    /// is two words that stay in registers (two `u32` fields made the
    /// compiler build it in memory field by field and read it back whole,
    /// a store-forwarding stall on every step of a parsed tree's loops).
    key: u64,
}

impl<'t> Node<'t> {
    pub(super) const fn new(tree: &'t SyntaxTree, index: u32) -> Self {
        Self::hinted(tree, index, super::NONE)
    }

    /// An element of an edited tree with its run's item.
    pub(super) const fn hinted(tree: &'t SyntaxTree, index: u32, hint: u32) -> Self {
        Self {
            tree,
            key: (index as u64) | ((hint as u64) << 32),
        }
    }

    /// The element's slot.
    #[inline]
    const fn idx(self) -> u32 {
        self.key as u32
    }

    /// The element's hint.
    #[inline]
    pub(super) const fn hint(self) -> u32 {
        (self.key >> 32) as u32
    }

    fn at(self, index: usize) -> Self {
        Self::new(self.tree, u32::try_from(index).unwrap_or(super::NONE))
    }

    fn i(self) -> usize {
        self.idx() as usize
    }

    /// The element's slot: a 4-byte name of the element for the tree's
    /// whole life (edits never reuse a slot), resolved with
    /// [`SyntaxTree::node_at_slot`]. Meant for compact indexes
    /// over one tree; unlike a [`NodeId`](super::NodeId) it does not name
    /// its tree, so it is meaningful only with the tree that issued it (and
    /// a [`fork`](SyntaxTree::fork) of it, for slots issued before the
    /// fork). O(1).
    #[must_use]
    pub const fn slot(self) -> u32 {
        self.idx()
    }

    /// The tree has been edited: read through the overlay.
    fn edited(self) -> bool {
        self.tree.edits.is_some()
    }

    fn with_slot(self, slot: Option<u32>) -> Option<Self> {
        slot.map(|s| Self::new(self.tree, s))
    }

    /// The element of a hinted step.
    fn with_hinted(self, step: Option<(u32, u32)>) -> Option<Self> {
        step.map(|(s, h)| Self::hinted(self.tree, s, h))
    }

    /// [`Node::parent`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn parent_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_parent_hinted(self.idx(), self.hint()))
    }

    /// [`Node::first_child`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn first_child_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_first_hinted(self.idx(), self.hint()))
    }

    /// [`Node::last_child`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn last_child_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_last_hinted(self.idx(), self.hint()))
    }

    /// [`Node::next_sibling`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn next_sibling_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_next_hinted(self.idx(), self.hint()))
    }

    /// [`Node::prev_sibling`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn prev_sibling_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_prev_hinted(self.idx(), self.hint()))
    }

    /// [`Node::pred`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn pred_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_pred_hinted(self.idx(), self.hint()))
    }

    /// [`Node::succ`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn succ_edited(self) -> Option<Self> {
        self.with_hinted(self.tree.nav_succ_hinted(self.idx(), self.hint()))
    }

    /// A stable handle to this element.
    #[must_use]
    pub fn id(self) -> NodeId {
        self.tree.id_of(self.idx())
    }

    /// The element's position in preorder (= source order), 0 for the root.
    /// Positions compare elements of one tree: an element comes before
    /// everything in its subtree and after its preceding siblings. O(1);
    /// O(log R) in an edited tree (R: runs, see [`SyntaxTree::is_edited`]).
    #[must_use]
    #[inline]
    pub fn preorder_index(self) -> usize {
        if self.edited() {
            return self.rank_edited();
        }
        self.idx() as usize
    }

    /// [`Node::preorder_index`] in an edited tree (out of line).
    #[inline(never)]
    fn rank_edited(self) -> usize {
        self.tree
            .nav_rank_hinted(self.idx(), self.hint())
            .and_then(|(r, _)| usize::try_from(r).ok())
            .unwrap_or(usize::MAX)
    }

    /// The tree this element belongs to.
    #[must_use]
    pub const fn tree(self) -> &'t SyntaxTree {
        self.tree
    }

    /// What the element is.
    #[must_use]
    #[inline]
    pub fn kind(self) -> SyntaxKind {
        // A base slot's kind is the base arrays', edited or not
        // (edits never change an arena); only an inserted slot reads its
        // arena through the overlay.
        match self.tree.kind_at(self.i()) {
            Some(k) => k,
            None => self.kind_edited(),
        }
    }

    /// [`Node::kind`] in an edited tree (out of line, so the unedited
    /// read stays small enough to inline).
    #[inline(never)]
    fn kind_edited(self) -> SyntaxKind {
        self.tree
            .kind_hinted(self.idx(), self.hint())
            .unwrap_or(SyntaxKind::Stream)
    }

    /// `true` for a leaf token.
    #[must_use]
    #[inline]
    pub fn is_token(self) -> bool {
        self.kind().is_token()
    }

    /// The source bytes the element covers (UTF-8 byte range); in an
    /// edited tree, the range in [`SyntaxTree::emit`]'s text. O(1) (a run
    /// of nodes opening at one byte aside); edited: O(log R) when the
    /// subtree is untouched, else O(depth).
    #[must_use]
    pub fn span(self) -> Span {
        if self.edited() {
            return self.span_edited();
        }
        self.tree.span_of(self.i())
    }

    /// The source text the element covers; in an edited tree, its text in
    /// [`SyntaxTree::emit`]. Borrowed from the tree (always for a parsed
    /// tree and for tokens); a node whose text an edit split over several
    /// arenas is built from them on each call (O(node size)): the tree
    /// keeps no copies of its text.
    #[must_use]
    pub fn text(self) -> Cow<'t, str> {
        // A base token's text is its source bytes, edited or not.
        if self.edited()
            && !self
                .tree
                .kind_at(self.i())
                .is_some_and(SyntaxKind::is_token)
        {
            return self.text_edited();
        }
        let span = self.tree.span_of(self.i());
        Cow::Borrowed(self.tree.source.get(span.start..span.end).unwrap_or(""))
    }

    /// [`Node::span`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn span_edited(self) -> Span {
        self.tree.nav_span_hinted(self.idx(), self.hint())
    }

    /// [`Node::text`] in an edited tree (out of line, see [`Node::kind`]).
    #[inline(never)]
    fn text_edited(self) -> Cow<'t, str> {
        self.tree.edited_text_hinted(self.idx(), self.hint())
    }

    /// The enclosing node; `None` for the root.
    #[must_use]
    pub fn parent(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.parent_edited();
        }
        self.tree.parent_of(self.i()).map(|p| self.at(p))
    }

    /// The first child; `None` for a token or an empty node.
    #[must_use]
    pub fn first_child(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.first_child_edited();
        }
        let next = self.i().saturating_add(1);
        (next < self.tree.end_of(self.i())).then(|| self.at(next))
    }

    /// The last child. O(depth).
    #[must_use]
    pub fn last_child(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.last_child_edited();
        }
        let end = self.tree.end_of(self.i());
        if end <= self.i().saturating_add(1) {
            return None;
        }
        // The last element of the subtree, then up to our child.
        let mut j = end.saturating_sub(1);
        while let Some(p) = self.tree.parent_of(j) {
            if p == self.i() {
                return Some(self.at(j));
            }
            j = p;
        }
        None
    }

    /// The next sibling.
    #[must_use]
    pub fn next_sibling(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.next_sibling_edited();
        }
        let parent = self.tree.parent_of(self.i())?;
        let next = self.tree.end_of(self.i());
        (next < self.tree.end_of(parent)).then(|| self.at(next))
    }

    /// The previous sibling. O(depth).
    #[must_use]
    pub fn prev_sibling(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.prev_sibling_edited();
        }
        let parent = self.tree.parent_of(self.i())?;
        let mut j = self.i().checked_sub(1)?;
        if j == parent {
            return None;
        }
        // The element before us ends the previous sibling's subtree.
        while let Some(p) = self.tree.parent_of(j) {
            if p == parent {
                return Some(self.at(j));
            }
            j = p;
        }
        None
    }

    /// The previous element in preorder (a token, or an enclosing or empty
    /// node). O(1); O(log R) in an edited tree.
    pub(super) fn pred(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.pred_edited();
        }
        self.with_slot(self.tree.nav_pred(self.idx()))
    }

    /// The next element in preorder. O(1); O(log R) in an edited tree.
    pub(super) fn succ(self) -> Option<Self> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited() {
            return self.succ_edited();
        }
        self.with_slot(self.tree.nav_succ(self.idx()))
    }

    /// The children, in order.
    #[must_use]
    pub fn children(self) -> Children<'t> {
        if self.edited() {
            return self.children_edited();
        }
        Children {
            tree: self.tree,
            edited: false,
            next: self.first_child().map(Self::i),
            hint: super::NONE,
            end: self.tree.end_of(self.i()),
        }
    }

    /// [`Node::children`] in an edited tree (out of line, see
    /// [`Node::kind`]).
    #[inline(never)]
    fn children_edited(self) -> Children<'t> {
        // (Without a hint, the first step finds one: see `Children`.)
        let first = self.first_child();
        let hint = first.map_or(super::NONE, |f| f.hint());
        Children {
            tree: self.tree,
            edited: true,
            next: first.map(Self::i),
            hint,
            end: self.tree.run_slots(hint).1 as usize,
        }
    }

    /// The enclosing nodes, innermost first.
    #[must_use]
    pub fn ancestors(self) -> Ancestors<'t> {
        Ancestors {
            next: self.parent(),
        }
    }

    /// This element and everything under it, in preorder (= source order).
    #[must_use]
    pub fn descendants(self) -> Descendants<'t> {
        if self.edited() {
            // Edited: `next` is a slot, `end` the count still to yield (the
            // subtree is that many consecutive elements of the order).
            // (Without a hint, the first step finds one, see `cross`.)
            let hint = self.hint();
            return Descendants {
                tree: self.tree,
                edited: true,
                next: self.i(),
                hint,
                run_end: self.tree.run_slots(hint).1,
                end: usize::try_from(self.tree.nav_size(self.idx())).unwrap_or(0),
            };
        }
        Descendants {
            tree: self.tree,
            edited: false,
            next: self.i(),
            hint: super::NONE,
            run_end: 0,
            end: self.tree.end_of(self.i()),
        }
    }

    /// A preorder walk of this subtree that also reports leaving each
    /// element (tokens are entered and left at once).
    #[must_use]
    pub fn walk(self) -> Walk<'t> {
        let end = if self.edited() {
            // Edited: `end` is the count still to enter.
            usize::try_from(self.tree.nav_size(self.idx())).unwrap_or(0)
        } else {
            self.tree.end_of(self.i())
        };
        // (Without a hint, the first step finds one.)
        let hint = self.hint();
        let (run_start, run_end) = self.tree.run_slots(hint);
        Walk {
            tree: self.tree,
            edited: self.edited(),
            root: self.idx(),
            next: self.i(),
            hint,
            run_start,
            run_end,
            end,
            current: NO_CURRENT,
            current_hint: super::NONE,
        }
    }

    /// The diagnostic of an [`SyntaxKind::Error`] node (in an edited tree,
    /// with its spans moved to the edited text). Past `max_diagnostics` it
    /// is a skeleton, without texts (see
    /// [`SyntaxTree::suppressed_diagnostics`](crate::tree::SyntaxTree::suppressed_diagnostics)).
    #[must_use]
    pub fn diagnostic(self) -> Option<Cow<'t, Diagnostic>> {
        if self.kind() != SyntaxKind::Error {
            return None;
        }
        self.tree.diagnostic_at(self.i())
    }

    // ------------------------------------------------------------ meaning

    /// The first child of `kind` among a YAML node's own leading children
    /// (its properties and header), before its content.
    fn prefix_token(self, kind: SyntaxKind) -> Option<Self> {
        if !self.kind().is_yaml_node() {
            return None;
        }
        self.children()
            .take_while(|c| {
                matches!(
                    c.kind(),
                    SyntaxKind::Anchor
                        | SyntaxKind::Tag
                        | SyntaxKind::BlockScalarHeader
                        | SyntaxKind::Whitespace
                        | SyntaxKind::LineBreak
                        | SyntaxKind::Comment
                        | SyntaxKind::Error
                        | SyntaxKind::Skipped
                )
            })
            .find(|c| c.kind() == kind)
    }

    /// A YAML node's anchor (§6.9.2), read as the event stream reads it.
    #[must_use]
    pub fn anchor(self) -> Option<Anchor<'t>> {
        let token = self.prefix_token(SyntaxKind::Anchor)?;
        if self.edited() {
            let loc = self.tree.locate_hinted(token.idx(), token.hint())?;
            let local = loc.flat.token_span(loc.local).start;
            let mut a = crate::parser::anchor_at(loc.flat.source, local)?;
            a.span = self.tree.to_global(token.idx(), a.span);
            return Some(a);
        }
        crate::parser::anchor_at(&self.tree.source, token.span().start)
    }

    /// A YAML node's tag, resolved as the event stream resolves it
    /// (`%TAG` prefix and `%XX` escapes, §6.9.1).
    ///
    /// Cost: O(prefix + suffix) **per call**; the resolved text copies the
    /// `%TAG` prefix, which may be long. To read many tags, cache by
    /// [`TagSource::key`] ([`Node::tag_source`]) and resolve each distinct
    /// tag once.
    #[must_use]
    pub fn tag(self) -> Option<Tag<'t>> {
        self.tag_source()?.resolve()
    }

    /// A YAML node's tag as written, not yet resolved: O(the property's
    /// text), whatever the `%TAG` prefix's length. Its
    /// span is in the edited text, as [`Node::tag`]'s.
    #[must_use]
    pub fn tag_source(self) -> Option<TagSource<'t>> {
        let (mut t, token) = self.tag_in_arena()?;
        if self.edited() {
            t.span = self.tree.to_global(token, t.span);
        }
        Some(t)
    }

    /// The resolved text of the node's tag ([`Node::tag`] without its
    /// span: the well-formedness comparison reads
    /// only this, and moving the span to the edited text cost an O(log R)
    /// rank per node).
    pub(super) fn resolved_tag(self) -> Option<Cow<'t, str>> {
        self.tag_in_arena()
            .and_then(|(t, _)| t.resolve())
            .map(|t| t.resolved)
    }

    /// The node's tag as written, its span in the arena holding the
    /// tag token (the source for an unedited tree), and that token's slot.
    /// The one reading behind [`Node::tag`] and
    /// [`Node::resolved_tag`], so the two never differ.
    fn tag_in_arena(self) -> Option<(TagSource<'t>, u32)> {
        let token = self.prefix_token(SyntaxKind::Tag)?;
        if self.edited() {
            let loc = self.tree.locate_hinted(token.idx(), token.hint())?;
            let own = self.tree.locate_hinted(self.idx(), self.hint())?;
            // A tag token moves only with its node (edit.rs): same arena.
            if !core::ptr::eq(loc.flat.source, own.flat.source) {
                return None;
            }
            let prefix = own.flat.extra(own.local).and_then(|e| e.tag_prefix);
            let local = loc.flat.token_span(loc.local).start;
            let t = crate::parser::tag_source_at(loc.flat.source, local, prefix)?;
            return Some((t, token.idx()));
        }
        let prefix = self.tree.extra(self.i()).and_then(|e| e.tag_prefix);
        let t = crate::parser::tag_source_at(&self.tree.source, token.span().start, prefix)?;
        Some((t, token.idx()))
    }

    /// A block scalar's header as the event carried it (C5).
    #[must_use]
    pub fn block_header(self) -> Option<BlockHeader> {
        if self.kind() != SyntaxKind::Scalar {
            return None;
        }
        if self.edited() {
            let mut h = self.local_block_header()?;
            let token = self.prefix_token(SyntaxKind::BlockScalarHeader)?;
            h.span = self.tree.to_global(token.idx(), h.span);
            return Some(h);
        }
        self.tree.extra(self.i()).and_then(|e| e.block)
    }

    /// An edited tree's scalar header, spans local to its arena.
    fn local_block_header(self) -> Option<BlockHeader> {
        let own = self.tree.locate_hinted(self.idx(), self.hint())?;
        own.flat.extra(own.local).and_then(|e| e.block)
    }

    /// A scalar node's value after escapes, folding, and chomping, decoded
    /// by the event stream's own decoder. Borrowed from the source
    /// when it is a slice of it.
    #[must_use]
    pub fn value(self) -> Option<Cow<'t, str>> {
        if self.kind() != SyntaxKind::Scalar {
            return None;
        }
        // A scalar's text token is always its last child.
        let text = if self.edited() {
            self.last_child()?
        } else {
            self.at(self.tree.end_of(self.i()).checked_sub(1)?)
        };
        let style = match text.kind() {
            SyntaxKind::PlainScalar => ScalarStyle::Plain,
            SyntaxKind::SingleQuotedScalar => ScalarStyle::SingleQuoted,
            SyntaxKind::DoubleQuotedScalar => ScalarStyle::DoubleQuoted,
            SyntaxKind::LiteralScalar => ScalarStyle::Literal,
            SyntaxKind::FoldedScalar => ScalarStyle::Folded,
            _ => return None,
        };
        if self.edited() {
            let loc = self.tree.locate_hinted(text.idx(), text.hint())?;
            let span = loc.flat.token_span(loc.local);
            let header = self.local_block_header();
            let Some(h) = header.filter(|h| h.chomping == crate::event::Chomping::Keep) else {
                return crate::parser::scalar_value(loc.flat.source, span, style, header);
            };
            // A keep scalar's value depends on the text
            // after it (an unterminated last line of spaces at the end of
            // the text is one more empty line). The token's arena ends
            // where its fragment did, so that is read from the tree's own
            // text, never from the arena's (at most `content_indent`
            // bytes).
            let own = loc.flat.source.get(span.start..span.end)?;
            let mut v = crate::parser::scalar_value(own, Span::new(0, own.len()), style, header)?
                .into_owned();
            let end = u64::try_from(text.span().end).ok()?;
            let total = u64::try_from(self.tree.root().span().end).ok()?;
            let rest = total.saturating_sub(end);
            let n = u64::try_from(h.content_indent).unwrap_or(u64::MAX);
            let spaces =
                rest > 0 && rest <= n && (end..total).all(|i| self.tree.byte_at(i) == Some(b' '));
            if spaces {
                v.push('\n');
            }
            return Some(Cow::Owned(v));
        }
        // Only a block scalar has a header (read from the extras by binary
        // search: skipped for flow scalars).
        let header = if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) {
            self.block_header()
        } else {
            None
        };
        crate::parser::scalar_value(&self.tree.source, text.span(), style, header)
    }

    /// An alias node's anchor name, without `*`.
    #[must_use]
    pub fn alias_name(self) -> Option<&'t str> {
        if self.kind() != SyntaxKind::Alias {
            return None;
        }
        let token = self.first_child()?;
        if self.edited() {
            // A token lies in one arena: its text is always borrowed.
            let loc = self.tree.locate_hinted(token.idx(), token.hint())?;
            return loc.flat.token_text(loc.local).strip_prefix('*');
        }
        self.tree
            .source
            .get(token.span().start..token.span().end)?
            .strip_prefix('*')
    }
}

impl PartialEq for Node<'_> {
    /// The same element of the same tree instance.
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.tree, other.tree) && self.idx() == other.idx()
    }
}

impl Eq for Node<'_> {}

impl core::hash::Hash for Node<'_> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        core::ptr::hash(self.tree, state);
        self.idx().hash(state);
    }
}

impl core::fmt::Debug for Node<'_> {
    /// `Kind@start..end` (not the subtree: see [`SyntaxTree`]'s `Debug`).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let span = self.span();
        write!(f, "{:?}@{}..{}", self.kind(), span.start, span.end)
    }
}

/// The children of a node ([`Node::children`]).
#[derive(Clone)]
pub struct Children<'t> {
    tree: &'t SyntaxTree,
    next: Option<usize>,
    /// The hint of `next` (edited trees).
    hint: u32,
    /// One past the last index of the parent's subtree; edited: one past
    /// the last slot of `next`'s run (without a hint: 0 before the first
    /// step, 1 after it).
    end: usize,
    /// The tree was edited when the iterator was made (read once, so the
    /// unedited loop stays as tight as before the overlay existed).
    edited: bool,
}

impl<'t> Iterator for Children<'t> {
    type Item = Node<'t>;

    #[inline]
    fn next(&mut self) -> Option<Node<'t>> {
        super::cost::count(super::cost::Step::Nav);
        let i = self.next?;
        if self.edited {
            return self.next_edited(i);
        }
        let after = self.tree.end_of(i);
        self.next = (after < self.end && after > i).then_some(after);
        Some(Node::new(self.tree, u32::try_from(i).ok()?))
    }
}

impl<'t> Children<'t> {
    /// An edited tree's step (kept out of line so the unedited loop stays
    /// small enough to inline).
    #[inline(never)]
    fn next_edited(&mut self, i: usize) -> Option<Node<'t>> {
        let tree = self.tree;
        let slot = u32::try_from(i).ok()?;
        if self.hint == super::NONE {
            if self.end == 0 {
                // The first step without a hint is the unhinted step (many
                // iterations stop after one child); `end` = 1 marks it.
                self.next = tree.nav_next(slot).map(|s| s as usize);
                self.end = 1;
                return Some(Node::new(tree, slot));
            }
            // Going on: one run lookup (what every step cost before
            // the hints), then the steps inside the run are the arrays'
            // arithmetic.
            self.hint = tree.hint_of(slot);
            self.end = tree.run_slots(self.hint).1 as usize;
        }
        let hint = self.hint;
        // A base element whose subtree and the element after it
        // lie in its run steps by the base arrays, as in an unedited tree
        // (see `SyntaxTree::nav_next_hinted`).
        if hint != super::NONE && i < tree.kinds.len() {
            let after = tree.end_of(i);
            if after < self.end {
                let limit = tree
                    .parent_of(i)
                    .map_or(tree.kinds.len(), |p| tree.end_of(p));
                self.next = (after < limit && after > i).then_some(after);
                return Some(Node::hinted(tree, slot, hint));
            }
        }
        let next = tree.nav_next_hinted(slot, hint);
        self.next = next.map(|(s, _)| s as usize);
        let h = next.map_or(super::NONE, |(_, h)| h);
        if h != self.hint {
            self.hint = h;
            self.end = tree.run_slots(h).1 as usize;
        }
        Some(Node::hinted(tree, slot, hint))
    }
}

impl core::iter::FusedIterator for Children<'_> {}

// The iterators' `Debug` shows only their index range, never the tree
// (whose own `Debug` prints every element).
impl core::fmt::Debug for Children<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Children")
            .field("next", &self.next)
            .field("end", &self.end)
            .finish()
    }
}

/// The ancestors of an element ([`Node::ancestors`]).
#[derive(Clone, Debug)]
pub struct Ancestors<'t> {
    next: Option<Node<'t>>,
}

impl<'t> Iterator for Ancestors<'t> {
    type Item = Node<'t>;

    fn next(&mut self) -> Option<Node<'t>> {
        let node = self.next?;
        self.next = node.parent();
        Some(node)
    }
}

impl core::iter::FusedIterator for Ancestors<'_> {}

/// A subtree in preorder ([`Node::descendants`]).
#[derive(Clone)]
pub struct Descendants<'t> {
    tree: &'t SyntaxTree,
    next: usize,
    /// The hint of `next` and one past the last slot of its run
    /// (edited trees).
    hint: u32,
    run_end: u32,
    end: usize,
    /// See [`Children`].
    edited: bool,
}

impl<'t> Iterator for Descendants<'t> {
    type Item = Node<'t>;

    #[inline]
    fn next(&mut self) -> Option<Node<'t>> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited {
            // Inside a run, the same increment as unedited.
            let i = self.next;
            let n = i.saturating_add(1);
            if self.end > 1 && n < self.run_end as usize {
                self.end = self.end.saturating_sub(1);
                self.next = n;
                return Some(Node::hinted(self.tree, u32::try_from(i).ok()?, self.hint));
            }
            return self.next_edited();
        }
        if self.next >= self.end {
            return None;
        }
        let i = self.next;
        self.next = i.saturating_add(1);
        Some(Node::new(self.tree, u32::try_from(i).ok()?))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.edited {
            return (self.end, Some(self.end));
        }
        let n = self.end.saturating_sub(self.next);
        (n, Some(n))
    }
}

impl<'t> Descendants<'t> {
    /// An edited tree's step: `next` is a slot, `end` the count still to
    /// yield (out of line, see [`Children`]).
    #[inline(never)]
    fn next_edited(&mut self) -> Option<Node<'t>> {
        if self.end == 0 {
            return None;
        }
        self.end = self.end.saturating_sub(1);
        let slot = u32::try_from(self.next).ok()?;
        let hint = self.hint;
        // An increment inside a run, a treap step to the next
        // run; the element after the last one is not looked for.
        if self.end > 0 {
            match slot.checked_add(1).filter(|n| *n < self.run_end) {
                Some(n) => self.next = n as usize,
                None => self.cross(slot),
            }
        }
        Some(Node::hinted(self.tree, slot, hint))
    }

    /// Moves to the run after the current one (`slot` its last element).
    fn cross(&mut self, slot: u32) {
        let next = if self.hint == super::NONE {
            // (A hint is always set; a lookup keeps a broken one exact.)
            self.tree
                .nav_succ_hinted(slot, super::NONE)
                .map(|(s, h)| (h, s, self.tree.run_slots(h).1))
        } else {
            self.tree.run_after(self.hint)
        };
        (self.next, self.hint, self.run_end) = match next {
            Some((h, s, t)) => (s as usize, h, t),
            None => (usize::MAX, super::NONE, 0),
        };
    }
}

impl ExactSizeIterator for Descendants<'_> {}
impl core::iter::FusedIterator for Descendants<'_> {}

impl core::fmt::Debug for Descendants<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Descendants")
            .field("next", &self.next)
            .field("end", &self.end)
            .finish()
    }
}

/// A step of a [`Walk`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WalkEvent<T> {
    /// Entering an element (before its children).
    Enter(T),
    /// Leaving an element (after its children).
    Leave(T),
}

/// A preorder walk with enter and leave steps ([`Node::walk`]). Holds no
/// stack: the element to leave next is found through parent links.
#[derive(Clone)]
pub struct Walk<'t> {
    tree: &'t SyntaxTree,
    next: usize,
    end: usize,
    /// The innermost element entered and not yet left (`NO_CURRENT`: none).
    current: usize,
    root: u32,
    /// The hint of `next` and the slots `run_start..run_end` of
    /// its run, and the hint of `current` (edited trees). With `current`
    /// and `root` narrowed, the walk is as large as before them (56 bytes:
    /// a walk is moved around with its readers, as measured).
    hint: u32,
    run_start: u32,
    run_end: u32,
    current_hint: u32,
    /// See [`Children`].
    edited: bool,
}

/// `Walk::current` when no element is entered and not yet left.
const NO_CURRENT: usize = usize::MAX;

impl<'t> Iterator for Walk<'t> {
    type Item = WalkEvent<Node<'t>>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        super::cost::count(super::cost::Step::Nav);
        if self.edited {
            return self.next_edited();
        }
        let c = self.current;
        if c != NO_CURRENT && self.tree.end_of(c) <= self.next {
            self.current = if c == self.root as usize {
                NO_CURRENT
            } else {
                self.tree.parent_of(c).unwrap_or(NO_CURRENT)
            };
            return Some(WalkEvent::Leave(Node::new(
                self.tree,
                u32::try_from(c).ok()?,
            )));
        }
        if self.next >= self.end {
            return None;
        }
        let i = self.next;
        self.next = i.saturating_add(1);
        self.current = i;
        Some(WalkEvent::Enter(Node::new(
            self.tree,
            u32::try_from(i).ok()?,
        )))
    }
}

impl<'t> Walk<'t> {
    /// An edited tree's walk: `next` is the slot to enter, `end` the count
    /// still to enter; an element is left once the next one to enter is
    /// not its child. Inside one base run, entering a child and
    /// leaving an element are the base arrays' steps, as unedited.
    #[inline(never)]
    fn next_edited(&mut self) -> Option<WalkEvent<Node<'t>>> {
        let tree = self.tree;
        let (n, h, c) = (self.next, self.hint, self.current);
        let base = tree.kinds.len();
        if c != NO_CURRENT && h != super::NONE && n < base {
            // `next` is a base element of `next`'s run: its parent is the
            // base arrays'.
            let parent = tree.parent_of(n);
            if parent == Some(c) && self.end > 1 && n.saturating_add(1) < self.run_end as usize {
                // Entering a child of `current`, the run going on.
                self.end = self.end.saturating_sub(1);
                self.next = n.saturating_add(1);
                self.current = n;
                self.current_hint = h;
                return Some(WalkEvent::Enter(Node::hinted(
                    tree,
                    u32::try_from(n).ok()?,
                    h,
                )));
            }
            if parent != Some(c)
                && self.end > 0
                && self.current_hint == h
                && c != self.root as usize
            {
                // Leaving a base `current` of the same run.
                let up = tree.parent_of(c);
                self.current = up.unwrap_or(NO_CURRENT);
                self.current_hint = match up {
                    Some(p) if p >= self.run_start as usize => h,
                    _ => super::NONE,
                };
                return Some(WalkEvent::Leave(Node::hinted(
                    tree,
                    u32::try_from(c).ok()?,
                    h,
                )));
            }
        }
        if c != NO_CURRENT {
            let c32 = u32::try_from(c).ok()?;
            let ch = self.current_hint;
            // Parents from the base arrays, or through hints.
            let leave = self.end == 0
                || u32::try_from(n).ok().and_then(|n| tree.parent_slot(n, h)) != Some(c32);
            if leave {
                let up = if c == self.root as usize {
                    None
                } else if c < base {
                    // A base parent keeps the run's hint when it lies in
                    // `next`'s run.
                    tree.parent_of(c).and_then(|p| {
                        let ph = if ch == h && p >= self.run_start as usize {
                            ch
                        } else {
                            super::NONE
                        };
                        Some((u32::try_from(p).ok()?, ph))
                    })
                } else {
                    tree.nav_parent_hinted(c32, ch)
                };
                self.current = up.map_or(NO_CURRENT, |(p, _)| p as usize);
                self.current_hint = up.map_or(super::NONE, |(_, ph)| ph);
                return Some(WalkEvent::Leave(Node::hinted(tree, c32, ch)));
            }
        }
        if self.end == 0 {
            return None;
        }
        self.end = self.end.saturating_sub(1);
        let i = u32::try_from(n).ok()?;
        if self.end > 0 {
            match i.checked_add(1).filter(|m| *m < self.run_end) {
                Some(m) => self.next = m as usize,
                None => {
                    let next = if h == super::NONE {
                        // (Without a hint, the step finds one: one run
                        // lookup. `run_start` is then where `next` is,
                        // which only costs the parents' hints.)
                        tree.nav_succ_hinted(i, super::NONE)
                            .map(|(s, sh)| (sh, s, tree.run_slots(sh).1))
                    } else {
                        tree.run_after(h)
                    };
                    (self.next, self.hint, self.run_start, self.run_end) = match next {
                        Some((nh, s, t)) => (s as usize, nh, s, t),
                        None => (usize::MAX, super::NONE, 0, 0),
                    };
                }
            }
        }
        self.current = i as usize;
        self.current_hint = h;
        Some(WalkEvent::Enter(Node::hinted(tree, i, h)))
    }
}

impl core::iter::FusedIterator for Walk<'_> {}

impl core::fmt::Debug for Walk<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let current = (self.current != NO_CURRENT).then_some(self.current);
        f.debug_struct("Walk")
            .field("root", &self.root)
            .field("next", &self.next)
            .field("end", &self.end)
            .field("current", &current)
            .finish()
    }
}
