//! What an insertion or a removal must keep doing: the limits of the rules in
//! `insert_style.rs` (what is not copied, what is still quoted, what stays).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Patch, Settings, Value};

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

#[test]
fn what_is_not_a_string_is_never_quoted() {
    let src = "a: \"x\"\nb: \"y\"\n";
    assert_eq!(
        patched(src, r#"[{"op":"add","path":"/c","value":3}]"#),
        "a: \"x\"\nb: \"y\"\nc: 3\n"
    );
    assert_eq!(
        patched(src, r#"[{"op":"add","path":"/c","value":true}]"#),
        "a: \"x\"\nb: \"y\"\nc: true\n"
    );
    assert_eq!(
        patched(src, r#"[{"op":"add","path":"/c","value":null}]"#),
        "a: \"x\"\nb: \"y\"\nc: null\n"
    );
}

#[test]
fn text_that_needs_escapes_or_a_block_stays_so_whatever_the_siblings_do() {
    let out = patched_reads(
        "a: 'x'\nb: 'y'\n",
        r#"[{"op":"add","path":"/c","value":"bell\u0007"}]"#,
        r#"{"a":"x","b":"y","c":"bell\u0007"}"#,
    );
    assert!(
        out.contains("c: \"bell\\a\"") || out.contains("c: \"bell\\x07\""),
        "{out:?}"
    );
    let out = patched_reads(
        "a: 'x'\nb: 'y'\n",
        r#"[{"op":"add","path":"/c","value":"two\nlines"}]"#,
        r#"{"a":"x","b":"y","c":"two\nlines"}"#,
    );
    assert_eq!(out, "a: 'x'\nb: 'y'\nc: |-\n  two\n  lines\n");
}

#[test]
fn the_strings_of_a_new_collection_are_not_siblings_of_the_old_ones() {
    assert_eq!(
        patched(
            "a: \"x\"\nb: \"y\"\n",
            r#"[{"op":"add","path":"/c","value":{"k":"v"}}]"#
        ),
        "a: \"x\"\nb: \"y\"\nc:\n  k: v\n"
    );
}

#[test]
fn a_key_that_would_not_read_back_as_that_string_is_still_quoted() {
    for key in [
        "true", "null", "1", "1.5", "~", "a: b", "#c", " x", "x ", "", "- a", "a #b",
    ] {
        let ops = format!(
            r#"[{{"op":"add","path":"/{}","value":1}}]"#,
            key.replace('~', "~0").replace('"', "\\\"")
        );
        let out = patched("a: 0\n", &ops);
        let read: Value = oneyaml::from_str(&out).unwrap();
        assert_eq!(
            read.get(key).and_then(Value::as_i64),
            Some(1),
            "{key:?} gave {out:?}"
        );
        assert_eq!(out.lines().count(), 2, "{key:?} gave {out:?}");
    }
}

#[test]
fn with_yaml_1_1_booleans_on_those_keys_are_quoted() {
    let settings = Settings::new().yaml11_booleans(true);
    let mut doc = Document::parse_with("a: 1\n", &settings).unwrap();
    doc.root_mut().unwrap().insert("y", 1).unwrap();
    assert_eq!(doc.to_string(), "a: 1\n'y': 1\n");
}

#[test]
fn a_new_string_value_is_still_quoted_when_a_yaml_1_1_reader_reads_a_boolean() {
    let mut doc = Document::parse("a: 1\n").unwrap();
    doc.root_mut().unwrap().insert("b", "no").unwrap();
    assert_eq!(doc.to_string(), "a: 1\nb: 'no'\n");
}

// ----------------------------------------------------------- the layout

#[test]
fn text_of_one_line_reads_no_siblings_for_its_layout() {
    // A mapping of many plain entries and no nested one: adding a plain
    // value must not depend on how wide the mapping is.
    let mut src = String::new();
    for i in 0..5000 {
        src.push_str(&format!("k{i}: v\n"));
    }
    let out = patched(&src, r#"[{"op":"add","path":"/new","value":"z"}]"#);
    assert!(out.ends_with("k4999: v\nnew: z\n"));
}

// ------------------------------------------------------------- removing

#[test]
fn blank_lines_after_a_removed_entry_stay_when_no_kept_scalar_comes_before() {
    // The blank line above the removed entry goes with it; the one below
    // stays, as before.
    assert_eq!(
        patched(
            "x: |\n  two\n\ny: 1\n\n\nz: 2\n",
            r#"[{"op":"remove","path":"/y"}]"#
        ),
        "x: |\n  two\n\n\nz: 2\n"
    );
}

#[test]
fn removing_the_first_entry_takes_its_own_leading_comment() {
    let mut doc = Document::parse("# lead\nk: 1\nj: 2\n").unwrap();
    let removed = doc.root_mut().unwrap().remove("k").unwrap();
    assert_eq!(doc.to_string(), "j: 2\n");
    assert_eq!(removed.comments(), ["# lead"]);
    // A comment set apart by a blank line is the document's header.
    let mut doc = Document::parse("# header\n\nk: 1\nj: 2\n").unwrap();
    let removed = doc.root_mut().unwrap().remove("k").unwrap();
    assert_eq!(doc.to_string(), "# header\n\nj: 2\n");
    assert!(removed.comments().is_empty());
}
