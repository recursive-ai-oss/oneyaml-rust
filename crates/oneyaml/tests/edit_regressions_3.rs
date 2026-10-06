//! `Document` edit regression fixes. Each test names its
//! case; the scaling shapes of some are in
//! `document_scaling.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, Settings, Stream, Value, path};

/// Every document of `s`, and the stream, report exactly the diagnostics
/// a recovering parse of the edited text reports.
fn same_as_reparse(s: &Stream) {
    let text = s.to_string();
    let again = Stream::parse_recovering_with(text.clone(), s.settings()).into_value();
    assert_eq!(s.diagnostics(), again.diagnostics(), "{text:?}");
    assert_eq!(s.has_errors(), again.has_errors(), "{text:?}");
    assert_eq!(s.len(), again.len());
    for i in 0..s.len() {
        let (a, b) = (s.document(i).unwrap(), again.document(i).unwrap());
        assert_eq!(a.diagnostics(), b.diagnostics(), "document {i} of {text:?}");
        assert_eq!(a.has_errors(), b.has_errors(), "document {i} of {text:?}");
    }
}

// ------------------------------- per document diagnostics agree with a reparse

/// A document's diagnostics after edits come from its own region and its
/// own meaning check, and agree with a re-parse — including a recovering
/// stream whose meaning checks stop at its first syntax error (a meaning
/// error after it is not reported, as parsing does not report it).
#[test]
fn per_document_diagnostics_agree_with_a_reparse() {
    let src = "a: 1\n---\nb: *nope\n---\nc: [1]]\n---\nd: *gone\ne: 2\n";
    let p = Stream::parse_recovering(src);
    assert!(p.has_errors());
    let mut s = p.into_value();
    same_as_reparse(&s);
    s.document_mut(0)
        .unwrap()
        .set_path(&path!["z"], "new")
        .unwrap();
    same_as_reparse(&s);
    // The last document lies past the syntax error: its undefined alias
    // is not reported, before or after an edit of it.
    let mut last = s.document_mut(3).unwrap();
    assert!(!last.has_errors());
    last.set_path(&path!["e"], 3).unwrap();
    assert!(!last.has_errors());
    same_as_reparse(&s);
    assert_eq!(
        s.to_string(),
        "a: 1\nz: new\n---\nb: *nope\n---\nc: [1]]\n---\nd: *gone\ne: 3\n"
    );
    // The second document's error moved with the edit before it.
    let d = s.document(1).unwrap().diagnostics();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].primary.span.start, "a: 1\nz: new\n---\nb: ".len());
}

/// In a strict stream every document is checked
/// on its own, so each edited document reports its own first meaning
/// error; the stream lists them in order, the first being the error
/// `Stream::parse` of the edited text fails with. Before, only the
/// stream's first error was kept, and the second document looked clean.
#[test]
fn strict_stream_documents_report_their_own_errors() {
    let settings = Settings::new().merge_keys(true);
    let src = "base: &b {x: 1}\nuse:\n  <<: *b\n---\nbase: &b {x: 1}\nuse:\n  <<: *b\n";
    let mut s = Stream::parse_with(src, &settings).unwrap();
    for i in 0..2 {
        let mut d = s.document_mut(i).unwrap();
        d.set_path(&path!["base"], 3).unwrap();
        assert!(d.has_errors(), "document {i}");
    }
    for i in 0..2 {
        let d = s.document(i).unwrap();
        assert!(d.has_errors(), "document {i}");
        let errors: Vec<_> = d
            .diagnostics()
            .into_iter()
            .filter(|d| d.severity == oneyaml::Severity::Error)
            .collect();
        assert_eq!(errors.len(), 1, "document {i}");
    }
    let all = s.diagnostics();
    assert_eq!(all.len(), 2);
    let e = Stream::parse_with(s.to_string(), &settings).unwrap_err();
    assert_eq!(all[0].code, e.code());
    assert_eq!(all[0].primary.span, e.diagnostic().primary.span);
    // A recovering parse of the text reports the same, per document.
    same_as_reparse(&s);
}

/// Restricted mode per document: a document's guard is found from its own
/// region and kept when the stream switches to another document and back.
#[test]
fn guards_are_per_document_and_survive_switches() {
    let src = "a: 1\n---\nx: 1\ny: *nope\nz: 2\n";
    let mut s = Stream::parse_recovering(src).into_value();
    for round in 0..3 {
        let mut second = s.document_mut(1).unwrap();
        let e = second.set_path(&path!["y"], 5).unwrap_err();
        assert!(matches!(e, EditError::ReadOnly { .. }), "{round}: {e:?}");
        second.set_path(&path!["z"], round).unwrap();
        let mut first = s.document_mut(0).unwrap();
        first.set_path(&path!["a"], round).unwrap();
    }
    assert_eq!(s.to_string(), "a: 2\n---\nx: 1\ny: *nope\nz: 2\n");
    same_as_reparse(&s);
}

// ------------------------------------------------- document spans follow edits

/// Document spans are read from the document nodes (not a list rebuilt
/// per edit): after edits that grow and shrink documents, every
/// document's span, text and diagnostics are those of a re-parse.
#[test]
fn document_spans_follow_edits() {
    let src = "a: 1\n---\nb: [x]]\n---\nc: 3\n";
    let mut s = Stream::parse_recovering(src).into_value();
    let mut first = s.document_mut(0).unwrap();
    first.set_path(&path!["long"], "y".repeat(50)).unwrap();
    let mut third = s.document_mut(2).unwrap();
    third.set_path(&path!["c"], vec![1, 2, 3]).unwrap();
    let text = s.to_string();
    let again = Stream::parse_recovering(text.clone()).into_value();
    for i in 0..3 {
        let (a, b) = (s.document(i).unwrap(), again.document(i).unwrap());
        assert_eq!(a.span(), b.span(), "{i}");
        assert_eq!(a.to_string(), b.to_string(), "{i}");
        assert_eq!(a.to_string(), &text[a.span().start..a.span().end]);
    }
    same_as_reparse(&s);
    // The first document of an empty input is added to the list.
    let mut d = Document::parse("").unwrap();
    d.set_root(vec![1]).unwrap();
    assert_eq!(d.to_string(), "- 1\n");
    assert!(d.diagnostics().is_empty());
    agrees_with_its_text(&d);
}

// ------------------------------------------------ append after a folded scalar

/// Appending after a folded block scalar and removing the item again
/// (the shape the review measured) is exact; its cost is in
/// `document_scaling.rs`.
#[test]
fn append_after_a_folded_scalar() {
    let src = "s:\n  - v0\n  - v1\n  - >-\n    folded\n";
    let mut d = Document::parse(src).unwrap();
    let id = d.get_mut("s").unwrap().push("x").unwrap().id();
    assert_eq!(
        d.to_string(),
        "s:\n  - v0\n  - v1\n  - >-\n    folded\n  - x\n"
    );
    agrees_with_its_text(&d);
    let _ = d.remove_node(id).unwrap();
    assert_eq!(d.to_string(), src);
    agrees_with_its_text(&d);
}

// ------------------------------------------------ values of bare explicit keys

fn path_of(keys: &[&str]) -> oneyaml::Path {
    let mut p = oneyaml::Path::new();
    for k in keys {
        p.push_key(*k);
    }
    p
}

/// Setting the value of a bare `? key` entry
/// (no `:`) must work: `? key\n: value` in block context, `key: value` in
/// flow context. Before: `Unsupported` ("a value in this position") in
/// block context, `NotWellFormed` in flow context.
#[test]
fn values_of_bare_explicit_keys() {
    let cases: &[(&str, &[&str], Value, &str)] = &[
        ("? a\n? b\n", &["a"], Value::from(1), "? a\n: 1\n? b\n"),
        ("? a\n? b\n", &["b"], Value::from(1), "? a\n? b\n: 1\n"),
        ("? a\n", &["a"], Value::from("x"), "? a\n: x\n"),
        (
            "? a # c\n? b\n",
            &["a"],
            Value::from(1),
            "? a # c\n: 1\n? b\n",
        ),
        (
            "k:\n  ? a\n  ? b\n",
            &["k", "a"],
            Value::from(1),
            "k:\n  ? a\n  : 1\n  ? b\n",
        ),
        (
            "? a\n? b\n",
            &["a"],
            Value::from("l1\nl2\n"),
            "? a\n: |\n  l1\n  l2\n? b\n",
        ),
        (
            "? |\n  t\n? b\n",
            &["t\n"],
            Value::from(1),
            "? |\n  t\n: 1\n? b\n",
        ),
        ("{a, b}\n", &["a"], Value::from(1), "{a: 1, b}\n"),
        ("{a, b}\n", &["b"], Value::from(1), "{a, b: 1}\n"),
        ("{? a}\n", &["a"], Value::from(1), "{? a: 1}\n"),
        ("[? a]\n", &[], Value::from(1), "[? a: 1]\n"),
    ];
    for (src, at, v, want) in cases {
        let mut d = Document::parse(*src).unwrap();
        let id = if at.is_empty() {
            // The pair's value.
            let seq = d.root().unwrap().as_sequence().unwrap();
            seq.get(0)
                .unwrap()
                .as_mapping()
                .unwrap()
                .iter()
                .next()
                .unwrap()
                .1
                .id()
        } else {
            d.id_of(path_of(at)).unwrap()
        };
        let new = d
            .node_mut(id)
            .unwrap()
            .set(v)
            .unwrap_or_else(|e| panic!("{src:?}: {e:?}"))
            .id();
        assert_eq!(d.to_string(), *want, "{src:?}");
        assert!(d.node(new).is_ok(), "{src:?}");
        if !at.is_empty() {
            assert_eq!(d.id_of(path_of(at)).unwrap(), new, "{src:?}");
        }
        agrees_with_its_text(&d);
    }
    // A nested block value.
    let mut d = Document::parse("? a\n? b\n").unwrap();
    let mut m = oneyaml::Mapping::new();
    m.insert(Value::from("x"), Value::from(1));
    d.set_path(&path!["a"], Value::Mapping(m)).unwrap();
    assert_eq!(d.to_string(), "? a\n:\n  x: 1\n? b\n");
    agrees_with_its_text(&d);
}

/// `!!set` members: a value can be set (the text then means a mapping
/// with that value), an anchor put on the empty value, and `set_scalar`
/// used, in block and flow context.
#[test]
fn set_members() {
    let mut d = Document::parse("--- !!set\n? a\n? b\n").unwrap();
    // The empty value set to null, what it already means,
    // changes no byte (this wrote `: null` before).
    d.set_path(&path!["b"], &Value::Null).unwrap();
    assert_eq!(d.to_string(), "--- !!set\n? a\n? b\n");
    d.set_path(&path!["b"], 1).unwrap();
    assert_eq!(d.to_string(), "--- !!set\n? a\n? b\n: 1\n");
    agrees_with_its_text(&d);
    let a = d.id_of(&path!["a"]).unwrap();
    let a = d.set_anchor(a, Some("m")).unwrap();
    assert_eq!(d.to_string(), "--- !!set\n? a\n: &m\n? b\n: 1\n");
    agrees_with_its_text(&d);
    let _ = d.node_mut(a).unwrap().set_scalar("v").unwrap();
    assert_eq!(d.to_string(), "--- !!set\n? a\n: &m v\n? b\n: 1\n");
    agrees_with_its_text(&d);

    // After a key that ends its line (a block scalar), in a nested set.
    let mut d = Document::parse("s: !!set\n  ? |\n    t\n  ? u\n").unwrap();
    let t = d.id_of(&path!["s", "t\n"]).unwrap();
    let _ = d.set_anchor(t, Some("m")).unwrap();
    assert_eq!(d.to_string(), "s: !!set\n  ? |\n    t\n  : &m\n  ? u\n");
    agrees_with_its_text(&d);

    let mut d = Document::parse("--- !!set {a, b}\n").unwrap();
    let b = d.id_of(&path!["b"]).unwrap();
    let _ = d.set_anchor(b, Some("m")).unwrap();
    assert_eq!(d.to_string(), "--- !!set {a, b: &m}\n");
    agrees_with_its_text(&d);
    let a = d.id_of(&path!["a"]).unwrap();
    let _ = d.node_mut(a).unwrap().set_scalar("it's").unwrap();
    agrees_with_its_text(&d);
    assert_eq!(d.to_value().unwrap()["a"], Value::from("it's"));
}

// ------------------------------------------ alias keys are compared by meaning

/// `Removed.key_value` of an entry whose key is an alias reads through the
/// alias, as duplicate detection does: re-inserting it next to an equal
/// key is `KeyExists`, elsewhere it goes in.
#[test]
fn alias_keys_are_compared_by_meaning() {
    let src = "a: &k x\nm:\n  *k : 1\nn:\n  x: 2\no:\n  y: 3\n";
    let mut d = Document::parse(src).unwrap();
    let r = d.remove_path(&path!["m", "x"]).unwrap();
    assert_eq!(r.key(), Some("x"));
    let n = d.id_of(&path!["n"]).unwrap();
    let e = d
        .node_mut(n)
        .unwrap()
        .insert_removed(r.clone())
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::KeyExists { .. }), "{e:?}");
    let o = d.id_of(&path!["o"]).unwrap();
    d.node_mut(o).unwrap().insert_removed(r).unwrap();
    assert_eq!(
        d.to_string(),
        "a: &k x\nm: {}\nn:\n  x: 2\no:\n  y: 3\n  *k : 1\n"
    );
    agrees_with_its_text(&d);
}

// -------------------------------- meaning errors made by edits stay repairable

/// Guard freshness (documented on `Document`): the guard is found once per
/// document, before its first edit, and not refreshed by later edits. A
/// meaning error an accepted edit creates is reported by `diagnostics()`
/// and `has_errors()` but does not make its region read-only, so the next
/// edit can repair it — in a `Document`, and in a stream across switches.
#[test]
fn meaning_errors_made_by_edits_stay_repairable() {
    let settings = Settings::new().merge_keys(true);
    let src = "base: &b {x: 1}\nuse:\n  <<: *b\n";
    let mut d = Document::parse_with(src, &settings).unwrap();
    d.set_path(&path!["base"], 3).unwrap();
    assert!(d.has_errors());
    assert_eq!(d.diagnostics()[0].code, oneyaml::code::INVALID_MERGE_VALUE);
    agrees_with_its_text(&d);
    let mut m = oneyaml::Mapping::new();
    m.insert(Value::from("x"), Value::from(1));
    d.set_path(&path!["base"], Value::Mapping(m.clone()))
        .unwrap();
    assert!(!d.has_errors());
    agrees_with_its_text(&d);

    let two = format!("a: 1\n---\n{src}");
    let mut s = Stream::parse_with(two, &settings).unwrap();
    s.document_mut(1)
        .unwrap()
        .set_path(&path!["base"], 3)
        .unwrap();
    assert!(s.has_errors());
    s.document_mut(0).unwrap().set_path(&path!["a"], 2).unwrap();
    // Back to the second document: its guard is the one found before its
    // first edit (kept across the switch, not found again from the
    // edited stream), so the repair is accepted.
    let mut second = s.document_mut(1).unwrap();
    second.set_path(&path!["base"], Value::Mapping(m)).unwrap();
    assert!(!second.has_errors());
    assert!(!s.has_errors());
    same_as_reparse(&s);
}
