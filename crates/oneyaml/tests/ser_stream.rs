//! The streaming serializer's contracts (no unsafe,
//! no panic) — a failed document writes nothing (`to_string`, `to_writer`,
//! and a multi-document `Serializer`), the limits keep their codes
//! (`Y2005`, `Y2007`, `Y3016`, `Y3014`), a `Value` inside typed data is
//! handed over whole, and deep inputs are refused or written on a small
//! stack, never a crash. Byte equality with the old build-then-emit path
//! is `crates/bench/tests/ser_differential.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use oneyaml::{Limits, Serializer, Settings, Tag, TaggedValue, Value, code};
use serde::Serialize;
use serde::ser::{self, SerializeMap, SerializeSeq, SerializeStruct, Serializer as _};

/// A sequence of `ok` numbers, then a custom error (`Y3012`) midway.
struct FailsMidway {
    ok: usize,
}

impl Serialize for FailsMidway {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        for i in 0..self.ok {
            seq.serialize_element(&i)?;
        }
        seq.serialize_element(&Boom)?;
        seq.end()
    }
}

struct Boom;

impl Serialize for Boom {
    fn serialize<S: ser::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(ser::Error::custom("boom"))
    }
}

#[derive(Serialize)]
struct Doc {
    name: &'static str,
    items: FailsMidway,
}

#[test]
fn a_failed_document_writes_nothing() {
    let bad = Doc {
        name: "x",
        items: FailsMidway { ok: 3 },
    };
    let e = oneyaml::to_string(&bad).unwrap_err();
    assert_eq!(e.code(), code::SERIALIZE_CUSTOM);
    assert_eq!(e.path().unwrap().to_string(), "items[3]");

    let mut out = Vec::new();
    let e = oneyaml::to_writer(&mut out, &bad).unwrap_err();
    assert_eq!(e.code(), code::SERIALIZE_CUSTOM);
    assert!(out.is_empty());

    // Multi-document: the failed document leaves no text, and the next
    // one is still the second document of the stream.
    let mut out = Vec::new();
    let mut ser = Serializer::new(&mut out);
    1.serialize(&mut ser).unwrap();
    assert!(bad.serialize(&mut ser).is_err());
    assert!(FailsMidway { ok: 0 }.serialize(&mut ser).is_err());
    assert_eq!(ser.documents(), 1);
    vec!["a"].serialize(&mut ser).unwrap();
    assert_eq!(ser.documents(), 2);
    ser.into_inner().unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "1\n---\n- a\n");
}

#[test]
fn a_document_collection_abandoned_midway_writes_nothing() {
    let mut out = Vec::new();
    let mut ser = Serializer::new(&mut out);
    {
        let mut seq = (&mut ser).serialize_seq(None).unwrap();
        seq.serialize_element(&1).unwrap();
        assert_eq!(
            seq.serialize_element(&Boom).unwrap_err().code(),
            code::SERIALIZE_CUSTOM
        );
        // Dropped without `end`, as `?` does.
    }
    assert_eq!(ser.documents(), 0);
    BTreeMap::from([("k", 1)]).serialize(&mut ser).unwrap();
    ser.into_inner().unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "k: 1\n");
}

/// Swallows its element's error and goes on.
struct Swallows;

impl Serialize for Swallows {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        let _ignored = seq.serialize_element(&Doc {
            name: "half",
            items: FailsMidway { ok: 1 },
        });
        seq.serialize_element(&2)?;
        seq.end()
    }
}

#[derive(Serialize)]
enum Wrap {
    N(Boom),
    T(Boom, u8),
}

/// Swallows a variant whose content fails before any serializer call,
/// then writes `1` (the pending variant must not wrap it).
struct SwallowsVariant(Wrap);

impl Serialize for SwallowsVariant {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        let _ignored = seq.serialize_element(&self.0);
        seq.serialize_element(&1)?;
        seq.end()
    }
}

#[test]
fn a_swallowed_variant_error_is_y3016() {
    for wrap in [Wrap::N(Boom), Wrap::T(Boom, 1)] {
        let v = SwallowsVariant(wrap);
        for s in [Settings::new(), Settings::new().max_depth(1)] {
            let e = oneyaml::to_string_with(&v, &s).unwrap_err();
            assert_eq!(e.code(), code::INVALID_SERIALIZE_CALLS);
        }
    }
    // Past max_depth too: the swallowed error decides.
    struct DeepThenSwallow;
    impl Serialize for DeepThenSwallow {
        fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut seq = s.serialize_seq(None)?;
            seq.serialize_element(&vec![vec![1]])?;
            let _ignored = seq.serialize_element(&Boom);
            seq.end()
        }
    }
    let e = oneyaml::to_string_with(&DeepThenSwallow, &Settings::new().max_depth(1)).unwrap_err();
    assert_eq!(e.code(), code::INVALID_SERIALIZE_CALLS);
}

#[test]
fn a_swallowed_error_still_fails_the_document() {
    // The failed element's text is already written: the document is
    // refused rather than written with half an element (Y3016).
    let e = oneyaml::to_string(&Swallows).unwrap_err();
    assert_eq!(e.code(), code::INVALID_SERIALIZE_CALLS);
    let mut out = Vec::new();
    let mut ser = Serializer::new(&mut out);
    assert_eq!(
        Swallows.serialize(&mut ser).unwrap_err().code(),
        code::INVALID_SERIALIZE_CALLS
    );
    assert_eq!(ser.documents(), 0);
    ser.into_inner().unwrap();
    assert!(out.is_empty());
}

#[derive(Serialize)]
struct Tree(Vec<Tree>);

fn tree(depth: usize) -> Tree {
    let mut t = Tree(Vec::new());
    for _ in 0..depth {
        t = Tree(vec![t]);
    }
    t
}

fn nested_value(depth: usize) -> Value {
    let mut v = Value::Null;
    for _ in 0..depth {
        v = Value::from(vec![v]);
    }
    v
}

#[test]
fn limits_keep_their_codes() {
    // Y2005: the output nests deeper than max_depth (typed data).
    let s = Settings::new().max_depth(3);
    assert_eq!(
        oneyaml::to_string_with(&vec![vec![vec![1]]], &s).unwrap(),
        "- - - 1\n"
    );
    let e = oneyaml::to_string_with(&vec![vec![vec![vec![1]]]], &s).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    // Empty collections count, enum variants in mapping form count.
    let e = oneyaml::to_string_with(&vec![vec![vec![Vec::<u8>::new()]]], &s).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    #[derive(Serialize)]
    enum E {
        N(u8),
    }
    assert!(oneyaml::to_string_with(&vec![vec![E::N(1)]], &s).is_ok());
    let e = oneyaml::to_string_with(&vec![vec![vec![E::N(1)]]], &s).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    // A map key's own depth counts, as in `Value::depth`.
    let key = BTreeMap::from([(vec![vec![1]], 1)]);
    assert!(oneyaml::to_string_with(&key, &s).is_ok());
    let deep_key = BTreeMap::from([(vec![vec![vec![1]]], 1)]);
    let e = oneyaml::to_string_with(&deep_key, &s).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    // A handed-over Value, too.
    let e = oneyaml::to_string_with(&vec![nested_value(3)], &s).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    // And through the document Serializer, which then writes nothing.
    let mut out = Vec::new();
    let mut ser = Serializer::with_settings(&mut out, &s);
    let e = vec![vec![vec![vec![1]]]].serialize(&mut ser).unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
    ser.into_inner().unwrap();
    assert!(out.is_empty());

    // Y2007: serde's own recursion into a recursive type.
    let typed = Settings::new().limits(Limits::default().max_typed_depth(8));
    // Each `Tree` level is two serde levels (the newtype, its `Vec`), as
    // in the `Value` build.
    assert!(oneyaml::to_string_with(&tree(3), &typed).is_ok());
    assert!(oneyaml::to_value_with(tree(3), &typed).is_ok());
    let e = oneyaml::to_string_with(&tree(4), &typed).unwrap_err();
    assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
    let e = oneyaml::to_value_with(tree(4), &typed).unwrap_err();
    assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
}

#[test]
fn deep_typed_data_is_refused_within_the_documented_stack() {
    // Serde's recursion is refused at the default max_typed_depth, which
    // fits a 2 MiB stack (ser/mod.rs, *Stack use*); the test's own derived
    // `Tree` drops recursively, so it stays shallow enough for this stack.
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let e = oneyaml::to_string(&tree(300)).unwrap_err();
            assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_values_never_crash_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            // A deep Value is handed over whole and written iteratively
            // (no recursion), refused past max_depth or written when unlimited.
            let v = vec![nested_value(1_000_000)];
            let e = oneyaml::to_string(&v).unwrap_err();
            assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
            let s = Settings::new().limits(Limits::unlimited());
            let out = oneyaml::to_string_with(&v, &s).unwrap();
            assert!(out.starts_with("- - - "));
            let mut buf = Vec::new();
            let mut ser = Serializer::with_settings(&mut buf, &s);
            v.serialize(&mut ser).unwrap();
            ser.into_inner().unwrap();
            assert_eq!(buf, out.as_bytes());
        })
        .unwrap()
        .join()
        .unwrap();
}

fn nested_mapping(depth: usize) -> Value {
    let mut v = Value::Null;
    for _ in 0..depth {
        let mut m = oneyaml::Mapping::new();
        m.insert(Value::from("a"), v);
        v = Value::from(m);
    }
    v
}

/// Serializes one entry whose key is `.0` (a map key built as a `Value`).
struct KeyedBy<'a>(&'a Value);

impl Serialize for KeyedBy<'_> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry(self.0, &1)?;
        m.end()
    }
}

/// A nested *mapping* chain writes about
/// `indent × d² / 2` bytes, so it must be refused before any of it is
/// written. At 100,000 levels, writing first would be ~10 GB (it was:
/// 0.7 s and 923 MB at 30,000 levels before the fix, OOM at 100,000).
#[test]
fn deep_mapping_chains_are_refused_before_writing() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let deep = nested_mapping(100_000);
            let started = std::time::Instant::now();
            for e in [
                oneyaml::to_string(&deep).unwrap_err(),
                oneyaml::to_string(&vec![&deep]).unwrap_err(),
                oneyaml::to_string(&BTreeMap::from([("k", &deep)])).unwrap_err(),
                oneyaml::to_string(&KeyedBy(&deep)).unwrap_err(),
                deep.to_yaml_string().unwrap_err(),
            ] {
                assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
            }
            let mut out = Vec::new();
            let mut ser = Serializer::new(&mut out);
            let e = vec![&deep].serialize(&mut ser).unwrap_err();
            assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
            ser.into_inner().unwrap();
            assert!(out.is_empty());
            // Linear work (clone and a walk that stops at the limit), not
            // quadratic output: generous for a debug build.
            assert!(started.elapsed() < std::time::Duration::from_secs(20));
            // Within the limit, the same chain is written (1,024 levels,
            // about 1 MB), and one level more is refused.
            let ok = nested_mapping(1024);
            assert_eq!(oneyaml::to_string(&ok).unwrap(), ok_text(1024, 0));
            let e = oneyaml::to_string(&vec![&ok]).unwrap_err();
            assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
            let ok = nested_mapping(1023);
            assert_eq!(oneyaml::to_string(&vec![&ok]).unwrap(), ok_text(1023, 2));
        })
        .unwrap()
        .join()
        .unwrap();
}

/// The expected block text of `nested_mapping(d)` starting at column
/// `col`, written after `- ` when `col > 0`.
fn ok_text(d: usize, col: usize) -> String {
    let mut s = String::new();
    if col > 0 {
        s.push_str("- ");
    }
    for i in 0..d {
        if i > 0 {
            s.push('\n');
            s.push_str(&" ".repeat(col.saturating_add(i.saturating_mul(2))));
        }
        s.push_str("a:");
    }
    s.push_str(" null\n");
    s
}

/// Drives a map out of order.
enum MapMisuse {
    KeyTwice,
    ValueFirst,
    EndAfterKey,
}

impl Serialize for MapMisuse {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        match self {
            MapMisuse::KeyTwice => {
                m.serialize_key("a")?;
                m.serialize_key("b")?;
            }
            MapMisuse::ValueFirst => m.serialize_value(&1)?,
            MapMisuse::EndAfterKey => m.serialize_key("a")?,
        }
        m.end()
    }
}

/// A struct with the same field twice, after `n` distinct fields.
struct DupField(usize);

const NAMES: [&str; 12] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];

impl Serialize for DupField {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("DupField", self.0.saturating_add(1))?;
        for name in NAMES.iter().take(self.0) {
            st.serialize_field(name, &1)?;
        }
        st.serialize_field("a", &2)?;
        st.end()
    }
}

#[test]
fn protocol_and_duplicate_errors() {
    for (misuse, root) in [
        (MapMisuse::KeyTwice, true),
        (MapMisuse::ValueFirst, true),
        (MapMisuse::EndAfterKey, true),
        (MapMisuse::KeyTwice, false),
        (MapMisuse::ValueFirst, false),
        (MapMisuse::EndAfterKey, false),
    ] {
        let e = if root {
            oneyaml::to_string(&misuse).unwrap_err()
        } else {
            oneyaml::to_string(&vec![misuse]).unwrap_err()
        };
        assert_eq!(e.code(), code::INVALID_SERIALIZE_CALLS);
    }
    let mut out = Vec::new();
    let mut ser = Serializer::new(&mut out);
    for misuse in [
        MapMisuse::KeyTwice,
        MapMisuse::ValueFirst,
        MapMisuse::EndAfterKey,
    ] {
        let e = misuse.serialize(&mut ser).unwrap_err();
        assert_eq!(e.code(), code::INVALID_SERIALIZE_CALLS);
    }
    ser.into_inner().unwrap();
    assert!(out.is_empty());

    // Y3014 with field names held inline (few fields) and in a set.
    for n in [1, 7, 8, 11] {
        let e = oneyaml::to_string(&DupField(n)).unwrap_err();
        assert_eq!(e.code(), code::SERIALIZE_DUPLICATE_KEY, "{n}");
        assert_eq!(e.path().unwrap().to_string(), "a", "{n}");
    }
    // Map keys: compared one by one, then in a set; collection keys by
    // meaning (order-independent mappings).
    struct DupKeys(usize);
    impl Serialize for DupKeys {
        fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut m = s.serialize_map(None)?;
            for i in 0..self.0 {
                m.serialize_entry(&i, &i)?;
            }
            m.serialize_entry(&0, &0)?;
            m.end()
        }
    }
    for n in [1, 8, 9, 30] {
        let e = oneyaml::to_string(&DupKeys(n)).unwrap_err();
        assert_eq!(e.code(), code::SERIALIZE_DUPLICATE_KEY, "{n}");
    }
    let a: BTreeMap<&str, u8> = BTreeMap::from([("x", 1), ("y", 2)]);
    let b: Value = oneyaml::from_str("{y: 2, x: 1}").unwrap();
    struct TwoKeys<'a>(&'a BTreeMap<&'a str, u8>, &'a Value);
    impl Serialize for TwoKeys<'_> {
        fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut m = s.serialize_map(None)?;
            m.serialize_entry(self.0, &1)?;
            m.serialize_entry(self.1, &2)?;
            m.end()
        }
    }
    let e = oneyaml::to_string(&TwoKeys(&a, &b)).unwrap_err();
    assert_eq!(e.code(), code::SERIALIZE_DUPLICATE_KEY);
}

#[test]
fn values_inside_typed_data_are_handed_over() {
    #[derive(Serialize)]
    struct Holder {
        tagged: Value,
        list: Vec<Value>,
        #[serde(flatten)]
        rest: oneyaml::Mapping,
    }
    let tagged = Value::from(TaggedValue {
        tag: Tag::new("!t"),
        value: Value::from("x"),
    });
    let mut rest = oneyaml::Mapping::new();
    rest.insert(Value::from("r"), tagged.clone());
    let h = Holder {
        tagged: tagged.clone(),
        list: vec![tagged.clone(), Value::from(vec![])],
        rest,
    };
    assert_eq!(
        oneyaml::to_string(&h).unwrap(),
        "tagged: !t x\nlist:\n  - !t x\n  - []\nr: !t x\n"
    );
}

#[test]
fn settings_errors_come_first_and_write_nothing() {
    let bad = Settings::new().indent(0);
    let e = oneyaml::to_string_with(&vec![Boom], &bad).unwrap_err();
    assert_eq!(e.code(), code::INCORRECT_SETTINGS);
    let mut out = Vec::new();
    let mut ser = Serializer::with_settings(&mut out, &bad);
    assert_eq!(
        1.serialize(&mut ser).unwrap_err().code(),
        code::INCORRECT_SETTINGS
    );
    assert_eq!(
        vec![1].serialize(&mut ser).unwrap_err().code(),
        code::INCORRECT_SETTINGS
    );
    ser.into_inner().unwrap();
    assert!(out.is_empty());
}

#[test]
fn the_serializer_reuses_its_buffer_across_documents() {
    // A stream of documents is not one allocation per document
    // pattern-for-pattern of the old path; only its bytes are checked here
    // (counts are gated in oracles/tests/ser_allocations.rs).
    let mut out = Vec::new();
    let mut ser = Serializer::new(&mut out);
    for i in 0..3 {
        BTreeMap::from([("i", i)]).serialize(&mut ser).unwrap();
    }
    ser.into_inner().unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "i: 0\n---\ni: 1\n---\ni: 2\n"
    );
}

/// An error raised inside a root collection written
/// through the document `Serializer` carries the same path (and the same
/// text) as `to_string`'s.
#[test]
fn serializer_errors_carry_the_path_of_to_string() {
    struct Nope;
    impl Serialize for Nope {
        fn serialize<S: ser::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(ser::Error::custom("nope"))
        }
    }
    fn check<T: Serialize>(v: &T, path: &str) {
        let want = oneyaml::to_string(v).unwrap_err();
        assert_eq!(want.code(), code::SERIALIZE_CUSTOM);
        assert_eq!(want.path().unwrap().to_string(), path);
        let mut out = Vec::new();
        let mut ser = Serializer::new(&mut out);
        let got = v.serialize(&mut ser).unwrap_err();
        assert_eq!(got.code(), code::SERIALIZE_CUSTOM);
        assert_eq!(got.path().unwrap().to_string(), path);
        assert_eq!(got.to_string(), want.to_string());
        assert!(got.to_string().contains("nope"), "{got}");
        assert!(got.to_string().contains(&format!("at {path}")), "{got}");
        ser.into_inner().unwrap();
        assert!(out.is_empty());
    }
    check(&vec![Nope], "[0]");
    check(&BTreeMap::from([("b", vec![Nope])]), "b[0]");
    check(&BTreeMap::from([("k", Nope)]), "k");
}
