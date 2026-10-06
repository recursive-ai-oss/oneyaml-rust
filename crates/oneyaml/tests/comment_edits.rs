//! Node edits and comments (`docs/comment-attachment.md`): node edits
//! carry exactly the comments the rulebook gives them — pinned shapes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{CommentOwner, Document, EditError, path};

fn texts(r: &oneyaml::Removed) -> Vec<&str> {
    r.comments().iter().map(String::as_str).collect()
}

/// (a) A removal takes exactly the entry's comments (its leading lines,
/// same-line comment, the comments inside it, the footers of the
/// collections inside it) with the blank lines they carry (rule 8), and
/// nothing else; putting it back restores the bytes.
#[test]
fn remove_takes_exactly_the_owned_comments_and_blank_lines() {
    for nl in ["\n", "\r\n", "\r"] {
        let src = "# head\n\na: 1 # ta\n\n# about b\nb: # tb\n  x: 1 # tx\n  # footer of b's map\n# about c\nc: 3\n# doc footer\n"
            .replace('\n', nl);
        let mut d = Document::parse(&src).unwrap();
        let b = d.id_of(&path!["b"]).unwrap();
        let r = d.remove_node(b).unwrap();
        assert_eq!(
            texts(&r),
            ["# about b", "# tb", "# tx", "# footer of b's map"],
            "{nl:?}"
        );
        assert_eq!(r.comment_ids().len(), 4);
        let want = "# head\n\na: 1 # ta\n# about c\nc: 3\n# doc footer\n".replace('\n', nl);
        assert_eq!(d.to_string().as_bytes(), want.as_bytes());
        // Every other comment kept its owner (rules 2, 3/4, 5, 7).
        let c = d.get_path(&path!["c"]).unwrap().key().unwrap().id();
        assert_eq!(d.comments(c).unwrap().leading()[0].text(), "about c");
        let hf: Vec<String> = d
            .header()
            .unwrap()
            .iter()
            .map(|c| c.text().to_owned())
            .collect();
        assert_eq!(hf, ["head"]);
        // Back at its place: the original bytes (blank line included).
        d.root_mut().unwrap().insert_removed_at(1, r).unwrap();
        assert_eq!(d.to_string().as_bytes(), src.as_bytes(), "{nl:?}");
    }
}

/// `remove_keep_comments`: the leading comments become the next entry's.
#[test]
fn remove_keep_comments_rehomes_to_the_next_entry() {
    let src = "k:\n  a: 1\n  # about b\n  b: 2 # tb\n  c: 3\n";
    let mut d = Document::parse(src).unwrap();
    let b = d.id_of(&path!["k", "b"]).unwrap();
    let r = d.remove_keep_comments(b).unwrap();
    assert_eq!(texts(&r), ["# tb"]);
    assert_eq!(d.to_string(), "k:\n  a: 1\n  # about b\n  c: 3\n");
    let c = d.get_path(&path!["k", "c"]).unwrap().key().unwrap().id();
    let e = d.explain_comments().unwrap();
    let about = e.iter().find(|x| x.text().contains("about b")).unwrap();
    assert_eq!(about.owner(), CommentOwner::Leading(c));
}

/// `remove_keep_comments` of a collection's last entry: the leading
/// comments become the collection's footer (rule 6: deeper than its
/// parent entry).
#[test]
fn remove_keep_comments_of_the_last_entry_rehomes_to_the_footer() {
    let src = "k:\n  a: 1\n  # about b\n  b: 2 # tb\nz: 0\n";
    let mut d = Document::parse(src).unwrap();
    let b = d.id_of(&path!["k", "b"]).unwrap();
    let r = d.remove_keep_comments(b).unwrap();
    assert_eq!(texts(&r), ["# tb"]);
    assert_eq!(d.to_string(), "k:\n  a: 1\n  # about b\nz: 0\n");
    let k = d.id_of(&path!["k"]).unwrap();
    let e = d.explain_comments().unwrap();
    let about = e.iter().find(|x| x.text().contains("about b")).unwrap();
    assert_eq!(about.owner(), CommentOwner::Footer(k));
}

/// Refusals of `remove_keep_comments`, each leaving the text unchanged:
/// an only entry (its collection becomes `{}`) and a flow entry have no
/// owner for the comments; a comment the rules would give to another
/// owner than the next entry or the footer re-attaches. For a
/// root collection's last entry the call re-homes them to the
/// document footer (the call is the explicit request).
#[test]
fn remove_keep_comments_refusals() {
    for (src, p) in [
        ("k:\n  # about a\n  a: 1\nz: 0\n", path!["k", "a"]),
        ("k: [\n  # about a\n  a,\n  b\n  ]\n", path!["k", 0]),
    ] {
        let mut d = Document::parse(src).unwrap();
        let id = d.id_of(&p).unwrap();
        let e = d.remove_keep_comments(id).unwrap_err();
        assert!(matches!(e, EditError::Unsupported { .. }), "{src:?}: {e:?}");
        assert_eq!(d.to_string(), src);
    }
    // The document footer, as the call asks.
    let mut d = Document::parse("a: 1\n# about b\nb: 2\n").unwrap();
    let b = d.id_of(&path!["b"]).unwrap();
    assert!(d.remove_keep_comments(b).unwrap().comments().is_empty());
    assert_eq!(d.to_string(), "a: 1\n# about b\n");
    let e = d.explain_comments().unwrap();
    assert_eq!(e[0].owner(), CommentOwner::DocumentFooter);
    // A shallow leading comment (rule 10) that, without the entry below
    // it, would lead the next root entry: refused, nothing changed.
    let src = "k:\n  a: 1\n# about b\n  b: 2\nz: 0\n";
    let mut d = Document::parse(src).unwrap();
    let b = d.id_of(&path!["k", "b"]).unwrap();
    let e = d.remove_keep_comments(b).unwrap_err();
    assert!(matches!(e, EditError::CommentReattaches { .. }), "{e:?}");
    assert_eq!(d.to_string(), src);
    // Without leading comments there is nothing to keep: a plain removal.
    let mut d = Document::parse("a: 1\nb: 2 # tb\n").unwrap();
    let b = d.id_of(&path!["b"]).unwrap();
    assert_eq!(texts(&d.remove_keep_comments(b).unwrap()), ["# tb"]);
    assert_eq!(d.to_string(), "a: 1\n");
}

/// Re-insertions at the original place that the sweep
/// (`bench/tests/reinsert_roundtrip.rs`) found: an empty folded scalar
/// followed by a blank line (yaml-test-suite K858) goes back byte for
/// byte; a flow entry followed by another entry's comment before its `,`
/// (7TMG) and a `%TAG`-handle tag (6CK3) are documented refusals, which
/// change nothing and hand the `Removed` back.
#[test]
fn reinsertion_shapes_from_the_suite() {
    let src = "strip: >-\n\nclip: >\n\nkeep: |+\n\n";
    let mut d = Document::parse(src).unwrap();
    let r = d.remove_path(&path!["clip"]).unwrap();
    d.root_mut().unwrap().insert_removed_at(1, r).unwrap();
    assert_eq!(d.to_string(), src);

    for (src, reason) in [
        (
            "---\n[ word1\n# comment\n, word2]\n",
            "a flow entry with comments around it cannot be re-inserted yet",
        ),
        (
            "%TAG !e! tag:example.com,2000:app/\n---\n- !local foo\n- !e!tag%21 baz\n",
            "a tag written into a document with %TAG directives",
        ),
    ] {
        let mut d = Document::parse(src).unwrap();
        let at = if src.starts_with('%') { 1 } else { 0 };
        let r = d.remove_path(&path![at]).unwrap();
        let after = d.to_string();
        let keep = r.clone();
        let e = d.root_mut().unwrap().insert_removed_at(at, r).unwrap_err();
        assert!(
            matches!(e.error(), EditError::Unsupported { reason: x, .. } if *x == reason),
            "{src:?}: {e:?}"
        );
        assert_eq!(e.removed(), &keep);
        assert_eq!(d.to_string(), after);
    }
}

/// The document header never changes as a side effect: an anchor
/// that would make an empty root visible (turning the header below `---`
/// into the footer) is refused, as is removing the anchor that makes it
/// visible (the footer would become the header).
#[test]
fn anchors_on_an_empty_root_keep_the_header_and_footer() {
    let src = "---\n# only a comment\n";
    let mut d = Document::parse(src).unwrap();
    let root = d.root().unwrap().id();
    let e = d.set_anchor(root, Some("x")).unwrap_err();
    assert!(matches!(e, EditError::CommentReattaches { .. }), "{e:?}");
    assert_eq!(d.to_string(), src);

    let src = "--- &x\n# c\n";
    let mut d = Document::parse(src).unwrap();
    let root = d.root().unwrap().id();
    let e = d.set_anchor(root, None).unwrap_err();
    assert!(matches!(e, EditError::CommentReattaches { .. }), "{e:?}");
    assert_eq!(d.to_string(), src);

    // Without comments around, both are accepted.
    let mut d = Document::parse("---\n").unwrap();
    let root = d.root().unwrap().id();
    let root = d.set_anchor(root, Some("x")).unwrap();
    assert_eq!(d.to_string(), "--- &x\n");
    d.set_anchor(root, None).unwrap();
    assert_eq!(d.to_string(), "---\n");
}

/// A move whose comment lines would become block scalar content (the
/// leading comments of the first entry, `  # c0` at column 2, below a
/// `|2-` scalar moved first) is never an `Internal` error (edit fuzz
/// seed 10): those leading comment lines are written at the entry's
/// column instead (as the baseline accepted them), owners unchanged.
#[test]
fn comment_lines_never_become_block_scalar_content() {
    for nl in ["\n", "\r\n"] {
        let src = "  # c0\n# c1\na: null\n'[q]': a,b\nb: |2-\n   a\n  b\n".replace('\n', nl);
        let mut d = Document::parse(&src).unwrap();
        let before = d.to_value().unwrap();
        d.root_mut().unwrap().move_entry(2, 0).unwrap();
        let want = "b: |2-\n   a\n  b\n# c0\n# c1\na: null\n'[q]': a,b\n".replace('\n', nl);
        assert_eq!(d.to_string(), want);
        assert_eq!(d.to_value().unwrap(), before);
        let a = d.get_path(&path!["a"]).unwrap().key().unwrap().id();
        for e in d.explain_comments().unwrap() {
            assert_eq!(e.owner(), CommentOwner::Leading(a));
        }
    }
}

/// The documented limitations: each 0.3 limitation class S1-S6 is
/// refused with a specific, documented error and changes nothing; these
/// are the examples on `Document`'s docs.
#[test]
fn documented_classes_are_refused_by_name() {
    fn reason(e: &EditError) -> &'static str {
        match e {
            EditError::Unsupported { reason, .. } => reason,
            _ => "",
        }
    }
    // S1: `# d` would read as the inner sequence's footer (rule 6).
    let src = "- - a\n  - b\n # c\n    # d\n- s\n";
    let mut d = Document::parse(src).unwrap();
    let e = d.remove_path(&path![1]).unwrap_err();
    assert!(
        matches!(e, EditError::CommentReattaches { .. }),
        "S1: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // S2: the new first entry has an empty key (rule 3).
    let src = "a: 1\n# c\n: x\n";
    let mut d = Document::parse(src).unwrap();
    let e = d.remove_path(&path!["a"]).unwrap_err();
    assert!(
        matches!(e, EditError::CommentReattaches { .. }),
        "S2: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // S3: another entry's comment after the `,` before the entry.
    let src = "- [ a, b, # t\n  c ]\n";
    let mut d = Document::parse(src).unwrap();
    let e = d.remove_path(&path![0, 2]).unwrap_err();
    assert_eq!(
        reason(&e),
        "removing a flow entry next to comments of other owners that the flow layout cannot keep",
        "S3: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // S4: the properties line of `&n` holds a comment.
    let src = "top: # t\n  &n # u\n  k: v\n";
    let mut d = Document::parse(src).unwrap();
    let top = d.id_of(&path!["top"]).unwrap();
    let e = d.set_anchor(top, None).unwrap_err();
    assert_eq!(
        reason(&e),
        "removing the anchor of a node whose properties stand on a line of their own above \
         comment lines",
        "S4: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // S5: two comments of other owners between `&x` and `a: b`.
    let src = "key: &x\n  # c1\n  !!map # t\n  a: b\n";
    let mut d = Document::parse(src).unwrap();
    let e = d.remove_path(&path!["key", "a"]).unwrap_err();
    assert_eq!(
        reason(&e),
        "comments of other owners between a collection's properties and its only entry",
        "S5: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // S6: `# t` lies past the empty value of `? - a`.
    let src = "? - a # t\n";
    let mut d = Document::parse(src).unwrap();
    let key = d
        .root()
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .0
        .id();
    let e = d.node_mut(key).unwrap().push("new").unwrap_err();
    assert_eq!(
        reason(&e),
        "appending after comments that lie past an empty value (a `? ` key written without `:`)",
        "S6: {e:?}"
    );
    assert_eq!(d.to_string(), src);
    // The same shapes without the comment go through.
    let mut d = Document::parse("? - a\n").unwrap();
    let key = d
        .root()
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .0
        .id();
    d.node_mut(key).unwrap().push("new").unwrap();
    assert_eq!(d.to_string(), "? - a\n  - new\n");
    let mut d = Document::parse("- [ a, b,\n  c ]\n").unwrap();
    let _ = d.remove_path(&path![0, 2]).unwrap();
    assert_eq!(d.to_string(), "- [ a, b ]\n");
}
