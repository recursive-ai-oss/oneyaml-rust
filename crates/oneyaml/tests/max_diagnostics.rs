//! `Limits::max_diagnostics`.
//!
//! A list of diagnostics keeps its first `max_diagnostics` entries, the
//! first error wherever it is, and ends with one `Y2010` summary counting
//! the rest exactly. One budget per stream; every view agrees; edits
//! keep `diagnostics()` equal to a re-parse with the same limits. The peak
//! memory of the security-review triggers is checked in
//! `crates/oracles/tests/diagnostics_memory.rs` (allocation counter).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{
    Compat, Deserializer, Diagnostic, Document, DuplicateKeys, Limits, Settings, Severity, Stream,
    Value, code, path,
};
use serde::Deserialize;

/// `n` duplicate keys tolerated by `LastWins`: `n` warnings.
fn dups(n: usize) -> String {
    let mut s = String::from("a: 0\n");
    for i in 0..n {
        s.push_str(&format!("a: {i}\n"));
    }
    s
}

/// `n` `Y0028` warnings under `lenient_inline_colons`.
fn colons(n: usize) -> String {
    "- a: b: c\n".repeat(n)
}

/// `n` orphan `%YAML` directives (an error each) for a recovering parse.
fn orphans(n: usize) -> String {
    "%YAML 1.2\n...\n".repeat(n)
}

fn last_wins(limits: Limits) -> Settings {
    Compat::serde_yaml().limits(limits)
}

fn lenient(limits: Limits) -> Settings {
    Settings::new().lenient_inline_colons(true).limits(limits)
}

fn key(d: &Diagnostic) -> (oneyaml::Code, Severity, usize, usize, String) {
    (
        d.code,
        d.severity,
        d.primary.span.start,
        d.primary.span.end,
        d.message.clone(),
    )
}

fn keys(list: &[Diagnostic]) -> Vec<(oneyaml::Code, Severity, usize, usize, String)> {
    list.iter().map(key).collect()
}

/// The summary of a capped list, if it has one.
fn summary(list: &[Diagnostic]) -> Option<&Diagnostic> {
    list.iter().find(|d| d.code == code::DIAGNOSTICS_SUPPRESSED)
}

#[test]
fn y2010_is_a_registered_warning_naming_the_setting() {
    let info = code::DIAGNOSTICS_SUPPRESSED.info().unwrap();
    assert_eq!(code::DIAGNOSTICS_SUPPRESSED.to_string(), "Y2010");
    assert_eq!(info.severity, Severity::Warning);
    let p =
        Document::parse_recovering_with(dups(3), &last_wins(Limits::default().max_diagnostics(1)));
    let s = summary(p.diagnostics()).unwrap();
    assert_eq!(s.message, "2 more diagnostics suppressed");
    assert_eq!(
        s.help.as_deref(),
        Some("raise Limits::max_diagnostics (currently 1) to see them")
    );
}

/// Exactly at the cap nothing is suppressed; one over keeps the cap and the
/// summary, located at the first suppressed diagnostic, counting one.
#[test]
fn boundaries_at_the_cap_and_one_over() {
    for limit in [0usize, 1, 5] {
        let s = last_wins(Limits::untrusted().max_diagnostics(limit));
        let at = Document::parse_with(dups(limit), &s).unwrap();
        assert_eq!(at.diagnostics().len(), limit, "at {limit}");
        assert!(summary(&at.diagnostics()).is_none());
        let full = Document::parse_with(dups(limit + 1), &last_wins(Limits::unlimited())).unwrap();
        let over = Document::parse_with(dups(limit + 1), &s).unwrap();
        let d = over.diagnostics();
        assert_eq!(d.len(), limit + 1, "over {limit}");
        assert_eq!(keys(&d[..limit]), keys(&full.diagnostics()[..limit]));
        assert_eq!(d[limit].message, "1 more diagnostic suppressed");
        assert_eq!(
            d[limit].primary.span.start,
            full.diagnostics()[limit].primary.span.start
        );
        assert_eq!(d[limit].severity, Severity::Warning);
    }
}

/// The three triggers of the security review, at two sizes and under both
/// profiles: every list ends with the exact summary and holds at most
/// `max_diagnostics + 2` entries.
#[test]
#[ignore = "slow: runs in CI"]
fn the_triggers_are_capped_with_an_exact_summary() {
    for (profile, limit) in [
        (Limits::untrusted(), 1_000usize),
        (Limits::default(), 100_000),
    ] {
        for n in [3_000usize, 30_000, 150_000] {
            if limit > n && n != 3_000 {
                continue;
            }
            // Duplicate keys (LastWins), every reader.
            let src = dups(n);
            let s = last_wins(profile.clone());
            let doc = Document::parse_with(src.as_str(), &s).unwrap();
            let d = doc.diagnostics();
            check(&d, n, limit, "document dups");
            let (_, w) = oneyaml::from_str_with_diagnostics::<Value>(&src, &s).unwrap();
            check(&w, n, limit, "serde dups");
            assert_eq!(keys(&w), keys(&d), "serde and Document agree");
            // Lenient inline colons.
            let src = colons(n);
            let s = lenient(profile.clone());
            let doc = Document::parse_with(src.as_str(), &s).unwrap();
            check(&doc.diagnostics(), n, limit, "document colons");
            let (_, w) = oneyaml::from_str_with_diagnostics::<Value>(&src, &s).unwrap();
            assert_eq!(keys(&w), keys(&doc.diagnostics()), "colons agree");
            // Orphan directives, recovering.
            let src = orphans(n);
            let p = Stream::parse_recovering_with(
                src.as_str(),
                &Settings::new().limits(profile.clone()),
            );
            check(p.diagnostics(), n, limit, "orphans");
            assert!(p.has_errors());
            assert_eq!(keys(&p.value().diagnostics()), keys(p.diagnostics()));
        }
    }
}

fn check(list: &[Diagnostic], found: usize, limit: usize, what: &str) {
    if found <= limit {
        assert_eq!(list.len(), found, "{what}");
        assert!(summary(list).is_none(), "{what}");
        return;
    }
    assert!(list.len() <= limit + 2, "{what}: {} kept", list.len());
    let s = summary(list).unwrap();
    let kept = list.len() - 1;
    assert_eq!(
        s.message,
        format!("{} more diagnostics suppressed", found - kept),
        "{what}"
    );
}

/// The first error is kept wherever it is: `has_errors` and the strict
/// outcome never change, and a strict parse fails with it, carrying the
/// capped warnings before it.
#[test]
fn the_first_error_is_kept_past_the_cap() {
    let s = last_wins(Limits::default().max_diagnostics(3));
    let src = format!("{}b: !!int x\n", dups(10));
    let p = Document::parse_recovering_with(src.as_str(), &s);
    let d = p.diagnostics();
    assert_eq!(d.len(), 5);
    assert_eq!(d[3].message, "7 more diagnostics suppressed");
    assert_eq!(d[4].code, code::INVALID_SCALAR_FOR_TAG);
    assert!(p.has_errors());
    let strict = Document::parse_with(src.as_str(), &s).unwrap_err();
    assert_eq!(strict.code(), code::INVALID_SCALAR_FOR_TAG);
    assert_eq!(strict.warnings().len(), 4);
    assert_eq!(
        strict.warnings()[3].message,
        "7 more diagnostics suppressed"
    );
    let serde = oneyaml::from_str_with::<Value>(&src, &s).unwrap_err();
    assert_eq!(serde.code(), strict.code());
    assert_eq!(serde.span(), strict.span());
    assert_eq!(keys(serde.warnings()), keys(strict.warnings()));
    // A syntax error past the cap, with syntax warnings before it.
    let s = lenient(Limits::default().max_diagnostics(2));
    let src = format!("{}- [\n", colons(6));
    let p = Document::parse_recovering_with(src.as_str(), &s);
    let d = p.diagnostics();
    assert_eq!(d.len(), 4);
    assert_eq!(d[2].message, "4 more diagnostics suppressed");
    assert_eq!(d[3].severity, Severity::Error);
    let strict = Document::parse_with(src.as_str(), &s).unwrap_err();
    assert_eq!(
        (strict.code(), strict.span()),
        (d[3].code, Some(d[3].primary.span))
    );
    assert_eq!(strict.warnings().len(), 3);
    assert_eq!(
        strict.warnings()[2].message,
        "4 more diagnostics suppressed"
    );
    let serde = oneyaml::from_str_with_diagnostics::<Value>(&src, &s).unwrap_err();
    assert_eq!(keys(serde.warnings()), keys(strict.warnings()));
}

/// Errors past the cap keep their regions, and reading them still reports
/// the right error (found again from the text).
#[test]
fn suppressed_error_regions_still_explain_themselves() {
    let src = "a: [\n---\nb: [\n---\nc: [\n";
    let unlimited = Stream::parse_recovering(src);
    let capped = Stream::parse_recovering_with(
        src,
        &Settings::new().limits(Limits::default().max_diagnostics(1)),
    );
    for (i, (a, b)) in unlimited
        .value()
        .documents()
        .zip(capped.value().documents())
        .enumerate()
    {
        let ea = a.to_value().unwrap_err();
        let eb = b.to_value().unwrap_err();
        assert_eq!(
            (ea.code(), ea.span()),
            (eb.code(), eb.span()),
            "document {i}"
        );
        // Past the cap the same code and spans, with the code's
        // summary for a message and a help naming the limit.
        let shape = |d: &oneyaml::Diagnostic| (d.code, d.severity, d.primary.span);
        let ra: Vec<_> = a
            .errors()
            .map(|e| e.diagnostic().map(|d| shape(&d)))
            .collect();
        let rb: Vec<_> = b
            .errors()
            .map(|e| e.diagnostic().map(|d| shape(&d)))
            .collect();
        assert_eq!(ra, rb, "document {i}");
        if i == 2 {
            let d = b.errors().next().unwrap().diagnostic().unwrap();
            assert_eq!(d.message, "flow collection not closed");
            assert!(
                d.help
                    .as_deref()
                    .unwrap()
                    .contains("Limits::max_diagnostics (currently 1)")
            );
        }
        assert_eq!(a.has_errors(), b.has_errors());
    }
}

/// One budget per stream: the documents' views tile the stream's capped
/// list, the summary belongs to the document where suppression starts, and
/// the documents after it own nothing.
#[test]
fn a_stream_shares_one_budget_and_its_documents_tile_it() {
    let s = lenient(Limits::default().max_diagnostics(4));
    let src = format!("{}---\n{}---\n{}", colons(3), colons(3), colons(3));
    let p = Stream::parse_recovering_with(src.as_str(), &s);
    tiles(p.diagnostics(), p.value());
    let st = Stream::parse_with(src.as_str(), &s).unwrap();
    tiles(&st.diagnostics(), &st);
}

fn tiles(all: &[Diagnostic], st: &Stream) {
    assert_eq!(all.len(), 5);
    assert_eq!(all[4].message, "5 more diagnostics suppressed");
    let per: Vec<Vec<Diagnostic>> = st.documents().map(|d| d.diagnostics()).collect();
    assert_eq!(per[0].len(), 3);
    assert_eq!(per[1].len(), 2, "one kept and the summary");
    assert_eq!(per[1][1].code, code::DIAGNOSTICS_SUPPRESSED);
    assert!(per[2].is_empty());
    let tiled: Vec<Diagnostic> = per.into_iter().flatten().collect();
    assert_eq!(keys(&tiled), keys(all));
}

/// The multi-document `Deserializer` keeps the same warnings an error
/// carries: capped, with the summary.
#[test]
fn the_deserializer_iterator_caps_an_errors_warnings() {
    let s = last_wins(Limits::default().max_diagnostics(2));
    let src = format!("{}---\nx: !!int y\n", dups(6));
    let mut it = Deserializer::from_str_with(&src, &s);
    let first = it.next().unwrap();
    let _ = Value::deserialize(first).unwrap();
    let second = it.next().unwrap();
    let e = Value::deserialize(second).unwrap_err();
    assert_eq!(e.code(), code::INVALID_SCALAR_FOR_TAG);
    let w = e.warnings();
    assert_eq!(w.len(), 3);
    assert_eq!(w[2].message, "4 more diagnostics suppressed");
    let doc = Stream::parse_recovering_with(src.as_str(), &s);
    assert_eq!(keys(&w[..2]), keys(&doc.diagnostics()[..2]));
}

/// Paths that discard warnings build at most the first
/// 16: an error carries them, then a `Y2010` summary counting the rest;
/// the target's `Deserialize` runs once.
#[test]
fn discarding_paths_carry_sixteen_warnings_and_a_summary() {
    let s = last_wins(Limits::default());
    // Within 16: the same warnings as the keeping path.
    let src = format!("{}b: !!int x\n", dups(5));
    let a = oneyaml::from_str_with::<Value>(&src, &s).unwrap_err();
    let b = oneyaml::from_str_with_diagnostics::<Value>(&src, &s).unwrap_err();
    assert_eq!(a.to_string(), b.to_string());
    assert_eq!(keys(a.warnings()), keys(b.warnings()));
    assert_eq!(a.warnings().len(), 5);
    // Past 16: the first 16 and the summary.
    let src = format!("{}b: !!int x\n", dups(20));
    let full = oneyaml::from_str_with_diagnostics::<Value>(&src, &s).unwrap_err();
    assert_eq!(full.warnings().len(), 20);
    let doc = Document::parse_recovering_with(src.as_str(), &s).into_value();
    for e in [
        oneyaml::from_str_with::<Value>(&src, &s).unwrap_err(),
        oneyaml::from_slice_with::<Value>(src.as_bytes(), &s).unwrap_err(),
        doc.to_value().unwrap_err(),
        doc.deserialize::<Value>().unwrap_err(),
        doc.root().unwrap().deserialize::<Value>().unwrap_err(),
    ] {
        assert_eq!(e.to_string(), full.to_string());
        let w = e.warnings();
        assert_eq!(w.len(), 17);
        assert_eq!(keys(&w[..16]), keys(&full.warnings()[..16]));
        assert_eq!(w[16].message, "4 more diagnostics suppressed");
        assert_eq!(
            w[16].primary.span.start,
            full.warnings()[16].primary.span.start
        );
    }
    // A smaller `max_diagnostics` caps them first.
    let small = last_wins(Limits::default().max_diagnostics(3));
    let e = oneyaml::from_str_with::<Value>(&src, &small).unwrap_err();
    assert_eq!(e.warnings().len(), 4);
    assert_eq!(e.warnings()[3].message, "17 more diagnostics suppressed");
    // `Deserialize` runs once, even on the error path.
    struct Once;
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    impl<'de> Deserialize<'de> for Once {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Value::deserialize(d).map(|_| Once)
        }
    }
    assert!(oneyaml::from_str_with::<Once>(&src, &s).is_err());
    assert_eq!(CALLS.load(std::sync::atomic::Ordering::SeqCst), 1);
    // Without warnings, nothing to carry; success drops them.
    let e = oneyaml::from_str_with::<u8>("x", &s).unwrap_err();
    assert!(e.warnings().is_empty());
    assert_eq!(
        oneyaml::from_str_with::<Value>(&dups(5), &s).unwrap()["a"],
        Value::from(4)
    );
}

/// The re-parse a document's diagnostics after edits must equal.
fn reparse(text: &str, s: &Settings) -> Vec<Diagnostic> {
    Document::parse_recovering_with(text, s)
        .value()
        .diagnostics()
}

/// Edits at the cap: `diagnostics()` equals a re-parse of the edited text
/// with the same limits, for edits that keep, add, and remove diagnostics,
/// for meaning warnings and for syntax warnings the tree dropped.
#[test]
fn edits_at_the_cap_match_a_reparse() {
    let cases = [
        (dups(8), last_wins(Limits::default().max_diagnostics(4))),
        (
            format!("z: 1\n{}", dups(8)),
            last_wins(Limits::default().max_diagnostics(4)),
        ),
        (
            format!("z: 1\nl:\n{}", colons(8).replace("- ", "  - ")),
            lenient(Limits::default().max_diagnostics(4)),
        ),
    ];
    for (src, s) in cases {
        let mut doc = Document::parse_recovering_with(src.as_str(), &s).into_value();
        assert_eq!(keys(&doc.diagnostics()), keys(&reparse(&src, &s)));
        let root = doc.root().unwrap().as_mapping().unwrap();
        let last = root
            .iter()
            .last()
            .unwrap()
            .0
            .as_scalar()
            .unwrap()
            .value()
            .into_owned();
        // An edit that changes no diagnostic.
        doc.set_path(&path![last.as_str()], 7).unwrap();
        assert_eq!(
            keys(&doc.diagnostics()),
            keys(&reparse(&doc.to_string(), &s)),
            "{src:?}"
        );
        // One that adds an entry, and one that removes one.
        doc.root_mut().unwrap().insert("new", 1).unwrap();
        assert_eq!(
            keys(&doc.diagnostics()),
            keys(&reparse(&doc.to_string(), &s)),
            "{src:?}"
        );
        let _ = doc.root_mut().unwrap().remove("new").unwrap();
        assert_eq!(
            keys(&doc.diagnostics()),
            keys(&reparse(&doc.to_string(), &s)),
            "{src:?}"
        );
        // One that removes diagnostics in front of the boundary (kept and
        // suppressed ones alike).
        let first = doc
            .root()
            .unwrap()
            .as_mapping()
            .unwrap()
            .iter()
            .next()
            .unwrap()
            .0;
        let first = first.as_scalar().unwrap().value().into_owned();
        doc.set_path(&path![first.as_str()], [1, 2]).unwrap();
        assert_eq!(
            keys(&doc.diagnostics()),
            keys(&reparse(&doc.to_string(), &s)),
            "{src:?}"
        );
    }
}

/// Edits at the cap in a stream: every document's view equals the
/// re-parsed stream's, before and after an edit that removes a diagnostic
/// in front of the boundary (moving it).
#[test]
fn stream_edits_at_the_cap_match_a_reparse() {
    let s = last_wins(Limits::default().max_diagnostics(3));
    let src = format!("{}---\n{}---\n{}", dups(2), dups(2), dups(2));
    let mut st = Stream::parse_recovering_with(src.as_str(), &s).into_value();
    let views =
        |st: &Stream| -> Vec<Vec<_>> { st.documents().map(|d| keys(&d.diagnostics())).collect() };
    let again = |st: &Stream| {
        let p = Stream::parse_recovering_with(st.to_string(), &s);
        let v = views(p.value());
        (v, keys(p.diagnostics()))
    };
    assert_eq!((views(&st), keys(&st.diagnostics())), again(&st));
    // Doc 0 loses its duplicates: the boundary moves into doc 2.
    st.document_mut(0)
        .unwrap()
        .set_root(std::collections::BTreeMap::from([("a", 1)]))
        .unwrap();
    assert_eq!((views(&st), keys(&st.diagnostics())), again(&st));
    assert!(
        views(&st)[2]
            .iter()
            .any(|k| k.0 == code::DIAGNOSTICS_SUPPRESSED)
    );
    // An edit in doc 1 that keeps its diagnostics.
    st.document_mut(1)
        .unwrap()
        .root_mut()
        .unwrap()
        .insert("b", 2)
        .unwrap();
    assert_eq!((views(&st), keys(&st.diagnostics())), again(&st));
}

/// Deep nesting with many diagnostics never crashes on a small stack.
#[test]
fn capped_lists_on_a_small_stack() {
    let h = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let src = format!("{}{}", "[".repeat(10_000), colons(10));
            let p = Document::parse_recovering_with(
                src.as_str(),
                &lenient(Limits::unlimited().max_diagnostics(2)),
            );
            p.diagnostics().len()
        })
        .unwrap();
    assert!(h.join().unwrap() <= 4);
}

#[test]
fn duplicate_key_policy_error_is_unchanged() {
    let s = Settings::new()
        .duplicate_keys(DuplicateKeys::Error)
        .limits(Limits::default().max_diagnostics(0));
    let e = Document::parse_with("a: 1\na: 2\n", &s).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    assert!(e.warnings().is_empty());
}
