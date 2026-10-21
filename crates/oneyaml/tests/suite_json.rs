//! `from_str::<Value>` over every valid yaml-test-suite case, compared with
//! the case's `in.json` under JSON-compatible semantics (the consistency
//! check available before the tree builder lands; consistency checks (2)/(3)
//! follow).
//!
//! JSON view of a `Value`: integers and floats as JSON numbers, integer
//! and float mapping keys as their YAML text, custom tags dropped (JSON
//! has none). Every known difference is listed in [`EXPECTED`] with its
//! reason, so both a regression and a fix show up as a failure.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};

use oneyaml::{Deserializer, Value};
use serde::Deserialize;

/// Cases whose `in.json` we do not match, and why.
const EXPECTED: &[(&str, &str)] = &[];

/// Whether this crate is being tested inside the oneyaml workspace (its
/// manifest two levels down from a `[workspace]` of `crates/*`), rather
/// than from its unpacked package. Inside, workspace data must be there:
/// a missing file fails the test instead of skipping it silently.
fn in_workspace() -> bool {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    std::fs::read_to_string(root)
        .is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
}

/// The vendored suite. It lives in the workspace, not in the published
/// package: a test run from the unpacked crate skips these tests, saying
/// so; inside the workspace, missing data fails them.
fn suite_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    if root.is_dir() {
        Some(root)
    } else {
        assert!(
            !in_workspace(),
            "yaml-test-suite data missing at {} inside the workspace",
            root.display()
        );
        eprintln!(
            "skipped: yaml-test-suite data not found at {} (run from the oneyaml workspace)",
            root.display()
        );
        None
    }
}

fn cases() -> Vec<PathBuf> {
    let Some(root) = suite_root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            }
        }
        if dir.join("in.yaml").is_file()
            && dir.join("in.json").is_file()
            && !dir.join("error").exists()
        {
            out.push(dir);
        }
    }
    out.sort();
    out
}

fn name(dir: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    dir.strip_prefix(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// A `Value` as JSON; `None` if JSON cannot express it (NaN, infinities,
/// integers wider than 64 bits, collection keys).
fn to_json(v: &Value) -> Option<serde_json::Value> {
    use serde_json::Value as J;
    Some(match v.untag_ref() {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => {
            if let Some(f) = n.as_float() {
                J::Number(serde_json::Number::from_f64(f)?)
            } else if let Some(u) = n.as_u64() {
                J::from(u)
            } else {
                J::from(n.as_i64()?)
            }
        }
        Value::String(s) => J::String(s.clone()),
        Value::Sequence(s) => J::Array(s.iter().map(to_json).collect::<Option<_>>()?),
        Value::Mapping(m) => {
            let mut o = serde_json::Map::new();
            for (k, v) in m {
                let key = match k.untag_ref() {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => String::new(),
                    _ => return None,
                };
                o.insert(key, to_json(v)?);
            }
            J::Object(o)
        }
        Value::Tagged(_) => return None,
        _ => return None,
    })
}

/// JSON numbers compare by value, as JSON has no integer/float
/// distinction (UGM3's `price: 450.00` is written `450` in its in.json):
/// exactly when both are integers, as `f64` otherwise.
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value as J;
    match (a, b) {
        (J::Number(x), J::Number(y)) => match (x.is_f64(), y.is_f64()) {
            (false, false) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (J::Array(x), J::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b))
        }
        (J::Object(x), J::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn check(dir: &Path) -> Result<(), String> {
    let yaml = fs::read_to_string(dir.join("in.yaml")).map_err(|e| e.to_string())?;
    let json = fs::read_to_string(dir.join("in.json")).map_err(|e| e.to_string())?;
    let want: Vec<serde_json::Value> = serde_json::Deserializer::from_str(&json)
        .into_iter()
        .collect::<Result<_, _>>()
        .map_err(|e| format!("in.json: {e}"))?;
    let got: Vec<Value> = Deserializer::from_str(&yaml)
        .map(Value::deserialize)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("error {}: {e}", e.code()))?;
    if got.len() != want.len() {
        return Err(format!(
            "{} documents, in.json has {}",
            got.len(),
            want.len()
        ));
    }
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        let g = to_json(g).ok_or_else(|| format!("doc {i}: not expressible as JSON: {g:?}"))?;
        if !same(&g, w) {
            return Err(format!("doc {i}: got {g}, in.json {w}"));
        }
    }
    Ok(())
}

#[test]
fn values_match_in_json() {
    if suite_root().is_none() {
        return;
    }
    let cases = cases();
    assert!(
        cases.len() > 250,
        "found the vendored suite ({})",
        cases.len()
    );
    let mut mismatches = Vec::new();
    for dir in &cases {
        if let Err(why) = check(dir) {
            mismatches.push((name(dir), why));
        }
    }
    for (n, why) in &mismatches {
        eprintln!("{n}: {why}");
    }
    let got: Vec<&str> = mismatches.iter().map(|(n, _)| n.as_str()).collect();
    let want: Vec<&str> = EXPECTED.iter().map(|(n, _)| *n).collect();
    assert_eq!(
        got,
        want,
        "{} of {} cases differ from in.json",
        got.len(),
        cases.len()
    );
}

/// Every case directory with an `in.yaml`.
fn all_cases() -> Vec<PathBuf> {
    let Some(root) = suite_root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            }
        }
        if dir.join("in.yaml").is_file() {
            out.push(dir);
        }
    }
    out.sort();
    out
}

#[test]
fn valid_cases_deserialize_and_error_cases_fail() {
    let mut wrong = Vec::new();
    if suite_root().is_none() {
        return;
    }
    let all = all_cases();
    assert!(all.len() > 380, "found the vendored suite ({})", all.len());
    for dir in &all {
        let yaml = fs::read_to_string(dir.join("in.yaml")).unwrap();
        let result: Result<Vec<Value>, _> = Deserializer::from_str(&yaml)
            .map(Value::deserialize)
            .collect();
        let invalid = dir.join("error").exists();
        if result.is_ok() == invalid {
            wrong.push(format!(
                "{} (invalid: {invalid}): {:?}",
                name(dir),
                result.err()
            ));
        }
    }
    for w in &wrong {
        eprintln!("{w}");
    }
    // Syntactically valid, but each has a duplicate mapping key, which YAML
    // 1.2 forbids when composing (§3.2.1.1: keys are unique), so building
    // a `Value` is correctly an error. The suite checks events only.
    //   2JQS: `: a` and `: b` are two empty (null) keys.
    //   X38W: `*a` repeats the key `&a [a, b]`.
    const DUPLICATE_KEY_CASES: [&str; 2] = ["2JQS", "X38W"];
    let unexpected: Vec<&String> = wrong
        .iter()
        .filter(|w| {
            !DUPLICATE_KEY_CASES
                .iter()
                .any(|c| w.starts_with(&format!("{c} ")) && w.contains("Y1013"))
        })
        .collect();
    assert!(unexpected.is_empty(), "{unexpected:#?}");
    let lw = oneyaml::Settings::new().duplicate_keys(oneyaml::DuplicateKeys::LastWins);
    for c in DUPLICATE_KEY_CASES {
        let dir = all.iter().find(|d| name(d) == c).unwrap();
        let yaml = fs::read_to_string(dir.join("in.yaml")).unwrap();
        let (_, w): (Value, _) = oneyaml::from_str_with_diagnostics(&yaml, &lw).unwrap();
        assert_eq!(w.len(), 1, "{c}");
        assert_eq!(w[0].code, oneyaml::code::DUPLICATE_KEY_LAST_WINS);
    }
}
