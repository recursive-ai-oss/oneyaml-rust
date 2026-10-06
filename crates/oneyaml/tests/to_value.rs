//! `to_value` / `to_value_with`: `serde_yaml`'s `to_value`, so
//! migrating is a `use` swap. Every error arm is reached, and
//! a deep `Value` handed over converts on a small stack.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use oneyaml::{Limits, Sequence, Settings, Value, code};
use serde::{Serialize, Serializer};

#[derive(Serialize, serde::Deserialize, Debug, PartialEq)]
struct Config {
    name: String,
    port: u16,
    tags: Vec<String>,
    limits: BTreeMap<String, f64>,
    note: Option<String>,
}

fn config() -> Config {
    Config {
        name: "web".into(),
        port: 8080,
        tags: vec!["a".into(), "yes".into()],
        limits: BTreeMap::from([("cpu".into(), 0.5)]),
        note: None,
    }
}

#[test]
fn to_value_is_what_the_serialized_text_reads_as() {
    let v = oneyaml::to_value(config()).unwrap();
    let text = oneyaml::to_string(&config()).unwrap();
    let read: Value = oneyaml::from_str(&text).unwrap();
    assert_eq!(v, read);
    assert_eq!(v["port"].as_u64(), Some(8080));
    assert!(v["note"].is_null());
    // And back through `from_value`.
    let back: Config = oneyaml::from_value(v).unwrap();
    assert_eq!(back, config());
}

#[test]
fn to_value_of_a_value_is_the_value() {
    let src = "a: !t [1, {b: 2}]\n? [k]\n: 0x10\n";
    let v: Value = oneyaml::from_str(src).unwrap();
    assert_eq!(oneyaml::to_value(&v).unwrap(), v);
}

#[test]
fn to_value_with_applies_max_typed_depth() {
    let shallow = Settings::new().limits(Limits::default().max_typed_depth(2));
    assert!(oneyaml::to_value_with(vec![vec![1]], &shallow).is_ok());
    let e = oneyaml::to_value_with(vec![vec![vec![1]]], &shallow).unwrap_err();
    assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
    // The default bound (128) also applies to `to_value`.
    let mut deep = serde_json::json!(1);
    for _ in 0..200 {
        deep = serde_json::json!([deep]);
    }
    let e = oneyaml::to_value(&deep).unwrap_err();
    assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
}

#[test]
fn to_value_refuses_duplicate_keys() {
    struct Twice;
    impl Serialize for Twice {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut m = s.serialize_map(Some(2))?;
            m.serialize_entry("a", &1)?;
            m.serialize_entry("a", &2)?;
            m.end()
        }
    }
    let e = oneyaml::to_value(&Twice).unwrap_err();
    assert_eq!(e.code(), code::SERIALIZE_DUPLICATE_KEY);
}

#[test]
fn to_value_reports_a_failing_serialize_impl() {
    struct Fails;
    impl Serialize for Fails {
        fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("no"))
        }
    }
    let e = oneyaml::to_value(vec![Fails]).unwrap_err();
    assert_eq!(e.code(), code::SERIALIZE_CUSTOM);
}

fn deep(depth: usize) -> Value {
    let mut v = Value::from(1);
    for _ in 0..depth {
        v = Value::Sequence(Sequence::from(vec![v]));
    }
    v
}

/// `max_depth` applies to the `Value` `to_value` returns, as to
/// every `Value` our APIs consume (`from_value`, `to_string`).
#[test]
fn to_value_applies_max_depth() {
    let v = deep(2_000);
    let e = oneyaml::to_value(&v).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    // The same code as the other entry points that take a `Value`.
    assert_eq!(
        oneyaml::to_string(&v).unwrap_err().code(),
        code::VALUE_DEPTH_EXCEEDED
    );
    assert_eq!(
        oneyaml::from_value::<Value>(v.clone()).unwrap_err().code(),
        code::VALUE_DEPTH_EXCEEDED
    );
    // By value, as serde_yaml's signature takes it.
    assert_eq!(
        oneyaml::to_value(v).unwrap_err().code(),
        code::VALUE_DEPTH_EXCEEDED
    );
}

/// Two handed-over `Value`s under a struct: each alone fits, the combined
/// result does not.
#[test]
fn to_value_checks_the_combined_depth() {
    #[derive(Serialize)]
    struct Holder {
        a: Value,
        b: Value,
    }
    // `deep(n)` nests n collections (scalars cost no level).
    let limit = Settings::new().max_depth(10);
    let fits = deep(10);
    assert!(oneyaml::to_value_with(&fits, &limit).is_ok());
    let h = Holder {
        a: fits.clone(),
        b: fits,
    };
    let e = oneyaml::to_value_with(&h, &limit).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    assert_eq!(
        oneyaml::to_string_with(&h, &limit).unwrap_err().code(),
        code::VALUE_DEPTH_EXCEEDED
    );
    let roomy = Settings::new().max_depth(11);
    assert!(oneyaml::to_value_with(&h, &roomy).is_ok());
    // And a small limit on a plain Rust value.
    let one = Settings::new().max_depth(1);
    assert!(oneyaml::to_value_with(vec![1], &one).is_ok());
    let e = oneyaml::to_value_with(vec![vec![1]], &one).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
}

#[test]
fn a_deep_value_converts_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            let v = deep(100_000);
            // Under the defaults: Y2005, found without recursing.
            let e = oneyaml::to_value(&v).unwrap_err();
            assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
            // With no limits: handed over whole (a `Value` costs one typed
            // level), Ok and equal, never a stack overflow.
            let unlimited = Settings::new().limits(Limits::unlimited());
            let out = oneyaml::to_value_with(&v, &unlimited).unwrap();
            assert!(out == v);
        })
        .unwrap()
        .join()
        .unwrap();
}
