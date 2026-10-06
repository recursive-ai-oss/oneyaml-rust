//! Release-gate fixes: merge keys inside
//! complex keys, anchored `<<`, aliases of `<<`, and navigation through
//! merges mean the same on every path: the typed planner, the
//! strict `Document` check, `Value`, and `MappingRef::get`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::{BTreeMap, HashMap};

use oneyaml::{Code, Compat, Document, DuplicateKeys, Schema, Settings, Value, code};

fn merge_settings() -> Vec<Settings> {
    vec![
        Settings::new().merge_keys(true),
        Settings::new()
            .merge_keys(true)
            .duplicate_keys(DuplicateKeys::LastWins),
        Settings::new().merge_keys(true).schema(Schema::Failsafe),
        Compat::serde_yaml(),
    ]
}

fn value(src: &str, s: &Settings) -> Result<Value, Code> {
    oneyaml::from_str_with::<Value>(src, s).map_err(|e| e.code())
}

fn strict(src: &str, s: &Settings) -> Result<Value, Code> {
    Document::parse_with(src, s)
        .map_err(|e| e.code())
        .and_then(|d| d.to_value().map_err(|e| e.code()))
}

/// The typed map, as a `Value` mapping (contents compared, not length).
fn typed(src: &str, s: &Settings) -> Result<Value, Code> {
    oneyaml::from_str_with::<HashMap<Value, Value>>(src, s)
        .map(|m| Value::Mapping(m.into_iter().collect()))
        .map_err(|e| e.code())
}

#[test]
fn merge_sequence_inside_a_complex_key_plans() {
    // It was Y9000 on the typed path: a `<<` sequence's merge-source item
    // has no `Value` of its own, and the sequence asked for one.
    for src in [
        "{? {<<: [{a: 1}]} : x}",
        "s: &s [{a: 1}]\n? {<<: *s}\n: x\n",
        "{? {<<: [{a: 1}, {b: 2, a: 3}], c: 4} : x}",
        "{? [{<<: [{a: 1}]}, {<<: {b: [1, {<<: {c: 2}}]}}] : x}",
        "{? {<<: {<<: [{a: 1}]}} : x}",
    ] {
        for s in merge_settings() {
            let v = value(src, &s);
            assert!(v.is_ok(), "{src:?} {s:?}: {v:?}");
            assert_eq!(typed(src, &s), v, "{src:?} {s:?}");
            assert_eq!(strict(src, &s), v, "{src:?} {s:?}");
        }
    }
    let s = Settings::new().merge_keys(true);
    let m = typed("{? {<<: [{a: 1}], b: 2} : x}", &s).unwrap();
    let key = oneyaml::from_str::<Value>("{b: 2, a: 1}").unwrap();
    assert_eq!(m.as_mapping().unwrap().get(&key), Some(&Value::from("x")));
}

#[test]
fn complex_key_identity_includes_merged_entries() {
    let s = Settings::new().merge_keys(true);
    // A false error: `{<<: {a: 1}}` is `{a: 1}`, not `{}`.
    let src = "{? {<<: {a: 1}} : x, ? {} : y}";
    assert!(value(src, &s).is_ok());
    assert_eq!(strict(src, &s), value(src, &s));
    assert_eq!(typed(src, &s), value(src, &s));
    // A missed error: they are equal keys.
    for src in [
        "{? {<<: {a: 1}} : x, ? {a: 1} : y}",
        "{? {<<: [{a: 1}, {a: 2, b: 3}]} : x, ? {b: 3, a: 1} : y}",
        "{? [{<<: {a: 1}}] : x, ? [{a: 1}] : y}",
        "{? {<<: {<<: {a: 1}}} : x, ? {a: 1} : y}",
    ] {
        assert_eq!(value(src, &s), Err(code::DUPLICATE_KEY), "{src:?}");
        let e = Document::parse_with(src, &s).unwrap_err();
        assert_eq!(e.code(), code::DUPLICATE_KEY, "{src:?}");
        assert_eq!(typed(src, &s), Err(code::DUPLICATE_KEY), "{src:?}");
    }
    // Explicit entries win over merged ones in the identity too.
    let src = "{? {<<: {a: 1}, a: 2} : x, ? {a: 2} : y}";
    assert_eq!(strict(src, &s), Err(code::DUPLICATE_KEY));
    let src = "{? {<<: {a: 1}, a: 2} : x, ? {a: 1} : y}";
    assert!(strict(src, &s).is_ok());
}

#[test]
fn merges_through_aliases_in_keys_never_raise_false_errors() {
    // `Document` does not expand aliases; a complex key that merges
    // an alias has the merged entries in its identity (built
    // within the copy budget, as an alias key's value), so it is never a
    // false duplicate and a true one is reported as serde reports it.
    let s = Settings::new().merge_keys(true);
    let ok = "s: &s {a: 1}\nk: {? {<<: *s} : x, ? {} : y}\n";
    assert!(value(ok, &s).is_ok());
    assert!(Document::parse_with(ok, &s).is_ok());
    let ok = "s: &s {a: 1}\nt: &t {a: 2}\nk: {? {<<: [*s, *t]} : x, ? {<<: [*t, *s]} : y}\n";
    assert!(value(ok, &s).is_ok());
    assert!(Document::parse_with(ok, &s).is_ok());
    let hidden = "s: &s {a: 1}\nk: {? {<<: *s} : x, ? {a: 1} : y}\n";
    assert_eq!(value(hidden, &s), Err(code::DUPLICATE_KEY));
    // No longer hidden from the strict parse.
    let serde = oneyaml::from_str_with::<Value>(hidden, &s).unwrap_err();
    let e = Document::parse_with(hidden, &s).unwrap_err();
    assert_eq!((e.code(), e.span()), (serde.code(), serde.span()));
}

#[test]
fn anchored_merge_key_and_its_aliases() {
    let s = Settings::new().merge_keys(true);
    // A false error: `&m <<` redefines `m`, so `*m` is a merge key in `d`.
    let src = "k: &m a\nc: {&m <<: {x: 1}}\nd: {*m : {z: 1}, a: 2}\n";
    let v = value(src, &s).unwrap();
    assert_eq!(v["d"]["z"], Value::from(1));
    assert_eq!(strict(src, &s), Ok(v.clone()));
    assert_eq!(typed(src, &s), Ok(v));
    // A missed error: `*m` as a key is a merge key whose value is 5.
    let src = "c: {&m <<: {x: 1}}\nd: {*m : 5}\n";
    assert_eq!(value(src, &s), Err(code::INVALID_MERGE_VALUE));
    let e = Document::parse_with(src, &s).unwrap_err();
    assert_eq!(e.code(), code::INVALID_MERGE_VALUE);
    // The typed path reads expanded events: the same.
    let e = oneyaml::from_str_with::<BTreeMap<String, Value>>(src, &s).unwrap_err();
    assert_eq!(e.code(), code::INVALID_MERGE_VALUE);
    // A `<<` value anchored, then aliased as a key: a merge key.
    let src = "k: &m <<\nd: {*m : {a: 1}, b: 2}\n";
    let v = value(src, &s).unwrap();
    assert_eq!(v["d"]["a"], Value::from(1));
    assert_eq!(strict(src, &s), Ok(v));
    // An anchored merge key aliased as a value is the string `<<`.
    let src = "c: {&m <<: {x: 1}}\nd: *m\n";
    let v = value(src, &s).unwrap();
    assert_eq!(v["d"], Value::from("<<"));
    assert_eq!(strict(src, &s), Ok(v));
    let t: BTreeMap<String, Value> = oneyaml::from_str_with(src, &s).unwrap();
    assert_eq!(t["d"], Value::from("<<"));
    // A second `<<` through an alias is a duplicate `<<`.
    let src = "c: {&m <<: {x: 1}}\nd: {<<: {a: 1}, *m : {b: 1}}\n";
    assert_eq!(value(src, &s), Err(code::DUPLICATE_KEY));
    assert_eq!(strict(src, &s), Err(code::DUPLICATE_KEY));
    // Under the JSON schema a plain `<<` value matches nothing: the error
    // comes where the alias is expanded, from the anchored definition.
    let json = Settings::new().merge_keys(true).schema(Schema::Json);
    let src = "{\"c\": {&m <<: {\"x\": 1}}, \"d\": *m}";
    assert_eq!(value(src, &json), Err(code::JSON_UNMATCHED_SCALAR));
    assert_eq!(strict(src, &json), Err(code::JSON_UNMATCHED_SCALAR));
    // Without merge keys, `<<` is an ordinary key everywhere.
    let off = Settings::new();
    let src = "k: &m a\nc: {&m <<: {x: 1}}\nd: {*m : {z: 1}, a: 2}\n";
    assert_eq!(strict(src, &off), value(src, &off));
}

#[test]
fn typed_paths_agree_on_anchored_merge_keys() {
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct D {
        a: i64,
        z: i64,
    }
    let s = Settings::new().merge_keys(true);
    let src = "c: {&m <<: {x: 1}}\nd: {*m : {z: 1}, a: 2}\n";
    let t: BTreeMap<String, BTreeMap<String, i64>> = oneyaml::from_str_with(src, &s).unwrap();
    assert_eq!(t["d"]["z"], 1);
    assert_eq!(t["c"]["x"], 1);
    let t: BTreeMap<String, D> =
        oneyaml::from_str_with("c: {&m <<: {a: 0, z: 0}}\nd: {*m : {z: 1}, a: 2}\n", &s).unwrap();
    assert_eq!(t["d"], D { a: 2, z: 1 });
}

/// `z`, `m0`, then `mi: {<<: [*m(i-1), *m(i-1)]}`, then
/// `top: {<<: [*m{n}, *z]}`: a merge DAG whose paths double per level.
fn shared_dag(n: usize) -> String {
    let mut s = String::from("z: &z {target: 1}\nm0: &m0 {a: 0}\n");
    for i in 1..=n {
        let p = i - 1;
        s.push_str(&format!("m{i}: &m{i} {{<<: [*m{p}, *m{p}]}}\n"));
    }
    s.push_str(&format!("top: {{<<: [*m{n}, *z]}}\n"));
    s
}

#[test]
fn get_through_a_shared_merge_dag_is_exact_and_fast() {
    let s = Settings::new().merge_keys(true);
    let src = shared_dag(12);
    let doc = Document::parse_with(src.as_str(), &s).unwrap();
    assert_eq!(doc.to_value().unwrap()["top"]["target"], Value::from(1));
    let got = doc.get("top").unwrap().get("target").unwrap();
    assert_eq!(got.as_scalar().unwrap().value(), "1");
    assert_eq!(doc.get("top").unwrap().get("a").unwrap().text(), "0");
    // O(merge closure): 60 levels would be 2^60 paths.
    let src = shared_dag(60);
    let doc = Document::parse_with(src.as_str(), &s).unwrap();
    let t = std::time::Instant::now();
    assert!(doc.get("top").unwrap().get("nope").is_none());
    assert_eq!(doc.get("top").unwrap().get("target").unwrap().text(), "1");
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn get_follows_merge_precedence_and_repeated_merge_keys() {
    let s = Settings::new().merge_keys(true);
    for src in [
        // Earlier sources first, depth first; explicit keys first.
        "a: &a {k: 1}\nb: &b {k: 2, j: 2}\nm: {<<: [*a, *b], j: 3}\n",
        "a: &a {<<: {k: 1}}\nb: &b {k: 2}\nm: {<<: [*b, *a]}\n",
        "a: &a {k: 1}\nb: &b {<<: *a, k: 4}\nm: {<<: [*a, *b]}\n",
        "b: &b {x: 2}\na: &a {<<: *b, k: 1}\nm: {<<: [*b, *a]}\n",
    ] {
        let doc = Document::parse_with(src, &s).unwrap();
        let v = doc.to_value().unwrap();
        for key in ["k", "j", "x"] {
            let got = doc
                .get("m")
                .unwrap()
                .get(key)
                .map(|n| n.as_scalar().unwrap().resolved().unwrap());
            assert_eq!(got.as_ref(), v["m"].get(key), "{src:?} {key}");
        }
    }
    // LastWins: the last `<<` replaces the earlier ones.
    let lw = Settings::new()
        .merge_keys(true)
        .duplicate_keys(DuplicateKeys::LastWins);
    let src = "m: {<<: {a: 1}, <<: {b: 2}}\n";
    let doc = Document::parse_with(src, &lw).unwrap();
    assert_eq!(doc.to_value().unwrap()["m"].get("a"), None);
    assert!(doc.get("m").unwrap().get("a").is_none());
    assert_eq!(doc.get("m").unwrap().get("b").unwrap().text(), "2");
    // An alias of `<<` as a key merges in the view too.
    let src = "c: {&m <<: {x: 1}}\nd: {*m : {z: 1}}\n";
    let doc = Document::parse_with(src, &s).unwrap();
    assert_eq!(doc.get("d").unwrap().get("z").unwrap().text(), "1");
}
