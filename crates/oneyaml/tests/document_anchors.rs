//! Anchors and aliases, restricted mode, moves with
//! `Removed`, streams, empty documents, and one test per `EditError`
//! variant the edit API returns.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{
    Document, EditError, Emit, Limits, NodeKind, Settings, Stream, Tag, TaggedValue, Value, code,
    path,
};

fn doc(src: &str) -> Document {
    Document::parse(src).unwrap()
}

#[test]
fn editing_an_anchor_is_seen_through_its_aliases() {
    // Editing the anchored node is seen through every alias: one node, one
    // meaning.
    let mut d = doc("base: &b {x: 1}\nuse: *b\n");
    d.set_path(&path!["base", "x"], 2).unwrap();
    assert_eq!(d.to_value().unwrap()["use"]["x"].as_u64(), Some(2));
    // Through the alias: the anchored node is edited.
    d.set_path(&path!["use", "z"], 3).unwrap();
    assert_eq!(d.to_string(), "base: &b {x: 2, z: 3}\nuse: *b\n");
    agrees_with_its_text(&d);
    // Replacing the alias itself replaces only the alias.
    d.set_path(&path!["use"], 0).unwrap();
    assert_eq!(d.to_string(), "base: &b {x: 2, z: 3}\nuse: 0\n");
    agrees_with_its_text(&d);
}

#[test]
fn replacing_an_anchored_node_keeps_its_anchor() {
    // A replacement keeps the anchor.
    let mut d = doc("a: &x 1\nb: *x\nc:\n  - &y [1]\n  - *y\n");
    d.set_path(&path!["a"], vec![1, 2]).unwrap();
    assert_eq!(
        d.to_string(),
        "a: &x\n  - 1\n  - 2\nb: *x\nc:\n  - &y [1]\n  - *y\n"
    );
    d.set_path(&path!["c", 0], serde_json::json!({"k": 1, "l": 2}))
        .unwrap();
    assert_eq!(
        d.to_string(),
        "a: &x\n  - 1\n  - 2\nb: *x\nc:\n  - &y\n    k: 1\n    l: 2\n  - *y\n"
    );
    agrees_with_its_text(&d);
    let v = d.to_value().unwrap();
    assert_eq!(v["b"], v["a"]);
    assert_eq!(v["c"][1], v["c"][0]);
}

#[test]
fn anchors_in_use_are_not_removed() {
    // An anchor an alias still uses cannot be removed.
    let mut d = doc("a: &x {k: 1}\nb: [*x, *x]\nc: 2\n");
    let a = d.id_of(&path!["a"]).unwrap();
    let uses = d.aliases_of(a).unwrap();
    assert_eq!(uses.len(), 2);
    let e = d.remove_path(&path!["a"]).unwrap_err();
    match e {
        EditError::AnchorInUse {
            anchor, aliases, ..
        } => {
            assert_eq!(anchor, "x");
            assert_eq!(aliases, uses);
        }
        other => panic!("{other:?}"),
    }
    // A descendant's anchor counts too; replacing the node keeps only its
    // own anchor.
    let mut d2 = doc("a:\n  in: &y 1\nb: *y\n");
    let e = d2.set_path(&path!["a"], 0).unwrap_err();
    assert!(matches!(e, EditError::AnchorInUse { .. }), "{e:?}");
    assert_eq!(d2.to_string(), "a:\n  in: &y 1\nb: *y\n");
    // Remove the aliases first, then the anchor.
    let _ = d.remove_path(&path!["b"]).unwrap();
    let _ = d.remove_path(&path!["a"]).unwrap();
    assert_eq!(d.to_string(), "c: 2\n");
    agrees_with_its_text(&d);
    assert_eq!(d.aliases_of(d.id_of(&path!["c"]).unwrap()).unwrap(), vec![]);
}

#[test]
fn set_anchor_renames_adds_and_removes() {
    // Renaming an anchor renames its aliases.
    let mut d = doc("a: &x 1\nb: *x\nc: [*x]\nd: 2\n");
    let a = d.id_of(&path!["a"]).unwrap();
    let a = d.set_anchor(a, Some("y")).unwrap();
    assert_eq!(d.to_string(), "a: &y 1\nb: *y\nc: [*y]\nd: 2\n");
    agrees_with_its_text(&d);
    let dd = d.id_of(&path!["d"]).unwrap();
    let e = d.set_anchor(dd, Some("y")).unwrap_err();
    assert!(
        matches!(e, EditError::DuplicateAnchor { ref anchor, .. } if anchor == "y"),
        "{e:?}"
    );
    let e = d.set_anchor(a, None).unwrap_err();
    assert!(
        matches!(e, EditError::AnchorInUse { ref anchor, .. } if anchor == "y"),
        "{e:?}"
    );
    let dd = d.set_anchor(dd, Some("z")).unwrap();
    assert_eq!(d.to_string(), "a: &y 1\nb: *y\nc: [*y]\nd: &z 2\n");
    d.set_anchor(dd, None).unwrap();
    assert_eq!(d.to_string(), "a: &y 1\nb: *y\nc: [*y]\nd: 2\n");
    agrees_with_its_text(&d);
    // Block collections take and lose anchors on their indicator line.
    let mut d = doc("m:\n  k: 1\ns:\n  - k: 1\n    l: 2\n");
    let m = d.id_of(&path!["m"]).unwrap();
    let m = d.set_anchor(m, Some("m")).unwrap();
    let s0 = d.id_of(&path!["s", 0]).unwrap();
    d.set_anchor(s0, Some("s")).unwrap();
    assert_eq!(
        d.to_string(),
        "m: &m\n  k: 1\ns:\n  - &s\n    k: 1\n    l: 2\n"
    );
    agrees_with_its_text(&d);
    d.set_anchor(m, None).unwrap();
    assert_eq!(
        d.to_string(),
        "m:\n  k: 1\ns:\n  - &s\n    k: 1\n    l: 2\n"
    );
    agrees_with_its_text(&d);
    // An invalid name is a typed error (YAML 1.2.2
    // production 102 `ns-anchor-char`), not `Unsupported`.
    let bad = d.id_of(&path!["s", 0, "k"]).unwrap();
    for name in ["a b", "", "a,b", "a]", "a\u{FEFF}", "a\u{FFFE}", "a\tb"] {
        assert!(
            matches!(
                d.set_anchor(bad, Some(name)).unwrap_err(),
                EditError::InvalidAnchorName { name: ref n, .. } if n == name
            ),
            "{name:?}"
        );
    }
    // U+0085 and U+2028 are `ns-char` in YAML 1.2 (not line breaks).
    for name in ["n\u{85}l", "l\u{2028}s", "\u{e9}"] {
        let id = d.id_of(&path!["s", 0, "k"]).unwrap();
        let id = d.set_anchor(id, Some(name)).unwrap();
        agrees_with_its_text(&d);
        assert_eq!(d.node(id).unwrap().anchor(), Some(name));
        d.set_anchor(id, None).unwrap();
    }
    assert_eq!(
        d.to_string(),
        "m:\n  k: 1\ns:\n  - &s\n    k: 1\n    l: 2\n"
    );
}

#[test]
fn inserting_aliases() {
    // An alias must follow its anchor.
    let mut d = doc("a: &x 1\nlist: [0]\nlate: &l 2\nplain: 3\n");
    let x = d.id_of(&path!["a"]).unwrap();
    let l = d.id_of(&path!["late"]).unwrap();
    let p = d.id_of(&path!["plain"]).unwrap();
    let mut list = d.get_mut("list").unwrap();
    list.push_alias(x).unwrap();
    let e = list.push_alias(l).unwrap_err();
    assert!(
        matches!(e, EditError::AliasBeforeAnchor { ref anchor, .. } if anchor == "l"),
        "{e:?}"
    );
    assert!(matches!(
        list.push_alias(p).unwrap_err(),
        EditError::NotAnchored { .. }
    ));
    let mut root = d.root_mut().unwrap();
    root.insert_alias("again", l).unwrap();
    assert!(matches!(
        root.insert_alias("again", x).unwrap_err(),
        EditError::KeyExists { .. }
    ));
    assert_eq!(
        d.to_string(),
        "a: &x 1\nlist: [0, *x]\nlate: &l 2\nplain: 3\nagain: *l\n"
    );
    agrees_with_its_text(&d);
    // Not inside its own anchored node.
    let mut d = doc("a: &x [1]\n");
    let x = d.id_of(&path!["a"]).unwrap();
    let e = d.node_mut(x).unwrap().push_alias(x).unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
}

#[test]
fn moves_and_removed_nodes() {
    let mut d = doc("a: 1 # one\nb:\n  x: 1\nc: [q]\n");
    d.root_mut().unwrap().move_entry(0, 2).unwrap();
    assert_eq!(d.to_string(), "b:\n  x: 1\nc: [q]\na: 1 # one\n");
    agrees_with_its_text(&d);
    let r = d.remove_path(&path!["a"]).unwrap();
    d.get_mut("b").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "b:\n  x: 1\n  a: 1 # one\nc: [q]\n");
    agrees_with_its_text(&d);
    // Into a flow sequence: an item.
    let item = d.remove_path(&path!["c", 0]).unwrap();
    let r = d.remove_path(&path!["b", "x"]).unwrap();
    let mut b = d.get_mut("b").unwrap();
    assert!(matches!(
        b.insert_removed(item.clone()).unwrap_err().into_error(),
        EditError::Unsupported { .. }
    ));
    b.insert_removed_at(0, r.clone()).unwrap();
    assert!(matches!(
        b.insert_removed(r).unwrap_err().into_error(),
        EditError::KeyExists { .. }
    ));
    d.get_mut("c").unwrap().insert_removed(item).unwrap();
    assert_eq!(d.to_string(), "b:\n  x: 1\n  a: 1 # one\nc: [q]\n");
    agrees_with_its_text(&d);
    // Another document's removed node.
    let mut other = doc("z: 1\ny: 2\n");
    let foreign = other.remove_path(&path!["z"]).unwrap();
    assert_eq!(
        d.root_mut()
            .unwrap()
            .insert_removed(foreign)
            .unwrap_err()
            .into_error(),
        EditError::WrongDocument
    );
    // A block value moved into a flow mapping that cannot hold it.
    let mut d = doc("k: x, y\nf: {a: 1}\n");
    let r = d.remove_path(&path!["k"]).unwrap();
    let e = d
        .get_mut("f")
        .unwrap()
        .insert_removed(r)
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::NotWellFormed { .. }), "{e:?}");
    assert_eq!(d.to_string(), "f: {a: 1}\n");
    // Reorders keep anchors before their aliases (rule 6).
    let mut d = doc("- &a 1\n- *a\n- 3\n");
    let e = d.root_mut().unwrap().move_entry(0, 2).unwrap_err();
    assert!(
        matches!(e, EditError::AliasBeforeAnchor { ref anchor, .. } if anchor == "a"),
        "{e:?}"
    );
    d.root_mut().unwrap().move_entry(2, 0).unwrap();
    assert_eq!(d.to_string(), "- 3\n- &a 1\n- *a\n");
    agrees_with_its_text(&d);
}

#[test]
fn entry_api() {
    let mut d = doc("a: 1\n");
    let e = d.root_mut().unwrap().entry("a").unwrap();
    assert!(e.is_occupied());
    let n = e.or_insert(5).unwrap();
    assert_eq!(n.get().unwrap().text(), "1");
    let e = d.root_mut().unwrap().entry("b").unwrap();
    assert!(!e.is_occupied());
    e.or_insert(vec!["x"]).unwrap();
    assert_eq!(d.to_string(), "a: 1\nb:\n  - x\n");
    agrees_with_its_text(&d);
}

#[test]
fn merge_keys_are_ordinary_entries() {
    // Merge keys get no special treatment: an explicit key shadows a merged
    // one.
    let s = Settings::new().merge_keys(true);
    let mut d = Document::parse_with("base: &b {image: a, port: 1}\nsvc:\n  <<: *b\n", &s).unwrap();
    d.set_path(&path!["svc", "image"], "b").unwrap();
    assert_eq!(
        d.to_string(),
        "base: &b {image: a, port: 1}\nsvc:\n  <<: *b\n  image: b\n"
    );
    assert_eq!(d.to_value().unwrap()["svc"]["image"].as_str(), Some("b"));
    assert_eq!(d.to_value().unwrap()["base"]["image"].as_str(), Some("a"));
    // Editing the merged key's target changes every merge.
    let port = d.id_of(&path!["svc", "port"]).unwrap();
    d.node_mut(port).unwrap().set(2).unwrap();
    assert_eq!(d.to_value().unwrap()["svc"]["port"].as_u64(), Some(2));
    agrees_with_its_text(&d);
}

#[test]
fn restricted_documents_protect_error_regions() {
    let src = "good:\n  x: 1\nbad: @x\nmore: [1]\n";
    let p = Document::parse_recovering(src);
    assert!(p.has_errors());
    let mut d = p.into_value();
    // Next to the error region: read-only.
    let e = d.root_mut().unwrap().insert("new", 1).unwrap_err();
    assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
    let e = d.set_path(&path!["bad"], 1).unwrap_err();
    assert!(
        matches!(e, EditError::ReadOnly { .. } | EditError::ErrorNode { .. }),
        "{e:?}"
    );
    let e = d.set_path(&path!["bad", "k"], 1).unwrap_err();
    assert!(matches!(e, EditError::ErrorNode { .. }), "{e:?}");
    // Elsewhere: editable, and the error set is unchanged.
    d.set_path(&path!["good", "x"], 2).unwrap();
    d.get_mut("good").unwrap().insert("z", 3).unwrap();
    assert_eq!(d.to_string(), "good:\n  x: 2\n  z: 3\nbad: @x\nmore: [1]\n");
    assert!(d.has_errors());
    agrees_with_its_text(&d);
    // Encoding errors: nothing is editable (the bytes are kept as they were).
    let mut d = Document::from_slice_recovering(b"a: \xff\nb: 1\n", &Settings::new()).into_value();
    let e = d.set_path(&path!["b"], 2).unwrap_err();
    assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
}

#[test]
fn stream_documents_are_copy_on_write() {
    // This test once pinned handles going stale on a snapshot's first edit. A
    // snapshot is still copy-on-write (the stream never changes through it),
    // but handles taken before the copy resolve in both, and the stream is
    // edited through `Stream::document_mut`.
    let mut s = Stream::parse("a: 1\n---\nb: 2\nc: 3\n").unwrap();
    let mut second = s.document(1).unwrap();
    let b = second.id_of(&path!["b"]).unwrap();
    let c = second.id_of(&path!["c"]).unwrap();
    second.set_path(&path!["b"], 3).unwrap();
    assert_eq!(second.to_string(), "---\nb: 3\nc: 3\n"); // its region
    assert_eq!(s.to_string(), "a: 1\n---\nb: 2\nc: 3\n");
    // The replaced node's handle is stale; the others resolve in both.
    assert_eq!(second.node(b).unwrap_err(), EditError::StaleNode);
    assert_eq!(second.node(c).unwrap().text(), "3");
    let mut in_stream = s.document_mut(1).unwrap();
    assert_eq!(in_stream.node(b).unwrap().text(), "2");
    in_stream.node_mut(c).unwrap().set(4).unwrap();
    assert_eq!(s.to_string(), "a: 1\n---\nb: 2\nc: 4\n");
    assert_eq!(second.to_value().unwrap()["c"].as_u64(), Some(3));
    assert!(second.diagnostics().is_empty());
    // A clone is independent, and keeps the handles.
    let b2 = second.id_of(&path!["b"]).unwrap();
    let mut copy = second.clone();
    copy.set_path(&path!["b"], 5).unwrap();
    assert_eq!(second.to_value().unwrap()["b"].as_u64(), Some(3));
    assert_eq!(copy.to_value().unwrap()["b"].as_u64(), Some(5));
    assert_eq!(second.node(b2).unwrap().text(), "3");
    assert_eq!(copy.node(c).unwrap().text(), "3");
}

#[test]
fn empty_documents() {
    for (src, want) in [
        ("", "k: v\n"),
        // The header never changes as a side effect: `# c`
        // is the header of a document without a root (rule 2); a blank
        // line keeps it so above the new root (without it, rule 3 would
        // make it the first entry's leading comment).
        ("# c\n", "# c\n\nk: v\n"),
        ("# c", "# c\n\nk: v\n"),
    ] {
        let mut d = doc(src);
        assert!(d.root().is_none());
        assert!(matches!(
            d.root_mut().unwrap_err(),
            EditError::NotFound { .. }
        ));
        d.set_path(&path!["k"], "v").unwrap();
        assert_eq!(d.to_string(), want);
        agrees_with_its_text(&d);
    }
    let mut d = doc("");
    d.set_root(vec![1, 2]).unwrap();
    assert_eq!(d.to_string(), "- 1\n- 2\n");
    d.set_root("x").unwrap();
    assert_eq!(d.to_string(), "x\n");
    agrees_with_its_text(&d);
    let mut d = doc("---\n");
    d.set_root(1).unwrap();
    assert_eq!(d.to_string(), "--- 1\n");
    agrees_with_its_text(&d);
    let mut d = doc("--- x\n");
    d.set_root(serde_json::json!({"a": 1})).unwrap();
    assert_eq!(d.to_string(), "---\na: 1\n");
    agrees_with_its_text(&d);
}

#[test]
fn every_error_variant() {
    let mut d = doc("m: {a: 1}\ns: [1]\nx: 1\n");
    assert!(matches!(
        d.set_path(&path!["x", "y"], 1).unwrap_err(),
        EditError::NotAMapping { .. }
    ));
    assert!(matches!(
        d.set_path(&path!["m", 0], 1).unwrap_err(),
        EditError::NotASequence { .. }
    ));
    assert!(matches!(
        d.set_path(&path!["s", 5], 1).unwrap_err(),
        EditError::IndexOutOfBounds {
            index: 5,
            len: 1,
            ..
        }
    ));
    assert!(matches!(
        d.set_path(&path!["new", 3], 1).unwrap_err(),
        EditError::IndexOutOfBounds {
            index: 3,
            len: 0,
            ..
        }
    ));
    assert!(matches!(
        d.get_mut("m").unwrap().remove("zz").unwrap_err(),
        EditError::NotFound { .. }
    ));
    assert!(matches!(
        d.get_mut("x").unwrap().push(1).unwrap_err(),
        EditError::NotASequence { .. }
    ));
    assert!(matches!(
        d.get_mut("s").unwrap().insert("k", 1).unwrap_err(),
        EditError::NotAMapping { .. }
    ));
    let key = d
        .root()
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .0
        .id();
    assert!(matches!(
        d.node_mut(key).unwrap().set(1).unwrap_err(),
        EditError::Unsupported { .. }
    ));
    let root = d.root().unwrap().id();
    assert!(matches!(
        d.remove_node(root).unwrap_err(),
        EditError::Unsupported { .. }
    ));
    // Rejected: coded diagnostics.
    let tagged = Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("!t"),
        value: Value::from(1),
    }));
    let e = d.set_path(&path!["x"], &tagged).unwrap_err();
    assert_eq!(e.diagnostic().unwrap().code, code::UNREPRESENTABLE_VALUE);
    let s = Settings::new().emit(Emit::new().line_width(Some(80)));
    let mut w = Document::parse_with("a: 1\n", &s).unwrap();
    let e = w.set_path(&path!["a"], 2).unwrap_err();
    assert_eq!(e.diagnostic().unwrap().code, code::UNSUPPORTED_SETTING);
    let s = Settings::new().limits(Limits::new().max_depth(4));
    let mut deep = Document::parse_with("a: 1\n", &s).unwrap();
    let v: Value = oneyaml::from_str("[[[[[[1]]]]]]").unwrap();
    let e = deep.set_path(&path!["a"], &v).unwrap_err();
    assert_eq!(e.diagnostic().unwrap().code, code::VALUE_DEPTH_EXCEEDED);
    assert_eq!(deep.to_string(), "a: 1\n");
    // Walking into an error region.
    let mut r = Document::parse_recovering("a: [1\n").into_value();
    let e = r.set_path(&path!["a", 0, "k"], 1).unwrap_err();
    assert!(
        matches!(
            e,
            EditError::ErrorNode { .. }
                | EditError::NotAMapping { .. }
                | EditError::ReadOnly { .. }
        ),
        "{e:?}"
    );
    // Every error displays, and names where it happened when it can.
    for e in [
        EditError::StaleNode,
        EditError::WrongDocument,
        EditError::Internal,
    ] {
        assert!(!e.to_string().is_empty());
        assert!(e.at().is_none());
    }
    assert_eq!(d.node(key).unwrap().kind(), NodeKind::Scalar);
    agrees_with_its_text(&d);
}

#[test]
fn reading_nodes_as_types() {
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Cfg {
        port: String,
        n: u16,
    }
    let mut d = doc("port: 8080\nn: 1\n");
    let c: Cfg = d.deserialize().unwrap();
    assert_eq!(
        c,
        Cfg {
            port: "8080".into(),
            n: 1
        }
    );
    d.set_path(&path!["n"], 2).unwrap();
    let c: Cfg = d.deserialize().unwrap();
    assert_eq!(c.n, 2);
    assert!(d.deserialize::<Vec<u8>>().is_err());
}

#[test]
fn removed_nodes_keep_alias_order() {
    // An alias re-inserted before its anchor is refused.
    let mut d = doc("a: &x 1\nlist: [*x]\nfirst: []\n");
    let r = d.remove_path(&path!["list", 0]).unwrap();
    assert_eq!(r.text(), "*x");
    let mut list = d.get_mut("list").unwrap();
    list.insert_removed(r.clone()).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nlist: [*x]\nfirst: []\n");
    let mut d = doc("first: []\na: &x 1\nlist: [*x]\n");
    let r = d.remove_path(&path!["list", 0]).unwrap();
    let e = d
        .get_mut("first")
        .unwrap()
        .insert_removed(r)
        .unwrap_err()
        .into_error();
    assert!(
        matches!(e, EditError::AliasBeforeAnchor { ref anchor, .. } if anchor == "x"),
        "{e:?}"
    );
    assert_eq!(d.to_string(), "first: []\na: &x 1\nlist: []\n");
    // A re-inserted anchor must not capture a later alias.
    let mut d = doc("k: [&y 1]\nfirst: []\nlater: &y 2\nuse: *y\n");
    let r = d.remove_path(&path!["k", 0]).unwrap();
    let e = d
        .get_mut("first")
        .unwrap()
        .insert_removed(r)
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::DuplicateAnchor { .. }), "{e:?}");
    agrees_with_its_text(&d);
}

#[test]
fn get_path_or_create_creates_only_what_is_missing() {
    let mut d = doc("a:\n  b: 1\n");
    let id = d.get_path_or_create(&path!["a", "b"]).unwrap().id();
    assert_eq!(d.node(id).unwrap().text(), "1");
    assert_eq!(d.to_string(), "a:\n  b: 1\n");
    let mut n = d.get_path_or_create(&path!["a", "c", "d"]).unwrap();
    assert_eq!(n.get().unwrap().kind(), NodeKind::Scalar);
    let _ = &mut n;
    assert_eq!(d.to_string(), "a:\n  b: 1\n  c:\n    d: null\n");
    let mut n = d.get_path_or_create(&path!["list", 0]).unwrap();
    n.set("x").unwrap();
    assert_eq!(
        d.to_string(),
        "a:\n  b: 1\n  c:\n    d: null\nlist:\n  - x\n"
    );
    agrees_with_its_text(&d);
    // Nothing is created implicitly: reads and navigation never create.
    assert!(d.get_path_mut(&path!["nope"]).is_err());
    assert!(d.get_mut("nope").is_err());
    assert_eq!(d.to_value().unwrap()["nope"], Value::Null);
    assert_eq!(
        d.to_string(),
        "a:\n  b: 1\n  c:\n    d: null\nlist:\n  - x\n"
    );
}

/// A move that would lose the aliases of several anchors it
/// defines names the first of those anchors in document order, on every
/// run (`check_defines` read a hash set's order: E76Z, a fuzz corpus input
/// named either anchor). Each parse gets new hash seeds, so 40 runs of
/// each order would show any dependence on them.
#[test]
fn alias_before_anchor_names_the_first_offending_anchor() {
    for (src, to, first) in [
        ("e: {p: &a 1, q: &b 2}\nu: *a\nv: *b\nz: 0\n", 3, "a"),
        ("e: {p: &b 1, q: &a 2}\nu: *a\nv: *b\nz: 0\n", 3, "b"),
        (
            "e:\n  p: &c 1\n  q: &a 2\n  r: &b 3\nu: [*b, *a, *c]\nz: 0\n",
            2,
            "c",
        ),
    ] {
        for _ in 0..40 {
            let mut d = doc(src);
            let e = d.root_mut().unwrap().move_entry(0, to).unwrap_err();
            assert!(
                matches!(e, EditError::AliasBeforeAnchor { ref anchor, .. } if anchor == first),
                "{src:?}: {e:?}"
            );
            assert_eq!(d.to_string(), src, "refused: nothing changed");
        }
    }
}
