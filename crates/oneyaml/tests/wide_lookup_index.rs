//! The lookup index of wide collections (`document/lookup.rs`) answers every
//! key- and position-addressed lookup exactly as the linear scans do — keys by
//! meaning (tags, `!!str` vs plain, numbers in other radixes, null forms,
//! collection keys), the last of repeated keys, merge keys, alias keys (read
//! live), positions through inserts, removals and moves, refused edits (rolled
//! back), clones, and stream documents. The timing shapes are the `wide_*`
//! tests of `document_scaling.rs`.
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
use oneyaml::{Document, DuplicateKeys, EditError, Settings, Stream, Value, path};

/// Filler entries, so the mapping is wide (well past the index's width).
fn filler(s: &mut String, from: usize, to: usize) {
    for i in from..to {
        writeln!(s, "f{i}: {i}").unwrap();
    }
}

fn wide_keys() -> String {
    let mut s = String::from("anchor: &k name\n");
    s.push_str("m:\n");
    let mut inner = String::new();
    filler(&mut inner, 0, 40);
    inner.push_str("'1': quoted one\n");
    inner.push_str("1: int one\n");
    inner.push_str("!!str 2: tagged two\n");
    inner.push_str("0x10: hex sixteen\n");
    inner.push_str("~: tilde null\n");
    inner.push_str("true: bool\n");
    inner.push_str("[1, 2]: complex\n");
    inner.push_str("*k : alias key\n");
    inner.push_str("!t x: custom tag\n");
    filler(&mut inner, 40, 80);
    for l in inner.lines() {
        writeln!(s, "  {l}").unwrap();
    }
    s.push_str("other: {16: a, null: b, '2': c, x: d}\n");
    s
}

#[test]
fn keys_are_found_by_meaning() {
    let mut d = Document::parse(wide_keys()).unwrap();
    // The root, `m`, its `[1, 2]` key, `other`.
    assert_eq!(d.check_lookup_indexes(), Ok(4));
    let m = d.get("m").unwrap();
    assert_eq!(m.as_mapping().unwrap().len(), 89);
    // `get` matches keys that resolve to that string only.
    assert_eq!(m.get("1").unwrap().text(), "quoted one");
    assert_eq!(m.get("2").unwrap().text(), "tagged two");
    assert!(m.get("0x10").is_none(), "an integer key");
    assert!(m.get("16").is_none());
    assert!(m.get("~").is_none() && m.get("true").is_none());
    assert!(m.get("x").is_none(), "a custom-tagged key is no string");
    assert_eq!(m.get("name").unwrap().text(), "alias key");
    assert_eq!(m.get("f79").unwrap().text(), "79");
    assert!(m.get("f80").is_none());
    // Re-inserting keys equal by meaning is refused (`KeyExists`): `16`
    // is `0x10`, `null` is `~`, `'2'` is `!!str 2`.
    // (Removed by node: `16` and `null` are no string keys.)
    let value_of = |d: &Document, key: &str| {
        let other = d.get("other").unwrap().as_mapping().unwrap();
        other.iter().find(|(k, _)| k.text() == key).unwrap().1.id()
    };
    for key in ["16", "null", "'2'"] {
        let r = d.remove_node(value_of(&d, key)).unwrap();
        let e = d.get_mut("m").unwrap().insert_removed(r).unwrap_err();
        assert!(
            matches!(e.error(), EditError::KeyExists { .. }),
            "{key}: {e:?}"
        );
        let r = e.into_removed();
        d.get_mut("other").unwrap().insert_removed(r).unwrap();
    }
    // `x` (plain) is not `!t x`: it goes in.
    let r = d.get_mut("other").unwrap().remove("x").unwrap();
    d.get_mut("m").unwrap().insert_removed(r).unwrap();
    assert_eq!(d.get_path(&path!["m", "x"]).unwrap().text(), "d");
    assert_eq!(d.check_lookup_indexes(), Ok(4));
    agrees_with_its_text(&d);
}

#[test]
fn alias_keys_follow_their_anchored_node() {
    let mut d = Document::parse(wide_keys()).unwrap();
    assert_eq!(d.get_path(&path!["m", "name"]).unwrap().text(), "alias key");
    // Replacing the anchored node (it keeps its anchor)
    // changes what the alias key means, with no edit of the mapping.
    d.set_path(&path!["anchor"], "renamed").unwrap();
    assert!(d.get_path(&path!["m", "name"]).is_none());
    assert_eq!(
        d.get_path(&path!["m", "renamed"]).unwrap().text(),
        "alias key"
    );
    assert_eq!(d.check_lookup_indexes(), Ok(4));
    agrees_with_its_text(&d);
}

#[test]
fn the_last_of_repeated_keys_counts() {
    let mut src = String::from("dup: first\n");
    filler(&mut src, 0, 100);
    src.push_str("dup: last\n");
    filler(&mut src, 100, 120);
    let s = Settings::new().duplicate_keys(DuplicateKeys::LastWins);
    let mut d = Document::parse_with(src, &s).unwrap();
    assert_eq!(d.get("dup").unwrap().text(), "last");
    d.set_path(&path!["dup"], "set").unwrap();
    assert!(d.to_string().starts_with("dup: first\n"));
    assert_eq!(d.get("dup").unwrap().text(), "set");
    let r = d.root_mut().unwrap().remove("dup").unwrap();
    assert_eq!(r.text(), "dup: set");
    assert_eq!(d.get("dup").unwrap().text(), "first");
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    assert_eq!(d.to_value().unwrap()["dup"].as_str(), Some("first"));
}

#[test]
fn merge_keys_on_and_off() {
    let mut src = String::from("base: &b {inherited: yes, f3: from base}\nm:\n");
    let mut inner = String::new();
    filler(&mut inner, 0, 50);
    inner.push_str("<<: *b\n");
    filler(&mut inner, 50, 100);
    for l in inner.lines() {
        writeln!(src, "  {l}").unwrap();
    }
    // On: `<<` merges; own keys win; a second merge key is refused.
    let on = Settings::new().merge_keys(true);
    let mut d = Document::parse_with(src.as_str(), &on).unwrap();
    let m = d.get("m").unwrap();
    assert_eq!(m.get("inherited").unwrap().text(), "yes");
    assert_eq!(m.get("f3").unwrap().text(), "3");
    assert!(m.get("<<").is_none());
    let base = d.id_of(&path!["base"]).unwrap();
    let e = d.get_mut("m").unwrap().insert_merge(&[base]).unwrap_err();
    assert!(matches!(e, EditError::KeyExists { .. }), "{e:?}");
    // An own key shadows the merged one.
    d.set_path(&path!["m", "inherited"], "own").unwrap();
    assert_eq!(d.get_path(&path!["m", "inherited"]).unwrap().text(), "own");
    assert_eq!(d.check_lookup_indexes(), Ok(3));
    agrees_with_its_text(&d);
    // Off: `<<` is an ordinary string key whose value is the alias.
    let d = Document::parse(src.as_str()).unwrap();
    let m = d.get("m").unwrap();
    assert_eq!(m.get("<<").unwrap().text(), "*b");
    assert!(m.get("inherited").is_none());
    assert_eq!(d.check_lookup_indexes(), Ok(3));
}

#[test]
fn positions_follow_edits() {
    let mut src = String::new();
    for i in 0..100 {
        writeln!(src, "- v{i}").unwrap();
    }
    let mut d = Document::parse(src).unwrap();
    let mut model: Vec<String> = (0..100).map(|i| format!("v{i}")).collect();
    let texts = |d: &Document| -> Vec<String> {
        let s = d.root().unwrap().as_sequence().unwrap();
        (0..s.len())
            .map(|i| s.get(i).unwrap().text().into_owned())
            .collect()
    };
    assert_eq!(texts(&d), model);
    let mut root = d.root_mut().unwrap();
    root.insert_at(50, "x").unwrap();
    model.insert(50, "x".into());
    root.insert_at(0, "first").unwrap();
    model.insert(0, "first".into());
    root.push("last").unwrap();
    model.push("last".into());
    let _ = root.remove_at(10).unwrap();
    model.remove(10);
    root.move_entry(5, 90).unwrap();
    let v = model.remove(5);
    model.insert(90, v);
    root.move_entry(99, 3).unwrap();
    let v = model.remove(99);
    model.insert(3, v);
    root.replace_at(60, "replaced").unwrap();
    model[60] = "replaced".into();
    assert_eq!(texts(&d), model);
    assert_eq!(d.get_path(&path![60]).unwrap().text(), "replaced");
    let e = d.get_path_mut(&path![102]).unwrap_err();
    assert!(
        matches!(e, EditError::IndexOutOfBounds { len: 102, .. }),
        "{e:?}"
    );
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    agrees_with_its_text(&d);
    // A mapping: removed and put back where it was.
    let mut src = String::new();
    filler(&mut src, 0, 100);
    let mut d = Document::parse(src).unwrap();
    let r = d.root_mut().unwrap().remove("f50").unwrap();
    d.root_mut().unwrap().insert_removed_at(50, r).unwrap();
    let m = d.root().unwrap().as_mapping().unwrap();
    assert_eq!(m.iter().nth(50).unwrap().0.text(), "f50");
    assert_eq!(m.len(), 100);
    assert_eq!(d.check_lookup_indexes(), Ok(1));
}

/// A refused edit (its group rolled back, after a removal that went
/// through) leaves the index as it was.
#[test]
fn refused_edits_roll_the_index_back() {
    let mut src = String::from("- &a 1\n- *a\n");
    for i in 0..100 {
        writeln!(src, "- v{i}").unwrap();
    }
    let mut d = Document::parse(src.as_str()).unwrap();
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    // Moving the anchor after its alias: removed, then refused at the
    // re-insertion (an alias must follow its anchor), all undone.
    let e = d.root_mut().unwrap().move_entry(0, 50).unwrap_err();
    assert!(matches!(e, EditError::AliasBeforeAnchor { .. }), "{e:?}");
    assert_eq!(d.to_string(), src);
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    assert_eq!(d.get_path(&path![0]).unwrap().text(), "&a 1");
    assert_eq!(d.get_path(&path![101]).unwrap().text(), "v99");
    // And edits after it are tracked as usual.
    d.root_mut().unwrap().move_entry(50, 2).unwrap();
    assert_eq!(d.get_path(&path![2]).unwrap().text(), "v48");
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    agrees_with_its_text(&d);
}

/// Clones share the index until one is edited (then the copy keeps an
/// exact copy: a fork keeps every handle); stream documents share the
/// stream's.
#[test]
fn clones_and_streams() {
    let mut src = String::new();
    filler(&mut src, 0, 100);
    let mut a = Document::parse(src.as_str()).unwrap();
    assert!(a.get("f99").is_some());
    let b = a.clone();
    a.root_mut().unwrap().insert("new", 1).unwrap();
    let _ = a.root_mut().unwrap().remove("f0").unwrap();
    assert!(a.get("f0").is_none() && a.get("new").is_some());
    assert!(b.get("f0").is_some() && b.get("new").is_none());
    assert_eq!(a.check_lookup_indexes(), Ok(1));
    assert_eq!(b.check_lookup_indexes(), Ok(1));
    let mut c = b.clone();
    c.root_mut().unwrap().move_entry(99, 0).unwrap();
    assert_eq!(
        c.root()
            .unwrap()
            .as_mapping()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .0
            .text(),
        "f99"
    );
    assert_eq!(c.check_lookup_indexes(), Ok(1));
    assert_eq!(b.check_lookup_indexes(), Ok(1));

    let stream_src = format!("a: 1\n---\n{src}---\n{src}");
    let mut s = Stream::parse(stream_src).unwrap();
    let first = s.document(1).unwrap();
    assert!(first.get("f50").is_some());
    let mut d = s.document_mut(2).unwrap();
    d.set_path(&path!["f50"], "changed").unwrap();
    let _ = d.root_mut().unwrap().remove("f10").unwrap();
    for i in 0..3 {
        assert_eq!(
            s.document(i).unwrap().check_lookup_indexes().map(|_| ()),
            Ok(())
        );
    }
    assert_eq!(
        s.document(2).unwrap().to_value().unwrap()["f50"],
        Value::from("changed")
    );
    assert_eq!(
        s.document(1).unwrap().to_value().unwrap()["f50"],
        Value::from(50)
    );
    assert!(s.document(2).unwrap().get("f10").is_none());
}

/// A restricted document (errors elsewhere): its
/// collections are indexed too, with the answers of the linear scans
/// (error regions are presented where they broke nodes): the root and the
/// cut-off `[1, 2`.
#[test]
fn restricted_documents_use_the_index() {
    let mut src = String::new();
    filler(&mut src, 0, 100);
    src.push_str("broken: [1, 2\n");
    let p = Document::parse_recovering(src);
    assert!(p.has_errors());
    let d = p.into_value();
    assert_eq!(d.check_lookup_indexes(), Ok(2));
    assert_eq!(d.get("f99").unwrap().text(), "99");
    assert!(d.get("missing").is_none());
}
