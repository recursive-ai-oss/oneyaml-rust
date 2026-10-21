//! Entries whose key is a collection: moved between mappings and between
//! flow and block context, refused as duplicates when an equal key is there,
//! with anchors and aliases in the key.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, EditError, NodeId, Value, path};

/// The value node of entry `n` of the mapping at `at` (a key of the root).
fn entry_value(doc: &Document, at: &str, n: usize) -> NodeId {
    doc.get_path(&path![at])
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .nth(n)
        .map(|(_, v)| v.id())
        .unwrap()
}

/// Moves entry `n` of the mapping `from` into the mapping `to`.
fn moved(src: &str, from: &str, n: usize, to: &str) -> Result<String, EditError> {
    let mut doc = Document::parse(src).unwrap();
    let id = entry_value(&doc, from, n);
    let removed = doc.remove_node(id).unwrap();
    let target = doc.get_path(&path![to]).unwrap().id();
    doc.node_mut(target)
        .unwrap()
        .insert_removed(removed)
        .map_err(oneyaml::ReinsertError::into_error)?;
    Ok(doc.to_string())
}

fn value(text: &str) -> Value {
    oneyaml::from_str(text).unwrap()
}

/// The moved document reads as `want`, a flow-written model of it.
fn moves_as(src: &str, want: &str) -> String {
    let out = moved(src, "a", 0, "b").unwrap();
    assert_eq!(value(&out), value(want), "{src:?} gave {out:?}");
    out
}

// ------------------------------------------- flow explicit entry to block

#[test]
fn a_flow_entry_with_an_explicit_collection_key_goes_into_a_block_mapping_as_two_lines() {
    assert_eq!(
        moves_as(
            "a: {? [x] : 1}\nb:\n  c: 2\n",
            "{a: {}, b: {c: 2, ? [x] : 1}}"
        ),
        "a: {}\nb:\n  c: 2\n  ? [x]\n  : 1\n"
    );
    assert_eq!(
        moves_as(
            "a: {? {p: q} : 1}\nb:\n  c: 2\n",
            "{a: {}, b: {c: 2, ? {p: q} : 1}}"
        ),
        "a: {}\nb:\n  c: 2\n  ? {p: q}\n  : 1\n"
    );
    assert_eq!(
        moves_as(
            "a: {? [x]: 1}\nb:\n  c: 2\n",
            "{a: {}, b: {c: 2, ? [x] : 1}}"
        ),
        "a: {}\nb:\n  c: 2\n  ? [x]\n  : 1\n"
    );
}

#[test]
fn a_flow_entry_with_an_explicit_scalar_key_goes_into_a_block_mapping_as_two_lines() {
    assert_eq!(
        moves_as("a: {? x : 1}\nb:\n  c: 2\n", "{a: {}, b: {c: 2, x: 1}}"),
        "a: {}\nb:\n  c: 2\n  ? x\n  : 1\n"
    );
}

#[test]
fn the_two_lines_keep_properties_of_the_key_and_the_value() {
    let out = moves_as(
        "a: {? !t [x] : &v 1}\nb:\n  c: 2\n",
        "{a: {}, b: {c: 2, ? !t [x] : 1}}",
    );
    assert_eq!(out, "a: {}\nb:\n  c: 2\n  ? !t [x]\n  : &v 1\n");
}

#[test]
fn a_flow_entry_with_an_explicit_key_and_no_value_goes_in_as_one_line() {
    let out = moves_as(
        "a: {? [x]}\nb:\n  c: 2\n",
        "{a: {}, b: {c: 2, ? [x] : null}}",
    );
    assert_eq!(out, "a: {}\nb:\n  c: 2\n  ? [x]\n");
}

#[test]
fn a_flow_entry_with_an_explicit_key_of_several_lines_is_refused_for_a_block_mapping() {
    let r = moved("a: {? [x,\n  y] : 1}\nb:\n  c: 2\n", "a", 0, "b");
    assert!(matches!(r, Err(EditError::Unsupported { .. })), "{r:?}");
}

#[test]
fn an_explicit_flow_entry_goes_into_another_flow_mapping_as_written() {
    let out = moved("a: {? [x] : 1}\nb: {c: 2}\n", "a", 0, "b").unwrap();
    assert_eq!(
        value(&out),
        value("{a: {}, b: {c: 2, ? [x] : 1}}"),
        "{out:?}"
    );
}

// ------------------------------------------------ equal keys are refused

fn refused_as_duplicate(src: &str) {
    let r = moved(src, "a", 0, "b");
    assert!(
        matches!(r, Err(EditError::KeyExists { .. })),
        "{src:?}: {r:?}"
    );
}

#[test]
fn an_equal_collection_key_is_a_duplicate() {
    refused_as_duplicate("a:\n  ? [x]\n  : 1\nb:\n  ? [x]\n  : 2\n");
    refused_as_duplicate("a: {? [x] : 1}\nb:\n  ? [x]\n  : 2\n");
}

#[test]
fn mapping_keys_are_equal_whatever_the_order_of_their_entries() {
    refused_as_duplicate("a:\n  ? {p: 1, q: 2}\n  : 1\nb:\n  ? {q: 2, p: 1}\n  : 2\n");
}

#[test]
fn keys_are_equal_by_value_not_by_spelling() {
    refused_as_duplicate("a:\n  ? [1]\n  : 1\nb:\n  ? [0x1]\n  : 2\n");
}

#[test]
fn an_alias_in_a_key_is_what_it_names() {
    refused_as_duplicate("s: &s x\na:\n  ? [*s]\n  : 1\nb:\n  ? [x]\n  : 2\n");
    refused_as_duplicate("s: &s x\na:\n  ? [x]\n  : 1\nb:\n  ? [*s]\n  : 2\n");
}

#[test]
fn keys_that_differ_are_no_duplicates() {
    let out = moved("a:\n  ? [x]\n  : 1\nb:\n  ? [y]\n  : 2\n", "a", 0, "b").unwrap();
    assert_eq!(out, "a: {}\nb:\n  ? [y]\n  : 2\n  ? [x]\n  : 1\n");
    // A tag makes another key.
    let out = moved("a:\n  ? !t [x]\n  : 1\nb:\n  ? [x]\n  : 2\n", "a", 0, "b").unwrap();
    assert_eq!(value(&out), value("{a: {}, b: {? [x] : 2, ? !t [x] : 1}}"));
}

#[test]
fn a_scalar_key_is_never_equal_to_a_collection_key() {
    let out = moved("a:\n  ? [x]\n  : 1\nb:\n  '[x]': 2\n", "a", 0, "b").unwrap();
    assert_eq!(value(&out), value("{a: {}, b: {'[x]': 2, ? [x] : 1}}"));
}

// ------------------------------------------- moves, anchors and aliases

#[test]
fn a_collection_key_entry_moves_between_block_mappings_and_back() {
    let src = "a:\n  ? [x, y]\n  : 1\n  k: 2\nb:\n  c: 3\n";
    let mut doc = Document::parse(src).unwrap();
    let id = entry_value(&doc, "a", 0);
    let removed = doc.remove_node(id).unwrap();
    assert_eq!(doc.to_string(), "a:\n  k: 2\nb:\n  c: 3\n");
    let b = doc.get_path(&path!["b"]).unwrap().id();
    doc.node_mut(b)
        .unwrap()
        .insert_removed(removed)
        .map_err(oneyaml::ReinsertError::into_error)
        .unwrap();
    assert_eq!(
        doc.to_string(),
        "a:\n  k: 2\nb:\n  c: 3\n  ? [x, y]\n  : 1\n"
    );
}

#[test]
fn an_alias_in_a_moved_key_keeps_naming_its_anchor() {
    let out = moved("s: &s x\na:\n  ? [*s]\n  : 1\nb:\n  c: 2\n", "a", 0, "b").unwrap();
    assert_eq!(out, "s: &s x\na: {}\nb:\n  c: 2\n  ? [*s]\n  : 1\n");
}

#[test]
fn an_anchor_in_a_moved_key_moves_with_it() {
    let out = moved("a:\n  ? [&k x, y]\n  : *k\nb:\n  c: 2\n", "a", 0, "b").unwrap();
    assert_eq!(out, "a: {}\nb:\n  c: 2\n  ? [&k x, y]\n  : *k\n");
    // And two keys that define an anchor, with equal meaning, are duplicates.
    refused_as_duplicate("a:\n  ? [&k x]\n  : 1\nb:\n  ? [x]\n  : 2\n");
}

/// The value node of the first (or last) entry of the mapping at `at` whose
/// key is a collection.
fn collection_entry(doc: &Document, at: &str, last: bool) -> NodeId {
    let m = doc.get_path(&path![at]).unwrap().as_mapping().unwrap();
    let mut found = m
        .iter()
        .filter(|(k, _)| k.as_scalar().is_none())
        .map(|(_, v)| v.id());
    if last { found.last() } else { found.next() }.unwrap()
}

#[test]
fn a_run_of_moves_back_and_forth_keeps_finding_the_duplicate() {
    let mut doc =
        Document::parse("a:\n  ? [x]\n  : 1\n  k: 0\nb:\n  ? [y]\n  : 2\n  m: 0\n").unwrap();
    for _ in 0..3 {
        let id = collection_entry(&doc, "a", false);
        let removed = doc.remove_node(id).unwrap();
        let b = doc.get_path(&path!["b"]).unwrap().id();
        doc.node_mut(b)
            .unwrap()
            .insert_removed(removed)
            .map_err(oneyaml::ReinsertError::into_error)
            .unwrap();
        // Back again: `x` is now the last collection key of `b`.
        let id = collection_entry(&doc, "b", true);
        let removed = doc.remove_node(id).unwrap();
        let a = doc.get_path(&path!["a"]).unwrap().id();
        doc.node_mut(a)
            .unwrap()
            .insert_removed(removed)
            .map_err(oneyaml::ReinsertError::into_error)
            .unwrap();
    }
    assert_eq!(
        value(&doc.to_string()),
        value("{a: {? [x] : 1, k: 0}, b: {? [y] : 2, m: 0}}")
    );
    // `x` is in `a` again: another `x` put there is a duplicate.
    let mut other = Document::parse("a:\n  ? [x]\n  : 1\nb:\n  ? [x]\n  : 9\n").unwrap();
    let id = collection_entry(&other, "b", false);
    let removed = other.remove_node(id).unwrap();
    let a = other.get_path(&path!["a"]).unwrap().id();
    let mut target = other.node_mut(a).unwrap();
    let r = target.insert_removed(removed);
    assert!(matches!(r, Err(ref e) if matches!(e.error(), EditError::KeyExists { .. })));
}
