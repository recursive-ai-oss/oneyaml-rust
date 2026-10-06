//! The serializer corpus shared by `ser_differential.rs`:
//! the inputs, and [`corpus`], which writes every one of them through
//! the public entry points (`to_string_with`, the document `Serializer`)
//! and reports each outcome. This file uses only API that existed in
//! the last build-then-emit serializer, so it is copied
//! unchanged into that revision to record
//! `crates/bench/data/ser-baseline.tsv` (see `SOURCE` in
//! `ser_differential.rs`).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;

use bench::ser_run::{Data, SerWorkload, prepare};
use bench::serde_run::SerdeLib;
use oneyaml::{Compat, Emit, Layout, Limits, QuoteStyle, Settings, Value};
use serde::Serialize;
use serde::ser::{self, SerializeMap, SerializeSeq};

/// Output settings × reading settings (the compat sweep).
pub fn settings() -> Vec<(String, Settings)> {
    let emits = [
        ("default", Emit::new()),
        ("indent4", Emit::new().indent(4)),
        ("indent1", Emit::new().indent(1)),
        ("flat-seq", Emit::new().indent_sequences(false)),
        ("double", Emit::new().quote_style(QuoteStyle::Double)),
        ("serde_yaml", Emit::new().layout(Layout::SerdeYaml)),
        (
            "serde_yaml-double",
            Emit::new()
                .layout(Layout::SerdeYaml)
                .quote_style(QuoteStyle::Double),
        ),
    ];
    let readings = [
        ("core", Settings::new()),
        ("merge_keys", Settings::new().merge_keys(true)),
        ("honor", Settings::new().honor_version_directive(true)),
        ("bools", Settings::new().yaml11_booleans(true)),
        ("octal", Settings::new().legacy_octal(true)),
        ("sexagesimal", Settings::new().sexagesimal(true)),
        ("separators", Settings::new().digit_separators(true)),
        ("int_forms", Settings::new().yaml11_int_forms(true)),
        ("compat_serde_yaml", Compat::serde_yaml()),
        ("compat_yaml_1_1", Compat::yaml_1_1()),
    ];
    let mut out = Vec::new();
    for (e, emit) in &emits {
        for (r, reading) in &readings {
            // Compat presets carry their own layout; ours overrides it.
            out.push((format!("{e}/{r}"), reading.clone().emit(emit.clone())));
        }
    }
    out
}

/// A `Value` as plain serde data, so every node goes through the serde
/// calls (not the whole-`Value` hand-over). A local tag `!name` becomes a
/// newtype variant `name`, any other tag the one-entry map `{tag: v}`.
pub struct Plain<'a>(pub &'a Value);

impl Serialize for Plain<'_> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Number(n) => {
                if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                    s.serialize_f64(f)
                } else if let Some(u) = n.as_u64() {
                    s.serialize_u64(u)
                } else if let Some(i) = n.as_i64() {
                    s.serialize_i64(i)
                } else {
                    // Wider integers: hand the number over (its own impl).
                    n.serialize(s)
                }
            }
            Value::String(t) => s.serialize_str(t),
            Value::Sequence(q) => {
                let mut out = s.serialize_seq(Some(q.len()))?;
                for v in q {
                    out.serialize_element(&Plain(v))?;
                }
                out.end()
            }
            Value::Mapping(m) => {
                let mut out = s.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    out.serialize_entry(&Plain(k), &Plain(v))?;
                }
                out.end()
            }
            // (a `Tag` is read through its text.)
            Value::Tagged(t) => match t.tag.to_string().strip_prefix('!') {
                Some(name) => {
                    let name: &'static str = Box::leak(name.to_owned().into_boxed_str());
                    s.serialize_newtype_variant("T", 0, name, &Plain(&t.value))
                }
                None => {
                    let mut out = s.serialize_map(Some(1))?;
                    out.serialize_entry(&t.tag.to_string(), &Plain(&t.value))?;
                    out.end()
                }
            },
            _ => Err(ser::Error::custom("unknown Value variant")),
        }
    }
}

pub fn suite_values() -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for case in test_suite::load_suite(&test_suite::default_data_dir()).unwrap() {
        let Ok(text) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        // Documents read up to the first error (error cases give some).
        for (i, doc) in oneyaml::Deserializer::from_str(&text).enumerate() {
            let Ok(v) = <Value as serde::Deserialize>::deserialize(doc) else {
                break;
            };
            out.push((format!("{} doc {i}", case.id), v));
        }
    }
    out
}

// `Deserialize`/`PartialEq`/`Debug`: the tagged-variant exception of
// `ser_differential.rs` reads the tagged-scalar output back.
#[derive(Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
pub enum E {
    Unit,
    Newtype(i32),
    NewStr(String),
    NewSeq(Vec<u8>),
    NewNone(Option<u8>),
    NewNested(Box<E>),
    Tuple(u8, String),
    Struct {
        a: u8,
        b: Vec<String>,
    },
    EmptyStruct {},
    EmptyTuple(),
    #[serde(rename = "")]
    Nameless(String),
    #[serde(rename = "a b!é")]
    Odd(Vec<u8>),
    #[serde(rename = "multi\nline")]
    MultiLine(u8),
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
pub enum Untagged {
    A(u8),
    B { x: String },
}

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
pub enum Internal {
    One { v: u8 },
    Two,
}

#[derive(Serialize, Clone)]
#[serde(tag = "t", content = "c")]
pub enum Adjacent {
    One(u8),
    Two { s: String },
}

#[derive(Serialize, Clone)]
pub struct Inner {
    s: String,
    o: Option<Box<Inner>>,
}

#[derive(Serialize, Clone)]
pub struct Flat {
    a: u8,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Serialize, Clone)]
pub struct Everything {
    pub unit: (),
    pub b: bool,
    pub i: i64,
    pub u: u128,
    pub neg: i128,
    pub f: f64,
    pub f32: f32,
    pub c: char,
    pub s: String,
    pub multi: String,
    pub lead: String,
    pub none: Option<u8>,
    pub some: Option<Option<String>>,
    pub bytes: serde_bytes_like::Bytes,
    pub empty_seq: Vec<u8>,
    pub empty_map: BTreeMap<String, u8>,
    pub nested_empty: Vec<Vec<BTreeMap<u8, Vec<u8>>>>,
    pub int_keys: BTreeMap<i32, String>,
    pub bool_keys: BTreeMap<bool, u8>,
    pub seq_keys: BTreeMap<Vec<u8>, String>,
    pub empty_seq_key: BTreeMap<Vec<u8>, u8>,
    pub long_key: BTreeMap<String, u8>,
    pub enums: Vec<E>,
    pub enum_keys: BTreeMap<String, E>,
    pub untagged: Vec<Untagged>,
    pub internal: Vec<Internal>,
    pub adjacent: Vec<Adjacent>,
    pub tuple: (u8, (String, [u8; 2])),
    pub inner: Inner,
    pub value: Value,
    pub values: Vec<Value>,
    pub flat: Flat,
}

/// `serialize_bytes` without a dependency.
pub mod serde_bytes_like {
    #[derive(Clone)]
    pub struct Bytes(pub Vec<u8>);
    impl serde::Serialize for Bytes {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_bytes(&self.0)
        }
    }
}

pub fn tagged(tag: &str, v: Value) -> Value {
    Value::from(oneyaml::TaggedValue {
        tag: oneyaml::Tag::new(tag),
        value: v,
    })
}

pub fn everything() -> Everything {
    let e_all = vec![
        E::Unit,
        E::Newtype(-3),
        E::NewStr("x y".into()),
        E::NewStr("yes".into()),
        E::NewStr("a\nb\n".into()),
        E::NewSeq(vec![]),
        E::NewSeq(vec![1, 2]),
        E::NewNone(None),
        E::NewNested(Box::new(E::NewStr("in".into()))),
        E::NewNested(Box::new(E::Newtype(1))),
        E::NewNested(Box::new(E::NewNested(Box::new(E::Struct {
            a: 1,
            b: vec![],
        })))),
        E::NewNested(Box::new(E::Unit)),
        E::Tuple(1, "t".into()),
        E::Struct {
            a: 2,
            b: vec!["x".into(), "".into()],
        },
        E::EmptyStruct {},
        E::EmptyTuple(),
        E::Nameless("n".into()),
        E::Odd(vec![7]),
        E::MultiLine(3),
    ];
    let mut rest = BTreeMap::new();
    rest.insert("t".to_owned(), tagged("!t", Value::from("x")));
    rest.insert("n".to_owned(), Value::Null);
    let mut m = oneyaml::Mapping::new();
    m.insert(Value::from(vec![Value::from(1)]), Value::from("complex"));
    m.insert(
        tagged("!k", Value::from("tk")),
        tagged("!v", Value::from(vec![])),
    );
    Everything {
        unit: (),
        b: true,
        i: i64::MIN,
        u: u128::MAX,
        neg: i128::MIN,
        f: 1e16,
        f32: 1.1,
        c: '#',
        s: "no".into(),
        multi: "line one\nline two\n".into(),
        lead: " lead\ntrail ".into(),
        none: None,
        some: Some(None),
        bytes: serde_bytes_like::Bytes(vec![0, 255]),
        empty_seq: vec![],
        empty_map: BTreeMap::new(),
        nested_empty: vec![
            vec![],
            vec![BTreeMap::new()],
            vec![BTreeMap::from([(1, vec![])])],
        ],
        int_keys: BTreeMap::from([(-1, "m".into()), (7, "".into())]),
        bool_keys: BTreeMap::from([(false, 0), (true, 1)]),
        seq_keys: BTreeMap::from([(vec![1, 2], "a".into()), (vec![], "e".into())]),
        empty_seq_key: BTreeMap::from([(vec![], 1)]),
        long_key: BTreeMap::from([("k".repeat(1100), 1), ("q".repeat(129), 2)]),
        enum_keys: e_all
            .iter()
            .enumerate()
            .map(|(i, e)| (format!("k{i}"), e.clone()))
            .collect(),
        enums: e_all,
        untagged: vec![Untagged::A(1), Untagged::B { x: "y".into() }],
        internal: vec![Internal::One { v: 1 }, Internal::Two],
        adjacent: vec![Adjacent::One(1), Adjacent::Two { s: "s".into() }],
        tuple: (1, ("t".into(), [3, 4])),
        inner: Inner {
            s: "outer".into(),
            o: Some(Box::new(Inner {
                s: "@x".into(),
                o: None,
            })),
        },
        value: Value::from(m),
        values: vec![
            Value::Null,
            tagged("!s", Value::from("v")),
            tagged("!q", Value::from(vec![Value::from(1)])),
            Value::from(vec![]),
        ],
        flat: Flat { a: 1, rest },
    }
}

/// Fails at the `n`-th call it sees (a custom error, `Y3012`).
pub struct FailAt(pub usize);

impl Serialize for FailAt {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        for i in 0..self.0 {
            seq.serialize_element(&i)?;
        }
        Err(ser::Error::custom("boom"))
    }
}

#[derive(Serialize)]
pub struct Dup;

/// Serializes a map with a repeated key.
pub struct DupKeys(pub Value);

impl Serialize for DupKeys {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry(&Plain(&self.0), &1)?;
        m.serialize_entry(&Plain(&self.0), &2)?;
        m.end()
    }
}

// ---- The recorded corpus ----

/// What one call gave: the text, or the error's code and path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok(String),
    Err { code: String, path: Option<String> },
}

impl Outcome {
    pub fn of(r: Result<String, oneyaml::Error>) -> Self {
        match r {
            Ok(t) => Outcome::Ok(t),
            Err(e) => Outcome::Err {
                code: format!("{:?}", e.code()),
                path: e.path().map(ToString::to_string),
            },
        }
    }
}

/// One document through `to_string_with`.
pub fn doc<T: ?Sized + Serialize>(v: &T, s: &Settings) -> Outcome {
    Outcome::of(oneyaml::to_string_with(v, s))
}

/// Documents through one document `Serializer`: the stream, or the first
/// error.
pub fn stream<T: Serialize>(docs: &[T], s: &Settings) -> Outcome {
    let mut out = Vec::new();
    let mut ser = oneyaml::Serializer::with_settings(&mut out, s);
    for d in docs {
        if let Err(e) = d.serialize(&mut ser) {
            return Outcome::of(Err(e));
        }
    }
    if let Err(e) = ser.into_inner() {
        return Outcome::of(Err(e));
    }
    Outcome::Ok(String::from_utf8(out).unwrap())
}

/// The bench models' documents for `case`, as (group, one visit per
/// document and one for the stream).
fn model_group<T: Serialize>(
    group: &str,
    docs: &[T],
    s: &Settings,
    visit: &mut dyn FnMut(&str, &str, Outcome),
) {
    for (i, d) in docs.iter().enumerate() {
        visit(group, &format!("doc {i}"), doc(d, s));
    }
    visit(group, "stream", stream(docs, s));
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

fn nested_sequence(depth: usize) -> Value {
    (0..depth).fold(Value::from(1), |v, _| Value::from(vec![v]))
}

#[derive(Serialize)]
struct Tree(Vec<Tree>);

fn tree(depth: usize) -> Tree {
    (0..depth).fold(Tree(Vec::new()), |t, _| Tree(vec![t]))
}

/// Serializes one entry whose key is `.0`.
struct KeyedBy<'a>(&'a Value);

impl Serialize for KeyedBy<'_> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry(self.0, &1)?;
        m.end()
    }
}

/// Every input of the corpus written through the public entry points,
/// in a fixed order: `visit(group, item, outcome)`.
pub fn corpus(visit: &mut dyn FnMut(&str, &str, Outcome)) {
    let all = settings();
    // 1. The bench models: the whole sweep up to 100 KB, the 1 MB stream
    //    with the defaults.
    // The recorded corpus predates helm-values, so it
    //    is not part of it.
    for case in bench::corpus::CASES
        .iter()
        .filter(|c| c.base <= 1024 * 1024 && c.name != bench::corpus::HELM_VALUES)
    {
        let Ok(data) = prepare(SerdeLib::OneYaml, SerWorkload::Typed, case) else {
            continue;
        };
        for (label, s) in &all {
            if case.base > 100 * 1024 && label != "default/core" {
                continue;
            }
            let group = format!("model/{}/{label}", case.name);
            match &data {
                Data::K8s(d) => model_group(&group, d, s, visit),
                Data::Gha(d) => model_group(&group, d, s, visit),
                Data::Compose(d) => model_group(&group, d, s, visit),
                Data::OpenApi(d) => model_group(&group, d, s, visit),
                other => panic!("{}: unexpected data {other:?}", case.name),
            }
        }
    }
    // 2. The suite's `Value`s, handed over and as plain serde data.
    let values = suite_values();
    for (label, s) in &all {
        let group = format!("suite/{label}");
        for (id, v) in &values {
            visit(&group, &format!("{id} value"), doc(v, s));
            visit(&group, &format!("{id} plain"), doc(&Plain(v), s));
        }
        let plain: Vec<Plain<'_>> = values.iter().map(|(_, v)| Plain(v)).collect();
        let handed: Vec<&Value> = values.iter().map(|(_, v)| v).collect();
        for (i, chunk) in plain.chunks(7).enumerate() {
            visit(&group, &format!("plain stream {i}"), stream(chunk, s));
        }
        for (i, chunk) in handed.chunks(7).enumerate() {
            visit(&group, &format!("value stream {i}"), stream(chunk, s));
        }
    }
    // 3. The generated type set.
    let every = everything();
    let roots = [
        Value::Null,
        Value::from("x"),
        Value::from(vec![]),
        Value::from(oneyaml::Mapping::new()),
        tagged("!t", Value::from(vec![Value::from(1)])),
    ];
    let docs = [
        Value::from(1),
        Value::from("x"),
        Value::from(vec![]),
        Value::from(vec![Value::from(1)]),
        tagged("!t", Value::from("s")),
        tagged("!t", Value::from(vec![Value::from(1)])),
        Value::from(oneyaml::Mapping::new()),
    ];
    for (label, s) in &all {
        let group = format!("generated/{label}");
        let g = group.as_str();
        visit(g, "everything", doc(&every, s));
        for (i, e) in every.enums.iter().enumerate() {
            visit(g, &format!("enum {i}"), doc(e, s));
            visit(g, &format!("[enum {i}]"), doc(&[e], s));
            visit(g, &format!("Some(enum {i})"), doc(&Some(e), s));
            visit(
                g,
                &format!("{{k: enum {i}}}"),
                doc(&BTreeMap::from([("k", e)]), s),
            );
        }
        for (i, root) in roots.iter().enumerate() {
            visit(g, &format!("root {i}"), doc(root, s));
        }
        visit(g, "u128", doc(&u128::MAX, s));
        visit(g, "char", doc(&'\n', s));
        visit(g, "str", doc(" a\nb", s));
        visit(g, "empty", doc(&Vec::<u8>::new(), s));
        let plain: Vec<Plain<'_>> = docs.iter().map(Plain).collect();
        visit(g, "plain stream", stream(&plain, s));
        visit(g, "value stream", stream(&docs, s));
        let enums: Vec<&E> = every.enums.iter().collect();
        visit(g, "enum stream", stream(&enums, s));
    }
    // 4. Errors, with their codes and paths.
    let deep = nested_sequence(40);
    let bad_tag = tagged("!", Value::from("x"));
    let tagged_number = tagged("!n", Value::from(1));
    let mixed = BTreeMap::from([
        ("a", Value::from(vec![bad_tag.clone()])),
        ("b", deep.clone()),
    ]);
    let both = Value::from(vec![bad_tag.clone(), deep.clone()]);
    for (label, s) in [
        ("default", Settings::new()),
        ("depth3", Settings::new().max_depth(3)),
        (
            "typed3",
            Settings::new().limits(Limits::default().max_typed_depth(3)),
        ),
        (
            "serde_yaml",
            Settings::new().emit(Emit::new().layout(Layout::SerdeYaml)),
        ),
    ] {
        let group = format!("errors/{label}");
        let g = group.as_str();
        visit(g, "fail", doc(&FailAt(3), &s));
        visit(g, "fail nested", doc(&vec![FailAt(1)], &s));
        visit(
            g,
            "fail in map",
            doc(&BTreeMap::from([("k", vec![FailAt(2)])]), &s),
        );
        visit(g, "dup", doc(&DupKeys(Value::from("k")), &s));
        visit(
            g,
            "dup complex",
            doc(&DupKeys(Value::from(vec![Value::from(1)])), &s),
        );
        visit(
            g,
            "dup nested",
            doc(&BTreeMap::from([("m", DupKeys(Value::from("k")))]), &s),
        );
        visit(g, "deep", doc(&deep, &s));
        visit(g, "deep plain", doc(&Plain(&deep), &s));
        visit(g, "bad tag", doc(&Plain(&bad_tag), &s));
        visit(g, "bad tag value", doc(&vec![bad_tag.clone()], &s));
        visit(g, "tagged number", doc(&vec![tagged_number.clone()], &s));
        visit(g, "mixed", doc(&(mixed.clone(), FailAt(0)), &s));
        visit(g, "both handed", doc(&both, &s));
        visit(g, "both plain", doc(&Plain(&both), &s));
        visit(g, "unit struct", doc(&Dup, &s));
        visit(g, "stream fail", stream(&[FailAt(0)], &s));
        visit(g, "stream dup", stream(&[DupKeys(Value::from("k"))], &s));
    }
    // 5. Limit boundaries.
    let chain = nested_mapping(1024);
    let chain_less = nested_mapping(1023);
    let chain_deep = nested_mapping(30_000);
    let g = "limits";
    visit(g, "mapping chain 1024", doc(&chain, &Settings::new()));
    visit(
        g,
        "[mapping chain 1024]",
        doc(&vec![&chain], &Settings::new()),
    );
    visit(
        g,
        "[mapping chain 1023]",
        doc(&vec![&chain_less], &Settings::new()),
    );
    visit(g, "mapping chain 30000", doc(&chain_deep, &Settings::new()));
    visit(
        g,
        "key mapping chain 1023",
        doc(&KeyedBy(&chain_less), &Settings::new()),
    );
    visit(
        g,
        "key mapping chain 1024",
        doc(&KeyedBy(&chain), &Settings::new()),
    );
    for d in [2, 3, 4] {
        let s = Settings::new().max_depth(3);
        visit(g, &format!("seq {d} at 3"), doc(&nested_sequence(d), &s));
        visit(
            g,
            &format!("plain seq {d} at 3"),
            doc(&Plain(&nested_sequence(d)), &s),
        );
        let typed = Settings::new().limits(Limits::default().max_typed_depth(2 * d));
        visit(
            g,
            &format!("tree 3 at typed {}", 2 * d),
            doc(&tree(3), &typed),
        );
    }
    visit(g, "tree 200", doc(&tree(200), &Settings::new()));
}
