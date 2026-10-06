//! Shared checks for the edit tests: every read of an (edited) tree
//! agrees with its emitted text and with the other reads.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Node, SyntaxKind, SyntaxTree, WalkEvent, compare_trees};

/// Every navigation and reading of `tree` is consistent with `emit()`:
/// preorder positions, handles, spans and texts, children and siblings,
/// the walk, and the diagnostics. O(n · depth): for small trees.
pub fn invariants(tree: &SyntaxTree) {
    let text = tree.emit();
    let all: Vec<Node<'_>> = tree.root().descendants().collect();
    assert_eq!(all.len(), tree.len(), "descendants vs len");
    assert_eq!(tree.root().descendants().len(), tree.len());
    let joined: String = all
        .iter()
        .filter(|n| n.is_token())
        .map(|n| n.text())
        .collect();
    assert_eq!(joined, text, "tokens vs emit");
    assert_eq!(tree.root().span().start, 0);
    // The O(1) error count agrees with the list.
    assert_eq!(
        tree.has_errors(),
        tree.diagnostics()
            .any(|d| d.severity == oneyaml_core::Severity::Error),
        "has_errors vs diagnostics"
    );
    assert_eq!(tree.root().span().end, text.len());
    for (i, n) in all.iter().enumerate() {
        assert_eq!(n.preorder_index(), i, "{n:?}");
        assert_eq!(tree.get(n.id()).unwrap(), *n);
        let sp = n.span();
        assert_eq!(n.text(), &text[sp.start..sp.end], "{n:?}");
        let children: Vec<Node<'_>> = n.children().collect();
        assert_eq!(n.first_child(), children.first().copied(), "{n:?}");
        assert_eq!(n.last_child(), children.last().copied(), "{n:?}");
        let mut at = sp.start;
        for c in &children {
            assert_eq!(c.parent(), Some(*n));
            assert_eq!(c.span().start, at, "{c:?} in {n:?}");
            at = c.span().end;
        }
        if !children.is_empty() {
            assert_eq!(at, sp.end, "{n:?}");
        }
        for pair in children.windows(2) {
            assert_eq!(pair[0].next_sibling(), Some(pair[1]));
            assert_eq!(pair[1].prev_sibling(), Some(pair[0]));
        }
        if let Some(f) = children.first() {
            assert_eq!(f.prev_sibling(), None);
        }
        if let Some(l) = children.last() {
            assert_eq!(l.next_sibling(), None);
        }
        let size = n.descendants().count();
        assert_eq!(n.descendants().len(), size);
        let walked = n
            .walk()
            .filter(|s| matches!(s, WalkEvent::Enter(_)))
            .count();
        assert_eq!(walked, size, "{n:?}");
        if n.kind() == SyntaxKind::Error {
            assert!(n.diagnostic().is_some(), "{n:?}");
        }
        // The meaning readers never panic and point into the text.
        if let Some(a) = n.anchor() {
            assert_eq!(&text[a.span.start..a.span.end], format!("&{}", a.name));
        }
        if let Some(t) = n.tag() {
            assert!(text.get(t.span.start..t.span.end).unwrap().starts_with('!'));
        }
        if let Some(h) = n.block_header() {
            assert!(text.get(h.span.start..h.span.end).is_some());
        }
        let _ = (n.value(), n.alias_name());
    }
    let mut depth = 0isize;
    let mut entered = 0;
    for step in tree.root().walk() {
        match step {
            WalkEvent::Enter(_) => {
                entered += 1;
                depth += 1;
            }
            _ => depth -= 1,
        }
        assert!(depth >= 0);
    }
    assert_eq!((entered, depth), (tree.len(), 0));
    // Diagnostics: in element order, attached to live elements, spans in
    // the text.
    let by: Vec<_> = tree.diagnostics_by_element().collect();
    assert_eq!(by.len(), tree.diagnostics().len());
    let mut last = 0usize;
    for (n, d) in &by {
        let at = n.map_or(usize::MAX, |n| n.preorder_index());
        assert!(at >= last);
        last = at;
        assert!(
            text.get(d.primary.span.start..d.primary.span.end).is_some(),
            "{d:?} in {text:?}"
        );
    }
    assert_eq!(
        tree.diagnostics_within(tree.root()).count(),
        by.iter().filter(|(n, _)| n.is_some()).count()
    );
    assert_eq!(
        tree.has_errors(),
        tree.diagnostics()
            .any(|d| d.severity == oneyaml_core::Severity::Error)
    );
}

/// The re-parse invariant: the tree parsed from the edited tree's text is
/// structurally equal to it.
pub fn reparses(tree: &SyntaxTree, settings: &Settings) {
    let again = SyntaxTree::parse_with(tree.emit(), settings);
    if let Err(e) = compare_trees(&again, tree) {
        panic!("re-parse differs: {e}\ntext {:?}", tree.emit());
    }
    assert_eq!(&again, tree);
}

/// The first element of `kind` in preorder.
pub fn find(tree: &SyntaxTree, kind: SyntaxKind) -> Node<'_> {
    tree.root()
        .descendants()
        .find(|n| n.kind() == kind)
        .unwrap_or_else(|| panic!("no {kind:?}"))
}

/// The `n`-th element of `kind` in preorder.
pub fn nth(tree: &SyntaxTree, kind: SyntaxKind, n: usize) -> Node<'_> {
    tree.root()
        .descendants()
        .filter(|x| x.kind() == kind)
        .nth(n)
        .unwrap_or_else(|| panic!("no {kind:?} #{n}"))
}

/// The scalar whose value is `v`.
pub fn scalar<'t>(tree: &'t SyntaxTree, v: &str) -> Node<'t> {
    tree.root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Scalar && n.value().as_deref() == Some(v))
        .unwrap_or_else(|| panic!("no scalar {v:?}"))
}
