//! Consistency check (1): a tree agrees with the event stream it
//! was built from, and emits the original bytes.

use oneyaml_core::Diagnostic;

use super::{SyntaxKind, SyntaxTree, WalkEvent};
use crate::event::{EventKind, EventList, Sourced};

/// Why [`check_tree`] rejected a tree.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeMismatch {
    /// What differs, for humans.
    pub message: String,
}

impl core::fmt::Display for TreeMismatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TreeMismatch {}

fn mismatch<T>(message: String) -> Result<T, TreeMismatch> {
    Err(TreeMismatch { message })
}

/// Checks that `tree` is a lossless image of `events`:
///
/// * it emits exactly the source bytes;
/// * its tokens are, in order, exactly the events that cover bytes (every
///   trivia, scalar, alias, directive, and error event, explicit document
///   markers, flow brackets), with the same spans;
/// * its YAML nodes are, in preorder, exactly the node events, with the
///   same nesting, and read the same anchors, tags, values, alias names,
///   and block headers;
/// * its diagnostics are the stream's, in order.
///
/// # Errors
///
/// The first difference found.
pub fn check_tree(events: &EventList<'_>, tree: &SyntaxTree) -> Result<(), TreeMismatch> {
    if tree.source() != events.source() {
        return mismatch("the tree was built from another source".to_owned());
    }
    let emitted = tree.emit();
    if emitted.as_bytes() != events.source().as_bytes() {
        return mismatch(format!(
            "emit differs from the source ({} vs {} bytes)",
            emitted.len(),
            events.source().len()
        ));
    }
    check_tokens(events, tree)?;
    check_nodes(events, tree)?;
    let ours: Vec<std::borrow::Cow<'_, Diagnostic>> = tree.diagnostics().collect();
    let theirs: Vec<&Diagnostic> = events.iter().filter_map(|e| e.diagnostic()).collect();
    if ours.len() != theirs.len() || ours.iter().zip(&theirs).any(|(a, b)| **a != **b) {
        return mismatch(format!(
            "diagnostics differ: tree has {}, stream has {}",
            ours.len(),
            theirs.len()
        ));
    }
    Ok(())
}

/// The token an event becomes, if any.
fn is_token_event(kind: &EventKind, zero_width: bool) -> bool {
    match kind {
        EventKind::Trivia(_)
        | EventKind::Scalar { .. }
        | EventKind::Alias
        | EventKind::Directive(_)
        | EventKind::Error(_) => true,
        EventKind::DocumentStart { explicit } | EventKind::DocumentEnd { explicit } => *explicit,
        EventKind::MappingStart { .. }
        | EventKind::MappingEnd
        | EventKind::SequenceStart { .. }
        | EventKind::SequenceEnd => !zero_width,
        _ => false,
    }
}

fn check_tokens(events: &EventList<'_>, tree: &SyntaxTree) -> Result<(), TreeMismatch> {
    let mut theirs = events
        .iter()
        .filter(|e| is_token_event(e.kind(), e.span().is_empty()));
    for token in tree.root().descendants().filter(|n| n.is_token()) {
        let Some(event) = theirs.next() else {
            return mismatch(format!("extra token {token:?}"));
        };
        if token.span() != event.span() {
            return mismatch(format!("token {token:?} but event {event:?}"));
        }
    }
    if let Some(event) = theirs.next() {
        return mismatch(format!("no token for {event:?}"));
    }
    Ok(())
}

/// A YAML node, as either side sees it.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    class: u8,
    /// Ordinal of the enclosing YAML node, if any.
    parent: Option<usize>,
    anchor: Option<String>,
    tag: Option<String>,
    value: Option<String>,
    alias: Option<String>,
    header: Option<crate::event::BlockHeader>,
}

fn stream_nodes(events: &EventList<'_>) -> Vec<Seen> {
    let mut out = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for e in events.sourced() {
        let class = match e.kind() {
            EventKind::MappingStart { .. } => 1,
            EventKind::SequenceStart { .. } => 2,
            EventKind::Scalar { .. } => 3,
            EventKind::Alias => 4,
            EventKind::MappingEnd | EventKind::SequenceEnd => {
                open.pop();
                continue;
            }
            _ => continue,
        };
        out.push(seen(&e, class, open.last().copied()));
        if class <= 2 {
            open.push(out.len().saturating_sub(1));
        }
    }
    out
}

fn seen(e: &Sourced<'_, &crate::event::Event<'_>>, class: u8, parent: Option<usize>) -> Seen {
    Seen {
        class,
        parent,
        anchor: e.anchor().map(|a| a.name.to_owned()),
        tag: e.tag().map(|t| t.resolved.into_owned()),
        value: e.value().map(|v| v.into_owned()),
        alias: e.alias_name().map(str::to_owned),
        header: e.block_header(),
    }
}

fn tree_nodes(tree: &SyntaxTree) -> Vec<Seen> {
    let mut out = Vec::new();
    // The walk's leave steps pop an explicit stack.
    let mut open: Vec<Option<usize>> = Vec::new();
    for step in tree.root().walk() {
        match step {
            WalkEvent::Enter(n) => {
                let class = match n.kind() {
                    SyntaxKind::BlockMapping | SyntaxKind::FlowMapping | SyntaxKind::FlowPair => 1,
                    SyntaxKind::BlockSequence | SyntaxKind::FlowSequence => 2,
                    SyntaxKind::Scalar => 3,
                    SyntaxKind::Alias => 4,
                    _ => {
                        open.push(None);
                        continue;
                    }
                };
                let parent = open.iter().rev().find_map(|o| *o);
                out.push(Seen {
                    class,
                    parent,
                    anchor: n.anchor().map(|a| a.name.to_owned()),
                    tag: n.tag().map(|t| t.resolved.into_owned()),
                    value: n.value().map(|v| v.into_owned()),
                    alias: n.alias_name().map(str::to_owned),
                    header: n.block_header(),
                });
                open.push((class <= 2).then(|| out.len().saturating_sub(1)));
            }
            WalkEvent::Leave(_) => {
                open.pop();
            }
        }
    }
    out
}

fn check_nodes(events: &EventList<'_>, tree: &SyntaxTree) -> Result<(), TreeMismatch> {
    let theirs = stream_nodes(events);
    let ours = tree_nodes(tree);
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        if a != b {
            return mismatch(format!("YAML node {i}: tree {a:?}, stream {b:?}"));
        }
    }
    if ours.len() != theirs.len() {
        return mismatch(format!(
            "{} YAML nodes in the tree, {} in the stream",
            ours.len(),
            theirs.len()
        ));
    }
    Ok(())
}

/// One step of a tree's structural reading: what [`compare_trees`], and
/// `SyntaxTree`'s `PartialEq` and `Hash`, look at.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(super) enum Item<'t> {
    /// Entering an element: its kind, a token's text, a block scalar's
    /// header, and a YAML node's resolved tag.
    Enter {
        kind: SyntaxKind,
        text: Option<std::borrow::Cow<'t, str>>,
        header: Option<crate::event::BlockHeader>,
        tag: Option<std::borrow::Cow<'t, str>>,
    },
    /// Leaving the innermost element.
    Leave,
    /// A diagnostic, with the preorder position of its element.
    Diagnostic {
        at: Option<usize>,
        diagnostic: std::borrow::Cow<'t, Diagnostic>,
    },
}

/// The structural reading of `tree`, in order (a flat walk).
fn items(tree: &SyntaxTree) -> impl Iterator<Item = Item<'_>> + '_ {
    let nodes = tree.root().walk().map(|step| match step {
        WalkEvent::Enter(n) => {
            let kind = n.kind();
            Item::Enter {
                kind,
                text: kind.is_token().then(|| n.text()),
                header: n.block_header(),
                tag: n.tag().map(|t| t.resolved),
            }
        }
        WalkEvent::Leave(_) => Item::Leave,
    });
    let diagnostics = tree
        .diagnostics_by_element()
        .map(|(n, d)| Item::Diagnostic {
            at: n.map(super::Node::preorder_index),
            diagnostic: d,
        });
    nodes.chain(diagnostics)
}

/// Feeds the structural reading of `tree` to `f`.
pub(super) fn for_each_item(tree: &SyntaxTree, mut f: impl FnMut(Item<'_>)) {
    for item in items(tree) {
        f(item);
    }
}

/// Checks that two trees are **structurally equal**: the same elements in
/// preorder with the same nesting and kinds, the same token texts (so the
/// same emitted text), the same block scalar headers (spans included) and
/// resolved tags on their nodes, and the same diagnostics attached to the
/// same elements. How the trees were built or edited, their stamps, and
/// handle generations do not count. This is `SyntaxTree`'s `PartialEq`,
/// with the first difference reported. The edit layer's guarantee (see
/// *Well-formed edits* in the [module docs](super)) is stated with it:
/// `compare_trees` of `SyntaxTree::parse_with(t.emit(), t.settings())` and
/// an edited tree `t` is `Ok`, whether or not the text has errors
/// (recovered input included), with one known exception: under
/// `lenient_inline_colons`, a document holding a failed directive block
/// whose content ends in a `Y0028` warning can hide an unclosed quote's
/// read-on past a byte order mark from the validator (a known gap,
/// not yet closed). It is checked by the edit sweep of
/// `tests/reparse_sweep.rs` and the edit-script fuzzes, after every
/// accepted edit.
///
/// A diagnostic kept as a skeleton (past `max_diagnostics`)
/// holds no texts, and `compare_trees` (derived `Diagnostic` equality)
/// reports it unequal to the same diagnostic with texts. Past the cap the
/// guarantee therefore holds on the validator's own skeleton equality
/// (code, severity, primary span, label reach and a keyed fingerprint of
/// the texts, labels and fixes), not on `compare_trees`: the edited tree
/// may keep a skeleton where the re-parse has the diagnostic's texts.
///
/// # Errors
///
/// The first difference found.
pub fn compare_trees(a: &SyntaxTree, b: &SyntaxTree) -> Result<(), TreeMismatch> {
    let mut left = items(a);
    let mut right = items(b);
    let mut i = 0usize;
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ok(()),
            (Some(x), Some(y)) if x == y => {}
            (x, y) => return mismatch(format!("item {i}: {x:?} vs {y:?}")),
        }
        i = i.saturating_add(1);
    }
}
