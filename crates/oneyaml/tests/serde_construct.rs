//! Shared construction through the serde entry points: anchors/aliases,
//! the alias budget, merge keys, duplicate keys, settings diagnostics, and
//! agreement between the `Value` builder and streamed targets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unreachable
)]

use std::collections::BTreeMap;
use std::fmt;

use oneyaml::{
    Compat, Deserializer, DuplicateKeys, ErrorKind, Limits, Schema, Settings, Value, code,
    from_reader, from_reader_with, from_slice, from_str, from_str_with, from_str_with_diagnostics,
};
use serde::Deserialize;
use serde::de::{MapAccess, SeqAccess, Visitor};

/// Records the exact event order a streamed target sees, as a `Value`
/// built through serde's visitor protocol (not the shared builder), so
/// comparing it with `from_str::<Value>` checks both readers end to end.
#[derive(Debug, PartialEq)]
struct Streamed(Value);

impl<'de> Deserialize<'de> for Streamed {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Streamed;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Streamed, E> {
                Ok(Streamed(Value::from(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Streamed, E> {
                Ok(Streamed(Value::from(v)))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Streamed, E> {
                Ok(Streamed(Value::from(v)))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Streamed, E> {
                Ok(Streamed(Value::from(v)))
            }
            fn visit_str<E>(self, v: &str) -> Result<Streamed, E> {
                Ok(Streamed(Value::from(v)))
            }
            fn visit_unit<E>(self) -> Result<Streamed, E> {
                Ok(Streamed(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Streamed, A::Error> {
                let mut out = Vec::new();
                while let Some(Streamed(v)) = a.next_element()? {
                    out.push(v);
                }
                Ok(Streamed(Value::from(out)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Streamed, A::Error> {
                let mut m = oneyaml::Mapping::new();
                while let Some((Streamed(k), Streamed(v))) = a.next_entry()? {
                    m.insert(k, v);
                }
                Ok(Streamed(Value::from(m)))
            }
        }
        d.deserialize_any(V)
    }
}

fn agree(src: &str, settings: &Settings) -> Value {
    let built: Value = from_str_with(src, settings).unwrap();
    let Streamed(streamed) = from_str_with(src, settings).unwrap();
    assert_eq!(built, streamed, "builder and stream differ for {src:?}");
    built
}

fn yaml(src: &str) -> Value {
    from_str(src).unwrap()
}

#[test]
fn anchors_and_aliases() {
    let v = agree("a: &x [1, {b: &y 2}]\nc: *x\nd: *y\n", &Settings::new());
    assert_eq!(v["c"], v["a"]);
    assert_eq!(v["d"], Value::from(2));
    // Redefinition: an alias means the anchor as defined before it, also
    // when replayed from inside another anchor.
    let v = agree("- &a 1\n- &b [*a]\n- &a 2\n- *b\n- *a\n", &Settings::new());
    assert_eq!(v, yaml("[1, [1], 2, [1], 2]"));
    // Aliases as keys.
    let v = agree("- &k key\n- {*k : v}\n", &Settings::new());
    assert_eq!(v[1]["key"], Value::from("v"));
    // Anchors are document-scoped.
    let docs: Vec<Result<Value, oneyaml::Error>> = Deserializer::from_str("&a 1\n--- *a\n")
        .map(Value::deserialize)
        .collect();
    assert_eq!(docs[0].as_ref().unwrap(), &Value::from(1));
    assert_eq!(docs[1].as_ref().unwrap_err().code(), code::UNDEFINED_ALIAS);
}

#[test]
fn undefined_and_recursive_aliases() {
    let e = from_str::<Value>("a: *nope\n").unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
    assert_eq!(e.kind(), ErrorKind::Schema);
    assert_eq!(e.location().unwrap().column(), 4);
    let e = from_str::<Value>("&a [*a]\n").unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
    assert!(e.to_string().contains("recursive"), "{e}");
}

#[test]
fn alias_budget_is_incremental_and_ratio_based() {
    // A billion laughs: nine levels of ten aliases each.
    let mut src = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for i in 1..9 {
        let refs = vec![format!("*a{}", i - 1); 10].join(", ");
        src.push_str(&format!("a{i}: &a{i} [{refs}]\n"));
    }
    let e = from_str::<Value>(&src).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BUDGET_EXCEEDED);
    assert_eq!(e.kind(), ErrorKind::Limit);
    assert!(
        e.diagnostic()
            .help
            .as_deref()
            .unwrap()
            .contains("alias_budget")
    );
    // The typed path shares the budget (and ignored content counts too).
    #[derive(Debug, Deserialize)]
    struct Nothing {}
    assert_eq!(
        from_str::<Nothing>(&src).unwrap_err().code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
    // A small fixed budget.
    let tight = Settings::new().limits(Limits::default().alias_budget(3));
    assert!(from_str_with::<Value>("- &a [1]\n- *a\n", &tight).is_ok());
    assert_eq!(
        from_str_with::<Value>("- &a [1, 2]\n- *a\n- *a\n", &tight)
            .unwrap_err()
            .code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
    // Ratio: max(factor × own nodes, floor).
    let ratio = Settings::new().limits(Limits::default().alias_budget_ratio(1, 0));
    assert!(from_str_with::<Value>("- &a [1]\n- *a\n", &ratio).is_ok());
    assert!(from_str_with::<Value>("- &a [1, 2, 3]\n- *a\n- *a\n", &ratio).is_err());
    // Unlimited disables it.
    let unlimited = Settings::new().limits(Limits::unlimited());
    let small = "a: &a [1, 1]\nb: &b [*a, *a]\nc: [*b, *b]\n";
    assert!(from_str_with::<Value>(small, &unlimited).is_ok());
}

#[test]
fn alias_expansion_respects_max_depth() {
    let s = Settings::new().max_depth(4);
    let src = "a: &a [[[1]]]\nb: [[*a]]\n";
    let e = from_str_with::<Value>(src, &s).unwrap_err();
    assert_eq!(e.code(), code::DEPTH_LIMIT_EXCEEDED);
    assert!(from_str_with::<Value>("a: &a [[1]]\nb: [*a]\n", &s).is_ok());
}

#[test]
fn merge_keys_follow_the_spec() {
    let on = Settings::new().merge_keys(true);
    let src = "\
base: &base {a: 1, b: 1}
more: &more {b: 2, c: 2}
x:
  <<: [*base, *more]
  a: 0
  d: 4
";
    let v = agree(src, &on);
    // Explicit entries in order, then merged ones (earlier sources win).
    assert_eq!(v["x"], yaml("{a: 0, d: 4, b: 1, c: 2}"));
    // Explicit keys win even when they come first.
    let v = agree("b: &b {k: 1}\nx: {k: 0, <<: *b}\n", &on);
    assert_eq!(v["x"], yaml("{k: 0}"));
    // Nested merges.
    let v = agree("a: &a {x: 1}\nb: &b {<<: *a, y: 2}\nc: {<<: *b}\n", &on);
    assert_eq!(v["c"], yaml("{y: 2, x: 1}"));
    // Off: `<<` is an ordinary key.
    let v = agree("a: &a {x: 1}\nb: {<<: *a}\n", &Settings::new());
    assert_eq!(v["b"]["<<"], yaml("{x: 1}"));
    // Quoted `<<` is never a merge key; `!!merge` is.
    let v = agree("a: &a {x: 1}\nb: {'<<': *a}\n", &on);
    assert!(v["b"].get("<<").is_some());
    let v = agree("a: &a {x: 1}\nb: {!!merge <<: *a}\n", &on);
    assert_eq!(v["b"], yaml("{x: 1}"));
    // Typed targets see merged fields; plain text survives.
    #[derive(Deserialize, Debug, PartialEq)]
    struct Svc {
        image: String,
        port: u16,
    }
    #[derive(Deserialize)]
    struct Doc {
        web: Svc,
    }
    let d: Doc = from_str_with(
        "x-base: &b {image: 3.10, port: 80}\nweb: {<<: *b, port: 8080}\n",
        &on,
    )
    .unwrap();
    assert_eq!(
        d.web,
        Svc {
            image: "3.10".into(),
            port: 8080
        }
    );
}

#[test]
fn merge_key_errors() {
    let on = Settings::new().merge_keys(true);
    for src in [
        "a: {<<: 1}\n",
        "a: {<<: [{x: 1}, 2]}\n",
        "a: {<<: }\n",
        "a: {<<: !t {x: 1}}\n",
    ] {
        let e = from_str_with::<Value>(src, &on).unwrap_err();
        assert_eq!(e.code(), code::INVALID_MERGE_VALUE, "{src}");
        let e = from_str_with::<BTreeMap<String, BTreeMap<String, u8>>>(src, &on).unwrap_err();
        assert_eq!(e.code(), code::INVALID_MERGE_VALUE, "typed {src}");
    }
    // Two `<<` keys are duplicate keys.
    let src = "a: &a {x: 1}\nb: &b {x: 2, y: 2}\nc:\n  <<: *a\n  <<: *b\n";
    assert_eq!(
        from_str_with::<Value>(src, &on).unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    // Under LastWins the last `<<` wins, with a warning.
    let lw = on.clone().duplicate_keys(DuplicateKeys::LastWins);
    let (v, w): (Value, _) = from_str_with_diagnostics(src, &lw).unwrap();
    assert_eq!(v["c"], yaml("{x: 2, y: 2}"));
    assert_eq!(w[0].code, code::DUPLICATE_KEY_LAST_WINS);
    let Streamed(s) = from_str_with(src, &lw).unwrap();
    assert_eq!(s, v);
}

#[test]
fn duplicate_keys() {
    let src = "a: 1\nb: 2\na: 3\n";
    let e = from_str::<Value>(src).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    assert_eq!(e.location().unwrap().line(), 3);
    let first = &e.diagnostic().secondary[0];
    assert_eq!(first.message, "first defined here");
    assert_eq!(first.span.start, 0);
    // Streamed targets too, including maps and ignored content.
    assert_eq!(
        from_str::<BTreeMap<String, u8>>(src).unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    #[derive(Debug, Deserialize)]
    struct B {
        #[allow(dead_code)]
        b: u8,
    }
    assert_eq!(from_str::<B>(src).unwrap_err().code(), code::DUPLICATE_KEY);
    // Keys are compared by meaning: `1` and `0x1` are the same integer,
    // `1` and `'1'` are not.
    assert_eq!(
        from_str::<Value>("{1: a, 0x1: b}").unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    assert!(from_str::<Value>("{1: a, '1': b}").is_ok());
    assert_eq!(
        from_str::<Value>("{[a]: 1, [a]: 2}").unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    assert_eq!(
        from_str::<Streamed>("{[a]: 1, [a]: 2}").unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    // Spec §3.2.1.1: mapping keys are unordered, so reordered
    // complex mapping keys are the same key, on every consumer.
    let reordered = "? {a: 1, b: 2}\n: x\n? {b: 2, a: 1}\n: y\n";
    let e = from_str::<Value>(reordered).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    assert_eq!(e.location().unwrap().line(), 3);
    assert_eq!(
        from_str::<Streamed>(reordered).unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    assert_eq!(
        from_str::<Value>("{{a: 1, b: {c: 2, d: 3}}: x, {b: {d: 3, c: 2}, a: 1}: y}")
            .unwrap_err()
            .code(),
        code::DUPLICATE_KEY
    );
    // Different values under the same keys are different keys.
    assert!(from_str::<Value>("? {a: 1, b: 2}\n: x\n? {b: 1, a: 2}\n: y\n").is_ok());
    let (v, w): (Value, _) = from_str_with_diagnostics(
        reordered,
        &Settings::new().duplicate_keys(DuplicateKeys::LastWins),
    )
    .unwrap();
    assert_eq!(v.as_mapping().unwrap().len(), 1);
    assert_eq!(w[0].code, code::DUPLICATE_KEY_LAST_WINS);
    assert_eq!(v, yaml("? {b: 2, a: 1}\n: y\n"));
    // Compat: last wins (first position), always with a warning.
    for s in [
        Compat::serde_yaml(),
        Settings::new().duplicate_keys(DuplicateKeys::LastWins),
    ] {
        let (v, w): (Value, _) = from_str_with_diagnostics(src, &s).unwrap();
        assert_eq!(v, yaml("{a: 3, b: 2}"));
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].code, code::DUPLICATE_KEY_LAST_WINS);
        assert_eq!(w[0].secondary[0].message, "first defined here");
        let Streamed(t) = from_str_with(src, &s).unwrap();
        assert_eq!(t, v, "builder and stream differ");
        #[derive(Deserialize)]
        struct A {
            a: u8,
        }
        assert_eq!(from_str_with::<A>(src, &s).unwrap().a, 3);
    }
}

#[test]
fn resolver_errors_have_codes() {
    let cases = [
        ("!!int twelve", code::INVALID_SCALAR_FOR_TAG),
        ("!!bool yes", code::INVALID_SCALAR_FOR_TAG),
        ("!!seq a", code::COLLECTION_TAG_ON_SCALAR),
        ("!!str [a]", code::WRONG_TAG_ON_COLLECTION),
        ("!!map [a]", code::WRONG_TAG_ON_COLLECTION),
    ];
    for (src, c) in cases {
        assert_eq!(from_str::<Value>(src).unwrap_err().code(), c, "{src}");
        assert_eq!(
            from_str::<Streamed>(src).unwrap_err().code(),
            c,
            "streamed {src}"
        );
    }
    let json = Settings::new().schema(Schema::Json);
    let e = from_str_with::<Value>("[1, hello]", &json).unwrap_err();
    assert_eq!(e.code(), code::JSON_UNMATCHED_SCALAR);
    assert_eq!(e.path().unwrap().to_string(), "[1]");
    assert_eq!(
        from_str_with::<Value>("[1, 'hello']", &json).unwrap()[1],
        Value::from("hello")
    );
    // Failsafe: everything is a string.
    let fs = Settings::new().schema(Schema::Failsafe);
    assert_eq!(from_str_with::<Value>("1", &fs).unwrap(), Value::from("1"));
}

#[test]
fn settings_diagnostics_and_yaml11_int_forms() {
    let s = Settings::new().yaml11_int_forms(true);
    assert_eq!(from_str_with::<i32>("-0x1F", &s).unwrap(), -31);
    assert_eq!(from_str_with::<u8>("0b101", &s).unwrap(), 5);
    assert_eq!(from_str::<String>("0b101").unwrap(), "0b101");
    assert_eq!(from_str::<Value>("0b101").unwrap(), Value::from("0b101"));
    // No signed `0o` (neither YAML 1.1 nor 1.2), even with every 1.1
    // behaviour on; a quoted-looking read into an integer is refused.
    for set in [s.clone(), Compat::yaml_1_1()] {
        assert_eq!(
            from_str_with::<Value>("-0o17", &set).unwrap(),
            Value::from("-0o17")
        );
        assert_eq!(
            from_str_with::<i32>("-0o17", &set).unwrap_err().code(),
            code::TYPE_MISMATCH
        );
    }
    assert_eq!(
        from_str_with::<i32>("-017", &Compat::yaml_1_1()).unwrap(),
        -15
    );
    // Meaningless combination: a warning, returned by *_with_diagnostics
    // and ignored by from_str_with.
    let json = Settings::new().schema(Schema::Json).yaml11_int_forms(true);
    let (v, w): (Value, _) = from_str_with_diagnostics("1", &json).unwrap();
    assert_eq!(v, Value::from(1));
    assert_eq!(w[0].code, code::MEANINGLESS_SETTINGS);
    assert!(from_str_with::<Value>("1", &json).is_ok());
    // `%YAML 1.1` warnings come through too, and honouring the directive
    // switches the document to 1.1 typing.
    let (v, w): (Value, _) =
        from_str_with_diagnostics("%YAML 1.1\n--- yes\n", &Settings::new()).unwrap();
    assert_eq!(v, Value::from("yes"));
    assert_eq!(w[0].code, code::YAML_VERSION_1_1);
    let honor = Settings::new().honor_version_directive(true);
    assert_eq!(
        from_str_with::<Value>("%YAML 1.1\n--- 0b11\n", &honor).unwrap(),
        Value::from(3)
    );
    // Errors carry the warnings recorded before them.
    let e = from_str_with::<u8>("%YAML 1.1\n--- x\n", &Settings::new()).unwrap_err();
    assert_eq!(e.warnings()[0].code, code::YAML_VERSION_1_1);
}

#[test]
fn multi_document_deserializer() {
    let src = "a: 1\n---\n[2]\n--- 3\n";
    let docs: Vec<Value> = Deserializer::from_str(src)
        .map(Value::deserialize)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(docs, vec![yaml("a: 1"), yaml("[2]"), Value::from(3)]);
    // Skipping unread documents.
    let mut it = Deserializer::from_str(src);
    let _first = it.next().unwrap();
    let second = it.next().unwrap();
    let third = it.next().unwrap();
    assert_eq!(u8::deserialize(third).unwrap(), 3);
    assert!(it.next().is_none());
    let e = Vec::<u8>::deserialize(second).unwrap_err();
    assert_eq!(e.code(), code::DOCUMENT_UNAVAILABLE);
    // Requesting the next document skips an unread one.
    let mut it = Deserializer::from_str(src);
    let first = it.next().unwrap();
    let _second = it.next().unwrap();
    assert_eq!(
        Value::deserialize(first).unwrap_err().code(),
        code::DOCUMENT_UNAVAILABLE
    );
    // The stream itself cannot be read once iterated.
    let mut it = Deserializer::from_str(src);
    let _ = it.next();
    assert_eq!(
        Value::deserialize(it).unwrap_err().code(),
        code::DOCUMENT_UNAVAILABLE
    );
    // An error ends the stream after being reported once.
    let mut it = Deserializer::from_str("1\n--- [\n--- 3\n");
    assert_eq!(u8::deserialize(it.next().unwrap()).unwrap(), 1);
    let bad = it.next().unwrap();
    assert_eq!(
        Value::deserialize(bad).unwrap_err().kind(),
        ErrorKind::Syntax
    );
    assert!(it.next().is_none());
    // Once an error is yielded the iterator is fused, whether
    // the failing handle is read or not.
    for src in ["{a: 1, a: 2}\n--- 5\n", "[\n--- 5\n", "["] {
        assert!(
            Deserializer::from_str(src).take(1000).count() <= 2,
            "{src:?}"
        );
    }
    assert_eq!(Deserializer::from_str("{a: 1, a: 2}\n--- 5\n").count(), 2);
    assert_eq!(Deserializer::from_str("[\n--- 5\n").count(), 2);
    let bad = Settings::new().limits(Limits::new().max_depth(0));
    assert!(Deserializer::from_str_with("[1]", &bad).count() <= 2);
    let mut it = Deserializer::from_str("{a: 1, a: 2}\n--- 5\n");
    let _unread = it.next().unwrap();
    let failed = it.next().unwrap();
    assert!(it.next().is_none());
    assert_eq!(
        Value::deserialize(failed).unwrap_err().code(),
        code::DUPLICATE_KEY
    );
    assert!(it.next().is_none());
    // Used directly: exactly one document.
    assert_eq!(u8::deserialize(Deserializer::from_str("7")).unwrap(), 7);
    assert_eq!(
        u8::deserialize(Deserializer::from_str("7\n--- 8"))
            .unwrap_err()
            .code(),
        code::MULTIPLE_DOCUMENTS
    );
    // Output-only settings conflicts do not affect reading.
    let bad = Settings::new().emit(oneyaml::Emit::new().indent(0));
    assert!(Value::deserialize(Deserializer::from_str_with("1", &bad)).is_ok());
    assert_eq!(Deserializer::from_str("").count(), 0);
}

#[test]
fn slices_and_readers() {
    let utf16: Vec<u8> = "\u{feff}a: é\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let v: Value = from_slice(&utf16).unwrap();
    assert_eq!(v["a"], Value::from("é"));
    let e = from_slice::<Value>(b"a: \xff\n").unwrap_err();
    assert_eq!(e.code(), code::INVALID_UTF8);
    assert_eq!(e.kind(), ErrorKind::Syntax);
    let v: Value = from_reader(&b"[1, 2]"[..]).unwrap();
    assert_eq!(v, yaml("[1, 2]"));
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk on fire"))
        }
    }
    let e = from_reader::<_, Value>(Broken).unwrap_err();
    assert_eq!(e.code(), code::IO_ERROR);
    assert_eq!(e.kind(), ErrorKind::Io);
    let small = Settings::new().limits(Limits::default().max_document_size(2));
    let e = from_reader_with::<_, Value>(&b"[1, 2, 3]"[..], &small).unwrap_err();
    assert_eq!(e.code(), code::DOCUMENT_TOO_LARGE);
}

#[test]
fn value_serialize_feeds_other_formats() {
    let v = yaml("{a: [1, -2, 1.5, true, ~, x], b: !u c, 7: {}}");
    let j = serde_json::to_value(&v).unwrap();
    assert_eq!(
        j,
        serde_json::json!({"a": [1, -2, 1.5, true, null, "x"], "b": {"!u": "c"}, "7": {}})
    );
    assert_eq!(serde_json::to_string(&yaml("[1, x]")).unwrap(), "[1,\"x\"]");
    // Value round-trips through another data model.
    let back: Value =
        serde_json::from_value(serde_json::to_value(yaml("{a: [1, x]}")).unwrap()).unwrap();
    assert_eq!(back, yaml("{a: [1, x]}"));
    // Mapping / Sequence / Number deserialize and serialize on their own.
    let m: oneyaml::Mapping = from_str("{a: 1}").unwrap();
    assert_eq!(serde_json::to_string(&m).unwrap(), "{\"a\":1}");
    let s: oneyaml::Sequence = from_str("[1]").unwrap();
    assert_eq!(serde_json::to_string(&s).unwrap(), "[1]");
    let n: oneyaml::Number = from_str("-3").unwrap();
    assert_eq!(serde_json::to_string(&n).unwrap(), "-3");
    assert!(from_str::<oneyaml::Mapping>("[1]").is_err());
    assert!(from_str::<oneyaml::Sequence>("{}").is_err());
    assert!(from_str::<oneyaml::Number>("x").is_err());
    // Too deep to serialize: Y2007 (max_typed_depth).
    let mut deep = Value::Null;
    for _ in 0..1100 {
        deep = Value::from(vec![deep]);
    }
    let e = serde_json::to_string(&deep).unwrap_err();
    assert!(e.to_string().contains("Y2007"), "{e}");
    let Value::Sequence(seq) = deep.clone() else {
        unreachable!()
    };
    assert!(serde_json::to_string(&seq).is_err());
    let mut m = oneyaml::Mapping::new();
    m.insert(Value::Null, deep);
    assert!(serde_json::to_string(&m).is_err());
}
