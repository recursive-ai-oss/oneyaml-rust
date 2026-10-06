//! Regression tests for the coverage-guided fuzzing findings
//! (`crates/fuzz`). Each repro also lives in `crates/fuzz/regressions/`,
//! replayed by `cargo test -p fuzz`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use oneyaml::{
    CommentOwner, Document, DuplicateKeys, EditError, Settings, Stream, Value, code, from_str_with,
    from_value_with,
};
use serde::Deserialize;

/// F1 (consistency check 2): a stray `...` between a directive and the
/// next `---` hid the directive's error from the tree-fed read, which
/// reported the second document instead of the parser's first error.
#[test]
fn f1_error_between_directive_and_marker_comes_first() {
    for src in [
        "t\n...\n%~\n...\n---",
        "a\n...\n%YAML 1.2\n...\n--- b\n",
        "%TAG ! tag:x,2000:\n...\n---\n",
    ] {
        let serde = oneyaml::from_str::<Value>(src).map_err(|e| e.code());
        let tree = Document::parse_recovering(src)
            .into_value()
            .to_value()
            .map_err(|e| e.code());
        assert_eq!(tree, serde, "{src:?}");
        assert_eq!(serde, Err(code::DIRECTIVE_WITHOUT_DOCUMENT), "{src:?}");
        let strict = Document::parse(src).map(|_| ()).map_err(|e| e.code());
        assert_eq!(strict, Err(code::DIRECTIVE_WITHOUT_DOCUMENT), "{src:?}");
    }
    // The same through bytes (the fuzzer's `from_slice` form).
    let bytes = b"Document\n)--\n# Empty\n...\n%Y\0\0\0\0\0\0\0\0---\nmatc\n";
    let p = Document::from_slice_recovering(bytes, &Settings::new());
    assert_eq!(
        p.value().to_value().map_err(|e| e.code()),
        oneyaml::from_slice::<Value>(bytes).map_err(|e| e.code())
    );
}

/// F2: a document behind a byte order mark (U+FEFF, §5.2) could not be
/// edited: `set_root` was refused as an undocumented `Unsupported` ("a root
/// node in this position"), block inserts and pushes as `NotWellFormed`
/// (the edit layer counted the BOM as a column; the parser does not).
#[test]
fn f2_documents_behind_a_bom_are_editable() {
    let mut d = Document::parse("\u{feff}~!!str0p|+").unwrap();
    d.set_root(true).unwrap();
    assert_eq!(d.to_string(), "\u{feff}true");

    let mut d = Document::parse("\u{feff}a: 1\nb: 2\n").unwrap();
    d.root_mut().unwrap().insert("z", 1).unwrap();
    assert_eq!(d.to_string(), "\u{feff}a: 1\nb: 2\nz: 1\n");

    let mut d = Document::parse("\u{feff}- a\n- b\n").unwrap();
    d.root_mut().unwrap().push("y").unwrap();
    assert_eq!(d.to_string(), "\u{feff}- a\n- b\n- 'y'\n");

    let mut d = Document::parse("\u{feff}k:\n  - a\n").unwrap();
    d.get_mut("k").unwrap().push("b").unwrap();
    let v: Value = oneyaml::from_str(&d.to_string()).unwrap();
    assert_eq!(v, oneyaml::from_str::<Value>("k: [a, b]").unwrap());

    // A stream document behind a BOM, and one after `---`.
    let mut s = Stream::parse("\u{feff}a\n--- b\n").unwrap();
    s.document_mut(0).unwrap().set_root(1).unwrap();
    assert_eq!(s.to_string(), "\u{feff}1\n--- b\n");
}

/// F3: a refused node edit dropped a comment's explicit owner
/// (`attach_comment`): the edit group's rollback restored the tree but not
/// the explicit owners the removal step had changed.
#[test]
fn f3_a_refused_edit_keeps_explicit_owners() {
    // A duplicate key (a meaning error) makes the document restricted.
    let src = "hr: # c\n  - a\nhr: 1\n";
    let mut d = Document::parse_recovering(src).into_value();
    let c = d.comment_ids()[0];
    d.attach_comment(c, CommentOwner::DocumentHeader).unwrap();
    let root = d.root().unwrap().id();
    let r = d.node_mut(root).and_then(|mut m| m.move_entry(0, 1));
    assert!(matches!(r, Err(EditError::ReadOnly { .. })), "{r:?}");
    assert_eq!(d.to_string(), src);
    let e = d.explain_comment(c).unwrap();
    assert_eq!(e.owner(), CommentOwner::DocumentHeader);
    assert_eq!(e.rule(), oneyaml::CommentRule::Explicit);

    // The same through a stream document.
    let mut s = Stream::parse_recovering(src).into_value();
    let c = s.document(0).unwrap().comment_ids()[0];
    s.document_mut(0)
        .unwrap()
        .attach_comment(c, CommentOwner::DocumentHeader)
        .unwrap();
    let root = s.document(0).unwrap().root().unwrap().id();
    let r = s
        .document_mut(0)
        .unwrap()
        .node_mut(root)
        .and_then(|mut m| m.move_entry(0, 1));
    assert!(r.is_err());
    let doc = s.document(0).unwrap();
    assert_eq!(
        doc.explain_comment(c).unwrap().owner(),
        CommentOwner::DocumentHeader
    );
}

#[derive(Debug, PartialEq, Deserialize)]
enum External {
    #[serde(rename = "a")]
    A,
    #[serde(rename = "b")]
    B(String),
}

/// F4: under `DuplicateKeys::LastWins` (and merge keys), an
/// externally tagged enum read from a mapping counted the raw entries
/// ("invalid length 2") where the `Value` has one key: the variant mapping
/// was not planned like every other mapping.
#[test]
fn f4_enum_mappings_are_read_with_duplicates_resolved_and_merges_applied() {
    let s = Settings::new()
        .duplicate_keys(DuplicateKeys::LastWins)
        .merge_keys(true);
    for (src, want) in [
        ("b: x\nb: y\n", External::B("y".into())),
        ("{b: x, b: y}", External::B("y".into())),
        ("a: 1\na:\n", External::A),
        ("<<: {b: m}\n", External::B("m".into())),
    ] {
        let got: External = from_str_with(src, &s).unwrap();
        assert_eq!(got, want, "{src:?}");
        let v: Value = from_str_with(src, &s).unwrap();
        assert_eq!(from_value_with::<External>(v, &s).unwrap(), want, "{src:?}");
    }
    // Two distinct keys stay an error (the target's single-key rule).
    let e = from_str_with::<External>("a:\nb: x\n", &s).unwrap_err();
    assert_eq!(e.code(), code::INVALID_LENGTH);
}

#[derive(Debug, Deserialize)]
struct Field {
    #[allow(dead_code)]
    b: String,
}

/// F4 follow-up: under the default policy a repeated key
/// is the input's meaning error, Y1013 at the second key, for every typed
/// target as for `Value` — not the target's verdict on the repeated key
/// (Y3007 "invalid length" for a variant mapping, Y3008 "duplicate
/// field" for a derived struct). The second value is still validated
/// first, as the builder does (Y1015 before Y1013).
#[test]
fn f4_default_policy_duplicates_are_y1013_for_every_target() {
    fn loc(e: &oneyaml::Error) -> (usize, usize) {
        let l = e.location().unwrap();
        (l.line(), l.column())
    }
    for (src, want, at) in [
        ("b: x\nb: y\n", code::DUPLICATE_KEY, (2, 1)),
        ("{b: x, b: y}", code::DUPLICATE_KEY, (1, 8)),
        ("b: x\nb: *nope\n", code::UNDEFINED_ALIAS, (2, 4)),
    ] {
        let v = oneyaml::from_str::<Value>(src).unwrap_err();
        assert_eq!((v.code(), loc(&v)), (want, at), "Value {src:?}");
        let e = oneyaml::from_str::<External>(src).unwrap_err();
        assert_eq!((e.code(), loc(&e)), (want, at), "External {src:?}");
        let f = oneyaml::from_str::<Field>(src).unwrap_err();
        assert_eq!((f.code(), loc(&f)), (want, at), "struct {src:?}");
        let m = oneyaml::from_str::<std::collections::BTreeMap<String, String>>(src).unwrap_err();
        assert_eq!((m.code(), loc(&m)), (want, at), "map {src:?}");
    }
    // Nested: the variant mapping inside a struct field.
    #[derive(Debug, Deserialize)]
    struct Outer {
        #[allow(dead_code)]
        e: External,
    }
    let e = oneyaml::from_str::<Outer>("e: {b: x, b: y}\n").unwrap_err();
    assert_eq!((e.code(), loc(&e)), (code::DUPLICATE_KEY, (1, 11)));
    // A repeated complex key: Y1013, not the repeated entry's value type
    // error (the value is validated, not read into the target).
    let src = "? [k]\n: a\n? [k]\n: [1]\n";
    let f = oneyaml::from_str::<std::collections::HashMap<Value, String>>(src).unwrap_err();
    let v = oneyaml::from_str::<Value>(src).unwrap_err();
    assert_eq!(v.code(), code::DUPLICATE_KEY);
    assert_eq!((f.code(), loc(&f)), (v.code(), loc(&v)));
    // Different keys naming one field keep Y3008 (the registry's meaning).
    #[derive(Debug, Deserialize)]
    struct Aliased {
        #[serde(alias = "c")]
        #[allow(dead_code)]
        b: u8,
    }
    let a = oneyaml::from_str::<Aliased>("{b: 1, c: 2}").unwrap_err();
    assert_eq!(a.code(), code::DUPLICATE_FIELD);
}

// ================================================ comment and BOM edits

/// The rulebook owners of a document's comments: (text, rule, owner text).
fn owners(d: &Document) -> Vec<(String, u8, Option<String>)> {
    let t = d.to_string();
    let base = d.span().start;
    d.explain_comments()
        .unwrap()
        .iter()
        .map(|e| {
            let o = e
                .owner_span()
                .map(|s| t[s.start - base..s.end - base].to_owned());
            (e.text().to_owned(), e.rule().number(), o)
        })
        .collect()
}

/// E1: `remove_keep_comments` leaves only
/// the entry's *leading* comments written above it (`Leading(key or item)`);
/// the entry's other comments go with it. A value's rule-11 comment above
/// an empty-key `: x` line was left behind and re-owned by the next entry.
#[test]
fn e1_remove_keep_comments_leaves_only_the_leading_comments_above() {
    let mut d = Document::parse("p: v\n  # c1\n: x\nq: 2\n").unwrap();
    // (The entry with the empty key.)
    let x = {
        let root = d.root().unwrap();
        let m = root.as_mapping().unwrap();
        m.iter().find(|(k, _)| k.text().is_empty()).unwrap().1.id()
    };
    let rm = d.remove_keep_comments(x).unwrap();
    assert_eq!(rm.comments(), ["# c1".to_owned()]);
    assert_eq!(d.to_string(), "p: v\nq: 2\n");
    // Leading comments above the entry stay (re-homed), as before; one
    // written inside the entry (after `? `) goes with it.
    let mut d = Document::parse("x: 0\n# lead\n? # in\n  a\n: 1\nb: 2\n").unwrap();
    let a = d.get("a").unwrap().id();
    let rm = d.remove_keep_comments(a).unwrap();
    assert_eq!(rm.comments(), ["# in".to_owned()]);
    assert_eq!(d.to_string(), "x: 0\n# lead\nb: 2\n");
    assert_eq!(owners(&d), [("# lead".into(), 4, Some("b".into()))]);
}

/// E2: a byte order mark between a comment and the first entry
/// (`# c⏎\u{feff}a: 1`, at stream level, §5.2) made removing that entry an
/// `Internal` error; the comment goes with the entry, the mark stays.
#[test]
fn e2_removing_the_first_entry_behind_a_bom_after_comments() {
    let src = "# c\n\u{feff}a: 1\nb: 2\n";
    let mut d = Document::parse(src).unwrap();
    let rm = d.remove_path(&oneyaml::path!["a"]).unwrap();
    assert_eq!(rm.comments(), ["# c".to_owned()]);
    assert_eq!(d.to_string(), "\u{feff}b: 2\n");
    let mut s = Stream::parse(src).unwrap();
    let rm = s
        .document_mut(0)
        .unwrap()
        .remove_path(&oneyaml::path!["a"])
        .unwrap();
    assert_eq!(rm.comments(), ["# c".to_owned()]);
    assert_eq!(s.to_string(), "\u{feff}b: 2\n");
    assert_eq!(
        Stream::parse(s.to_string()).unwrap().to_string(),
        s.to_string()
    );
}

/// E3 (`oneyaml-syntax`, §5.2): a byte order
/// mark is no column for the comment rules (the parser counts none): rule 6
/// read the root of `\u{feff}k: v` as indented by one, so a comment line at
/// column 1 after it became the document footer.
#[test]
fn e3_a_bom_is_no_column_for_the_comment_rules() {
    for src in [
        "a:\n  - 1\n # c\n",
        "k: v\n # c\n",
        "a:\n  b: 1\n # c\nz: 1\n",
        "a: 1\n# c\n",
        "- a\n- b:\n    c: 1\n  # f\n",
    ] {
        let plain = Document::parse(src).unwrap();
        let bom = Document::parse(format!("\u{feff}{src}")).unwrap();
        assert_eq!(owners(&bom), owners(&plain), "{src:?}");
    }
    let d = Document::parse("\u{feff}k: v\n # c\n").unwrap();
    let e = d.explain_comment(d.comment_ids()[0]).unwrap();
    assert_eq!(e.rule(), oneyaml::CommentRule::CollectionFooter);
}

/// White space before a byte
/// order mark keeps its indentation for the parser (`scan_trivia` resets
/// the line start, not the indent), so edits there are refused or read
/// back as the document reports (counting columns from the mark once
/// accepted `insert("z", 1)` into `␠␠\u{feff}k: v`, whose text reads
/// Y0004). Such a mark is now a parse error.
#[test]
fn e3_white_space_before_a_byte_order_mark_is_refused_or_consistent() {
    fn consistent(d: &Document, before: &str, r: Result<(), EditError>) {
        let text = d.to_string();
        match r {
            Err(e) => {
                assert!(!matches!(e, EditError::Internal), "{before:?}: {e:?}");
                assert_eq!(text, before, "refused, changed: {e:?}");
            }
            Ok(()) => {
                let again = Document::parse_recovering(text.clone()).into_value();
                let diags = |d: &Document| {
                    d.diagnostics()
                        .iter()
                        .map(|x| (x.code, x.primary.span))
                        .collect::<Vec<_>>()
                };
                assert_eq!(diags(d), diags(&again), "same meaning: {text:?}");
                assert_eq!(
                    d.to_value().map_err(|e| e.code()),
                    again.to_value().map_err(|e| e.code()),
                    "same meaning: {text:?}"
                );
            }
        }
    }
    // The mark is a (recoverable) syntax error now; the
    // recovered document is edited, and must stay consistent.
    let recovered = |src: &str| {
        let e = Document::parse(src).unwrap_err();
        assert_eq!(
            e.code(),
            oneyaml::code::MISPLACED_BYTE_ORDER_MARK,
            "{src:?}"
        );
        Document::parse_recovering(src).into_value()
    };
    let src = "  \u{feff}k: v\n";
    let mut d = recovered(src);
    let r = d.root_mut().unwrap().insert("z", 1).map(|_| ());
    consistent(&d, src, r);
    let src = " \u{feff}- a\n - b\n";
    let mut d = recovered(src);
    let removed = d.root_mut().unwrap().remove_at(0);
    let r = removed.and_then(|rm| {
        d.root_mut()
            .unwrap()
            .insert_removed_at(0, rm)
            .map(|_| ())
            .map_err(|e| e.error().clone())
    });
    // (A refused re-insertion leaves the removal: compare with its text.)
    if r.is_ok() {
        consistent(&d, src, r);
    } else {
        consistent(&d, &d.to_string(), r);
    }
    let mut d = recovered(src);
    let r = d.root_mut().unwrap().move_entry(0, 1);
    consistent(&d, src, r);
    let mut d = recovered(src);
    let r = d.set_root(vec!["x", "y"]).map(|_| ());
    consistent(&d, src, r);
}

/// E4 (limitation class S3/P3): inserting into a single-pair
/// mapping of a flow sequence (`[ k: v ]`, rewritten `{k: v, new: 1}`)
/// gave the pair's leading and same-line comments to the new `{…}` item.
/// Now refused with `CommentReattaches`, nothing changed; without such
/// comments it is accepted.
#[test]
fn e4_inserting_into_a_single_pair_keeps_its_comments_owners() {
    for (src, rule) in [
        ("[\nfoo: bar # t\n]\n", oneyaml::CommentRule::Trailing),
        ("[\n# c\nfoo: bar\n]\n", oneyaml::CommentRule::Flow),
    ] {
        let mut d = Document::parse(src).unwrap();
        let pair = d
            .root()
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .id();
        let r = d
            .node_mut(pair)
            .and_then(|mut m| m.insert("z", 1).map(|n| n.id()));
        match r {
            Err(EditError::CommentReattaches { rule: got, .. }) => assert_eq!(got, rule, "{src:?}"),
            other => panic!("{src:?}: {other:?}"),
        }
        assert_eq!(d.to_string(), src);
    }
    let mut d = Document::parse("[foo: bar] # t\n").unwrap();
    let pair = d
        .root()
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .id();
    d.node_mut(pair).unwrap().insert("z", 1).unwrap();
    assert_eq!(d.to_string(), "[{foo: bar, z: 1}] # t\n");
}

/// E5: a final line of spaces after a keep (`|+`)
/// block scalar is part of its value (`k: |+⏎  a⏎␠` reads `"a\n\n"`) but
/// lies outside its span. An entry written after the scalar moved that
/// line away and changed `k` to `"a\n"`; it is refused `NotWellFormed`.
/// A new multi-line value before such a line keeps its own value (the
/// `|+` form would take the line: it is written double-quoted).
#[test]
fn e5_an_edit_after_a_keep_scalar_and_a_space_line() {
    for src in ["k: |+\n  a\n ", "k: |+\n  a\n\n  "] {
        let want = oneyaml::from_str::<Value>(src).unwrap();
        let mut d = Document::parse(src).unwrap();
        let r = d.root_mut().unwrap().insert("z", 1).map(|_| ());
        assert!(matches!(r, Err(EditError::NotWellFormed { .. })), "{r:?}");
        assert_eq!(d.to_string(), src);
        assert_eq!(d.to_value().unwrap(), want);
    }
    for v in ["x\n\n", "x\ny\n\n", "x\n"] {
        let mut d = Document::parse("a: 1\n ").unwrap();
        d.root_mut().unwrap().insert("z", v).unwrap();
        let text = d.to_string();
        let read = oneyaml::from_str::<Value>(&text).unwrap();
        assert_eq!(read, d.to_value().unwrap(), "{text:?}");
        assert_eq!(read["z"], Value::from(v), "{text:?}");
    }
}

/// E7 (found by probing E5's open case, then by the
/// fuzz once its values included one): a string ending in two or more
/// line breaks written as the last node of a document (a `|+` block
/// scalar) lost its last break: `insert("z", "x\n\n")` into `a: 1⏎` wrote
/// `z: |+⏎  x⏎`, which reads `"x\n"`. The edit layer dropped the
/// emitter's final line break, which there is the scalar's own last line.
#[test]
fn e7_keep_scalars_at_the_end_keep_every_line_break() {
    for v in ["x\n\n", "x\n\n\n", "x\ny\n\n"] {
        let want = Value::from(v);
        let check = |d: &Document, got: &Value| {
            let text = d.to_string();
            assert_eq!(got, &want, "{text:?}");
            assert_eq!(
                oneyaml::from_str::<Value>(&text).unwrap(),
                d.to_value().unwrap(),
                "same meaning: {text:?}"
            );
        };
        for src in ["a: 1\n", "a: 1", "a: 1\r\n", "a: 1 # c\n# f\n"] {
            let mut d = Document::parse(src).unwrap();
            d.root_mut().unwrap().insert("z", v).unwrap();
            check(&d, &d.to_value().unwrap()["z"]);
            let mut d = Document::parse(src).unwrap();
            d.set_path(&oneyaml::path!["a"], v).unwrap();
            check(&d, &d.to_value().unwrap()["a"]);
            let mut d = Document::parse(src).unwrap();
            let a = d.get("a").unwrap().id();
            d.node_mut(a).unwrap().set_scalar(v).unwrap();
            check(&d, &d.to_value().unwrap()["a"]);
        }
        let mut d = Document::parse("- 1\n").unwrap();
        d.root_mut().unwrap().push(v).unwrap();
        check(&d, &d.to_value().unwrap()[1]);
        let mut d = Document::parse("k:\n  a: 1\n").unwrap();
        d.set_path(&oneyaml::path!["k", "a"], v).unwrap();
        check(&d, &d.to_value().unwrap()["k"]["a"]);
        let mut d = Document::parse("a: 1\n").unwrap();
        d.set_root(v).unwrap();
        check(&d, &d.to_value().unwrap());
    }
    let mut d = Document::parse("a: 1\n").unwrap();
    d.root_mut().unwrap().insert("z", "x\n\n").unwrap();
    assert_eq!(d.to_string(), "a: 1\nz: |+\n  x\n\n");
}

/// E5, the open case: a keep scalar the edit writes
/// itself right before a final line of spaces (a replacement, a new root,
/// an entry created by `set_path`) would take that line into its value
/// (`"a\n\n"` read as `"a\n\n\n"`); and an anchored or tagged keep scalar
/// (`&b !t |+`) was not recognised as one. The syntax layer refuses such
/// splices, and the value is written double-quoted instead.
#[test]
fn e5_keep_scalars_written_before_a_space_line_keep_their_value() {
    let tagged = Value::Tagged(Box::new(oneyaml::TaggedValue {
        tag: oneyaml::Tag::new("!t"),
        value: Value::from("a\n\n"),
    }));
    for v in [Value::from("a\n\n"), tagged] {
        type Edit = fn(&mut Document, &Value) -> Value;
        let edits: [(&str, Edit); 4] = [
            ("# c\n\n&b # c\n  a: 1\n ", |d, v| {
                d.set_root(v).unwrap();
                d.to_value().unwrap()
            }),
            ("k: >-\n  x\n  ", |d, v| {
                d.set_path(&oneyaml::path!["k"], v).unwrap();
                d.to_value().unwrap()["k"].clone()
            }),
            ("m:\n  ? - x\n  : true\n ", |d, v| {
                d.set_path(&oneyaml::path!["n", "n"], v).unwrap();
                d.to_value().unwrap()["n"]["n"].clone()
            }),
            ("k: 1\n ", |d, v| {
                d.root_mut().unwrap().insert("z", v).unwrap();
                d.to_value().unwrap()["z"].clone()
            }),
        ];
        for (src, edit) in edits {
            let mut d = Document::parse(src).unwrap();
            let got = edit(&mut d, &v);
            let text = d.to_string();
            assert_eq!(got, v, "{src:?} -> {text:?}");
            assert_eq!(
                oneyaml::from_str::<Value>(&text).unwrap(),
                d.to_value().unwrap(),
                "same meaning: {text:?}"
            );
        }
    }
    // An entry after an existing tagged keep scalar and a space line.
    let src = "k: !t |+\n  a\n\n ";
    let mut d = Document::parse(src).unwrap();
    let r = d.root_mut().unwrap().insert("z", 1).map(|_| ());
    assert!(matches!(r, Err(EditError::NotWellFormed { .. })), "{r:?}");
    assert_eq!(d.to_string(), src);
}

/// E7, the root of a document without content: the new root's text keeps
/// a keep scalar's last empty line, and no other break is added.
#[test]
fn e7_a_first_root_that_ends_with_a_keep_scalar() {
    for src in ["", "# only a comment\n", "# c\n---\n"] {
        for v in ["a\n\n", "a\n\n\n"] {
            let mut d = Document::parse(src).unwrap();
            let mut m = oneyaml::Mapping::new();
            m.insert(Value::from("k"), Value::from(v));
            let m = Value::Mapping(m);
            d.set_root(&m).unwrap();
            let text = d.to_string();
            assert_eq!(d.to_value().unwrap(), m, "{src:?} -> {text:?}");
            assert_eq!(oneyaml::from_str::<Value>(&text).unwrap(), m, "{text:?}");
        }
    }
}

/// E8 (restricted mode; a fuzz finding, in the code
/// since 0.2): an error region right after a node can be about that node
/// (`[[a, x⏎ ab]: 1]`: Y0007, the implicit key spans two lines; `: 1]` is
/// the error region). Setting `x⏎ ab` to one line was accepted and made
/// the key legal, so the document still reported Y0007 while its text
/// read Y0008 further on. Edits inside a node beside an error region are
/// now refused `ReadOnly`; edits elsewhere still work.
#[test]
fn e8_no_edit_inside_a_node_beside_an_error_region() {
    let src = "[[a, x\n ab]: 1, \0]\n";
    let mut d = Document::parse_recovering(src).into_value();
    let x = {
        let root = d.root().unwrap();
        let outer = root.as_sequence().unwrap();
        let inner = outer.iter().next().unwrap();
        inner.as_sequence().unwrap().iter().nth(1).unwrap().id()
    };
    let r = d.node_mut(x).and_then(|mut m| m.set("a,b").map(|_| ()));
    assert!(matches!(r, Err(EditError::ReadOnly { .. })), "{r:?}");
    assert_eq!(d.to_string(), src);
    // Elsewhere in the same document.
    let src = "a: 1\nb: [[a, x\n ab]: 1]\n";
    let mut d = Document::parse_recovering(src).into_value();
    d.set_path(&oneyaml::path!["a"], 2).unwrap();
    assert_eq!(d.to_string(), "a: 2\nb: [[a, x\n ab]: 1]\n");
    let text = d.to_string();
    assert_eq!(
        d.to_value().map_err(|e| e.code()),
        oneyaml::from_str::<Value>(&text).map_err(|e| e.code())
    );
}

// ================================================ the tail and error checks

/// The E5 check is on values, once per
/// edit. Edits that keep the kept scalar's value are accepted (0.4 got
/// them right); edits that would change it are refused, nothing changed.
#[test]
fn e5_the_check_is_on_values_once_per_edit() {
    for (src, p) in [
        ("a: 1\nk: |+\n  x\n ", oneyaml::path!["k"]),
        ("- 1\n- |+\n  x\n ", oneyaml::path![1]),
    ] {
        let want = oneyaml::from_str::<Value>(src).unwrap();
        // An anchor on the scalar: its value stays.
        let mut d = Document::parse(src).unwrap();
        let id = d.id_of(&p).unwrap();
        d.set_anchor(id, Some("zz")).unwrap();
        assert_eq!(d.to_value().unwrap(), want, "{src:?}");
        // Removed and re-inserted at its place: the bytes come back.
        let mut d = Document::parse(src).unwrap();
        let id = d.id_of(&p).unwrap();
        let r = d.remove_node(id).unwrap();
        d.root_mut().unwrap().insert_removed_at(1, r).unwrap();
        assert_eq!(d.to_string(), src);
        // Moved away from the line: its value would change.
        let mut d = Document::parse(src).unwrap();
        let r = d.root_mut().unwrap().move_entry(1, 0);
        assert!(matches!(r, Err(EditError::NotWellFormed { .. })), "{r:?}");
        assert_eq!(d.to_string(), src);
    }
    // A comment rewrite whose steps pass through the line: accepted, the
    // value as it was.
    let src = "- 1\n- |+\n  x\n# c\n ";
    let want = oneyaml::from_str::<Value>(src).unwrap();
    let mut d = Document::parse(src).unwrap();
    d.set_footer(&["f"]).unwrap();
    assert_eq!(d.to_value().unwrap(), want);
    assert_eq!(
        oneyaml::from_str::<Value>(&d.to_string()).unwrap(),
        d.to_value().unwrap()
    );
    // Removing the entry after the kept scalar: the line of spaces goes,
    // so `k` keeps its value.
    let mut d = Document::parse("k: |+\n  x\n\nz: 1\n ").unwrap();
    let _ = d.root_mut().unwrap().remove("z").unwrap();
    assert_eq!(d.to_string(), "k: |+\n  x\n\n");
    assert_eq!(d.to_value().unwrap()["k"].as_str(), Some("x\n\n"));
}

/// An edit next to an error region is
/// accepted when the document's errors stay those its text reads; only one
/// that changes them is refused.
#[test]
fn e8_edits_beside_an_error_region_that_keep_its_errors() {
    for (src, p, want) in [
        ("{a: 1, b: 2} ]\n", oneyaml::path!["a"], "{a: 9, b: 2} ]\n"),
        ("[1, [2, 3]] ]\n", oneyaml::path![0], "[9, [2, 3]] ]\n"),
        (
            "k: {a: 1, b: 2} ]\nz: 1\n",
            oneyaml::path!["k", "a"],
            "k: {a: 9, b: 2} ]\nz: 1\n",
        ),
    ] {
        let mut d = Document::parse_recovering(src).into_value();
        d.set_path(&p, 9).unwrap();
        assert_eq!(d.to_string(), want);
        let again = Document::parse_recovering(want).into_value();
        let codes = |d: &Document| {
            d.diagnostics()
                .iter()
                .map(|x| (x.code, x.primary.span))
                .collect::<Vec<_>>()
        };
        assert_eq!(codes(&d), codes(&again), "{src:?}");
    }
}

/// E9: replacing a root whose first entry's leading
/// comment lines have a byte order mark at the start of one of them
/// (`#S⏎\u{feff}#⏎- a`) was `EditError::Internal` (a debug assertion:
/// "comments of the entry and of other owners interleave"); the mark is
/// trivia of those lines and goes with them, as when they are written
/// without one.
#[test]
fn e9_a_byte_order_mark_between_leading_comment_lines() {
    for (src, plain) in [
        ("#S\n\u{feff}#\n- a\n", "#S\n#\n- a\n"),
        ("# a\n\u{feff}# b\nk: v\n", "# a\n# b\nk: v\n"),
    ] {
        for v in [Value::from(1), Value::Sequence(vec![Value::from(2)].into())] {
            let mut d = Document::parse(src).unwrap();
            d.set_root(&v).unwrap();
            let mut p = Document::parse(plain).unwrap();
            p.set_root(&v).unwrap();
            assert_eq!(d.to_string(), p.to_string(), "{src:?}");
            let mut st = Stream::parse(src).unwrap();
            st.document_mut(0).unwrap().set_root(&v).unwrap();
            assert_eq!(st.to_string(), p.to_string(), "{src:?}");
        }
    }
}

/// H1: a mapping value's `clear_comments` with
/// nothing to clear, in a document with errors, was refused (`ReadOnly`,
/// the edit-group check beside an error region) where the base layer's
/// two calls are no-ops; it is a no-op too.
#[test]
fn h1_value_handle_clears_nothing_beside_an_error() {
    let src = " - [ : empty key ]\n- [: another empty key]\nmments.\n";
    let mut d = Document::parse_recovering(src).into_value();
    assert!(d.has_errors());
    let (k, v) = {
        let root = d.root().unwrap().as_sequence().unwrap();
        let first = root.get(0).unwrap();
        let m = first.as_sequence().unwrap().get(0).unwrap();
        let (k, v) = m.as_mapping().unwrap().iter().next().unwrap();
        (k.id(), v.id())
    };
    let mut base = d.clone();
    assert_eq!(base.clear_comments(k).unwrap(), Vec::<String>::new());
    assert_eq!(base.clear_comments(v).unwrap(), Vec::<String>::new());
    let gone = d.node_mut(v).unwrap().clear_comments().unwrap();
    assert!(gone.is_empty());
    assert_eq!(d.to_string(), src);
}

/// E10 (comment rule 9): replacing a
/// single-pair flow mapping item (`[k: v]`) dropped the comments its key
/// owns that are written outside the pair (after the `[`, or after the
/// pair on its line), which a comment after a `,` and any other item's
/// keep: they stay, the new item's. One inside the pair (`[k: # c⏎ v]`)
/// goes with it.
#[test]
fn e10_comments_around_a_replaced_pair_item_stay() {
    for (src, want) in [
        ("[\n#\nt:]", "[\n#\nx]"),
        ("[ a,\n# c\n k: v]", "[ a,\n# c\n x]"),
        ("[: # \n]", "[x # \n]"),
        ("[k: v # c\n]", "[x # c\n]"),
        ("[k: # c\n v]", "[x]"),
    ] {
        let mut d = Document::parse(src).unwrap();
        let at = d.root().unwrap().as_sequence().unwrap().len() - 1;
        d.set_path(&oneyaml::path![at], "x").unwrap();
        assert_eq!(d.to_string(), want, "{src:?}");
        let item = d
            .root()
            .unwrap()
            .as_sequence()
            .unwrap()
            .get(at)
            .unwrap()
            .id();
        for e in d.explain_comments().unwrap() {
            assert!(
                matches!(e.owner(), CommentOwner::Leading(n) | CommentOwner::Trailing(n) if n == item),
                "{src:?}: {:?}",
                e.owner()
            );
        }
    }
}

/// H3: with `merge_keys` on, setting a node to the
/// value it has rewrote it (`'<<'`) when it held a `<<` written as a
/// value or an item, which means the string `<<`: only a merge *key*
/// makes the equal-value set write the value anew.
#[test]
fn h3_a_merge_key_text_as_a_value_is_kept() {
    let s = Settings::new().merge_keys(true);
    for src in [
        "one: <<\n",
        "a: [<<]\n",
        "m:\n  one: <<\n  two: x\n",
        "- <<\n",
    ] {
        let mut d = Document::parse_with(src, &s).unwrap();
        let v = d.to_value().unwrap();
        let id = d.root().unwrap().id();
        let new = d.node_mut(id).unwrap().set(&v).unwrap().id();
        assert_eq!(new, id, "{src:?}");
        assert_eq!(d.to_string(), src);
    }
    // A merge key still makes the node one written as any value.
    let src = "b: &b {p: 1}\nm:\n  <<: *b\n";
    let mut d = Document::parse_with(src, &s).unwrap();
    let v = d.get("m").unwrap().deserialize::<Value>().unwrap();
    let id = d.get("m").unwrap().id();
    let new = d.node_mut(id).unwrap().set(&v).unwrap().id();
    assert_ne!(new, id);
    assert_eq!(d.to_value().unwrap()["m"]["p"].as_u64(), Some(1));
}

/// E15: removing the anchor line of a root block
/// collection after a tab (`\t&a⏎ k: v`) by tokens left `\tk: v`, which
/// reads as an error the edited tree did not have; it goes the way it
/// went before tokens were used (refused: the text cannot be written so). The
/// same anchor line after spaces still goes by tokens.
#[test]
fn e15_unanchoring_a_root_after_a_tab() {
    for src in ["\t&n1\n p: 1", "\t&n1\n p: 1\n", "\t&n1\n - 1\n"] {
        let mut d = Document::parse(src).unwrap();
        let id = d.root().unwrap().id();
        match d.set_anchor(id, None) {
            Ok(_) => {
                let again = Document::parse_recovering(d.to_string()).into_value();
                assert_eq!(again.diagnostics(), d.diagnostics(), "{src:?}");
                assert_eq!(again.to_value().unwrap(), d.to_value().unwrap());
            }
            Err(e) => {
                assert!(
                    matches!(e, EditError::NotWellFormed { .. }),
                    "{src:?}: {e:?}"
                );
                assert_eq!(d.to_string(), src);
            }
        }
    }
    let mut st = Stream::parse_recovering("\t?k").into_value();
    st.document_mut(0).unwrap().set_root("a: b").unwrap();
    st.document_mut(0)
        .unwrap()
        .root_mut()
        .unwrap()
        .set_anchor(Some("n1"))
        .unwrap();
    let m: std::collections::BTreeMap<&str, i32> = [("p", 1)].into_iter().collect();
    st.document_mut(0)
        .unwrap()
        .set_path(oneyaml::Path::new(), &m)
        .unwrap();
    let before = st.to_string();
    assert_eq!(before, "\t&n1\n p: 1");
    let r = st
        .document_mut(0)
        .unwrap()
        .root_mut()
        .and_then(|mut x| x.set_anchor(None).map(|_| ()));
    assert!(matches!(r, Err(EditError::NotWellFormed { .. })), "{r:?}");
    assert_eq!(st.to_string(), before);
    // After spaces: by tokens, the handle kept.
    let mut d = Document::parse(" &n1\n p: 1\n").unwrap();
    let id = d.root().unwrap().id();
    assert_eq!(d.set_anchor(id, None).unwrap(), id);
    assert_eq!(d.to_string(), " p: 1\n");
}

/// E13 (clean input): removing an anchor that stands alone
/// on a line after its tag, with a same-line comment, gave that comment
/// to the first entry (`Trailing` → `Leading`) and was accepted; the
/// token path now checks the owners as the rewrite does, and refuses
/// with `CommentReattaches`, nothing changed. The same shape without the
/// comment, or with the anchor on the tag's line, is still accepted.
#[test]
fn e13_unanchoring_keeps_a_same_line_comment_owner() {
    let refused = [
        ("k: !t\n  &zz #c\n  a: b\n", Some("k")),
        ("!t\n&zz #c\nk: v\n", None),
    ];
    for (src, key) in refused {
        let mut d = Document::parse(src).unwrap();
        let before = owners(&d);
        let id = match key {
            Some(k) => d.id_of(&oneyaml::path![k]).unwrap(),
            None => d.root().unwrap().id(),
        };
        let e = d.set_anchor(id, None).unwrap_err();
        assert!(
            matches!(e, EditError::CommentReattaches { .. }),
            "{src:?}: {e:?}"
        );
        assert_eq!(d.to_string(), src);
        assert_eq!(owners(&d), before, "{src:?}");
        assert_eq!(d.node(id).unwrap().anchor(), Some("zz"));
    }
    let accepted = [
        "k: !t\n  &zz\n  a: b\n",
        "k: !t &zz #c\n  a: b\n",
        "!t &zz #c\nk: v\n",
    ];
    for src in accepted {
        let mut d = Document::parse(src).unwrap();
        let before = owners(&d);
        let id = d
            .id_of(&oneyaml::path!["k"])
            .filter(|_| src.starts_with('k'))
            .unwrap_or_else(|| d.root().unwrap().id());
        d.set_anchor(id, None).unwrap();
        assert!(!d.to_string().contains("&zz"), "{src:?}");
        let again = Document::parse(d.to_string()).unwrap();
        assert_eq!(owners(&again), owners(&d), "{src:?}");
        assert_eq!(owners(&d).len(), before.len(), "{src:?}");
    }
}
