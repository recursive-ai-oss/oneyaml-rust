//! Per-document accessors of a `Stream` cost O(own size + log n),
//! so reading every document is linear in the stream (once,
//! `diagnostics`/`has_errors`/`to_bytes` used to scan the whole stream per
//! document, 6.6 s at 32k documents).
//!
//! Each case times reading every document at 8k and 32k documents: linear
//! work grows 4×, quadratic 16×; the test fails above 9×. The best of
//! three runs is taken to damp scheduler noise.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod work;

use oneyaml::{Settings, Stream};

fn read_all(stream: &Stream) -> usize {
    let mut work = 0usize;
    for d in stream.documents() {
        work += d.diagnostics().len();
        work += usize::from(d.has_errors());
        work += d.to_bytes().len();
        work += usize::from(d.to_value().is_ok());
    }
    work
}

fn best_of_three(stream: &Stream) -> u64 {
    (0..3)
        .map(|_| {
            work::reset();
            std::hint::black_box(read_all(stream));
            work::taken()
        })
        .min()
        .unwrap()
}

fn assert_linear(name: &str, make: impl Fn(usize) -> Stream, per_doc_errors: bool) {
    let small = make(8_000);
    let large = make(32_000);
    assert_eq!(small.len(), 8_000, "{name}");
    assert_eq!(large.len(), 32_000, "{name}");
    // Every document owns its own diagnostics, and only those.
    for s in [&small, &large] {
        let errs = s.documents().filter(|d| d.has_errors()).count();
        let expected = if per_doc_errors { s.len() } else { 0 };
        assert_eq!(errs, expected, "{name}");
        assert!(s.documents().all(|d| d.diagnostics().len() <= 2), "{name}");
    }
    let (t_small, t_large) = (best_of_three(&small), best_of_three(&large));
    let ratio = work::ratio(name, t_small, t_large);
    eprintln!("{name}: 8k {t_small:?} steps, 32k {t_large:?}, ratio {ratio:.1}");
    assert!(
        ratio < 9.0,
        "{name}: reading every document is not linear: 8k {t_small:?}, 32k {t_large:?}"
    );
}

#[test]
fn every_document_with_a_syntax_error() {
    assert_linear(
        "syntax",
        |n| Stream::parse_recovering("--- [a]]\n".repeat(n)).into_value(),
        true,
    );
}

/// Every document holds an error node whose diagnostic the next
/// document owns (an unclosed `[` cut off by `---`); `has_errors` stays
/// O(log n) per document.
#[test]
fn every_document_with_an_error_node_reported_by_the_next() {
    assert_linear(
        "cut-off",
        |n| Stream::parse_recovering("--- [a\n".repeat(n)).into_value(),
        true,
    );
}

#[test]
fn every_document_with_invalid_bytes() {
    assert_linear(
        "decoding",
        |n| {
            let bytes = b"--- \xff\n".repeat(n);
            Stream::from_slice_recovering(&bytes, &Settings::new()).into_value()
        },
        true,
    );
}

#[test]
fn clean_documents() {
    assert_linear(
        "clean",
        |n| Stream::parse("---\na: 1\n".repeat(n)).unwrap(),
        false,
    );
}

/// A stream whose diagnostics
/// exceed `max_diagnostics`, edited and read after every edit. Each read
/// used to rebuild the capped list from every document, or (past the cap
/// on syntax diagnostics, whose texts the parse did not keep) parse the
/// whole edited text again: O(stream) per edit. It now reads the
/// documents' counts. 100 edits + reads of the last document at 2k and
/// 16k documents: O(stream) per read grows 8×, the counted read stays
/// flat; the test fails above 3×.
fn assert_flat_over_cap(name: &str, settings: &oneyaml::Settings, doc: impl Fn(usize) -> String) {
    use oneyaml::Path;
    let key = |n: usize| Path::new().with_key(format!("k{}", n - 1));
    let make = |n: usize| {
        let src: String = (0..n).map(&doc).collect();
        let mut s = Stream::parse_with(src, settings).unwrap();
        // The first read after an edit builds what the next ones use.
        s.document_mut(n - 1).unwrap().set_path(key(n), 0).unwrap();
        assert_eq!(s.diagnostics().len(), 5, "{name}");
        s
    };
    let run = |s: &mut Stream| {
        let last = s.len() - 1;
        let k = key(s.len());
        (0..3)
            .map(|_| {
                work::reset();
                for step in 0..100 {
                    s.document_mut(last).unwrap().set_path(&k, step).unwrap();
                    std::hint::black_box(s.diagnostics());
                    std::hint::black_box(s.document(last).unwrap().diagnostics());
                }
                work::taken()
            })
            .min()
            .unwrap()
    };
    let (mut small, mut large) = (make(2_000), make(16_000));
    let (t_small, t_large) = (run(&mut small), run(&mut large));
    let ratio = work::ratio(name, t_small, t_large);
    eprintln!(
        "{name}: over-cap edits+reads: 2k {t_small:?} steps, 16k {t_large:?}, ratio {ratio:.1}"
    );
    assert!(
        ratio < 3.0,
        "{name}: over-cap reads after edits grow with the stream: 2k {t_small:?}, 16k {t_large:?}"
    );
}

/// Past the cap on meaning diagnostics (a tolerated duplicate key each).
#[test]
fn over_cap_stream_edits_and_reads() {
    use oneyaml::{DuplicateKeys, Limits};
    let settings = Settings::new()
        .duplicate_keys(DuplicateKeys::LastWins)
        .limits(Limits::default().max_diagnostics(4));
    assert_flat_over_cap("meaning", &settings, |i| {
        format!("---\na: 1\na: 2\nk{i}: 0\n")
    });
}

/// Past the cap on syntax diagnostics (a lenient `Y0028`
/// each; the parse keeps skeletons past the cap).
#[test]
fn over_cap_on_syntax_stream_edits_and_reads() {
    use oneyaml::Limits;
    let settings = Settings::new()
        .lenient_inline_colons(true)
        .limits(Limits::default().max_diagnostics(4));
    assert_flat_over_cap("syntax", &settings, |i| format!("---\nt: x: y\nk{i}: 0\n"));
}
