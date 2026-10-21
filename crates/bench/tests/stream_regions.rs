//! A byte-identical round trip for a single document of a stream: over the
//! yaml-test-suite, every document of a stream writes its own region of
//! the stream's text — the regions are consecutive and cover the text, and
//! each document's text, parsed again on its own, has the same bytes and
//! the same comments with the same owners, rules, and blank lines as the
//! document reports inside the stream. Checked untouched and after an
//! edit through `Stream::document_mut`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use oneyaml::{CommentOwner, Document, Stream, path};

/// A comment as a document reports it, in the document's own coordinates.
fn comments(doc: &Document, base: usize) -> Vec<(String, u8, &'static str, usize, Option<usize>)> {
    doc.explain_comments()
        .unwrap()
        .into_iter()
        .map(|e| {
            let kind = match e.owner() {
                CommentOwner::DocumentHeader => "header",
                CommentOwner::DocumentFooter => "footer",
                CommentOwner::Leading(_) => "leading",
                CommentOwner::Trailing(_) => "trailing",
                CommentOwner::Footer(_) => "coll-footer",
                _ => "other",
            };
            assert_eq!(e.span().start - base, e.span().start.saturating_sub(base));
            (
                e.text().to_owned(),
                e.rule().number(),
                kind,
                e.blank_lines_before(),
                e.owner_span().map(|s| s.start - base),
            )
        })
        .collect()
}

/// The region invariants of every document of `stream`; the number of
/// documents checked.
fn check(name: &str, stream: &Stream) -> usize {
    let text = stream.to_string();
    let mut joined = String::new();
    let mut at = 0;
    for (i, d) in stream.documents().enumerate() {
        let own = d.to_string();
        let span = d.span();
        assert_eq!(
            span.start, at,
            "{name}: document {i} starts where the last ended"
        );
        assert_eq!(
            &text[span.start..span.end],
            own,
            "{name}: document {i} region"
        );
        assert_eq!(d.to_bytes(), own.as_bytes(), "{name}: document {i} bytes");
        at = span.end;
        joined.push_str(&own);
        let again = Document::parse(own.clone())
            .unwrap_or_else(|e| panic!("{name}: document {i} alone: {e} in {own:?}"));
        assert_eq!(again.to_string(), own, "{name}: document {i} re-parsed");
        assert_eq!(
            comments(&again, 0),
            comments(&d, span.start),
            "{name}: document {i} comments re-parsed from {own:?}"
        );
    }
    if !stream.is_empty() {
        assert_eq!(joined, text, "{name}: the regions cover the stream");
    }
    stream.len()
}

#[test]
fn stream_documents_are_their_regions_over_the_suite() {
    let cases = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    let (mut streams, mut docs, mut edited) = (0, 0, 0);
    for case in &cases {
        let Ok(src) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        let Ok(mut stream) = Stream::parse(src.clone()) else {
            continue;
        };
        streams += 1;
        docs += check(&case.id, &stream);
        // After an edit of each document with a mapping root.
        let mut changed = false;
        for i in 0..stream.len() {
            let mut d = stream.document_mut(i).unwrap();
            if d.root().is_some_and(|r| r.as_mapping().is_some())
                && d.set_path(&path!["stream_regions_new_key"], 1).is_ok()
            {
                changed = true;
            }
        }
        if changed {
            edited += 1;
            check(&format!("{} (edited)", case.id), &stream);
            let again = Stream::parse(stream.to_string()).unwrap();
            assert_eq!(again.len(), stream.len(), "{}", case.id);
        }
    }
    assert!(
        streams > 250 && docs > streams && edited > 100,
        "{streams} {docs} {edited}"
    );
}

#[test]
fn the_unit_33_example_and_its_neighbours() {
    let src = "# head\n\na: 1 # t\n---\nb: 2 # u\n# foot\n";
    let s = Stream::parse(src).unwrap();
    assert_eq!(s.document(0).unwrap().to_string(), "# head\n\na: 1 # t\n");
    assert_eq!(
        s.document(1).unwrap().to_string(),
        "---\nb: 2 # u\n# foot\n"
    );
    check("example", &s);
    // Blank lines between two documents go with the next one, with its
    // header comment; a footer at a deeper column stays with the first
    // (rule 6) when no `---` follows.
    let src = "a:\n  b: 1\n  # footer of a\n...\n\n# head\nc: 2\n";
    let s = Stream::parse(src).unwrap();
    assert_eq!(
        s.document(0).unwrap().to_string(),
        "a:\n  b: 1\n  # footer of a\n...\n"
    );
    assert_eq!(s.document(1).unwrap().to_string(), "\n# head\nc: 2\n");
    check("footer", &s);
    // A stray `...` goes with the next document.
    let src = "a\n...\n...\n# c\n---\nb\n";
    let s = Stream::parse(src).unwrap();
    assert_eq!(s.document(0).unwrap().to_string(), "a\n...\n");
    assert_eq!(s.document(1).unwrap().to_string(), "...\n# c\n---\nb\n");
    check("stray", &s);
    // A block scalar ends its own line.
    let s = Stream::parse("a: |\n  x\n# c\n---\nb\n").unwrap();
    assert_eq!(s.document(0).unwrap().to_string(), "a: |\n  x\n");
    check("block", &s);
    // CRLF, and no final line break.
    let s = Stream::parse("a: 1 # t\r\n---\r\nb: 2 # u").unwrap();
    assert_eq!(s.document(0).unwrap().to_string(), "a: 1 # t\r\n");
    assert_eq!(s.document(1).unwrap().to_string(), "---\r\nb: 2 # u");
    check("crlf", &s);
}

#[test]
fn edited_documents_keep_their_regions() {
    let src = "# head\n\na: 1 # t\n# end of a\n...\n---\nb: 2 # u\n# foot\n";
    let mut s = Stream::parse(src).unwrap();
    let mut first = s.document_mut(0).unwrap();
    first.set_path(&path!["c"], 3).unwrap();
    first.set_footer(&["new end"]).unwrap();
    assert_eq!(
        first.to_string(),
        "# head\n\na: 1 # t\nc: 3\n# new end\n...\n"
    );
    let mut second = s.document_mut(1).unwrap();
    second.set_header(&["about b"]).unwrap();
    assert_eq!(second.to_string(), "# about b\n---\nb: 2 # u\n# foot\n");
    check("edited", &s);
    assert_eq!(
        s.to_string(),
        "# head\n\na: 1 # t\nc: 3\n# new end\n...\n# about b\n---\nb: 2 # u\n# foot\n"
    );
}

/// A diagnostic in its document's own coordinates.
fn diags(list: &[oneyaml::Diagnostic], base: usize) -> Vec<(String, usize, usize)> {
    list.iter()
        .map(|d| {
            (
                d.code.to_string(),
                d.primary.span.start.saturating_sub(base),
                d.primary.span.end.saturating_sub(base),
            )
        })
        .collect()
}

/// The mismatches of one recovered stream: (document, stream-only,
/// alone-only diagnostics), after the boundary exceptions.
///
/// **Boundary exceptions** (the one class a document's text alone cannot
/// reproduce): an error *about the boundary* between two documents is
/// reported at the start of the later one (an unclosed `[` cut off by the
/// next `---`; a directive after a document that did not
/// end with `...`). Parsed alone, the earlier document reports the
/// unclosed collection at its own end instead, and the later one has no
/// boundary error. So a stream-only diagnostic at relative offset 0, and
/// an alone-only one at the document's end, are accepted when every one
/// of them has a boundary code (`Y0022` unterminated flow collection,
/// `Y0003` directive in a document not ended with `...`, `Y0009` a quoted
/// scalar cut off by a document marker); anything else
/// is a mismatch.
type Diag = (String, usize, usize);
/// A document whose diagnostics differ: its index, stream-only, alone-only.
type Mismatch = (usize, Vec<Diag>, Vec<Diag>);
fn recovered_mismatches(bytes: &[u8]) -> (Vec<Mismatch>, usize) {
    let settings = oneyaml::Settings::new();
    let stream = Stream::from_slice_recovering(bytes, &settings).into_value();
    let mut joined = Vec::new();
    let mut out = Vec::new();
    let mut excused = 0;
    for (i, d) in stream.documents().enumerate() {
        let own = d.to_bytes();
        joined.extend_from_slice(&own);
        let base = d.span().start;
        let len = d.to_string().len();
        let alone = Document::from_slice_recovering(&own, &settings);
        let a = diags(alone.diagnostics(), 0);
        let s = diags(&d.diagnostics(), base);
        let s_only: Vec<Diag> = s.iter().filter(|x| !a.contains(x)).cloned().collect();
        let a_only: Vec<Diag> = a.iter().filter(|x| !s.contains(x)).cloned().collect();
        if s_only.is_empty() && a_only.is_empty() {
            continue;
        }
        let codes = [
            oneyaml::code::UNTERMINATED_FLOW_COLLECTION.to_string(),
            oneyaml::code::DIRECTIVE_IN_DOCUMENT.to_string(),
            oneyaml::code::UNTERMINATED_QUOTED_SCALAR.to_string(),
        ];
        let coded = |x: &Diag| codes.contains(&x.0);
        let boundary = s_only.iter().all(|x| x.1 == 0 && coded(x))
            && a_only.iter().all(|x| x.1 == len && coded(x));
        if boundary {
            excused += 1;
        } else {
            out.push((i, s_only, a_only));
        }
    }
    if !stream.is_empty() {
        assert_eq!(joined, bytes, "recovered regions cover the input");
    }
    (out, excused)
}

#[test]
fn recovered_documents_report_their_own_diagnostics() {
    let cases = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    let (mut bad, mut excused, mut streams) = (Vec::new(), 0, 0);
    let mut inputs: Vec<(String, Vec<u8>)> = cases
        .iter()
        .map(|c| (c.id.clone(), c.input.clone()))
        .collect();
    // An invalid byte in the first document's trailing
    // comment is the first document's (it was reported by the second).
    inputs.push(("review".into(), b"a: 1 # \xff\n---\nb\n".to_vec()));
    inputs.push(("review-2".into(), b"a: 1\n# \xfe\n---\nb: \xff\n".to_vec()));
    for (id, input) in &inputs {
        streams += 1;
        let (m, e) = recovered_mismatches(input);
        excused += e;
        for (i, s, a) in m {
            bad.push(format!(
                "{id} doc {i}: stream-only {s:?} alone-only {a:?} in {:?}",
                String::from_utf8_lossy(input)
            ));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
    // The boundary exceptions stay rare: in the suite, N782 (an unclosed
    // `[` cut off by `---`, reported at that `---` by the next document:
    // a recovery choice) and EB22 (a `%YAML` directive after a document not
    // ended with `...`, reported by the next document at its start), and 5TRB
    // (a quote cut off by `---`, reported at that `---`).
    assert!(excused <= 4, "{excused} boundary exceptions");
    assert!(streams > 400);
    let settings = oneyaml::Settings::new();
    let s = Stream::from_slice_recovering(b"a: 1 # \xff\n---\nb\n", &settings).into_value();
    let first = s.document(0).unwrap();
    assert!(first.has_errors());
    assert_eq!(first.diagnostics().len(), 1);
    assert_eq!(first.diagnostics()[0].code, oneyaml::code::INVALID_UTF8);
    assert!(s.document(1).unwrap().diagnostics().is_empty());
    assert!(!s.document(1).unwrap().has_errors());
    // Also through `parse_recovering` (UTF-8 text, syntax errors).
    for case in &cases {
        let Ok(src) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        let s = Stream::parse_recovering(src.clone()).into_value();
        let owned: usize = s.documents().map(|d| d.diagnostics().len()).sum();
        if !s.is_empty() {
            assert_eq!(
                owned,
                s.diagnostics().len(),
                "{}: each diagnostic once",
                case.id
            );
        }
    }
}
