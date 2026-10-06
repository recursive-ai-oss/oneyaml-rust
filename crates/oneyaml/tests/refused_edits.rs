//! Refused edits and syntax errors (refused edits
//! change nothing, never a panic; an edit never erases a syntax error,
//! so both readers agree; a refused re-insert hands the `Removed` back).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{Document, EditError, Removed, Stream, Value, path};

const BREAKS: [&str; 3] = ["\n", "\r\n", "\r"];

/// `src` with every `\n` written as `nl`.
fn with(src: &str, nl: &str) -> String {
    src.replace('\n', nl)
}

/// Every diagnostic, error flag, and meaning of `s` equal a re-parse of
/// its text in the same mode.
fn stream_matches_reparse(s: &Stream, recovering: bool) {
    let text = s.to_string();
    if recovering {
        let again = Stream::parse_recovering_with(text.clone(), s.settings()).into_value();
        assert_eq!(s.diagnostics(), again.diagnostics(), "{text:?}");
        assert_eq!(s.has_errors(), again.has_errors(), "{text:?}");
        assert_eq!(s.len(), again.len(), "{text:?}");
    } else {
        let again = Stream::parse_with(text.clone(), s.settings()).unwrap();
        assert_eq!(s.diagnostics(), again.diagnostics(), "{text:?}");
        for i in 0..s.len() {
            let (a, b) = (s.document(i).unwrap(), again.document(i).unwrap());
            assert_eq!(a.to_value().ok(), b.to_value().ok(), "{text:?}");
        }
    }
}

/// The outcome of a move is all-or-nothing: refused, the text is
/// byte-identical; never `Internal`; accepted, the moved entry is there
/// once and the text re-parses to the same meaning.
fn move_is_atomic(src: &str, from: usize, to: usize) {
    for nl in BREAKS {
        let src = with(src, nl);
        let mut d = Document::parse(src.clone()).unwrap();
        let before = d.to_value().unwrap();
        let r = d.root_mut().unwrap().move_entry(from, to);
        let text = d.to_string();
        match r {
            Ok(()) => {
                let again = Document::parse(text.clone()).unwrap();
                assert_eq!(again.to_value().unwrap(), d.to_value().unwrap(), "{text:?}");
                assert_eq!(
                    before.as_mapping().map(|m| m.len()),
                    again.to_value().unwrap().as_mapping().map(|m| m.len()),
                    "{text:?}"
                );
                assert_eq!(text.matches("# c").count(), 1, "{text:?}");
            }
            Err(EditError::Internal) => panic!("Internal on {src:?}: {text:?}"),
            Err(e) => assert_eq!(text, src, "refused ({e:?}) but changed"),
        }
    }
}

#[test]
fn move_of_a_block_scalar_entry_before_a_comment_line() {
    move_is_atomic("k: |\n  text\n# c\nm: 2\n# tail\n", 0, 1);
}

#[test]
fn move_to_the_end_keeps_the_final_break() {
    move_is_atomic("k: |\n  text\n# c\nm: 2\n", 0, 1);
}

#[test]
fn move_past_two_entries() {
    move_is_atomic("k: |\n  text\n# c\nm: 2\nn: 3\n", 0, 2);
}

#[test]
fn block_scalar_moves_in_sequences_too() {
    for (src, from, to) in [
        ("- |\n  text\n# c\n- 2\n# tail\n", 0, 1),
        ("- |\n  text\n# c\n- 2\n", 0, 1),
        ("- |\n  text\n# c\n- 2\n- 3\n", 0, 2),
    ] {
        for nl in BREAKS {
            let src = with(src, nl);
            let mut d = Document::parse(src.clone()).unwrap();
            let r = d.root_mut().unwrap().move_entry(from, to);
            let text = d.to_string();
            match r {
                Ok(()) => {
                    let again = Document::parse(text.clone()).unwrap();
                    assert_eq!(again.to_value().unwrap(), d.to_value().unwrap());
                    assert_eq!(text.matches("# c").count(), 1, "{text:?}");
                }
                Err(EditError::Internal) => panic!("Internal on {src:?}: {text:?}"),
                Err(e) => assert_eq!(text, src, "refused ({e:?}) but changed"),
            }
        }
    }
}

/// Remove, then re-insert at the end, in a stream document.
#[test]
fn reinsert_in_a_stream_is_all_or_nothing() {
    for (src, key) in [
        ("k: |\n  text\n# c\nm: 2\n---\ny\n", Some("k")),
        ("- |\n  text\n# c\n- 2\n---\ny\n", None),
    ] {
        for nl in BREAKS {
            let src = with(src, nl);
            let mut s = Stream::parse(src.clone()).unwrap();
            let mut d = s.document_mut(0).unwrap();
            let rm = match key {
                Some(k) => d.remove_path(&path![k]).unwrap(),
                None => d.remove_path(&path![0]).unwrap(),
            };
            let after_remove = s.to_string();
            let mut d = s.document_mut(0).unwrap();
            let r = d.root_mut().unwrap().insert_removed(rm).map(|n| n.id());
            let text = s.to_string();
            match r {
                Ok(_) => {
                    assert_eq!(text.matches("# c").count(), 1, "{text:?}");
                    stream_matches_reparse(&s, false);
                }
                Err(e) => {
                    assert_ne!(e.error(), &EditError::Internal, "{src:?}: {text:?}");
                    assert_eq!(text, after_remove, "refused ({e:?}) but changed");
                    assert!(e.removed().comments().iter().any(|c| c == "# c"));
                }
            }
        }
    }
}

/// A document whose subtree holds an error node is
/// restricted whoever owns the diagnostic (here the next document, at
/// whose `---` the unclosed `[` is reported); the error region is never
/// replaced, and the stream's diagnostics keep agreeing with a re-parse.
#[test]
fn an_error_node_owned_by_the_next_document_restricts_its_document() {
    for nl in BREAKS {
        let src = with("a: [1, 2\n---\nt: !!int x\n---\nz: 1\n", nl);
        let mut s = Stream::parse_recovering(src.clone()).into_value();
        let d0 = s.document(0).unwrap();
        assert_eq!(d0.errors().count(), 1);
        drop(d0);
        let before = s.diagnostics();
        {
            let mut d = s.document_mut(0).unwrap();
            let r = d.set_path(&path!["a"], "l1\nl2");
            assert!(matches!(r, Err(EditError::ReadOnly { .. })), "{r:?}");
            let r = d.set_root("l1\nl2");
            assert!(matches!(r, Err(EditError::ReadOnly { .. })), "{r:?}");
            let r = d.set_path(&path!["b"], 1);
            assert!(matches!(r, Err(EditError::ReadOnly { .. })), "{r:?}");
        }
        assert_eq!(s.to_string(), src);
        assert_eq!(s.diagnostics(), before);
        stream_matches_reparse(&s, true);
        // A document without errors is editable.
        {
            let mut d = s.document_mut(2).unwrap();
            d.set_path(&path!["u"], 1).unwrap();
        }
        stream_matches_reparse(&s, true);
        assert_eq!(s.document(0).unwrap().errors().count(), 1);
    }
}

/// The error-node shape in a single document: the same shape through
/// `Document`.
#[test]
fn documents_with_an_error_node_are_restricted() {
    let mut d = Document::parse_recovering("a: [1, 2\n").into_value();
    assert!(d.has_errors());
    let r = d.set_path(&path!["a"], "x");
    assert!(matches!(r, Err(EditError::ReadOnly { .. })), "{r:?}");
    assert_eq!(d.to_string(), "a: [1, 2\n");
}

/// Item 4: a refused re-insertion hands the `Removed` back, with
/// its comments, and changes nothing; it then goes in elsewhere.
#[test]
fn a_refused_reinsert_hands_the_removed_back() {
    // AliasBeforeAnchor: an alias must follow its anchor.
    let mut d = Document::parse("first: []\na: &x 1\nlist: [*x] # c\n").unwrap();
    let r = d.remove_path(&path!["list", 0]).unwrap();
    let text = d.to_string();
    let err = d
        .get_mut("first")
        .unwrap()
        .insert_removed(r.clone())
        .unwrap_err();
    assert!(
        matches!(err.error(), EditError::AliasBeforeAnchor { .. }),
        "{err}"
    );
    assert_eq!(d.to_string(), text);
    assert_eq!(err.removed(), &r);
    let back: Removed = err.into_removed();
    d.get_mut("list").unwrap().insert_removed(back).unwrap();
    assert_eq!(d.to_string(), "first: []\na: &x 1\nlist: [*x] # c\n");

    // NotWellFormed: block text into a flow mapping that cannot hold it.
    let mut d = Document::parse("k: x, y\nf: {a: 1}\n").unwrap();
    let r = d.remove_path(&path!["k"]).unwrap();
    let text = d.to_string();
    let err = d.get_mut("f").unwrap().insert_removed_at(0, r).unwrap_err();
    let (e, back) = err.into_parts();
    assert!(matches!(e, EditError::NotWellFormed { .. }), "{e:?}");
    assert_eq!(d.to_string(), text);
    assert_eq!(back.text(), "k: x, y");
    d.root_mut().unwrap().insert_removed(back).unwrap();
    assert_eq!(d.to_string(), "f: {a: 1}\nk: x, y\n");
}

/// Item 5: after an edit, reading one document of a stream
/// (`to_value`, `Index`, `deserialize`) costs O(that document), never the
/// emit of the whole edited stream: reading the first document of an
/// edited stream of 32k documents takes about as long as of 8k (the old
/// cost grew 4×; the test fails above 2.5×, best of three).
#[test]
fn reading_an_edited_stream_document_is_local() {
    use std::time::{Duration, Instant};
    fn edited(n: usize) -> Stream {
        // Documents of ~250 bytes, so that emitting the stream dominates.
        let pad = "x".repeat(200);
        let src: String = (0..n)
            .map(|i| format!("---\nk: {i}\nl: [1, 2]\n# {pad}\n"))
            .collect();
        let mut s = Stream::parse(src).unwrap();
        s.document_mut(0)
            .unwrap()
            .set_path(&path!["k"], "edited")
            .unwrap();
        s
    }
    fn read(s: &mut Stream) -> Duration {
        (0..3)
            .map(|_| {
                let t = Instant::now();
                for _ in 0..40 {
                    let d = s.document(0).unwrap();
                    assert_eq!(d.to_value().unwrap()["k"].as_str(), Some("edited"));
                    assert_eq!(d.to_value().unwrap()["l"][1].as_u64(), Some(2));
                    let v: std::collections::BTreeMap<String, Value> = d.deserialize().unwrap();
                    assert_eq!(v.len(), 2);
                    let g = s.document_mut(0).unwrap();
                    assert_eq!(g.to_value().unwrap()["k"].as_str(), Some("edited"));
                }
                t.elapsed()
            })
            .min()
            .unwrap()
    }
    let (mut small, mut large) = (edited(8_000), edited(32_000));
    let (a, b) = (read(&mut small), read(&mut large));
    let ratio = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("8k {a:?}, 32k {b:?}, ratio {ratio:.1}");
    assert!(ratio < 2.5, "8k {a:?}, 32k {b:?}");
}

/// Found by the stream fuzz: a moved `|+` entry whose inserted text
/// ends with the indentation of the next entry read one line break too
/// many (the value was computed from the fragment's own text, which ended
/// there, as if the scalar ended the document).
#[test]
fn a_moved_keep_scalar_reads_as_its_text() {
    for nl in BREAKS {
        let src = with("- a\n- b: |+\n    keep\n\n# after\n- p: 1\n", nl);
        let mut d = Document::parse(src).unwrap();
        let rm = d.remove_path(&path![1, "b"]).unwrap();
        d.get_path_mut(&path![2])
            .unwrap()
            .insert_removed_at(0, rm)
            .unwrap();
        let text = d.to_string();
        assert_eq!(
            text,
            with("- a\n- {}\n# after\n- b: |+\n    keep\n\n  p: 1\n", nl)
        );
        let again = Document::parse(text.clone()).unwrap().to_value().unwrap();
        assert_eq!(d.to_value().unwrap(), again, "{text:?}");
        assert_eq!(again[2]["b"].as_str(), Some("keep\n\n"));
    }
    // At the very end of the text, spaces after a keep scalar are one
    // more empty line, in an edited tree as in a parsed one.
    let mut d = Document::parse("a: 1\nb: |+\n  x\n ").unwrap();
    d.set_path(&path!["a"], 2).unwrap();
    let again = Document::parse(d.to_string()).unwrap().to_value().unwrap();
    assert_eq!(d.to_value().unwrap(), again);
}
