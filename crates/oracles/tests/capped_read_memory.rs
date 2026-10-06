//! Reading the capped diagnostics of an edited
//! stream whose parse kept syntax diagnostics as skeletons (past
//! `max_diagnostics`) holds no more than parsing its edited text again
//! with the same limits does: the cap bounds the texts a read builds as
//! it bounds a parse's. Once, a read re-parsed a document's
//! region with no cap (one diagnostic with texts per `kN: x: y` line under
//! `lenient_inline_colons`), so its peak grew with the document, not the
//! cap: 47 MB against a 9.7 MB parse at 20,000 lines (518 MB process peak
//! at 200,000 lines, against 121 MB for a parse of the whole text).
//!
//! Measured per thread with `allocation-counter` (an `unsafe impl
//! GlobalAlloc` dev-dependency, so here: the only crate allowed one, as
//! `diagnostics_memory.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Limits, Path, Settings, Stream};

/// Peak bytes held while `f` runs on this thread.
fn peak(f: impl FnOnce()) -> u64 {
    allocation_counter::measure(f).bytes_max
}

fn settings(cap: usize) -> Settings {
    Settings::new()
        .lenient_inline_colons(true)
        .limits(Limits::default().max_diagnostics(cap))
}

/// `---⏎a: 0⏎` and, when `early`, `cap + 1` lines `tI: x: y` (their
/// warnings keep their texts in the parse), then `lines` lines
/// `kN: x: y` (skeletons past the cap); then `rest` small documents.
fn stream(lines: usize, cap: usize, early: bool, rest: usize) -> String {
    let mut s = String::from("---\na: 0\n");
    if early {
        for i in 0..=cap {
            s.push_str(&format!("t{i}: x: y\n"));
        }
    }
    for i in 0..lines {
        s.push_str(&format!("k{i}: x: y\n"));
    }
    for i in 0..rest {
        s.push_str(&format!("---\nb{i}: 0\n"));
    }
    s
}

/// Reads `s`'s diagnostics (stream and document 0) and checks them
/// against a parse of its text; returns the read's peak and the
/// parse's.
fn read_and_parse(s: &Stream, cap: usize, ctx: &str) -> (u64, u64) {
    let mut got = Vec::new();
    let read = peak(|| {
        got = s.diagnostics();
        assert!(!s.document(0).unwrap().diagnostics().is_empty());
    });
    let text = s.to_string();
    let parse = peak(|| {
        std::hint::black_box(Stream::parse_with(text.as_str(), &settings(cap)).unwrap());
    });
    let again = Stream::parse_with(text, &settings(cap)).unwrap();
    assert_eq!(got, again.diagnostics(), "{ctx}");
    // The first `cap`, and the summary.
    assert_eq!(got.len(), cap + 1, "{ctx}");
    (read, parse)
}

#[test]
fn capped_reads_of_edited_streams_hold_no_more_than_a_parse() {
    let cap = 1000;
    let lines = 20_000;
    for (name, early, rest) in [
        // The review's shape: one document (parsed again
        // whole).
        ("one document", false, 0),
        // Two documents, the shown entries with their texts: the first
        // document is read for them, with no re-read.
        ("two documents", false, 1),
        // Two documents whose shown entries become skeletons once the
        // early warnings are removed: the first document's region is
        // read again for their texts, under the cap.
        ("shown skeletons", true, 1),
    ] {
        let mut s = Stream::parse_with(stream(lines, cap, early, rest), &settings(cap)).unwrap();
        assert!(s.syntax().suppressed_diagnostics() > 0, "{name}");
        if early {
            for i in 0..=cap {
                let t = Path::new().with_key(format!("t{i}"));
                let _removed = s.document_mut(0).unwrap().remove_path(&t).unwrap();
            }
        }
        let a = Path::new().with_key("a");
        s.document_mut(0).unwrap().set_path(&a, 1).unwrap();
        let (first, parse) = read_and_parse(&s, cap, name);
        // A later read after another edit (the counts kept).
        s.document_mut(rest).unwrap().set_path(&a, 2).unwrap();
        let (second, _) = read_and_parse(&s, cap, name);
        println!(
            "{name}: {lines} lines, cap {cap}: parse peak {} KB, reads {} KB, {} KB",
            parse / 1000,
            first / 1000,
            second / 1000
        );
        // Measured: at most the parse's peak (the one-document read is
        // that parse); an earlier read took 47 MB for a 9.7 MB parse.
        let bound = parse + parse / 4;
        assert!(first <= bound, "{name}: read peak {first} over {bound}");
        assert!(second <= bound, "{name}: read peak {second} over {bound}");
    }
}

/// On streams of many small documents
/// the first capped read after an edit, which builds the documents'
/// counts, holds no more than a parse, and the counts it keeps are small
/// per document. Before: 50.8 MB against a 21.8 MB parse for 131,073
/// empty flow mappings (the counts took about 240 bytes per document, and
/// every document's empty meaning list was kept for the read), 31.5 MB
/// kept.
#[test]
fn many_small_documents_read_within_a_parse_and_keep_little() {
    let cap = 4;
    let s = |src: &str| {
        Settings::new()
            .lenient_inline_colons(true)
            .duplicate_keys(oneyaml::DuplicateKeys::LastWins)
            .limits(
                Limits::default()
                    .max_diagnostics(cap)
                    .max_documents(1 << 20)
                    .max_document_size(src.len() + (1 << 20)),
            )
    };
    let empty_flows = {
        let mut src = String::from("--- {k0: 0}\n");
        for i in 0..10 {
            src.push_str(&format!("--- {{t{i}: 0}}\n---\nt{i}: x: y\n"));
        }
        src.push_str(&"--- {}\n".repeat(131_072));
        src
    };
    let one_warning_each: String = (0..100_000)
        .map(|i| format!("---\nt: x: y\nk{i}: 0\n"))
        .collect();
    let one_duplicate_each: String = (0..100_000)
        .map(|i| format!("--- {{a: 1, a: 2, k{i}: 0}}\n"))
        .collect();
    for (name, src) in [
        ("empty flow mappings", empty_flows),
        ("a lenient warning each", one_warning_each),
        ("a duplicate key each", one_duplicate_each),
    ] {
        let settings = s(&src);
        let parse = peak(|| {
            std::hint::black_box(Stream::parse_with(src.as_str(), &settings).unwrap());
        });
        let mut st = Stream::parse_with(src.as_str(), &settings).unwrap();
        let docs = st.len() as u64;
        st.document_mut(0)
            .unwrap()
            .set_path(Path::new().with_key("k0"), 1)
            .unwrap();
        let mut got = Vec::new();
        let m = allocation_counter::measure(|| got = st.diagnostics());
        let kept = u64::try_from(m.bytes_current).unwrap_or(0);
        let again = Stream::parse_with(st.to_string(), &settings).unwrap();
        assert_eq!(got, again.diagnostics(), "{name}");
        println!(
            "{name}: {docs} documents: parse peak {} KB, first read peak {} KB, kept {} KB ({} B/document)",
            parse / 1000,
            m.bytes_max / 1000,
            kept / 1000,
            kept / docs
        );
        assert!(
            m.bytes_max <= parse,
            "{name}: read peak {} over a parse's {parse}",
            m.bytes_max
        );
        // The counts: 16 bytes per document, and the list's few entries.
        assert!(kept <= 20 * docs + (64 << 10), "{name}: kept {kept}");
    }
}
