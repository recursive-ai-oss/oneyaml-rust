//! `Document` edit regression fixes. Each test
//! names its case; the scaling shape of one is in
//! `document_scaling.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod document_common;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, NodeId, Settings, Stream, path};

/// Every document of `s`, and the stream, report exactly the diagnostics
/// a recovering parse of the edited text reports.
fn same_as_reparse(s: &Stream) {
    let text = s.to_string();
    let again = Stream::parse_recovering_with(text.clone(), s.settings()).into_value();
    assert_eq!(s.diagnostics(), again.diagnostics(), "{text:?}");
    assert_eq!(s.has_errors(), again.has_errors(), "{text:?}");
    assert_eq!(s.len(), again.len());
    for i in 0..s.len() {
        let (a, b) = (s.document(i).unwrap(), again.document(i).unwrap());
        assert_eq!(a.diagnostics(), b.diagnostics(), "document {i} of {text:?}");
        assert_eq!(a.has_errors(), b.has_errors(), "document {i} of {text:?}");
    }
}

// ----------------------- a stream clone keeps the guard of the edited document

/// The inner scalar of the second key (`b` of `? [b]`) of document `i`.
fn second_key_item(s: &Stream, i: usize) -> NodeId {
    let d = s.document(i).unwrap();
    let root = d.root().unwrap();
    let (k, _) = root.as_mapping().unwrap().iter().nth(1).unwrap();
    k.as_sequence().unwrap().get(0).unwrap().id()
}

/// A meaning error created by an accepted edit does not
/// make its region read-only — in a clone of the stream too. Before the fix
/// the clone's editor kept the original's identity, so its next
/// `document_mut` took the switch path and rebuilt the guard from the
/// edited diagnostics: `Err(ReadOnly)` on the clone, `Ok` on the original.
#[test]
fn a_stream_clone_keeps_the_guard_of_the_edited_document() {
    let mut s = Stream::parse("x: 0\n---\n? [a]\n: 1\n? [b]\n: 2\n").unwrap();
    let b = second_key_item(&s, 1);
    s.document_mut(1)
        .unwrap()
        .node_mut(b)
        .unwrap()
        .set_scalar("a")
        .unwrap();
    assert!(s.document(1).unwrap().has_errors(), "Y1013 accepted");
    same_as_reparse(&s);

    // The replaced scalar has a new handle.
    let b = second_key_item(&s, 1);
    let mut c = s.clone();
    c.document_mut(1)
        .unwrap()
        .node_mut(b)
        .unwrap()
        .set_scalar("c")
        .unwrap();
    assert_eq!(c.to_string(), "x: 0\n---\n? [a]\n: 1\n? [c]\n: 2\n");
    assert!(!c.has_errors());
    same_as_reparse(&c);

    // The original is unchanged by the clone's edit, and repairs the same
    // way.
    assert_eq!(s.to_string(), "x: 0\n---\n? [a]\n: 1\n? [a]\n: 2\n");
    assert!(s.has_errors());
    s.document_mut(1)
        .unwrap()
        .node_mut(b)
        .unwrap()
        .set_scalar("d")
        .unwrap();
    assert_eq!(s.to_string(), "x: 0\n---\n? [a]\n: 1\n? [d]\n: 2\n");
    same_as_reparse(&s);
}

/// The clone's editor has the clone's identity: a [`oneyaml::Removed`]
/// taken from the clone goes back into the clone, and not into the
/// original (and the reverse).
#[test]
fn removed_belongs_to_the_clone_it_was_taken_from() {
    let mut s = Stream::parse("a: 1\n---\nm: 1\nk: 2\nz: 3\n").unwrap();
    // The editor points at document 1 before the clone.
    s.document_mut(1).unwrap().set_path(&path!["k"], 3).unwrap();
    let mut c = s.clone();
    let r = c.document_mut(1).unwrap().remove_path(&path!["k"]).unwrap();
    let e = s
        .document_mut(1)
        .unwrap()
        .root_mut()
        .unwrap()
        .insert_removed(r)
        .unwrap_err()
        .into_error();
    assert!(matches!(e, EditError::WrongDocument), "{e:?}");
    let r = c.document_mut(1).unwrap().remove_path(&path!["m"]).unwrap();
    let mut d = c.document_mut(1).unwrap();
    assert!(d.root_mut().unwrap().insert_removed(r).is_ok());
    assert_eq!(c.to_string(), "a: 1\n---\nz: 3\nm: 1\n");
    // The original's own removal still goes back into the original.
    let r = s.document_mut(1).unwrap().remove_path(&path!["k"]).unwrap();
    let mut d = s.document_mut(1).unwrap();
    assert!(d.root_mut().unwrap().insert_removed(r).is_ok());
    assert_eq!(s.to_string(), "a: 1\n---\nm: 1\nz: 3\nk: 3\n");
    same_as_reparse(&s);
    same_as_reparse(&c);
}

// --------------------- the last documents trailing warning moves with the text

/// The diagnostics attached after the last element (a `Y0028` warning
/// whose scalar ends the text) belong to the last document; after an edit
/// of another document they move with the end of the text, as a re-parse
/// reports them (read from the parse, never by rebuilding the stream's
/// diagnostics; its scaling is in `document_scaling.rs`).
#[test]
fn the_last_documents_trailing_warning_moves_with_the_text() {
    let lenient = Settings::new().lenient_inline_colons(true);
    let mut s = Stream::parse_with("a: 1\n---\nb: 1\nk: x: z", &lenient).unwrap();
    let before = s.document(1).unwrap().diagnostics();
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0].code, oneyaml::code::LENIENT_INLINE_COLON);
    // Attached after the last element (the shape this finding is about).
    let (at, _) = s.syntax().diagnostics_by_element().last().unwrap();
    assert!(at.is_none());
    s.document_mut(0)
        .unwrap()
        .set_path(&path!["a"], 12_345)
        .unwrap();
    assert_eq!(s.to_string(), "a: 12345\n---\nb: 1\nk: x: z");
    let after = s.document(1).unwrap().diagnostics();
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(
        after[0].primary.span.start,
        before[0].primary.span.start + 4
    );
    assert!(s.document(0).unwrap().diagnostics().is_empty());
    same_as_reparse(&s);
    // Shrinking the text moves it back.
    s.document_mut(0).unwrap().set_path(&path!["a"], 1).unwrap();
    assert_eq!(s.document(1).unwrap().diagnostics(), before);
    same_as_reparse(&s);
    // An edit of the last document itself moves it too.
    s.document_mut(1)
        .unwrap()
        .set_path(&path!["b"], 100)
        .unwrap();
    assert_eq!(s.to_string(), "a: 1\n---\nb: 100\nk: x: z");
    let last = s.document(1).unwrap().diagnostics();
    assert_eq!(last.len(), 1, "{last:?}");
    assert_eq!(last[0].primary.span.start, before[0].primary.span.start + 2);
    same_as_reparse(&s);
}

// ----------------------------- a bare multi line collection key gets its value

/// Sets the value of the first entry of the mapping at `at` (a bare
/// `? key` written without `:`) to `v`.
fn set_bare_value(doc: &mut Document, at: &oneyaml::Path, v: i64) -> Result<(), EditError> {
    let value = {
        let m = doc.get_path(at).unwrap();
        let (_, v) = m.as_mapping().unwrap().iter().next().unwrap();
        v.id()
    };
    doc.node_mut(value)?.set(v).map(|_| ())
}

/// `explicit_synth` for a value written without `:` after a multi-line
/// collection key: accepted and correct (the `:` goes at the key's
/// indentation, on the line after the key, before a comment line and after
/// a comment ending the key), never `Internal` (the criterion allowed a
/// typed refusal; every shape here is accepted, so the test asserts it).
///
/// Found while building it: when the entry is the last of its
/// mapping, the key line's comment lies outside the mapping and the `:`
/// line went in before it (`? - x⏎  - y⏎: 1 # c`, also `? a # c` alone):
/// the comment now stays on the key line, as for any other entry.
#[test]
fn a_bare_multi_line_collection_key_gets_its_value() {
    let cases: &[(&str, &str)] = &[
        ("? - x\n  - y\n", "? - x\n  - y\n: 1\n"),
        ("? - x\n  - y\nb: 2\n", "? - x\n  - y\n: 1\nb: 2\n"),
        ("? a: 1\n  b: 2\n", "? a: 1\n  b: 2\n: 1\n"),
        (
            "m:\n  ? - x\n    - y\n  b: 2\n",
            "m:\n  ? - x\n    - y\n  : 1\n  b: 2\n",
        ),
        (
            "? - x\n  - [y,\n     z]\n# after\n",
            "? - x\n  - [y,\n     z]\n: 1\n# after\n",
        ),
        ("? - x\n  - y # c\n", "? - x\n  - y # c\n: 1\n"),
        ("? a # c\n", "? a # c\n: 1\n"),
        ("? a # c", "? a # c\n: 1"),
        (
            "m:\n  ? - x\n    - y # c\nn: 1\n",
            "m:\n  ? - x\n    - y # c\n  : 1\nn: 1\n",
        ),
        (
            "? - x\n  - |\n    text\nb: 2\n",
            "? - x\n  - |\n    text\n: 1\nb: 2\n",
        ),
    ];
    for (src, want) in cases {
        let mut doc = Document::parse(*src).unwrap();
        let at = if src.starts_with("m:") {
            path!["m"]
        } else {
            oneyaml::Path::new()
        };
        set_bare_value(&mut doc, &at, 1).unwrap_or_else(|e| panic!("{src:?}: {e:?}"));
        assert_eq!(doc.to_string(), *want, "{src:?}");
        agrees_with_its_text(&doc);
        // The same over CRLF (the document's own line break;
        // a text without one writes LF).
        if !src.contains('\n') {
            continue;
        }
        let crlf = src.replace('\n', "\r\n");
        let mut doc = Document::parse(crlf.as_str()).unwrap();
        set_bare_value(&mut doc, &at, 1).unwrap_or_else(|e| panic!("{crlf:?}: {e:?}"));
        assert_eq!(doc.to_string(), want.replace('\n', "\r\n"));
        agrees_with_its_text(&doc);
    }
}

/// The key line's comment stays there when the new value is a block (a
/// `|` scalar goes through the line's end once the comment has left, a
/// block mapping goes on the lines after the `:`).
#[test]
fn the_key_comment_stays_for_block_values() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "? a # c\n",
            "? a # c\n: |-\n  l1\n  l2\n",
            "? a # c\n:\n  k: 1\n",
        ),
        (
            "? - x\n  - y # c\n# d\n",
            "? - x\n  - y # c\n: |-\n  l1\n  l2\n# d\n",
            "? - x\n  - y # c\n:\n  k: 1\n# d\n",
        ),
    ];
    for (src, string, map) in cases {
        let mut m = std::collections::BTreeMap::new();
        m.insert("k", 1);
        for eol in ["\n", "\r\n"] {
            let src = src.replace('\n', eol);
            let mut doc = Document::parse(src.as_str()).unwrap();
            set_bare(&mut doc, &"l1\nl2");
            assert_eq!(doc.to_string(), string.replace('\n', eol));
            agrees_with_its_text(&doc);
            let mut doc = Document::parse(src.as_str()).unwrap();
            set_bare(&mut doc, &m);
            assert_eq!(doc.to_string(), map.replace('\n', eol));
            agrees_with_its_text(&doc);
        }
    }
}

/// Sets the value of the root mapping's first entry.
fn set_bare<T: ?Sized + serde::Serialize>(doc: &mut Document, v: &T) {
    let value = {
        let m = doc.root().unwrap();
        let (_, v) = m.as_mapping().unwrap().iter().next().unwrap();
        v.id()
    };
    doc.node_mut(value).unwrap().set(v).unwrap();
}
