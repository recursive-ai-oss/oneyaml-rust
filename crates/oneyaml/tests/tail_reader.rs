//! Re-setting the tail reader (`Document`'s `TailGuard`: the
//! kept block scalar that reads an unterminated final line of spaces) to
//! the value it has keeps it as written (it was written
//! double-quoted), and the constant-factor rewrites of node edits keep
//! every outcome (pinned here on the shapes they changed: big entries
//! removed, replaced and collapsed with comments, explicit owners and
//! anchors inside).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::fmt::Write as _;

use oneyaml::{CommentOwner, Document, Value, path};

fn value_at(d: &Document, p: &oneyaml::Path) -> Value {
    d.get_path(p).unwrap().deserialize::<Value>().unwrap()
}

/// The tail reader set to its own value: the text is unchanged (style,
/// indentation indicator, properties, header comment), and the document
/// reads the same value back. Setting any node to its own
/// value is a no-op, so the node and its handle stay (it once was a
/// replacement, a new node).
#[test]
fn re_setting_the_tail_reader_keeps_its_style() {
    let cases: [(&str, oneyaml::Path); 11] = [
        ("k: |+\n  x\n ", path!["k"]),
        ("k: |+\n  x\n\n ", path!["k"]),
        ("a: 1\nk: |+\n  x\n\n  ", path!["k"]),
        ("- |+\n  x\n\n ", path![0]),
        ("k: |2+\n  x\n ", path!["k"]),
        ("k: &a |+\n  x\n ", path!["k"]),
        ("k: !t |+\n  x\n ", path!["k"]),
        ("k: &a !t |+ # c\n  x\n ", path!["k"]),
        ("k: |+ # c\n  x\n\n  # d\n ", path!["k"]),
        ("k: |+\n\n ", path!["k"]),
        ("k: >+\n  x\n ", path!["k"]),
    ];
    for (src, p) in cases {
        let mut d = Document::parse(src).unwrap();
        let v = value_at(&d, &p);
        let old = d.id_of(&p).unwrap();
        let new = d.node_mut(old).unwrap().set(&v).unwrap().id();
        assert_eq!(d.to_string().as_bytes(), src.as_bytes(), "{src:?}");
        assert_eq!(new, old, "{src:?}");
        assert!(d.node(old).is_ok(), "{src:?}");
        assert_eq!(value_at(&d, &p), v, "{src:?}");
        let again = Document::parse(d.to_string()).unwrap();
        assert_eq!(value_at(&again, &p), v, "{src:?}");
        // Through `set_path` too.
        let mut d = Document::parse(src).unwrap();
        d.set_path(&p, &v).unwrap();
        assert_eq!(d.to_string().as_bytes(), src.as_bytes(), "{src:?}");
    }
}

/// Only its own value keeps it: another value, or the same value on a
/// scalar that does not read the final space line, is written as before
/// (style inference), and every value reads back.
#[test]
fn other_values_are_written_as_before() {
    let mut d = Document::parse("k: |+\n  x\n ").unwrap();
    d.set_path(&path!["k"], "y\n\n").unwrap();
    assert_eq!(d.to_string(), "k: \"y\\n\\n\"\n ");
    assert_eq!(value_at(&d, &path!["k"]), Value::from("y\n\n"));
    // No final space line: no tail reader; the value it has (a
    // byte no-op, the `|+` stays; it once wrote the inferred
    // `|`), and another value in the inferred block style.
    let mut d = Document::parse("k: |+\n  x\n").unwrap();
    d.set_path(&path!["k"], "x\n").unwrap();
    assert_eq!(d.to_string(), "k: |+\n  x\n");
    d.set_path(&path!["k"], "y\n").unwrap();
    assert_eq!(d.to_string(), "k: |\n  y\n");
    // A kept scalar that is not the last: not the reader.
    let mut d = Document::parse("k: |+\n  x\n\nz: 1\n ").unwrap();
    let v = value_at(&d, &path!["k"]);
    d.set_path(&path!["k"], &v).unwrap();
    assert_eq!(value_at(&d, &path!["k"]), v);
    // The value with its tag dropped is another value.
    let mut d = Document::parse("k: !t |+\n  x\n ").unwrap();
    d.set_path(&path!["k"], "x\n\n").unwrap();
    assert_eq!(value_at(&d, &path!["k"]), Value::from("x\n\n"));
    assert!(!d.to_string().contains("!t"));
}

fn big(n: usize) -> String {
    let mut s = String::from("# head\n\na: 0\nbig:\n");
    for i in 0..n {
        match i % 5 {
            0 => writeln!(s, "  # lead {i}\n  k{i}: v{i} # t{i}").unwrap(),
            1 => writeln!(s, "  k{i}: &x{i} {{p: [q, {i}]}}").unwrap(),
            2 => writeln!(s, "  k{i}:\n    - {i}\n    # foot {i}").unwrap(),
            3 => writeln!(s, "  k{i}: |\n    line {i}\n").unwrap(),
            _ => writeln!(s, "  k{i}: value {i}").unwrap(),
        }
    }
    s.push_str("  # big footer\nonly:\n  inner:\n");
    for i in 0..n {
        writeln!(s, "    o{i}: {i} # o{i}").unwrap();
    }
    s.push_str("z: 1\n");
    s
}

/// Big entries with comments inside: removed (their comments go with
/// them, and come back at the same place), replaced, and collapsed; an
/// explicit owner inside the removed entry follows it back.
#[test]
fn big_entries_carry_their_comments() {
    let src = big(400);
    let comments = |d: &Document| d.explain_comments().unwrap().len();
    // Removed and re-inserted: the original bytes.
    let mut d = Document::parse(src.as_str()).unwrap();
    let all = comments(&d);
    let r = d.remove_node(d.id_of(&path!["big"]).unwrap()).unwrap();
    // (80 leading and 80 trailing comments, 80 inner footers, its footer.)
    assert_eq!(r.comments().len(), 80 * 3 + 1);
    assert_eq!(comments(&d) + r.comments().len(), all);
    d.root_mut().unwrap().insert_removed_at(1, r).unwrap();
    assert_eq!(d.to_string(), src);
    // Replaced: the old value's comments go with it.
    let mut d = Document::parse(src.as_str()).unwrap();
    d.set_path(&path!["big"], 1).unwrap();
    // (In the old value's slot, on a line of its own.)
    assert!(
        d.to_string()
            .starts_with("# head\n\na: 0\nbig:\n  1\nonly:\n")
    );
    assert_eq!(comments(&d), 1 + 400);
    // The only entry's collection collapses to `{}`: its comments go.
    let mut d = Document::parse(src.as_str()).unwrap();
    let r = d
        .remove_node(d.id_of(&path!["only", "inner"]).unwrap())
        .unwrap();
    assert_eq!(r.comments().len(), 400);
    assert!(d.to_string().contains("only: {}\nz: 1\n"));
    // An explicit owner inside the entry, of a comment outside it.
    let mut d = Document::parse(src.as_str()).unwrap();
    let head = d.comment_ids()[0];
    let k5 = d.id_of(&path!["big", "k5"]).unwrap();
    d.attach_comment(head, CommentOwner::Trailing(k5)).unwrap();
    let r = d.remove_node(d.id_of(&path!["big"]).unwrap()).unwrap();
    d.root_mut().unwrap().insert_removed_at(1, r).unwrap();
    assert_eq!(d.to_string(), src);
    let k5 = d.id_of(&path!["big", "k5"]).unwrap();
    let owned = d.comments(k5).unwrap();
    assert!(
        owned.trailing().iter().any(|c| c.is_explicit()),
        "{owned:?}"
    );
}
