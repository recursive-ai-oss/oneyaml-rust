//! The serializer's property gate and robustness:
//!
//! * `deserialize(serialize(v)) == v` for generated `Value`s (every scalar
//!   kind, the awkward strings, strings and tags generated over a hazard
//!   alphabet, any-`Value` keys, deep nesting) and typed structs, under the
//!   default settings, `Compat::serde_yaml()`, and other output settings;
//! * one interpretation: `Value::try_from(&Document::parse(out))` gives the
//!   same value;
//! * no recursion, no crash: a 1,000,000-deep `Value` serializes on a small
//!   stack; `max_depth` (`Y2005`) and `max_typed_depth` (`Y2007`) are errors,
//!   never crashes;
//! * never a panic: failing writers are errors.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod ser_common;

use std::io::{self, Write};

use oneyaml::{Compat, Document, ErrorKind, Limits, Sequence, Settings, Value, code};
use ser_common::{Service, Shape, property_settings, property_values, service, tree};
use serde::Serialize;

#[test]
fn values_read_back_equal_under_every_setting() {
    let values = property_values(600);
    for (name, settings) in property_settings() {
        for (i, v) in values.iter().enumerate() {
            let out = oneyaml::to_string_with(v, &settings)
                .unwrap_or_else(|e| panic!("{name} #{i}: {e}\n{v:?}"));
            let back: Value = oneyaml::from_str_with(&out, &settings)
                .unwrap_or_else(|e| panic!("{name} #{i}: {e}\n{out}"));
            assert_eq!(back, *v, "{name} #{i}:\n{out}");
            // A second pass is byte-identical (determinism).
            assert_eq!(
                oneyaml::to_string_with(&back, &settings).unwrap(),
                out,
                "{name} #{i}"
            );
        }
    }
}

/// The property gate over **generated** strings (seeded, over the
/// hazard alphabet, `AWKWARD` kept as seeds): each string as the root, a
/// sequence item, a mapping key and value, and under a random tag, under
/// every property setting; typed `String`s too.
#[test]
fn generated_strings_read_back_equal_under_every_setting() {
    use ser_common::{Rng, gen_block_shape, gen_string, gen_tag};
    let mut r = Rng::new(0x20b_5eed);
    let mut strings: Vec<String> = ser_common::AWKWARD
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    strings.extend((0..1500).map(|_| gen_string(&mut r)));
    strings.extend((0..300).map(|_| gen_block_shape(&mut r)));
    // The generator must produce the block shapes the gate is about:
    // whitespace-only lines before the content and several trailing
    // line breaks, in one string.
    let hard = strings
        .iter()
        .filter(|s| {
            let lead_blank = s
                .split('\n')
                .next()
                .is_some_and(|l| l.chars().all(|c| c == ' ' || c == '\t'))
                && s.contains('\n');
            lead_blank && s.ends_with("\n\n") && !s.trim().is_empty()
        })
        .count();
    assert!(hard >= 20, "only {hard} block shapes with lead and trail");
    for (name, settings) in property_settings() {
        for (i, s) in strings.iter().enumerate() {
            let mut m = oneyaml::Mapping::new();
            m.insert(Value::from(s.as_str()), Value::from(s.as_str()));
            let tag = gen_tag(&mut r);
            for v in [
                Value::from(s.as_str()),
                Value::from(vec![Value::from(s.as_str()), Value::from(1)]),
                Value::from(m),
                Value::from(oneyaml::TaggedValue {
                    tag: oneyaml::Tag::new(tag.as_str()),
                    value: Value::from(s.as_str()),
                }),
            ] {
                let out = oneyaml::to_string_with(&v, &settings)
                    .unwrap_or_else(|e| panic!("{name} #{i} {s:?} tag {tag:?}: {e}"));
                let back: Value = oneyaml::from_str_with(&out, &settings)
                    .unwrap_or_else(|e| panic!("{name} #{i} {s:?}: {e}\n{out}"));
                assert_eq!(back, v, "{name} #{i} {s:?}:\n{out}");
                let doc = Document::parse_with(out.as_str(), &settings)
                    .unwrap_or_else(|e| panic!("{name} #{i} {s:?}: {e}\n{out}"));
                assert_eq!(doc.to_string().as_bytes(), out.as_bytes(), "{name} #{i}");
            }
            typed_round_trip(s, &settings);
        }
    }
}

#[test]
fn document_reads_the_same_value() {
    for (name, settings) in property_settings() {
        for (i, v) in property_values(300).iter().enumerate() {
            let out = oneyaml::to_string_with(v, &settings).unwrap();
            let doc = Document::parse_with(out.as_str(), &settings)
                .unwrap_or_else(|e| panic!("{name} #{i}: {e}\n{out}"));
            assert_eq!(doc.to_string().as_bytes(), out.as_bytes(), "{name} #{i}");
            let from_doc = Value::try_from(&doc).unwrap();
            assert_eq!(from_doc, *v, "{name} #{i}:\n{out}");
        }
    }
}

fn typed_round_trip<T>(v: &T, settings: &Settings)
where
    T: Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let out = oneyaml::to_string_with(v, settings).unwrap();
    let back: T = oneyaml::from_str_with(&out, settings).unwrap_or_else(|e| panic!("{e}\n{out}"));
    assert_eq!(back, *v, "\n{out}");
}

#[test]
fn typed_values_read_back_equal() {
    use std::collections::{BTreeMap, HashMap};
    for (_, settings) in property_settings() {
        typed_round_trip(&service(), &settings);
        for s in ser_common::AWKWARD {
            typed_round_trip(&(*s).to_owned(), &settings);
            typed_round_trip(&Shape::Text((*s).to_owned()), &settings);
            typed_round_trip(&vec![Some((*s).to_owned()), None], &settings);
            typed_round_trip(
                &BTreeMap::from([((*s).to_owned(), (*s).to_owned())]),
                &settings,
            );
        }
        for f in [
            0.0,
            -0.0,
            1.5,
            1e16,
            1e-300,
            f64::MAX,
            f64::MIN_POSITIVE,
            5e-324,
        ] {
            typed_round_trip(&f, &settings);
        }
        for f in [0.1f32, 1e-7, 3.4e38, 16_777_217.0, f32::MIN_POSITIVE] {
            typed_round_trip(&f, &settings);
        }
        for c in ['a', '#', '\'', '\n', '\u{0}', '😀', ' ', 'y'] {
            typed_round_trip(&c, &settings);
        }
        typed_round_trip(&(i128::MIN, u128::MAX, i64::MIN, u64::MAX), &settings);
        typed_round_trip(
            &HashMap::from([(1u8, "a".to_owned()), (2, "b".to_owned())]),
            &settings,
        );
        typed_round_trip(
            &BTreeMap::from([(vec![1, 2], Shape::Unit), (vec![], Shape::Count(-1))]),
            &settings,
        );
        typed_round_trip(&tree(20), &settings);
        typed_round_trip(&Some(Some(1)), &settings);
        typed_round_trip(&vec![(); 3], &settings);
    }
}

#[test]
fn exact_scalars() {
    assert_eq!(oneyaml::to_string(&Value::Null).unwrap(), "null\n");
    assert_eq!(oneyaml::to_string(&Value::from(true)).unwrap(), "true\n");
    assert_eq!(oneyaml::to_string(&Value::from(-7)).unwrap(), "-7\n");
    assert_eq!(oneyaml::to_string(&Value::from(2.0)).unwrap(), "2.0\n");
    assert_eq!(oneyaml::to_string(&Value::from("x")).unwrap(), "x\n");
    assert_eq!(oneyaml::to_string(&Value::from("")).unwrap(), "''\n");
    assert_eq!(oneyaml::to_string(&Sequence::new()).unwrap(), "[]\n");
    assert_eq!(
        oneyaml::to_string(&oneyaml::Mapping::new()).unwrap(),
        "{}\n"
    );
    assert_eq!(oneyaml::to_string(&()).unwrap(), "null\n");
    assert_eq!(oneyaml::to_string("no").unwrap(), "'no'\n");
    let mut out = Vec::new();
    oneyaml::to_writer(&mut out, &[1, 2]).unwrap();
    assert_eq!(out, b"- 1\n- 2\n");
}

const DEPTH: usize = 1_000_000;
const STACK: usize = 128 * 1024;

fn on_stack(stack: usize, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(f)
        .unwrap()
        .join()
        .expect("no stack overflow / panic");
}

fn deep_seq(depth: usize) -> Value {
    let mut v = Value::from("leaf");
    for _ in 0..depth {
        v = Value::Sequence(Sequence::from(vec![v]));
    }
    v
}

#[test]
fn million_deep_value_on_a_small_stack() {
    on_stack(STACK, || {
        let v = deep_seq(DEPTH);
        let unlimited = Settings::new().limits(Limits::unlimited());
        let out = oneyaml::to_string_with(&v, &unlimited).unwrap();
        assert_eq!(out.len(), DEPTH * 2 + "leaf\n".len());
        assert!(out.starts_with("- - - ") && out.ends_with("- leaf\n"));
        // Through a `Serializer` stream and inside a struct as well.
        #[derive(Serialize)]
        struct Holder<'a> {
            v: &'a Value,
        }
        let mut sink = Vec::new();
        let mut ser = oneyaml::Serializer::with_settings(&mut sink, &unlimited);
        Holder { v: &v }.serialize(&mut ser).unwrap();
        assert_eq!(ser.documents(), 1);
        // The default max_depth refuses it with Y2005.
        let e = oneyaml::to_string(&v).unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
        assert_eq!(e.kind(), ErrorKind::Limit);
        assert!(
            e.diagnostic()
                .help
                .as_deref()
                .unwrap()
                .contains("max_depth")
        );
        // Through a third-party serializer: Y2007 (max_typed_depth).
        let e = serde_json_error(&v);
        assert!(e.contains("Y2007"), "{e}");
        drop(v);
    });
}

fn serde_json_error(v: &Value) -> String {
    serde_json::to_string(v).unwrap_err().to_string()
}

#[test]
fn deep_mapping_and_tag_chains_are_safe() {
    on_stack(STACK, || {
        // Mapping nesting writes O(depth^2) indentation, so a moderate depth.
        let mut v = Value::from(1);
        for _ in 0..2_000 {
            let mut m = oneyaml::Mapping::new();
            m.insert(v, Value::Null);
            v = Value::Mapping(m);
        }
        let s = Settings::new().max_depth(5_000);
        let out = oneyaml::to_string_with(&v, &s).unwrap();
        let back: Value = oneyaml::from_str_with(&out, &s).unwrap();
        assert_eq!(back, v);
        // A chain of a million tags is refused at its first link.
        let mut t = Value::from("x");
        for _ in 0..DEPTH {
            t = Value::from(oneyaml::TaggedValue {
                tag: oneyaml::Tag::new("!t"),
                value: t,
            });
        }
        let e =
            oneyaml::to_string_with(&t, &Settings::new().limits(Limits::unlimited())).unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE);
    });
}

#[test]
fn recursive_user_types_stop_at_max_typed_depth() {
    // A thread with room for the default 128 typed levels in debug builds.
    on_stack(16 << 20, || {
        let t = tree(5_000);
        let e = oneyaml::to_string(&t).unwrap_err();
        assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
        assert!(
            e.diagnostic()
                .help
                .as_deref()
                .unwrap()
                .contains("max_typed_depth")
        );
        let e =
            oneyaml::to_string_with(&t, &Settings::new().limits(Limits::untrusted())).unwrap_err();
        assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
        // Within the limit it works (each Tree level is a newtype and a
        // sequence: two typed levels).
        assert!(oneyaml::to_string(&tree(60)).is_ok());
        std::mem::forget(t); // Tree's derived drop recurses per level.
    });
}

struct Failing {
    fail_flush: bool,
}

impl Write for Failing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_flush {
            Ok(buf.len())
        } else {
            Err(io::Error::other("disk full"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush failed"))
    }
}

#[test]
fn failing_writers_are_errors() {
    let e = oneyaml::to_writer(Failing { fail_flush: false }, &1).unwrap_err();
    assert_eq!(e.code(), code::WRITE_ERROR);
    assert_eq!(e.kind(), ErrorKind::Io);
    assert!(std::error::Error::source(&e).is_some());
    let e = oneyaml::to_writer(Failing { fail_flush: true }, &1).unwrap_err();
    assert_eq!(e.code(), code::WRITE_ERROR);
    let mut ser = oneyaml::Serializer::new(Failing { fail_flush: false });
    assert_eq!(1.serialize(&mut ser).unwrap_err().code(), code::WRITE_ERROR);
    assert_eq!(ser.documents(), 0);
    let ser = oneyaml::Serializer::new(Failing { fail_flush: true });
    assert_eq!(ser.into_inner().err().unwrap().code(), code::WRITE_ERROR);
}

#[test]
fn a_failed_document_leaves_the_stream_valid() {
    let mut out = Vec::new();
    let mut ser = oneyaml::Serializer::new(&mut out);
    1.serialize(&mut ser).unwrap();
    let bad = oneyaml::TaggedValue {
        tag: oneyaml::Tag::new("!n"),
        value: Value::from(1),
    };
    assert!(Value::from(bad).serialize(&mut ser).is_err());
    vec![Service::clone(&service()).name]
        .serialize(&mut ser)
        .unwrap();
    assert_eq!(ser.documents(), 2);
    ser.into_inner().unwrap();
    let docs: Vec<Value> = oneyaml::Deserializer::from_str(std::str::from_utf8(&out).unwrap())
        .map(|d| Value::deserialize(d).unwrap())
        .collect();
    assert_eq!(
        docs,
        vec![Value::from(1), Value::from(vec![Value::from("web")])]
    );
    // Settings errors surface at the first document.
    let mut ser = oneyaml::Serializer::with_settings(Vec::new(), &Settings::new().indent(0));
    assert_eq!(
        1.serialize(&mut ser).unwrap_err().code(),
        code::INCORRECT_SETTINGS
    );
}

#[test]
fn every_serde_entry_writes_a_document() {
    #[derive(Serialize)]
    struct Unit;
    #[derive(Serialize)]
    struct New(u8);
    #[derive(Serialize)]
    struct TupleS(u8, u8);
    let mut out = Vec::new();
    let mut ser = oneyaml::Serializer::with_settings(&mut out, &Compat::serde_yaml());
    true.serialize(&mut ser).unwrap();
    'c'.serialize(&mut ser).unwrap();
    serde::Serializer::serialize_bytes(&mut ser, b"\x01").unwrap();
    None::<u8>.serialize(&mut ser).unwrap();
    Some(1u8).serialize(&mut ser).unwrap();
    ().serialize(&mut ser).unwrap();
    Unit.serialize(&mut ser).unwrap();
    New(1).serialize(&mut ser).unwrap();
    TupleS(1, 2).serialize(&mut ser).unwrap();
    (1, 2).serialize(&mut ser).unwrap();
    Shape::Unit.serialize(&mut ser).unwrap();
    Shape::Count(1).serialize(&mut ser).unwrap();
    Shape::Pair(1, "a".to_owned()).serialize(&mut ser).unwrap();
    Shape::Point { x: 1.0, y: 2.0 }.serialize(&mut ser).unwrap();
    std::collections::BTreeMap::from([(1, 2)])
        .serialize(&mut ser)
        .unwrap();
    (
        1.5f32, 2.5f64, -1i8, 1u16, -1i16, 1u32, -1i32, 1u64, -1i64, 1u128, -1i128,
    )
        .serialize(&mut ser)
        .unwrap();
    for n in [1u8, 2] {
        n.serialize(&mut ser).unwrap();
    }
    1.5f32.serialize(&mut ser).unwrap();
    1u128.serialize(&mut ser).unwrap();
    (-1i128).serialize(&mut ser).unwrap();
    1.5f64.serialize(&mut ser).unwrap();
    "s".serialize(&mut ser).unwrap();
    let n = ser.documents();
    ser.into_inner().unwrap();
    let text = String::from_utf8(out).unwrap();
    let docs: Vec<Value> = oneyaml::Deserializer::from_str(&text)
        .map(|d| Value::deserialize(d).unwrap())
        .collect();
    assert_eq!(docs.len(), n);
}

use serde::Deserialize as _;

/// The round trip: a `Number` serialized on
/// its own, as a struct field, or as a stream document is written exactly
/// as `Value::Number` is, under every property setting, and reads back as
/// the same `Number`; third-party serializers get plain serde data (the
/// text of an integer wider than 128 bits).
#[test]
fn numbers_serialize_as_value_numbers() {
    use oneyaml::Number;
    use ser_common::{WithNumber, wide_numbers, with_number};
    let mut numbers = wide_numbers();
    numbers.extend([
        Number::from(0),
        Number::from(-1),
        Number::from(u64::MAX),
        Number::from(i128::MIN),
        Number::from(u128::MAX),
        Number::from(1.5),
        Number::from(f64::NAN),
        Number::from(0.1f32),
    ]);
    for (name, settings) in property_settings() {
        for n in &numbers {
            let bare = oneyaml::to_string_with(n, &settings).unwrap();
            let as_value = oneyaml::to_string_with(&Value::Number(n.clone()), &settings).unwrap();
            assert_eq!(bare, as_value, "{name} {n}");
            let back: Number = oneyaml::from_str_with(&bare, &settings)
                .unwrap_or_else(|e| panic!("{name} {n}: {e}\n{bare}"));
            assert_eq!(back, *n, "{name}:\n{bare}");
            let back: Value = oneyaml::from_str_with(&bare, &settings).unwrap();
            assert_eq!(back, Value::Number(n.clone()), "{name}:\n{bare}");
            // As a field of a user type and inside a `Vec`.
            #[derive(Serialize)]
            struct Field<'a> {
                n: &'a Number,
            }
            let field = oneyaml::to_string_with(&Field { n }, &settings).unwrap();
            let mut m = oneyaml::Mapping::new();
            m.insert(Value::from("n"), Value::Number(n.clone()));
            assert_eq!(
                field,
                oneyaml::to_string_with(&Value::Mapping(m), &settings).unwrap(),
                "{name} {n}"
            );
            typed_round_trip(&vec![n.clone()], &settings);
            // Through the document `Serializer`.
            let mut out = Vec::new();
            let mut ser = oneyaml::Serializer::with_settings(&mut out, &settings);
            n.serialize(&mut ser).unwrap();
            ser.into_inner().unwrap();
            assert_eq!(out, bare.as_bytes(), "{name} {n}");
        }
        typed_round_trip::<WithNumber>(&with_number(), &settings);
    }
    // Plain serde data for everyone else: wide integers as their text,
    // narrow ones as numbers.
    let wide = &wide_numbers()[2];
    assert_eq!(serde_json::to_string(wide).unwrap(), format!("\"{wide}\""));
    assert_eq!(serde_json::to_string(&Number::from(-7)).unwrap(), "-7");
    assert_eq!(
        serde_json::to_string(&Value::Number(wide.clone())).unwrap(),
        format!("\"{wide}\"")
    );
}

/// The public serializer types are `Debug`, without
/// requiring `W: Debug`.
#[test]
fn serializer_types_are_debug() {
    struct Opaque(Vec<u8>);
    impl Write for Opaque {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut ser = oneyaml::Serializer::new(Opaque(Vec::new()));
    1.serialize(&mut ser).unwrap();
    let shown = format!("{ser:?}");
    assert!(
        shown.starts_with("Serializer {") && shown.contains("documents: 1"),
        "{shown}"
    );
    let seq = serde::Serializer::serialize_seq(&mut ser, None).unwrap();
    let shown = format!("{seq:?}");
    assert!(
        shown.starts_with("DocumentCompound {") && shown.contains("documents: 1"),
        "{shown}"
    );
    serde::ser::SerializeSeq::end(seq).unwrap();
    assert_eq!(ser.into_inner().unwrap().0, b"1\n---\n[]\n");
}
