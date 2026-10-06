//! The serializer's allocation gate:
//! exact allocation counts for small typed values, so an extra allocation
//! per field, per string key, or per `f32` fails here.
//!
//! Counting is per thread (`allocation-counter`, a dev-dependency whose
//! `#[global_allocator]` is an `unsafe impl GlobalAlloc`). It lives here,
//! not in `oneyaml`'s tests: unsafe-containing
//! dependencies are allowed only in the `oracles` crate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use serde::Serialize;

#[derive(Serialize)]
struct Small {
    name: &'static str,
    port: u16,
    enabled: bool,
}

#[derive(Serialize)]
struct Wider {
    name: &'static str,
    port: u16,
    enabled: bool,
    ratio: f32,
    retries: u8,
    weight: f64,
}

/// Allocations made by `f` on this thread (after one warm-up run, so
/// lazily initialised thread-locals are not counted).
fn allocations(f: impl Fn()) -> u64 {
    f();
    allocation_counter::measure(f).count_total
}

fn yaml<T: Serialize>(v: &T) -> impl Fn() + '_ {
    move || {
        std::hint::black_box(oneyaml::to_string(v).unwrap());
    }
}

/// Exact counts (0% allocation tolerance). A change here needs a
/// written justification citing a higher-ranked goal. An earlier version made
/// 18 and 29: one extra `String` per struct field and one per
/// `f32`. The streaming serializer (no intermediate
/// `Value`) lowered them from 15 and 22 to 4 and 5: only the output
/// `String` growing remains (struct field names are held inline for
/// duplicate detection).
#[test]
fn serializing_a_small_struct_allocates_exactly() {
    let small = Small {
        name: "web",
        port: 8080,
        enabled: true,
    };
    assert_eq!(allocations(yaml(&small)), 4);
    let wider = Wider {
        name: "web",
        port: 8080,
        enabled: true,
        ratio: 1.1,
        retries: 3,
        weight: 0.5,
    };
    assert_eq!(allocations(yaml(&wider)), 5);
}

/// A map key costs no depth walk (which allocated a
/// `Vec` per key, 1,021 allocations for these 1,000 entries); what is left
/// is the output `String` growing and the duplicate-key set (a `Vec` of
/// the first 8 keys, then a `HashSet` growing). Integer keys are
/// `Value::Number`s, which do not allocate.
#[test]
fn map_keys_allocate_exactly() {
    let ints: std::collections::BTreeMap<u32, u8> = (0..1000).map(|i| (i, 1)).collect();
    assert_eq!(allocations(yaml(&ints)), MAP_KEYS);
    // String keys add one `String` each (the key `Value`, kept for
    // duplicate detection) and nothing else.
    let strs: std::collections::BTreeMap<String, u8> =
        (0..1000).map(|i| (format!("k{i}"), 1)).collect();
    assert_eq!(allocations(yaml(&strs)), STR_KEYS);
}

const MAP_KEYS: u64 = 21;

/// `Value`s inside typed data (handed over whole, each cloned once by its
/// `Serialize` impl, as the build-then-emit serializer did) and collection keys
/// cost no stack of their own: scalars are written straight through, and the
/// `Value` walk and the depth walk reuse one stack per document. Counts at
/// 100,000 elements; the build-then-emit serializer made 28, 100,028, 100,071,
/// 700,046 and 300,089 (the build-then-emit path), then 100,018, 200,018,
/// 200,035, 1,100,019 and 751,201. What is left per element is the hand-over's
/// clone (a `String`; seven blocks for the mapping) or the key's `Value` (its
/// `Vec`, plus its digest walk for duplicate detection); the rest is the output
/// `String` and the key set growing.
#[test]
fn handed_over_values_and_collection_keys_allocate_exactly() {
    use oneyaml::{Mapping, Number, Value};
    use std::collections::BTreeMap;
    const N: u32 = 100_000;
    let numbers: Vec<Number> = (0..N).map(Number::from).collect();
    let a = allocations(yaml(&numbers));
    let strings: Vec<Value> = (0..N).map(|i| Value::from(format!("s{i}"))).collect();
    let b = allocations(yaml(&strings));
    let ints: BTreeMap<String, Value> = (0..N).map(|i| (format!("k{i}"), Value::from(i))).collect();
    let c = allocations(yaml(&ints));
    let maps: Vec<Value> = (0..N)
        .map(|_| {
            let mut m = Mapping::new();
            m.insert(Value::from("a"), Value::from(1));
            m.insert(Value::from("b"), Value::from(vec![Value::from("x")]));
            Value::from(m)
        })
        .collect();
    let d = allocations(yaml(&maps));
    let keys: BTreeMap<Vec<u32>, u8> = (0..N).map(|i| (vec![i], 1)).collect();
    let e = allocations(yaml(&keys));
    // Vec<Number>, Vec<Value::String>, BTreeMap<String, Value(int)>,
    // Vec<Value{a:1,b:[x]}>, BTreeMap<Vec<u32>, u8>.
    assert_eq!([a, b, c, d, e], HANDED);
}

const HANDED: [u64; 5] = [18, 100_018, 100_035, 700_022, 200_037];
const STR_KEYS: u64 = MAP_KEYS + 1000;

#[test]
fn an_f32_allocates_no_more_than_the_same_f64() {
    for v in [
        1.1f32,
        f32::MIN_POSITIVE,
        f32::MAX,
        -1e-45,
        f32::NAN,
        f32::INFINITY,
    ] {
        // The `f64` the serializer writes for `v` (its shortest digits).
        let same: f64 = v.to_string().parse().unwrap();
        assert_eq!(allocations(yaml(&v)), allocations(yaml(&same)), "{v}");
    }
    // A short `f32` root costs what an integer root costs.
    assert_eq!(allocations(yaml(&1.1f32)), allocations(yaml(&1u8)));
}
