//! The first error a strict parse fails with is the
//! first error of `diagnostics()` (which is ordered by position), code and
//! span, over every yaml-test-suite input as is and with a byte order mark
//! (U+FEFF) inserted at every character boundary (LF), and at every line
//! start with CRLF and CR line breaks. A diagnostic that accompanies
//! another (a mark that makes a construct fail) must be emitted in the
//! order of the two labels, or the views disagree.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};

use oneyaml::{Severity, Stream};

fn in_workspace() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    std::fs::read_to_string(root)
        .is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
}

/// Every suite `in.yaml` (valid and invalid), sorted; empty outside the
/// workspace (the data is not in the published package).
fn suite() -> Vec<(String, String)> {
    let root: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    if !root.is_dir() {
        assert!(!in_workspace(), "suite data missing at {}", root.display());
        eprintln!("skipped: no suite data at {}", root.display());
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    let id = p.parent().unwrap().strip_prefix(&root).unwrap();
                    out.push((id.display().to_string(), text));
                }
            }
        }
    }
    out.sort();
    out
}

/// `None` when the views agree, else a description.
fn disagreement(src: &str) -> Option<String> {
    let first = Stream::parse_recovering(src)
        .diagnostics()
        .iter()
        .find(|d| d.severity == Severity::Error)
        .map(|d| (d.code, d.primary.span));
    let strict = Stream::parse(src)
        .err()
        .map(|e| (e.diagnostic().code, e.diagnostic().primary.span));
    (first != strict)
        .then(|| format!("{src:?}: strict {strict:?}, diagnostics()[first error] {first:?}"))
}

/// The inputs checked for one suite text.
fn variants(text: &str) -> Vec<String> {
    let mut out = vec![text.to_owned()];
    for (at, _) in text.char_indices().chain([(text.len(), ' ')]) {
        let mut s = text.to_owned();
        s.insert(at, '\u{feff}');
        out.push(s);
    }
    for brk in ["\r\n", "\r"] {
        let t = text.replace('\n', brk);
        let mut starts = vec![0];
        starts.extend(t.match_indices(brk).map(|(i, b)| i.saturating_add(b.len())));
        for at in starts {
            let mut s = t.clone();
            s.insert(at, '\u{feff}');
            out.push(s);
        }
    }
    out
}

#[test]
fn strict_failure_is_the_first_diagnostic() {
    let cases = suite();
    let mut checked = 0usize;
    let mut failing_as_is = 0usize;
    let mut bad = Vec::new();
    for (id, text) in &cases {
        if Stream::parse(text.as_str()).is_err() {
            failing_as_is += 1;
        }
        for s in variants(text) {
            checked += 1;
            if let Some(d) = disagreement(&s) {
                bad.push(format!("{id}: {d}"));
            }
        }
    }
    if !cases.is_empty() {
        // The suite's invalid inputs, checked above without a mark too.
        assert!(failing_as_is >= 90, "{failing_as_is}");
        assert!(checked > 10_000, "{checked}");
    }
    assert!(
        bad.is_empty(),
        "{} of {checked} disagree, e.g.:\n{}",
        bad.len(),
        bad.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// Known examples, which an earlier order got wrong.
#[test]
fn known_examples_agree() {
    for src in [
        "k: @x \u{feff}\n",
        "- | \u{feff}# x\n  a\n",
        "a: 1\nSecond \u{feff}x: y\n",
        "'k' \u{feff}: v\n",
        "%FOO \u{feff}bar\n--- a\n",
        "- |\n    \n \u{feff} a\n",
    ] {
        assert_eq!(disagreement(src), None);
    }
}
