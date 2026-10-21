//! RFC 6902 patches, RFC 7396 merge patches and RFC 6901 pointers on a
//! lossless document.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{
    Document, Limits, MergePatch, Patch, PatchError, PatchErrorKind, PatchOp, Pointer, Settings,
    Stream, Value,
};

fn patch(json: &str) -> Patch {
    Patch::from_json(json).unwrap()
}

fn merge(json: &str) -> MergePatch {
    MergePatch::from_json(json).unwrap()
}

/// The text of `src` after applying `ops`, which must succeed.
fn applied(src: &str, ops: &str) -> String {
    let mut doc = Document::parse(src).unwrap();
    doc.apply_patch(&patch(ops)).unwrap();
    doc.to_string()
}

/// The error `ops` fails with on `src`, after checking the document did not change.
fn refused(src: &str, ops: &str) -> PatchError {
    let mut doc = Document::parse(src).unwrap();
    let err = doc.apply_patch(&patch(ops)).unwrap_err();
    assert_eq!(
        doc.to_string(),
        src,
        "a failed patch must leave the bytes alone"
    );
    err
}

fn kind_of(src: &str, ops: &str) -> PatchErrorKind {
    refused(src, ops).kind()
}

// ---- pointers -------------------------------------------------------------

#[test]
fn pointer_tokens_unescape_and_display_escapes() {
    let p = Pointer::parse("/a~1b/~0c/0//~01").unwrap();
    assert_eq!(p.tokens(), ["a/b", "~c", "0", "", "~1"]);
    assert_eq!(p.to_string(), "/a~1b/~0c/0//~01");
    assert!(Pointer::parse("").unwrap().is_root());
    assert_eq!(Pointer::from_tokens(["x/y", "z"]).to_string(), "/x~1y/z");
    assert_eq!(Pointer::root().with("k").tokens(), ["k"]);
}

#[test]
fn pointer_text_must_start_with_a_slash_and_escape_properly() {
    for bad in ["a", "a/b", "/a~", "/a~2", "/~x/y"] {
        let e = Pointer::parse(bad).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::Invalid, "{bad}");
    }
}

#[test]
fn a_pointer_of_many_tokens_is_read_in_linear_time_and_named_briefly() {
    let long = "/a".repeat(320_000);
    let p = Pointer::parse(&long).unwrap();
    assert_eq!(p.tokens().len(), 320_000);
    let err = refused(
        "x: 1\n",
        &format!(r#"[{{"op": "remove", "path": "{long}"}}]"#),
    );
    assert_eq!(err.kind(), PatchErrorKind::Path);
    assert!(err.pointer().unwrap().len() < 300);
    assert!(err.to_string().len() < 600);
}

#[test]
fn escaped_tokens_name_keys_with_slashes_and_tildes() {
    let src = "a/b: 1\nm~n: 2\n";
    assert_eq!(
        applied(src, r#"[{"op": "replace", "path": "/a~1b", "value": 10}]"#),
        "a/b: 10\nm~n: 2\n"
    );
    assert_eq!(
        applied(src, r#"[{"op": "replace", "path": "/m~0n", "value": 20}]"#),
        "a/b: 1\nm~n: 20\n"
    );
    let doc = Document::parse(src).unwrap();
    assert_eq!(doc.get_pointer("/m~0n").unwrap().as_i64(), Some(2));
}

#[test]
fn tokens_match_keys_by_their_scalar_text() {
    // `80`, `true`, `0x10` and `1` are addressed by their spelling.
    assert_eq!(
        applied(
            "80: a\nb: c\n",
            r#"[{"op": "replace", "path": "/80", "value": "x"}]"#
        ),
        "80: x\nb: c\n"
    );
    assert_eq!(
        applied(
            "true: a\nb: c\n",
            r#"[{"op": "replace", "path": "/true", "value": "x"}]"#
        ),
        "true: x\nb: c\n"
    );
    assert_eq!(
        applied(
            "0x10: a\nb: c\n",
            r#"[{"op": "replace", "path": "/0x10", "value": "x"}]"#
        ),
        "0x10: x\nb: c\n"
    );
    // `1` and `1.0` are different texts: no ambiguity.
    assert_eq!(
        applied(
            "1: a\n1.0: b\n",
            r#"[{"op": "replace", "path": "/1", "value": "x"}]"#
        ),
        "1: x\n1.0: b\n"
    );
    // The value of the key does not matter: 0x10 is not the token 16.
    assert_eq!(
        kind_of(
            "0x10: a\n",
            r#"[{"op": "replace", "path": "/16", "value": "x"}]"#
        ),
        PatchErrorKind::Path
    );
    // A quoted key has the text between its quotes.
    assert_eq!(
        applied(
            "\"80\": a\n",
            r#"[{"op": "replace", "path": "/80", "value": "x"}]"#
        ),
        "\"80\": x\n"
    );
}

#[test]
fn two_keys_with_one_text_are_ambiguous() {
    let src = "80: a\n\"80\": b\n";
    for ops in [
        r#"[{"op": "replace", "path": "/80", "value": "c"}]"#,
        r#"[{"op": "remove", "path": "/80"}]"#,
        r#"[{"op": "add", "path": "/80", "value": "c"}]"#,
        r#"[{"op": "test", "path": "/80", "value": "a"}]"#,
        r#"[{"op": "copy", "from": "/80", "path": "/x"}]"#,
    ] {
        assert_eq!(kind_of(src, ops), PatchErrorKind::RefusedAmbiguous, "{ops}");
    }
    // A null key written empty and a quoted empty key both have the empty text.
    assert_eq!(
        kind_of(
            "\"\": 1\n? # no content\n: 2\nb: 3\n",
            r#"[{"op": "replace", "path": "/", "value": 9}]"#
        ),
        PatchErrorKind::RefusedAmbiguous
    );
    // Ambiguity deeper than the first token, and after a good edit.
    assert_eq!(
        kind_of(
            "a: 1\nm:\n  80: 1\n  \"80\": 2\n",
            r#"[{"op": "replace", "path": "/a", "value": 2},
                {"op": "remove", "path": "/m/80"}]"#
        ),
        PatchErrorKind::RefusedAmbiguous
    );
}

#[test]
fn a_key_that_is_a_collection_is_not_addressable() {
    let src = "? [a, b]\n: x\nk: y\n";
    assert_eq!(
        kind_of(src, r#"[{"op": "replace", "path": "/a", "value": 1}]"#),
        PatchErrorKind::Path
    );
    assert_eq!(
        applied(src, r#"[{"op": "replace", "path": "/k", "value": 1}]"#),
        "? [a, b]\n: x\nk: 1\n"
    );
}

#[test]
fn get_pointer_reads_and_follows_aliases() {
    let doc = Document::parse("base: &b\n  x: [1, 2]\ncopy: *b\nn: ~\n").unwrap();
    assert_eq!(doc.get_pointer("/base/x/1").unwrap().as_i64(), Some(2));
    assert_eq!(doc.get_pointer("/copy/x/0").unwrap().as_i64(), Some(1));
    assert!(doc.get_pointer("/n").unwrap().is_null());
    assert_eq!(doc.get_pointer("").unwrap().id(), doc.root().unwrap().id());
    assert_eq!(
        doc.get_pointer("/zz").unwrap_err().kind(),
        PatchErrorKind::Path
    );
    assert_eq!(
        doc.get_pointer("/base/x/2").unwrap_err().kind(),
        PatchErrorKind::Path
    );
    assert_eq!(
        doc.get_pointer("/base/x/-").unwrap_err().kind(),
        PatchErrorKind::Path
    );
    assert_eq!(
        doc.get_pointer("/base/x/01").unwrap_err().kind(),
        PatchErrorKind::Path
    );
    assert_eq!(
        doc.get_pointer("x").unwrap_err().kind(),
        PatchErrorKind::Invalid
    );
    let amb = Document::parse("1: a\n'1': b\n").unwrap();
    assert_eq!(
        amb.get_pointer("/1").unwrap_err().kind(),
        PatchErrorKind::RefusedAmbiguous
    );
}

// ---- the operations -------------------------------------------------------

#[test]
fn add_puts_a_new_member_at_the_end_and_keeps_everything_else() {
    assert_eq!(
        applied(
            "# top\na: 1   # one\nb: 2  # two\n# tail\n",
            r#"[{"op": "add", "path": "/c", "value": 3}]"#
        ),
        "# top\na: 1   # one\nb: 2  # two\nc: 3\n# tail\n"
    );
    assert_eq!(
        applied(
            "a: {x: 1}\n",
            r#"[{"op": "add", "path": "/a/z", "value": 2}]"#
        ),
        "a: {x: 1, z: 2}\n"
    );
}

#[test]
fn add_on_an_existing_member_replaces_its_value() {
    assert_eq!(
        applied(
            "a: 1  # c\nb: 2\n",
            r#"[{"op": "add", "path": "/a", "value": 9}]"#
        ),
        "a: 9  # c\nb: 2\n"
    );
}

#[test]
fn add_to_a_sequence_inserts_at_an_index_or_appends() {
    let src = "- a\n- b\n- c\n";
    let add = |path: &str| {
        applied(
            src,
            &format!(r#"[{{"op": "add", "path": "{path}", "value": "x"}}]"#),
        )
    };
    assert_eq!(add("/0"), "- x\n- a\n- b\n- c\n");
    assert_eq!(add("/1"), "- a\n- x\n- b\n- c\n");
    assert_eq!(add("/3"), "- a\n- b\n- c\n- x\n");
    assert_eq!(add("/-"), "- a\n- b\n- c\n- x\n");
    assert_eq!(
        applied(
            "k: [1, 2]\n",
            r#"[{"op": "add", "path": "/k/-", "value": 3}]"#
        ),
        "k: [1, 2, 3]\n"
    );
}

#[test]
fn add_with_a_bad_location_is_a_path_error() {
    let src = "a: 1\nl: [x]\nn: ~\n";
    for (path, why) in [
        ("/l/2", "past the end"),
        ("/l/01", "leading zero"),
        ("/l/x", "not a number"),
        ("/l/-1", "signed"),
        ("/missing/child", "missing parent"),
        ("/a/child", "below a scalar"),
        ("/n/child", "below null"),
    ] {
        let ops = format!(r#"[{{"op": "add", "path": "{path}", "value": 1}}]"#);
        assert_eq!(kind_of(src, &ops), PatchErrorKind::Path, "{why}");
    }
    assert_eq!(
        kind_of("", r#"[{"op": "add", "path": "/a", "value": 1}]"#),
        PatchErrorKind::Path
    );
}

#[test]
fn remove_takes_out_a_member_or_an_item() {
    assert_eq!(
        applied(
            "a: 1  # one\nb: 2\nc: 3\n",
            r#"[{"op": "remove", "path": "/b"}]"#
        ),
        "a: 1  # one\nc: 3\n"
    );
    assert_eq!(
        applied("- a\n- b\n- c\n", r#"[{"op": "remove", "path": "/1"}]"#),
        "- a\n- c\n"
    );
    assert_eq!(
        kind_of("a: 1\n", r#"[{"op": "remove", "path": "/zz"}]"#),
        PatchErrorKind::Path
    );
    assert_eq!(
        kind_of("- a\n", r#"[{"op": "remove", "path": "/1"}]"#),
        PatchErrorKind::Path
    );
    assert_eq!(
        kind_of("- a\n", r#"[{"op": "remove", "path": "/-"}]"#),
        PatchErrorKind::Path
    );
}

#[test]
fn replace_changes_one_value_and_needs_it_to_exist() {
    assert_eq!(
        applied(
            "port: 80  # http\nhost: h\n",
            r#"[{"op": "replace", "path": "/port", "value": 9090}]"#
        ),
        "port: 9090  # http\nhost: h\n"
    );
    assert_eq!(
        kind_of("a: 1\n", r#"[{"op": "replace", "path": "/b", "value": 1}]"#),
        PatchErrorKind::Path
    );
}

#[test]
fn replace_of_the_whole_document_keeps_what_is_outside_the_value() {
    assert_eq!(
        applied(
            "--- # c\n",
            r#"[{"op": "replace", "path": "", "value": {"a": 1}}]"#
        ),
        "--- # c\na: 1\n"
    );
    assert_eq!(
        applied(
            "--- |\n  old\n",
            r#"[{"op": "replace", "path": "", "value": "new\n"}]"#
        ),
        "--- |\n  new\n"
    );
    assert_eq!(
        applied(
            "# head\n\na: 1\n# foot\n",
            r#"[{"op": "add", "path": "", "value": [1, 2]}]"#
        )
        .lines()
        .next(),
        Some("# head")
    );
    assert_eq!(
        kind_of("a: 1\n", r#"[{"op": "remove", "path": ""}]"#),
        PatchErrorKind::Invalid
    );
}

#[test]
fn move_removes_then_adds() {
    let moved = |src: &str, from: &str, to: &str| {
        applied(
            src,
            &format!(r#"[{{"op": "move", "from": "{from}", "path": "{to}"}}]"#),
        )
    };
    assert_eq!(moved("a: 1\nb: 2\n", "/a", "/c"), "b: 2\nc: 1\n");
    assert_eq!(moved("a: 1\nb: 2\n", "/a", "/b"), "b: 1\n");
    assert_eq!(
        moved("a:\n  x: 1\nb: 2\n", "/a", "/c"),
        "b: 2\nc:\n  x: 1\n"
    );
    assert_eq!(
        moved("a:\n  x: 1\n  y: 2\nb: 3\n", "/a/x", "/z"),
        "a:\n  y: 2\nb: 3\nz: 1\n"
    );
    assert_eq!(moved("- a\n- b\n- c\n", "/0", "/2"), "- b\n- c\n- a\n");
    assert_eq!(moved("- a\n- b\n- c\n", "/2", "/0"), "- c\n- a\n- b\n");
    assert_eq!(moved("k: [1, 2]\nm: x\n", "/k/0", "/m"), "k: [2]\nm: 1\n");
    // A move onto itself changes nothing.
    assert_eq!(moved("a: 1\n", "/a", "/a"), "a: 1\n");
    // Two moves that end where they started.
    assert_eq!(
        applied(
            "a: 1\n",
            r#"[{"op": "move", "from": "/a", "path": "/b"}, {"op": "move", "from": "/b", "path": "/a"}]"#
        ),
        "a: 1\n"
    );
}

#[test]
fn move_errors() {
    assert_eq!(
        kind_of(
            "a:\n  b: 1\n",
            r#"[{"op": "move", "from": "/a", "path": "/a/c"}]"#
        ),
        PatchErrorKind::Invalid
    );
    assert_eq!(
        kind_of("a: 1\n", r#"[{"op": "move", "from": "/zz", "path": "/b"}]"#),
        PatchErrorKind::Path
    );
    assert_eq!(
        kind_of(
            "a: 1\nb: 2\n",
            r#"[{"op": "replace", "path": "/b", "value": 9},
                {"op": "move", "from": "/a", "path": "/x/y"}]"#
        ),
        PatchErrorKind::Path
    );
}

#[test]
fn copy_writes_the_value_without_changing_the_source() {
    let copied = |src: &str, from: &str, to: &str| {
        applied(
            src,
            &format!(r#"[{{"op": "copy", "from": "{from}", "path": "{to}"}}]"#),
        )
    };
    assert_eq!(copied("a: 1\nb: 2\n", "/a", "/c"), "a: 1\nb: 2\nc: 1\n");
    assert_eq!(copied("a: 1\nb: 2\n", "/a", "/b"), "a: 1\nb: 1\n");
    assert_eq!(
        copied("a:\n  x: 1\n", "/a", "/b"),
        "a:\n  x: 1\nb:\n  x: 1\n"
    );
    assert_eq!(copied("- a\n- b\n", "/0", "/-"), "- a\n- b\n- a\n");
    // A copy is its own node.
    assert_eq!(
        applied(
            "a: 1\n",
            r#"[{"op": "copy", "from": "/a", "path": "/b"}, {"op": "replace", "path": "/b", "value": 9}]"#
        ),
        "a: 1\nb: 9\n"
    );
    assert_eq!(
        kind_of("a: 1\n", r#"[{"op": "copy", "from": "/zz", "path": "/b"}]"#),
        PatchErrorKind::Path
    );
}

#[test]
fn copy_of_a_value_with_aliases_writes_them_out() {
    // The alias names an anchor that stays where it is; the copy is a value.
    assert_eq!(
        applied(
            "base: &b\n  x: 1\ncopy: *b\n",
            r#"[{"op": "copy", "from": "/copy", "path": "/z"}]"#
        ),
        "base: &b\n  x: 1\ncopy: *b\nz:\n  x: 1\n"
    );
    // A copy through an alias reads the shared value.
    assert_eq!(
        applied(
            "base: &b\n  x: 1\ncopy: *b\n",
            r#"[{"op": "copy", "from": "/copy/x", "path": "/d"}]"#
        ),
        "base: &b\n  x: 1\ncopy: *b\nd: 1\n"
    );
    // An alias is followed to the anchor defined last before it.
    assert_eq!(
        applied(
            "a: &b\n se: &b\n   1\ncopy: *b\n",
            r#"[{"op": "copy", "from": "/copy", "path": "/z"}]"#
        ),
        "a: &b\n se: &b\n   1\ncopy: *b\nz: 1\n"
    );
}

// ---- test ------------------------------------------------------------------

fn passes(src: &str, path: &str, expected: &str) -> bool {
    let mut doc = Document::parse(src).unwrap();
    let ops = format!(r#"[{{"op": "test", "path": "{path}", "value": {expected}}}]"#);
    match doc.apply_patch(&patch(&ops)) {
        Ok(()) => true,
        Err(e) => {
            assert_eq!(e.kind(), PatchErrorKind::Test, "{src} {expected}");
            false
        }
    }
}

#[test]
fn test_compares_by_the_yaml_data_model() {
    // The same string in any style.
    assert!(passes("a: x\n", "/a", r#""x""#));
    assert!(passes("a: 'x'\n", "/a", r#""x""#));
    assert!(passes("a: |\n  x\n", "/a", r#""x\n""#));
    // Integers by value, however written; an integer is not a float.
    assert!(passes("a: 0x1F\n", "/a", "31"));
    assert!(!passes("a: 1\n", "/a", "1.0"));
    assert!(!passes("a: 1.0\n", "/a", "1"));
    assert!(passes("a: 1.0\n", "/a", "1.0"));
    // Types are not mixed up.
    assert!(!passes("a: 1\n", "/a", r#""1""#));
    assert!(!passes("a: '1'\n", "/a", "1"));
    assert!(passes("a: yes\n", "/a", r#""yes""#));
    assert!(!passes("a: yes\n", "/a", "true"));
    assert!(passes("a: !!str 1\n", "/a", r#""1""#));
    assert!(!passes("a: !!str 1\n", "/a", "1"));
    // Null, and an empty value.
    assert!(passes("a:\n", "/a", "null"));
    assert!(passes("a: ~\n", "/a", "null"));
    // Mappings ignore key order, sequences do not.
    assert!(passes("a: {x: 1, y: 2}\n", "/a", r#"{"y": 2, "x": 1}"#));
    assert!(!passes("a: {x: 1, y: 2}\n", "/a", r#"{"x": 1}"#));
    assert!(passes("a: [1, 2]\n", "/a", "[1, 2]"));
    assert!(!passes("a: [1, 2]\n", "/a", "[2, 1]"));
    assert!(passes(
        "a:\n  - {k: 1}\n  - {k: 2}\n",
        "/a",
        r#"[{"k": 1}, {"k": 2}]"#
    ));
    // A mapping key matches a string only when it is one under the schema.
    assert!(!passes("1: a\n", "", r#"{"1": "a"}"#));
    assert!(passes("'1': a\n", "", r#"{"1": "a"}"#));
    // Reads follow aliases.
    assert!(passes("base: &b [1, 2]\ncopy: *b\n", "/copy", "[1, 2]"));
    assert!(passes("base: &b {x: 1}\ncopy: *b\n", "/copy/x", "1"));
    // The whole document.
    assert!(passes("a: 1\n", "", r#"{"a": 1}"#));
}

#[test]
fn a_failed_test_is_a_test_error_and_stops_the_patch() {
    let err = refused(
        "a: 1\nb: 2\n",
        r#"[{"op": "test", "path": "/a", "value": 2}, {"op": "replace", "path": "/b", "value": 9}]"#,
    );
    assert_eq!(err.kind(), PatchErrorKind::Test);
    assert_eq!(err.op_index(), Some(0));
    assert_eq!(err.operation(), Some("test"));
    assert_eq!(err.pointer(), Some("/a"));
    assert_eq!(
        kind_of("a: 1\n", r#"[{"op": "test", "path": "/nope", "value": 1}]"#),
        PatchErrorKind::Path
    );
    // A test, then an edit that depends on it.
    assert_eq!(
        applied(
            "a: 1\nb: 2\n",
            r#"[{"op": "test", "path": "/a", "value": 1}, {"op": "replace", "path": "/b", "value": 9}]"#
        ),
        "a: 1\nb: 9\n"
    );
}

#[test]
fn a_patch_of_tests_only_reads_a_shared_document() {
    let stream = Stream::parse("a: 1\n").unwrap();
    let mut shared = stream.document(0).unwrap();
    shared
        .apply_patch(&patch(r#"[{"op": "test", "path": "/a", "value": 1}]"#))
        .unwrap();
    assert!(
        shared
            .apply_patch(&patch(r#"[{"op": "test", "path": "/a", "value": 2}]"#))
            .is_err()
    );
    assert_eq!(shared.to_string(), "a: 1\n");
}

// ---- atomicity -------------------------------------------------------------

#[test]
fn a_late_failure_undoes_every_earlier_operation() {
    let src = "# c\na: 1  # one\nb:\n  - x\n  - y\nc: {k: v}\n";
    let good = r#"{"op": "replace", "path": "/a", "value": 99},
        {"op": "add", "path": "/d", "value": [1, 2]},
        {"op": "remove", "path": "/b/0"},
        {"op": "copy", "from": "/c", "path": "/e"},
        {"op": "move", "from": "/c/k", "path": "/f"}"#;
    // The good operations change the document ...
    let changed = applied(src, &format!("[{good}]"));
    assert_ne!(changed, src);
    // ... and each kind of failure after them undoes all of it.
    for (bad, kind) in [
        (
            r#"{"op": "remove", "path": "/missing"}"#,
            PatchErrorKind::Path,
        ),
        (
            r#"{"op": "test", "path": "/a", "value": 1}"#,
            PatchErrorKind::Test,
        ),
        (
            r#"{"op": "replace", "path": "/b"}"#,
            PatchErrorKind::Invalid,
        ),
    ] {
        let ops = format!("[{good}, {bad}]");
        let result = if kind == PatchErrorKind::Invalid {
            // Not even readable: nothing is applied.
            Patch::from_json(&ops).map(|_| ())
        } else {
            let mut doc = Document::parse(src).unwrap();
            let r = doc.apply_patch(&patch(&ops)).inspect_err(|e| {
                assert_eq!(e.kind(), kind);
                assert_eq!(e.op_index(), Some(5));
            });
            assert_eq!(doc.to_string(), src);
            r
        };
        assert!(result.is_err());
    }
}

#[test]
fn an_edit_layer_refusal_late_in_a_patch_undoes_the_rest() {
    // Removing an anchored node that an alias uses is refused after a good edit.
    let src = "a: 1\nbase: &b\n  x: 1\ncopy: *b\n";
    let e = refused(
        src,
        r#"[{"op": "replace", "path": "/a", "value": 2}, {"op": "remove", "path": "/base"}]"#,
    );
    assert_eq!(e.kind(), PatchErrorKind::RefusedAlias);
    assert_eq!(e.op_index(), Some(1));
}

#[test]
fn a_failed_patch_on_a_stream_leaves_the_whole_stream_alone() {
    let src = "a: 1\n---\nb: 2\nc: 3\n---\nd: 4\n";
    let mut stream = Stream::parse(src).unwrap();
    let e = stream
        .apply_patch(
            1,
            &patch(r#"[{"op": "replace", "path": "/b", "value": 9}, {"op": "remove", "path": "/nope"}]"#),
        )
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Path);
    assert_eq!(stream.to_string(), src);
    // And the stream still edits afterwards.
    stream
        .apply_patch(
            1,
            &patch(r#"[{"op": "replace", "path": "/b", "value": 9}]"#),
        )
        .unwrap();
    assert_eq!(stream.to_string(), "a: 1\n---\nb: 9\nc: 3\n---\nd: 4\n");
}

#[test]
fn handles_to_untouched_nodes_survive_a_patch_and_a_failed_one() {
    let mut doc = Document::parse("a: 1\nb: [x]\nc: 3\n").unwrap();
    let c = doc.get("c").unwrap().id();
    doc.apply_patch(&patch(r#"[{"op": "add", "path": "/b/-", "value": "y"}]"#))
        .unwrap();
    assert_eq!(doc.node(c).unwrap().as_i64(), Some(3));
    let _ = doc.apply_patch(&patch(
        r#"[{"op": "remove", "path": "/a"}, {"op": "remove", "path": "/zz"}]"#,
    ));
    assert_eq!(doc.node(c).unwrap().as_i64(), Some(3));
}

#[test]
fn an_empty_patch_changes_nothing() {
    let mut doc = Document::parse("a: 1\n").unwrap();
    doc.apply_patch(&Patch::new(Vec::new())).unwrap();
    assert_eq!(doc.to_string(), "a: 1\n");
}

// ---- multiple documents -----------------------------------------------------

#[test]
fn a_patch_addresses_one_document_by_index() {
    let src = "a: 1\n---\nb: 2\n---\nc: 3\n";
    let mut stream = Stream::parse(src).unwrap();
    stream
        .apply_patch(1, &patch(r#"[{"op": "add", "path": "/z", "value": 0}]"#))
        .unwrap();
    assert_eq!(stream.to_string(), "a: 1\n---\nb: 2\nz: 0\n---\nc: 3\n");
    stream.apply_merge_patch(2, &merge(r#"{"c": 4}"#)).unwrap();
    assert_eq!(stream.to_string(), "a: 1\n---\nb: 2\nz: 0\n---\nc: 4\n");
    let e = stream
        .apply_patch(3, &patch(r#"[{"op": "add", "path": "/z", "value": 0}]"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Path);
    assert_eq!(e.op_index(), None);
    let e = stream.apply_merge_patch(7, &merge("{}")).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Path);
    // Through a guard.
    let mut guard = stream.document_mut(0).unwrap();
    guard
        .apply_patch(&patch(r#"[{"op": "replace", "path": "/a", "value": 10}]"#))
        .unwrap();
    guard.apply_merge_patch(&merge(r#"{"extra": 1}"#)).unwrap();
    assert_eq!(
        stream.to_string(),
        "a: 10\nextra: 1\n---\nb: 2\nz: 0\n---\nc: 4\n"
    );
}

// ---- aliases ---------------------------------------------------------------

#[test]
fn writes_below_an_alias_are_refused_whatever_the_operation() {
    let src = "base: &b\n  x: 1\ncopy: *b\nseq: &s [1, 2]\nt: *s\n";
    for ops in [
        r#"[{"op": "replace", "path": "/copy/x", "value": 2}]"#,
        r#"[{"op": "remove", "path": "/copy/x"}]"#,
        r#"[{"op": "add", "path": "/copy/y", "value": 2}]"#,
        r#"[{"op": "replace", "path": "/t/0", "value": 9}]"#,
        r#"[{"op": "add", "path": "/t/-", "value": 9}]"#,
        r#"[{"op": "move", "from": "/copy/x", "path": "/m"}]"#,
        r#"[{"op": "move", "from": "/base/x", "path": "/copy/m"}]"#,
        r#"[{"op": "copy", "from": "/base/x", "path": "/copy/m"}]"#,
    ] {
        assert_eq!(kind_of(src, ops), PatchErrorKind::RefusedAlias, "{ops}");
    }
    let nested = "- &a {x: 1}\n- *a\n";
    assert_eq!(
        kind_of(nested, r#"[{"op": "replace", "path": "/1/x", "value": 2}]"#),
        PatchErrorKind::RefusedAlias
    );
    let deep = "a: &a {k: {z: 1}}\nb:\n  c: *a\n";
    assert_eq!(
        kind_of(
            deep,
            r#"[{"op": "replace", "path": "/b/c/k/z", "value": 2}]"#
        ),
        PatchErrorKind::RefusedAlias
    );
}

#[test]
fn the_alias_node_itself_and_its_anchor_can_be_replaced() {
    // Replacing the alias node is allowed.
    assert_eq!(
        applied(
            "a: &x 1\nb: *x\n",
            r#"[{"op": "replace", "path": "/b", "value": 5}]"#
        ),
        "a: &x 1\nb: 5\n"
    );
    // Writing at the anchor shows through every alias, and the anchor stays.
    let mut doc = Document::parse("a: &x 1\nb: *x\n").unwrap();
    doc.apply_patch(&patch(r#"[{"op": "replace", "path": "/a", "value": 7}]"#))
        .unwrap();
    assert_eq!(doc.to_string(), "a: &x 7\nb: *x\n");
    assert_eq!(doc.get("b").unwrap().as_i64(), Some(7));
    // An alias node can be removed.
    assert_eq!(
        applied("a: &x 1\nb: *x\n", r#"[{"op": "remove", "path": "/b"}]"#),
        "a: &x 1\n"
    );
}

#[test]
fn removing_or_moving_an_anchored_node_that_is_still_aliased_is_refused() {
    let src = "base: &b\n  x: 1\ncopy: *b\n";
    for ops in [
        r#"[{"op": "remove", "path": "/base"}]"#,
        r#"[{"op": "move", "from": "/base", "path": "/moved"}]"#,
        r#"[{"op": "replace", "path": "/base/x", "value": 2}, {"op": "remove", "path": "/base"}]"#,
    ] {
        let e = refused(src, ops);
        assert_eq!(e.kind(), PatchErrorKind::RefusedAlias, "{ops}");
    }
    // The anchor is deeper inside what is removed.
    assert_eq!(
        kind_of(
            "l:\n  - a: &x 1\nr: *x\n",
            r#"[{"op": "remove", "path": "/l"}]"#
        ),
        PatchErrorKind::RefusedAlias
    );
    // Without aliases it goes, and the mapping it leaves is empty.
    assert_eq!(
        applied(
            "base: &b\n  x: 1\n",
            r#"[{"op": "remove", "path": "/base"}]"#
        ),
        "{}\n"
    );
}

// ---- errors ----------------------------------------------------------------

#[test]
fn error_kinds_have_stable_names() {
    use PatchErrorKind as K;
    assert_eq!(K::Path.name(), "patch-path");
    assert_eq!(K::Test.name(), "patch-test");
    assert_eq!(K::Invalid.name(), "patch-invalid");
    assert_eq!(K::RefusedAlias.name(), "refused-alias");
    assert_eq!(K::RefusedAmbiguous.name(), "refused-ambiguous");
    assert_eq!(K::Limit.name(), "limit");
    assert_eq!(K::Edit.name(), "patch-edit");
}

#[test]
fn an_error_says_which_operation_failed_and_why() {
    let e = refused(
        "a: 1\n",
        r#"[{"op": "add", "path": "/b", "value": 1}, {"op": "replace", "path": "/zz", "value": 2}]"#,
    );
    assert_eq!(e.kind(), PatchErrorKind::Path);
    assert_eq!(e.op_index(), Some(1));
    assert_eq!(e.operation(), Some("replace"));
    assert_eq!(e.pointer(), Some("/zz"));
    let text = e.to_string();
    assert!(
        text.starts_with("patch-path: operation 1 (replace)"),
        "{text}"
    );
    assert!(text.contains("/zz"), "{text}");
    assert!(!e.message().is_empty());
}

#[test]
fn a_patch_that_is_not_well_formed_is_invalid() {
    for (json, why) in [
        (r#"{"op": "add"}"#, "not an array"),
        (r#"[1]"#, "operation not an object"),
        (r#"[{"path": "/a"}]"#, "no op"),
        (r#"[{"op": 1, "path": "/a"}]"#, "op not a string"),
        (r#"[{"op": "frobnicate", "path": "/a"}]"#, "unknown op"),
        (r#"[{"op": "add", "path": "/a"}]"#, "add without a value"),
        (
            r#"[{"op": "replace", "path": "/a"}]"#,
            "replace without a value",
        ),
        (r#"[{"op": "test", "path": "/a"}]"#, "test without a value"),
        (r#"[{"op": "add", "value": 1}]"#, "no path"),
        (
            r#"[{"op": "add", "path": 3, "value": 1}]"#,
            "path not a string",
        ),
        (r#"[{"op": "move", "path": "/a"}]"#, "move without from"),
        (r#"[{"op": "copy", "from": "/a"}]"#, "copy without a path"),
        ("[{", "not readable"),
    ] {
        let e = Patch::from_json(json).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::Invalid, "{why}");
    }
    let e = Patch::from_json(r#"[{"op": "remove", "path": "/a"}, {"op": "move", "path": "/a"}]"#)
        .unwrap_err();
    assert_eq!(e.op_index(), Some(1));
    assert_eq!(e.operation(), Some("move"));
    // A malformed pointer fails at its operation, and the document is unchanged.
    let e = refused("a: 1\n", r#"[{"op": "remove", "path": "a"}]"#);
    assert_eq!(e.kind(), PatchErrorKind::Invalid);
    assert_eq!(e.op_index(), Some(0));
    let e = refused(
        "a: 1\n",
        r#"[{"op": "copy", "from": "/a", "path": "/b~2"}]"#,
    );
    assert_eq!(e.kind(), PatchErrorKind::Invalid);
    // Members an operation does not use are ignored.
    let p =
        Patch::from_json(r#"[{"op": "remove", "path": "/a", "value": 1, "from": "/z"}]"#).unwrap();
    assert_eq!(p.ops(), [PatchOp::remove("/a")]);
}

#[test]
fn a_typed_patch_is_built_without_json_text() {
    let ops = vec![
        PatchOp::test("/a", Value::from(1)),
        PatchOp::replace("/a", Value::from(2)),
        PatchOp::add("/l/-", Value::from("z")),
        PatchOp::move_value("/b", "/c"),
        PatchOp::copy("/c", "/d"),
        PatchOp::remove("/e"),
    ];
    assert_eq!(ops[3].name(), "move");
    let patch: Patch = ops.into_iter().collect();
    let mut doc = Document::parse("a: 1\nb: x\ne: gone\nl: [y]\n").unwrap();
    doc.apply_patch(&patch).unwrap();
    assert_eq!(doc.to_string(), "a: 2\nl: [y, z]\nc: x\nd: x\n");
    // The number types are the JSON ones: 1 is an integer, 1.0 a float.
    let mut doc = Document::parse("a: 1\n").unwrap();
    let e = doc
        .apply_patch(&Patch::new(vec![PatchOp::test("/a", Value::from(1.0))]))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Test);
}

#[test]
fn the_edit_layer_refusing_the_edit_is_its_own_kind() {
    // Input with encoding errors cannot be edited at all.
    let parsed = Document::from_slice_recovering(b"a: \xff\nb: 1\n", &Settings::new());
    let mut doc = parsed.into_value();
    let before = doc.to_bytes();
    let e = doc
        .apply_patch(&patch(r#"[{"op": "replace", "path": "/b", "value": 2}]"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Edit);
    assert_eq!(doc.to_bytes(), before);
    let e = doc.apply_merge_patch(&merge(r#"{"b": 2}"#)).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Edit);
    assert_eq!(doc.to_bytes(), before);
}

// ---- limits ----------------------------------------------------------------

/// A document whose last member holds `n` copies of a long string.
fn doubling_patch(copies: usize) -> String {
    let ops: Vec<String> = (0..copies)
        .map(|i| format!(r#"{{"op": "copy", "from": "", "path": "/k{i}"}}"#))
        .collect();
    format!("[{}]", ops.join(","))
}

#[test]
fn copies_are_charged_to_the_alias_byte_budget() {
    // Each copy of the whole document doubles it: forty of them would be a
    // terabyte. They are refused, and the document is as it was. (A smaller
    // budget than the default keeps the test quick; the default is the same
    // check at a larger size.)
    let settings = Settings::new().limits(Limits::default().alias_byte_budget(4 * 1024 * 1024));
    let src = "a: xxxxxxxxxxxxxxxx\n";
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let e = doc.apply_patch(&patch(&doubling_patch(40))).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Limit);
    assert_eq!(e.operation(), Some("copy"));
    assert!(e.op_index().is_some_and(|i| i < 40));
    assert_eq!(doc.to_string(), src);
    // A few copies are fine.
    doc.apply_patch(&patch(&doubling_patch(3))).unwrap();
    assert!(doc.to_string().len() > src.len() * 4);
}

#[test]
fn a_small_alias_byte_budget_bounds_what_a_patch_copies() {
    let settings = Settings::new().limits(Limits::default().alias_byte_budget(1_000));
    let src = format!("v: {}\n", "x".repeat(300));
    let mut doc = Document::parse_with(src.clone(), &settings).unwrap();
    let ops = r#"[{"op": "copy", "from": "/v", "path": "/a"},
                  {"op": "copy", "from": "/v", "path": "/b"},
                  {"op": "copy", "from": "/v", "path": "/c"}]"#;
    let e = doc.apply_patch(&patch(ops)).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Limit);
    assert_eq!(doc.to_string(), src);
    // Without the cap on the budget the same patch applies.
    let mut doc = Document::parse(src).unwrap();
    doc.apply_patch(&patch(ops)).unwrap();
    assert!(doc.get("c").is_some());
}

#[test]
fn nested_aliases_are_not_written_out_without_bound() {
    // Three levels of ten aliases is a thousand nodes; the alias budget is
    // small here, so writing them out is refused.
    let src = "s: [0]\na: &a [x, x, x, x, x, x, x, x, x, x]\n\
               b: &b [*a, *a, *a, *a, *a, *a, *a, *a, *a, *a]\n\
               c: &c [*b, *b, *b, *b, *b, *b, *b, *b, *b, *b]\n";
    let settings = Settings::new().limits(Limits::default().alias_budget(100));
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let e = doc
        .apply_patch(&patch(r#"[{"op": "copy", "from": "/c", "path": "/s/0"}]"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Limit);
    assert_eq!(doc.to_string(), src);
}

#[test]
fn the_result_may_not_pass_the_document_size_limit() {
    let settings = Settings::new().limits(Limits::default().max_document_size(200));
    let src = "a: 1\n";
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let big = "y".repeat(500);
    let e = doc
        .apply_patch(&patch(&format!(
            r#"[{{"op": "add", "path": "/b", "value": "{big}"}}]"#
        )))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Limit);
    assert_eq!(doc.to_string(), src);
    let e = doc
        .apply_merge_patch(&merge(&format!(r#"{{"b": "{big}"}}"#)))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Limit);
    assert_eq!(doc.to_string(), src);
}

#[test]
fn a_value_nested_past_the_depth_limit_is_a_limit_error_not_a_crash() {
    // Run on a small stack: nothing on the way may recurse with the depth.
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut deep = Value::from(1);
            for _ in 0..200_000 {
                deep = Value::from(vec![deep]);
            }
            let mut doc = Document::parse("a: 1\n").unwrap();
            let e = doc
                .apply_patch(&Patch::new(vec![PatchOp::add("/b", deep.clone())]))
                .unwrap_err();
            assert_eq!(e.kind(), PatchErrorKind::Limit);
            let e = doc
                .apply_merge_patch(&MergePatch::new(deep.clone()))
                .unwrap_err();
            assert_eq!(e.kind(), PatchErrorKind::Limit);
            let e = doc
                .apply_patch(&Patch::new(vec![PatchOp::test("/a", deep)]))
                .unwrap_err();
            assert_eq!(e.kind(), PatchErrorKind::Test);
            assert_eq!(doc.to_string(), "a: 1\n");
            // Deep JSON text is refused by the reader.
            let text = format!("[{}]", "[".repeat(100_000));
            assert!(Patch::from_json(&text).is_err());
            assert!(MergePatch::from_json(&"[".repeat(100_000)).is_err());
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A mapping of `n` members `k00000: v`.
fn wide(n: usize) -> String {
    let mut src = String::new();
    for i in 0..n {
        src.push_str(&format!("k{i:05}: v\n"));
    }
    src
}

fn removals(n: usize) -> Patch {
    let ops: Vec<String> = (0..n)
        .map(|i| format!(r#"{{"op": "remove", "path": "/k{i:05}"}}"#))
        .collect();
    patch(&format!("[{}]", ops.join(",")))
}

#[test]
fn many_operations_on_a_wide_mapping_are_bounded_in_the_work_they_do() {
    // Each operation finds its key through the mapping's lookup index, so a
    // patch of n operations on a mapping of n members is n lookups, not n*n/2
    // key reads, and it is not refused for its size. (The count of key reads
    // is pinned by a unit test inside the crate.)
    let n = 3_500;
    let mut doc = Document::parse(wide(n)).unwrap();
    doc.apply_patch(&removals(n)).unwrap();
    assert_eq!(doc.to_string(), "{}\n");
    doc.check_lookup_indexes().unwrap();
    // The same patch, one operation too many, fails as a whole.
    let n = 300;
    let src = wide(n);
    let mut doc = Document::parse(src.clone()).unwrap();
    let mut ops = removals(n).ops().to_vec();
    ops.push(PatchOp::remove("/k99999"));
    let e = doc.apply_patch(&Patch::new(ops)).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Path);
    assert_eq!(e.op_index(), Some(n));
    assert_eq!(doc.to_string(), src);
    doc.check_lookup_indexes().unwrap();
}

// ---- merge patch -----------------------------------------------------------

fn merged(src: &str, json: &str) -> String {
    let mut doc = Document::parse(src).unwrap();
    doc.apply_merge_patch(&merge(json)).unwrap();
    doc.to_string()
}

#[test]
fn merge_patch_follows_rfc_7396() {
    // The example of the RFC's section 3, one member at a time.
    let target = r#"{"a": "b", "c": {"d": "e", "f": "g"}}"#;
    let yaml = "a: b\nc:\n  d: e\n  f: g\n";
    assert_eq!(
        merged(yaml, r#"{"a": "z", "c": {"f": null}}"#),
        "a: z\nc:\n  d: e\n"
    );
    assert!(passes(&merged(yaml, "{}"), "", target));
    assert_eq!(merged("a: b\n", r#"{"b": "c"}"#), "a: b\nb: c\n");
    assert_eq!(merged("a:\n  - b\nz: 1\n", r#"{"a": "c"}"#), "a: c\nz: 1\n");
    assert_eq!(
        merged("a:\n  - b\n  - c\n", r#"{"a": [1, 2, 3]}"#),
        "a:\n  - 1\n  - 2\n  - 3\n"
    );
    assert_eq!(merged("a: b\nc: d\n", r#"{"a": null}"#), "c: d\n");
    assert_eq!(merged("a: b\n", r#"{"x": null}"#), "a: b\n");
    assert_eq!(merged("e: null\n", r#"{"a": 1}"#), "e: null\na: 1\n");
    assert_eq!(
        merged("a:\n  b: c\n", r#"{"a": {"b": "d", "c": null}}"#),
        "a:\n  b: d\n"
    );
    assert_eq!(
        merged("x:\n  y: 1\na: 1\n", r#"{"a": {"b": 2}}"#),
        "x:\n  y: 1\na:\n  b: 2\n"
    );
    // Members are added in the flow style of their mapping.
    assert_eq!(
        merged("a: {x: 1}\n", r#"{"a": {"z": 2}}"#),
        "a: {x: 1, z: 2}\n"
    );
}

#[test]
fn merge_patch_keeps_comments_layout_and_line_breaks() {
    assert_eq!(
        merged("# top\na: 1   # one\n\nb: 2\n# tail\n", r#"{"b": 3}"#),
        "# top\na: 1   # one\n\nb: 3\n# tail\n"
    );
    assert_eq!(
        merged("a: 1\r\nb: 2\r\n", r#"{"c": 3}"#),
        "a: 1\r\nb: 2\r\nc: 3\r\n"
    );
}

#[test]
fn a_merge_patch_that_is_not_an_object_replaces_the_document() {
    assert_eq!(merged("a: 1\nb: 2\n", "5"), "5\n");
    assert_eq!(merged("a: 1\n", r#"[1, 2]"#), "- 1\n- 2\n");
    assert_eq!(merged("a: 1\n", r#""text""#), "text\n");
    // What is outside the root stays: a header comment, set apart by a blank line.
    assert_eq!(merged("# c\n\na: 1\n", "null"), "# c\n\nnull\n");
}

#[test]
fn a_merge_into_a_value_that_is_not_a_mapping_writes_the_patch_without_nulls() {
    assert_eq!(
        merged(
            "a: [1]\n",
            r#"{"a": {"b": 1, "c": null, "d": {"e": null}}}"#
        ),
        "a:\n  b: 1\n  d: {}\n"
    );
    assert_eq!(merged("5\n", r#"{"a": 1, "b": null}"#), "a: 1\n");
    assert_eq!(merged("", r#"{"a": 1}"#), "a: 1\n");
}

#[test]
fn deletions_are_applied_after_the_other_members() {
    // Deleting the only member and adding one in the same mapping.
    assert_eq!(merged("a: 1\n", r#"{"a": null, "b": 2}"#), "b: 2\n");
    assert_eq!(merged("a: 1\n", r#"{"b": 2, "a": null}"#), "b: 2\n");
    assert_eq!(
        merged("x:\n  a: 1\n", r#"{"x": {"a": null, "b": 2}}"#),
        "x:\n  b: 2\n"
    );
}

#[test]
fn merging_into_an_alias_is_refused_unless_the_object_is_empty() {
    let src = "base: &b\n  x: 1\ncopy: *b\n";
    let mut doc = Document::parse(src).unwrap();
    let e = doc
        .apply_merge_patch(&merge(r#"{"copy": {"x": 2}}"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAlias);
    assert_eq!(doc.to_string(), src);
    // The same, after an edit that must be undone.
    let e = doc
        .apply_merge_patch(&merge(r#"{"base": {"y": 1}, "copy": {"x": 2}}"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAlias);
    assert_eq!(doc.to_string(), src);
    // An empty object changes nothing; a non-object replaces the alias node.
    assert_eq!(merged(src, r#"{"copy": {}}"#), src);
    assert_eq!(merged(src, r#"{"copy": 5}"#), "base: &b\n  x: 1\ncopy: 5\n");
    // The anchor itself can be merged into: the alias shows it.
    assert_eq!(
        merged(src, r#"{"base": {"x": 2}}"#),
        "base: &b\n  x: 2\ncopy: *b\n"
    );
    // Removing the last member leaves an empty mapping.
    assert_eq!(merged("a: 1\n", r#"{"a": null}"#), "{}\n");
}

#[test]
fn a_merge_patch_member_that_names_two_keys_is_refused() {
    let src = "80: a\n\"80\": b\n";
    let mut doc = Document::parse(src).unwrap();
    let e = doc.apply_merge_patch(&merge(r#"{"80": 1}"#)).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAmbiguous);
    assert_eq!(doc.to_string(), src);
    let e = doc
        .apply_merge_patch(&merge(r#"{"80": null}"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAmbiguous);
    assert_eq!(doc.to_string(), src);
}

#[test]
fn merge_patch_names_members_by_scalar_text() {
    assert_eq!(merged("80: a\nb: c\n", r#"{"80": "x"}"#), "80: x\nb: c\n");
    assert_eq!(merged("true: a\nb: c\n", r#"{"true": null}"#), "b: c\n");
    // A name must be a string.
    let mut doc = Document::parse("a: 1\n").unwrap();
    let mut m = oneyaml::Mapping::new();
    m.insert(Value::from(1), Value::from(2));
    let e = doc
        .apply_merge_patch(&MergePatch::new(Value::from(m)))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Invalid);
    assert_eq!(doc.to_string(), "a: 1\n");
}

#[test]
fn a_merge_patch_with_many_members_finds_them_in_one_pass() {
    let n = 2_000;
    let src = wide(n);
    // Delete every other member, replace the rest, add a few.
    let mut members = Vec::new();
    for i in 0..n {
        if i % 2 == 0 {
            members.push(format!(r#""k{i:05}": null"#));
        } else {
            members.push(format!(r#""k{i:05}": "w""#));
        }
    }
    members.push(r#""new1": 1, "new2": 2"#.to_string());
    let mut doc = Document::parse(src).unwrap();
    doc.apply_merge_patch(&merge(&format!("{{{}}}", members.join(","))))
        .unwrap();
    let v = doc.to_value().unwrap();
    let m = v.as_mapping().unwrap();
    assert_eq!(m.len(), n / 2 + 2);
    assert_eq!(v["k00001"], Value::from("w"));
    assert!(v.get("k00000").is_none());
    assert_eq!(v["new2"], Value::from(2));
}

#[test]
fn merge_patch_text_is_read_as_json() {
    let p = MergePatch::from_json(r#"{"a": [1, 2.5, null, true], "b": {"c": "d"}}"#).unwrap();
    assert!(p.value().is_mapping());
    assert!(MergePatch::from_json("{").is_err());
    assert_eq!(
        MergePatch::from_json("{").unwrap_err().kind(),
        PatchErrorKind::Invalid
    );
    assert_eq!(MergePatch::from(Value::Null).value(), &Value::Null);
}

// ---- reading patches --------------------------------------------------------

#[test]
fn patch_text_numbers_are_integers_or_floats_by_how_json_writes_them() {
    let p = patch(
        r#"[{"op": "test", "path": "/a", "value": 1},
                      {"op": "test", "path": "/b", "value": 1.0},
                      {"op": "test", "path": "/c", "value": 1e3}]"#,
    );
    let mut doc = Document::parse("a: 1\nb: 1.0\nc: 1000.0\n").unwrap();
    doc.apply_patch(&p).unwrap();
    let mut other = Document::parse("a: 1.0\n").unwrap();
    assert!(
        other
            .apply_patch(&patch(r#"[{"op": "test", "path": "/a", "value": 1}]"#))
            .is_err()
    );
}

// ---- keys merged in by `<<` ------------------------------------------------

fn merging() -> Settings {
    Settings::new().merge_keys(true)
}

const MERGED: &str = "base: &b {x: 1}\nm:\n  <<: *b\n  y: 2\n";

fn merged_doc(src: &str) -> Document {
    Document::parse_with(src, &merging()).unwrap()
}

#[test]
fn pointers_read_keys_merged_in_by_the_merge_key() {
    let mut doc = merged_doc(MERGED);
    assert_eq!(doc.get_path("m.x").and_then(|n| n.as_i64()), Some(1));
    assert_eq!(doc.get_pointer("/m/x").unwrap().as_i64(), Some(1));
    assert_eq!(doc.get_pointer("/m/y").unwrap().as_i64(), Some(2));
    assert_eq!(
        doc.get_pointer("/m/nope").unwrap_err().kind(),
        PatchErrorKind::Path
    );
    doc.apply_patch(&patch(
        r#"[{"op": "test", "path": "/m/x", "value": 1},
            {"op": "test", "path": "/m", "value": {"x": 1, "y": 2}}]"#,
    ))
    .unwrap();
    let e = doc
        .apply_patch(&patch(r#"[{"op": "test", "path": "/m/x", "value": 2}]"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Test);
    // A copy reads it.
    doc.apply_patch(&patch(
        r#"[{"op": "copy", "from": "/m/x", "path": "/m/copied"}]"#,
    ))
    .unwrap();
    assert_eq!(doc.get_pointer("/m/copied").unwrap().as_i64(), Some(1));
    assert_eq!(doc.get_pointer("/base/x").unwrap().as_i64(), Some(1));
}

#[test]
fn a_write_that_lands_on_a_merged_in_key_is_refused() {
    for ops in [
        r#"[{"op": "replace", "path": "/m/x", "value": 5}]"#,
        r#"[{"op": "remove", "path": "/m/x"}]"#,
        r#"[{"op": "add", "path": "/m/x", "value": 5}]"#,
        r#"[{"op": "move", "from": "/m/x", "path": "/m/z"}]"#,
        r#"[{"op": "copy", "from": "/m/y", "path": "/m/x"}]"#,
    ] {
        let mut doc = merged_doc(MERGED);
        let e = doc.apply_patch(&patch(ops)).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::RefusedAlias, "{ops}");
        assert_eq!(doc.to_string(), MERGED, "{ops}");
    }
    // A change of the key's own value, or to a key the mapping owns, is fine.
    let mut doc = merged_doc(MERGED);
    doc.apply_patch(&patch(
        r#"[{"op": "replace", "path": "/m/y", "value": 3}, {"op": "add", "path": "/m/w", "value": 4}]"#,
    ))
    .unwrap();
    assert_eq!(doc.get_pointer("/m/y").unwrap().as_i64(), Some(3));
    assert_eq!(doc.get_pointer("/base/x").unwrap().as_i64(), Some(1));
}

#[test]
fn a_write_below_a_merged_in_mapping_is_refused() {
    let src = "base: &b {n: {k: 1}}\nm:\n  <<: *b\n  y: 2\n";
    for ops in [
        r#"[{"op": "replace", "path": "/m/n/k", "value": 5}]"#,
        r#"[{"op": "add", "path": "/m/n/j", "value": 5}]"#,
        r#"[{"op": "remove", "path": "/m/n/k"}]"#,
    ] {
        let mut doc = merged_doc(src);
        let e = doc.apply_patch(&patch(ops)).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::RefusedAlias, "{ops}");
        assert_eq!(doc.to_string(), src);
    }
    let mut doc = merged_doc(src);
    assert_eq!(doc.get_pointer("/m/n/k").unwrap().as_i64(), Some(1));
    doc.apply_patch(&patch(r#"[{"op": "test", "path": "/m/n/k", "value": 1}]"#))
        .unwrap();
}

#[test]
fn an_own_key_hides_a_merged_in_one_and_can_be_changed() {
    let src = "base: &b {x: 1}\nm:\n  <<: *b\n  x: 10\n";
    let mut doc = merged_doc(src);
    assert_eq!(doc.get_pointer("/m/x").unwrap().as_i64(), Some(10));
    doc.apply_patch(&patch(
        r#"[{"op": "replace", "path": "/m/x", "value": 11}]"#,
    ))
    .unwrap();
    assert_eq!(doc.get_pointer("/m/x").unwrap().as_i64(), Some(11));
    assert_eq!(doc.get_pointer("/base/x").unwrap().as_i64(), Some(1));
    // Removing the own key lets the merged-in one show again.
    doc.apply_patch(&patch(r#"[{"op": "remove", "path": "/m/x"}]"#))
        .unwrap();
    assert_eq!(doc.get_pointer("/m/x").unwrap().as_i64(), Some(1));
}

#[test]
fn a_merge_of_several_mappings_is_searched_in_order() {
    let src = "a: &a {x: 1, y: 1}\nb: &b {y: 2, z: 2}\nm:\n  <<: [*a, *b]\n  w: 0\n";
    let doc = merged_doc(src);
    assert_eq!(doc.get_pointer("/m/x").unwrap().as_i64(), Some(1));
    assert_eq!(doc.get_pointer("/m/y").unwrap().as_i64(), Some(1));
    assert_eq!(doc.get_pointer("/m/z").unwrap().as_i64(), Some(2));
    for ops in [
        r#"[{"op": "replace", "path": "/m/z", "value": 5}]"#,
        r#"[{"op": "remove", "path": "/m/y"}]"#,
    ] {
        let mut doc = merged_doc(src);
        let e = doc.apply_patch(&patch(ops)).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::RefusedAlias);
        assert_eq!(doc.to_string(), src);
    }
}

#[test]
fn the_merge_key_is_an_ordinary_key_when_merging_is_off() {
    let mut doc = Document::parse(MERGED).unwrap();
    assert_eq!(
        doc.get_pointer("/m/x").unwrap_err().kind(),
        PatchErrorKind::Path
    );
    doc.apply_patch(&patch(
        r#"[{"op": "test", "path": "/m/<<", "value": {"x": 1}}]"#,
    ))
    .unwrap();
}

#[test]
fn a_merge_patch_does_not_report_success_for_a_merged_in_key_it_cannot_change() {
    for json in [
        r#"{"m": {"x": null}}"#,
        r#"{"m": {"x": 2}}"#,
        r#"{"m": {"x": {"deeper": 1}}}"#,
    ] {
        let mut doc = merged_doc(MERGED);
        let e = doc.apply_merge_patch(&merge(json)).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::RefusedAlias, "{json}");
        assert_eq!(doc.to_string(), MERGED, "{json}");
    }
    // The member's own keys, and names nothing has, are as before.
    let mut doc = merged_doc(MERGED);
    doc.apply_merge_patch(&merge(r#"{"m": {"y": null, "gone": null, "w": 1}}"#))
        .unwrap();
    assert_eq!(doc.get_pointer("/m/w").unwrap().as_i64(), Some(1));
    assert!(doc.get_pointer("/m/gone").is_err());
    assert_eq!(doc.get_pointer("/base/x").unwrap().as_i64(), Some(1));
    // An empty object changes nothing, even where the member is merged in.
    let mut doc = merged_doc("base: &b {x: {a: 1}}\nm:\n  <<: *b\n");
    let before = doc.to_string();
    doc.apply_merge_patch(&merge(r#"{"m": {"x": {}}}"#))
        .unwrap();
    assert_eq!(doc.to_string(), before);
}

// ---- finding members through the lookup index -------------------------------

/// A mapping wide enough to be indexed, with the keys `extra` first.
fn wide_with(extra: &str) -> String {
    format!("{extra}{}", wide(200))
}

#[test]
fn tokens_find_keys_by_text_in_a_wide_mapping() {
    let src = wide_with(
        "80: int\n\"quoted\": q\n0x10: hex\ntrue: yes\n~: tilde\n!!str 7: seven\n1.5: float\n",
    );
    let mut doc = Document::parse(src).unwrap();
    for (token, want) in [
        ("80", "int"),
        ("quoted", "q"),
        ("0x10", "hex"),
        ("true", "yes"),
        ("~0", "tilde"),
        ("7", "seven"),
        ("1.5", "float"),
        ("k00123", "v"),
    ] {
        let node = doc
            .get_pointer(&format!("/{token}"))
            .map_err(|e| format!("{token}: {e}"))
            .unwrap();
        assert_eq!(node.as_str().as_deref(), Some(want), "{token}");
    }
    doc.apply_patch(&patch(
        r#"[{"op": "replace", "path": "/80", "value": "x"},
            {"op": "replace", "path": "/0x10", "value": "yy"},
            {"op": "remove", "path": "/true"}]"#,
    ))
    .unwrap();
    let text = doc.to_string();
    assert!(
        text.starts_with("80: x\n\"quoted\": q\n0x10: yy\n~: tilde"),
        "{}",
        &text[..120]
    );
    doc.check_lookup_indexes().unwrap();
    // A write adds a member the index then finds.
    doc.apply_patch(&patch(
        r#"[{"op": "add", "path": "/fresh", "value": 1},
            {"op": "replace", "path": "/fresh", "value": 2}]"#,
    ))
    .unwrap();
    assert_eq!(doc.get_pointer("/fresh").unwrap().as_i64(), Some(2));
    doc.check_lookup_indexes().unwrap();
}

#[test]
fn two_keys_with_one_text_are_ambiguous_in_a_wide_mapping() {
    // `1` is an integer and `'1'` a string: two keys, one text.
    let src = wide_with("1: a\n'1': b\n");
    let mut doc = Document::parse(src.clone()).unwrap();
    for ops in [
        r#"[{"op": "replace", "path": "/1", "value": "x"}]"#,
        r#"[{"op": "remove", "path": "/1"}]"#,
        r#"[{"op": "test", "path": "/1", "value": "a"}]"#,
    ] {
        let e = doc.apply_patch(&patch(ops)).unwrap_err();
        assert_eq!(e.kind(), PatchErrorKind::RefusedAmbiguous, "{ops}");
        assert_eq!(doc.to_string(), src);
    }
    let e = doc.get_pointer("/1").unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAmbiguous);
    let e = doc.apply_merge_patch(&merge(r#"{"1": null}"#)).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAmbiguous);
    assert_eq!(doc.to_string(), src);
    // Narrow mappings agree.
    let e = refused("1: a\n'1': b\n", r#"[{"op": "remove", "path": "/1"}]"#);
    assert_eq!(e.kind(), PatchErrorKind::RefusedAmbiguous);
}

#[test]
fn keys_merged_in_are_found_in_a_wide_mapping_too() {
    let mut src = String::from("base: &b {x: 1}\nm:\n  <<: *b\n");
    for i in 0..200 {
        src.push_str(&format!("  k{i}: v\n"));
    }
    let mut doc = merged_doc(&src);
    assert_eq!(doc.get_pointer("/m/x").unwrap().as_i64(), Some(1));
    let e = doc
        .apply_patch(&patch(r#"[{"op": "remove", "path": "/m/x"}]"#))
        .unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::RefusedAlias);
    assert_eq!(doc.to_string(), src);
    doc.apply_patch(&patch(r#"[{"op": "remove", "path": "/m/k7"}]"#))
        .unwrap();
}

#[test]
fn a_move_whose_destination_exists_only_once_the_source_is_gone() {
    // The destination `/d/1/k` goes through the item that is at index 1 only
    // after the item at index 0 has been removed.
    let src = "d: [{a: 1}, q, {b: 5}]\n";
    let mut doc = Document::parse(src).unwrap();
    doc.apply_patch(&patch(
        r#"[{"op": "move", "from": "/d/0", "path": "/d/1/k"}]"#,
    ))
    .unwrap();
    assert_eq!(doc.to_string(), "d: [q, {b: 5, k: {a: 1}}]\n");
}

#[test]
fn copies_are_charged_three_times_against_the_budget() {
    // A value of 100 bytes copied under a budget of 1,000 bytes, each copied
    // byte counting three times: three copies fit, a fourth does not, and
    // nothing of the patch stays.
    let settings = Settings::new().limits(Limits::default().alias_byte_budget(1_000));
    let src = format!("v: {}\n", "x".repeat(100));
    let copies = |n: usize| {
        let ops: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"op": "copy", "from": "/v", "path": "/c{i}"}}"#))
            .collect();
        patch(&format!("[{}]", ops.join(",")))
    };
    let mut doc = Document::parse_with(src.clone(), &settings).unwrap();
    doc.apply_patch(&copies(3)).unwrap();
    let mut doc = Document::parse_with(src.clone(), &settings).unwrap();
    let e = doc.apply_patch(&copies(4)).unwrap_err();
    assert_eq!(e.kind(), PatchErrorKind::Limit);
    assert_eq!(e.op_index(), Some(3));
    assert_eq!(doc.to_string(), src);
}
