//! A typed enum reads a `%TAG` tag that names one of its
//! variants in O(the tag's suffix): it is handed that variant's own
//! `'static` name, never the tag's prefix joined to its suffix (that
//! join would cost O(prefix) per use). A tag that names no variant
//! (`#[serde(other)]`, hand-written visitors, `deserialize_any`) is still
//! handed its whole text, correctly. Every reader agrees: `from_str`,
//! `from_value`, `Document::deserialize` and per-node reads.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::cell::Cell;
use std::fmt;
use std::time::{Duration, Instant};

use oneyaml::{Document, Limits, Settings, Value, from_str_with, from_value};
use serde::de::{self, Deserialize, Deserializer, EnumAccess, VariantAccess, Visitor};

thread_local! {
    /// The variant names the probe enum declares (built at run time, so
    /// they can be as long as a `%TAG` prefix).
    static VARIANTS: Cell<&'static [&'static str]> = const { Cell::new(&[]) };
}

fn set_variants(names: Vec<String>) {
    let names: Vec<&'static str> = names
        .into_iter()
        .map(|n| &*Box::leak(n.into_boxed_str()))
        .collect();
    VARIANTS.with(|v| v.set(Box::leak(names.into_boxed_slice())));
}

/// Which variant a probe read, and how its name arrived.
#[derive(Debug, PartialEq, Clone)]
enum Name {
    /// Variant `index`; `zero_copy`: handed the variant's own `'static`
    /// name (the same bytes), not a copy.
    Variant { index: usize, zero_copy: bool },
    /// No variant has this name (a `#[serde(other)]`-style read).
    Other(String),
}

/// An enum whose variants are [`VARIANTS`], read with `deserialize_enum`
/// as derived enums are, with newtype content.
#[derive(Debug, PartialEq, Clone)]
struct Probe {
    name: Name,
    content: Value,
}

struct Ident(Name);

impl<'de> Deserialize<'de> for Ident {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Ident;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a variant name")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Ident, E> {
                let names = VARIANTS.with(Cell::get);
                // By address first (O(1)): the library's zero-copy match.
                if let Some(index) = names
                    .iter()
                    .position(|n| n.as_ptr() == s.as_ptr() && n.len() == s.len())
                {
                    return Ok(Ident(Name::Variant {
                        index,
                        zero_copy: true,
                    }));
                }
                Ok(Ident(match names.iter().position(|n| *n == s) {
                    Some(index) => Name::Variant {
                        index,
                        zero_copy: false,
                    },
                    None => Name::Other(s.to_owned()),
                }))
            }
        }
        d.deserialize_identifier(V)
    }
}

impl<'de> Deserialize<'de> for Probe {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Probe;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a probe enum")
            }
            fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Probe, A::Error> {
                let (Ident(name), access) = data.variant::<Ident>()?;
                let content = access.newtype_variant::<Value>()?;
                Ok(Probe { name, content })
            }
        }
        d.deserialize_enum("Probe", VARIANTS.with(Cell::get), V)
    }
}

fn untrusted() -> Settings {
    Settings::new().limits(Limits::untrusted())
}

/// `untrusted()` without the alias byte budget: the scaling
/// documents below repeat a 1 MB prefix per use, so the preset now
/// refuses them (`Y2009`, checked in `matching_variants_cost_their_suffix`).
fn untrusted_no_byte_budget() -> Settings {
    Settings::new().limits(Limits::untrusted().alias_byte_budget(usize::MAX))
}

/// Every reader's `Vec<T>` of `src` (a sequence): `from_str`,
/// `from_value`, `Document::deserialize`, and per-node reads.
fn all_readers<T>(src: &str, settings: &Settings) -> [Vec<T>; 4]
where
    T: de::DeserializeOwned + fmt::Debug,
{
    let a: Vec<T> = from_str_with(src, settings).unwrap();
    let v: Value = from_str_with(src, settings).unwrap();
    let b: Vec<T> = from_value(v).unwrap();
    let doc = Document::parse_with(src, settings).unwrap();
    let c: Vec<T> = doc.deserialize().unwrap();
    let d: Vec<T> = doc
        .root()
        .unwrap()
        .as_sequence()
        .unwrap()
        .iter()
        .map(|n| n.deserialize::<T>().unwrap())
        .collect();
    [a, b, c, d]
}

// ========================================================== one interpretation

/// The meaning a probe read: which variant (or name), and its content.
/// Whether the name arrived without a copy is not meaning: a verbatim
/// `!<tag:x,1:point>` read with the rest of its document shares the
/// `%TAG` spelling's instance (one instance per text), read
/// alone (`NodeRef::deserialize`) it is held in one piece.
fn meaning(v: &[Probe]) -> Vec<(Result<usize, String>, Value)> {
    v.iter()
        .map(|p| {
            let n = match &p.name {
                Name::Variant { index, .. } => Ok(*index),
                Name::Other(s) => Err(s.clone()),
            };
            (n, p.content.clone())
        })
        .collect()
}

fn zero_copy(p: &Probe) -> bool {
    matches!(
        p.name,
        Name::Variant {
            zero_copy: true,
            ..
        }
    )
}

/// Matching and non-matching `%TAG` tags, scalar and collection content,
/// local and verbatim tags, through an `Option` (a re-entered scalar):
/// every reader gives the same result, and a matching `%TAG` tag is handed
/// the variant's own name on every path.
#[test]
fn every_reader_reads_the_same_variant() {
    set_variants(vec![
        "tag:x,1:point".to_owned(),
        "tag:x,1:circle".to_owned(),
        "Local".to_owned(),
    ]);
    let src = "%TAG !e! tag:x,1:\n---\n\
               - !e!point v\n\
               - !e!circle {r: 1}\n\
               - !e!other w\n\
               - !e!other [1]\n\
               - !Local x\n\
               - !<tag:x,1:point> y\n\
               - !e!poin%74 z\n";
    let readers = all_readers::<Probe>(src, &Settings::new());
    for (i, r) in readers.iter().enumerate() {
        assert_eq!(meaning(r), meaning(&readers[0]), "reader {i}");
        // Every `%TAG`-spelled match is handed the variant's own name.
        for at in [0, 1, 6] {
            assert!(zero_copy(&r[at]), "reader {i}, item {at}: {:?}", r[at]);
        }
    }
    let a = &readers[0];
    let names: Vec<Name> = a.iter().map(|p| p.name.clone()).collect();
    let v = |index, zero_copy| Name::Variant { index, zero_copy };
    assert_eq!(
        names,
        [
            v(0, true),
            v(1, true),
            Name::Other("tag:x,1:other".to_owned()),
            Name::Other("tag:x,1:other".to_owned()),
            // Held in one piece: its own text (O(its length) anyway).
            v(2, false),
            // Verbatim: in a whole read, the `%TAG` spelling's instance
            // (equal text, one instance), so matched as it is.
            v(0, true),
            // An escape in the suffix: decoded, then matched.
            v(0, true),
        ]
    );
    assert_eq!(a[0].content, Value::from("v"));
    assert_eq!(a[2].content, Value::from("w"));
    // Re-entered through `Option` (the scalar handed back to the target).
    let readers = all_readers::<Option<Probe>>(src, &Settings::new());
    for r in &readers {
        let r: Vec<Probe> = r.iter().map(|p| p.clone().unwrap()).collect();
        assert_eq!(meaning(&r), meaning(a));
        for at in [0, 1, 6] {
            assert!(zero_copy(&r[at]), "item {at}: {:?}", r[at]);
        }
    }
}

/// A prefix that ends in an escape the suffix continues (non-ASCII escapes
/// stay as written, so the name holds them), and a prefix starting with `!`
/// (the variant name drops it): matched without a copy, as the tag's text
/// reads.
#[test]
fn split_characters_and_local_prefixes_match() {
    set_variants(vec![
        "tag:x,1:%C3%A9t%C3%A9".to_owned(),
        "loc-a".to_owned(),
        "!".to_owned(),
    ]);
    let src = "%TAG !e! tag:x,1:%C3\n%TAG !l! !loc-\n---\n\
               - !e!%A9t%C3%A9 v\n\
               - !l!a w\n\
               - !l!b x\n";
    let [a, b, c, d] = all_readers::<Probe>(src, &Settings::new());
    assert_eq!((&a, &a, &a), (&b, &c, &d));
    let names: Vec<Name> = a.iter().map(|p| p.name.clone()).collect();
    assert_eq!(
        names,
        [
            Name::Variant {
                index: 0,
                zero_copy: true
            },
            Name::Variant {
                index: 1,
                zero_copy: true
            },
            Name::Other("loc-b".to_owned()),
        ]
    );
}

#[derive(Debug, PartialEq, serde::Deserialize)]
enum Shape {
    #[serde(rename = "tag:x,1:point")]
    Point(String),
    #[serde(rename = "tag:x,1:circle")]
    Circle {
        r: u8,
    },
    Local(String),
    #[serde(other)]
    Rest,
}

/// Derived enums: a renamed variant matches its `%TAG` tag, a local tag its
/// variant, and an unknown tag falls to `#[serde(other)]`, the same way
/// for every reader.
#[test]
fn derived_enums_with_other_agree() {
    let src = "%TAG !e! tag:x,1:\n---\n\
               - !e!point v\n\
               - !e!circle {r: 1}\n\
               - !Local w\n\
               - !e!unknown\n\
               - !<tag:x,1:point> y\n";
    let [a, b, c, d] = all_readers::<Shape>(src, &Settings::new());
    assert_eq!((&a, &a, &a), (&b, &c, &d));
    assert_eq!(
        a,
        [
            Shape::Point("v".into()),
            Shape::Circle { r: 1 },
            Shape::Local("w".into()),
            Shape::Rest,
            Shape::Point("y".into()),
        ]
    );
}

/// An unknown `%TAG` variant is an error naming the whole tag (its text is
/// joined for the message, as before).
#[test]
fn unknown_variants_name_the_whole_tag() {
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    enum Strict {
        A(String),
    }
    let src = "%TAG !e! tag:x,1:\n--- !e!zz v\n";
    let e = oneyaml::from_str::<Strict>(src).unwrap_err();
    assert!(e.to_string().contains("tag:x,1:zz"), "{e}");
    let doc = Document::parse(src).unwrap();
    let e2 = doc.deserialize::<Strict>().unwrap_err();
    assert_eq!(e.code(), e2.code());
}

// =========================================================== scaling

static TIMING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn best(f: impl Fn()) -> Duration {
    let mut min = Duration::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        f();
        min = min.min(t.elapsed());
    }
    min
}

fn ratio(small: Duration, large: Duration) -> f64 {
    large.as_secs_f64() / small.as_secs_f64().max(1e-9)
}

/// `uses` items rotating over 20 `%TAG` tags of a `prefix`-byte prefix,
/// each naming one of the probe's 20 variants (set here).
fn rotating(prefix: usize, uses: usize) -> String {
    let p = "a".repeat(prefix);
    set_variants((0..20).map(|i| format!("tag:x,1:{p}x{i}")).collect());
    let mut s = format!("%TAG !e! tag:x,1:{p}\n---\n");
    for i in 0..uses {
        s.push_str(&format!("- !e!x{} v\n", i % 20));
    }
    s
}

type Run = Box<dyn Fn()>;

/// The typed-enum paths over `src`; each checks that every item was handed
/// its variant's own name (no join).
fn paths(src: &str) -> Vec<(&'static str, Run)> {
    let s = untrusted_no_byte_budget();
    let check = |v: &[Probe]| {
        assert!(v.iter().all(|p| matches!(
            p.name,
            Name::Variant {
                zero_copy: true,
                ..
            }
        )));
    };
    let owned = src.to_owned();
    let value: Value = from_str_with(src, &s).unwrap();
    let doc = Document::parse_with(src, &s).unwrap();
    let doc2 = doc.clone();
    let s2 = s.clone();
    vec![
        (
            "from_str",
            Box::new(move || check(&from_str_with::<Vec<Probe>>(&owned, &s2).unwrap())),
        ),
        (
            "from_value",
            Box::new(move || check(&from_value::<Vec<Probe>>(value.clone()).unwrap())),
        ),
        (
            "Document::deserialize",
            Box::new(move || check(&doc.deserialize::<Vec<Probe>>().unwrap())),
        ),
        (
            "node reads",
            Box::new(move || {
                let v: Vec<Probe> = doc2
                    .root()
                    .unwrap()
                    .as_sequence()
                    .unwrap()
                    .iter()
                    .map(|n| n.deserialize::<Probe>().unwrap())
                    .collect();
                check(&v);
            }),
        ),
    ]
}

fn scales(what: &str, small: &str, large: &str, bound: f64, node_reads: bool) {
    // (`rotating` set the variants for `large` last: re-set per input.)
    let prefix_of = |src: &str| {
        let line = src.lines().next().unwrap();
        line.len() - "%TAG !e! tag:x,1:".len()
    };
    let (ps, pl) = (prefix_of(small), prefix_of(large));
    let p = |n: usize| "a".repeat(n);
    let names =
        |n: usize| -> Vec<String> { (0..20).map(|i| format!("tag:x,1:{}x{i}", p(n))).collect() };
    let small_paths = {
        set_variants(names(ps));
        paths(small)
    };
    let small_times: Vec<Duration> = small_paths.iter().map(|(_, f)| best(f)).collect();
    set_variants(names(pl));
    let large_paths = paths(large);
    for ((name, f), ta) in large_paths.iter().zip(small_times) {
        if *name == "node reads" && !node_reads {
            continue;
        }
        let tb = best(f);
        println!("{what}, {name}: {ta:?} vs {tb:?} ({:.2}x)", ratio(ta, tb));
        assert!(
            ratio(ta, tb) < bound,
            "{what}, {name}: {:.2}x",
            ratio(ta, tb)
        );
    }
}

/// The bound: a 1 MB prefix, 2k vs 20k uses. Each path grows with
/// the uses (10×) at most; joining the prefix per use made 20k uses cost
/// 20 GB of copies (2.5 s at 160k uses before this unit).
#[test]
fn matching_variants_cost_their_suffix() {
    let _serial = TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The preset refuses the typed read (the logical size is
    // 2k × 1 MB of tag text), the expected new refusal; the scaling is of
    // reads with the byte budget lifted.
    let e = from_str_with::<Vec<Probe>>(&rotating(1_000_000, 2_000), &untrusted()).unwrap_err();
    assert_eq!(e.code(), oneyaml::code::ALIAS_BYTE_BUDGET_EXCEEDED);
    scales(
        "1 MB prefix, uses 2k/20k",
        &rotating(1_000_000, 2_000),
        &rotating(1_000_000, 20_000),
        15.0,
        true,
    );
    // And at 20k uses, a 1 MB prefix costs about what a 10-byte one does
    // (the input is 6× larger; per use, nothing grows with the prefix).
    // Not for node reads: each `NodeRef::deserialize` is one read, which
    // compares the prefix once against the matching variant's name (here
    // 1 MB long; a variant name is the program's own text, and serde's
    // derived identifier compares it in full anyway).
    scales(
        "20k uses, 10 B vs 1 MB prefix",
        &rotating(10, 20_000),
        &rotating(1_000_000, 20_000),
        12.0,
        false,
    );
}
