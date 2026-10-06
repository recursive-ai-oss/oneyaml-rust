//! Reads of edited trees through run hints.
//!
//! * every hinted step (`Node` methods, `Children`, `Descendants`, `Walk`)
//!   agrees with the overlay's own links (`nav_*`, read through the link
//!   overrides and the run lookup), after edit scripts with accepted and
//!   refused edits, groups rolled back and ended, forks and clones;
//! * a full sequential read (tokens and texts, a walk, a children
//!   traversal, emit) takes O(R) lookups and treap edges for R runs,
//!   whatever the number of elements (deterministic counters, `cost.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use super::{Fragment, NONE, Node, NodeId, SyntaxKind, SyntaxTree, WalkEvent, cost};

/// A deterministic generator (the edit scripts must not depend on the
/// treap's per-tree priorities).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

/// The overlay's own answer, element by element: what the reads did without the
/// per-run read shortcuts (link overrides, run lookups), the reference.
fn check_against_overlay(t: &SyntaxTree) {
    let total = t.len();
    let mut seen = 0usize;
    let mut walk_ref = Vec::new();
    // The reference order: `nav_succ` from the root.
    let mut x = Some(0u32);
    while let Some(s) = x {
        walk_ref.push(s);
        x = t.nav_succ(s);
    }
    assert_eq!(walk_ref.len(), total);
    for (rank, n) in t.root().descendants().enumerate() {
        let s = n.slot();
        assert_eq!(s, walk_ref[rank], "descendants order at {rank}");
        let hint = n.hint();
        if t.is_edited() && rank > 0 {
            // Iterators carry the run's item (found by the first step when
            // the iteration starts from a node without one, as the root).
            assert_ne!(hint, NONE, "{n:?} has no hint");
            assert_eq!(t.run_item(s).map(|r| r.0), Some(hint), "{n:?}");
        }
        let slot = |m: Option<Node<'_>>| m.map(Node::slot);
        assert_eq!(slot(n.parent()), t.nav_parent(s), "parent of {n:?}");
        assert_eq!(slot(n.first_child()), t.nav_first(s), "first of {n:?}");
        assert_eq!(slot(n.last_child()), t.nav_last(s), "last of {n:?}");
        assert_eq!(slot(n.next_sibling()), t.nav_next(s), "next of {n:?}");
        assert_eq!(slot(n.prev_sibling()), t.nav_prev(s), "prev of {n:?}");
        assert_eq!(slot(n.succ()), t.nav_succ(s), "succ of {n:?}");
        assert_eq!(slot(n.pred()), t.nav_pred(s), "pred of {n:?}");
        assert_eq!(Some(n.kind()), t.kind_of(s), "kind of {n:?}");
        let (r, b) = t.nav_rank(s).unwrap();
        assert_eq!(n.preorder_index() as u64, r);
        let (a, e) = t.nav_bytes(s).unwrap();
        assert_eq!((n.span().start as u64, n.span().end as u64), (a, e));
        assert_eq!(a, b);
        let text = t.try_text_range(a, e).unwrap();
        assert_eq!(n.text(), text, "text of {n:?}");
        // The hints a step hands on are right too (or absent).
        for m in [
            n.parent(),
            n.first_child(),
            n.last_child(),
            n.next_sibling(),
            n.prev_sibling(),
            n.succ(),
            n.pred(),
        ]
        .into_iter()
        .flatten()
        {
            if m.hint() != NONE {
                assert_eq!(t.run_item(m.slot()).map(|r| r.0), Some(m.hint()));
            }
        }
        // Children, by the overlay's links.
        let mut kids = Vec::new();
        let mut c = t.nav_first(s);
        while let Some(k) = c {
            kids.push(k);
            c = t.nav_next(k);
        }
        let got: Vec<u32> = n.children().map(Node::slot).collect();
        assert_eq!(got, kids, "children of {n:?}");
        for k in n.children() {
            if t.is_edited() && k.hint() != NONE {
                assert_eq!(t.run_item(k.slot()).map(|r| r.0), Some(k.hint()));
            }
        }
        // A subtree in preorder, from an unhinted node too.
        let size = t.nav_size(s) as usize;
        let sub: Vec<u32> = n.descendants().map(Node::slot).collect();
        assert_eq!(sub, walk_ref[rank..rank + size], "subtree of {n:?}");
        let bare = Node::new(t, s);
        let sub2: Vec<u32> = bare.descendants().map(Node::slot).collect();
        assert_eq!(sub2, sub);
        let kids2: Vec<u32> = bare.children().map(Node::slot).collect();
        assert_eq!(kids2, kids);
        seen += 1;
    }
    assert_eq!(seen, total);
    // The walk: enter in preorder, leave once the next element is not a
    // descendant (by the overlay's parents).
    let mut want = Vec::new();
    let mut open: Vec<u32> = Vec::new();
    for &s in &walk_ref {
        let p = t.nav_parent(s);
        while let Some(&top) = open.last() {
            if Some(top) == p {
                break;
            }
            want.push((false, open.pop().unwrap()));
        }
        want.push((true, s));
        open.push(s);
    }
    while let Some(top) = open.pop() {
        want.push((false, top));
    }
    let got: Vec<(bool, u32)> = t
        .root()
        .walk()
        .map(|e| match e {
            WalkEvent::Enter(n) => (true, n.slot()),
            WalkEvent::Leave(n) => (false, n.slot()),
        })
        .collect();
    assert_eq!(got, want, "walk");
    // A walk of a subtree from an unhinted node.
    if let Some(d) = t.root().first_child() {
        let a: Vec<_> = d.walk().collect();
        let b: Vec<_> = Node::new(t, d.slot()).walk().collect();
        assert_eq!(a, b);
    }
    assert_eq!(t.emit(), t.try_text_range(0, t.nav_total().1).unwrap());
}

const INPUTS: &[&str] = &[
    "a: 1\nb: [x, y, {p: q}]\nc:\n  - d\n  - e: f\n    g: h # c\n# end\n",
    "- !t &a [1, 2]\n- *a\n- |\n  block\n  text\n- >-\n  folded\n- 'q' # c\n",
    "--- a\n...\n# between\n--- {k: v, w: [1, 2]}\n--- \n- x\n- y\n",
    "%TAG !e! tag:e,2000:\n--- !e!m\nk: !e!v v\nl:\n  m:\n    n: o\n",
    "? complex\n: value\n? [k]\n: {v: 1}\nplain: multi\n  line\n",
];

/// The pieces inserted by the scripts.
const PIECES: &[&str] = &["zz", "[q, r]", "{s: t}", "# c", "\n", "w"];

/// One random edit of `t` (any outcome); returns whether it was accepted.
fn random_edit(t: &mut SyntaxTree, rng: &mut Rng) -> bool {
    let all: Vec<(NodeId, SyntaxKind)> =
        t.root().descendants().map(|n| (n.id(), n.kind())).collect();
    let (id, kind) = all[rng.below(all.len())];
    let piece = PIECES[rng.below(PIECES.len())];
    match rng.below(5) {
        0 if kind.is_token() => {
            let text = match kind {
                SyntaxKind::Comment => "# r",
                SyntaxKind::LineBreak => "\n",
                SyntaxKind::Whitespace => "  ",
                _ => "v",
            };
            t.replace_token(id, text).is_ok()
        }
        1 => Fragment::parse(piece).is_ok_and(|f| t.replace(id, f).is_ok()),
        2 => Fragment::parse(piece).is_ok_and(|f| t.insert_after(id, f).is_ok()),
        3 => Fragment::parse(piece).is_ok_and(|f| t.insert_before(id, f).is_ok()),
        _ => t.remove(id).is_ok(),
    }
}

/// Every hinted read agrees with the overlay's links through random edit
/// scripts: single edits, groups ended and rolled back, forks and clones
/// (each copy edited on), on varied inputs and on the yaml-test-suite.
#[test]
fn hinted_reads_agree_with_the_overlay() {
    let mut inputs: Vec<String> = INPUTS.iter().map(|s| (*s).to_owned()).collect();
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    let mut suite: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path().join("in.yaml"))
        .filter(|p| p.is_file())
        .collect();
    suite.sort();
    inputs.extend(
        suite
            .iter()
            .step_by(7)
            .map(|p| std::fs::read_to_string(p).unwrap()),
    );
    let mut rng = Rng(0x48f);
    let (mut accepted, mut checked) = (0usize, 0usize);
    for src in &inputs {
        let mut t = SyntaxTree::parse(src.as_str());
        check_against_overlay(&t);
        for round in 0..12 {
            match round % 4 {
                0 | 1 => accepted += usize::from(random_edit(&mut t, &mut rng)),
                2 => {
                    let mark = t.begin_group();
                    for _ in 0..3 {
                        accepted += usize::from(random_edit(&mut t, &mut rng));
                    }
                    if rng.below(2) == 0 {
                        t.rollback_group(mark).unwrap();
                    } else {
                        t.end_group(mark).unwrap();
                    }
                }
                _ => {
                    // A copy made inside an open group settles its edits.
                    let mark = t.begin_group();
                    accepted += usize::from(random_edit(&mut t, &mut rng));
                    let mut copy = if rng.below(2) == 0 {
                        t.fork()
                    } else {
                        t.clone()
                    };
                    t.rollback_group(mark).unwrap();
                    check_against_overlay(&copy);
                    accepted += usize::from(random_edit(&mut copy, &mut rng));
                    check_against_overlay(&copy);
                    checked += 2;
                }
            }
            check_against_overlay(&t);
            checked += 1;
        }
    }
    assert!(accepted > 150, "{accepted} edits accepted");
    assert!(checked > 500, "{checked}");
}

/// The hint fits in what was padding (no growth of a node view).
#[test]
fn a_node_is_two_words() {
    assert_eq!(core::mem::size_of::<Node<'_>>(), 16);
    assert_eq!(core::mem::size_of::<Option<Node<'_>>>(), 16);
    // And a walk is as large as before its hints (it is moved around with
    // the readers that hold one).
    assert_eq!(core::mem::size_of::<super::Walk<'_>>(), 56);
}

/// `entries` lines of block entries with flow values, and `k` edits
/// spread over them (token replacements and node replacements).
fn scattered(entries: usize, k: usize) -> SyntaxTree {
    let src: String = (0..entries)
        .map(|i| format!("k{i}: [a{i}, b, {{x: y}}] # c\n"))
        .collect();
    let mut t = SyntaxTree::parse(src);
    if k == 0 {
        return t;
    }
    let targets: Vec<NodeId> = t
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::PlainScalar && n.text().starts_with('a'))
        .map(|n| n.id())
        .collect();
    let step = (targets.len() / k).max(1);
    for (j, id) in targets.iter().step_by(step).take(k).enumerate() {
        if j % 2 == 0 {
            t.replace_token(*id, "zz").unwrap();
        } else {
            let p = t.get(*id).unwrap().parent().unwrap().id();
            let _ = t.replace(p, Fragment::parse("[q]").unwrap()).unwrap();
        }
    }
    t
}

/// The steps a full sequential read takes, `(lookups, edges)` per read:
/// every token's kind and text by `descendants`, a walk, a children
/// traversal (each node's `children()`), and emit.
fn read_costs(t: &SyntaxTree) -> [(u64, u64); 4] {
    let mut out = [(0, 0); 4];
    cost::take();
    let mut s = 0usize;
    for n in t.root().descendants() {
        s += n.kind() as usize;
        if n.is_token() {
            s += n.text().len();
        }
    }
    out[0] = cost::take();
    s += t.root().walk().count();
    out[1] = cost::take();
    let mut stack = vec![t.root()];
    while let Some(n) = stack.pop() {
        for c in n.children() {
            s += 1;
            stack.push(c);
        }
    }
    out[2] = cost::take();
    s += t.emit().len();
    out[3] = cost::take();
    assert!(s > 0);
    out
}

/// The runs of an edited tree (all non-empty: R is at most the number of
/// elements).
fn runs(t: &SyntaxTree) -> u64 {
    let (lengths, named) = t.run_lengths();
    assert!(lengths.iter().all(|n| *n > 0), "an empty run");
    assert_eq!(lengths.len(), named);
    lengths.len() as u64
}

/// Deterministically: a full sequential read of an edited tree takes O(1)
/// lookups and treap edges **per run**, never per element: the same counts at a
/// fixed number of edits whatever the document's size, and emit takes no lookup
/// at all. Without the shortcuts every element of a read cost a run lookup and
/// often a link-override lookup (a 100-entry document with 20 edits took ~5,000
/// lookups to read its tokens, a 1,600-entry one ~80,000).
#[test]
fn sequential_reads_cost_per_run_not_per_element() {
    let names = ["tokens", "walk", "children", "emit"];
    let mut by_size = Vec::new();
    for entries in [100usize, 400, 1_600] {
        let t = scattered(entries, 20);
        let r = runs(&t);
        let c = read_costs(&t);
        eprintln!(
            "{entries} entries, 20 edits, {r} runs, {} elements: {c:?}",
            t.len()
        );
        for (i, (l, e)) in c.iter().enumerate() {
            let what = names[i];
            // O(R): a few lookups and edges per run, a constant per read.
            assert!(*l <= 6 * r + 8, "{what}: {l} lookups for {r} runs");
            assert!(*e <= 6 * r + 64, "{what}: {e} edges for {r} runs");
        }
        // Emit reads the runs in order: no map, no root query.
        assert_eq!(c[3].0, 0, "emit lookups");
        by_size.push(c);
    }
    // Independent of the document: 16 times the elements, the same steps
    // up to the treap's shape (its priorities are per tree).
    for i in 0..4 {
        let (small, large) = (by_size[0][i], by_size[2][i]);
        assert!(
            large.0 <= small.0 + small.0 / 4 + 8 && large.1 <= small.1 + small.1 / 2 + 64,
            "{}: {small:?} at 100 entries, {large:?} at 1,600",
            names[i]
        );
    }
    // At a fixed size, many edits cost per run: per element far below one
    // lookup (without the hints: two or more per element).
    let t = scattered(1_600, 400);
    let r = runs(&t);
    let c = read_costs(&t);
    let elements = t.len() as u64;
    eprintln!("1600 entries, 400 edits, {r} runs, {elements} elements: {c:?}");
    for (i, (l, e)) in c.iter().enumerate() {
        assert!(*l <= 6 * r + 8, "{}: {l} lookups for {r} runs", names[i]);
        assert!(*e <= 6 * r + 64, "{}: {e} edges for {r} runs", names[i]);
        assert!(
            *l * 4 < elements,
            "{}: {l} lookups, {elements} elements",
            names[i]
        );
    }
    // An unedited tree takes none.
    let c = read_costs(&scattered(400, 0));
    assert_eq!(c, [(0, 0); 4]);
}

/// `entries` lines `k{i}: v # c{i}` with every other comment removed (base
/// removals: many runs, no arena), and the whitespace tokens the removals
/// left at those lines' ends.
fn after_base_removals(entries: usize) -> (SyntaxTree, Vec<NodeId>) {
    let src: String = (0..entries).map(|i| format!("k{i}: v # c{i}\n")).collect();
    let mut t = SyntaxTree::parse(src);
    let comments: Vec<NodeId> = t
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .map(|n| n.id())
        .step_by(2)
        .collect();
    let mut ends = Vec::new();
    for c in comments {
        let space = t.get(c).unwrap().prev_sibling().unwrap().id();
        let _ = t.remove(c).unwrap();
        ends.push(space);
    }
    (t, ends)
}

/// Edit cost is independent of edit
/// history): a compaction of the arena slab visits every treap item, and is
/// paid for by the frees that trigger it. After base removals (runs, no
/// arenas), cycles of 20 inserted comments and their removals used to
/// compact once per cycle over all the runs (visits per edit growing with
/// the runs the history left); now visits per edit stay bounded, the same
/// for 500 and 2,000 runs.
#[test]
fn compactions_are_paid_for_by_the_frees() {
    let mut per_edit = Vec::new();
    for entries in [1_000usize, 4_000] {
        let (mut t, ends) = after_base_removals(entries);
        let r = runs(&t);
        assert!(r as usize >= entries / 2, "{r} runs");
        let comment = Fragment::parse("# x").unwrap();
        cost::take_visits();
        let mut edits = 0u64;
        for cycle in 0..40 {
            let mut inserted = Vec::new();
            for j in 0..20 {
                let at = ends[(cycle * 20 + j) % ends.len()];
                inserted.push(t.insert_after(at, comment.clone()).unwrap().first);
                edits += 1;
            }
            for id in inserted {
                let _ = t.remove(id).unwrap();
                edits += 1;
            }
        }
        let visits = cost::take_visits();
        eprintln!("{entries} entries, {r} runs: {visits} compaction visits for {edits} edits");
        assert!(
            visits <= 8 * edits,
            "{visits} visits for {edits} edits ({r} runs)"
        );
        per_edit.push(visits as f64 / edits as f64);
        check_against_overlay(&t);
        assert_eq!(t, SyntaxTree::parse(t.emit()));
    }
    // Independent of the runs the history left.
    assert!(per_edit[1] <= per_edit[0] * 1.5 + 0.5, "{per_edit:?}");
}

/// Compactions renumber every run, those of an inserted arena
/// split into several runs included, and every read agrees with the
/// overlay after each one.
#[test]
fn compactions_keep_split_arenas_readable() {
    let src: String = (0..130).map(|i| format!("k{i}: v{i}\n")).collect();
    let mut t = SyntaxTree::parse(src);
    // An arena split into several runs by edits inside it.
    let v3 = t
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Scalar && n.text() == "v3")
        .unwrap()
        .id();
    let _ = t
        .replace(v3, Fragment::parse("[a, b, c, d, e]").unwrap())
        .unwrap();
    for x in ["b", "d"] {
        let tok = t
            .root()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::PlainScalar && n.text() == x)
            .unwrap()
            .id();
        let _ = t.replace_token(tok, &format!("{x}{x}")).unwrap();
    }
    let before = runs(&t);
    let comment = Fragment::parse(" # x").unwrap();
    let anchors: Vec<NodeId> = t
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Scalar && n.text().starts_with('v'))
        .map(|n| n.id())
        .collect();
    let mut compactions = 0;
    for round in 0..4 {
        let mut inserted = Vec::new();
        for j in 0..30 {
            // (Each on a line of its own: kept comments stay.)
            let at = anchors[round * 30 + j];
            inserted.push(t.insert_after(at, comment.clone()).unwrap());
        }
        // Interleaved with the frees: the slab compacts with live arenas
        // at high indices, which move.
        cost::take_visits();
        for (j, ins) in inserted.into_iter().enumerate() {
            if j % 6 != 5 {
                let _ = t.remove_range(ins.first, ins.last).unwrap();
            }
        }
        if cost::take_visits() > 0 {
            compactions += 1;
        }
        let (live, free) = t.arena_counts();
        assert!(
            free <= live.max(16).max(runs(&t) as usize),
            "{live} live, {free} free"
        );
        check_against_overlay(&t);
        assert_eq!(t, SyntaxTree::parse(t.emit()), "round {round}");
    }
    assert!(compactions >= 2, "{compactions} compactions");
    assert!(before >= 5, "{before} runs");
    assert!(t.emit().contains("[a, bb, c, dd, e]"));
}

/// An insertion refused because the arena slab is full
/// leaves the tree, its text and its handles as they were, and the tree
/// edits on once there is room.
#[test]
fn a_full_arena_slab_refuses_with_nothing_changed() {
    let src: String = (0..20).map(|i| format!("k{i}: [a{i}, b]\n")).collect();
    let mut t = SyntaxTree::parse(src);
    let first = t
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PlainScalar && n.text() == "a0")
        .unwrap()
        .id();
    let _ = t.replace_token(first, "z").unwrap();
    t.limit_arenas(0);
    let text = t.emit();
    let all: Vec<(NodeId, oneyaml_core::Span)> =
        t.root().descendants().map(|n| (n.id(), n.span())).collect();
    let shape = t.run_lengths();
    let token = t
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::PlainScalar && n.text() == "a5")
        .unwrap();
    let (target, node) = (token.id(), token.parent().unwrap().id());
    for f in ["[q]", "w"] {
        let r = t.replace(node, Fragment::parse(f).unwrap());
        assert_eq!(r.err(), Some(super::TreeEditError::Capacity), "{f}");
        assert_eq!(t.emit(), text);
        assert_eq!(t.run_lengths(), shape);
        for (id, span) in &all {
            assert_eq!(t.get(*id).unwrap().span(), *span);
        }
        check_against_overlay(&t);
    }
    assert_eq!(
        t.replace_token(target, "y"),
        Err(super::TreeEditError::Capacity)
    );
    assert_eq!(t.emit(), text);
    // Room again: the edit goes through.
    t.limit_arenas(1);
    let _ = t.replace_token(target, "y").unwrap();
    assert!(t.emit().contains("[y, b]"));
    assert_eq!(t, SyntaxTree::parse(t.emit()));
}
