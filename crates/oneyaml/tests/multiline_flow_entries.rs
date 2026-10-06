//! The column of a flow entry of several lines, refusals in recovered input,
//! and `Debug` of documents and streams. The timing shapes are in
//! `document_scaling.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

use std::fmt::Write as _;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, Stream, path};

/// Near its line's start, a flow entry of several lines keeps
/// its real column (the layout it had relative to its first line, as
/// before); far along a long line, its column comes from its own
/// continuation lines (just past the least indented one), so moving it
/// into a block collection costs O(the entry) and reads back.
#[test]
fn flow_entries_of_several_lines_move_into_block_context() {
    // Near: `a` starts at column 7, its continuation line at 9: moved
    // into `blk` (entries at column 2) its continuation goes to column 4.
    let src = "blk:\n  x: 1\nflow: {a: [1,\n         2]}\n";
    let mut d = Document::parse(src).unwrap();
    let r = d.get_mut("flow").unwrap().remove("a").unwrap();
    d.get_mut("blk").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.to_string(), "blk:\n  x: 1\n  a: [1,\n    2]\nflow: {}\n");
    agrees_with_its_text(&d);
    // Far: the same entry after a line of 5,000 entries.
    let mut src = String::from("blk:\n  x: 1\nflow: {");
    for i in 0..5_000 {
        write!(src, "k{i}: v, ").unwrap();
    }
    src.push_str("a: [1,\n   2]}\n");
    let mut d = Document::parse(src.as_str()).unwrap();
    let r = d.get_mut("flow").unwrap().remove("a").unwrap();
    d.get_mut("blk").unwrap().insert_removed(r).unwrap();
    assert!(
        d.to_string()
            .starts_with("blk:\n  x: 1\n  a: [1,\n   2]\nflow: {")
    );
    assert_eq!(d.get_path(&path!["blk", "a", 1]).unwrap().text(), "2");
    agrees_with_its_text(&d);
    // A sequence item too.
    let mut src = String::from("blk:\n  - x\nflow: [");
    for i in 0..5_000 {
        write!(src, "v{i}, ").unwrap();
    }
    src.push_str("[1,\n   2]]\n");
    let mut d = Document::parse(src.as_str()).unwrap();
    let r = d.get_mut("flow").unwrap().remove_at(5_000).unwrap();
    d.get_mut("blk").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.get_path(&path!["blk", 1, 1]).unwrap().text(), "2");
    agrees_with_its_text(&d);
}

/// Refusals in recovered input are the restricted-mode ones,
/// found before any entry is located; nothing changes.
#[test]
fn refusals_in_recovered_input() {
    let src = "a: 1\nb: 2\nbad: x: y\n";
    let mut d = Document::parse_recovering(src).into_value();
    let e = d.root_mut().unwrap().insert("new", 1).unwrap_err();
    assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
    let e = d.root_mut().unwrap().remove("a").unwrap_err();
    assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
    assert_eq!(d.to_string(), src);
    // A key is still refused as a key (read from the index of a wide
    // mapping, or its neighbours).
    let mut wide = String::new();
    for i in 0..100 {
        writeln!(wide, "k{i}: v").unwrap();
    }
    let mut d = Document::parse(wide.as_str()).unwrap();
    let k = d
        .root()
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .nth(50)
        .unwrap()
        .0
        .id();
    let e = d.remove_node(k).unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    let e = d.remove_keep_comments(k).unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    // An input whose only content is an error region: no root goes in.
    for src in ["]\n", "# c\n]\n"] {
        let mut d = Document::parse_recovering(src).into_value();
        let e = d.set_root(1).unwrap_err();
        assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
        let e = d.set_path(&path!["k"], 1).unwrap_err();
        assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
        assert_eq!(d.to_string(), src);
    }
    // An empty input still gets one.
    let mut d = Document::parse("# c\n").unwrap();
    d.set_root(1).unwrap();
    assert_eq!(d.to_value().unwrap()["k"], oneyaml::Value::Null);
    assert_eq!(d.root().unwrap().text(), "1");
}

/// `Debug` shows what a document or stream is, never its caches
/// (lookup indexes, values): the same for two documents of the same input
/// and edits, and short whatever the input.
#[test]
fn debug_is_deterministic_and_bounded() {
    let mut src = String::new();
    for i in 0..80 {
        writeln!(src, "k{i}: v{i}").unwrap();
    }
    let shown = |warm: bool| {
        let mut d = Document::parse(src.as_str()).unwrap();
        if warm {
            assert_eq!(d.check_lookup_indexes(), Ok(1));
            assert_eq!(d.to_value().unwrap()["k3"].as_str(), Some("v3"));
        }
        d.set_path(&path!["k0"], 1).unwrap();
        format!("{d:?}")
    };
    let (a, b) = (shown(true), shown(false));
    assert_eq!(a, b);
    assert!(a.len() < 2_000, "{}", a.len());
    assert!(a.contains("edited: true"), "{a}");
    assert!(!a.contains("Treap") && !a.contains("lookups"), "{a}");
    let big = "- v\n".repeat(100_000);
    let d = Document::parse(big.as_str()).unwrap();
    assert!(d.get_path(&path![99_999]).is_some());
    assert!(format!("{d:?}").len() < 2_000);
    let mut s = Stream::parse("a: 1\n---\nb: 2\n").unwrap();
    let one = format!("{s:?}");
    assert!(one.starts_with("Stream { documents: 2"), "{one}");
    let mut m = s.document_mut(1).unwrap();
    m.set_path(&path!["b"], 3).unwrap();
    let shown = format!("{m:?}");
    assert!(
        shown.contains("index: Some(1)") && shown.len() < 2_000,
        "{shown}"
    );
    assert!(format!("{s:?}").contains("edited: true"));
    // The same as another stream with the same edit.
    let mut t = Stream::parse("a: 1\n---\nb: 2\n").unwrap();
    t.document_mut(1).unwrap().set_path(&path!["b"], 3).unwrap();
    assert_eq!(format!("{s:?}"), format!("{t:?}"));
    assert_eq!(
        format!("{:?}", s.document(0).unwrap()),
        format!("{:?}", t.document(0).unwrap())
    );
}
