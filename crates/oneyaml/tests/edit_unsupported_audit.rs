//! The `EditError::Unsupported` audit against the 0.2
//! limitation list. Two refusals outside the list were
//! gaps and are fixed here; a comment duplicated by moving an entry whose
//! value is a block scalar was found on the way (nothing is destroyed
//! silently" — nor created).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, Value, path};

fn comments(text: &str) -> usize {
    text.matches('#').count()
}

/// `insert_alias` under a key too long (> 1024 characters) or multi-line
/// for an implicit key takes the serializer's `? ` form, as
/// `insert` of a value already did; before, `Unsupported`.
#[test]
fn aliases_under_keys_that_need_the_explicit_form() {
    let long = "x".repeat(1100);
    let cases: [(&str, &str, &str); 4] = [
        (
            "a: &x 1\nm:\n  b: 2\n",
            "m",
            "a: &x 1\nm:\n  b: 2\n  ? K\n  : *x\n",
        ),
        ("a: &x 1\nm: {b: 2}\n", "m", "a: &x 1\nm: {b: 2, ? K: *x}\n"),
        ("a: &x 1\nm: {}\n", "m", "a: &x 1\nm: {? K: *x}\n"),
        (
            "a: &x 1\nm:\n  - k:\n      b: 2\n",
            "k",
            "a: &x 1\nm:\n  - k:\n      b: 2\n      ? K\n      : *x\n",
        ),
    ];
    for (src, at, want) in cases {
        let mut d = Document::parse(src).unwrap();
        let a = d.id_of(&path!["a"]).unwrap();
        let m = if at == "k" {
            d.id_of(&path!["m", 0, "k"]).unwrap()
        } else {
            d.id_of(&path!["m"]).unwrap()
        };
        let id = d.node_mut(m).unwrap().insert_alias(&long, a).unwrap().id();
        assert_eq!(d.to_string().replace(&long, "K"), want);
        assert_eq!(d.aliases_of(a).unwrap(), vec![id]);
        agrees_with_its_text(&d);
        let v = Value::try_from(&d).unwrap();
        let inner = if at == "k" { &v["m"][0]["k"] } else { &v["m"] };
        assert_eq!(inner[long.as_str()], Value::from(1));
    }
    // A multi-line key: a `|-` block key in block context, a quoted one
    // in flow context.
    let mut d = Document::parse("a: &x 1\nm:\n  b: 2\n").unwrap();
    let (a, m) = (d.id_of(&path!["a"]).unwrap(), d.id_of(&path!["m"]).unwrap());
    d.node_mut(m).unwrap().insert_alias("p\nq", a).unwrap();
    assert_eq!(
        d.to_string(),
        "a: &x 1\nm:\n  b: 2\n  ? |-\n    p\n    q\n  : *x\n"
    );
    agrees_with_its_text(&d);
    let mut d = Document::parse("a: &x 1\nm: {b: 2}\n").unwrap();
    let (a, m) = (d.id_of(&path!["a"]).unwrap(), d.id_of(&path!["m"]).unwrap());
    d.node_mut(m).unwrap().insert_alias("p\nq", a).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nm: {b: 2, \"p\\nq\": *x}\n");
    agrees_with_its_text(&d);
    // A short key keeps the implicit form.
    let mut d = Document::parse("a: &x 1\nm:\n  b: 2\n").unwrap();
    let (a, m) = (d.id_of(&path!["a"]).unwrap(), d.id_of(&path!["m"]).unwrap());
    d.node_mut(m).unwrap().insert_alias("s", a).unwrap();
    assert_eq!(d.to_string(), "a: &x 1\nm:\n  b: 2\n  s: *x\n");
}

/// A moved entry keeps its comments when it is appended after a block
/// scalar (the collection's last line ends inside it), and when its own
/// value is a block scalar with comment lines after it; before, the first
/// was `Unsupported` ("comment lines after an entry appended after a block
/// scalar"), the second wrote the comment twice.
///
/// Comment rule 4: the comment lines between
/// the moved entry and the next one lead the next entry, so they stay
/// with it; the moved entry takes its same-line comment only.
#[test]
fn moved_comments_after_block_scalars() {
    // (source, moved entry, destination index or append, expected)
    let cases: [(&str, &str, Option<usize>, &str); 6] = [
        (
            "k:\n  a: |\n    text\no:\n  b: 1 # same\n  # after\n  c: 2\n",
            "b",
            None,
            "k:\n  a: |\n    text\n  b: 1 # same\no:\n  # after\n  c: 2\n",
        ),
        (
            "k:\n  a: |\n    text\no:\n  b: 1\n  # after\n  c: 2\n",
            "b",
            None,
            "k:\n  a: |\n    text\n  b: 1\no:\n  # after\n  c: 2\n",
        ),
        (
            "k:\n  a: 1\no:\n  b: |\n    t\n  # after\n  c: 2\n",
            "b",
            None,
            "k:\n  a: 1\n  b: |\n    t\no:\n  # after\n  c: 2\n",
        ),
        (
            "k:\n  a: 1\no:\n  b: |\n    t\n  # after\n  c: 2\n",
            "b",
            Some(0),
            "k:\n  b: |\n    t\n  a: 1\no:\n  # after\n  c: 2\n",
        ),
        (
            "k:\n  a: |\n    s\no:\n  b: |\n    t\n  # after\n  c: 2\n",
            "b",
            None,
            "k:\n  a: |\n    s\n  b: |\n    t\no:\n  # after\n  c: 2\n",
        ),
        (
            "k:\n  a: |\n    s\no:\n  b: 1 # one\n  # two\n  # three\n  c: 2\n",
            "b",
            None,
            "k:\n  a: |\n    s\n  b: 1 # one\no:\n  # two\n  # three\n  c: 2\n",
        ),
    ];
    for (src, key, at, want) in cases {
        let mut d = Document::parse(src).unwrap();
        let r = d.get_mut("o").unwrap().remove(key).unwrap();
        let mut k = d.get_mut("k").unwrap();
        match at {
            Some(i) => k.insert_removed_at(i, r).unwrap(),
            None => k.insert_removed(r).unwrap(),
        };
        let text = d.to_string();
        assert_eq!(text, want, "{src:?}");
        assert_eq!(comments(&text), comments(src), "{src:?}");
        agrees_with_its_text(&d);
    }
    // The same in a block sequence.
    let src = "k:\n  - |\n    text\no:\n  - 1 # same\n  # after\n  - 2\n";
    let mut d = Document::parse(src).unwrap();
    let r = d.get_mut("o").unwrap().remove_at(0).unwrap();
    d.get_mut("k").unwrap().insert_removed(r).unwrap();
    assert_eq!(
        d.to_string(),
        "k:\n  - |\n    text\n  - 1 # same\no:\n  # after\n  - 2\n"
    );
    agrees_with_its_text(&d);
}

/// Edits that create a meaning error are accepted in 0.2 and reported
/// through `diagnostics()` (documented on `Document`): both readers agree
/// on the erroring text.
#[test]
fn accepted_meaning_errors_are_reported() {
    let has = |d: &Document, code| d.diagnostics().iter().any(|x| x.code == code);
    // Y1016: the node a merge key reads through an alias replaced with a
    // non-mapping.
    let settings = oneyaml::Settings::new().merge_keys(true);
    let src = "base: &b {x: 1}\nuse:\n  <<: *b\n  y: 2\n";
    let mut d = Document::parse_with(src, &settings).unwrap();
    d.set_path(&path!["base"], 5).unwrap();
    assert_eq!(d.to_string(), "base: &b 5\nuse:\n  <<: *b\n  y: 2\n");
    assert!(
        has(&d, oneyaml::code::INVALID_MERGE_VALUE),
        "{:?}",
        d.diagnostics()
    );
    assert!(d.has_errors());
    agrees_with_its_text(&d);
    // Y1013: a complex key made equal to a sibling key.
    let mut d = Document::parse("? [a]\n: 1\n? [b]\n: 2\n").unwrap();
    let inner = {
        let root = d.root().unwrap();
        let m = root.as_mapping().unwrap();
        let (k, _) = m.iter().nth(1).unwrap();
        k.as_sequence().unwrap().get(0).unwrap().id()
    };
    d.node_mut(inner).unwrap().set("a").unwrap();
    assert_eq!(d.to_string(), "? [a]\n: 1\n? [a]\n: 2\n");
    assert!(
        has(&d, oneyaml::code::DUPLICATE_KEY),
        "{:?}",
        d.diagnostics()
    );
    agrees_with_its_text(&d);
}

/// A refusal of a documented limitation changes nothing.
#[test]
fn documented_limitations_refuse_without_change() {
    // Replacing a mapping key in place.
    let src = "a: 1\nb: [x]\n";
    let mut d = Document::parse(src).unwrap();
    let key = {
        let root = d.root().unwrap();
        let (k, _) = root.as_mapping().unwrap().iter().next().unwrap();
        k.id()
    };
    let e = d.node_mut(key).unwrap().set(2).unwrap_err();
    assert!(matches!(e, EditError::Unsupported { .. }), "{e:?}");
    assert_eq!(d.to_string(), src);
    // Block content moved into a flow collection.
    let src = "a:\n  k: 1 # c\n  j: 2\nb: {x: 1}\n";
    let mut d = Document::parse(src).unwrap();
    let r = d.get_mut("a").unwrap().remove("k").unwrap();
    let text = d.to_string();
    let mut wrong = Document::parse("f: {x: 1}\n").unwrap();
    let e = wrong
        .get_mut("f")
        .unwrap()
        .insert_removed(r.clone())
        .unwrap_err()
        .into_error();
    assert_eq!(e, EditError::WrongDocument);
    let e = d
        .get_mut("b")
        .unwrap()
        .insert_removed(r)
        .unwrap_err()
        .into_error();
    assert!(
        matches!(e, EditError::Unsupported { reason, .. } if reason.contains("flow")),
        "{e:?}"
    );
    assert_eq!(d.to_string(), text);
}

/// Found by the edit benchmark: removing the last entry of a
/// block collection whose previous value is a block scalar left the
/// entry's line break behind (a new blank line each insert + remove; for
/// a `|+` scalar, a changed value). Insert then remove is the identity.
#[test]
fn removing_the_entry_after_a_block_scalar_takes_its_line_break() {
    for src in [
        "a:\n  b: >-\n    x\n\n",
        "a:\n  b: >-\n    x\n",
        "a: |\n  x\n\n",
        "a: |\n  x\n",
        "a: 1\n\n",
        "s:\n  - |\n    x\nz: 1\n",
    ] {
        let mut d = Document::parse(src).unwrap();
        for round in 0..3 {
            if src.starts_with("s:") {
                d.get_mut("s").unwrap().push("x").unwrap();
                agrees_with_its_text(&d);
                let _ = d.get_mut("s").unwrap().remove_at(1).unwrap();
            } else {
                d.root_mut().unwrap().insert("k", "x").unwrap();
                d.set_path(&path!["k"], "y").unwrap();
                agrees_with_its_text(&d);
                let _ = d.root_mut().unwrap().remove("k").unwrap();
            }
            assert_eq!(d.to_string(), src, "round {round}");
            agrees_with_its_text(&d);
        }
    }
    // A keep-chomped scalar keeps its value when the entry after it goes.
    let src = "a: |+\n  x\n\nb: 1\n";
    let mut d = Document::parse(src).unwrap();
    let _ = d.root_mut().unwrap().remove("b").unwrap();
    assert_eq!(d.to_string(), "a: |+\n  x\n\n");
    assert_eq!(d.to_value().unwrap()["a"].as_str(), Some("x\n\n"));
    agrees_with_its_text(&d);
    // The same with a block scalar in a nested mapping, in a stream.
    let src = "k:\n  a: |\n    t\n  b: 1\n---\nz: 2\n";
    let mut s = oneyaml::Stream::parse(src).unwrap();
    let mut first = s.document_mut(0).unwrap();
    let _ = first.get_mut("k").unwrap().remove("b").unwrap();
    assert_eq!(s.to_string(), "k:\n  a: |\n    t\n---\nz: 2\n");
    assert!(!s.has_errors());
}
