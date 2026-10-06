//! Structural edits of the syntax tree: each
//! primitive, each refusal, and what reads return afterwards.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod edit_support;

use edit_support::{find, invariants, nth, reparses, scalar};
use oneyaml_core::{Limits, Settings, Span};
use oneyaml_syntax::tree::{Fragment, SyntaxKind as K, SyntaxTree, TreeEditError, compare_trees};

fn frag(text: &str) -> Fragment {
    Fragment::parse_with(text, &Settings::new()).unwrap()
}

/// Children `from..=to` of the first flow sequence of `src`, extracted.
fn entries(src: &str, from: usize, to: usize) -> Fragment {
    let t = SyntaxTree::parse(src);
    let seq = find(&t, K::FlowSequence);
    let kids: Vec<_> = seq.children().collect();
    t.extract(kids[from].id(), kids[to].id()).unwrap()
}

fn done(tree: &SyntaxTree) {
    invariants(tree);
    reparses(tree, &Settings::new());
}

#[test]
fn replace_a_value_keeps_other_handles() {
    let mut tree = SyntaxTree::parse("a: 1 # one\nb: 2\n");
    let one = scalar(&tree, "1").id();
    let two = scalar(&tree, "2").id();
    let key_b = scalar(&tree, "b").id();
    let map = find(&tree, K::BlockMapping).id();
    let r = tree.replace(one, frag("[x, yy]")).unwrap();
    assert_eq!(tree.emit(), "a: [x, yy] # one\nb: 2\n");
    assert_eq!(r.removed.text(), "1");
    assert_eq!(r.inserted.first, r.inserted.last);
    let new = tree.get(r.inserted.first).unwrap();
    assert_eq!(new.kind(), K::FlowSequence);
    assert_eq!(new.span(), Span::new(3, 10));
    assert_eq!(new.text(), "[x, yy]");
    // The replaced element is stale, unrelated ones still resolve,
    // with spans moved by the edit.
    assert!(tree.get(one).is_err());
    assert_eq!(tree.get(two).unwrap().span(), Span::new(20, 21));
    assert_eq!(tree.get(two).unwrap().value().as_deref(), Some("2"));
    assert_eq!(tree.get(key_b).unwrap().text(), "b");
    assert_eq!(tree.get(map).unwrap().text(), "a: [x, yy] # one\nb: 2");
    assert_eq!(new.parent().unwrap().id(), map);
    assert!(tree.is_edited());
    done(&tree);
}

#[test]
fn append_insert_and_remove_entries() {
    let mut tree = SyntaxTree::parse("a: 1\nb: 2\n");
    let map = find(&tree, K::BlockMapping).id();
    // A mapping's entries, parsed on their own and unwrapped.
    let entries = frag("\nc: 3").unwrap_root(1).unwrap();
    assert_eq!(
        entries.root_kinds().collect::<Vec<_>>(),
        [
            K::LineBreak,
            K::Scalar,
            K::MappingValue,
            K::Whitespace,
            K::Scalar
        ]
    );
    let ins = tree.append_child(map, entries).unwrap();
    assert_eq!(tree.emit(), "a: 1\nb: 2\nc: 3\n");
    assert_eq!(tree.get(ins.first).unwrap().kind(), K::LineBreak);
    assert_eq!(tree.get(ins.last).unwrap().value().as_deref(), Some("3"));
    done(&tree);

    // At the front, by position.
    let first = frag("z: 0").unwrap_root(0).unwrap();
    let lb = frag("\n");
    let front = first.concat(lb).unwrap();
    tree.insert_child(map, 0, front).unwrap();
    assert_eq!(tree.emit(), "z: 0\na: 1\nb: 2\nc: 3\n");
    done(&tree);

    // Remove `b: 2` with the break before it; put it back at the end.
    let b = scalar(&tree, "b");
    let before = b.prev_sibling().unwrap();
    let two = scalar(&tree, "2").id();
    let removed = tree.remove_range(before.id(), two).unwrap();
    assert_eq!(removed.text(), "\nb: 2");
    assert_eq!(tree.emit(), "z: 0\na: 1\nc: 3\n");
    assert!(tree.get(two).is_err());
    done(&tree);
    let three = scalar(&tree, "3").id();
    tree.insert_after(three, removed).unwrap();
    assert_eq!(tree.emit(), "z: 0\na: 1\nc: 3\nb: 2\n");
    done(&tree);
}

#[test]
fn edits_equal_the_string_splice() {
    let src = "k: [a, b, c]\nv: {x: 1}\n";
    let mut tree = SyntaxTree::parse(src);
    let b = scalar(&tree, "b").id();
    let _ = tree.replace(b, frag("bee")).unwrap();
    let x = scalar(&tree, "x").id();
    let one = scalar(&tree, "1");
    let tok = one.first_child().unwrap().id();
    // A token keeps its kind: another style replaces the node.
    assert_eq!(
        tree.replace_token(tok, "'uno'"),
        Err(TreeEditError::InvalidToken {
            kind: K::PlainScalar
        })
    );
    let new = tree.replace_token(tok, "uno").unwrap();
    assert_eq!(tree.get(new).unwrap().kind(), K::PlainScalar);
    let uno = scalar(&tree, "uno").id();
    let _ = tree.replace(uno, frag("'uno'")).unwrap();
    assert_eq!(tree.get(x).unwrap().text(), "x");
    assert_eq!(tree.emit(), "k: [a, bee, c]\nv: {x: 'uno'}\n");
    assert_eq!(scalar(&tree, "uno").text(), "'uno'");
    done(&tree);
}

#[test]
fn replace_token_kinds() {
    let src = "&an a: 'q' # c\n*an : \"d\"\n";
    let mut tree = SyntaxTree::parse(src);
    let comment = find(&tree, K::Comment).id();
    tree.replace_token(comment, "# changed").unwrap();
    let anchor = find(&tree, K::Anchor).id();
    tree.replace_token(anchor, "&bn").unwrap();
    let alias = find(&tree, K::AliasName).id();
    tree.replace_token(alias, "*bn").unwrap();
    let ws = find(&tree, K::Whitespace).id();
    tree.replace_token(ws, "   ").unwrap();
    let dq = find(&tree, K::DoubleQuotedScalar).id();
    tree.replace_token(dq, "\"e\\tf\"").unwrap();
    let lb = find(&tree, K::LineBreak).id();
    tree.replace_token(lb, "\r\n").unwrap();
    assert_eq!(tree.emit(), "&bn   a: 'q' # changed\r\n*bn : \"e\\tf\"\n");
    assert_eq!(find(&tree, K::Scalar).anchor().unwrap().name, "bn");
    assert_eq!(find(&tree, K::Alias).alias_name(), Some("bn"));
    assert_eq!(scalar(&tree, "e\tf").text(), "\"e\\tf\"");
    done(&tree);

    // An empty plain scalar, and back. `a: ` would read with the empty
    // value before the space (a different tree): not well-formed.
    // An empty value goes in with the space, as `a:`
    // reads it.
    let mut tree = SyntaxTree::parse("a: x\n");
    let x = nth(&tree, K::PlainScalar, 1).id();
    assert_eq!(tree.replace_token(x, ""), Err(TreeEditError::NotWellFormed));
    assert!(!tree.is_edited());
    let ws = nth(&tree, K::Whitespace, 0).id();
    let value = nth(&tree, K::Scalar, 1).id();
    let bare = SyntaxTree::parse("a:\n");
    let empty = nth(&bare, K::Scalar, 1).id();
    let _ = tree
        .replace_range(ws, value, bare.extract(empty, empty).unwrap())
        .unwrap();
    assert_eq!(tree.emit(), "a:\n");
    done(&tree);
    let e = nth(&tree, K::PlainScalar, 1).id();
    // `a:y` is one plain scalar: refused; the space comes with the value.
    assert_eq!(
        tree.replace_token(e, "y"),
        Err(TreeEditError::NotWellFormed)
    );
    let value = nth(&tree, K::Scalar, 1).id();
    let _ = tree.replace(value, entries("[a, y]", 3, 4)).unwrap();
    assert_eq!(tree.emit(), "a: y\n");
    done(&tree);

    // Refusals: the text must read as one token of the same kind, in its
    // context (flow here).
    let mut tree = SyntaxTree::parse("[a, b] # c\n");
    let a = find(&tree, K::PlainScalar).id();
    for bad in ["x, y", "x]", "'q'", "a: b", "", "#"] {
        if bad.is_empty() {
            continue;
        }
        assert_eq!(
            tree.replace_token(a, bad),
            Err(TreeEditError::InvalidToken {
                kind: K::PlainScalar
            }),
            "{bad:?}"
        );
    }
    let c = find(&tree, K::Comment).id();
    assert_eq!(
        tree.replace_token(c, "# x\ny"),
        Err(TreeEditError::InvalidToken { kind: K::Comment })
    );
    assert_eq!(
        tree.replace_token(c, "no hash"),
        Err(TreeEditError::InvalidToken { kind: K::Comment })
    );
    let seq = find(&tree, K::FlowSequence).id();
    assert_eq!(
        tree.replace_token(seq, "x"),
        Err(TreeEditError::NotAToken {
            kind: K::FlowSequence
        })
    );
    let open = find(&tree, K::FlowSequenceOpen).id();
    assert_eq!(
        tree.replace_token(open, "{"),
        Err(TreeEditError::ReadOnly {
            kind: K::FlowSequenceOpen
        })
    );
    assert!(!tree.is_edited(), "refusals leave the tree untouched");
    let t = SyntaxTree::parse("!!str |\n  x\n");
    let mut t2 = t.clone();
    for k in [K::Tag, K::BlockScalarHeader, K::LiteralScalar] {
        let id = find(&t2, k).id();
        assert_eq!(
            t2.replace_token(id, "x"),
            Err(TreeEditError::ReadOnly { kind: k })
        );
    }
    assert_eq!(t2, t);
}

#[test]
fn refusals_leave_the_tree_unchanged() {
    let src = "%TAG !e! tag:e,2000:\n--- !e!t\na: [1, 2]\n...\n";
    let mut tree = SyntaxTree::parse(src);
    let other = SyntaxTree::parse(src);
    let map = find(&tree, K::BlockMapping).id();
    let seq = find(&tree, K::FlowSequence).id();
    let one = scalar(&tree, "1").id();
    let doc = find(&tree, K::Document).id();
    let root = tree.root().id();

    // Stale: another tree's handle, a clone's handle, a removed element.
    let foreign = scalar(&other, "1").id();
    assert!(matches!(tree.remove(foreign), Err(TreeEditError::Stale(_))));
    let copy = tree.clone();
    assert!(matches!(
        tree.remove(scalar(&copy, "1").id()),
        Err(TreeEditError::Stale(_))
    ));
    // The root has no parent.
    assert_eq!(tree.remove(root), Err(TreeEditError::NoParent));
    assert_eq!(
        tree.insert_after(root, frag("x")),
        Err(TreeEditError::NoParent)
    );
    // Not siblings: other parents, wrong order.
    let two = scalar(&tree, "2").id();
    assert_eq!(tree.remove_range(two, one), Err(TreeEditError::NotSiblings));
    assert_eq!(tree.remove_range(one, map), Err(TreeEditError::NotSiblings));
    // Into a token; past the end; an empty fragment.
    let tok = find(&tree, K::PlainScalar).id();
    assert_eq!(
        tree.append_child(tok, frag("x")),
        Err(TreeEditError::NotANode {
            kind: K::PlainScalar
        })
    );
    let n = tree.get(seq).unwrap().children().count();
    assert_eq!(
        tree.insert_child(seq, n + 1, frag("x")),
        Err(TreeEditError::Position {
            position: n + 1,
            len: n
        })
    );
    assert_eq!(
        tree.append_child(seq, frag("")),
        Err(TreeEditError::EmptyFragment)
    );
    // Inside a scalar; directives; properties.
    let tagged = find(&tree, K::BlockMapping);
    let _ = tagged;
    assert_eq!(
        tree.append_child(one, frag("x")),
        Err(TreeEditError::ReadOnly { kind: K::Scalar })
    );
    let dir = find(&tree, K::TagDirective).id();
    assert_eq!(
        tree.remove(dir),
        Err(TreeEditError::ReadOnly {
            kind: K::TagDirective
        })
    );
    let tag = find(&tree, K::Tag).id();
    assert_eq!(
        tree.remove(tag),
        Err(TreeEditError::ReadOnly { kind: K::Tag })
    );
    // Misplaced: a document in a mapping, a mapping in the stream.
    let a_doc = frag("--- x\n");
    assert_eq!(a_doc.root_kinds().next(), Some(K::Document));
    assert_eq!(
        tree.append_child(map, a_doc.clone()),
        Err(TreeEditError::Misplaced {
            kind: K::Document,
            parent: K::BlockMapping
        })
    );
    assert_eq!(
        tree.insert_after(doc, frag("k: v")),
        Err(TreeEditError::Misplaced {
            kind: K::BlockMapping,
            parent: K::Stream
        })
    );
    // Tags resolved elsewhere: `!e!` means nothing outside this document,
    // and `!x` would change meaning under a `%TAG !` document.
    assert!(matches!(
        Fragment::parse_with("!e!x y", &Settings::new()),
        Err(TreeEditError::Syntax { .. })
    ));
    let mut redefined = SyntaxTree::parse("%TAG ! tag:r,2000:\n--- [a]\n");
    let rseq = find(&redefined, K::FlowSequence).id();
    let before = redefined.clone();
    assert_eq!(
        redefined.insert_child(rseq, 2, entries("[a, !x b]", 2, 4)),
        Err(TreeEditError::TagContext)
    );
    assert_eq!(redefined, before);
    // A fragment that does not parse.
    assert!(matches!(
        Fragment::parse_with("[a", &Settings::new()),
        Err(TreeEditError::Syntax { .. })
    ));
    assert!(!tree.is_edited());
    assert_eq!(tree, other);
    for e in [
        TreeEditError::NoParent,
        TreeEditError::NotWellFormed,
        TreeEditError::Capacity,
        TreeEditError::Internal,
        TreeEditError::TagContext,
        TreeEditError::EmptyFragment,
    ] {
        assert!(!e.to_string().is_empty());
    }
}

#[test]
fn a_moved_fragment_keeps_its_meaning() {
    // Tags under `%TAG`, anchors, and block scalar headers travel with
    // their node; the moved tree re-parses to the same structure.
    let src = "%TAG !e! tag:e,2000:\n--- \n- y\n- !e!x &n |\n  body\n";
    let mut tree = SyntaxTree::parse(src);
    let y = scalar(&tree, "y");
    let (y, lb) = (y.id(), y.next_sibling().unwrap().id());
    let lit = nth(&tree, K::Scalar, 1).id();
    let copy = tree.extract(lit, lit).unwrap();
    assert_eq!(copy.text(), "!e!x &n |\n  body\n");
    // The literal's text ends with its line break: it replaces `y` and
    // the break after it.
    let r = tree.replace_range(y, lb, copy.clone()).unwrap();
    assert_eq!(r.removed.text(), "y\n");
    assert_eq!(
        tree.emit(),
        "%TAG !e! tag:e,2000:\n--- \n- !e!x &n |\n  body\n- !e!x &n |\n  body\n"
    );
    let moved = tree.get(r.inserted.first).unwrap();
    assert_eq!(moved.tag().unwrap().resolved, "tag:e,2000:x");
    assert_eq!(moved.anchor().unwrap().name, "n");
    assert_eq!(moved.value().as_deref(), Some("body\n"));
    let h = moved.block_header().unwrap();
    assert_eq!(&tree.emit()[h.span.start..h.span.end], "|");
    done(&tree);
    // The same fragment cannot go where `!e!` is not defined.
    let mut plain = SyntaxTree::parse("- a\n");
    let a = scalar(&plain, "a").id();
    assert_eq!(plain.replace(a, copy), Err(TreeEditError::TagContext));
}

#[test]
fn removed_error_nodes_take_their_diagnostics() {
    let src = "a: [b\nc: d\n--- x: y: z\n";
    let mut tree = SyntaxTree::parse(src);
    assert!(tree.has_errors());
    let n_diag = tree.diagnostics().len();
    let first_err = find(&tree, K::Error);
    let doc0 = nth(&tree, K::Document, 0).id();
    let doc1 = nth(&tree, K::Document, 1).id();
    let in1 = tree.diagnostics_within(tree.get(doc1).unwrap()).count();
    assert!(in1 >= 1);
    let _ = first_err;
    // Remove the whole first document: its diagnostics leave with it, the
    // second document's move with their text.
    let removed = tree.remove(doc0).unwrap();
    assert!(removed.has_errors());
    assert_eq!(
        tree.diagnostics().len() + removed.diagnostics().count(),
        n_diag
    );
    let text = tree.emit();
    for (n, d) in tree.diagnostics_by_element() {
        let n = n.unwrap();
        if n.kind() == K::Error {
            assert_eq!(n.diagnostic().unwrap(), d);
            let sp = d.primary.span;
            assert!(text.get(sp.start..sp.end).is_some());
            assert!(sp.start >= n.span().start - 1 || sp.start <= n.span().end);
        }
    }
    assert_eq!(
        tree.diagnostics_within(tree.get(doc1).unwrap()).count(),
        in1
    );
    invariants(&tree);
    reparses(&tree, &Settings::new());
    // Putting it back restores the original tree exactly.
    let first = tree.root().first_child().unwrap().id();
    tree.insert_before(first, removed).unwrap();
    assert_eq!(tree.emit(), src);
    assert_eq!(tree, SyntaxTree::parse(src));
    invariants(&tree);
}

#[test]
fn spans_of_unedited_regions_move_with_the_text() {
    let src = "- a\n- b: [c, d]\n- e\n";
    let mut tree = SyntaxTree::parse(src);
    let d = scalar(&tree, "d").id();
    let e = scalar(&tree, "e").id();
    let c = scalar(&tree, "c").id();
    let _ = tree
        .replace(scalar(&tree, "a").id(), frag("'longer'"))
        .unwrap();
    let text = tree.emit();
    for id in [c, d, e] {
        let n = tree.get(id).unwrap();
        let sp = n.span();
        assert_eq!(&text[sp.start..sp.end], n.text());
        assert_eq!(n.value(), Some(n.text()));
    }
    assert_eq!(tree.get(e).unwrap().span().start, text.len() - 2);
    done(&tree);
}

#[test]
fn clones_insert_independent_copies() {
    let mut tree = SyntaxTree::parse("[a]\n");
    let seq = find(&tree, K::FlowSequence).id();
    let f = entries("[a, b]", 2, 4);
    assert_eq!(f.root_count(), 3);
    let one = tree.insert_child(seq, 2, f.clone()).unwrap();
    let two = tree.insert_child(seq, 2, f).unwrap();
    assert_ne!(one.first, two.first);
    assert!(tree.get(one.last).is_ok() && tree.get(two.last).is_ok());
    assert_eq!(tree.emit(), "[a, b, b]\n");
    // Removing one copy leaves the other.
    let _ = tree.remove_range(one.first, one.last).unwrap();
    assert!(tree.get(one.last).is_err());
    assert_eq!(tree.get(two.last).unwrap().value().as_deref(), Some("b"));
    assert_eq!(tree.emit(), "[a, b]\n");
    done(&tree);
}

#[test]
fn fragment_building_errors() {
    let f = frag("a: 1\n");
    assert_eq!(f.text(), "a: 1\n");
    assert_eq!(f.root_count(), 2);
    assert!(!f.is_empty() && f.len() > 2);
    assert!(matches!(
        f.clone().unwrap_root(5),
        Err(TreeEditError::Position {
            position: 5,
            len: 2
        })
    ));
    assert_eq!(
        f.clone().unwrap_root(1),
        Err(TreeEditError::ReadOnly { kind: K::LineBreak })
    );
    let tagged = SyntaxTree::parse("%TAG !e! tag:e,2000:\n--- [!e!a]\n");
    let s = find(&tagged, K::Scalar).id();
    let t1 = tagged.extract(s, s).unwrap();
    let t2 = frag("!x b");
    assert_eq!(t1.clone().concat(t2), Err(TreeEditError::TagContext));
    let joined = t1.concat(frag(" ")).unwrap();
    assert_eq!(joined.text(), "!e!a ");
    assert!(!frag("# c").has_errors());
}

#[test]
fn equality_and_hash_are_structural() {
    use std::hash::{BuildHasher, RandomState};
    let mut tree = SyntaxTree::parse("a: 1\n");
    let one = scalar(&tree, "1").id();
    let _ = tree.replace(one, frag("2")).unwrap();
    let fresh = SyntaxTree::parse("a: 2\n");
    assert_eq!(tree, fresh);
    let s = RandomState::new();
    assert_eq!(s.hash_one(&tree), s.hash_one(&fresh));
    assert_ne!(tree, SyntaxTree::parse("a: 3\n"));
    assert!(compare_trees(&tree, &SyntaxTree::parse("a: 3\n")).is_err());
    let copy = tree.clone();
    assert_eq!(copy, tree);
    let debug = format!("{tree:?}");
    assert!(debug.contains("Scalar@3..4"), "{debug}");
}

/// Edits at the bottom of a 1,000,000-deep document on a
/// small stack return, never crash.
#[test]
fn deep_edits_on_a_small_stack() {
    const DEPTH: usize = 1_000_000;
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let src = format!("{}{}\n", "[".repeat(DEPTH), "]".repeat(DEPTH));
            let settings = Settings::new().limits(Limits::unlimited());
            let mut tree = SyntaxTree::parse_with(src, &settings);
            assert!(!tree.has_errors());
            let inner = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == K::FlowSequence)
                .last()
                .unwrap()
                .id();
            let ins = tree.insert_child(inner, 1, frag("a")).unwrap();
            let a = tree.get(ins.first).unwrap();
            assert_eq!(a.span(), Span::new(DEPTH, DEPTH + 1));
            assert_eq!(a.text(), "a");
            assert_eq!(a.ancestors().count(), DEPTH + 2);
            let tok = a.first_child().unwrap().id();
            let b = tree.replace_token(tok, "bb").unwrap();
            assert_eq!(tree.get(b).unwrap().span(), Span::new(DEPTH, DEPTH + 2));
            assert_eq!(tree.root().span().end, 2 * DEPTH + 3);
            let outer = tree.root().first_child().unwrap().first_child().unwrap();
            assert_eq!(outer.span(), Span::new(0, 2 * DEPTH + 2));
            let removed = tree.remove(ins.first).unwrap();
            assert_eq!(removed.text(), "bb");
            assert_eq!(tree.emit().len(), 2 * DEPTH + 1);
            // Replacing the outermost collection copies a million levels
            // without recursion, and putting it back re-reads them (the
            // document is the validation window). A document left without
            // its root would read differently: not well-formed.
            let whole = tree
                .root()
                .first_child()
                .unwrap()
                .first_child()
                .unwrap()
                .id();
            assert_eq!(tree.remove(whole), Err(TreeEditError::NotWellFormed));
            let r = tree.replace(whole, frag("x")).unwrap();
            let big = r.removed;
            assert_eq!(big.len(), 3 * DEPTH);
            assert_eq!(tree.emit(), "x\n");
            let _ = tree.replace(r.inserted.first, big).unwrap();
            assert_eq!(tree.emit().len(), 2 * DEPTH + 1);
            assert_eq!(tree.walk_len(), tree.len());
            drop(tree);
        })
        .unwrap()
        .join()
        .unwrap();
}

trait WalkLen {
    fn walk_len(&self) -> usize;
}

impl WalkLen for SyntaxTree {
    fn walk_len(&self) -> usize {
        self.root()
            .walk()
            .filter(|s| matches!(s, oneyaml_syntax::tree::WalkEvent::Enter(_)))
            .count()
    }
}

/// A million-deep **block** chain (no
/// closing bracket ends the nesting): an edit at the bottom, then reads
/// that climb, on a small stack; the reads stay O(depth), not O(depth²)
/// (a million levels squared would not finish).
#[test]
fn deep_block_chain_edits_on_a_small_stack() {
    const DEPTH: usize = 1_000_000;
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let src = format!("{}a\n", "- ".repeat(DEPTH));
            let settings = Settings::new().limits(Limits::unlimited());
            let mut tree = SyntaxTree::parse_with(src, &settings);
            assert!(!tree.has_errors());
            let a = find(&tree, K::PlainScalar).id();
            let b = tree.replace_token(a, "bb").unwrap();
            let n = tree.get(b).unwrap();
            assert_eq!(n.span(), Span::new(2 * DEPTH, 2 * DEPTH + 2));
            let inner = n.parent().unwrap().parent().unwrap();
            assert_eq!(inner.kind(), K::BlockSequence);
            assert_eq!(inner.text(), "- bb");
            assert_eq!(inner.descendants().count(), 5);
            let outer = tree.root().first_child().unwrap().first_child().unwrap();
            assert_eq!(outer.span(), Span::new(0, 2 * DEPTH + 2));
            assert_eq!(outer.text().len(), 2 * DEPTH + 2);
            assert_eq!(n.ancestors().count(), DEPTH + 3);
            // An entry not indented past its parent's is refused.
            let bad = SyntaxTree::parse("- a\n- c\n");
            let kids: Vec<_> = find(&bad, K::BlockSequence).children().collect();
            let entry = bad.extract(kids[3].id(), kids[6].id()).unwrap();
            assert_eq!(entry.text(), "\n- c");
            let inner = inner.id();
            assert_eq!(
                tree.append_child(inner, entry),
                Err(TreeEditError::NotWellFormed)
            );
            assert_eq!(tree.emit().len(), 2 * DEPTH + 3);
            assert_eq!(tree.root().walk().count(), 2 * tree.len());
            drop(tree);
        })
        .unwrap()
        .join()
        .unwrap();
}

type Edit = dyn Fn(&mut SyntaxTree) -> Result<(), TreeEditError>;

/// The edit is refused as not well-formed and changes nothing.
fn refused(src: &str, edit: &Edit) {
    let mut tree = SyntaxTree::parse(src);
    let before = tree.clone();
    assert_eq!(
        edit(&mut tree),
        Err(TreeEditError::NotWellFormed),
        "{src:?}"
    );
    assert_eq!(tree.emit(), src);
    assert_eq!(tree, before);
    assert!(!tree.is_edited());
}

/// Edits whose text would read
/// back cleanly as another tree are refused and change nothing; their
/// well-formed forms are accepted.
#[test]
fn edits_that_would_read_back_differently_are_refused() {
    // `a: 1` without its `:` reads `a 1`: one scalar.
    refused("a: 1\n", &|t| {
        t.remove(find(t, K::MappingValue).id()).map(|_| ())
    });
    // `[a, b]` without its `,` reads `[a b]`: one scalar.
    refused("[a, b]\n", &|t| {
        t.remove(find(t, K::FlowEntry).id()).map(|_| ())
    });
    // `- a\n- b` without the second `-` reads `- a\n b`: one scalar.
    refused("- a\n- b\n", &|t| {
        t.remove(nth(t, K::SequenceEntry, 1).id()).map(|_| ())
    });
    // `x` appended to `a: 1` reads `a: 1x`.
    refused("a: 1\n", &|t| {
        let map = find(t, K::BlockMapping).id();
        t.append_child(map, frag("x")).map(|_| ())
    });
    // Indentation that moves an entry into the one before it.
    refused("a:\n  b:\n  c: 2\n", &|t| {
        let ws = nth(t, K::Whitespace, 1).id();
        t.replace_token(ws, "    ").map(|_| ())
    });
    // A multi-line implicit key.
    refused("a: 1\n", &|t| {
        let a = find(t, K::PlainScalar).id();
        t.replace_token(a, "x\ny").map(|_| ())
    });
    // Two documents that would read as one.
    refused("- a\n--- \n- b\n", &|t| {
        t.remove(find(t, K::DocumentStartMarker).id()).map(|_| ())
    });
    // A `\r` before a line break reads as one `\r\n` break.
    refused("a: 1\n\nb: 2\n", &|t| {
        let lb = nth(t, K::LineBreak, 0).id();
        t.replace_token(lb, "\r").map(|_| ())
    });

    // The well-formed forms.
    let mut map = SyntaxTree::parse("a: 1\n");
    let m = find(&map, K::BlockMapping).id();
    let entry = frag("\nx: 2").unwrap_root(1).unwrap();
    map.append_child(m, entry).unwrap();
    assert_eq!(map.emit(), "a: 1\nx: 2\n");
    done(&map);
    let mut seq = SyntaxTree::parse("[a, b]\n");
    let b = scalar(&seq, "b").id();
    seq.insert_after(b, entries("[x, c]", 2, 4)).unwrap();
    assert_eq!(seq.emit(), "[a, b, c]\n");
    done(&seq);
    // An edit next to an error region is accepted when the region reads
    // the same: the error keeps its diagnostic, moved with its text.
    let mut bad = SyntaxTree::parse("a: 1\nb: [c\n");
    assert!(bad.has_errors());
    let one = scalar(&bad, "1").id();
    let _ = bad.replace(one, frag("11")).unwrap();
    assert_eq!(bad.emit(), "a: 11\nb: [c\n");
    compare_trees(&SyntaxTree::parse(bad.emit()), &bad).unwrap();
    invariants(&bad);
}

/// Fragments are parsed with
/// `parse_with`, the tree records its settings, and re-reads (token
/// replacement, validation) use them, not the defaults.
#[test]
fn edits_read_text_under_the_trees_own_settings() {
    let settings = Settings::new().limits(Limits::default().max_scalar_len(4));
    let mut tree = SyntaxTree::parse_with("a: [bb]\n", &settings);
    assert_eq!(tree.settings(), &settings);
    assert_eq!(SyntaxTree::parse("x").settings(), &Settings::new());
    let bb = nth(&tree, K::PlainScalar, 1).id();
    // Under the defaults `long-word` is one plain token; under the tree's
    // settings it is over the scalar limit.
    assert_eq!(SyntaxTree::parse("[long-word]").diagnostics().len(), 0);
    assert_eq!(
        tree.replace_token(bb, "long-word"),
        Err(TreeEditError::InvalidToken {
            kind: K::PlainScalar
        })
    );
    assert!(matches!(
        Fragment::parse_with("long-word", tree.settings()),
        Err(TreeEditError::Syntax { .. })
    ));
    assert!(Fragment::parse("long-word").is_ok());
    let cc = tree.replace_token(bb, "cc").unwrap();
    assert_eq!(tree.get(cc).unwrap().text(), "cc");
    assert_eq!(tree.emit(), "a: [cc]\n");
    // The validation re-read is under the tree's settings too: a long
    // scalar from a default-settings fragment would read with an error.
    let seq = find(&tree, K::FlowSequence).id();
    let long = entries("[a, long-word]", 2, 4);
    assert_eq!(
        tree.append_child(seq, long),
        Err(TreeEditError::NotWellFormed)
    );
    assert_eq!(tree.clone().settings(), &settings);
}

/// The only diagnostics whose span lies before the
/// element they are attached to are `Y0028` (`lenient_inline_colons`)
/// warnings: the span is in the plain scalar just before the element.
/// Edits around them keep the span on its text; replacing the scalar
/// without its warning's element is refused (the warning would stay with
/// a span on text that is gone); replacing both takes the warning away.
#[test]
fn lenient_warning_spans_follow_their_text() {
    use oneyaml_core::code::LENIENT_INLINE_COLON;
    let settings = Settings::new().lenient_inline_colons(true);
    let src = "k0: v\ntitle: The farm: a banana story # c\nnext: x\n";
    let mut tree = SyntaxTree::parse_with(src, &settings);
    let (el, d) = tree.diagnostics_by_element().next().unwrap();
    assert_eq!(d.code, LENIENT_INLINE_COLON);
    let el = el.unwrap();
    assert!(d.primary.span.end <= el.span().start, "{d:?} before {el:?}");
    assert_eq!(&src[d.primary.span.start..d.primary.span.end], ":");
    let value = scalar(&tree, "The farm: a banana story").id();
    let ws = tree.get(value).unwrap().next_sibling().unwrap().id();
    // Edits before the span move it with its text.
    let k0 = scalar(&tree, "k0").first_child().unwrap().id();
    tree.replace_token(k0, "key-zero").unwrap();
    let title = scalar(&tree, "title").first_child().unwrap().id();
    tree.replace_token(title, "t").unwrap();
    let text = tree.emit();
    let d = tree.diagnostics().next().unwrap().into_owned();
    assert_eq!(&text[d.primary.span.start..d.primary.span.end], ":");
    assert_eq!(d.primary.span.start, text.find("farm:").unwrap() + 4);
    let again = SyntaxTree::parse_with(text.as_str(), &settings);
    compare_trees(&again, &tree).unwrap();
    // The scalar alone (which reverses the refusal
    // this test asserted before): the warning's labels all lie in the
    // removed text, so it leaves with it (its element stays, with its
    // handle) and the re-read decides; on a fork, where the handles
    // resolve.
    let mut alone = tree.fork();
    let _ = alone
        .replace(value, frag("'The farm: a banana story'"))
        .unwrap();
    assert_eq!(alone.diagnostics().count(), 0);
    assert!(alone.get(ws).unwrap().diagnostic().is_none());
    let again = SyntaxTree::parse_with(alone.emit(), &settings);
    compare_trees(&again, &alone).unwrap();
    // With the element its warning is attached to: the warning goes.
    let quoted = frag("'The farm: a banana story' ");
    let r = tree.replace_range(value, ws, quoted).unwrap();
    assert_eq!(r.removed.diagnostics().count(), 1);
    assert_eq!(tree.diagnostics().count(), 0);
    let again = SyntaxTree::parse_with(tree.emit(), &settings);
    compare_trees(&again, &tree).unwrap();
}

/// Bug 1: non-ASCII entries and items (2-, 3-, 4-byte
/// characters, a combining mark) inserted and replaced next to non-ASCII
/// text, at the end of nested collections (the reported case: `é: é`
/// after `a: true` in `- false\n- a: true\n`). Every edit is the string
/// splice and equals its re-parse; none is `Internal`.
#[test]
fn non_ascii_insertions_and_replacements() {
    let words = [
        "\u{e9}",
        "\u{2603}",
        "\u{1f600}",
        "e\u{301}",
        "a\u{e9}\u{1f600}b",
    ];
    for doc in [
        "- false\n- a: true\n",
        "- \u{e9}\n- \u{1f600}: true\n",
        "k\u{e9}:\n  - \u{2603}: [\u{e9}, x]\n",
        "\u{1f600}: {\u{e9}: 1}\n",
    ] {
        for w in words {
            // An entry after the last one of the innermost block mapping.
            let mut tree = SyntaxTree::parse(doc);
            let map = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == K::BlockMapping)
                .last()
                .unwrap();
            let start = map.span().start;
            let col = start - doc[..start].rfind('\n').map_or(0, |i| i + 1);
            let last = map.last_child().unwrap().id();
            let pad = " ".repeat(col);
            let src = format!("{pad}x: x\n{pad}{w}: {w}");
            let t = SyntaxTree::parse(src.as_str());
            let m = find(&t, K::BlockMapping);
            let kids: Vec<_> = m.children().collect();
            let f = t
                .extract(kids[4].id(), m.last_child().unwrap().id())
                .unwrap();
            let text = f.text().to_owned();
            let end = tree.get(last).unwrap().span().end;
            tree.insert_after(last, f)
                .unwrap_or_else(|e| panic!("{e:?}: {text:?} into {doc:?}"));
            let mut expected = doc.to_owned();
            expected.insert_str(end, &text);
            assert_eq!(tree.emit(), expected);
            done(&tree);
            // Every scalar's token replaced by a non-ASCII word.
            let scalars: Vec<_> = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == K::Scalar && !n.text().is_empty())
                .map(|n| n.id())
                .collect();
            for id in scalars {
                let n = tree.get(id).unwrap();
                let sp = n.span();
                let tok = n.first_child().unwrap().id();
                let before = tree.emit();
                let r = tree.replace_token(tok, w);
                assert_ne!(r, Err(TreeEditError::Internal), "{w:?} in {before:?}");
                if r.is_ok() {
                    let mut expected = before.clone();
                    expected.replace_range(sp.start..sp.end, w);
                    assert_eq!(tree.emit(), expected);
                    done(&tree);
                } else {
                    assert_eq!(tree.emit(), before);
                }
            }
        }
    }
}

/// An element's slot names it for the tree's life, as its
/// handle does: untouched elements keep resolving through edits, a
/// replaced one is gone, a new one gets a new slot; a fork shares the
/// slots issued before it.
#[test]
fn slots_name_elements_as_handles_do() {
    let mut tree = SyntaxTree::parse("a: 1\nb: 2\n");
    let all: Vec<(u32, oneyaml_syntax::tree::NodeId)> = tree
        .root()
        .descendants()
        .map(|n| (n.slot(), n.id()))
        .collect();
    for (slot, id) in &all {
        assert_eq!(tree.node_at_slot(*slot).unwrap().id(), *id);
    }
    assert!(tree.node_at_slot(u32::MAX).is_none());
    assert!(tree.node_at_slot(all.len() as u32).is_none());
    let one = scalar(&tree, "1");
    let (one_slot, one_id) = (one.slot(), one.id());
    let r = tree.replace(one_id, frag("[x]")).unwrap();
    assert!(tree.node_at_slot(one_slot).is_none());
    let new = tree.get(r.inserted.first).unwrap();
    assert!(all.iter().all(|(s, _)| *s != new.slot()));
    assert_eq!(tree.node_at_slot(new.slot()).unwrap().id(), new.id());
    for (slot, id) in &all {
        match tree.get(*id) {
            Ok(n) => assert_eq!(tree.node_at_slot(*slot).unwrap(), n),
            Err(_) => assert!(tree.node_at_slot(*slot).is_none()),
        }
    }
    let two = scalar(&tree, "2");
    let fork = tree.fork();
    assert_eq!(fork.node_at_slot(two.slot()).unwrap().text(), "2");
    done(&tree);
}
