//! `Value` differentials, over yaml-test-suite and the benchmark
//! corpus, under default, `merge_keys`, and YAML 1.1 settings:
//!
//! 1. **`from_value`**: for a wide set of target types,
//!    `from_value::<T>(from_str::<Value>(s)?)` ≡ `from_str::<T>(s)`: equal
//!    values, or the same error code and path. The one documented
//!    exception (`de/value.rs`: a `Value` has no source text, so the
//!    plain-scalar-to-string read has nothing to read) is *classified*,
//!    never assumed: `from_value` must fail with `Y3000` at a node (or
//!    key) that is a `Null`, `Bool`, or `Number` in the value, and those
//!    cases are counted and reported.
//! 2. **`NodeRef::deserialize`**: for every node of every document (not
//!    below an alias), the node's `Value` equals the document's `Value` at
//!    that position, and typed reads equal the streaming deserializer's
//!    read of the same position of the whole input (a seed that walks the
//!    path and ignores the rest): equal values, or the same error code.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Debug};
use std::marker::PhantomData;

use oneyaml::{
    Code, Compat, Document, NodeRef, PathSegment, Settings, Spanned, Stream, Value, code,
    from_str_with, from_value, from_value_with,
};
use serde::de::{self, DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn settings() -> [(&'static str, Settings); 3] {
    [
        ("default", Settings::new()),
        ("merge_keys", Settings::new().merge_keys(true)),
        ("yaml_1_1", Compat::yaml_1_1()),
    ]
}

/// The inputs: every suite case, and the benchmark corpus up to 100 KB,
/// each split into its documents' texts when it has several.
fn inputs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for case in test_suite::load_suite(&test_suite::default_data_dir()).unwrap() {
        if let Ok(text) = String::from_utf8(case.input.clone()) {
            out.push((case.id.clone(), text));
        }
    }
    assert!(out.len() > 300);
    for case in bench::corpus::CASES.iter().filter(|c| c.base <= 100 * 1024) {
        out.push((case.name.to_owned(), case.generate(1)));
    }
    out
}

/// The single-document texts of `src` under `s` (one, or each document's
/// region of a stream).
pub(crate) fn documents(src: &str, s: &Settings) -> Vec<String> {
    match Stream::parse_with(src, s) {
        Ok(stream) if stream.len() > 1 => stream.documents().map(|d| d.to_string()).collect(),
        _ => vec![src.to_owned()],
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Untagged {
    Null(()),
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Untagged>),
    Map(BTreeMap<String, Untagged>),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum External {
    #[serde(rename = "a")]
    A,
    #[serde(rename = "b")]
    B(String),
    #[serde(rename = "c")]
    C(Vec<Value>),
    #[serde(rename = "d")]
    D { x: Option<Value> },
    #[serde(rename = "e")]
    E(i64, String),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
enum Adjacent {
    #[serde(rename = "a")]
    A,
    #[serde(rename = "b")]
    B(i64),
    #[serde(rename = "c")]
    C(String, i64),
    #[serde(rename = "d")]
    D { x: Value },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Internal {
    #[serde(rename = "a")]
    A,
    #[serde(rename = "b")]
    B { x: Option<Value> },
    #[serde(rename = "c")]
    C(BTreeMap<String, Value>),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Flat {
    #[serde(default)]
    name: Option<Value>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

/// `IgnoredAny` with `PartialEq`.
#[derive(Debug, PartialEq)]
struct Ignored;

impl<'de> Deserialize<'de> for Ignored {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(d).map(|_| Ignored)
    }
}

/// As a unit (for [`same`]'s comparison through `Value`).
impl Serialize for Ignored {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit()
    }
}

#[derive(Default)]
pub(crate) struct Tally {
    checks: usize,
    same_ok: usize,
    same_err: usize,
    /// The documented difference: a resolved scalar into a string target.
    no_source_text: usize,
}

/// Whether `e` (from `from_value`) is the documented difference: `Y3000`
/// at a node of `v` that is `Null`, `Bool`, or `Number`, or at a mapping
/// with such a key.
fn no_source_text(v: &Value, e: &oneyaml::Error, s: &Settings) -> bool {
    if e.code() != code::TYPE_MISMATCH {
        return false;
    }
    let msg = e.diagnostic().message.as_str();
    // A wide integer (beyond `i128`/`u128`) has no serde `Unexpected`
    // value to show: `invalid type: integer, expected ...`.
    let resolved = [
        "unit value",
        "boolean `",
        "integer `",
        "integer,",
        "floating point `",
    ];
    if !resolved
        .iter()
        .any(|p| msg.starts_with(&format!("invalid type: {p}")))
    {
        return false;
    }
    // Only a string-like target reads a plain scalar's text: a
    // string or `char` (both `Hint::Str`), a map key, a struct field name,
    // an enum variant name.
    let Some((_, expected)) = msg.split_once(", expected ") else {
        return false;
    };
    let stringy = [
        "a string",
        "a character",
        "field identifier",
        "variant identifier",
        "enum ",
        // An adjacently tagged enum's tag.
        "variant of enum ",
        // The key visitors of adjacently and internally tagged enums:
        // `"t", "c", or other ignored fields`.
        "\"",
    ];
    if !stringy.iter().any(|p| expected.starts_with(p)) {
        return false;
    }
    let mut node = v;
    if let Some(path) = e.path() {
        for seg in path.segments() {
            let next = match (seg, node.untag_ref()) {
                (PathSegment::Index(i), Value::Sequence(q)) => q.get(*i),
                (PathSegment::Key(k), Value::Mapping(m)) => m
                    .iter()
                    .find(|(key, _)| names_key(k, key, s))
                    .map(|(_, v)| v),
                _ => None,
            };
            match next {
                Some(n) => node = n,
                None => return false,
            }
        }
    }
    let scalar = |x: &Value| matches!(x, Value::Null | Value::Bool(_) | Value::Number(_));
    scalar(node) || node.as_mapping().is_some_and(|m| m.keys().any(scalar))
}

/// The entry positions along `e`'s path in `v` (document order).
fn positions(v: &Value, segs: &[PathSegment], s: &Settings) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    let mut node = v;
    for seg in segs {
        let (i, next) = match (seg, node.untag_ref()) {
            (PathSegment::Index(i), Value::Sequence(q)) => (*i, q.get(*i)?),
            (PathSegment::Key(k), Value::Mapping(m)) => m
                .iter()
                .enumerate()
                .find(|(_, (key, _))| names_key(k, key, s))
                .map(|(i, (_, v))| (i, v))?,
            _ => return None,
        };
        out.push(i);
        node = next;
    }
    Some(out)
}

/// Whether `from_str`'s error `a` is not before `from_value`'s `b` in
/// document order: at or after `b`'s node, or at an enclosing collection
/// (reported once its entries were read).
/// A key error (`b` at a mapping with a resolved scalar key) is also
/// passed when `a` is inside that mapping: `from_str` read its keys (as
/// source text, which is why `a`'s path may name them differently).
fn not_before(v: &Value, a: &oneyaml::Error, b: &oneyaml::Error, s: &Settings) -> bool {
    not_before_at(v, &segments(a), &segments(b), s)
}

/// [`not_before`] on paths (relative to `v`).
fn not_before_at(v: &Value, sa: &[PathSegment], sb: &[PathSegment], s: &Settings) -> bool {
    if sa.len() > sb.len() && sa.starts_with(sb) {
        return positions(v, sb, s)
            .and_then(|p| {
                let mut node = v;
                for i in p {
                    node = match node.untag_ref() {
                        Value::Sequence(q) => q.get(i)?,
                        Value::Mapping(m) => m.get_index(i)?.1,
                        _ => return None,
                    };
                }
                node.as_mapping()
            })
            .is_some_and(|m| {
                m.keys()
                    .any(|k| matches!(k, Value::Null | Value::Bool(_) | Value::Number(_)))
            });
    }
    let (Some(pa), Some(pb)) = (positions(v, sa, s), positions(v, sb, s)) else {
        return false;
    };
    match pa.iter().zip(&pb).find(|(x, y)| x != y) {
        Some((x, y)) => x > y,
        None => pa.len() <= pb.len(),
    }
}

/// Whether path segment `k` names mapping key `key`: by its display (how
/// `from_value` paths name keys), or, for a resolved scalar key, by source
/// text that resolves to it under `s` (how `from_str` paths name them:
/// `on` for `true` under YAML 1.1, `""` for an empty null key).
/// a `Value` has no source text, the documented difference.
fn names_key(k: &str, key: &Value, s: &Settings) -> bool {
    // An error path shows at most 64 characters of a key,
    // then `…`; the key is named by that shown form.
    let shown = |d: String| {
        if d.chars().count() > 64 {
            format!("{}…", d.chars().take(64).collect::<String>())
        } else {
            d
        }
    };
    key_display(key).map(shown).as_deref() == Some(k)
        || (matches!(key, Value::Null | Value::Bool(_) | Value::Number(_))
            && from_str_with::<Value>(k, s).ok().as_ref() == Some(key))
}

/// The same entries, named by source text (`sa`) and by display (`sb`).
fn same_entries(v: &Value, sa: &[PathSegment], sb: &[PathSegment], s: &Settings) -> bool {
    sa.len() == sb.len()
        && positions(v, sa, s).is_some_and(|p| positions(v, sb, s).is_some_and(|q| p == q))
}

fn key_display(k: &Value) -> Option<String> {
    match k {
        Value::String(s) => Some(s.clone()),
        Value::Null => Some("null".to_owned()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Tagged(t) => t.value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Equal values, where a NaN equals a NaN (a typed `f64` holds
/// the `.nan` that `Value` compares equal; derived `PartialEq` does not).
/// Compared through `Value` (whose equality reads a NaN as
/// equal to a NaN), not through `Debug` forms, which could hide other
/// differences.
fn same<T: PartialEq + Serialize>(a: &T, b: &T) -> bool {
    a == b
        || matches!(
            (oneyaml::to_value(a), oneyaml::to_value(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

fn from_value_check<T: DeserializeOwned + PartialEq + Debug + Serialize>(
    name: &str,
    src: &str,
    s: &Settings,
    v: &Value,
    t: &mut Tally,
) {
    t.checks += 1;
    let direct = from_str_with::<T>(src, s);
    // Under the same limits as `from_str` (only the limits apply).
    let via = from_value_with::<T>(v.clone(), s);
    match (direct, via) {
        (Ok(a), Ok(b)) => {
            assert!(
                same(&a, &b),
                "{name} {}: {src:?}\n{a:?}\n{b:?}",
                std::any::type_name::<T>()
            );
            t.same_ok += 1;
        }
        (Err(a), Err(b))
            if a.code() == b.code()
                && (a.path() == b.path() || same_entries(v, &segments(&a), &segments(&b), s)) =>
        {
            t.same_err += 1;
        }
        // The documented difference: `from_str` read the text there (Ok),
        // or read it and failed at a later node (or at the same node, or at
        // an enclosing collection once its entries were read).
        (Ok(_), Err(b)) if no_source_text(v, &b, s) => t.no_source_text += 1,
        (Err(a), Err(b)) if no_source_text(v, &b, s) && not_before(v, &a, &b, s) => {
            t.no_source_text += 1;
        }
        (a, b) => panic!(
            "{name} {}: from_str {:?} vs from_value {:?}\n{src:?}",
            std::any::type_name::<T>(),
            a.map(|_| ()).map_err(|e| (e.code(), e.to_string())),
            b.map(|_| ()).map_err(|e| (e.code(), e.to_string())),
        ),
    }
}

pub(crate) fn from_value_all(name: &str, src: &str, s: &Settings, v: &Value, t: &mut Tally) {
    from_value_check::<Value>(name, src, s, v, t);
    from_value_check::<serde_json::Value>(name, src, s, v, t);
    from_value_check::<Ignored>(name, src, s, v, t);
    from_value_check::<BTreeMap<String, Value>>(name, src, s, v, t);
    from_value_check::<HashMap<String, String>>(name, src, s, v, t);
    from_value_check::<BTreeMap<String, Vec<String>>>(name, src, s, v, t);
    from_value_check::<Vec<Value>>(name, src, s, v, t);
    from_value_check::<Vec<String>>(name, src, s, v, t);
    from_value_check::<Vec<Option<i64>>>(name, src, s, v, t);
    from_value_check::<Vec<BTreeMap<String, String>>>(name, src, s, v, t);
    from_value_check::<String>(name, src, s, v, t);
    from_value_check::<Option<String>>(name, src, s, v, t);
    from_value_check::<i64>(name, src, s, v, t);
    from_value_check::<u8>(name, src, s, v, t);
    from_value_check::<f64>(name, src, s, v, t);
    from_value_check::<bool>(name, src, s, v, t);
    from_value_check::<()>(name, src, s, v, t);
    from_value_check::<(Value, Value)>(name, src, s, v, t);
    from_value_check::<Untagged>(name, src, s, v, t);
    from_value_check::<External>(name, src, s, v, t);
    from_value_check::<Vec<External>>(name, src, s, v, t);
    from_value_check::<Flat>(name, src, s, v, t);
    from_value_check::<Adjacent>(name, src, s, v, t);
    from_value_check::<Vec<Adjacent>>(name, src, s, v, t);
    from_value_check::<Internal>(name, src, s, v, t);
    from_value_check::<Vec<Internal>>(name, src, s, v, t);
    from_value_check::<u128>(name, src, s, v, t);
    from_value_check::<i128>(name, src, s, v, t);
    from_value_check::<char>(name, src, s, v, t);
    from_value_check::<Vec<char>>(name, src, s, v, t);
    from_value_check::<BTreeMap<i64, Value>>(name, src, s, v, t);
    from_value_check::<HashMap<bool, String>>(name, src, s, v, t);
    from_value_check::<BTreeMap<String, (i64, String)>>(name, src, s, v, t);
    from_value_check::<(i64, String)>(name, src, s, v, t);
    from_value_check::<(Value, Value, Value)>(name, src, s, v, t);
    from_value_check::<Vec<(String, Value)>>(name, src, s, v, t);
    from_value_check::<bench::models::k8s::Object>(name, src, s, v, t);
    from_value_check::<bench::models::gha::Workflow>(name, src, s, v, t);
    from_value_check::<bench::models::compose::Compose>(name, src, s, v, t);
    from_value_check::<bench::models::openapi::OpenApi>(name, src, s, v, t);
    from_value_check::<bench::models::openapi::Sparse>(name, src, s, v, t);
}

#[test]
#[ignore = "slow: runs in CI"]
fn from_value_agrees_with_from_str() {
    let inputs = inputs();
    for (label, s) in settings() {
        let mut t = Tally::default();
        let mut json = (0usize, 0usize);
        let mut docs = 0usize;
        for (name, src) in &inputs {
            for text in documents(src, &s) {
                let Ok(v) = from_str_with::<Value>(&text, &s) else {
                    continue;
                };
                docs += 1;
                from_value_all(name, &text, &s, &v, &mut t);
                from_value_nodes(name, &text, &s, &v, &mut t);
                json_round_trip(name, &v, &mut json);
            }
        }
        eprintln!(
            "from_value [{label}]: {docs} documents, {} checks: {} equal values, {} equal \
             errors, {} resolved scalars into string targets (documented), 0 disagreements",
            t.checks, t.same_ok, t.same_err, t.no_source_text
        );
        eprintln!(
            "serde_json [{label}]: {} values converted and back unchanged, {} without a JSON form",
            json.0, json.1
        );
        assert!(docs > 300);
        assert!(json.0 > 200);
        assert!(t.same_ok > t.checks / 10);
    }
}

/// Every node of `v` with its path (not through tags: the reference seed
/// reads plain collections), sampled to at most about `per_doc`.
/// An explicit stack.
fn value_nodes(v: &Value, per_doc: usize) -> Vec<(Vec<Seg>, &Value)> {
    let mut out = Vec::new();
    let mut stack = vec![(Vec::new(), v)];
    while let Some((path, node)) = stack.pop() {
        match node {
            Value::Sequence(q) => {
                for (i, item) in q.iter().enumerate() {
                    let mut p = path.clone();
                    p.push(Seg::Index(i));
                    stack.push((p, item));
                }
            }
            Value::Mapping(m) => {
                for (k, item) in m {
                    let mut p = path.clone();
                    p.push(Seg::Key(k.clone()));
                    stack.push((p, item));
                }
            }
            _ => {}
        }
        out.push((path, node));
    }
    let step = (out.len() / per_doc.max(1)).max(1);
    out.into_iter().step_by(step).collect()
}

/// `from_value::<T>` of a node against the streaming read of `T` at the
/// same position of the input: equal values, the same code and path
/// below the node, or the documented difference (classified as above).
fn node_check<T: DeserializeOwned + PartialEq + Debug + Serialize>(
    name: &str,
    src: &str,
    s: &Settings,
    path: &[Seg],
    sub: &Value,
    t: &mut Tally,
) {
    t.checks += 1;
    let direct = reference::<T>(src, s, 0, path);
    let via = from_value_with::<T>(sub.clone(), s);
    match (direct, via) {
        (Ok(Some(a)), Ok(b)) => {
            assert!(
                same(&a, &b),
                "{name} {path:?} {}\n{a:?}\n{b:?}",
                std::any::type_name::<T>()
            );
            t.same_ok += 1;
        }
        (Err((a, pa)), Err(b))
            if a == b.code()
                && pa.len() == path.len() + segments(&b).len()
                && (pa.ends_with(&segments(&b))
                    || same_entries(sub, &pa[path.len()..], &segments(&b), s)) =>
        {
            t.same_err += 1;
        }
        (Ok(Some(_)), Err(b)) if no_source_text(sub, &b, s) => t.no_source_text += 1,
        (Err((_, pa)), Err(b))
            if no_source_text(sub, &b, s)
                && pa.len() >= path.len()
                && not_before_at(sub, &pa[path.len()..], &segments(&b), s) =>
        {
            t.no_source_text += 1;
        }
        (a, b) => panic!(
            "{name} {path:?} {}: stream {:?} vs from_value {:?}",
            std::any::type_name::<T>(),
            a.map(|x| x.is_some()),
            b.map(|_| ()).map_err(|e| (e.code(), e.to_string())),
        ),
    }
}

/// Scalar and tuple targets at every (sampled) node of the document, so
/// the comparison is mostly of values, not of errors at the root.
pub(crate) fn from_value_nodes(name: &str, src: &str, s: &Settings, v: &Value, t: &mut Tally) {
    // The root is `from_value_all`'s (and an empty stream has no document
    // for the reference seed to read).
    // Every node of the suite's documents; a sample of the large corpus
    // ones (each check reads the whole input).
    let per_doc = if src.len() > 2_000 { 12 } else { 200 };
    for (path, sub) in value_nodes(v, per_doc)
        .into_iter()
        .filter(|(p, _)| !p.is_empty())
    {
        node_check::<String>(name, src, s, &path, sub, t);
        node_check::<Option<String>>(name, src, s, &path, sub, t);
        node_check::<i64>(name, src, s, &path, sub, t);
        node_check::<u8>(name, src, s, &path, sub, t);
        node_check::<u128>(name, src, s, &path, sub, t);
        node_check::<f64>(name, src, s, &path, sub, t);
        node_check::<bool>(name, src, s, &path, sub, t);
        node_check::<()>(name, src, s, &path, sub, t);
        node_check::<char>(name, src, s, &path, sub, t);
        node_check::<(Value, Value)>(name, src, s, &path, sub, t);
        node_check::<(i64, String)>(name, src, s, &path, sub, t);
        node_check::<External>(name, src, s, &path, sub, t);
    }
}

/// `serde_json` conversions (feature on through this crate's
/// dev-dependency): a value with a JSON form converts, agrees with serde's
/// generic path (`serde_json::to_value`), and comes back unchanged.
pub(crate) fn json_round_trip(name: &str, v: &Value, n: &mut (usize, usize)) {
    match serde_json::Value::try_from(v) {
        Ok(j) => {
            assert_eq!(serde_json::to_value(v).unwrap(), j, "{name}");
            let back = Value::try_from(j.clone()).unwrap();
            assert_eq!(serde_json::Value::try_from(&back).unwrap(), j, "{name}");
            n.0 += 1;
        }
        Err(e) => {
            assert_eq!(e.span(), None);
            n.1 += 1;
        }
    }
}

/// Mutation self-test of the classification: a resolved scalar refused
/// by a *non-string* target is a disagreement, never the documented
/// difference.
#[test]
fn classification_rejects_non_string_targets() {
    let refused = |v: Value, e: oneyaml::Error| no_source_text(&v, &e, &Settings::new());
    let one = Value::from(1);
    assert!(refused(
        one.clone(),
        from_value::<String>(one.clone()).unwrap_err()
    ));
    let t = Value::Bool(true);
    assert!(!refused(
        t.clone(),
        from_value::<f64>(t.clone()).unwrap_err()
    ));
    assert!(!refused(
        t.clone(),
        from_value::<i64>(t.clone()).unwrap_err()
    ));
    assert!(!refused(
        one.clone(),
        from_value::<bool>(one.clone()).unwrap_err()
    ));
    let s = Value::from("x");
    assert!(!refused(s.clone(), from_value::<u8>(s).unwrap_err()));
    let m: Value = oneyaml::from_str("1: a").unwrap();
    assert!(refused(
        m.clone(),
        from_value::<BTreeMap<String, String>>(m).unwrap_err()
    ));
}

// ---- 2. Per-node reads ----------------------------------------------------

/// A path segment by meaning: a mapping key as its `Value`.
#[derive(Clone, Debug)]
enum Seg {
    Key(Value),
    Index(usize),
}

/// Reads `T` at `path` of the input through the streaming deserializer,
/// ignoring everything else: the reference for a node's typed read.
struct AtPath<'p, T> {
    path: &'p [Seg],
    target: PhantomData<T>,
}

impl<'de, T: Deserialize<'de>> DeserializeSeed<'de> for AtPath<'_, T> {
    type Value = Option<T>;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Option<T>, D::Error> {
        match self.path.split_first() {
            None => T::deserialize(d).map(Some),
            Some((Seg::Key(_), _)) => d.deserialize_map(self),
            Some((Seg::Index(_), _)) => d.deserialize_seq(self),
        }
    }
}

impl<'de, T: Deserialize<'de>> de::Visitor<'de> for AtPath<'_, T> {
    type Value = Option<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the path's collection")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Option<T>, A::Error> {
        let Some((Seg::Key(want), rest)) = self.path.split_first() else {
            return Ok(None);
        };
        let mut found = None;
        while let Some(k) = a.next_key::<Value>()? {
            if found.is_none() && &k == want {
                found = a.next_value_seed(AtPath::<T> {
                    path: rest,
                    target: PhantomData,
                })?;
            } else {
                a.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Option<T>, A::Error> {
        let Some((Seg::Index(want), rest)) = self.path.split_first() else {
            return Ok(None);
        };
        let mut i = 0usize;
        let mut found = None;
        loop {
            if i == *want {
                match a.next_element_seed(AtPath::<T> {
                    path: rest,
                    target: PhantomData,
                })? {
                    Some(v) => found = v,
                    None => break,
                }
            } else if a.next_element::<IgnoredAny>()?.is_none() {
                break;
            }
            i += 1;
        }
        Ok(found)
    }
}

/// `T` at `path` of document `doc` of `src`, read by the streaming
/// deserializer.
fn reference<T: DeserializeOwned>(
    src: &str,
    s: &Settings,
    doc: usize,
    path: &[Seg],
) -> Result<Option<T>, (Code, Vec<PathSegment>)> {
    // The seed's walk down the path is typed recursion too: the node's own
    // `max_typed_depth` budget starts below it (a node reads as a document
    // of its own).
    let limits = s.get_limits().clone();
    let typed = limits.get_max_typed_depth().saturating_add(path.len());
    let s = s.clone().limits(limits.max_typed_depth(typed));
    let Some(d) = oneyaml::Deserializer::from_str_with(src, &s).nth(doc) else {
        return Ok(None);
    };
    AtPath::<T> {
        path,
        target: PhantomData,
    }
    .deserialize(d)
    .map_err(|e| (e.code(), segments(&e)))
}

#[derive(Default)]
struct NodeTally {
    nodes: usize,
    typed: usize,
    typed_err: usize,
    /// Node reads in documents with errors; those holding the first error.
    restricted: usize,
    restricted_err: usize,
}

fn node_typed<T: DeserializeOwned + PartialEq + Debug>(
    name: &str,
    src: &str,
    s: &Settings,
    doc: usize,
    node: NodeRef<'_>,
    path: &[Seg],
    t: &mut NodeTally,
) {
    let got = node
        .deserialize::<T>()
        .map_err(|e| (e.code(), segments(&e)));
    let want = reference::<T>(src, s, doc, path);
    match (got, want) {
        (Ok(a), Ok(Some(b))) => {
            assert_eq!(
                a,
                b,
                "{name} doc {doc} {path:?} {}",
                std::any::type_name::<T>()
            );
            t.typed += 1;
        }
        // The same code, and the same path below the node (the whole read's
        // path is the node's path, then the node read's).
        (Err((a, pa)), Err((b, pb)))
            if a == b && pb.len() == path.len() + pa.len() && pb.ends_with(&pa) =>
        {
            t.typed_err += 1;
        }
        (a, b) => panic!(
            "{name} doc {doc} {path:?} {}: node {:?} vs whole {:?}",
            std::any::type_name::<T>(),
            a.map(|_| ()),
            b.map(|x| x.is_some())
        ),
    }
}

fn segments(e: &oneyaml::Error) -> Vec<PathSegment> {
    e.path().map(|p| p.segments().to_vec()).unwrap_or_default()
}

/// Every node of a document (keys, values, items, error regions; not
/// below an alias). An explicit stack, no recursion.
fn all_nodes<'d>(doc: &'d Document) -> Vec<NodeRef<'d>> {
    let mut out: Vec<NodeRef<'d>> = doc.errors().collect();
    let mut stack: Vec<NodeRef<'d>> = doc.root().into_iter().collect();
    while let Some(n) = stack.pop() {
        out.push(n);
        if n.alias_name().is_some() {
            continue;
        }
        if let Some(m) = n.as_mapping() {
            for (k, v) in m.iter() {
                stack.push(k);
                stack.push(v);
            }
        } else if let Some(q) = n.as_sequence() {
            stack.extend(q.iter());
        }
    }
    out
}

/// Documents with errors (restricted): every node reads without a
/// panic, and a node holding the document's first error (a syntax error)
/// fails with that error's code, as the whole read does.
fn restricted_document(name: &str, doc: &Document, t: &mut NodeTally) {
    let Err(first) = doc.deserialize::<Value>() else {
        return;
    };
    let at = first
        .span()
        .filter(|_| first.kind() == oneyaml::ErrorKind::Syntax);
    for n in all_nodes(doc) {
        let got = n.deserialize::<Value>();
        t.restricted += 1;
        let Some(at) = at else { continue };
        let span = n.span();
        if span.start <= at.start && at.end <= span.end && !span.is_empty() {
            let e = got.unwrap_err();
            assert_eq!(e.code(), first.code(), "{name}: node {span:?}");
            t.restricted_err += 1;
        }
    }
}

/// A node, its path, the document's `Value` there, and whether the path
/// crosses no tagged collection (the reference seed reads plain maps and
/// sequences only).
type Visit<'d> = (NodeRef<'d>, Vec<Seg>, Value, bool);

/// Every node of `doc` (not below an alias), with its path and the
/// document's `Value` there. An explicit stack, no recursion.
fn nodes_of<'d>(doc: &'d Document, root: Value) -> Vec<Visit<'d>> {
    let mut out = Vec::new();
    let Some(r) = doc.root() else {
        return out;
    };
    let mut stack = vec![(r, Vec::new(), root, true)];
    while let Some((n, path, v, plain)) = stack.pop() {
        let inner = plain && !v.is_tagged();
        if n.alias_name().is_none() {
            if let (Some(m), Some(vm)) = (n.as_mapping(), v.untag_ref().as_mapping()) {
                for (k, child) in m.iter() {
                    let Ok(kv) = k.deserialize::<Value>() else {
                        continue;
                    };
                    if let Some(cv) = vm.get(&kv) {
                        let mut p = path.clone();
                        p.push(Seg::Key(kv));
                        stack.push((child, p, cv.clone(), inner));
                    }
                }
            } else if let (Some(q), Some(vq)) = (n.as_sequence(), v.untag_ref().as_sequence()) {
                for (i, (child, cv)) in q.iter().zip(vq.iter()).enumerate() {
                    let mut p = path.clone();
                    p.push(Seg::Index(i));
                    stack.push((child, p, cv.clone(), inner));
                }
            }
        }
        out.push((n, path, v, plain));
    }
    out
}

#[test]
#[ignore = "slow: runs in CI"]
fn node_reads_agree_with_the_whole_document() {
    let inputs = inputs();
    for (label, s) in settings() {
        let mut t = NodeTally::default();
        for (name, src) in &inputs {
            let Ok(stream) = Stream::parse_with(src.as_str(), &s) else {
                let parsed = Stream::parse_recovering_with(src.as_str(), &s);
                for doc in parsed.value().documents() {
                    restricted_document(name, &doc, &mut t);
                }
                continue;
            };
            for (i, doc) in stream.documents().enumerate() {
                let Ok(whole) = doc.to_value() else {
                    continue;
                };
                let nodes = nodes_of(&doc, whole);
                // The large corpus documents: every node's `Value`, typed
                // reads on a sample (each is a whole-input read).
                let step = (nodes.len() / 20).max(1);
                for (k, (n, path, v, plain)) in nodes.iter().enumerate() {
                    t.nodes += 1;
                    assert_eq!(
                        n.deserialize::<Value>().unwrap(),
                        *v,
                        "{name} [{label}] doc {i} {path:?}"
                    );
                    if k % step != 0 || !plain {
                        continue;
                    }
                    node_typed::<Value>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<String>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Vec<String>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<BTreeMap<String, String>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Option<i64>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<f64>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Untagged>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<External>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<serde_json::Value>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Flat>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<(Value, Value)>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<(i64, String)>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Adjacent>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Internal>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<BTreeMap<i64, Value>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<HashMap<bool, String>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Spanned<Value>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<Vec<Spanned<String>>>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<u128>(name, src, &s, i, *n, path, &mut t);
                    node_typed::<char>(name, src, &s, i, *n, path, &mut t);
                }
            }
        }
        eprintln!(
            "node reads [{label}]: {} nodes equal to the document's Value; typed: {} equal \
             values, {} equal errors (code and path), 0 disagreements; documents with \
             errors: {} node reads, {} holding the first error fail with its code",
            t.nodes, t.typed, t.typed_err, t.restricted, t.restricted_err
        );
        assert!(t.nodes > 5000);
        assert!(t.restricted_err > 50, "{}", t.restricted_err);
    }
}
