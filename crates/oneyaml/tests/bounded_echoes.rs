//! Bounded echoes of input
//! text in errors, lookups next to error regions and
//! under `LastWins` repeats. The timing shapes of
//! the lookups are in `document_scaling.rs`.
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
use oneyaml::{Document, DuplicateKeys, EditError, Settings, Value, path};

/// 2 MiB of anchor-name characters.
fn huge_name() -> String {
    "n".repeat(2 << 20)
}

/// What an error shows: bounded whatever the input (a message is at most
/// 3 KiB, a help text too, a path 8 KiB).
const SHOWN: usize = 12 << 10;

#[test]
fn undefined_and_recursive_alias_errors_are_bounded() {
    let name = huge_name();
    for src in [format!("a: *{name}\n"), format!("a: &{name} [*{name}]\n")] {
        let e = Document::parse(src.as_str()).unwrap_err();
        assert_eq!(e.code(), oneyaml::code::UNDEFINED_ALIAS);
        let d = e.diagnostic();
        assert!(d.message.len() < 4096, "{}", d.message.len());
        assert!(d.help.as_ref().is_none_or(|h| h.len() < 4096));
        assert!(d.message.contains('…'));
        assert!(e.to_string().len() < SHOWN);
        let e = oneyaml::from_str::<Value>(&src).unwrap_err();
        assert!(e.diagnostic().help.as_ref().is_none_or(|h| h.len() < 4096));
        assert!(e.to_string().len() < SHOWN);
        let p = Document::parse_recovering(src.as_str());
        for d in p.diagnostics() {
            assert!(d.message.len() < 4096 && d.help.as_ref().is_none_or(|h| h.len() < 4096));
        }
    }
}

#[test]
fn edit_errors_show_input_text_bounded() {
    let name = huge_name();
    let src = format!("a: &{name} 1\nb: *{name}\nc: 2\n");
    let mut d = Document::parse(src.as_str()).unwrap();
    // AnchorInUse: the field keeps the name, `Display` shows a segment.
    let e = d.remove_path(&path!["a"]).unwrap_err();
    assert!(matches!(&e, EditError::AnchorInUse { anchor, .. } if anchor.len() == name.len()));
    assert!(e.to_string().len() < 200, "{}", e.to_string().len());
    // DuplicateAnchor.
    let c = d.id_of(&path!["c"]).unwrap();
    let e = d.set_anchor(c, Some(&name)).unwrap_err();
    assert!(matches!(e, EditError::DuplicateAnchor { .. }), "{e:?}");
    assert!(e.to_string().len() < 200);
    // InvalidAnchorName.
    let bad = format!("{name},");
    let e = d.set_anchor(c, Some(&bad)).unwrap_err();
    assert!(matches!(e, EditError::InvalidAnchorName { .. }), "{e:?}");
    assert!(e.to_string().len() < 200);
    // InvalidComment.
    let text = format!("{name}\nx");
    let e = d.set_leading(c, &[text.as_str()]).unwrap_err();
    assert!(matches!(e, EditError::InvalidComment { .. }), "{e:?}");
    assert!(e.to_string().len() < 200);
    // A path naming a key of the caller's.
    let e = d.get_path_mut(&path![name.clone(), "x"]).unwrap_err();
    assert!(matches!(e, EditError::NotFound { .. }), "{e:?}");
    assert!(e.to_string().len() < SHOWN);
    assert_eq!(d.to_string(), src);
}

/// An index next to error regions answers as the
/// linear scans do, and edits of a clean collection in a restricted
/// document keep it exact; an edit in the collection holding the error
/// region is still refused.
#[test]
fn indexes_next_to_error_regions() {
    let mut src = String::from("m:\n");
    for i in 0..100 {
        writeln!(src, "  k{i}: v{i}").unwrap();
    }
    src.push_str("bad: x: y\n");
    let mut d = Document::parse_recovering(src).into_value();
    assert!(d.has_errors());
    assert!(d.check_lookup_indexes().unwrap() >= 2);
    d.set_path(&path!["m", "k99"], "changed").unwrap();
    d.get_mut("m").unwrap().insert("new", 1).unwrap();
    let _ = d.get_mut("m").unwrap().remove("k0").unwrap();
    d.get_mut("m").unwrap().move_entry(98, 0).unwrap();
    assert!(d.check_lookup_indexes().unwrap() >= 2);
    assert_eq!(d.get_path(&path!["m", "k99"]).unwrap().text(), "changed");
    assert!(d.get_path(&path!["m", "k0"]).is_none());
    let e = d.root_mut().unwrap().insert("z", 1).unwrap_err();
    assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
    agrees_with_its_text(&d);
}

/// Under `LastWins` the last repeat governs, through
/// edits that remove it, replace it, and append a new one.
#[test]
fn last_wins_repeats_keep_the_governing_key() {
    let s = Settings::new().duplicate_keys(DuplicateKeys::LastWins);
    let mut src = String::new();
    for i in 0..100 {
        writeln!(src, "a: {i}").unwrap();
    }
    let mut d = Document::parse_with(src, &s).unwrap();
    assert_eq!(d.get("a").unwrap().text(), "99");
    let _ = d.root_mut().unwrap().remove("a").unwrap();
    assert_eq!(d.get("a").unwrap().text(), "98");
    d.set_path(&path!["a"], "set").unwrap();
    assert_eq!(d.get("a").unwrap().text(), "set");
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    let r = d.root_mut().unwrap().remove("a").unwrap();
    assert_eq!(r.text(), "a: set");
    // A refused re-insertion (another `a` is there) changes nothing.
    let e = d.root_mut().unwrap().insert_removed(r).unwrap_err();
    assert!(matches!(e.error(), EditError::KeyExists { .. }));
    assert_eq!(d.get("a").unwrap().text(), "97");
    let first = d
        .root()
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .next()
        .unwrap();
    let _ = d.remove_node(first.1.id()).unwrap();
    assert_eq!(d.get("a").unwrap().text(), "97");
    assert_eq!(d.check_lookup_indexes(), Ok(1));
    assert_eq!(d.to_value().unwrap()["a"], Value::from(97));
}
