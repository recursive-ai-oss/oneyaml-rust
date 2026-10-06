//! `Value` ⇄ `serde_json::Value` (feature `serde_json`).
//! The lossiness table is in `src/value/json.rs`; every row is here.
#![cfg(feature = "serde_json")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{Code, Limits, Mapping, Number, Settings, Value, code, from_str};
use serde_json::json;

type Json = serde_json::Value;

fn yaml(src: &str) -> Value {
    from_str(src).unwrap()
}

fn fails(v: &Value) -> (Code, String, String) {
    let e = Json::try_from(v).unwrap_err();
    assert_eq!(e.span(), None);
    let path = e.path().map(ToString::to_string).unwrap_or_default();
    (e.code(), path, e.to_string())
}

#[test]
fn yaml_to_json_what_converts() {
    let v = yaml(
        "s: text\nq: '123'\nn: ~\nb: false\ni: -12\nu: 18446744073709551615\nm: -9223372036854775808\n\
         f: 1.5\nz: -0.0\nx: 0x1F\nl: [1, [2, {k: v}]]\ne: {}\nes: []\n",
    );
    let j = Json::try_from(&v).unwrap();
    assert_eq!(
        j,
        json!({
            "s": "text", "q": "123", "n": null, "b": false, "i": -12,
            "u": u64::MAX, "m": i64::MIN, "f": 1.5, "z": -0.0, "x": 31,
            "l": [1, [2, {"k": "v"}]], "e": {}, "es": []
        })
    );
    assert!(j["z"].as_f64().unwrap().is_sign_negative());
    assert_eq!(Json::try_from(v.clone()).unwrap(), j);
    assert_eq!(v.to_json_with(&Settings::new()).unwrap(), j);
    // Scalars at the root.
    assert_eq!(Json::try_from(&yaml("7")).unwrap(), json!(7));
    assert_eq!(Json::try_from(&yaml("")).unwrap(), Json::Null);
}

#[test]
fn yaml_to_json_what_has_no_json_form() {
    let cases: &[(&str, Code, &str, &str)] = &[
        (
            "a: [1, .nan]",
            code::INVALID_VALUE,
            "a[1]",
            "float .nan has no JSON form (JSON numbers are finite) at a[1]",
        ),
        (
            "a: .inf",
            code::INVALID_VALUE,
            "a",
            "float .inf has no JSON form (JSON numbers are finite) at a",
        ),
        (
            "- -.inf",
            code::INVALID_VALUE,
            "[0]",
            "float -.inf has no JSON form (JSON numbers are finite) at [0]",
        ),
        (
            "n: 18446744073709551616",
            code::INTEGER_OUT_OF_RANGE,
            "n",
            "integer 18446744073709551616 is out of range for a JSON number (i64 or u64) at n",
        ),
        (
            "n: -9223372036854775809",
            code::INTEGER_OUT_OF_RANGE,
            "n",
            "integer -9223372036854775809 is out of range for a JSON number (i64 or u64) at n",
        ),
        (
            "n: 1234567890123456789012345678901234567890123",
            code::INTEGER_OUT_OF_RANGE,
            "n",
            "integer 1234567890123456789012345678901234567890123 is out of range for a JSON number (i64 or u64) at n",
        ),
        (
            "a: {b: !rgb 0f0}",
            code::TYPE_MISMATCH,
            "a.b",
            "tagged value `!rgb` has no JSON form at a.b",
        ),
        (
            "!!set {a, b}",
            code::TYPE_MISMATCH,
            "",
            "tagged value `tag:yaml.org,2002:set` has no JSON form",
        ),
        (
            "a: {1: x}",
            code::TYPE_MISMATCH,
            "a",
            "mapping key `1` has no JSON form (JSON object keys are strings) at a",
        ),
        (
            "- {x: 1}\n- {true: y}",
            code::TYPE_MISMATCH,
            "[1]",
            "mapping key `true` has no JSON form (JSON object keys are strings) at [1]",
        ),
        (
            "~: x",
            code::TYPE_MISMATCH,
            "",
            "mapping key `null` has no JSON form (JSON object keys are strings)",
        ),
        (
            "? [a]\n: x",
            code::TYPE_MISMATCH,
            "",
            "mapping key `Sequence [String(\"a\")]` has no JSON form (JSON object keys are strings)",
        ),
    ];
    for (src, want, path, msg) in cases {
        let (got, at, text) = fails(&yaml(src));
        assert_eq!(
            (got, at.as_str(), text.as_str()),
            (*want, *path, *msg),
            "{src}"
        );
    }
}

#[test]
fn max_depth_applies() {
    let v = yaml("[[[1]]]");
    let shallow = Settings::new().limits(Limits::default().max_depth(2));
    let e = v.to_json_with(&shallow).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    let deep = Settings::new().limits(Limits::default().max_depth(3));
    assert_eq!(v.to_json_with(&deep).unwrap(), json!([[[1]]]));
}

#[test]
fn json_to_yaml_is_total_and_exact() {
    let j = json!({
        "name": "web", "replicas": 3, "neg": -4, "big": u64::MAX, "min": i64::MIN,
        "ratio": 0.25, "tiny": 5e-324, "huge": 1.7976931348623157e308, "negzero": -0.0,
        "on": "on", "empty": {}, "list": [null, true, "8080", [1.0]]
    });
    let v = Value::try_from(j.clone()).unwrap();
    assert_eq!(v.get("replicas"), Some(&Value::from(3)));
    assert_eq!(v.get("big").and_then(Value::as_u64), Some(u64::MAX));
    assert_eq!(v.get("min").and_then(Value::as_i64), Some(i64::MIN));
    assert_eq!(v.get("tiny").and_then(Value::as_f64), Some(5e-324));
    assert!(
        v.get("negzero")
            .unwrap()
            .as_f64()
            .unwrap()
            .is_sign_negative()
    );
    // A JSON string stays a string, whatever it looks like.
    assert_eq!(v.get("on"), Some(&Value::from("on")));
    assert_eq!(v["list"][2], Value::from("8080"));
    // `1.0` is a JSON float and stays a float (an integer never equals a float).
    assert_eq!(v["list"][3][0], Value::Number(Number::from(1.0)));
    // And back, unchanged.
    assert_eq!(Json::try_from(&v).unwrap(), j);
    // YAML out: every-reader-safe quoting keeps "on" and "8080" strings.
    let text = oneyaml::to_string(&v).unwrap();
    let again: Value = from_str(&text).unwrap();
    assert_eq!(again, v);
}

/// Object order is serde_json's map order (sorted, or insertion order with
/// its `preserve_order` feature); YAML keeps whatever order it is given.
#[test]
fn object_order_follows_the_json_map() {
    let j: Json = serde_json::from_str(r#"{"b": 1, "a": 2, "c": 3}"#).unwrap();
    let json_order: Vec<String> = j.as_object().unwrap().keys().cloned().collect();
    let v = Value::try_from(j).unwrap();
    let yaml_order: Vec<String> = v
        .as_mapping()
        .unwrap()
        .keys()
        .map(|k| k.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(yaml_order, json_order);
    // YAML → JSON inserts in YAML order; the JSON map decides iteration.
    let mut m = Mapping::new();
    for k in ["z", "y", "x"] {
        m.insert(Value::from(k), Value::from(1));
    }
    let j = Json::try_from(&Value::Mapping(m)).unwrap();
    assert_eq!(j.as_object().unwrap().len(), 3);
}

/// JSON → YAML → JSON is the identity.
#[test]
fn json_round_trip() {
    for text in [
        "null",
        "[]",
        "{}",
        r#"{"a": [1, 2.5, "x", {"b": null}], "c": {"d": [[]]}}"#,
        "[-0.0, 0.0, 1e300, -1e-300, 123456789, -123456789]",
        r#""a string with \n newline""#,
    ] {
        let j: Json = serde_json::from_str(text).unwrap();
        let back = Json::try_from(&Value::try_from(j.clone()).unwrap()).unwrap();
        assert_eq!(back, j, "{text}");
    }
}

/// Both directions are iterative. A 1,000,000-deep JSON array
/// converts on a small stack (taken apart as it is read, so serde_json's
/// recursive `Drop` never sees the depth); the result is refused by
/// `max_depth` on the way back.
#[test]
fn deep_values_on_a_small_stack() {
    let run = || {
        let mut j = Json::Null;
        for _ in 0..1_000_000 {
            j = Json::Array(vec![j]);
        }
        let v = Value::try_from(j).unwrap();
        assert_eq!(v.depth(), 1_000_000);
        let e = Json::try_from(&v).unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    };
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

/// The JSON built here is at most 128 levels deep even
/// under `Limits::unlimited()`, so dropping it (serde_json's `Drop`
/// recurses) is safe on a small stack.
#[test]
fn json_output_depth_is_bounded() {
    let run = || {
        let nest = |depth: usize| {
            let mut v = Value::from(1);
            for _ in 0..depth {
                v = Value::from(vec![v]);
            }
            v
        };
        let unlimited = Settings::new().limits(Limits::unlimited());
        let e = nest(200_000).to_json_with(&unlimited).unwrap_err();
        assert_eq!(e.code(), code::JSON_DEPTH_EXCEEDED);
        let e = Json::try_from(&nest(129)).unwrap_err();
        assert_eq!(e.code(), code::JSON_DEPTH_EXCEEDED);
        let help = e.diagnostic().help.clone().unwrap();
        assert!(
            help.contains("fixed") && help.contains("Value::serialize"),
            "{help}"
        );
        let deepest = nest(128).to_json_with(&unlimited).unwrap();
        drop(deepest);
        // Mappings count the same.
        let mut m = Value::Null;
        for _ in 0..129 {
            let mut map = Mapping::new();
            map.insert(Value::from("k"), m);
            m = Value::Mapping(map);
        }
        assert_eq!(
            Json::try_from(&m).unwrap_err().code(),
            code::JSON_DEPTH_EXCEEDED
        );
        // JSON -> YAML: a deep input is taken apart, the result drops
        // iteratively.
        let mut j = Json::Null;
        for _ in 0..200_000 {
            let mut o = serde_json::Map::new();
            o.insert("k".to_owned(), j);
            j = Json::Object(o);
        }
        let v = Value::try_from(j).unwrap();
        assert_eq!(v.depth(), 200_000);
        drop(v);
    };
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}
