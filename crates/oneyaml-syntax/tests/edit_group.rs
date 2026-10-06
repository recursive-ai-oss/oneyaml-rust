//! Edit groups (a multi-step edit is all or nothing) and
//! the refusal of edits that remove an error region.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod edit_support;

use edit_support::{find, invariants, nth, reparses, scalar};
use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Fragment, SyntaxKind as K, SyntaxTree, TreeEditError};

/// The block-scalar shape: a block scalar entry, a comment line, then an entry
/// appended as lines after the block scalar (`insert_line_after` moves the
/// line break it splits across levels) and one more edit; rolling the
/// group back gives the exact text, handles, and diagnostics back.
#[test]
fn a_rolled_back_group_restores_the_tree_exactly() {
    for nl in ["\n", "\r\n", "\r"] {
        let src = format!("k: |{nl}  text{nl}# c{nl}m: 2{nl}# tail{nl}");
        let mut tree = SyntaxTree::parse(src.clone());
        let two = scalar(&tree, "2").id();
        let k = scalar(&tree, "k").id();
        let block = tree
            .root()
            .descendants()
            .find(|n| n.block_header().is_some())
            .unwrap()
            .id();
        let mark = tree.begin_group();
        assert!(tree.in_group());
        // An entry as the lines after the block scalar (its break is inside
        // its text), then one after `m: 2` (whose break is outside the
        // mapping: a copy goes in, the original moves), then a replace.
        tree.insert_line_after(block, entry("n: 1", nl)).unwrap();
        tree.insert_line_after(two, entry("o: 3", nl)).unwrap();
        let _ = tree.replace(k, Fragment::parse("kk").unwrap()).unwrap();
        assert_ne!(tree.emit(), src);
        tree.rollback_group(mark).unwrap();
        assert!(!tree.in_group());
        assert_eq!(tree.emit().as_bytes(), src.as_bytes());
        assert!(tree.get(two).is_ok() && tree.get(k).is_ok() && tree.get(block).is_ok());
        assert_eq!(tree, SyntaxTree::parse(src.clone()));
        invariants(&tree);
        reparses(&tree, &Settings::new());
    }
}

/// The elements of a one-entry block mapping `text`, with the line break
/// that ends it.
fn entry(text: &str, nl: &str) -> Fragment {
    let t = SyntaxTree::parse(format!("{text}{nl}"));
    let m = find(&t, K::BlockMapping);
    let lb = t
        .root()
        .descendants()
        .filter(|n| n.kind() == K::LineBreak)
        .last()
        .unwrap();
    t.extract(m.first_child().unwrap().id(), m.last_child().unwrap().id())
        .unwrap()
        .concat(t.extract(lb.id(), lb.id()).unwrap())
        .unwrap()
}

/// Groups nest: an inner rollback undoes the inner edits only; the outer
/// group keeps the rest when it ends, merging its runs as single edits do.
#[test]
fn nested_groups() {
    let src = "a: 1\nb: 2\nc: 3\n";
    let mut tree = SyntaxTree::parse(src);
    let outer = tree.begin_group();
    let one = scalar(&tree, "1").id();
    let f = Fragment::parse("10").unwrap();
    let ten = tree.replace(one, f).unwrap().inserted.first;
    let inner = tree.begin_group();
    let two = scalar(&tree, "2").id();
    let _ = tree.replace(two, Fragment::parse("20").unwrap()).unwrap();
    let three = scalar(&tree, "3").id();
    let _ = tree.replace(three, Fragment::parse("30").unwrap()).unwrap();
    assert_eq!(tree.emit(), "a: 10\nb: 20\nc: 30\n");
    tree.rollback_group(inner).unwrap();
    assert_eq!(tree.emit(), "a: 10\nb: 2\nc: 3\n");
    assert!(tree.in_group());
    tree.end_group(outer).unwrap();
    assert!(!tree.in_group());
    assert!(tree.get(ten).is_ok());
    assert_eq!(tree.emit(), "a: 10\nb: 2\nc: 3\n");
    invariants(&tree);
    reparses(&tree, &Settings::new());
}

/// A mark closed out of order is refused (`Internal`), nothing changes;
/// a group rolled back over the tree's first edit leaves it unedited.
#[test]
fn marks_are_checked_and_the_first_edit_rolls_back_to_unedited() {
    let mut tree = SyntaxTree::parse("a: 1\n");
    let outer = tree.begin_group();
    let inner = tree.begin_group();
    assert_eq!(tree.end_group(outer), Err(TreeEditError::Internal));
    assert_eq!(tree.rollback_group(outer), Err(TreeEditError::Internal));
    let one = scalar(&tree, "1").id();
    let _ = tree.replace(one, Fragment::parse("2").unwrap()).unwrap();
    assert!(tree.is_edited());
    tree.end_group(inner).unwrap();
    tree.rollback_group(outer).unwrap();
    assert!(!tree.is_edited());
    assert_eq!(tree.emit(), "a: 1\n");
    assert!(tree.get(one).is_ok());
}

/// An edit never removes or replaces an error region,
/// even one whose diagnostic is located in the next document; a whole
/// document still goes with its errors.
#[test]
fn error_nodes_are_never_removed_or_replaced() {
    let src = "a: [1, 2\n---\nt: x\n";
    let mut tree = SyntaxTree::parse(src);
    assert!(tree.has_errors());
    let errors = tree.error_count();
    assert!(errors > 0);
    let seq = find(&tree, K::FlowSequence).id();
    let refused = Err(TreeEditError::ReadOnly { kind: K::Error });
    assert_eq!(
        tree.replace(seq, Fragment::parse("l1").unwrap())
            .map(|_| ()),
        refused
    );
    let map = find(&tree, K::BlockMapping).id();
    assert_eq!(tree.remove(map).map(|_| ()), refused);
    let err = find(&tree, K::Error).id();
    assert_eq!(tree.remove(err).map(|_| ()), refused);
    assert_eq!(tree.emit(), src);
    assert_eq!(tree.error_count(), errors);
    // Next to it, edits work.
    let x = scalar(&tree, "x").id();
    let _ = tree.replace(x, Fragment::parse("y").unwrap()).unwrap();
    assert_eq!(tree.error_count(), errors);
    // The whole document goes with its error.
    let doc0 = nth(&tree, K::Document, 0).id();
    let gone = tree.remove(doc0).unwrap();
    assert!(gone.has_errors());
    assert_eq!(tree.error_count(), 0);
    invariants(&tree);
}

/// Found by the stream fuzz: a refused edit inside a group
/// undoes itself by re-inserting runs as new treap items; the group's
/// earlier undo records name runs by slot, so rolling back stays exact.
#[test]
fn a_refused_edit_inside_a_group_keeps_the_rollback_exact() {
    for nl in ["\n", "\r\n", "\r"] {
        let src = format!("---{nl}m: 2{nl}k: &x |{nl}  text{nl}# c{nl}...{nl}");
        let mut tree = SyntaxTree::parse(src.clone());
        // Earlier edits leave several runs (and free treap items).
        for (from, to) in [("2", "20"), ("20", "2")] {
            let n = scalar(&tree, from).id();
            let _ = tree.replace(n, Fragment::parse(to).unwrap()).unwrap();
        }
        let a = find(&tree, K::Anchor).id();
        let a = tree.replace_token(a, "&y").unwrap();
        let _ = tree.replace_token(a, "&x").unwrap();
        let src = tree.emit();
        let mark = tree.begin_group();
        let c = find(&tree, K::Comment).id();
        let _ = tree.remove(c).unwrap();
        // The anchor's run is replaced again; this edit's undo
        // record names the run before it, which starts at the line break
        // after `2` (the earlier edits cut the text there).
        let a = find(&tree, K::Anchor).id();
        let _ = tree.replace_token(a, "&y").unwrap();
        // Genuinely refused (`m: 2k: &y |`): the removal starts exactly at
        // that run, so the refusal removes it and re-inserts it as a new
        // treap item. An undo record naming runs by treap item (before
        // the fix) would then name a freed item.
        let map = find(&tree, K::BlockMapping);
        let kids: Vec<_> = map.children().collect();
        let at = kids.iter().position(|n| n.kind() == K::LineBreak).unwrap();
        let refused = tree.remove(kids[at].id()).map(|_| ());
        assert_eq!(refused, Err(TreeEditError::NotWellFormed));
        tree.rollback_group(mark).unwrap();
        assert_eq!(tree.emit().as_bytes(), src.as_bytes());
        assert_eq!(tree, SyntaxTree::parse(src.clone()));
        invariants(&tree);
    }
}

/// A rolled-back group never gives its slots out again,
/// so a handle of an element the group inserted stays stale for good,
/// and a fork taken inside the group resolves it to the same element or
/// not at all.
#[test]
fn a_rolled_back_group_never_reissues_its_slots() {
    for first_edit_in_group in [false, true] {
        let mut tree = SyntaxTree::parse("a: 1\nb: 2\n");
        if !first_edit_in_group {
            let one = scalar(&tree, "1").id();
            let _ = tree.replace(one, Fragment::parse("11").unwrap()).unwrap();
        }
        let m = tree.begin_group();
        let two = scalar(&tree, "2").id();
        let h = tree
            .replace(two, Fragment::parse("[x, y]").unwrap())
            .unwrap()
            .inserted
            .first;
        let fork = tree.fork();
        assert_eq!(fork.get(h).unwrap().kind(), K::FlowSequence);
        tree.rollback_group(m).unwrap();
        assert!(tree.get(h).is_err());
        assert_eq!(tree.is_edited(), !first_edit_in_group);
        // Later insertions take new slots.
        let v = scalar(&tree, if first_edit_in_group { "1" } else { "11" }).id();
        let z = tree.replace(v, Fragment::parse("zzz").unwrap()).unwrap();
        assert!(
            tree.get(h).is_err(),
            "a stale handle names {:?}",
            tree.get(h)
        );
        assert_ne!(z.inserted.first, h);
        for _ in 0..5 {
            let s = find(&tree, K::Scalar).id();
            let _ = tree.replace(s, Fragment::parse("q").unwrap()).unwrap();
            assert!(tree.get(h).is_err());
        }
        // The fork still names the sequence; the tree's own later forks
        // never resolve `h`.
        assert_eq!(fork.get(h).unwrap().kind(), K::FlowSequence);
        assert!(tree.fork().get(h).is_err());
        invariants(&tree);
        reparses(&tree, &Settings::new());
    }
}

/// A mark is tied to the tree that issued it; a mark of another
/// tree (of the same depth) is refused and nothing changes.
#[test]
fn marks_of_another_tree_are_refused() {
    let mut a = SyntaxTree::parse("a: 1\n");
    let mut b = SyntaxTree::parse("a: 1\n");
    let ma = a.begin_group();
    let mb = b.begin_group();
    let one = scalar(&a, "1").id();
    let _ = a.replace(one, Fragment::parse("2").unwrap()).unwrap();
    assert_eq!(b.rollback_group(ma), Err(TreeEditError::Internal));
    assert_eq!(b.end_group(ma), Err(TreeEditError::Internal));
    assert_eq!(a.rollback_group(mb), Err(TreeEditError::Internal));
    assert!(a.in_group() && b.in_group());
    assert_eq!(a.emit(), "a: 2\n");
    // A clone has its own stamp: the original's mark is refused there.
    let mut c = a.clone();
    assert!(!c.in_group());
    let mc = c.begin_group();
    assert_eq!(c.end_group(ma), Err(TreeEditError::Internal));
    c.end_group(mc).unwrap();
    a.rollback_group(ma).unwrap();
    b.end_group(mb).unwrap();
    assert_eq!(a.emit(), "a: 1\n");
    assert_eq!(c.emit(), "a: 2\n");
}
