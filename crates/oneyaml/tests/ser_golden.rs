//! Serializer golden output tests: every default individually, the awkward
//! strings, streams, tags, enums, complex keys, block scalars, serde
//! shapes, and the `Layout::SerdeYaml` outputs, against the frozen
//! snapshot `tests/ser_golden.snapshot`.
//!
//! **The snapshot is frozen** (output stability): default output bytes
//! change only to fix incorrect output, with owner approval. To regenerate
//! it after an approved change, run with `ONEYAML_BLESS=1` and review the
//! diff. Every case's output is also read back and compared with the value
//! it was written from (the round-trip property, on the golden corpus).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod ser_common;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use oneyaml::{Compat, Emit, Layout, QuoteStyle, Settings};
use ser_common::{Case, cases};

const SNAPSHOT: &str = "tests/ser_golden.snapshot";

fn render(cases: &[Case]) -> String {
    let mut out = String::from(
        "# oneyaml serializer golden outputs. FROZEN: changes need owner approval.\n\
         # Each case: `=== name` then the exact output bytes (or `!error Y####`).\n",
    );
    for c in cases {
        let _ = writeln!(out, "=== {}", c.name);
        match &c.output {
            Ok(s) => out.push_str(s),
            Err(code) => {
                let _ = writeln!(out, "!error {code}");
            }
        }
    }
    out
}

fn parse(snapshot: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in snapshot.split_inclusive('\n') {
        if let Some(name) = line.strip_prefix("=== ") {
            if let Some((n, body)) = current.take() {
                map.insert(n, body);
            }
            current = Some((name.trim_end_matches('\n').to_owned(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
        }
    }
    if let Some((n, body)) = current {
        map.insert(n, body);
    }
    map
}

#[test]
fn golden_outputs_match_the_frozen_snapshot() {
    let cases = cases();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    let rendered = render(&cases);
    if std::env::var_os("ONEYAML_BLESS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    let frozen = parse(&std::fs::read_to_string(&path).expect("tests/ser_golden.snapshot"));
    let current = parse(&rendered);
    let mut diffs = Vec::new();
    for (name, body) in &current {
        match frozen.get(name) {
            Some(f) if f.as_bytes() == body.as_bytes() => {}
            Some(f) => diffs.push(format!("{name}:\n--- frozen\n{f}--- now\n{body}")),
            None => diffs.push(format!("{name}: not in the snapshot")),
        }
    }
    for name in frozen.keys() {
        if !current.contains_key(name) {
            diffs.push(format!("{name}: in the snapshot but no longer a case"));
        }
    }
    assert!(
        diffs.is_empty(),
        "golden output changed:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn golden_outputs_read_back_as_their_values() {
    for c in cases() {
        if let (Ok(out), Some(check)) = (&c.output, &c.read_back) {
            check(out).unwrap_or_else(|e| panic!("{}: {e}\n{out}", c.name));
        }
    }
}

#[test]
fn case_names_are_unique_and_every_default_is_covered() {
    let cases = cases();
    let mut names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), n, "duplicate case names");
    // One case per default-output row at least.
    for prefix in [
        "quote_",
        "indent_",
        "block_",
        "null",
        "bool",
        "float_",
        "empty_",
        "complex_keys",
        "stream_",
        "tag_",
        "enum_",
        "serde_yaml_",
    ] {
        assert!(
            names.iter().any(|n| n.starts_with(prefix)),
            "no case for {prefix}"
        );
    }
    // Settings the golden set exercises.
    let _ = (
        Settings::new().emit(Emit::new().quote_style(QuoteStyle::Double)),
        Compat::serde_yaml().get_emit().get_layout() == Layout::SerdeYaml,
    );
}
