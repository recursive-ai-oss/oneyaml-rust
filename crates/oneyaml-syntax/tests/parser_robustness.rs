//! Robustness (no unsafe, no panic, no recursion): deep nesting on a small
//! stack, generated garbage, and truncated inputs never panic, always
//! terminate, and always yield a contract-conforming stream.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use oneyaml_core::code::DEPTH_LIMIT_EXCEEDED;
use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::event::{EventKind, check_contract};
use oneyaml_syntax::{parse, parse_with};

/// Parses `src` and checks the contract on a thread with a small stack, so
/// any recursion on input depth would overflow. Runs under
/// `Limits::unlimited()`: these tests are about stack safety, not the
/// `max_depth` resource limit (see `default_limits_stop_deep_input`).
fn on_small_stack(src: String) -> (usize, usize, bool) {
    on_small_stack_with(src, Settings::new().limits(Limits::unlimited())).0
}

/// [`on_small_stack`] under `settings`; also returns the first error code.
fn on_small_stack_with(src: String, settings: Settings) -> ((usize, usize, bool), Option<u16>) {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let events = parse_with(&src, &settings);
            check_contract(&src, &events).expect("contract");
            let starts = events
                .iter()
                .filter(|e| {
                    matches!(
                        e.kind(),
                        EventKind::SequenceStart { .. } | EventKind::MappingStart { .. }
                    )
                })
                .count();
            let has_error = events.iter().any(|e| e.is_error());
            let first = events
                .iter()
                .find(|e| e.is_error())
                .and_then(|e| e.diagnostic())
                .map(|d| d.code.get());
            ((events.len(), starts, has_error), first)
        })
        .unwrap()
        .join()
        .expect("no panic or stack overflow")
}

/// One million levels on a small stack.
const DEPTH: usize = 1_000_000;

#[test]
fn deep_compact_sequences() {
    let (_, starts, err) = on_small_stack(format!("{}a\n", "- ".repeat(DEPTH)));
    assert_eq!((starts, err), (DEPTH, false));
}

#[test]
fn deep_explicit_keys() {
    let (_, starts, err) = on_small_stack(format!("{}a\n", "? ".repeat(DEPTH)));
    assert_eq!((starts, err), (DEPTH, false));
}

#[test]
fn deep_compact_mappings() {
    // Compact mappings nest under `- ` and `? ` (production 185), with a
    // `:` value at the innermost level.
    let src = format!("{}k: v\n", "- ? ".repeat(DEPTH / 2));
    let (_, starts, err) = on_small_stack(src);
    assert_eq!((starts, err), (DEPTH + 1, false));
}

#[test]
fn deep_indented_mappings_close_at_once() {
    let depth = 2_000;
    let mut src = String::new();
    for d in 0..depth {
        src.push_str(&" ".repeat(d));
        src.push_str("k:\n");
    }
    src.push_str("end: 1\n");
    let (_, starts, err) = on_small_stack(src);
    assert_eq!((starts, err), (depth, false));
}

/// Under the default limits, the same million-level inputs
/// stop at `max_depth` with the limit code, on a small stack, and the
/// stream still conforms.
#[test]
fn default_limits_stop_deep_input() {
    let limit = Limits::default().get_max_depth();
    for src in [
        format!("{}a\n", "- ".repeat(DEPTH)),
        format!("{}a\n", "? ".repeat(DEPTH)),
        format!("{}k: v\n", "- ? ".repeat(DEPTH / 2)),
        format!("{}{}", "[".repeat(DEPTH), "]".repeat(DEPTH)),
        format!("{}x{}", "{a: ".repeat(DEPTH), "}".repeat(DEPTH)),
        format!("{}x{}", "[a: ".repeat(DEPTH), "]".repeat(DEPTH)),
        format!("k: {}{}\n", "[".repeat(DEPTH), "]".repeat(DEPTH)),
    ] {
        let ((_, starts, err), first) = on_small_stack_with(src, Settings::new());
        assert!(err);
        assert_eq!(first, Some(DEPTH_LIMIT_EXCEEDED.get()));
        assert_eq!(starts, limit, "exactly max_depth collections open");
    }
}

#[test]
fn deep_invalid_input_errors_cleanly() {
    for src in [
        "[".repeat(DEPTH),
        "{".repeat(DEPTH),
        "[a: ".repeat(DEPTH),
        "&a ".repeat(DEPTH),
        format!("{}@", "- ".repeat(DEPTH)),
        format!("{}|0\n", "? ".repeat(DEPTH)),
        format!("{}]]", "[".repeat(DEPTH)),
    ] {
        let (_, _, err) = on_small_stack(src);
        assert!(err);
    }
}

#[test]
fn deep_flow_collections() {
    let seqs = format!("{}{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
    assert_eq!(on_small_stack(seqs).1, DEPTH);
    let maps = format!("{}x{}", "{a: ".repeat(DEPTH), "}".repeat(DEPTH));
    assert_eq!(on_small_stack(maps).1, DEPTH);
    // Each `[a: ` opens a flow sequence and a flow pair.
    let pairs = format!("{}x{}", "[a: ".repeat(DEPTH), "]".repeat(DEPTH));
    let (_, starts, err) = on_small_stack(pairs);
    assert_eq!((starts, err), (2 * DEPTH, false));
    // A deep flow collection as a block value.
    let value = format!("k: {}{}\n", "[".repeat(DEPTH), "]".repeat(DEPTH));
    let (_, starts, err) = on_small_stack(value);
    assert_eq!((starts, err), (DEPTH + 1, false));
    // As a block key it must fit the 1024-character implicit-key limit
    // (production 155): 512 levels do, a million do not.
    let key = format!("{}{}: |\n  v\n", "[".repeat(512), "]".repeat(512));
    let (_, starts, err) = on_small_stack(key);
    assert_eq!((starts, err), (513, false));
    let key = format!("{}{}: v\n", "[".repeat(DEPTH), "]".repeat(DEPTH));
    assert!(
        on_small_stack(key).2,
        "a million-level key exceeds the limit"
    );
}

/// A tiny deterministic generator (no dev-dependency needed).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize
    }
}

const PIECES: &[&str] = &[
    "a",
    "b",
    " ",
    "  ",
    "\t",
    "\n",
    "\r\n",
    "\r",
    ":",
    ": ",
    "-",
    "- ",
    "?",
    "? ",
    "#",
    " #c",
    "&x",
    "*x",
    "!",
    "!!str",
    "!e!t",
    "!<u>",
    "'",
    "''",
    "\"",
    "\\",
    "\\n",
    "\\x4",
    "---",
    "...",
    "%YAML 1.2",
    "%TAG !e! p",
    "%",
    "|",
    ">",
    "[",
    "]",
    "{",
    "}",
    ",",
    "@",
    "`",
    "\u{feff}",
    "é",
    "\u{1}",
    "\u{a0}",
    "k: v",
    "\n  ",
    "\n- ",
    "\n? ",
];

#[test]
fn generated_garbage_conforms() {
    let mut rng = Lcg(0x5eed);
    for _ in 0..20_000 {
        let len = rng.next() % 24;
        let src: String = (0..len)
            .map(|_| PIECES[rng.next() % PIECES.len()])
            .collect();
        let events = parse(&src);
        if let Err(err) = check_contract(&src, &events) {
            panic!("{src:?}: {err}");
        }
    }
}

#[test]
fn every_truncation_conforms() {
    let docs = [
        "%YAML 1.2\n%TAG !e! tag:e,2000:\n--- !e!a &x\nk: 'v''w'\n? - \"q\\t\n  r\"\n: *x\n...\n",
        "a:\n  - b\n  -\n    c: d # e\n  - &y !!str\nf: |\n  g\n",
        "- - x\n  - 'y\n\n    z'\n- ? k\n  : v\n#end\r\n",
    ];
    for doc in docs {
        for (i, _) in doc.char_indices().chain([(doc.len(), ' ')]) {
            let src = &doc[..i];
            let events = parse(src);
            if let Err(err) = check_contract(src, &events) {
                panic!("{src:?}: {err}");
            }
        }
    }
}

/// The settings the parser reads: the leniency and tight limits, so the
/// limit and leniency paths meet every kind of garbage (C6 R1).
fn tight_settings() -> [Settings; 3] {
    let tight = Limits::new()
        .max_depth(2)
        .max_scalar_len(3)
        .max_document_size(40)
        .max_documents(2);
    [
        Settings::new().lenient_inline_colons(true),
        Settings::new().limits(tight.clone()),
        Settings::new().lenient_inline_colons(true).limits(tight),
    ]
}

#[test]
fn generated_garbage_conforms_under_other_settings() {
    for settings in tight_settings() {
        let mut rng = Lcg(0xfeed);
        for _ in 0..20_000 {
            let len = rng.next() % 24;
            let src: String = (0..len)
                .map(|_| PIECES[rng.next() % PIECES.len()])
                .collect();
            let events = parse_with(&src, &settings);
            if let Err(err) = check_contract(&src, &events) {
                panic!("{src:?} under {settings:?}: {err}");
            }
        }
    }
}

#[test]
fn every_truncation_conforms_under_other_settings() {
    let docs = [
        "a: b: c\n- x: y: z # c\nk: &a !!str v: w\n  more\n---\n[a, [b, [c]]]: {d: [e]}\n",
        "%YAML 1.2\n---\nk: 'long value'\n...\n--- |\n  text\n--- \"q\\tr\"\n",
    ];
    for settings in tight_settings() {
        for doc in docs {
            for (i, _) in doc.char_indices().chain([(doc.len(), ' ')]) {
                let src = &doc[..i];
                let events = parse_with(src, &settings);
                if let Err(err) = check_contract(src, &events) {
                    panic!("{src:?} under {settings:?}: {err}");
                }
            }
        }
    }
}
