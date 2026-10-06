//! Comment attachment: which numbered rule of
//! `docs/comment-attachment.md` gives each comment its owner; and the
//! comment API's reads: a node's comments by owner
//! ([`Document::comments`]), the document header and footer, and explicit
//! re-owning ([`Document::attach_comment`], [`Document::detach_comment`]).
//! The writes are in `comment_edit.rs`.
//!
//! Owners are decided by the syntax layer over the document's one syntax
//! tree (no second scanner), on demand: nothing is computed or kept
//! while parsing. [`Document::explain_comment`] decides one
//! comment (O(depth + the comment lines around it));
//! [`Document::explain_comments`] decides every comment of the document
//! in one O(n) pass; a node's comments are read from the few runs of
//! comment lines around it, one run at a time
//! (`SyntaxTree::comment_run_attachments`), never from the whole document.
//!
//! **Storage and view**: comments are stored byte for byte; only
//! [`Comment::text`] strips the `#` and one space after it, and the
//! setters add them.
//!
//! Every walk here is a loop over tokens or siblings.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use oneyaml_core::Span;
// Syntax types stay out of public signatures; the syntax layer's
// attachment types are named through their module only.
use oneyaml_syntax::tree as syntax;
use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind};

use super::place::{self, Coll};
use super::{Document, EditError, NodeId, NodeRef, TreeId};

/// A stable handle to a comment of a [`Document`].
///
/// Like a [`NodeId`], it stays valid across edits that do not remove or
/// replace the comment itself, and across clones; a handle
/// whose comment is gone yields [`EditError::StaleNode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommentId(TreeId);

/// The numbered rule of `docs/comment-attachment.md` that decided a
/// comment's owner. Numbers are stable within a major version;
/// rule 8 (blank-line counts, [`CommentExplanation::blank_lines_before`])
/// decides no owner and has no variant.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CommentRule {
    /// Rule 1: own-line, before the document's `---` → document header.
    HeaderBeforeStart,
    /// Rule 2: own-line, before the root with a blank line after it (or in
    /// a document with no content) → document header.
    HeaderBlankLine,
    /// Rule 3: own-line, before the root with no blank line after it →
    /// leading comment of the first entry, item, or node.
    FirstNodeLeading,
    /// Rule 4: own-line, above a node (blank lines between allowed) at or
    /// right of its column → its leading comment (the entry's, for a key
    /// or `-`).
    Leading,
    /// Rule 5: after content on its line → trailing comment of that
    /// entry or item (or the root; header or footer on a marker line).
    Trailing,
    /// Rule 6: own-line, after a block collection's last child, right of
    /// the column of what follows → that collection's footer.
    CollectionFooter,
    /// Rule 7: own-line, after the root at root column (or after the last
    /// document) → document footer.
    DocumentFooter,
    /// Rule 9: own-line, inside a flow collection → leading comment of the
    /// next item, or the collection's footer before its closing bracket.
    Flow,
    /// Rule 10: own-line, left of the column of the block node below →
    /// still that node's leading comment.
    ShallowLeading,
    /// Rule 11: own-line, directly above the `:` of an explicit `? ` entry
    /// → leading comment of the entry's value.
    ExplicitValue,
    /// Rule 12: own-line, followed by a property, a block scalar header,
    /// or error bytes inside a node → leading comment of that node.
    Inside,
    /// Rule 13: own-line, in a run of comment lines whose earlier comments
    /// already went to an outer owner → the owner of the comment before it
    /// (owners only move outward within a run).
    RunOrder,
    /// Not a rule of the rulebook: the owner was set explicitly with
    /// [`Document::attach_comment`], and beats the rules until
    /// [`Document::detach_comment`]. Its [`CommentRule::number`] is 0.
    ///
    /// An explicit owner lives in the [`Document`] only: the document's
    /// text does not carry it, so parsing that text again gives the comment
    /// its rulebook owner.
    Explicit,
}

impl CommentRule {
    /// The rule's number in `docs/comment-attachment.md` (0 for
    /// [`CommentRule::Explicit`], which is not a rule of the rulebook).
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Explicit => 0,
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

    /// A one-line summary of the rule.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::HeaderBeforeStart => "document header: before `---`",
            Self::HeaderBlankLine => "document header: a blank line before the root",
            Self::FirstNodeLeading => "leading: the document's first node",
            Self::Leading => "leading: directly above the node",
            Self::Trailing => "trailing: after content on its line",
            Self::CollectionFooter => {
                "collection footer: after the last child, deeper than what follows"
            }
            Self::DocumentFooter => "document footer: after the root",
            Self::Flow => "flow collection: the next item, or the footer",
            Self::ShallowLeading => "leading: left of the node below",
            Self::ExplicitValue => "leading: the value of an explicit `? ` entry",
            Self::Inside => "leading: inside the node (properties, header)",
            Self::RunOrder => "run order: the owner of the comment before it",
            Self::Explicit => "explicit: attached with Document::attach_comment",
        }
    }
}

/// Who owns a comment. A mapping **entry** is named by its key's
/// [`NodeId`], a sequence item by its node's; resolve them with
/// [`Document::node`]. Also what [`Document::attach_comment`] takes to
/// re-own a comment explicitly.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommentOwner {
    /// The document header.
    DocumentHeader,
    /// The document footer.
    DocumentFooter,
    /// A leading comment of this entry (its key), item, or node.
    Leading(NodeId),
    /// The trailing (same-line) comment of this entry (its key), item, or
    /// node.
    Trailing(NodeId),
    /// The footer of this collection.
    Footer(NodeId),
}

/// Why a comment belongs where it does ([`Document::explain_comment`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CommentExplanation {
    id: CommentId,
    span: Span,
    text: String,
    owner: CommentOwner,
    owner_span: Option<Span>,
    rule: CommentRule,
    blank_lines_before: usize,
}

impl CommentExplanation {
    /// The comment's handle.
    #[must_use]
    pub const fn id(&self) -> CommentId {
        self.id
    }

    /// The comment's bytes in the document's text (UTF-8 byte range).
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The comment exactly as written, `#` included (byte-exact storage).
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The owner.
    #[must_use]
    pub const fn owner(&self) -> CommentOwner {
        self.owner
    }

    /// The span of the owning node (`None` for the document header and
    /// footer).
    #[must_use]
    pub const fn owner_span(&self) -> Option<Span> {
        self.owner_span
    }

    /// The rule that decided the owner.
    #[must_use]
    pub const fn rule(&self) -> CommentRule {
        self.rule
    }

    /// Blank lines between the previous non-blank line and the comment's
    /// line (rule 8); 0 for a same-line comment. Blank lines that are a
    /// block scalar's content (`|+`) are not counted.
    #[must_use]
    pub const fn blank_lines_before(&self) -> usize {
        self.blank_lines_before
    }
}

impl core::fmt::Display for CommentExplanation {
    /// `rule N (summary): owner`, compact.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let what = match self.owner {
            CommentOwner::DocumentHeader => "document header",
            CommentOwner::DocumentFooter => "document footer",
            CommentOwner::Leading(_) => "leading comment of the node",
            CommentOwner::Trailing(_) => "trailing comment of the node",
            CommentOwner::Footer(_) => "footer of the collection",
        };
        write!(
            f,
            "comment at {}..{}: rule {} ({}): {what}",
            self.span.start,
            self.span.end,
            self.rule.number(),
            self.rule.summary()
        )?;
        if let Some(s) = self.owner_span {
            write!(f, " at {}..{}", s.start, s.end)?;
        }
        Ok(())
    }
}

/// One comment, as the comment API reads it.
///
/// Storage is byte-exact ([`Comment::raw`]); [`Comment::text`] is the
/// view, with the `#` and one space after it stripped.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Comment {
    id: CommentId,
    span: Span,
    raw: String,
    blank_lines_before: usize,
    explicit: bool,
}

impl Comment {
    /// The comment's handle.
    #[must_use]
    pub const fn id(&self) -> CommentId {
        self.id
    }

    /// The comment's bytes in the document's text (UTF-8 byte range).
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// The comment exactly as stored, `#` included (tabs, a missing space,
    /// trailing white space: all kept).
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The view: [`Comment::raw`] without its `#` and the one space after
    /// it, if any (`# note` and `#note` both read `note`; `#  x` reads
    /// ` x`). The setters take text in this form.
    #[must_use]
    pub fn text(&self) -> &str {
        view(&self.raw)
    }

    /// Blank lines between the previous non-blank line and the comment's
    /// line (rule 8); 0 for a same-line comment.
    #[must_use]
    pub const fn blank_lines_before(&self) -> usize {
        self.blank_lines_before
    }

    /// `true` when the comment belongs here because of
    /// [`Document::attach_comment`], not the rulebook.
    #[must_use]
    pub const fn is_explicit(&self) -> bool {
        self.explicit
    }
}

/// No comment lines: what the comment setters take to remove the lines
/// they set (an empty array's item type cannot be inferred, so `[]` alone
/// does not compile there).
///
/// ```
/// use oneyaml::{Document, NO_COMMENTS};
///
/// let mut doc = Document::parse("# about a\na: 1\n").unwrap();
/// doc.get_mut("a").unwrap().set_leading(NO_COMMENTS).unwrap();
/// assert_eq!(doc.to_string(), "a: 1\n");
/// ```
pub const NO_COMMENTS: [&str; 0] = [];

/// The comments a node owns ([`Document::comments`]), each list in
/// source order.
///
/// A mapping **entry** is named by its key: its leading and trailing
/// comments are read from the key's handle (see [`NodeRef::key`]); a value
/// written on the line below its key (`k:⏎  # c⏎  v`) has leading comments
/// of its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Comments {
    leading: Vec<Comment>,
    trailing: Vec<Comment>,
    footer: Vec<Comment>,
    blank_lines_before: usize,
}

impl Comments {
    /// The comment lines above the entry, item, or node (rules 3, 4, 9–13).
    #[must_use]
    pub fn leading(&self) -> &[Comment] {
        &self.leading
    }

    /// The same-line comments (rule 5): usually one, on the entry's first
    /// line; an entry whose flow value spans lines can also have one after
    /// its closing bracket.
    #[must_use]
    pub fn trailing(&self) -> &[Comment] {
        &self.trailing
    }

    /// The footer of a collection (rules 6 and 9): the comment lines after
    /// its last entry (block), or before its closing bracket (flow).
    #[must_use]
    pub fn footer(&self) -> &[Comment] {
        &self.footer
    }

    /// Blank lines right above the line where the entry, item, or node
    /// starts (rule 8; below its leading comments, if any); 0 when it does
    /// not start its line.
    #[must_use]
    pub fn blank_lines_before(&self) -> usize {
        self.blank_lines_before
    }

    /// `true` without comments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.leading.is_empty() && self.trailing.is_empty() && self.footer.is_empty()
    }
}

/// The view of a stored comment: without its `#` and one space after it.
pub(super) fn view(raw: &str) -> &str {
    match raw.strip_prefix('#') {
        Some(r) => r.strip_prefix(' ').unwrap_or(r),
        None => raw,
    }
}

/// The characters a comment written by the API may hold: YAML 1.2.2
/// production 34 `nb-char` (a comment's characters, production 75),
/// `c-printable` without line breaks and the byte order mark, and also
/// without U+0085 (NEL), U+2028 and U+2029 (every
/// every-reader principle): YAML 1.1 readers (PyYAML, libyaml) and
/// Unicode-aware tools read them as line breaks, which would end the
/// comment there and turn the rest of the line into content.
fn comment_char(c: char) -> bool {
    matches!(c,
        '\t'
        | '\u{20}'..='\u{7E}'
        | '\u{A0}'..='\u{2027}'
        | '\u{202A}'..='\u{D7FF}'
        | '\u{E000}'..='\u{FFFD}'
        | '\u{10000}'..='\u{10FFFF}')
        && c != '\u{FEFF}'
}

/// The stored form of view text `text` (the `#` and one space are
/// added, so `text` reads back from [`Comment::text`]; empty text is a bare
/// `#`).
///
/// # Errors
///
/// [`EditError::InvalidComment`] when `text` holds a character a comment
/// cannot hold (a line break, the byte order mark, a control character),
/// or one some reader takes for a line break (U+0085, U+2028, U+2029):
/// refused, never mangled.
pub(super) fn raw_of(text: &str) -> Result<String, EditError> {
    if !text.chars().all(comment_char) {
        return Err(EditError::InvalidComment {
            text: text.to_owned(),
        });
    }
    Ok(if text.is_empty() {
        "#".to_owned()
    } else {
        format!("# {text}")
    })
}

/// The public rule of a syntax-layer rule; `None` for one this version
/// does not know (an internal invariant).
pub(super) fn rule_of(r: syntax::AttachRule) -> Option<CommentRule> {
    use syntax::AttachRule as R;
    Some(match r {
        R::HeaderBeforeStart => CommentRule::HeaderBeforeStart,
        R::HeaderBlankLine => CommentRule::HeaderBlankLine,
        R::FirstNodeLeading => CommentRule::FirstNodeLeading,
        R::Leading => CommentRule::Leading,
        R::Trailing => CommentRule::Trailing,
        R::CollectionFooter => CommentRule::CollectionFooter,
        R::DocumentFooter => CommentRule::DocumentFooter,
        R::Flow => CommentRule::Flow,
        R::ShallowLeading => CommentRule::ShallowLeading,
        R::ExplicitValue => CommentRule::ExplicitValue,
        R::Inside => CommentRule::Inside,
        R::RunOrder => CommentRule::RunOrder,
        _ => return None,
    })
}

/// The public owner of a syntax-layer attachment, with the owning node's
/// span; `None` for an owner this version does not know.
///
/// No owner is the empty placeholder the parser puts right after an
/// error region (it has no bytes and ends its collection, so no rule
/// reaches it; rulebook section 5, checked by the syntax fuzz): an owner
/// is already the node navigation presents, no mapping needed.
pub(super) fn owner_of(a: &syntax::Attachment<'_>) -> Option<(CommentOwner, Option<Span>)> {
    let node = |o: TreeNode<'_>| (NodeId(o.id()), Some(o.span()));
    Some(match a.owner() {
        syntax::CommentOwner::DocumentHeader(_) => (CommentOwner::DocumentHeader, None),
        syntax::CommentOwner::DocumentFooter(_) => (CommentOwner::DocumentFooter, None),
        syntax::CommentOwner::Leading(o) => {
            let (id, s) = node(o);
            (CommentOwner::Leading(id), s)
        }
        syntax::CommentOwner::Trailing(o) => {
            let (id, s) = node(o);
            (CommentOwner::Trailing(id), s)
        }
        syntax::CommentOwner::Footer(o) => {
            let (id, s) = node(o);
            (CommentOwner::Footer(id), s)
        }
        _ => return None,
    })
}

/// The public form of a syntax-layer attachment; `None` for a rule or
/// owner this version does not know (an internal invariant).
fn explain(n: TreeNode<'_>, a: &syntax::Attachment<'_>) -> Option<CommentExplanation> {
    let rule = rule_of(a.rule())?;
    let (owner, owner_span) = owner_of(a)?;
    Some(CommentExplanation {
        id: CommentId(n.id()),
        span: n.span(),
        text: n.text().into_owned(),
        owner,
        owner_span,
        rule,
        blank_lines_before: a.blank_lines_before(),
    })
}

// ============================================================ locating

/// A token with bytes that is not trivia: what the rulebook calls a
/// content token (a run of comment lines lies between two of them).
pub(super) fn visible(t: TreeNode<'_>) -> bool {
    t.is_token() && !t.kind().is_trivia() && !t.span().is_empty()
}

/// The token ends its line: a line break, or a block scalar's text (which
/// ends with its own).
pub(super) fn ends_line(t: TreeNode<'_>) -> bool {
    t.kind() == SyntaxKind::LineBreak || (t.is_token() && t.text().ends_with(['\n', '\r']))
}

/// White space or a byte order mark (inside a line, before content).
pub(super) fn blankish(t: TreeNode<'_>) -> bool {
    matches!(t.kind(), SyntaxKind::Whitespace | SyntaxKind::ByteOrderMark)
        || (t.kind() != SyntaxKind::LineBreak && t.span().is_empty())
}

/// The token before `t`, white space and byte order marks skipped: what
/// ends the previous line when `t` starts its line (`None` at the start of
/// the text).
pub(super) fn before_indent(t: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = place::prev_token(t);
    while let Some(x) = at.filter(|x| blankish(*x)) {
        at = place::prev_token(x);
    }
    at
}

/// Only white space lies between the start of `t`'s line and `t` (a line
/// starts after a line break, after a block scalar's text, or at the start
/// of the text).
pub(super) fn line_start(t: TreeNode<'_>) -> bool {
    before_indent(t).is_none_or(ends_line)
}

/// The white space between the start of `t`'s line and `t` (its
/// indentation, when it starts its line).
pub(super) fn line_prefix(t: TreeNode<'_>) -> String {
    let mut parts = Vec::new();
    let mut at = place::prev_token(t);
    while let Some(x) = at.filter(|x| x.kind() == SyntaxKind::Whitespace) {
        parts.push(x.text().into_owned());
        at = place::prev_token(x);
    }
    parts.iter().rev().map(String::as_str).collect()
}

/// The first token of `n`'s subtree (`n` for a token; the token after it
/// for a node without tokens).
pub(super) fn first_token(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = n;
    loop {
        if at.is_token() {
            return Some(at);
        }
        match at.first_child() {
            Some(c) => at = c,
            None => return place::next_token(at),
        }
    }
}

/// The last token of `n`'s subtree (`n` for a token; the token before it
/// for a node without tokens).
pub(super) fn last_token(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    if n.is_token() {
        return Some(n);
    }
    if let Some(after) = place::next_token(n) {
        return place::prev_token(after);
    }
    // At the end of the text: down the last children (a loop).
    let mut at = n;
    while let Some(c) = at.last_child() {
        at = c;
    }
    if at.is_token() {
        Some(at)
    } else {
        place::prev_token(at)
    }
}

/// Blank lines above a line whose previous line ends with `end` (rule 8,
/// as the syntax layer counts them for comments).
fn blank_before(end: Option<TreeNode<'_>>) -> usize {
    let Some(mut at) = end else { return 0 };
    let mut count = 0usize;
    loop {
        if at.kind() != SyntaxKind::LineBreak {
            // A block scalar's text: its lines are content.
            return count;
        }
        let mut u = place::prev_token(at);
        while let Some(t) = u.filter(|t| blankish(*t)) {
            u = place::prev_token(t);
        }
        match u {
            None => return count.saturating_add(1),
            Some(t) if t.kind() == SyntaxKind::LineBreak => {
                count = count.saturating_add(1);
                at = t;
            }
            Some(t) if ends_line(t) => return count.saturating_add(1),
            Some(_) => return count,
        }
    }
}

/// Where the entry, item, or node `x` starts: its `?` or `-`, else `x`.
pub(super) fn entry_start(x: TreeNode<'_>) -> TreeNode<'_> {
    let indicator = match x.parent().and_then(Coll::of) {
        Some(Coll::BlockSeq) => SyntaxKind::SequenceEntry,
        Some(c) if c.is_map() => SyntaxKind::MappingKey,
        _ => return x,
    };
    let mut at = x.prev_sibling();
    while let Some(s) = at {
        if s.kind() == indicator {
            return s;
        }
        if !s.kind().is_trivia() {
            break;
        }
        at = s.prev_sibling();
    }
    x
}

/// `x` is a value of a mapping (not a key).
pub(super) fn is_value(x: TreeNode<'_>) -> bool {
    x.parent()
        .filter(|p| Coll::of(*p).is_some_and(Coll::is_map))
        .is_some_and(|p| !place::is_key(p, x))
}

/// The value of the entry whose key is `x`.
pub(super) fn value_of(x: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = x.next_sibling();
    while let Some(s) = at {
        if place::is_item(s) {
            return Some(s);
        }
        at = s.next_sibling();
    }
    None
}

/// The comments before `from` back to the previous content token (a `,`
/// passed in flow context, rule 9), pushed onto `out`; returns that token.
pub(super) fn comments_back<'t>(
    from: TreeNode<'t>,
    flow: bool,
    out: &mut Vec<TreeNode<'t>>,
) -> Option<TreeNode<'t>> {
    let mut at = place::prev_token(from);
    while let Some(t) = at {
        if visible(t) && !(flow && t.kind() == SyntaxKind::FlowEntry) {
            return Some(t);
        }
        if t.kind() == SyntaxKind::Comment {
            out.push(t);
        }
        at = place::prev_token(t);
    }
    None
}

/// The comments after `from`'s subtree up to the next content token,
/// pushed onto `out`.
fn comments_forward<'t>(from: TreeNode<'t>, out: &mut Vec<TreeNode<'t>>) {
    let mut at = place::next_token(from);
    while let Some(t) = at {
        if visible(t) {
            return;
        }
        if t.kind() == SyntaxKind::Comment {
            out.push(t);
        }
        at = place::next_token(t);
    }
}

/// The comment line before own-line comment `c` in its run (`None` at
/// the run's content token, or before a same-line comment). O(the blank
/// lines between).
pub(super) fn prev_comment_in_run(c: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = place::prev_token(c);
    while let Some(t) = at {
        if visible(t) {
            return None;
        }
        if t.kind() == SyntaxKind::Comment {
            return line_start(t).then_some(t);
        }
        at = place::prev_token(t);
    }
    None
}

/// The comment right after `p`'s subtree on its line (white space
/// between only).
pub(super) fn comment_after(p: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = place::next_token(p);
    while let Some(t) = at.filter(|t| blankish(*t)) {
        at = place::next_token(t);
    }
    at.filter(|t| t.kind() == SyntaxKind::Comment)
}

/// The owner of a comment as the rulebook decides it, with its document
/// (header and footer owners are compared with it).
pub(super) type Key = (CommentOwner, Option<TreeId>);

/// A comment found for an owner: its node, blank lines, whether the
/// owner is explicit.
#[derive(Clone, Copy)]
pub(super) struct Hit<'t> {
    pub(super) node: TreeNode<'t>,
    blank: usize,
    pub(super) explicit: bool,
}

/// What reading one owner's comments found.
#[derive(Default)]
pub(super) struct Found<'t> {
    pub(super) leading: Vec<Hit<'t>>,
    pub(super) trailing: Vec<Hit<'t>>,
    pub(super) footer: Vec<Hit<'t>>,
    /// Every comment of the runs read, with its rulebook owner: what a
    /// comment edit checks afterwards.
    pub(super) seen: Vec<(TreeNode<'t>, Option<Key>)>,
    pub(super) blank_lines_before: usize,
}

fn to_comment(h: &Hit<'_>) -> Comment {
    Comment {
        id: CommentId(h.node.id()),
        span: h.node.span(),
        raw: h.node.text().into_owned(),
        blank_lines_before: h.blank,
        explicit: h.explicit,
    }
}

fn to_comments(hits: &[Hit<'_>]) -> Vec<Comment> {
    hits.iter().map(to_comment).collect()
}

/// Which list of [`Found`] an owner goes to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Leading,
    Trailing,
    Footer,
}

impl CommentId {
    pub(super) const fn of(id: TreeId) -> Self {
        Self(id)
    }

    /// The syntax-tree handle.
    pub(super) const fn tree_id(self) -> TreeId {
        self.0
    }
}

/// Which owner an explicit attachment names, as an ordered key: its kind
/// and its node (for the document header and footer, the document).
pub(super) type OwnerKey = (u8, Option<TreeId>);

/// A comment's explicit entry before a change made inside an open edit
/// group ([`Document::undo_explicit`]).
pub(super) type ExplicitUndo = (TreeId, Option<(CommentOwner, OwnerKey)>);

/// The key of `owner` in document `doc`.
fn owner_key(owner: CommentOwner, doc: Option<TreeId>) -> OwnerKey {
    match owner {
        CommentOwner::DocumentHeader => (0, doc),
        CommentOwner::DocumentFooter => (1, doc),
        CommentOwner::Leading(n) => (2, Some(n.0)),
        CommentOwner::Trailing(n) => (3, Some(n.0)),
        CommentOwner::Footer(n) => (4, Some(n.0)),
    }
}

/// Explicit comment owners ([`Document::attach_comment`]), indexed both
/// ways:
/// a read looks up the comments of its own owners, O(log k + found),
/// never all k. Shared between a document's clones and stream snapshots
/// (`Arc`, copied on the first change after they diverge, as the tree).
#[derive(Clone, Debug, Default)]
pub(super) struct Explicit {
    by_comment: BTreeMap<TreeId, (CommentOwner, OwnerKey)>,
    by_owner: BTreeMap<OwnerKey, BTreeSet<TreeId>>,
    /// Where incremental pruning goes on ([`Document::prune_explicit`]).
    cursor: Option<TreeId>,
}

impl Explicit {
    fn get(&self, c: TreeId) -> Option<CommentOwner> {
        self.by_comment.get(&c).map(|(o, _)| *o)
    }

    pub(super) fn contains(&self, c: TreeId) -> bool {
        self.by_comment.contains_key(&c)
    }

    fn insert(&mut self, c: TreeId, owner: CommentOwner, doc: Option<TreeId>) {
        self.remove(c);
        let key = owner_key(owner, doc);
        self.by_comment.insert(c, (owner, key));
        self.by_owner.entry(key).or_default().insert(c);
    }

    fn remove(&mut self, c: TreeId) -> Option<CommentOwner> {
        let (owner, key) = self.by_comment.remove(&c)?;
        if let Some(set) = self.by_owner.get_mut(&key) {
            set.remove(&c);
            if set.is_empty() {
                self.by_owner.remove(&key);
            }
        }
        Some(owner)
    }

    /// Puts comment `c`'s entry back as it was (`None`: no entry).
    fn restore(&mut self, c: TreeId, prev: Option<(CommentOwner, OwnerKey)>) {
        self.remove(c);
        if let Some((owner, key)) = prev {
            self.by_comment.insert(c, (owner, key));
            self.by_owner.entry(key).or_default().insert(c);
        }
    }

    fn of_owner(&self, key: OwnerKey) -> impl Iterator<Item = TreeId> + '_ {
        self.by_owner.get(&key).into_iter().flatten().copied()
    }
}

impl Document {
    /// The explicit owner of comment `c` ([`Document::attach_comment`]),
    /// when it still names something of this document.
    pub(super) fn explicit_owner(&self, c: TreeId) -> Option<CommentOwner> {
        let o = self.explicit.as_deref()?.get(c)?;
        self.owner_live(o).then_some(o)
    }

    /// The explicit owner entry of comment `c`, live or not (node
    /// edits carry it).
    pub(super) fn explicit_raw(&self, c: TreeId) -> Option<CommentOwner> {
        self.explicit.as_deref()?.get(c)
    }

    /// The comments explicitly owned by one of `nodes` (leading, trailing,
    /// or footer), with those owners: O(|nodes| · log k) (a
    /// removed entry's explicit owners, wherever their comments are).
    pub(super) fn explicit_owned_by(&self, nodes: &[TreeId]) -> Vec<(TreeId, CommentOwner)> {
        let Some(e) = self.explicit.as_deref() else {
            return Vec::new();
        };
        if e.by_comment.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for n in nodes {
            for kind in [2u8, 3, 4] {
                for c in e.of_owner((kind, Some(*n))) {
                    if let Some(o) = e.get(c) {
                        out.push((c, o));
                    }
                }
            }
        }
        out
    }

    /// Some comment has an explicit owner entry (live or lapsed): without
    /// one, [`Document::explicit_owner`] and
    /// [`Document::explicit_owned_by`] answer nothing.
    pub(super) fn any_explicit(&self) -> bool {
        self.explicit
            .as_deref()
            .is_some_and(|e| !e.by_comment.is_empty())
    }

    /// `c` has an explicit owner entry (live or lapsed).
    pub(super) fn has_explicit(&self, c: TreeId) -> bool {
        self.explicit.as_deref().is_some_and(|e| e.contains(c))
    }

    /// Records comment `c`'s explicit entry before a change inside an open
    /// edit group, so that a rollback restores it (a refused edit
    /// changes nothing). O(log k) per change; nothing outside a group.
    fn log_explicit(&mut self, c: TreeId) {
        if self.tree().in_group() {
            let prev = self
                .explicit
                .as_deref()
                .and_then(|e| e.by_comment.get(&c).copied());
            self.explicit_log.push((c, prev));
        }
    }

    /// Sets comment `c`'s explicit owner, logged for a rollback.
    fn explicit_insert(&mut self, c: TreeId, owner: CommentOwner, doc: Option<TreeId>) {
        self.log_explicit(c);
        self.explicit_mut().insert(c, owner, doc);
    }

    /// Drops comment `c`'s explicit owner, logged for a rollback.
    fn explicit_remove(&mut self, c: TreeId) {
        self.log_explicit(c);
        self.explicit_mut().remove(c);
    }

    /// Reverts the explicit-owner changes logged from position `from` on
    /// (a rolled-back edit group), newest first.
    pub(super) fn undo_explicit(&mut self, from: usize) {
        let ops = self
            .explicit_log
            .split_off(from.min(self.explicit_log.len()));
        if ops.is_empty() {
            return;
        }
        let e = self.explicit_mut();
        for (c, prev) in ops.into_iter().rev() {
            e.restore(c, prev);
        }
    }

    /// The length of the explicit-owner log (where an edit group starts).
    pub(super) fn explicit_log_len(&self) -> usize {
        self.explicit_log.len()
    }

    /// Forgets the log once the outermost edit group is settled.
    pub(super) fn settle_explicit_log(&mut self) {
        if !self.tree().in_group() {
            self.explicit_log.clear();
        }
    }

    /// The explicit owners, for a change (copied first when a clone or a
    /// snapshot shares them).
    fn explicit_mut(&mut self) -> &mut Explicit {
        Arc::make_mut(self.explicit.get_or_insert_with(Arc::default))
    }

    /// Drops the explicit owners of comments `gone` (removed by a comment
    /// write): O(|gone| log k).
    pub(super) fn forget_explicit(&mut self, gone: &[TreeId]) {
        for c in gone {
            if self.has_explicit(*c) {
                self.explicit_remove(*c);
            }
        }
    }

    /// Incremental pruning (memory follows the tree, not
    /// the edit history"): after every edit, the next few explicit entries
    /// (round robin) are checked, and those whose comment is gone or whose
    /// owner lapsed are dropped. O(log k) per edit; an entry left behind by
    /// an edit is dropped within k/`PRUNE` later edits, and until then
    /// reads ignore it.
    pub(super) fn prune_explicit(&mut self) {
        const PRUNE: usize = 4;
        let Some(e) = self.explicit.as_deref() else {
            return;
        };
        // Inside an open edit group, a removed comment may come back with
        // a rollback: only settled trees are pruned.
        if self.tree().in_group() {
            return;
        }
        if e.by_comment.is_empty() {
            return;
        }
        let from = e.cursor;
        let mut batch: Vec<(TreeId, CommentOwner, OwnerKey)> = match from {
            Some(c) => e
                .by_comment
                .range((std::ops::Bound::Excluded(c), std::ops::Bound::Unbounded))
                .take(PRUNE)
                .map(|(k, (o, key))| (*k, *o, *key))
                .collect(),
            None => Vec::new(),
        };
        if batch.len() < PRUNE {
            let more = PRUNE.saturating_sub(batch.len());
            batch.extend(
                e.by_comment
                    .iter()
                    .take(more)
                    .map(|(k, (o, key))| (*k, *o, *key)),
            );
        }
        let tree = self.tree();
        // Alive in the tree, whichever document of a stream this one is
        // now (a `Stream` keeps every document's explicit owners).
        let alive = |o: CommentOwner, key: OwnerKey| match o {
            CommentOwner::DocumentHeader | CommentOwner::DocumentFooter => {
                key.1.is_none_or(|d| tree.get(d).is_ok())
            }
            CommentOwner::Leading(n) | CommentOwner::Trailing(n) => {
                tree.get(n.0).is_ok_and(|x| x.kind().is_yaml_node())
            }
            CommentOwner::Footer(n) => tree.get(n.0).is_ok_and(|x| Coll::of(x).is_some()),
        };
        let dead: Vec<TreeId> = batch
            .iter()
            .filter(|(c, o, key)| tree.get(*c).is_err() || !alive(*o, *key))
            .map(|(c, _, _)| *c)
            .collect();
        let next = batch.last().map(|(c, _, _)| *c);
        if dead.is_empty() && next == from {
            return;
        }
        let e = self.explicit_mut();
        for c in dead {
            e.remove(c);
        }
        e.cursor = next;
    }

    /// Sets comment `c`'s explicit owner (node edits carry it).
    pub(super) fn set_explicit(&mut self, c: TreeId, owner: CommentOwner, doc: Option<TreeId>) {
        self.explicit_insert(c, owner, doc);
    }

    /// `owner` names something of this document that can own comments.
    pub(super) fn owner_is_live(&self, owner: CommentOwner) -> bool {
        self.owner_live(owner)
    }

    /// `owner` names something of this document that can own comments.
    fn owner_live(&self, owner: CommentOwner) -> bool {
        match owner {
            CommentOwner::DocumentHeader => true,
            CommentOwner::DocumentFooter => self.doc_node().is_some(),
            CommentOwner::Leading(n) | CommentOwner::Trailing(n) => self
                .node(n)
                .is_ok_and(|r| r.tree_node().kind().is_yaml_node()),
            CommentOwner::Footer(n) => self
                .node(n)
                .is_ok_and(|r| Coll::of(r.tree_node()).is_some()),
        }
    }

    /// The span of an owner (its node's).
    fn owner_span(&self, owner: CommentOwner) -> Option<Span> {
        match owner {
            CommentOwner::Leading(n) | CommentOwner::Trailing(n) | CommentOwner::Footer(n) => {
                self.node(n).ok().map(NodeRef::span)
            }
            _ => None,
        }
    }

    /// [`explain`] with this document's explicit owners applied.
    fn explained(&self, n: TreeNode<'_>, a: &syntax::Attachment<'_>) -> Option<CommentExplanation> {
        let mut e = explain(n, a)?;
        if let Some(o) = self.explicit_owner(n.id()) {
            e.owner = o;
            e.owner_span = self.owner_span(o);
            e.rule = CommentRule::Explicit;
        }
        Some(e)
    }

    /// Every comment of this document, in source order: the comments
    /// the rules give it (its own, and those between it and its neighbours
    /// that belong to its header or footer).
    ///
    /// Explaining agrees with navigation ([`Document::node`],
    /// [`Document::root`]): for a document of a [`Stream`](crate::Stream)
    /// these are that document's comments; for [`Document::parse_recovering`]
    /// of input with several documents, the **first** document's, as
    /// navigation reaches only the first (read the others through
    /// [`Stream`](crate::Stream)); for an input without documents, all of its
    /// comments (the document header). O(this document and the
    /// stream-level text around it).
    #[must_use]
    pub fn comment_ids(&self) -> Vec<CommentId> {
        self.attachments()
            .into_iter()
            .map(|(n, _)| CommentId(n.id()))
            .collect()
    }

    /// Which numbered rule of `docs/comment-attachment.md` attaches the
    /// comment `id` names, and to what: a debug helper for
    /// misattributions. A comment re-owned with
    /// [`Document::attach_comment`] reports its explicit owner and
    /// [`CommentRule::Explicit`]. O(depth + the comment lines around it;
    /// next to stray `...` markers between documents, plus the
    /// stream-level elements up to the neighbouring documents).
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no comment of this
    /// document (see [`Document::comment_ids`]): removed or replaced by an
    /// edit, from another document, or a comment the rules give another
    /// document of the same input (another document of a stream, or a
    /// second document of a multi-document [`Document::parse_recovering`]);
    /// [`EditError::Internal`] should the syntax layer report a rule this
    /// version does not know (an internal invariant).
    pub fn explain_comment(&self, id: CommentId) -> Result<CommentExplanation, EditError> {
        let (n, a) = self.own_comment(id)?;
        self.explained(n, &a).ok_or_else(internal)
    }

    /// The comment `id` names and its attachment, when it is one of this
    /// document's ([`Document::comment_ids`]).
    fn own_comment(
        &self,
        id: CommentId,
    ) -> Result<(TreeNode<'_>, syntax::Attachment<'_>), EditError> {
        let tree = &self.shared.tree;
        let n = tree.get(id.0).map_err(|_| EditError::StaleNode)?;
        if n.kind() != SyntaxKind::Comment {
            return Err(EditError::StaleNode);
        }
        let a = tree.comment_attachment(n).ok_or(EditError::StaleNode)?;
        // The comments `comment_ids` lists, and no others.
        if a.document() != self.doc_node() {
            return Err(EditError::StaleNode);
        }
        Ok((n, a))
    }

    /// Comment `id` is one of this document's ([`Document::comment_ids`]):
    /// the membership check of [`Document::attach_comment`] and
    /// [`Document::detach_comment`].
    ///
    /// Bulk re-owning: in a tree without errors a comment inside
    /// a document's node is that document's (rulebook section 4: only
    /// comments at stream level, between documents, can go either way;
    /// section 5: the exceptions are recovered input), so this costs
    /// O(depth); for other comments the rules' document is read from the
    /// syntax layer's comment-run index (O(depth + log)).
    /// Pinned over the suite and the syntax fuzz (`oneyaml-syntax`
    /// `tests/comments.rs`).
    pub(super) fn check_own_comment(&self, id: CommentId) -> Result<(), EditError> {
        let tree = &self.shared.tree;
        let n = tree.get(id.0).map_err(|_| EditError::StaleNode)?;
        if n.kind() != SyntaxKind::Comment {
            return Err(EditError::StaleNode);
        }
        if !tree.has_errors() {
            let d = n.ancestors().find(|a| a.kind() == SyntaxKind::Document);
            if let (Some(d), Some(own)) = (d, self.doc_node()) {
                if d == own {
                    return Ok(());
                }
            }
        }
        // The rules' document of the comment from the
        // syntax layer's comment-run index, O(depth + log), never its run
        // or the stray `...` markers between it and the documents around
        // it (bulk re-owning of a stretch is linear).
        if tree.comment_document(n) == Some(self.doc_node()) {
            Ok(())
        } else {
            Err(EditError::StaleNode)
        }
    }

    /// [`Document::explain_comment`] for every comment of the document
    /// ([`Document::comment_ids`] order), in one O(n) pass (plus
    /// O(log k) per comment for k explicit owners).
    ///
    /// # Errors
    ///
    /// [`EditError::Internal`] as for [`Document::explain_comment`].
    pub fn explain_comments(&self) -> Result<Vec<CommentExplanation>, EditError> {
        self.attachments()
            .iter()
            .map(|(n, a)| self.explained(*n, a).ok_or_else(internal))
            .collect()
    }

    /// The syntax layer's attachments of this document's comments.
    fn attachments(&self) -> Vec<(TreeNode<'_>, syntax::Attachment<'_>)> {
        let tree = &self.shared.tree;
        match (self.doc_node(), self.index) {
            // This document's comments only, so every
            // owner resolves with `Document::node` and a header or footer
            // is this document's.
            (Some(d), _) => tree.document_comment_attachments(d),
            // An input without documents: its comments are its header.
            (None, None) => tree.comment_attachments(),
            (None, Some(_)) => Vec::new(),
        }
    }

    // ================================================= reading by owner

    /// The comments the rules (or explicit owners) give node `id`:
    /// its leading and trailing comments — an entry's, when `id`
    /// is a mapping key (an entry is named by its key, see
    /// [`NodeRef::key`]) — its footer when it is a collection, and the
    /// blank lines above it. Comments re-owned with
    /// [`Document::attach_comment`] are read where they are attached.
    ///
    /// O(local): the runs of comment lines before the entry, after
    /// its lines' content, and after (block) or at the end of (flow) the
    /// collection, each decided once (runs between documents a stretch at
    /// a time); plus O(log k) for k explicit owners. In a document whose
    /// own subtree holds an error region (restricted, rulebook section 5),
    /// O(the document): its comments are
    /// decided in one pass, so reads always agree with
    /// [`Document::explain_comments`].
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no node of this document;
    /// [`EditError::Internal`] should the syntax layer report an owner this
    /// version does not know.
    pub fn comments(&self, id: NodeId) -> Result<Comments, EditError> {
        let x = self.node(id)?.tree_node();
        let f = self.find_comments(x)?;
        Ok(Comments {
            leading: to_comments(&f.leading),
            trailing: to_comments(&f.trailing),
            footer: to_comments(&f.footer),
            blank_lines_before: f.blank_lines_before,
        })
    }

    /// The document header (rules 1, 2, and 5 on a `---` or
    /// directive line; explicit owners included), in source order. For an
    /// input without documents, all of its comments. O(the header region:
    /// directives, the comment lines before the root).
    ///
    /// # Errors
    ///
    /// [`EditError::Internal`] should the syntax layer report an owner this
    /// version does not know (never an incomplete header).
    pub fn header(&self) -> Result<Vec<Comment>, EditError> {
        Ok(to_comments(&self.find_doc_comments()?.leading))
    }

    /// The document footer (rules 7, and 5 on a `...` line; explicit
    /// owners included), in source order. O(the comment lines after the
    /// root).
    ///
    /// # Errors
    ///
    /// [`EditError::Internal`] as [`Document::header`].
    pub fn footer(&self) -> Result<Vec<Comment>, EditError> {
        Ok(to_comments(&self.find_doc_comments()?.footer))
    }

    /// The attachments of every comment of the runs holding `seeds`, each
    /// run decided once. A run between documents is decided with the whole
    /// stretch of stream-level elements it lies in
    /// (`SyntaxTree::comment_attachments_between`: the documents around it
    /// found once), so N stray `...` markers cost O(N),
    /// never O(N²).
    pub(super) fn decide_runs<'s>(
        &'s self,
        seeds: impl IntoIterator<Item = TreeNode<'s>>,
    ) -> Vec<(TreeNode<'s>, syntax::Attachment<'s>)> {
        let tree = self.tree();
        let mut done = BTreeSet::new();
        let mut out = Vec::new();
        for s in seeds {
            if s.kind() != SyntaxKind::Comment || done.contains(&s.id()) {
                continue;
            }
            // The stream-level element holding it (a climb).
            let mut top = s;
            while let Some(p) = top.parent().filter(|p| p.kind() != SyntaxKind::Stream) {
                top = p;
            }
            let decided = if top.kind() == SyntaxKind::Document {
                tree.comment_run_attachments(s)
            } else {
                let is_doc = |n: &TreeNode<'_>| n.kind() == SyntaxKind::Document;
                let mut before = top.prev_sibling();
                while let Some(b) = before.filter(|b| !is_doc(b)) {
                    before = b.prev_sibling();
                }
                let mut after = top.next_sibling();
                while let Some(a) = after.filter(|a| !is_doc(a)) {
                    after = a.next_sibling();
                }
                tree.comment_attachments_between(before, after)
            };
            for (c, a) in decided {
                if done.insert(c.id()) {
                    out.push((c, a));
                }
            }
        }
        out
    }

    /// Sorts decided comments into `found` by owner (explicit owners
    /// applied): those `wanted` names; every one is recorded in `seen`.
    fn classify<'s>(
        &'s self,
        decided: Vec<(TreeNode<'s>, syntax::Attachment<'s>)>,
        found: &mut Found<'s>,
        wanted: &[(CommentOwner, Slot)],
    ) -> Result<(), EditError> {
        let own = self.doc_node().map(TreeNode::id);
        let keys: Vec<(OwnerKey, Slot)> = wanted
            .iter()
            .map(|(o, s)| (owner_key(*o, own), *s))
            .collect();
        let pick = |o: CommentOwner, doc: Option<TreeId>| {
            let k = owner_key(o, doc);
            keys.iter().find(|(x, _)| *x == k).map(|(_, s)| *s)
        };
        for (c, a) in decided {
            let doc = a.document().map(TreeNode::id);
            let heuristic = owner_of(&a).ok_or_else(internal)?.0;
            found.seen.push((c, Some((heuristic, doc))));
            let (owner, explicit) = match self.explicit_owner(c.id()) {
                // (An explicit owner is only ever this document's.)
                Some(o) if doc == own => (o, true),
                _ => (heuristic, false),
            };
            let hit = Hit {
                node: c,
                blank: a.blank_lines_before(),
                explicit,
            };
            match pick(owner, doc) {
                Some(Slot::Leading) => found.leading.push(hit),
                Some(Slot::Trailing) => found.trailing.push(hit),
                Some(Slot::Footer) => found.footer.push(hit),
                None => {}
            }
        }
        for list in [&mut found.leading, &mut found.trailing, &mut found.footer] {
            list.sort_by_key(|h| h.node.preorder_index());
        }
        Ok(())
    }

    /// This document's own subtree holds an error region (only
    /// only then do reads decide the whole document; a clean document next
    /// to a broken one keeps local reads). O(log documents): the documents
    /// with error nodes are found once per parse and kept up to date by
    /// edits (`Shared::error_docs`). An input without documents: whether
    /// the tree has errors.
    pub(super) fn holds_error_regions(&self) -> bool {
        if self.doc.is_none() {
            return self.tree().has_errors();
        }
        let i = self.index.unwrap_or(0);
        self.shared.error_docs.binary_search(&i).is_ok()
    }

    /// Records every comment of the runs holding `seeds` and the explicit
    /// comments of `wanted`, sorting those owned by one of `wanted`
    /// (explicitly, else by the rulebook) into `found`. O(the runs, plus
    /// log k and the explicit comments found), never all k explicit owners
    /// (see the explicit-owner index).
    ///
    /// **Recovered input**: in a document whose own
    /// subtree holds an error region
    /// (rulebook section 5: owners are best effort there, and the parser's
    /// error regions put comments where no local reading finds them), the
    /// document's comments are decided in one pass instead, so reads always
    /// agree with [`Document::explain_comments`]: O(the document) per read
    /// in a restricted document.
    fn collect_runs<'s>(
        &'s self,
        mut seeds: Vec<TreeNode<'s>>,
        found: &mut Found<'s>,
        wanted: &[(CommentOwner, Slot)],
    ) -> Result<(), EditError> {
        let tree = self.tree();
        if self.holds_error_regions() {
            return self.classify(self.attachments(), found, wanted);
        }
        let own = self.doc_node().map(TreeNode::id);
        if let Some(e) = self.explicit.as_deref() {
            for (o, _) in wanted {
                let k = owner_key(*o, own);
                seeds.extend(e.of_owner(k).filter_map(|c| tree.get(c).ok()));
            }
        }
        let decided = self.decide_runs(seeds);
        self.classify(decided, found, wanted)
    }

    /// The comments node `x` owns, and the runs read to find them (see
    /// [`Document::comments`]).
    pub(super) fn find_comments<'s>(&'s self, x: TreeNode<'s>) -> Result<Found<'s>, EditError> {
        let parent = x.parent();
        let flow = parent.and_then(Coll::of).is_some_and(Coll::is_flow);
        let value = is_value(x);
        let key = !value && parent.and_then(Coll::of).is_some_and(Coll::is_map);
        let start = entry_start(x);
        let mut seeds = Vec::new();
        // Leading (rules 3, 4, 9–11, 13): the run before the entry's start
        // (`,` passed in flow context), and, for a value, the run before
        // its `:` (rule 11).
        let stop = comments_back(start, flow, &mut seeds);
        // Between a `-` or `?` and the node on a later line.
        if start != x {
            comments_back(x, flow, &mut seeds);
        }
        if value {
            if let Some(colon) = stop.filter(|s| s.kind() == SyntaxKind::MappingValue) {
                comments_back(colon, flow, &mut seeds);
            }
        }
        // Rule 12: inside the node, before its first item.
        seeds.extend(
            x.children()
                .take_while(|c| !place::is_item(*c))
                .filter(|c| c.kind() == SyntaxKind::Comment),
        );
        // Trailing (rule 5): after each piece of the entry that can end a
        // line: its `?`/`-`, the key, its `:`, the value's properties and
        // opening bracket, the value, a flow entry's `,`.
        let mut points = vec![start, x];
        let v = if key { value_of(x) } else { Some(x) };
        if key {
            let mut at = x.next_sibling();
            while let Some(s) = at.filter(|s| !place::is_item(*s)) {
                if s.kind() == SyntaxKind::MappingValue {
                    points.push(s);
                }
                at = s.next_sibling();
            }
        }
        if let Some(v) = v {
            points.push(v);
            points.extend(
                v.children()
                    .take_while(|c| !place::is_item(*c))
                    .filter(|c| !c.kind().is_trivia()),
            );
            if flow {
                let mut at = v.next_sibling();
                while let Some(s) = at.filter(|s| s.kind().is_trivia()) {
                    at = s.next_sibling();
                }
                points.extend(at.filter(|s| s.kind() == SyntaxKind::FlowEntry));
            }
        }
        seeds.extend(points.into_iter().filter_map(comment_after));
        // Footer (rules 6, 9).
        match Coll::of(x) {
            Some(c) if c.is_flow() => {
                if let Some(close) = x.last_child().filter(|l| {
                    matches!(
                        l.kind(),
                        SyntaxKind::FlowSequenceClose | SyntaxKind::FlowMappingClose
                    )
                }) {
                    comments_back(close, true, &mut seeds);
                }
            }
            Some(_) => comments_forward(x, &mut seeds),
            None => {}
        }
        let me = NodeId(x.id());
        let mut found = Found {
            blank_lines_before: if line_start(start) {
                blank_before(before_indent(start))
            } else {
                0
            },
            ..Found::default()
        };
        let wanted = [
            (CommentOwner::Leading(me), Slot::Leading),
            (CommentOwner::Trailing(me), Slot::Trailing),
            (CommentOwner::Footer(me), Slot::Footer),
        ];
        self.collect_runs(seeds, &mut found, &wanted)?;
        Ok(found)
    }

    /// The document header (`leading`) and footer (`footer`), and the runs
    /// read to find them.
    ///
    /// # Errors
    ///
    /// [`EditError::Internal`] for an owner this version does not know.
    pub(super) fn find_doc_comments(&self) -> Result<Found<'_>, EditError> {
        let tree = self.tree();
        let mut found = Found::default();
        let Some(d) = self.doc_node() else {
            if self.index.is_none() {
                // An input without documents: every comment is its header.
                for (c, a) in tree.comment_attachments() {
                    found.seen.push((c, owner_of(&a).map(|o| (o.0, None))));
                    found.leading.push(Hit {
                        node: c,
                        blank: a.blank_lines_before(),
                        explicit: false,
                    });
                }
            }
            return Ok(found);
        };
        let mut seeds = Vec::new();
        let root = d.children().find(|c| c.kind().is_yaml_node());
        let stop = root.and_then(first_token);
        // Back from the document's start to the previous content, then on
        // through its directives and `---` to the root (or, without a root,
        // to the next document).
        if let Some(ft) = first_token(d) {
            // Back to the previous document's content: stray `...` markers
            // and error regions between documents are passed (rule
            // 5/C16 gives the comments before them to this document).
            let mut at = place::prev_token(ft);
            while let Some(t) = at {
                if visible(t) && t.ancestors().any(|a| a.kind() == SyntaxKind::Document) {
                    break;
                }
                if t.kind() == SyntaxKind::Comment {
                    seeds.push(t);
                }
                at = place::prev_token(t);
            }
            let mut at = Some(ft);
            while let Some(t) = at {
                if Some(t) == stop || (root.is_none() && in_other_document(t, d)) {
                    break;
                }
                if t.kind() == SyntaxKind::Comment {
                    seeds.push(t);
                }
                at = place::next_token(t);
            }
        }
        // After the root, up to the next document.
        if let Some(r) = root {
            let mut at = place::next_token(r);
            while let Some(t) = at {
                if in_other_document(t, d) {
                    break;
                }
                if t.kind() == SyntaxKind::Comment {
                    seeds.push(t);
                }
                at = place::next_token(t);
            }
        }
        let wanted = [
            (CommentOwner::DocumentHeader, Slot::Leading),
            (CommentOwner::DocumentFooter, Slot::Footer),
        ];
        let picked = self.collect_runs(seeds, &mut found, &wanted);
        // Unreachable, but never an incomplete header or footer in
        // release: the error is reported.
        debug_assert!(picked.is_ok(), "unknown comment owner");
        picked?;
        Ok(found)
    }

    // ============================================== explicit re-owning

    /// Re-owns comment `id` **explicitly** ("`attach_comment`: when
    /// the heuristic got it wrong"): from now on this document reads it as
    /// `owner`'s ([`Document::comments`], [`Document::header`],
    /// [`Document::footer`]), [`Document::explain_comment`] reports
    /// `owner` with [`CommentRule::Explicit`], and the comment writes treat
    /// it as `owner`'s (e.g. [`Document::clear_comments`] of `owner`
    /// removes it). The text does not move: nothing changes in the
    /// document's bytes.
    ///
    /// Cost: O(depth + log k) for k explicit owners when the comment lies
    /// inside the document in a tree without errors (so re-owning every
    /// comment of a long run is linear); otherwise O(its run), as
    /// [`Document::explain_comment`] — for a comment between documents,
    /// that includes the stretch of stream-level elements (stray `...`
    /// markers) up to the documents around it, so re-owning every comment
    /// of such a stretch one by one is quadratic in its length (documented: which document a comment there belongs to depends on
    /// both neighbours; a cheaper bulk path would need a stretch-level
    /// index).
    ///
    /// **Not in the text.** An explicit owner lives in this [`Document`]
    /// (and its clones; for a [`Stream`](crate::Stream), in the stream):
    /// the YAML text cannot carry it, so parsing the document's text
    /// again gives the comment its rulebook owner. This is the documented
    /// exception to "the re-parsed text has the same owners". Entries
    /// whose comment or owner is gone are dropped incrementally, a few per
    /// edit.
    ///
    /// **Through node edits.** The explicit owner follows its
    /// comment when a node edit carries or rewrites it (the comment then
    /// has a new handle): a moved entry's comments, a comment rewritten
    /// where the parser puts it, a same-line comment moved onto a new block
    /// scalar's header. An owner inside a removed entry is recorded
    /// relative to it and resolved on the new entry when the [`Removed`](crate::Removed)
    /// is re-inserted (for a comment that stayed, too); a replaced node's
    /// explicit owners name the new node. It lapses (the rules decide
    /// again) when its comment leaves the document with a removal that is
    /// never re-inserted, or when `owner`'s node is removed for good.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no comment of this document
    /// or `owner` names a node that is not this document's;
    /// [`EditError::Unsupported`] when `owner` cannot own comments: a
    /// footer of a node that is not a collection, an error region, or the
    /// document footer of an input without documents.
    pub fn attach_comment(&mut self, id: CommentId, owner: CommentOwner) -> Result<(), EditError> {
        self.check_own_comment(id)?;
        let unsupported = |reason| EditError::Unsupported {
            at: oneyaml_core::Path::new(),
            reason,
        };
        match owner {
            CommentOwner::DocumentHeader => {}
            CommentOwner::DocumentFooter => {
                if self.doc_node().is_none() {
                    return Err(unsupported(
                        "an input without documents has no document footer",
                    ));
                }
            }
            CommentOwner::Leading(n) | CommentOwner::Trailing(n) => {
                if !self.node(n)?.tree_node().kind().is_yaml_node() {
                    return Err(unsupported("an error region owns no comments"));
                }
            }
            CommentOwner::Footer(n) => {
                if Coll::of(self.node(n)?.tree_node()).is_none() {
                    return Err(unsupported("only a collection has a footer"));
                }
            }
        }
        let doc = self.doc_node().map(TreeNode::id);
        self.explicit_insert(id.0, owner, doc);
        self.prune_explicit();
        Ok(())
    }

    /// Undoes [`Document::attach_comment`]: the rulebook decides comment
    /// `id`'s owner again. Returns the explicit owner it had (`None` when
    /// it had none, or its owner's node is gone). Nothing changes in the
    /// document's bytes.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no comment of this
    /// document.
    pub fn detach_comment(&mut self, id: CommentId) -> Result<Option<CommentOwner>, EditError> {
        self.check_own_comment(id)?;
        let live = self.explicit_owner(id.0);
        if self.has_explicit(id.0) {
            self.explicit_remove(id.0);
        }
        Ok(live)
    }
}

/// Token `t` lies in a document other than `d`.
pub(super) fn in_other_document(t: TreeNode<'_>, d: TreeNode<'_>) -> bool {
    t.ancestors()
        .find(|a| a.kind() == SyntaxKind::Document)
        .is_some_and(|a| a != d)
}

impl<'d> NodeRef<'d> {
    /// The comments this node owns ([`Document::comments`]).
    ///
    /// # Errors
    ///
    /// As [`Document::comments`].
    pub fn comments(self) -> Result<Comments, EditError> {
        self.document().comments(self.id())
    }

    /// For a value of a mapping entry, the entry's key, which names the
    /// entry's comments (`key:  # c` trails the entry); `None` for
    /// any other node.
    #[must_use]
    pub fn key(self) -> Option<NodeRef<'d>> {
        let n = self.tree_node();
        let parent = n.parent()?;
        if !is_value(n) {
            return None;
        }
        let e = place::entry_of(parent, n).ok()?;
        e.key.map(|k| NodeRef::new(self.document(), k))
    }
}

/// An internal invariant failed (`debug_assert!` plus an error in
/// release).
fn internal() -> EditError {
    debug_assert!(false, "comment attachment: unknown rule or owner");
    EditError::Internal
}
