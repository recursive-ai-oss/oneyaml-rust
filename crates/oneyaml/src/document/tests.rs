#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::*;

#[test]
fn strict_errors_follow_the_stream_order() {
    // A syntax error in the first document comes before the second
    // document (the serde path meets it first).
    let s = Shared::from_str("[\n--- a\n".to_owned(), &Settings::new());
    let e = s.first_error(true).unwrap();
    assert_eq!(
        e.code(),
        crate::from_str::<Value>("[\n--- a\n").unwrap_err().code()
    );
    // A second document before any error is `MULTIPLE_DOCUMENTS`; a
    // stream accepts it.
    let s = Shared::from_str("a\n--- [\n".to_owned(), &Settings::new());
    assert_eq!(
        s.first_error(true).unwrap().code(),
        code::MULTIPLE_DOCUMENTS
    );
    assert_ne!(
        s.first_error(false).unwrap().code(),
        code::MULTIPLE_DOCUMENTS
    );
    let s = Shared::from_str("a\n--- b\n".to_owned(), &Settings::new());
    assert!(s.first_error(false).is_none());
    // Decoding errors come first of all.
    let s = Shared::from_slice(b"\xff: [", &Settings::new());
    assert!(s.first_error(true).unwrap().code().range() == oneyaml_core::CodeRange::Syntax);
    assert_eq!(s.pre, 1);
}

#[test]
fn parsed_clone_and_display() {
    let p = Document::parse_recovering("a: [1\n");
    assert!(p.has_errors());
    assert!(!p.diagnostics().is_empty());
    assert_eq!(p.value().to_string(), "a: [1\n");
    let doc = p.into_value();
    // No `Index` on `Document`; a meaning with errors is an
    // error, never a silent `Null`.
    assert!(doc.to_value().is_err());
    let copy = doc.clone();
    assert_eq!(copy.to_string(), doc.to_string());
    let ok = Document::parse("a: 1").unwrap();
    assert_eq!(ok.to_value().unwrap()["a"].as_u64(), Some(1));
    assert_eq!(ok.span(), Span::new(0, 4));
    assert_eq!(ok.encoding(), Encoding::Utf8);
    let stream = Stream::parse("x\n--- y\n").unwrap();
    // A stream document's text is its whole region, its
    // final line break included (was `--- y`, a fixed defect).
    assert_eq!(stream.document(1).unwrap().span(), Span::new(2, 8));
    assert_eq!(stream.document(1).unwrap().to_bytes(), b"--- y\n");
    assert_eq!(format!("{stream}"), "x\n--- y\n");
}

/// `moved_to_end` repeats the syntax layer's shift of the
/// warnings attached after the last element (`shift_diagnostic` /
/// `shift_span` in `oneyaml-syntax`'s `tree/arena.rs`, the source of
/// truth); on an edited document with such a warning both give the same
/// diagnostics, spans included.
#[test]
fn trailing_warnings_move_as_the_syntax_layer_moves_them() {
    use oneyaml_core::path;
    // `k: x: z` with no final line break: the lenient `Y0028` is
    // attached after the last element.
    let settings = Settings::new().lenient_inline_colons(true);
    let src = "b: 1\nk: x: z";
    for (value, grows) in [(12345, true), (7, false)] {
        let mut doc = Document::parse_with(src, &settings).unwrap();
        assert_eq!(doc.shared.trailing.len(), 1);
        assert_eq!(doc.shared.trailing[0].code, code::LENIENT_INLINE_COLON);
        doc.set_path(&path!["b"], value).unwrap();
        let tree = &doc.shared.tree;
        let base = tree.source().len();
        let end = tree.root().span().end;
        assert_eq!(end > base, grows, "{:?}", doc.to_string());
        let ours: Vec<Diagnostic> = doc
            .shared
            .trailing
            .iter()
            .map(|d| moved_to_end(d, base, end))
            .collect();
        let syntax: Vec<Diagnostic> = tree
            .diagnostics_by_element()
            .filter(|(n, _)| n.is_none())
            .map(|(_, d)| d.into_owned())
            .collect();
        assert_eq!(ours, syntax);
        // And what the document reports is the re-parse's.
        let text = doc.to_string();
        let re = Document::parse_with(text.clone(), &settings).unwrap();
        assert_eq!(doc.diagnostics(), re.diagnostics(), "{text:?}");
        assert_eq!(doc.diagnostics().last(), syntax.last());
    }
}

thread_local! {
    /// How many per-document reads (a region's syntax
    /// diagnostics, a document's meaning check) this thread made.
    static DOC_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Whole-text re-parses (`Shared::reparsed`) and region
    /// re-reads for skeleton texts (`Shared::doc_head_filled`).
    static REPARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FILLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Capped reads whose shown skeletons a region re-read did
    /// not fill, so the whole text was parsed again.
    static FALLBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn count_fallback() {
    FALLBACKS.with(|c| c.set(c.get().saturating_add(1)));
}

fn fallbacks() -> usize {
    FALLBACKS.with(std::cell::Cell::get)
}

pub(super) fn count_reparse() {
    REPARSES.with(|c| c.set(c.get().saturating_add(1)));
}

pub(super) fn count_fill() {
    FILLS.with(|c| c.set(c.get().saturating_add(1)));
}

fn reparses() -> usize {
    REPARSES.with(std::cell::Cell::get)
}

fn fills() -> usize {
    FILLS.with(std::cell::Cell::get)
}

/// Counts one per-document read (see `DOC_READS`).
pub(super) fn count_doc_read() {
    DOC_READS.with(|c| c.set(c.get().saturating_add(1)));
}

fn doc_reads() -> usize {
    DOC_READS.with(std::cell::Cell::get)
}

/// A stream of `d` documents, each with a tolerated duplicate
/// key (one warning); every `error_every`-th (when recovering) with a
/// syntax error too.
fn warned_stream(d: usize, error_every: Option<usize>) -> String {
    let mut s = String::new();
    for i in 0..d {
        match error_every {
            Some(e) if i % e == e - 1 => s.push_str("--- [a]]\n"),
            _ => s.push_str(&format!("---\na: 1\na: 2\nk{i}: 0\n")),
        }
    }
    s
}

fn last_wins(cap: usize) -> Settings {
    Settings::new()
        .duplicate_keys(oneyaml_core::DuplicateKeys::LastWins)
        .limits(
            oneyaml_core::Limits::default()
                .max_diagnostics(cap)
                .max_document_size(1 << 14),
        )
}

/// The stream's capped list as the scan of every document builds it (the
/// reference the counted read must equal), and as a parse of its text
/// gives it.
fn check_against_scan(s: &Stream, settings: &Settings, recovering: bool, ctx: &str) {
    let got = s.diagnostics();
    let snapshot = s.document(0).unwrap();
    if snapshot.shared.edited && snapshot.shared.cap_possible() {
        assert_eq!(got, snapshot.shared.capped_scan(), "{ctx}");
    }
    let text = s.to_string();
    let again = if recovering {
        Stream::parse_recovering_with(text, settings).into_value()
    } else {
        Stream::parse_with(text, settings).unwrap()
    };
    assert_eq!(got, again.diagnostics(), "{ctx}");
    for i in 0..s.len() {
        let (a, b) = (s.document(i).unwrap(), again.document(i).unwrap());
        assert_eq!(a.diagnostics(), b.diagnostics(), "{ctx} document {i}");
    }
}

/// The counted capped read equals the scan and a re-parse, for
/// caps 0, 1, small and at the boundary, strict and recovering streams
/// (first errors before, at and after the cut), after edits of documents
/// before, at and after the cut, refused edits (an edit group rolled
/// back), clones and snapshots edited on their own.
#[test]
fn counted_capped_reads_equal_the_scan() {
    use oneyaml_core::path;
    for (recovering, errors) in [(false, None), (true, Some(4)), (true, Some(9))] {
        let d = 12;
        let src = warned_stream(d, errors);
        let total = Stream::parse_recovering_with(src.as_str(), &last_wins(usize::MAX))
            .diagnostics()
            .len();
        for cap in [0, 1, 2, 3, 7, total.saturating_sub(1), total, total + 1] {
            let settings = last_wins(cap);
            let mut s = if recovering {
                Stream::parse_recovering_with(src.as_str(), &settings).into_value()
            } else {
                Stream::parse_with(src.as_str(), &settings).unwrap()
            };
            let editable: Vec<usize> = (0..d)
                .filter(|i| errors.is_none_or(|e| i % e != e - 1))
                .collect();
            for (step, &i) in editable.iter().cycle().take(20).enumerate() {
                let ctx = format!("rec {recovering} cap {cap} step {step} doc {i}");
                let mut doc = s.document_mut(i).unwrap();
                match step % 4 {
                    // A new warning, or one gone.
                    0 => {
                        doc.set_path(&path!["b"], 1).unwrap();
                    }
                    1 => {
                        let _ = doc.remove_path(&path!["b"]);
                        let k = oneyaml_core::Path::new().with_key(format!("k{i}"));
                        doc.set_path(&k, step).unwrap();
                    }
                    // A refused edit (its group rolled back: past
                    // `max_document_size`, which the syntax layer checks
                    // inside the group).
                    2 => {
                        let long = "x".repeat(20_000);
                        assert!(doc.set_path(&path!["a"], &long).is_err(), "{ctx}");
                        doc.set_path(&path!["c"], "x").unwrap();
                    }
                    _ => {
                        // A clone edited on its own, then this stream.
                        let mut copy = s.clone();
                        copy.document_mut(i)
                            .unwrap()
                            .set_path(&path!["z"], 3)
                            .unwrap();
                        check_against_scan(&copy, &settings, recovering, &format!("{ctx} clone"));
                        // A snapshot edited on its own (a fork).
                        let mut snap = s.document(i).unwrap();
                        snap.set_path(&path!["y"], 4).unwrap();
                        let _ = snap.diagnostics();
                        s.document_mut(i).unwrap().set_path(&path!["a"], 5).unwrap();
                    }
                }
                check_against_scan(&s, &settings, recovering, &ctx);
            }
        }
    }
}

/// Reading an over-cap edited stream's
/// diagnostics after each edit reads the documents edited since the last
/// read and those holding what the capped list shows, never every
/// document: the per-document reads per edit and read are the same at
/// 64 and 1,024 documents (before: two per document of the stream).
#[test]
fn over_cap_reads_after_edits_do_not_read_every_document() {
    use oneyaml_core::path;
    let per_edit = |d: usize| {
        let settings = Settings::new()
            .duplicate_keys(oneyaml_core::DuplicateKeys::LastWins)
            .limits(oneyaml_core::Limits::default().max_diagnostics(4));
        let mut s = Stream::parse_with(warned_stream(d, None), &settings).unwrap();
        let last = d - 1;
        // The first capped read builds the counts (O(stream), once).
        s.document_mut(last)
            .unwrap()
            .set_path(&path!["b"], 0)
            .unwrap();
        let first = s.diagnostics();
        assert_eq!(first.len(), 5, "{first:?}");
        let k = 20;
        let before = doc_reads();
        for step in 0..k {
            let mut doc = s.document_mut(last).unwrap();
            doc.set_path(&path!["b"], step).unwrap();
            assert_eq!(s.diagnostics().len(), 5);
            assert_eq!(s.document(last).unwrap().diagnostics().len(), 0);
            assert_eq!(s.document(0).unwrap().diagnostics().len(), 1);
        }
        (doc_reads() - before) / k
    };
    let (small, large) = (per_edit(64), per_edit(1_024));
    eprintln!("per-document reads per edit and read: 64 docs {small}, 1024 docs {large}");
    assert!(small <= 32, "{small}");
    assert_eq!(small, large);
}

/// The counts' slot adds 16 bytes to a parse.
#[test]
fn the_counts_slot_is_small() {
    assert!(std::mem::size_of::<std::sync::Mutex<Option<Box<super::cap_index::CapIndex>>>>() <= 16);
}

/// A stream past the cap on its *syntax*
/// diagnostics (the parse kept skeletons): `t{i}: x: y` under
/// `lenient_inline_colons` gives one `Y0028` per document; `%YAML 1.1`
/// one directive warning per document; recovering streams with more syntax
/// errors than the cap. The counted list (texts of shown skeletons read
/// from their document again) equals a parse of the edited text, whole
/// lists compared, for caps 0, 1, 2, 3 and 7, after edits that keep and
/// that remove early warnings (so skeletons come into the shown part).
#[test]
fn skeleton_streams_read_their_capped_list_from_the_counts() {
    let lenient = |d: usize| -> String {
        (0..d)
            .map(|i| format!("---\nt{i}: x: y\nk{i}: 0\n"))
            .collect()
    };
    let yaml11 = |d: usize| -> String {
        (0..d)
            .map(|i| format!("%YAML 1.1\n---\nt{i}: 1\nk{i}: 0\n...\n"))
            .collect()
    };
    let errors = |d: usize| -> String {
        (0..d)
            .map(|i| {
                if i % 2 == 0 {
                    format!("--- [e{i}]]\n")
                } else {
                    format!("---\nt{i}: 1\nk{i}: 0\n")
                }
            })
            .collect()
    };
    let shapes: [(&str, String, bool, bool); 3] = [
        ("lenient", lenient(12), true, false),
        ("yaml11", yaml11(12), false, false),
        ("errors", errors(12), false, true),
    ];
    let (fills0, reparses0) = (fills(), reparses());
    let mut counted_reads = 0usize;
    for (name, src, lenient_on, recovering) in shapes {
        for cap in [0usize, 1, 2, 3, 7] {
            let settings = Settings::new()
                .lenient_inline_colons(lenient_on)
                .limits(oneyaml_core::Limits::default().max_diagnostics(cap));
            let mut s = if recovering {
                Stream::parse_recovering_with(src.as_str(), &settings).into_value()
            } else {
                Stream::parse_with(src.as_str(), &settings).unwrap()
            };
            if s.syntax().suppressed_diagnostics() == 0 {
                // (Within the cap on syntax diagnostics: the other test's.)
                assert!(cap >= 6, "{name} {cap}");
                continue;
            }
            let docs: Vec<usize> = (0..s.len()).filter(|i| !recovering || i % 2 == 1).collect();
            for (step, &i) in docs.iter().enumerate() {
                let ctx = format!("{name} cap {cap} step {step} doc {i}");
                let mut doc = s.document_mut(i).unwrap();
                let k = oneyaml_core::Path::new().with_key(format!("k{i}"));
                doc.set_path(&k, step).unwrap();
                // Every other step: the document's own warning goes (its
                // `t` entry), so later skeletons come into the list.
                if step % 2 == 0 {
                    let t = oneyaml_core::Path::new().with_key(format!("t{i}"));
                    let _ = doc.remove_path(&t);
                }
                let before = reparses();
                let got = s.diagnostics();
                if reparses() == before {
                    counted_reads += 1;
                }
                let again = s.document(0).unwrap().shared.reparsed().diagnostics;
                assert_eq!(got, again, "{ctx}");
                let text = s.to_string();
                let fresh = if recovering {
                    Stream::parse_recovering_with(text, &settings).into_value()
                } else {
                    Stream::parse_with(text, &settings).unwrap()
                };
                assert_eq!(got, fresh.diagnostics(), "{ctx}");
                for j in 0..s.len() {
                    assert_eq!(
                        s.document(j).unwrap().diagnostics(),
                        fresh.document(j).unwrap().diagnostics(),
                        "{ctx} document {j}"
                    );
                }
            }
        }
    }
    // The counted read served them (the re-parses above are the test's
    // own), filling skeletons from their documents.
    assert!(fills() > fills0, "no skeleton was filled");
    let _ = reparses0;
    eprintln!("counted reads {counted_reads}, fills {}", fills() - fills0);
    assert!(counted_reads > 100, "{counted_reads}");
}

/// Edits and reads of a stream past the cap on
/// syntax diagnostics never parse the whole text again, and read as
/// many documents at 64 as at 1,024 documents.
#[test]
fn skeleton_stream_reads_after_edits_do_not_reparse() {
    let per_edit = |d: usize| {
        let settings = Settings::new()
            .lenient_inline_colons(true)
            .limits(oneyaml_core::Limits::default().max_diagnostics(4));
        let src: String = (0..d).map(|i| format!("---\nt: x: y\nk{i}: 0\n")).collect();
        let mut s = Stream::parse_with(src, &settings).unwrap();
        assert!(s.syntax().suppressed_diagnostics() > 0);
        let last = d - 1;
        let k = oneyaml_core::Path::new().with_key(format!("k{last}"));
        s.document_mut(last).unwrap().set_path(&k, 0).unwrap();
        assert_eq!(s.diagnostics().len(), 5);
        let (reads, reparsed) = (doc_reads(), reparses());
        for step in 0..20 {
            s.document_mut(last).unwrap().set_path(&k, step).unwrap();
            assert_eq!(s.diagnostics().len(), 5);
            assert_eq!(s.document(last).unwrap().diagnostics().len(), 0);
        }
        assert_eq!(reparses(), reparsed, "{d}: the whole text was parsed again");
        (doc_reads() - reads) / 20
    };
    let (small, large) = (per_edit(64), per_edit(1_024));
    eprintln!(
        "skeleton stream, per-document reads per edit and read: 64 docs {small}, 1024 docs {large}"
    );
    assert!(small <= 32, "{small}");
    assert_eq!(small, large);
}

/// A region read again for its shown skeletons' texts
/// is parsed under the stream's own cap. Where the region read on its own
/// does not find what the whole text does (an unclosed flow cut off by the
/// next `---` is that document's error in the whole text, and not in its
/// region; the documents' errors and warnings then interleave differently),
/// a shown skeleton is left unfilled and the whole text is parsed again:
/// the list always equals a parse of the edited text. Generated streams of
/// lenient `: ` warnings, cut-off flows, directives and stray `...`, caps
/// 0 to 3, recovering, after edits of each document that accepts them.
#[test]
fn capped_region_rereads_fall_back_where_a_region_differs() {
    use oneyaml_core::path;
    let pieces = [
        "---\nt: x: y\nu: x: y\n",
        "--- [a\n",
        "--- {b: c: d\n",
        "---\n- [e, f: g: h\n- i: j: k\n",
        "%YAML 1.1\n---\nv: x: y\n",
        "...\n",
        "--- [l]]\n",
        "---\nw: 0\n",
        "# c\n",
    ];
    let mut x: u64 = 0x48d3_5eed;
    let mut next = |n: usize| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        usize::try_from(x % n as u64).unwrap()
    };
    let (fills0, fallbacks0) = (fills(), fallbacks());
    // Crafted: with cap 2 the parse keeps the texts of the three warnings
    // and of the first error (the `[a` cut off at the third `---`); the
    // second error (the `[b` cut off at the fourth `---`, so the last
    // document's) is a skeleton. Once the warnings are removed it is
    // shown, and the last document's region read on its own (`---⏎w: 0⏎`)
    // has no error to fill it from.
    let settings = Settings::new()
        .lenient_inline_colons(true)
        .limits(oneyaml_core::Limits::default().max_diagnostics(2));
    let src = "---\nt: x: y\nu: x: y\nv: x: y\n--- [a\n--- [b\n---\nw: 0\n";
    let mut s = Stream::parse_recovering_with(src, &settings).into_value();
    assert_eq!(s.syntax().suppressed_diagnostics(), 1);
    for key in ["t", "u", "v"] {
        let _removed = s
            .document_mut(0)
            .unwrap()
            .remove_path(oneyaml_core::Path::new().with_key(key))
            .unwrap();
    }
    let before = fallbacks();
    let got = s.diagnostics();
    assert_eq!(fallbacks(), before + 1, "{got:?}");
    let fresh = Stream::parse_recovering_with(s.to_string(), &settings).into_value();
    assert_eq!(got, fresh.diagnostics());
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.iter().all(|d| !SyntaxTree::is_skeleton(d)), "{got:?}");
    let mut reads = 0usize;
    for g in 0..300 {
        let k = 3 + next(8);
        let src: String = (0..k).map(|_| pieces[next(pieces.len())]).collect();
        for cap in 0..4usize {
            let settings = Settings::new()
                .lenient_inline_colons(true)
                .limits(oneyaml_core::Limits::default().max_diagnostics(cap));
            let mut s = Stream::parse_recovering_with(src.as_str(), &settings).into_value();
            if s.syntax().suppressed_diagnostics() == 0 {
                continue;
            }
            for i in 0..s.len() {
                let ctx = format!("gen {g} cap {cap} doc {i}: {src:?}");
                let Some(mut doc) = s.document_mut(i) else {
                    continue;
                };
                // Removing a warned entry brings later skeletons into the
                // shown part; refused edits change nothing.
                let edited =
                    doc.remove_path(&path!["t"]).is_ok() | doc.set_path(&path!["z"], i).is_ok();
                if !edited {
                    continue;
                }
                reads += 1;
                let got = s.diagnostics();
                let fresh = Stream::parse_recovering_with(s.to_string(), &settings).into_value();
                assert_eq!(got, fresh.diagnostics(), "{ctx}");
                for j in 0..s.len() {
                    assert_eq!(
                        s.document(j).unwrap().diagnostics(),
                        fresh.document(j).unwrap().diagnostics(),
                        "{ctx} document {j}"
                    );
                }
            }
        }
    }
    let (filled, fell_back) = (fills() - fills0, fallbacks() - fallbacks0);
    eprintln!("capped region re-reads: {reads} reads, {filled} fills, {fell_back} fallbacks");
    assert!(filled > 0, "no region was read again");
    assert!(fell_back > 0, "no region read differed from the whole text");
}

/// A read after an edit reads the edited document and its neighbours' counts,
/// never an untouched document whose entries the last list showed. A huge first
/// document holds every shown entry (skeletons past the cap, filled from its
/// text read again); editing a small document two documents later reads only
/// that document and its neighbours (4 per-document reads: its meaning check
/// and three regions' syntax counts), and no region is read again for skeleton
/// texts. Before: the huge document was read again on every read (its meaning,
/// its region twice, a re-parse).
#[test]
fn reads_after_edits_take_untouched_shown_documents_from_the_last_list() {
    use oneyaml_core::Path;
    let cap = 100;
    let settings = Settings::new()
        .lenient_inline_colons(true)
        .limits(oneyaml_core::Limits::default().max_diagnostics(cap));
    let mut src = String::from("---\nk0: 0\n");
    for i in 0..=cap {
        src.push_str(&format!("e{i}: x: y\n"));
    }
    for i in 0..5_000 {
        src.push_str(&format!("h{i}: x: y\n"));
    }
    for i in 1..=3 {
        src.push_str(&format!("---\nk{i}: 0\n"));
    }
    let mut s = Stream::parse_with(src, &settings).unwrap();
    // The early warnings (whose texts the parse kept) go: the shown
    // entries are skeletons.
    for i in 0..=cap {
        let _removed = s
            .document_mut(0)
            .unwrap()
            .remove_path(Path::new().with_key(format!("e{i}")))
            .unwrap();
    }
    let k = |i: usize| Path::new().with_key(format!("k{i}"));
    s.document_mut(2).unwrap().set_path(k(2), 0).unwrap();
    let first = s.diagnostics();
    let (reads, filled, reparsed) = (doc_reads(), fills(), reparses());
    for step in 0..10 {
        s.document_mut(2).unwrap().set_path(k(2), step).unwrap();
        assert_eq!(s.diagnostics(), first, "step {step}");
    }
    let per_edit = (doc_reads() - reads) / 10;
    eprintln!("huge shown document, small one edited: {per_edit} per-document reads per edit");
    assert_eq!(
        fills(),
        filled,
        "a region was read again for skeleton texts"
    );
    assert_eq!(reparses(), reparsed);
    assert!(per_edit <= 4, "{per_edit}");
    // An edit before the huge document's entries moves them: taken from
    // the last list, moved.
    s.document_mut(0).unwrap().set_path(k(0), "longer").unwrap();
    let got = s.diagnostics();
    let fresh = Stream::parse_with(s.to_string(), &settings).unwrap();
    assert_eq!(got, fresh.diagnostics());
    s.document_mut(3).unwrap().set_path(k(3), "x").unwrap();
    assert_eq!(
        s.diagnostics(),
        Stream::parse_with(s.to_string(), &settings)
            .unwrap()
            .diagnostics()
    );
}

/// Many documents shown at a large
/// cap. 2,000 documents with a warning each under a cap of 1,000: the
/// list shows 1,001 of them. Editing the last one (not shown) reads only
/// it and its neighbour (before: every shown document, 2,002 per-document
/// reads per edit and read); editing one before the shown ones moves
/// them, read from the last list.
#[test]
fn reads_after_edits_do_not_read_the_shown_documents_again() {
    use oneyaml_core::Path;
    let cap = 1_000;
    let d = 2_000;
    let settings = Settings::new()
        .lenient_inline_colons(true)
        .limits(oneyaml_core::Limits::default().max_diagnostics(cap));
    let src: String = (0..d).map(|i| format!("---\nt: x: y\nk{i}: 0\n")).collect();
    let mut s = Stream::parse_with(src, &settings).unwrap();
    let k = |i: usize| Path::new().with_key(format!("k{i}"));
    let last = d - 1;
    s.document_mut(last).unwrap().set_path(k(last), 0).unwrap();
    let first = s.diagnostics();
    assert_eq!(first.len(), cap + 1);
    let reads = doc_reads();
    for step in 0..10 {
        s.document_mut(last)
            .unwrap()
            .set_path(k(last), step)
            .unwrap();
        assert_eq!(s.diagnostics(), first, "step {step}");
    }
    let per_edit = (doc_reads() - reads) / 10;
    eprintln!("1,001 shown documents, the last edited: {per_edit} per-document reads per edit");
    assert!(per_edit <= 4, "{per_edit}");
    // Editing the second document (shown) moves the later ones.
    s.document_mut(1)
        .unwrap()
        .set_path(k(1), "a longer value")
        .unwrap();
    let reads = doc_reads();
    let got = s.diagnostics();
    // (Its counts, its neighbours' syntax counts, and its own entries.)
    assert!(doc_reads() - reads <= 5, "{}", doc_reads() - reads);
    let fresh = Stream::parse_with(s.to_string(), &settings).unwrap();
    assert_eq!(got, fresh.diagnostics());
    for i in [0, 1, 2, 999, 1000, 1001, last] {
        assert_eq!(
            s.document(i).unwrap().diagnostics(),
            fresh.document(i).unwrap().diagnostics(),
            "document {i}"
        );
    }
}

/// The first edit of a stream document reads none
/// of its diagnostics (before: its whole meaning check, to count them,
/// `Shared::record_count`): no per-document read, at any stream size.
#[test]
fn first_stream_edit_reads_no_document() {
    use oneyaml_core::path;
    for d in [1usize, 16, 256] {
        let settings = last_wins(100_000);
        let src = format!("{}---\nz: 0\n", warned_stream(d, None));
        let mut s = Stream::parse_with(src.as_str(), &settings).unwrap();
        let last = s.len() - 1;
        let before = doc_reads();
        s.document_mut(last)
            .unwrap()
            .set_path(&path!["z"], 1)
            .unwrap();
        assert_eq!(doc_reads() - before, 0, "{d} documents, first edit");
        // A later document's first edit, after the stream was edited, still
        // reads its own region (O(that document): stream edits are local).
        s.document_mut(0)
            .unwrap()
            .set_path(&path!["k0"], 1)
            .unwrap();
        check_against_scan(&s, &settings, false, "first edit");
    }
    // Strict default settings: a plain stream.
    let mut s = Stream::parse("a: 1\n---\nb: 2\n").unwrap();
    let before = doc_reads();
    s.document_mut(1).unwrap().set_path(&path!["b"], 3).unwrap();
    assert_eq!(doc_reads() - before, 0);
}

/// The count of an untouched document read from the parse's
/// list (`Shared::parsed_count`) equals its complete count (what a fresh count
/// read, `Shared::complete_count`), over the yaml-test-suite inputs and
/// the fuzz corpora, strict and recovering, under several caps and every
/// meaning switch (merge keys, `LastWins`, lenient colons, both `Compat`
/// presets).
#[test]
#[ignore = "slow: runs in CI"]
fn parsed_counts_equal_complete_counts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut inputs: Vec<String> = Vec::new();
    let mut stack = vec![root.join("test-suite/data/yaml-test-suite")];
    for dir in ["consistency", "cap", "edit", "handles", "serde"] {
        stack.push(root.join("fuzz/corpus").join(dir));
    }
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let suite = p.file_name().is_some_and(|n| n == "in.yaml");
            let fuzz = p.to_string_lossy().contains("/fuzz/corpus/");
            if suite || fuzz {
                if let Ok(t) = String::from_utf8(std::fs::read(&p).unwrap()) {
                    inputs.push(t);
                }
            }
        }
    }
    assert!(inputs.len() > 1_000, "{}", inputs.len());
    let mut compared = 0usize;
    for src in &inputs {
        for cap in [1usize, 16, 100_000] {
            let limits = oneyaml_core::Limits::default().max_diagnostics(cap);
            let base = Settings::new().limits(limits.clone());
            // Every switch that changes the meaning checks.
            let all = [
                base.clone(),
                base.clone()
                    .duplicate_keys(oneyaml_core::DuplicateKeys::LastWins),
                base.clone().merge_keys(true),
                base.clone().lenient_inline_colons(true),
                oneyaml_core::Compat::serde_yaml().limits(limits.clone()),
                oneyaml_core::Compat::yaml_1_1().limits(limits),
            ];
            for settings in all {
                for strict in [false, true] {
                    let s = if strict {
                        match Stream::parse_with(src.as_str(), &settings) {
                            Ok(s) => s,
                            Err(_) => continue,
                        }
                    } else {
                        Stream::parse_recovering_with(src.as_str(), &settings).into_value()
                    };
                    let Some(doc) = s.document(0) else {
                        continue;
                    };
                    let shared = &doc.shared;
                    for i in 0..s.len() {
                        if let Some(n) = shared.parsed_count(i) {
                            compared += 1;
                            assert_eq!(
                                n,
                                shared.complete_count(i),
                                "{src:?} document {i} cap {cap} strict {strict}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(compared > 1_000, "{compared}");
}

// ---------------------------------------------------- counted work

fn ckey_work() -> usize {
    ckeys::WORK.with(std::cell::Cell::get)
}

fn reset_ckey_work() {
    ckeys::WORK.with(|w| w.set(0));
}

/// The value node of the first collection-key entry of the mapping `at`.
fn first_collection_entry(doc: &Document, at: &str) -> NodeId {
    doc.get_path(&crate::path![at])
        .unwrap()
        .as_mapping()
        .unwrap()
        .iter()
        .find(|(k, _)| k.as_scalar().is_none())
        .map(|(_, v)| v.id())
        .unwrap()
}

/// Moves the first collection-key entry of `from` into `to`.
fn move_first(doc: &mut Document, from: &str, to: &str) {
    let id = first_collection_entry(doc, from);
    let removed = doc.remove_node(id).unwrap();
    let target = doc.get_path(&crate::path![to]).unwrap().id();
    doc.node_mut(target)
        .unwrap()
        .insert_removed(removed)
        .map_err(|e| e.to_string())
        .unwrap();
}

fn collection_keys(k: usize) -> String {
    let mut s = String::from("m:\n");
    for i in 0..k {
        s.push_str(&format!("  ? [{i}]\n  : {i}\n"));
    }
    s.push_str("n:\n  z: 0\n");
    s
}

/// Work to move entries back and forth `rounds` times between a mapping of
/// `k` collection keys and a small one (two operations each).
fn moving_work(k: usize, rounds: usize) -> usize {
    let mut doc = Document::parse(collection_keys(k)).unwrap();
    reset_ckey_work();
    for _ in 0..rounds {
        move_first(&mut doc, "m", "n");
        move_first(&mut doc, "n", "m");
    }
    ckey_work()
}

#[test]
fn putting_collection_keys_back_costs_the_keys_once_and_each_operation_its_own_key() {
    let (w_200, w_400) = (moving_work(200, 20), moving_work(400, 20));
    let w_400_more = moving_work(400, 40);
    // Reading K keys: a few units each, once.
    assert!(w_200 <= 8 * 200, "{w_200}");
    assert!(w_400 <= 8 * 400, "{w_400}");
    // Twice the keys costs about twice the reading; twice the operations
    // costs only the operations (each reads its own small key).
    assert!(w_400 - w_200 <= 8 * 200, "{w_200} {w_400}");
    assert!(w_400_more - w_400 <= 40 * 2 * 8, "{w_400} {w_400_more}");
}

#[test]
fn an_anchored_node_named_by_many_keys_is_read_once() {
    let items: Vec<String> = (0..10_000).map(|i| i.to_string()).collect();
    let mut src = format!("big: &b [{}]\nm:\n", items.join(", "));
    for i in 0..100 {
        src.push_str(&format!("  ? [*b, {i}]\n  : {i}\n"));
    }
    src.push_str("n:\n  z: 0\n");
    let mut doc = Document::parse(src).unwrap();
    reset_ckey_work();
    move_first(&mut doc, "m", "n");
    let first = ckey_work();
    // The anchor once (10,000 items and the sequence), the keys beside it.
    assert!(first <= 10_000 + 100 * 8 + 100, "{first}");
    move_first(&mut doc, "n", "m");
    move_first(&mut doc, "m", "n");
    let all = ckey_work();
    assert!(all - first <= 2 * 16, "{first} {all}");
}

#[test]
fn a_one_line_entry_put_back_reads_no_siblings_for_its_layout() {
    let mut src = String::new();
    for i in 0..1000 {
        src.push_str(&format!("k{i}: v\n"));
    }
    let mut doc = Document::parse(src).unwrap();
    let removed = doc.root_mut().unwrap().remove("k500").unwrap();
    place::LOOK_WORK.with(|w| w.set(0));
    doc.root_mut()
        .unwrap()
        .insert_removed(removed)
        .map_err(|e| e.to_string())
        .unwrap();
    assert_eq!(place::LOOK_WORK.with(std::cell::Cell::get), 0);
    // Nor does a new one-line value.
    doc.root_mut().unwrap().insert("new", "x").unwrap();
    assert_eq!(place::LOOK_WORK.with(std::cell::Cell::get), 0);
    // New block content reads the entries it copies its layout from.
    doc.root_mut()
        .unwrap()
        .insert("nested", vec![1, 2])
        .unwrap();
    assert!(place::LOOK_WORK.with(std::cell::Cell::get) > 0);
}
