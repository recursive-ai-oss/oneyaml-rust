//! Handle identity across forks: a copy keeps every pre-fork handle valid on
//! both copies in O(1), with no per-copy translation maps. Never a panic, and a
//! handle never names a wrong element.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod edit_support;

use std::hash::{BuildHasher, RandomState};

use edit_support::invariants;
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind as K, SyntaxTree};

fn plain(t: &SyntaxTree, text: &str) -> NodeId {
    t.root()
        .descendants()
        .find(|x| x.kind() == K::PlainScalar && x.text() == text)
        .unwrap_or_else(|| panic!("no {text:?} in {:?}", t.emit()))
        .id()
}

fn scalar(t: &SyntaxTree, text: &str) -> NodeId {
    t.root()
        .descendants()
        .find(|x| x.kind() == K::Scalar && x.text() == text)
        .unwrap_or_else(|| panic!("no {text:?} in {:?}", t.emit()))
        .id()
}

fn seq(t: &SyntaxTree) -> NodeId {
    t.root()
        .descendants()
        .find(|x| x.kind() == K::FlowSequence)
        .unwrap()
        .id()
}

/// `, x` as the last entries of a flow sequence (cut from a parse).
fn entry(x: &str) -> Fragment {
    let t = SyntaxTree::parse(format!("[a, {x}]"));
    let s = t
        .root()
        .descendants()
        .find(|n| n.kind() == K::FlowSequence)
        .unwrap();
    let kids: Vec<_> = s.children().collect();
    t.extract(kids[2].id(), kids[4].id()).unwrap()
}

/// Inserts `frag` before the closing bracket of flow sequence `s`.
fn append_entry(t: &mut SyntaxTree, s: NodeId, frag: Fragment) -> oneyaml_syntax::tree::Inserted {
    let close = t.get(s).unwrap().last_child().unwrap().id();
    t.insert_before(close, frag).unwrap()
}

#[test]
fn every_element_keeps_its_handle_in_a_fork() {
    let mut tree = SyntaxTree::parse("a: [1, 2]\nb: {c: d} # e\n");
    // An edit first, so the fork copies an overlay too.
    let one = plain(&tree, "1");
    let _ = tree.replace_token(one, "11").unwrap();
    let before: Vec<(NodeId, K, String)> = tree
        .root()
        .descendants()
        .map(|n| (n.id(), n.kind(), n.text().into_owned()))
        .collect();
    let fork = tree.fork();
    assert_eq!(fork, tree);
    let s = RandomState::new();
    assert_eq!(s.hash_one(&fork), s.hash_one(&tree));
    for (id, kind, text) in &before {
        for t in [&tree, &fork] {
            let n = t.get(*id).unwrap();
            assert_eq!(n.kind(), *kind);
            assert_eq!(n.text(), text.as_str());
            // The same value from either copy.
            assert_eq!(n.id(), *id);
        }
    }
    // Walking both gives the same handles in the same order.
    let a: Vec<NodeId> = tree.root().descendants().map(|n| n.id()).collect();
    let b: Vec<NodeId> = fork.root().descendants().map(|n| n.id()).collect();
    assert_eq!(a, b);
    invariants(&fork);
}

/// The staleness matrix: which handle resolves in which copy.
#[test]
fn staleness_matrix() {
    let src = "a: [1, 2, 3]\nb: x\n";
    let mut a = SyntaxTree::parse(src);
    let two = scalar(&a, "2");
    let three = scalar(&a, "3");
    let x = plain(&a, "x");
    let root = a.root().id();
    let mut b = a.fork();
    // Removed in A only, in B only.
    let s = seq(&a);
    let kids: Vec<NodeId> = a.get(s).unwrap().children().map(|n| n.id()).collect();
    // [ 1 , ␠ 2 , ␠ 3 ]: remove `2, ` (kids 4..=6) in A.
    assert_eq!(a.get(kids[4]).unwrap().text(), "2");
    let _ = a.remove_range(kids[4], kids[6]).unwrap();
    assert_eq!(a.emit(), "a: [1, 3]\nb: x\n");
    let bkids: Vec<NodeId> = b.get(s).unwrap().children().map(|n| n.id()).collect();
    assert_eq!(bkids, kids, "pre-fork handles are the same values");
    // `, 3` in B.
    let _ = b.remove_range(kids[5], kids[7]).unwrap();
    assert_eq!(b.emit(), "a: [1, 2]\nb: x\n");
    // New in A, new in B.
    let a_new = a.replace_token(x, "y").unwrap();
    let b_new = b.replace_token(x, "z").unwrap();
    assert_ne!(a_new, b_new);
    let a_ins = append_entry(&mut a, s, entry("q"));
    let b_ins = append_entry(&mut b, s, entry("r"));
    assert_eq!(a.emit(), "a: [1, 3, q]\nb: y\n");
    assert_eq!(b.emit(), "a: [1, 2, r]\nb: z\n");
    // Unrelated and cloned trees.
    let other = SyntaxTree::parse(src);
    let alien = scalar(&other, "2");
    let clone = a.clone();
    let cloned = clone
        .get(clone.root().first_child().unwrap().id())
        .unwrap()
        .id();

    let cases: [(&str, NodeId, bool, bool); 12] = [
        ("root", root, true, true),
        ("`1`, kept by both", scalar(&a, "1"), true, true),
        ("`2`, removed in A", two, false, true),
        ("`3`, removed in B", three, true, false),
        ("`x`, replaced in both", x, false, false),
        ("A's new token", a_new, true, false),
        ("B's new token", b_new, false, true),
        ("A's inserted entry", a_ins.last, true, false),
        ("B's inserted entry", b_ins.last, false, true),
        ("A's inserted comma", a_ins.first, true, false),
        ("an unrelated tree's", alien, false, false),
        ("a clone's", cloned, false, false),
    ];
    for (what, id, in_a, in_b) in cases {
        assert_eq!(a.get(id).is_ok(), in_a, "{what} in A");
        assert_eq!(b.get(id).is_ok(), in_b, "{what} in B");
    }
    // Slots are reused across copies (both allocate from the fork's
    // limit): the same slot is a different element in each, and each
    // copy's handle names only its own.
    assert_eq!(
        a.get(a_new).unwrap().text(),
        "y",
        "A's handle names A's element"
    );
    assert_eq!(b.get(b_new).unwrap().text(), "z");
    // A clone is independent: none of A's handles resolve in it.
    assert!(clone.get(root).is_err());
    assert!(clone.get(a_new).is_err());
    invariants(&a);
    invariants(&b);
}

/// 500 fork + edit cycles (the `Document` copy-on-write pattern): every
/// handle ever issued resolves where its element lives, and resolving
/// stays cheap (no per-copy maps to go through).
#[test]
fn five_hundred_fork_and_edit_cycles() {
    let mut cur = SyntaxTree::parse("k: [v0]\nz: 0\n");
    let s = seq(&cur);
    // (handle, the text it names, the cycle it was issued in)
    let mut issued: Vec<(NodeId, String, usize)> = Vec::new();
    let mut old = Vec::new();
    for i in 0..500 {
        let next = cur.fork();
        let mut next = next;
        let ins = append_entry(&mut next, s, entry(&format!("v{}", i + 1)));
        let text = next.get(ins.last).unwrap().text().into_owned();
        issued.push((ins.last, text, i));
        // Every earlier handle resolves in the new copy and in the old
        // one (each holds all entries issued before it forked).
        for (id, text, _) in &issued {
            assert_eq!(next.get(*id).unwrap().text(), text.as_str());
        }
        for (id, text, when) in &issued {
            assert_eq!(cur.get(*id).is_ok(), *when < i, "cycle {i}");
            if *when < i {
                assert_eq!(cur.get(*id).unwrap().text(), text.as_str());
            }
        }
        old.push(std::mem::replace(&mut cur, next));
        // Keep a few old copies: their handles stay theirs.
        if old.len() > 3 {
            old.remove(0);
        }
    }
    assert!(cur.emit().starts_with("k: [v0, v1, v2"));
    assert!(cur.emit().contains("v500]"));
    // Resolution does not slow down with the chain: 10⁶ lookups of the
    // oldest and newest handles.
    let mut hits = 0usize;
    for _ in 0..1_000_000 / 2 {
        hits += usize::from(cur.get(issued[0].0).is_ok());
        hits += usize::from(cur.get(issued[499].0).is_ok());
    }
    assert_eq!(hits, 1_000_000);
    invariants(&cur);
}

/// A chain of forks (a fork of a fork of …), edited at each level, and
/// a sibling fork at each level: a handle resolves in the copies that
/// descend from the copy that issued it after it was issued, and nowhere
/// else.
#[test]
fn deep_fork_chains_and_siblings() {
    let base = SyntaxTree::parse("k: [a]\n");
    let s = seq(&base);
    let mut chain = vec![base];
    let mut siblings = Vec::new();
    let mut issued = Vec::new();
    for i in 0..200 {
        let last = chain.last().unwrap();
        // A sibling forked before this level's edit.
        siblings.push(last.fork());
        let mut next = last.fork();
        if i % 3 != 2 {
            // Two of three levels edit; the others fork untouched.
            let ins = append_entry(&mut next, s, entry(&format!("e{i}")));
            issued.push((ins.last, chain.len()));
        }
        chain.push(next);
    }
    for (id, level) in &issued {
        for (k, t) in chain.iter().enumerate() {
            assert_eq!(
                t.get(*id).is_ok(),
                k >= *level,
                "level {level} in chain {k}"
            );
        }
        for (k, t) in siblings.iter().enumerate() {
            // Sibling k was forked from chain[k], before chain[k + 1]'s
            // edit.
            assert_eq!(
                t.get(*id).is_ok(),
                k >= *level,
                "level {level} in sibling {k}"
            );
        }
    }
    // Each sibling's own edits stay its own.
    let mut sib = siblings.pop().unwrap();
    let ins = append_entry(&mut sib, s, entry("own"));
    assert!(sib.get(ins.last).is_ok());
    assert!(chain.iter().all(|t| t.get(ins.last).is_err()));
    assert!(siblings.iter().all(|t| t.get(ins.last).is_err()));
}

/// Forking an unedited tree, and forking again without edits in between,
/// changes nothing about which handles resolve; `Clone` still gives an
/// independent tree.
#[test]
fn forks_of_unedited_trees_and_clones() {
    let t = SyntaxTree::parse("- a\n- b\n");
    let a = plain(&t, "a");
    let f1 = t.fork();
    let f2 = f1.fork();
    let f3 = f2.fork();
    for x in [&t, &f1, &f2, &f3] {
        assert_eq!(x.get(a).unwrap().text(), "a");
    }
    let c = f3.clone();
    assert!(c.get(a).is_err());
    let c2 = c.fork();
    let ca = plain(&c, "a");
    assert_eq!(c2.get(ca).unwrap().text(), "a");
    assert!(t.get(ca).is_err());
}
