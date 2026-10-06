//! The peak memory of the
//! diagnostics triggers of a security review is bounded by
//! `max_diagnostics` × a diagnostic's size plus O(input), at 1× and 10×,
//! under `Limits::default()` and `Limits::untrusted()`; and paths that
//! discard warnings build at most 16. Without the cap, duplicate-key warnings
//! (`Compat::serde_yaml()`) took 2.4 GB for 15 MB of input, and so did
//! `from_str`, which drops them.
//!
//! Measured per thread with `allocation-counter` (an `unsafe impl
//! GlobalAlloc` dev-dependency, so here: only this crate may use one, as
//! `ser_allocations.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Compat, Document, Limits, Settings, Stream, Value, code};

fn dups(n: usize) -> String {
    let mut s = String::from("a: 0\n");
    for i in 0..n {
        s.push_str(&format!("a: {i}\n"));
    }
    s
}

fn colons(n: usize) -> String {
    "- a: b: c\n".repeat(n)
}

fn orphans(n: usize) -> String {
    "%YAML 1.2\n...\n".repeat(n)
}

fn peak(f: impl FnOnce()) -> u64 {
    allocation_counter::measure(f).bytes_max
}

/// What a kept diagnostic may cost, generously (the measured cost is
/// about 150–800 bytes, as the review found).
const PER_DIAGNOSTIC: u64 = 2048;

/// Bytes per input byte the readers may hold besides diagnostics with
/// texts, measured at 1× and 10× (release):
///
/// * the syntax tree (up to ~17× for these dense shapes);
/// * every diagnostic past the cap is kept in the tree as a
///   skeleton (code, severity, spans; 32 bytes with its element
///   index), so meaning never depends on the cap; ~25× in all for
///   `- a: b: c` lines (one per 10 bytes);
/// * invalid bytes: the runs are kept compactly (12 bytes each
///   and one pool of their bytes), ~17-21× in all for `\x80a`
///   repeated (one run per 2 bytes); each run kept its own bytes before,
///   ~86×, which this bound fails.
const PER_INPUT_BYTE: u64 = 48;

/// The same for dense reserved directives waiting for a `---` (`%A` lines: a
/// warning per 3 bytes). They are written into the arena as they come (not
/// buffered), each warning in 32 bytes; measured 31-37x (85-102x when buffered,
/// 185x with unordered collectors, which this bound fails).
const BUFFERED_PER_INPUT_BYTE: u64 = 48;

/// The same for orphan directives (a `%YAML` line and `...` each, with an
/// error): ~11-12x; buffered, 29-46x, which
/// this bound fails.
const ORPHANS_PER_INPUT_BYTE: u64 = 20;

#[test]
fn triggers_peak_within_the_cap_plus_the_input() {
    for (name, profile) in [
        ("untrusted", Limits::untrusted()),
        ("default", Limits::default()),
    ] {
        let limit = profile.get_max_diagnostics() as u64;
        for scale in [1usize, 10] {
            let n = 30_000 * scale;
            let check_with = |what: &str, input: usize, found: u64, per: u64, bytes: u64| {
                let kept = found.min(limit) + 2;
                let bound = per * input as u64 + PER_DIAGNOSTIC * kept;
                eprintln!(
                    "{name} {scale}x {what}: {} KB input, peak {} MB, {}x (bound {} MB)",
                    input / 1000,
                    bytes / 1_000_000,
                    bytes / input as u64,
                    bound / 1_000_000
                );
                assert!(
                    bytes <= bound,
                    "{name} {scale}x {what}: peak {bytes} over {bound}"
                );
            };
            let check = |what: &str, input: usize, bytes: u64| {
                check_with(what, input, n as u64, PER_INPUT_BYTE, bytes);
            };
            // Duplicate keys under LastWins (Compat::serde_yaml()).
            let src = dups(n);
            let s = Compat::serde_yaml().limits(profile.clone());
            check(
                "dups from_str",
                src.len(),
                peak(|| {
                    oneyaml::from_str_with::<Value>(&src, &s).unwrap();
                }),
            );
            check(
                "dups from_str_with_diagnostics",
                src.len(),
                peak(|| {
                    oneyaml::from_str_with_diagnostics::<Value>(&src, &s).unwrap();
                }),
            );
            check(
                "dups Document::parse",
                src.len(),
                peak(|| {
                    Document::parse_with(src.as_str(), &s).unwrap();
                }),
            );
            // Lenient inline colons.
            let src = colons(n);
            let s = Settings::new()
                .lenient_inline_colons(true)
                .limits(profile.clone());
            // (`IgnoredAny`: a `Value` of 30,000 small mappings is itself
            // ~100× its text, which is not what is measured here.)
            check(
                "colons from_str",
                src.len(),
                peak(|| {
                    oneyaml::from_str_with::<serde::de::IgnoredAny>(&src, &s).unwrap();
                }),
            );
            check(
                "colons from_str_with_diagnostics",
                src.len(),
                peak(|| {
                    oneyaml::from_str_with_diagnostics::<serde::de::IgnoredAny>(&src, &s).unwrap();
                }),
            );
            check(
                "colons Document::parse_recovering",
                src.len(),
                peak(|| {
                    let p = Document::parse_recovering_with(src.as_str(), &s);
                    assert!(!p.has_errors());
                }),
            );
            // Orphan directives (an error each), recovering.
            let src = orphans(n);
            let s = Settings::new().limits(profile.clone());
            check_with(
                "orphans Stream::parse_recovering",
                src.len(),
                n as u64,
                ORPHANS_PER_INPUT_BYTE,
                peak(|| {
                    let p = Stream::parse_recovering_with(src.as_str(), &s);
                    if n as u64 > limit {
                        assert!(
                            p.diagnostics()
                                .iter()
                                .any(|d| d.code == code::DIAGNOSTICS_SUPPRESSED)
                        );
                    }
                }),
            );
            // Dense warnings: a reserved directive per line,
            // all waiting for the one `---` (the review measured 2.65 GB,
            // 185×, for 15 MB under `untrusted()` before the compact
            // skeletons).
            for (what, line) in [("%A", "%A\n"), ("%FOO bar", "%FOO bar\n")] {
                let src = format!("{}--- a\n", line.repeat(n * 3));
                check_with(
                    &format!("{what} Document::parse_recovering"),
                    src.len(),
                    3 * n as u64,
                    BUFFERED_PER_INPUT_BYTE,
                    peak(|| {
                        let p = Document::parse_recovering_with(src.as_str(), &s);
                        assert!(!p.has_errors());
                    }),
                );
                check_with(
                    &format!("{what} from_str"),
                    src.len(),
                    3 * n as u64,
                    PER_INPUT_BYTE,
                    peak(|| {
                        oneyaml::from_str_with::<Value>(&src, &s).unwrap();
                    }),
                );
            }
            // Invalid bytes: decoding builds at most the cap's
            // diagnostics; the runs are kept whole (and
            // compactly). One run (and diagnostic) per 2 bytes.
            let bytes = b"\x80a".repeat(n * 7);
            check(
                "invalid bytes from_slice",
                bytes.len(),
                peak(|| {
                    assert!(oneyaml::from_slice_with::<Value>(&bytes, &s).is_err());
                }),
            );
            check_with(
                "invalid bytes Document::from_slice_recovering",
                bytes.len(),
                7 * n as u64,
                PER_INPUT_BYTE,
                peak(|| {
                    let p = Document::from_slice_recovering(&bytes, &s);
                    assert!(p.has_errors());
                }),
            );
            check_with(
                "orphans Document::parse_recovering",
                src.len(),
                n as u64,
                ORPHANS_PER_INPUT_BYTE,
                peak(|| {
                    let _ = Document::parse_recovering_with(src.as_str(), &s);
                }),
            );
        }
    }
}

/// Allocations made by `f` on this thread (after one warm-up run).
fn allocations(f: impl Fn()) -> u64 {
    f();
    allocation_counter::measure(f).count_total
}

/// Paths that discard warnings build at most 16: reading
/// 10,000 tolerated duplicates with `from_str` allocates about as much as
/// reading 20. Building each warning allocates several times (message,
/// label, help), so a regression is tens of thousands of allocations.
#[test]
fn discarding_paths_build_at_most_sixteen_warnings() {
    let s = Compat::serde_yaml();
    let few = dups(20);
    let many = dups(10_000);
    let typed = |src: &str| {
        let src = src.to_owned();
        let s = s.clone();
        move || {
            std::hint::black_box(
                oneyaml::from_str_with::<serde::de::IgnoredAny>(&src, &s).unwrap(),
            );
        }
    };
    let (a, b) = (allocations(typed(&few)), allocations(typed(&many)));
    eprintln!("from_str (IgnoredAny): {a} allocations for 20 duplicates, {b} for 10,000");
    assert!(b <= a + 64, "from_str builds warnings: {a} vs {b}");
    // `Document::to_value` discards them too (the tree's are not copied).
    let doc_few = Document::parse_with(few.as_str(), &s).unwrap();
    let doc_many = Document::parse_with(many.as_str(), &s).unwrap();
    let (c, d) = (
        allocations(|| {
            std::hint::black_box(doc_few.deserialize::<serde::de::IgnoredAny>().unwrap());
        }),
        allocations(|| {
            std::hint::black_box(doc_many.deserialize::<serde::de::IgnoredAny>().unwrap());
        }),
    );
    eprintln!("Document::deserialize: {c} allocations for 20 duplicates, {d} for 10,000");
    assert!(
        d <= c + 64,
        "Document::deserialize builds warnings: {c} vs {d}"
    );
}
