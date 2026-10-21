//! Setting a value writes exactly that value, in the style the node already
//! has where that style can hold it, and changes nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Settings, Value, path};

fn set(src: &str, key: &str, v: &str) -> String {
    let mut d = Document::parse(src).unwrap();
    d.set_path(&path![key], v).unwrap();
    let out = d.to_string();
    // The document reads back as exactly the value set.
    let read: Value = oneyaml::from_str(&out).unwrap();
    assert_eq!(
        read.get(key).and_then(Value::as_str),
        Some(v),
        "{src:?} <- {v:?} gave {out:?}"
    );
    out
}

#[test]
fn a_literal_block_keeps_its_style_and_only_changes_its_chomping_as_needed() {
    let src = "#hello\na: 10\n\n#abc 123\nb: |\n  this is a test\nc: 13\n";
    // One final line break: clip, as the node was.
    assert_eq!(
        set(src, "b", "this is a test2\n"),
        "#hello\na: 10\n\n#abc 123\nb: |\n  this is a test2\nc: 13\n"
    );
    // None: the header becomes the strip form.
    assert_eq!(
        set(src, "b", "this is a test2"),
        "#hello\na: 10\n\n#abc 123\nb: |-\n  this is a test2\nc: 13\n"
    );
    // Several: keep.
    assert_eq!(
        set(src, "b", "this is a test2\n\n"),
        "#hello\na: 10\n\n#abc 123\nb: |+\n  this is a test2\n\nc: 13\n"
    );
}

#[test]
fn strip_and_keep_headers_stay_while_they_hold_the_value() {
    assert_eq!(set("a: |-\n  old\nb: 1\n", "a", "x"), "a: |-\n  x\nb: 1\n");
    // Strip cannot hold a final line break: clip.
    assert_eq!(set("a: |-\n  old\nb: 1\n", "a", "x\n"), "a: |\n  x\nb: 1\n");
    // Keep holds one final line break, and the empty line the old value
    // ended with is not left behind (it would be part of the new value).
    assert_eq!(
        set("a: |+\n  old\n\nb: 1\n", "a", "new\n"),
        "a: |+\n  new\nb: 1\n"
    );
    assert_eq!(
        set("a: |+\n  old\n\nb: 1\n", "a", "new"),
        "a: |-\n  new\nb: 1\n"
    );
}

#[test]
fn a_folded_block_stays_folded() {
    assert_eq!(
        set("a: >\n  old text\nb: 1\n", "a", "new text\n"),
        "a: >\n  new text\nb: 1\n"
    );
    assert_eq!(
        set("a: >\n  old\nb: 1\n", "a", "short"),
        "a: >-\n  short\nb: 1\n"
    );
    assert_eq!(
        set("a: >-\n  old\nb: 1\n", "a", "short"),
        "a: >-\n  short\nb: 1\n"
    );
    // One line break between lines of text folds to a space, so it is
    // written as an empty line.
    assert_eq!(
        set("a: >\n  old\nb: 1\n", "a", "one\ntwo\n"),
        "a: >\n  one\n\n  two\nb: 1\n"
    );
    assert_eq!(
        set("a: >\n  old\nb: 1\n", "a", "one\n\ntwo\n"),
        "a: >\n  one\n\n\n  two\nb: 1\n"
    );
    // A more-indented line is not folded: one break is kept as it is.
    assert_eq!(
        set("a: >\n  old\nb: 1\n", "a", "one\n  two\nthree\n"),
        "a: >\n  one\n    two\n  three\nb: 1\n"
    );
}

#[test]
fn block_scalars_in_sequences_and_nested_mappings_read_back() {
    for src in [
        "- |\n  old\n- 2\n",
        "- >-\n    old\n- 2\n",
        "k:\n  - a: |+\n      old\n\n    b: 1\n",
        "k:\n    a: |2\n       old\n    b: 1\n",
    ] {
        for v in [
            "x",
            "x\n",
            "x\n\n",
            "x y\nz\n",
            " lead\nx\n",
            "a\n\n\nb",
            "\nx\n",
            "x\n  y\n",
            "tab\there\n",
        ] {
            let mut d = Document::parse(src).unwrap();
            let before = d.to_string();
            let target = if src.starts_with("- ") {
                path![0]
            } else if src.starts_with("k:\n  -") {
                path!["k", 0, "a"]
            } else {
                path!["k", "a"]
            };
            d.set_path(&target, v).unwrap();
            let out = d.to_string();
            let read: Value = oneyaml::from_str(&out).unwrap();
            let got = match &read {
                Value::Sequence(s) => s.get(0).cloned(),
                m => m
                    .get("k")
                    .and_then(|k| k.get(0).and_then(|i| i.get("a")).or_else(|| k.get("a")))
                    .cloned(),
            };
            assert_eq!(got, Some(Value::from(v)), "{src:?} <- {v:?}: {out:?}");
            assert_ne!(before, out);
            // The node stays the block style it was.
            let style = if src.contains('>') { '>' } else { '|' };
            assert!(out.contains(style), "{src:?} <- {v:?}: {out:?}");
        }
    }
}

#[test]
fn a_block_scalar_keeps_the_indentation_of_its_old_content() {
    assert_eq!(
        set("a: |\n      old\nb: 1\n", "a", "new\nmore\n"),
        "a: |\n      new\n      more\nb: 1\n"
    );
}

#[test]
fn a_text_that_cannot_be_a_block_scalar_is_written_another_way() {
    // An escape is needed: the style that can hold it.
    let out = set("a: |\n  old\nb: 1\n", "a", "bell\u{7}\n");
    assert!(out.starts_with("a: \""), "{out:?}");
}

#[test]
fn white_space_after_the_indicator_is_the_documents_own() {
    assert_eq!(set("a:\t1\nb:\t2\n", "b", "x"), "a:\t1\nb:\tx\n");
    let mut d = Document::parse("a:\t1\nb:\t2\n").unwrap();
    d.set_path(&path!["b"], 9).unwrap();
    assert_eq!(d.to_string(), "a:\t1\nb:\t9\n");
    let mut d = Document::parse("k:\n   -   x\n   -   y\n").unwrap();
    d.set_path(&path!["k", 1], "z").unwrap();
    assert_eq!(d.to_string(), "k:\n   -   x\n   -   z\n");
    let mut d = Document::parse("a:    1 # c\n").unwrap();
    d.set_path(&path!["a"], 2).unwrap();
    assert_eq!(d.to_string(), "a:    2 # c\n");
}

#[test]
fn a_string_set_is_plain_when_the_documents_schema_reads_a_string() {
    assert_eq!(set("a: 1\n", "a", "yes"), "a: yes\n");
    assert_eq!(set("a: 1\n", "a", "no"), "a: no\n");
    assert_eq!(set("a: 1\n", "a", "1_000"), "a: 1_000\n");
    // Another type under the schema: quoted.
    assert_eq!(set("a: x\n", "a", "12"), "a: '12'\n");
    assert_eq!(set("a: x\n", "a", "true"), "a: 'true'\n");
    assert_eq!(set("a: x\n", "a", "null"), "a: 'null'\n");
    // Syntax plain text cannot hold.
    assert_eq!(set("a: x\n", "a", "# no"), "a: '# no'\n");
    assert_eq!(set("a: x\n", "a", "k: v"), "a: 'k: v'\n");
    assert_eq!(set("a: x\n", "a", " x"), "a: ' x'\n");
    // A document read with the 1.1 booleans quotes them.
    let s = Settings::new().yaml11_booleans(true);
    let mut d = Document::parse_with("a: 1\n", &s).unwrap();
    d.set_path(&path!["a"], "yes").unwrap();
    assert_eq!(d.to_string(), "a: 'yes'\n");
}

#[test]
fn a_text_with_line_breaks_replacing_an_inline_scalar_is_double_quoted() {
    assert_eq!(set("a: x\n", "a", "x\ny"), "a: \"x\\ny\"\n");
    assert_eq!(
        set("a: \"old\"\nb: 1\n", "a", "x\ny"),
        "a: \"x\\ny\"\nb: 1\n"
    );
    assert_eq!(set("a: 'old'\nb: 1\n", "a", "x\ny"), "a: \"x\\ny\"\nb: 1\n");
    assert_eq!(set("a: x # c\n", "a", "x\ny\n"), "a: \"x\\ny\\n\" # c\n");
    let mut d = Document::parse("- 1\n- 2\n").unwrap();
    d.set_path(&path![0], "x\ny").unwrap();
    assert_eq!(d.to_string(), "- \"x\\ny\"\n- 2\n");
}

#[test]
fn a_custom_tag_stays_while_the_value_is_a_string() {
    let src = "a: !foo longoldvaluexxxxxxx\nb: 1\nc: 2\n";
    let mut d = Document::parse(src).unwrap();
    d.set_path(&path!["a"], "hi").unwrap();
    assert_eq!(d.to_string(), "a: !foo hi\nb: 1\nc: 2\n");
    d.set_path(&path!["a"], "z").unwrap();
    assert_eq!(d.to_string(), "a: !foo z\nb: 1\nc: 2\n");
    // The anchor and the tag keep their order.
    let mut d = Document::parse("a: !foo &x v\nb: *x\n").unwrap();
    d.set_path(&path!["a"], "w").unwrap();
    assert_eq!(d.to_string(), "a: !foo &x w\nb: *x\n");
    let mut d = Document::parse("a: &x !foo v\n").unwrap();
    d.set_path(&path!["a"], "w").unwrap();
    assert_eq!(d.to_string(), "a: &x !foo w\n");
}

#[test]
fn a_core_tag_stays_only_when_it_describes_the_new_value() {
    let mut d = Document::parse("a: !!str 1\nb: !!int 2\nc: !!binary aGk=\n").unwrap();
    d.set_path(&path!["a"], "x").unwrap();
    d.set_path(&path!["b"], 7).unwrap();
    assert_eq!(d.to_string(), "a: !!str x\nb: !!int 7\nc: !!binary aGk=\n");
    // A string under `!!int`, an integer under `!!str`, a string under
    // `!!binary` would be another value: the tag goes.
    d.set_path(&path!["b"], "seven").unwrap();
    d.set_path(&path!["a"], 3).unwrap();
    d.set_path(&path!["c"], "plain").unwrap();
    assert_eq!(d.to_string(), "a: 3\nb: seven\nc: plain\n");
}

#[test]
fn removing_the_last_entry_takes_the_lines_that_would_join_a_block_scalar() {
    // White space wider than the scalar's content would become content.
    let mut d =
        Document::parse("top:\n   key1: |\n     t\n   key2: v\n      \nafter: 1\n").unwrap();
    let _ = d.remove_path(&path!["top", "key2"]).unwrap();
    d.set_path(&path!["top", "key1"], "a b").unwrap();
    assert_eq!(d.to_string(), "top:\n   key1: |-\n     a b\nafter: 1\n");
    // So would a comment indented under the removed entry.
    let mut d = Document::parse("a:\n  k: |\n    v\n  x: 1\n    # c\nb: 2\n").unwrap();
    let _ = d.remove_path(&path!["a", "x"]).unwrap();
    assert_eq!(d.to_string(), "a:\n  k: |\n    v\nb: 2\n");
    let read: Value = oneyaml::from_str(&d.to_string()).unwrap();
    assert_eq!(
        read.get("a").and_then(|a| a.get("k")),
        Some(&Value::from("v\n"))
    );
    // A comment less indented than the content is not part of the scalar:
    // it stays where it is.
    let mut d = Document::parse("a:\n  k: |\n    v\n  x: 1\n  # c\nb: 2\n").unwrap();
    let _ = d.remove_path(&path!["a", "x"]).unwrap();
    assert_eq!(d.to_string(), "a:\n  k: |\n    v\n  # c\nb: 2\n");
}

#[test]
fn removing_the_only_entry_with_a_block_scalar_key_before_a_margin_colon() {
    let mut d = Document::parse("c:\n  ? >\n   a\n: 1\n").unwrap();
    let _ = d.remove_path(&path!["c", "a\n"]).unwrap();
    assert_eq!(d.to_string(), "c: {}\n: 1\n");
}

#[test]
fn an_empty_path_key_names_the_entry_with_no_key_text() {
    let mut d = Document::parse("[: 0]\n").unwrap();
    let _ = d.remove_path(&path![0, ""]).unwrap();
    assert_eq!(d.to_string(), "[{}]\n");
    let d = Document::parse("a: 1\n: 2\n").unwrap();
    assert_eq!(
        d.get_path(&path![""]).map(|n| n.text().into_owned()),
        Some("2".into())
    );
    // An empty string key is found as before, and a missing one is missing.
    let d = Document::parse("'': 3\na: 1\n").unwrap();
    assert_eq!(
        d.get_path(&path![""]).map(|n| n.text().into_owned()),
        Some("3".into())
    );
    let d = Document::parse("a: 1\n").unwrap();
    assert!(d.get_path(&path![""]).is_none());
}

/// Every string set over a node of any style reads back exactly (the set is
/// never refused, never written as another value).
#[test]
fn every_string_set_reads_back_exactly() {
    let alphabet = [
        'a', 'b', ' ', ' ', '\n', '\n', '\t', '#', ':', '-', '\'', '"', 'é',
    ];
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let srcs = [
        "a: x\nb: 1\n",
        "a: 'x'\nb: 1\n",
        "a: \"x\"\nb: 1\n",
        "a: |\n  x\nb: 1\n",
        "a: |-\n   x\nb: 1\n",
        "a: |+\n  x\n\nb: 1\n",
        "a: >\n  x\nb: 1\n",
        "a: >-\n  x\nb: 1\n",
        "a: >+\n x\n\nb: 1\n",
        "a: !t x\nb: 1\n",
        "k:\n  - |\n    x\n  - 2\n",
        "a: |2\n   x\n",
        "a: |\n  x\n",
    ];
    for _ in 0..400 {
        let len = (next() % 9) as usize;
        let v: String = (0..len)
            .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
            .collect();
        for src in srcs {
            let mut d = Document::parse(src).unwrap();
            let target = if src.starts_with("k:") {
                path!["k", 0]
            } else {
                path!["a"]
            };
            d.set_path(&target, v.as_str())
                .map_err(|e| format!("{src:?} <- {v:?}: {e}"))
                .unwrap();
            let out = d.to_string();
            let read: Value = oneyaml::from_str(&out)
                .map_err(|e| format!("{src:?} <- {v:?}: {out:?}: {e}"))
                .unwrap();
            let got = match read.get("k") {
                Some(k) => k.get(0).cloned(),
                None => read.get("a").cloned(),
            };
            let got = got.map(|g| match g {
                Value::Tagged(t) => t.value.clone(),
                g => g,
            });
            assert_eq!(
                got,
                Some(Value::from(v.as_str())),
                "{src:?} <- {v:?}: {out:?}"
            );
            if src.starts_with("a:") && !src.ends_with("|2\n   x\n") {
                assert_eq!(
                    read.get("b").and_then(Value::as_i64).unwrap_or(1),
                    1,
                    "{out:?}"
                );
            }
        }
    }
}

#[test]
fn an_indentation_indicator_counts_from_the_dash_or_the_key() {
    for (src, target) in [
        ("- |1\n x\n- 2\n", path![0]),
        ("- >2-\n   x\n- 2\n", path![0]),
        ("k:\n  - |1\n     x\n  - 2\n", path!["k", 0]),
        ("k: |1\n x\nz: 2\n", path!["k"]),
    ] {
        for v in ["y", " y\n", "  y\nz\n", "y\n\n"] {
            let mut d = Document::parse(src).unwrap();
            d.set_path(&target, v).unwrap();
            let out = d.to_string();
            let read: Value = oneyaml::from_str(&out).unwrap();
            let got = match (&read, target.len()) {
                (Value::Sequence(s), 1) => s.get(0).cloned(),
                (r, _) => r
                    .get("k")
                    .and_then(|k| k.get(0).cloned().or_else(|| Some(k.clone()))),
            };
            assert_eq!(got, Some(Value::from(v)), "{src:?} <- {v:?}: {out:?}");
        }
    }
}

#[test]
fn a_tab_after_the_indicator_stays_for_a_scalar_and_gives_way_to_a_collection() {
    use std::collections::BTreeMap;
    let mut map = BTreeMap::new();
    map.insert("k", "v");
    for (src, target) in [
        ("-\tx\n- y\n", path![0]),
        ("- \tx\n- y\n", path![0]),
        ("k:\n  -\tx\n", path!["k", 0]),
        ("k:\tx\n", path!["k"]),
    ] {
        // A scalar keeps the gap.
        let mut d = Document::parse(src).unwrap();
        d.set_path(&target, "s").unwrap();
        assert!(d.to_string().contains('\t'), "{src:?}: {:?}", d.to_string());
        // A block collection starts after one space.
        let mut d = Document::parse(src).unwrap();
        d.set_path(&target, vec![1, 2]).unwrap();
        let out = d.to_string();
        assert!(!out.contains('\t'), "{src:?}: {out:?}");
        let read: Value = oneyaml::from_str(&out).unwrap();
        assert!(format!("{read:?}").contains("Number"), "{out:?}");
        let mut d = Document::parse(src).unwrap();
        d.set_path(&target, &map).unwrap();
        let out = d.to_string();
        assert!(!out.contains('\t'), "{src:?}: {out:?}");
        let _: Value = oneyaml::from_str(&out).unwrap();
    }
}

#[test]
fn a_block_scalar_under_a_wide_indent_never_multiplies_past_the_document_limit() {
    use oneyaml::Limits;
    let settings = Settings::new().limits(Limits::default().max_document_size(1 << 20));
    let src = format!("a: |\n{}x\nb: 1\n", " ".repeat(2000));
    let mut d = Document::parse_with(src.as_str(), &settings).unwrap();
    // 1000 lines at 2000 columns would be two megabytes of block text; the
    // value is written another way instead of being built first.
    let text = "y\n".repeat(1000);
    d.set_path(&path!["a"], text.as_str()).unwrap();
    let out = d.to_string();
    assert!(out.len() < 20_000, "{}", out.len());
    let read: Value = oneyaml::from_str(&out).unwrap();
    assert_eq!(read.get("a").and_then(Value::as_str), Some(text.as_str()));
    // Within the limit the block style stays.
    let src = format!("a: |\n{}x\nb: 1\n", " ".repeat(20));
    let mut d = Document::parse_with(src.as_str(), &settings).unwrap();
    d.set_path(&path!["a"], "y\nz\n").unwrap();
    assert!(d.to_string().starts_with("a: |\n"), "{:?}", d.to_string());
}

#[test]
fn the_empty_path_key_names_the_key_with_no_content() {
    use oneyaml::EditError;
    for src in [
        "a: 1\n: 2\n",
        "a: 1\n&x : 2\n",
        "a: 1\n!!null : 2\n",
        "a: 1\n'': 2\n",
        "a: 1\n\"\": 2\n",
        "{a: 1, : 2}\n",
    ] {
        let d = Document::parse(src).unwrap();
        let two = |d: &Document| d.get_path(&path![""]).map(|n| n.text().into_owned());
        assert_eq!(two(&d).as_deref(), Some("2"), "{src:?}");
        assert_eq!(
            d.get("").map(|n| n.text().into_owned()).as_deref(),
            Some("2")
        );
        // Setting replaces that value and creates no second key.
        let mut s = d.clone();
        s.set_path(&path![""], 9).unwrap();
        let out = s.to_string();
        assert_eq!(out.matches("9").count(), 1, "{src:?}: {out:?}");
        assert_eq!(
            Document::parse(out.as_str())
                .unwrap()
                .get("")
                .map(|n| n.text().into_owned())
                .as_deref(),
            Some("9")
        );
        assert_eq!(
            Document::parse(out.as_str())
                .unwrap()
                .get("a")
                .map(|n| n.text().into_owned())
                .as_deref(),
            Some("1")
        );
        // Removing takes that entry.
        let mut r = d.clone();
        let _ = r.remove_path(&path![""]).unwrap();
        assert!(
            r.get_path(&path![""]).is_none(),
            "{src:?}: {:?}",
            r.to_string()
        );
        assert!(r.get_path(&path!["a"]).is_some());
        let mut m = d.clone();
        m.get_path_mut(&path![""]).unwrap();
    }
    // Both kinds present: refused as ambiguous, nothing changes.
    for src in ["'': 1\n: 2\n", "\"\": 1\n&x : 2\n"] {
        let d = Document::parse(src).unwrap();
        assert!(d.get_path(&path![""]).is_none(), "{src:?}");
        assert!(d.get("").is_none());
        let mut e = d.clone();
        assert!(matches!(
            e.set_path(&path![""], 9),
            Err(EditError::Unsupported { .. })
        ));
        assert!(matches!(
            e.remove_path(&path![""]),
            Err(EditError::Unsupported { .. })
        ));
        assert!(matches!(
            e.get_path_mut(&path![""]),
            Err(EditError::Unsupported { .. })
        ));
        assert_eq!(e.to_string(), src);
    }
    // No empty key yet: setting creates one, and setting again replaces it.
    let mut d = Document::parse("a: 1\n").unwrap();
    d.set_path(&path![""], 1).unwrap();
    d.set_path(&path![""], 2).unwrap();
    assert_eq!(
        d.to_string()
            .lines()
            .filter(|l| l.starts_with("''") || l.starts_with(':'))
            .count(),
        1,
        "{:?}",
        d.to_string()
    );
}

#[test]
fn a_block_scalar_that_ends_the_file_without_a_line_break_keeps_it_missing() {
    for (src, v, want) in [
        ("a: |\n  old", "n", "a: |-\n  n"),
        ("a: >\n  old", "n", "a: >-\n  n"),
        ("- |\n  old", "n", "- |-\n  n"),
        ("a: |\n  old", "n\n", "a: |\n  n\n"),
        ("a: |-\r\n  old", "n", "a: |-\r\n  n"),
    ] {
        let mut d = Document::parse(src).unwrap();
        let target = if src.starts_with('-') {
            path![0]
        } else {
            path!["a"]
        };
        d.set_path(&target, v).unwrap();
        assert_eq!(d.to_string(), want, "{src:?}");
        let read: Value = oneyaml::from_str(want).unwrap();
        let got = match &read {
            Value::Sequence(s) => s.get(0).cloned(),
            r => r.get("a").cloned(),
        };
        assert_eq!(got, Some(Value::from(v)), "{src:?}");
    }
    // A value that is not a block scalar leaves the line unfinished too.
    let mut d = Document::parse("a: |\n  old").unwrap();
    d.set_path(&path!["a"], 5).unwrap();
    assert_eq!(d.to_string(), "a: 5");
}
