//! Owner findings, pinned (the rulebook's edit
//! semantics, `docs/comment-attachment.md`). Each edit either keeps every
//! comment with its rulebook owner (the owners are checked with
//! `explain_comments`) or is refused with nothing changed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{CommentOwner, Document, EditError, Value, path};

/// Every comment as `text=owner`, the owner by kind and node text.
fn owners(d: &Document) -> Vec<String> {
    d.explain_comments()
        .unwrap()
        .iter()
        .map(|e| {
            let node = |n| d.node(n).unwrap().text().into_owned();
            let o = match e.owner() {
                CommentOwner::Leading(n) => format!("L({})", node(n)),
                CommentOwner::Trailing(n) => format!("T({})", node(n)),
                CommentOwner::Footer(n) => format!("F({})", node(n)),
                CommentOwner::DocumentHeader => "header".to_owned(),
                CommentOwner::DocumentFooter => "footer".to_owned(),
                other => format!("{other:?}"),
            };
            format!("{}={o}", e.text())
        })
        .collect()
}

/// An only entry leaving a collection keeps its
/// non-default tag; only the Core defaults `!!map`/`!!seq` may go.
#[test]
fn only_entry_collapse_keeps_the_tag() {
    for (src, p, want) in [
        (
            "--- !!omap\n- Mark McGwire: 65\n",
            path![0],
            "--- !!omap []\n",
        ),
        ("k: !!set\n  ? a\n", path!["k", "a"], "k: !!set {}\n"),
        ("k: !foo\n  - 1\n", path!["k", 0], "k: !foo []\n"),
        ("k: !!map\n  a: 1\n", path!["k", "a"], "k: {}\n"),
        ("k: !!seq\n- 1\n", path!["k", 0], "k: []\n"),
    ] {
        let mut d = Document::parse(src).unwrap();
        let tag_before = Value::try_from(&d).unwrap();
        let _ = d.remove_path(&p).unwrap();
        assert_eq!(d.to_string(), want, "{src:?}");
        // The meaning after is the one a re-parse gives, and a
        // non-default tag is still there.
        let again: Value = oneyaml::from_str(&d.to_string()).unwrap();
        assert_eq!(Value::try_from(&d).unwrap(), again, "{src:?}");
        let tagged = |v: &Value| format!("{v:?}").contains("Tagged");
        let default = src.contains("!!map") || src.contains("!!seq");
        assert_eq!(tagged(&again), tagged(&tag_before) && !default, "{src:?}");
    }
}

/// Replacing a value keeps the entry's (and the node's
/// own) comments that sit inside the old value's first line.
#[test]
fn replace_keeps_the_entrys_comments_inside_the_old_value() {
    for (src, p, want, owner) in [
        (
            "top2: &node2 # comment\n  key2: two\n",
            path!["top2"],
            "top2: &node2 7 # comment\n",
            "# comment=T(top2)",
        ),
        (
            "k: [ # lead\n  a, b]\n",
            path!["k"],
            "k: 7 # lead\n",
            "# lead=T(k)",
        ),
        (
            "- !!map # Block collection\n  foo : bar\n",
            path![0],
            "- 7 # Block collection\n",
            "# Block collection=T(7)",
        ),
        (
            "- | # Empty header\n literal\n",
            path![0],
            "- 7 # Empty header\n",
            "# Empty header=T(7)",
        ),
        // A same-line comment of the value too: the entry's goes on the
        // key's line, the value below it.
        (
            "k: [ # a\n  b] # c\n",
            path!["k"],
            "k: # a\n  7 # c\n",
            "# a=T(k)",
        ),
    ] {
        let mut d = Document::parse(src).unwrap();
        d.set_path(&p, 7).unwrap();
        assert_eq!(d.to_string(), want, "{src:?}");
        assert!(
            owners(&d).contains(&owner.to_owned()),
            "{src:?}: {:?}",
            owners(&d)
        );
    }
    // New values on lines of their own: on the key's (or dash's) line.
    let mut d = Document::parse("- &b one # Explicit compact\n").unwrap();
    d.set_path(&path![0], vec![1, 2]).unwrap();
    assert_eq!(d.to_string(), "- &b # Explicit compact\n  - 1\n  - 2\n");
    let mut d = Document::parse("- !!map # Block collection\n  foo : bar\n").unwrap();
    d.set_path(&path![0], vec![1, 2]).unwrap();
    assert_eq!(d.to_string(), "- # Block collection\n  - 1\n  - 2\n");
}

/// A flow entry's comment before its `,` stays its own
/// when a new entry goes in before it.
#[test]
fn flow_insert_keeps_a_comment_before_the_comma() {
    let mut d = Document::parse("[ word1\n# comment\n, word2]\n").unwrap();
    assert_eq!(owners(&d), ["# comment=L(word2)"]);
    d.root_mut().unwrap().insert_at(1, 5).unwrap();
    assert_eq!(d.to_string(), "[ word1, 5\n# comment\n, word2]\n");
    assert_eq!(owners(&d), ["# comment=L(word2)"]);
}

/// A new anchor on a block collection below comment
/// lines goes on the key's line; the comments keep their owners.
#[test]
fn set_anchor_keeps_the_first_entrys_leading_comments() {
    let mut d = Document::parse("k:\n  # about a\n  a: 1\n  b: 2\n").unwrap();
    let k = d.id_of(&path!["k"]).unwrap();
    d.set_anchor(k, Some("x")).unwrap();
    assert_eq!(d.to_string(), "k: &x\n  # about a\n  a: 1\n  b: 2\n");
    assert_eq!(owners(&d), ["# about a=L(a)"]);
    // After `---`: the anchor's line above the comment lines.
    let mut d = Document::parse("---\n# c\na: 1\n").unwrap();
    assert_eq!(owners(&d), ["# c=L(a)"]);
    let root = d.root().unwrap().id();
    d.set_anchor(root, Some("x")).unwrap();
    assert_eq!(d.to_string(), "---\n&x\n# c\na: 1\n");
    assert_eq!(owners(&d), ["# c=L(a)"]);
    // Comment rule 12: a root without `---` below comment
    // lines of its first entry: the anchor's line goes above them, and
    // they stay the entry's (the baseline re-owned them; a later version
    // refused).
    for (src, want, owner) in [
        ("# c\na: 1\n", "&x\n# c\na: 1\n", "# c=L(a)"),
        ("# c\n- 1\n", "&x\n# c\n- 1\n", "# c=L(1)"),
    ] {
        let mut d = Document::parse(src).unwrap();
        assert_eq!(owners(&d), [owner], "{src:?}");
        let root = d.root().unwrap().id();
        d.set_anchor(root, Some("x")).unwrap();
        assert_eq!(d.to_string(), want);
        assert_eq!(owners(&d), [owner], "{src:?}");
    }
    // A header above them (rule 2) stays the header.
    let mut d = Document::parse("# h\n\n# c\na: 1\n").unwrap();
    let root = d.root().unwrap().id();
    d.set_anchor(root, Some("x")).unwrap();
    assert_eq!(d.to_string(), "# h\n\n&x\n# c\na: 1\n");
    assert_eq!(owners(&d), ["# h=header", "# c=L(a)"]);
}

/// A nested collection an only entry leaves keeps its
/// footer comments inside the new flow collection, never handing them to
/// a sibling.
#[test]
fn nested_collapse_keeps_the_footer() {
    let mut d = Document::parse("k:\n  a: 1\n  # footer k\nz: 1\n").unwrap();
    let _ = d.remove_path(&path!["k", "a"]).unwrap();
    assert_eq!(d.to_string(), "k: {\n  # footer k\n  }\nz: 1\n");
    assert_eq!(owners(&d), ["# footer k=F({\n  # footer k\n  })"]);
    let mut d = Document::parse("- a: 1\n  # f\n- 2\n").unwrap();
    let _ = d.remove_path(&path![0, "a"]).unwrap();
    assert_eq!(d.to_string(), "- {\n  # f\n  }\n- 2\n");
}

/// The old value's inner comments go with it, wherever
/// they stand (the leading comment of its first item, between the key and
/// the value; above a root).
#[test]
fn replace_takes_the_old_values_inner_comments() {
    let src = "rbi:\n  # 1998 rbi ranking\n  - Sammy Sosa\n  - Ken Griffey\nz: 1\n";
    let mut d = Document::parse(src).unwrap();
    assert_eq!(owners(&d), ["# 1998 rbi ranking=L(Sammy Sosa)"]);
    d.set_path(&path!["rbi"], 7).unwrap();
    assert!(!d.to_string().contains('#'), "{:?}", d.to_string());
    assert!(owners(&d).is_empty());
    // A comment block above a root collection (rule 3: its first entry's).
    let mut d = Document::parse("---\n# Products\n- a: 1\n").unwrap();
    d.set_root(1).unwrap();
    assert_eq!(d.to_string(), "---\n1\n");
    // With a blank line it is the document header (rule 2), and stays.
    let mut d = Document::parse("# Products\n\n- a: 1\n").unwrap();
    d.set_root(1).unwrap();
    assert_eq!(owners(&d), ["# Products=header"]);
}

/// The refusals the findings' fixes add are specific and change nothing.
#[test]
fn owner_refusals_are_named() {
    // Two comments of the entry inside the old value.
    let src = "key: &a\n  # c1\n  !!map # t\n  a: b\n";
    let mut d = Document::parse(src).unwrap();
    let e = d.set_path(&path!["key"], 7).unwrap_err();
    assert!(
        matches!(&e, EditError::Unsupported { reason, .. } if reason.contains("cannot be written back")),
        "{e:?}"
    );
    assert_eq!(d.to_string(), src);
}

/// The documented limitations: the residual classes P1-P7 of the
/// broadened differential are refused with a specific, documented error
/// and change nothing; these are the examples on `Document`'s docs.
#[test]
fn documented_p_classes_are_refused_by_name() {
    fn reason(e: &EditError) -> &'static str {
        match e {
            EditError::Unsupported { reason, .. } => reason,
            _ => "",
        }
    }
    fn refused_move(src: &str, from: usize, to: usize) -> EditError {
        let mut d = Document::parse(src).unwrap();
        let e = d.root_mut().unwrap().move_entry(from, to).unwrap_err();
        assert_eq!(d.to_string(), src);
        e
    }
    // P1: a block entry with a comment into a flow collection; the
    // refused re-insertion hands the `Removed` back, text unchanged.
    let mut d = Document::parse("a: 1 # t\nb: {c: 2}\n").unwrap();
    let r = d.remove_path(&path!["a"]).unwrap();
    assert_eq!(r.comments(), ["# t"]);
    let keep = r.clone();
    let e = d
        .get_path_mut(&path!["b"])
        .unwrap()
        .insert_removed(r)
        .unwrap_err();
    assert_eq!(
        reason(e.error()),
        "block content or comments cannot move into a flow collection",
        "P1: {e:?}"
    );
    assert_eq!(e.removed(), &keep);
    assert_eq!(d.to_string(), "b: {c: 2}\n");
    // P2: moving the empty-key entry to the middle re-owns `# c`.
    let e = refused_move("a: 1\n  # c\n: # e\n # d\n\"q\":\n- e\n", 2, 1);
    assert!(
        matches!(e, EditError::CommentReattaches { .. }),
        "P2: {e:?}"
    );
    // P3: a flow entry with a leading comment line moved.
    let e = refused_move("[\n# c\n  b, c, d]\n", 0, 1);
    assert_eq!(
        reason(&e),
        "a flow entry with comments around it cannot be re-inserted yet",
        "P3: {e:?}"
    );
    // P4: see `owner_refusals_are_named`. P5: `  # c` would read as the
    // block scalar's content.
    let e = refused_move("a: 1\n\n  # c\n\nb: 2\n\nt: |\n  x\n", 2, 1);
    assert!(matches!(e, EditError::NotWellFormed { .. }), "P5: {e:?}");
    // P6: removing `&n`, whose line holds `# t`.
    let src = "k:\n  &n # t\n  a: 1\n";
    let mut d = Document::parse(src).unwrap();
    let k = d.id_of(&path!["k"]).unwrap();
    let e = d.set_anchor(k, None).unwrap_err();
    assert!(
        matches!(e, EditError::CommentReattaches { .. }),
        "P6: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // P7: a nested collapse with `# p` on the properties' line and a
    // footer `# f`.
    let src = "e:\n &n # p\n - x: y\n  # f\nz: 1\n";
    let mut d = Document::parse(src).unwrap();
    let e = d.remove_path(&path!["e", 0]).unwrap_err();
    assert_eq!(
        reason(&e),
        "the footer comments of a nested collection whose only entry is removed have no owner",
        "P7: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // Without the footer, the collapse keeps the properties' comment.
    let mut d = Document::parse("e:\n &n # p\n - x: y\nz: 1\n").unwrap();
    let _ = d.remove_path(&path!["e", 0]).unwrap();
    assert_eq!(d.to_string(), "e: &n [] # p\nz: 1\n");
}
