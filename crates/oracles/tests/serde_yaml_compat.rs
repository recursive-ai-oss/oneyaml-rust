//! `Compat::serde_yaml()` (`Layout::SerdeYaml`) reproduces
//! `serde_yaml`'s output, checked **byte for byte** against
//! `serde_yaml_ng::to_string` over typed values, `Value`s, the awkward
//! strings, and multi-document streams.
//!
//! Best effort: every case whose bytes differ is listed in [`DIVERGENCES`]
//! with the reason, and the test fails both when an unlisted case differs
//! and when a listed one stops differing, so the list is always exact.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

#[path = "../../oneyaml/tests/ser_common/mod.rs"]
mod ser_common;

use std::collections::BTreeMap;

use ser_common::{AWKWARD, Shape, service};
use serde::Serialize;

/// Why a case's bytes differ from `serde_yaml_ng`'s (reason codes are
/// explained in [`REASONS`]).
const DIVERGENCES: &[(&str, &str)] = &[
    ("service", "serde_yaml_error"),
    ("shape#11", "serde_yaml_error"),
    ("shapes#11", "serde_yaml_error"),
    ("str#150", "root_indicator"),
    ("str#151", "root_indicator"),
    ("str#152", "root_indicator"),
    ("str#158", "root_indicator"),
    ("str#159", "root_indicator"),
    ("str#160", "root_indicator"),
    ("str#161", "root_indicator"),
    ("str#136", "ls_ps"),
    ("seq#136", "ls_ps"),
    ("key#136", "ls_ps"),
    ("str#137", "ls_ps"),
    ("seq#137", "ls_ps"),
    ("key#137", "ls_ps"),
    ("char", "char"),
    ("f32", "f32_layout"),
    ("str#127", "merge_key"),
    ("seq#127", "merge_key"),
    ("key#127", "merge_key"),
    ("str#128", "merge_key"),
    ("seq#128", "merge_key"),
    ("key#128", "merge_key"),
];

/// The reasons, each a deliberate choice (correctness >
/// round-trip exactness > compatibility).
const REASONS: &[(&str, &str)] = &[
    (
        "serde_yaml_error",
        "serde_yaml refuses a newtype variant around another variant (\"serializing nested \
         enums in YAML is not supported yet\"); oneyaml writes the outer one as `Name: \
         !Inner ...`, which reads back",
    ),
    (
        "root_indicator",
        "a root string whose first line starts with a space or a line break needs a block \
         indentation indicator; libyaml counts it from 0 (`|2`) where YAML 1.2 counts from -1 \
         (\u{a7}8.1.1.1), so readers disagree and oneyaml double-quotes it instead",
    ),
    (
        "ls_ps",
        "libyaml writes U+2028/U+2029 as line breaks inside single quotes, which YAML 1.2 \
         readers read as other text; oneyaml double-quotes them as `\\L`/`\\P`",
    ),
    (
        "char",
        "serde_yaml always single-quotes a `char`; oneyaml's data model (`Value`) keeps a char \
         as a string, quoted only when needed",
    ),
    (
        "merge_key",
        "serde_yaml writes the strings `<<` and `=` plain, which every merge-key reader \
         (PyYAML, Go, Psych, oneyaml with `merge_keys`) reads as the merge key or the YAML 1.1 \
         value key: a string key `<<` would merge its value into the parent (an \
         injection); oneyaml quotes them in every layout",
    ),
    (
        "f32_layout",
        "serde_yaml lays out an `f32` with ryu's f32 thresholds (`1e14`); oneyaml keeps floats \
         as `f64` and uses the f64 thresholds (`100000000000000.0`); same digits, same value",
    ),
];

struct Case {
    name: String,
    ours: Result<String, String>,
    theirs: Result<String, String>,
}

fn case<T: Serialize + ?Sized>(name: &str, v: &T) -> Case {
    Case {
        name: name.to_owned(),
        ours: oneyaml::to_string_with(v, &oneyaml::Compat::serde_yaml()).map_err(|e| e.to_string()),
        theirs: serde_yaml_ng::to_string(v).map_err(|e| e.to_string()),
    }
}

/// A YAML text read by each library into its own `Value`, then written.
fn text_case(name: &str, src: &str) -> Case {
    let ours: oneyaml::Value = oneyaml::from_str(src).unwrap();
    let theirs: serde_yaml_ng::Value = serde_yaml_ng::from_str(src).unwrap();
    Case {
        name: name.to_owned(),
        ours: oneyaml::to_string_with(&ours, &oneyaml::Compat::serde_yaml())
            .map_err(|e| e.to_string()),
        theirs: serde_yaml_ng::to_string(&theirs).map_err(|e| e.to_string()),
    }
}

fn stream_case(name: &str, docs: &[serde_json::Value]) -> Case {
    let mut ours = Vec::new();
    let mut ser = oneyaml::Serializer::with_settings(&mut ours, &oneyaml::Compat::serde_yaml());
    for d in docs {
        d.serialize(&mut ser).unwrap();
    }
    ser.into_inner().unwrap();
    let mut theirs = Vec::new();
    {
        let mut ser = serde_yaml_ng::Serializer::new(&mut theirs);
        for d in docs {
            d.serialize(&mut ser).unwrap();
        }
    }
    Case {
        name: name.to_owned(),
        ours: Ok(String::from_utf8(ours).unwrap()),
        theirs: Ok(String::from_utf8(theirs).unwrap()),
    }
}

/// A stream of YAML documents, each read by each library into its own
/// `Value`, then written through each library's `Serializer`.
fn text_stream_case(name: &str, docs: &[&str]) -> Case {
    let mut ours = Vec::new();
    let mut ser = oneyaml::Serializer::with_settings(&mut ours, &oneyaml::Compat::serde_yaml());
    for d in docs {
        oneyaml::from_str::<oneyaml::Value>(d)
            .unwrap()
            .serialize(&mut ser)
            .unwrap();
    }
    ser.into_inner().unwrap();
    let mut theirs = Vec::new();
    {
        let mut ser = serde_yaml_ng::Serializer::new(&mut theirs);
        for d in docs {
            serde_yaml_ng::from_str::<serde_yaml_ng::Value>(d)
                .unwrap()
                .serialize(&mut ser)
                .unwrap();
        }
    }
    Case {
        name: name.to_owned(),
        ours: Ok(String::from_utf8(ours).unwrap()),
        theirs: Ok(String::from_utf8(theirs).unwrap()),
    }
}

/// Everything `serde_yaml` writes the same way: must match exactly.
#[derive(Serialize)]
struct Config {
    name: String,
    port: u16,
    enabled: bool,
    ratio: f64,
    small: f32,
    big: i128,
    huge: u128,
    note: Option<String>,
    missing: Option<String>,
    unit: (),
    pair: (i32, String),
    bytes: Vec<u8>,
    inner: ser_common::Inner,
    shapes: Vec<Shape>,
    empty_list: Vec<i32>,
    empty_map: BTreeMap<String, i32>,
    script: String,
    id: ser_common::Id,
    marker: ser_common::Marker,
    words: Vec<&'static str>,
}

fn config() -> Config {
    let s = service();
    Config {
        name: s.name,
        port: s.port,
        enabled: s.enabled,
        ratio: s.ratio,
        small: s.small,
        big: s.big,
        huge: s.huge,
        note: s.note,
        missing: s.missing,
        unit: s.unit,
        pair: s.pair,
        bytes: s.bytes,
        inner: s.inner,
        shapes: vec![
            Shape::Unit,
            Shape::Text("t".to_owned()),
            Shape::List(vec![1]),
            Shape::List(vec![]),
            Shape::Pair(1, "x".to_owned()),
            Shape::Point { x: 0.5, y: 1e16 },
            Shape::Spaced("s".to_owned()),
        ],
        empty_list: s.empty_list,
        empty_map: s.empty_map,
        script: s.script,
        id: s.id,
        marker: s.marker,
        words: vec![
            "yes",
            "no",
            "1.0",
            "~",
            "2001-12-14",
            "-a",
            "a: b",
            "\u{e9}",
            "\u{1f600}",
            "multi\nline\n",
        ],
    }
}

fn cases() -> Vec<Case> {
    let mut c = vec![
        case("config", &config()),
        case("service", &service()),
        case("unit", &()),
        case("none", &None::<i32>),
        case("some", &Some(1)),
        case("bool", &true),
        case("i128", &i128::MIN),
        case("u128", &u128::MAX),
        case("char", &'x'),
        case("bytes", &serde_json::json!([0, 255])),
        case("tuple", &(1, "a", false)),
        case("empty_vec", &Vec::<i32>::new()),
        case("empty_map", &BTreeMap::<String, i32>::new()),
        case("nested_vec", &vec![vec![1, 2], vec![], vec![3]]),
        case("deep_vec", &vec![vec![vec![vec![1]]]]),
        case("int_keys", &BTreeMap::from([(1, "a"), (2, "b\nc")])),
        case("tuple_keys", &BTreeMap::from([((1, 2), "a")])),
        case(
            "vec_keys",
            &BTreeMap::from([(vec![1], vec![2]), (vec![], vec![])]),
        ),
        case("f32", &vec![1.1f32, 0.1, 3.4e38, 1e-7, 16_777_216.0, 1e14]),
        case(
            "f64",
            &vec![
                1.0,
                0.1,
                -2.5,
                1e15,
                1e16,
                1e300,
                1e-5,
                1e-7,
                1.5e-7,
                5e-324,
                f64::MAX,
                -0.0,
            ],
        ),
        case(
            "f64_special",
            &vec![f64::INFINITY, f64::NEG_INFINITY, f64::NAN],
        ),
        case(
            "enum_map",
            &BTreeMap::from([("a", Shape::Unit), ("b", Shape::Pair(2, "b".to_owned()))]),
        ),
        case(
            "long_key",
            &BTreeMap::from([("k".repeat(129), 1), ("j".repeat(128), 2)]),
        ),
        case(
            "block_in_map",
            &BTreeMap::from([("k", "a\nb\n"), ("s", " a\nb"), ("t", "a\n\n")]),
        ),
        case(
            "json_nested",
            &serde_json::json!({"a": {"b": [1, {"c": null}], "d": []}, "e": {}}),
        ),
        text_case(
            "tags",
            "a: !t x\nb: !t [1, 2]\nc: !t {k: v}\nd: !t []\ne: !t {}\n",
        ),
        text_case("tag_seq", "- !t x\n- !u [1]\n- !v {k: 1}\n"),
        text_case("tag_root", "!t\n- a\n"),
        text_case(
            "complex_keys",
            "? [1, 2]\n: a\n? {k: v}\n: [b]\n? [x]\n: {y: z}\n[]: e\n{}: f\n",
        ),
        text_case("null_keys", "~: a\n1: b\n1.5: c\ntrue: d\n"),
        text_case("multi_line_key", "? |\n  a\n  b\n: c\n"),
        text_stream_case(
            "tagged_stream",
            &["1", "!t {k: v}", "!t [1]", "!t x", "!t []", "a: !t [1]"],
        ),
        stream_case(
            "stream",
            &[
                serde_json::json!({"a": 1}),
                serde_json::json!([1]),
                serde_json::json!("x"),
                serde_json::json!(null),
                serde_json::json!([]),
                serde_json::json!("a\nb"),
            ],
        ),
    ];
    for (i, s) in AWKWARD.iter().enumerate() {
        c.push(case(&format!("str#{i}"), s));
        c.push(case(&format!("seq#{i}"), &[s]));
        c.push(case(&format!("key#{i}"), &BTreeMap::from([(s, 1)])));
    }
    let shapes = [
        Shape::Unit,
        Shape::Text("hello".to_owned()),
        Shape::Text("on".to_owned()),
        Shape::Count(3),
        Shape::Flag(false),
        Shape::Maybe(None),
        Shape::Maybe(Some(7)),
        Shape::List(vec![1, 2]),
        Shape::List(vec![]),
        Shape::Pair(1, "x".to_owned()),
        Shape::Point { x: 1.0, y: -2.5 },
        Shape::Nested(Box::new(Shape::Count(1))),
        Shape::Nested(Box::new(Shape::Unit)),
        Shape::Spaced("s".to_owned()),
    ];
    for (i, s) in shapes.iter().enumerate() {
        c.push(case(&format!("shape#{i}"), s));
        c.push(case(&format!("shapes#{i}"), &[s]));
    }
    c
}

#[test]
fn serde_yaml_layout_matches_serde_yaml_ng_except_the_listed_divergences() {
    let listed: BTreeMap<&str, &str> = DIVERGENCES.iter().copied().collect();
    assert_eq!(
        listed.len(),
        DIVERGENCES.len(),
        "a divergence is listed twice"
    );
    for reason in listed.values() {
        assert!(
            REASONS.iter().any(|(r, _)| r == reason),
            "unknown reason {reason}"
        );
    }
    let mut report = Vec::new();
    let mut seen = Vec::new();
    for c in cases() {
        let same = c.ours == c.theirs;
        match (same, listed.get(c.name.as_str())) {
            (true, None) => {}
            (true, Some(_)) => report.push(format!(
                "{}: listed as a divergence but now matches",
                c.name
            )),
            (false, Some(_)) => seen.push(c.name.clone()),
            (false, None) => report.push(format!(
                "{}: differs (unlisted)\n--- ours\n{}\n--- serde_yaml_ng\n{}",
                c.name,
                c.ours.clone().unwrap_or_else(|e| format!("error: {e}")),
                c.theirs.clone().unwrap_or_else(|e| format!("error: {e}"))
            )),
        }
    }
    for name in listed.keys() {
        if !seen.iter().any(|s| s == name)
            && !report.iter().any(|r| r.starts_with(&format!("{name}:")))
        {
            report.push(format!("{name}: listed but not a case"));
        }
    }
    assert!(
        report.is_empty(),
        "{} problems:\n{}",
        report.len(),
        report.join("\n")
    );
}
