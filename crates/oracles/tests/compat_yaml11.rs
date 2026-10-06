//! `Compat::yaml_1_1()` against a YAML 1.1 reference:
//! PyYAML's `SafeLoader`, the YAML 1.1 resolver most YAML 1.1 users
//! meet (PyYAML is libyaml's sibling; libyaml itself is a parser only and
//! types nothing, so `oracles::libyaml` cannot be the reference for
//! typing).
//!
//! PyYAML is not a Rust crate, so its answers are recorded once by
//! `data/pyyaml-1.1/generate.py` (see `data/pyyaml-1.1/SOURCE.md`) and
//! compared here: every plain scalar of `scalars.tsv` read as `k: <scalar>`,
//! and the merge-key documents of `documents.tsv`.
//!
//! Every disagreement is listed in [`DIVERGENCES`] with its reason; the
//! test fails on an unlisted disagreement and on a listed one that no
//! longer occurs, so the list stays exact.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;

/// Scalars on which `Compat::yaml_1_1()` and PyYAML disagree, by reason.
const DIVERGENCES: &[(&str, &[&str])] = &[
    ("y_n_booleans", &["y", "Y", "n", "N"]),
    (
        "core_meanings_kept",
        &["08", "09", "0o17", "-.5", "1.5e3", "1e3", "1E+3", "1e400"],
    ),
    (
        "no_timestamp_type",
        &[
            "2001-12-14",
            "2001-12-14t21:59:43.10-05:00",
            "2001-12-14 21:59:43.10 -5",
            "2002-12-14",
        ],
    ),
    ("pyyaml_value_key", &["=", "<<"]),
];

/// Why (each a deliberate choice).
const REASONS: &[(&str, &str)] = &[
    (
        "y_n_booleans",
        "the YAML 1.1 bool type (yaml.org/type/bool) lists y|Y|n|N; PyYAML deliberately leaves \
         them strings. `yaml11_booleans` follows the 1.1 type repository",
    ),
    (
        "core_meanings_kept",
        "1.1 settings only add meanings on top of Core: `08`/`09` stay decimal, \
         `0o17` stays Core octal, and Core floats (`1e3`, `-.5`, `1e400` = inf) stay floats \
         where PyYAML's 1.1 regexes read strings",
    ),
    (
        "no_timestamp_type",
        "oneyaml has no timestamp type (schemas: Core, JSON, Failsafe, plus the named 1.1 \
         settings); `2001-12-14` is a string, as in YAML 1.2. PyYAML builds a datetime",
    ),
    (
        "pyyaml_value_key",
        "PyYAML resolves a plain `=` (yaml.org/type/value) and a plain `<<` value to special \
         tags and then fails to construct them; oneyaml reads both as strings (`<<` merges \
         only as a key)",
    ),
];

fn data(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data/pyyaml-1.1")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

/// `(type, value)` as `generate.py` writes them.
fn ours(scalar: &str) -> (String, String) {
    let src = format!("k: {scalar}\n");
    let v = match oneyaml::from_str_with::<oneyaml::Value>(&src, &oneyaml::Compat::yaml_1_1()) {
        Ok(v) => v,
        Err(_) => return ("error".to_owned(), String::new()),
    };
    let v = v.get("k").unwrap();
    match v {
        oneyaml::Value::Null => ("null".into(), "~".into()),
        oneyaml::Value::Bool(b) => ("bool".into(), b.to_string()),
        oneyaml::Value::Number(n) if n.is_float() => {
            let f = n.as_f64().unwrap();
            let s = if f.is_nan() {
                "nan".to_owned()
            } else {
                format!("{f:?}")
            };
            ("float".into(), s)
        }
        oneyaml::Value::Number(n) => ("int".into(), n.to_core_string()),
        oneyaml::Value::String(s) => ("str".into(), s.clone()),
        other => ("other".into(), format!("{other:?}")),
    }
}

#[test]
fn yaml_1_1_preset_types_scalars_like_pyyaml_except_the_listed_divergences() {
    let listed: BTreeMap<&str, &str> = DIVERGENCES
        .iter()
        .flat_map(|(r, s)| s.iter().map(move |s| (*s, *r)))
        .collect();
    for (r, _) in DIVERGENCES {
        assert!(REASONS.iter().any(|(x, _)| x == r), "unknown reason {r}");
    }
    let mut report = Vec::new();
    let mut rows = 0usize;
    for line in data("scalars.tsv").lines().filter(|l| !l.starts_with('#')) {
        let mut f = line.split('\t');
        let (scalar, kind, value) = (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
        rows += 1;
        let (ok, ov) = ours(scalar);
        let same = ok == kind && ov == value;
        match (same, listed.contains_key(scalar)) {
            (true, true) => report.push(format!("{scalar:?}: listed but now agrees")),
            (false, false) => report.push(format!(
                "{scalar:?}: ours {ok} {ov:?}, PyYAML {kind} {value:?}"
            )),
            _ => {}
        }
    }
    for s in listed.keys() {
        if !data("scalars.tsv")
            .lines()
            .any(|l| l.split('\t').next() == Some(s))
        {
            report.push(format!("{s:?}: listed but not in scalars.tsv"));
        }
    }
    assert!(rows > 100, "scalars.tsv has {rows} rows");
    assert!(report.is_empty(), "{}", report.join("\n"));
}

/// Merge keys (and the 1.1 typing around them) load to the same data as
/// PyYAML's (mappings compared as unordered, as PyYAML's dicts are).
#[test]
fn yaml_1_1_preset_merges_like_pyyaml() {
    let mut n = 0usize;
    for line in data("documents.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
    {
        let mut f = line.split('\t');
        let name = f.next().unwrap();
        let src: String = serde_json::from_str(f.next().unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(f.next().unwrap()).unwrap();
        let got: serde_json::Value =
            oneyaml::from_str_with(&src, &oneyaml::Compat::yaml_1_1()).unwrap();
        assert_eq!(got, expected, "{name}");
        n += 1;
    }
    assert_eq!(n, 5);
}
