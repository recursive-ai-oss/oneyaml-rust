//! Every consumer reports the same meaning.
//!
//! - A tolerated duplicate key (`Y1014`, `DuplicateKeys::LastWins`) is
//!   reported once per source duplicate by `Value`, typed and `IgnoredAny`
//!   reads and by `Document`, however often aliases or merge keys replay
//!   it (alias keys and repeated `<<` included).
//! - Under `Schema::Json`, the first error of a variant mapping whose key
//!   the enum refuses is the one `Value`, `IgnoredAny` and a strict
//!   `Document` report: the mapping's content is judged first, duplicate
//!   keys included.
//! - A target may ignore a target error (`Y3xxx`, the
//!   `serde_with::DefaultOnError` / `.ok()` pattern) and read on, in step:
//!   the refused node, or the rest of a partly read collection, is skipped
//!   and checked. Ignoring the input's own error, or an error whose rest
//!   cannot be skipped, is `Y3018`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::fmt;

use oneyaml::{
    Document, DuplicateKeys, Schema, Settings, Span, Value, code, from_str_with,
    from_str_with_diagnostics, from_value,
};
use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

/// A typed target that reads everything through serde's visitor protocol
/// (not the `Value` builder): keys and values of mappings, items of
/// sequences, any scalar.
struct Tree;

impl<'de> Deserialize<'de> for Tree {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Tree;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Tree, A::Error> {
                while a.next_key::<Tree>()?.is_some() {
                    a.next_value::<Tree>()?;
                }
                Ok(Tree)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Tree, A::Error> {
                while a.next_element::<Tree>()?.is_some() {}
                Ok(Tree)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_i128<E: de::Error>(self, _: i128) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_u128<E: de::Error>(self, _: u128) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_none<E: de::Error>(self) -> Result<Tree, E> {
                Ok(Tree)
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Tree, D::Error> {
                Tree::deserialize(d)
            }
            fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Tree, D::Error> {
                Tree::deserialize(d)
            }
            fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<Tree, A::Error> {
                use serde::de::VariantAccess;
                let (Tree, v) = a.variant::<Tree>()?;
                v.newtype_variant::<Tree>()?;
                Ok(Tree)
            }
        }
        d.deserialize_any(V)
    }
}

/// The primary spans of the `Y1014` warnings each consumer reports.
fn duplicate_spans(src: &str, s: &Settings) -> [(&'static str, Vec<Span>); 4] {
    let spans = |ds: &[oneyaml::Diagnostic]| -> Vec<Span> {
        let mut v: Vec<Span> = ds
            .iter()
            .filter(|d| d.code == code::DUPLICATE_KEY_LAST_WINS)
            .map(|d| d.primary.span)
            .collect();
        v.sort_by_key(|sp| (sp.start, sp.end));
        v
    };
    let (_, w) = from_str_with_diagnostics::<Value>(src, s).unwrap();
    let value = spans(&w);
    let (_, w) = from_str_with_diagnostics::<Tree>(src, s).unwrap();
    let typed = spans(&w);
    let (_, w) = from_str_with_diagnostics::<IgnoredAny>(src, s).unwrap();
    let ignored = spans(&w);
    let doc = Document::parse_with(src, s).unwrap();
    let document = spans(&doc.diagnostics());
    [
        ("Value", value),
        ("typed", typed),
        ("IgnoredAny", ignored),
        ("Document", document),
    ]
}

#[test]
fn a_tolerated_duplicate_is_reported_once_per_source_duplicate() {
    let s = Settings::new()
        .duplicate_keys(DuplicateKeys::LastWins)
        .merge_keys(true);
    for (src, want) in [
        // An anchored mapping with a duplicate, replayed by aliases.
        ("a: &x {k: 1, k: 2}\nb: *x\nc: [*x, *x]\n", 1),
        // Nested replays: an alias inside an anchored node that is replayed.
        ("a: &x {k: 1, k: 2}\nb: &y [*x]\nc: *y\nd: [*y, *y]\n", 1),
        // Alias keys: `*k` is `key`.
        ("a: &k key\nm: {*k : 1, key: 2}\n", 1),
        ("a: &k key\nn: &n {*k : 1, *k : 2}\no: *n\np: *n\n", 1),
        // A merge source with a duplicate, merged (replayed) twice.
        (
            "b: &b {x: 1, x: 2}\nm: {<<: *b, y: 1}\nn: {<<: [*b, *b]}\n",
            1,
        ),
        // A repeated `<<` (a duplicate key), in a replayed mapping.
        (
            "b: &b {x: 1}\nm: &m {<<: *b, <<: *b}\nn: *m\no: {<<: *m}\n",
            1,
        ),
        // Two source duplicates, each replayed.
        ("a: &x {k: 1, k: 2, j: 1, j: 2}\nb: *x\n", 2),
        // An explicit key overriding a merged one is no duplicate.
        ("b: &b {x: 1}\nm: {<<: *b, x: 2}\nn: *b\n", 0),
    ] {
        let all = duplicate_spans(src, &s);
        for (who, spans) in &all {
            assert_eq!(spans.len(), want, "{who}: {src:?}: {spans:?}");
            assert_eq!(spans, &all[0].1, "{who} vs Value: {src:?}");
        }
    }
}

#[derive(Debug, PartialEq, serde::Deserialize)]
#[allow(dead_code)]
enum Shape {
    Square(f64),
    Circle { r: f64 },
}

#[test]
fn the_first_error_under_json_is_the_same_on_every_path() {
    let s = Settings::new().schema(Schema::Json);
    for src in [
        // A variant mapping the enum refuses (its key, a collection key, a
        // second entry); a plain scalar after it matches no JSON pattern
        // (Y1010) and is the first error everywhere.
        "!Square 2.0: x\n",
        "{\"Unknown\": hello}\n",
        "{!Square 1: [2, nope]}\n",
        "{[\"a\"]: 1, \"b\": x}\n",
        "{[1]: hello}\n",
        "{[a]: 1}\n",
        "{\"Square\": 1, \"other\": x}\n",
        "{\"Square\": 1, \"Square\": x}\n",
    ] {
        let value = from_str_with::<Value>(src, &s).unwrap_err();
        let typed = from_str_with::<Shape>(src, &s).unwrap_err();
        let ignored = from_str_with::<IgnoredAny>(src, &s).unwrap_err();
        let doc = Document::parse_with(src, &s).unwrap_err();
        assert_eq!(
            value.code(),
            code::JSON_UNMATCHED_SCALAR,
            "{src:?}: {value}"
        );
        for (who, e) in [
            ("typed", &typed),
            ("IgnoredAny", &ignored),
            ("Document", &doc),
        ] {
            assert_eq!(e.code(), value.code(), "{who}: {src:?}: {e}");
            assert_eq!(e.span(), value.span(), "{who}: {src:?}: {e}");
        }
    }
    // With clean content, the target's own rules still decide.
    let e = from_str_with::<Shape>("{\"Square\": 1, \"other\": 2}\n", &s).unwrap_err();
    assert_eq!(e.code(), code::INVALID_LENGTH);
}

/// Variant mappings with duplicate keys after a refused variant key, a
/// second entry, or a collection key: `Y1013`, at the duplicate, on every
/// path, in the default schema and under `Schema::Json`.
#[test]
fn duplicates_in_a_refused_variant_mapping_come_first() {
    let json = Settings::new().schema(Schema::Json);
    for (src, s, at) in [
        ("{Unknown: 1, Unknown: 2}\n", Settings::new(), 13),
        ("{Square: 1, other: 2, other: 3}\n", Settings::new(), 22),
        ("{[a]: 1, [a]: 2}\n", Settings::new(), 9),
        ("{Square: 1, Square: 2}\n", Settings::new(), 12),
        ("{Nope: [x], other: 1, Nope: 2}\n", Settings::new(), 22),
        ("{\"Unknown\": 1, \"Unknown\": 2}\n", json.clone(), 15),
        (
            "{\"Square\": 1, \"other\": 2, \"other\": 3}\n",
            json.clone(),
            26,
        ),
        ("{[\"a\"]: 1, [\"a\"]: 2}\n", json, 11),
    ] {
        let value = from_str_with::<Value>(src, &s).unwrap_err();
        assert_eq!(value.code(), code::DUPLICATE_KEY, "{src:?}: {value}");
        assert_eq!(value.span().map(|sp| sp.start), Some(at), "{src:?}");
        for (who, e) in [
            ("typed", from_str_with::<Shape>(src, &s).unwrap_err()),
            (
                "IgnoredAny",
                from_str_with::<IgnoredAny>(src, &s).unwrap_err(),
            ),
            ("Document", Document::parse_with(src, &s).unwrap_err()),
        ] {
            assert_eq!(e.code(), value.code(), "{who}: {src:?}: {e}");
            assert_eq!(e.span(), value.span(), "{who}: {src:?}: {e}");
        }
    }
}

/// `serde_with::DefaultOnError`: the target's default when reading fails.
#[derive(Debug, PartialEq)]
struct OrDefault<T>(T);

impl<'de, T: Deserialize<'de> + Default> Deserialize<'de> for OrDefault<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(OrDefault(T::deserialize(d).unwrap_or_default()))
    }
}

/// The `.ok()` pattern.
#[derive(Debug, PartialEq)]
struct Lenient<T>(Option<T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Lenient(T::deserialize(d).ok()))
    }
}

#[derive(Debug, Default, PartialEq, serde::Deserialize)]
struct Inner {
    a: u8,
    b: u8,
}

/// Reads one item of a sequence, then fails (a partly read collection).
#[derive(Debug, Default, PartialEq)]
struct TakeOne;

impl<'de> Deserialize<'de> for TakeOne {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = TakeOne;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<TakeOne, A::Error> {
                a.next_element::<IgnoredAny>()?;
                Err(de::Error::custom("one is enough"))
            }
        }
        d.deserialize_seq(V)
    }
}

#[derive(Debug, PartialEq, serde::Deserialize)]
struct Outer<X> {
    x: X,
    y: u8,
}

/// `from_str`, a `Document` node read, and (for valid input) `from_value`
/// agree; the outcome is `from_str`'s.
fn read<X>(src: &str, s: &Settings) -> Result<Outer<X>, oneyaml::Code>
where
    X: for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
{
    let direct = from_str_with::<Outer<X>>(src, s).map_err(|e| e.code());
    if let Ok(doc) = Document::parse_with(src, s) {
        let node = doc.root().unwrap().deserialize::<Outer<X>>();
        assert_eq!(node.map_err(|e| e.code()), direct, "node read: {src:?}");
        let v: Value = from_str_with(src, s).unwrap();
        let via = from_value::<Outer<X>>(v).map_err(|e| e.code());
        assert_eq!(via, direct, "from_value: {src:?}");
    }
    direct
}

#[test]
fn an_ignored_target_error_reads_on_in_step() {
    let s = Settings::new();
    // The known triggers: the collection was read to its end.
    let r = read::<Lenient<Inner>>("x: {a: 1}\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: Lenient(None),
            y: 2
        })
    );
    let r = read::<Lenient<[u8; 2]>>("x: [1]\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: Lenient(None),
            y: 2
        })
    );
    let r = read::<OrDefault<Inner>>("x: {a: 1}\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: OrDefault(Inner::default()),
            y: 2
        })
    );
    // Refused before anything inside was read: skipped whole.
    for src in [
        "x: [1, {k: [2]}]\ny: 2\n",
        "x: !t {k: v}\ny: 2\n",
        "x: 7\ny: 2\n",
    ] {
        let r = read::<Lenient<Inner>>(src, &s);
        assert_eq!(
            r,
            Ok(Outer {
                x: Lenient(None),
                y: 2
            }),
            "{src:?}"
        );
    }
    let r = read::<Lenient<u8>>("x: {a: [1]}\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: Lenient(None),
            y: 2
        })
    );
    let r = read::<Lenient<[u8; 1]>>("x: [1, [2], 3]\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: Lenient(None),
            y: 2
        })
    );
    // Partly read: the rest is skipped.
    let r = read::<Lenient<TakeOne>>("x: [1, [2, 3], {k: v}]\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: Lenient(None),
            y: 2
        })
    );
    let r = read::<OrDefault<Inner>>("x: {a: 1, b: no, c: [1, {d: 2}]}\ny: 2\n", &s);
    assert_eq!(
        r,
        Ok(Outer {
            x: OrDefault(Inner::default()),
            y: 2
        })
    );
    // A variant mapping: refused key, bad content, second entry.
    for src in [
        "x: {Nope: [1]}\ny: 2\n",
        "x: {Square: [1], other: {k: v}}\ny: 2\n",
        "x: {Square: 1, other: 2}\ny: 2\n",
        "x: {[k]: 1}\ny: 2\n",
        "x: !Nope [1]\ny: 2\n",
    ] {
        let r = read::<Lenient<Shape>>(src, &s);
        assert_eq!(r.map(|o| (o.x.0.is_none(), o.y)), Ok((true, 2)), "{src:?}");
    }
}

#[test]
fn an_ignored_input_error_poisons_the_read() {
    let y3018 = Err(code::READ_AFTER_IGNORED_ERROR);
    let s = Settings::new();
    // An input error in the unread rest (a duplicate key, also of a key
    // read before the error): the rest cannot be skipped.
    for src in [
        "x: {a: 1, b: no, a: 2}\ny: 2\n",
        "x: {a: 1, b: no, c: {d: 1, d: 2}}\ny: 2\n",
        "x: [1, {k: 1, k: 2}]\ny: 2\n",
        "x: [{k: 1, k: 2}]\ny: 2\n",
    ] {
        assert_eq!(
            from_str_with::<Value>(src, &s).unwrap_err().code(),
            code::DUPLICATE_KEY
        );
        let r = from_str_with::<Outer<Lenient<Inner>>>(src, &s).map(|_| ());
        let t = from_str_with::<Outer<Lenient<TakeOne>>>(src, &s).map(|_| ());
        assert_eq!(r.map_err(|e| e.code()), y3018, "{src:?}");
        assert_eq!(t.map_err(|e| e.code()), y3018, "{src:?}");
    }
    // The input's own error, ignored: under `Schema::Json` `hello` matches
    // no JSON pattern.
    let json = Settings::new().schema(Schema::Json);
    let src = "{\"x\": {\"a\": hello}, \"y\": 2}";
    assert_eq!(
        from_str_with::<Value>(src, &json).unwrap_err().code(),
        code::JSON_UNMATCHED_SCALAR
    );
    let r = from_str_with::<Outer<Lenient<Inner>>>(src, &json).map(|_| ());
    assert_eq!(r.map_err(|e| e.code()), y3018);
}

/// `from_slice` decodes once (`decode_strict`) and
/// reports the first decoding error exactly as `first_error` does.
#[test]
fn from_slice_reports_the_first_decoding_error() {
    let cases: &[&[u8]] = &[
        b"a: \xFF\nb: \xFE\n",
        b"k: v\n\xC3",
        b"\xFF\xFEa\x00:\x00 \x00\x00\xD8\n\x00",
        b"\x00\x00\xFE\xFFx",
        b"a\x00\x00\x00\x00\xD8\x00\x00",
    ];
    for bytes in cases {
        let (d, _) = oneyaml_syntax::first_error(bytes).expect("an invalid input");
        let e = oneyaml::from_slice::<Value>(bytes).unwrap_err();
        assert_eq!(e.code(), d.code, "{bytes:?}");
        assert_eq!(e.span(), Some(d.primary.span), "{bytes:?}");
        assert!(e.to_string().contains(&d.message), "{bytes:?}: {e}");
    }
    // Valid input in each encoding still reads.
    let v: Value = oneyaml::from_slice(b"\xFF\xFEa\x00:\x00 \x001\x00").unwrap();
    assert_eq!(v, oneyaml::from_str::<Value>("a: 1").unwrap());
    let v: Value = oneyaml::from_slice(b"a: 1").unwrap();
    assert_eq!(v, oneyaml::from_str::<Value>("a: 1").unwrap());
}

// -------------------------------------------------------------- ignored errors

/// Swallows the error of `variant()` (and of the content) and returns.
#[derive(Debug, PartialEq)]
struct SwallowsVariant;

impl<'de> Deserialize<'de> for SwallowsVariant {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = SwallowsVariant;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an enum")
            }
            fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<SwallowsVariant, A::Error> {
                use serde::de::VariantAccess;
                if let Ok((Known, v)) = a.variant::<Known>() {
                    let _ = v.newtype_variant::<u8>();
                }
                Ok(SwallowsVariant)
            }
        }
        d.deserialize_enum("E", &["A"], V)
    }
}

/// A variant name that only `A` is.
struct Known;

impl<'de> Deserialize<'de> for Known {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        if name == "A" {
            Ok(Known)
        } else {
            Err(de::Error::unknown_variant(&name, &["A"]))
        }
    }
}

/// Reads a mapping's keys as `u8`, ignoring every error, values ignored.
#[derive(Debug, PartialEq)]
struct IgnoresKeys;

impl<'de> Deserialize<'de> for IgnoresKeys {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = IgnoresKeys;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a mapping")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<IgnoresKeys, A::Error> {
                let mut errors = 0;
                loop {
                    match a.next_key::<u8>() {
                        Ok(Some(_)) => {
                            a.next_value::<IgnoredAny>()?;
                        }
                        Ok(None) => return Ok(IgnoresKeys),
                        Err(e) => {
                            errors += 1;
                            if errors > 4 {
                                return Err(e);
                            }
                        }
                    }
                }
            }
        }
        d.deserialize_map(V)
    }
}

/// `from_str`, a node read and `from_value` agree, by code; never Y9000.
fn agree<T: for<'de> Deserialize<'de>>(src: &str, s: &Settings) -> Result<(), oneyaml::Code> {
    let code = |r: Result<T, oneyaml::Error>| r.map(|_| ()).map_err(|e| e.code());
    let direct = code(from_str_with::<T>(src, s));
    assert_ne!(direct, Err(code::INTERNAL_ERROR), "from_str: {src:?}");
    if let Ok(doc) = Document::parse_with(src, s) {
        let node = code(doc.root().unwrap().deserialize::<T>());
        assert_eq!(node, direct, "node read: {src:?}");
        let v: Value = from_str_with(src, s).unwrap();
        assert_eq!(
            code(oneyaml::from_value_with::<T>(v, s)),
            direct,
            "from_value: {src:?}"
        );
    }
    direct
}

#[test]
fn known_ignored_errors_read_on() {
    let s = Settings::new();
    // 1: `variant()`'s error swallowed in a variant mapping; 3: on a tagged
    // collection.
    for src in [
        "x: {Z: 1}\ny: 2\n",
        "x: {A: [1]}\ny: 2\n",
        "x: {Z: [1, {k: v}]}\ny: 2\n",
        "x: !Z [1]\ny: 2\n",
        "x: !Z {k: [1]}\ny: 2\n",
    ] {
        assert_eq!(agree::<Outer<SwallowsVariant>>(src, &s), Ok(()), "{src:?}");
    }
    // A swallowed variant error with a second entry: the target's rule.
    assert_eq!(
        agree::<Outer<SwallowsVariant>>("x: {Z: 1, W: 2}\ny: 2\n", &s),
        Err(code::INVALID_LENGTH)
    );
    // 2a: a key error ignored; the next key is read, not the value.
    for src in [
        "{a: b, c: d}",
        "{a: [b], 1: {c: d}, e: f}",
        "{[a]: b, 2: c}",
    ] {
        assert_eq!(agree::<IgnoresKeys>(src, &s), Ok(()), "{src:?}");
    }
    // 2b: the input's own error in a key, ignored: Y3018, as `Value` and
    // `Document` refuse it (Y1007).
    let src = "{? [1, !!int x]\n: 1, b: 2}";
    assert_eq!(
        from_str_with::<Value>(src, &s).unwrap_err().code(),
        code::INVALID_SCALAR_FOR_TAG
    );
    assert_eq!(
        Document::parse_with(src, &s).unwrap_err().code(),
        code::INVALID_SCALAR_FOR_TAG
    );
    assert_eq!(
        agree::<IgnoresKeys>(src, &s),
        Err(code::READ_AFTER_IGNORED_ERROR)
    );
    let src = "{!!int x: 1, b: 2}";
    assert_eq!(
        agree::<IgnoresKeys>(src, &s),
        Err(code::READ_AFTER_IGNORED_ERROR)
    );
}

/// Item 4: a collection key and an alias of an equal (or the same)
/// collection are duplicates for `Document` too, with the same codes and
/// spans as the serde paths.
#[test]
fn alias_collection_keys_are_duplicates_everywhere() {
    for src in [
        "{? &ck [1] : 1, ? *ck : 2}",
        "{? &ck {a: 1} : 1, ? *ck : 2}",
        "x: &ck [1]\ny: {? *ck : 1, [1]: 2}",
        "x: &ck {b: [1], a: 2}\ny: {{a: 2, b: [1]}: 1, ? *ck : 2}",
        "x: &ck [1]\ny: {? *ck : 1, ? *ck : 2}",
        "x: &o [&i [1]]\ny: {? *o : 1, [[1]]: 2}",
        "x: &i [1]\ny: &o [*i]\nz: {? *o : 1, [[1]]: 2}",
        "x: &ck [1]\ny: {? [*ck] : 1, [[1]]: 2}",
    ] {
        let value = from_str_with::<Value>(src, &Settings::new()).unwrap_err();
        let doc = Document::parse(src).unwrap_err();
        let typed = from_str_with::<IgnoredAny>(src, &Settings::new()).unwrap_err();
        assert_eq!(value.code(), code::DUPLICATE_KEY, "{src:?}");
        assert_eq!(
            (doc.code(), doc.span()),
            (value.code(), value.span()),
            "{src:?}"
        );
        assert_eq!(
            (typed.code(), typed.span()),
            (value.code(), value.span()),
            "{src:?}"
        );
    }
    // Distinct collections stay distinct.
    for src in [
        "x: &ck [1]\ny: {? *ck : 1, [2]: 2}",
        "x: &ck [1]\ny: {? *ck : 1, ? [*ck] : 2}",
    ] {
        assert!(Document::parse(src).is_ok(), "{src:?}");
        assert!(
            from_str_with::<Value>(src, &Settings::new()).is_ok(),
            "{src:?}"
        );
    }
    // LastWins: every tolerated duplicate, with its span, on both.
    let s = Settings::new().duplicate_keys(DuplicateKeys::LastWins);
    let src = "a: &a {? &ck [1] : 1, ? *ck : 2, [1]: 3}\nb: *a\n";
    let spans = |ds: &[oneyaml::Diagnostic]| -> Vec<Span> {
        ds.iter()
            .filter(|d| d.code == code::DUPLICATE_KEY_LAST_WINS)
            .map(|d| d.primary.span)
            .collect()
    };
    let (_, w) = from_str_with_diagnostics::<Value>(src, &s).unwrap();
    let doc = Document::parse_with(src, &s).unwrap();
    assert_eq!(spans(&w), [Span::new(24, 27), Span::new(33, 34)]);
    assert_eq!(spans(&doc.diagnostics()), spans(&w));
}

/// The chaos target: reads everything, asking for assorted types on a
/// fixed schedule, and ignores every error it gets (bounded, so a
/// poisoned read ends). Shared by every path through `SCHEDULE`.
struct Chaos;

thread_local! {
    static SCHEDULE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn tick() -> usize {
    SCHEDULE.with(|c| {
        let n = c.get();
        c.set(n.wrapping_add(1));
        n
    })
}

/// One node read as the schedule says, its error ignored.
struct Probe;

impl<'de> Deserialize<'de> for Probe {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let _ignored = match tick() % 7 {
            0 => u8::deserialize(d).map(|_| ()),
            1 => <[u8; 2]>::deserialize(d).map(|_| ()),
            2 => Inner::deserialize(d).map(|_| ()),
            3 => Shape::deserialize(d).map(|_| ()),
            4 => String::deserialize(d).map(|_| ()),
            5 => SwallowsVariant::deserialize(d).map(|_| ()),
            _ => Chaos::deserialize(d).map(|_| ()),
        };
        Ok(Probe)
    }
}

impl<'de> Deserialize<'de> for Chaos {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Chaos;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Chaos, A::Error> {
                let mut errors = 0;
                loop {
                    match a.next_element::<Probe>() {
                        Ok(Some(_)) => {}
                        Ok(None) => return Ok(Chaos),
                        Err(e) => {
                            errors += 1;
                            if errors > 3 {
                                return Err(e);
                            }
                        }
                    }
                }
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Chaos, A::Error> {
                let mut errors = 0;
                loop {
                    let r = if tick() % 3 == 0 {
                        a.next_key::<u8>().map(|k| k.map(|_| ()))
                    } else {
                        a.next_key::<Probe>().map(|k| k.map(|_| ()))
                    };
                    match r {
                        Ok(Some(())) => {
                            if tick() % 4 != 0 {
                                let _ = a.next_value::<Probe>();
                            }
                        }
                        Ok(None) => return Ok(Chaos),
                        Err(e) => {
                            errors += 1;
                            if errors > 3 {
                                return Err(e);
                            }
                        }
                    }
                }
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_i128<E: de::Error>(self, _: i128) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_u128<E: de::Error>(self, _: u128) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_none<E: de::Error>(self) -> Result<Chaos, E> {
                Ok(Chaos)
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Chaos, D::Error> {
                Chaos::deserialize(d)
            }
            fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Chaos, D::Error> {
                Chaos::deserialize(d)
            }
            fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<Chaos, A::Error> {
                use serde::de::VariantAccess;
                if let Ok((Probe, v)) = a.variant::<Probe>() {
                    let _ = v.newtype_variant::<Probe>();
                }
                Ok(Chaos)
            }
        }
        d.deserialize_any(V)
    }
}

/// Reads like [`Chaos`], but returns `Ok` right after the first
/// error it ignores (a `DefaultOnError` that stops reading).
struct OneShot;

impl<'de> Deserialize<'de> for OneShot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OneShot;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<OneShot, A::Error> {
                loop {
                    let r = if tick() % 2 == 0 {
                        a.next_element::<OneShot>().map(|x| x.map(|_| ()))
                    } else {
                        a.next_element::<u8>().map(|x| x.map(|_| ()))
                    };
                    match r {
                        Ok(Some(())) => {}
                        Ok(None) | Err(_) => return Ok(OneShot),
                    }
                }
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<OneShot, A::Error> {
                loop {
                    let k = if tick() % 2 == 0 {
                        a.next_key::<OneShot>().map(|x| x.map(|_| ()))
                    } else {
                        a.next_key::<u8>().map(|x| x.map(|_| ()))
                    };
                    match k {
                        Ok(Some(())) => {
                            if a.next_value::<OneShot>().is_err() {
                                return Ok(OneShot);
                            }
                        }
                        Ok(None) | Err(_) => return Ok(OneShot),
                    }
                }
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_i128<E: de::Error>(self, _: i128) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_u128<E: de::Error>(self, _: u128) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_unit<E: de::Error>(self) -> Result<OneShot, E> {
                Ok(OneShot)
            }
            fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<OneShot, A::Error> {
                use serde::de::VariantAccess;
                // The name is read without the schedule: a plain number,
                // bool or null names a variant for `from_str` (its source
                // text) but not for `from_value` (no text: the documented
                // difference), so reading it must not shift the schedule.
                if let Ok((IgnoredAny, v)) = a.variant::<IgnoredAny>() {
                    let _ = v.newtype_variant::<OneShot>();
                }
                Ok(OneShot)
            }
        }
        if tick() % 5 == 0 {
            d.deserialize_enum("E", &["A"], V)
        } else {
            d.deserialize_any(V)
        }
    }
}

/// Reads `src` as `T` on `path`, from schedule `seed`.
fn chaos_read<T: for<'de> Deserialize<'de>>(
    path: u8,
    src: &str,
    s: &Settings,
    seed: usize,
) -> Option<Result<(), oneyaml::Code>> {
    SCHEDULE.with(|c| c.set(seed));
    let code = |r: Result<T, oneyaml::Error>| r.map(|_| ()).map_err(|e| e.code());
    match path {
        0 => Some(code(from_str_with::<T>(src, s))),
        1 => {
            let doc = Document::parse_with(src, s).ok()?;
            Some(code(doc.root()?.deserialize::<T>()))
        }
        _ => {
            let v: Value = from_str_with(src, s).ok()?;
            Some(code(oneyaml::from_value_with::<T>(v, s)))
        }
    }
}

/// The yaml-test-suite inputs, and the known shapes.
fn corpus() -> Vec<String> {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    out.push(t);
                }
            }
        }
    }
    out.sort();
    // Truncations (every fifth character boundary) and inputs
    // with an inserted bad tag or undefined alias, so syntax and meaning
    // errors meet ignoring targets everywhere.
    let whole = out.clone();
    for (i, src) in whole.iter().enumerate() {
        for (cut, _) in src.char_indices().skip(1).step_by(5) {
            out.push(src[..cut].to_owned());
        }
        let at = src
            .char_indices()
            .map(|(k, _)| k)
            .nth(i % src.chars().count().max(1))
            .unwrap_or(0);
        for insert in ["!!int q ", "*nope ", "{a: 1, a: 2} "] {
            let mut m = src.clone();
            m.insert_str(at, insert);
            out.push(m);
        }
    }
    for extra in [
        "---\n[ [ a, b, c ]\n",
        "- {a: !!int q b, c: d}",
        "{*nope : 1, b: 2}",
        "{? {a: 1, a: 2}\n: 1, b: 2}",
        "x: {Z: 1}\ny: 2\n",
        "x: {A: [1]}\ny: 2\n",
        "{a: b, c: d}",
        "{? [1, !!int x]\n: 1, b: 2}",
        "x: !Z [1]\ny: 2\n",
        "a: &a {k: [1, 2], j: {m: n}}\nb: *a\nc: {<<: *a, k: 1}\n",
        "{? &ck [1] : 1, ? *ck : 2}",
        "[{Square: [1]}, {Circle: {r: x}}, !Square 2, !Nope {a: 1}]",
        "{a: 1, b: no, c: [1, {d: 2}], e: {f: [g]}}",
    ] {
        out.push(extra.to_owned());
    }
    out
}

/// Whatever a target ignores, no read ends in Y9000,
/// invalid input (one the `Value` read refuses) is never accepted, and
/// `from_str`, a node read and `from_value` agree.
#[test]
#[ignore = "slow: runs in CI"]
fn ignoring_every_error_never_breaks_a_read() {
    let settings = [
        Settings::new(),
        Settings::new()
            .duplicate_keys(DuplicateKeys::LastWins)
            .merge_keys(true),
        Settings::new().schema(Schema::Json),
    ];
    fn one<T: for<'de> Deserialize<'de>>(src: &str, s: &Settings, seed: usize, valid: bool) {
        let direct = chaos_read::<T>(0, src, s, seed).unwrap();
        if direct == Err(code::INTERNAL_ERROR) {
            SCHEDULE.with(|c| c.set(seed));
            let e = from_str_with::<T>(src, s).map(|_| ()).unwrap_err();
            assert_ne!(
                e.code(),
                code::INTERNAL_ERROR,
                "{}: {src:?} {seed}: {e}",
                std::any::type_name::<T>()
            );
        }
        assert!(
            valid || direct.is_err(),
            "invalid input accepted: {src:?} {seed}"
        );
        for path in [1, 2] {
            if let Some(other) = chaos_read::<T>(path, src, s, seed) {
                assert_eq!(other, direct, "path {path}: {src:?} seed {seed}");
            }
        }
    }
    let mut reads = 0usize;
    let mut invalid = 0usize;
    for src in corpus() {
        for s in &settings {
            let valid = from_str_with::<Value>(&src, s).is_ok();
            invalid += usize::from(!valid);
            for seed in 0..3 {
                one::<Chaos>(&src, s, seed, valid);
                one::<OneShot>(&src, s, seed, valid);
                reads += 2;
            }
        }
    }
    assert!(reads > 50_000 && invalid > 5_000, "{reads} {invalid}");
}

/// The known triggers — a visitor that ignores one key
/// error and returns `Ok` (the rest of the mapping holds the input's
/// error), and an ignore-everything visitor on syntax errors: `Y3018`
/// (an error, never `Y9000` nor a value).
#[test]
fn an_ignored_input_error_is_never_a_value() {
    struct FirstKey;
    impl<'de> Deserialize<'de> for FirstKey {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = FirstKey;
                fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str("a mapping")
                }
                fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<FirstKey, A::Error> {
                    let _ = a.next_key::<IgnoredAny>();
                    Ok(FirstKey)
                }
            }
            d.deserialize_map(V)
        }
    }
    let s = Settings::new();
    for src in [
        "{? [1, !!int x]\n: 1, b: 2}",
        "{*nope : 1, b: 2}",
        "{? {a: 1, a: 2}\n: 1, b: 2}",
    ] {
        assert!(from_str_with::<Value>(src, &s).is_err(), "{src:?}");
        let e = from_str_with::<FirstKey>(src, &s).map(|_| ()).unwrap_err();
        assert_eq!(e.code(), code::READ_AFTER_IGNORED_ERROR, "{src:?}: {e}");
    }
    for src in ["---\n[ [ a, b, c ]\n", "- {a: !!int q b, c: d}"] {
        for seed in 0..7 {
            for r in [
                chaos_read::<Chaos>(0, src, &s, seed).unwrap(),
                chaos_read::<OneShot>(0, src, &s, seed).unwrap(),
            ] {
                let e = r.unwrap_err();
                assert_ne!(e, code::INTERNAL_ERROR, "{src:?}");
            }
        }
    }
    // Minor 4: a sequence visitor that ignored a poisoning error and
    // returned before the end: Y3018, not the target's count error.
    let e = from_str_with::<Outer<Lenient<OneShotSeq>>>("x: [{a: 1, a: 2}, 2, 3]\ny: 2\n", &s)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code(), code::READ_AFTER_IGNORED_ERROR, "{e}");
}

/// Reads one item of a sequence as `u8`, ignores its error, and returns.
#[derive(Debug, PartialEq)]
struct OneShotSeq;

impl<'de> Deserialize<'de> for OneShotSeq {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OneShotSeq;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<OneShotSeq, A::Error> {
                let _ = a.next_element::<u8>();
                Ok(OneShotSeq)
            }
        }
        d.deserialize_seq(V)
    }
}

/// `n` items, then `uses` alias keys of them, then `last`.
fn alias_keys(n: usize, uses: usize, last: &str) -> String {
    let items: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    let mut src = format!("a: &n [{}]\ntop:\n", items.join(", "));
    for _ in 0..uses {
        src.push_str("- {? *n : 1}\n");
    }
    src.push_str(last);
    src.push('\n');
    src
}

/// `Document` gives an alias key its
/// anchor's value within a copy budget no smaller than the `Value` read's
/// alias node budget, so the paths agree while that read is within its
/// budget; past it that read refuses (`Y2004`) and `Document` keeps the
/// alias's opaque identity (never a false duplicate).
#[test]
fn the_copy_budget_covers_the_value_reads_alias_budget() {
    let untrusted = Settings::new().limits(oneyaml::Limits::untrusted());
    let inline = |n: usize| {
        let items: Vec<String> = (0..n).map(|i| i.to_string()).collect();
        format!("[{}]", items.join(", "))
    };
    // The known shapes: a duplicate after many alias keys, inside the
    // `Value` read's budget (default: 10,000 x 90; untrusted: 3,000 x 30).
    for (n, uses, s) in [
        (10_000, 90, Settings::new()),
        (3_000, 30, untrusted.clone()),
    ] {
        let src = alias_keys(n, uses, &format!("- {{? *n : 1, {}: 2}}", inline(n)));
        let serde = from_str_with::<Value>(&src, &s).unwrap_err();
        let doc = Document::parse_with(src.as_str(), &s).unwrap_err();
        assert_eq!(serde.code(), code::DUPLICATE_KEY, "{n} x {uses}");
        assert_eq!(
            (doc.code(), doc.span()),
            (serde.code(), serde.span()),
            "{n} x {uses}"
        );
        // An error under an alias key: the same path (the anchor's value).
        let src = alias_keys(n, uses, "- {? *n : {k: !!int x}}");
        let serde = from_str_with::<Value>(&src, &s).unwrap_err();
        let doc = Document::parse_with(src.as_str(), &s).unwrap_err();
        assert_eq!(doc.code(), code::INVALID_SCALAR_FOR_TAG);
        assert_eq!(
            doc.path().unwrap().to_string(),
            serde.path().unwrap().to_string()
        );
    }
    // Past the `Value` read's budget it refuses (Y2004); `Document` keeps
    // the opaque identity: no false duplicate, the alias in paths, and two
    // aliases of one anchor still duplicates.
    for (n, uses, s) in [(10_000, 120, Settings::new()), (3_000, 40, untrusted)] {
        let src = alias_keys(n, uses, &format!("- {{? *n : 1, {}: 2}}", inline(n + 1)));
        let serde = from_str_with::<Value>(&src, &s).unwrap_err();
        assert_eq!(serde.code(), code::ALIAS_BUDGET_EXCEEDED, "{n} x {uses}");
        assert!(
            Document::parse_with(src.as_str(), &s).is_ok(),
            "{n} x {uses}"
        );
        let src = alias_keys(n, uses, "- {? *n : {k: !!int x}}");
        let doc = Document::parse_with(src.as_str(), &s).unwrap_err();
        assert_eq!(doc.code(), code::INVALID_SCALAR_FOR_TAG);
        assert_eq!(doc.path().unwrap().to_string(), format!("top[{uses}].*n.k"));
        let src = alias_keys(n, uses, "- {? *n : 1, ? *n : 2}");
        let doc = Document::parse_with(src.as_str(), &s).unwrap_err();
        assert_eq!(doc.code(), code::DUPLICATE_KEY);
    }
}

/// Alias keys of a large scalar are metered by
/// bytes too, against the alias byte budget: past it the `Value` read
/// refuses (`Y2009`) and `Document` keeps the opaque identity, without
/// copying the scalar per use.
#[test]
fn the_copy_budget_meters_bytes() {
    let big = "x".repeat(100_000);
    let mut src = format!("a: &n [{big}]\ntop:\n");
    for _ in 0..700 {
        src.push_str("- {? *n : 1}\n");
    }
    src.push_str(&format!("- {{? *n : 1, [{big}]: 2}}\n"));
    let serde = from_str_with::<Value>(&src, &Settings::new()).unwrap_err();
    assert_eq!(serde.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    assert!(Document::parse(src.as_str()).is_ok());
}

/// Anchors keep only a slot of their node, and only redefined
/// names keep their earlier definitions; an alias inside a replayed node
/// still means the last definition before it (§3.2.2.2), whatever came
/// after, and whether the definition was built, open or a scalar.
#[test]
fn alias_keys_mean_the_definition_before_them() {
    let dup = [
        // Redefined after the replayed node: the earlier definition.
        "i: &i [1]\no: &o [*i]\nj: &i [2]\nz: {? *o : 1, [[1]]: 2}",
        // ... after being built for an alias key of its own.
        "i: &i [1]\ny: {? *i : 1}\no: &o [*i]\nj: &i [2]\nz: {? *o : 1, [[1]]: 2}",
        // A scalar definition, redefined.
        "i: &i a\no: &o [*i]\nj: &i b\nz: {? *o : 1, [a]: 2}",
        // Redefined inside the replayed node, then again after it.
        "o: &o [&i [1], *i]\nj: &i 3\nz: {? *o : 1, [[1], [1]]: 2}",
        // An open definition replaced inside itself: the inner one.
        "o: &i [&i [1]]\nz: {? *i : 1, [1]: 2}",
        // The current definition is open when the alias is
        // replayed (its previous one is meant).
        "a: &i [1]\no: &o [*i ]\nz: &i {? *o : 1, [[1]]: 2}",
        // ... with older ones (the history beyond the previous one).
        "a: &i [0]\nb: &i [1]\no: &o [*i ]\nz: &i {? *o : 1, [[1]]: 2}",
        // Several redefinitions, the alias between two of them.
        "a: &i [1]\nb: &i [2]\no: &o [*i]\nc: &i [3]\nd: &i [4]\nz: {? *o : 1, [[2]]: 2}",
        // A `%TAG` shorthand inside the replayed node.
        "%TAG !e! tag:e.com,2000:\n---\ni: &i !e!t [1]\no: &o [*i]\nj: &i [2]\nz: {? *o : 1, [!<tag:e.com,2000:t> [1]]: 2}",
    ];
    for src in dup {
        let value = from_str_with::<Value>(src, &Settings::new()).unwrap_err();
        assert_eq!(value.code(), code::DUPLICATE_KEY, "{src:?}");
        let doc = Document::parse(src).unwrap_err();
        assert_eq!(
            (doc.code(), doc.span()),
            (value.code(), value.span()),
            "{src:?}"
        );
        // In a stream, after another document defining the same names.
        let first = "--- {i: &i [9], o: &o [*i]}\n";
        let stream = if src.starts_with('%') {
            format!("%TAG !e! tag:e.com,2000:\n{first}...\n{src}")
        } else {
            format!("{first}---\n{src}")
        };
        let e = oneyaml::Stream::parse(stream.as_str()).unwrap_err();
        // The same error, at the same place in the document.
        let offset = stream.len() - src.len();
        let at = value.span().unwrap();
        let at = Span::new(at.start + offset, at.end + offset);
        assert_eq!((e.code(), e.span()), (value.code(), Some(at)), "{stream:?}");
    }
    // Distinct: the later definitions are not what the aliases mean.
    for src in [
        "i: &i [1]\no: &o [*i]\nj: &i [2]\nz: {? *o : 1, [[2]]: 2}",
        "i: &i a\no: &o [*i]\nj: &i b\nz: {? *o : 1, [b]: 2}",
        "a: &i [1]\nb: &i [2]\no: &o [*i]\nc: &i [3]\nz: {? *o : 1, [[3]]: 2}",
    ] {
        assert!(
            from_str_with::<Value>(src, &Settings::new()).is_ok(),
            "{src:?}"
        );
        assert!(Document::parse(src).is_ok(), "{src:?}");
    }
    // An edited document: the replay reads the edited tree.
    let mut doc =
        Document::parse("i: &i [1]\no: &o [*i]\nj: &i [2]\nz: {? *o : 1, [[5]]: 2}\n").unwrap();
    assert!(doc.diagnostics().is_empty());
    doc.set_path(&oneyaml::path!["i"], [5]).unwrap();
    let text = doc.to_string();
    let value = from_str_with::<Value>(&text, &Settings::new()).unwrap_err();
    assert_eq!(value.code(), code::DUPLICATE_KEY, "{text:?}");
    let found: Vec<_> = doc
        .diagnostics()
        .iter()
        .map(|d| (d.code, d.primary.span))
        .collect();
    assert_eq!(found, [(value.code(), value.span().unwrap())], "{text:?}");
}

/// Clones replay their own trees. An edit on a clone that makes
/// an alias key a duplicate is found on the clone only; an edit on the
/// original that repairs it leaves the clone's error.
#[test]
fn alias_keys_of_clones_read_their_own_tree() {
    let src = "i: &i [1]\no: &o [*i]\nj: &i [2]\nz: {? *o : 1, [[5]]: 2}\n";
    let errors = |d: &Document| -> Vec<(oneyaml::Code, Span)> {
        d.diagnostics()
            .iter()
            .map(|d| (d.code, d.primary.span))
            .collect()
    };
    let expect = |d: &Document| -> Vec<(oneyaml::Code, Span)> {
        match from_str_with::<Value>(&d.to_string(), &Settings::new()) {
            Ok(_) => Vec::new(),
            Err(e) => vec![(e.code(), e.span().unwrap())],
        }
    };
    let original = Document::parse(src).unwrap();
    let mut clone = original.clone();
    clone.set_path(&oneyaml::path!["i"], [5]).unwrap();
    assert_eq!(errors(&original), []);
    assert_eq!(errors(&clone), expect(&clone));
    assert_eq!(errors(&clone).len(), 1);
    // A clone of the edited clone, repaired; the edited one keeps its error.
    let mut repaired = clone.clone();
    repaired.set_path(&oneyaml::path!["i"], [6]).unwrap();
    assert_eq!(errors(&repaired), []);
    assert_eq!(errors(&repaired), expect(&repaired));
    assert_eq!(errors(&clone), expect(&clone));
    assert_eq!(errors(&clone).len(), 1);
    // The original, edited the other way: the later definition changes,
    // which the alias inside `o` does not mean.
    let mut other = original.clone();
    other.set_path(&oneyaml::path!["j"], [5]).unwrap();
    assert_eq!(errors(&other), []);
    assert_eq!(errors(&other), expect(&other));
    assert_eq!(errors(&original), []);
}

/// A complex key that merges an alias (`? {<<: *a}`)
/// has the merged entries in its identity, as the `Value` read builds it:
/// the same errors, codes and spans, and the same tolerated duplicates.
#[test]
fn complex_keys_merging_aliases_agree() {
    let settings = [
        Settings::new().merge_keys(true),
        Settings::new()
            .duplicate_keys(DuplicateKeys::LastWins)
            .merge_keys(true),
        oneyaml::Compat::yaml_1_1(),
        Settings::new(),
    ];
    let cases = [
        // The review's cases.
        "a: &a {q: 1}\nz: {? {<<: *a } : 1, {q: 1}: 2}",
        "b: &b []\nz: {? {<<: *b } : 1, {}: 2}",
        // Sequences of sources: earlier ones win.
        "a: &a {q: 1}\nb: &b {r: 2}\nz: {? {<<: [*a, *b]} : 1, {q: 1, r: 2}: 2}",
        "a: &a {q: 1}\nb: &b {q: 2}\nz: {? {<<: [*a, *b]} : 1, {q: 1}: 2}",
        "a: &a {q: 1}\nb: &b {q: 2}\nz: {? {<<: [*a, *b]} : 1, {q: 2}: 2}",
        "a: &a {q: 1}\nz: {? {<<: [*a, {r: 2}]} : 1, {q: 1, r: 2}: 2}",
        // An alias of a sequence of mappings.
        "s: &s [{q: 1}, {q: 2, r: 3}]\nz: {? {<<: *s} : 1, {q: 1, r: 3}: 2}",
        // Explicit entries win.
        "a: &a {q: 1}\nz: {? {<<: *a, q: 5} : 1, {q: 5}: 2}",
        "a: &a {q: 1}\nz: {? {<<: *a, q: 5} : 1, {q: 1}: 2}",
        // Deeper in a key, and both keys merging.
        "a: &a {q: 1}\nz: {? [{<<: *a}] : 1, [{q: 1}]: 2}",
        "a: &a {q: 1}\nz: {? {<<: *a} : 1, ? {<<: *a} : 2}",
        "a: &a {q: 1}\nb: &b {q: 1}\nz: {? {<<: *a} : 1, ? {<<: *b} : 2}",
        // Distinct.
        "a: &a {q: 1}\nz: {? {<<: *a} : 1, {q: 2}: 2}",
        "a: &a {q: 1}\nz: {? {<<: *a} : 1, {}: 2}",
        // The anchor's value merges itself; an alias of one that does.
        "a: &a {<<: {q: 1}}\nz: {? {<<: *a} : 1, {q: 1}: 2}",
        "a: &a {q: 1}\nm: &m {<<: *a, r: 2}\nz: {? {<<: *m} : 1, {q: 1, r: 2}: 2}",
        // A nested complex key in the merged value.
        "a: &a {[1]: x}\nz: {? {<<: *a} : 1, {[1]: x}: 2}",
        // Invalid merge values (and items).
        "a: &a !t {q: 1}\nz: {? {<<: *a} : 1}",
        "a: &a [1]\nz: {? {<<: *a} : 1}",
        "s: &s [{q: 1}]\nz: {? {<<: [*s]} : 1}",
        // A later error after the key: the duplicate comes first.
        "a: &a {q: 1}\nz: {? {<<: *a} : 1, {q: 1}: 2, w: !!int x}",
    ];
    for s in &settings {
        for src in cases {
            let spans = |ds: &[oneyaml::Diagnostic]| -> Vec<(oneyaml::Code, Span)> {
                ds.iter().map(|d| (d.code, d.primary.span)).collect()
            };
            match from_str_with_diagnostics::<Value>(src, s) {
                Ok((_, warnings)) => {
                    let doc = Document::parse_with(src, s).expect(src);
                    assert_eq!(spans(&doc.diagnostics()), spans(&warnings), "{src:?}");
                }
                Err(serde) => {
                    let doc = Document::parse_with(src, s).unwrap_err();
                    assert_eq!(
                        (doc.code(), doc.span()),
                        (serde.code(), serde.span()),
                        "{src:?}"
                    );
                }
            }
        }
    }
}

/// An alias key whose anchor's scalar bytes
/// exceed what is left of the byte budget is refused before any of it is
/// copied. At and around the boundary the paths still agree: while the
/// `Value` read is within its byte budget, `Document` reports the same
/// code at the same span; past it (`Y2009`), `Document` reports no false
/// duplicate and nothing before the `Value` read's error.
#[test]
fn byte_heavy_alias_keys_agree_at_the_byte_budget() {
    let shapes = [
        // A collection of scalars, used as a key twice, then written out.
        "a: &n [abc, defgh]\ntop:\n- {? *n : 1}\n- {? *n : 1, [abc, defgh]: 2}\n",
        // Nested: an anchor whose bytes come through aliases inside it.
        "a: &n [abcd]\nb: &m [*n, *n, x]\ntop:\n- {? *m : 1}\n\
         - {? *m : 1, [[abcd], [abcd], x]: 2}\n",
        // Two aliases of one anchor in one mapping: duplicates whatever
        // the budget.
        "a: &n [abcdef]\ntop:\n- {? *n : 1, ? *n : 2}\n",
        // A byte-heavy anchor, then a small one that still fits.
        "a: &n [abcdefghij]\nb: &m [z]\ntop:\n- {? *n : 1}\n- {? *m : 1, [z]: 2}\n",
    ];
    let mut seen = [false; 2];
    for src in shapes {
        for budget in 0..=60 {
            let s = Settings::new().limits(oneyaml::Limits::new().alias_byte_budget(budget));
            let serde = from_str_with::<Value>(src, &s).map_err(|e| (e.code(), e.span()));
            let doc = Document::parse_with(src, &s).map_err(|e| (e.code(), e.span()));
            match serde {
                Err((c, at)) if c == code::ALIAS_BYTE_BUDGET_EXCEEDED => {
                    seen[0] = true;
                    let at = at.unwrap().start;
                    if let Err((c, d)) = doc {
                        assert_eq!(c, code::DUPLICATE_KEY, "{budget}: {src:?}");
                        assert!(d.unwrap().start >= at, "{budget}: {src:?}");
                    }
                }
                other => {
                    seen[1] = true;
                    assert_eq!(doc.map(|_| ()), other.map(|_| ()), "{budget}: {src:?}");
                }
            }
        }
    }
    // Both sides of the boundary were exercised.
    assert_eq!(seen, [true, true]);
}

/// An alias key of an anchor with more scalar
/// bytes than the whole byte budget (100 MB in 202 nodes) keeps its
/// opaque identity where the `Value` read refuses (`Y2009`). This checks
/// the meaning only; that nothing is copied for it is gated by
/// `distinct_over_budget_alias_keys_cost_one_budget` (oracles,
/// `alias_key_memory.rs`), which measures allocations.
#[test]
fn an_over_budget_byte_heavy_alias_key_keeps_its_opaque_identity() {
    let big = "x".repeat(500_000);
    let items = vec!["*a0"; 200].join(", ");
    let src = |m: &str| format!("a0: &a0 {big}\na1: &a1 [{items}]\nm: {m}\n");
    let one = src("{? *a1 : 1}");
    let serde = from_str_with::<Value>(&one, &Settings::new()).unwrap_err();
    assert_eq!(serde.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    assert!(Document::parse(one.as_str()).is_ok());
    // Two aliases of it: still duplicates.
    let two = src("{? *a1 : 1, ? *a1 : 2}");
    let e = Document::parse(two.as_str()).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
}
