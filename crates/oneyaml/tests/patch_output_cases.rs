//! Byte-exact results of patches on small documents: where a new or replaced
//! value is written plain, how a replaced root keeps the comment above it,
//! what an empty flow value, a missing final line break and a nested
//! sequence's footer comment do to an edit, how keys beyond the range of a
//! double compare, and which kind of error a second byte order mark and an
//! undeclared tag handle are. Each test is one input and the exact text
//! expected.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{Document, ErrorKind, MergePatch, Patch, Stream, Value, code};

/// `src` after the JSON Patch `ops`, which must apply.
fn patch(src: &str, ops: &str) -> String {
    let mut s = Stream::from_slice(src.as_bytes()).unwrap();
    s.apply_patch(0, &Patch::from_json(ops).unwrap())
        .unwrap_or_else(|e| panic!("{src:?} {ops}: {e}"));
    s.to_string()
}

/// `src` after the merge patch `doc`, which must apply.
fn merge(src: &str, doc: &str) -> String {
    let mut s = Stream::from_slice(src.as_bytes()).unwrap();
    s.apply_merge_patch(0, &MergePatch::from_json(doc).unwrap())
        .unwrap_or_else(|e| panic!("{src:?} {doc}: {e}"));
    s.to_string()
}

#[test]
fn a_merge_patch_on_a_sequence_writes_plain_keys() {
    // `n` is a string under the Core schema; only a 1.1 reader reads a
    // boolean, so the key needs no quotes.
    assert_eq!(merge("- a\n- b\n", r#"{"n":1}"#), "n: 1\n");
}

#[test]
fn strings_that_yaml_1_1_reads_as_other_types_are_added_plain_in_core() {
    let ops = r#"[
        {"op":"add","path":"/c","value":"yes"},
        {"op":"add","path":"/d","value":"2024-05-01"},
        {"op":"add","path":"/e","value":"1_000"},
        {"op":"add","path":"/f","value":"0b101"}
    ]"#;
    assert_eq!(
        patch("a: x\nb: y\n", ops),
        "a: x\nb: y\nc: yes\nd: 2024-05-01\ne: 1_000\nf: 0b101\n"
    );
}

#[test]
fn replacing_the_root_mapping_keeps_the_comment_above_it() {
    assert_eq!(
        patch(
            "# head\na: 1\nb: 2\n",
            r#"[{"op":"replace","path":"","value":{"q":1}}]"#
        ),
        "# head\nq: 1\n"
    );
}

#[test]
fn setting_the_value_of_a_flow_key_with_an_empty_value() {
    assert_eq!(
        patch(
            "{a: 1, b:, c: 3}\n",
            r#"[{"op":"replace","path":"/b","value":2}]"#
        ),
        "{a: 1, b: 2, c: 3}\n"
    );
}

#[test]
fn a_key_added_after_a_block_scalar_with_no_final_line_break() {
    let add = r#"[{"op":"add","path":"/zz","value":"v"}]"#;
    assert_eq!(patch("foo: |-\n  bar", add), "foo: |-\n  bar\nzz: v");
    assert_eq!(patch("foo: |\n  x\n   ", add), "foo: |\n  x\n   \nzz: v");
}

#[test]
fn an_item_added_after_a_keep_scalar_of_one_line_of_spaces_with_no_final_line_break() {
    assert_eq!(
        patch("- |+\n   ", r#"[{"op":"add","path":"/-","value":"zz"}]"#),
        "- |+\n   \n- zz"
    );
}

#[test]
fn removing_the_only_item_of_a_nested_sequence_keeps_the_footer_comment() {
    assert_eq!(
        patch(
            "-\n  - []\n  # note\n",
            r#"[{"op":"remove","path":"/0/0"}]"#
        ),
        "- []\n  # note\n"
    );
}

#[test]
fn a_merge_patch_into_a_tagged_marker_keeps_the_comment_after_it() {
    assert_eq!(merge("--- !!null # c\n", r#"{"a":1}"#), "--- # c\na: 1\n");
}

#[test]
fn float_keys_beyond_a_double_are_compared_by_their_exact_value() {
    // `1e400` is a finite value and `.inf` is not: two keys. A library may
    // refuse such keys with a limit, but must not call them duplicates.
    let read = |src: &str| oneyaml::from_str::<Value>(src);
    for src in ["1e400: a\n.inf: b\n", "1e-400: a\n2e-400: b\n"] {
        match read(src) {
            Ok(v) => assert_eq!(v.as_mapping().map(|m| m.len()), Some(2), "{src:?}"),
            Err(e) => assert_eq!(e.kind(), oneyaml::ErrorKind::Limit, "{src:?}: {e}"),
        }
        // The document is not refused as having a duplicate key either.
        match Document::parse(src) {
            Ok(d) => assert!(!d.has_errors(), "{src:?}"),
            Err(e) => assert_eq!(e.kind(), ErrorKind::Limit, "{src:?}: {e}"),
        }
    }
    // The same key twice is still a duplicate.
    assert!(read("1e400: a\n1.0e+400: b\n").is_err());
    assert!(read("1e-400: a\n1e-400: b\n").is_err());
}

#[test]
fn several_byte_order_marks_before_a_document_are_valid() {
    // Every document prefix may start with a mark (YAML 1.2.2 productions
    // 202 and 211), so marks in a row read as the single-mark form.
    let one = oneyaml::from_str::<Value>("\u{feff}a: 1\n").unwrap();
    for src in [
        "\u{feff}\u{feff}a: 1\n",
        "\u{feff}\u{feff}\u{feff}a: 1\n",
        "\u{feff}\n\u{feff}\na: 1\n",
    ] {
        assert_eq!(oneyaml::from_str::<Value>(src).unwrap(), one, "{src:?}");
        assert!(!Document::parse(src).unwrap().has_errors(), "{src:?}");
    }
    // A mark in the middle of a line is still misplaced.
    let e = oneyaml::from_str::<Value>("a: 1 \u{feff}\n").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Syntax);
    assert_eq!(e.code(), code::MISPLACED_BYTE_ORDER_MARK);
}

#[test]
fn an_undeclared_tag_handle_is_a_syntax_error() {
    for src in ["!x!y z\n", "a: !x!y z\n", "- !x!y z\n"] {
        let e = oneyaml::from_str::<Value>(src).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Syntax, "{src:?}");
        assert_eq!(e.code(), code::UNDECLARED_TAG_HANDLE, "{src:?}");
    }
    // Declared, it reads.
    assert!(oneyaml::from_str::<Value>("%TAG !x! tag:t,2000:\n---\n!x!y z\n").is_ok());
}

/// A block scalar with no lines, followed by a line of white space that ends
/// the input, takes the new entry before that white space (the text after
/// the entry is the same white space as before).
#[test]
fn a_block_scalar_with_no_lines_and_a_last_line_of_white_space_still_takes_an_append() {
    let add = r#"[{"op":"add","path":"/zz","value":"v"}]"#;
    for (src, want) in [
        ("a: 1\nk: |\n ", "a: 1\nk: |\nzz: v\n "),
        ("a: 1\nk: |\n\t", "a: 1\nk: |\nzz: v\n\t"),
        ("a: 1\nk: |-\n ", "a: 1\nk: |-\nzz: v\n "),
        ("a: 1\nk: |-\n\t", "a: 1\nk: |-\nzz: v\n\t"),
        ("a: 1\nk: >\n ", "a: 1\nk: >\nzz: v\n "),
        ("a: 1\nk: |2\n  ", "a: 1\nk: |2\nzz: v\n  "),
        ("a: 1\nk: | # c\n ", "a: 1\nk: | # c\nzz: v\n "),
        ("k: |\n ", "k: |\nzz: v\n "),
        ("a: 1\nk: |+\n\t", "a: 1\nk: |+\nzz: v\n\t"),
    ] {
        assert_eq!(patch(src, add), want, "{src:?}");
    }
    assert_eq!(
        patch("- a\n- |\n ", r#"[{"op":"add","path":"/-","value":"zz"}]"#),
        "- a\n- |\n- zz\n "
    );
    // A keep scalar whose only line is spaces owns that line: the new entry
    // goes after it, and the input still ends without a break.
    assert_eq!(patch("a: 1\nk: |+\n ", add), "a: 1\nk: |+\n \nzz: v");
}

/// A multi-line value appended after a block scalar with no final line
/// break is written as a block scalar, and the input then ends with that
/// scalar's own line break (the one exception to "a missing final break
/// stays missing").
#[test]
fn a_block_scalar_appended_after_an_open_scalar_ends_with_its_line_break() {
    let add = r#"[{"op":"add","path":"/zz","value":"a\nb"}]"#;
    assert_eq!(
        patch("foo: |-\n  bar", add),
        "foo: |-\n  bar\nzz: |-\n  a\n  b\n"
    );
}

/// Only an empty root node that holds just properties keeps the comment on
/// its `---` line when it is replaced by a collection; every other root
/// keeps refusing the replacement, as the comment would be re-owned.
#[test]
fn a_comment_on_the_marker_line_stays_only_for_an_empty_root() {
    assert_eq!(merge("--- !!null # c\n", r#"{"a":1}"#), "--- # c\na: 1\n");
    for src in ["--- foo # c\n", "--- [a] # c\n", "--- {a: 1} # c\n"] {
        let mut d = Document::parse(src).unwrap();
        let e = d.set_root(vec![1]).unwrap_err();
        assert!(
            matches!(e, oneyaml::EditError::CommentReattaches { .. }),
            "{src:?}: {e:?}"
        );
        assert_eq!(d.to_string(), src);
    }
}

/// Reading with any YAML 1.1 setting keeps the every-reader quoting of new
/// strings: the plain form applies to a plain Core document.
#[test]
fn new_strings_stay_quoted_under_any_yaml_1_1_setting() {
    use oneyaml::{Compat, Settings};
    let add = |s: &Settings, v: &str| {
        let mut d = Document::parse_with("a: x\n", s).unwrap();
        d.root_mut().unwrap().insert("b", v).unwrap();
        d.to_string()
    };
    let plain = Settings::default();
    let others = [
        Compat::yaml_1_1(),
        Settings::default().yaml11_booleans(true),
        Settings::default().legacy_octal(true),
        Settings::default().sexagesimal(true),
        Settings::default().digit_separators(true),
        Settings::default().yaml11_int_forms(true),
    ];
    for v in ["2024-05-01", "=", "1,000", "0o8", "yes"] {
        assert_eq!(add(&plain, v), format!("a: x\nb: {v}\n"), "{v:?}");
        for s in &others {
            assert_eq!(add(s, v), format!("a: x\nb: '{v}'\n"), "{v:?}");
        }
    }
    // A document that declares YAML 1.1 keeps the quotes too.
    let mut d = Document::parse("%YAML 1.1\n---\na: x\n").unwrap();
    d.root_mut().unwrap().insert("b", "yes").unwrap();
    assert_eq!(d.to_string(), "%YAML 1.1\n---\na: x\nb: 'yes'\n");
}
