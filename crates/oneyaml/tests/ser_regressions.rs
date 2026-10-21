//! Serializer regression tests, one per contract:
//!
//! * never a panic: every `Emit::indent` value is accepted by the type;
//!   values outside `1..=Emit::MAX_INDENT` are `Y1012`, never a panic or
//!   an allocation blow-up;
//! * §6.8.2.1 Example 6.25: tags with no spec-valid form are `Y3015`;
//! * every reader reads a plain string as a string: Psych and Go number
//!   readings are quoted, and so are Psych's null/booleans in any case and
//!   `,` in binary/hex (also in `AWKWARD`);
//! * third-party serializers see plain serde data;
//! * `Value::to_yaml_string` emits a `&Value` without cloning;
//! * one interpretation: a spoofed private newtype name gets no hand-over;
//! * `Number` `==`/`Hash` are meaning-only; no `0o` output;
//! * scalar keys of any type are implicit;
//! * hash-flooding resistance: wide-integer hashes are keyed per process;
//!   keys crafted to collide modulo a fixed modulus parse in linear time;
//! * cross-radix equality converts only up to 4,096 digits;
//! * never a panic: serializing (and deserializing) a `Value` from a
//!   thread-local destructor never panics; oneyaml's own
//!   `Serializer`/`Deserializer` refuse it with `Y3017`;
//! * a `Serialize` impl that calls oneyaml re-entrantly keeps its caller's
//!   mark: the delegated `Value` stays tagged;
//! * wide non-negative octal/hex integers are written as `0x` (linear);
//!   negative (or base-60) ones are converted to decimal only when both
//!   forms have at most 4,096 digits, otherwise `Y3015`; a wide
//!   sexagesimal integer of either sign past the bound is `Y3015`, and the
//!   message says so;
//! * `#[serde(flatten)]` of a `Mapping` keeps its tagged values tagged;
//! * a handed-over scalar (`Number`, scalar `Value`) is a leaf and costs no
//!   `max_typed_depth` level, like a `u64` leaf; a handed-over collection
//!   still costs one.
//!
//! Covered elsewhere: the hazard-alphabet property generator
//! (`tests/ser_props.rs`,
//! `generated_strings_read_back_equal_under_every_setting`, and the `AWKWARD`
//! seeds), in-place key and number writing (the emitter's
//! `emit::scalar::tests::implicit_keys` and every golden and property test that
//! writes keys and numbers), other `Number` cases (its unit tests), more
//! property and corpus cases (`tests/ser_props.rs`, `tests/ser_common/mod.rs`)
//! and allocation counts (`crates/oracles/tests/ser_allocations.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod ser_common;

use std::fmt::Write as _;

use oneyaml::{Emit, ErrorKind, Mapping, Sequence, Settings, Tag, TaggedValue, Value, code};
use serde::Serialize;
use serde::ser::{self, SerializeMap, SerializeSeq};

// ------------------------------ any indent is an error or output never a panic

fn root_block() -> Value {
    Value::from("line 1\nline 2\n")
}

fn nested_seq() -> Value {
    Value::from(vec![Value::from(vec![Value::from(vec![Value::from(
        " a\nb",
    )])])])
}

fn nested_map() -> Value {
    let mut inner = Mapping::new();
    inner.insert(Value::from("k"), Value::from(vec![Value::from("v")]));
    let mut outer = Mapping::new();
    outer.insert(Value::from("a"), Value::from(inner));
    Value::from(outer)
}

#[test]
fn any_indent_is_an_error_or_output_never_a_panic() {
    let refused = [
        0,
        Emit::MAX_INDENT + 1,
        1 << 20,
        1 << 40,
        usize::MAX - 1,
        usize::MAX,
    ];
    for indent in refused {
        for v in [root_block(), nested_seq(), nested_map()] {
            let settings = Settings::new().indent(indent);
            let e = oneyaml::to_string_with(&v, &settings).unwrap_err();
            assert_eq!(e.code(), code::INCORRECT_SETTINGS, "indent {indent}");
            assert_eq!(e.kind(), ErrorKind::Schema, "indent {indent}");
            let e = v.to_yaml_string_with(&settings).unwrap_err();
            assert_eq!(e.code(), code::INCORRECT_SETTINGS, "indent {indent}");
            // The stream serializer refuses it too, and writes nothing.
            let mut out = Vec::new();
            let mut ser = oneyaml::Serializer::with_settings(&mut out, &settings);
            assert_eq!(
                v.serialize(&mut ser).unwrap_err().code(),
                code::INCORRECT_SETTINGS
            );
            assert_eq!(ser.documents(), 0);
            assert!(out.is_empty());
        }
    }
    // The message names the accepted range (in its help).
    let e = oneyaml::to_string_with(&1, &Settings::new().indent(usize::MAX)).unwrap_err();
    assert!(e.to_string().contains("1..=64"), "{e}");
    // 1 and the largest accepted value produce output that reads back.
    for indent in [1, 2, 9, 10, Emit::MAX_INDENT] {
        let settings = Settings::new().indent(indent);
        for v in [root_block(), nested_seq(), nested_map()] {
            let out = oneyaml::to_string_with(&v, &settings).unwrap();
            let back: Value = oneyaml::from_str(&out).unwrap();
            assert_eq!(back, v, "indent {indent}:\n{out}");
        }
    }
    let out =
        oneyaml::to_string_with(&nested_map(), &Settings::new().indent(Emit::MAX_INDENT)).unwrap();
    let pad = " ".repeat(Emit::MAX_INDENT);
    assert_eq!(out, format!("a:\n{pad}k:\n{pad}{pad}- v\n"));
}

// ------------------------------------------------- spec invalid tags are y3015

fn tagged(tag: &str, v: Value) -> Value {
    Value::from(TaggedValue {
        tag: Tag::new(tag),
        value: v,
    })
}

#[test]
fn spec_invalid_tags_are_y3015() {
    for tag in ["!", "$:?", "x", "a b", ":x", "1a:b"] {
        let e = oneyaml::to_string(&tagged(tag, Value::from("x"))).unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE, "{tag:?}");
        let e = oneyaml::to_string(&tagged(tag, Value::from(vec![Value::from(1)]))).unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE, "{tag:?}");
        // As a key: explicit form, then the same error.
        let mut m = Mapping::new();
        m.insert(tagged(tag, Value::from("k")), Value::from("v"));
        let e = oneyaml::to_string(&m).unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE, "{tag:?}");
    }
    // URIs with a scheme and local tags are written.
    assert_eq!(
        oneyaml::to_string(&tagged("urn:a", Value::from("x"))).unwrap(),
        "!<urn:a> x\n"
    );
    assert_eq!(
        oneyaml::to_string(&tagged("!!", Value::from("x"))).unwrap(),
        "!%21 x\n"
    );
}

// --------------------------------------------- psych and go numbers are quoted

#[test]
fn psych_and_go_numbers_are_quoted() {
    let psych_go = [
        "1,000", "1,2", "+1,000", "0,7", "1,000.5", "09:15", "00:30", "0:30", ".iNf", "+.INf",
        ".nAn", "+_1", "1e_5", "0_x1F", "1_e_5",
        // Psych's `/i` words and `,` in binary/hex.
        "tRUE", "nULL", "yEs", "oFF", "FaLsE", "NuLl", "0b1,0", "0x1,F", "0x,1F", "-0b1,1",
    ];
    for s in psych_go {
        let out = oneyaml::to_string(s).unwrap();
        assert_eq!(out, format!("'{s}'\n"), "{s}");
        let mut m = Mapping::new();
        m.insert(Value::from(s), Value::from(1));
        assert_eq!(
            oneyaml::to_string(&m).unwrap(),
            format!("'{s}': 1\n"),
            "{s}"
        );
        let back: String = oneyaml::from_str(&out).unwrap();
        assert_eq!(back, s);
    }
    for s in ["a,b", "x_1", "1:a", "v1.2"] {
        assert_eq!(oneyaml::to_string(s).unwrap(), format!("{s}\n"), "{s}");
    }
}

// -------------------------------------- third party serializers see plain data

/// A serializer that is **not** transparent to newtypes and records every
/// call's name, like RON (`Name(…)`): any private wrapper would show.
struct Recorder<'a>(&'a mut String);

#[derive(Debug)]
struct RecErr(String);

impl std::fmt::Display for RecErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RecErr {}
impl ser::Error for RecErr {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

struct RecCompound<'a> {
    out: &'a mut String,
    first: bool,
    close: &'static str,
}

impl RecCompound<'_> {
    fn sep(&mut self) {
        if !self.first {
            self.out.push(',');
        }
        self.first = false;
    }
}

macro_rules! rec_scalar {
    ($($m:ident($t:ty)),*) => {$(
        fn $m(self, v: $t) -> Result<(), RecErr> {
            let _ = write!(self.0, "{v:?}");
            Ok(())
        }
    )*};
}

impl<'a> ser::Serializer for Recorder<'a> {
    type Ok = ();
    type Error = RecErr;
    type SerializeSeq = RecCompound<'a>;
    type SerializeTuple = RecCompound<'a>;
    type SerializeTupleStruct = RecCompound<'a>;
    type SerializeTupleVariant = RecCompound<'a>;
    type SerializeMap = RecCompound<'a>;
    type SerializeStruct = RecCompound<'a>;
    type SerializeStructVariant = RecCompound<'a>;
    rec_scalar!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8])
    );
    fn serialize_none(self) -> Result<(), RecErr> {
        self.0.push_str("None");
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), RecErr> {
        self.0.push_str("Some(");
        v.serialize(Recorder(&mut *self.0))?;
        self.0.push(')');
        Ok(())
    }
    fn serialize_unit(self) -> Result<(), RecErr> {
        self.0.push_str("()");
        Ok(())
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), RecErr> {
        self.0.push_str(name);
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        v: &'static str,
    ) -> Result<(), RecErr> {
        self.0.push_str(v);
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        v: &T,
    ) -> Result<(), RecErr> {
        let _ = write!(self.0, "{name}(");
        v.serialize(Recorder(&mut *self.0))?;
        self.0.push(')');
        Ok(())
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<(), RecErr> {
        let _ = write!(self.0, "{variant}(");
        v.serialize(Recorder(&mut *self.0))?;
        self.0.push(')');
        Ok(())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<RecCompound<'a>, RecErr> {
        self.0.push('[');
        Ok(RecCompound {
            out: self.0,
            first: true,
            close: "]",
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<RecCompound<'a>, RecErr> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<RecCompound<'a>, RecErr> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        len: usize,
    ) -> Result<RecCompound<'a>, RecErr> {
        self.serialize_seq(Some(len))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<RecCompound<'a>, RecErr> {
        self.0.push('{');
        Ok(RecCompound {
            out: self.0,
            first: true,
            close: "}",
        })
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<RecCompound<'a>, RecErr> {
        self.serialize_map(Some(len))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        len: usize,
    ) -> Result<RecCompound<'a>, RecErr> {
        self.serialize_map(Some(len))
    }
}

impl ser::SerializeSeq for RecCompound<'_> {
    type Ok = ();
    type Error = RecErr;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), RecErr> {
        self.sep();
        v.serialize(Recorder(&mut *self.out))
    }
    fn end(self) -> Result<(), RecErr> {
        self.out.push_str(self.close);
        Ok(())
    }
}

macro_rules! rec_seq_like {
    ($($tr:ident::$m:ident),*) => {$(
        impl ser::$tr for RecCompound<'_> {
            type Ok = ();
            type Error = RecErr;
            fn $m<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), RecErr> {
                SerializeSeq::serialize_element(self, v)
            }
            fn end(self) -> Result<(), RecErr> {
                SerializeSeq::end(self)
            }
        }
    )*};
}
rec_seq_like!(
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field
);

impl ser::SerializeMap for RecCompound<'_> {
    type Ok = ();
    type Error = RecErr;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, k: &T) -> Result<(), RecErr> {
        self.sep();
        k.serialize(Recorder(&mut *self.out))?;
        self.out.push(':');
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), RecErr> {
        v.serialize(Recorder(&mut *self.out))
    }
    fn end(self) -> Result<(), RecErr> {
        self.out.push_str(self.close);
        Ok(())
    }
}

macro_rules! rec_struct_like {
    ($($tr:ident),*) => {$(
        impl ser::$tr for RecCompound<'_> {
            type Ok = ();
            type Error = RecErr;
            fn serialize_field<T: ?Sized + Serialize>(
                &mut self,
                name: &'static str,
                v: &T,
            ) -> Result<(), RecErr> {
                SerializeMap::serialize_key(self, name)?;
                SerializeMap::serialize_value(self, v)
            }
            fn end(self) -> Result<(), RecErr> {
                SerializeMap::end(self)
            }
        }
    )*};
}
rec_struct_like!(SerializeStruct, SerializeStructVariant);

fn record<T: ?Sized + Serialize>(v: &T) -> String {
    let mut out = String::new();
    v.serialize(Recorder(&mut out)).unwrap();
    out
}

#[test]
fn third_party_serializers_see_plain_data() {
    assert_eq!(record(&Value::from(1)), "1");
    assert_eq!(record(&Value::from("x")), "\"x\"");
    assert_eq!(record(&Value::Null), "()");
    assert_eq!(
        record(&Value::from(vec![Value::from(1), Value::from(true)])),
        "[1,true]"
    );
    let mut m = Mapping::new();
    m.insert(Value::from("a"), Value::from(vec![Value::from(2)]));
    assert_eq!(record(&Value::from(m.clone())), "{\"a\":[2]}");
    assert_eq!(record(&m), "{\"a\":[2]}");
    assert_eq!(record(&Sequence::from(vec![Value::from(3)])), "[3]");
    assert_eq!(record(&tagged("!t", Value::from("x"))), "{\"!t\":\"x\"}");
    // Inside a user type, and after oneyaml's serializer ran on this thread.
    #[derive(Serialize)]
    struct Holder {
        v: Value,
        s: Sequence,
    }
    let h = Holder {
        v: Value::from(1),
        s: Sequence::from(vec![Value::from("z")]),
    };
    assert_eq!(oneyaml::to_string(&h).unwrap(), "v: 1\ns:\n  - z\n");
    assert_eq!(record(&h), "{\"v\":1,\"s\":[\"z\"]}");
    assert_eq!(
        serde_json::to_string(&h).unwrap(),
        "{\"v\":1,\"s\":[\"z\"]}"
    );
}

/// A user type whose `Serialize` (called by oneyaml's builder) first runs
/// a third-party serializer on a `Value`, then delegates to a `Value`.
struct Nosy(Value);

impl Serialize for Nosy {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let seen = record(&self.0);
        if seen != "{\"!t\":\"x\"}" {
            return Err(ser::Error::custom(format!("third party saw {seen}")));
        }
        self.0.serialize(s)
    }
}

#[test]
fn the_mark_never_leaks_into_other_serializers() {
    let v = tagged("!t", Value::from("x"));
    // The delegated `Value` still reaches the emitter as a tagged value.
    assert_eq!(oneyaml::to_string(&Nosy(v.clone())).unwrap(), "!t x\n");
    assert_eq!(
        oneyaml::to_string(&vec![Nosy(v.clone()), Nosy(v)]).unwrap(),
        "- !t x\n- !t x\n"
    );
}

// -------------------------------------------- to yaml string matches to string

#[test]
fn to_yaml_string_matches_to_string() {
    for v in ser_common::property_values(200) {
        assert_eq!(
            v.to_yaml_string().map_err(|e| e.code()),
            oneyaml::to_string(&v).map_err(|e| e.code())
        );
        let compat = oneyaml::Compat::serde_yaml();
        assert_eq!(
            v.to_yaml_string_with(&compat).map_err(|e| e.code()),
            oneyaml::to_string_with(&v, &compat).map_err(|e| e.code())
        );
    }
    // Max_depth still applies.
    let mut deep = Value::from(1);
    for _ in 0..10 {
        deep = Value::from(vec![deep]);
    }
    let e = deep
        .to_yaml_string_with(&Settings::new().max_depth(5))
        .unwrap_err();
    assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
}

// -------------------------------------- spoofed private name gets no hand over

const PRIVATE: &str = "$__oneyaml_private_Value";

/// A user type spoofing the private newtype name.
struct Spoof<T>(T);

impl<T: Serialize> Serialize for Spoof<T> {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_newtype_struct(PRIVATE, &self.0)
    }
}

/// Content that runs a third-party serializer on a `Value` while oneyaml's
/// builder is serializing it.
struct Probe;

impl Serialize for Probe {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let inner = record(&Value::from(vec![Value::from(1)]));
        s.serialize_str(&inner)
    }
}

#[test]
fn spoofed_private_name_gets_no_hand_over() {
    assert_eq!(oneyaml::to_string(&Spoof(5)).unwrap(), "5\n");
    assert_eq!(oneyaml::to_string(&Spoof("a")).unwrap(), "a\n");
    assert_eq!(oneyaml::to_string(&Spoof(Probe)).unwrap(), "'[1]'\n");
    assert_eq!(
        oneyaml::to_string(&vec![Spoof(Probe)]).unwrap(),
        "- '[1]'\n"
    );
    // A real `Value` next to the spoof still goes through the hand-over.
    let v = tagged("!t", Value::from("x"));
    assert_eq!(
        oneyaml::to_string(&(Spoof(1), v.clone(), Spoof(Probe))).unwrap(),
        "- 1\n- !t x\n- '[1]'\n"
    );
    // The third-party serializer sees the plain data too.
    assert_eq!(record(&Spoof(v)), format!("{PRIVATE}({{\"!t\":\"x\"}})"));
}

// ------------------------------------ numbers are meaning only and never octal

#[test]
fn numbers_are_meaning_only_and_never_octal() {
    let read = |s: &str| -> Value { oneyaml::from_str(s).unwrap() };
    // Exact integers never kept a radix: `0x10` and `16` are one key.
    assert_eq!(read("0x10"), read("16"));
    assert_eq!(read("0o20"), read("16"));
    let e = oneyaml::from_str::<Value>("0x10: a\n16: b\n").unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    // Wide integers too, whatever radix they were written in.
    let hex = read("0x100000000000000000000000000000000");
    let oct = read("0o4000000000000000000000000000000000000000000");
    let dec = read("340282366920938463463374607431768211456");
    assert_eq!(hex, dec);
    assert_eq!(oct, dec);
    assert_eq!(hex, oct);
    let mut m = Mapping::new();
    m.insert(hex.clone(), Value::from("a"));
    assert!(m.contains_key(&dec) && m.contains_key(&oct));
    let e = oneyaml::from_str::<Value>(
        "0x100000000000000000000000000000000: a\n340282366920938463463374607431768211456: b\n",
    )
    .unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    assert_ne!(read("340282366920938463463374607431768211457"), hex);
    // Output: decimal or `0x`, never `0o`; a wide octal as `0x`
    // (linear).
    assert_eq!(
        oneyaml::to_string(&oct).unwrap(),
        "0x100000000000000000000000000000000\n"
    );
    assert_eq!(
        oneyaml::to_string(&hex).unwrap(),
        "0x100000000000000000000000000000000\n"
    );
    for v in [&hex, &oct, &dec] {
        let out = oneyaml::to_string(v).unwrap();
        assert!(!out.contains("0o"), "{out}");
        assert_eq!(&read(&out), v);
    }
}

// ---------------------------------------------------- scalar keys are implicit

#[test]
fn scalar_keys_are_implicit() {
    let mut m = Mapping::new();
    m.insert(Value::from(1), Value::from("x"));
    m.insert(Value::from(true), Value::from("x"));
    m.insert(Value::Null, Value::from("x"));
    m.insert(Value::from(1.5), Value::from("x"));
    m.insert(Value::from(f64::NEG_INFINITY), Value::from("x"));
    m.insert(Value::from(vec![Value::from(1)]), Value::from("x"));
    assert_eq!(
        oneyaml::to_string(&m).unwrap(),
        "1: x\ntrue: x\nnull: x\n1.5: x\n-.inf: x\n? - 1\n: x\n"
    );
}

// ------------------------------------ colliding wide keys parse in linear time

/// A block mapping of `n` wide decimal keys `10^45 + k·step`.
fn wide_keys(n: u128, step: u128) -> String {
    let mut src = String::new();
    for k in 0..n {
        let _ = writeln!(src, "1{:045}: {k}", k * step);
    }
    src
}

fn parse_checked(src: &str) {
    let v: Value = oneyaml::from_str(src).unwrap();
    assert!(v.as_mapping().is_some());
}

#[test]
fn colliding_wide_keys_parse_in_linear_time() {
    // Every key is congruent modulo 2^61 - 1 (the fixed modulus used once):
    // one hash bucket and O(n²) duplicate detection before the fix.
    let n = 8000;
    let colliding = wide_keys(n, (1 << 61) - 1);
    let control = wide_keys(n, 1);
    // Both shapes read (a quadratic duplicate check would make the first
    // one far slower, which a hang here would show; no time is measured).
    parse_checked(&control);
    parse_checked(&colliding);
    // Distinct keys stay distinct; equal ones are still duplicates.
    let dup = format!("1{:045}: a\n1{:045}: b\n", 7u128, 7u128);
    let e = oneyaml::from_str::<Value>(&dup).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
}

// ------------------------------ cross radix equality is bounded at 4096 digits

/// The decimal digits of 16^k (schoolbook, base 10^9 limbs).
fn pow16_decimal(k: usize) -> String {
    let mut limbs: Vec<u64> = vec![1];
    for _ in 0..k {
        let mut carry = 0;
        for l in &mut limbs {
            let t = *l * 16 + carry;
            *l = t % 1_000_000_000;
            carry = t / 1_000_000_000;
        }
        if carry > 0 {
            limbs.push(carry);
        }
    }
    let mut out = limbs.last().unwrap().to_string();
    for l in limbs.iter().rev().skip(1) {
        let _ = write!(out, "{l:09}");
    }
    out
}

#[test]
fn cross_radix_equality_is_bounded_at_4096_digits() {
    // 16^2999: 3,000 hex digits, 3,612 decimal digits. Both within the
    // bound: the same key (explicit `? ` keys: implicit ones stop at 1024).
    let dec = pow16_decimal(2999);
    assert!(dec.len() <= 4096);
    let src = format!("? 0x1{}\n: a\n? {dec}\n: b\n", "0".repeat(2999));
    let e = oneyaml::from_str::<Value>(&src).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
    // 16^3499: 3,500 hex digits (within), 4,214 decimal digits (beyond).
    // **Both** sides must be within the bound: compared by (radix,
    // digits), two keys.
    let dec = pow16_decimal(3499);
    assert!(dec.len() > 4096);
    let src = format!("? 0x1{}\n: a\n? {dec}\n: b\n", "0".repeat(3499));
    let v: Value = oneyaml::from_str(&src).unwrap();
    assert_eq!(v.as_mapping().unwrap().len(), 2);
    // Octal is stored as hexadecimal, so octal and hexadecimal compare
    // digit by digit at any width: 8^99999 = 2^299997 = `0x2` + 74,999
    // zeros is a duplicate; a 100,000-digit decimal is a third key, and
    // it all parses in linear time (the `Number` unit test times the
    // equal decimal twin).
    let wide = format!(
        "? 0o1{z}\n: a\n? 1{z}\n: b\n? 0x3{h}\n: c\n",
        z = "0".repeat(99_999),
        h = "0".repeat(74_999)
    );
    let v: Value = oneyaml::from_str(&wide).unwrap();
    assert_eq!(v.as_mapping().unwrap().len(), 3);
    let dup = format!(
        "? 0o1{z}\n: a\n? 0x2{h}\n: c\n",
        z = "0".repeat(99_999),
        h = "0".repeat(74_999)
    );
    let e = oneyaml::from_str::<Value>(&dup).unwrap_err();
    assert_eq!(e.code(), code::DUPLICATE_KEY);
}

// ----------------------- serializing in a thread local destructor never panics

/// What the thread-local destructors below observed.
static OBSERVED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Runs its serialization when the thread's thread-locals are destroyed.
struct SerializeOnDrop;

impl Drop for SerializeOnDrop {
    fn drop(&mut self) {
        let v = tagged("!t", Value::from(vec![Value::from(1), Value::from("x")]));
        let mut out = Vec::new();
        let mut ser = oneyaml::Serializer::new(&mut out);
        let r = v.serialize(&mut ser).map_err(|e| e.code().to_string());
        let doc = r.map(|()| String::from_utf8(out).unwrap());
        // Nested: a user type holding a `Value`, through the builder.
        let nested = oneyaml::to_string(&vec![v.clone()]).map_err(|e| e.code().to_string());
        let de = oneyaml::from_str::<Value>("!t [1, x]").map_err(|e| e.code().to_string());
        // A third-party serializer keeps getting plain serde data.
        let json = serde_json::to_string(&v).map_err(|e| e.to_string());
        let mut seen = OBSERVED.lock().unwrap();
        seen.push(format!("ser {doc:?}"));
        seen.push(format!("nested {nested:?}"));
        seen.push(format!(
            "de {:?}",
            de.map(|d| d == v).map_err(|c| c.to_string())
        ));
        seen.push(format!("json {json:?}"));
    }
}

thread_local! {
    static ON_DROP: SerializeOnDrop = const { SerializeOnDrop };
}

#[test]
fn serializing_in_a_thread_local_destructor_never_panics() {
    std::thread::Builder::new()
        .spawn(|| {
            // Registered first, so destroyed after oneyaml's slots (TLS
            // destructors run in reverse order of registration).
            ON_DROP.with(|_| ());
            // Touch oneyaml's thread-locals: serialize and deserialize.
            let v = tagged("!t", Value::from("x"));
            assert_eq!(oneyaml::to_string(&v).unwrap(), "!t x\n");
            let back: Value = oneyaml::from_str("!t x").unwrap();
            assert_eq!(back, v);
        })
        .unwrap()
        .join()
        .unwrap();
    let seen = OBSERVED.lock().unwrap().clone();
    // Without the hand-over slot, oneyaml's own serializers refuse the
    // `Value` with Y3017 (`code::THREAD_LOCAL_UNAVAILABLE`):
    // never a panic, and never the lossy plain data
    // (`'!t': [1, x]`) that third-party serializers get.
    assert_eq!(code::THREAD_LOCAL_UNAVAILABLE.to_string(), "Y3017");
    let torn_down = [
        "ser Err(\"Y3017\")".to_owned(),
        "nested Err(\"Y3017\")".to_owned(),
        // Deserializing a `Value` cannot hand it over without the slot:
        // Y3017, never a panic or a wrong value.
        "de Err(\"Y3017\")".to_owned(),
        r#"json Ok("{\"!t\":[1,\"x\"]}")"#.to_owned(),
    ];
    if TLS_DTORS_LIFO {
        assert_eq!(seen, torn_down);
    } else {
        // Destructor order is not guaranteed here: each observation is
        // either the torn-down one or an ordinary result, never lossy.
        eprintln!("note: thread-local destructor order is not guaranteed here; lenient check");
        assert_eq!(seen.len(), torn_down.len());
        for (got, down) in seen.iter().zip(&torn_down) {
            assert!(
                got == down || (got.contains("Ok(") && !got.contains("'!t'")),
                "{got}"
            );
        }
    }
}

/// Whether thread-local destructors run in reverse order of registration,
/// which this test and `code_coverage::value_in_tls_destructor` rely on to
/// run after oneyaml's slots are gone. Rust's std documents no order; in
/// practice it is LIFO on Linux and Android (glibc's
/// `__cxa_thread_atexit_impl` prepends and runs from the head; without it,
/// std's own list pops from the end) and on Windows (std's list). Other
/// platforms (Apple's `_tlv_atexit`, ...) are checked leniently.
const TLS_DTORS_LIFO: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
));

// ------------------------------- reentrant oneyaml calls keep the callers mark

/// A user type whose `Serialize` calls oneyaml itself before delegating.
struct Reentrant(Value);

impl Serialize for Reentrant {
    fn serialize<S: ser::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let inner = oneyaml::to_string(&1u8).map_err(ser::Error::custom)?;
        if inner != "1\n" {
            return Err(ser::Error::custom("inner call failed"));
        }
        let parsed: Value = oneyaml::from_str("[1]").map_err(ser::Error::custom)?;
        if parsed.as_sequence().map(Sequence::len) != Some(1) {
            return Err(ser::Error::custom("inner parse failed"));
        }
        self.0.serialize(s)
    }
}

#[test]
fn reentrant_oneyaml_calls_keep_the_callers_mark() {
    let v: Value = oneyaml::from_str("!t x").unwrap();
    assert_eq!(oneyaml::to_string(&Reentrant(v.clone())).unwrap(), "!t x\n");
    assert_eq!(
        oneyaml::to_string(&vec![Reentrant(v.clone()), Reentrant(v.clone())]).unwrap(),
        "- !t x\n- !t x\n"
    );
    // Such values take the iterative path: deeper than max_typed_depth is
    // fine (bounded by max_depth only), not Y2007.
    let mut deep = Value::from(1);
    for _ in 0..1000 {
        deep = Value::from(vec![deep]);
    }
    let out = oneyaml::to_string(&vec![Reentrant(deep.clone())]).unwrap();
    let back: Vec<Value> = oneyaml::from_str(&out).unwrap();
    assert_eq!(back, vec![deep]);
}

// ----------------------------- wide non decimal integers are linear or refused

#[test]
fn wide_non_decimal_integers_are_linear_or_refused() {
    let read =
        |s: &str, settings: &Settings| -> Value { oneyaml::from_str_with(s, settings).unwrap() };
    let core = Settings::new();
    // A 200,000-digit octal is written as `0x` in linear time.
    let oct = read(&format!("0o{}", "7".repeat(200_000)), &core);
    let out = oneyaml::to_string(&oct).unwrap();
    assert_eq!(out, format!("0x{}\n", "f".repeat(150_000)));
    assert_eq!(read(&out, &core), oct);
    // A negative wide octal (YAML 1.1 `-0…` under legacy_octal): decimal
    // within 4,096 digits...
    let legacy = Settings::new().legacy_octal(true);
    let neg = read("-04000000000000000000000000000000000000000000", &legacy);
    assert_eq!(
        oneyaml::to_string(&neg).unwrap(),
        "-340282366920938463463374607431768211456\n"
    );
    // ...and Y3015 beyond: no linear Core form exists (`-0x…` is not Core).
    let neg = read(&format!("-0{}", "7".repeat(5000)), &legacy);
    let e = oneyaml::to_string(&neg).unwrap_err();
    assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE);
    assert_eq!(
        neg.to_yaml_string().unwrap_err().code(),
        code::UNREPRESENTABLE_VALUE
    );
    // `Number::to_core_string` is an explicit call: it converts (O(d²)).
    let n = neg.as_number().unwrap().to_core_string();
    assert!(n.starts_with('-') && n.bytes().skip(1).all(|b| b.is_ascii_digit()));
    // A negative wide hexadecimal (yaml11_int_forms) likewise.
    let forms = Settings::new().yaml11_int_forms(true);
    let neg = read(&format!("-0x{}", "f".repeat(5000)), &forms);
    assert_eq!(
        oneyaml::to_string(&neg).unwrap_err().code(),
        code::UNREPRESENTABLE_VALUE
    );
}

// --------------------------------------- flattened mappings keep tagged values

#[test]
fn flattened_mappings_keep_tagged_values() {
    #[derive(Serialize)]
    struct Outer {
        name: String,
        #[serde(flatten)]
        rest: Mapping,
    }
    let mut rest = Mapping::new();
    rest.insert(Value::from("color"), tagged("!rgb", Value::from("ff0000")));
    rest.insert(
        Value::from("shape"),
        tagged("!poly", Value::from(vec![Value::from(3)])),
    );
    rest.insert(tagged("!k", Value::from("key")), Value::from(1));
    let v = Outer {
        name: "a".to_owned(),
        rest,
    };
    let out = oneyaml::to_string(&v).unwrap();
    assert_eq!(
        out,
        "name: a\ncolor: !rgb ff0000\nshape: !poly\n  - 3\n!k key: 1\n"
    );
    // It reads back as the same data. (Deserializing it into `Outer` again
    // goes through serde's flatten buffer, which cannot hold a tagged
    // value: a deserializer-side limit, reported separately.)
    let back: Value = oneyaml::from_str(&out).unwrap();
    let mut want = Mapping::new();
    want.insert(Value::from("name"), Value::from("a"));
    for (k, x) in &v.rest {
        want.insert(k.clone(), x.clone());
    }
    assert_eq!(back, Value::from(want));
    // The flattened `Mapping` itself goes through serde's flatten wrapper,
    // a serializer that is not ours, so its depth is checked first:
    // a nested `Value` deeper than max_typed_depth is refused (Y2007,
    // raised through that wrapper's `custom` error) rather than recursed.
    let mut deep = Value::from(1);
    for _ in 0..300 {
        deep = Value::from(vec![deep]);
    }
    let mut rest = Mapping::new();
    rest.insert(Value::from("deep"), deep);
    let e = oneyaml::to_string(&Outer {
        name: "b".to_owned(),
        rest,
    })
    .unwrap_err();
    assert!(e.to_string().contains("Y2007"), "{e}");
    // Third-party serializers still see the plain one-entry map.
    let json = serde_json::to_string(&Outer {
        name: "c".to_owned(),
        rest: {
            let mut m = Mapping::new();
            m.insert(Value::from("color"), tagged("!rgb", Value::from("ff0000")));
            m
        },
    })
    .unwrap();
    assert_eq!(json, "{\"name\":\"c\",\"color\":{\"!rgb\":\"ff0000\"}}");
}

// ----------------------- wide sexagesimal of either sign is decimal or refused

#[test]
fn wide_sexagesimal_of_either_sign_is_decimal_or_refused() {
    let sx = Settings::new().sexagesimal(true);
    let read = |s: &str| -> Value { oneyaml::from_str_with(s, &sx).unwrap() };
    // Within the bound: decimal, which Core reads back as an equal value.
    for sign in ["", "-"] {
        let v = read(&format!("{sign}{}:59", "1".repeat(100)));
        assert!(v.as_number().unwrap().is_big_integer());
        let out = oneyaml::to_string(&v).unwrap();
        assert!(
            out.trim_end()
                .trim_start_matches('-')
                .bytes()
                .all(|b| b.is_ascii_digit()),
            "{out}"
        );
        let back: Value = oneyaml::from_str(&out).unwrap();
        assert_eq!(back, v);
    }
    // Past the bound: Y3015 for either sign, naming the sexagesimal case.
    for sign in ["", "-"] {
        let v = read(&format!("{sign}{}:00", "1".repeat(5000)));
        let e = oneyaml::to_string(&v).unwrap_err();
        assert_eq!(e.code(), code::UNREPRESENTABLE_VALUE, "{sign}");
        assert!(e.to_string().contains("sexagesimal"), "{e}");
        assert!(e.to_string().contains("either sign"), "{e}");
        assert_eq!(
            v.to_yaml_string().unwrap_err().code(),
            code::UNREPRESENTABLE_VALUE
        );
    }
}

// ------------------------------------- handed over scalars cost no typed level

#[test]
fn handed_over_scalars_cost_no_typed_level() {
    use oneyaml::{Limits, Number};
    let one = Settings::new().limits(Limits::default().max_typed_depth(1));
    let depth_err = |r: Result<String, oneyaml::Error>| {
        assert_eq!(r.unwrap_err().code(), code::TYPED_DEPTH_EXCEEDED);
    };
    // Leaves: a `Number` or scalar `Value` costs what a `u64` costs.
    assert_eq!(oneyaml::to_string_with(&vec![1u64], &one).unwrap(), "- 1\n");
    assert_eq!(
        oneyaml::to_string_with(&vec![Number::from(1)], &one).unwrap(),
        "- 1\n"
    );
    assert_eq!(
        oneyaml::to_string_with(
            &vec![
                Value::from(1),
                Value::from("x"),
                Value::Null,
                Value::Bool(true)
            ],
            &one
        )
        .unwrap(),
        "- 1\n- x\n- null\n- true\n"
    );
    // One level below that, both kinds of leaf are refused alike.
    depth_err(oneyaml::to_string_with(&vec![vec![1u64]], &one));
    depth_err(oneyaml::to_string_with(&vec![vec![Number::from(1)]], &one));
    // Handed-over collections (and tags) still cost a level.
    let seq = Value::from(vec![Value::from(1)]);
    depth_err(oneyaml::to_string_with(&vec![seq.clone()], &one));
    depth_err(oneyaml::to_string_with(
        &vec![Sequence::from(vec![Value::from(1)])],
        &one,
    ));
    depth_err(oneyaml::to_string_with(&vec![Mapping::new()], &one));
    let tagged = Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("!t"),
        value: Value::from(1),
    }));
    depth_err(oneyaml::to_string_with(&vec![tagged], &one));
    // ... and fit one level up.
    let two = Settings::new().limits(Limits::default().max_typed_depth(2));
    assert_eq!(
        oneyaml::to_string_with(&vec![seq], &two).unwrap(),
        "- - 1\n"
    );
    // A root scalar needs no level at all.
    let zero = Settings::new().limits(Limits::default().max_typed_depth(0));
    assert_eq!(
        oneyaml::to_string_with(&Number::from(7), &zero).unwrap(),
        "7\n"
    );
    assert_eq!(oneyaml::to_string_with(&7u8, &zero).unwrap(), "7\n");
    depth_err(oneyaml::to_string_with(
        &Value::from(vec![Value::Null]),
        &zero,
    ));
}
