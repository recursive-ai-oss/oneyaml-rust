//! `Document` edit API regression fixes. Each test
//! failed on the first edit API.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, Limits, Settings, Stream, Value, path};

fn doc(src: &str) -> Document {
    Document::parse(src).unwrap()
}

/// Anchors and aliases are edited by token, never
/// by text search: removing an anchor never touches text that merely
/// looks like it.
#[test]
fn anchor_removal_is_by_token() {
    let mut d = doc("k: &a\n  s: 'x &a y'\n");
    let k = d.id_of(&path!["k"]).unwrap();
    d.set_anchor(k, None).unwrap();
    assert_eq!(d.to_string(), "k:\n  s: 'x &a y'\n");
    assert_eq!(d.to_value().unwrap()["k"]["s"].as_str(), Some("x &a y"));
    agrees_with_its_text(&d);
    let mut d = doc("a: &a 'x &a y' # &a\nb: [&a q, '&a']\n");
    let a = d.id_of(&path!["a"]).unwrap();
    let a = d.set_anchor(a, None).unwrap();
    assert_eq!(d.to_string(), "a: 'x &a y' # &a\nb: [&a q, '&a']\n");
    assert_eq!(d.node(a).unwrap().anchor(), None);
    let q = d.id_of(&path!["b", 0]).unwrap();
    d.set_anchor(q, None).unwrap();
    assert_eq!(d.to_string(), "a: 'x &a y' # &a\nb: [q, '&a']\n");
    agrees_with_its_text(&d);
    // A collection keeps its handle when its anchor goes.
    let mut d = doc("m: &m {a: 1}\n");
    let m = d.id_of(&path!["m"]).unwrap();
    assert_eq!(d.set_anchor(m, None).unwrap(), m);
    assert_eq!(d.to_string(), "m: {a: 1}\n");
    agrees_with_its_text(&d);
}

/// Section 5.4: CR-only documents keep
/// their structure when entries move.
#[test]
fn cr_line_breaks_on_the_edit_path() {
    let mut d = doc("b:\r  c: 1\rx:\r  y: 1\r");
    d.root_mut().unwrap().move_entry(0, 1).unwrap();
    assert_eq!(d.to_string(), "x:\r  y: 1\rb:\r  c: 1\r");
    assert_eq!(d.to_value().unwrap()["b"]["c"].as_u64(), Some(1));
    assert_eq!(d.to_value().unwrap()["x"]["y"].as_u64(), Some(1));
    agrees_with_its_text(&d);
    let mut d = doc("top:\r\n  b:\r\n    c: 1\r\n  x: 2\r\nlist:\r\n  - 0\r\n");
    let r = d.remove_path(&path!["top", "b"]).unwrap();
    d.get_mut("top").unwrap().insert_removed(r).unwrap();
    assert_eq!(
        d.to_string(),
        "top:\r\n  x: 2\r\n  b:\r\n    c: 1\r\nlist:\r\n  - 0\r\n"
    );
    agrees_with_its_text(&d);
}

/// New text uses the document's own
/// line break.
#[test]
fn new_text_uses_the_documents_line_break() {
    for nl in ["\r\n", "\r"] {
        let src = format!("a: 1{nl}b: 2{nl}");
        let mut d = doc(&src);
        d.set_path(&path!["c"], 3).unwrap();
        d.set_path(&path!["d", "e"], vec![1, 2]).unwrap();
        d.set_path(&path!["a"], "l1\nl2").unwrap();
        d.set_path(&path!["f"], "m1\nm2").unwrap();
        d.get_mut("b")
            .unwrap()
            .set(serde_json::json!({"k": [true]}))
            .unwrap();
        let want = format!(
            "a: \"l1\\nl2\"{nl}b:{nl}  k:{nl}    - true{nl}c: 3{nl}d:{nl}  e:{nl}    - 1{nl}    - 2{nl}f: |-{nl}  m1{nl}  m2{nl}"
        );
        assert_eq!(d.to_string(), want, "{nl:?}");
        assert!(!d.to_string().replace(nl, "").contains(['\n', '\r']));
        agrees_with_its_text(&d);
    }
}

/// The nearest preceding anchor of that name is
/// the intended target; `Removed` records alias targets: aliases never
/// silently change their anchor.
#[test]
fn aliases_refer_to_their_nearest_preceding_anchor() {
    let mut d = doc("a: &x 1\nb: &x 2\nc: []\n");
    let a = d.id_of(&path!["a"]).unwrap();
    let b = d.id_of(&path!["b"]).unwrap();
    let mut c = d.get_mut("c").unwrap();
    let e = c.push_alias(a).unwrap_err();
    assert!(
        matches!(e, EditError::AliasBeforeAnchor { ref anchor, .. } if anchor == "x"),
        "{e:?}"
    );
    c.push_alias(b).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nb: &x 2\nc: [*x]\n");
    assert_eq!(d.to_value().unwrap()["c"][0].as_u64(), Some(2));
    agrees_with_its_text(&d);
    // Moving an alias past a redefinition of its anchor's name.
    let mut d = doc("a: &x 1\nl:\n  - *x\n  - &x 2\n  - 3\n");
    let before = d.to_string();
    let e = d.get_mut("l").unwrap().move_entry(0, 2).unwrap_err();
    assert!(matches!(e, EditError::AliasBeforeAnchor { .. }), "{e:?}");
    assert_eq!(d.to_string(), before);
    // A removed alias records its target, not just its name.
    let mut d = doc("a: &x 1\nl: [*x]\nm: &x 2\nn: []\n");
    let r = d.remove_path(&path!["l", 0]).unwrap();
    let e = d
        .get_mut("n")
        .unwrap()
        .insert_removed(r.clone())
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::AliasBeforeAnchor { .. }), "{e:?}");
    d.get_mut("l").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nl: [*x]\nm: &x 2\nn: []\n");
    assert_eq!(d.to_value().unwrap()["l"][0].as_u64(), Some(1));
    // A moved anchor takes its aliases along (an exact check).
    let mut d = doc("- &x 1\n- 2\n- *x\n");
    d.root_mut().unwrap().move_entry(0, 1).unwrap();
    assert_eq!(d.to_string(), "- 2\n- &x 1\n- *x\n");
    assert_eq!(d.to_value().unwrap()[2].as_u64(), Some(1));
    agrees_with_its_text(&d);
}

/// The comments of the removed range travel with `Removed` and come back with
/// it.
#[test]
fn comments_travel_with_removed() {
    let mut d = doc("a: 1\n# about b\nb: 2\n");
    let r = d.remove_path(&path!["b"]).unwrap();
    assert_eq!(r.comments(), ["# about b"]);
    assert_eq!(d.to_string(), "a: 1\n");
    d.root_mut().unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "a: 1\n# about b\nb: 2\n");
    agrees_with_its_text(&d);
    // Comment rule 4: the comment above `b` is `b`'s leading
    // comment, so removing `a` leaves it with `b` (before, the removed
    // range of the first entry took it along).
    let mut d = doc("a: 1\n# about b\nb: 2\n");
    let r = d.remove_path(&path!["a"]).unwrap();
    assert!(r.comments().is_empty());
    assert_eq!(d.to_string(), "# about b\nb: 2\n");
    d.root_mut().unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "# about b\nb: 2\na: 1\n");
    agrees_with_its_text(&d);
    // Flow: the comment inside the removed range is kept.
    let mut d = doc("f: [a, # note\n  b]\n");
    let r = d.remove_path(&path!["f", 0]).unwrap();
    assert_eq!(r.comments(), ["# note"]);
    assert_eq!(d.to_string(), "f: [b]\n");
    // Moves carry them (comment rule 4): each
    // comment line leads the item below it and moves with it (0.2 left
    // the first item's leading comment at the top).
    let mut d = doc("k:\n  # one\n  - 1 # uno\n  # two\n  - 2\n");
    d.get_mut("k").unwrap().move_entry(1, 0).unwrap();
    assert_eq!(d.to_string(), "k:\n  # two\n  - 2\n  # one\n  - 1 # uno\n");
    agrees_with_its_text(&d);
}

/// Cloning never makes handles stale on
/// either copy; a stream is edited through the stream.
#[test]
fn clones_and_streams_keep_handles() {
    let mut d = doc("a: 1\nb: 2\nc: [3]\n");
    let a = d.id_of(&path!["a"]).unwrap();
    let c0 = d.id_of(&path!["c", 0]).unwrap();
    let mut copy = d.clone();
    copy.set_path(&path!["b"], 20).unwrap();
    d.set_path(&path!["b"], 200).unwrap();
    for x in [&d, &copy] {
        assert_eq!(x.node(a).unwrap().text(), "1");
        assert_eq!(x.node(c0).unwrap().text(), "3");
    }
    copy.node_mut(a).unwrap().set(10).unwrap();
    assert_eq!(copy.to_string(), "a: 10\nb: 20\nc: [3]\n");
    assert_eq!(d.to_string(), "a: 1\nb: 200\nc: [3]\n");
    assert_eq!(copy.node(a).unwrap_err(), EditError::StaleNode);
    assert_eq!(d.node(a).unwrap().text(), "1");
    // A handle made in one copy after they diverged names nothing in the
    // other.
    let new = copy.id_of(&path!["a"]).unwrap();
    assert_eq!(d.node(new).unwrap_err(), EditError::StaleNode);
    // Clones of clones.
    let mut third = copy.clone();
    third.set_path(&path!["b"], 30).unwrap();
    assert_eq!(third.node(c0).unwrap().text(), "3");
    assert_eq!(third.node(new).unwrap().text(), "10");

    let mut s = Stream::parse("a: &x 1\n---\nb: 2\nc: &x 3\n").unwrap();
    let snapshot = s.document(1).unwrap();
    let c = snapshot.id_of(&path!["c"]).unwrap();
    let b = snapshot.id_of(&path!["b"]).unwrap();
    let mut second = s.document_mut(1).unwrap();
    second.node_mut(b).unwrap().set(4).unwrap();
    assert_eq!(second.node(c).unwrap().text(), "&x 3");
    // Anchors are per document.
    let x = second.id_of(&path!["b"]).unwrap();
    second.node_mut(x).unwrap().set_anchor(Some("y")).unwrap();
    assert_eq!(s.to_string(), "a: &x 1\n---\nb: &y 4\nc: &x 3\n");
    assert_eq!(snapshot.to_string(), "---\nb: 2\nc: &x 3\n"); // its region
    assert_eq!(
        s.document(1).unwrap().to_value().unwrap()["b"].as_u64(),
        Some(4)
    );
    let mut first = s.document_mut(0).unwrap();
    first.set_path(&path!["z"], [1]).unwrap();
    assert_eq!(s.to_string(), "a: &x 1\nz:\n  - 1\n---\nb: &y 4\nc: &x 3\n");
    assert!(!s.has_errors());
    assert!(s.document_mut(2).is_none());
}

/// New multi-line strings are `|` block
/// scalars wherever the context permits (GHA `run: |` steps).
#[test]
fn block_layout_and_block_scalars() {
    let mut d = doc("a: 1\n");
    d.set_path(
        &path!["cfg"],
        serde_json::json!({"list": [1, 2], "note": "a\nb"}),
    )
    .unwrap();
    assert_eq!(
        d.to_string(),
        "a: 1\ncfg:\n  list:\n    - 1\n    - 2\n  note: |-\n    a\n    b\n"
    );
    agrees_with_its_text(&d);
    let src =
        "jobs:\n  build:\n    steps:\n      - name: test\n        run: |\n          cargo test\n";
    let mut d = doc(src);
    d.set_path(
        &path!["jobs", "build", "steps", 0, "run"],
        "cargo build\ncargo test\n",
    )
    .unwrap();
    assert_eq!(
        d.to_string(),
        "jobs:\n  build:\n    steps:\n      - name: test\n        run: |\n          cargo build\n          cargo test\n"
    );
    agrees_with_its_text(&d);
    let mut steps = d.get_path_mut(&path!["jobs", "build", "steps"]).unwrap();
    steps
        .push(serde_json::json!({"name": "lint", "run": "cargo clippy\n"}))
        .unwrap();
    assert_eq!(
        d.to_string(),
        "jobs:\n  build:\n    steps:\n      - name: test\n        run: |\n          cargo build\n          cargo test\n      - name: lint\n        run: |\n          cargo clippy\n"
    );
    agrees_with_its_text(&d);
}

/// Limit refusals carry the edit's span, not `0..0`.
#[test]
fn limit_diagnostics_point_at_the_edit() {
    let s = Settings::new().limits(Limits::default().max_document_size(24));
    let mut d = Document::parse_with("a: 1\nb: 2\n", &s).unwrap();
    let e = d
        .set_path(&path!["b"], "a long value that is too long")
        .unwrap_err();
    let diag = e.diagnostic().unwrap();
    assert_eq!(diag.code, oneyaml::code::DOCUMENT_TOO_LARGE, "{diag:?}");
    assert_eq!(diag.primary.span, oneyaml::Span::new(7, 9), "{diag:?}");
    assert_eq!(d.to_string(), "a: 1\nb: 2\n");
    let _ = Value::Null;
}

/// Adding entries to a single-pair flow mapping must work (it becomes a flow
/// mapping).
#[test]
fn single_pair_flow_mappings_take_entries() {
    let mut d = doc("l: [a: 1, x]\n");
    let pair = d.id_of(&path!["l", 0]).unwrap();
    let mut p = d.node_mut(pair).unwrap();
    let c = p.insert("c", 2).unwrap().id();
    assert_eq!(d.to_string(), "l: [{a: 1, c: 2}, x]\n");
    assert_eq!(d.node(c).unwrap().text(), "2");
    assert_eq!(d.to_value().unwrap()["l"][0]["c"].as_u64(), Some(2));
    agrees_with_its_text(&d);
    let mut d = doc("l: [a: 1]\n");
    let pair = d.id_of(&path!["l", 0]).unwrap();
    let r = d.node_mut(pair).unwrap().remove("a").unwrap();
    assert_eq!(r.key(), Some("a"));
    assert_eq!(d.to_string(), "l: [{}]\n");
    agrees_with_its_text(&d);
}
