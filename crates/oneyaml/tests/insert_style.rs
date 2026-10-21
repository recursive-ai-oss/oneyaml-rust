//! New content is written the way the document writes its own: strings take
//! the quotes of the strings beside them, keys the document's schema reads as
//! strings stay plain, and the layout of nested content is copied from the
//! nearest siblings. Removing an entry never changes the value of the entry
//! before it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Patch, Stream, Value};

/// `src` after the patch `ops`, which must apply.
fn patched(src: &str, ops: &str) -> String {
    let mut doc = Document::parse(src).unwrap();
    doc.apply_patch(&Patch::from_json(ops).unwrap()).unwrap();
    doc.to_string()
}

/// `src` after `ops`, which must apply; the result must read back as the
/// same data a model of the patch gives (`want`, as JSON).
fn patched_reads(src: &str, ops: &str, want: &str) -> String {
    let out = patched(src, ops);
    let read: Value = oneyaml::from_str(&out).unwrap();
    let want: Value = oneyaml::from_str(want).unwrap();
    assert_eq!(read, want, "{out:?}");
    out
}

// ------------------------------------------------ quotes of the siblings

#[test]
fn a_string_added_next_to_double_quoted_items_is_double_quoted() {
    assert_eq!(
        patched(
            "- \"a\"\n- \"b\"\n",
            r#"[{"op":"add","path":"/-","value":"c"}]"#
        ),
        "- \"a\"\n- \"b\"\n- \"c\"\n"
    );
}

#[test]
fn a_string_added_next_to_quoted_values_takes_their_quotes() {
    let add = r#"[{"op":"add","path":"/c","value":"z"}]"#;
    assert_eq!(
        patched("a: \"x\"\nb: \"y\"\n", add),
        "a: \"x\"\nb: \"y\"\nc: \"z\"\n"
    );
    assert_eq!(patched("a: 'x'\nb: 'y'\n", add), "a: 'x'\nb: 'y'\nc: 'z'\n");
}

#[test]
fn a_copied_string_takes_the_quotes_of_its_new_siblings() {
    assert_eq!(
        patched(
            "a: 'q'\nb: 'r'\n",
            r#"[{"op":"copy","from":"/a","path":"/c"}]"#
        ),
        "a: 'q'\nb: 'r'\nc: 'q'\n"
    );
}

#[test]
fn the_api_insert_copies_quotes_too() {
    let mut doc = Document::parse("a: \"x\"\nb: \"y\"\n").unwrap();
    doc.root_mut().unwrap().insert("c", "z").unwrap();
    assert_eq!(doc.to_string(), "a: \"x\"\nb: \"y\"\nc: \"z\"\n");
    let mut doc = Document::parse("- 'a'\n- 'b'\n").unwrap();
    doc.root_mut().unwrap().push("c").unwrap();
    assert_eq!(doc.to_string(), "- 'a'\n- 'b'\n- 'c'\n");
}

#[test]
fn the_nearest_strings_decide_and_a_plain_string_says_plain() {
    let add = r#"[{"op":"add","path":"/c","value":"z"}]"#;
    assert_eq!(patched("a: 'x'\nb: y\n", add), "a: 'x'\nb: y\nc: z\n");
    assert_eq!(
        patched("a: y\nb: \"x\"\n", add),
        "a: y\nb: \"x\"\nc: \"z\"\n"
    );
    // Numbers, booleans and nulls say nothing about strings.
    assert_eq!(
        patched("a: \"x\"\nb: 1\nc2: true\nn: ~\n", add),
        "a: \"x\"\nb: 1\nc2: true\nn: ~\nc: \"z\"\n"
    );
}

#[test]
fn flow_collections_copy_quotes_too() {
    assert_eq!(
        patched(
            "a: [\"x\", \"y\"]\n",
            r#"[{"op":"add","path":"/a/-","value":"z"}]"#
        ),
        "a: [\"x\", \"y\", \"z\"]\n"
    );
    assert_eq!(
        patched("{a: 'x'}\n", r#"[{"op":"add","path":"/b","value":"z"}]"#),
        "{a: 'x', b: 'z'}\n"
    );
}

#[test]
fn a_new_key_takes_the_quotes_of_the_keys_beside_it() {
    assert_eq!(
        patched(
            "\"a\": 1\n\"b\": 2\n",
            r#"[{"op":"add","path":"/c","value":3}]"#
        ),
        "\"a\": 1\n\"b\": 2\n\"c\": 3\n"
    );
    // Not the keys of a collection written under it.
    assert_eq!(
        patched("'a': 1\n", r#"[{"op":"add","path":"/c","value":{"k":1}}]"#),
        "'a': 1\nc:\n  k: 1\n"
    );
}

// ------------------------------------------------------------ plain keys

#[test]
fn keys_a_yaml_1_2_reader_reads_as_strings_are_written_plain() {
    assert_eq!(
        patched("a: {x: 1}\n", r#"[{"op":"add","path":"/a/y","value":2}]"#),
        "a: {x: 1, y: 2}\n"
    );
    assert_eq!(
        patched(
            "k:\n- a\nm: x\n",
            r#"[{"op":"add","path":"/n","value":[1,2]}]"#
        ),
        "k:\n- a\nm: x\nn:\n- 1\n- 2\n"
    );
    let mut doc = Document::parse("a: 1\n").unwrap();
    doc.root_mut().unwrap().insert("y", 1).unwrap();
    doc.root_mut().unwrap().insert("n", 2).unwrap();
    assert_eq!(doc.to_string(), "a: 1\ny: 1\nn: 2\n");
}

#[test]
fn a_nested_sibling_after_hundreds_of_plain_entries_still_gives_the_indentation() {
    let mut src = String::new();
    for i in 0..300 {
        src.push_str(&format!("k{i}: v\n"));
    }
    src.push_str("x:\n    y: 1\n    z:\n        - 1\n");
    let out = patched(
        &src,
        r#"[{"op":"add","path":"/new","value":{"a":{"b":1}}}]"#,
    );
    assert!(
        out.ends_with("        - 1\nnew:\n    a:\n        b: 1\n"),
        "{}",
        &out[out.len() - 80..]
    );
}

#[test]
fn removing_the_last_entry_leaves_the_kept_scalar_before_it_as_it_was() {
    let remove = r#"[{"op":"remove","path":"/y"}]"#;
    assert_eq!(
        patched_reads(
            "x: |+\n  two\n\ny: 1\n\n# end\n",
            remove,
            r#"{"x":"two\n\n"}"#
        ),
        "x: |+\n  two\n\n# end\n"
    );
    assert_eq!(
        patched_reads("x: |+\n  two\n\ny: 1\n\n", remove, r#"{"x":"two\n\n"}"#),
        "x: |+\n  two\n\n"
    );
    assert_eq!(
        patched_reads(
            "x: |+\n  two\n\ny: 1 # c\n\n\n# end\n",
            remove,
            r#"{"x":"two\n\n"}"#
        ),
        "x: |+\n  two\n\n# end\n"
    );
}

#[test]
fn removing_a_middle_entry_leaves_the_kept_scalar_before_it_as_it_was() {
    assert_eq!(
        patched_reads(
            "x: |+\n  two\n\ny: 1\n\nz: 2\n",
            r#"[{"op":"remove","path":"/y"}]"#,
            r#"{"x":"two\n\n","z":2}"#
        ),
        "x: |+\n  two\n\nz: 2\n"
    );
    // After a kept scalar nested deeper.
    assert_eq!(
        patched_reads(
            "a:\n  b: |+\n    t\n\ny: 1\n\nq: 1\n",
            r#"[{"op":"remove","path":"/y"}]"#,
            r#"{"a":{"b":"t\n\n"},"q":1}"#
        ),
        "a:\n  b: |+\n    t\n\nq: 1\n"
    );
}

/// The value node of the entry whose key is a collection.
fn collection_key_value(doc: &Document) -> oneyaml::NodeId {
    doc.root()
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_scalar().is_none())
        .map(|(_, v)| v.id())
        .unwrap()
}

// ------------------------------------------------- the whole document

#[test]
fn an_entry_with_a_collection_key_can_be_removed_and_put_back() {
    let src = "a: 1\n? [x, y]\n: 2\nc: 3\n";
    let mut doc = Document::parse(src).unwrap();
    let id = collection_key_value(&doc);
    let removed = doc.remove_node(id).unwrap();
    assert_eq!(doc.to_string(), "a: 1\nc: 3\n");
    assert_eq!(removed.key(), Some("[x, y]"));
    doc.root_mut()
        .unwrap()
        .insert_removed_at(1, removed)
        .map_err(|e| e.to_string())
        .unwrap();
    assert_eq!(doc.to_string(), src);
}

#[test]
fn an_entry_with_a_collection_key_in_a_flow_mapping_can_be_removed() {
    let mut doc = Document::parse("{a: 1, ? [x, y] : 2, c: 3}\n").unwrap();
    let id = collection_key_value(&doc);
    let removed = doc.remove_node(id).unwrap();
    assert_eq!(removed.key(), Some("[x, y]"));
    let read: Value = oneyaml::from_str(&doc.to_string()).unwrap();
    assert_eq!(read, oneyaml::from_str::<Value>("{a: 1, c: 3}").unwrap());
}

#[test]
fn replacing_the_document_with_a_kept_scalar_before_a_marker_writes_no_extra_blank_line() {
    let mut stream = Stream::parse("- a\n\n---\nb: 1\n").unwrap();
    let ops = Patch::from_json(r#"[{"op":"replace","path":"","value":{"k":"two\n\n"}}]"#).unwrap();
    stream.apply_patch(0, &ops).unwrap();
    assert_eq!(stream.to_string(), "k: |+\n  two\n\n---\nb: 1\n");
    let mut doc = Document::parse("a: 1\n\n").unwrap();
    doc.apply_patch(&ops).unwrap();
    assert_eq!(doc.to_string(), "k: |+\n  two\n\n");
    let read: Value = oneyaml::from_str(&doc.to_string()).unwrap();
    assert_eq!(read.get("k").and_then(Value::as_str), Some("two\n\n"));
}

#[test]
fn a_set_value_that_is_a_kept_scalar_takes_the_blank_lines_after_the_old_one() {
    let mut doc = Document::parse("a:\n  p: 1\n\nb: 2\n").unwrap();
    doc.set_path(&oneyaml::path!["a"], "two\n\n").unwrap();
    let read: Value = oneyaml::from_str(&doc.to_string()).unwrap();
    assert_eq!(read.get("a").and_then(Value::as_str), Some("two\n\n"));
    assert_eq!(doc.to_string(), "a: |+\n  two\n\nb: 2\n");
}

#[test]
fn a_document_that_declares_yaml_1_1_keeps_quoting_keys_a_1_1_reader_reads_as_booleans() {
    let mut doc = Document::parse("%YAML 1.1\n---\na: 1\n").unwrap();
    doc.root_mut().unwrap().insert("plain", 3).unwrap();
    doc.root_mut().unwrap().insert("y", 1).unwrap();
    doc.root_mut().unwrap().insert("n", 2).unwrap();
    assert_eq!(
        doc.to_string(),
        "%YAML 1.1\n---\na: 1\nplain: 3\n'y': 1\n'n': 2\n"
    );
    // Declared YAML 1.2 (or nothing): plain.
    let mut doc = Document::parse("%YAML 1.2\n---\na: 1\n").unwrap();
    doc.root_mut().unwrap().insert("y", 1).unwrap();
    assert_eq!(doc.to_string(), "%YAML 1.2\n---\na: 1\ny: 1\n");
}
