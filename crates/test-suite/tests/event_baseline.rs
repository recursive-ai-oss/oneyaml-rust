//! One-interpretation guard for the diagnostic polish: the parser's
//! polish adds secondary labels, help, and fixes to diagnostics and must
//! change nothing else. Over every yaml-test-suite case (valid and
//! invalid) and the polish inputs below, the event stream (every kind,
//! node info, span, and each diagnostic's code, severity, message, and
//! primary span) hashes to the baseline recorded from commit `ffa41db`
//! (before the polish), in `event-baseline.txt`.
//!
//! The baseline is data, not a golden to re-bless: it was produced by this very
//! file run with `ONEYAML_BLESS=1` in a `git archive` export of that commit
//! (the event API is the same there). A mismatch means the parser changed
//! events, codes, messages or spans.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use oneyaml_syntax::event::EventKind;

const BASELINE: &str = "event-baseline.txt";

/// Inputs the polish touches, beyond the suite.
const EXTRA: &[&str] = &[
    "a: 1\nb:2\n",
    "key: 'it's'\n",
    "key: \"say \"hi\"\"\n",
    "key: \"a\"#c\n",
    "k: 'it's'\n  more\n",
    "k: \"a\"b\\\"\n",
    "k: \"a\"b\\q\"\n",
    "path: \"C:\\path\"\n",
    "path: \"C:\\data\\new\"\n",
    "q: \"it\\'s\"\n",
    "a: @foo\n",
    "a: @foo\n  bar\n",
    "a: `cmd` # c\n",
    "@k: v\n",
    "- &a !t \n  !t @|\nk",
    "a: [1, 2\n",
    "a: [1, {b: 2\n",
    "a: [1, 2\nb: 3\n",
    "? \r\n{&a ",
    "a: \"abc\nb: 1\n",
    "a: \"abc\\\nb: 1\n",
    "a: 'abc # note\n",
    "k: [\"C:\\path\", b]\n",
    "[@x]\n",
    "{a: @x, b: `y`}\n",
    "[@x\n]\n",
    "a:\n\t- b\n",
    // Long properties and aliases: span bugs that only bite
    // past ~40 bytes must be caught.
    "x: &aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa !tag:example.com,2026:long-tag-name-here v\ny: *aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nz: @bad\n",
    // Byte order marks where a document prefix may start stay
    // valid and unchanged (a misplaced mark became a new error, so inputs
    // with one are not in the baseline).
    "\u{feff}a: 1\n",
    "\u{feff}--- a\n",
    "\u{feff}%YAML 1.2\n--- a\n",
    "\u{feff}# c\nk:\n  v\n",
    "a\n...\n\u{feff}--- b\n...\n\u{feff}c\n",
    "a\n...\n\u{feff}# c\n\u{feff}%YAML 1.2\n---\nb\n",
    "\u{feff}\u{feff}a\n",
    "\u{feff}",
    // Production 211: a mark at the start of a line after a complete document
    // starts a document prefix. (the baseline revision rejected these; they
    // record the valid streams, so they are recorded at a later tree, see
    // `LATER`.)
    "--- a\n\u{feff}--- b\n",
    "a\n\u{feff}--- b\n",
    "--- a\n\u{feff}# c\n--- b\n",
    "a\n\u{feff}",
    // Production 211: `...` after such a prefix is `l-document-suffix`, after
    // which any document may follow. (the baseline revision and later trees
    // rejected both; recorded later still, see `LATER`.)
    "a\n\u{feff}...\n",
    "a\n\u{feff}...\nb\n",
];

/// Extras whose baseline lines were recorded later, because
/// the baseline revision parsed them wrongly (spec-valid input it rejected).
/// Their lines were produced by this file with `ONEYAML_BLESS=1` at the
/// tree that fixed them (`extra/40`, `extra/41`: a later tree, appended by
/// hand); every other line is from the baseline revision.
const LATER: &[&str] = &[
    "extra/36", "extra/37", "extra/38", "extra/39", "extra/40", "extra/41",
];

/// The stream of `src` in a stable text form, without what the polish may
/// add (secondary labels, help, suggestions).
fn dump(src: &str) -> String {
    let mut out = String::new();
    for e in oneyaml_syntax::parse(src).events() {
        let span = e.span();
        match e.kind() {
            EventKind::Error(d) | EventKind::Warning(d) => {
                let _ = writeln!(
                    out,
                    "!{} {:?} {:?} {:?} @{:?}",
                    d.code, d.severity, d.message, d.primary.span, span
                );
            }
            kind => {
                let _ = writeln!(out, "{kind:?} @{span:?}");
            }
        }
    }
    out
}

/// FNV-1a, 64 bits.
fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn cases() -> Vec<(String, String)> {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/yaml-test-suite");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|f| f == "in.yaml") {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let id = path
                    .parent()
                    .unwrap()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((id, src));
            }
        }
    }
    for (k, src) in EXTRA.iter().enumerate() {
        out.push((format!("extra/{k:02}"), (*src).to_owned()));
    }
    out.sort();
    out
}

#[test]
fn events_codes_and_spans_match_the_pre_polish_baseline() {
    let mut now = BTreeMap::new();
    let mut text = String::from(
        "# Event-stream hashes per case, recorded at ffa41db (see tests/event_baseline.rs).\n",
    );
    for (id, src) in cases() {
        let d = dump(&src);
        let _ = writeln!(text, "{id} {:016x} {}", fnv(&d), d.lines().count());
        now.insert(id, (fnv(&d), d));
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE);
    if std::env::var_os("ONEYAML_BLESS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let baseline = std::fs::read_to_string(&path).expect(BASELINE);
    let mut seen = 0;
    let mut diffs = Vec::new();
    for line in baseline.lines().filter(|l| !l.starts_with('#')) {
        let mut parts = line.split(' ');
        let (id, hash) = (parts.next().unwrap(), parts.next().unwrap());
        let hash = u64::from_str_radix(hash, 16).unwrap();
        seen += 1;
        match now.get(id) {
            Some((h, _)) if *h == hash => {}
            Some((_, d)) => diffs.push(format!("{id}: events differ; now:\n{d}")),
            None => diffs.push(format!("{id}: no longer a case")),
        }
    }
    assert_eq!(seen, now.len(), "the baseline covers every case");
    for id in LATER {
        assert!(now.contains_key(*id), "{id}: `LATER` names no case");
    }
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}
