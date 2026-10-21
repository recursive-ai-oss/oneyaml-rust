//! Typed reads with merge keys stream their mappings: only `<<` values are
//! buffered. They accept and reject what the `Value` read does, read the
//! same values as a fully planned read (the planner `DuplicateKeys::LastWins`
//! uses, the oracle here: on input without duplicate keys it reads the same
//! content), and report errors as typed reads with merge keys off do: the
//! first one met while reading.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::Path;

use oneyaml::{
    Deserializer as Docs, Document, DuplicateKeys, Error, Limits, Settings, Value, code,
};
use oneyaml::{from_str_with, from_str_with_diagnostics};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// Any content, read through the typed visitor protocol (never the
/// `Value` builder), keeping mapping order.
#[derive(Debug, PartialEq)]
enum Any {
    Unit,
    Bool(bool),
    Int(i128),
    Uint(u128),
    /// The bits, so NaN compares equal to itself.
    Float(u64),
    Str(String),
    Bytes(Vec<u8>),
    Some(Box<Any>),
    Seq(Vec<Any>),
    Map(Vec<(Any, Any)>),
    Enum(String, Box<Any>),
}

impl<'de> Deserialize<'de> for Any {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(AnyVisitor)
    }
}

struct AnyVisitor;

impl<'de> Visitor<'de> for AnyVisitor {
    type Value = Any;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("anything")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Any, E> {
        Ok(Any::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Any, E> {
        Ok(Any::Int(v.into()))
    }
    fn visit_i128<E>(self, v: i128) -> Result<Any, E> {
        Ok(Any::Int(v))
    }
    fn visit_u64<E>(self, v: u64) -> Result<Any, E> {
        Ok(Any::Uint(v.into()))
    }
    fn visit_u128<E>(self, v: u128) -> Result<Any, E> {
        Ok(Any::Uint(v))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Any, E> {
        Ok(Any::Float(v.to_bits()))
    }
    fn visit_str<E>(self, v: &str) -> Result<Any, E> {
        Ok(Any::Str(v.to_owned()))
    }
    fn visit_bytes<E>(self, v: &[u8]) -> Result<Any, E> {
        Ok(Any::Bytes(v.to_vec()))
    }
    fn visit_unit<E>(self) -> Result<Any, E> {
        Ok(Any::Unit)
    }
    fn visit_none<E>(self) -> Result<Any, E> {
        Ok(Any::Unit)
    }
    fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Any, D::Error> {
        Any::deserialize(d).map(|a| Any::Some(Box::new(a)))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Any, A::Error> {
        let mut v = Vec::new();
        while let Some(x) = a.next_element()? {
            v.push(x);
        }
        Ok(Any::Seq(v))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Any, A::Error> {
        let mut v = Vec::new();
        while let Some(e) = a.next_entry()? {
            v.push(e);
        }
        Ok(Any::Map(v))
    }
    fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<Any, A::Error> {
        use de::VariantAccess;
        let (name, variant): (String, _) = a.variant()?;
        let content: Any = variant.newtype_variant()?;
        Ok(Any::Enum(name, Box::new(content)))
    }
}

/// A docker-compose-like model, for type errors and unknown fields.
#[derive(Debug, PartialEq, Deserialize)]
#[allow(dead_code)]
struct Compose {
    services: Option<BTreeMap<String, Service>>,
    version: Option<String>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[allow(dead_code)]
struct Service {
    image: Option<String>,
    ports: Option<Vec<u16>>,
    environment: Option<BTreeMap<String, String>>,
    restart: Option<String>,
}

/// (code, primary span, path, message) of an error.
fn shape(e: &Error) -> (oneyaml::Code, oneyaml::Span, String, String) {
    (
        e.code(),
        e.diagnostic().primary.span,
        e.path().map(ToString::to_string).unwrap_or_default(),
        e.to_string(),
    )
}

fn outcome<T: fmt::Debug>(
    r: Result<T, Error>,
) -> Result<String, (oneyaml::Code, oneyaml::Span, String, String)> {
    r.map(|v| format!("{v:?}")).map_err(|e| shape(&e))
}

fn merge() -> Settings {
    Settings::new().merge_keys(true)
}

/// The fully planned read (merge keys, duplicates tolerated).
fn planned() -> Settings {
    merge().duplicate_keys(DuplicateKeys::LastWins)
}

/// An error only a typed read has: the target's refusal, or its nesting
/// past `Limits::max_typed_depth`.
fn typed_only(e: &Error) -> bool {
    e.code().range() == oneyaml::CodeRange::Serde || e.code() == code::TYPED_DEPTH_EXCEEDED
}

/// Reads the first entry of a mapping (a value ignored), then stops: the
/// rest is still checked as the `Value` read checks it.
#[derive(Debug, PartialEq)]
struct First(Option<String>);

impl<'de> Deserialize<'de> for First {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = First;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<First, A::Error> {
                let e: Option<(String, IgnoredAny)> = a.next_entry()?;
                Ok(First(e.map(|x| x.0)))
            }
        }
        d.deserialize_map(V)
    }
}

/// A typed read of one input: a whole document, or one node of a
/// `Document`, under merge keys (`planned`: duplicates tolerated, the
/// fully planned read).
trait Reader {
    fn read<T: DeserializeOwned>(&self, planned: bool) -> Result<T, Error>;
    fn label(&self) -> String;
}

struct Whole<'a>(&'a str);

impl Reader for Whole<'_> {
    fn read<T: DeserializeOwned>(&self, p: bool) -> Result<T, Error> {
        from_str_with(self.0, &if p { planned() } else { merge() })
    }
    fn label(&self) -> String {
        format!("{:?}", self.0)
    }
}

/// The same node of a document parsed under merge keys and under the
/// planned read's settings.
struct OneNode<'d> {
    merge: oneyaml::NodeRef<'d>,
    planned: oneyaml::NodeRef<'d>,
    label: String,
}

impl Reader for OneNode<'_> {
    fn read<T: DeserializeOwned>(&self, p: bool) -> Result<T, Error> {
        if p {
            self.planned.deserialize()
        } else {
            self.merge.deserialize()
        }
    }
    fn label(&self) -> String {
        self.label.clone()
    }
}

/// Checks one input: accept/reject and the first error's code and place
/// equal the `Value` read's (and its path, for an error inside a `<<`
/// value, where key paths are the builder's); every target rejects what
/// the `Value` read rejects, an early-stopping one included; on input the
/// `Value` read accepts, every target reads exactly what the planned read
/// does.
fn check_read(r: &impl Reader) {
    let src = r.label();
    let value = r.read::<Value>(false);
    let any = r.read::<Any>(false);
    match &value {
        Err(ve) => {
            let ae = any.as_ref().expect_err(&src);
            // Unless the target refuses something first (an integer too
            // wide for it, nesting too deep), the first error met is the
            // `Value` read's.
            assert!(
                typed_only(ae)
                    || (ae.code(), ae.diagnostic().primary.span)
                        == (ve.code(), ve.diagnostic().primary.span),
                "{src}\n{ae}\n{ve}"
            );
            let vp = ve.path().map(ToString::to_string).unwrap_or_default();
            if !typed_only(ae) && vp.split('.').any(|seg| seg.starts_with("<<")) {
                let ap = ae.path().map(ToString::to_string).unwrap_or_default();
                assert_eq!(ap, vp, "{src}\n{ae}\n{ve}");
            }
            for (who, e) in [
                ("IgnoredAny", r.read::<IgnoredAny>(false).err()),
                ("First", r.read::<First>(false).err()),
                (
                    "BTreeMap",
                    r.read::<BTreeMap<String, IgnoredAny>>(false).err(),
                ),
                ("Compose", r.read::<Compose>(false).err()),
            ] {
                assert!(e.is_some(), "{who} accepted invalid input {src}: {ve}");
            }
            return;
        }
        // Valid input: only the target may refuse it (a wide integer).
        Ok(_) => assert!(
            any.as_ref().map_or_else(typed_only, |_| true),
            "{src}: {:?}",
            any.as_ref().err()
        ),
    }
    assert_eq!(outcome(any), outcome(r.read::<Any>(true)), "{src}");
    assert_eq!(
        outcome(r.read::<First>(false)),
        outcome(r.read::<First>(true)),
        "{src}"
    );
    assert_eq!(
        outcome(r.read::<Compose>(false)),
        outcome(r.read::<Compose>(true)),
        "{src}"
    );
    assert_eq!(
        outcome(r.read::<BTreeMap<String, IgnoredAny>>(false)),
        outcome(r.read::<BTreeMap<String, IgnoredAny>>(true)),
        "{src}"
    );
    assert_eq!(
        outcome(r.read::<Vec<Any>>(false)),
        outcome(r.read::<Vec<Any>>(true)),
        "{src}"
    );
    assert_eq!(
        outcome(r.read::<HashMap<String, Any>>(false).map(|m| m.len())),
        outcome(r.read::<HashMap<String, Any>>(true).map(|m| m.len())),
        "{src}"
    );
}

/// [`check_read`] on the whole input, plus warnings: the same as the
/// `Value` read's, counts included.
fn check(src: &str) {
    check_read(&Whole(src));
    let s = merge();
    let Ok((_, vw)) = from_str_with_diagnostics::<Value>(src, &s) else {
        return;
    };
    let aw = match from_str_with_diagnostics::<Any>(src, &s) {
        Ok((_, w)) => w,
        // A target refusal carries the warnings recorded with it.
        Err(e) => e.warnings().to_vec(),
    };
    assert_eq!(aw, vw, "{src:?}");
}

/// [`check_read`] on the root and every top-level node of the input as a
/// `Document`, strictly parsed (when it parses) and recovered: a node read
/// replays anchors from outside the node, which it never checked.
fn check_nodes(src: &str) {
    let recovered = |s: &Settings| Document::parse_recovering_with(src, s).into_value();
    let strict = |s: &Settings| Document::parse_with(src, s).ok();
    let mut docs = vec![(recovered(&merge()), recovered(&planned()), "recovered")];
    if let (Some(m), Some(p)) = (strict(&merge()), strict(&planned())) {
        docs.push((m, p, "strict"));
    }
    for (m, p, how) in &docs {
        let (Some(mr), Some(pr)) = (m.root(), p.root()) else {
            continue;
        };
        let mut nodes = vec![(mr, pr, "root".to_owned())];
        match (
            mr.as_mapping(),
            pr.as_mapping(),
            mr.as_sequence(),
            pr.as_sequence(),
        ) {
            (Some(a), Some(b), _, _) => {
                for (i, ((_, x), (_, y))) in a.iter().zip(b.iter()).enumerate() {
                    nodes.push((x, y, format!("value {i}")));
                }
            }
            (_, _, Some(a), Some(b)) => {
                for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                    nodes.push((x, y, format!("item {i}")));
                }
            }
            _ => {}
        }
        for (merge, planned, at) in nodes {
            check_read(&OneNode {
                merge,
                planned,
                label: format!("{src:?} ({how}, {at})"),
            });
        }
    }
}

/// Merge shapes: sources inline, aliased, in sequences, chained, nested,
/// repeated, invalid, with explicit keys before and after.
fn synthetic() -> Vec<String> {
    let mut out = Vec::new();
    let bases = [
        "base: &b {x: 1, y: 2}\n",
        "base: &b {x: 1, y: {z: [1, 2]}, <<: {w: 9}}\n",
        "base: &b [{x: 1}, {y: 2, x: 3}]\n",
        "base: &b {x: 1, <<: &c {q: 1}}\nc2: *c\n",
        "base: &b x\n",
        // Replayed sources whose values hold merges, tags, aliases and
        // collections; and ones that are planned (a tag on the anchored
        // mapping, a complex key, a `<<` alias key).
        "o: &o {q: [1, {r: 2}]}\nbase: &b {n: {<<: [*o, {z: 1}], y: 2}, t: !!str 1, a: *o, '<<x': 1}\n",
        "base: &b !!map {x: 1, y: 2}\n",
        "base: &b {[1]: x, y: 2}\n",
        "mk: &mk <<\nbase: &b {*mk : {x: 1}, y: 2}\n",
        "base: &b {}\n",
        // Anchors holding the input's errors: a whole read refuses them
        // where they stand, a node read of a merging node where it merges
        // them.
        "base: &b {x: 1, x: 2}\n",
        "base: &b {x: !!int y}\n",
        "base: &b {n: {<<: 5}}\n",
        "base: &b {n: {k: 1, k: 2}}\n",
        "base: &b {!!int y: 1}\n",
        "base: &b {x: 1, z: [[[[[1]]]]]}\n",
    ];
    let bodies = [
        "m: {<<: *b}\n",
        "m: {a: 1, <<: *b}\n",
        "m: {k: {<<: *b}}\n",
        "m: {a: 1, <<: *b, x: 5}\n",
        "m: {x: 5, <<: *b}\n",
        "m:\n  <<: *b\n  y: 7\n",
        "m: {<<: [*b, {k: 1}]}\n",
        "m: {<<: [{x: 0}, *b]}\n",
        "m: {<<: *b, <<: {k: 1}}\n",
        "m: {<<: 1}\n",
        "m: {<<: [1]}\n",
        "m: {<<: {a: {<<: *b, x: !!int no}}}\n",
        "m: {a: 1, a: 2, <<: *b}\n",
        "m: {<<: *b, a: 1, a: 2}\n",
        "m: {<<: {<<: {<<: *b}}}\n",
        "m: {? [1] : 2, <<: *b}\n",
        "m: {? {<<: *b} : 2}\n",
        "m: {\"<<\": *b}\n",
        "m: {!!merge <<: *b}\n",
        "m: {<<: *b}\nn: [{<<: *b, x: 2}, {<<: [*b]}]\n",
        "m: {<<: *b, v: !Tag {<<: *b}}\n",
        "m: {<<: !!map {k: 1}}\n",
        "m: {<<: !custom {k: 1}}\n",
        "m: {<<: {k: 1}, k: [x, !!int y]}\n",
        "m: {<<: {k: !!int y}}\n",
        "m: {<<: *undefined}\n",
        "<<: *b\n",
        "services:\n  web:\n    <<: *b\n    image: nginx\n    ports: [80]\n",
        "services:\n  web:\n    image: nginx\n    ports: [eighty]\n    <<: {restart: always}\n",
        "services:\n  web: {<<: {image: 1}}\n",
    ];
    for base in bases {
        for body in bodies {
            out.push(format!("{base}{body}"));
        }
    }
    out
}

/// Every `in.yaml` of the yaml-test-suite and every input of the serde
/// fuzz corpus and its regressions.
fn corpus() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>, only_in_yaml: bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out, only_in_yaml);
            } else if !only_in_yaml || p.file_name().is_some_and(|n| n == "in.yaml") {
                if let Ok(bytes) = std::fs::read(&p) {
                    out.push(String::from_utf8_lossy(&bytes).into_owned());
                }
            }
        }
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = Vec::new();
    walk(
        &crates.join("test-suite/data/yaml-test-suite"),
        &mut out,
        true,
    );
    walk(&crates.join("fuzz/corpus/serde"), &mut out, false);
    walk(&crates.join("fuzz/regressions"), &mut out, false);
    assert!(out.len() > 1000, "corpus not found: {}", out.len());
    out
}

fn big_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn synthetic_merges_agree_with_the_value_read_and_the_planned_read() {
    big_stack(|| {
        for src in synthetic() {
            check(&src);
            check_nodes(&src);
        }
    });
}

#[test]
#[ignore = "slow: runs in CI"]
fn corpus_agrees_with_the_value_read_and_the_planned_read() {
    big_stack(|| {
        for src in corpus() {
            check(&src);
            check_nodes(&src);
        }
    });
}

#[test]
fn merged_entries_follow_the_explicit_ones() {
    let src = "b: &b {x: 1, y: 2, z: 3}\nm: {y: 0, <<: [*b, {w: 4, x: 9}], a: 5}\n";
    let m: BTreeMap<String, Any> = from_str_with(src, &merge()).unwrap();
    assert_eq!(
        m["m"],
        Any::Map(vec![
            (Any::Str("y".into()), Any::Uint(0)),
            (Any::Str("a".into()), Any::Uint(5)),
            (Any::Str("x".into()), Any::Uint(1)),
            (Any::Str("z".into()), Any::Uint(3)),
            (Any::Str("w".into()), Any::Uint(4)),
        ])
    );
}

/// Typed reads with merge keys report errors as with merge keys off: the
/// first met while reading, with the key as written in its path.
#[test]
fn errors_are_reported_as_with_merge_keys_off() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct S {
        a: u32,
        b: Option<u32>,
    }
    let cases = [
        // A type error before a duplicate key.
        ("a: x\nb: 1\nb: 2\n", code::TYPE_MISMATCH),
        // A type error before an invalid scalar.
        ("a: x\nb: !!int y\n", code::TYPE_MISMATCH),
    ];
    for (src, want) in cases {
        let on = from_str_with::<S>(src, &merge()).unwrap_err();
        let off = from_str_with::<S>(src, &Settings::new()).unwrap_err();
        assert_eq!(on.code(), want, "{src:?}: {on}");
        assert_eq!(shape(&on), shape(&off), "{src:?}");
        // The `Value` read reports the input's error; both reject.
        assert_ne!(
            from_str_with::<Value>(src, &merge()).unwrap_err().code(),
            want
        );
    }
    // Paths use the key as written.
    for (src, path) in [
        ("~: {a: !!int x}\n", "~.a"),
        ("0x10: {a: !!int x}\n", "0x10.a"),
    ] {
        let e = from_str_with::<HashMap<Value, HashMap<String, u32>>>(src, &merge()).unwrap_err();
        assert_eq!(e.code(), code::INVALID_SCALAR_FOR_TAG);
        assert_eq!(e.path().unwrap().to_string(), path, "{src:?}");
    }
    // An error inside a `<<` value is met at the `<<`, before a later
    // type error, with the path below `<<`.
    let src = "a: x\n";
    let e = from_str_with::<S>(&format!("<<: {{c: !!int y}}\n{src}"), &merge()).unwrap_err();
    assert_eq!(e.code(), code::INVALID_SCALAR_FOR_TAG);
    assert_eq!(e.path().unwrap().to_string(), "<<.c");
    let e = from_str_with::<S>(&format!("{src}<<: {{c: !!int y}}\n"), &merge()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    // A repeated `<<` and an invalid merge value are refused.
    let e =
        from_str_with::<BTreeMap<String, u32>>("<<: {a: 1}\n<<: {b: 2}\n", &merge()).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    let src = "a: 1\n<<: [1]\n";
    let e = from_str_with::<BTreeMap<String, u32>>(src, &merge()).unwrap_err();
    assert_eq!(e.code(), code::INVALID_MERGE_VALUE);
    assert_eq!(
        shape(&e),
        shape(&from_str_with::<Value>(src, &merge()).unwrap_err())
    );
}

/// A target that swallows an error in the input gets `Y3018` on its
/// next read, as with merge keys off.
#[test]
fn a_swallowed_input_error_poisons_the_read() {
    fn lenient<'de, D: de::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        Ok(u32::deserialize(d).unwrap_or(0))
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct S {
        #[serde(deserialize_with = "lenient")]
        a: u32,
        b: Option<u32>,
    }
    let src = "a: !!int x\nb: 1\n";
    let on = from_str_with::<S>(src, &merge()).unwrap_err();
    let off = from_str_with::<S>(src, &Settings::new()).unwrap_err();
    assert_eq!(on.code(), code::READ_AFTER_IGNORED_ERROR, "{on}");
    assert_eq!(shape(&on), shape(&off));
    // A swallowed type error reads on.
    let s: S = from_str_with("a: x\n<<: {b: 2}\n", &merge()).unwrap();
    assert_eq!((s.a, s.b), (0, Some(2)));
}

#[test]
fn early_stopping_targets_still_check_merges() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct A {
        a: u32,
    }
    let ok: A = from_str_with("a: 1\n<<: {b: 2}\nc: 3\n", &merge()).unwrap();
    assert_eq!(ok.a, 1);
    for (src, want) in [
        ("a: 1\n<<: [1]\n", code::INVALID_MERGE_VALUE),
        ("a: 1\n<<: {b: 2}\n<<: {c: 3}\n", code::DUPLICATE_KEY),
        ("a: 1\n<<: {b: !!int x}\n", code::INVALID_SCALAR_FOR_TAG),
    ] {
        for e in [
            from_str_with::<A>(src, &merge()).unwrap_err(),
            from_str_with::<IgnoredAny>(src, &merge()).unwrap_err(),
            from_str_with::<Vec<A>>(
                &format!("- {{{}}}\n", src.trim_end().replace('\n', ", ")),
                &merge(),
            )
            .unwrap_err(),
        ] {
            assert_eq!(e.code(), want, "{src:?}: {e}");
        }
    }
}

#[test]
fn enum_variant_mappings_apply_merges() {
    #[derive(Debug, PartialEq, Deserialize)]
    enum E {
        A(u32),
        B { x: u32, y: u32 },
    }
    let e: E = from_str_with("<<: {A: 1}\n", &merge()).unwrap();
    assert_eq!(e, E::A(1));
    let e: E = from_str_with("B: {<<: {x: 1, y: 3}, y: 2}\n", &merge()).unwrap();
    assert_eq!(e, E::B { x: 1, y: 2 });
}

#[test]
fn documents_of_a_stream_each_stream_their_merges() {
    let src = "a: &a {x: 1}\nb: {<<: *a}\n---\nc: {<<: {y: 2}, y: 3}\n---\nd: {<<: 1}\n";
    type Doc = BTreeMap<String, BTreeMap<String, u32>>;
    let docs: Vec<Result<Doc, Error>> = Docs::from_str_with(src, &merge())
        .map(BTreeMap::deserialize)
        .collect();
    assert_eq!(docs[0].as_ref().unwrap()["b"]["x"], 1);
    assert_eq!(docs[1].as_ref().unwrap()["c"]["y"], 3);
    assert_eq!(
        docs[2].as_ref().unwrap_err().code(),
        code::INVALID_MERGE_VALUE
    );
}

#[test]
fn merge_sources_count_toward_the_alias_budget() {
    // Each `<<: *l3` expands 1,000 nodes; the untrusted budget (2 × own
    // nodes, at least 100,000) refuses them as the `Value` read does.
    let mut src = String::from("l0: &l0 [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]\n");
    for i in 1..=3 {
        src.push_str(&format!("l{i}: &l{i} {{"));
        for j in 0..10 {
            src.push_str(&format!("k{j}: *l{}, ", i - 1));
        }
        src.push_str("}\n");
    }
    for i in 0..200 {
        src.push_str(&format!("m{i}: {{<<: *l3}}\n"));
    }
    let s = merge().limits(Limits::untrusted());
    let v = from_str_with::<Value>(&src, &s).unwrap_err();
    let t = from_str_with::<BTreeMap<String, IgnoredAny>>(&src, &s).unwrap_err();
    let a = from_str_with::<Any>(&src, &s).unwrap_err();
    assert_eq!(v.code(), code::ALIAS_BUDGET_EXCEEDED);
    assert_eq!(
        (t.code(), t.diagnostic().primary.span),
        (v.code(), v.diagnostic().primary.span)
    );
    assert_eq!(
        (a.code(), a.diagnostic().primary.span),
        (v.code(), v.diagnostic().primary.span)
    );
}

#[test]
fn deep_merge_values_do_not_overflow_the_stack() {
    // A `<<` value nested to the depth limit is planned iteratively.
    let depth = 1000;
    let src = format!(
        "a: {{<<: {}{{k: 1}}{}}}\n",
        "{<<: ".repeat(depth),
        "}".repeat(depth)
    );
    let r = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            from_str_with::<BTreeMap<String, BTreeMap<String, IgnoredAny>>>(&src, &merge())
                .map(|m| m["a"].len())
                .map_err(|e| e.code())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(r, Ok(1));
}

/// A node read replays anchors from outside the node, which it never
/// read: their content is checked where the node merges them, as the
/// `Value` read of the node checks it.
#[test]
fn node_reads_check_merged_anchors_from_outside_the_node() {
    let s = merge();
    let doc =
        Document::parse_recovering_with("b: &b {x: 1, x: 2}\nm: {x: 0, <<: *b}\n", &s).into_value();
    let m = doc.get("m").unwrap();
    let v = m.deserialize::<Value>().unwrap_err();
    let t = m.deserialize::<BTreeMap<String, IgnoredAny>>().unwrap_err();
    assert_eq!(v.code(), code::DUPLICATE_KEY);
    assert_eq!(shape(&t), shape(&v));
    for (b, want) in [
        ("{x: 1, x: 2}", code::DUPLICATE_KEY),
        ("{x: !!int y}", code::INVALID_SCALAR_FOR_TAG),
        ("{n: {<<: 5}}", code::INVALID_MERGE_VALUE),
        ("{n: {k: 1, k: 2}}", code::DUPLICATE_KEY),
    ] {
        let src = format!("b: &b {b}\nm: {{a: 1, <<: *b}}\n");
        let doc = Document::parse_recovering_with(&src, &s).into_value();
        let m = doc.get("m").unwrap();
        let v = m.deserialize::<Value>().unwrap_err();
        assert_eq!(v.code(), want, "{src:?}");
        // An early-stopping target, one that reads every entry, and one
        // that ignores the node: the `Value` read's error, path included
        // (it stands below `<<`).
        for e in [
            m.deserialize::<First>().unwrap_err(),
            m.deserialize::<Any>().unwrap_err(),
            m.deserialize::<BTreeMap<String, IgnoredAny>>().unwrap_err(),
        ] {
            assert_eq!(shape(&e), shape(&v), "{src:?}");
        }
    }
}

/// An error the expander meets part-way through a merged anchor from
/// outside a node read (nesting too deep after expansion), after content
/// the read never checked: the `Value` read's error, never an internal
/// one.
#[test]
fn a_limit_inside_an_unchecked_merged_anchor_is_reported() {
    let s = merge().max_depth(4);
    let src = "b: &b {x: !!int y, z: [[1]]}\nm: {k: {<<: *b}}\n";
    let doc = Document::parse_recovering_with(src, &s).into_value();
    let m = doc.get("m").unwrap();
    let v = m.deserialize::<Value>().unwrap_err();
    let t = m.deserialize::<BTreeMap<String, Any>>().unwrap_err();
    assert_eq!(v.code(), code::INVALID_SCALAR_FOR_TAG);
    assert_eq!(shape(&t), shape(&v));
    assert_eq!(t.path().unwrap().to_string(), "k.<<.x");
    // Checked content met at the same limit: the path below `<<`.
    let src = "b: &b {x: 1, z: [1]}\nm: {<<: *b}\n";
    let s = merge().max_depth(3);
    let v = from_str_with::<Value>(src, &s).unwrap_err();
    let t = from_str_with::<BTreeMap<String, Any>>(src, &s).unwrap_err();
    assert_eq!(v.code(), code::DEPTH_LIMIT_EXCEEDED);
    assert_eq!(shape(&t), shape(&v));
}

/// An error met where a `<<` value starts (an alias to an anchor of an
/// earlier document) stands below `<<`, as the `Value` read reports it.
#[test]
fn an_error_where_a_merge_value_starts_stands_below_the_merge_key() {
    let src = "a: &a {x: 1}\n---\nb: {<<: *a}\n";
    type D = BTreeMap<String, BTreeMap<String, u32>>;
    let typed: Vec<Result<D, Error>> = Docs::from_str_with(src, &merge())
        .map(D::deserialize)
        .collect();
    let value: Vec<Result<Value, Error>> = Docs::from_str_with(src, &merge())
        .map(Value::deserialize)
        .collect();
    let (t, v) = (
        typed[1].as_ref().unwrap_err(),
        value[1].as_ref().unwrap_err(),
    );
    assert_eq!(v.code(), code::UNDEFINED_ALIAS);
    assert_eq!(shape(t), shape(v));
    assert_eq!(t.path().unwrap().to_string(), "b.<<");
}

/// Edits accepted on a strictly parsed document may give it a meaning
/// error (a merge-key target replaced by a non-mapping); a node that
/// merges the anchor holding it reports it on a typed read, as on a
/// `Value` read.
#[test]
fn an_edit_that_breaks_a_merged_anchor_is_reported_by_node_reads() {
    let src = "c: &c {z: 1}\nb: &b {n: {<<: *c}}\nm: {a: 1, <<: *b}\n";
    let mut doc = Document::parse_with(src, &merge()).unwrap();
    doc.set_path("c", 5).unwrap();
    assert!(doc.has_errors(), "{doc}");
    let m = doc.get("m").unwrap();
    let v = m.deserialize::<Value>().unwrap_err();
    assert_eq!(v.code(), code::INVALID_MERGE_VALUE, "{doc}");
    for e in [
        m.deserialize::<First>().unwrap_err(),
        m.deserialize::<Any>().unwrap_err(),
        m.deserialize::<BTreeMap<String, IgnoredAny>>().unwrap_err(),
    ] {
        assert_eq!(shape(&e), shape(&v), "{doc}");
    }
    // The whole document refuses it where the anchor stands.
    let whole = doc.deserialize::<BTreeMap<String, Any>>().unwrap_err();
    let wv = doc.deserialize::<Value>().unwrap_err();
    assert_eq!(shape(&whole), shape(&wv), "{doc}");
}
