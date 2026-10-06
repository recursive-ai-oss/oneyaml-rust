//! `Document` edit regression fixes, one section per case; the timing
//! shapes of some are in `document_scaling.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, NodeKind, Settings, Stream, Value, path};

fn doc(src: &str) -> Document {
    Document::parse(src).unwrap()
}

/// The names of the aliases of the node at `p`, read through the index.
fn alias_texts(d: &Document, p: &oneyaml::Path) -> Vec<String> {
    let id = d.id_of(p).unwrap();
    d.aliases_of(id)
        .unwrap()
        .into_iter()
        .map(|a| d.node(a).unwrap().text().into_owned())
        .collect()
}

// --------------------------------------------------- index with repeated names

/// The anchor index stays exact when names repeat (valid YAML, section
/// 3.2.2.2): removals of subtrees holding one name many times, renames
/// with many aliases, and re-insertions, checked against a fresh parse.
#[test]
fn index_with_repeated_names() {
    let src = "a: &x 1\nsub:\n  - &x 2\n  - *x\n  - &x 3\n  - *x\nb: *x\nc: &x 4\nd: *x\ne: *x\n";
    let mut d = doc(src);
    // Build the index, then remove a subtree with two `&x` and two `*x`.
    assert_eq!(alias_texts(&d, &path!["c"]), ["*x", "*x"]);
    let r = d.remove_path(&path!["sub"]);
    // `b: *x` refers to `&x 3` inside: refused, nothing changed.
    assert!(matches!(r, Err(EditError::AnchorInUse { .. })), "{r:?}");
    assert_eq!(d.to_string(), src);
    let _ = d.remove_path(&path!["b"]).unwrap();
    let removed = d.remove_path(&path!["sub"]).unwrap();
    assert_eq!(alias_texts(&d, &path!["a"]), Vec::<String>::new());
    assert_eq!(alias_texts(&d, &path!["c"]), ["*x", "*x"]);
    agrees_with_its_text(&d);
    // Moved back to the end: its anchors come after `c`'s, and capture
    // nothing (no alias of `x` follows the end).
    d.root_mut().unwrap().insert_removed(removed).unwrap();
    assert_eq!(alias_texts(&d, &path!["c"]), ["*x", "*x"]);
    assert_eq!(alias_texts(&d, &path!["sub", 0]), ["*x"]);
    assert_eq!(alias_texts(&d, &path!["sub", 2]), ["*x"]);
    agrees_with_its_text(&d);
    // Rename `c`'s anchor: its two aliases follow; the others keep `x`.
    let c = d.id_of(&path!["c"]).unwrap();
    d.set_anchor(c, Some("y")).unwrap();
    assert_eq!(
        d.to_string(),
        "a: &x 1\nc: &y 4\nd: *y\ne: *y\nsub:\n  - &x 2\n  - *x\n  - &x 3\n  - *x\n"
    );
    assert_eq!(alias_texts(&d, &path!["c"]), ["*y", "*y"]);
    assert_eq!(alias_texts(&d, &path!["sub", 2]), ["*x"]);
    agrees_with_its_text(&d);
    // And back, then remove every alias and anchor one at a time.
    d.set_anchor(c, Some("z")).unwrap();
    assert_eq!(alias_texts(&d, &path!["c"]), ["*z", "*z"]);
    for p in [path!["d"], path!["e"], path!["sub", 3], path!["sub", 1]] {
        let _ = d.remove_path(&p).unwrap();
    }
    for p in [path!["a"], path!["c"], path!["sub", 0], path!["sub", 0]] {
        let id = d.id_of(&p).unwrap();
        assert_eq!(d.aliases_of(id).unwrap(), vec![]);
    }
    let a = d.id_of(&path!["a"]).unwrap();
    d.set_anchor(a, None).unwrap();
    let s1 = d.id_of(&path!["sub", 1]).unwrap();
    d.set_anchor(s1, None).unwrap();
    let s0 = d.id_of(&path!["sub", 0]).unwrap();
    d.get_mut("sub").unwrap().push_alias(s0).unwrap();
    assert_eq!(alias_texts(&d, &path!["sub", 0]), ["*x"]);
    agrees_with_its_text(&d);
}

// ----------------------------------------------------- document mut is a guard

/// `Stream::document_mut` is a guard; edits
/// through it change document `i` only (the swap it prevents is the
/// `compile_fail` example on `DocumentMut`).
#[test]
fn document_mut_is_a_guard() {
    let mut s = Stream::parse("a: 1\n---\nb: 2\n---\nc: 3\n").unwrap();
    let mut second = s.document_mut(1).unwrap();
    assert_eq!(second.to_value().unwrap()["b"].as_u64(), Some(2));
    assert_eq!(second.to_string(), "---\nb: 2\n"); // its region
    let b = second.id_of(&path!["b"]).unwrap();
    second.node_mut(b).unwrap().set(20).unwrap();
    second.set_path(&path!["list"], [1]).unwrap();
    let r = second.remove_path(&path!["list"]).unwrap();
    second.root_mut().unwrap().insert_removed(r).unwrap();
    let b = second.id_of(&path!["b"]).unwrap();
    second.node_mut(b).unwrap().set_anchor(Some("q")).unwrap();
    second.get_mut("b").unwrap().set(21).unwrap();
    assert!(second.get_path_mut(&path!["list", 0]).is_ok());
    let _ = second.get_path_or_create(&path!["m"]).unwrap();
    let m = second.id_of(&path!["m"]).unwrap();
    let _ = second.remove_node(m).unwrap();
    assert_eq!(
        s.to_string(),
        "a: 1\n---\nb: &q 21\nlist:\n  - 1\n---\nc: 3\n"
    );
    let mut third = s.document_mut(2).unwrap();
    third.set_root(vec!["x"]).unwrap();
    assert_eq!(
        s.to_string(),
        "a: 1\n---\nb: &q 21\nlist:\n  - 1\n---\n- x\n"
    );
    assert_eq!(s.len(), 3);
    assert_eq!(
        s.document(0).unwrap().to_value().unwrap()["a"].as_u64(),
        Some(1)
    );
    assert!(!s.has_errors());
}

// ---------------------------------- set anchor never captures a dangling alias

/// Setting or renaming an anchor must not
/// capture a later alias that did not already resolve to that node; a
/// dangling alias stays dangling (the errors stay).
#[test]
fn set_anchor_never_captures_a_dangling_alias() {
    for src in [
        "a: 1\nb: *y\n",
        "a: &x 1\nb: *y\nc: *x\n",
        "a: {k: 1}\nb: [*y]\n",
    ] {
        let p = Document::parse_recovering(src);
        assert!(p.has_errors(), "{src:?}");
        let mut d = p.into_value();
        let a = d.id_of(&path!["a"]).unwrap();
        let e = d.set_anchor(a, Some("y")).unwrap_err();
        assert!(
            matches!(&e, EditError::DuplicateAnchor { anchor, .. } if anchor == "y"),
            "{src:?}: {e:?}"
        );
        assert_eq!(d.to_string(), src);
        assert!(d.has_errors(), "{src:?}");
        agrees_with_its_text(&d);
    }
    // A dangling alias *before* the node stays dangling: allowed.
    let mut d = Document::parse_recovering("b: *y\na: 1\n").into_value();
    let a = d.id_of(&path!["a"]).unwrap();
    d.set_anchor(a, Some("y")).unwrap();
    assert_eq!(d.to_string(), "b: *y\na: &y 1\n");
    assert!(d.has_errors());
    agrees_with_its_text(&d);
    // Another document's alias is not this one's (section 3.2.2.2).
    let mut s = Stream::parse_recovering("a: 1\n---\nb: *y\n").into_value();
    let mut first = s.document_mut(0).unwrap();
    let a = first.id_of(&path!["a"]).unwrap();
    first.node_mut(a).unwrap().set_anchor(Some("y")).unwrap();
    assert_eq!(s.to_string(), "a: &y 1\n---\nb: *y\n");
    assert!(s.has_errors());
}

// -------------------------------------------------------- empty values of sets

/// `is_key` decides the ambiguous shapes (the empty values of `? a` and
/// `{a, b}`) from the neighbours: the right entries go.
#[test]
fn empty_values_of_sets() {
    for (src, want) in [
        ("{a, b, c}\n", "{a, c}\n"),
        ("? a\n? b\n? c\n", "? a\n? c\n"),
        ("{a, b: 1, c}\n", "{a, c}\n"),
    ] {
        let mut d = doc(src);
        let v = d
            .root()
            .unwrap()
            .as_mapping()
            .unwrap()
            .iter()
            .nth(1)
            .unwrap()
            .1;
        assert_eq!(v.text(), if src.contains("b: 1") { "1" } else { "" });
        let id = v.id();
        let r = d.remove_node(id).unwrap();
        assert_eq!(r.key(), Some("b"), "{src:?}");
        assert_eq!(d.to_string(), want, "{src:?}");
        agrees_with_its_text(&d);
        // A key is refused as a key (read from its neighbours too).
        let k = d
            .root()
            .unwrap()
            .as_mapping()
            .unwrap()
            .iter()
            .nth(1)
            .unwrap()
            .0;
        let e = d.remove_node(k.id()).unwrap_err();
        assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    }
}

// ---------------------------------------------- stream diagnostics after edits

/// After edits, `Stream::diagnostics()`
/// and `Stream::has_errors()` both describe the edited state.
#[test]
fn stream_diagnostics_after_edits() {
    // An edit before an error moves it: the diagnostics are those of the
    // edited text (spans included), not of the input.
    let p = Stream::parse_recovering("a: 1\n---\nb: *x\n");
    assert!(p.has_errors());
    let mut s = p.into_value();
    let before = s.diagnostics();
    assert_eq!(before.len(), 1);
    s.document_mut(0)
        .unwrap()
        .set_path(&path!["z"], "new")
        .unwrap();
    assert_eq!(s.to_string(), "a: 1\nz: new\n---\nb: *x\n");
    assert!(s.has_errors());
    let again = Stream::parse_recovering(s.to_string()).into_value();
    assert_eq!(s.diagnostics(), again.diagnostics());
    assert_ne!(s.diagnostics(), before);
    assert_eq!(
        s.diagnostics()[0].primary.span.start,
        before[0].primary.span.start + "z: new\n".len()
    );
    // And the other way: an edit that makes an error (the anchored
    // mapping a merge key reads becomes a scalar) shows in both.
    let settings = Settings::new().merge_keys(true);
    let src = "a: 1\n---\nbase: &b {x: 1}\nuse:\n  <<: *b\n";
    let mut s = Stream::parse_with(src, &settings).unwrap();
    assert!(!s.has_errors());
    s.document_mut(1)
        .unwrap()
        .set_path(&path!["base"], 3)
        .unwrap();
    assert!(s.has_errors());
    let d = s.diagnostics();
    let text = s.to_string();
    let e = Stream::parse_with(text.clone(), &settings).unwrap_err();
    assert_eq!(d.last().unwrap().code, e.code(), "{text:?}");
}

// ------------------------------------------------------- moving the only entry

/// A move of the only entry (whose removal
/// rewrites the collection and leaves no undo steps) is the identity; the
/// public API never reaches it with another position.
#[test]
fn moving_the_only_entry() {
    for src in ["k:\n  a: 1\n", "k:\n  - 1\n", "k: {a: 1}\n", "k: [a: 1]\n"] {
        let mut d = doc(src);
        let mut k = d.get_mut("k").unwrap();
        k.move_entry(0, 0).unwrap();
        let e = k.move_entry(0, 1).unwrap_err();
        assert!(
            matches!(
                e,
                EditError::IndexOutOfBounds {
                    index: 1,
                    len: 1,
                    ..
                }
            ),
            "{src:?}: {e:?}"
        );
        assert_eq!(d.to_string(), src);
    }
}

/// The comment accounting when the last
/// entry of a block collection goes: the previous entry's same-line
/// comment is copied after the collection and its original goes with the
/// removed range, so nothing is duplicated or lost.
#[test]
fn last_entry_comments_are_neither_lost_nor_duplicated() {
    let comments = |d: &Document| {
        d.syntax()
            .root()
            .descendants()
            .filter(|n| n.kind() == oneyaml_syntax::tree::SyntaxKind::Comment)
            .map(|n| n.text().into_owned())
            .collect::<Vec<_>>()
    };
    for (src, key, left, taken) in [
        (
            "a: 1 # one\nb: 2 # two\n",
            "b",
            "a: 1 # one\n",
            vec!["# two"],
        ),
        ("a: 1 # one\nb: 2\n", "b", "a: 1 # one\n", vec![]),
        (
            "k:\n  a: 1 # one\n  # lead\n  b: 2 # two\nz: 0\n",
            "b",
            "k:\n  a: 1 # one\nz: 0\n",
            vec!["# lead", "# two"],
        ),
    ] {
        let mut d = doc(src);
        let before = comments(&d).len();
        let r = if src.starts_with('k') {
            d.get_mut("k").unwrap().remove(key).unwrap()
        } else {
            d.root_mut().unwrap().remove(key).unwrap()
        };
        assert_eq!(d.to_string(), left, "{src:?}");
        assert_eq!(r.comments(), taken.as_slice(), "{src:?}");
        assert_eq!(comments(&d).len() + r.comments().len(), before, "{src:?}");
        agrees_with_its_text(&d);
        // And back: every comment returns once.
        if src.starts_with('k') {
            d.get_mut("k").unwrap().insert_removed(r).unwrap();
        } else {
            d.root_mut().unwrap().insert_removed(r).unwrap();
        }
        assert_eq!(comments(&d).len(), before, "{src:?}: {}", d);
        agrees_with_its_text(&d);
    }
}

// --------------------------------------- handles through generations of copies

/// Handles across copies (the syntax layer's `SyntaxTree::fork`, no handle
/// maps): a handle taken at the start resolves through many generations of
/// clone + edit, and one removed in a generation stays stale in its
/// descendants.
#[test]
fn handles_through_generations_of_copies() {
    let mut d = doc("keep: 1\ngone: 2\nlist: [0]\n");
    let keep = d.id_of(&path!["keep"]).unwrap();
    let gone = d.id_of(&path!["gone"]).unwrap();
    let orig = d.clone();
    let mut held = Vec::new();
    for i in 0..12 {
        let mut c = d.clone();
        c.get_mut("list").unwrap().push(i).unwrap();
        if i == 3 {
            let _ = c.remove_node(gone).unwrap();
        }
        held.push(c.id_of(&path!["list", 0]).unwrap());
        assert_eq!(c.node(keep).unwrap().text(), "1");
        // The original is untouched by its copies.
        assert_eq!(orig.node(gone).unwrap().text(), "2");
        assert_eq!(orig.get("list").unwrap().text(), "[0]");
        d = c;
    }
    assert_eq!(d.node(gone).unwrap_err(), EditError::StaleNode);
    for h in held {
        assert_eq!(d.node(h).unwrap().text(), "0");
    }
    assert_eq!(d.get("list").unwrap().as_sequence().unwrap().len(), 13);
    assert_eq!(d.node(keep).unwrap().kind(), NodeKind::Scalar);
    assert_eq!(d.to_value().unwrap()["keep"], Value::from(1));
}

// ---------------------------------- moves that would make the meaning an error

/// Gaps the stricter meaning oracle of the edit fuzz found: a
/// move that would make the meaning an error is refused, and nothing
/// changes.
#[test]
fn moves_that_would_make_the_meaning_an_error() {
    // An alias moved into its own anchored node (a recursive alias).
    let mut d = doc("a: &x [1]\nb: [*x, 2]\n");
    let r = d.get_mut("b").unwrap().remove_at(0).unwrap();
    let text = d.to_string();
    let e = d
        .get_mut("a")
        .unwrap()
        .insert_removed(r.clone())
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    assert_eq!(d.to_string(), text);
    d.get_mut("b").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "a: &x [1]\nb: [2, *x]\n");
    agrees_with_its_text(&d);
    // A second merge key in one mapping (a duplicate key).
    let s = Settings::new().merge_keys(true);
    let src = "base: &b {x: 1}\nm1:\n  <<: *b\n  k: 1\nm2:\n  <<: *b\n  j: 2\n";
    let mut d = Document::parse_with(src, &s).unwrap();
    let v = d
        .get("m1")
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .1;
    let r = d.remove_node(v.id()).unwrap();
    let text = d.to_string();
    let e = d
        .get_mut("m2")
        .unwrap()
        .insert_removed(r)
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::KeyExists { .. }), "{e:?}");
    assert_eq!(d.to_string(), text);
    agrees_with_its_text(&d);
    // Keys equal by meaning, not by text: `~`, `null` and an empty key.
    let mut d = doc("m: {~: 1}\nn: {null: 2, k: 3}\n");
    let v = d
        .get("n")
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .1;
    let r = d.remove_node(v.id()).unwrap();
    let e = d
        .get_mut("m")
        .unwrap()
        .insert_removed(r.clone())
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::KeyExists { .. }), "{e:?}");
    assert_eq!(d.to_string(), "m: {~: 1}\nn: {k: 3}\n");
    d.get_mut("n").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "m: {~: 1}\nn: {k: 3, null: 2}\n");
    agrees_with_its_text(&d);
}
