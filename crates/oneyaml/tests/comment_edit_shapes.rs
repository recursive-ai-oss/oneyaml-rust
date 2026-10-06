//! Remaining shapes, pinned (the rulebook's edit semantics,
//! `docs/comment-attachment.md`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{CommentOwner, Document, EditError, path};

/// The first item of a flow collection nested in a block collection, with a
/// leading comment line at column 0, is removed with its comment (rule 9), and
/// the next item keeps its indentation (at column 0 it would end the block
/// collection). The edit-differential baseline accepted it and gave `# c` to
/// the next item; a later version refused it.
#[test]
fn flow_first_item_with_a_column_0_comment_line() {
    for nl in ["\n", "\r\n", "\r"] {
        for (src, p, want) in [
            ("- [\n# c\n  b, c]\n", path![0, 0], "- [\n  c]\n"),
            ("k: [\n# c\n  b, c]\n", path!["k", 0], "k: [\n  c]\n"),
            (
                "k: {\n# c\n  b: 1, c: 2}\n",
                path!["k", "b"],
                "k: {\n  c: 2}\n",
            ),
            // The indented comment line, as before.
            ("- [\n  # c\n  b, c]\n", path![0, 0], "- [\n  c]\n"),
        ] {
            let (src, want) = (src.replace('\n', nl), want.replace('\n', nl));
            let mut d = Document::parse(&src).unwrap();
            let r = d.remove_path(&p).unwrap();
            assert_eq!(r.comments(), ["# c"], "{src:?}");
            assert_eq!(d.to_string(), want, "{src:?}");
            assert!(d.explain_comments().unwrap().is_empty());
            // The document reads as its text.
            let again: oneyaml::Value = oneyaml::from_str(&d.to_string()).unwrap();
            assert_eq!(d.to_value().unwrap(), again);
        }
    }
    // Moving that item, or putting it back, meets the documented flow
    // limitation (classes S3/P3): refused by name, nothing changed.
    let src = "- [\n# c\n  b, c]\n";
    let mut d = Document::parse(src).unwrap();
    let e = d
        .get_path_mut(&path![0])
        .unwrap()
        .move_entry(0, 1)
        .unwrap_err();
    assert!(
        matches!(&e, EditError::Unsupported { reason, .. } if reason.contains("re-inserted yet")),
        "{e:?}"
    );
    assert_eq!(d.to_string(), src);
}

/// Moving a flow collection's first entry away from the
/// bracket's line leaves no white space ending that line; white space the
/// move did not leave stays.
#[test]
fn flow_moves_leave_no_trailing_space_after_the_bracket() {
    for (src, want) in [
        (
            "{ a: 1,\n# c1\nbar: 42, ?foo: bar\n}\n",
            "{\n# c1\nbar: 42, a: 1, ?foo: bar\n}\n",
        ),
        ("[ a,\n  # c\n  b, c, d]\n", "[\n  # c\n  b, a, c, d]\n"),
        ("[ a,\n  b]\n", "[ b, a]\n"),
        ("[ \n  a, b]\n", "[ \n  b, a]\n"),
    ] {
        let mut d = Document::parse(src).unwrap();
        d.root_mut().unwrap().move_entry(0, 1).unwrap();
        assert_eq!(d.to_string(), want, "{src:?}");
    }
}

/// `remove_keep_comments` of the last entry when the text
/// ends without a line break: the kept comment lines stay where they are,
/// with their rulebook owner (the document footer, as the call asks; a nested
/// collection's footer).
#[test]
fn keep_comments_of_a_last_entry_at_the_end_of_the_text() {
    for (src, p, want, footer) in [
        (
            "k: |\n  text\n# c\nm: 2",
            path!["m"],
            "k: |\n  text\n# c",
            true,
        ),
        ("k: 1\n# c\nm: 2", path!["m"], "k: 1\n# c", true),
        ("- 1\n# c\n- 2", path![1], "- 1\n# c", true),
        (
            "o:\n  k: 1\n  # c\n  m: 2",
            path!["o", "m"],
            "o:\n  k: 1\n  # c",
            false,
        ),
    ] {
        let mut d = Document::parse(src).unwrap();
        let m = d.id_of(&p).unwrap();
        let r = d.remove_keep_comments(m).unwrap();
        assert!(r.comments().is_empty());
        assert_eq!(d.to_string(), want, "{src:?}");
        let e = d.explain_comments().unwrap();
        let owner = e[0].owner();
        if footer {
            assert_eq!(owner, CommentOwner::DocumentFooter, "{src:?}");
        } else {
            let o = d.id_of(&path!["o"]).unwrap();
            assert_eq!(owner, CommentOwner::Footer(o), "{src:?}");
        }
    }
}

/// Removing such an item and putting
/// it back at the same place gives the original bytes, with `# c` its own
/// again (the baseline gave the bytes back too; moves and other re-insertions
/// stay the P3 limitation).
#[test]
fn flow_item_with_a_column_0_comment_line_goes_back_in_place() {
    for nl in ["\n", "\r\n", "\r"] {
        for (src, coll, i) in [
            ("- [\n# c\n  b, c]\n", path![0], 0),
            ("k: [\n# c\n  b, c, d]\n", path!["k"], 0),
            ("k: {\n# c\n  b: 1, c: 2}\n", path!["k"], 0),
            ("- - [a,\n# c\n    b, c]\n  - [d]\n", path![0, 0], 1),
            ("- [\n# c1\n# c2\n  b, c]\n", path![0], 0),
        ] {
            let src = src.replace('\n', nl);
            let mut d = Document::parse(&src).unwrap();
            let owners = |d: &Document| {
                d.explain_comments()
                    .unwrap()
                    .iter()
                    .map(|e| match e.owner() {
                        CommentOwner::Leading(n) => d.node(n).unwrap().text().into_owned(),
                        o => format!("{o:?}"),
                    })
                    .collect::<Vec<_>>()
            };
            let before = owners(&d);
            let v = {
                let c = d.get_path(&coll).unwrap();
                match c.as_mapping() {
                    Some(m) => m.iter().nth(i).unwrap().1.id(),
                    None => c.as_sequence().unwrap().get(i).unwrap().id(),
                }
            };
            let r = d.remove_node(v).unwrap();
            let c = d.id_of(&coll).unwrap();
            d.node_mut(c).unwrap().insert_removed_at(i, r).unwrap();
            assert_eq!(d.to_string().as_bytes(), src.as_bytes(), "{src:?}");
            assert_eq!(owners(&d), before, "{src:?}");
        }
    }
}

/// Limitation P8: anchoring a root mapping whose first entry has
/// an empty key below comment lines (`# c⏎: x⏎`) would give those lines to
/// the `:` entry's value (rule for explicit values): refused with
/// `CommentReattaches`, nothing changed (the baseline re-owned them).
#[test]
fn p8_root_anchor_above_an_empty_key() {
    let src = "# c\n: x\n";
    let mut d = Document::parse(src).unwrap();
    let root = d.root().unwrap().id();
    let e = d.set_anchor(root, Some("x")).unwrap_err();
    assert!(
        matches!(e, EditError::CommentReattaches { .. })
            && format!("{e:?}").contains("ExplicitValue"),
        "{e:?}"
    );
    assert_eq!(d.to_string(), src);
}
