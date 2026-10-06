//! `Document::has_errors()` describes
//! the document's own contents: true iff its subtree holds an error node
//! or it owns an error diagnostic, whichever document the diagnostic
//! belongs to under the stream ownership rule (which is unchanged).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Document, NodeKind, Severity, Stream, path};

const TRIGGER: &str = "a: [1, 2\n---\nt: !!int x\n";

fn owns_error(d: &Document) -> bool {
    d.diagnostics()
        .iter()
        .any(|x| x.severity == Severity::Error)
}

/// The rule, checked on every document of `s`.
fn check_rule(s: &Stream, src: &str) {
    for (i, d) in s.documents().enumerate() {
        let expected = d.errors().next().is_some() || owns_error(&d);
        assert_eq!(d.has_errors(), expected, "{src:?} document {i}");
    }
}

#[test]
fn trigger_first_document_holds_the_error_node() {
    let s = Stream::parse_recovering(TRIGGER).into_value();
    let d0 = s.document(0).unwrap();
    let errors: Vec<_> = d0.errors().collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].kind(), NodeKind::Error);
    assert_eq!((errors[0].span().start, errors[0].span().end), (9, 9));
    assert!(d0.has_errors());
    // Ownership is unchanged: the diagnostic at the `---` is the second
    // document's, so the first document's diagnostics hold no error.
    assert!(!owns_error(&d0));
    let d1 = s.document(1).unwrap();
    assert!(d1.has_errors());
    assert!(owns_error(&d1));
    assert!(d1.errors().next().is_none());
    check_rule(&s, TRIGGER);
}

#[test]
fn trigger_first_document_is_restricted_and_consistent() {
    let mut s = Stream::parse_recovering(TRIGGER).into_value();
    // Restricted mode agrees with `has_errors`: the edit next to the error
    // region is refused.
    let mut d0 = s.document_mut(0).unwrap();
    assert!(d0.has_errors());
    assert!(d0.set_path(&path!["a", 0], 5).is_err());
    assert_eq!(s.to_string(), TRIGGER);
}

#[test]
fn has_errors_survives_edits_and_clones() {
    let src = "a: [1, 2\n---\nb: 1\n---\nc: 2\n";
    let mut s = Stream::parse_recovering(src).into_value();
    assert!(s.document(0).unwrap().has_errors());
    assert!(s.document(1).unwrap().has_errors());
    assert!(!s.document(2).unwrap().has_errors());
    {
        let mut d2 = s.document_mut(2).unwrap();
        d2.set_path(&path!["c"], 3).unwrap();
    }
    assert_eq!(s.to_string(), "a: [1, 2\n---\nb: 1\n---\nc: 3\n");
    let copy = s.clone();
    for st in [&s, &copy] {
        assert!(st.document(0).unwrap().has_errors());
        assert!(st.document(1).unwrap().has_errors());
        assert!(!st.document(2).unwrap().has_errors());
        check_rule(st, src);
    }
}

#[test]
fn clean_documents_and_owned_errors_are_unaffected() {
    for src in [
        "a: 1\n---\nb: 2\n",
        "a: 1\n---\nb: [1\n",
        "a: 'x\n---\nb: 2\n",
        "--- [a]]\n--- b\n",
        "a: {x: 1\n---\n- [\n---\nc\n",
        "t: !!int x\n---\na: 1\n",
    ] {
        let s = Stream::parse_recovering(src).into_value();
        check_rule(&s, src);
    }
    let s = Stream::parse("a: 1\n---\nb: 2\n").unwrap();
    assert!(s.documents().all(|d| !d.has_errors()));
}

#[test]
fn whole_document_parse_is_unchanged() {
    let doc = Document::parse_recovering("a: [1, 2\n").into_value();
    assert!(doc.has_errors());
    assert!(doc.errors().next().is_some());
    let doc = Document::parse("a: [1, 2]\n").unwrap();
    assert!(!doc.has_errors());
    assert!(doc.errors().next().is_none());
}
