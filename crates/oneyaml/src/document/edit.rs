//! The lossless edit API of [`Document`].
//!
//! Every edit is planned from the current tree, written as text through the
//! serializer's own layout and scalar rules (`render.rs`) with the indentation
//! its neighbours use (`place::infer`) and the document's own line break, read
//! back through the one parser into a fragment (there is no second parser), and
//! spliced in by the syntax layer, which refuses any edit whose text would not
//! read back as the edited tree (an edited tree always agrees with its own
//! text). Fragments are always whole entries: a key with its value, an item
//! with its `-`, with the line break and indentation that separate it from its
//! neighbours. Content that ends its line (a `|` block scalar, whose text ends
//! with its line break) goes in through the syntax layer's line-end edits
//! (`replace_to_line_end`, `insert_line_after`).
//!
//! Cost: an edit reads the entries next to the one it changes
//! (found from their siblings), the tokens back to the start of a line,
//! and the value it writes; the syntax layer's cost is local too. Anchor
//! and alias bookkeeping goes through the incrementally maintained
//! index (`anchors.rs`): binary searches over one name's list, never a
//! scan of the document.
//!
//! Nothing here recurses on input: collections are walked with
//! loops, values are written by the iterative emitter and flow writer.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use oneyaml_core::{Diagnostic, Label, Path, PathSegment, Severity, Span, code};
use oneyaml_syntax::tree::{
    Fragment, GroupMark, Inserted, Node as TreeNode, SyntaxKind, SyntaxTree, TreeEditError,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::anchors::{Names, Removal};
use super::carry::{self, Carry, Expect, Rel, Want, Window};
use super::comments as cm;
use super::key::{self, AsKey, KeyArg};
use super::path_arg::{self, IntoPath};
use super::place::{self, Coll, SlotKind};
use super::render::{BlockStyle, Render, multiline, reindent};
use super::view::KeyQuery;
use super::{CommentId, Document, EditError, NodeId, NodeKind, NodeRef, ScalarStyle};
use crate::error::Error;
use crate::value::{Mapping, Value};

/// A syntax-tree handle (never in a public signature).
type TreeId = oneyaml_syntax::tree::NodeId;

/// What restricted mode protects in a document, found on its first
/// edit (a copy of the tree is a fork: the handles stay valid in it).
#[derive(Clone, Debug)]
pub(super) struct Guard {
    /// The input had encoding errors: its bytes cannot be kept, so
    /// nothing is editable.
    readonly: bool,
    /// The document has errors, or holds an error node.
    restricted: bool,
    /// The nodes the errors point at (the deepest node holding each
    /// error's start), and the document's error nodes.
    blamed: Vec<TreeId>,
    /// The nodes holding an error node as a child, found
    /// once with the error nodes (accepted edits never create, remove or
    /// move one), so an edit asks in O(log errors) whether its
    /// container holds one, never by scanning the container's children.
    error_parents: std::collections::BTreeSet<TreeId>,
}

/// Maps a refusal of the syntax layer (never re-exported); `span` is
/// the edited range, for the coded diagnostics.
/// The `Y2002` refusal of an edit that would make the text larger than
/// `Limits::max_document_size` (the one the syntax layer's limit check
/// gives, [`map_tree`]), for edits that check before allocating.
pub(super) fn too_large(span: Span) -> EditError {
    map_tree(
        TreeEditError::Limit {
            code: code::DOCUMENT_TOO_LARGE,
        },
        span,
    )
}

pub(super) fn map_tree(e: TreeEditError, span: Span) -> EditError {
    let at = Path::new();
    match e {
        TreeEditError::Stale(_) => EditError::StaleNode,
        TreeEditError::ReadOnly { .. } => EditError::ReadOnly { at },
        TreeEditError::TagContext => EditError::Unsupported {
            at,
            reason: "a tag written into a document with %TAG directives",
        },
        TreeEditError::Capacity => EditError::Rejected {
            diagnostic: Box::new(Diagnostic::error(
                code::TREE_CAPACITY_EXCEEDED,
                "the syntax tree's element or byte capacity is exhausted",
                Label::new(span, "this edit"),
            )),
        },
        TreeEditError::NotWellFormed
        | TreeEditError::Syntax { .. }
        | TreeEditError::InvalidToken { .. }
        | TreeEditError::Misplaced { .. }
        | TreeEditError::EmptyFragment => EditError::NotWellFormed { at },
        TreeEditError::Limit { code } => EditError::Rejected {
            diagnostic: Box::new(
                Diagnostic::error(
                    code,
                    "the edited document would exceed a limit",
                    Label::new(span, "this edit"),
                )
                .with_help("raise the limit in the document's Limits (limits apply to edits)"),
            ),
        },
        // The syntax layer's own invariant (its bug, reported as ours).
        TreeEditError::Internal => EditError::Internal,
        _ => {
            debug_assert!(false, "unexpected syntax refusal {e:?}");
            EditError::Internal
        }
    }
}

/// A tag that does not resolve (`Y9000` from `DocTags::try_tag`, ruled out
/// by the parser): the edit layer's internal error, never a reading
/// as untagged.
fn tag_internal(_: Error) -> EditError {
    EditError::Internal
}

fn bad() -> EditError {
    EditError::NotWellFormed { at: Path::new() }
}

/// Limitation S3: re-inserting a flow entry whose comments the
/// target layout cannot keep (documented on `Document`).
const FLOW_REINSERT_LIMIT: &str = "a flow entry with comments around it cannot be re-inserted yet";

/// Limitation S3: removing a flow entry next to comments of other
/// owners the flow layout cannot keep in place (documented on `Document`).
const FLOW_REMOVE_LIMIT: &str =
    "removing a flow entry next to comments of other owners that the flow layout cannot keep";

fn unsupported(reason: &'static str) -> EditError {
    EditError::Unsupported {
        at: Path::new(),
        reason,
    }
}

fn sp(n: usize) -> String {
    " ".repeat(n)
}

/// Whether `text` is a block scalar written with the clip indicator (no `-`
/// or `+`): its header is the last word of the first line, before a comment.
fn clip_header(text: &str) -> bool {
    let first = text.split(['\n', '\r']).next().unwrap_or_default();
    let first = first.split(" #").next().unwrap_or_default();
    first
        .split_whitespace()
        .next_back()
        .is_some_and(|w| w.starts_with(['|', '>']) && !w.contains(['-', '+']))
}

/// Whether text ends with a line break.
fn ends_break(t: &str) -> bool {
    t.ends_with(['\n', '\r'])
}

/// The block scalar `n` ends with (itself, or its last value's, at any
/// depth), if any. A loop down the last children, not recursion.
///
/// A missing last value (the empty node after `? key` with no `:`) has no
/// text, so what ends the text before it is the item before it: a block
/// scalar key ends with its line break there.
fn last_block(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = Some(n);
    while let Some(x) = at {
        if x.block_header().is_some() {
            return Some(x);
        }
        at = x.last_child().filter(|c| c.kind().is_yaml_node());
        if let Some(c) = at.filter(|c| c.span().is_empty()) {
            at = c.prev_sibling().filter(|p| p.kind().is_yaml_node());
        }
    }
    None
}

/// Whether `n` ends its line inside its own text (it ends with a block
/// scalar, whose text ends with its line break).
fn ends_line(n: TreeNode<'_>) -> bool {
    last_block(n).is_some_and(|b| ends_break(&b.text()))
}

/// Whether a refusal may go away with multi-line strings written as
/// one-line flow text (`|` "wherever the context permits").
fn retry_flat(e: &EditError) -> bool {
    matches!(e, EditError::NotWellFormed { .. })
}

/// Whether a refused insertion may go through with a re-inserted entry's
/// comment lines at its own column.
fn retry_lead(e: &EditError) -> bool {
    matches!(e, EditError::CommentReattaches { .. })
}

/// YAML 1.2.2 production 102 `ns-anchor-char`: `ns-char` minus the flow
/// indicators (`ns-char` is printable, not white space, not a line break,
/// not the byte order mark).
fn is_anchor_char(c: char) -> bool {
    let printable = matches!(c,
        '\u{21}'..='\u{7E}'
        | '\u{85}'
        | '\u{A0}'..='\u{D7FF}'
        | '\u{E000}'..='\u{FFFD}'
        | '\u{10000}'..='\u{10FFFF}');
    printable && c != '\u{FEFF}' && !matches!(c, ',' | '[' | ']' | '{' | '}')
}

/// The first YAML node of the first document of `t`.
fn root_node(t: &SyntaxTree) -> Option<TreeNode<'_>> {
    t.root()
        .children()
        .find(|c| c.kind() == SyntaxKind::Document)?
        .children()
        .find(|c| c.kind().is_yaml_node())
}

/// The siblings after the first `indicator` child of `n`, through its last
/// child.
fn after_first(n: TreeNode<'_>, indicator: SyntaxKind) -> Option<(TreeNode<'_>, TreeNode<'_>)> {
    let ind = n.children().find(|c| c.kind() == indicator)?;
    Some((ind.next_sibling()?, n.last_child()?))
}

/// The white space and comment after `coll` on its last line, when there
/// is a comment (siblings).
fn trailing_comment(coll: TreeNode<'_>) -> Option<(TreeNode<'_>, TreeNode<'_>)> {
    let first = place::next_token(coll)?;
    let mut t = first;
    loop {
        match t.kind() {
            SyntaxKind::Whitespace => {}
            SyntaxKind::Comment => {
                return (t.parent() == first.parent()).then_some((first, t));
            }
            _ => return None,
        }
        t = place::next_token(t)?;
    }
}

/// Comments that go in after a collection with an entry appended to it
/// (the parser places them after the collection).
enum Tail {
    /// Before the token after the collection (they start on the entry's
    /// line: its same-line comment, or its line break).
    Before(Fragment),
    /// As the lines after the collection (the entry ended its line).
    Lines(Fragment),
}

/// The trivia after `coll` through its last comment, when there is one
/// (siblings): comments written after an appended entry, which the
/// parser places after the collection.
fn trailing_trivia(coll: TreeNode<'_>) -> Option<(TreeNode<'_>, TreeNode<'_>)> {
    let first = place::next_token(coll)?;
    let mut last = None;
    let mut at = Some(first);
    while let Some(t) = at.filter(|t| t.kind().is_trivia()) {
        if t.kind() == SyntaxKind::Comment {
            last = Some(t);
        }
        at = t.next_sibling();
    }
    Some((first, last?))
}

/// The line break after the value of `e`'s line in a synthetic tree.
fn break_after(v: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = v.next_sibling();
    while let Some(x) = at {
        match x.kind() {
            SyntaxKind::LineBreak => return Some(x),
            k if k.is_trivia() => at = x.next_sibling(),
            _ => return None,
        }
    }
    None
}

/// The lines right after the last entry (value `last`) of a block collection
/// whose previous entry ends with a block scalar (`prev_value`) that would
/// read as content of that scalar once the entry is removed: comment lines
/// indented as far as its content, and white space wider than that.
///
/// The lines are taken from the line right after the entry; the scan stops at
/// the first line that is not empty and not as wide as the content, because
/// that line ends the scalar. `covered` is the last element of the lines
/// the entry already takes with it (the footers inside it), and whether that
/// range ends the text without a line break: the scan starts after it.
///
/// Returns the first and last element taken (siblings, with the lines
/// between them in the range), the lines as written without their breaks,
/// and the comments among them. `None` when there are none.
fn block_tail(
    prev_value: TreeNode<'_>,
    last: TreeNode<'_>,
    covered: Option<(TreeNode<'_>, bool)>,
) -> Option<(TreeId, TreeId, Vec<String>, Vec<TreeId>)> {
    let indent = last_block(prev_value)?.block_header()?.content_indent;
    if ends_line(last) {
        return None;
    }
    let mut from = match covered {
        Some((l, false)) => place::next_token(l),
        // The range ends the text of the last line; its break follows.
        Some((l, true)) => match place::next_token(l) {
            Some(b) if b.kind() == SyntaxKind::LineBreak => place::next_token(b),
            _ => return None,
        },
        None => {
            // The line after the entry's last line.
            let mut at = place::next_token(last);
            loop {
                match at {
                    Some(x) if matches!(x.kind(), SyntaxKind::Whitespace | SyntaxKind::Comment) => {
                        at = place::next_token(x);
                    }
                    Some(x) if x.kind() == SyntaxKind::LineBreak => break place::next_token(x),
                    _ => return None,
                }
            }
        }
    };
    let mut lines: Vec<String> = Vec::new();
    let mut comments: Vec<TreeId> = Vec::new();
    let mut first: Option<TreeNode<'_>> = None;
    let mut end: Option<(TreeNode<'_>, usize, usize)> = None;
    while let Some(head) = from {
        let (ws, rest) = if head.kind() == SyntaxKind::Whitespace {
            (Some(head), place::next_token(head))
        } else {
            (None, Some(head))
        };
        let ws_text = ws.map(|w| w.text().into_owned()).unwrap_or_default();
        if ws_text.contains('\t') {
            break;
        }
        let width = ws_text.chars().count();
        let Some(r) = rest else {
            break;
        };
        let (last_tok, text, reads, blank) = match r.kind() {
            SyntaxKind::LineBreak => (r, ws_text, width > indent, true),
            SyntaxKind::Comment => {
                let text = format!("{ws_text}{}", r.text());
                match place::next_token(r) {
                    Some(b) if b.kind() == SyntaxKind::LineBreak => {
                        (b, text, width >= indent, false)
                    }
                    None => (r, text, width >= indent, false),
                    _ => break,
                }
            }
            _ => break,
        };
        if reads {
            first.get_or_insert(head);
            if r.kind() == SyntaxKind::Comment {
                comments.push(r.id());
            }
            lines.push(text);
            end = Some((last_tok, lines.len(), comments.len()));
        } else if blank && first.is_some() {
            // An empty line between lines taken: kept in the range when a
            // later line is taken too.
            lines.push(text);
        } else {
            // Not directly after the entry, or a line that ends the scalar.
            break;
        }
        from = place::next_token(last_tok);
    }
    let (first, (end, n, c)) = (first?, end?);
    if first.parent() != end.parent() {
        return None;
    }
    lines.truncate(n);
    comments.truncate(c);
    Some((first.id(), end.id(), lines, comments))
}

/// `text` (which starts with the single space after an indicator) with that
/// space replaced by `gap`, the white space the document has there.
fn with_gap(text: &str, gap: Option<&str>) -> String {
    match (gap, text.strip_prefix(' ')) {
        (Some(g), Some(rest)) => format!("{g}{rest}"),
        _ => text.to_owned(),
    }
}

/// The properties (anchor, tag) a node written in place of `n` keeps, as the
/// source wrote them and in its order. The anchor always stays. The tag stays
/// only when reading the new value back under it gives exactly that value:
/// a custom tag on a string, `!!str` on a string, `!!int` on an integer and
/// so on; any other tag would change what the new value means.
fn node_props(n: TreeNode<'_>, v: &Value) -> Option<String> {
    let keep_tag = n.kind() == SyntaxKind::Scalar && keeps_tag(n, v);
    let mut out: Vec<String> = Vec::new();
    for c in n.children() {
        match c.kind() {
            SyntaxKind::Anchor => out.push(c.text().into_owned()),
            SyntaxKind::Tag if keep_tag => out.push(c.text().into_owned()),
            SyntaxKind::Whitespace | SyntaxKind::LineBreak | SyntaxKind::Comment => {}
            _ => break,
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(" "))
    }
}

/// Whether scalar `n`'s tag still describes `v` (see [`node_props`]).
fn keeps_tag(n: TreeNode<'_>, v: &Value) -> bool {
    const CORE: &str = "tag:yaml.org,2002:";
    if !matches!(
        v,
        Value::String(_) | Value::Bool(_) | Value::Null | Value::Number(_)
    ) {
        return false;
    }
    let Some(tag) = n.tag() else {
        return false;
    };
    // A document with `%TAG` directives gives tags meanings the text of an
    // edit is not read under (see `Document::synth`): the tag goes, as it
    // always did.
    let directives = n
        .ancestors()
        .find(|a| a.kind() == SyntaxKind::Document)
        .is_some_and(|d| d.children().any(|c| c.kind() == SyntaxKind::TagDirective));
    if directives {
        return false;
    }
    let t: &str = &tag.resolved;
    let core = |name: &str| t.strip_prefix(CORE) == Some(name);
    match v {
        Value::String(_) => core("str") || !t.starts_with(CORE),
        Value::Bool(_) => core("bool"),
        Value::Null => core("null"),
        Value::Number(x) => {
            if x.is_integer() {
                core("int")
            } else {
                core("float")
            }
        }
        _ => false,
    }
}

/// The style of block scalar `n` in `slot`, for writing its replacement the
/// same way; `None` where the replacement is not written after `key:` or
/// `-` on the same line, or the old scalar has no usable layout.
fn block_style(n: TreeNode<'_>, slot: SlotKind) -> Option<BlockStyle> {
    let parent = match slot {
        SlotKind::MapValue { col } | SlotKind::SeqItem { col } => col,
        _ => return None,
    };
    let h = n.block_header()?;
    let folded = n
        .last_child()
        .is_some_and(|c| c.kind() == SyntaxKind::FoldedScalar);
    Some(BlockStyle {
        folded,
        chomping: h.chomping,
        indicator: h.indent_indicator,
        content: h.content_indent,
        parent,
    })
}

/// `text` (a new value written on lines of its own) with
/// `comment` (and the white space before it) on its first line, the line
/// of its key, `-`, or properties; a compact collection item (`- - a`)
/// gets its collection below the comment (`- # c⏎  - a`).
fn header_text(kind: SlotKind, text: &str, comment: &str, nl: &str) -> Option<String> {
    match (kind, text.strip_prefix(' ')) {
        (SlotKind::SeqItem { col }, Some(rest))
            if !rest.starts_with(['&', '!', '|', '>', '\n', '\r']) =>
        {
            Some(format!("{comment}{nl}{}{rest}", sp(col.saturating_add(2))))
        }
        _ => with_header_comment(text, comment),
    }
}

/// `text` (a block scalar's text: its header, then its lines) with
/// `comment` (the white space and comment that followed the value it
/// replaces) at the end of its first line, the header line.
fn with_header_comment(text: &str, comment: &str) -> Option<String> {
    let i = text.find(['\n', '\r'])?;
    Some(format!("{}{comment}{}", text.get(..i)?, text.get(i..)?))
}

/// An open multi-step edit (all or nothing): the syntax
/// tree's edit group, and where this edit's anchor-index changes start in
/// [`Document`]'s log.
struct Txn {
    mark: GroupMark,
    ix: usize,
    /// Where this edit's explicit comment-owner changes start.
    explicit: usize,
    /// Where this edit's lookup-index changes start.
    lookups: super::lookup::Mark,
}

/// An unterminated last line of spaces at the end of the text is one more line
/// of the kept (`|+`) block scalar right before it, its **tail reader**, but
/// lies outside that scalar's text. An edit changes which scalar reads it (or
/// whether one does) without touching the line, so the check is on values, once
/// per edit, at the end of its outermost group: the reader before the edit,
/// when the edit left it in place, keeps its value; a scalar the edit wrote
/// reads the line only when it is meant to (a re-inserted [`Removed`] that read
/// it where it was taken from, a node rewritten with its meaning kept); every
/// other value is the one its own text has. The syntax layer has no such rule:
/// a tree's values follow its text, so it always agrees with its text.
///
/// No value is computed. A kept scalar's value has the
/// line exactly while it is the tail reader, and an edit never rewrites a
/// value in place (a new value is a new node), so "the reader before keeps
/// its value" is "it is still the reader": O(depth + the tail's tokens)
/// per edit, whatever the scalar's size (reading its value would cost
/// O(scalar) per edit).
#[derive(Clone, Debug, Default)]
pub(super) struct TailGuard {
    /// The tail reader when the outermost group began.
    before: Option<TreeId>,
    /// Nodes the group wrote, and whether a tail reader inside them is
    /// meant (re-insertions and meaning-keeping rewrites only).
    intents: Vec<(TreeId, bool)>,
}

/// A change of the anchor index made inside an open edit group, to be
/// reverted with it: the siblings `first..=last` left or joined
/// the tree (only those holding anchors or aliases are logged).
#[derive(Clone, Copy, Debug)]
pub(super) enum IxOp {
    Removed(TreeId, TreeId),
    Inserted(TreeId, TreeId),
}

/// How an insertion is placed.
#[derive(Clone, Copy)]
pub(super) enum Put {
    After(TreeId),
    Before(TreeId),
    Append(TreeId),
    /// As the lines after the line `TreeId` ends.
    LineAfter(TreeId),
}

/// Where new content goes in a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Where {
    /// After the last entry.
    Append,
    /// Before the entry whose value (or item) is this node.
    Before(TreeId),
}

/// What an insertion writes.
#[derive(Clone, Copy)]
enum Payload<'a> {
    /// A new value (with its key, for mappings).
    Value(&'a Value),
    /// A new mapping entry whose key is not a string (an
    /// integer, `bool`, null, or tagged scalar key), and its value.
    Entry(&'a Value, &'a Value),
    /// A removed entry or item, written back as it was.
    Removed(&'a Removed),
    /// An alias `*name` to the anchored node.
    Alias(&'a str, TreeId),
    /// A new mapping entry whose key is not a string and whose
    /// value is an alias `*name` to the anchored node.
    AliasEntry(&'a Value, &'a str, TreeId),
    /// A merge key: `<<: *a`, or `<<: [*a, *b]`, aliases to
    /// the anchored nodes.
    Merge(&'a [(String, TreeId)]),
    /// A null written as its neighbours write theirs (also for
    /// `get_path_or_create`): `""` (an empty value, `k:`), `~`, `null`,
    /// `Null`, or `NULL`; block collections only.
    Null(&'a str),
}

/// A node removed from a [`Document`]: its entry (or item) as
/// written, with **exactly the comments the
/// [rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/comment-attachment.md) gives it** ("nothing destroyed silently"): its
/// leading comment lines, its same-line comment, the comments inside it,
/// and the footers of the collections inside it, with the blank lines
/// they carry (rule 8; not those above a collection's first entry, which
/// are the collection's). Every other comment stays with its owner.
/// Re-insert it into the **same** document to move it
/// ([`NodeMut::insert_removed`]), or drop it:
///
/// ```
/// use oneyaml::Document;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let src = "dev:\n  debug: true  # noisy\n  port: 80\nprod:\n  port: 443\n";
/// let mut doc = Document::parse(src)?;
/// let debug = doc.get_mut("dev")?.remove("debug")?;
/// doc.get_mut("prod")?.insert_removed(debug)?;
/// assert_eq!(
///     doc.to_string(),
///     "dev:\n  port: 80\nprod:\n  port: 443\n  debug: true  # noisy\n"
/// );
/// # Ok(())
/// # }
/// ```
#[must_use = "a removed node is lost unless it is re-inserted"]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    doc: u64,
    /// The entry (`key: value`) or item (its value, without `-`), with its
    /// same-line comment.
    text: String,
    /// The column of the text's first character.
    col: usize,
    /// The column where the entry starts (its key, `?`, or `-`): the lines
    /// of `lead` and `tail` are indented from it.
    entry_col: usize,
    /// The text was written in a flow collection.
    flow: bool,
    /// A mapping entry's key (its scalar content), `None` for an item.
    key: Option<String>,
    /// The key is a merge key (`<<` with `merge_keys` on): it goes back
    /// only into a mapping without one (a second is a duplicate key).
    merge: bool,
    /// The key's resolved value (when it resolves): it goes back only into
    /// a mapping without an equal key (`~`, `null` and `` are one key).
    key_value: Option<Value>,
    kind: NodeKind,
    /// Whole lines above the entry that went with it, as written (without
    /// line breaks): the blank lines it carries and its leading comment
    /// lines (block context).
    lead: Vec<String>,
    /// Whole lines after the entry's last line that went with it, as
    /// written: the footers of the collections inside it (block context).
    tail: Vec<String>,
    /// A flow entry's comments outside its text (its leading and trailing
    /// comments): re-inserting them is a later phase.
    loose: bool,
    /// A block sequence item: what separates its `-` from its value, as
    /// written (`" "`, a tab, a line break and indentation).
    head: Option<String>,
    /// A flow entry: the separator after it (`, `, `,⏎  `), when an entry
    /// followed, or before it (from the previous entry), when it was the
    /// last; written back as they were (a re-insertion at the
    /// same place gives the bytes back).
    sep_after: Option<String>,
    sep_before: Option<String>,
    /// A collection's first entry: the blank lines above the next entry,
    /// which its removal left at the top of the collection (with the
    /// collection's own); re-inserting it first gives them back to that
    /// entry (the bytes come back).
    next_blank: usize,
    /// A root collection's first entry: the next entry's lines
    /// above it as they became (without the blank lines after their first
    /// comment line, comment rule 2) and as they were; re-inserting the entry
    /// first gives them back as they were.
    next_lead: Option<(String, String)>,
    /// A flow entry removed as whole lines of its own, with its comments
    /// or its text after the previous value (a last entry):
    /// as written, and the mode (0/1: lines without/with its `,`; 2: the
    /// text right after the previous value, its `,` first).
    flow_lines: Option<(String, u8)>,
    /// Every comment taken, in order (with its handle before the
    /// removal, its owner relative to the entry, and its explicit owner).
    comments: Vec<String>,
    ids: Vec<CommentId>,
    owners: Vec<Rel>,
    explicit: Vec<Option<Rel>>,
    /// Comments left in the document whose explicit owner is inside the
    /// entry: that owner follows the entry back when it is re-inserted.
    outside: Vec<(TreeId, Rel)>,
    /// Anchors the text defines (names).
    anchors: Vec<String>,
    /// Aliases in the text whose anchor lies outside it: (name, the
    /// anchored node): a move keeps each alias's target.
    external: Vec<(String, TreeId)>,
    /// Aliases outside that referred to anchors of the text (a move takes
    /// them along; a plain removal refuses them).
    referenced: Vec<TreeId>,
    /// `referenced` as runs: (anchor name, how many), in order; each run
    /// is a stretch of that name's alias list, in document order.
    referenced_runs: Vec<(String, usize)>,
    /// Taken by a move (`op_move`), re-inserted before any other edit:
    /// `referenced` is still exactly those runs of live aliases, so the
    /// check that a moved alias still follows its anchor is O(log k).
    moving: bool,
    /// The entry held the tail reader where it was
    /// taken from (its kept scalar read the text's final line of spaces):
    /// re-inserted, it must read it again (see [`TailGuard`]).
    reads_tail: bool,
}

impl Removed {
    /// The removed text: `key: value` for a mapping entry, the value for a
    /// sequence item, exactly as written, with its same-line comment. The
    /// comment lines around it that went with it are in
    /// [`Removed::comments`].
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Every comment the removal took ("nothing destroyed
    /// silently"), in document order, `#` included: exactly those the
    /// rulebook gives the entry — its leading comment lines, its same-line
    /// comment, those inside [`Removed::text`], and the footers of the
    /// collections inside it. Re-inserting the entry writes them back
    /// around it, with the blank lines they carried.
    #[must_use]
    pub fn comments(&self) -> &[String] {
        &self.comments
    }

    /// The handles the comments of [`Removed::comments`] had in the
    /// document (same order). They name nothing any more (the comments
    /// left); a re-insertion gives the comments new handles.
    #[must_use]
    pub fn comment_ids(&self) -> &[CommentId] {
        &self.ids
    }

    /// The key of a removed mapping entry (its scalar content).
    #[must_use]
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// What the removed value was.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        self.kind
    }
}

/// A refused re-insertion of a [`Removed`] ("nothing destroyed
/// silently"): the reason, and the removed content with its
/// comments, handed back so the caller can put it elsewhere (or keep it).
/// The document is unchanged.
///
/// ```
/// use oneyaml::{Document, EditError, path};
///
/// let mut d = Document::parse("a: 1\nb: [2]\n").unwrap();
/// let r = d.remove_path(&path!["a"]).unwrap();
/// let err = d.get_mut("b").unwrap().insert_removed(r).unwrap_err();
/// assert!(matches!(err.error(), EditError::Unsupported { .. }));
/// let r = err.into_removed(); // not lost
/// d.root_mut().unwrap().insert_removed(r).unwrap();
/// assert_eq!(d.to_string(), "b: [2]\na: 1\n");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReinsertError {
    error: EditError,
    removed: Box<Removed>,
}

impl ReinsertError {
    /// Why the re-insertion was refused.
    #[must_use]
    pub fn error(&self) -> &EditError {
        &self.error
    }

    /// The removed content, as it was before the attempt.
    pub fn removed(&self) -> &Removed {
        &self.removed
    }

    /// Takes the removed content back.
    #[must_use = "a removed node is lost unless it is re-inserted"]
    pub fn into_removed(self) -> Removed {
        *self.removed
    }

    /// The reason (dropping the removed content).
    #[must_use]
    pub fn into_error(self) -> EditError {
        self.error
    }

    /// Both parts.
    pub fn into_parts(self) -> (EditError, Removed) {
        (self.error, *self.removed)
    }
}

impl core::fmt::Display for ReinsertError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the removed node was not re-inserted: {}", self.error)
    }
}

impl std::error::Error for ReinsertError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Serde → [`Value`] for an edit (moving values between serde and
/// the edit tree"): the serializer's own builder, bounded by
/// `max_typed_depth` (`Y2007`); the result is bounded by `max_depth`
/// (`Y2005`).
fn to_value<T: ?Sized + Serialize>(v: &T, doc: &Document) -> Result<Value, EditError> {
    let limits = doc.settings().get_limits();
    let value = crate::ser::build::build(
        v,
        crate::ser::build::ToValue::root(limits.get_max_typed_depth()),
    )
    .map_err(|e| EditError::rejected(&e))?;
    value
        .check_depth(limits)
        .map_err(|e| EditError::rejected(&e.into_error()))?;
    Ok(value)
}

/// A nested value for the path segments `rest`, ending in `leaf` (what
/// `set_path` creates below the deepest existing node). An index segment
/// must be 0 (a new sequence).
fn nest(rest: &[PathSegment], leaf: Value, at: &Path) -> Result<Value, EditError> {
    let mut v = leaf;
    // Built from the innermost segment out, in a loop.
    for seg in rest.iter().rev() {
        v = match seg {
            PathSegment::Key(k) => {
                let mut m = Mapping::new();
                m.insert(Value::from(k.as_str()), v);
                Value::Mapping(m)
            }
            PathSegment::Index(0) => Value::Sequence(vec![v].into()),
            PathSegment::Index(i) => {
                return Err(EditError::IndexOutOfBounds {
                    at: at.clone(),
                    index: *i,
                    len: 0,
                });
            }
            _ => return Err(unsupported("a path segment of an unknown kind")),
        };
    }
    Ok(v)
}

/// The span of the siblings `a..=b` of `t` (for diagnostics).
fn span_of(t: &SyntaxTree, a: TreeId, b: TreeId) -> Span {
    match (t.get(a), t.get(b)) {
        (Ok(a), Ok(b)) => Span::new(a.span().start, b.span().end.max(a.span().start)),
        (Ok(a), _) => a.span(),
        _ => Span::empty(0),
    }
}

impl Document {
    // ============================================================ plumbing

    pub(super) fn tree(&self) -> &SyntaxTree {
        &self.shared.tree
    }

    pub(super) fn tree_mut(&mut self) -> Result<&mut SyntaxTree, EditError> {
        Arc::get_mut(&mut self.shared)
            .map(|s| &mut s.tree)
            .ok_or(EditError::Internal)
    }

    /// The document's line break (2026-09-28).
    pub(super) fn nl(&self) -> &'static str {
        self.shared.nl
    }

    /// Before an edit: the document owns its tree (a tree shared with a
    /// clone or a stream snapshot is copied, keeping every handle valid),
    /// and restricted mode is known for this
    /// tree.
    pub(super) fn prepare(&mut self) -> Result<(), EditError> {
        if !self.tree().in_group() {
            *self.error_neighbour.get_mut() = false;
        }
        if Arc::get_mut(&mut self.shared).is_none() {
            // A fork keeps every handle (the
            // document node, the guard's blamed nodes) valid in the copy.
            self.shared = Arc::new(self.shared.copy());
        }
        if self.guard.is_none() {
            self.guard = Some(self.compute_guard());
        }
        match &self.guard {
            Some(g) if g.readonly => Err(EditError::ReadOnly { at: Path::new() }),
            Some(_) => Ok(()),
            None => Err(EditError::Internal),
        }
    }

    fn compute_guard(&self) -> Guard {
        let s = &self.shared;
        let readonly = s.decoded > 0 || !s.origin.invalid.is_empty();
        // The complete errors, never the capped
        // list, so restricted mode does not depend on `max_diagnostics`.
        let errors = self.own_errors();
        if let Some(i) = self.index {
            s.record_count(i);
        }
        // The error nodes of this document's subtree, whoever
        // owns their diagnostic (an unclosed `[` cut off by the next `---`
        // is reported by the next document). O(document), once.
        let top = if self.index.is_none() {
            Some(self.tree().root())
        } else {
            self.doc_node()
        };
        let error_nodes: Vec<TreeId> = if self.tree().has_errors() {
            top.into_iter()
                .flat_map(TreeNode::descendants)
                .filter(|n| n.kind() == SyntaxKind::Error)
                .map(|n| n.id())
                .collect()
        } else {
            Vec::new()
        };
        let restricted = !error_nodes.is_empty() || !errors.is_empty();
        let error_parents = error_nodes
            .iter()
            .filter_map(|e| self.tree().get(*e).ok()?.parent().map(|p| p.id()))
            .collect();
        let mut blamed = error_nodes;
        if restricted {
            let top = self.doc_node();
            // Only the errors whose element lies
            // in this document's own region are blamed on its nodes. A
            // document that only reports its neighbour's error (an
            // unclosed `[` cut off by its `---`: the error node is the
            // previous document's, which that document's guard protects)
            // blames nothing: it stays restricted, never wholly read-only.
            // The neighbour's: attached to an element of the previous
            // document (O(log d + its diagnostics)).
            let foreign: Vec<(oneyaml_core::Code, oneyaml_core::Span)> = match (self.index, top) {
                (Some(_), Some(t)) => {
                    let mut p = t.prev_sibling();
                    while let Some(x) = p.filter(|x| x.kind() != SyntaxKind::Document) {
                        p = x.prev_sibling();
                    }
                    p.map(|prev| {
                        self.tree()
                            .diagnostics_within(prev)
                            .filter(|(n, d)| n.is_some() && d.severity == Severity::Error)
                            .map(|(_, d)| (d.code, d.primary.span))
                            .collect()
                    })
                    .unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let own = errors.iter().filter(|e| !foreign.contains(e));
            for (_, span) in own {
                let at = span.start;
                let mut node = top;
                // Descend, one level per step.
                while let Some(n) = node {
                    let deeper = n.children().find(|c| {
                        (place::is_item(*c))
                            && c.span().start <= at
                            && (at < c.span().end || c.span().is_empty())
                    });
                    match deeper {
                        Some(c) => node = Some(c),
                        None => break,
                    }
                }
                if let Some(n) = node {
                    blamed.push(n.id());
                }
            }
        }
        Guard {
            readonly,
            restricted,
            blamed,
            error_parents,
        }
    }

    /// After an accepted edit.
    pub(super) fn commit(&mut self) {
        if let Some(s) = Arc::get_mut(&mut self.shared) {
            s.after_edit();
            if let Some(i) = self.index {
                s.mark_edited(i);
            }
        }
        // Explicit comment owners follow the tree (O(log k)).
        self.prune_explicit();
    }

    /// Restricted mode: an edit touching `nodes`, in `container`, is
    /// refused when they hold or overlap an error region, or when the
    /// container holds one (adjacent). Only restricted documents pay for
    /// it: O(container + nodes' diagnostics + errors).
    pub(super) fn check_region(
        &self,
        nodes: &[TreeNode<'_>],
        container: Option<TreeNode<'_>>,
    ) -> Result<(), EditError> {
        let g = self.guard.as_ref().ok_or(EditError::Internal)?;
        if !g.restricted {
            return Ok(());
        }
        let ro = || EditError::ReadOnly { at: Path::new() };
        if container.is_some_and(|c| g.error_parents.contains(&c.id())) {
            return Err(ro());
        }
        let tree = self.tree();
        // An error region right after (or
        // before) a node can be about that node's shape (`[[a, x⏎ ab]: 1]`:
        // Y0007, the implicit key `[a, x⏎ ab]` spans two lines, the `: 1`
        // is the error region). An edit inside such a node can change
        // whether the error exists (setting `x⏎ ab` to one line), which the
        // syntax layer's local check does not see. Refusing every such edit
        // would be too broad: the edit goes ahead, and
        // at the end of its group the document's errors are checked against
        // its text; only an edit that changes them is refused. O(depth)
        // here.
        let beside_error = |x: TreeNode<'_>| {
            let mut p = x.prev_sibling();
            while let Some(s) = p.filter(|s| s.kind().is_trivia()) {
                p = s.prev_sibling();
            }
            let mut q = x.next_sibling();
            while let Some(s) = q.filter(|s| s.kind().is_trivia()) {
                q = s.next_sibling();
            }
            [p, q]
                .into_iter()
                .flatten()
                .any(|s| s.kind() == SyntaxKind::Error)
        };
        for n in nodes {
            let mut at = Some(*n);
            while let Some(a) = at.filter(|a| a.kind() != SyntaxKind::Document) {
                if beside_error(a) {
                    self.error_neighbour
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
                at = a.parent();
            }
            if n.kind() == SyntaxKind::Error
                || tree
                    .diagnostics_within(*n)
                    .any(|(_, d)| d.severity == Severity::Error)
            {
                return Err(ro());
            }
            let s = n.span();
            for b in &g.blamed {
                if let Ok(b) = tree.get(*b) {
                    let t = b.span();
                    if s.start <= t.end && t.start <= s.end {
                        return Err(ro());
                    }
                }
            }
        }
        Ok(())
    }

    /// An edit inside a collection that holds an
    /// error region is refused (as [`Document::check_region`] would)
    /// before any of its entries is located: in recovered input that can
    /// mean counting the collection's items (`place::is_key`'s fallback),
    /// O(width) for a refusal. O(log errors).
    fn refuse_error_container(&self, coll: TreeNode<'_>) -> Result<(), EditError> {
        let g = self.guard.as_ref().ok_or(EditError::Internal)?;
        if g.restricted && g.error_parents.contains(&coll.id()) {
            return Err(EditError::ReadOnly { at: Path::new() });
        }
        Ok(())
    }

    /// Whether `n` is a key of mapping `map` (`place::is_key`), its
    /// fallback answered from the lookup index of a wide mapping (the
    /// parity of `n`'s position, O(log width)).
    pub(super) fn key_in(&self, map: TreeNode<'_>, n: TreeNode<'_>) -> bool {
        place::local_is_key(map, n)
            .or_else(|| self.item_parity(map, n))
            .unwrap_or_else(|| place::is_key(map, n))
    }

    /// Parses synthetic text under the tree's settings (the one
    /// parser); text that does not parse cleanly cannot be inserted.
    /// The document's size limit is the syntax layer's to check, on the
    /// whole edited text: a fragment is read without it.
    pub(super) fn synth(&self, text: &str) -> Result<SyntaxTree, EditError> {
        let settings = self.tree().settings();
        let limits = settings.get_limits().clone().max_document_size(usize::MAX);
        let t = SyntaxTree::parse_with(text, &settings.clone().limits(limits));
        if t.has_errors() {
            // A known limitation: text using a `%TAG` handle of
            // the document reads without its directives here; that is the
            // documented tag limitation, not malformed text.
            let handle = t
                .diagnostics()
                .any(|d| d.code == code::UNDEFINED_TAG_HANDLE);
            if handle {
                return Err(map_tree(TreeEditError::TagContext, Span::empty(0)));
            }
            // New text past a limit (a scalar longer than
            // `max_scalar_len`) is refused with that limit's code, not as
            // text that would not read back.
            let limit = t
                .diagnostics()
                .find(|d| d.code.range() == oneyaml_core::code::CodeRange::Limits);
            if let Some(d) = limit {
                return Err(map_tree(
                    TreeEditError::Limit { code: d.code },
                    Span::empty(0),
                ));
            }
            return Err(bad());
        }
        Ok(t)
    }

    fn extract(t: &SyntaxTree, a: TreeNode<'_>, b: TreeNode<'_>) -> Result<Fragment, EditError> {
        t.extract(a.id(), b.id())
            .map_err(|e| map_tree(e, Span::new(a.span().start, b.span().end)))
    }

    /// The siblings `a..=b` of a synthetic tree, ending with the line break
    /// that ends their line (theirs, or the next token's).
    fn line_fragment(
        t: &SyntaxTree,
        a: TreeNode<'_>,
        b: TreeNode<'_>,
    ) -> Result<Fragment, EditError> {
        Self::line_fragment_in(t, a, b, false)
    }

    /// [`Document::line_fragment`]; `open`: the text may end without a line
    /// break (the document does).
    fn line_fragment_in(
        t: &SyntaxTree,
        a: TreeNode<'_>,
        b: TreeNode<'_>,
        open: bool,
    ) -> Result<Fragment, EditError> {
        let f = Self::extract(t, a, b)?;
        if ends_break(f.text()) || (open && place::next_token(b).is_none()) {
            return Ok(f);
        }
        let lb = place::next_token(b)
            .filter(|x| x.kind() == SyntaxKind::LineBreak)
            .ok_or_else(bad)?;
        f.concat(Self::extract(t, lb, lb)?)
            .map_err(|e| map_tree(e, Span::empty(0)))
    }

    /// The renderer for new content in `context` (style inference);
    /// `flat`: multi-line strings as one-line flow text.
    fn render(&self, context: Option<TreeNode<'_>>, flat: bool) -> Result<Render, EditError> {
        let settings = self.settings();
        let emit = match context {
            Some(c) => place::infer(c, settings.get_emit()),
            None => settings.get_emit().clone(),
        };
        let r = Render::new(settings, &emit, self.nl())?;
        Ok(if flat { r.one_line() } else { r })
    }

    /// The live syntax node of a handle of this document.
    pub(super) fn tree_node(&self, id: NodeId) -> Result<TreeNode<'_>, EditError> {
        self.node(id).map(NodeRef::tree_node)
    }

    // ============================================= tree edits + the index

    /// The anchor index, when built, with the tree (for maintenance).
    fn index_parts(&mut self) -> Option<(&SyntaxTree, &mut super::anchors::Index)> {
        let s = Arc::get_mut(&mut self.shared)?;
        let tree = &s.tree;
        s.anchors.get_mut().map(|ix| (tree, ix))
    }

    /// Before an edit removing the elements holding `names`, where
    /// they sit in the index (when it is built); applied by
    /// [`Document::index_removed`] once the edit is accepted.
    fn index_plan(&self, names: &Names<'_>) -> Option<Removal> {
        if names.is_empty() {
            return None;
        }
        let ix = self.shared.anchors.get()?;
        Some(ix.plan_removal(self.tree(), names))
    }

    /// [`Document::index_plan`] for the siblings `a..=b`: their names are
    /// read only when the index is built (an edit of a
    /// document whose index was never needed walks nothing).
    fn index_plan_of(&self, a: TreeNode<'_>, b: TreeNode<'_>) -> Option<Removal> {
        self.shared.anchors.get()?;
        self.index_plan(&Names::of(a, b))
    }

    /// The index after the edit `plan` was read for (one splice per
    /// touched name).
    fn index_removed(&mut self, plan: Option<Removal>) {
        let Some(plan) = plan else { return };
        if let Some((_, ix)) = self.index_parts() {
            ix.apply_removal(plan);
        }
        self.index_check();
    }

    /// As in `lookup.rs`: an index whose list broke
    /// an invariant (a bug) is dropped, to be rebuilt from the tree on
    /// next use, never read; a debug build then stops on the bug.
    pub(super) fn index_check(&mut self) {
        let poisoned = self.shared.anchors.get().is_some_and(|ix| ix.poisoned());
        if poisoned {
            if let Some(s) = Arc::get_mut(&mut self.shared) {
                s.anchors = OnceLock::new();
            }
            debug_assert!(false, "the anchor index broke an invariant");
        }
    }

    /// The index after the siblings `first..=last` went in; `true`
    /// when it changed.
    fn index_inserted(&mut self, first: TreeId, last: TreeId) -> bool {
        let Some((tree, ix)) = self.index_parts() else {
            return false;
        };
        let names = match (tree.get(first), tree.get(last)) {
            (Ok(a), Ok(b)) => Names::of(a, b),
            _ => return false,
        };
        if names.is_empty() {
            return false;
        }
        ix.inserted(tree, &names);
        self.index_check();
        true
    }

    /// Removes the siblings `a..=b`, keeping the index (O(removed)).
    pub(super) fn t_remove(&mut self, a: TreeId, b: TreeId) -> Result<(), EditError> {
        let (plan, span) = {
            let t = self.tree();
            let na = t.get(a).map_err(|_| EditError::StaleNode)?;
            let nb = t.get(b).map_err(|_| EditError::StaleNode)?;
            (self.index_plan_of(na, nb), span_of(t, a, b))
        };
        let lookup = self.lookup_before(a, b);
        // (What was removed is not kept: the edit group undoes it.)
        let _removed = self
            .tree_mut()?
            .remove_range(a, b)
            .map_err(|e| map_tree(e, span))?;
        self.log_ix(plan.is_some(), IxOp::Removed(a, b));
        self.index_removed(plan);
        self.lookup_removed(lookup);
        Ok(())
    }

    /// Inside an edit group, an index change to revert with it.
    fn log_ix(&mut self, touched: bool, op: IxOp) {
        if touched && self.tree().in_group() {
            self.ix_log.push(op);
        }
    }

    /// Inserts `frag` as `put` says, keeping the index.
    pub(super) fn t_insert(&mut self, put: Put, frag: Fragment) -> Result<Inserted, EditError> {
        let span = match put {
            Put::After(x) | Put::Before(x) | Put::LineAfter(x) | Put::Append(x) => {
                self.tree().get(x).map_or(Span::empty(0), TreeNode::span)
            }
        };
        let tree = self.tree_mut()?;
        let r = match put {
            Put::After(x) => tree.insert_after(x, frag),
            Put::Before(x) => tree.insert_before(x, frag),
            Put::Append(p) => tree.append_child(p, frag),
            Put::LineAfter(x) => tree.insert_line_after(x, frag),
        }
        .map_err(|e| map_tree(e, span))?;
        let touched = self.index_inserted(r.first, r.last);
        self.log_ix(touched, IxOp::Inserted(r.first, r.last));
        self.lookup_inserted(r.first, r.last);
        Ok(r)
    }

    /// Replaces the siblings `a..=b` (through their line's end when
    /// `line`), keeping the index. Returns the inserted range.
    pub(super) fn t_replace(
        &mut self,
        a: TreeId,
        b: TreeId,
        frag: Fragment,
        line: bool,
    ) -> Result<Inserted, EditError> {
        let (plan, span) = {
            let t = self.tree();
            let na = t.get(a).map_err(|_| EditError::StaleNode)?;
            let nb = t.get(b).map_err(|_| EditError::StaleNode)?;
            (self.index_plan_of(na, nb), span_of(t, a, b))
        };
        let lookup = self.lookup_before(a, b);
        let tree = self.tree_mut()?;
        let r = if line {
            tree.replace_to_line_end(a, b, frag)
        } else {
            tree.replace_range(a, b, frag)
        }
        .map_err(|e| map_tree(e, span))?;
        self.log_ix(plan.is_some(), IxOp::Removed(a, b));
        self.index_removed(plan);
        let touched = self.index_inserted(r.inserted.first, r.inserted.last);
        self.log_ix(touched, IxOp::Inserted(r.inserted.first, r.inserted.last));
        self.lookup_removed(lookup);
        self.lookup_inserted(r.inserted.first, r.inserted.last);
        Ok(r.inserted)
    }

    /// Removes the siblings `a..=b` through their line's end, putting
    /// `trivia` where the parser puts it (the syntax layer's
    /// `remove_to_line_end`), keeping the index.
    pub(super) fn t_remove_to_line_end(
        &mut self,
        a: TreeId,
        b: TreeId,
        trivia: Option<Fragment>,
    ) -> Result<(), EditError> {
        let (plan, span) = {
            let t = self.tree();
            let na = t.get(a).map_err(|_| EditError::StaleNode)?;
            let nb = t.get(b).map_err(|_| EditError::StaleNode)?;
            (self.index_plan_of(na, nb), span_of(t, a, b))
        };
        let lookup = self.lookup_before(a, b);
        let _removed = self
            .tree_mut()?
            .remove_to_line_end(a, b, trivia)
            .map_err(|e| map_tree(e, span))?;
        self.log_ix(plan.is_some(), IxOp::Removed(a, b));
        self.index_removed(plan);
        self.lookup_removed(lookup);
        Ok(())
    }

    /// Runs the steps of one edit as a whole (all or
    /// nothing): the syntax tree's edit group records every accepted step,
    /// and when `f` fails, all of them are undone exactly (the tree, its
    /// text, every handle, and the anchor index are as before), whatever
    /// the steps did around them (line breaks moved across levels
    /// included). Groups nest: an inner edit's failure undoes its own steps
    /// only.
    pub(super) fn atomic<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, EditError>,
    ) -> Result<T, EditError> {
        // The outermost group checks the final space line's
        // reader once, at its end (never per step).
        let outer = !self.tree().in_group();
        if outer {
            let before = self.tail_reader();
            self.tail = TailGuard {
                before,
                intents: Vec::new(),
            };
        }
        let txn = Txn {
            ix: self.ix_log.len(),
            explicit: self.explicit_log_len(),
            lookups: self.lookup_mark(),
            mark: self.tree_mut()?.begin_group(),
        };
        match f(self) {
            Ok(v) => {
                if outer {
                    let neighbour = std::mem::take(self.error_neighbour.get_mut());
                    let checked = self.check_tail().and_then(|()| {
                        if neighbour && !self.errors_read_back() {
                            Err(EditError::ReadOnly { at: Path::new() })
                        } else {
                            Ok(())
                        }
                    });
                    self.tail = TailGuard::default();
                    if let Err(e) = checked {
                        self.rollback(txn)?;
                        return Err(e);
                    }
                }
                let r = self.tree_mut()?.end_group(txn.mark);
                if !self.tree().in_group() {
                    self.ix_log.clear();
                    self.lookup_settle();
                }
                self.settle_explicit_log();
                r.map_err(|_| EditError::Internal)?;
                Ok(v)
            }
            Err(e) => {
                if outer {
                    self.tail = TailGuard::default();
                    *self.error_neighbour.get_mut() = false;
                }
                self.rollback(txn)?;
                Err(e)
            }
        }
    }

    /// The tail reader (see [`TailGuard`]): the scalar whose kept block
    /// text comes last, followed only by white space of spaces (at least
    /// one) to the end of the text. O(depth + the tokens after it);
    /// the white space is read only after a kept scalar (then at most its
    /// content indentation long: a longer line would be its content).
    pub(super) fn tail_reader(&self) -> Option<TreeId> {
        let tree = self.tree();
        let last = cm::last_token(tree.root());
        // First the token before the white space (no text read, no
        // allocation), then, after a kept scalar only, the white
        // space itself.
        let mut spaces = false;
        let mut at = last;
        let body = loop {
            let t = at?;
            match t.kind() {
                SyntaxKind::LiteralScalar | SyntaxKind::FoldedScalar => break t,
                _ if t.span().is_empty() => {}
                SyntaxKind::Whitespace => spaces = true,
                _ => return None,
            }
            at = place::prev_token(t);
        };
        let s = body.parent().filter(|s| s.kind() == SyntaxKind::Scalar)?;
        let keep = s
            .children()
            .any(|h| h.kind() == SyntaxKind::BlockScalarHeader && h.text().contains('+'));
        if !keep || !spaces {
            return None;
        }
        let mut at = last;
        while let Some(t) = at.filter(|t| *t != body) {
            if !t.text().bytes().all(|c| c == b' ') {
                return None;
            }
            at = place::prev_token(t);
        }
        Some(s.id())
    }

    /// This document's syntax diagnostics
    /// are those a parse of its own text gives (codes, severities, spans
    /// from its start). O(document), only after an edit next to an error
    /// region (see [`Document::check_region`]); a document whose text does
    /// not read the same alone (an error another document reports) fails
    /// it, which refuses the edit as before.
    fn errors_read_back(&self) -> bool {
        let tree = self.tree();
        let Some(doc) = self.doc.and_then(|d| tree.get(d).ok()) else {
            return false;
        };
        let base = doc.span().start;
        let key = |d: &oneyaml_core::Diagnostic, base: usize| {
            (
                d.code,
                d.severity,
                d.primary.span.start.saturating_sub(base),
                d.primary.span.end.saturating_sub(base),
            )
        };
        let mut ours: Vec<_> = tree
            .diagnostics_within(doc)
            .map(|(_, d)| key(&d, base))
            .collect();
        let again = SyntaxTree::parse_with(doc.text().into_owned(), tree.settings());
        let mut theirs: Vec<_> = again.diagnostics().map(|d| key(&d, 0)).collect();
        ours.sort_unstable_by_key(|k| (k.2, k.3, k.0.get()));
        theirs.sort_unstable_by_key(|k| (k.2, k.3, k.0.get()));
        ours == theirs
    }

    /// After a removal, a kept scalar the removal
    /// left right before the text's final line of spaces (`z: 1` removed
    /// from `k: |+⏎  x⏎⏎z: 1⏎␠`) would read that line as one more line of
    /// its value; the line goes instead (white space that means nothing
    /// else), so every value stays. O(depth + the line's tokens).
    fn keep_tail_values(&mut self) -> Result<(), EditError> {
        let Some(r) = self.tail_reader() else {
            return Ok(());
        };
        if self.tail.before == Some(r) {
            return Ok(());
        }
        let spaces: Vec<TreeId> = {
            let tree = self.tree();
            let mut out = Vec::new();
            let mut at = cm::last_token(tree.root());
            while let Some(t) =
                at.filter(|t| t.kind() == SyntaxKind::Whitespace || t.span().is_empty())
            {
                if !t.span().is_empty() {
                    out.push(t.id());
                }
                at = place::prev_token(t);
            }
            out
        };
        for ws in spaces {
            self.t_remove(ws, ws)?;
        }
        Ok(())
    }

    /// Whether node `id` is `r` or holds it.
    fn holds(&self, id: TreeId, r: TreeId) -> bool {
        self.tree()
            .get(r)
            .is_ok_and(|n| n.id() == id || n.ancestors().any(|a| a.id() == id))
    }

    /// Records that the group wrote node `id`, and whether it is meant to
    /// hold the tail reader (see [`TailGuard`]).
    pub(super) fn tail_intend(&mut self, id: TreeId, reads: bool) {
        self.tail.intents.push((id, reads));
    }

    /// Whether node `id` holds the tail reader recorded when the outermost
    /// group began (a rewrite keeping its meaning keeps that).
    pub(super) fn held_tail(&self, id: TreeId) -> bool {
        self.tail.before.is_some_and(|r| self.holds(id, r))
    }

    /// The check of [`TailGuard`], at the end of the outermost group.
    /// Refused as `NotWellFormed` (the text another form of the value
    /// takes may pass: `retry_flat`). O(depth × (intents + 1)); no value
    /// is read.
    fn check_tail(&self) -> Result<(), EditError> {
        let g = &self.tail;
        if g.before.is_none() && g.intents.is_empty() {
            // No reader before and none meant: only a new one can be
            // wrong.
            return match self.tail_reader() {
                None => Ok(()),
                Some(_) => Err(bad()),
            };
        }
        let after = self.tail_reader();
        // The reader before, left in place, keeps its value: it still
        // reads the line (see [`TailGuard`]).
        if let Some(r) = g.before {
            if self.tree().get(r).is_ok() && after != Some(r) {
                return Err(bad());
            }
        }
        // Every node written holds a reader exactly when meant.
        for (id, reads) in &g.intents {
            if self.tree().get(*id).is_err() {
                continue;
            }
            if after.is_some_and(|r| self.holds(*id, r)) != *reads {
                return Err(bad());
            }
        }
        // A reader the edit wrote must be meant.
        if let Some(r) = after {
            let old = g.before == Some(r);
            let meant = g
                .intents
                .iter()
                .any(|(id, reads)| *reads && self.holds(*id, r));
            if !old && !meant {
                return Err(bad());
            }
        }
        Ok(())
    }

    /// Undoes the steps of `txn` (see [`Document::atomic`]).
    fn rollback(&mut self, txn: Txn) -> Result<(), EditError> {
        let ops = self.ix_log.split_off(txn.ix.min(self.ix_log.len()));
        let built = self.shared.anchors.get().is_some();
        // The index follows the tree back: what went in leaves it first
        // (while live), what went out joins it again once restored (the
        // same handles: the syntax layer puts back the same elements).
        // When a range is neither wholly live nor wholly gone, the index is
        // dropped and rebuilt on next use (correct, O(n) once).
        let mut exact = true;
        let live =
            |d: &Self, a: TreeId, b: TreeId| (d.tree().get(a).is_ok(), d.tree().get(b).is_ok());
        if built {
            for op in ops.iter().rev() {
                if let IxOp::Inserted(a, b) = *op {
                    match live(self, a, b) {
                        (true, true) => {
                            let plan = match (self.tree().get(a), self.tree().get(b)) {
                                (Ok(na), Ok(nb)) => self.index_plan_of(na, nb),
                                _ => None,
                            };
                            self.index_removed(plan);
                        }
                        (false, false) => {}
                        _ => exact = false,
                    }
                }
            }
        }
        let r = self.tree_mut()?.rollback_group(txn.mark);
        // The lookup indexes follow the tree back (positions and
        // handles exact: the same elements return), or all go.
        self.lookup_undo(txn.lookups, r.is_ok());
        self.undo_explicit(txn.explicit);
        self.settle_explicit_log();
        if built {
            for op in &ops {
                if let IxOp::Removed(a, b) = *op {
                    match live(self, a, b) {
                        (true, true) => {
                            self.index_inserted(a, b);
                        }
                        (false, false) => {}
                        _ => exact = false,
                    }
                }
            }
        }
        if !self.tree().in_group() {
            self.ix_log.clear();
        }
        if !exact || r.is_err() {
            if let Some(s) = Arc::get_mut(&mut self.shared) {
                s.anchors = OnceLock::new();
            }
        }
        self.commit();
        if r.is_err() {
            debug_assert!(false, "rolling back an edit group failed: {r:?}");
            return Err(EditError::Internal);
        }
        Ok(())
    }

    // ======================================================== fragments

    /// The fragment that fills slot `kind` with `text` (the text after the
    /// slot's indicator, or the node's own text).
    /// The second value: the text ends with a block scalar.
    fn slot_fragment(&self, kind: SlotKind, text: &str) -> Result<(Fragment, bool), EditError> {
        self.slot_fragment_in(kind, text, None)
    }

    /// [`Document::slot_fragment`]; `gap`: the white space the document has
    /// between the indicator and the value, written in place of the single
    /// space `text` starts with.
    fn slot_fragment_in(
        &self,
        kind: SlotKind,
        text: &str,
        gap: Option<&str>,
    ) -> Result<(Fragment, bool), EditError> {
        let text = &with_gap(text, gap);
        let with = |t: &SyntaxTree, a, b: TreeNode<'_>| {
            Self::extract(t, a, b).map(|f| (f, last_block(b).is_some()))
        };
        match kind {
            SlotKind::MapValue { col } | SlotKind::SeqItem { col } => {
                let (ind, want, ikind) = slot_parts(kind);
                let t = self.synth(&format!("{}{ind}{text}", sp(col)))?;
                let c = root_node(&t).filter(|m| m.kind() == want).ok_or_else(bad)?;
                let (a, b) = after_first(c, ikind).ok_or_else(bad)?;
                with(&t, a, b)
            }
            SlotKind::AfterMarker => {
                let t = self.synth(&format!("---{text}"))?;
                let d = t
                    .root()
                    .children()
                    .find(|c| c.kind() == SyntaxKind::Document);
                let (a, b) = d
                    .and_then(|d| after_first(d, SyntaxKind::DocumentStartMarker))
                    .ok_or_else(bad)?;
                with(&t, a, b)
            }
            SlotKind::Own { col } => {
                let t = self.synth(&format!("{}{text}", sp(col)))?;
                let n = root_node(&t).ok_or_else(bad)?;
                with(&t, n, n)
            }
            SlotKind::ExplicitValue { col } => {
                let t = self.synth(&explicit_synth(col, text, "", self.nl()))?;
                let (a, b) = explicit_range(&t)?;
                with(&t, a, b)
            }
            SlotKind::FlowValue => {
                let t = self.synth(&format!("{{x{text}}}"))?;
                let (a, b) = flow_value_range(&t)?;
                Ok((Self::extract(&t, a, b)?, false))
            }
            SlotKind::Flow => {
                let t = self.synth(&format!("[{text}]"))?;
                let s = root_node(&t).filter(|m| m.kind() == SyntaxKind::FlowSequence);
                let items: Vec<_> = s
                    .map(|s| s.children().filter(|c| c.kind().is_yaml_node()).collect())
                    .unwrap_or_default();
                match items.as_slice() {
                    [n] => Ok((Self::extract(&t, *n, *n)?, false)),
                    _ => Err(bad()),
                }
            }
        }
    }

    /// [`Document::slot_fragment`] ending with the line break that ends
    /// its line (for `replace_to_line_end`).
    fn slot_line_fragment(&self, kind: SlotKind, text: &str) -> Result<Fragment, EditError> {
        self.slot_line_fragment_in(kind, text, None, false)
    }

    /// [`Document::slot_line_fragment`]; `gap` as for
    /// [`Document::slot_fragment_in`]; `open_end`: the replaced text ends the
    /// document without a line break, which stays missing.
    fn slot_line_fragment_in(
        &self,
        kind: SlotKind,
        text: &str,
        gap: Option<&str>,
        open_end: bool,
    ) -> Result<Fragment, EditError> {
        let text = &with_gap(text, gap);
        // Text that already ends its line (a block scalar's, kept as
        // written: `|+` keeps its trailing lines) gets no other break. A
        // block scalar the document ends in without a line break (`open_end`)
        // gets none either, unless the text needs one (a clip header).
        let nl = if ends_break(text) || (open_end && !clip_header(text)) {
            ""
        } else {
            self.nl()
        };
        match kind {
            SlotKind::MapValue { col } | SlotKind::SeqItem { col } => {
                let (ind, want, ikind) = slot_parts(kind);
                let t = self.synth(&format!("{}{ind}{text}{nl}", sp(col)))?;
                let c = root_node(&t).filter(|m| m.kind() == want).ok_or_else(bad)?;
                let (a, b) = after_first(c, ikind).ok_or_else(bad)?;
                Self::line_fragment_in(&t, a, b, open_end)
            }
            SlotKind::AfterMarker => {
                let t = self.synth(&format!("---{text}{nl}"))?;
                let d = t
                    .root()
                    .children()
                    .find(|c| c.kind() == SyntaxKind::Document);
                let (a, b) = d
                    .and_then(|d| after_first(d, SyntaxKind::DocumentStartMarker))
                    .ok_or_else(bad)?;
                Self::line_fragment_in(&t, a, b, open_end)
            }
            SlotKind::Own { col } => {
                let t = self.synth(&format!("{}{text}{nl}", sp(col)))?;
                let n = root_node(&t).ok_or_else(bad)?;
                Self::line_fragment_in(&t, n, n, open_end)
            }
            SlotKind::ExplicitValue { col } => {
                let t = self.synth(&explicit_synth(col, text, nl, self.nl()))?;
                let (a, b) = explicit_range(&t)?;
                Self::line_fragment_in(&t, a, b, open_end)
            }
            SlotKind::Flow | SlotKind::FlowValue => Err(bad()),
        }
    }

    /// The fragment inserting `text` (an entry or item, first line
    /// unindented) before an entry of a block collection at column `col`:
    /// written between two dummy entries, so that comment lines leading
    /// or trailing the text stay inside the collection; it ends with the
    /// line break and indentation before the next entry.
    fn block_before_fragment(
        &self,
        map: bool,
        col: usize,
        text: &str,
    ) -> Result<Fragment, EditError> {
        let (d0, d1, kind) = dummies(map);
        let (pad, nl) = (sp(col), self.nl());
        // Text ending its line (a block scalar: `|+` keeps its trailing
        // lines) gets no other break.
        let end = if ends_break(text) { "" } else { nl };
        let t = self.synth(&format!("{pad}{d0}{nl}{pad}{text}{end}{pad}{d1}"))?;
        let c = root_node(&t).filter(|c| c.kind() == kind).ok_or_else(bad)?;
        let es = place::entries(c)?;
        let (Some(first), Some(last), 3) = (es.first(), es.last(), es.len()) else {
            return Err(bad());
        };
        // After the first dummy's line (its break, if not in its value).
        let mut a = if ends_line(first.value) {
            first.value.next_sibling()
        } else {
            break_after(first.value).and_then(|b| b.next_sibling())
        };
        if let Some(w) = a.filter(|w| w.kind() == SyntaxKind::Whitespace) {
            a = w.next_sibling();
        }
        let a = a.ok_or_else(bad)?;
        let b = last.first.prev_sibling().ok_or_else(bad)?;
        Self::extract(&t, a, b)
    }

    /// Whole lines `text` (each ending with a line break: blank lines,
    /// comment lines, entry lines, each with its own indentation) as a
    /// fragment for between two entries of a block collection at column
    /// `col` (`insert_line_after` the line before): written between two
    /// dummy entries, so that they read as they will there.
    fn lines_fragment(&self, map: bool, col: usize, text: &str) -> Result<Fragment, EditError> {
        let (d0, d1, kind) = dummies(map);
        let (pad, nl) = (sp(col), self.nl());
        let t = self.synth(&format!("{pad}{d0}{nl}{text}{pad}{d1}"))?;
        let c = root_node(&t).filter(|c| c.kind() == kind).ok_or_else(bad)?;
        let es = place::entries(c)?;
        let (Some(first), Some(last)) = (es.first(), es.last()) else {
            return Err(bad());
        };
        let a = break_after(first.value)
            .and_then(|b| b.next_sibling())
            .ok_or_else(bad)?;
        let mut b = last.first.prev_sibling().ok_or_else(bad)?;
        if b.kind() == SyntaxKind::Whitespace {
            b = b.prev_sibling().ok_or_else(bad)?;
        }
        Self::extract(&t, a, b)
    }

    /// Entry text `body` (first line unindented) at column `col` of a block
    /// collection ends with a block scalar.
    fn ends_with_block(&self, map: bool, col: usize, body: &str) -> Result<bool, EditError> {
        let (d0, _, kind) = dummies(map);
        let (pad, nl) = (sp(col), self.nl());
        let t = self.synth(&format!("{pad}{d0}{nl}{pad}{body}"))?;
        let c = root_node(&t).filter(|c| c.kind() == kind).ok_or_else(bad)?;
        Ok(last_block(c).is_some())
    }

    /// The fragment appending `text` (whole lines above the entry, then the
    /// entry at its column, its first line indented: no line break at the
    /// end) after the last entry of a block collection at column `col`,
    /// with `moved` first: the last entry's own trivia after the
    /// collection (its same-line comment, then footer lines, when
    /// `same_line`; else footer lines after a line it ended). `tail`: the
    /// new entry's own lines after it, which the parser places after the
    /// collection (the second fragment).
    fn append_fragment(
        &self,
        map: bool,
        col: usize,
        moved: &str,
        same_line: bool,
        text: &str,
        tail: &[String],
    ) -> Result<(Fragment, Option<Fragment>), EditError> {
        let (d0, _, kind) = dummies(map);
        let (pad, nl) = (sp(col), self.nl());
        let head = if same_line || moved.is_empty() {
            format!("{pad}{d0}{moved}{nl}")
        } else {
            // A dummy value ending its line, whose text no moved line can
            // continue (it is indented past all of them).
            let ind = if map { "x:" } else { "-" };
            let deep = moved
                .split(['\n', '\r'])
                .map(|l| l.len().saturating_sub(l.trim_start_matches(' ').len()))
                .max()
                .unwrap_or(0)
                .max(col)
                .saturating_add(2);
            format!("{pad}{ind} |{nl}{}x{nl}{moved}{nl}", sp(deep))
        };
        let mut src = format!("{head}{text}");
        for l in tail {
            if !ends_break(&src) {
                src.push_str(nl);
            }
            src.push_str(l);
        }
        let t = self.synth(&src)?;
        let c = root_node(&t).filter(|c| c.kind() == kind).ok_or_else(bad)?;
        let es = place::entries(c)?;
        let (a, b) = es
            .first()
            .and_then(|e| Some((e.value.next_sibling()?, c.last_child()?)))
            .ok_or_else(bad)?;
        let tail = match trailing_trivia(c) {
            Some((x, y)) => Some(Self::extract(&t, x, y)?),
            None => None,
        };
        Ok((Self::extract(&t, a, b)?, tail))
    }

    /// The fragment inserting `text` (whole lines: lines above the entry,
    /// the entry, its own lines after it; ending with a line break) as the
    /// lines after the last entry of a block collection
    /// (`insert_line_after`).
    ///
    /// The second fragment: the comments after the entry, which
    /// the parser places after the collection. When the entry's text ends
    /// its line (a block scalar) they are whole lines, inserted as the
    /// lines after the collection ([`Tail::Lines`]); otherwise they start
    /// with the entry's same-line comment or line break and go before the
    /// token after the collection, which is then the entry's own line
    /// break ([`Tail::Before`], as for appends).
    fn block_line_fragment(
        &self,
        map: bool,
        col: usize,
        text: &str,
    ) -> Result<(Fragment, Option<Tail>), EditError> {
        let (d0, _, kind) = dummies(map);
        let (pad, nl) = (sp(col), self.nl());
        let synth = |text: &str| {
            let end = if ends_break(text) { "" } else { nl };
            self.synth(&format!("{pad}{d0}{nl}{text}{end}"))
        };
        let t = synth(text)?;
        let c = root_node(&t).filter(|c| c.kind() == kind).ok_or_else(bad)?;
        let (tail, bare) = match trailing_trivia(c) {
            None => (None, None),
            Some((first, last)) if c.last_child().is_some_and(ends_line) => (
                Some(Tail::Lines(Self::line_fragment(&t, first, last)?)),
                None,
            ),
            Some((first, last)) => {
                // The entry without them: its text up to `first`.
                let lead = pad.len().saturating_add(d0.len()).saturating_add(nl.len());
                let cut = first.span().start.checked_sub(lead).ok_or_else(bad)?;
                let bare = text.get(..cut).ok_or_else(bad)?;
                (
                    Some(Tail::Before(Self::extract(&t, first, last)?)),
                    Some(synth(bare)?),
                )
            }
        };
        let c = match &bare {
            Some(b) => {
                let c = root_node(b).filter(|c| c.kind() == kind).ok_or_else(bad)?;
                if trailing_trivia(c).is_some() {
                    return Err(bad());
                }
                c
            }
            None => c,
        };
        let t = bare.as_ref().unwrap_or(&t);
        let es = place::entries(c)?;
        let first = es.first().ok_or_else(bad)?;
        let a = if ends_line(first.value) {
            first.value.next_sibling()
        } else {
            break_after(first.value).and_then(|b| b.next_sibling())
        };
        if a.is_some_and(|w| w.kind() == SyntaxKind::LineBreak) && !text.starts_with(['\n', '\r']) {
            return Err(bad());
        }
        let a = a.ok_or_else(bad)?;
        let b = c.last_child().ok_or_else(bad)?;
        Ok((Self::line_fragment(t, a, b)?, tail))
    }

    /// The fragment inserting flow `text` at `place` in a flow collection
    /// (`empty`: it has no entries yet).
    fn flow_insert_fragment(
        &self,
        map: bool,
        text: &str,
        append: bool,
        empty: bool,
        seps: (Option<&str>, Option<&str>),
    ) -> Result<Fragment, EditError> {
        let (open, close, dummy) = if map {
            ('{', '}', "x: x")
        } else {
            ('[', ']', "x")
        };
        let (before, after) = (seps.0.unwrap_or(", "), seps.1.unwrap_or(", "));
        let src = if empty {
            format!("{open}{text}{close}")
        } else if append {
            format!("{open}{dummy}{before}{text}{close}")
        } else {
            format!("{open}{text}{after}{dummy}{close}")
        };
        let t = self.synth(&src)?;
        let c = root_node(&t)
            .filter(|c| Coll::of(*c).is_some_and(|k| k.is_flow() && k.is_map() == map))
            .ok_or_else(bad)?;
        let es = place::entries(c)?;
        if es.len() != if empty { 1 } else { 2 } {
            return Err(bad());
        }
        let range = match (es.first(), es.last()) {
            (Some(f), Some(l)) if empty => Some((f.first, l.value)),
            (Some(f), Some(l)) if append => f.value.next_sibling().map(|a| (a, l.value)),
            (Some(f), Some(l)) => l.first.prev_sibling().map(|b| (f.first, b)),
            _ => None,
        };
        let (a, b) = range.ok_or_else(bad)?;
        Self::extract(&t, a, b)
    }

    /// A whole line `line` (indentation, text, line break) for a flow
    /// collection, read between dummy entries: before an entry (`line` is
    /// `text,`) or after the last one (`line` is `, text`).
    fn flow_line_fragment(
        &self,
        map: bool,
        line: &str,
        before: bool,
        comma: bool,
    ) -> Result<Fragment, EditError> {
        let (open, close, dummy) = if map {
            ('{', '}', "x: x")
        } else {
            ('[', ']', "x")
        };
        let nl = self.nl();
        let sep = if comma { "," } else { "" };
        let src = if before {
            format!("{open}{dummy},{nl}{line}{dummy}{close}")
        } else {
            format!("{open}{dummy}{sep}{nl}{line}{close}")
        };
        let t = self.synth(&src)?;
        let c = root_node(&t)
            .filter(|c| Coll::of(*c).is_some_and(|k| k.is_flow() && k.is_map() == map))
            .ok_or_else(bad)?;
        let es = place::entries(c)?;
        let first = es.first().ok_or_else(bad)?;
        // The line starts after the first line break after the first entry.
        let mut at = first.value.next_sibling();
        while let Some(x) = at.filter(|x| x.kind() != SyntaxKind::LineBreak) {
            at = x.next_sibling();
        }
        let a = at.and_then(TreeNode::next_sibling).ok_or_else(bad)?;
        let b = if before {
            es.last().and_then(|l| l.first.prev_sibling())
        } else {
            c.last_child().and_then(TreeNode::prev_sibling)
        }
        .filter(|b| b.kind() == SyntaxKind::LineBreak)
        .ok_or_else(bad)?;
        Self::extract(&t, a, b)
    }

    /// The steps re-inserting a flow entry removed with comments around it (so
    /// no edit the baseline accepted is refused): its whole lines above the
    /// entry `before` (above `lead`, that entry's first comment line) or above
    /// the closing bracket, or its text after the last value (see
    /// [`Removed::flow_lines`]); a `,` after the last value when one is needed.
    /// Other shapes stay a documented limitation.
    fn flow_lines_steps(
        &self,
        coll: TreeNode<'_>,
        r: &Removed,
        before: Option<&place::Entry<'_>>,
        last: Option<&place::Entry<'_>>,
        lead: Option<TreeNode<'_>>,
        map: bool,
    ) -> Result<Vec<(Put, Fragment)>, EditError> {
        let limit = || unsupported(FLOW_REINSERT_LIMIT);
        let (text, mode) = r.flow_lines.as_ref().ok_or_else(limit)?;
        let (open, close_c, dummy) = if map {
            ('{', '}', "x: x")
        } else {
            ('[', ']', "x")
        };
        let nl = self.nl();
        let close = coll
            .last_child()
            .filter(|c| {
                matches!(
                    c.kind(),
                    SyntaxKind::FlowSequenceClose | SyntaxKind::FlowMappingClose
                )
            })
            .ok_or_else(limit)?;
        // Whole lines, read after `dummy,` and before the bracket.
        let lines = |d: &Self| -> Result<Fragment, EditError> {
            let t = d.synth(&format!("{open}{dummy},{nl}{text}{close_c}"))?;
            let c = root_node(&t).ok_or_else(bad)?;
            let es = place::entries(c)?;
            let first = es.first().ok_or_else(bad)?;
            let mut at = first.value.next_sibling();
            while let Some(x) = at.filter(|x| x.kind() != SyntaxKind::LineBreak) {
                at = x.next_sibling();
            }
            let a = at.and_then(TreeNode::next_sibling).ok_or_else(bad)?;
            let b = c
                .last_child()
                .and_then(TreeNode::prev_sibling)
                .filter(|b| b.kind() == SyntaxKind::LineBreak)
                .ok_or_else(bad)?;
            Self::extract(&t, a, b)
        };
        let comma = |d: &Self| -> Result<Fragment, EditError> {
            let t = d.synth(&format!("{open}{dummy},{dummy}{close_c}"))?;
            let c = root_node(&t).ok_or_else(bad)?;
            let x = c
                .children()
                .find(|x| x.kind() == SyntaxKind::FlowEntry)
                .ok_or_else(bad)?;
            Self::extract(&t, x, x)
        };
        // The last value has its `,` after it already.
        let has_comma = |l: &place::Entry<'_>| {
            let mut at = l.value.next_sibling();
            while let Some(x) =
                at.filter(|x| x.kind().is_trivia() || x.kind() == SyntaxKind::FlowEntry)
            {
                if x.kind() == SyntaxKind::FlowEntry {
                    return true;
                }
                at = x.next_sibling();
            }
            false
        };
        let close_head = || {
            cm::line_start(close)
                .then(|| carry::line_head(close))
                .ok_or_else(limit)
        };
        Ok(match (*mode, before, last) {
            (1, Some(e), _) => {
                let first = cm::first_token(e.first).ok_or(EditError::Internal)?;
                let head = match lead {
                    Some(c) => carry::line_head(c),
                    // (The entry node itself, not its first token, which
                    // lies inside it.)
                    None if cm::line_start(first) => match carry::line_head(first) {
                        h if h == first => e.first,
                        h => h,
                    },
                    None => return Err(limit()),
                };
                vec![(Put::Before(head.id()), lines(self)?)]
            }
            (0 | 1, None, None) => vec![(Put::Before(close_head()?.id()), lines(self)?)],
            (0 | 1, None, Some(l)) => {
                let head = close_head()?;
                let mut out = Vec::new();
                if !has_comma(l) {
                    out.push((Put::After(l.value.id()), comma(self)?));
                }
                out.push((Put::Before(head.id()), lines(self)?));
                out
            }
            (2, None, Some(l)) if !has_comma(l) => {
                let t = self.synth(&format!("{open}{dummy}{text}{nl}{close_c}"))?;
                let c = root_node(&t).ok_or_else(bad)?;
                let es = place::entries(c)?;
                let a = es
                    .first()
                    .and_then(|f| f.value.next_sibling())
                    .ok_or_else(bad)?;
                let b = c
                    .last_child()
                    .and_then(TreeNode::prev_sibling)
                    .filter(|b| b.kind() == SyntaxKind::LineBreak)
                    .and_then(TreeNode::prev_sibling)
                    .ok_or_else(bad)?;
                vec![(Put::After(l.value.id()), Self::extract(&t, a, b)?)]
            }
            // Column-0 comment lines above an entry on an
            // indented line of its own (`[⏎# c⏎  b, c]`), before an entry
            // that starts such a line: the lines go above its indentation,
            // then the entry and its separator before it.
            (3, Some(e), _) => {
                let first = cm::first_token(e.first).ok_or(EditError::Internal)?;
                let ws = place::prev_token(first)
                    .filter(|w| w.kind() == SyntaxKind::Whitespace && cm::line_start(*w))
                    .ok_or_else(limit)?;
                let seps = (r.sep_before.as_deref(), r.sep_after.as_deref());
                let entry = self.flow_insert_fragment(map, &r.text, false, false, seps)?;
                vec![
                    (Put::Before(ws.id()), lines(self)?),
                    (Put::Before(e.first.id()), entry),
                ]
            }
            // A last entry's text (after its `,`, with its same-line
            // comment) before an entry: `entry # c` on a line, then that
            // entry after `, ` on the next.
            (2, Some(e), _) => {
                let entry = text.trim_start_matches([',', ' ', '\t', '\n', '\r']);
                let pad = sp(place::col_of(e.first));
                let t = self.synth(&format!("{open}{entry}{nl}{pad}, {dummy}{close_c}"))?;
                let c = root_node(&t).ok_or_else(bad)?;
                let es = place::entries(c)?;
                let (Some(f), Some(l), 2) = (es.first(), es.last(), es.len()) else {
                    return Err(bad());
                };
                let b = l.first.prev_sibling().ok_or_else(bad)?;
                vec![(Put::Before(e.first.id()), Self::extract(&t, f.first, b)?)]
            }
            _ => return Err(limit()),
        })
    }

    // ============================================= anchor and alias checks

    /// The position just past the siblings `first..=last`.
    fn end_of(last: TreeNode<'_>) -> usize {
        place::next_token(last).map_or(usize::MAX, TreeNode::preorder_index)
    }

    /// An anchor still in use: the aliases outside `first..=last` (after it)
    /// that refer to anchors it defines, except `keep`'s own (a replacement
    /// keeps it, rule 3); with the first such anchor's name. O(log k + found)
    /// per anchor.
    fn outside_refs(
        &self,
        names: &Names<'_>,
        last: TreeNode<'_>,
        keep: Option<TreeId>,
    ) -> (Option<String>, Vec<TreeId>, Vec<(String, usize)>) {
        // The index is only built when anchors are involved.
        if names.anchors.iter().all(|(_, id)| Some(*id) == keep) {
            return (None, Vec::new(), Vec::new());
        }
        let tree = self.tree();
        let (_, doc_end) = self.doc_range();
        let end = Self::end_of(last);
        let ix = self.anchors();
        let mut first = None;
        let mut out = Vec::new();
        let mut runs = Vec::new();
        for (name, id) in &names.anchors {
            if Some(*id) == keep {
                continue;
            }
            let Ok(n) = tree.get(*id) else { continue };
            let at = n.preorder_index();
            let next = ix.next_after(tree, name, at, doc_end);
            let found = ix.aliases_in(tree, name, end.max(at.saturating_add(1)), next);
            if found.is_empty() {
                continue;
            }
            if first.is_none() {
                first = Some((*name).to_owned());
            }
            runs.push(((*name).to_owned(), found.len()));
            // (One copy of the found aliases.)
            if out.is_empty() {
                out = found;
            } else {
                out.extend(found);
            }
        }
        (first, out, runs)
    }

    /// Anchors still in use, for removing or replacing `first..=last`.
    fn check_removal(
        &self,
        first: TreeNode<'_>,
        last: TreeNode<'_>,
        keep: Option<TreeId>,
    ) -> Result<(), EditError> {
        let names = Names::of(first, last);
        let (name, refs, _) = self.outside_refs(&names, last, keep);
        match name {
            Some(anchor) => Err(EditError::AnchorInUse {
                anchor,
                aliases: refs.into_iter().map(NodeId).collect(),
            }),
            None => Ok(()),
        }
    }

    /// An alias must follow its anchor, also when re-inserted: for aliases
    /// written at preorder position `point`: each must refer to its recorded
    /// anchored node, which must be the **nearest preceding** anchor of its
    /// name there (aliases are edited by token, never by name search).
    fn check_uses(&self, uses: &[(String, TreeId)], point: usize) -> Result<(), EditError> {
        let (start, _) = self.doc_range();
        for (name, target) in uses {
            let want = *target;
            let found = self
                .anchors()
                .nearest_before(self.tree(), name, start, point);
            if found != Some(want) {
                return Err(EditError::AliasBeforeAnchor {
                    anchor: name.clone(),
                });
            }
        }
        Ok(())
    }

    /// An alias written into collection `coll` must not lie inside
    /// its own anchored node (a recursive alias, which YAML cannot express:
    /// the anchored node is not complete there). O(depth) per alias.
    fn check_inside(&self, coll: TreeNode<'_>, uses: &[(String, TreeId)]) -> Result<(), EditError> {
        for (_, target) in uses {
            let want = *target;
            if coll.id() == want || coll.ancestors().any(|a| a.id() == want) {
                return Err(unsupported("an alias inside its own anchored node"));
            }
        }
        Ok(())
    }

    /// Re-inserted anchors must not capture other aliases: for anchors named
    /// `defines` written at `point`: the aliases after `point` they capture (up
    /// to the next anchor of each name) must be exactly `expected` (the aliases
    /// that referred to them before a move; none for a new node), and no other
    /// alias of those names may follow.
    ///
    /// `runs` (a move, see [`Removed::moving`]): `expected` as runs of
    /// live aliases in document order, one per name; then the check is
    /// first, last and count of each run against the ranks of its name's
    /// aliases at `point` (O(names × log k)), and only a
    /// refusal (or another shape) goes through the sets below (O(k)).
    fn check_defines(
        &self,
        defines: &[String],
        expected: &[TreeId],
        runs: Option<&[(String, usize)]>,
        point: usize,
    ) -> Result<(), EditError> {
        let tree = self.tree();
        let (_, end) = self.doc_range();
        let ix = self.anchors();
        if runs.is_some_and(|r| self.defines_by_runs(defines, expected, r, point, end)) {
            return Ok(());
        }
        let want: HashSet<TreeId> = expected.iter().copied().collect();
        let mut captured = HashSet::new();
        let mut done = HashSet::new();
        for name in defines {
            if !done.insert(name.as_str()) {
                continue;
            }
            let next = ix
                .first_anchor_in(tree, name, point, end)
                .and_then(|a| tree.get(a).ok())
                .map_or(end, TreeNode::preorder_index);
            // Conservatively (a re-inserted anchor must capture no alias), no
            // other alias of the name may follow at all, captured or shadowed.
            let after = ix.aliases_in(tree, name, point, end);
            if after.iter().any(|a| !want.contains(a)) {
                return Err(EditError::DuplicateAnchor {
                    anchor: name.clone(),
                });
            }
            captured.extend(ix.aliases_in(tree, name, point, next));
        }
        // The first missed alias of `expected` (whose order is the
        // document order of the anchors they referred to, as recorded at
        // the removal), never the first of a hash set's order, so the
        // error names the same anchor on every run.
        if let Some(missed) = expected.iter().find(|a| !captured.contains(*a)) {
            let anchor = tree
                .get(*missed)
                .ok()
                .and_then(|n| n.alias_name())
                .unwrap_or_default()
                .to_owned();
            return Err(EditError::AliasBeforeAnchor { anchor });
        }
        Ok(())
    }

    /// The fast path of [`Document::check_defines`]: `true` only when the
    /// sets there would accept (each run of `expected` is its name's
    /// aliases from `point` to the end exactly, before any anchor of that
    /// name there; a defined name without a run has no alias there).
    fn defines_by_runs(
        &self,
        defines: &[String],
        expected: &[TreeId],
        runs: &[(String, usize)],
        point: usize,
        end: usize,
    ) -> bool {
        let tree = self.tree();
        let ix = self.anchors();
        let mut by: std::collections::BTreeMap<&str, (usize, usize)> =
            std::collections::BTreeMap::new();
        let mut off = 0usize;
        for (name, count) in runs {
            if *count == 0 || by.insert(name.as_str(), (off, *count)).is_some() {
                return false;
            }
            off = off.saturating_add(*count);
        }
        let defined: std::collections::BTreeSet<&str> =
            defines.iter().map(String::as_str).collect();
        if off != expected.len() || by.keys().any(|n| !defined.contains(n)) {
            return false;
        }
        defined.iter().all(|name| {
            let (run, count) = match by.get(name) {
                Some(&(o, c)) => {
                    let last = o.saturating_add(c).saturating_sub(1);
                    match (expected.get(o), expected.get(last)) {
                        (Some(f), Some(l)) => (Some((*f, *l)), c),
                        _ => return false,
                    }
                }
                None => (None, 0),
            };
            ix.aliases_exactly(tree, name, point, end, run, count)
        })
    }

    /// The aliases whose anchor is `n`, in document order (O(log k +
    /// found)).
    fn aliases_of_node(&self, n: TreeNode<'_>) -> Vec<TreeId> {
        let Some(name) = n.anchor().map(|a| a.name) else {
            return Vec::new();
        };
        let (_, end) = self.doc_range();
        self.anchors()
            .aliases_of(self.tree(), name, n.preorder_index(), end)
    }

    // ========================================================== replace

    /// Replaces node `target` by `v` (or by the scalar text `forced`), keeping
    /// its anchor (a replacement keeps the anchor). Returns the new node.
    /// Multi-line strings are `|` blocks where the context permits, else
    /// one-line flow text.
    fn op_replace(
        &mut self,
        target: TreeId,
        v: &Value,
        forced: Option<&str>,
    ) -> Result<TreeId, EditError> {
        self.op_replace_keeping(target, v, forced, false)
    }

    /// [`Document::op_replace`]; `keep`: the comments the old value owns
    /// after it stay (see [`Document::replace_carry`]).
    fn op_replace_keeping(
        &mut self,
        target: TreeId,
        v: &Value,
        forced: Option<&str>,
        keep: bool,
    ) -> Result<TreeId, EditError> {
        match self.op_replace_with(target, v, forced, false, keep) {
            Err(e) if retry_flat(&e) && forced.is_none() && multiline(v) => {
                self.op_replace_with(target, v, forced, true, keep)
            }
            r => r,
        }
    }

    /// The replacement a **public setter** makes (`set`, `set_scalar`,
    /// `set_path`, `set_root`, `insert` on an existing key, `replace_at`):
    /// a node set to the value it already means is accepted
    /// and changes no byte (style, comments, anchor and handle kept), once
    /// the edit would be allowed there (restricted mode) and the
    /// node is one an edit may replace (a key stays `Unsupported`). The
    /// edit is recorded (the document counts as edited). Internal edit
    /// steps (the collapse of a collection's only entry) never use it:
    /// they call [`Document::op_replace`] directly (the collapse
    /// of a merge-only mapping, which means `{}`, must still write `{}`).
    fn op_set(
        &mut self,
        target: TreeId,
        v: &Value,
        forced: Option<&str>,
    ) -> Result<TreeId, EditError> {
        self.prepare()?;
        let same = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            if !n.kind().is_yaml_node() {
                return Err(EditError::ReadOnly { at: Path::new() });
            }
            self.check_region(&[n], n.parent())?;
            // (Whether it is a key is asked only of an equal value.)
            self.means(n, v)
                && !n
                    .parent()
                    .filter(|p| Coll::of(*p).is_some_and(Coll::is_map))
                    .is_some_and(|p| self.key_in(p, n))
        };
        if same {
            self.commit();
            return Ok(target);
        }
        self.op_replace(target, v, forced)
    }

    /// Whether node `n` means `v`, as [`Value`] equality
    /// says (the node read as [`NodeRef::deserialize`] reads it, which
    /// is what [`Document::to_value`] has there).
    ///
    /// A node that is or holds an alias or a merge key never
    /// counts as equal (it is written as any value): keeping it would keep
    /// a link to an anchored node the caller just replaced by a plain value
    /// (aliases denote the node). So the read never expands an alias.
    ///
    /// Cost: O(the node's own elements), never what aliases reach:
    /// one walk for aliases and merge keys, cheap checks next (a scalar
    /// against a string compares content; another kind or length differs),
    /// then the node is read (O(the node)) only when its shape matches.
    /// A node that cannot be read (an error inside it) means nothing here.
    fn means(&self, n: TreeNode<'_>, v: &Value) -> bool {
        let r = NodeRef::new(self, n);
        if n.kind() == SyntaxKind::Scalar {
            match v {
                Value::String(s) => {
                    return r.as_scalar().is_some_and(|x| x.means_string(s));
                }
                Value::Sequence(_) | Value::Mapping(_) => return false,
                _ => {}
            }
        }
        if !self.plain_subtree(n) {
            return false;
        }
        let untagged = matches!(r.tag(), Ok(None));
        match r.kind() {
            NodeKind::Scalar if untagged => {
                return r
                    .as_scalar()
                    .is_some_and(|s| s.resolved().is_ok_and(|x| x == *v));
            }
            NodeKind::Sequence if untagged => match v {
                Value::Sequence(items) => {
                    if r.as_sequence().map(super::SequenceRef::len) != Some(items.len()) {
                        return false;
                    }
                }
                _ => return false,
            },
            // (Repeated keys, which `DuplicateKeys::LastWins` allows, are
            // checked after the read, at every depth.)
            NodeKind::Mapping if untagged => match v {
                Value::Mapping(m) => {
                    let written = r.as_mapping().map(super::MappingRef::len);
                    if written != Some(m.len()) {
                        return false;
                    }
                }
                _ => return false,
            },
            NodeKind::Error | NodeKind::Alias => return false,
            _ => {}
        }
        // Repeated keys (`DuplicateKeys::LastWins`) at any depth: the node
        // is rewritten, which drops the repeats, as for any other value
        // (the same at every depth). The entries written are
        // counted against the entries meant, O(the node).
        let repeats = self.settings().get_duplicate_keys() != oneyaml_core::DuplicateKeys::Error;
        r.deserialize::<Value>()
            .is_ok_and(|x| x == *v && (!repeats || written_entries(n) == value_entries(&x)))
    }

    /// Whether node `n` neither is nor holds an alias or (with
    /// `merge_keys` on) a merge key. O(its elements).
    fn plain_subtree(&self, n: TreeNode<'_>) -> bool {
        let merges = NodeRef::new(self, n).resolver().merge_keys();
        // (`descendants` includes `n` itself.)
        n.descendants().all(|d| match d.kind() {
            SyntaxKind::Alias => false,
            // Fuzz finding H3: only a merge *key* counts;
            // a `<<` written as a value or item means the string `<<`, and
            // setting it to that value changes no byte. (A tag that does
            // not resolve counts, conservatively.) The key test only runs
            // for a `<<` scalar: O(log width) each (`key_in`).
            SyntaxKind::Scalar if merges => {
                let merge = NodeRef::new(self, d).is_merge_key().unwrap_or(true);
                !(merge
                    && d.parent()
                        .filter(|p| Coll::of(*p).is_some_and(Coll::is_map))
                        .is_some_and(|p| self.key_in(p, d)))
            }
            _ => true,
        })
    }

    fn op_replace_with(
        &mut self,
        target: TreeId,
        v: &Value,
        forced: Option<&str>,
        flat: bool,
        keep: bool,
    ) -> Result<TreeId, EditError> {
        self.prepare()?;
        let nl = self.nl();
        let carried;
        let mut post: Option<String> = None;
        let plan = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            if !n.kind().is_yaml_node() {
                return Err(EditError::ReadOnly { at: Path::new() });
            }
            self.check_region(&[n], n.parent())?;
            self.check_removal(n, n, Some(n.id()))?;
            let slot = place::slot_of(n)?;
            carried = Some(self.replace_carry(n, slot.first, slot.last, keep)?);
            let anchor = node_props(n, v);
            let context = n.parent().filter(|p| Coll::of(*p).is_some());
            let render = self.render(context, flat)?;
            // A string set over an existing node is quoted when the
            // document's own schema reads it as another type, not whenever
            // some YAML reader might (`yes` is a string in YAML 1.2).
            let render = if matches!(v, Value::String(_)) {
                render.schema_floor()
            } else {
                render
            };
            let block = matches!(v.untag_ref(), Value::Sequence(s) if !s.is_empty())
                || matches!(v.untag_ref(), Value::Mapping(m) if !m.is_empty());
            let compact = block && !v.is_tagged();
            // Style inference: a string replacing a quoted scalar keeps its
            // quotes when they can hold it.
            let kept = match (forced, v) {
                (None, Value::String(sv)) if n.kind() == SyntaxKind::Scalar => {
                    let style = NodeRef::new(self, n).as_scalar().map(|x| x.style());
                    match style {
                        Some(st @ (ScalarStyle::SingleQuoted | ScalarStyle::DoubleQuoted)) => {
                            render.keep_style(sv, st)
                        }
                        // A plain scalar stays plain unless the new text has a
                        // line break, which it writes double-quoted.
                        Some(ScalarStyle::Plain) if sv.contains('\n') && !n.span().is_empty() => {
                            render.keep_style(sv, ScalarStyle::Plain)
                        }
                        Some(ScalarStyle::Literal | ScalarStyle::Folded) => {
                            block_style(n, slot.kind).and_then(|old| render.block_scalar(sv, &old))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let forced = forced.or(kept.as_deref());
            // `? a` alone gets its `:` on the next line
            // (after the key line's comment, which stays there).
            // The last entry of its mapping (`? - x⏎  - y # c`
            // ending the mapping) has its key line's comment outside the
            // mapping, where the parser places it: it leaves there (a step
            // of its own) and is written back before the new `:` line, so
            // it stays on the key line (never moved onto the value's).
            let outer = match slot.kind {
                SlotKind::ExplicitValue { .. } if slot.last == n && !place::key_ends_line(n) => {
                    trailing_comment(n).filter(|(_, c)| c.parent() != n.parent())
                }
                _ => None,
            };
            let lead = || match outer {
                Some((w, c)) => format!("{}{nl}", place::range_text(w, c)),
                None => explicit_lead(n, slot.last, nl),
            };
            let text = match (slot.kind, forced) {
                (SlotKind::ExplicitValue { col }, Some(t)) => format!("{}{}: {t}", lead(), sp(col)),
                (SlotKind::ExplicitValue { col }, None) => {
                    format!("{}{}:{}", lead(), sp(col), render.after_key(v, col)?)
                }
                (SlotKind::FlowValue, Some(t)) => format!(": {t}"),
                (SlotKind::FlowValue, None) => format!(": {}", render.flow(v)?),
                (SlotKind::Flow, Some(t)) | (SlotKind::Own { .. }, Some(t)) => t.to_owned(),
                (_, Some(t)) => format!(" {t}"),
                (SlotKind::MapValue { col }, None) => render.after_key(v, col)?,
                (SlotKind::SeqItem { col }, None) => render.after_dash(v, col)?,
                (SlotKind::AfterMarker, None) => {
                    let t = render.root(v)?;
                    if compact {
                        format!("{nl}{t}")
                    } else {
                        format!(" {t}")
                    }
                }
                (SlotKind::Own { col }, None) => render.own(v, col)?,
                (SlotKind::Flow, None) => render.flow(v)?,
            };
            let text = match anchor {
                None => text,
                Some(a) => match slot.kind {
                    SlotKind::SeqItem { col } if compact && forced.is_none() => {
                        let rest = text.strip_prefix(' ').ok_or(EditError::Internal)?;
                        format!(" {a}{nl}{}{rest}", sp(col.saturating_add(2)))
                    }
                    SlotKind::MapValue { .. }
                    | SlotKind::SeqItem { .. }
                    | SlotKind::AfterMarker => {
                        format!(" {a}{text}")
                    }
                    SlotKind::Own { col } if compact && forced.is_none() => {
                        format!("{a}{nl}{}{text}", sp(col))
                    }
                    SlotKind::Own { .. } | SlotKind::Flow => format!("{a} {text}"),
                    // A value written without `:` has no properties (they
                    // would need the `:`).
                    SlotKind::ExplicitValue { .. } | SlotKind::FlowValue => {
                        // An invariant (callers never pass an anchor
                        // here), `Err(Internal)` in release.
                        debug_assert!(false, "an anchor on a value written without `:`");
                        return Err(EditError::Internal);
                    }
                },
            };
            // A nested collection an only
            // entry leaves keeps its footer lines inside the new flow
            // collection (`{⏎  # f⏎  }`).
            let text = match carried.as_ref().and_then(|rc| rc.footer_lines.as_deref()) {
                Some(f) => {
                    let close = if text.ends_with("{}") { '}' } else { ']' };
                    let open = text
                        .strip_suffix("{}")
                        .or_else(|| text.strip_suffix("[]"))
                        .ok_or(EditError::Internal)?;
                    let f = if ends_break(f) {
                        f.to_owned()
                    } else {
                        format!("{f}{nl}")
                    };
                    let at = place::col_of(n).max(1);
                    let open_char = if close == '}' { '{' } else { '[' };
                    format!("{open}{open_char}{nl}{f}{}{close}", sp(at))
                }
                None => text,
            };
            // The entry's (or the node's own)
            // comment inside the old value's first line is written back on
            // the new value's: on its header line when the new value is
            // written on lines of its own, else after it (a step of its
            // own, `post`).
            let text = match carried.as_ref().and_then(|rc| rc.header.clone()) {
                None => text,
                Some(c) => {
                    let refuse = || EditError::Unsupported {
                        at: Path::new(),
                        reason: carry::HEADER_COMMENT_LIMIT,
                    };
                    // A same-line comment after the value (not one that goes
                    // with the old value).
                    let trailing = {
                        let mut t = place::next_token(slot.last);
                        while t.is_some_and(|x| x.kind() == SyntaxKind::Whitespace) {
                            t = t.and_then(place::next_token);
                        }
                        t.is_some_and(|x| {
                            x.kind() == SyntaxKind::Comment
                                && !carried.as_ref().is_some_and(|rc| rc.gone.contains(&x.id()))
                        })
                    };
                    // Content after the value on its line (a flow context's
                    // `,`, a `:`): nowhere to write the comment.
                    let content_after = !ends_line(n) && {
                        let mut t = place::next_token(slot.last);
                        while t.is_some_and(|x| x.kind() == SyntaxKind::Whitespace) {
                            t = t.and_then(place::next_token);
                        }
                        t.is_some_and(|x| {
                            !matches!(x.kind(), SyntaxKind::LineBreak | SyntaxKind::Comment)
                        })
                    };
                    match slot.kind {
                        SlotKind::Flow | SlotKind::FlowValue | SlotKind::ExplicitValue { .. } => {
                            return Err(refuse());
                        }
                        _ if text.contains(['\n', '\r']) => {
                            if trailing {
                                return Err(refuse());
                            }
                            header_text(slot.kind, &text, &c, nl).ok_or_else(bad)?
                        }
                        _ if content_after => return Err(refuse()),
                        // The value has a same-line comment of its own too:
                        // the entry's goes on the key's (or dash's) line, the
                        // value below it (`k: # c⏎  7 # d`).
                        SlotKind::MapValue { col } | SlotKind::SeqItem { col } if trailing => {
                            format!(
                                "{c}{nl}{}{}",
                                sp(col.saturating_add(2)),
                                text.trim_start_matches(' ')
                            )
                        }
                        _ if trailing => return Err(refuse()),
                        _ => {
                            post = Some(c);
                            text
                        }
                    }
                }
            };
            // Content that ends its line (a block scalar's text ends with
            // its line break) is replaced through the line's end,
            // old or new.
            // The white space between `:` (or `-`) and the value stays as the
            // document wrote it (a tab, several spaces) when the new text
            // starts on the same line: only the value's own text changes.
            // A block collection written there (`- a: 1`) has its lines
            // aligned after exactly one space, and a tab cannot separate it
            // from the indicator: that gap is one space.
            let gap_text = match slot.kind {
                SlotKind::MapValue { .. } | SlotKind::SeqItem { .. }
                    if slot.first != n
                        && slot.first.kind() == SyntaxKind::Whitespace
                        && slot.first.next_sibling() == Some(n)
                        && !block
                        && text.starts_with(' ') =>
                {
                    Some(slot.first.text().into_owned())
                }
                _ => None,
            };
            let gap = gap_text.as_deref();
            // A block scalar ending the document without a line break: its
            // replacement leaves the break missing too.
            let open_end = last_block(n).is_some_and(|b| !ends_break(&b.text()));
            let range = (slot.first.id(), slot.last.id());
            if matches!(slot.kind, SlotKind::ExplicitValue { .. }) && place::key_ends_line(n) {
                // `? |` alone: the empty value sits at the start of the
                // next line; the new `: value` line ends with its own line
                // break, spliced in there (never through that line's end).
                let frag = self.slot_line_fragment_in(slot.kind, &text, gap, open_end)?;
                (range, frag, false, None)
            } else if let Some((w, c)) = outer {
                // The comment written back (see `outer`), once it has left
                // (then this is `? a⏎` alone): a new block scalar goes
                // through the line's end, as there.
                let (frag, new_block) = self.slot_fragment_in(slot.kind, &text, gap)?;
                if new_block {
                    let frag = self.slot_line_fragment_in(slot.kind, &text, gap, open_end)?;
                    (range, frag, true, Some((w.id(), c.id())))
                } else {
                    (range, frag, false, Some((w.id(), c.id())))
                }
            } else {
                let (frag, new_block) = self.slot_fragment_in(slot.kind, &text, gap)?;
                if ends_line(n) || new_block {
                    // A same-line comment after
                    // the replaced value moves onto the new block scalar's
                    // header line (`a: 1 # c` → `a: |- # c`), byte for byte.
                    // (`? a` alone: its slot already keeps the comment on the
                    // key line.)
                    let explicit = matches!(slot.kind, SlotKind::ExplicitValue { .. });
                    // (not a comment the old value owns, rule 5:
                    // it goes with the old value.)
                    let gone = |c: TreeNode<'_>| {
                        carried.as_ref().is_some_and(|rc| rc.gone.contains(&c.id()))
                    };
                    let comment = trailing_comment(n)
                        .filter(|_| new_block && !ends_line(n) && !explicit)
                        .filter(|(_, c)| !gone(*c));
                    match comment {
                        Some((w, c)) => {
                            let text = with_header_comment(&text, &place::range_text(w, c))
                                .ok_or_else(bad)?;
                            let frag =
                                self.slot_line_fragment_in(slot.kind, &text, gap, open_end)?;
                            let line = !open_end || ends_break(frag.text());
                            (range, frag, line, Some((w.id(), c.id())))
                        }
                        None => {
                            let frag =
                                self.slot_line_fragment_in(slot.kind, &text, gap, open_end)?;
                            let line = !open_end || ends_break(frag.text());
                            (range, frag, line, None)
                        }
                    }
                } else if text.contains(['\n', '\r'])
                    && !matches!(slot.kind, SlotKind::ExplicitValue { .. })
                    && carried.as_ref().is_some_and(|rc| rc.header.is_none())
                {
                    // Likewise onto the header line of a new
                    // collection written on lines of its own (`- a # c` →
                    // `- # c⏎  - x`), so the entry keeps it.
                    let gone = |c: TreeNode<'_>| {
                        carried.as_ref().is_some_and(|rc| rc.gone.contains(&c.id()))
                    };
                    match trailing_comment(n).filter(|(_, c)| !gone(*c)) {
                        Some((w, c)) => {
                            let text = header_text(slot.kind, &text, &place::range_text(w, c), nl)
                                .ok_or_else(bad)?;
                            let (frag, _) = self.slot_fragment_in(slot.kind, &text, gap)?;
                            (range, frag, false, Some((w.id(), c.id())))
                        }
                        None => (range, frag, false, None),
                    }
                } else {
                    (range, frag, false, None)
                }
            }
        };
        let (range, frag, line, comment) = plan;
        let rc = carried.ok_or(EditError::Internal)?;
        // The old value's comments after it go first (its
        // whole lines, then a same-line one), then the comment moving to
        // a new block scalar's header leaves its place (each a step of its
        // own, undone if the replacement is refused: never a panic).
        let (new, moved) = self.atomic(|d| {
            if let Some((a, b)) = rc.lines {
                d.t_remove(a, b)?;
            }
            if let Some((a, b)) = rc.lead {
                d.t_remove(a, b)?;
            }
            if let Some((a, b)) = rc.same_line {
                d.t_remove(a, b)?;
            }
            if let Some((w, c)) = comment {
                d.t_remove(w, c)?;
            }
            let ins = d.t_replace(range.0, range.1, frag, line)?;
            let new = d.last_node(ins).unwrap_or(ins.last);
            if let Some(c) = &post {
                let frag = d.trivia_fragment(c)?;
                let puts = {
                    let n = d.tree().get(new).map_err(|_| EditError::Internal)?;
                    super::comment_edit::after_chain(n)
                };
                let mut done = Err(bad());
                for p in puts {
                    done = d.t_insert(p, frag.clone());
                    if done.is_ok() {
                        break;
                    }
                }
                done?;
            }
            let moved = d.check_window(rc.window, &rc.expect, &[new])?;
            Ok((new, moved))
        })?;
        self.forget_explicit(&rc.gone);
        self.carry_explicit(&moved, &[], &[new], Some((target, new)));
        self.commit();
        Ok(new)
    }

    /// Node `n` and the YAML nodes inside it, in order (the
    /// corresponding nodes of a node rewritten with the same structure).
    fn node_list(&self, n: TreeId) -> Result<Vec<TreeId>, EditError> {
        let n = self.tree().get(n).map_err(|_| EditError::Internal)?;
        Ok(carry::entry_nodes(None, n))
    }

    /// The last YAML node among the inserted siblings (the new value).
    fn last_node(&self, ins: Inserted) -> Option<TreeId> {
        let nodes = self.inserted_nodes(ins);
        nodes.last().copied()
    }

    /// The YAML nodes among the inserted siblings.
    fn inserted_nodes(&self, ins: Inserted) -> Vec<TreeId> {
        let tree = self.tree();
        let mut at = tree.get(ins.first).ok();
        let mut nodes = Vec::new();
        while let Some(n) = at {
            if n.kind().is_yaml_node() {
                nodes.push(n.id());
            }
            if n.id() == ins.last {
                break;
            }
            at = n.next_sibling();
        }
        nodes
    }

    // ========================================================== insert

    /// The text of `payload` as an entry (with `key`) or item of `coll`.
    fn payload_text(
        &self,
        coll: TreeNode<'_>,
        kind: Coll,
        col: usize,
        key: Option<&str>,
        payload: Payload<'_>,
        flat: bool,
    ) -> Result<String, EditError> {
        let render = self.render(Some(coll), flat)?;
        let flow = kind.is_flow();
        let key_text = |k: &str| render.key(k, flow);
        Ok(match (payload, kind.is_map()) {
            (Payload::Value(v), true) => {
                let k = Value::from(key.ok_or(EditError::Internal)?);
                if flow {
                    render.flow_entry(&k, v)?
                } else {
                    render.entry(&k, v, col)?
                }
            }
            (Payload::Entry(k, v), true) => {
                if flow {
                    render.flow_entry(k, v)?
                } else {
                    render.entry(k, v, col)?
                }
            }
            (Payload::Entry(..), false) => return Err(EditError::Internal),
            (Payload::Value(v), false) => {
                if flow {
                    render.flow(v)?
                } else {
                    render.item(v, col)?
                }
            }
            (Payload::Alias(name, _), true) => {
                let key = key.ok_or(EditError::Internal)?;
                match key_text(key) {
                    Ok(k) => format!("{k}: *{name}"),
                    // A key too long or multi-line for the implicit form takes
                    // the serializer's `? ` form (as the serializer writes it),
                    // written for a null value whose `null` (the last token of
                    // the entry) becomes the alias.
                    Err(EditError::Unsupported { .. }) => {
                        let k = Value::from(key);
                        let text = if flow {
                            render.flow_entry(&k, &Value::Null)?
                        } else {
                            render.entry(&k, &Value::Null, col)?
                        };
                        let head = text
                            .trim_end_matches(['\n', '\r'])
                            .strip_suffix("null")
                            .ok_or(EditError::Internal)?;
                        format!("{head}*{name}")
                    }
                    Err(e) => return Err(e),
                }
            }
            (Payload::AliasEntry(k, name, _), true) => {
                // Written for a null value whose `null` (the last token of
                // the entry) becomes the alias, as a long string key's.
                let text = if flow {
                    render.flow_entry(k, &Value::Null)?
                } else {
                    render.entry(k, &Value::Null, col)?
                };
                let head = text
                    .trim_end_matches(['\n', '\r'])
                    .strip_suffix("null")
                    .ok_or(EditError::Internal)?;
                format!("{head}*{name}")
            }
            (Payload::AliasEntry(..), false) => return Err(EditError::Internal),
            (Payload::Alias(name, _), false) => {
                if flow {
                    format!("*{name}")
                } else {
                    format!("- *{name}")
                }
            }
            (Payload::Merge(targets), true) => {
                // A plain `<<` is the merge key (the
                // serializer would quote a `<<` string key).
                let names: Vec<String> = targets.iter().map(|(n, _)| format!("*{n}")).collect();
                match names.as_slice() {
                    [one] => format!("<<: {one}"),
                    _ => format!("<<: [{}]", names.join(", ")),
                }
            }
            (Payload::Merge(_), false) => {
                return Err(unsupported("a merge key goes into a mapping"));
            }
            (Payload::Null(text), is_map) => {
                let sep = if text.is_empty() { "" } else { " " };
                match (is_map, flow) {
                    (_, true) => return Err(EditError::Internal),
                    (true, false) => {
                        let k = key.ok_or(EditError::Internal)?;
                        match key_text(k) {
                            Ok(k) => format!("{k}:{sep}{text}"),
                            // A key that needs the `? ` form: as `null`.
                            Err(EditError::Unsupported { .. }) => {
                                render.entry(&Value::from(k), &Value::Null, col)?
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    (false, false) => format!("-{sep}{text}"),
                }
            }
            (Payload::Removed(r), is_map) => {
                if is_map != r.key.is_some() {
                    return Err(unsupported(
                        "a removed entry goes into a mapping, a removed item into a sequence",
                    ));
                }
                let multi = r.text.contains(['\n', '\r']);
                // (a block entry's same-line comment, inside its
                // text, counts too; it was refused as `NotWellFormed`.)
                let block_comments = !r.flow && !r.comments.is_empty();
                if flow && (!r.flow && multi || block_comments) {
                    // 0.2 limitation: re-rendering moved
                    // content in the target style is a later phase.
                    return Err(unsupported(
                        "block content or comments cannot move into a flow collection",
                    ));
                }
                if r.flow && r.loose && r.flow_lines.is_none() {
                    return Err(unsupported(FLOW_REINSERT_LIMIT));
                }
                let target = if flow || is_map {
                    col
                } else {
                    col.saturating_add(2)
                };
                // Flow text back into a flow collection keeps its lines as
                // written (indentation means nothing there, rule 9).
                let body = if flow && r.flow {
                    r.text.clone()
                } else {
                    reindent(&r.text, r.col, target).ok_or_else(|| {
                        unsupported("the removed text cannot be re-indented there")
                    })?
                };
                if flow || is_map {
                    body
                } else {
                    match r.head.as_deref().filter(|_| !r.flow) {
                        // Its own separator, its indentation moved with the
                        // entry (the bytes come back).
                        Some(h) => {
                            let delta = |c: usize| {
                                if col >= r.entry_col {
                                    c.saturating_add(col.saturating_sub(r.entry_col))
                                } else {
                                    c.saturating_sub(r.entry_col.saturating_sub(col))
                                }
                            };
                            // Each line after the first moved with the entry
                            // (its comment lines, then the value's line).
                            let mut out = String::with_capacity(h.len());
                            for (i, l) in carry::split_keep(h).into_iter().enumerate() {
                                if i == 0 {
                                    out.push_str(l);
                                    continue;
                                }
                                let rest = l.trim_start_matches(' ');
                                let n = l.len().saturating_sub(rest.len());
                                out.push_str(&sp(delta(n)));
                                out.push_str(rest);
                            }
                            let h = out;
                            let body = reindent(&r.text, r.col, delta(r.col)).ok_or_else(|| {
                                unsupported("the removed text cannot be re-indented there")
                            })?;
                            format!("-{h}{body}")
                        }
                        // An empty value with a same-line comment: its text
                        // starts with the white space before the comment.
                        None if body.starts_with([' ', '\t']) => format!("-{body}"),
                        None => format!("- {body}"),
                    }
                }
            }
        })
    }

    /// Inserts `payload` into collection `coll` at `at`. Returns the new
    /// value (or item) node. Multi-line strings as in
    /// [`Document::op_replace`].
    fn op_insert(
        &mut self,
        coll_id: TreeId,
        key: Option<&str>,
        at: Where,
        payload: Payload<'_>,
    ) -> Result<TreeId, EditError> {
        // (flat, inferred blank lines): a new value's blank lines follow
        // its neighbours, unless they cannot be written
        // there (after a `|+` scalar they would be its content);
        // multi-line strings are `|` blocks where the context permits.
        let multi = matches!(payload, Payload::Value(v) | Payload::Entry(_, v) if multiline(v));
        // Then a re-inserted entry's leading comment lines at its own column
        // (as the baseline wrote them), when theirs would give them to another
        // owner there (a deeper collection's footer, rule 6).
        let mut last = Err(EditError::Internal);
        for flat in [false, true] {
            for level in 0u8..3 {
                if flat && !multi {
                    continue;
                }
                last = self.op_insert_with(coll_id, key, at, payload, flat, level);
                match &last {
                    Err(e) if retry_flat(e) || retry_lead(e) => {}
                    _ => return last,
                }
            }
        }
        // Limitation S3: a flow entry removed with comments around
        // it whose lines do not read back in any layout tried (a comment
        // would read with other text, or not at all) is this limitation,
        // refused by name.
        match (&last, payload) {
            (Err(EditError::NotWellFormed { .. }), Payload::Removed(r)) if r.flow && r.loose => {
                Err(unsupported(FLOW_REINSERT_LIMIT))
            }
            _ => last,
        }
    }

    /// An insertion adds no comments of its own and
    /// leaves every comment with its owner; a re-inserted entry brings its
    /// own back around it (see `carry.rs`). Block collections: before an
    /// entry, the new lines go above that entry's own lines (its blank
    /// lines and leading comment lines); at the end, after the last
    /// entry's own lines (its same-line comment, the footers inside it)
    /// and before the collection's footer.
    fn op_insert_with(
        &mut self,
        coll_id: TreeId,
        key: Option<&str>,
        at: Where,
        payload: Payload<'_>,
        flat: bool,
        level: u8,
    ) -> Result<TreeId, EditError> {
        self.prepare()?;
        let blanks = level == 0;
        /// The steps of an insertion.
        enum Steps {
            /// Flow: `frag` at `put`.
            Flow(Put, Fragment),
            /// Flow: each fragment at its place, in order.
            FlowMany(Vec<(Put, Fragment)>),
            /// Before an entry that is not the first: whole lines after
            /// the line `anchor` ends.
            Lines(TreeId, Fragment),
            /// Before the first entry: its leading comment lines `pre`
            /// (siblings) leave; `frag` goes in before its first element;
            /// the new entry's own lines above it (`lead`, whole lines) go
            /// before the collection.
            First {
                pre: Option<(TreeId, TreeId)>,
                first: TreeId,
                frag: Fragment,
                lead: Option<String>,
                /// The collection starts on its parent's `- `/`? ` line:
                /// the white space before it, which `lead` replaces (with a
                /// line break before and the indentation after).
                compact: Option<(TreeId, usize)>,
            },
            /// At the end: the last entry's own trivia after the collection
            /// (`relocate`, siblings; `whole`: through its line break)
            /// moves into `frag`, which goes at `put`; `tail` after the
            /// collection.
            Append {
                put: Put,
                frag: Fragment,
                tail: Option<Tail>,
                relocate: Option<(TreeId, TreeId)>,
                remove_first: bool,
            },
        }
        let (steps, window, expect, map) = {
            let coll = self.tree().get(coll_id).map_err(|_| EditError::StaleNode)?;
            let kind = Coll::of(coll).ok_or(EditError::NotAMapping { at: Path::new() })?;
            if kind == Coll::Pair {
                return self.op_pair_insert(coll_id, key, at, payload, flat);
            }
            self.refuse_error_container(coll)?;
            let before = match at {
                Where::Before(v) => {
                    let n = self.tree().get(v).map_err(|_| EditError::StaleNode)?;
                    Some(place::entry_of(coll, n)?)
                }
                Where::Append => None,
            };
            let last = place::last_entry(coll)?;
            // The neighbours of the insertion point.
            let mut near = Vec::new();
            let (prev, next) = match &before {
                None => (last, None),
                Some(e) => (place::prev_entry(coll, e)?, Some(*e)),
            };
            for e in prev.into_iter().chain(next) {
                near.extend(e.key);
                near.push(e.value);
            }
            self.check_region(&near, Some(coll))?;
            // An alias must follow its anchor: where the new content goes, in
            // order.
            let point = match &before {
                Some(e) => e.first.preorder_index(),
                None => Self::end_of(coll),
            };
            match payload {
                Payload::Removed(r) => {
                    if r.doc != self.id {
                        return Err(EditError::WrongDocument);
                    }
                    if let Some(k) = &r.key {
                        let m = NodeRef::new(self, coll).as_mapping();
                        let own = m.and_then(|m| m.own_value(k));
                        // A second merge key is a duplicate key.
                        let merge = r.merge
                            && m.map(|m| m.has_merge_key())
                                .transpose()
                                .map_err(tag_internal)?
                                .unwrap_or(false);
                        // Equal keys by meaning, not by text.
                        let equal = !r.merge
                            && r.key_value
                                .as_ref()
                                .is_some_and(|v| m.is_some_and(|m| m.has_key_value(v)));
                        if own.is_some() || merge || equal {
                            return Err(EditError::KeyExists { at: Path::new() });
                        }
                    }
                    self.check_inside(coll, &r.external)?;
                    self.check_uses(&r.external, point)?;
                    self.check_defines(
                        &r.anchors,
                        &r.referenced,
                        r.moving.then_some(r.referenced_runs.as_slice()),
                        point,
                    )?;
                }
                Payload::Alias(name, target) | Payload::AliasEntry(_, name, target) => {
                    self.check_uses(&[(name.to_owned(), target)], point)?;
                }
                Payload::Merge(targets) => self.check_uses(targets, point)?,
                Payload::Value(_) | Payload::Entry(..) | Payload::Null(_) => {}
            }
            // Flow text is written without a column (see
            // `payload_text`): finding it would walk back over the line.
            let col = match place::first_entry(coll)? {
                Some(e) if !kind.is_flow() => place::col_of(e.first),
                _ => 0,
            };
            let map = kind.is_map();
            let body = self.payload_text(coll, kind, col, key, payload, flat)?;
            let nl = self.nl();
            let pad = sp(col);
            // The comments a re-inserted entry brings back.
            let carried: Vec<Expect> = match payload {
                Payload::Removed(r) => r
                    .comments
                    .iter()
                    .zip(&r.owners)
                    .zip(&r.explicit)
                    .zip(&r.ids)
                    .map(|(((t, o), x), id)| Expect {
                        text: t.clone(),
                        want: Want::Rel(*o),
                        old: Some(id.tree_id()),
                        explicit: *x,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            // The window: the content tokens around the insertion point;
            // the comments between stay, the carried ones go in at `split`.
            // In flow, the `,` before the entry
            // the new one goes before: comments before it can be that
            // entry's (rule 9), so the window starts at the content before.
            let flow_comma = match &before {
                Some(e) if kind.is_flow() => cm::first_token(e.first)
                    .and_then(carry::visible_before)
                    .filter(|t| t.kind() == SyntaxKind::FlowEntry),
                _ => None,
            };
            let (wa, wb, split_owner) = if kind.is_flow() {
                let (a, b) = match &before {
                    Some(e) => {
                        let f = cm::first_token(e.first).ok_or(EditError::Internal)?;
                        let a = match flow_comma {
                            Some(c) => carry::visible_before(c),
                            None => carry::visible_before(f),
                        };
                        (a, Some(f))
                    }
                    None => {
                        let close = coll.last_child().ok_or(EditError::Internal)?;
                        (carry::visible_before(close), Some(close))
                    }
                };
                (a, b, None)
            } else if let Some(e) = &before {
                let f = cm::first_token(e.first).ok_or(EditError::Internal)?;
                (carry::visible_before(f), Some(f), Some(e))
            } else {
                let l = cm::last_token(coll).ok_or(EditError::Internal)?;
                (Some(l), carry::visible_after(l), None)
            };
            let comments = carry::window_comments(self.tree().root(), wa, wb);
            let owners = self.rule_owners(&comments)?;
            // Before an entry: its own comments (rules 3, 4, 10) come after
            // the new ones. At the end: the last entry's own ones (inside
            // it) come before.
            // Membership by walking up from the owner
            // (O(depth) per comment of the window), never by listing the
            // last entry's nodes (O(its subtree): O(document) for a root
            // mapping's last entry).
            let last_entry = match (&before, &last) {
                (None, Some(l)) => Some((l.key.map(TreeNode::id), l.value.id())),
                _ => None,
            };
            let tree = self.tree();
            let in_last = |n: TreeId| {
                last_entry.is_some_and(|(k, v)| {
                    tree.get(n).is_ok_and(|x| {
                        std::iter::once(x)
                            .chain(x.ancestors())
                            .any(|a| a.id() == v || Some(a.id()) == k)
                    })
                })
            };
            let next_node = split_owner.map(|e| e.key.unwrap_or(e.value).id());
            let mut expect = Vec::with_capacity(comments.len().saturating_add(carried.len()));
            let mut split = None;
            let mut lead_first: Option<TreeNode<'_>> = None;
            let mut relocate_last: Option<TreeNode<'_>> = None;
            for (i, c) in comments.iter().enumerate() {
                let k = *owners.get(&c.id()).ok_or(EditError::Internal)?;
                let node = carry::owner_node(k.0).map(|(_, n)| n.0);
                if split.is_none() {
                    let at_split = match (&before, next_node) {
                        (Some(_), Some(n)) => node == Some(n),
                        (Some(_), None) => false,
                        (None, _) => !node.is_some_and(in_last),
                    };
                    if at_split {
                        split = Some(i);
                        if before.is_some() {
                            lead_first = Some(*c);
                        }
                    } else if before.is_none() && !kind.is_flow() {
                        relocate_last = Some(*c);
                    }
                }
                expect.push(Expect {
                    text: c.text().into_owned(),
                    want: Want::Rel(Rel::Fixed(k)),
                    old: Some(c.id()),
                    explicit: None,
                });
            }
            // Flow, before an entry with comment lines of its own above it
            // (rule 9): the new text goes above them.
            let flow_lead: Option<(usize, TreeNode<'_>)> = match &before {
                Some(e) if kind.is_flow() => {
                    // Owned by the entry: its key, value, or a node inside
                    // them (a pair's key).
                    let own = [e.first.id(), e.value.id()];
                    let tree = self.tree();
                    let mut found = None;
                    let comma_at = flow_comma.map(TreeNode::preorder_index);
                    for (i, c) in comments.iter().enumerate() {
                        // (Before the `,`: see `own_before_comma`.)
                        if comma_at.is_some_and(|a| c.preorder_index() < a) {
                            continue;
                        }
                        let k = owners.get(&c.id()).ok_or(EditError::Internal)?;
                        let node = carry::owner_node(k.0).map(|(_, n)| n.0);
                        let inside = node.and_then(|n| tree.get(n).ok()).is_some_and(|n| {
                            own.contains(&n.id()) || n.ancestors().any(|a| own.contains(&a.id()))
                        });
                        if inside {
                            found = Some((i, *c));
                            break;
                        }
                    }
                    found.filter(|(_, c)| cm::line_start(*c))
                }
                _ => None,
            };
            // The entry has comments of its own
            // before its `,` (`word1⏎# c⏎, word2`): the new entry goes right
            // after the entry before it (`word1, new⏎# c⏎, word2`), before
            // those comments.
            let own_before_comma: Option<usize> = match (&before, flow_comma) {
                (Some(e), Some(comma)) => {
                    let own = [e.first.id(), e.value.id()];
                    let tree = self.tree();
                    let mut found = None;
                    for (i, c) in comments.iter().enumerate() {
                        if c.preorder_index() > comma.preorder_index() {
                            break;
                        }
                        let k = owners.get(&c.id()).ok_or(EditError::Internal)?;
                        let node = carry::owner_node(k.0).map(|(_, n)| n.0);
                        let inside = node.and_then(|n| tree.get(n).ok()).is_some_and(|n| {
                            own.contains(&n.id()) || n.ancestors().any(|a| own.contains(&a.id()))
                        });
                        if inside {
                            found = Some(i);
                            break;
                        }
                    }
                    found
                }
                _ => None,
            };
            let split = if kind.is_flow() {
                // Flow: the new text goes right before the entry (or the
                // closing bracket), after every comment of the window.
                own_before_comma
                    .or(flow_lead.map(|(i, _)| i))
                    .unwrap_or(expect.len())
            } else {
                split.unwrap_or(expect.len())
            };
            let tail_expect = expect.split_off(split.min(expect.len()));
            expect.extend(carried);
            expect.extend(tail_expect);
            // The lines above and below the new entry: a re-inserted
            // entry's own (moved to this column); a new value's blank
            // lines follow the entry it goes before (or, at the end, the
            // last entry), as style inference.
            let (lead, tail): (Vec<String>, Vec<String>) = match payload {
                // (Without their blank lines when they cannot be written
                // there: after a `|+` scalar they would be its content.)
                Payload::Removed(r) => {
                    let keep = |ls: Vec<String>| -> Vec<String> {
                        if blanks {
                            ls
                        } else {
                            ls.into_iter().filter(|l| !l.trim().is_empty()).collect()
                        }
                    };
                    let mut lead = keep(carry::shift_lines(&r.lead, r.entry_col, col));
                    if level >= 2 {
                        for l in &mut lead {
                            let t = l.trim_start_matches([' ', '\t']);
                            if t.starts_with('#') {
                                *l = format!("{}{t}", sp(col));
                            }
                        }
                    }
                    (lead, keep(carry::shift_lines(&r.tail, r.entry_col, col)))
                }
                _ => {
                    let like = match (&before, &last) {
                        (Some(e), _) if prev.is_some() => Some(*e),
                        (None, Some(l)) if place::prev_entry(coll, l)?.is_some() => Some(*l),
                        _ => None,
                    };
                    let n = match like.filter(|_| blanks) {
                        Some(e) => cm::first_token(e.first)
                            .filter(|t| cm::line_start(*t))
                            .map_or(0, |t| self.blank_lines_above(carry::line_head(t))),
                        None => 0,
                    };
                    (vec![String::new(); n], Vec::new())
                }
            };
            let lines = |ls: &[String]| {
                let mut out = String::new();
                for l in ls {
                    out.push_str(l);
                    out.push_str(nl);
                }
                out
            };
            let body_end = if ends_break(&body) { "" } else { nl };
            let window = Window {
                a: wa.map(TreeNode::id),
                b: wb.map(TreeNode::id),
            };
            let steps = if kind.is_flow() {
                let empty = last.is_none();
                // A re-inserted flow entry's own separators.
                let seps = match payload {
                    Payload::Removed(r) if r.flow => {
                        (r.sep_before.as_deref(), r.sep_after.as_deref())
                    }
                    _ => (None, None),
                };
                // The entry before which the
                // new one goes has its own comment lines above it, or the
                // last entry has a comment on its line: the new entry takes
                // a line of its own (above those lines, or below that
                // comment), so every comment keeps its owner.
                let line_put = match (&before, &last) {
                    (Some(e), _) => {
                        flow_lead.map(|(_, c)| (carry::line_head(c), place::col_of(e.first), true))
                    }
                    (None, Some(l)) => {
                        let mut at = l.value.next_sibling();
                        let mut comma = false;
                        let mut same = None;
                        while let Some(x) = at {
                            match x.kind() {
                                SyntaxKind::Comment => {
                                    same = Some(x);
                                    break;
                                }
                                SyntaxKind::FlowEntry => comma = true,
                                SyntaxKind::Whitespace => {}
                                _ => break,
                            }
                            at = x.next_sibling();
                        }
                        let next_line = same
                            .and_then(TreeNode::next_sibling)
                            .filter(|b| b.kind() == SyntaxKind::LineBreak)
                            .and_then(TreeNode::next_sibling);
                        next_line.map(|h| (h, place::col_of(l.first), comma))
                    }
                    (None, None) => None,
                };
                let loose = match payload {
                    Payload::Removed(r) if r.flow && r.loose => Some(r),
                    _ => None,
                };
                let after_prev = own_before_comma.and(prev.filter(|_| before.is_some()));
                if let (Some(p), None) = (after_prev, loose) {
                    // (See `own_before_comma`: `, new` after the entry
                    // before.)
                    let frag = self.flow_insert_fragment(map, &body, true, false, seps)?;
                    Steps::Flow(Put::After(p.value.id()), frag)
                } else if let Some(r) = loose {
                    Steps::FlowMany(self.flow_lines_steps(
                        coll,
                        r,
                        before.as_ref(),
                        last.as_ref(),
                        flow_lead.map(|(_, c)| c),
                        map,
                    )?)
                } else if let Some((head, indent, comma)) = line_put {
                    let line = match &before {
                        Some(_) => format!("{}{body},{nl}", sp(indent)),
                        None if comma => format!("{}{body}{nl}", sp(indent)),
                        None => format!("{}, {body}{nl}", sp(indent)),
                    };
                    let frag = self.flow_line_fragment(map, &line, before.is_some(), comma)?;
                    Steps::Flow(Put::Before(head.id()), frag)
                } else {
                    let frag =
                        self.flow_insert_fragment(map, &body, before.is_none(), empty, seps)?;
                    let put = match (&before, &last) {
                        (Some(e), _) => Put::Before(e.first.id()),
                        (None, Some(l)) => Put::After(l.value.id()),
                        (None, None) => {
                            let open = coll
                                .children()
                                .find(|c| {
                                    matches!(
                                        c.kind(),
                                        SyntaxKind::FlowMappingOpen | SyntaxKind::FlowSequenceOpen
                                    )
                                })
                                .ok_or(EditError::Internal)?;
                            Put::After(open.id())
                        }
                    };
                    Steps::Flow(put, frag)
                }
            } else if let Some(e) = before {
                let first_tok = cm::first_token(e.first).ok_or(EditError::Internal)?;
                if prev.is_some() {
                    // The entry's own lines start at its first leading
                    // comment line (or its line), after its blank lines.
                    if !cm::line_start(first_tok) {
                        return Err(bad());
                    }
                    let head = lead_first.map_or(carry::line_head(first_tok), carry::line_head);
                    // On a retry, the blank lines above the
                    // entry stay with what is above them (after a block
                    // scalar they may be its content) and the new text goes
                    // right below them.
                    let top = if level >= 1 {
                        head
                    } else {
                        self.after_blank(head)
                    };
                    // An empty block scalar's empty content token ends
                    // nothing: the line ends at the break before it.
                    let mut before_top = place::prev_token(top);
                    while let Some(t) = before_top.filter(|t| t.text().is_empty()) {
                        before_top = place::prev_token(t);
                    }
                    let mut anchor = before_top.filter(|t| cm::ends_line(*t)).ok_or_else(bad)?;
                    // The collection's child that ends there (a block
                    // scalar ends the value holding it).
                    while let Some(p) = anchor.parent().filter(|p| *p != coll) {
                        anchor = p;
                    }
                    if anchor.parent() != Some(coll) {
                        return Err(bad());
                    }
                    let text = format!("{}{pad}{body}{body_end}{}", lines(&lead), lines(&tail));
                    let frag = self.lines_fragment(map, col, &text)?;
                    Steps::Lines(anchor.id(), frag)
                } else {
                    // The first entry: its leading comment lines (before
                    // the collection) move after the new entry, with the
                    // blank lines above them, which become its own once it
                    // is not the first (rule 8; a removal of the
                    // first entry left them there). Not below a comment
                    // line: they may be what makes it the document header
                    // (rule 2).
                    let line = carry::line_head(first_tok);
                    let head = lead_first.map_or(line, carry::line_head);
                    let take = match payload {
                        Payload::Removed(r) => r.next_blank,
                        _ => 0,
                    };
                    let from = if cm::line_start(first_tok) {
                        self.blank_lines_up(head, take)
                    } else {
                        head
                    };
                    let pre = if from == line {
                        None
                    } else {
                        let lb = place::prev_token(line)
                            .filter(|t| t.kind() == SyntaxKind::LineBreak)
                            .ok_or_else(bad)?;
                        if carry::siblings(from, lb).is_none() {
                            return Err(bad());
                        }
                        Some((from, lb))
                    };
                    let pre_text = pre.map_or_else(String::new, |(a, b)| {
                        let mut t = String::new();
                        let mut at = Some(a);
                        while let Some(x) = at {
                            t.push_str(&x.text());
                            if x == b {
                                break;
                            }
                            at = x.next_sibling();
                        }
                        t
                    });
                    // The lines as they were before the entry's
                    // removal made the next one first (see `next_lead`).
                    let pre_text = match payload {
                        Payload::Removed(r) => match &r.next_lead {
                            Some((now, was)) if *now == pre_text => was.clone(),
                            _ => pre_text,
                        },
                        _ => pre_text,
                    };
                    // The old first entry's leading comment
                    // lines had any column above the collection (rule 3);
                    // below the new entry, a deeper one would read as its
                    // footer (rule 6): as a fallback they are written at
                    // the entry's column, owners unchanged.
                    let pre_text = if level >= 2 {
                        let mut out = String::with_capacity(pre_text.len());
                        for l in carry::split_keep(&pre_text) {
                            let t = l.trim_start_matches([' ', '\t']);
                            if t.starts_with('#') {
                                out.push_str(&sp(col));
                                out.push_str(t);
                            } else {
                                out.push_str(l);
                            }
                        }
                        out
                    } else {
                        pre_text
                    };
                    // Comment rules 2 and 3: at the top of a document, a blank
                    // line between a comment and the root makes it the
                    // header, so the new first entry's leading comments
                    // come without the blank lines they had.
                    let at_top = coll
                        .parent()
                        .is_some_and(|p| p.kind() == SyntaxKind::Document);
                    let lead: Vec<String> = if at_top {
                        lead.into_iter().filter(|l| !l.trim().is_empty()).collect()
                    } else {
                        lead
                    };
                    // A collection starting on its parent's
                    // `- `/`? ` line gets a line of its own below the new
                    // entry's comment lines (`- - a` → `-⏎  # c⏎  - b⏎  - a`).
                    let compact = if !lead.is_empty() && !cm::line_start(first_tok) {
                        let ws = place::prev_token(first_tok)
                            .filter(|w| w.kind() == SyntaxKind::Whitespace)
                            .ok_or_else(bad)?;
                        Some((ws.id(), col))
                    } else {
                        None
                    };
                    let text = format!("{body}{body_end}{}{pre_text}", lines(&tail));
                    let frag = self.block_before_fragment(map, col, &text)?;
                    Steps::First {
                        pre: pre.map(|(a, b)| (a.id(), b.id())),
                        first: e.first.id(),
                        frag,
                        lead: (!lead.is_empty()).then(|| lines(&lead)),
                        compact,
                    }
                }
            } else {
                let last_child = coll.last_child().ok_or(EditError::Internal)?;
                // The last entry's own trivia after the collection: its
                // same-line comment and the footers inside it (they lie
                // after the collection, C4) come into it, before the new
                // entry.
                let relocate = match relocate_last {
                    Some(c) => {
                        let first = place::next_token(coll).ok_or(EditError::Internal)?;
                        // Limitation S6: the last entry's comments
                        // lie past an empty value the collection's parent
                        // holds (`? - a # t` with no `:`): they cannot be
                        // moved after the new entry.
                        if carry::siblings(first, c).is_none() {
                            return Err(unsupported(
                                "appending after comments that lie past an empty value (a `? ` key \
                                 written without `:`)",
                            ));
                        }
                        Some((first, c))
                    }
                    // White space ending the last entry's line (after the
                    // collection, C4) stays on that line.
                    None => place::next_token(coll)
                        .filter(|w| {
                            w.kind() == SyntaxKind::Whitespace
                                && !ends_line(last_child)
                                && place::next_token(*w)
                                    .is_none_or(|b| b.kind() == SyntaxKind::LineBreak)
                        })
                        .map(|w| (w, w)),
                };
                // The new text ends with a block scalar (its last line then
                // ends inside its text).
                let new_block = ends_break(&body) || self.ends_with_block(map, col, &body)?;
                // The last line ends the text without a line break: the new
                // lines start with one (a block scalar's text then ends
                // the text with its own).
                let eof = !ends_line(last_child) && place::next_token(coll).is_none();
                match relocate {
                    None if !ends_line(last_child) && (!new_block || eof) => {
                        let text = format!("{}{pad}{body}", lines(&lead));
                        let (frag, tail_frag) =
                            self.append_fragment(map, col, "", false, &text, &tail)?;
                        Steps::Append {
                            put: Put::After(last_child.id()),
                            frag,
                            tail: tail_frag.map(Tail::Before),
                            relocate: None,
                            remove_first: false,
                        }
                    }
                    None => {
                        // After a line that ends inside a block
                        // scalar, or new text ending with one: whole lines.
                        let text = format!("{}{pad}{body}{body_end}", lines(&lead));
                        let text = format!("{text}{}", lines(&tail));
                        let (frag, tail) = self.block_line_fragment(map, col, &text)?;
                        Steps::Append {
                            put: Put::LineAfter(last_child.id()),
                            frag,
                            tail,
                            relocate: None,
                            remove_first: false,
                        }
                    }
                    Some((a, b)) => {
                        let moved = place::range_text(a, b);
                        let same_line = !cm::line_start(a);
                        let text = format!("{}{pad}{body}", lines(&lead));
                        let (frag, tail_frag) =
                            self.append_fragment(map, col, &moved, same_line, &text, &tail)?;
                        // A new block scalar ends its own line: the moved
                        // lines go through their line break. Whole lines
                        // (the last entry ended its line) go first, as a
                        // block scalar before them would read them; a
                        // same-line comment after, as the new entry's line
                        // ends with it meanwhile.
                        let rb = if new_block {
                            b.next_sibling()
                                .filter(|x| x.kind() == SyntaxKind::LineBreak)
                                .ok_or_else(bad)?
                        } else {
                            b
                        };
                        Steps::Append {
                            put: Put::After(last_child.id()),
                            frag,
                            tail: tail_frag.map(Tail::Before),
                            relocate: Some((a.id(), rb.id())),
                            // Otherwise a same-line comment
                            // goes first too; removed after the new entry it
                            // would follow the new entry's plain scalar, a
                            // removal the syntax layer cannot decide locally
                            // (it re-read the whole last entry: O(document)
                            // for a root mapping's).
                            remove_first: new_block != same_line,
                        }
                    }
                }
            };
            (steps, window, expect, map)
        };
        let want = if map { 2 } else { 1 };
        // A re-inserted entry reads the final space line again
        // exactly when it did where it was taken from; a new value never.
        let reads_tail = match payload {
            Payload::Removed(r) => r.reads_tail,
            _ => false,
        };
        let (new, moved, inner) = self.atomic(|d| {
            let ins = match steps {
                Steps::Flow(put, frag) => d.t_insert(put, frag)?,
                Steps::FlowMany(steps) => {
                    let mut ins = None;
                    for (put, frag) in steps {
                        let i = d.t_insert(put, frag)?;
                        // The entry's own text: the one holding YAML nodes.
                        if !d.inserted_nodes(i).is_empty() {
                            ins = Some(i);
                        }
                    }
                    ins.ok_or(EditError::Internal)?
                }
                Steps::Lines(anchor, frag) => d.t_insert(Put::LineAfter(anchor), frag)?,
                Steps::First {
                    pre,
                    first,
                    frag,
                    lead,
                    compact,
                } => {
                    if let Some((a, b)) = pre {
                        d.t_remove(a, b)?;
                    }
                    let ins = d.t_insert(Put::Before(first), frag)?;
                    if let (Some(lead), Some((ws, col))) = (&lead, compact) {
                        let text = format!("{}{lead}{}", d.nl(), sp(col));
                        // (Lines carried from the document.)
                        let frag = d.carried_fragment(&text)?;
                        d.t_replace(ws, ws, frag, false)?;
                    } else if let Some(lead) = lead {
                        let above = {
                            let n = d.tree().get(ins.first).map_err(|_| EditError::Internal)?;
                            let t = cm::first_token(n).ok_or(EditError::Internal)?;
                            cm::before_indent(t).map(|x| x.id())
                        };
                        d.insert_trivia_lines(above, &lead)?;
                    }
                    ins
                }
                Steps::Append {
                    put,
                    frag,
                    tail,
                    relocate,
                    remove_first,
                } => {
                    if let (Some((a, b)), true) = (relocate, remove_first) {
                        d.t_remove(a, b)?;
                    }
                    let ins = d.t_insert(put, frag)?;
                    if let (Some((a, b)), false) = (relocate, remove_first) {
                        d.t_remove(a, b)?;
                    }
                    d.place_tail(coll_id, tail)?;
                    ins
                }
            };
            // The new value: the last YAML node inserted (the value of a
            // `key: value` entry, or the item). Any other count is
            // our bug, undone and reported.
            let nodes = d.inserted_nodes(ins);
            let (new, inner) = match nodes.as_slice() {
                [k, v] if want == 2 => {
                    let (k, v) = (d.tree().get(*k), d.tree().get(*v));
                    match (k, v) {
                        (Ok(k), Ok(v)) => (v.id(), carry::entry_nodes(Some(k), v)),
                        _ => return Err(EditError::Internal),
                    }
                }
                [v] if want == 1 => {
                    let v = d.tree().get(*v).map_err(|_| EditError::Internal)?;
                    (v.id(), carry::entry_nodes(None, v))
                }
                _ => {
                    debug_assert!(false, "inserted {} nodes", nodes.len());
                    return Err(EditError::Internal);
                }
            };
            d.tail_intend(new, reads_tail);
            let moved = d.check_window(window, &expect, &inner)?;
            Ok((new, moved, inner))
        })?;
        let outside = match payload {
            Payload::Removed(r) => r.outside.clone(),
            _ => Vec::new(),
        };
        self.carry_explicit(&moved, &outside, &inner, None);
        self.commit();
        Ok(new)
    }

    /// The appended text's own last comments, after the collection (inside
    /// the caller's [`Document::atomic`]).
    fn place_tail(&mut self, coll_id: TreeId, tail: Option<Tail>) -> Result<(), EditError> {
        let tail = match tail {
            Some(Tail::Lines(lines)) => {
                self.t_insert(Put::LineAfter(coll_id), lines)?;
                None
            }
            Some(Tail::Before(f)) => Some(f),
            None => None,
        };
        if let Some(tail) = tail {
            let after_coll = self
                .tree()
                .get(coll_id)
                .ok()
                .and_then(place::next_token)
                .map(|t| t.id());
            match after_coll {
                Some(t) => self.t_insert(Put::Before(t), tail)?,
                None => self.t_insert(Put::After(coll_id), tail)?,
            };
        }
        Ok(())
    }

    /// Inserts into a single-pair flow mapping (`[a: b]`, YAML 1.2.2
    /// section 7.4.1): the pair is rewritten as a flow mapping `{a: b, …}`
    /// in its place (must work). The pair's key and value are
    /// new nodes (their old handles go stale).
    fn op_pair_insert(
        &mut self,
        pair: TreeId,
        key: Option<&str>,
        at: Where,
        payload: Payload<'_>,
        flat: bool,
    ) -> Result<TreeId, EditError> {
        let (frag, append, window, expect, outside) = {
            let p = self.tree().get(pair).map_err(|_| EditError::StaleNode)?;
            self.check_region(&[p], p.parent())?;
            // The comments around the pair
            // keep their owners (the pair's own are the new mapping's).
            let (window, expect, outside) = self.pair_carry(p)?;
            let append = at == Where::Append;
            let point = if append {
                Self::end_of(p)
            } else {
                p.preorder_index()
            };
            match payload {
                Payload::Removed(r) => {
                    if r.doc != self.id {
                        return Err(EditError::WrongDocument);
                    }
                    let k = r.key.as_deref().unwrap_or_default();
                    let m = NodeRef::new(self, p).as_mapping();
                    let own = m.and_then(|m| m.own_value(k));
                    let merge = r.merge
                        && m.map(|m| m.has_merge_key())
                            .transpose()
                            .map_err(tag_internal)?
                            .unwrap_or(false);
                    let equal = !r.merge
                        && r.key_value
                            .as_ref()
                            .is_some_and(|v| m.is_some_and(|m| m.has_key_value(v)));
                    if own.is_some() || merge || equal {
                        return Err(EditError::KeyExists { at: Path::new() });
                    }
                    self.check_inside(p, &r.external)?;
                    self.check_uses(&r.external, point)?;
                    self.check_defines(
                        &r.anchors,
                        &r.referenced,
                        r.moving.then_some(r.referenced_runs.as_slice()),
                        point,
                    )?;
                }
                Payload::Alias(name, target) | Payload::AliasEntry(_, name, target) => {
                    self.check_uses(&[(name.to_owned(), target)], point)?;
                }
                Payload::Merge(targets) => self.check_uses(targets, point)?,
                Payload::Value(_) | Payload::Entry(..) | Payload::Null(_) => {}
            }
            let entry = self.payload_text(p, Coll::FlowMap, 0, key, payload, flat)?;
            let old = p.text();
            let text = if append {
                format!("{{{old}, {entry}}}")
            } else {
                format!("{{{entry}, {old}}}")
            };
            let frag = self.slot_fragment(SlotKind::Flow, &text)?.0;
            (frag, append, window, expect, outside)
        };
        let (new, moved, inner) = self.atomic(|d| {
            let ins = d.t_replace(pair, pair, frag, false)?;
            let m = d.tree().get(ins.last).map_err(|_| EditError::Internal)?;
            let items: Vec<TreeId> = m
                .children()
                .filter(|c| c.kind().is_yaml_node())
                .map(|c| c.id())
                .collect();
            let v = if append { items.last() } else { items.get(1) };
            let new = v.copied().ok_or(EditError::Internal)?;
            let skip = if append {
                items.len().saturating_sub(2)
            } else {
                0
            };
            let inner = carry::pair_nodes(m, Some(skip));
            let moved = d.check_window(window, &expect, &inner)?;
            Ok((new, moved, inner))
        })?;
        self.carry_explicit(&moved, &outside, &inner, None);
        self.commit();
        Ok(new)
    }

    // ========================================================== remove

    /// Removes the entry of `coll` whose value (or item) is `value`, with
    /// exactly the comments the rulebook gives it (see
    /// `carry.rs`). Returns what was removed, and the collection's new node
    /// when it had to be rewritten (`{}`/`[]` for a block collection's
    /// only entry). `check_aliases`: refuse removing anchors that aliases
    /// outside still use (a move takes them along instead). `keep_lead`:
    /// the entry's leading comments stay, owned by the next entry or the
    /// collection's footer afterwards, or the removal is refused
    /// ([`Document::remove_keep_comments`]).
    fn op_remove(
        &mut self,
        coll_id: TreeId,
        value: TreeId,
        check_aliases: bool,
        keep_lead: bool,
    ) -> Result<(Removed, Option<TreeId>), EditError> {
        // When the next entry's leading comment lines would
        // read as the footer of the collection now above them (rule 6, by
        // their column), they are written at its column instead.
        match self.op_remove_with(coll_id, value, check_aliases, keep_lead, false) {
            Err(EditError::CommentReattaches { .. }) => {
                self.op_remove_with(coll_id, value, check_aliases, keep_lead, true)
            }
            r => r,
        }
    }

    fn op_remove_with(
        &mut self,
        coll_id: TreeId,
        value: TreeId,
        check_aliases: bool,
        keep_lead: bool,
        reindent: bool,
    ) -> Result<(Removed, Option<TreeId>), EditError> {
        self.prepare()?;
        struct Plan {
            removed: Removed,
            carry: Carry,
            /// The last entry's same-line comment, after the collection.
            outside: Option<(TreeId, TreeId)>,
            /// Lines right after the last entry that would read as content of
            /// the block scalar before it once the entry is gone.
            block_tail: Option<(TreeId, TreeId)>,
            only: bool,
            flow: bool,
            empty: (bool, Option<crate::value::Tag>),
            /// The next entry (its first element) and its column.
            next_at: Option<(TreeId, usize)>,
        }
        let plan = {
            let coll = self.tree().get(coll_id).map_err(|_| EditError::StaleNode)?;
            let kind = Coll::of(coll).ok_or(EditError::NotAMapping { at: Path::new() })?;
            let v = self.tree().get(value).map_err(|_| EditError::StaleNode)?;
            self.refuse_error_container(coll)?;
            let e = place::entry_of(coll, v)?;
            let prev = place::prev_entry(coll, &e)?;
            let next = place::next_entry(coll, &e)?;
            let mut near: Vec<TreeNode<'_>> = e.key.into_iter().collect();
            near.push(e.value);
            self.check_region(&near, Some(coll))?;
            let names = Names::of(e.first, e.value);
            let (name, referenced, referenced_runs) = self.outside_refs(&names, e.value, None);
            if let (true, Some(anchor)) = (check_aliases, name) {
                return Err(EditError::AnchorInUse {
                    anchor,
                    aliases: referenced.into_iter().map(NodeId).collect(),
                });
            }
            // Aliases inside whose anchor lies outside (a move keeps
            // their targets). Set membership, not a scan.
            let inside: HashSet<TreeId> = names.anchors.iter().map(|(_, id)| *id).collect();
            let (start, _) = self.doc_range();
            let mut external = Vec::new();
            for (name, alias) in &names.aliases {
                let at = self.tree().get(*alias).map_or(0, TreeNode::preorder_index);
                let target = self.anchors().nearest_before(self.tree(), name, start, at);
                if let Some(t) = target.filter(|t| !inside.contains(t)) {
                    external.push(((*name).to_owned(), t));
                }
            }
            let mut text = if kind.is_map() {
                place::range_text(e.first, e.value)
            } else {
                e.value.text().into_owned()
            };
            // A column only serves to re-indent text of
            // several lines (or block lines around it); finding it walks
            // back to the start of the line, O(the entries before it on a
            // line of flow text: minified JSON). A single-line flow entry
            // needs none.
            let one_line = kind.is_flow() && !text.contains(['\n', '\r']);
            // A flow entry of several lines finds it within
            // O(the entry) (`place::flow_col`).
            let start = if kind.is_map() { e.first } else { e.value };
            let col = match (one_line, kind.is_flow()) {
                (true, _) => 0,
                (false, true) => place::flow_col(start, &text),
                (false, false) => place::col_of(start),
            };
            let text_span = Span::new(
                if kind.is_map() {
                    e.first.span().start
                } else {
                    e.value.span().start
                },
                e.value.span().end,
            );
            let only = prev.is_none() && next.is_none();
            let flow = kind.is_flow();
            // Comment rule 5: the entry's same-line comment is its own and
            // goes with its text (inside the collection, or after it for
            // the last entry of a block collection). (not after
            // a value that ends its line, a block scalar: what follows is
            // on the next line.)
            let mut outside = None;
            if !flow && !ends_line(e.value) {
                if next.is_some() {
                    let mut at = e.value.next_sibling();
                    let mut tail = String::new();
                    while let Some(x) = at {
                        match x.kind() {
                            SyntaxKind::Whitespace => tail.push_str(&x.text()),
                            SyntaxKind::Comment => {
                                tail.push_str(&x.text());
                                text.push_str(&tail);
                                break;
                            }
                            // (White space ending the line is the entry's
                            // too.)
                            SyntaxKind::LineBreak => {
                                text.push_str(&tail);
                                break;
                            }
                            _ => break,
                        }
                        at = x.next_sibling();
                    }
                } else if let Some((a, b)) = trailing_comment(coll) {
                    text.push_str(&place::range_text(a, b));
                    outside = Some((a.id(), b.id()));
                } else if let Some(w) = place::next_token(coll).filter(|w| {
                    w.kind() == SyntaxKind::Whitespace
                        && place::next_token(*w).is_none_or(|b| b.kind() == SyntaxKind::LineBreak)
                }) {
                    text.push_str(&w.text());
                    outside = Some((w.id(), w.id()));
                }
            }
            // A block sequence item's separator after its `-`.
            let head = (kind == Coll::BlockSeq && e.first != e.value)
                .then(|| e.first.next_sibling())
                .flatten()
                .filter(|x| *x != e.value)
                .and_then(|x| {
                    // Trivia only (comment lines too, the value's
                    // leading comments, written back with it).
                    let last = e.value.prev_sibling()?;
                    let mut at = Some(x);
                    while let Some(t) = at {
                        if !t.kind().is_trivia() {
                            return None;
                        }
                        if t == last {
                            break;
                        }
                        at = t.next_sibling();
                    }
                    Some(place::range_text(x, last))
                });
            // A flow entry's separators, when they hold no comment.
            let only_space = |a: Option<TreeNode<'_>>, b: Option<TreeNode<'_>>| {
                let (a, b) = (a?, b?);
                let t = place::range_text(a, b);
                t.chars()
                    .all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | ','))
                    .then_some(t)
            };
            let (sep_before, sep_after) = if flow {
                match (&prev, &next) {
                    (_, Some(n)) => (
                        None,
                        only_space(e.value.next_sibling(), n.first.prev_sibling())
                            .filter(|t| e.value.next_sibling() != Some(n.first) && !t.is_empty()),
                    ),
                    (Some(p), None) => (
                        only_space(p.value.next_sibling(), e.first.prev_sibling())
                            .filter(|_| p.value.next_sibling() != Some(e.first)),
                        None,
                    ),
                    (None, None) => (None, None),
                }
            } else {
                (None, None)
            };
            let doc = self.doc_node().map(TreeNode::id);
            let rehome = match (keep_lead, &next) {
                (false, _) => None,
                (true, Some(n)) => {
                    let owner = NodeId(n.key.unwrap_or(n.value).id());
                    Some((super::CommentOwner::Leading(owner), doc))
                }
                // A root collection's last entry: the
                // document footer (the call is the explicit request).
                (true, None)
                    if !flow
                        && !only
                        && coll
                            .parent()
                            .is_some_and(|p| p.kind() == SyntaxKind::Document) =>
                {
                    Some((super::CommentOwner::DocumentFooter, doc))
                }
                (true, None) if !flow && !only => {
                    Some((super::CommentOwner::Footer(NodeId(coll.id())), doc))
                }
                (true, None) => Some((super::CommentOwner::DocumentHeader, None)),
            };
            let around = (only && !flow).then_some(coll);
            let mut carry = self.entry_carry(coll, &e, prev.is_some(), rehome, around)?;
            // The last entry after a block scalar: lines under it that are
            // indented as the scalar's content (a comment, or white space
            // wider than its indentation) would become part of the scalar's
            // text, so they go with the entry (after the footers it takes
            // anyway).
            let covered = match &carry.tail {
                Some((_, l, _, shifted)) => Some((
                    self.tree().get(*l).map_err(|_| EditError::StaleNode)?,
                    *shifted,
                )),
                None => None,
            };
            let tail_lines = match (&prev, &next) {
                (Some(p), None) if !flow => block_tail(p.value, e.value, covered),
                _ => None,
            };
            let block_tail = tail_lines.as_ref().map(|t| (t.0, t.1));
            // The comments removed with those lines are the entry's now:
            // `Removed` keeps them, with the owner the rules gave them, and
            // they are no longer expected to stay where their owner had them.
            let mut taken: Vec<Expect> = Vec::new();
            if let Some(t) = &tail_lines {
                for c in &t.3 {
                    let at = carry
                        .rest
                        .iter()
                        .position(|x| x.old == Some(*c))
                        .ok_or(EditError::NotWellFormed { at: Path::new() })?;
                    let x = carry.rest.remove(at);
                    if !matches!(x.want, Want::Rel(_)) {
                        return Err(EditError::NotWellFormed { at: Path::new() });
                    }
                    taken.push(x);
                }
            }
            if keep_lead && (only || flow) && carry.kept_lead > 0 {
                return Err(unsupported(
                    "the leading comments of a flow entry or of a collection's only entry \
                     have no entry or block footer to go to",
                ));
            }
            if only && !flow {
                // The collection becomes `{}`/`[]`, a new node: comments it
                // owned fall to the rules there.
                let own: HashSet<TreeId> = std::iter::once(coll.id()).collect();
                for x in &mut carry.rest {
                    if let Want::Rel(Rel::Fixed((o, _))) = x.want {
                        if carry::owner_node(o).is_some_and(|(_, n)| own.contains(&n.0)) {
                            x.want = Want::Collapsed;
                        }
                    }
                }
            }
            // The first entry: the blank lines above the next entry's own
            // lines, which then join those above the collection.
            let next_blank = match (&prev, &next) {
                (None, Some(n)) if !flow => {
                    let first = cm::first_token(n.first).ok_or(EditError::Internal)?;
                    let owner = n.key.unwrap_or(n.value).id();
                    let lead = carry.rest.iter().find_map(|x| match x.want {
                        Want::Rel(Rel::Fixed((super::CommentOwner::Leading(o), _)))
                            if o.0 == owner =>
                        {
                            x.old.and_then(|c| self.tree().get(c).ok())
                        }
                        _ => None,
                    });
                    let head = carry::line_head(lead.unwrap_or(first));
                    if cm::line_start(head) {
                        self.blank_lines_above(head)
                    } else {
                        0
                    }
                }
                _ => 0,
            };
            let key = match e.key {
                Some(k) => Some(
                    NodeRef::new(self, k)
                        .as_scalar()
                        .map(|s| s.value().into_owned())
                        .ok_or_else(|| unsupported("removing an entry with a collection key"))?,
                ),
                None => None,
            };
            let merge = match e.key {
                Some(k) => NodeRef::new(self, k).is_merge_key().map_err(tag_internal)?,
                None => false,
            };
            let key_value = e
                .key
                .and_then(|k| NodeRef::new(self, k).as_scalar()?.resolved().ok());
            let mut anchors: Vec<String> =
                names.anchors.iter().map(|(n, _)| (*n).to_owned()).collect();
            anchors.dedup();
            let lines = |t: Option<&String>, shifted: bool| {
                t.map_or_else(Vec::new, |t| {
                    let t = if shifted {
                        t.trim_start_matches(['\n', '\r'])
                    } else {
                        t.as_str()
                    };
                    carry::split_lines(t)
                })
            };
            let lead = lines(carry.lead.as_ref().map(|l| &l.2), false);
            let mut tail = lines(
                carry.tail.as_ref().map(|l| &l.2),
                carry.tail.as_ref().is_some_and(|l| l.3),
            );
            if let Some((_, _, extra, _)) = tail_lines {
                tail.extend(extra);
            }
            let tree = self.tree();
            // Another owner's comments between the entry and its `,` (rule
            // 9): the removal writes them back as lines of their own (see
            // `remove_flow_entry`), so the entry's place cannot be given
            // back byte for byte either.
            // Set membership, never a scan.
            let owned_set: HashSet<TreeId> = carry.owned.iter().map(|x| x.0).collect();
            let foreign_before_comma = || {
                let mut at = e.value.next_sibling();
                while let Some(x) = at {
                    match x.kind() {
                        SyntaxKind::Comment if !owned_set.contains(&x.id()) => {
                            return next.is_some() || prev.is_none();
                        }
                        SyntaxKind::FlowEntry => return false,
                        k if k.is_trivia() => {}
                        _ => return false,
                    }
                    at = x.next_sibling();
                }
                false
            };
            let loose = flow
                && (carry.owned.iter().any(|(c, ..)| {
                    tree.get(*c).is_ok_and(|c| {
                        let s = c.span();
                        s.start < text_span.start || s.end > text_span.end
                    })
                }) || foreign_before_comma());
            let reads_tail = self
                .tail_reader()
                .is_some_and(|r| self.holds(e.value.id(), r));
            let removed = Removed {
                doc: self.id,
                text,
                col,
                // (As `col`: only lines around the entry use it.)
                // (A flow entry's start is the text's: `col`.)
                entry_col: if flow && lead.is_empty() && tail.is_empty() {
                    col
                } else {
                    place::col_of(e.first)
                },
                flow,
                key,
                merge,
                key_value,
                kind: NodeRef::new(self, e.value).kind(),
                lead,
                tail,
                loose,
                head,
                sep_after,
                sep_before,
                next_blank,
                next_lead: None,
                flow_lines: None,
                comments: carry
                    .owned
                    .iter()
                    .map(|x| x.1.clone())
                    .chain(taken.iter().map(|x| x.text.clone()))
                    .collect(),
                ids: carry
                    .owned
                    .iter()
                    .map(|x| CommentId::of(x.0))
                    .chain(taken.iter().filter_map(|x| x.old.map(CommentId::of)))
                    .collect(),
                owners: carry
                    .owned
                    .iter()
                    .map(|x| x.2)
                    .chain(taken.iter().filter_map(|x| match x.want {
                        Want::Rel(r) => Some(r),
                        _ => None,
                    }))
                    .collect(),
                explicit: carry
                    .owned
                    .iter()
                    .map(|x| x.3)
                    .chain(taken.iter().map(|x| x.explicit))
                    .collect(),
                outside: carry.outside_explicit.clone(),
                anchors,
                external,
                referenced,
                referenced_runs,
                moving: false,
                reads_tail,
            };
            Plan {
                removed,
                carry,
                outside,
                block_tail,
                only,
                flow,
                // The document's tag table (a `%TAG` prefix is
                // shared, never copied).
                empty: (
                    kind.is_map(),
                    self.shared.tags.value_tag(coll).map_err(tag_internal)?,
                ),
                next_at: next
                    .filter(|_| reindent && !flow)
                    .map(|n| (n.first.id(), place::col_of(n.first))),
            }
        };
        let Plan {
            removed,
            carry,
            outside,
            block_tail,
            only,
            flow,
            empty,
            next_at,
        } = plan;
        let window = carry.window.ok_or(EditError::Internal)?;
        let owned: Vec<TreeId> = carry.owned.iter().map(|x| x.0).collect();
        // Membership in O(1).
        let owned_set: HashSet<TreeId> = owned.iter().copied().collect();
        let mut next_lead = None;
        let mut flow_lines = None;
        let (new, moved) = self.atomic(|d| {
            // The entry's own lines after it, then above it (whole lines of
            // trivia, each a local edit), then the entry.
            if let Some((f, l, _, _)) = carry.tail {
                d.t_remove(f, l)?;
            }
            if let Some((f, l, _)) = carry.lead {
                d.t_remove(f, l)?;
            }
            if let Some((f, l)) = block_tail {
                d.t_remove(f, l)?;
            }
            let new = if flow {
                // Limitation S3: comments of other owners around
                // the entry that the flow layout cannot keep in place after
                // the removal are this limitation, refused by name.
                flow_lines =
                    d.remove_flow_entry(coll_id, value, &owned_set)
                        .map_err(|e| match e {
                            EditError::NotWellFormed { .. }
                                if d.foreign_comments(coll_id, &owned_set) =>
                            {
                                unsupported(FLOW_REMOVE_LIMIT)
                            }
                            e => e,
                        })?;
                None
            } else if only {
                if let Some((a, b)) = outside {
                    d.t_remove(a, b)?;
                }
                // The collection becomes `{}`/`[]`, keeping its anchor and
                // custom tag (the entry's anchors were checked above).
                let (is_map, tag) = &empty;
                let mut v = if *is_map {
                    Value::Mapping(Mapping::new())
                } else {
                    Value::Sequence(Vec::new().into())
                };
                // Only the Core defaults a collection has without a tag
                // (`!!map`, `!!seq`) may be dropped; `!!omap`, `!!set`,
                // `!!pairs` and every other tag are kept.
                if let Some(t) = tag
                    .as_ref()
                    .filter(|t| **t != "tag:yaml.org,2002:map" && **t != "tag:yaml.org,2002:seq")
                {
                    v = Value::Tagged(Box::new(crate::value::TaggedValue {
                        tag: t.clone(),
                        value: v,
                    }));
                }
                // A comment inside the collection that is not the entry's
                // (on the line of its properties: the parent entry's, rule
                // 5) is written back after `{}`/`[]`, on that line.
                let inside: Vec<String> = {
                    let c = d.tree().get(coll_id).map_err(|_| EditError::StaleNode)?;
                    c.descendants()
                        .filter(|x| x.kind() == SyntaxKind::Comment && !owned_set.contains(&x.id()))
                        .map(|x| x.text().into_owned())
                        .collect()
                };
                if inside.len() > 1 {
                    return Err(unsupported(
                        "comments of other owners between a collection's properties and its \
                         only entry",
                    ));
                }
                let new = d.op_replace_keeping(coll_id, &v, None, true)?;
                if let Some(c) = inside.first() {
                    let frag = d.trivia_fragment(&format!(" {c}"))?;
                    let puts = {
                        let n = d.tree().get(new).map_err(|_| EditError::Internal)?;
                        super::comment_edit::after_chain(n)
                    };
                    let mut done = Err(bad());
                    for p in puts {
                        done = d.t_insert(p, frag.clone());
                        if done.is_ok() {
                            break;
                        }
                    }
                    done?;
                }
                Some(new)
            } else {
                next_lead = d.remove_block_entry(coll_id, value, outside)?;
                None
            };
            if let Some((n, col)) = next_at {
                d.reindent_lead_lines(n, col)?;
            }
            d.keep_tail_values()?;
            let moved = d.check_window(window, &carry.rest, &[])?;
            Ok((new, moved))
        })?;
        self.forget_explicit(&owned);
        self.carry_explicit(&moved, &[], &[], None);
        // The comments outside the entry it recorded may have been
        // rewritten (relocated) by the removal: follow them.
        let mut removed = removed;
        removed.next_lead = next_lead;
        removed.flow_lines = flow_lines;
        // Indexed once, not searched per owner.
        if !removed.outside.is_empty() {
            let renamed: std::collections::HashMap<TreeId, TreeId> = moved
                .iter()
                .filter_map(|(o, n, _)| Some(((*o)?, *n)))
                .collect();
            for (c, _) in &mut removed.outside {
                if let Some(new) = renamed.get(c) {
                    *c = *new;
                }
            }
        }
        self.commit();
        Ok((removed, new))
    }

    /// The lines of block collection `coll`'s entry `value`, removed (its
    /// comment lines above and below went already): not its only entry.
    /// Where the parser puts trivia (C4: never at a collection's end or
    /// start) decides the pieces; see the cases below.
    fn remove_block_entry(
        &mut self,
        coll_id: TreeId,
        value: TreeId,
        outside: Option<(TreeId, TreeId)>,
    ) -> Result<Option<(String, String)>, EditError> {
        /// One way to remove the entry.
        enum Step {
            /// These siblings.
            Range(TreeId, TreeId),
            /// These siblings through their line's end, trivia put where
            /// the parser puts it (`remove_to_line_end`).
            ToLineEnd(TreeId, TreeId, Option<Fragment>),
            /// These siblings, then comment lines written back (the next
            /// entry's, moved out of the collection) after this line end
            /// (`None`: the start of the text).
            Relocate(TreeId, TreeId, String, Option<TreeId>),
            /// These siblings, then the white space between the parent's
            /// `- `/`? ` and the collection replaced by this trivia (a line
            /// break, the next entry's comment lines, its indentation): the
            /// collection no longer starts on the parent's line.
            Compact(TreeId, TreeId, TreeId, String),
            /// These siblings, then trivia written back after the
            /// collection (the previous entry's line end, at the end of
            /// the text).
            AfterColl(TreeId, TreeId, Fragment),
        }
        if let Some((a, b)) = outside {
            self.t_remove(a, b)?;
        }
        let mut stripped = None;
        let step = {
            let tree = self.tree();
            let coll = tree.get(coll_id).map_err(|_| EditError::StaleNode)?;
            let v = tree.get(value).map_err(|_| EditError::StaleNode)?;
            let e = place::entry_of(coll, v)?;
            let prev = place::prev_entry(coll, &e)?;
            let next = place::next_entry(coll, &e)?;
            let starts_line = cm::first_token(e.first).is_some_and(cm::line_start);
            // The entry's line head inside the collection: its indentation
            // (a sibling) or its first element.
            let head = match e.first.prev_sibling() {
                Some(w) if starts_line && w.kind() == SyntaxKind::Whitespace => w,
                _ => e.first,
            };
            // The element ending the entry's last line inside the
            // collection: its line break, or its value's block scalar.
            let line_end = || -> Option<TreeNode<'_>> {
                if ends_line(e.value) {
                    return Some(e.value);
                }
                let mut at = e.value.next_sibling();
                while let Some(x) = at {
                    match x.kind() {
                        SyntaxKind::Whitespace | SyntaxKind::Comment => {}
                        SyntaxKind::LineBreak => return Some(x),
                        _ => return None,
                    }
                    at = x.next_sibling();
                }
                None
            };
            match (prev, next) {
                // A middle entry: its whole lines.
                (Some(_), Some(_)) => {
                    let end = line_end().ok_or_else(bad)?;
                    let above = head.prev_sibling().ok_or_else(bad)?;
                    if !(head != e.first || starts_line)
                        || !(above.kind() == SyntaxKind::LineBreak || ends_line(above))
                    {
                        return Err(bad());
                    }
                    Step::Range(head.id(), end.id())
                }
                // The first entry: through the next entry's indentation
                // (the collection starts at the next entry, whose
                // indentation is then the one before the collection); the
                // next entry's comment lines move out with it.
                (None, Some(n)) => {
                    let n_head = n.first.prev_sibling().ok_or_else(bad)?;
                    let end = line_end().ok_or_else(bad)?;
                    let from = end.next_sibling().ok_or_else(bad)?;
                    let n_line = if n_head.kind() == SyntaxKind::Whitespace {
                        n_head
                    } else {
                        n.first
                    };
                    if from == n_line {
                        Step::Range(e.first.id(), n_head.id())
                    } else {
                        let pre = n_line.prev_sibling().ok_or_else(bad)?;
                        let original = place::range_text(from, pre);
                        // Comment rules 2 and 3: at the
                        // top of a document a blank line between the next
                        // entry's leading comments and it would make them
                        // the header; they become the first entry's
                        // leading comments without those blank lines (the
                        // `Removed` records them for a re-insertion).
                        let top = coll
                            .parent()
                            .is_some_and(|p| p.kind() == SyntaxKind::Document);
                        let lines = if top {
                            carry::strip_blank_after_comment(&original)
                        } else {
                            original.clone()
                        };
                        if lines != original {
                            stripped = Some((lines.clone(), original));
                        }
                        let first = cm::first_token(e.first).ok_or_else(bad)?;
                        if starts_line {
                            let above = cm::before_indent(first).map(|t| t.id());
                            Step::Relocate(e.first.id(), n_head.id(), lines, above)
                        } else {
                            // The collection starts on its parent's `- `/`? `
                            // line; the next entry's comment lines go above it,
                            // which then starts a line of its own (`- - a⏎  #
                            // c⏎  - b` → `-⏎  # c⏎  - b`).
                            let ws = place::prev_token(first)
                                .filter(|w| w.kind() == SyntaxKind::Whitespace)
                                .ok_or_else(bad)?;
                            let text =
                                format!("{}{lines}{}", self.nl(), sp(place::col_of(n.first)));
                            Step::Compact(e.first.id(), n_head.id(), ws.id(), text)
                        }
                    }
                }
                // The last entry: the collection ends at the previous
                // value, so what lies between it and the entry (its line
                // end, the footers of the collections inside it) moves
                // after the collection, where the parser puts it.
                (Some(p), None) => {
                    let first = p.value.next_sibling().ok_or_else(bad)?;
                    if first == head {
                        Step::ToLineEnd(head.id(), e.value.id(), None)
                    } else {
                        let lb = head
                            .prev_sibling()
                            .filter(|x| x.kind() == SyntaxKind::LineBreak)
                            .ok_or_else(bad)?;
                        let eof = !ends_line(e.value) && place::next_token(e.value).is_none();
                        if first == lb && !ends_line(e.value) {
                            Step::Range(lb.id(), e.value.id())
                        } else if eof {
                            let keep = lb.prev_sibling().ok_or_else(bad)?;
                            let frag = Self::extract(tree, first, keep)?;
                            Step::AfterColl(first.id(), e.value.id(), frag)
                        } else {
                            let frag = Self::extract(tree, first, lb)?;
                            Step::ToLineEnd(first.id(), e.value.id(), Some(frag))
                        }
                    }
                }
                (None, None) => return Err(EditError::Internal),
            }
        };
        match step {
            Step::Range(a, b) => self.t_remove(a, b)?,
            Step::ToLineEnd(a, b, f) => self.t_remove_to_line_end(a, b, f)?,
            Step::Relocate(a, b, lines, above) => {
                self.t_remove(a, b)?;
                self.insert_trivia_lines(above, &lines)?;
            }
            Step::Compact(a, b, ws, text) => {
                self.t_remove(a, b)?;
                let frag = self.trivia_fragment(&text)?;
                self.t_replace(ws, ws, frag, false)?;
            }
            Step::AfterColl(a, b, frag) => {
                self.t_remove(a, b)?;
                let after = self
                    .tree()
                    .get(coll_id)
                    .ok()
                    .and_then(place::next_token)
                    .map(|t| t.id());
                match after {
                    Some(t) => {
                        self.t_insert(Put::Before(t), frag)?;
                    }
                    None => {
                        // At the end of the text,
                        // the trivia goes where the parser puts it: after
                        // the collection, or after the node or document
                        // ending with it (a root collection's: at stream
                        // level, after its document).
                        let puts: Vec<TreeId> = {
                            let c = self.tree().get(coll_id).map_err(|_| EditError::StaleNode)?;
                            std::iter::once(c)
                                .chain(c.ancestors())
                                .take_while(|x| x.kind() != SyntaxKind::Stream)
                                .map(TreeNode::id)
                                .collect()
                        };
                        let mut last = Err(bad());
                        for p in puts {
                            last = self.t_insert(Put::After(p), frag.clone());
                            if !matches!(last, Err(EditError::NotWellFormed { .. })) {
                                break;
                            }
                        }
                        last?;
                    }
                }
            }
        }
        Ok(stripped)
    }

    /// The comment lines right above node `n` (its leading ones, after a
    /// removal) written again at column `col`: same texts, blank
    /// lines kept, new handles. Nothing when there are none.
    fn reindent_lead_lines(&mut self, n: TreeId, col: usize) -> Result<(), EditError> {
        let (range, text) = {
            let node = self.tree().get(n).map_err(|_| EditError::StaleNode)?;
            let first = cm::first_token(node).ok_or(EditError::Internal)?;
            let head = carry::line_head(first);
            if !cm::line_start(head) {
                return Ok(());
            }
            // Back over whole trivia lines (a loop).
            let mut from: Option<TreeNode<'_>> = None;
            let mut at = place::prev_token(head);
            let last = at.filter(|b| b.kind() == SyntaxKind::LineBreak);
            while let Some(b) = at.filter(|b| b.kind() == SyntaxKind::LineBreak) {
                let mut x = place::prev_token(b);
                let mut start = None;
                while let Some(t) =
                    x.filter(|t| matches!(t.kind(), SyntaxKind::Whitespace | SyntaxKind::Comment))
                {
                    start = Some(t);
                    x = place::prev_token(t);
                }
                match (start, x) {
                    // A whole trivia line (or a blank one).
                    (s, Some(p)) if p.kind() == SyntaxKind::LineBreak => {
                        from = Some(s.unwrap_or(b));
                        at = Some(p);
                    }
                    _ => break,
                }
            }
            let (Some(a), Some(b)) = (from, last) else {
                return Ok(());
            };
            if carry::siblings(a, b).is_none() {
                return Ok(());
            }
            let old = place::range_text(a, b);
            let mut out = String::with_capacity(old.len());
            for l in carry::split_keep(&old) {
                let t = l.trim_start_matches([' ', '\t']);
                if t.starts_with('#') {
                    out.push_str(&sp(col));
                    out.push_str(t);
                } else {
                    out.push_str(l);
                }
            }
            if out == old {
                return Ok(());
            }
            ((a.id(), b.id()), out)
        };
        let frag = self.trivia_fragment(&text)?;
        self.t_replace(range.0, range.1, frag, false)?;
        Ok(())
    }

    /// Whole lines of trivia `lines` (each ending with a line break) as
    /// the lines after the line `above` ends (`None`: the start of the
    /// text), where the parser puts them.
    pub(super) fn insert_trivia_lines(
        &mut self,
        above: Option<TreeId>,
        lines: &str,
    ) -> Result<Inserted, EditError> {
        let puts = {
            let e = match above {
                Some(e) => Some(self.tree().get(e).map_err(|_| EditError::StaleNode)?),
                None => None,
            };
            super::comment_edit::after_line(self.tree().root(), e)
        };
        // Lines carried from the document as they
        // are, which need not read as trivia again.
        let frag = self.carried_fragment(lines)?;
        let mut last = bad();
        for p in puts {
            match self.t_insert(p, frag.clone()) {
                Ok(i) => return Ok(i),
                Err(e @ (EditError::NotWellFormed { .. } | EditError::ReadOnly { .. })) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// Whether flow collection `coll` holds a comment outside `mine`
    /// (names the S3 removal limitation).
    fn foreign_comments(&self, coll: TreeId, mine: &HashSet<TreeId>) -> bool {
        self.tree().get(coll).is_ok_and(|c| {
            c.descendants()
                .any(|x| x.kind() == SyntaxKind::Comment && !mine.contains(&x.id()))
        })
    }

    /// Removes flow collection `coll`'s entry `value` with the comments
    /// `mine` (its own, comment rules 9 and 5), and nothing else: whole lines
    /// when it has its lines to itself, else from its first comment or
    /// token to its `,` (or, for the last entry, from the `,` before it).
    fn remove_flow_entry(
        &mut self,
        coll_id: TreeId,
        value: TreeId,
        mine: &HashSet<TreeId>,
    ) -> Result<Option<(String, u8)>, EditError> {
        let (range, comma, extra, lines) = {
            let tree = self.tree();
            let coll = tree.get(coll_id).map_err(|_| EditError::StaleNode)?;
            let v = tree.get(value).map_err(|_| EditError::StaleNode)?;
            let e = place::entry_of(coll, v)?;
            let prev = place::prev_entry(coll, &e)?;
            let next = place::next_entry(coll, &e)?;
            // The entry's leading comments (siblings before it, after the
            // previous `,` or bracket).
            let mut lo = e.first;
            let mut at = e.first.prev_sibling();
            while let Some(x) = at.filter(|x| x.kind().is_trivia()) {
                if x.kind() == SyntaxKind::Comment && mine.contains(&x.id()) {
                    lo = x;
                }
                at = x.prev_sibling();
            }
            // Its `,` and trailing comments; the next entry's comments met
            // before that `,` (rule 9: `,` skipped).
            let mut hi = e.value;
            let mut foreign = Vec::new();
            let mut at = e.value.next_sibling();
            while let Some(x) = at {
                match x.kind() {
                    // Its `,` (a last entry's trailing one too: the one
                    // before it then stays).
                    SyntaxKind::FlowEntry => {
                        hi = x;
                        break;
                    }
                    SyntaxKind::Comment if mine.contains(&x.id()) => hi = x,
                    SyntaxKind::Comment => foreign.push(x),
                    k if k.is_trivia() => {}
                    _ => break,
                }
                at = x.next_sibling();
            }
            if hi.kind() == SyntaxKind::FlowEntry {
                // Comments after the `,` on its line are the entry's too.
                let mut at = hi.next_sibling();
                while let Some(x) = at.filter(|x| x.kind().is_trivia()) {
                    match x.kind() {
                        SyntaxKind::Comment if mine.contains(&x.id()) => hi = x,
                        SyntaxKind::LineBreak => break,
                        _ => {}
                    }
                    at = x.next_sibling();
                }
            }
            if !foreign.is_empty() && hi.kind() == SyntaxKind::FlowEntry {
                // They lie between the entry and its `,`: the entry and the
                // `,` go, and they are written back as lines of their own,
                // still above the next entry (each rewritten).
                let start = match lo.prev_sibling() {
                    Some(w) if w.kind() == SyntaxKind::Whitespace => w,
                    _ => lo,
                };
                let at_line = start
                    .prev_sibling()
                    .is_some_and(|b| b.kind() == SyntaxKind::LineBreak);
                let nl = self.nl();
                let mut text = String::new();
                for (i, f) in foreign.iter().enumerate() {
                    if i > 0 || !at_line {
                        text.push_str(nl);
                    }
                    if cm::line_start(*f) {
                        text.push_str(&cm::line_prefix(*f));
                    }
                    text.push_str(&f.text());
                }
                text.push_str(nl);
                let (a, b) = (start.id(), hi.id());
                let frag = self.trivia_fragment(&text)?;
                self.t_replace(a, b, frag, false)?;
                return Ok(None);
            }
            // Whole lines when the entry has them to itself.
            let lo_head = match lo.prev_sibling() {
                Some(w) if w.kind() == SyntaxKind::Whitespace && cm::line_start(w) => Some(w),
                Some(b) if b.kind() == SyntaxKind::LineBreak => Some(lo),
                _ => None,
            };
            let hi_break = {
                let mut at = hi.next_sibling();
                while let Some(w) = at.filter(|w| w.kind() == SyntaxKind::Whitespace) {
                    at = w.next_sibling();
                }
                at.filter(|b| b.kind() == SyntaxKind::LineBreak)
            };
            let own_lines = lo_head.zip(hi_break);
            // The range starts right after the previous value (its `,`).
            let mut after_prev = false;
            // The column-0 comment lines of the case below,
            // as written, for a re-insertion before the next entry (mode 3).
            let mut col0_lines: Option<(String, usize, usize)> = None;
            let (range, comma) = match (&prev, &next) {
                (Some(p), None) if hi.kind() != SyntaxKind::FlowEntry => {
                    let between: Vec<TreeNode<'_>> = {
                        let mut out = Vec::new();
                        let mut at = p.value.next_sibling();
                        while let Some(x) = at.filter(|x| *x != lo) {
                            out.push(x);
                            at = x.next_sibling();
                        }
                        out
                    };
                    let comments = between.iter().any(|x| x.kind() == SyntaxKind::Comment);
                    let comma_at = between.iter().find(|x| x.kind() == SyntaxKind::FlowEntry);
                    if comments {
                        // The previous entry's comments stay: the entry's
                        // own lines (or tokens) go, then the `,` before it.
                        let comma = comma_at.map(|x| x.id());
                        let (a, b) = own_lines.unwrap_or((lo, hi));
                        ((a.id(), b.id()), comma)
                    } else {
                        let first = p.value.next_sibling().ok_or(EditError::Internal)?;
                        after_prev = true;
                        ((first.id(), hi.id()), None)
                    }
                }
                _ => match own_lines {
                    Some((a, b)) => ((a.id(), b.id()), None),
                    // Leading comment lines at column 0 above
                    // an entry that starts an indented line of its own
                    // (`- [⏎# c⏎  b, c]`): the entry's text goes from its own
                    // token on, so the next entry keeps that indentation
                    // (at column 0 it would end the enclosing block
                    // collection), and the comment lines go as lines of
                    // their own (below, with the entry's other comments).
                    None if lo != e.first
                        && lo_head == Some(lo)
                        && e.first.prev_sibling().is_some_and(|w| {
                            w.kind() == SyntaxKind::Whitespace && cm::line_start(w)
                        }) =>
                    {
                        let mut end = hi;
                        if let Some(n) = &next {
                            let mut at = hi.next_sibling();
                            let mut last = None;
                            while let Some(x) = at.filter(|x| *x != n.first) {
                                if x.kind() != SyntaxKind::Whitespace {
                                    last = None;
                                    break;
                                }
                                last = Some(x);
                                at = x.next_sibling();
                            }
                            if let Some(l) = last {
                                end = l;
                            }
                            // The lines from `lo` through the line break
                            // before the entry's indentation.
                            let lb = e
                                .first
                                .prev_sibling()
                                .and_then(TreeNode::prev_sibling)
                                .filter(|b| b.kind() == SyntaxKind::LineBreak);
                            if let Some(lb) = lb {
                                col0_lines = Some((
                                    place::range_text(lo, lb),
                                    lo.preorder_index(),
                                    lb.preorder_index(),
                                ));
                            }
                        }
                        ((e.first.id(), end.id()), None)
                    }
                    None => {
                        // Through the white space and line breaks before
                        // the next entry, when it has no comments of its
                        // own there (they would change lines).
                        let mut end = hi;
                        if next.is_none() && prev.is_none() {
                            // The only entry: through the white space, line
                            // breaks and trailing `,` before the closing
                            // bracket (as the baseline wrote them), when no
                            // comment is there: two white space tokens would
                            // meet.
                            let mut at = hi.next_sibling();
                            while let Some(x) = at.filter(|x| {
                                matches!(
                                    x.kind(),
                                    SyntaxKind::Whitespace
                                        | SyntaxKind::LineBreak
                                        | SyntaxKind::FlowEntry
                                )
                            }) {
                                end = x;
                                at = x.next_sibling();
                            }
                            if at.is_some_and(|x| x.kind().is_trivia()) {
                                end = hi;
                            }
                        }
                        if let Some(n) = &next {
                            let mut at = hi.next_sibling();
                            let mut last = None;
                            while let Some(x) = at.filter(|x| *x != n.first) {
                                if !matches!(
                                    x.kind(),
                                    SyntaxKind::Whitespace | SyntaxKind::LineBreak
                                ) {
                                    last = None;
                                    break;
                                }
                                last = Some(x);
                                at = x.next_sibling();
                            }
                            if let Some(l) = last {
                                end = l;
                            }
                        }
                        // Two white space tokens would meet (`a , b , }`
                        // without `b ,`): the one after goes too.
                        let is_ws = |x: &TreeNode<'_>| x.kind() == SyntaxKind::Whitespace;
                        if lo.prev_sibling().is_some_and(|w| is_ws(&w)) {
                            if let Some(w) = end.next_sibling().filter(is_ws) {
                                end = w;
                            }
                        }
                        ((lo.id(), end.id()), None)
                    }
                },
            };
            // The entry's comments outside that range (before the `,`
            // that precedes it, rule 9): each on its own, with its line
            // when it is alone on it.
            let (ra, rb) = (
                tree.get(range.0).map_or(0, TreeNode::preorder_index),
                tree.get(range.1).map_or(0, TreeNode::preorder_index),
            );
            // From the entry's own comments (never a scan of the
            // collection), in document order.
            let mut own: Vec<TreeNode<'_>> = mine
                .iter()
                .filter_map(|c| tree.get(*c).ok())
                .filter(|x| x.parent() == Some(coll))
                .collect();
            own.sort_by_key(|x| x.preorder_index());
            let mut extra = Vec::new();
            for x in own {
                let i = x.preorder_index();
                if x.kind() == SyntaxKind::Comment && !(ra <= i && i <= rb) {
                    let ws = x
                        .prev_sibling()
                        .filter(|w| w.kind() == SyntaxKind::Whitespace);
                    let brk = x
                        .next_sibling()
                        .filter(|b| b.kind() == SyntaxKind::LineBreak);
                    let alone = cm::line_start(x);
                    extra.push(match (alone, brk) {
                        (true, Some(b)) => (ws.unwrap_or(x).id(), b.id()),
                        _ => (ws.unwrap_or(x).id(), x.id()),
                    });
                }
            }
            // Whole lines of its own, or the text after the previous value:
            // kept for a re-insertion (mode 0/1: lines without
            // or with its `,`; 2: after the previous value).
            let after = after_prev
                .then(|| {
                    let a = tree.get(range.0).ok()?;
                    let b = tree.get(range.1).ok()?;
                    Some((place::range_text(a, b), 2u8))
                })
                .flatten()
                .filter(|_| extra.is_empty());
            let lines = own_lines
                .filter(|(a, b)| (a.id(), b.id()) == range && extra.is_empty())
                .map(|(a, b)| {
                    let mut comma = false;
                    let mut at = Some(a);
                    while let Some(x) = at {
                        comma |= x.kind() == SyntaxKind::FlowEntry;
                        if x == b {
                            break;
                        }
                        at = x.next_sibling();
                    }
                    (place::range_text(a, b), u8::from(comma))
                })
                .or(after);
            // Mode 3: the entry's other comments are exactly those
            // lines (whole lines above it), and it has its `,`.
            let lines = match col0_lines {
                // (Comment lines only: a blank line among them stays in
                // place, so it would come back twice.)
                Some((text, a, b))
                    if hi.kind() == SyntaxKind::FlowEntry
                        && carry::split_lines(&text)
                            .iter()
                            .all(|l| l.trim_start().starts_with('#')) =>
                {
                    let within = extra.iter().all(|(x, y)| {
                        let at = |i: &TreeId| tree.get(*i).map_or(0, TreeNode::preorder_index);
                        a <= at(x) && at(y) <= b
                    });
                    if within { Some((text, 3)) } else { lines }
                }
                _ => lines,
            };
            (range, comma, extra, lines)
        };
        // The entry first, then its other comments, then the `,` before it
        // (each step reads as the tree it leaves).
        self.t_remove(range.0, range.1)?;
        for (a, b) in extra.into_iter().rev() {
            self.t_remove(a, b)?;
        }
        if let Some(c) = comma {
            self.t_remove(c, c)?;
        }
        Ok(lines)
    }

    /// Removes the entry of a single-pair flow mapping: the pair becomes
    /// an empty flow mapping `{}`. Its comments are the entry's.
    fn op_pair_remove(
        &mut self,
        pair: TreeId,
        check_aliases: bool,
    ) -> Result<(Removed, Option<TreeId>), EditError> {
        self.prepare()?;
        let (removed, window, rest) = {
            let p = self.tree().get(pair).map_err(|_| EditError::StaleNode)?;
            self.check_region(&[p], p.parent())?;
            let items: Vec<TreeNode<'_>> =
                p.children().filter(|c| c.kind().is_yaml_node()).collect();
            let (Some(k), Some(v)) = (items.first(), items.get(1)) else {
                return Err(EditError::Internal);
            };
            let names = Names::of(*k, *v);
            let (name, referenced, referenced_runs) = self.outside_refs(&names, p, None);
            if let (true, Some(anchor)) = (check_aliases, name) {
                return Err(EditError::AnchorInUse {
                    anchor,
                    aliases: referenced.into_iter().map(NodeId).collect(),
                });
            }
            let inside: HashSet<TreeId> = names.anchors.iter().map(|(_, id)| *id).collect();
            let (start, _) = self.doc_range();
            let external = names
                .aliases
                .iter()
                .filter_map(|(name, alias)| {
                    let at = self.tree().get(*alias).ok()?.preorder_index();
                    let t = self
                        .anchors()
                        .nearest_before(self.tree(), name, start, at)?;
                    (!inside.contains(&t)).then(|| ((*name).to_owned(), t))
                })
                .collect();
            let first = cm::first_token(p).ok_or(EditError::Internal)?;
            let last = cm::last_token(p).ok_or(EditError::Internal)?;
            let (wa, wb) = (carry::visible_before(first), carry::visible_after(last));
            let all = carry::window_comments(self.tree().root(), wa, wb);
            let owners = self.rule_owners(&all)?;
            let nodes: carry::IdMap<usize> = carry::entry_nodes(Some(*k), *v)
                .into_iter()
                .enumerate()
                .map(|(i, n)| (n, i))
                .collect();
            let (ps, pe) = (p.span().start, p.span().end);
            let (mut mine, mut rest) = (Vec::new(), Vec::new());
            for c in &all {
                let key = *owners.get(&c.id()).ok_or(EditError::Internal)?;
                let s = c.span();
                if ps <= s.start && s.end <= pe {
                    let rel = match carry::owner_node(key.0) {
                        Some((kd, n)) => nodes
                            .get(&n.0)
                            .map_or(Rel::Fixed(key), |i| Rel::Inner(kd, *i)),
                        None => Rel::Fixed(key),
                    };
                    let explicit =
                        self.explicit_owner(c.id())
                            .map(|o| match carry::owner_node(o) {
                                Some((kd, n)) => nodes
                                    .get(&n.0)
                                    .map_or(Rel::Fixed((o, key.1)), |i| Rel::Inner(kd, *i)),
                                None => Rel::Fixed((o, key.1)),
                            });
                    mine.push((c.id(), c.text().into_owned(), rel, explicit));
                } else {
                    // The pair's own comments are its entry's; outside it,
                    // its owner as a node (a leading comment of the item)
                    // is rewritten with it.
                    let want = match carry::owner_node(key.0) {
                        // (Or a node inside it.)
                        Some((_, n))
                            if self
                                .tree()
                                .get(n.0)
                                .is_ok_and(|x| x == p || x.ancestors().any(|a| a == p)) =>
                        {
                            Want::Rewritten
                        }
                        _ => Want::Rel(Rel::Fixed(key)),
                    };
                    rest.push(Expect {
                        text: c.text().into_owned(),
                        want,
                        old: Some(c.id()),
                        explicit: None,
                    });
                }
            }
            // A single-line pair needs no column.
            let text = p.text().into_owned();
            let col = if text.contains(['\n', '\r']) {
                place::flow_col(p, &text)
            } else {
                0
            };
            let removed = Removed {
                doc: self.id,
                text,
                col,
                entry_col: col,
                flow: true,
                key: Some(
                    NodeRef::new(self, *k)
                        .as_scalar()
                        .map(|s| s.value().into_owned())
                        .ok_or_else(|| unsupported("removing an entry with a collection key"))?,
                ),
                merge: NodeRef::new(self, *k)
                    .is_merge_key()
                    .map_err(tag_internal)?,
                key_value: NodeRef::new(self, *k)
                    .as_scalar()
                    .and_then(|s| s.resolved().ok()),
                kind: NodeRef::new(self, *v).kind(),
                lead: Vec::new(),
                tail: Vec::new(),
                loose: false,
                head: None,
                sep_after: None,
                sep_before: None,
                next_blank: 0,
                next_lead: None,
                flow_lines: None,
                comments: mine.iter().map(|x| x.1.clone()).collect(),
                ids: mine.iter().map(|x| CommentId::of(x.0)).collect(),
                owners: mine.iter().map(|x| x.2).collect(),
                explicit: mine.iter().map(|x| x.3).collect(),
                outside: Vec::new(),
                anchors: names.anchors.iter().map(|(n, _)| (*n).to_owned()).collect(),
                external,
                referenced,
                referenced_runs,
                moving: false,
                reads_tail: false,
            };
            let window = Window {
                a: wa.map(TreeNode::id),
                b: wb.map(TreeNode::id),
            };
            (removed, window, rest)
        };
        let (frag, _) = self.slot_fragment(SlotKind::Flow, "{}")?;
        let (ins, moved) = self.atomic(|d| {
            let ins = d.t_replace(pair, pair, frag, false)?;
            let moved = d.check_window(window, &rest, &[])?;
            Ok((ins, moved))
        })?;
        let gone: Vec<TreeId> = removed.ids.iter().map(|c| c.tree_id()).collect();
        self.forget_explicit(&gone);
        self.carry_explicit(&moved, &[], &[], None);
        self.commit();
        Ok((removed, Some(ins.last)))
    }

    /// Removes the entry of `coll` holding `value` (a pair, or any other
    /// collection).
    fn op_remove_any(
        &mut self,
        coll_id: TreeId,
        value: TreeId,
        check_aliases: bool,
        keep_lead: bool,
    ) -> Result<(Removed, Option<TreeId>), EditError> {
        let pair = self
            .tree()
            .get(coll_id)
            .is_ok_and(|c| Coll::of(c) == Some(Coll::Pair));
        if pair {
            if keep_lead {
                return Err(unsupported(
                    "the leading comments of a flow entry or of a collection's only entry \
                     have no entry or block footer to go to",
                ));
            }
            self.op_pair_remove(coll_id, check_aliases)
        } else {
            self.op_remove(coll_id, value, check_aliases, keep_lead)
        }
    }

    /// Moves the entry whose value is `from` to `to` (a reorder). The alias
    /// checks are exact (a move never puts an alias before its anchor): every
    /// alias inside keeps its anchor, and the aliases that referred to anchors
    /// inside are the ones they capture at the new place.
    fn op_move(&mut self, coll_id: TreeId, from: TreeId, to: Where) -> Result<(), EditError> {
        self.prepare()?;
        if to == Where::Before(from) {
            return Ok(());
        }
        // Removing the only entry rewrites the
        // collection (`{}`/`[]`, or a pair's `{}`) with no undo steps, so a
        // failed re-insertion could not put it back. Moving the only
        // entry anywhere is the identity: nothing to do. (Not reachable
        // from `move_entry`, whose indexes then name no other position.)
        let only = {
            let coll = self.tree().get(coll_id).map_err(|_| EditError::StaleNode)?;
            if Coll::of(coll) == Some(Coll::Pair) {
                true
            } else {
                let v = self.tree().get(from).map_err(|_| EditError::StaleNode)?;
                let e = place::entry_of(coll, v)?;
                place::prev_entry(coll, &e)?.is_none() && place::next_entry(coll, &e)?.is_none()
            }
        };
        if only {
            return Ok(());
        }
        // The removal and the re-insertion are one edit
        // group, so a refused re-insertion undoes the removal exactly,
        // whatever its steps moved.
        let spaced = self.bracket_line_space(coll_id).is_some();
        self.atomic(|d| {
            let (mut removed, _) = d.op_remove_any(coll_id, from, false, false)?;
            // Re-inserted right away: its referenced aliases are the runs
            // recorded (an O(log k) rule 8 check).
            removed.moving = true;
            let key = removed.key.clone();
            d.op_insert(coll_id, key.as_deref(), to, Payload::Removed(&removed))
                .map(|_| ())?;
            // Moving a flow collection's first entry away from
            // the bracket's line (`{ a,⏎# c⏎b}`) left the white space after
            // the bracket ending that line: it goes too (a trivia edit;
            // white space the move did not leave stays).
            if !spaced {
                if let Some(ws) = d.bracket_line_space(coll_id) {
                    d.t_remove(ws, ws)?;
                }
            }
            Ok(())
        })
    }

    /// The white space right after flow collection `coll`'s opening
    /// bracket when it ends the bracket's line.
    fn bracket_line_space(&self, coll: TreeId) -> Option<TreeId> {
        let c = self.tree().get(coll).ok()?;
        // Only a flow collection has a bracket, before its
        // first item (after its properties): never a scan of the entries.
        if !Coll::of(c)?.is_flow() {
            return None;
        }
        let mut head = c.children().take_while(|x| !x.kind().is_yaml_node());
        let open = head.find(|x| {
            matches!(
                x.kind(),
                SyntaxKind::FlowMappingOpen | SyntaxKind::FlowSequenceOpen
            )
        })?;
        let ws = open
            .next_sibling()
            .filter(|w| w.kind() == SyntaxKind::Whitespace)?;
        ws.next_sibling()
            .filter(|b| b.kind() == SyntaxKind::LineBreak)
            .map(|_| ws.id())
    }

    // ========================================================== anchors

    fn op_set_anchor(&mut self, target: TreeId, name: Option<&str>) -> Result<TreeId, EditError> {
        self.prepare()?;
        if let Some(n) = name {
            // YAML 1.2.2 production 102, a typed error.
            if n.is_empty() || !n.chars().all(is_anchor_char) {
                return Err(EditError::InvalidAnchorName { name: n.to_owned() });
            }
        }
        let nl = self.nl();
        // A new anchor on a root block collection is a
        // token insertion (O(the lines above its first entry)), not a
        // rewrite of the whole root: after the same checks, in the same
        // order, as below.
        if let Some(new) = name {
            let by_token = {
                let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
                let fits = n.anchor().is_none()
                    && n.parent().is_some_and(|p| p.kind() == SyntaxKind::Document)
                    && matches!(Coll::of(n), Some(Coll::BlockMap | Coll::BlockSeq));
                if fits {
                    self.check_region(&[n], n.parent())?;
                    let (start, end) = self.doc_range();
                    if self.anchors().has_anchor(self.tree(), new, start, end) {
                        return Err(EditError::DuplicateAnchor {
                            anchor: new.to_owned(),
                        });
                    }
                    self.check_defines(&[new.to_owned()], &[], None, n.preorder_index())?;
                    let slot = place::slot_of(n)?;
                    matches!(slot.kind, SlotKind::Own { .. })
                        && slot.first == n
                        && slot.last == n
                        && place::indicator_slot(n).is_none()
                } else {
                    false
                }
            };
            if by_token {
                if let Some(id) = self.anchor_root_by_token(target, new)? {
                    return Ok(id);
                }
            }
        }
        let (old, aliases, slot, text, lead, pc) = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            if n.kind() == SyntaxKind::Alias || !n.kind().is_yaml_node() {
                return Err(unsupported(
                    "an alias or error region cannot have an anchor",
                ));
            }
            self.check_region(&[n], n.parent())?;
            let old = n.anchor().map(|a| a.name.to_owned());
            if old.as_deref() == name {
                return Ok(target);
            }
            if let Some(new) = name {
                // Anchors are per document (section 3.2.2.2).
                let (start, end) = self.doc_range();
                if self.anchors().has_anchor(self.tree(), new, start, end) {
                    return Err(EditError::DuplicateAnchor {
                        anchor: new.to_owned(),
                    });
                }
                // Edited by token: the new name must not
                // capture an alias that did not already resolve to this
                // node. No anchor of that name is left in the document, so
                // every alias `*new` after the node (inside it included)
                // is dangling now and would resolve to it: refused (a
                // dangling alias stays dangling). O(log k).
                self.check_defines(&[new.to_owned()], &[], None, n.preorder_index())?;
            }
            let aliases = self.aliases_of_node(n);
            if name.is_none() && !aliases.is_empty() {
                return Err(EditError::AnchorInUse {
                    anchor: old.unwrap_or_default(),
                    aliases: aliases.into_iter().map(NodeId).collect(),
                });
            }
            let slot = place::slot_of(n)?;
            // Below comment lines, a new anchor
            // goes on the key's or dash's line.
            let trivia_slot = match slot.kind {
                SlotKind::Own { .. } if name.is_some() && old.is_none() => place::indicator_slot(n),
                _ => None,
            };
            let with_trivia = trivia_slot.is_some();
            let slot = trivia_slot.unwrap_or(slot);
            let lead = explicit_lead(n, slot.last, nl);
            // Only a new anchor rewrites the slot here.
            let pc = if old.is_none() {
                Some(self.property_carry(n, slot.first, slot.last)?)
            } else {
                None
            };
            // A block collection on lines of
            // its own whose first entry has comment lines right above it:
            // the new anchor's line goes above them (they stay its first
            // entry's), rewriting them with the node.
            let own_compact = matches!(slot.kind, SlotKind::Own { .. })
                && matches!(Coll::of(n), Some(Coll::BlockMap | Coll::BlockSeq))
                // (Its first token, not its text: reading the text would cost
                // O(node).)
                && !cm::first_token(n).is_some_and(|t| t.text().starts_with(['!', '[', '{']));
            // (Only when those lines stand at the node's column, where the
            // anchor's line goes.)
            let first = match pc.as_ref().and_then(|p| p.lead) {
                Some(c) if own_compact && name.is_some() => self
                    .tree()
                    .get(c)
                    .ok()
                    .filter(|c| place::col_of(*c) == place::col_of(n))
                    .filter(|c| carry::siblings(*c, slot.last).is_some())
                    .unwrap_or(slot.first),
                _ => slot.first,
            };
            // Comment rule 12: a root written without `---` below those
            // lines, which lie before its document (not its siblings): they
            // leave, and are written back below the anchor's line.
            let detached = match pc.as_ref().and_then(|p| p.lead) {
                Some(c) if own_compact && name.is_some() && first == slot.first => {
                    // (Any column: below the anchor's line, rule 12 gives
                    // them to the first entry whatever their column.)
                    let c =
                        self.tree().get(c).ok().filter(|_| {
                            n.parent().is_some_and(|p| p.kind() == SyntaxKind::Document)
                        });
                    // (The node's indentation stays, before its slot.)
                    let brk = cm::first_token(n)
                        .map(carry::line_head)
                        .and_then(place::prev_token)
                        .filter(|b| b.kind() == SyntaxKind::LineBreak);
                    match (c, brk) {
                        (Some(c), Some(b)) => {
                            let head = carry::line_head(c);
                            carry::siblings(head, b)
                                .map(|_| (head.id(), b.id(), place::range_text(head, b)))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            // The slot's text is written only for a new
            // anchor; a rename or a removal edits tokens (copying a root's
            // text made them O(document)).
            let (text, node_text) = if old.is_none() {
                let text = place::range_text(first, slot.last);
                let node_text = match &detached {
                    // (The lines as written, then the node at its column.)
                    Some((_, _, lines)) => {
                        format!("{lines}{}{}", sp(place::col_of(n)), n.text())
                    }
                    None if first == slot.first => n.text().into_owned(),
                    None => text.clone(),
                };
                (text, node_text)
            } else {
                (String::new(), String::new())
            };
            (
                old,
                aliases,
                slot.kind,
                (
                    text,
                    node_text,
                    (first.id(), slot.last.id()),
                    with_trivia,
                    detached.map(|(a, b, _)| (a, b)),
                ),
                lead,
                pc,
            )
        };
        let (slot_text, node_text, range, with_trivia, detached) = text;
        match (&old, name) {
            (Some(old), Some(new)) => {
                // Rename: the anchor token and every alias to it (rule 4),
                // token by token; undone on failure.
                self.rename(target, old, new, &aliases)?;
                self.commit();
                Ok(target)
            }
            (Some(old), None) => {
                let new = self.remove_anchor(target, old)?;
                self.commit();
                Ok(new)
            }
            (None, Some(new)) => {
                let compact = !node_text.starts_with(['!', '[', '{'])
                    && self.tree().get(target).is_ok_and(|n| {
                        matches!(Coll::of(n), Some(Coll::BlockMap | Coll::BlockSeq))
                    });
                // The node's own column: its continuation lines keep theirs.
                // (Read only for a block collection: the column costs a line
                // read.)
                let own = || self.tree().get(target).map_or(0, place::col_of);
                let new_text = match slot {
                    SlotKind::SeqItem { .. }
                        if compact && !with_trivia && slot_text.starts_with(' ') =>
                    {
                        format!(" &{new}{nl}{}{node_text}", sp(own()))
                    }
                    SlotKind::MapValue { .. }
                    | SlotKind::SeqItem { .. }
                    | SlotKind::AfterMarker => {
                        format!(" &{new}{slot_text}")
                    }
                    SlotKind::Own { .. } if compact && detached.is_some() => {
                        format!("&{new}{nl}{node_text}")
                    }
                    SlotKind::Own { .. } if compact => {
                        format!("&{new}{nl}{}{node_text}", sp(own()))
                    }
                    SlotKind::Own { .. } | SlotKind::Flow => format!("&{new} {node_text}"),
                    // `? a` alone, `{a, b}`.
                    SlotKind::ExplicitValue { col } => format!("{lead}{}: &{new}", sp(col)),
                    SlotKind::FlowValue => format!(": &{new}"),
                };
                let ends = self.tree().get(target).is_ok_and(ends_line);
                // `? |` alone: a whole new line (see `op_replace_with`).
                let own_line = matches!(slot, SlotKind::ExplicitValue { .. })
                    && self.tree().get(target).is_ok_and(place::key_ends_line);
                let frag = if ends || own_line {
                    self.slot_line_fragment(slot, &new_text)?
                } else {
                    self.slot_fragment(slot, &new_text)?.0
                };
                let pc = pc.ok_or(EditError::Internal)?;
                // Every comment keeps its owner (the header and footer
                // above all: an anchor on an empty root makes it visible).
                let (new, moved) = self.atomic(|d| {
                    // The node keeps its meaning, so its reading
                    // of the final space line too.
                    let reads = d.held_tail(target);
                    if let Some((a, b)) = detached {
                        d.t_remove(a, b)?;
                    }
                    let ins = d.t_replace(range.0, range.1, frag, ends)?;
                    let new = d.last_node(ins).ok_or(EditError::Internal)?;
                    d.tail_intend(new, reads);
                    let inner = d.node_list(new)?;
                    let moved = d.check_window(pc.window, &pc.expect, &inner)?;
                    Ok((new, moved))
                })?;
                self.carry_explicit(&moved, &[], &[new], Some((target, new)));
                self.commit();
                Ok(new)
            }
            (None, None) => Ok(target),
        }
    }

    /// The comments between the content before root
    /// `n` and token `b` (the first token of its first entry), each with
    /// the owner it has now, and the first of the comment lines right
    /// above the root that a node inside it owns (as
    /// [`Document::property_carry`] finds them, over these lines only:
    /// properties on a line of their own above a root's first entry change
    /// no owner past that entry's first token, rules 3, 4 and 12).
    fn root_head_carry(
        &self,
        n: TreeNode<'_>,
        b: TreeNode<'_>,
    ) -> Result<(carry::Window, Vec<Expect>, Option<TreeId>), EditError> {
        let first = cm::first_token(n).ok_or(EditError::Internal)?;
        let a = carry::visible_before(first);
        let comments = carry::window_comments(self.tree().root(), a, Some(b));
        let owners = self.rule_owners(&comments)?;
        let mut expect = Vec::with_capacity(comments.len());
        for c in &comments {
            let key = *owners.get(&c.id()).ok_or(EditError::Internal)?;
            expect.push(Expect {
                text: c.text().into_owned(),
                want: Want::Rel(Rel::Fixed(key)),
                old: Some(c.id()),
                explicit: None,
            });
        }
        let inside = |c: TreeNode<'_>| {
            owners
                .get(&c.id())
                .and_then(|k| carry::owner_node(k.0))
                .and_then(|(_, o)| self.tree().get(o.0).ok())
                .is_some_and(|o| o == n || o.ancestors().any(|x| x == n))
        };
        let mut lead = None;
        let mut at = place::prev_token(first);
        while let Some(t) = at {
            match t.kind() {
                SyntaxKind::Whitespace | SyntaxKind::LineBreak => {}
                SyntaxKind::Comment => {
                    if !inside(t) || !cm::line_start(carry::line_head(t)) {
                        break;
                    }
                    lead = Some(t.id());
                }
                _ => break,
            }
            at = place::prev_token(t);
        }
        let window = carry::Window {
            a: a.map(TreeNode::id),
            b: Some(b.id()),
        };
        Ok((window, expect, lead))
    }

    /// A new anchor `&new` on root block collection
    /// `target` (no properties, written on lines of its own, slot `Own`),
    /// as tokens inserted before its first entry: `&new`, the line break,
    /// the first entry's leading comment lines (which lie before the
    /// document and move below the anchor line, rule 12, as in the
    /// rewrite), and the root's indentation. The text and every comment's
    /// owner are exactly those of the rewrite ([`Document::op_set_anchor`]),
    /// and the node keeps its handle. `None` when the shape is not that
    /// one, or the edit is refused: the caller then rewrites the root as
    /// before (same outcome).
    fn anchor_root_by_token(
        &mut self,
        target: TreeId,
        new: &str,
    ) -> Result<Option<TreeId>, EditError> {
        let nl = self.nl();
        let plan = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            let Some(fc) = n.first_child() else {
                return Ok(None);
            };
            let entry = fc.kind().is_yaml_node() || fc.kind() == SyntaxKind::SequenceEntry;
            // As the rewrite decides (`compact`): a root whose text starts
            // with a tag or a flow collection (its first key's) gets the
            // anchor on its first line instead.
            let Some(b) =
                cm::first_token(fc).filter(|b| entry && !b.text().starts_with(['!', '[', '{']))
            else {
                return Ok(None);
            };
            let map = Coll::of(n) == Some(Coll::BlockMap);
            let (window, expect, lead) = self.root_head_carry(n, b)?;
            // Rule 12: the first entry's leading comment lines,
            // before the document, go below the anchor line.
            let detached = match lead.and_then(|c| self.tree().get(c).ok()) {
                None => None,
                Some(c) => {
                    let brk = place::prev_token(carry::line_head(b))
                        .filter(|x| x.kind() == SyntaxKind::LineBreak);
                    let head = carry::line_head(c);
                    match brk.filter(|k| carry::siblings(head, *k).is_some()) {
                        Some(k) => Some((head.id(), k.id(), place::range_text(head, k))),
                        None => return Ok(None),
                    }
                }
            };
            let col = place::col_of(n);
            let lines = detached.as_ref().map_or("", |d| d.2.as_str());
            let head_text = format!("&{new}{nl}{lines}{}", sp(col));
            (
                fc.id(),
                b.id(),
                map,
                col,
                head_text,
                window,
                expect,
                detached,
            )
        };
        let (fc, _b, map, col, head_text, window, expect, detached) = plan;
        let Ok(frag) = self.properties_fragment(map, col, &head_text) else {
            return Ok(None);
        };
        let r = self.atomic(|d| {
            let reads = d.held_tail(target);
            if let Some((a, b, _)) = detached {
                d.t_remove(a, b)?;
            }
            d.t_insert(Put::Before(fc), frag)?;
            d.tail_intend(target, reads);
            d.check_window(window, &expect, &[])
        });
        let Ok(moved) = r else {
            return Ok(None);
        };
        if let Some((tree, ix)) = self.index_parts() {
            let mut names = Names::default();
            names.anchors.push((new, target));
            ix.inserted(tree, &names);
        }
        self.index_check();
        self.carry_explicit(&moved, &[], &[], None);
        self.commit();
        Ok(Some(target))
    }

    /// Removes the anchor line of root block collection
    /// `target` (`&name`, its line break and the indentation after it,
    /// right before its first entry) as tokens; the text and every
    /// comment's owner are exactly those of the rewrite
    /// ([`Document::rewrite_without`]), and the node keeps its handle.
    /// `false` when the shape is not that one, or the edit is refused: the
    /// caller then goes on as before (same outcome).
    fn unanchor_root_by_token(
        &mut self,
        target: TreeId,
        name: &str,
        token: TreeId,
    ) -> Result<bool, EditError> {
        let plan = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            let root = n.parent().is_some_and(|p| p.kind() == SyntaxKind::Document);
            let a = n.first_child().filter(|a| a.id() == token && root);
            let Some(a) = a else { return Ok(false) };
            // Fuzz finding E15: after a tab on the anchor's
            // line (`\t&a⏎ k: v`) the first entry would start behind the tab
            // (`\tk: v`), which reads otherwise; the syntax layer does not
            // see that from the removed tokens. Such a root goes the way it
            // went before.
            if cm::line_prefix(a).contains('\t') {
                return Ok(false);
            }
            let Some(lb) = a
                .next_sibling()
                .filter(|x| x.kind() == SyntaxKind::LineBreak)
            else {
                return Ok(false);
            };
            let mut end = lb;
            let mut fc = lb.next_sibling();
            if let Some(w) = fc.filter(|w| w.kind() == SyntaxKind::Whitespace) {
                end = w;
                fc = w.next_sibling();
            }
            let fc =
                fc.filter(|x| x.kind().is_yaml_node() || x.kind() == SyntaxKind::SequenceEntry);
            let Some(b) = fc.and_then(cm::first_token) else {
                return Ok(false);
            };
            let (window, expect, _) = self.root_head_carry(n, b)?;
            (a.id(), end.id(), window, expect)
        };
        let (a, end, window, expect) = plan;
        let r = self.atomic(|d| {
            let reads = d.held_tail(target);
            d.t_remove(a, end)?;
            d.tail_intend(target, reads);
            d.check_window(window, &expect, &[])
        });
        let Ok(moved) = r else { return Ok(false) };
        if let Some((tree, ix)) = self.index_parts() {
            ix.forget(tree, name, target);
        }
        self.index_check();
        self.carry_explicit(&moved, &[], &[], None);
        Ok(true)
    }

    /// The properties `text` (`&a` and what follows up to the
    /// first entry, its indentation included) of a block collection at
    /// column `col`, as a fragment to insert before its first entry: read
    /// before a dummy entry, so they lex as they will there.
    fn properties_fragment(
        &self,
        map: bool,
        col: usize,
        text: &str,
    ) -> Result<Fragment, EditError> {
        let (d0, _, kind) = dummies(map);
        let t = self.synth(&format!("{}{text}{d0}", sp(col)))?;
        let c = root_node(&t).filter(|c| c.kind() == kind).ok_or_else(bad)?;
        let a = c
            .first_child()
            .filter(|a| a.kind() == SyntaxKind::Anchor)
            .ok_or_else(bad)?;
        let entry = c
            .children()
            .find(|x| x.kind().is_yaml_node() || x.kind() == SyntaxKind::SequenceEntry)
            .ok_or_else(bad)?;
        let b = entry.prev_sibling().ok_or_else(bad)?;
        Self::extract(&t, a, b)
    }

    /// Removes the `&name` token of node `target` (by token,
    /// never by text search), with the white space that separated it: the
    /// space after it when content follows on its line, else the space
    /// before the node too (`k: &a⏎` → `k:⏎`). A collection keeps its
    /// handle; the syntax layer keeps the inside of a scalar read-only, so
    /// a scalar is rewritten from its own tokens without those (a new
    /// handle). Returns the node.
    fn remove_anchor(&mut self, target: TreeId, name: &str) -> Result<TreeId, EditError> {
        let (token, last, before) = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            let a = n
                .children()
                .find(|c| c.kind() == SyntaxKind::Anchor)
                .ok_or(EditError::Internal)?;
            let ws = a
                .next_sibling()
                .filter(|w| w.kind() == SyntaxKind::Whitespace);
            let after = ws.map_or(a.next_sibling(), |w| w.next_sibling());
            let line_ends = after.is_none_or(|x| x.kind() == SyntaxKind::LineBreak);
            let last = ws.unwrap_or(a);
            // At the end of its line: the space before the node goes too
            // (no trailing white space is left behind).
            let before = if line_ends {
                let prev = if a.prev_sibling().is_none() {
                    n.prev_sibling()
                } else {
                    a.prev_sibling()
                };
                prev.filter(|p| p.kind() == SyntaxKind::Whitespace)
                    .map(|p| p.id())
            } else {
                None
            };
            (a.id(), last.id(), before)
        };
        let scalar = self.tree().get(target).is_ok_and(|n| Coll::of(n).is_none());
        if scalar {
            return self.rewrite_without(target, token, last, before);
        }
        // A root block collection's anchor line leaves
        // as tokens (O(the lines above its first entry)), never by a
        // rewrite of the whole root.
        if self.unanchor_root_by_token(target, name, token)? {
            return Ok(target);
        }
        // Fuzz finding E13: every comment between the
        // content before the node and the content after the anchor keeps
        // its owner, as the rewrite checks it (`property_carry`): `k: !t⏎
        // &zz #c⏎  a: b` without `&zz` would give `#c` to `a`. O(those
        // tokens), never the node.
        let (window, expect) = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            let first = cm::first_token(n).ok_or(EditError::Internal)?;
            let end = self.tree().get(last).map_err(|_| EditError::Internal)?;
            self.fixed_owners(carry::visible_before(first), carry::visible_after(end))?
        };
        let r = self.atomic(|d| {
            d.t_remove(token, last)?;
            if let Some(w) = before {
                d.t_remove(w, w)?;
            }
            d.check_window(window, &expect, &[])
        });
        match r {
            Ok(moved) => self.carry_explicit(&moved, &[], &[], None),
            Err(e @ EditError::CommentReattaches { .. }) => return Err(e),
            // The syntax layer keeps this inside read-only, or the two
            // splices cannot be read one at a time: rewrite the node.
            Err(EditError::ReadOnly { .. } | EditError::NotWellFormed { .. }) => {
                return self.rewrite_without(target, token, last, before);
            }
            Err(e) => return Err(e),
        }
        // The node stays: its id leaves the anchor list by hand.
        if let Some((tree, ix)) = self.index_parts() {
            ix.forget(tree, name, target);
        }
        self.index_check();
        Ok(target)
    }

    /// The comments between content tokens `a` and `b` (`None`: the
    /// tree's start or end), each with the owner it has now, as the
    /// window check of an edit that must change none of them.
    fn fixed_owners(
        &self,
        a: Option<TreeNode<'_>>,
        b: Option<TreeNode<'_>>,
    ) -> Result<(carry::Window, Vec<Expect>), EditError> {
        let comments = carry::window_comments(self.tree().root(), a, b);
        let owners = self.rule_owners(&comments)?;
        let mut expect = Vec::with_capacity(comments.len());
        for c in &comments {
            let key = *owners.get(&c.id()).ok_or(EditError::Internal)?;
            expect.push(Expect {
                text: c.text().into_owned(),
                want: Want::Rel(Rel::Fixed(key)),
                old: Some(c.id()),
                explicit: None,
            });
        }
        let window = carry::Window {
            a: a.map(TreeNode::id),
            b: b.map(TreeNode::id),
        };
        Ok((window, expect))
    }

    /// Rewrites node `target`'s slot from its own tokens, without the
    /// children `token..=last` and the sibling `before` (anchors are edited
    /// by token, never by a text search). Returns the new node.
    fn rewrite_without(
        &mut self,
        target: TreeId,
        token: TreeId,
        last: TreeId,
        before: Option<TreeId>,
    ) -> Result<TreeId, EditError> {
        let (range, frag, line, pc) = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            let slot = place::slot_of(n)?;
            let mut text = String::new();
            let mut at = Some(slot.first);
            while let Some(x) = at {
                if x == n {
                    let mut skip = false;
                    for c in n.children() {
                        if c.id() == token {
                            skip = true;
                        }
                        if !skip {
                            text.push_str(&c.text());
                        }
                        if c.id() == last {
                            skip = false;
                        }
                    }
                } else if Some(x.id()) != before {
                    text.push_str(&x.text());
                }
                if x == slot.last {
                    break;
                }
                at = x.next_sibling();
            }
            let line = ends_line(n);
            let frag = if line {
                self.slot_line_fragment(slot.kind, &text)?
            } else {
                self.slot_fragment(slot.kind, &text)?.0
            };
            // Comment lines between the node's properties and its content that
            // the rewritten slot cannot hold (a root node's: they would stand
            // before it, outside the slot) refuse the edit.
            let comments = |t: &str| {
                SyntaxTree::parse_with(t, self.tree().settings())
                    .root()
                    .descendants()
                    .filter(|x| x.kind() == SyntaxKind::Comment)
                    .count()
            };
            let kept = comments(frag.text());
            let had = super::carry::window_comments(
                self.tree().root(),
                cm::first_token(slot.first).and_then(|t| place::prev_token(t)),
                cm::last_token(slot.last).and_then(place::next_token),
            )
            .len();
            if kept < had {
                return Err(unsupported(
                    "removing the anchor of a node whose properties stand on a line of their \
                     own above comment lines",
                ));
            }
            let pc = self.property_carry(n, slot.first, slot.last)?;
            ((slot.first.id(), slot.last.id()), frag, line, pc)
        };
        let (range, frag, line, pc) = (range, frag, line, pc);
        // Every comment keeps its owner (an empty root
        // that loses its anchor becomes invisible: its footer would become
        // the header).
        let (new, moved) = self.atomic(|d| {
            // As for a new anchor, the meaning is kept.
            let reads = d.held_tail(target);
            let ins = d.t_replace(range.0, range.1, frag, line)?;
            let new = d.last_node(ins).ok_or(EditError::Internal)?;
            d.tail_intend(new, reads);
            let inner = d.node_list(new)?;
            let moved = d.check_window(pc.window, &pc.expect, &inner)?;
            Ok((new, moved))
        })?;
        self.carry_explicit(&moved, &[], &[new], Some((target, new)));
        Ok(new)
    }

    /// Renames the anchor of `target` and its aliases, token by token, as
    /// one edit group: refused, nothing changed (handles
    /// included).
    fn rename(
        &mut self,
        target: TreeId,
        old: &str,
        new: &str,
        aliases: &[TreeId],
    ) -> Result<(), EditError> {
        let token = {
            let n = self.tree().get(target).map_err(|_| EditError::StaleNode)?;
            n.children()
                .find(|c| c.kind() == SyntaxKind::Anchor)
                .map(|c| (c.id(), c.text().into_owned(), c.span()))
                .ok_or(EditError::Internal)?
        };
        let alias_tokens: Vec<(TreeId, String, Span)> = aliases
            .iter()
            .filter_map(|a| {
                let n = self.tree().get(*a).ok()?;
                let t = n.children().find(|c| c.kind() == SyntaxKind::AliasName)?;
                Some((t.id(), t.text().into_owned(), t.span()))
            })
            .collect();
        let edits = std::iter::once((token.0, token.1, format!("&{new}"), token.2)).chain(
            alias_tokens
                .into_iter()
                .map(|(id, old, span)| (id, old, format!("*{new}"), span)),
        );
        // One edit group, so a refused step
        // undoes the earlier ones exactly (the tree, every handle), never a
        // hand-written replacement back (which left new token handles).
        let edits: Vec<_> = edits.collect();
        self.atomic(|d| {
            for (id, _, text, span) in edits {
                d.tree_mut()?
                    .replace_token(id, &text)
                    .map_err(|e| map_tree(e, span))?;
            }
            Ok(())
        })?;
        // The nodes keep their handles; their names change in the index.
        if let Some((tree, ix)) = self.index_parts() {
            ix.renamed(tree, old, new, target, aliases);
        }
        self.index_check();
        Ok(())
    }

    // ======================================================== root, paths

    /// Writes a first document into an empty stream.
    fn op_create_root(&mut self, v: &Value) -> Result<TreeId, EditError> {
        self.prepare()?;
        // An input without a root but with an error
        // region (`]`): the new root would follow that region, and the
        // syntax layer would re-read the whole input to refuse it. Refused
        // here, in O(1), as an edit next to an error region.
        if self.tree().has_errors() {
            return Err(EditError::ReadOnly { at: Path::new() });
        }
        let nl = self.nl();
        let render = self.render(None, false)?;
        let text = render.root(v)?;
        let (frag, stream) = {
            let current = self.tree().root();
            let tail = current.text();
            let lead = if tail.is_empty() || ends_break(&tail) {
                String::new()
            } else {
                nl.to_owned()
            };
            // Comment rule 2: comment lines of an input without content are
            // its header; a blank line keeps them so above the new root
            // (rule 3 would give them to its first entry). O(n) once, for
            // an input without content.
            let mut last = None;
            let mut breaks = 0usize;
            for t in current.descendants().filter(|t| t.is_token()) {
                match t.kind() {
                    SyntaxKind::LineBreak => breaks = breaks.saturating_add(1),
                    SyntaxKind::Whitespace | SyntaxKind::ByteOrderMark => {}
                    k => {
                        last = Some(k);
                        breaks = 0;
                    }
                }
            }
            let lead = if last == Some(SyntaxKind::Comment) && breaks < 2 {
                format!("{lead}{nl}")
            } else {
                lead
            };
            // Text ending its line (a keep scalar's last empty line,
            // fuzz finding E7) gets no other break.
            let end = if ends_break(&text) { "" } else { nl };
            let t = self.synth(&format!("{lead}{text}{end}"))?;
            let kids: Vec<_> = t.root().children().collect();
            let (a, b) = kids.first().zip(kids.last()).ok_or_else(bad)?;
            (Self::extract(&t, *a, *b)?, current.id())
        };
        // (One edit group, as every edit: the document's error check runs at
        // its end.)
        let ins = self.atomic(|d| d.t_insert(Put::Append(stream), frag))?;
        let (doc, root) = {
            let tree = self.tree();
            let mut at = tree.get(ins.first).ok();
            let mut found = None;
            while let Some(n) = at {
                if n.kind() == SyntaxKind::Document {
                    found = Some(n);
                    break;
                }
                if n.id() == ins.last {
                    break;
                }
                at = n.next_sibling();
            }
            let d = found.ok_or(EditError::Internal)?;
            let r = d
                .children()
                .find(|c| c.kind().is_yaml_node())
                .ok_or(EditError::Internal)?;
            (d.id(), r.id())
        };
        self.doc = Some(doc);
        self.commit();
        // The one edit that adds a document.
        if let Some(s) = Arc::get_mut(&mut self.shared) {
            s.documents_changed();
        }
        Ok(root)
    }

    fn set_root_value(&mut self, v: &Value) -> Result<TreeId, EditError> {
        match self.root().map(|r| r.tree_node().id()) {
            Some(r) => self.op_set(r, v, None),
            None if self.index.is_none() => self.op_create_root(v),
            None => Err(EditError::Internal),
        }
    }

    /// `set_path`'s engine: `leaf` at `path`, creating what is missing.
    fn set_path_value(
        &mut self,
        path: &Path,
        leaf: Value,
        replace: bool,
    ) -> Result<NodeId, EditError> {
        /// What the walk found.
        enum Act {
            /// The node exists: replace it (or return it).
            Replace(TreeId, NodeId),
            /// Insert into `coll` under `key` (or append), with the path
            /// segments from `rest` nested below.
            Insert(TreeId, Option<String>, usize),
        }
        let segs = path.segments();
        if self.root().is_none() {
            let v = nest(segs, leaf, &Path::new())?;
            self.prepare()?;
            self.set_root_value(&v)?;
            return self.walk_path(path).map(|n| n.id());
        }
        // One walk with views (no handle lookups per level).
        let (act, at) = {
            let mut at = Path::new();
            let mut node = self.root().ok_or(EditError::Internal)?;
            let mut act = None;
            for (i, seg) in segs.iter().enumerate() {
                let last = i.saturating_add(1) == segs.len();
                if node.resolved().kind() == NodeKind::Error {
                    return Err(EditError::ErrorNode { at });
                }
                match seg {
                    PathSegment::Key(k) => {
                        let Some(m) = node.as_mapping() else {
                            return Err(EditError::NotAMapping { at });
                        };
                        at.push_key(k.clone());
                        if last {
                            act = Some(match m.segment(k, true).map_err(|e| e.located(&at))? {
                                Some(v) => Act::Replace(v.tree_node().id(), v.id()),
                                None => Act::Insert(m.node().tree_node().id(), Some(k.clone()), i),
                            });
                            break;
                        }
                        match m.segment(k, false).map_err(|e| e.located(&at))? {
                            Some(n) => node = n,
                            None => {
                                act = Some(Act::Insert(
                                    m.node().tree_node().id(),
                                    Some(k.clone()),
                                    i,
                                ));
                                break;
                            }
                        }
                    }
                    PathSegment::Index(idx) => {
                        let Some(s) = node.as_sequence() else {
                            return Err(EditError::NotASequence { at });
                        };
                        let item = s.get(*idx);
                        if item.is_none() && *idx != s.len() {
                            return Err(EditError::IndexOutOfBounds {
                                at,
                                index: *idx,
                                len: s.len(),
                            });
                        }
                        at.push_index(*idx);
                        match item {
                            Some(item) if last => {
                                act = Some(Act::Replace(item.tree_node().id(), item.id()));
                                break;
                            }
                            Some(item) => node = item,
                            None => {
                                act = Some(Act::Insert(s.node().tree_node().id(), None, i));
                                break;
                            }
                        }
                    }
                    _ => return Err(unsupported("a path segment of an unknown kind")),
                }
            }
            let act = match act {
                Some(a) => a,
                // An empty path: the root.
                None => Act::Replace(node.tree_node().id(), node.id()),
            };
            (act, at)
        };
        match act {
            Act::Replace(_, id) if !replace => Ok(id),
            Act::Replace(_, _) if segs.is_empty() => self.set_root_value(&leaf).map(NodeId),
            Act::Replace(t, _) => self
                .op_set(t, &leaf, None)
                .map(NodeId)
                .map_err(|e| e.located(&at)),
            Act::Insert(coll, key, i) => {
                let rest = segs.get(i.saturating_add(1)..).unwrap_or_default();
                // A null `get_path_or_create` creates right
                // in an existing block collection is written as its
                // neighbour writes null (`k:`, `k: ~`, ...).
                let style = if !replace && rest.is_empty() && leaf == Value::Null {
                    self.null_style(coll)
                } else {
                    None
                };
                let v = nest(rest, leaf, &at)?;
                let payload = match &style {
                    Some(s) => Payload::Null(s),
                    None => Payload::Value(&v),
                };
                let new = self
                    .op_insert(coll, key.as_deref(), Where::Append, payload)
                    .map_err(|e| e.located(&at))?;
                // The node inside what was written (an accepted edit
                // returns Ok; a path through merge keys may resolve
                // elsewhere afterwards).
                let mut inner = Path::new();
                for seg in rest {
                    match seg {
                        PathSegment::Key(k) => inner.push_key(k.clone()),
                        PathSegment::Index(i) => inner.push_index(*i),
                        _ => return Err(EditError::Internal),
                    }
                }
                self.node(NodeId(new))?.walk(&inner).map(|n| n.id())
            }
        }
    }

    /// How the last entry (or item) of block collection `coll` writes a
    /// null value, when it is one written without properties: `""` (an
    /// empty value), `~`, `null`, `Null`, or `NULL` (the Core null forms).
    /// `None` otherwise (then new nulls are written `null`).
    /// O(1): the neighbour a new entry is appended after.
    fn null_style(&self, coll: TreeId) -> Option<String> {
        let c = self.tree().get(coll).ok()?;
        if Coll::of(c)?.is_flow() {
            return None;
        }
        let v = place::last_entry(c).ok()??.value;
        if v.kind() != SyntaxKind::Scalar {
            return None;
        }
        let r = NodeRef::new(self, v);
        // A tag that does not resolve (`Y9000`) counts as a tag: the
        // shortcut does not apply.
        if r.anchor().is_some() || !matches!(r.tag(), Ok(None)) {
            return None;
        }
        let s = r.as_scalar()?;
        if s.style() != ScalarStyle::Plain {
            return None;
        }
        let text = r.text();
        let null = s.resolved().is_ok_and(|x| x == Value::Null);
        (null && matches!(text.as_ref(), "" | "~" | "null" | "Null" | "NULL"))
            .then(|| text.into_owned())
    }

    // ========================================================= public API

    /// A mutable handle to node `id`.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no node of this document
    /// (its node was removed or replaced, or it was taken from another
    /// document, or from a clone after the two diverged);
    /// [`EditError::ReadOnly`] for a document whose input had encoding
    /// errors.
    pub fn node_mut(&mut self, id: NodeId) -> Result<NodeMut<'_>, EditError> {
        self.prepare()?;
        self.node(id)?;
        Ok(NodeMut { doc: self, id })
    }

    /// A mutable handle to the root node.
    ///
    /// # Errors
    ///
    /// [`EditError::NotFound`] for an empty document (use
    /// [`Document::set_root`]); as [`Document::node_mut`].
    pub fn root_mut(&mut self) -> Result<NodeMut<'_>, EditError> {
        self.prepare()?;
        let id = self
            .root()
            .map(|r| r.id())
            .ok_or(EditError::NotFound { at: Path::new() })?;
        Ok(NodeMut { doc: self, id })
    }

    /// The root mapping's value for `key` (any [`AsKey`](crate::AsKey)),
    /// mutable (see [`MappingRef::get`](crate::MappingRef::get) for which
    /// entry that is).
    ///
    /// Unlike `std`'s `get_mut` (an `Option`), this returns a `Result`:
    /// the error says why there is no node to edit (no root, a root that
    /// is not a mapping, a missing key, a read-only document), which an
    /// edit needs to report. The read-only lookups ([`Document::get`],
    /// [`Document::get_path`]) return `Option`.
    ///
    /// # Errors
    ///
    /// As [`Document::get_path_mut`].
    pub fn get_mut<K: AsKey>(&mut self, key: K) -> Result<NodeMut<'_>, EditError> {
        let k = key::arg(&key);
        match k.as_str() {
            Some(s) => self.get_path_mut(Path::new().with_key(s)),
            None => self.root_mut()?.get_mut(key),
        }
    }

    /// The node at `path`, mutable. Navigation follows aliases and merge
    /// keys as [`Document::get_path`] does: editing through an alias edits
    /// its anchored node, which every alias of it shows.
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidPath`] for a malformed path string;
    /// [`EditError::NotFound`], [`EditError::NotAMapping`],
    /// [`EditError::NotASequence`], [`EditError::IndexOutOfBounds`] or
    /// [`EditError::ErrorNode`] where the walk stops; as
    /// [`Document::node_mut`].
    pub fn get_path_mut<P: IntoPath>(&mut self, path: P) -> Result<NodeMut<'_>, EditError> {
        let path = path_arg::edit(&path)?;
        self.prepare()?;
        let id = self.walk_path(&path)?.id();
        Ok(NodeMut { doc: self, id })
    }

    /// Sets the node at `path` to `value` (any `Serialize`, or a
    /// [`Value`]), creating the missing entries on the way: a missing key
    /// becomes a new block entry (a mapping below it, or a sequence for an
    /// index segment, which must then be `0`); an index equal to a
    /// sequence's length appends. One of the two explicit creators of
    /// nodes (nothing is created implicitly).
    ///
    /// The last key segment sets this mapping's **own** entry: a key that
    /// is only merged in (`<<`) gets an own entry that shadows it.
    /// An existing node keeps its anchor. New
    /// content follows its neighbours' layout: multi-line strings
    /// are `|` block scalars wherever the context permits (else
    /// double-quoted), and new lines use the document's line break.
    /// A string set over an existing scalar keeps that scalar's style where
    /// the style can hold the text, and changes only the text: a block scalar
    /// stays a block scalar of its kind and indentation, with its chomping
    /// indicator changed only as far as the final line breaks need; a quoted
    /// scalar keeps its quotes; a text with a line break on a plain or quoted
    /// scalar is written double-quoted on the same line; a tag stays while the
    /// new value reads back under it as exactly that value; the white space
    /// after `:` or `-` stays. Such a string is written plain unless the
    /// document's schema reads it as another type.
    /// Setting a node to the value it already has (as [`Value`] equality
    /// says) changes no byte: its style, comments, anchor and handle stay;
    /// except that a node which is or holds an alias or a merge key is
    /// written as any value (the link to its anchored node goes).
    /// Returns a handle to the node.
    ///
    /// `path` is a path string or a [`Path`] ([`IntoPath`](crate::IntoPath));
    /// `value` is taken by value or by reference.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let mut doc = Document::parse("spec:\n  name: web  # shown\n").unwrap();
    /// doc.set_path("spec.replicas", 3).unwrap();
    /// doc.set_path(&path!["spec", "name"], &String::from("api")).unwrap();
    /// assert_eq!(doc.to_string(), "spec:\n  name: api  # shown\n  replicas: 3\n");
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidPath`] for a malformed path string;
    /// [`EditError::NotAMapping`]/[`EditError::NotASequence`] when a
    /// segment meets a scalar (never overwritten implicitly);
    /// [`EditError::IndexOutOfBounds`] past the end;
    /// [`EditError::Rejected`] for a value past `max_depth` (`Y2005`),
    /// `max_typed_depth` (`Y2007`), one YAML cannot represent (`Y3015`), or
    /// output settings that cannot write it (`Y1012`, `Y1017`); the other
    /// edit errors.
    pub fn set_path<P: IntoPath, T: Serialize>(
        &mut self,
        path: P,
        value: T,
    ) -> Result<NodeMut<'_>, EditError> {
        let path = path_arg::edit(&path)?;
        self.prepare()?;
        let v = to_value(&value, self)?;
        let id = self.set_path_value(&path, v, true)?;
        Ok(NodeMut { doc: self, id })
    }

    /// The node at `path`, created (as `null`, with the missing entries on
    /// the way, as [`Document::set_path`] creates them) when it does not
    /// exist.
    ///
    /// # Errors
    ///
    /// As [`Document::set_path`].
    pub fn get_path_or_create<P: IntoPath>(&mut self, path: P) -> Result<NodeMut<'_>, EditError> {
        let path = path_arg::edit(&path)?;
        self.prepare()?;
        let id = self.set_path_value(&path, Value::Null, false)?;
        Ok(NodeMut { doc: self, id })
    }

    /// Replaces the root node by `value` (keeping its anchor), or writes
    /// the first document of an empty input (laid out with
    /// `doc.settings().emit()`, the only context there is). The
    /// document header and footer stay.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("# generated\n\nold: config\n").unwrap();
    /// doc.set_root(&vec!["a", "b"]).unwrap().set_anchor(Some("list")).unwrap();
    /// assert_eq!(doc.to_string(), "# generated\n\n&list\n- a\n- b\n");
    /// let mut empty = Document::parse("").unwrap();
    /// empty.set_root(&1).unwrap();
    /// assert_eq!(empty.to_string(), "1\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::set_path`].
    pub fn set_root<T: Serialize>(&mut self, value: T) -> Result<NodeMut<'_>, EditError> {
        self.prepare()?;
        let v = to_value(&value, self)?;
        let id = NodeId(self.set_root_value(&v)?);
        Ok(NodeMut { doc: self, id })
    }

    /// Removes the node at `path` from its mapping or sequence (see
    /// [`NodeMut::remove`]).
    ///
    /// # Errors
    ///
    /// As [`Document::remove_node`].
    pub fn remove_path<P: IntoPath>(&mut self, path: P) -> Result<Removed, EditError> {
        let path = path_arg::edit(&path)?;
        let id = self.walk_path(&path)?.id();
        self.remove_node(id).map_err(|e| e.located(&path))
    }

    /// Removes node `id` from the collection holding it: its entry (key and
    /// value) from a mapping, or its item from a sequence. The comments of
    /// the removed range travel with the [`Removed`].
    ///
    /// # Errors
    ///
    /// [`EditError::Unsupported`] for a root node or a key;
    /// [`EditError::AnchorInUse`] when it defines an anchor an alias
    /// outside it still uses; the other edit errors.
    pub fn remove_node(&mut self, id: NodeId) -> Result<Removed, EditError> {
        self.prepare()?;
        let (coll, value) = {
            let n = self.tree_node(id)?;
            let parent = n.parent().ok_or(EditError::Internal)?;
            let Some(kind) = Coll::of(parent) else {
                return Err(unsupported("removing a root node (use set_root)"));
            };
            if kind.is_map() && self.key_in(parent, n) {
                return Err(unsupported("removing a key (remove its entry by value)"));
            }
            (parent.id(), n.id())
        };
        self.op_remove_any(coll, value, true, false).map(|(r, _)| r)
    }

    /// Removes node `id`'s entry as [`Document::remove_node`] does, except
    /// that its **leading comments stay** (`remove_keep_comments`):
    /// they become the leading comments of the next entry or item, or,
    /// when it was the last one, the footer of its collection; the last
    /// entry of a **root** collection (which owns no footer) gives them to
    /// the **document footer** (the call is the explicit request for
    /// it). Only the leading comments
    /// **written above** the entry stay (before its first token: its key,
    /// or the `? `/`- ` indicator or properties before it); a leading
    /// comment written inside the entry, after `?`, `-` or its properties
    /// (`?⏎  # c⏎  k⏎: v`), goes with it.
    /// The [`Removed`] takes its other comments (same-line, inside, the
    /// footers inside it). The comments must read back with those owners:
    /// otherwise the removal is refused and nothing changes (e.g.
    /// a leading comment at a column the rules give to the enclosing
    /// collection's neighbour).
    ///
    /// ```
    /// use oneyaml::{CommentOwner, Document, path};
    ///
    /// let mut d = Document::parse("a: 1\n# about b\nb: 2\nc: 3\n").unwrap();
    /// let b = d.id_of(&path!["b"]).unwrap();
    /// let r = d.remove_keep_comments(b).unwrap();
    /// assert!(r.comments().is_empty());
    /// assert_eq!(d.to_string(), "a: 1\n# about b\nc: 3\n");
    /// let c = d.get_path(&path!["c"]).unwrap().key().unwrap().id();
    /// assert_eq!(d.comments(c).unwrap().leading()[0].text(), "about b");
    ///
    /// // The last entry of the root mapping: the document footer.
    /// let c = d.id_of(&path!["c"]).unwrap();
    /// d.remove_keep_comments(c).unwrap();
    /// assert_eq!(d.to_string(), "a: 1\n# about b\n");
    /// let e = d.explain_comments().unwrap();
    /// assert_eq!(e[0].owner(), CommentOwner::DocumentFooter);
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::remove_node`]; [`EditError::CommentReattaches`] when
    /// a leading comment would not read back as the next entry's (or the
    /// collection's footer); [`EditError::Unsupported`] when there is no
    /// such owner: the only entry of a collection (it becomes `{}`/`[]`)
    /// or an entry of a flow collection, with leading comments.
    pub fn remove_keep_comments(&mut self, id: NodeId) -> Result<Removed, EditError> {
        self.prepare()?;
        let (coll, value) = {
            let n = self.tree_node(id)?;
            let parent = n.parent().ok_or(EditError::Internal)?;
            let Some(kind) = Coll::of(parent) else {
                return Err(unsupported("removing a root node (use set_root)"));
            };
            if kind.is_map() && self.key_in(parent, n) {
                return Err(unsupported("removing a key (remove its entry by value)"));
            }
            (parent.id(), n.id())
        };
        self.op_remove_any(coll, value, true, true).map(|(r, _)| r)
    }

    /// The aliases that refer to node `id`, in document order;
    /// empty when it has no anchor. O(log n + found).
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`].
    pub fn aliases_of(&self, id: NodeId) -> Result<Vec<NodeId>, EditError> {
        let n = self.tree_node(id)?;
        Ok(self.aliases_of_node(n).into_iter().map(NodeId).collect())
    }

    /// Sets, renames, or removes the anchor of node `id`:
    /// renaming renames every alias to it; removing an anchor that aliases
    /// still use is refused. Renaming and removing edit the `&name` token
    /// and keep the node's handle; setting a new anchor returns a new
    /// handle (the node is rewritten with it), except on a root block
    /// collection, whose anchor line is inserted as tokens (it keeps its
    /// handle). Use the handle returned.
    ///
    /// # Errors
    ///
    /// [`EditError::InvalidAnchorName`] (not `ns-anchor-char+`),
    /// [`EditError::DuplicateAnchor`] (the name is used by an anchor of this
    /// document, or by an alias after the node, which the new anchor would
    /// capture: a dangling alias stays dangling),
    /// [`EditError::AnchorInUse`], [`EditError::Unsupported`] (an alias),
    /// and the other edit errors.
    pub fn set_anchor(&mut self, id: NodeId, name: Option<&str>) -> Result<NodeId, EditError> {
        self.prepare()?;
        let target = self.tree_node(id)?.id();
        self.op_set_anchor(target, name).map(NodeId)
    }

    /// The document read as any `T: Deserialize`, through the same event
    /// source and resolver as [`Document::to_value`] and
    /// [`crate::from_str`], so plain scalars into strings keep their
    /// source text.
    ///
    /// # Errors
    ///
    /// As [`crate::from_str`].
    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<T, Error> {
        let tree = self.tree();
        if let Some(e) = self.shared.pre_error(self.index) {
            return Err(e);
        }
        let doc = self.doc_node().filter(|_| self.index.is_some());
        let source = || {
            match doc {
                Some(d) => crate::construct::tree::TreeSource::over(tree, d),
                None => crate::construct::tree::TreeSource::new(tree),
            }
            .with_doc_tags(&self.shared.tags)
        };
        let lines = Some(&self.shared.lines);
        // The text is read only to locate an error.
        // Warnings are discarded: at most 16 built.
        crate::de::read_discarding::<_, T>(source, || self.shared.text(), self.settings(), lines)
    }
}

/// The mapping entries written in node `n` and the nodes inside it.
/// O(the node).
fn written_entries(n: TreeNode<'_>) -> usize {
    // (`descendants` includes `n` itself.)
    n.descendants()
        .filter(|d| {
            matches!(
                d.kind(),
                SyntaxKind::BlockMapping | SyntaxKind::FlowMapping | SyntaxKind::FlowPair
            )
        })
        .map(|d| d.children().filter(|c| c.kind().is_yaml_node()).count() / 2)
        .fold(0usize, usize::saturating_add)
}

/// The mapping entries of `v` and the values inside it. A loop, not recursion.
fn value_entries(v: &Value) -> usize {
    let mut n = 0usize;
    let mut stack = vec![v];
    while let Some(x) = stack.pop() {
        match x.untag_ref() {
            Value::Mapping(m) => {
                n = n.saturating_add(m.len());
                for (k, v) in m.iter() {
                    stack.push(k);
                    stack.push(v);
                }
            }
            Value::Sequence(s) => stack.extend(s.iter()),
            _ => {}
        }
    }
    n
}

/// What goes before the `:` written for a value of `? key` alone: the
/// rest of the key line through its comment (the slot's
/// siblings after the empty value `n` through `last`), then the line break,
/// unless the key ends its line itself (a block scalar).
fn explicit_lead(n: TreeNode<'_>, last: TreeNode<'_>, nl: &str) -> String {
    let mut out = match n.next_sibling() {
        Some(first) if last != n => place::range_text(first, last),
        _ => String::new(),
    };
    if !place::key_ends_line(n) {
        out.push_str(nl);
    }
    out
}

/// Synthetic text for [`SlotKind::ExplicitValue`]: an explicit key at
/// column `col` followed by `text` (then `end`). The key ends its line as
/// the real one does: a plain `x` when `text` starts with the rest of the
/// key's line (its comment, its line break), else (`text` starts with the
/// indentation and `:`) a block scalar, whose text ends with its line
/// break.
fn explicit_synth(col: usize, text: &str, end: &str, nl: &str) -> String {
    let pad = sp(col);
    if text.trim_start_matches(' ').starts_with(':') {
        format!("{pad}? |{nl}{pad}  x{nl}{text}{end}")
    } else {
        format!("{pad}? x{text}{end}")
    }
}

/// In a synthetic `? x…` mapping: the siblings after the key `x` through
/// the mapping's last child (what replaces an empty value and the rest of
/// its slot), which must hold the `:` written.
fn explicit_range(t: &SyntaxTree) -> Result<(TreeNode<'_>, TreeNode<'_>), EditError> {
    let m = root_node(t)
        .filter(|m| m.kind() == SyntaxKind::BlockMapping)
        .ok_or_else(bad)?;
    let mut items = m.children().filter(|c| c.kind().is_yaml_node());
    let (Some(key), Some(_), None) = (items.next(), items.next(), items.next()) else {
        return Err(bad());
    };
    if !m.children().any(|c| c.kind() == SyntaxKind::MappingValue) {
        return Err(bad());
    }
    Ok((
        key.next_sibling().ok_or_else(bad)?,
        m.last_child().ok_or_else(bad)?,
    ))
}

/// In a synthetic `{x…}` mapping: the siblings after the key `x` through
/// the last one before `}`.
fn flow_value_range(t: &SyntaxTree) -> Result<(TreeNode<'_>, TreeNode<'_>), EditError> {
    let m = root_node(t)
        .filter(|m| m.kind() == SyntaxKind::FlowMapping)
        .ok_or_else(bad)?;
    let key = m
        .children()
        .find(|c| c.kind().is_yaml_node())
        .ok_or_else(bad)?;
    let close = m
        .last_child()
        .filter(|c| c.kind() == SyntaxKind::FlowMappingClose)
        .ok_or_else(bad)?;
    Ok((
        key.next_sibling().ok_or_else(bad)?,
        close.prev_sibling().ok_or_else(bad)?,
    ))
}

/// The indicator text, collection kind and indicator kind of a block slot.
fn slot_parts(kind: SlotKind) -> (&'static str, SyntaxKind, SyntaxKind) {
    if matches!(kind, SlotKind::MapValue { .. }) {
        ("x:", SyntaxKind::BlockMapping, SyntaxKind::MappingValue)
    } else {
        ("-", SyntaxKind::BlockSequence, SyntaxKind::SequenceEntry)
    }
}

/// Two dummy entries and the collection kind, for synthetic block text.
fn dummies(map: bool) -> (&'static str, &'static str, SyntaxKind) {
    if map {
        ("x: x", "y: y", SyntaxKind::BlockMapping)
    } else {
        ("- x", "- y", SyntaxKind::BlockSequence)
    }
}

/// The value node of entry `index` of a mapping or sequence view, and the
/// number of entries when there is none (O(log width) for a wide
/// collection; else O(index), then O(len)).
fn nth_value(t: NodeRef<'_>, index: usize) -> Result<Result<TreeId, usize>, EditError> {
    if let Some(m) = t.as_mapping() {
        return Ok(m
            .entry(index)
            .map(|(_, v)| v.tree_node().id())
            .ok_or_else(|| m.len()));
    }
    if let Some(s) = t.as_sequence() {
        return Ok(s
            .get(index)
            .map(|v| v.tree_node().id())
            .ok_or_else(|| s.len()));
    }
    Err(EditError::NotAMapping { at: Path::new() })
}

/// A mutable handle to one node of a [`Document`]: the edit
/// methods, each returning `Result`. Obtained from
/// [`Document::node_mut`], [`Document::get_path_mut`], and friends.
///
/// **Handle rule:** navigation consumes the handle; every edit that stays
/// on the same node takes `&mut self`.
///
/// Navigation is `get_mut`, `get_index_mut`, `entry` and `into_key`. An
/// edit that gives the node a new id (`set`, `set_scalar`, `set_anchor`,
/// removing a collection's last entry) updates the handle, which keeps
/// naming the node; an edit that creates a node (`push`, `insert`,
/// `insert_at`, `replace_at`, `push_alias`, `insert_alias`,
/// `insert_merge`, `insert_removed`) returns a handle to it.
///
/// So edits chain:
///
/// ```
/// use oneyaml::{Document, path};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut doc = Document::parse("env:\n  - name: A\n")?;
/// doc.get_path_mut(&path!["env"])?
///     .push(&[("name", "B")].into_iter().collect::<std::collections::BTreeMap<_, _>>())?
///     .set_leading(["added by the release script"])?;
/// assert_eq!(
///     doc.to_string(),
///     "env:\n  - name: A\n  # added by the release script\n  - name: B\n"
/// );
/// # Ok(())
/// # }
/// ```
///
/// # Values, keys and refusals
///
/// Values are anything `Serialize`, by value or by reference
/// (`push(8080)`, `push(&config)`), or a [`Value`](crate::Value). Keys are
/// anything [`AsKey`](crate::AsKey): `&str`, `String`, integers, `bool`, a
/// `Value`. Every edit returns `Result<_, EditError>`; a refused edit
/// changes nothing, and an edited document always means what its text
/// means. Setting a node to the value it already has changes no byte:
///
/// ```
/// use oneyaml::{Document, path};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut doc = Document::parse("env:\n  - LOG_LEVEL=info\nports:\n  80: http\n")?;
/// doc.get_path_mut(&path!["env"])?
///     .push("FEATURE_SEARCH=on")?
///     .set_leading(["Enables the new search page."])?;
/// doc.get_mut("ports")?.insert(443, "https")?.set_anchor(Some("tls"))?;
/// assert_eq!(
///     doc.to_string(),
///     "env:\n  - LOG_LEVEL=info\n  # Enables the new search page.\n  - FEATURE_SEARCH=on\nports:\n  80: http\n  443: &tls https\n"
/// );
/// // Setting a value to what it already is changes no byte.
/// doc.get_mut("ports")?.get_mut(80)?.set("http")?;
/// assert!(doc.to_string().ends_with("  80: http\n  443: &tls https\n"));
/// # Ok(())
/// # }
/// ```
///
/// Mapping and sequence operations on an alias edit its anchored node;
/// [`NodeMut::set`] on an alias replaces the alias itself.
///
/// **Comments**: a mapping entry is named by its key, so on a
/// handle to a mapping **value** the comment edits act on its entry:
/// [`NodeMut::set_leading`], [`NodeMut::set_trailing`] and
/// [`NodeMut::set_blank_lines_before`] edit the key's comments and the
/// blank lines above the entry, and [`NodeMut::clear_comments`] clears
/// the entry's comments and then the value's own (a collection's footer).
/// On any other node (a key, an item, the root) they act on the node.
/// [`NodeMut::push_footer`] always acts on the node (a collection). The
/// id-taking `Document` methods are the base layer: they act on exactly
/// the node they are given (so a value's own leading comment, `k:⏎  # c⏎
/// v`, is reached with the value's id). [`NodeMut::into_key`] navigates
/// to the key explicitly. A comment edit never returns `Ok` without doing
/// what it says: what it cannot do is refused with a typed error.
#[derive(Debug)]
pub struct NodeMut<'d> {
    doc: &'d mut Document,
    id: NodeId,
}

/// A mapping entry for [`NodeMut::entry`]: present or not.
#[derive(Debug)]
pub struct Entry<'d> {
    map: NodeMut<'d>,
    key: Value,
    value: Option<NodeId>,
}

impl<'d> Entry<'d> {
    /// `true` when the mapping has its own entry for the key.
    #[must_use]
    pub fn is_occupied(&self) -> bool {
        self.value.is_some()
    }

    /// A handle to the value, inserting `value` first when absent.
    ///
    /// # Errors
    ///
    /// As [`NodeMut::insert`].
    pub fn or_insert<T: Serialize>(self, value: T) -> Result<NodeMut<'d>, EditError> {
        let mut map = self.map;
        let id = match self.value {
            Some(id) => id,
            None => map.insert_id(&key::arg(&self.key), &value)?,
        };
        Ok(NodeMut { doc: map.doc, id })
    }
}

impl<'d> NodeMut<'d> {
    /// The node's handle.
    #[must_use]
    pub fn id(&self) -> NodeId {
        self.id
    }

    /// A read-only view of the node.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`].
    pub fn get(&self) -> Result<NodeRef<'_>, EditError> {
        self.doc.node(self.id)
    }

    /// The document.
    #[must_use]
    pub fn document(&self) -> &Document {
        self.doc
    }

    /// A handle to node `id` of the same document (an edit's result).
    fn at(&mut self, id: NodeId) -> NodeMut<'_> {
        NodeMut {
            doc: &mut *self.doc,
            id,
        }
    }

    /// The node with aliases followed: the collection the mapping and
    /// sequence operations change.
    fn target(&self) -> Result<NodeRef<'_>, EditError> {
        Ok(self.doc.node(self.id)?.resolved())
    }

    fn mapping(&self) -> Result<TreeId, EditError> {
        let t = self.target()?;
        t.as_mapping()
            .map(|m| m.node().tree_node().id())
            .ok_or(EditError::NotAMapping { at: Path::new() })
    }

    /// The sequence the item operations change.
    /// O(1), never its length (counting the items made every `push` onto a
    /// long sequence O(items)).
    fn sequence(&self) -> Result<TreeId, EditError> {
        let t = self.target()?;
        t.as_sequence()
            .map(|s| s.node().tree_node().id())
            .ok_or(EditError::NotASequence { at: Path::new() })
    }

    /// The value for `key` (any [`AsKey`](crate::AsKey); see
    /// [`MappingRef::get`](crate::MappingRef::get)), consuming this handle.
    ///
    /// A `Result`, unlike `std`'s `get_mut`: the error says why there is
    /// nothing to edit.
    ///
    /// # Errors
    ///
    /// [`EditError::NotAMapping`], [`EditError::NotFound`],
    /// [`EditError::StaleNode`].
    pub fn get_mut<K: AsKey>(self, key: K) -> Result<NodeMut<'d>, EditError> {
        let k = key::arg(&key);
        let id = {
            let t = self.target()?;
            let m = t
                .as_mapping()
                .ok_or(EditError::NotAMapping { at: Path::new() })?;
            m.get(&key)
                .ok_or_else(|| EditError::NotFound {
                    at: Path::new().with_key(k.label()),
                })?
                .id()
        };
        Ok(NodeMut { doc: self.doc, id })
    }

    /// Item `index` of a sequence, consuming this handle.
    ///
    /// # Errors
    ///
    /// [`EditError::NotASequence`], [`EditError::IndexOutOfBounds`],
    /// [`EditError::StaleNode`].
    pub fn get_index_mut(self, index: usize) -> Result<NodeMut<'d>, EditError> {
        let id = {
            let t = self.target()?;
            let s = t
                .as_sequence()
                .ok_or(EditError::NotASequence { at: Path::new() })?;
            s.get(index)
                .ok_or(EditError::IndexOutOfBounds {
                    at: Path::new(),
                    index,
                    len: s.len(),
                })?
                .id()
        };
        Ok(NodeMut { doc: self.doc, id })
    }

    /// Replaces this node by `value`, keeping its anchor;
    /// the entry's comments stay. The handle follows the new node.
    /// A value equal to the node's (as [`Value`] equality says) changes no
    /// byte: `k: |+` set to its own value stays a `|+` block scalar. A
    /// node that is or holds an alias or a merge key is written as any
    /// value even then: `set` on an alias replaces the alias itself (so a
    /// later edit of the anchored node no longer shows there); so is a
    /// node holding repeated keys (`DuplicateKeys::LastWins`), which drops
    /// the repeats. The check costs O(the node), never what its aliases
    /// reach.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let src = "k: |+\n  x\n\nn: 0x10 # hex\n";
    /// let mut doc = Document::parse(src).unwrap();
    /// doc.get_mut("k").unwrap().set("x\n\n").unwrap();
    /// doc.get_mut("n").unwrap().set(&16).unwrap();
    /// assert_eq!(doc.to_string(), src);
    /// doc.get_mut("n").unwrap().set(&17).unwrap().set_anchor(Some("n")).unwrap();
    /// assert_eq!(doc.to_string(), "k: |+\n  x\n\nn: &n 17 # hex\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::set_path`]; [`EditError::Unsupported`] for a mapping
    /// key; [`EditError::AnchorInUse`] when a replaced descendant's anchor
    /// is used by an alias outside. Nothing changes on error.
    pub fn set<T: Serialize>(&mut self, value: T) -> Result<&mut Self, EditError> {
        let v = to_value(&value, self.doc)?;
        let target = self.doc.tree_node(self.id)?.id();
        self.id = NodeId(self.doc.op_set(target, &v, None)?);
        Ok(self)
    }

    /// Sets a scalar's content to the string `text`, keeping the scalar's
    /// quoting style when it can hold `text` (style inference: `'a'`
    /// stays single-quoted, `"a"` double-quoted), else plain unless the
    /// document's schema reads `text` as another type (as
    /// [`Document::set_path`] writes a string). The handle follows the new
    /// node.
    ///
    /// # Errors
    ///
    /// [`EditError::Unsupported`] when the node is not a scalar; as
    /// [`NodeMut::set`].
    pub fn set_scalar(&mut self, text: &str) -> Result<&mut Self, EditError> {
        let (target, style) = {
            let r = self.doc.node(self.id)?;
            let s = r
                .as_scalar()
                .filter(|_| r.kind() == NodeKind::Scalar)
                .ok_or_else(|| unsupported("set_scalar on a node that is not a scalar"))?;
            (r.tree_node().id(), s.style())
        };
        let forced = {
            let render = self.doc.render(None, false)?;
            render.keep_style(text, style)
        };
        let v = Value::from(text);
        self.id = NodeId(self.doc.op_set(target, &v, forced.as_deref())?);
        Ok(self)
    }

    /// Sets this mapping's own entry for `key` (any
    /// [`AsKey`](crate::AsKey): `"name"`, `8080`, `true`, a [`Value`]) to
    /// `value`: replaces its value (keeping the entry's comments and the
    /// value's anchor), or appends a new entry laid out like its siblings.
    /// A key that is only merged in (`<<`) gets an own, shadowing
    /// entry. Returns a handle to the value.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("ports:\n  80: http\n").unwrap();
    /// let mut ports = doc.get_mut("ports").unwrap();
    /// ports.insert(443, "https").unwrap().set_anchor(Some("tls")).unwrap();
    /// ports.insert(String::from("8080"), "alt").unwrap();
    /// assert_eq!(doc.to_string(), "ports:\n  80: http\n  443: &tls https\n  '8080': alt\n");
    /// assert!(doc.get("ports").unwrap().get(443).is_some());
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::NotAMapping`]; [`EditError::Unsupported`] for a
    /// collection key; as [`Document::set_path`].
    pub fn insert<K: AsKey, T: Serialize>(
        &mut self,
        key: K,
        value: T,
    ) -> Result<NodeMut<'_>, EditError> {
        let id = self.insert_id(&key::arg(&key), &value)?;
        Ok(self.at(id))
    }

    fn insert_id<T: ?Sized + Serialize>(
        &mut self,
        key: &KeyArg<'_>,
        value: &T,
    ) -> Result<NodeId, EditError> {
        let v = to_value(value, self.doc)?;
        let map = self.mapping()?;
        if key.is_collection() {
            return Err(unsupported("inserting an entry whose key is a collection"));
        }
        let own = {
            let m = self
                .target()?
                .as_mapping()
                .ok_or(EditError::NotAMapping { at: Path::new() })?;
            m.own_value_of(KeyQuery::of(key))
                .map(|v| v.tree_node().id())
        };
        let at = Path::new().with_key(key.label());
        let new = match (own, key.as_str()) {
            (Some(value_id), _) => self.doc.op_set(value_id, &v, None),
            (None, Some(k)) => self
                .doc
                .op_insert(map, Some(k), Where::Append, Payload::Value(&v)),
            (None, None) => {
                let k = key.to_value();
                self.doc
                    .op_insert(map, None, Where::Append, Payload::Entry(&k, &v))
            }
        }
        .map_err(|e| e.located(&at))?;
        Ok(NodeId(new))
    }

    /// A mapping entry, to read or insert (`entry(key)?.or_insert(&v)`),
    /// consuming this handle.
    ///
    /// # Errors
    ///
    /// [`EditError::NotAMapping`], [`EditError::StaleNode`].
    pub fn entry<K: AsKey>(self, key: K) -> Result<Entry<'d>, EditError> {
        let k = key::arg(&key);
        let value = {
            let m = self
                .target()?
                .as_mapping()
                .ok_or(EditError::NotAMapping { at: Path::new() })?;
            m.own_value_of(KeyQuery::of(&k)).map(|v| v.id())
        };
        let key = k.to_value();
        Ok(Entry {
            map: self,
            key,
            value,
        })
    }

    /// Removes this mapping's own entry for `key` (any
    /// [`AsKey`](crate::AsKey)) and returns it. Removing a block
    /// collection's last entry turns it into `{}`/`[]`, a new node (this
    /// handle then follows it).
    ///
    /// # Errors
    ///
    /// [`EditError::NotFound`]; [`EditError::AnchorInUse`];
    /// the other edit errors.
    pub fn remove<K: AsKey>(&mut self, key: K) -> Result<Removed, EditError> {
        let k = key::arg(&key);
        let map = self.mapping()?;
        let value = self
            .target()?
            .as_mapping()
            .and_then(|m| m.own_value_of(KeyQuery::of(&k)).map(|v| v.tree_node().id()))
            .ok_or_else(|| EditError::NotFound {
                at: Path::new().with_key(k.label()),
            })?;
        self.remove_value(map, value)
    }

    fn remove_value(&mut self, coll: TreeId, value: TreeId) -> Result<Removed, EditError> {
        let alias = self.doc.node(self.id)?.kind() == NodeKind::Alias;
        let (r, new) = self.doc.op_remove_any(coll, value, true, false)?;
        if let (Some(n), false) = (new, alias) {
            self.id = NodeId(n);
        }
        Ok(r)
    }

    /// Removes item `index` of a sequence and returns it.
    ///
    /// # Errors
    ///
    /// [`EditError::NotASequence`], [`EditError::IndexOutOfBounds`];
    /// as [`NodeMut::remove`].
    pub fn remove_at(&mut self, index: usize) -> Result<Removed, EditError> {
        let seq = self.sequence()?;
        let value =
            nth_value(self.target()?, index)?.map_err(|len| EditError::IndexOutOfBounds {
                at: Path::new(),
                index,
                len,
            })?;
        self.remove_value(seq, value)
    }

    /// Appends `value` to a sequence (laid out like its items).
    /// Returns a handle to the new item.
    ///
    /// # Errors
    ///
    /// [`EditError::NotASequence`]; as [`Document::set_path`].
    pub fn push<T: Serialize>(&mut self, value: T) -> Result<NodeMut<'_>, EditError> {
        let v = to_value(&value, self.doc)?;
        let seq = self.sequence()?;
        let id = self
            .doc
            .op_insert(seq, None, Where::Append, Payload::Value(&v))?;
        Ok(self.at(NodeId(id)))
    }

    /// Inserts `value` into a sequence before item `index` (`index` =
    /// length appends). Returns a handle to the new item.
    ///
    /// # Errors
    ///
    /// [`EditError::IndexOutOfBounds`] past the length; as
    /// [`NodeMut::push`].
    pub fn insert_at<T: Serialize>(
        &mut self,
        index: usize,
        value: T,
    ) -> Result<NodeMut<'_>, EditError> {
        let v = to_value(&value, self.doc)?;
        let seq = self.sequence()?;
        let at = self.position(index, true)?;
        let id = self.doc.op_insert(seq, None, at, Payload::Value(&v))?;
        Ok(self.at(NodeId(id)))
    }

    /// Replaces item `index` of a sequence (keeping its anchor; an equal
    /// value changes nothing, as [`NodeMut::set`]). Returns a handle to
    /// the item.
    ///
    /// # Errors
    ///
    /// [`EditError::IndexOutOfBounds`]; as [`NodeMut::set`].
    pub fn replace_at<T: Serialize>(
        &mut self,
        index: usize,
        value: T,
    ) -> Result<NodeMut<'_>, EditError> {
        let v = to_value(&value, self.doc)?;
        let item = {
            let t = self.target()?;
            let s = t
                .as_sequence()
                .ok_or(EditError::NotASequence { at: Path::new() })?;
            s.get(index)
                .ok_or(EditError::IndexOutOfBounds {
                    at: Path::new(),
                    index,
                    len: s.len(),
                })?
                .tree_node()
                .id()
        };
        let id = self.doc.op_set(item, &v, None)?;
        Ok(self.at(NodeId(id)))
    }

    /// Moves entry (mapping) or item (sequence) `from` to position `to`
    /// (reorder); the moved entry keeps its text and comments, as a
    /// new node. Refused when an alias would change its anchor: one inside
    /// the entry whose anchor is not the nearest preceding one at the new
    /// place, or one outside that the entry's anchors would capture or
    /// lose.
    ///
    /// # Errors
    ///
    /// [`EditError::IndexOutOfBounds`], [`EditError::AliasBeforeAnchor`],
    /// and the other edit errors.
    pub fn move_entry(&mut self, from: usize, to: usize) -> Result<(), EditError> {
        let coll = {
            let t = self.target()?;
            match (t.as_mapping(), t.as_sequence()) {
                (Some(m), _) => m.node().tree_node().id(),
                (_, Some(s)) => s.node().tree_node().id(),
                _ => return Err(EditError::NotAMapping { at: Path::new() }),
            }
        };
        let oob = |index, len| EditError::IndexOutOfBounds {
            at: Path::new(),
            index,
            len,
        };
        let value = nth_value(self.target()?, from)?.map_err(|len| oob(from, len))?;
        if from == to {
            return Ok(());
        }
        // The entry `to` names once `from` is out: before the entry now at
        // `to` (moving up) or `to + 1` (moving down), else at the end.
        let dest = if to < from { to } else { to.saturating_add(1) };
        let at = match nth_value(self.target()?, dest)? {
            Ok(v) => Where::Before(v),
            Err(len) if dest == len && to < len => Where::Append,
            Err(len) => return Err(oob(to, len)),
        };
        self.doc.op_move(coll, value, at)
    }

    /// Where position `index` is: before entry `index`, or the end
    /// (`index` = length; past it only when `clamp`).
    fn position(&self, index: usize, strict: bool) -> Result<Where, EditError> {
        match nth_value(self.target()?, index)? {
            Ok(v) => Ok(Where::Before(v)),
            Err(len) if index == len || !strict => Ok(Where::Append),
            Err(len) => Err(EditError::IndexOutOfBounds {
                at: Path::new(),
                index,
                len,
            }),
        }
    }

    /// Re-inserts a [`Removed`] entry (into a mapping, at the end) or item
    /// (into a sequence, at the end) of the same document: a move. Returns
    /// a handle to the re-inserted value.
    ///
    /// # Errors
    ///
    /// A [`ReinsertError`], which hands the [`Removed`] back (a
    /// refusal destroys nothing), with the reason:
    /// [`EditError::WrongDocument`]; [`EditError::KeyExists`];
    /// [`EditError::AliasBeforeAnchor`]/[`EditError::DuplicateAnchor`];
    /// [`EditError::Unsupported`] across kinds (an entry into a
    /// sequence) and, as documented 0.2 limitations, for
    /// multi-line block text or comment lines moved into a flow
    /// collection, and a flow entry whose removal took comments.
    pub fn insert_removed(&mut self, removed: Removed) -> Result<NodeMut<'_>, ReinsertError> {
        self.insert_removed_at(usize::MAX, removed)
    }

    /// [`NodeMut::insert_removed`] before entry `index` (past the end:
    /// appended).
    ///
    /// # Errors
    ///
    /// As [`NodeMut::insert_removed`].
    pub fn insert_removed_at(
        &mut self,
        index: usize,
        removed: Removed,
    ) -> Result<NodeMut<'_>, ReinsertError> {
        match self.reinsert(index, &removed) {
            Ok(id) => Ok(self.at(id)),
            Err(error) => Err(ReinsertError {
                error,
                removed: Box::new(removed),
            }),
        }
    }

    fn reinsert(&mut self, index: usize, removed: &Removed) -> Result<NodeId, EditError> {
        let coll = {
            let t = self.target()?;
            match (t.as_mapping(), t.as_sequence()) {
                (Some(m), _) => m.node().tree_node().id(),
                (_, Some(s)) => s.node().tree_node().id(),
                _ => return Err(EditError::NotAMapping { at: Path::new() }),
            }
        };
        let at = if index == usize::MAX {
            Where::Append
        } else {
            self.position(index, false)?
        };
        let key = removed.key.clone();
        self.doc
            .op_insert(coll, key.as_deref(), at, Payload::Removed(removed))
            .map(NodeId)
    }

    /// Appends an alias to anchored node `target` to a sequence
    /// (`target` must be the nearest preceding anchor of its name there,
    /// so the alias refers to it). Returns a handle to the alias.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let src = "defaults: &defaults {retries: 3}\npolicies:\n  - *defaults\n";
    /// let mut doc = Document::parse(src).unwrap();
    /// let defaults = doc.id_of(&path!["defaults"]).unwrap();
    /// doc.get_mut("policies").unwrap().push_alias(defaults).unwrap();
    /// assert_eq!(doc.to_string(), format!("{src}  - *defaults\n"));
    /// let meaning = doc.to_value().unwrap();
    /// assert_eq!(meaning["policies"][1]["retries"].as_u64(), Some(3));
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::NotAnchored`], [`EditError::AliasBeforeAnchor`],
    /// [`EditError::Unsupported`] (inside its own anchored node).
    pub fn push_alias(&mut self, target: NodeId) -> Result<NodeMut<'_>, EditError> {
        let seq = self.sequence()?;
        let (name, t) = self.alias_name_for(target, seq)?;
        let id = self
            .doc
            .op_insert(seq, None, Where::Append, Payload::Alias(&name, t))?;
        Ok(self.at(NodeId(id)))
    }

    /// Sets this mapping's entry `key` (any [`AsKey`](crate::AsKey)) to an
    /// alias of anchored node `target` (a new entry; an existing own entry
    /// is refused with [`EditError::KeyExists`]). Returns a handle to the
    /// alias.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let mut doc = Document::parse("a: &x 1\nm: {}\n").unwrap();
    /// let x = doc.id_of(&path!["a"]).unwrap();
    /// let mut m = doc.get_mut("m").unwrap();
    /// m.insert_alias("k", x).unwrap();
    /// m.insert_alias(7, x).unwrap();
    /// assert_eq!(doc.to_string(), "a: &x 1\nm: {k: *x, 7: *x}\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`NodeMut::push_alias`]; [`EditError::KeyExists`];
    /// [`EditError::Unsupported`] for a collection key.
    pub fn insert_alias<K: AsKey>(
        &mut self,
        key: K,
        target: NodeId,
    ) -> Result<NodeMut<'_>, EditError> {
        let k = key::arg(&key);
        let map = self.mapping()?;
        if k.is_collection() {
            return Err(unsupported("inserting an entry whose key is a collection"));
        }
        let exists = self
            .target()?
            .as_mapping()
            .and_then(|m| m.own_value_of(KeyQuery::of(&k)))
            .is_some();
        if exists {
            return Err(EditError::KeyExists {
                at: Path::new().with_key(k.label()),
            });
        }
        let (name, t) = self.alias_name_for(target, map)?;
        let id = match k.as_str() {
            Some(s) => self
                .doc
                .op_insert(map, Some(s), Where::Append, Payload::Alias(&name, t))?,
            None => {
                let kv = k.to_value();
                self.doc
                    .op_insert(map, None, Where::Append, Payload::AliasEntry(&kv, &name, t))?
            }
        };
        Ok(self.at(NodeId(id)))
    }

    /// Adds a **merge key** to this mapping: `<<: *a` for one
    /// target, `<<: [*a, *b]` for several (YAML 1.1 merge, earlier
    /// targets win over later ones, and the mapping's own keys over all
    /// of them), as the mapping's first entry, where merge keys are
    /// conventionally written (its last, when the comments around the
    /// first entry would change owner there, or a target's anchor is in
    /// the mapping itself). The key is written
    /// as a plain `<<`, which is what makes it a merge key; a *string* key
    /// `<<` ([`NodeMut::insert_alias`]) is written quoted and merges
    /// nothing.
    ///
    /// Whether it merges is the document's setting
    /// ([`Settings::merge_keys`](crate::Settings::merge_keys)): with it
    /// on, [`Document::to_value`], serde, and navigation read the merged
    /// keys; with it off, the entry is the string key `<<` whose
    /// value is the alias(es), for every reader alike. Otherwise a merge
    /// key gets no special treatment: each target must be
    /// the nearest preceding anchor of its name, and the
    /// mapping's own entries keep shadowing merged ones. Returns a handle
    /// to the value (the alias, or the flow sequence of aliases).
    ///
    /// ```
    /// use oneyaml::{Document, Settings, path};
    ///
    /// let settings = Settings::new().merge_keys(true);
    /// let src = "x-common: &common\n  restart: always\nweb:\n  image: nginx\n";
    /// let mut doc = Document::parse_with(src, &settings).unwrap();
    /// let common = doc.id_of(&path!["x-common"]).unwrap();
    /// doc.get_mut("web").unwrap().insert_merge(&[common]).unwrap();
    /// assert_eq!(
    ///     doc.to_string(),
    ///     "x-common: &common\n  restart: always\nweb:\n  <<: *common\n  image: nginx\n"
    /// );
    /// let restart = doc.get_path(&path!["web", "restart"]).unwrap();
    /// assert_eq!(restart.text(), "always");
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::Unsupported`] for no target, a target a merge cannot
    /// read (`Y1016`: not a mapping, or a mapping with a tag other than
    /// `!!map`, e.g. `!custom {p: 1}`), or a target that
    /// holds this mapping (an alias inside its own anchored node);
    /// [`EditError::KeyExists`] when the mapping already has a merge key
    /// (or, with `merge_keys` off, a `<<` key); [`EditError::NotAMapping`]
    /// for a node that is not a mapping; as [`NodeMut::push_alias`]
    /// otherwise ([`EditError::NotAnchored`],
    /// [`EditError::AliasBeforeAnchor`]).
    pub fn insert_merge(&mut self, targets: &[NodeId]) -> Result<NodeMut<'_>, EditError> {
        let map = self.mapping()?;
        if targets.is_empty() {
            return Err(unsupported("a merge key without a target"));
        }
        let exists = self
            .target()?
            .as_mapping()
            .map(|m| Ok::<_, Error>(m.has_merge_key()? || m.own_value("<<").is_some()))
            .transpose()
            .map_err(tag_internal)?
            .unwrap_or(false);
        if exists {
            return Err(EditError::KeyExists {
                at: Path::new().with_key("<<"),
            });
        }
        let mut uses = Vec::with_capacity(targets.len());
        for t in targets {
            let (name, tid) = self.alias_name_for(*t, map)?;
            let is_map = self
                .doc
                .tree_node(*t)
                .ok()
                .map(|n| NodeRef::new(self.doc, n))
                .map(NodeRef::is_merge_source)
                .transpose()
                .map_err(tag_internal)?
                .unwrap_or(false);
            if !is_map {
                return Err(unsupported("a merge key's target must be a mapping"));
            }
            uses.push((name, tid));
        }
        // First, where merge keys are conventionally written (the position
        // has no meaning: own keys win wherever they are); last when the
        // comments around the first entry cannot keep their owners there.
        let front = self.position(0, false)?;
        let r = self
            .doc
            .op_insert(map, Some("<<"), front, Payload::Merge(&uses));
        let id = match r {
            Err(
                EditError::CommentReattaches { .. }
                | EditError::NotWellFormed { .. }
                | EditError::AliasBeforeAnchor { .. },
            ) if front != Where::Append => {
                self.doc
                    .op_insert(map, Some("<<"), Where::Append, Payload::Merge(&uses))
            }
            r => r,
        }?;
        Ok(self.at(NodeId(id)))
    }

    /// The anchor of `target` and its node, checked for an alias inside
    /// `coll`.
    fn alias_name_for(&self, target: NodeId, coll: TreeId) -> Result<(String, TreeId), EditError> {
        let t = self.doc.tree_node(target)?;
        let name = t
            .anchor()
            .map(|a| a.name.to_owned())
            .ok_or(EditError::NotAnchored { at: Path::new() })?;
        let c = self
            .doc
            .tree()
            .get(coll)
            .map_err(|_| EditError::StaleNode)?;
        if c == t || c.ancestors().any(|a| a == t) {
            return Err(unsupported("an alias inside its own anchored node"));
        }
        Ok((name, t.id()))
    }

    /// Sets, renames, or removes this node's anchor
    /// ([`Document::set_anchor`]); the handle follows the node.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("base: {a: 1}\nuse: x\n").unwrap();
    /// let base = doc.get_mut("base").unwrap().set_anchor(Some("b")).unwrap().id();
    /// doc.get_mut("use").unwrap().set_anchor(Some("u")).unwrap();
    /// assert_eq!(doc.to_string(), "base: &b {a: 1}\nuse: &u x\n");
    /// assert_eq!(doc.node(base).unwrap().anchor(), Some("b"));
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::set_anchor`].
    pub fn set_anchor(&mut self, name: Option<&str>) -> Result<&mut Self, EditError> {
        self.id = self.doc.set_anchor(self.id, name)?;
        Ok(self)
    }

    /// The key of this mapping value's entry, consuming this handle
    /// (navigation): a mapping entry is named by its key. The
    /// comment edits of a value handle already act on the entry; this
    /// gives the key itself (its handle, its text).
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("ports:\n  80: http\n").unwrap();
    /// doc.get_mut("ports")
    ///     .unwrap()
    ///     .insert(443, "https")
    ///     .unwrap()
    ///     .into_key()
    ///     .unwrap()
    ///     .set_trailing(Some("TLS"))
    ///     .unwrap();
    /// assert_eq!(doc.to_string(), "ports:\n  80: http\n  443: https # TLS\n");
    /// ```
    ///
    /// # Errors
    ///
    /// [`EditError::Unsupported`] for a node that is not the value of a
    /// mapping entry (a key, an item, the root); [`EditError::StaleNode`].
    pub fn into_key(self) -> Result<NodeMut<'d>, EditError> {
        let id = self
            .doc
            .node(self.id)?
            .key()
            .map(NodeRef::id)
            .ok_or_else(|| unsupported("into_key on a node that is not a mapping value"))?;
        Ok(NodeMut { doc: self.doc, id })
    }

    /// The node that names this node's entry for the comment edits: a
    /// mapping value's key, else the node itself.
    fn entry_owner(&self) -> Result<NodeId, EditError> {
        let r = self.doc.node(self.id)?;
        Ok(r.key().map_or(self.id, NodeRef::id))
    }

    /// Replaces the leading comments of this node's entry (for a mapping
    /// value, its key's: the lines above the entry) with one comment line
    /// per item of `lines` (any iterable of strings;
    /// [`NO_COMMENTS`](crate::NO_COMMENTS) removes them), as
    /// [`Document::set_leading`]. Returns the new comments' handles.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("a: 1\nb: 2\n").unwrap();
    /// let lines = vec![String::from("about b"), String::from("(two lines)")];
    /// doc.get_mut("b").unwrap().set_leading(&lines).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1\n# about b\n# (two lines)\nb: 2\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::set_leading`].
    pub fn set_leading<I>(&mut self, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        let id = self.entry_owner()?;
        self.doc.set_leading(id, lines)
    }

    /// Sets (or, with `None`, removes) the trailing (same-line) comment of
    /// this node's entry (for a mapping value, its key's: the comment on
    /// the entry's first line), as [`Document::set_trailing`]. Returns the
    /// comment's handle.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("ports:\n  80: http\n").unwrap();
    /// doc.get_mut("ports")
    ///     .unwrap()
    ///     .insert(443, "https")
    ///     .unwrap()
    ///     .set_trailing(Some("TLS"))
    ///     .unwrap();
    /// assert_eq!(doc.to_string(), "ports:\n  80: http\n  443: https # TLS\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::set_trailing`].
    pub fn set_trailing(&mut self, text: Option<&str>) -> Result<Option<CommentId>, EditError> {
        let id = self.entry_owner()?;
        self.doc.set_trailing(id, text)
    }

    /// Appends one comment line to the footer of this collection, as
    /// [`Document::push_footer`]. Returns the comment's handle.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("list:\n  - a\nnext: 1\n").unwrap();
    /// doc.get_mut("list").unwrap().push_footer("- b").unwrap();
    /// assert_eq!(doc.to_string(), "list:\n  - a\n  # - b\nnext: 1\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::push_footer`].
    pub fn push_footer(&mut self, text: &str) -> Result<CommentId, EditError> {
        self.doc.push_footer(self.id, text)
    }

    /// Removes every comment of this node's entry and returns their text:
    /// for a mapping value, its key's comments (leading and trailing), then
    /// the value's own (a collection's footer, leading lines of a value
    /// written below its key), as [`Document::clear_comments`] does for
    /// each. One edit: nothing changes on error.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("# a\na: # b\n  - x\n  # c\nz: 1 # t\n").unwrap();
    /// let gone = doc.get_mut("a").unwrap().clear_comments().unwrap();
    /// assert_eq!(gone, ["# a", "# b", "# c"]);
    /// assert_eq!(doc.get_mut("z").unwrap().clear_comments().unwrap(), ["# t"]);
    /// assert_eq!(doc.to_string(), "a:\n  - x\nz: 1\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::clear_comments`].
    pub fn clear_comments(&mut self) -> Result<Vec<String>, EditError> {
        let owner = self.entry_owner()?;
        let node = self.id;
        if owner == node {
            return self.doc.clear_comments(node);
        }
        self.doc.prepare()?;
        // Fuzz finding H1: nothing to clear is a no-op, as each
        // base-layer call is; the group's final check (an edit beside an
        // error region) is for edits, and refused this one.
        if !self.doc.has_comments_to_clear(owner)? && !self.doc.has_comments_to_clear(node)? {
            return Ok(Vec::new());
        }
        self.doc.atomic(|d| {
            let mut texts = d.clear_comments(owner)?;
            texts.extend(d.clear_comments(node)?);
            Ok(texts)
        })
    }

    /// Sets the blank lines right above the line where this node's entry
    /// starts (for a mapping value, above its key) to `n`, as
    /// [`Document::set_blank_lines_before`].
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let mut doc = Document::parse("a: 1\nb:\n  - x\n").unwrap();
    /// doc.get_mut("b").unwrap().set_blank_lines_before(1).unwrap();
    /// assert_eq!(doc.to_string(), "a: 1\n\nb:\n  - x\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::set_blank_lines_before`].
    pub fn set_blank_lines_before(&mut self, n: usize) -> Result<(), EditError> {
        let id = self.entry_owner()?;
        self.doc.set_blank_lines_before(id, n)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    /// `op_move` of a collection's only entry
    /// (whose removal rewrites the collection, leaving no undo steps) is
    /// the identity, whatever the destination; before the fix, a move to
    /// the end lost the entry.
    #[test]
    fn moving_the_only_entry_changes_nothing() {
        for src in ["k:\n  a: 1\n", "k:\n  - 1\n", "k: {a: 1}\n", "k: [a: 1]\n"] {
            let mut d = Document::parse(src).unwrap();
            let (coll, from) = {
                let k = d.get("k").unwrap().tree_node();
                let coll = if src.contains('[') {
                    k.children()
                        .find(|c| c.kind() == SyntaxKind::FlowPair)
                        .unwrap()
                } else {
                    k
                };
                let v = coll.children().filter(|c| c.kind().is_yaml_node()).last();
                (coll.id(), v.unwrap().id())
            };
            d.op_move(coll, from, Where::Append).unwrap();
            assert_eq!(d.to_string(), src);
        }
    }
}
