//! 1,000,000-level-deep `Value`s are built, cloned, compared,
//! hashed, debug-formatted, measured, indexed, and dropped on a small-stack
//! thread without overflowing it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::Write;

use oneyaml::{Mapping, Sequence, Tag, TaggedValue, Value};

const DEPTH: usize = 1_000_000;
/// Far smaller than the ~1M frames any recursive impl would need.
const STACK: usize = 128 * 1024;

fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("no stack overflow / panic on the small-stack thread");
}

fn deep_seq(leaf: Value) -> Value {
    let mut v = leaf;
    for _ in 0..DEPTH {
        v = Value::Sequence(Sequence::from(vec![v]));
    }
    v
}

fn deep_map_values(leaf: Value) -> Value {
    let mut v = leaf;
    for _ in 0..DEPTH {
        let mut m = Mapping::new();
        m.insert(Value::from("k"), v);
        v = Value::Mapping(m);
    }
    v
}

/// Key digests are cached in their entries, so a chain of
/// complex keys costs O(depth) to build, hash, and compare: the full
/// million levels. (Before the cache this was O(depth^2) and capped at
/// 5,000.)
const KEY_DEPTH: usize = DEPTH;

fn deep_map_keys(leaf: Value) -> Value {
    let mut v = leaf;
    for _ in 0..KEY_DEPTH {
        let mut m = Mapping::new();
        m.insert(v, Value::Null);
        v = Value::Mapping(m);
    }
    v
}

fn deep_tagged(leaf: Value) -> Value {
    let mut v = leaf;
    for _ in 0..DEPTH {
        v = Value::from(TaggedValue {
            tag: Tag::new("!t"),
            value: v,
        });
    }
    v
}

fn exercise(build: fn(Value) -> Value, expected_depth: usize) {
    let a = build(Value::from(1));
    let b = a.clone();
    assert!(a == b);
    let c = build(Value::from(2));
    assert!(a != c, "a difference at the deepest leaf is found");

    let s = RandomState::new();
    assert_eq!(s.hash_one(&a), s.hash_one(&b));

    let mut sink = std::io::sink();
    write!(sink, "{a:?}").unwrap();

    assert_eq!(a.depth(), expected_depth);
    // Indexing never panics, whatever the shape.
    let _ = &a[0][0]["k"][7];
    drop((a, b, c));
}

#[test]
fn deep_sequence() {
    on_small_stack(|| exercise(deep_seq, DEPTH));
}

#[test]
fn deep_mapping_values() {
    on_small_stack(|| exercise(deep_map_values, DEPTH));
}

#[test]
fn deep_mapping_keys() {
    on_small_stack(|| {
        exercise(deep_map_keys, KEY_DEPTH);
        // A deep key can be looked up, inserted, and removed.
        let key = deep_map_keys(Value::from(1));
        let mut m = Mapping::new();
        m.insert(key.clone(), Value::from("found"));
        assert_eq!(m.get(&key), Some(&Value::from("found")));
        assert_eq!(m.remove(&key), Some(Value::from("found")));
        assert!(m.is_empty());
    });
}

#[test]
fn deep_tag_chain() {
    on_small_stack(|| {
        exercise(deep_tagged, 0);
        let v = deep_tagged(Value::from("x"));
        assert_eq!(v.untag_ref(), &Value::from("x"));
        assert_eq!(v.untag(), Value::from("x"));
    });
}

#[test]
fn deep_mixed_and_wide() {
    on_small_stack(|| {
        // Alternate sequence / mapping / tag levels.
        let mut v = Value::Null;
        for i in 0..DEPTH {
            v = match i % 3 {
                0 => Value::Sequence(Sequence::from(vec![Value::from(i), v])),
                1 => {
                    let mut m = Mapping::new();
                    m.insert(Value::from(i), v);
                    Value::Mapping(m)
                }
                _ => Value::from(TaggedValue {
                    tag: Tag::new("!m"),
                    value: v,
                }),
            };
        }
        let w = v.clone();
        assert_eq!(v, w);
        assert!(v.depth() > DEPTH / 2);
        // A wide flat sequence drops fine too.
        let flat: Value = (0..DEPTH).map(Value::from).collect();
        assert_eq!(flat.depth(), 1);
        drop((v, w, flat));
    });
}
