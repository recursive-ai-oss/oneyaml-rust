//! The pull parser. A consumer that drops each event as it
//! reads it holds parser memory bounded by nesting depth plus the implicit-
//! key lookahead, never by the input's length; the collecting `parse` is
//! the same stream; the Y1000 wording follows `honor_version_directive`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use oneyaml_core::code::YAML_VERSION_1_1;
use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::event::{EventKind, check_contract};
use oneyaml_syntax::{Events, Sourced, events, events_with, parse, parse_with};

/// The largest `retained_capacity` seen while pulling `iter` to the end and
/// dropping every event, plus the number of events pulled.
fn peak_while_dropping(mut iter: Events<'_>) -> (usize, usize) {
    let mut peak = iter.retained_capacity();
    let mut count = 0usize;
    while let Some(event) = iter.next() {
        drop(event);
        count += 1;
        peak = peak.max(iter.retained_capacity());
    }
    (peak, count)
}

/// The implicit-key window in bytes: 1024 characters of at most 4 bytes
/// each (productions 154-155).
const WINDOW: usize = 4 * 1024;
/// Held items per byte of a single-line flow collection, at most: an entry
/// of `k` bytes plus its `,` yields at most `k + 1` byte-covering items and
/// 4 zero-width ones (pair start and end, empty key and value), so
/// `(k + 5) / (k + 1) <= 3`, reached by `[:,:,…]` and `[?,?,…]`.
const ITEMS_PER_BYTE: usize = 3;
/// A bound that does not depend on input length, derived rather than
/// measured (see *Streaming and buffering* in `parser/mod.rs`). The flow
/// hold holds at most `ITEMS_PER_BYTE * WINDOW` = 12,288 items and its
/// pair reservations at most `WINDOW / 2`; when the root is decided (or an
/// error ends it) the hold drains into the output queue, which then holds
/// as many plus one batch and the depth-bounded ends. A growing buffer's
/// capacity stays below twice its peak length (amortised doubling; `std`
/// rounds `VecDeque` to powers of two, so hold and queue both reach 16,384
/// today, measured peak ≈32,784). Nothing else scales with the input. The
/// 1024 covers the batch, the depth-bounded buffers of these shallow
/// shapes, and the `%TAG` handles they declare (none). About 65% above the
/// measured peak.
const FLAT_BOUND: usize = 2 * (2 * ITEMS_PER_BYTE * WINDOW + WINDOW / 2) + 1024;

/// Input shapes that scale with `n` at constant nesting depth.
fn shapes(n: usize) -> Vec<(&'static str, String)> {
    let mut v = Vec::new();
    v.push((
        "block mapping with comments",
        (0..n)
            .map(|i| format!("# c{i}\nk{i}: v{i} # t\n"))
            .collect(),
    ));
    v.push((
        "nested block sequences",
        (0..n)
            .map(|i| format!("- - a{i}\n  - {{x: [1, 2]}}\n  - |\n    lit\n    eral\n"))
            .collect(),
    ));
    v.push((
        "multi-line plain and quoted scalars",
        (0..n)
            .map(|i| format!("- a{i}\n  b c\n- 'q\n  r'\n- \"d\\t\n  e\"\n"))
            .collect(),
    ));
    v.push((
        "multi-line flow sequence",
        format!(
            "[{}]\n",
            (0..n).map(|i| format!("a{i},\n ")).collect::<String>()
        ),
    ));
    v.push((
        "multi-line flow mapping as a block value",
        format!(
            "k: {{{}}}\n",
            (0..n).map(|i| format!("a{i}: [b],\n ")).collect::<String>()
        ),
    ));
    v.push((
        "single-line flow sequence of pairs",
        format!(
            "[{}]\n",
            (0..n).map(|i| format!("a{i}: b, ")).collect::<String>()
        ),
    ));
    v.push((
        "single-line flow sequence as a block value",
        format!(
            "- k: [{}]\n",
            (0..n).map(|i| format!("{{a{i}: b}}, ")).collect::<String>()
        ),
    ));
    v.push((
        "many documents",
        (0..n)
            .map(|i| format!("--- &a{i} !t\nk: v\n...\n"))
            .collect(),
    ));
    v.push((
        "long comment run and blank lines",
        format!(
            "{}a: 1\n{}",
            "# comment\n\n".repeat(n),
            "   \n\t\n".repeat(n)
        ),
    ));
    v.push((
        "long block scalar",
        format!("k: |\n{}", "  line of text\n".repeat(n)),
    ));
    v.push((
        "error recovery skipping many lines",
        format!("a: b: c\n{}---\nok\n", "  skipped: [ line\n".repeat(n)),
    ));
    v.push((
        "single-line flow sequence of empty flow sequences",
        format!("[{}]\n", "[[],[]],".repeat(n)),
    ));
    v.push((
        "unterminated flow collection",
        format!("[{}", "a, ".repeat(n)),
    ));
    v.push((
        "single-line flow sequence of empty pairs (3 items per byte)",
        format!("[{}]\n", ":,".repeat(n)),
    ));
    // Errors after many white lines: the lines before the error are one
    // lazily lexed trivia run, not an event pair each.
    v.push((
        "block scalar: over-indented leading lines (Y0021)",
        format!("k: |\n{}  x\n", "     \n".repeat(n)),
    ));
    v.push((
        "block scalar: empty lines then a tab",
        format!("k: |\n{}\t\n", "\n".repeat(n)),
    ));
    v.push((
        "block scalar root: white lines then less indentation",
        format!("--- |\n{} x\n", "   \n".repeat(n)),
    ));
    v.push((
        "unterminated multi-line double-quoted scalar",
        format!("\"a\n{}", " b\n".repeat(n)),
    ));
    v.push((
        "multi-line flow collection as a key",
        format!("[a,\n{}]: v\n", " b,\n".repeat(n)),
    ));
    v.push((
        "multi-line flow collection in a key slot without `:`",
        format!("'k' : ---\n[\n{}  'x' : value]\n", "  a,\n".repeat(n)),
    ));
    v.push((
        "multi-line plain value then an inline colon",
        format!("k: a\n{}  c: d\n", "  b\n".repeat(n)),
    ));
    v.push((
        "reserved directives",
        format!("{}---\na\n", "%FOO bar\n".repeat(n)),
    ));
    v
}

#[test]
fn dropping_consumer_memory_is_independent_of_input_length() {
    // `small` is already past the 4096-byte window on every single-line
    // shape, so the peak must not move between 10x and 100x its size.
    let small = shapes(2_000);
    let large = shapes(20_000);
    for ((name, s), (_, l)) in small.iter().zip(&large) {
        let (peak_s, count_s) = peak_while_dropping(events(s));
        let (peak_l, count_l) = peak_while_dropping(events(l));
        eprintln!(
            "{name}: {} bytes -> peak {peak_s}; {} bytes -> peak {peak_l}",
            s.len(),
            l.len()
        );
        assert!(l.len() > 5 * s.len(), "{name}: the input scaled");
        assert!(count_l >= count_s, "{name}: the stream scaled");
        assert!(
            peak_l <= FLAT_BOUND,
            "{name}: peak {peak_l} elements for {} bytes exceeds {FLAT_BOUND}",
            l.len()
        );
        // Beyond the lookahead window nothing grows: 10x the input keeps
        // the same peak.
        assert!(
            peak_l <= peak_s,
            "{name}: peak grew from {peak_s} to {peak_l}"
        );
    }
}

#[test]
fn streaming_and_collecting_are_the_same_stream() {
    let settings = [
        Settings::new(),
        Settings::new().lenient_inline_colons(true),
        Settings::new().limits(Limits::untrusted()),
    ];
    for settings in &settings {
        for (name, src) in shapes(50) {
            let pulled: Vec<_> = events_with(&src, settings)
                .map(Sourced::into_event)
                .collect();
            let collected = parse_with(&src, settings);
            assert_eq!(pulled, collected.events(), "{name}");
            check_contract(&src, &pulled).unwrap_or_else(|e| panic!("{name}: {e}"));
            // Every pulled event is paired with the source it came from.
            assert!(events_with(&src, settings).all(|e| std::ptr::eq(e.source(), src.as_str())));
        }
    }
    let src = "a: [b, {c: d}]\n";
    let pulled: Vec<_> = events(src).map(Sourced::into_event).collect();
    assert_eq!(pulled, parse(src).into_events());
}

#[test]
fn iterator_is_fused() {
    let mut it = events("a\n");
    let last = it.by_ref().last().unwrap();
    assert!(matches!(last.kind(), EventKind::StreamEnd));
    assert!(it.next().is_none());
    assert!(it.next().is_none());
    // The empty input still yields a stream.
    let all: Vec<_> = events("").collect();
    assert!(matches!(
        all.first().unwrap().kind(),
        EventKind::StreamStart
    ));
    assert!(matches!(all.last().unwrap().kind(), EventKind::StreamEnd));
}

/// Pulling a million-level document on a small stack, dropping
/// each event, holds memory linear in depth (here: the depth) and no more.
#[test]
fn deep_input_streams_on_a_small_stack() {
    const DEPTH: usize = 1_000_000;
    let inputs = [
        format!("{}a\n", "- ".repeat(DEPTH)),
        format!("{}{}", "[".repeat(DEPTH), "]".repeat(DEPTH)),
        format!("k: {}x{}\n", "{a: ".repeat(DEPTH), "}".repeat(DEPTH)),
    ];
    for src in inputs {
        let (peak, starts) = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let settings = Settings::new().limits(Limits::unlimited());
                let mut iter = events_with(&src, &settings);
                let mut peak = 0;
                let mut starts = 0usize;
                while let Some(e) = iter.next() {
                    if matches!(
                        e.kind(),
                        EventKind::SequenceStart { .. } | EventKind::MappingStart { .. }
                    ) {
                        starts += 1;
                    }
                    assert!(!e.is_error());
                    peak = peak.max(iter.retained_capacity());
                }
                (peak, starts)
            })
            .unwrap()
            .join()
            .expect("no panic or stack overflow");
        assert!(starts >= DEPTH);
        // Stacks and owed ends are O(depth): a small constant per level
        // (amortised `Vec`/`VecDeque` doubling included).
        assert!(peak <= 8 * DEPTH, "peak {peak} for depth {DEPTH}");
    }
}

/// The `%YAML 1.1` warning (Y1000) says what happens to the document.
#[test]
fn yaml_1_1_warning_reflects_honor_version_directive() {
    let src = "%YAML 1.1\n---\na\n";
    let message = |settings: &Settings| {
        let events = parse_with(src, settings);
        check_contract(src, &events).unwrap();
        let warnings: Vec<_> = events.iter().filter(|e| e.is_warning()).collect();
        assert_eq!(warnings.len(), 1);
        let d = warnings[0].diagnostic().unwrap();
        assert_eq!(d.code, YAML_VERSION_1_1);
        d.message.clone()
    };
    let default = message(&Settings::new());
    assert!(default.contains("parsed as YAML 1.2"), "{default}");
    let honored = message(&Settings::new().honor_version_directive(true));
    assert!(!honored.contains("parsed as YAML 1.2"), "{honored}");
    assert!(honored.contains("honor_version_directive"), "{honored}");
    assert!(honored.contains("1.1 preset"), "{honored}");
}
