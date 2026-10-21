//! The `Document` edit API — each operation, each
//! `EditError`, style inference, anchors and aliases, restricted mode,
//! and one interpretation after every edit.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, NodeKind, Settings, Value, path};

fn doc(src: &str) -> Document {
    Document::parse(src).unwrap()
}

#[test]
fn untouched_is_byte_identical() {
    for src in ["a: 1 # c\nb: [x, y]\n", "", "# only\n", "--- |\n  x\n...\n"] {
        let d = Document::parse(src).unwrap();
        assert_eq!(d.to_string().as_bytes(), src.as_bytes());
    }
}

#[test]
fn set_path_replaces_scalars_in_place() {
    let mut d = doc("a: 1 # one\nb:\n  c: x\n");
    d.set_path(&path!["a"], 2).unwrap();
    assert_eq!(d.to_string(), "a: 2 # one\nb:\n  c: x\n");
    d.set_path(&path!["b", "c"], "yes").unwrap();
    assert_eq!(d.to_string(), "a: 2 # one\nb:\n  c: yes\n");
    agrees_with_its_text(&d);
    d.set_path(&path!["b", "c"], vec![1, 2]).unwrap();
    assert_eq!(d.to_string(), "a: 2 # one\nb:\n  c:\n    - 1\n    - 2\n");
    agrees_with_its_text(&d);
    d.set_path(&path!["b"], "flat").unwrap();
    assert_eq!(d.to_string(), "a: 2 # one\nb: flat\n");
    agrees_with_its_text(&d);
}

#[test]
fn insert_and_remove_entries() {
    let mut d = doc("a: 1 # c\nb: 2 # d\n");
    let mut root = d.root_mut().unwrap();
    root.insert("z", "new").unwrap();
    assert_eq!(d.to_string(), "a: 1 # c\nb: 2 # d\nz: new\n");
    agrees_with_its_text(&d);
    let mut root = d.root_mut().unwrap();
    let r = root.remove("b").unwrap();
    assert_eq!(r.text(), "b: 2 # d");
    assert_eq!(r.key(), Some("b"));
    assert_eq!(d.to_string(), "a: 1 # c\nz: new\n");
    agrees_with_its_text(&d);
    let mut root = d.root_mut().unwrap();
    let r = root.remove("a").unwrap();
    assert_eq!(r.kind(), NodeKind::Scalar);
    assert_eq!(d.to_string(), "z: new\n");
    let mut root = d.root_mut().unwrap();
    let _ = root.remove("z").unwrap();
    assert_eq!(d.to_string(), "{}\n");
    agrees_with_its_text(&d);
    let mut root = d.root_mut().unwrap();
    root.insert("k", &Value::Null).unwrap();
    assert_eq!(d.to_string(), "{k: null}\n");
    agrees_with_its_text(&d);
}

#[test]
fn sequences() {
    let mut d = doc("k:\n  - a\n  - b\nf: [1, 2]\n");
    let mut k = d.get_mut("k").unwrap();
    k.push("c").unwrap();
    k.insert_at(0, "z").unwrap();
    assert_eq!(d.to_string(), "k:\n  - z\n  - a\n  - b\n  - c\nf: [1, 2]\n");
    agrees_with_its_text(&d);
    let mut k = d.get_mut("k").unwrap();
    let r = k.remove_at(1).unwrap();
    assert_eq!(r.text(), "a");
    k.replace_at(0, 5).unwrap();
    assert_eq!(d.to_string(), "k:\n  - 5\n  - b\n  - c\nf: [1, 2]\n");
    let mut f = d.get_mut("f").unwrap();
    f.push("x,y").unwrap();
    f.insert_at(0, 0).unwrap();
    assert_eq!(
        d.to_string(),
        "k:\n  - 5\n  - b\n  - c\nf: [0, 1, 2, 'x,y']\n"
    );
    agrees_with_its_text(&d);
    let mut f = d.get_mut("f").unwrap();
    for _ in 0..4 {
        let _ = f.remove_at(0).unwrap();
    }
    assert_eq!(d.to_string(), "k:\n  - 5\n  - b\n  - c\nf: []\n");
    let mut f = d.get_mut("f").unwrap();
    f.push(vec![1]).unwrap();
    assert_eq!(d.to_string(), "k:\n  - 5\n  - b\n  - c\nf: [[1]]\n");
    agrees_with_its_text(&d);
    let e = d.get_mut("f").unwrap().remove_at(3).unwrap_err();
    assert!(
        matches!(
            e,
            EditError::IndexOutOfBounds {
                index: 3,
                len: 1,
                ..
            }
        ),
        "{e:?}"
    );
}

#[test]
fn style_inference_follows_the_neighbours() {
    // Four-space indentation, sequences not indented under their key.
    let mut d = doc("a:\n    b: 1\nlist:\n- x\n");
    d.set_path(&path!["c"], serde_json::json!({"d": [1, 2], "e": {"f": 1}}))
        .unwrap();
    assert_eq!(
        d.to_string(),
        "a:\n    b: 1\nlist:\n- x\nc:\n    d:\n    - 1\n    - 2\n    e:\n        f: 1\n"
    );
    agrees_with_its_text(&d);
    // No context: the settings.
    let mut d = doc("");
    d.set_path(&path!["a", "b"], 1).unwrap();
    assert_eq!(d.to_string(), "a:\n  b: 1\n");
    agrees_with_its_text(&d);
}

#[test]
fn handles_survive_unrelated_edits() {
    let mut d = doc("a: 1\nb: 2\nc: [3]\n");
    let b = d.id_of(&path!["b"]).unwrap();
    let c0 = d.id_of(&path!["c", 0]).unwrap();
    d.set_path(&path!["a"], 10).unwrap();
    d.root_mut().unwrap().insert("d", 4).unwrap();
    assert_eq!(d.node(b).unwrap().text(), "2");
    assert_eq!(d.node(c0).unwrap().text(), "3");
    let _ = d.remove_path(&path!["b"]).unwrap();
    assert_eq!(d.node(b).unwrap_err(), EditError::StaleNode);
    assert_eq!(d.node_mut(b).unwrap_err(), EditError::StaleNode);
    agrees_with_its_text(&d);
}

#[test]
fn set_scalar_keeps_quoting() {
    let mut d = doc("a: 'x'\nb: \"y\"\nc: z\n");
    let a = d.id_of(&path!["a"]).unwrap();
    d.node_mut(a).unwrap().set_scalar("new").unwrap();
    let b = d.id_of(&path!["b"]).unwrap();
    d.node_mut(b).unwrap().set_scalar("n\"ew").unwrap();
    let c = d.id_of(&path!["c"]).unwrap();
    d.node_mut(c).unwrap().set_scalar("true").unwrap();
    assert_eq!(d.to_string(), "a: 'new'\nb: \"n\\\"ew\"\nc: 'true'\n");
    agrees_with_its_text(&d);
}

#[test]
fn settings_are_the_documents() {
    let s = Settings::new().merge_keys(true);
    let mut d = Document::parse_with("base: &b {x: 1}\nuse:\n  <<: *b\n", &s).unwrap();
    d.set_path(&path!["use", "y"], 2).unwrap();
    assert_eq!(d.settings(), &s);
    agrees_with_its_text(&d);
}

#[test]
fn block_scalars() {
    // A block scalar's text ends with its line break: replacing it before
    // another entry keeps the line structure.
    let mut d = doc("a: |\n  x\nb: 1\n");
    d.set_path(&path!["a"], "z").unwrap();
    assert_eq!(d.to_string(), "a: |-\n  z\nb: 1\n");
    agrees_with_its_text(&d);
    let mut d = doc("a: 1\nb: |\n  x\nc: 2\n");
    let r = d.remove_path(&path!["b"]).unwrap();
    assert_eq!(r.text(), "b: |\n  x\n");
    assert_eq!(d.to_string(), "a: 1\nc: 2\n");
    agrees_with_its_text(&d);
    // New multi-line strings are `|` block scalars wherever the context
    // permits; a multi-line string set over an inline scalar stays on its
    // line, double-quoted.
    let mut d = doc("a: 1\nb: 2\n");
    d.set_path(&path!["a"], "l1\nl2").unwrap();
    d.set_path(&path!["c"], vec!["x\ny"]).unwrap();
    assert_eq!(
        d.to_string(),
        "a: \"l1\\nl2\"\nb: 2\nc:\n  - |-\n    x\n    y\n"
    );
    agrees_with_its_text(&d);
    // A block scalar ending its collection is replaced through its line
    // break (`replace_to_line_end`).
    let mut d = doc("a: 1\nb: |\n  x\n");
    d.set_path(&path!["b"], "z").unwrap();
    assert_eq!(d.to_string(), "a: 1\nb: |-\n  z\n");
    agrees_with_its_text(&d);
    d.set_path(&path!["b"], "p\nq\n").unwrap();
    assert_eq!(d.to_string(), "a: 1\nb: |\n  p\n  q\n");
    agrees_with_its_text(&d);
    d.set_path(&path!["c"], "r\ns").unwrap();
    assert_eq!(d.to_string(), "a: 1\nb: |\n  p\n  q\nc: |-\n  r\n  s\n");
    agrees_with_its_text(&d);
    // A same-line comment on the replaced value stays after it.
    let mut d = doc("a: 1 # c\nb: 2\n");
    d.set_path(&path!["a"], "l1\nl2").unwrap();
    assert_eq!(d.to_string(), "a: \"l1\\nl2\" # c\nb: 2\n");
    agrees_with_its_text(&d);
    // The last entry (its comment lies after the mapping), a sequence
    // item, an anchored value, and CRLF: the comment's bytes are kept.
    let mut d = doc("a: 0\nb: 1   #  c\n");
    d.set_path(&path!["b"], "l1\nl2\n").unwrap();
    assert_eq!(d.to_string(), "a: 0\nb: \"l1\\nl2\\n\"   #  c\n");
    agrees_with_its_text(&d);
    let mut d = doc("k:\n  - x # one\n  - y\n");
    d.set_path(&path!["k", 0], "p\nq").unwrap();
    assert_eq!(d.to_string(), "k:\n  - \"p\\nq\" # one\n  - y\n");
    agrees_with_its_text(&d);
    let mut d = doc("a: &x 1 # c\nb: *x\n");
    d.set_path(&path!["a"], "p\nq").unwrap();
    assert_eq!(d.to_string(), "a: &x \"p\\nq\" # c\nb: *x\n");
    agrees_with_its_text(&d);
    let mut d = doc("a: 1 # c\r\nb: 2\r\n");
    d.set_path(&path!["a"], "l1\nl2").unwrap();
    assert_eq!(d.to_string(), "a: \"l1\\nl2\" # c\r\nb: 2\r\n");
    agrees_with_its_text(&d);
    // Setting a one-line value keeps the comment where it was.
    d.set_path(&path!["b"], "z").unwrap();
    assert_eq!(d.to_string(), "a: \"l1\\nl2\" # c\r\nb: z\r\n");
    agrees_with_its_text(&d);
    // A collection replaced by a multi-line string is new content: a block
    // scalar, with the comment on the node's line moved onto its header.
    let mut d = doc("a: 0\nb: [1]   #  c\n");
    d.set_path(&path!["b"], "l1\nl2\n").unwrap();
    assert_eq!(d.to_string(), "a: 0\nb: |   #  c\n  l1\n  l2\n");
    agrees_with_its_text(&d);
    let mut d = doc("k:\n  - [x] # one\n  - y\n");
    d.set_path(&path!["k", 0], "p\nq").unwrap();
    assert_eq!(d.to_string(), "k:\n  - |- # one\n    p\n    q\n  - y\n");
    agrees_with_its_text(&d);
    let mut d = doc("a: &x [1] # c\nb: *x\n");
    d.set_path(&path!["a"], "p\nq").unwrap();
    assert_eq!(d.to_string(), "a: &x |- # c\n  p\n  q\nb: *x\n");
    agrees_with_its_text(&d);
    let mut d = doc("a: [1] # c\r\nb: 2\r\n");
    d.set_path(&path!["a"], "l1\nl2").unwrap();
    assert_eq!(d.to_string(), "a: |- # c\r\n  l1\r\n  l2\r\nb: 2\r\n");
    agrees_with_its_text(&d);
}
