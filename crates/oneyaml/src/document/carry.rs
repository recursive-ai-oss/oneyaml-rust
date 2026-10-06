//! Node edits carry the comments the rulebook gives them (its edit
//! semantics; `docs/comment-attachment.md` decides every owner).
//!
//! * **Removal** takes exactly the entry's owned comments (leading,
//!   trailing, and the footers of the collections inside it, wherever the
//!   parser put them) and the blank lines they carry (rule 8) into its
//!   [`Removed`](super::Removed); every other comment stays with its owner.
//!   `remove_keep_comments` leaves the leading ones, which must then read
//!   as the next entry's leading comments or the collection's footer.
//! * **Insertion** puts an entry's comments back around it, and adds none
//!   of its own; at the original place it gives back the original bytes.
//! * **Replacement** keeps the entry's comments (owned by the new value's
//!   part on their line afterwards; one inside the old value's first line
//!   is written back on the new value's line, or refused with
//!   [`HEADER_COMMENT_LIMIT`]) and the old value's inner comments go with
//!   it (`Document::replace_carry`); a nested collection an only entry
//!   leaves keeps its footer inside the new flow collection, or is refused
//!   with [`NESTED_COLLAPSE`]; **anchor edits** keep every owner
//!   (`Document::property_carry`).
//! * **Owners are checked, not assumed.** Every node edit records,
//!   before it, the comments of its **window** — the comments between the
//!   content token before the edited text and the content token after it,
//!   which no edit removes — with their rulebook owners; afterwards the
//!   window is read again, and each comment there must be the one expected
//!   (same text, in order) with the owner expected: its own for a comment
//!   that stayed, the corresponding node of the new entry for a comment
//!   carried in. Otherwise the edit is undone and refused with
//!   [`EditError::CommentReattaches`] (never a silent change of owner);
//!   when the window's comments are not the expected ones at all (comment
//!   lines read as block scalar content, or the reverse), with
//!   [`EditError::NotWellFormed`]: a meaning would change.
//!   Rule 13 (owners only move outward) makes each owner's comments
//!   contiguous, so the window is split between owners by position.
//! * **Relocation.** Where the parser puts trivia depends on what follows
//!   (C4: never at the end of a collection), so some edits rewrite comments
//!   that stay: the previous entry's same-line comment when the last entry
//!   goes, the next entry's comment lines when the first one goes. They
//!   keep their text and owner, and get a new handle; their explicit
//!   owners ([`Document::attach_comment`]) follow them.
//! * **Explicit owners** follow their comment through removal and
//!   re-insertion (recorded relative to the entry), and an explicit owner
//!   inside a removed entry follows it back when it is re-inserted.
//!
//! Cost: the window is the edited text plus the runs of comment
//! lines on either side, each decided once (`Document::decide_runs`).
//! Loops only.

use std::collections::HashMap;

use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind, SyntaxTree};

use super::comments::{self as cm, Key};
use super::place::{self, Coll};
use super::{CommentOwner, Document, EditError, NodeId, TreeId};

/// An internal invariant failed (`debug_assert!` plus an error in
/// release).
fn internal(what: &str) -> EditError {
    debug_assert!(false, "node edit comments: {what}");
    EditError::Internal
}

/// A comment's owner as seen from an entry that is removed or re-inserted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Rel {
    /// Owned by node `i` of the entry (its YAML nodes in preorder, the key
    /// first): 0 leading, 1 trailing, 2 footer.
    Inner(u8, usize),
    /// Any other owner, as it is.
    Fixed(Key),
}

/// What a comment of an edit's window must be afterwards.
#[derive(Clone, Debug)]
pub(super) struct Expect {
    pub(super) text: String,
    pub(super) want: Want,
    /// Its handle before the edit (a comment that stayed, or the handle a
    /// carried comment had before it was removed).
    pub(super) old: Option<TreeId>,
    /// A carried comment's explicit owner.
    pub(super) explicit: Option<Rel>,
}

/// The owner a comment of the window must have afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Want {
    /// This owner.
    Rel(Rel),
    /// The owner (of this kind, or any) is this node of the new entry or a
    /// node inside it (a replaced value's own comments: the new value's
    /// part on their line owns them).
    Within(usize),
    /// Its owner node was rewritten by the edit (the collection an only
    /// entry leaves becomes `{}`/`[]`): whatever the rules decide, except
    /// the document header or footer (they change only through explicit
    /// calls).
    Rewritten,
    /// Its owner was a root collection an only entry leaves, rewritten as
    /// `{}`/`[]`, which owns no footer: whatever the rules decide except
    /// the document header; its footers fall to the document footer (rule
    /// 7, as the baseline removal did).
    Collapsed,
}

/// A comment of an edit's window afterwards: its handle before (if it
/// had one), its handle now, and its carried explicit owner.
pub(super) type Moved = (Option<TreeId>, TreeId, Option<Rel>);

/// [`Document::pair_carry`]: the window, the comments as they must be
/// afterwards, and the comments outside it explicitly owned inside.
pub(super) type PairCarry = (Window, Vec<Expect>, Vec<(TreeId, Rel)>);

/// The window of an edit (see the module docs): the content tokens around
/// the edited text, which the edit keeps.
#[derive(Clone, Copy, Debug)]
pub(super) struct Window {
    pub(super) a: Option<TreeId>,
    pub(super) b: Option<TreeId>,
}

/// The maps of a node edit keyed by
/// syntax-tree handles (every comment and node of a removed entry) hash
/// them with a multiplicative mix instead of SipHash. Handles are arena
/// indices the tree assigns, never input bytes, so there is nothing to
/// flood (`RandomState` is for maps keyed by document
/// content); this made removing a large commented entry ~2x cheaper.
/// [`IdMap`]'s key type is `TreeId`, so
/// the type system keeps document content out of it; every map keyed by
/// text (anchor names, keys) keeps `RandomState`. This is in-house code,
/// not a dependency.
#[derive(Clone, Copy, Default)]
pub(super) struct IdHasher(u64);

impl IdHasher {
    fn mix(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(26) ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

impl std::hash::Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.mix(u64::from(*b));
        }
    }
    fn write_u8(&mut self, n: u8) {
        self.mix(u64::from(n));
    }
    fn write_u32(&mut self, n: u32) {
        self.mix(u64::from(n));
    }
    fn write_u64(&mut self, n: u64) {
        self.mix(n);
    }
    fn write_usize(&mut self, n: usize) {
        self.mix(n as u64);
    }
}

/// A map keyed by syntax-tree handles (see [`IdHasher`]).
pub(super) type IdMap<V> = HashMap<TreeId, V, std::hash::BuildHasherDefault<IdHasher>>;

/// Whole lines `text` without the blank lines after its first comment line
/// (comment rule 2: at the top of a document, a blank line between comment
/// lines and the root makes them the header).
pub(super) fn strip_blank_after_comment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut seen = false;
    for l in split_keep(text) {
        let blank = l.trim_matches([' ', '\t', '\n', '\r']).is_empty();
        if blank && seen {
            continue;
        }
        seen |= !blank;
        out.push_str(l);
    }
    out
}

/// `text` split after each line break (CR, LF, CRLF), breaks kept.
pub(super) fn split_keep(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while let Some(c) = b.get(i).copied() {
        let next = i.saturating_add(1);
        let end = match c {
            b'\r' if b.get(next) == Some(&b'\n') => Some(next.saturating_add(1)),
            b'\r' | b'\n' => Some(next),
            _ => None,
        };
        match end {
            Some(e) => {
                out.extend(text.get(start..e));
                start = e;
                i = e;
            }
            None => i = next,
        }
    }
    out.extend(text.get(start..).filter(|r| !r.is_empty()));
    out
}

/// The owner kind and node of an owner (`None` for the header and footer).
pub(super) fn owner_node(o: CommentOwner) -> Option<(u8, NodeId)> {
    match o {
        CommentOwner::Leading(n) => Some((0, n)),
        CommentOwner::Trailing(n) => Some((1, n)),
        CommentOwner::Footer(n) => Some((2, n)),
        _ => None,
    }
}

/// The owner of kind `k` (as [`owner_node`]) for node `n`.
pub(super) fn owner_of_kind(k: u8, n: NodeId) -> CommentOwner {
    match k {
        0 => CommentOwner::Leading(n),
        1 => CommentOwner::Trailing(n),
        _ => CommentOwner::Footer(n),
    }
}

/// The YAML nodes of an entry, in preorder: the key's subtree, then the
/// value's (an item: its node's). O(the entry).
/// The nodes of a single-pair flow mapping (or of the flow mapping it was
/// rewritten as, `skip`: the index of the inserted entry's key among its
/// YAML node children, whose entry is left out): the mapping, then its
/// (other) key's and value's nodes in preorder ([`entry_nodes`]).
pub(super) fn pair_nodes(m: TreeNode<'_>, skip: Option<usize>) -> Vec<TreeId> {
    let items: Vec<TreeNode<'_>> = m
        .children()
        .filter(|c| c.kind().is_yaml_node())
        .enumerate()
        .filter(|(i, _)| skip.is_none_or(|s| *i != s && *i != s.saturating_add(1)))
        .map(|(_, c)| c)
        .collect();
    let mut out = vec![m.id()];
    for c in items {
        out.extend(
            c.descendants()
                .filter(|n| n.kind().is_yaml_node() || n.kind() == SyntaxKind::Error)
                .map(TreeNode::id),
        );
    }
    out
}

pub(super) fn entry_nodes(key: Option<TreeNode<'_>>, value: TreeNode<'_>) -> Vec<TreeId> {
    key.into_iter()
        .chain(Some(value))
        .flat_map(TreeNode::descendants)
        .filter(|n| counted(*n))
        .map(TreeNode::id)
        .collect()
}

/// An element [`entry_nodes`] lists.
fn counted(n: TreeNode<'_>) -> bool {
    n.kind().is_yaml_node() || n.kind() == SyntaxKind::Error
}

/// The positions in [`entry_nodes`]`(key, value)` of
/// those of `targets` that it lists — what a map of the whole list answers
/// for them — without listing the entry. A target outside it is told by
/// its preorder position; the entry is read (one pass, no map per node)
/// only when a target lies in it. Removing a large entry without comments
/// inside reads none of it here.
pub(super) fn entry_positions(
    tree: &SyntaxTree,
    key: Option<TreeNode<'_>>,
    value: TreeNode<'_>,
    targets: impl IntoIterator<Item = TreeId>,
) -> IdMap<usize> {
    let mut out = IdMap::default();
    let mut targets = targets.into_iter().peekable();
    if targets.peek().is_none() {
        return out;
    }
    let part = |p: TreeNode<'_>| (p.preorder_index(), p.descendants().len());
    let parts = [key.map(part), Some(part(value))];
    let inside = |at: usize| {
        parts
            .iter()
            .flatten()
            .any(|(s, n)| *s <= at && at < s.saturating_add(*n))
    };
    let mut wanted: Vec<(usize, TreeId)> = targets
        .filter_map(|t| {
            let at = tree.get(t).ok()?.preorder_index();
            inside(at).then_some((at, t))
        })
        .collect();
    if wanted.is_empty() {
        return out;
    }
    wanted.sort_unstable_by_key(|w| w.0);
    wanted.dedup_by_key(|w| w.0);
    let mut next = wanted.iter().peekable();
    let mut count = 0usize;
    for (p, range) in [key, Some(value)].into_iter().zip(parts) {
        let (Some(p), Some((start, _))) = (p, range) else {
            continue;
        };
        for (k, n) in p.descendants().enumerate() {
            let at = start.saturating_add(k);
            while next.next_if(|w| w.0 < at).is_some() {}
            let Some(&&(w, id)) = next.peek() else {
                return out;
            };
            if !counted(n) {
                continue;
            }
            if w == at {
                out.insert(id, count);
                next.next();
            }
            count = count.saturating_add(1);
        }
    }
    out
}

/// The comments strictly between content tokens `a` and `b` (`None`: the
/// start, the end of the text), in order.
///
/// The tokens between them in order, as a token-by-token
/// walk meets them, but a subtree wholly before `b` is read with one pass
/// over its elements (a window can span a large entry); only `b`'s
/// enclosing nodes are entered. A subtree holds `b` when it is `b`'s
/// enclosing node at its depth, which the walk tracks (O(1) a node, after
/// O(depth) once). In an edited tree, where telling a subtree's extent
/// costs O(log R), the first [`WALKED`] nodes are entered as before.
/// O(window + depth).
pub(super) fn window_comments<'t>(
    root: TreeNode<'t>,
    a: Option<TreeNode<'t>>,
    b: Option<TreeNode<'t>>,
) -> Vec<TreeNode<'t>> {
    let mut out = Vec::new();
    let mut walked = if root.tree().is_edited() { 0 } else { WALKED };
    // Once needed: `b`'s enclosing nodes by depth (the root's is 0), and
    // the depth of the element at hand.
    let mut track: Option<(Vec<TreeNode<'t>>, usize)> = None;
    let mut at = match a {
        Some(a) => after(a, &mut track),
        None => Some(root),
    };
    while let Some(x) = at {
        if Some(x) == b {
            break;
        }
        if x.is_token() {
            if x.kind() == SyntaxKind::Comment {
                out.push(x);
            }
            at = after(x, &mut track);
            continue;
        }
        if walked < WALKED {
            walked = walked.saturating_add(1);
        } else {
            let (outer, depth) = track.get_or_insert_with(|| {
                let mut outer: Vec<TreeNode<'t>> =
                    b.into_iter().flat_map(TreeNode::ancestors).collect();
                outer.reverse();
                (outer, x.ancestors().count())
            });
            if outer.get(*depth) != Some(&x) {
                out.extend(x.descendants().filter(|t| t.kind() == SyntaxKind::Comment));
                at = after(x, &mut track);
                continue;
            }
        }
        at = match x.first_child() {
            Some(c) => {
                if let Some((_, depth)) = track.as_mut() {
                    *depth = depth.saturating_add(1);
                }
                Some(c)
            }
            None => after(x, &mut track),
        };
    }
    out
}

/// The nodes [`window_comments`] enters one by one, in an edited tree,
/// before it reads the subtrees wholly in the window in one pass each.
const WALKED: usize = 32;

/// The element after `n`'s subtree in preorder: its next sibling, or its
/// nearest ancestor's (`None` at the end); `track`'s depth follows it.
fn after<'t, T>(n: TreeNode<'t>, track: &mut Option<(T, usize)>) -> Option<TreeNode<'t>> {
    let mut at = n;
    loop {
        if let Some(s) = at.next_sibling() {
            return Some(s);
        }
        at = at.parent()?;
        if let Some((_, depth)) = track.as_mut() {
            *depth = depth.saturating_sub(1);
        }
    }
}

/// The content token before `t` (`None`: only trivia precedes it).
pub(super) fn visible_before(t: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = place::prev_token(t);
    while let Some(x) = at {
        if cm::visible(x) {
            return Some(x);
        }
        at = place::prev_token(x);
    }
    None
}

/// The content token after token `t` (`None`: only trivia follows it).
pub(super) fn visible_after(t: TreeNode<'_>) -> Option<TreeNode<'_>> {
    let mut at = place::next_token(t);
    while let Some(x) = at {
        if cm::visible(x) {
            return Some(x);
        }
        at = place::next_token(x);
    }
    None
}

/// Whether text is only blank (white space).
fn blank(line: &str) -> bool {
    line.chars().all(|c| c == ' ' || c == '\t')
}

/// Splits whole lines of text (each ending with a line break, the last
/// possibly not) into lines without their breaks.
pub(super) fn split_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        match rest.find(['\n', '\r']) {
            Some(i) => {
                out.push(rest.get(..i).unwrap_or("").to_owned());
                let brk = if rest.get(i..).is_some_and(|r| r.starts_with("\r\n")) {
                    2
                } else {
                    1
                };
                rest = rest.get(i.saturating_add(brk)..).unwrap_or("");
            }
            None => {
                out.push(rest.to_owned());
                rest = "";
            }
        }
    }
    out
}

/// Lines moved from column `from` to column `to`: each line's leading
/// spaces shifted by the difference (never below none); blank lines kept
/// as they are.
pub(super) fn shift_lines(lines: &[String], from: usize, to: usize) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            if from == to || blank(l) {
                return l.clone();
            }
            let t = l.trim_start_matches(' ');
            let lead = l.len().saturating_sub(t.len());
            let lead = if to >= from {
                lead.saturating_add(to.saturating_sub(from))
            } else {
                lead.saturating_sub(from.saturating_sub(to))
            };
            format!("{}{t}", " ".repeat(lead))
        })
        .collect()
}

/// The line-start element of token `t`'s line when only white space lies
/// before it on its line: the white space, or `t`.
pub(super) fn line_head(t: TreeNode<'_>) -> TreeNode<'_> {
    match place::prev_token(t).filter(|w| w.kind() == SyntaxKind::Whitespace) {
        Some(w) if cm::line_start(w) => w,
        _ => t,
    }
}

/// The token that ends the line `t` is on: the next line break, or a token
/// whose text ends its line (a block scalar's); `None` at the end of the
/// text.
pub(super) fn line_end(t: TreeNode<'_>) -> Option<TreeNode<'_>> {
    // An empty token (the text of an empty block scalar) ends nothing:
    // what ends the line is before it.
    let mut t = t;
    while t.span().is_empty() {
        match place::prev_token(t) {
            Some(p) => t = p,
            None => break,
        }
    }
    if cm::ends_line(t) {
        return Some(t);
    }
    let mut at = place::next_token(t);
    while let Some(x) = at {
        if cm::ends_line(x) {
            return Some(x);
        }
        at = place::next_token(x);
    }
    None
}

/// The siblings `a..=b` (same parent, `a` first), or `None`.
pub(super) fn siblings<'t>(a: TreeNode<'t>, b: TreeNode<'t>) -> Option<Vec<TreeNode<'t>>> {
    if a.parent() != b.parent() {
        return None;
    }
    let mut out = Vec::new();
    let mut at = Some(a);
    while let Some(x) = at {
        out.push(x);
        if x == b {
            return Some(out);
        }
        at = x.next_sibling();
    }
    None
}

/// What removing an entry takes (see [`Document::entry_carry`]).
#[derive(Clone, Debug, Default)]
pub(super) struct Carry {
    pub(super) window: Option<Window>,
    /// The entry's comments, in order: handle, text, owner, explicit owner.
    pub(super) owned: Vec<(TreeId, String, Rel, Option<Rel>)>,
    /// The window's other comments, as they must be afterwards.
    pub(super) rest: Vec<Expect>,
    /// The lines above the entry that go with it (its leading comment
    /// lines and the blank lines they and the entry carry): siblings, and
    /// their text.
    pub(super) lead: Option<(TreeId, TreeId, String)>,
    /// The lines after the entry's last line that go with it (the footers
    /// of the collections inside it): siblings, their text, and whether
    /// the range starts with the line break before them (the last one ends
    /// the text without a line break).
    pub(super) tail: Option<(TreeId, TreeId, String, bool)>,
    /// Comments outside the entry explicitly owned by a node inside it.
    pub(super) outside_explicit: Vec<(TreeId, Rel)>,
    /// Leading comments of the entry that stay (`remove_keep_comments`).
    pub(super) kept_lead: usize,
}

impl Document {
    /// The rulebook owners (explicit ones aside) of `comments`, deciding
    /// each run once.
    pub(super) fn rule_owners(&self, comments: &[TreeNode<'_>]) -> Result<IdMap<Key>, EditError> {
        let decided = self.decide_runs(comments.iter().copied());
        let mut out = IdMap::with_capacity_and_hasher(decided.len(), Default::default());
        for (c, a) in decided {
            let (o, _) = cm::owner_of(&a).ok_or_else(|| internal("unknown owner"))?;
            out.insert(c.id(), (o, a.document().map(TreeNode::id)));
        }
        Ok(out)
    }

    /// Before rewriting node `n`'s slot `first..=last` with its own text
    /// and an anchor added or removed (nothing but the node changes):
    /// the window around the slot, and every comment there keeping its
    /// owner exactly: one owned by `n` or a
    /// node inside it by the corresponding node of the rewritten node (same
    /// structure), any other by the same owner. Also the first of the
    /// comment lines right above the slot that `n`'s first entry owns
    /// ([`PropertyCarry::lead`]): a new anchor on a line of its own goes
    /// above them (on the key's or dash's line). An anchor that makes an
    /// empty root visible (or invisible) would turn header comments into
    /// footer ones: refused by the check.
    pub(super) fn property_carry(
        &self,
        n: TreeNode<'_>,
        first: TreeNode<'_>,
        last: TreeNode<'_>,
    ) -> Result<PropertyCarry, EditError> {
        let start = cm::first_token(first).or_else(|| cm::first_token(n));
        let end = cm::last_token(last).or_else(|| cm::last_token(n));
        let (a, b) = match (start, end) {
            (Some(s), Some(e)) => (visible_before(s), visible_after(e)),
            // An empty slot: the window is around the node's place.
            _ => (visible_before(n), visible_after(n)),
        };
        let comments = window_comments(self.tree().root(), a, b);
        let owners = self.rule_owners(&comments)?;
        // Every comment keeps its owner
        // exactly: one owned by `n` or a node inside it is owned by the
        // corresponding node of the rewritten node (same structure).
        // (only the window's owners are looked up.)
        let targets = owners
            .values()
            .filter_map(|k| owner_node(k.0))
            .map(|(_, x)| x.0);
        let nodes = entry_positions(self.tree(), None, n, targets);
        let mut expect = Vec::with_capacity(comments.len());
        for c in &comments {
            let key = *owners
                .get(&c.id())
                .ok_or_else(|| internal("an undecided comment"))?;
            expect.push(Expect {
                text: c.text().into_owned(),
                want: Want::Rel(Self::rel(key, &nodes)),
                old: Some(c.id()),
                explicit: None,
            });
        }
        // The comment lines right above the slot owned inside `n` (its
        // first entry's leading comments): a new anchor on a line of its
        // own goes above them, so they keep their owner.
        let mut lead = None;
        if let Some(s) = start {
            let mut at = place::prev_token(s);
            while let Some(t) = at {
                match t.kind() {
                    SyntaxKind::Whitespace | SyntaxKind::LineBreak => {}
                    SyntaxKind::Comment => {
                        let inside = owners
                            .get(&t.id())
                            .and_then(|k| owner_node(k.0))
                            .is_some_and(|(_, o)| nodes.contains_key(&o.0));
                        if !inside || !cm::line_start(line_head(t)) {
                            break;
                        }
                        lead = Some(t.id());
                    }
                    _ => break,
                }
                at = place::prev_token(t);
            }
        }
        Ok(PropertyCarry {
            window: Window {
                a: a.map(TreeNode::id),
                b: b.map(TreeNode::id),
            },
            expect,
            lead,
        })
    }

    /// `owner`'s explicit rulebook-free form relative to `nodes`.
    fn rel(key: Key, nodes: &IdMap<usize>) -> Rel {
        match owner_node(key.0) {
            Some((k, n)) => match nodes.get(&n.0) {
                Some(i) => Rel::Inner(k, *i),
                None => Rel::Fixed(key),
            },
            None => Rel::Fixed(key),
        }
    }

    /// Before rewriting single-pair flow mapping `pair` (`[k: v]`) as a
    /// flow mapping with one more entry (a fuzz finding): the
    /// window around it, and every comment there as it must be afterwards
    /// — owned by the corresponding node of the rewritten mapping (the pair
    /// itself, then its key's and value's nodes in preorder, as
    /// [`pair_nodes`] lists them), or by its owner outside as before —
    /// plus the comments outside the window explicitly owned inside the
    /// pair. A same-line comment of the pair's entry (`[k: v # c`) reads as
    /// the rewritten mapping's afterwards (`{k: v, x: 1} # c`): refused as
    /// [`EditError::CommentReattaches`], never re-owned silently.
    pub(super) fn pair_carry(&self, pair: TreeNode<'_>) -> Result<PairCarry, EditError> {
        let first = cm::first_token(pair).ok_or_else(|| internal("a pair without tokens"))?;
        let last = cm::last_token(pair).ok_or_else(|| internal("a pair without tokens"))?;
        // (A comment before the `,` ahead of the pair can be its, rule 9.)
        let mut wa = visible_before(first);
        while let Some(x) = wa.filter(|x| x.kind() == SyntaxKind::FlowEntry) {
            wa = visible_before(x);
        }
        let mut wb = visible_after(last);
        while let Some(x) = wb.filter(|x| x.kind() == SyntaxKind::FlowEntry) {
            wb = visible_after(x);
        }
        let window = Window {
            a: wa.map(TreeNode::id),
            b: wb.map(TreeNode::id),
        };
        let comments = window_comments(self.tree().root(), wa, wb);
        let owners = self.rule_owners(&comments)?;
        let list = pair_nodes(pair, None);
        let nodes: IdMap<usize> = list.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        let doc = self.doc_node().map(TreeNode::id);
        let mut expect = Vec::with_capacity(comments.len());
        for c in &comments {
            let key = *owners
                .get(&c.id())
                .ok_or_else(|| internal("an undecided comment"))?;
            expect.push(Expect {
                text: c.text().into_owned(),
                want: Want::Rel(Self::rel(key, &nodes)),
                old: Some(c.id()),
                explicit: self
                    .explicit_owner(c.id())
                    .map(|o| Self::rel((o, doc), &nodes)),
            });
        }
        let in_window: std::collections::HashSet<TreeId> =
            comments.iter().map(|c| c.id()).collect();
        let mut outside = Vec::new();
        for (c, o) in self.explicit_owned_by(&list) {
            if !in_window.contains(&c) {
                if let Rel::Inner(k, i) = Self::rel((o, doc), &nodes) {
                    outside.push((c, Rel::Inner(k, i)));
                }
            }
        }
        Ok((window, expect, outside))
    }

    /// Before removing entry `e` of block or flow collection `coll`: its
    /// comments and the window's others (see [`Carry`]). `take_blank`: the
    /// blank lines above the entry go with it (not for a collection's
    /// first entry: those lines are the collection's place, e.g. what
    /// separates the document header, comment rule 2). `keep_lead`: its
    /// leading comments stay, to be owned by this owner afterwards
    /// (`remove_keep_comments`). `around`: the window is taken around this
    /// node instead of the entry (the only entry's collection, rewritten).
    pub(super) fn entry_carry(
        &self,
        coll: TreeNode<'_>,
        e: &place::Entry<'_>,
        take_blank: bool,
        keep_lead: Option<Key>,
        around: Option<TreeNode<'_>>,
    ) -> Result<Carry, EditError> {
        let flow = Coll::of(coll).is_some_and(Coll::is_flow);
        let start = cm::first_token(e.first).ok_or_else(|| internal("an entry without tokens"))?;
        let end = cm::last_token(e.value).ok_or_else(|| internal("an entry without tokens"))?;
        let (wa, wb) = match around {
            Some(x) => {
                let f = cm::first_token(x).ok_or_else(|| internal("a node without tokens"))?;
                let l = cm::last_token(x).ok_or_else(|| internal("a node without tokens"))?;
                (visible_before(f), visible_after(l))
            }
            None => {
                // Flow: the commas around an entry may go with it.
                let mut before = visible_before(start);
                while let Some(x) = before.filter(|x| flow && x.kind() == SyntaxKind::FlowEntry) {
                    before = visible_before(x);
                }
                let mut after = visible_after(end);
                while let Some(x) = after.filter(|x| flow && x.kind() == SyntaxKind::FlowEntry) {
                    after = visible_after(x);
                }
                (before, after)
            }
        };
        let root = self.tree().root();
        let comments = window_comments(root, wa, wb);
        let owners = self.rule_owners(&comments)?;
        // Without explicit owners, only the window's
        // owners are looked up in the entry's node list (its whole list
        // otherwise: every node's explicit comments are read).
        let explicit = self.any_explicit();
        let nodes: IdMap<usize> = if explicit {
            entry_nodes(e.key, e.value)
                .into_iter()
                .enumerate()
                .map(|(i, n)| (n, i))
                .collect()
        } else {
            let targets = owners
                .values()
                .filter_map(|k| owner_node(k.0))
                .map(|(_, n)| n.0);
            entry_positions(self.tree(), e.key, e.value, targets)
        };
        let (s_at, e_at) = (start.preorder_index(), end.preorder_index());
        let mut carry = Carry {
            window: Some(Window {
                a: wa.map(TreeNode::id),
                b: wb.map(TreeNode::id),
            }),
            ..Carry::default()
        };
        // Position: 0 before the entry, 1 inside, 2 after.
        let mut seen_lead = false;
        let mut seen_other_after = false;
        let mut lead_first: Option<TreeNode<'_>> = None;
        let mut tail_last: Option<TreeNode<'_>> = None;
        for c in &comments {
            let key = *owners
                .get(&c.id())
                .ok_or_else(|| internal("an undecided comment"))?;
            let at = c.preorder_index();
            let pos = if at < s_at {
                0
            } else if at > e_at {
                2
            } else {
                1
            };
            let rel = Self::rel(key, &nodes);
            let mine = matches!(rel, Rel::Inner(..)) || pos == 1;
            let explicit = self
                .explicit_owner(c.id())
                .map(|o| Self::rel((o, key.1), &nodes));
            // What `remove_keep_comments` leaves are the
            // entry's *leading* comments — the rulebook's leading comments
            // of its first node (the key, or the item), written above the
            // entry. Other comments of the entry above it (a value's rule-11
            // comments above an empty-key `: v` line) go with the entry, as
            // does a leading comment written inside it (after `? `, `- ` or
            // its properties), which cannot stay without moving text.
            let lead = (pos == 0 && rel == Rel::Inner(0, 0))
                .then_some(keep_lead)
                .flatten();
            if let Some(home) = lead {
                // `remove_keep_comments`: the entry's leading comments
                // stay, re-homed (checked after the edit). They must all
                // be above the ones that go (the removed lines start at
                // the first of those).
                if lead_first.is_some() {
                    return Err(interleaved());
                }
                carry.rest.push(Expect {
                    text: c.text().into_owned(),
                    want: Want::Rel(Rel::Fixed(home)),
                    old: Some(c.id()),
                    explicit: None,
                });
                carry.kept_lead = carry.kept_lead.saturating_add(1);
                seen_lead = true;
                continue;
            }
            match (mine, pos) {
                (true, 0) => {
                    seen_lead = true;
                    lead_first.get_or_insert(*c);
                }
                (false, 0) if seen_lead => {
                    return Err(interleaved());
                }
                (true, 2) if seen_other_after => return Err(interleaved()),
                (true, 2) => {
                    if cm::line_start(*c) {
                        tail_last = Some(*c);
                    }
                }
                (false, 2) => seen_other_after = true,
                _ => {}
            }
            if mine {
                carry
                    .owned
                    .push((c.id(), c.text().into_owned(), rel, explicit));
            } else {
                carry.rest.push(Expect {
                    text: c.text().into_owned(),
                    want: Want::Rel(Rel::Fixed(key)),
                    old: Some(c.id()),
                    explicit: None,
                });
            }
        }
        // Comments outside the entry explicitly owned inside it (anywhere
        // in the document): their owner follows the entry back.
        let mine_ids: std::collections::HashSet<TreeId> = carry.owned.iter().map(|x| x.0).collect();
        let inner: Vec<TreeId> = if explicit {
            nodes.keys().copied().collect()
        } else {
            Vec::new()
        };
        for (c, o) in self.explicit_owned_by(&inner) {
            if !mine_ids.contains(&c) {
                let doc = self.doc_node().map(TreeNode::id);
                if let Rel::Inner(k, i) = Self::rel((o, doc), &nodes) {
                    carry.outside_explicit.push((c, Rel::Inner(k, i)));
                }
            }
        }
        if flow {
            return Ok(carry);
        }
        // The lines above: from the first leading comment's line (or,
        // taking blank lines, from the line after the last line of
        // something else) to the line break before the entry's line.
        let head = line_head(start);
        let before_head = place::prev_token(head);
        // (With `keep_lead`, `lead_first` is the first comment above the
        // entry that goes with it; the kept ones are all above it.)
        let lead_from = lead_first;
        let take_blank = take_blank && keep_lead.is_none();
        if cm::line_start(start) {
            // The line break above the entry's line. A document's first
            // line may start with a byte order mark, at stream level
            // after the comment lines (`# c⏎\u{feff}a: 1`, §5.2; a
            // fuzz finding): the comment lines go, the mark stays.
            let mut above = before_head;
            while let Some(t) = above.filter(|t| lead_from.is_some() && cm::blankish(*t)) {
                above = place::prev_token(t);
            }
            if let Some(lb) = above.filter(|t| t.kind() == SyntaxKind::LineBreak) {
                let first = match lead_from {
                    Some(c) if !take_blank => Some(line_head(c)),
                    Some(c) => Some(self.after_blank(line_head(c))),
                    None if take_blank => Some(self.after_blank(head)),
                    None => None,
                };
                if let Some(first) = first.filter(|f| *f != head) {
                    let lines = siblings(first, lb).ok_or_else(|| EditError::NotWellFormed {
                        at: oneyaml_core::Path::new(),
                    })?;
                    let text: String = lines.iter().map(|x| x.text().into_owned()).collect();
                    carry.lead = Some((first.id(), lb.id(), text));
                }
            } else if lead_from.is_some() {
                return Err(internal("leading comments above a line not ended"));
            }
        } else if lead_from.is_some() {
            return Err(internal(
                "leading comments of an entry not starting its line",
            ));
        }
        // The lines after: from the line after the entry's last line to the
        // line break after the last own-line comment of the entry.
        if let Some(last) = tail_last {
            let end_of_line = line_end(end).ok_or_else(|| internal("an entry line not ended"))?;
            let brk = last
                .next_sibling()
                .filter(|x| x.kind() == SyntaxKind::LineBreak);
            let range = match brk {
                Some(b) => place::next_token(end_of_line).map(|f| (f, b, false)),
                // The text ends with the comment: from the line break
                // before the tail (the entry's line end) through it.
                None => Some((end_of_line, last, true)),
            };
            let (f, l, shifted) = range.ok_or_else(|| internal("tail lines not found"))?;
            let lines = siblings(f, l).ok_or_else(|| EditError::NotWellFormed {
                at: oneyaml_core::Path::new(),
            })?;
            let text: String = lines.iter().map(|x| x.text().into_owned()).collect();
            carry.tail = Some((f.id(), l.id(), text, shifted));
        }
        Ok(carry)
    }

    /// The first element of the blank lines right above line-start element
    /// `head` (`head` itself without blank lines): after the line break
    /// (or line-ending text) of the last line that is not blank.
    pub(super) fn after_blank<'s>(&self, head: TreeNode<'s>) -> TreeNode<'s> {
        let mut first = head;
        let mut at = place::prev_token(head);
        // A loop back over the blank lines.
        loop {
            // `at` must be the line break ending the line above.
            let Some(lb) = at.filter(|t| t.kind() == SyntaxKind::LineBreak) else {
                return first;
            };
            // That line: white space only, then another line break (or
            // line-ending text) before it.
            let mut x = place::prev_token(lb);
            let mut ws = None;
            while let Some(w) = x.filter(|w| cm::blankish(*w)) {
                if w.kind() == SyntaxKind::Whitespace {
                    ws = Some(w);
                }
                x = place::prev_token(w);
            }
            match x {
                Some(p) if cm::ends_line(p) => {
                    first = ws.unwrap_or(lb);
                    at = Some(p).filter(|p| p.kind() == SyntaxKind::LineBreak);
                    if at.is_none() {
                        return first;
                    }
                }
                // The first line of the text is blank too.
                None => return ws.unwrap_or(lb),
                // The line above holds content or a comment: `lb` ends it.
                Some(_) => return first,
            }
        }
    }

    /// The first element of the last `k` blank lines right above
    /// line-start element `head` (fewer when there are fewer; `head` for
    /// none).
    pub(super) fn blank_lines_up<'s>(&self, head: TreeNode<'s>, k: usize) -> TreeNode<'s> {
        let top = self.after_blank(head);
        // The line starts of the blank lines, top down.
        let mut starts = Vec::new();
        let mut line = true;
        let mut at = Some(top);
        while let Some(x) = at.filter(|x| *x != head) {
            if line {
                starts.push(x);
            }
            line = x.kind() == SyntaxKind::LineBreak;
            at = place::next_token(x);
        }
        let n = starts.len();
        starts
            .get(n.saturating_sub(k))
            .copied()
            .filter(|_| k > 0)
            .unwrap_or(head)
    }

    /// The blank lines right above line-start element `head` (rule 8).
    pub(super) fn blank_lines_above(&self, head: TreeNode<'_>) -> usize {
        let mut n = 0usize;
        let mut at = Some(self.after_blank(head));
        while let Some(x) = at.filter(|x| *x != head) {
            if x.kind() == SyntaxKind::LineBreak {
                n = n.saturating_add(1);
            }
            at = place::next_token(x);
        }
        n
    }

    /// After an edit: the window between `w`'s tokens holds exactly the
    /// comments `expect` (same texts, in order), each with the owner
    /// wanted; `inner`: the new entry's nodes (for [`Rel::Inner`]). Returns
    /// each comment's old handle (if any), new handle, and explicit owner
    /// to carry.
    pub(super) fn check_window(
        &self,
        w: Window,
        expect: &[Expect],
        inner: &[TreeId],
    ) -> Result<Vec<Moved>, EditError> {
        let tree = self.tree();
        let a = match w.a {
            Some(a) => Some(
                tree.get(a)
                    .map_err(|_| internal("the window's start went"))?,
            ),
            None => None,
        };
        let b = match w.b {
            Some(b) => Some(tree.get(b).map_err(|_| internal("the window's end went"))?),
            None => None,
        };
        let found = window_comments(tree.root(), a, b);
        if found.len() != expect.len()
            || found
                .iter()
                .zip(expect)
                .any(|(f, x)| f.text().as_ref() != x.text.as_str())
        {
            // Not our invariant: the edited text reads comment lines as
            // content (below a block scalar whose indentation indicator
            // takes them in, `b: |2-` then `  # c`) or content as
            // comments. The edit would change a meaning: refused.
            return Err(EditError::NotWellFormed {
                at: oneyaml_core::Path::new(),
            });
        }
        let owners = self.rule_owners(&found)?;
        let doc = self.doc_node().map(TreeNode::id);
        let mut out = Vec::with_capacity(found.len());
        for (f, x) in found.iter().zip(expect) {
            let got = *owners
                .get(&f.id())
                .ok_or_else(|| internal("an undecided comment"))?;
            let ok = match x.want {
                Want::Rel(Rel::Fixed(k)) => got == k,
                Want::Rel(Rel::Inner(k, i)) => {
                    let n = inner
                        .get(i)
                        .ok_or_else(|| internal("a carried owner outside the entry"))?;
                    got == (owner_of_kind(k, NodeId(*n)), doc)
                }
                Want::Within(i) => {
                    let n = inner
                        .get(i)
                        .ok_or_else(|| internal("a carried owner outside the entry"))?;
                    // The new value itself, or
                    // its entry (the key of its pair), never a node inside it.
                    let key = tree.get(*n).ok().and_then(|x| {
                        let p = x.parent()?;
                        Coll::of(p)
                            .filter(|k| k.is_map())
                            .and_then(|_| place::prev_item(x).ok().flatten())
                            .filter(|k| place::is_key(p, *k))
                    });
                    owner_node(got.0)
                        .is_some_and(|(_, o)| o.0 == *n || key.is_some_and(|k| k.id() == o.0))
                }
                Want::Rewritten => !matches!(
                    got.0,
                    CommentOwner::DocumentHeader | CommentOwner::DocumentFooter
                ),
                Want::Collapsed => got.0 != CommentOwner::DocumentHeader,
            };
            if !ok {
                let (_, a) = self
                    .decide_runs(Some(*f))
                    .into_iter()
                    .find(|(c, _)| c == f)
                    .ok_or_else(|| internal("an undecided comment"))?;
                let rule = cm::rule_of(a.rule()).ok_or_else(|| internal("unknown rule"))?;
                return Err(EditError::CommentReattaches { rule });
            }
            out.push((x.old, f.id(), x.explicit));
        }
        Ok(out)
    }

    /// After an accepted node edit: explicit owners follow their comments
    /// (a comment that stayed but was rewritten, a carried comment with
    /// its owner inside the new entry resolved), `outside` (comments
    /// outside the entry explicitly owned inside it) point into the new
    /// entry `inner`, and an explicit owner naming `renamed.0` (a replaced
    /// node) names `renamed.1`. O(changed · log k).
    pub(super) fn carry_explicit(
        &mut self,
        moved: &[Moved],
        outside: &[(TreeId, Rel)],
        inner: &[TreeId],
        renamed: Option<(TreeId, TreeId)>,
    ) {
        if self.explicit.is_none() {
            return;
        }
        let doc = self.doc_node().map(TreeNode::id);
        let resolve = |r: Rel| match r {
            Rel::Inner(k, i) => inner.get(i).map(|n| owner_of_kind(k, NodeId(*n))),
            Rel::Fixed((o, _)) => Some(o),
        };
        let rename = |o: CommentOwner| match (owner_node(o), renamed) {
            (Some((k, n)), Some((from, to))) if n.0 == from => owner_of_kind(k, NodeId(to)),
            _ => o,
        };
        let mut sets: Vec<(TreeId, CommentOwner)> = Vec::new();
        let mut drops: Vec<TreeId> = Vec::new();
        for (old, new, carried) in moved {
            match (old, carried) {
                (_, Some(r)) => {
                    if let Some(o) = resolve(*r) {
                        sets.push((*new, o));
                    }
                }
                (Some(o), None) => {
                    if let Some(owner) = self.explicit_raw(*o) {
                        let owner = rename(owner);
                        if o != new || renamed.is_some() {
                            sets.push((*new, owner));
                        }
                    }
                    if o != new {
                        drops.push(*o);
                    }
                }
                (None, None) => {}
            }
        }
        // The rewritten handles, indexed once.
        let renamed: HashMap<TreeId, TreeId> = if outside.is_empty() {
            HashMap::new()
        } else {
            moved
                .iter()
                .filter_map(|(o, n, _)| Some(((*o)?, *n)))
                .collect()
        };
        for (c, r) in outside {
            // (Rewritten by this edit: its new handle.)
            let c = renamed.get(c).unwrap_or(c);
            if self.tree().get(*c).is_ok() {
                if let Some(o) = resolve(*r) {
                    sets.push((*c, o));
                }
            }
        }
        if sets.is_empty() && drops.is_empty() {
            return;
        }
        self.forget_explicit(&drops);
        for (c, o) in sets {
            if self.owner_is_live(o) {
                self.set_explicit(c, o, doc);
            }
        }
    }
}

/// What rewriting a node's properties does to the comments around it (see
/// [`Document::property_carry`]).
pub(super) struct PropertyCarry {
    pub(super) window: Window,
    pub(super) expect: Vec<Expect>,
    /// The first of the comment lines right above the node's slot that it
    /// (its first entry) owns.
    pub(super) lead: Option<TreeId>,
}

/// What replacing a node does to the comments around it (see
/// [`Document::replace_carry`]).
#[derive(Clone, Debug)]
pub(super) struct ReplaceCarry {
    pub(super) window: Window,
    pub(super) expect: Vec<Expect>,
    /// The old value's comments after it that go with it: a same-line one
    /// (siblings, the white space before it first), and whole lines.
    pub(super) same_line: Option<(TreeId, TreeId)>,
    pub(super) lines: Option<(TreeId, TreeId)>,
    /// The old value's comments before it
    /// (whole lines between its key and its content, or above a root:
    /// the leading comments of its first entry), which go with it.
    pub(super) lead: Option<(TreeId, TreeId)>,
    /// A comment inside the old value's text
    /// on its first line that the entry or the node itself owns (after
    /// its properties or a block scalar's header): its text (with the
    /// white space before it), written back on the new value's line.
    pub(super) header: Option<String>,
    /// A nested collection an only entry
    /// leaves: its footer lines (whole lines, as written), which the new
    /// flow collection holds (`{⏎  # f⏎  }`).
    pub(super) footer_lines: Option<String>,
    /// The old value's comments that go with it (handles), for their
    /// explicit owners.
    pub(super) gone: Vec<TreeId>,
}

impl Document {
    /// Before replacing node `n`, whose slot is `first..=last` (comment edit
    /// rules: "comments owned by the entry stay; comments inside the old value
    /// go with the old value"): the window around the slot; its comments that
    /// stay (the node's own leading and trailing comments, owned by the new
    /// value's part on their line afterwards; everything outside the node); and
    /// those after the slot that the old value owns (the footers of the
    /// collections in it, a same-line comment of an entry inside it), which go
    /// with it, unless `keep` (then they stay, and fall to the rules: the
    /// collection an only entry leaves).
    pub(super) fn replace_carry(
        &self,
        n: TreeNode<'_>,
        first: TreeNode<'_>,
        last: TreeNode<'_>,
        keep: bool,
    ) -> Result<ReplaceCarry, EditError> {
        let start = cm::first_token(first).ok_or_else(|| internal("a slot without tokens"))?;
        let end = cm::last_token(last).ok_or_else(|| internal("a slot without tokens"))?;
        let (a, b) = (visible_before(start), visible_after(end));
        let comments = window_comments(self.tree().root(), a, b);
        let owners = self.rule_owners(&comments)?;
        // What lies in `n` is told by preorder positions
        // (its subtree is consecutive), never by a set of its nodes.
        let (n_at, n_len) = (n.preorder_index(), n.descendants().len());
        let in_n = |at: usize| n_at <= at && at < n_at.saturating_add(n_len);
        let tree = self.tree();
        let inside = |o: TreeId| {
            tree.get(o)
                .is_ok_and(|x| x.kind().is_yaml_node() && in_n(x.preorder_index()))
        };
        let (s_at, e_at) = (start.preorder_index(), end.preorder_index());
        let n_end = cm::last_token(n).map_or(e_at, TreeNode::preorder_index);
        // A root collection rewritten as `{}`/`[]` (`keep`: an only entry
        // left): its footers fall to the document footer (`Want::Collapsed`).
        let root = n.parent().is_some_and(|p| p.kind() == SyntaxKind::Document);
        let mut expect = Vec::new();
        let mut gone = Vec::new();
        let (mut same_line, mut last_line): (Option<TreeNode<'_>>, Option<TreeNode<'_>>) =
            (None, None);
        let mut other_after = false;
        let mut lead: Option<(TreeNode<'_>, TreeNode<'_>)> = None;
        let mut header: Option<String> = None;
        let mut folded = false;
        let mut kept_inside = false;
        let n_first = cm::first_token(n);
        for c in &comments {
            let key = *owners
                .get(&c.id())
                .ok_or_else(|| internal("an undecided comment"))?;
            let at = c.preorder_index();
            let owner = owner_node(key.0);
            // By owner. The old value's
            // inner comments (owned by a node inside it, or its footer) go
            // with it, wherever they stand; the node's own leading and
            // trailing comments, and the entry's, stay.
            let within_n = at != n_at && in_n(at);
            // Fuzz finding E10 (comment
            // rule 9): a single-pair flow mapping item (`[k: v]`) has no
            // text of its own around its key, so a comment its key owns
            // written outside the pair (`[⏎# c⏎k: v]`, `[k: v # c⏎]`) is
            // the item's: it stays, the new node's, as the item's own do.
            let pair_outer = n.kind() == SyntaxKind::FlowPair
                && !within_n
                && owner.is_some_and(|(k, o)| k != 2 && inside(o.0));
            let own = owner.is_some_and(|(k, o)| o.0 == n.id() && k != 2) || pair_outer;
            let mine = owner.is_some_and(|(_, o)| inside(o.0)) && !own;
            // (An only entry leaving, `keep`: the comments inside the
            // collection's text go with the slot; the caller writes back
            // the one on its properties' line.)
            if keep && within_n {
                // (One of another owner, on the properties' line: the
                // caller writes it back after the new collection.)
                kept_inside |= !mine && !own;
                gone.push(c.id());
                continue;
            }
            let footer = owner.is_some_and(|(k, o)| o.0 == n.id() && k == 2);
            if footer && keep && !within_n {
                // An only entry left (`keep`): the collection's footers.
                if root {
                    expect.push(Expect {
                        text: c.text().into_owned(),
                        want: Want::Collapsed,
                        old: Some(c.id()),
                        explicit: None,
                    });
                    continue;
                }
                // A nested collection's footers stay its
                // own, inside the new flow collection (`k: {⏎  # f⏎  }`),
                // never handed to a sibling: whole lines after it, moved.
                if other_after || !cm::line_start(line_head(*c)) {
                    return Err(EditError::Unsupported {
                        at: oneyaml_core::Path::new(),
                        reason: NESTED_COLLAPSE,
                    });
                }
                gone.push(c.id());
                last_line = Some(*c);
                expect.push(Expect {
                    text: c.text().into_owned(),
                    want: Want::Within(0),
                    old: None,
                    explicit: None,
                });
                folded = true;
                continue;
            }
            if mine {
                gone.push(c.id());
                if within_n || (s_at <= at && at <= e_at) {
                    // Replaced with the slot.
                    continue;
                }
                if at < s_at {
                    // Whole lines before the slot (the leading comments of
                    // its first entry): removed with them.
                    if !cm::line_start(line_head(*c)) {
                        return Err(interleaved());
                    }
                    lead = Some((lead.map_or(*c, |(f, _)| f), *c));
                    continue;
                }
                if other_after {
                    return Err(interleaved());
                }
                if at > n_end && !cm::line_start(*c) {
                    same_line = Some(*c);
                } else {
                    last_line = Some(*c);
                }
                continue;
            }
            if within_n {
                // Inside the old value's text, the entry's or the node's own:
                // on its first line, it is written back on the new value's.
                let first_line = n_first.is_some_and(|f| {
                    let mut t = Some(f);
                    while let Some(x) = t {
                        if x == *c {
                            return true;
                        }
                        if x.kind() == SyntaxKind::LineBreak || cm::ends_line(x) {
                            return false;
                        }
                        t = place::next_token(x);
                    }
                    false
                });
                if !first_line || header.is_some() {
                    return Err(EditError::Unsupported {
                        at: oneyaml_core::Path::new(),
                        reason: HEADER_COMMENT_LIMIT,
                    });
                }
                let ws = c
                    .prev_sibling()
                    .filter(|w| w.kind() == SyntaxKind::Whitespace)
                    .map_or_else(|| " ".to_owned(), |w| w.text().into_owned());
                header = Some(format!("{ws}{}", c.text()));
                gone.push(c.id());
                expect.push(Expect {
                    text: c.text().into_owned(),
                    want: if own {
                        Want::Within(0)
                    } else {
                        Want::Rel(Rel::Fixed(key))
                    },
                    old: None,
                    explicit: None,
                });
                continue;
            }
            let want = if own {
                Want::Within(0)
            } else {
                if at > e_at {
                    other_after = true;
                }
                Want::Rel(Rel::Fixed(key))
            };
            expect.push(Expect {
                text: c.text().into_owned(),
                want,
                old: Some(c.id()),
                explicit: None,
            });
        }
        let lead = match lead {
            Some((f, l)) => {
                let not_wf = || EditError::NotWellFormed {
                    at: oneyaml_core::Path::new(),
                };
                let from = line_head(f);
                let to = l
                    .next_sibling()
                    .filter(|x| x.kind() == SyntaxKind::LineBreak)
                    .ok_or_else(not_wf)?;
                // Only these comments (and their lines) in between.
                // Fuzz finding E9: a byte order mark starting
                // one of the lines (`# a⏎\u{feff}# b⏎- x`) is trivia of
                // those lines too (a run of leading lines is a run of
                // sibling trivia, rule 13), and goes with them.
                let run = siblings(from, to).ok_or_else(interleaved)?;
                let ok = run.iter().all(|x| match x.kind() {
                    SyntaxKind::Whitespace | SyntaxKind::LineBreak | SyntaxKind::ByteOrderMark => {
                        true
                    }
                    SyntaxKind::Comment => gone.contains(&x.id()),
                    _ => false,
                });
                if !ok {
                    return Err(interleaved());
                }
                Some((from.id(), to.id()))
            }
            None => None,
        };
        let not_wf = || EditError::NotWellFormed {
            at: oneyaml_core::Path::new(),
        };
        let same_line = match same_line {
            Some(c) => {
                let w = c
                    .prev_sibling()
                    .filter(|w| w.kind() == SyntaxKind::Whitespace)
                    .unwrap_or(c);
                Some((w.id(), c.id()))
            }
            None => None,
        };
        // A comment on the properties' line written back after the new flow
        // collection would follow the footer lines moved into it.
        if folded && kept_inside {
            return Err(EditError::Unsupported {
                at: oneyaml_core::Path::new(),
                reason: NESTED_COLLAPSE,
            });
        }
        let mut footer_lines = None;
        let lines = match last_line {
            Some(l) => {
                let e = line_end(end).ok_or_else(not_wf)?;
                let from = place::next_token(e).ok_or_else(not_wf)?;
                let to = l
                    .next_sibling()
                    .filter(|x| x.kind() == SyntaxKind::LineBreak)
                    .ok_or_else(not_wf)?;
                siblings(from, to).ok_or_else(not_wf)?;
                if folded {
                    footer_lines = Some(place::range_text(from, to));
                }
                Some((from.id(), to.id()))
            }
            None => None,
        };
        Ok(ReplaceCarry {
            window: Window {
                a: a.map(TreeNode::id),
                b: b.map(TreeNode::id),
            },
            expect,
            same_line,
            lines,
            lead,
            header,
            footer_lines,
            gone,
        })
    }
}

/// Comments inside a replaced value's text
/// that the entry (or the node itself) owns, which the new value cannot
/// carry: more than one, one not on its first line, or one where the new
/// value leaves no place for it (a flow context, text after the value, or
/// a same-line comment of its own too when it is written on lines of its
/// own). Documented on `Document`.
pub(super) const HEADER_COMMENT_LIMIT: &str =
    "the entry's comment inside the replaced value cannot be written back there";

/// The refusal of removing the only entry of
/// a nested collection whose footer comments are not whole lines after it,
/// or that has a comment on its properties' line too (documented on
/// `Document`).
pub(super) const NESTED_COLLAPSE: &str =
    "the footer comments of a nested collection whose only entry is removed have no owner";

/// Comments of the entry and of other owners interleave in the window of
/// an edit. An internal invariant, not a refusal. Rule 13 (run
/// order) makes every owner's comments contiguous with it in each run, in
/// recovered input too (rulebook section 5: error bytes are content tokens,
/// which only split runs). So in the run above an entry, other owners'
/// comments (the closing collections' footers) come before the entry's
/// leading ones, and in the run below, the entry's inner footers before
/// other owners'; a same-line comment belongs to the token before it, so
/// one before a slot is never owned inside it; and a run of leading lines
/// is a run of sibling trivia. Never met by the edit fuzzes (bench
/// `edit_fuzz`, `stream_edit_fuzz`, `comment_fuzz`, larger seeds and
/// rounds) nor by every node edit of every yaml-test-suite input with
/// comment lines added at every line and column. A debug
/// assertion, `EditError::Internal` in release.
fn interleaved() -> EditError {
    debug_assert!(
        false,
        "comments of the entry and of other owners interleave"
    );
    EditError::Internal
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects
    )]

    use oneyaml_syntax::tree::{Fragment, Node as TreeNode, SyntaxKind, SyntaxTree};

    use super::{IdMap, entry_nodes, entry_positions, window_comments};

    /// The token walk `window_comments` stood for (before the comment-run
    /// index).
    fn walked<'t>(
        root: TreeNode<'t>,
        a: Option<TreeNode<'t>>,
        b: Option<TreeNode<'t>>,
    ) -> Vec<TreeNode<'t>> {
        let mut out = Vec::new();
        let mut at = match a {
            Some(a) => super::place::next_token(a),
            None => super::cm::first_token(root),
        };
        while let Some(t) = at {
            if Some(t) == b {
                break;
            }
            if t.kind() == SyntaxKind::Comment {
                out.push(t);
            }
            at = super::place::next_token(t);
        }
        out
    }

    fn inputs() -> Vec<String> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-suite/data/yaml-test-suite");
        let mut stack = vec![dir];
        let mut out = Vec::new();
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                    out.push(std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        assert!(out.len() > 300);
        // Windows over large entries with comments inside them.
        let mut big = String::from("# h\n\na: 0 # t\nbig:\n");
        for i in 0..80 {
            big.push_str(&format!("  # l{i}\n  k{i}: [p, {{q: {i}}}] # t{i}\n"));
        }
        big.push_str("  # f\nz: 1\n");
        out.push(big);
        out
    }

    /// Every window between two content tokens (and to the ends) of the
    /// tree: the comments the token walk finds; and every node's
    /// positions of the tree's nodes: what a map of its whole list says.
    fn check(tree: &SyntaxTree, src: &str) {
        let root = tree.root();
        let content: Vec<TreeNode<'_>> = root
            .descendants()
            .filter(|t| t.is_token() && super::cm::visible(*t))
            .collect();
        let ends = std::iter::once(None).chain(content.iter().copied().map(Some));
        let step_a = 1 + content.len() / 60;
        for (i, a) in ends.enumerate().step_by(step_a) {
            let step = 1 + content.len() / 24;
            let bs = content.iter().skip(i).step_by(step).copied().map(Some);
            for b in bs.chain([None]) {
                assert_eq!(
                    window_comments(root, a, b),
                    walked(root, a, b),
                    "{src:?} {a:?} {b:?}"
                );
            }
        }
        let all: Vec<_> = root.descendants().map(TreeNode::id).collect();
        for n in root.descendants().filter(|n| n.kind().is_yaml_node()) {
            let key = n.prev_sibling().filter(|k| k.kind().is_yaml_node());
            let list: IdMap<usize> = entry_nodes(key, n)
                .into_iter()
                .enumerate()
                .map(|(i, x)| (x, i))
                .collect();
            let got = entry_positions(tree, key, n, all.iter().copied());
            assert_eq!(got, list, "{src:?} {n:?}");
        }
    }

    /// `window_comments` finds what the token walk finds,
    /// and `entry_positions` what the entry's whole node list says, over
    /// the yaml-test-suite and a large commented entry, parsed and after
    /// edits (an edited tree takes the walk's first 32 nodes as before).
    #[test]
    fn windows_and_positions_agree_with_the_walks() {
        for src in inputs() {
            let mut tree = SyntaxTree::parse(src.as_str());
            check(&tree, &src);
            let scalars: Vec<_> = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::Scalar)
                .map(TreeNode::id)
                .collect();
            let mut edited = false;
            for (i, id) in scalars.iter().enumerate().take(6) {
                edited |= match i % 3 {
                    0 => tree.remove(*id).is_ok(),
                    1 => tree
                        .insert_after(*id, Fragment::parse("x").unwrap())
                        .is_ok(),
                    _ => tree.replace(*id, Fragment::parse("yy").unwrap()).is_ok(),
                };
            }
            if edited {
                assert!(tree.is_edited());
                check(&tree, &src);
            }
        }
    }
}
