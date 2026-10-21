//! Meaning never depends on
//! `max_diagnostics`. The cap bounds how many diagnostics are stored and
//! reported; what reads accept or refuse, `has_errors`, restricted mode,
//! edit acceptance, and the cost of a read are the same under every limit.
//! The corpus-wide property is `crates/bench/tests/cap_invariance.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]

mod work;

use oneyaml::{Document, Limits, Settings, Severity, Stream, Value, code, path};

fn with(limits: Limits) -> Settings {
    Settings::new().limits(limits)
}

/// What a read gives, without the message (the only part that may differ:
/// past the cap, an error's message is its code's summary).
fn read(r: Result<Value, oneyaml::Error>) -> Result<Value, (oneyaml::Code, Option<oneyaml::Span>)> {
    r.map_err(|e| (e.code(), e.span()))
}

/// Reading every document of a recovering stream whose errors
/// are past the cap costs the same per document at 2,000 and 16,000
/// documents (it re-lexed the stream per error: 66.7 s at 16k), and gives
/// the same errors as without the cap.
#[test]
fn reading_documents_past_the_cap_is_linear() {
    let timed = |n: usize| {
        let src = "a: [\n---\n".repeat(n);
        let capped = Limits::untrusted().max_documents(usize::MAX);
        let p = Stream::parse_recovering_with(src.as_str(), &with(capped));
        let docs: Vec<Document> = p.value().documents().collect();
        work::reset();
        let reads: Vec<_> = docs.iter().map(|d| read(d.to_value())).collect();
        let regions: usize = docs
            .iter()
            .flat_map(|d| {
                d.errors()
                    .map(|e| e.diagnostic().map(|x| x.code))
                    .collect::<Vec<_>>()
            })
            .count();
        let t = work::taken();
        assert!(regions >= n);
        (t, reads)
    };
    let (a, _) = timed(2_000);
    let (b, reads) = timed(16_000);
    let r = work::ratio("past the cap", a, b) / 8.0;
    eprintln!("reads past the cap: {a:?} at 2k, {b:?} at 16k (per-document ratio {r:.2})");
    assert!(
        r < 3.0,
        "reading a document past the cap scales with the stream: {r:.2}"
    );
    // The same errors as with no cap.
    let src = "a: [\n---\n".repeat(16_000);
    let full = Stream::parse_recovering_with(
        src.as_str(),
        &with(
            Limits::untrusted()
                .max_documents(usize::MAX)
                .max_diagnostics(usize::MAX),
        ),
    );
    let want: Vec<_> = full
        .value()
        .documents()
        .map(|d| read(d.to_value()))
        .collect();
    assert_eq!(reads, want);
}

/// Decoding errors past the cap still decide `has_errors` and
/// the reads (from the invalid runs, always complete).
#[test]
fn decoding_errors_past_the_cap_still_count() {
    let bytes = b"--- \xff\n".repeat(4);
    for limit in [0usize, 1, 2, usize::MAX] {
        let s = with(Limits::default().max_diagnostics(limit));
        let p = Stream::from_slice_recovering(&bytes, &s);
        assert_eq!(p.value().len(), 4);
        for (i, d) in p.value().documents().enumerate() {
            assert!(d.has_errors(), "limit {limit}, document {i}");
            let e = d.to_value().unwrap_err();
            assert_eq!(e.code(), code::INVALID_UTF8, "limit {limit}, document {i}");
            assert!(e.to_string().starts_with("invalid UTF-8 at input bytes"));
        }
        assert!(p.value().has_errors());
        let doc = Document::from_slice_recovering(&bytes, &s).into_value();
        assert!(doc.has_errors());
        assert!(oneyaml::from_slice_with::<Value>(&bytes, &s).is_err());
        assert!(Stream::from_slice_with(&bytes, &s).is_err());
    }
}

/// An edit next to warnings the tree keeps without texts is
/// accepted (or refused) exactly as without the cap, with the same text,
/// and its diagnostics equal a re-parse.
#[test]
fn edits_next_to_suppressed_warnings_do_not_depend_on_the_cap() {
    let src = "- a: b: c\n".repeat(6);
    let outcome = |limit: usize, at: usize| {
        let s = Settings::new()
            .lenient_inline_colons(true)
            .limits(Limits::default().max_diagnostics(limit));
        let mut doc = Document::parse_with(src.as_str(), &s).unwrap();
        let r = doc.root_mut().unwrap().remove_at(at).map(|_| ());
        let text = doc.to_string();
        let again = Document::parse_with(text.as_str(), &s)
            .unwrap()
            .diagnostics();
        let now = doc.diagnostics();
        assert_eq!(now, again, "limit {limit}, remove_at({at})");
        (r.is_ok(), text)
    };
    for at in 0..6 {
        let want = outcome(usize::MAX, at);
        assert!(want.0, "remove_at({at}) without a cap");
        for limit in [0usize, 1, 2, 3, 16] {
            assert_eq!(outcome(limit, at), want, "limit {limit}, remove_at({at})");
        }
    }
}

/// Stream edits are local: in a stream with no diagnostics, reading a
/// document's diagnostics and errors after an edit costs the same per
/// document whatever the stream's size, under default limits, at sizes
/// whose trees exceed `max_diagnostics` elements.
#[test]
fn a_documents_diagnostics_after_an_edit_stay_local() {
    let timed = |k: usize| -> (u64, usize) {
        let src = "---\na: 1\nb: 2\n".repeat(k);
        let mut s = Stream::parse(src).unwrap();
        let elements = s.syntax().len();
        let mut min = u64::MAX;
        for round in 0..3u64 {
            work::reset();
            for i in (0..k).step_by(k / 200) {
                let mut d = s.document_mut(i).unwrap();
                d.set_path(&path!["b"], round + 3).unwrap();
                assert!(!d.has_errors());
                assert!(d.diagnostics().is_empty());
            }
            min = min.min(work::taken());
        }
        (min, elements)
    };
    let (a, _) = timed(4_000);
    let (b, elements) = timed(40_000);
    assert!(elements > 200_000, "{elements} elements");
    let r = work::ratio("diagnostics after an edit", a, b);
    eprintln!(
        "200 edits + reads: {a:?} at 4k documents, {b:?} at 40k ({elements} elements, ratio {r:.2})"
    );
    assert!(
        r < 4.0,
        "a document's diagnostics after an edit scale with the stream: {r:.2}"
    );
}

/// Meaning errors past the cap still make their documents
/// have errors, and restricted mode is the same.
#[test]
fn meaning_errors_past_the_cap_still_count() {
    let src = "--- !!int x\n".repeat(4);
    let shape = |limit: usize| {
        let s = with(Limits::default().max_diagnostics(limit));
        let p = Stream::parse_recovering_with(src.as_str(), &s);
        let mut out = Vec::new();
        for i in 0..4 {
            let d = p.value().document(i).unwrap();
            let r = read(d.to_value());
            let mut st = p.value().clone();
            let edit = st
                .document_mut(i)
                .unwrap()
                .set_root(1)
                .map(|_| ())
                .map_err(|e| {
                    format!("{e:?}")
                        .split(['(', ' ', '{'])
                        .next()
                        .unwrap_or("")
                        .to_owned()
                });
            out.push((d.has_errors(), r, edit));
        }
        out
    };
    let want = shape(usize::MAX);
    assert!(want.iter().all(|(e, r, _)| *e && r.is_err()));
    for limit in [0usize, 1, 2] {
        assert_eq!(shape(limit), want, "limit {limit}");
    }
}

/// A document whose diagnostics the tree kept only as
/// skeletons ends its error's warnings with a summary at the first of
/// them.
#[test]
fn a_summary_is_placed_even_when_every_entry_is_a_skeleton() {
    let s = Settings::new()
        .lenient_inline_colons(true)
        .limits(Limits::default().max_diagnostics(1));
    let src = format!(
        "{}---\n{}- !!int x\n",
        "- a: b: c\n".repeat(3),
        "- a: b: c\n".repeat(2)
    );
    let st = Stream::parse_recovering_with(src.as_str(), &s).into_value();
    let e = st.document(1).unwrap().to_value().unwrap_err();
    assert_eq!(e.code(), code::INVALID_SCALAR_FOR_TAG);
    let w = e.warnings();
    let summary = w.last().unwrap();
    assert_eq!(summary.code, code::DIAGNOSTICS_SUPPRESSED);
    let second = src.find("---").unwrap();
    assert!(summary.primary.span.start > second, "{summary:?}");
    assert!(w.iter().all(|d| d.severity == Severity::Warning));
}
