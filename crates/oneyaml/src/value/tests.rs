#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;

use oneyaml_core::Limits;

use super::*;

fn s(x: &str) -> Value {
    Value::from(x)
}

fn map(pairs: &[(&str, i64)]) -> Mapping {
    pairs.iter().map(|(k, v)| (s(k), Value::from(*v))).collect()
}

fn keys(m: &Mapping) -> Vec<&str> {
    m.keys().map(|k| k.as_str().unwrap()).collect()
}

#[test]
fn mapping_keeps_insertion_order() {
    let mut m = map(&[("b", 1), ("a", 2), ("c", 3)]);
    assert_eq!(keys(&m), ["b", "a", "c"]);
    // Replacing keeps the position and returns the old value.
    assert_eq!(m.insert(s("a"), Value::from(20)), Some(Value::from(2)));
    assert_eq!(keys(&m), ["b", "a", "c"]);
    assert_eq!(m.get_index(1), Some((&s("a"), &Value::from(20))));
    assert_eq!(m.get_index(3), None);
    let owned: Vec<_> = m.clone().into_iter().map(|(k, _)| k).collect();
    assert_eq!(owned, [s("b"), s("a"), s("c")]);
    let borrowed: Vec<_> = (&m).into_iter().map(|(_, v)| v.clone()).collect();
    assert_eq!(borrowed, [Value::from(1), Value::from(20), Value::from(3)]);
    assert_eq!(m.iter().len(), 3);
}

#[test]
fn mapping_lookup_and_remove() {
    let mut m = map(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)]);
    assert_eq!(m.get(&s("c")), Some(&Value::from(3)));
    assert_eq!(m.get_str("c"), Some(&Value::from(3)));
    assert!(m.contains_key(&s("d")));
    assert!(!m.contains_key(&s("z")));
    assert_eq!(m.remove(&s("z")), None);
    assert_eq!(m.remove_entry(&s("b")), Some((s("b"), Value::from(2))));
    assert_eq!(keys(&m), ["a", "c", "d"]);
    // Every later entry is still found at its new position.
    for (k, v) in [("a", 1), ("c", 3), ("d", 4)] {
        assert_eq!(m.get_str(k), Some(&Value::from(v)), "{k}");
    }
    *m.get_mut(&s("d")).unwrap() = Value::from(40);
    for (_, v) in m.iter_mut() {
        if let Value::Number(n) = v {
            *v = Value::from(n.as_i64().unwrap() + 1);
        }
    }
    assert_eq!(
        m.values().cloned().collect::<Vec<_>>(),
        [2, 4, 41].map(Value::from)
    );
    for v in m.values_mut() {
        *v = Value::Null;
    }
    assert!(m.values().all(Value::is_null));
    m.clear();
    assert!(m.is_empty() && m.get_str("a").is_none());
    m.extend([(s("x"), Value::Null)]);
    assert_eq!(m.len(), 1);
    assert_eq!(Mapping::default(), Mapping::new());
}

#[test]
fn mapping_hash_collisions() {
    // Force every key into one index slot to exercise `Slot::Many`.
    let mut m = Mapping::new();
    for (i, k) in ["a", "b", "c"].iter().enumerate() {
        assert!(m.insert_hashed(7, s(k), Value::from(i)).is_none());
    }
    assert_eq!(
        m.insert_hashed(7, s("b"), Value::from(10)),
        Some(Value::from(1))
    );
    assert_eq!(m.find_hashed(7, KeyRef::Str("a")), Some(0));
    assert_eq!(m.find_hashed(7, KeyRef::Str("c")), Some(2));
    assert_eq!(m.find_hashed(7, KeyRef::Str("z")), None);
    assert_eq!(m.find_hashed(8, KeyRef::Str("a")), None);
    assert_eq!(m.shift_remove_index(1), Some((s("b"), Value::from(10))));
    assert_eq!(m.find_hashed(7, KeyRef::Str("c")), Some(1));
    assert_eq!(m.shift_remove_index(0), Some((s("a"), Value::from(0))));
    // Back to a single-entry slot.
    assert_eq!(m.find_hashed(7, KeyRef::Str("c")), Some(0));
    assert_eq!(m.shift_remove_index(5), None);
    assert_eq!(m.shift_remove_index(0), Some((s("c"), Value::from(2))));
    assert_eq!(m.find_hashed(7, KeyRef::Str("c")), None);
}

#[test]
fn any_value_keys_and_float_keys() {
    let mut m = Mapping::new();
    let seq_key: Value = vec![Value::from(1), Value::Null].into();
    m.insert(seq_key.clone(), s("seq"));
    m.insert(Value::from(f64::NAN), s("nan"));
    m.insert(Value::from(0.0), s("zero"));
    m.insert(Value::from(-0.0), s("negzero"));
    m.insert(Value::from(1), s("int"));
    m.insert(Value::from(1.0), s("float"));
    m.insert(Value::Null, s("null"));
    m.insert(Value::Bool(true), s("true"));
    assert_eq!(m.len(), 8);
    assert_eq!(m.get(&seq_key), Some(&s("seq")));
    assert_eq!(m.get(&Value::from(-f64::NAN)), Some(&s("nan")));
    assert_eq!(m.get(&Value::from(-0.0)), Some(&s("negzero")));
    assert_eq!(m.get(&Value::from(1.0)), Some(&s("float")));
    assert_eq!(m.get(&Value::Null), Some(&s("null")));
}

#[test]
fn str_lookup_hash_matches_value_hash() {
    // `KeyRef::Str` must hash exactly like `Value::String`.
    let m: Mapping = [(s("key"), Value::from(1))].into_iter().collect();
    let st = RandomState::new();
    assert_eq!(st.hash_one(KeyRef::Str("key")), st.hash_one(s("key")));
    assert_eq!(m.get_str("key"), Some(&Value::from(1)));
    let v = Value::Mapping(m);
    assert_eq!(v["key"], Value::from(1));
    assert_eq!(v[&s("key")], Value::from(1));
    assert_eq!(v[&String::from("key")], Value::from(1));
}

#[test]
fn equality_and_hash() {
    let st = RandomState::new();
    let a = Value::Mapping(map(&[("a", 1), ("b", 2)]));
    let b = Value::Mapping(map(&[("a", 1), ("b", 2)]));
    let reordered = Value::Mapping(map(&[("b", 2), ("a", 1)]));
    assert_eq!(a, b);
    assert_eq!(st.hash_one(&a), st.hash_one(&b));
    // Spec §3.2.1.1: mapping equality and hash are
    // order-independent; iteration keeps insertion order.
    assert_eq!(a, reordered);
    assert_eq!(st.hash_one(&a), st.hash_one(&reordered));
    assert_eq!(
        keys(reordered.as_mapping().unwrap()),
        ["b", "a"],
        "iteration order is untouched"
    );
    assert_ne!(a, Value::Mapping(map(&[("a", 1), ("b", 3)])));
    assert_ne!(a, Value::Mapping(map(&[("a", 1), ("c", 2)])));
    let t1 = Value::from(TaggedValue {
        tag: Tag::new("!x"),
        value: s("v"),
    });
    let t2 = Value::from(TaggedValue {
        tag: Tag::new("!y"),
        value: s("v"),
    });
    assert_ne!(t1, t2);
    assert_ne!(t1, s("v"));
    assert_eq!(t1.clone(), t1);
    assert_ne!(Value::from(vec![Value::Null]), Value::from(Vec::new()));
    assert_ne!(Value::Null, Value::Bool(false));
    assert_ne!(Value::Bool(true), Value::Bool(false));
    assert_ne!(Value::Mapping(map(&[("a", 1)])), a);
    // Wrapper types agree with `Value`.
    let (sa, sb) = (Sequence::from(vec![s("a")]), Sequence::from(vec![s("a")]));
    assert_eq!(sa, sb);
    assert_eq!(st.hash_one(&sa), st.hash_one(&sb));
    let (ma, mb) = (map(&[("a", 1)]), map(&[("a", 1)]));
    assert_eq!(st.hash_one(&ma), st.hash_one(&mb));
    let tv = t1.as_tagged().unwrap().clone();
    assert_eq!(&tv, t1.as_tagged().unwrap());
    assert_eq!(st.hash_one(&tv), st.hash_one(t1.as_tagged().unwrap()));
}

/// Builds `{k: v, ...}` from values, in order.
fn vmap(pairs: Vec<(Value, Value)>) -> Value {
    Value::Mapping(pairs.into_iter().collect())
}

#[test]
fn nested_mappings_compare_and_hash_order_free() {
    let st = RandomState::new();
    let inner1 = vmap(vec![(s("x"), Value::from(1)), (s("y"), Value::from(2))]);
    let inner2 = vmap(vec![(s("y"), Value::from(2)), (s("x"), Value::from(1))]);
    // Reordered at every level, including inside keys.
    let a = vmap(vec![
        (inner1.clone(), Value::from(vec![inner1.clone()])),
        (s("k"), inner1.clone()),
    ]);
    let b = vmap(vec![
        (s("k"), inner2.clone()),
        (inner2.clone(), Value::from(vec![inner2.clone()])),
    ]);
    assert_eq!(a, b);
    assert_eq!(st.hash_one(&a), st.hash_one(&b));
    // A lookup by a reordered complex key finds the entry.
    assert_eq!(
        a.as_mapping().unwrap().get(&inner2),
        Some(&Value::from(vec![inner1.clone()]))
    );
    // Sequences stay ordered.
    let s1 = Value::from(vec![s("a"), s("b")]);
    let s2 = Value::from(vec![s("b"), s("a")]);
    assert_ne!(s1, s2);
    assert_ne!(st.hash_one(&s1), st.hash_one(&s2));
    // Tagged mappings too.
    let t = |v: Value| {
        Value::from(TaggedValue {
            tag: Tag::new("!t"),
            value: v,
        })
    };
    assert_eq!(t(inner1.clone()), t(inner2.clone()));
    assert_eq!(st.hash_one(t(inner1)), st.hash_one(t(inner2)));
    // Empty mappings.
    assert_eq!(Mapping::new(), Mapping::with_capacity(4));
    assert_eq!(
        st.hash_one(Mapping::new()),
        st.hash_one(Mapping::with_capacity(4))
    );
}

#[test]
fn order_free_equality_backtracks_across_digest_collisions() {
    // Every key forged to one digest: matching must try candidates.
    let forged = |pairs: &[(&str, i64)]| {
        let mut m = Mapping::new();
        for (k, v) in pairs {
            m.insert_hashed(7, s(k), Value::from(*v));
        }
        m
    };
    let a = forged(&[("a", 1), ("b", 2), ("c", 3)]);
    let b = forged(&[("c", 3), ("a", 1), ("b", 2)]);
    assert_eq!(a, b);
    assert_ne!(a, forged(&[("c", 3), ("a", 1), ("b", 9)]));
    assert_ne!(a, forged(&[("c", 3), ("a", 1), ("z", 2)]));
    // Collisions nested inside a candidate's key: a failed inner match
    // fails only that candidate, and the next one is tried.
    let key = |m: Mapping| Value::Mapping(m);
    let mut x = Mapping::new();
    x.insert_hashed(9, key(forged(&[("p", 1), ("q", 2)])), s("first"));
    x.insert_hashed(9, key(forged(&[("p", 1), ("q", 3)])), s("second"));
    let mut y = Mapping::new();
    y.insert_hashed(9, key(forged(&[("q", 3), ("p", 1)])), s("second"));
    y.insert_hashed(9, key(forged(&[("q", 2), ("p", 1)])), s("first"));
    assert_eq!(x, y);
    let mut z = Mapping::new();
    z.insert_hashed(9, key(forged(&[("q", 3), ("p", 1)])), s("first"));
    z.insert_hashed(9, key(forged(&[("q", 2), ("p", 1)])), s("second"));
    assert_ne!(x, z, "keys match but values are swapped");
    // No candidate at all.
    let mut w = Mapping::new();
    w.insert_hashed(8, s("a"), Value::from(1));
    assert_ne!(forged(&[("a", 1)]), w);
}

#[test]
fn with_capacity_never_panics() {
    // Capacity overflow falls back to an empty, growable mapping.
    let mut m = Mapping::with_capacity(usize::MAX);
    assert!(m.is_empty());
    m.insert(s("a"), Value::Null);
    assert_eq!(m.len(), 1);
    assert!(Mapping::with_capacity(usize::MAX / 2).is_empty());
    assert!(Mapping::with_capacity(8).capacity() >= 8);
}

#[test]
fn sequence_api_never_panics() {
    let mut q = Sequence::new();
    assert!(q.is_empty() && q.first().is_none() && q.last().is_none());
    q.push(s("a"));
    assert_eq!(q.insert_at(1, s("c")), Ok(()));
    assert_eq!(q.insert_at(1, s("b")), Ok(()));
    assert_eq!(q.insert_at(0, s("_")), Ok(()));
    // Past the end: unchanged, the value comes back.
    assert_eq!(q.insert_at(9, s("x")), Err(s("x")));
    assert_eq!(q.insert_at(usize::MAX, s("x")), Err(s("x")));
    assert_eq!(q.as_slice(), [s("_"), s("a"), s("b"), s("c")]);
    assert_eq!(q.len(), 4);
    assert_eq!(q.get(1), Some(&s("a")));
    assert_eq!(q.get(4), None);
    assert_eq!(q.get(usize::MAX), None);
    *q.get_mut(0).unwrap() = s("0");
    assert!(q.get_mut(4).is_none());
    assert_eq!(q.remove_at(0), Some(s("0")));
    assert_eq!(q.remove_at(3), None);
    assert_eq!(q.remove_at(usize::MAX), None);
    assert_eq!((q.first(), q.last()), (Some(&s("a")), Some(&s("c"))));
    for v in q.iter_mut() {
        *v = Value::from(format!("{}!", v.as_str().unwrap()));
    }
    for v in &mut q {
        *v = Value::from(format!("{}?", v.as_str().unwrap()));
    }
    let texts: Vec<&str> = q.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(texts, ["a!?", "b!?", "c!?"]);
    q.extend([Value::Null]);
    assert_eq!(q.pop(), Some(Value::Null));
    q.clear();
    assert!(q.is_empty() && q.pop().is_none());
    assert_eq!(
        Sequence::from(vec![Value::Null]).into_vec(),
        vec![Value::Null]
    );
}

#[test]
fn clone_preserves_every_shape() {
    let mut m = Mapping::new();
    m.insert(Value::from(vec![Value::Null]), Value::from(Mapping::new()));
    m.insert(s("e"), Value::from(Sequence::new()));
    let v = Value::from(vec![
        Value::from(m),
        Value::from(TaggedValue {
            tag: Tag::new("!t"),
            value: Value::from(vec![s("x"), Value::from(1.5)]),
        }),
        Value::Bool(true),
    ]);
    let c = v.clone();
    assert_eq!(c, v);
    assert_eq!(format!("{c:?}"), format!("{v:?}"));
    // Cloned mappings keep a working index.
    let cm = c[0].as_mapping().unwrap();
    assert_eq!(cm.get_str("e"), Some(&Value::from(Sequence::new())));
    assert!(cm.contains_key(&Value::from(vec![Value::Null])));
}

#[test]
fn index_never_panics() {
    let mut inner = Mapping::new();
    inner.insert(s("k"), Value::from(vec![Value::from(10), Value::Null]));
    let v = Value::from(TaggedValue {
        tag: Tag::new("!wrapped"),
        value: Value::Mapping(inner),
    });
    // Indexing looks through tags.
    assert_eq!(v["k"][0], Value::from(10));
    assert!(v["k"][1].is_null());
    // Missing, out of range, wrong type: all the null placeholder.
    assert!(v["missing"].is_null());
    assert!(v["k"][99].is_null());
    assert!(v[0].is_null());
    assert!(Value::from(3)["k"].is_null());
    // `get` tells "missing" from "present and null".
    assert_eq!(v["k"].get(1), Some(&Value::Null));
    assert_eq!(v["k"].get(2), None);
    assert_eq!(v.get("nope"), None);

    let mut w = v.clone();
    *w.get_mut("k").unwrap().get_mut(1).unwrap() = s("set");
    assert_eq!(w["k"][1], s("set"));
    assert!(w.get_mut("absent").is_none());
    assert!(w.get_mut(0).is_none());
    assert!(w.get_mut(s("k")).is_some());
    assert!(w.get_mut(String::from("k")).is_some());
    assert!(Value::from(1).get_mut("k").is_none());
}

#[test]
fn accessors() {
    assert_eq!(Value::Null.as_null(), Some(()));
    assert_eq!(Value::from(1).as_null(), None);
    assert_eq!(Value::Bool(true).as_bool(), Some(true));
    assert!(Value::Bool(true).is_bool() && Value::from("x").as_bool().is_none());
    assert_eq!(Value::from(-3).as_i64(), Some(-3));
    assert!(Value::from(-3).is_i64() && !Value::from(-3).is_u64());
    assert_eq!(Value::from(u64::MAX).as_u64(), Some(u64::MAX));
    assert!(Value::from(u64::MAX).is_u64() && Value::from(1.5).is_f64());
    assert_eq!(Value::from(1.5).as_f64(), Some(1.5));
    assert!(Value::from(1.5).is_number() && Value::from(1).as_number().is_some());
    assert!(Value::Null.as_number().is_none() && !Value::Null.is_f64());
    assert_eq!(Value::from("s").as_str(), Some("s"));
    assert!(Value::from(String::from("s")).is_string());
    assert!(Value::Null.as_str().is_none());
    let mut seq = Value::from(vec![Value::Null]);
    assert!(seq.is_sequence() && seq.as_sequence().unwrap().len() == 1);
    seq.as_sequence_mut().unwrap().push(Value::from(true));
    assert_eq!(seq.as_sequence().unwrap().len(), 2);
    assert!(Value::Null.as_sequence().is_none() && Value::Null.as_sequence_mut().is_none());
    let mut m = Value::from(Mapping::new());
    assert!(m.is_mapping() && m.as_mapping().unwrap().is_empty());
    m.as_mapping_mut().unwrap().insert(Value::Null, Value::Null);
    assert!(Value::Null.as_mapping().is_none() && Value::Null.as_mapping_mut().is_none());
    assert!(!m.is_tagged() && m.as_tagged().is_none());
    let mut t = Value::from(TaggedValue {
        tag: Tag::new("!a"),
        value: Value::from(1),
    });
    assert!(t.is_tagged());
    assert_eq!(t.as_tagged().unwrap().tag.to_string(), "!a");
    *t.untag_mut() = Value::from(2);
    assert_eq!(t.untag_ref(), &Value::from(2));
    let (tag, content) = t.as_tagged().cloned().unwrap().into_parts();
    assert_eq!(
        (tag.to_string(), content),
        ("!a".to_owned(), Value::from(2))
    );
    assert_eq!(t.untag(), Value::from(2));
    assert_eq!(Value::default(), Value::Null);
    assert_eq!(Value::from(Number::from(7u8)), Value::from(7));
    let collected: Value = [Value::Null].into_iter().collect();
    assert_eq!(collected, Value::from(vec![Value::Null]));
    let sq: Sequence = [Value::Null].into_iter().collect();
    assert_eq!(sq.clone().into_vec(), vec![Value::Null]);
    assert_eq!((&sq).into_iter().count(), 1);
    assert_eq!(sq.into_iter().count(), 1);
    assert_eq!(Value::from(Sequence::new()), Value::from(Vec::new()));
    for v in [
        Value::from(1i8),
        Value::from(1i16),
        Value::from(1i32),
        Value::from(1i128),
        Value::from(1isize),
        Value::from(1u8),
        Value::from(1u16),
        Value::from(1u32),
        Value::from(1u128),
        Value::from(1usize),
    ] {
        assert_eq!(v, Value::from(1i64));
    }
    assert_eq!(Value::from(1.5f32), Value::from(1.5f64));
}

#[test]
fn debug_format() {
    let mut m = Mapping::new();
    m.insert(s("k"), Value::from(vec![Value::Null, Value::Bool(true)]));
    m.insert(Value::from(1), Value::from(1.5));
    let v = Value::from(TaggedValue {
        tag: Tag::new("!t"),
        value: Value::Mapping(m.clone()),
    });
    assert_eq!(
        format!("{v:?}"),
        "Tagged(!t, Mapping {String(\"k\"): Sequence [Null, Bool(true)], Number(1): Number(1.5)})"
    );
    assert_eq!(
        format!("{m:?}"),
        "{String(\"k\"): Sequence [Null, Bool(true)], Number(1): Number(1.5)}"
    );
    assert_eq!(
        format!("{:?}", Sequence::from(vec![s("a")])),
        "[String(\"a\")]"
    );
    assert_eq!(format!("{:?}", Sequence::new()), "[]");
    assert_eq!(
        format!("{:?}", v.as_tagged().unwrap()),
        "TaggedValue(!t, Mapping {String(\"k\"): Sequence [Null, Bool(true)], Number(1): Number(1.5)})"
    );
    assert_eq!(format!("{:?}", Tag::new("!t")), "Tag(!t)");
}

#[test]
fn depth_and_limit_check() {
    assert_eq!(Value::Null.depth(), 0);
    let mut v = Value::from(1);
    for _ in 0..5 {
        let mut m = Mapping::new();
        m.insert(Value::from(vec![v]), Value::Null);
        v = Value::Mapping(m);
    }
    // Each level is a mapping whose key is a sequence: two collections.
    assert_eq!(v.depth(), 10);
    assert_eq!(v.check_depth(&Limits::new().max_depth(10)), Ok(()));
    assert_eq!(
        v.check_depth(&Limits::new().max_depth(9)),
        Err(DepthExceeded { limit: 9 })
    );
    assert_eq!(Value::Null.check_depth(&Limits::new().max_depth(0)), Ok(()));
    assert_eq!(
        Value::from(Vec::new()).check_depth(&Limits::new().max_depth(0)),
        Err(DepthExceeded { limit: 0 })
    );
    // Tags add no depth.
    let t = Value::from(TaggedValue {
        tag: Tag::new("!x"),
        value: Value::from(Vec::new()),
    });
    assert_eq!(t.depth(), 1);
    // Consumers report the Y2005 limit diagnostic.
    let e = DepthExceeded { limit: 9 }.into_error();
    assert_eq!(e.code(), oneyaml_core::code::VALUE_DEPTH_EXCEEDED);
    let help = e.diagnostic().help.clone().unwrap_or_default();
    assert!(help.contains("Limits::max_depth"), "{help}");
}
