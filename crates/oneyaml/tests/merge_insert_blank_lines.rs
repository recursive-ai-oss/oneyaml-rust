//! Merge-key insertion (`NodeMut::insert_merge`), the blank-line
//! setters (`set_blank_lines_before`, `set_comment_blank_lines`, and their
//! `DocumentMut` forwarders), and neighbour-style nulls from
//! `get_path_or_create`: behaviour and every error path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{CommentRule, Document, EditError, Settings, Stream, Value, path};

fn merge_settings() -> Settings {
    Settings::new().merge_keys(true)
}

/// The edited text read by serde means what the document means.
fn agrees_with_its_text(doc: &Document) {
    let text = doc.to_string();
    let v: Value = oneyaml::from_str_with(&text, doc.settings()).unwrap();
    assert_eq!(v, doc.to_value().unwrap(), "{text}");
    let again = Document::parse_with(text.clone(), doc.settings()).unwrap();
    assert_eq!(again.to_string(), text);
}

// ============================================================== merge

#[test]
fn insert_merge_writes_one_or_several_targets() {
    let src = "a: &a\n  x: 1\n  y: 1\nb: &b\n  y: 2\n  z: 2\nc:\n  z: 3\n";
    let mut doc = Document::parse_with(src, &merge_settings()).unwrap();
    let a = doc.id_of(&path!["a"]).unwrap();
    let b = doc.id_of(&path!["b"]).unwrap();
    let v = doc
        .get_mut("c")
        .unwrap()
        .insert_merge(&[a, b])
        .unwrap()
        .id();
    assert_eq!(doc.node(v).unwrap().text(), "[*a, *b]");
    assert_eq!(
        doc.to_string(),
        "a: &a\n  x: 1\n  y: 1\nb: &b\n  y: 2\n  z: 2\nc:\n  <<: [*a, *b]\n  z: 3\n"
    );
    // Own keys win, then earlier targets.
    assert_eq!(doc.to_value().unwrap()["c"]["x"].as_u64(), Some(1));
    assert_eq!(doc.to_value().unwrap()["c"]["y"].as_u64(), Some(1));
    assert_eq!(doc.to_value().unwrap()["c"]["z"].as_u64(), Some(3));
    agrees_with_its_text(&doc);
}

#[test]
fn insert_merge_into_a_flow_mapping() {
    let src = "a: &a {x: 1}\nb: {y: 2}\n";
    let mut doc = Document::parse_with(src, &merge_settings()).unwrap();
    let a = doc.id_of(&path!["a"]).unwrap();
    doc.get_mut("b").unwrap().insert_merge(&[a]).unwrap();
    assert_eq!(doc.to_string(), "a: &a {x: 1}\nb: {<<: *a, y: 2}\n");
    assert_eq!(doc.to_value().unwrap()["b"]["x"].as_u64(), Some(1));
    agrees_with_its_text(&doc);
}

#[test]
fn insert_merge_without_merge_keys_is_a_string_key_for_every_reader() {
    let src = "a: &a {x: 1}\nb:\n  y: 2\n";
    let mut doc = Document::parse(src).unwrap();
    let a = doc.id_of(&path!["a"]).unwrap();
    doc.get_mut("b").unwrap().insert_merge(&[a]).unwrap();
    assert_eq!(doc.to_string(), "a: &a {x: 1}\nb:\n  <<: *a\n  y: 2\n");
    assert!(doc.to_value().unwrap()["b"]["x"].is_null());
    assert!(doc.to_value().unwrap()["b"]["<<"].is_mapping());
    agrees_with_its_text(&doc);
}

#[test]
fn insert_merge_errors() {
    let src = "s: &s [1]\nm: &m {x: 1}\nt:\n  <<: *m\nu:\n  k: 1\nplain: 1\n";
    let mut doc = Document::parse_with(src, &merge_settings()).unwrap();
    let s = doc.id_of(&path!["s"]).unwrap();
    let m = doc.id_of(&path!["m"]).unwrap();
    let plain = doc.id_of(&path!["plain"]).unwrap();
    let u = doc.id_of(&path!["u"]).unwrap();
    let mut node = doc.get_mut("u").unwrap();
    assert!(matches!(
        node.insert_merge(&[]),
        Err(EditError::Unsupported { .. })
    ));
    assert!(matches!(
        node.insert_merge(&[s]),
        Err(EditError::Unsupported { .. })
    ));
    // A mapping with a custom tag is no merge source either (Y1016).
    let _ = node;
    let tagged = "c: &c !custom {p: 1}\nd: &d !!map {q: 1}\nu:\n  k: 1\n";
    let mut doc3 = Document::parse_with(tagged, &merge_settings()).unwrap();
    let c = doc3.id_of(&path!["c"]).unwrap();
    let d = doc3.id_of(&path!["d"]).unwrap();
    assert!(matches!(
        doc3.get_mut("u").unwrap().insert_merge(&[c]),
        Err(EditError::Unsupported { .. })
    ));
    assert_eq!(doc3.to_string(), tagged);
    doc3.get_mut("u").unwrap().insert_merge(&[d]).unwrap();
    assert_eq!(doc3.to_value().unwrap()["u"]["q"].as_u64(), Some(1));
    agrees_with_its_text(&doc3);
    let mut node = doc.get_mut("u").unwrap();
    assert!(matches!(
        node.insert_merge(&[plain]),
        Err(EditError::NotAnchored { .. })
    ));
    // Inside its own anchored node.
    let _ = node;
    let mut doc2 = Document::parse_with("m: &m\n  n:\n    k: 1\n", &merge_settings()).unwrap();
    let m2 = doc2.id_of(&path!["m"]).unwrap();
    assert!(matches!(
        doc2.get_path_mut(&path!["m", "n"])
            .unwrap()
            .insert_merge(&[m2]),
        Err(EditError::Unsupported { .. })
    ));
    // A second merge key.
    assert!(matches!(
        doc.get_mut("t").unwrap().insert_merge(&[m]),
        Err(EditError::KeyExists { .. })
    ));
    // Not a mapping.
    assert!(matches!(
        doc.get_mut("s").unwrap().insert_merge(&[m]),
        Err(EditError::NotAMapping { .. })
    ));
    assert_eq!(doc.to_string(), src, "nothing changed");
    // The anchor must precede the alias.
    let mut doc = Document::parse_with("a:\n  k: 1\nb: &b {x: 1}\n", &merge_settings()).unwrap();
    let b = doc.id_of(&path!["b"]).unwrap();
    assert!(matches!(
        doc.get_mut("a").unwrap().insert_merge(&[b]),
        Err(EditError::AliasBeforeAnchor { .. })
    ));
    // With merge keys off, a `<<` key already there.
    let mut doc = Document::parse("m: &m {x: 1}\nt:\n  <<: 1\n").unwrap();
    let m = doc.id_of(&path!["m"]).unwrap();
    assert!(matches!(
        doc.get_mut("t").unwrap().insert_merge(&[m]),
        Err(EditError::KeyExists { .. })
    ));
    let _ = u;
}

#[test]
fn insert_merge_falls_back_to_the_end() {
    // The target's anchor is in the mapping itself: after it.
    let src = "base: &b {x: 1}\ny: 2\n";
    let mut doc = Document::parse_with(src, &merge_settings()).unwrap();
    let b = doc.id_of(&path!["base"]).unwrap();
    doc.root_mut().unwrap().insert_merge(&[b]).unwrap();
    assert_eq!(doc.to_string(), "base: &b {x: 1}\ny: 2\n<<: *b\n");
    assert_eq!(doc.to_value().unwrap()["x"].as_u64(), Some(1));
    agrees_with_its_text(&doc);
    // Leading comments of the first entry stay its own.
    let src = "m: &m {x: 1}\nt:\n  # about k\n  k: 1\n";
    let mut doc = Document::parse_with(src, &merge_settings()).unwrap();
    let m = doc.id_of(&path!["m"]).unwrap();
    doc.get_mut("t").unwrap().insert_merge(&[m]).unwrap();
    let k = doc.get_path(&path!["t", "k"]).unwrap().key().unwrap().id();
    assert_eq!(doc.comments(k).unwrap().leading().len(), 1, "{doc}");
    agrees_with_its_text(&doc);
}

// ========================================================= blank lines

#[test]
fn blank_lines_before_a_node() {
    let mut doc = Document::parse("a: 1\nb:\n  - x\n  -   \n\n  - y\n").unwrap();
    let y = doc.id_of(&path!["b", 2]).unwrap();
    assert_eq!(doc.comments(y).unwrap().blank_lines_before(), 1);
    doc.set_blank_lines_before(y, 0).unwrap();
    assert_eq!(doc.to_string(), "a: 1\nb:\n  - x\n  -   \n  - y\n");
    doc.set_blank_lines_before(y, 2).unwrap();
    assert_eq!(doc.to_string(), "a: 1\nb:\n  - x\n  -   \n\n\n  - y\n");
    // Same count: nothing to do.
    doc.set_blank_lines_before(y, 2).unwrap();
    agrees_with_its_text(&doc);
    // Whitespace-only blank lines go with their white space; CRLF kept.
    let mut doc = Document::parse("a: 1\r\n   \r\nb: 2\r\n").unwrap();
    let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    doc.set_blank_lines_before(b, 0).unwrap();
    assert_eq!(doc.to_string(), "a: 1\r\nb: 2\r\n");
    doc.set_blank_lines_before(b, 1).unwrap();
    assert_eq!(doc.to_string(), "a: 1\r\n\r\nb: 2\r\n");
}

#[test]
fn blank_line_setter_errors() {
    // Not at a line start: a value on its key's line.
    let mut doc = Document::parse("a: 1\n").unwrap();
    let v = doc.id_of(&path!["a"]).unwrap();
    assert!(matches!(
        doc.set_blank_lines_before(v, 1),
        Err(EditError::Unsupported { .. })
    ));
    // n = 0 there is already true.
    doc.set_blank_lines_before(v, 0).unwrap();
    // Removing the blank line under a header re-owns it (rule 2 → 3).
    let src = "# header\n\na: 1\n";
    let mut doc = Document::parse(src).unwrap();
    let root = doc.root().unwrap().id();
    assert!(matches!(
        doc.set_blank_lines_before(root, 0),
        Err(EditError::CommentReattaches {
            rule: CommentRule::FirstNodeLeading,
            ..
        })
    ));
    assert_eq!(doc.to_string(), src);
    // After a kept block scalar, blank lines are its content.
    let src = "a: |+\n  x\nb: 1\n";
    let mut doc = Document::parse(src).unwrap();
    let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    assert!(matches!(
        doc.set_blank_lines_before(b, 1),
        Err(EditError::Unsupported { .. })
    ));
    assert_eq!(doc.to_string(), src);
    // Stale handles.
    let mut doc = Document::parse("a: 1\nb: 2\n").unwrap();
    let b = doc.id_of(&path!["b"]).unwrap();
    let _ = doc.remove_path(&path!["b"]).unwrap();
    assert_eq!(doc.set_blank_lines_before(b, 1), Err(EditError::StaleNode));
}

/// Any `n` is safe; growth is bounded before it is
/// allocated, and refused with `Y2002`.
#[test]
fn blank_line_counts_are_bounded_before_allocating() {
    let too_large = |r: Result<(), EditError>| match r {
        Err(e) => e
            .diagnostic()
            .is_some_and(|d| d.code == oneyaml::code::DOCUMENT_TOO_LARGE),
        Ok(()) => false,
    };
    let src = "a: 1\n# c\nb: 2\n";
    let mut doc = Document::parse(src).unwrap();
    let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    let c = doc.comment_ids()[0];
    assert!(too_large(doc.set_blank_lines_before(b, usize::MAX)));
    assert!(too_large(doc.set_comment_blank_lines(c, usize::MAX)));
    assert!(too_large(doc.set_blank_lines_before(b, 1 << 34)));
    assert_eq!(doc.to_string(), src);
    // CRLF: two bytes per line, so `usize::MAX / 2 + 1` overflows.
    let crlf = "a: 1\r\nb: 2\r\n";
    let unlimited = Settings::new().limits(oneyaml::Limits::unlimited());
    let mut doc = Document::parse_with(crlf, &unlimited).unwrap();
    let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    assert!(too_large(doc.set_blank_lines_before(b, usize::MAX / 2 + 1)));
    assert!(too_large(doc.set_blank_lines_before(b, usize::MAX)));
    assert_eq!(doc.to_string(), crlf);
    // A small `max_document_size`: exactly at the limit is fine.
    let small = Settings::new().limits(oneyaml::Limits::new().max_document_size(12));
    let mut doc = Document::parse_with("a: 1\nb: 2\n", &small).unwrap();
    let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
    assert!(too_large(doc.set_blank_lines_before(b, 3)));
    doc.set_blank_lines_before(b, 2).unwrap();
    assert_eq!(doc.to_string(), "a: 1\n\n\nb: 2\n");
    // Through the stream's forwarders.
    let mut s = Stream::parse("a: 1\n---\n# c\nb: 2\n").unwrap();
    let mut second = s.document_mut(1).unwrap();
    let b = second.get_path(&path!["b"]).unwrap().key().unwrap().id();
    let c = second.comment_ids()[0];
    assert!(too_large(second.set_blank_lines_before(b, usize::MAX)));
    assert!(too_large(second.set_comment_blank_lines(c, usize::MAX)));
}

/// Blank lines right after a block scalar are refused with the
/// named `Unsupported`, whichever layer notices.
#[test]
fn blank_lines_after_block_scalars_are_unsupported() {
    let named = |r: Result<(), EditError>| matches!(r, Err(EditError::Unsupported { reason, .. }) if reason.contains("block scalar"));
    for (src, n) in [("a: |+\n  x\n\nb: 1\n", 2), ("a: |+\n  x\nb: 1\n", 1)] {
        let mut doc = Document::parse(src).unwrap();
        let b = doc.get_path(&path!["b"]).unwrap().key().unwrap().id();
        assert!(named(doc.set_blank_lines_before(b, n)), "{src:?}");
        assert_eq!(doc.to_string(), src);
    }
    let src = "- |+\n  x\n- y\n";
    let mut doc = Document::parse(src).unwrap();
    let y = doc.id_of(&path![1]).unwrap();
    assert!(named(doc.set_blank_lines_before(y, 1)));
    assert_eq!(doc.to_string(), src);
    let src = "a: |+\n  x\n# c\nb: 1\n";
    let mut doc = Document::parse(src).unwrap();
    let c = doc.comment_ids()[0];
    assert!(named(doc.set_comment_blank_lines(c, 1)));
    assert_eq!(doc.to_string(), src);
}

#[test]
fn blank_lines_before_a_comment() {
    let mut doc = Document::parse("a: 1\n# about b\nb: 2 # two\n").unwrap();
    let ids = doc.comment_ids();
    doc.set_comment_blank_lines(ids[0], 2).unwrap();
    assert_eq!(doc.to_string(), "a: 1\n\n\n# about b\nb: 2 # two\n");
    assert_eq!(doc.explain_comment(ids[0]).unwrap().blank_lines_before(), 2);
    // A same-line comment has none, and can get none.
    doc.set_comment_blank_lines(ids[1], 0).unwrap();
    assert!(matches!(
        doc.set_comment_blank_lines(ids[1], 1),
        Err(EditError::Unsupported { .. })
    ));
    // The header's blank line decides its owner (rule 2).
    let src = "# header\n\n# about a\na: 1\n";
    let mut doc = Document::parse(src).unwrap();
    let about = doc.comment_ids()[1];
    assert!(matches!(
        doc.set_comment_blank_lines(about, 0),
        Err(EditError::CommentReattaches { .. })
    ));
    assert_eq!(doc.to_string(), src);
    // A comment of another document.
    let s = Stream::parse("a: 1\n---\n# c\nb: 2\n").unwrap();
    let other = s.document(1).unwrap().comment_ids()[0];
    let mut first = s.document(0).unwrap();
    assert_eq!(
        first.set_comment_blank_lines(other, 1),
        Err(EditError::StaleNode)
    );
}

#[test]
fn stream_forwarders_set_blank_lines() {
    let mut s = Stream::parse("a: 1\n---\nb: 2\n# about c\nc: 3\n").unwrap();
    let mut second = s.document_mut(1).unwrap();
    let c = second.get_path(&path!["c"]).unwrap().key().unwrap().id();
    let about = second.comment_ids()[0];
    second.set_comment_blank_lines(about, 1).unwrap();
    second.set_blank_lines_before(c, 1).unwrap();
    assert_eq!(s.to_string(), "a: 1\n---\nb: 2\n\n# about c\n\nc: 3\n");
    assert_eq!(
        s.document(1).unwrap().to_string(),
        "---\nb: 2\n\n# about c\n\nc: 3\n"
    );
}

// ============================================================== nulls

#[test]
fn created_nulls_follow_the_neighbour_only_in_block_collections() {
    // Flow: `null`, as before.
    let mut doc = Document::parse("on: {push: , pull: }\n").unwrap();
    doc.get_path_or_create(&path!["on", "x"]).unwrap();
    assert!(doc.to_string().contains("x: null"), "{doc}");
    agrees_with_its_text(&doc);
    // A tagged or anchored null neighbour is not copied.
    let mut doc = Document::parse("a: !!null\nb: &n ~\n").unwrap();
    doc.get_path_or_create(&path!["c"]).unwrap();
    assert_eq!(doc.to_string(), "a: !!null\nb: &n ~\nc: null\n");
    // `NULL` is copied as written.
    let mut doc = Document::parse("a: NULL\n").unwrap();
    doc.get_path_or_create(&path!["b"]).unwrap();
    assert_eq!(doc.to_string(), "a: NULL\nb: NULL\n");
    agrees_with_its_text(&doc);
    // An existing node is returned, not rewritten.
    let mut doc = Document::parse("a:\nb: 1\n").unwrap();
    doc.get_path_or_create(&path!["a"]).unwrap();
    assert_eq!(doc.to_string(), "a:\nb: 1\n");
    // `set_path` with a null keeps writing `null`.
    doc.set_path(&path!["c"], ()).unwrap();
    assert_eq!(doc.to_string(), "a:\nb: 1\nc: null\n");
}
