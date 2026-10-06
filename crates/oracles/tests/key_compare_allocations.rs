//! Comparing a key that matches allocates nothing —
//! `Value` equality of scalars (`walk::eq` set up its context stack even
//! when there was nothing to walk: every confirmed `Mapping` lookup and
//! every equal `KeyMap` small key paid one allocation), and a `Document`
//! key lookup (`ScalarRef::is_string` decoded the matching key twice and
//! built its string value).
//!
//! Counting is per thread (`allocation-counter`, an `unsafe impl
//! GlobalAlloc` dev-dependency, so here: the only crate allowed one, as
//! `ser_allocations.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Mapping, Value};

/// Allocations made by `f` on this thread (after one warm-up run).
fn allocations(f: impl Fn()) -> u64 {
    f();
    allocation_counter::measure(f).count_total
}

#[test]
fn equal_scalars_compare_without_allocating() {
    let pairs = [
        (Value::from(7u64), Value::from(7u64)),
        (Value::from("key"), Value::from("key")),
        (Value::Bool(true), Value::Bool(true)),
        (Value::Null, Value::Null),
        (Value::from(1.5), Value::from(1.5)),
    ];
    for (a, b) in &pairs {
        assert_eq!(
            allocations(|| assert!(std::hint::black_box(a) == b)),
            0,
            "{a:?}"
        );
    }
    let mut m = Mapping::new();
    for i in 0..20u64 {
        m.insert(Value::from(i), Value::from(i * 2));
    }
    let key = Value::from(13u64);
    assert_eq!(
        allocations(|| assert_eq!(m.get(std::hint::black_box(&key)), Some(&Value::from(26u64)))),
        0
    );
}

/// A key that matches costs no allocation over one that matches nothing
/// (the search's own work list is the same for both).
#[test]
fn document_key_matches_allocate_nothing() {
    let doc = Document::parse("a: 1\nb: 2\nkey: 3\n'q': 4\n").unwrap();
    let root = doc.root().unwrap();
    let count = |k: &str| {
        allocations(|| {
            std::hint::black_box(root.get(std::hint::black_box(k)));
        })
    };
    let miss = count("missing");
    for k in ["key", "q", "a"] {
        assert_eq!(count(k), miss, "{k}");
    }
    assert_eq!(root.get("key").unwrap().text(), "3");
    assert_eq!(root.get("q").unwrap().text(), "4");
}
