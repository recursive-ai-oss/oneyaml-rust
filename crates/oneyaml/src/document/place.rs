//! Where a node sits in the syntax tree, for the edit layer: its
//! column, the entries of its collection, the slot it fills (after `key:`,
//! after `- `, at the root, in a flow collection), and the style its
//! neighbours use (style inference).
//!
//! Everything here is local: a walk back to the start of a line, the
//! entries of one collection, or one ancestor. Loops only, no recursion.

use oneyaml_core::Emit;
use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind};

use super::EditError;

/// The token right before `n` in preorder (`None` at the start).
pub(super) fn prev_token(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = n;
    loop {
        if let Some(mut p) = at.prev_sibling() {
            // Descend to the last token of that subtree.
            loop {
                if p.is_token() {
                    return Some(p);
                }
                match p.last_child() {
                    Some(c) => p = c,
                    None => break,
                }
            }
            at = p;
            continue;
        }
        at = at.parent()?;
    }
}

/// The token right after `n`'s subtree in preorder (`None` at the end).
pub(super) fn next_token(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = n;
    loop {
        if let Some(mut s) = at.next_sibling() {
            loop {
                if s.is_token() {
                    return Some(s);
                }
                match s.first_child() {
                    Some(c) => s = c,
                    None => break,
                }
            }
            at = s;
            continue;
        }
        at = at.parent()?;
    }
}

/// The column (in characters) where `n` starts: O(tokens back to the line
/// start).
pub(super) fn col_of(n: TreeNode<'_>) -> usize {
    let mut col = 0usize;
    let mut at = prev_token(n);
    while let Some(t) = at {
        // A byte order mark is not content: it starts the line it is on
        // (§5.2, like the parser's columns; a fuzz finding). White
        // space before one still counts.
        if t.kind() == SyntaxKind::ByteOrderMark {
            at = prev_token(t);
            continue;
        }
        let text = t.text();
        if let Some(i) = text.rfind(['\n', '\r']) {
            let tail = text.get(i.saturating_add(1)..).unwrap_or("");
            return col.saturating_add(tail.chars().count());
        }
        col = col.saturating_add(text.chars().count());
        at = prev_token(t);
    }
    col
}

/// [`col_of`] when `n` starts within `max` characters (and about as many
/// tokens) of its line's start; `None` past that, after O(max) steps.
pub(super) fn col_within(n: TreeNode<'_>, max: usize) -> Option<usize> {
    let mut col = 0usize;
    let mut steps = max.saturating_mul(2).saturating_add(16);
    let mut at = prev_token(n);
    while let Some(t) = at {
        steps = steps.checked_sub(1)?;
        if t.kind() == SyntaxKind::ByteOrderMark {
            at = prev_token(t);
            continue;
        }
        let text = t.text();
        if let Some(i) = text.rfind(['\n', '\r']) {
            let tail = text.get(i.saturating_add(1)..).unwrap_or("");
            return Some(col.saturating_add(tail.chars().count())).filter(|c| *c <= max);
        }
        col = col.saturating_add(text.chars().count());
        if col > max {
            return None;
        }
        at = prev_token(t);
    }
    Some(col)
}

/// How far past the entry's own text [`flow_col`] looks for its line's
/// start.
const FLOW_COL_LOOK: usize = 1024;

/// The column a flow entry of several lines starting at
/// `n` (its text `text`) is re-indented from when moved into block
/// context: its real column when its line starts within O(the entry)
/// before it (then exactly as [`col_of`]), else one less than its least
/// indented continuation line (the column it would have if that line were
/// indented just past it: its lines keep their layout relative to each
/// other). Never a walk over a long line of flow text before it, so the
/// cost is O(the entry) whatever precedes it on its line.
pub(super) fn flow_col(n: TreeNode<'_>, text: &str) -> usize {
    let max = text.len().saturating_add(FLOW_COL_LOOK);
    col_within(n, max).unwrap_or_else(|| {
        text.split(['\n', '\r'])
            .skip(1)
            .filter_map(|l| {
                let t = l.trim_start_matches(' ');
                (!t.is_empty() && !t.starts_with('#')).then(|| l.len().saturating_sub(t.len()))
            })
            .min()
            .unwrap_or(0)
            .saturating_sub(1)
    })
}

/// `true` when only white space lies between the start of `n`'s line and
/// `n`.
pub(super) fn at_line_start(n: TreeNode<'_>) -> bool {
    let mut at = prev_token(n);
    while let Some(t) = at {
        match t.kind() {
            SyntaxKind::LineBreak => return true,
            SyntaxKind::Whitespace | SyntaxKind::ByteOrderMark => at = prev_token(t),
            _ => return false,
        }
    }
    true
}

/// A YAML node child (content or error region).
pub(super) fn is_item(n: TreeNode<'_>) -> bool {
    n.kind().is_yaml_node() || n.kind() == SyntaxKind::Error
}

/// What kind of collection a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Coll {
    BlockMap,
    BlockSeq,
    FlowMap,
    FlowSeq,
    /// `[a: b]`: one entry, fixed.
    Pair,
}

impl Coll {
    pub(super) fn of(n: TreeNode<'_>) -> Option<Self> {
        Some(match n.kind() {
            SyntaxKind::BlockMapping => Self::BlockMap,
            SyntaxKind::BlockSequence => Self::BlockSeq,
            SyntaxKind::FlowMapping => Self::FlowMap,
            SyntaxKind::FlowSequence => Self::FlowSeq,
            SyntaxKind::FlowPair => Self::Pair,
            _ => return None,
        })
    }

    pub(super) fn is_map(self) -> bool {
        matches!(self, Self::BlockMap | Self::FlowMap | Self::Pair)
    }

    pub(super) fn is_flow(self) -> bool {
        !matches!(self, Self::BlockMap | Self::BlockSeq)
    }
}

/// One entry of a collection: its first element (the key, `?`, or `-`),
/// its key (mappings), and its value (or item).
#[derive(Clone, Copy, Debug)]
pub(super) struct Entry<'t> {
    pub(super) first: TreeNode<'t>,
    pub(super) key: Option<TreeNode<'t>>,
    pub(super) value: TreeNode<'t>,
}

/// The element starting the entry whose first node is `n`: `n`, or the
/// `?`/`-` indicator before it (trivia between skipped).
fn entry_start<'t>(n: TreeNode<'t>, indicator: SyntaxKind) -> TreeNode<'t> {
    let mut at = n.prev_sibling();
    while let Some(p) = at {
        if p.kind() == indicator {
            return p;
        }
        if !p.kind().is_trivia() {
            break;
        }
        at = p.prev_sibling();
    }
    n
}

/// The entries of collection `coll`, in order. O(children).
///
/// # Errors
///
/// [`EditError::ReadOnly`] when an entry is an error region;
/// [`EditError::Internal`] for a mapping with an odd number of nodes (the
/// parser pairs every key with a value).
pub(super) fn entries(coll: TreeNode<'_>) -> Result<Vec<Entry<'_>>, EditError> {
    let Some(kind) = Coll::of(coll) else {
        return Ok(Vec::new());
    };
    let items: Vec<TreeNode<'_>> = coll.children().filter(|c| is_item(*c)).collect();
    // An error region among the entries makes them read-only.
    if items.iter().any(|c| c.kind() == SyntaxKind::Error) {
        return Err(EditError::ReadOnly {
            at: oneyaml_core::Path::new(),
        });
    }
    let mut out = Vec::new();
    if kind.is_map() {
        if items.len() % 2 != 0 {
            return Err(EditError::Internal);
        }
        for pair in items.chunks(2) {
            if let [k, v] = pair {
                out.push(Entry {
                    first: entry_start(*k, SyntaxKind::MappingKey),
                    key: Some(*k),
                    value: *v,
                });
            }
        }
    } else {
        for v in items {
            let first = if kind == Coll::BlockSeq {
                entry_start(v, SyntaxKind::SequenceEntry)
            } else {
                v
            };
            out.push(Entry {
                first,
                key: None,
                value: v,
            });
        }
    }
    Ok(out)
}

fn read_only() -> EditError {
    EditError::ReadOnly {
        at: oneyaml_core::Path::new(),
    }
}

/// The nearest sibling before `n` that is not trivia.
fn prev_sig(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = n.prev_sibling();
    while let Some(p) = at.filter(|p| p.kind().is_trivia()) {
        at = p.prev_sibling();
    }
    at
}

/// The nearest sibling after `n` that is not trivia.
fn next_sig(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = n.next_sibling();
    while let Some(p) = at.filter(|p| p.kind().is_trivia()) {
        at = p.next_sibling();
    }
    at
}

/// The nearest item sibling (a YAML node) before `n`, skipping trivia and
/// indicators. In restricted mode, an error region there makes the
/// neighbourhood read-only.
pub(super) fn prev_item(n: TreeNode<'_>) -> Result<Option<TreeNode<'_>>, EditError> {
    let mut at = n.prev_sibling();
    while let Some(p) = at {
        if p.kind() == SyntaxKind::Error {
            return Err(read_only());
        }
        if p.kind().is_yaml_node() {
            return Ok(Some(p));
        }
        at = p.prev_sibling();
    }
    Ok(None)
}

/// The nearest item sibling after `n` (see [`prev_item`]).
fn next_item(n: TreeNode<'_>) -> Result<Option<TreeNode<'_>>, EditError> {
    let mut at = n.next_sibling();
    while let Some(p) = at {
        if p.kind() == SyntaxKind::Error {
            return Err(read_only());
        }
        if p.kind().is_yaml_node() {
            return Ok(Some(p));
        }
        at = p.next_sibling();
    }
    Ok(None)
}

/// The entry whose value (or item) is `v`, a child of `coll`, found from
/// its neighbours (O(local), never the whole collection).
///
/// # Errors
///
/// [`EditError::ReadOnly`] next to or at an error region (a
/// recovered tree may pair its nodes differently); [`EditError::Internal`]
/// when `v` is a key or `coll` no collection.
pub(super) fn entry_of<'t>(coll: TreeNode<'t>, v: TreeNode<'t>) -> Result<Entry<'t>, EditError> {
    let kind = Coll::of(coll).ok_or(EditError::Internal)?;
    if v.kind() == SyntaxKind::Error {
        return Err(read_only());
    }
    if kind.is_map() {
        if is_key(coll, v) {
            return Err(EditError::Internal);
        }
        let k = prev_item(v)?.ok_or_else(read_only)?;
        Ok(Entry {
            first: entry_start(k, SyntaxKind::MappingKey),
            key: Some(k),
            value: v,
        })
    } else {
        let first = if kind == Coll::BlockSeq {
            entry_start(v, SyntaxKind::SequenceEntry)
        } else {
            v
        };
        Ok(Entry {
            first,
            key: None,
            value: v,
        })
    }
}

/// The entry after `e` in `coll` (O(local)).
///
/// # Errors
///
/// As [`entry_of`].
pub(super) fn next_entry<'t>(
    coll: TreeNode<'t>,
    e: &Entry<'t>,
) -> Result<Option<Entry<'t>>, EditError> {
    let Some(n) = next_item(e.value)? else {
        return Ok(None);
    };
    if e.key.is_some() {
        let v = next_item(n)?.ok_or_else(read_only)?;
        entry_of(coll, v).map(Some)
    } else {
        entry_of(coll, n).map(Some)
    }
}

/// The entry before `e` in `coll` (O(local)).
///
/// # Errors
///
/// As [`entry_of`].
pub(super) fn prev_entry<'t>(
    coll: TreeNode<'t>,
    e: &Entry<'t>,
) -> Result<Option<Entry<'t>>, EditError> {
    let from = e.key.unwrap_or(e.value);
    match prev_item(from)? {
        Some(v) => entry_of(coll, v).map(Some),
        None => Ok(None),
    }
}

/// The last entry of `coll` (O(local)).
///
/// # Errors
///
/// As [`entry_of`].
pub(super) fn last_entry(coll: TreeNode<'_>) -> Result<Option<Entry<'_>>, EditError> {
    let Some(last) = coll.last_child() else {
        return Ok(None);
    };
    let v = if last.kind().is_yaml_node() {
        Some(last)
    } else if last.kind() == SyntaxKind::Error {
        return Err(read_only());
    } else {
        prev_item(last)?
    };
    match v {
        Some(v) => entry_of(coll, v).map(Some),
        None => Ok(None),
    }
}

/// The first entry of `coll` (O(local)).
///
/// # Errors
///
/// As [`entry_of`].
pub(super) fn first_entry(coll: TreeNode<'_>) -> Result<Option<Entry<'_>>, EditError> {
    let Some(first) = coll.first_child() else {
        return Ok(None);
    };
    let n = if first.kind().is_yaml_node() {
        Some(first)
    } else if first.kind() == SyntaxKind::Error {
        return Err(read_only());
    } else {
        next_item(first)?
    };
    let Some(n) = n else { return Ok(None) };
    if Coll::of(coll).is_some_and(Coll::is_map) {
        let v = next_item(n)?.ok_or_else(read_only)?;
        entry_of(coll, v).map(Some)
    } else {
        entry_of(coll, n).map(Some)
    }
}

/// The text of the siblings `first..=last` (O(their size)).
pub(super) fn range_text(first: TreeNode<'_>, last: TreeNode<'_>) -> String {
    let mut out = String::new();
    let mut at = Some(first);
    while let Some(n) = at {
        out.push_str(&n.text());
        if n == last {
            break;
        }
        at = n.next_sibling();
    }
    out
}

/// The slot a node fills, which decides how its replacement is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SlotKind {
    /// After `key:` of a block mapping entry whose key starts at `col`.
    MapValue { col: usize },
    /// After `-` at column `col`.
    SeqItem { col: usize },
    /// After a document's `---`.
    AfterMarker,
    /// At the start of its own line, at column `col` (a root node, or a
    /// value after a comment).
    Own { col: usize },
    /// Inside a flow collection.
    Flow,
    /// The empty value of an explicit-key block entry written without `:`
    /// (`? a` alone, as in `!!set`), whose `?` is at column `col`: the
    /// replacement writes `:` on the next line (`? a⏎: value`, which
    /// must work). The slot runs through the key
    /// line's same-line comment, which stays on that line.
    ExplicitValue { col: usize },
    /// The empty value of a flow entry written without `:` (`{a, b}`,
    /// `[? a]`): the replacement writes `: value` after the key.
    FlowValue,
}

/// A node's slot: what replacing it replaces (`first..=last`, siblings).
#[derive(Clone, Copy, Debug)]
pub(super) struct Slot<'t> {
    pub(super) kind: SlotKind,
    pub(super) first: TreeNode<'t>,
    pub(super) last: TreeNode<'t>,
}

/// The trivia siblings between the indicator of kind `indicator` and `n`:
/// `Some(first element after the indicator)` when the indicator is found
/// with only trivia in between and no comment; `None` otherwise.
fn after_indicator<'t>(n: TreeNode<'t>, indicator: SyntaxKind) -> Option<(TreeNode<'t>, bool)> {
    let mut first = n;
    let mut comment = false;
    let mut at = n.prev_sibling();
    while let Some(p) = at {
        if p.kind() == indicator {
            return Some((first, comment));
        }
        if !p.kind().is_trivia() {
            return None;
        }
        comment |= p.kind() == SyntaxKind::Comment;
        first = p;
        at = p.prev_sibling();
    }
    None
}

/// The slot of a block value or item that
/// stands on a line of its own below comment lines (`k:⏎  # c⏎  a: 1`),
/// taken from right after its `:` or `-`, the comments included, as a
/// value on the key's (or dash's) line: a new anchor goes on that line
/// (`k: &x⏎  # c⏎  a: 1`), so the comments keep their owners.
pub(super) fn indicator_slot(n: TreeNode<'_>) -> Option<Slot<'_>> {
    let parent = n.parent()?;
    match Coll::of(parent)? {
        Coll::BlockMap if !is_key(parent, n) => {
            let entry = entry_of(parent, n).ok()?;
            let (first, comment) = after_indicator(n, SyntaxKind::MappingValue)?;
            comment.then_some(Slot {
                kind: SlotKind::MapValue {
                    col: col_of(entry.first),
                },
                first,
                last: n,
            })
        }
        Coll::BlockSeq => {
            let (first, comment) = after_indicator(n, SyntaxKind::SequenceEntry)?;
            let dash = entry_start(n, SyntaxKind::SequenceEntry);
            comment.then_some(Slot {
                kind: SlotKind::SeqItem { col: col_of(dash) },
                first,
                last: n,
            })
        }
        _ => None,
    }
}

/// The slot of node `n` (a YAML node).
///
/// # Errors
///
/// [`EditError::Unsupported`] for a mapping key (keys are not replaced in
/// place) and for nodes in no known slot.
pub(super) fn slot_of(n: TreeNode<'_>) -> Result<Slot<'_>, EditError> {
    let unsupported = |reason| EditError::Unsupported {
        at: oneyaml_core::Path::new(),
        reason,
    };
    let parent = n.parent().ok_or(EditError::Internal)?;
    let own = |n: TreeNode<'_>| SlotKind::Own { col: col_of(n) };
    let slot = |kind, first| Slot {
        kind,
        first,
        last: n,
    };
    match Coll::of(parent) {
        Some(c) if c.is_flow() => {
            if c == Coll::Pair && is_key(parent, n) {
                return Err(unsupported("replacing a mapping key"));
            }
            if c == Coll::FlowMap && is_key(parent, n) {
                return Err(unsupported("replacing a mapping key"));
            }
            if c.is_map() && without_colon(n) {
                return Ok(slot(SlotKind::FlowValue, n));
            }
            Ok(slot(SlotKind::Flow, n))
        }
        Some(Coll::BlockMap) => {
            if is_key(parent, n) {
                return Err(unsupported("replacing a mapping key"));
            }
            let entry = entry_of(parent, n)?;
            match after_indicator(n, SyntaxKind::MappingValue) {
                Some((first, false)) => Ok(slot(
                    SlotKind::MapValue {
                        col: col_of(entry.first),
                    },
                    first,
                )),
                Some((_, true)) if at_line_start(n) => Ok(slot(own(n), n)),
                None if without_colon(n) => Ok(Slot {
                    kind: SlotKind::ExplicitValue {
                        col: col_of(entry.first),
                    },
                    first: n,
                    last: if key_ends_line(n) {
                        n
                    } else {
                        same_line_comment(n).unwrap_or(n)
                    },
                }),
                _ => Err(unsupported("a value in this position")),
            }
        }
        Some(Coll::BlockSeq) => match after_indicator(n, SyntaxKind::SequenceEntry) {
            Some((first, false)) => {
                let dash = entry_start(n, SyntaxKind::SequenceEntry);
                Ok(slot(SlotKind::SeqItem { col: col_of(dash) }, first))
            }
            Some((_, true)) if at_line_start(n) => Ok(slot(own(n), n)),
            _ => Err(unsupported("an item in this position")),
        },
        _ if parent.kind() == SyntaxKind::Document => {
            match after_indicator(n, SyntaxKind::DocumentStartMarker) {
                Some((first, false)) if !has_break(first, n) => {
                    Ok(slot(SlotKind::AfterMarker, first))
                }
                _ if at_line_start(n) => Ok(slot(own(n), n)),
                _ => Err(unsupported("a root node in this position")),
            }
        }
        _ => Err(unsupported("a node in this position")),
    }
}

/// `n` is a mapping value written without `:`: empty, right after its key
/// (the parser places it there, hugging the key, C4).
fn without_colon(n: TreeNode<'_>) -> bool {
    n.span().is_empty()
        && n.first_child()
            .is_none_or(|c| c.span().is_empty() && c.next_sibling().is_none())
        && n.prev_sibling().is_some_and(|p| p.kind().is_yaml_node())
}

/// The key before `n` (a value written without `:`) ends its line inside
/// its own text (a block scalar's text ends with its line break).
pub(super) fn key_ends_line(n: TreeNode<'_>) -> bool {
    prev_token(n).is_some_and(|t| t.text().ends_with(['\n', '\r']))
}

/// The comment on `n`'s line right after it (white space between only).
fn same_line_comment(n: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = n.next_sibling();
    while let Some(x) = at {
        match x.kind() {
            SyntaxKind::Whitespace => at = x.next_sibling(),
            SyntaxKind::Comment => return Some(x),
            _ => return None,
        }
    }
    None
}

/// A line break among the siblings `first..n`.
fn has_break(first: TreeNode<'_>, n: TreeNode<'_>) -> bool {
    let mut at = Some(first);
    while let Some(x) = at {
        if x == n {
            return false;
        }
        if x.kind() == SyntaxKind::LineBreak {
            return true;
        }
        at = x.next_sibling();
    }
    false
}

/// `true` when `n` is a key of mapping `map`, read from its neighbours
/// (O(local)). A key is followed by `:` unless its value is empty,
/// and follows `?`, `{`, `,`, a value, or nothing; a value follows `:`, or
/// (empty, without `:`) its key.
///
/// The ambiguous shape, an item `p` right before `n` with no indicator
/// between (the empty value of `? a` or of `{a, b}`), is decided from
/// `p`'s own neighbours (never a scan of
/// the mapping): `n` is a key exactly when `p` is a value. Only a shape
/// the parser does not build (two such items in a row) falls back to
/// counting the mapping's items.
pub(super) fn is_key(map: TreeNode<'_>, n: TreeNode<'_>) -> bool {
    local_is_key(map, n).unwrap_or_else(|| counted_key(map, n))
}

/// [`is_key`] without its O(width) fallback: `None` for the shape only
/// recovered input builds (`Document::key_in` then asks the
/// lookup index).
pub(super) fn local_is_key(_map: TreeNode<'_>, n: TreeNode<'_>) -> Option<bool> {
    match local_key(n) {
        Some(k) => Some(k),
        None => match prev_sig(n) {
            Some(p) if p.kind().is_yaml_node() => local_key(p).map(|p_is_key| !p_is_key),
            _ => None,
        },
    }
}

/// Whether `n` is a key, read from its own neighbours; `None` when an
/// item precedes it with no indicator between (see [`is_key`]).
pub(super) fn local_key(n: TreeNode<'_>) -> Option<bool> {
    if next_sig(n).is_some_and(|x| x.kind() == SyntaxKind::MappingValue) {
        return Some(true);
    }
    match prev_sig(n) {
        None => Some(true),
        Some(p) => match p.kind() {
            SyntaxKind::MappingValue => Some(false),
            SyntaxKind::MappingKey
            | SyntaxKind::FlowMappingOpen
            | SyntaxKind::FlowSequenceOpen
            | SyntaxKind::FlowEntry => Some(true),
            _ => None,
        },
    }
}

/// Whether `n` is at an even position among the items of `map` (a key):
/// O(width), the fallback of [`is_key`] only.
fn counted_key(map: TreeNode<'_>, n: TreeNode<'_>) -> bool {
    map.children()
        .filter(|c| c.kind().is_yaml_node())
        .enumerate()
        .any(|(i, c)| c == n && i % 2 == 0)
}

/// Style inference: the indentation width and sequence placement the
/// document already uses around `coll` (its entries' nested collections,
/// then its own offset from its parent entry), falling back to `emit`
/// only where there is no context.
///
/// Work: at most [`LOOK`] entries of `coll` and of one ancestor mapping
/// are read (fewer once both a nested mapping and a nested sequence are
/// found), so O(1) in the size of the document; callers ask only when the
/// text they write has a layout to copy. Retained memory: none.
pub(super) fn infer(coll: TreeNode<'_>, emit: &Emit) -> Emit {
    let mut indent = None;
    let mut seqs = None;
    look(coll, &mut indent, &mut seqs);
    // The collection's own offset from its parent entry.
    if indent.is_none() && at_line_start(coll) {
        if let Some(p) = coll
            .parent()
            .filter(|p| p.kind() == SyntaxKind::BlockMapping)
        {
            if !is_key(p, coll) {
                if let Ok(e) = entry_of(p, coll) {
                    let (kc, vc) = (col_of(e.first), col_of(coll));
                    if coll.kind() == SyntaxKind::BlockMapping && vc > kc {
                        indent = Some(vc.saturating_sub(kc));
                    }
                    if coll.kind() == SyntaxKind::BlockSequence && seqs.is_none() {
                        seqs = Some(vc > kc);
                    }
                }
            }
            if indent.is_none() || seqs.is_none() {
                look(p, &mut indent, &mut seqs);
            }
        }
    }
    let mut out = emit.clone();
    if let Some(i) = indent.filter(|i| (1..=Emit::MAX_INDENT).contains(i)) {
        out = out.indent(i);
    }
    if let Some(s) = seqs {
        out = out.indent_sequences(s);
    }
    out
}

/// How many entries style inference reads, counted from the start of a
/// mapping. It reads on until it has found both a nested mapping and a
/// nested sequence (or this many entries), so a mapping of many plain
/// entries followed by a nested one still shows how the document indents.
const LOOK: usize = 1024;

// Entries read by `look`, in tests (work units, not time).
#[cfg(test)]
thread_local! {
    pub(super) static LOOK_WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The layout the block entries of mapping `m` use for their nested block
/// collections (first found wins).
fn look(m: TreeNode<'_>, indent: &mut Option<usize>, seqs: &mut Option<bool>) {
    if m.kind() != SyntaxKind::BlockMapping {
        return;
    }
    // A sample of the first entries, never the whole mapping.
    let mut next = first_entry(m).ok().flatten();
    let mut seen = 0usize;
    while let Some(e) = next {
        if seen >= LOOK {
            break;
        }
        seen = seen.saturating_add(1);
        #[cfg(test)]
        LOOK_WORK.with(|w| w.set(w.get().saturating_add(1)));
        next = next_entry(m, &e).ok().flatten();
        let v = e.value;
        if !at_line_start(v) {
            continue;
        }
        let (kc, vc) = (col_of(e.first), col_of(v));
        if v.kind() == SyntaxKind::BlockMapping && indent.is_none() && vc > kc {
            *indent = Some(vc.saturating_sub(kc));
        }
        if v.kind() == SyntaxKind::BlockSequence && seqs.is_none() {
            *seqs = Some(vc > kc);
        }
        if indent.is_some() && seqs.is_some() {
            break;
        }
    }
}
