//! The serializer test corpus, shared by `ser_golden.rs`, `ser_props.rs`,
//! and the `oracles` crate's every-reader and `serde_yaml` comparisons
//! (included there by path).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;

use oneyaml::{
    Compat, Emit, Layout, Limits, Mapping, Number, QuoteStyle, Sequence, Settings, Tag,
    TaggedValue, Value,
};
use serde::{Deserialize, Serialize};

/// Strings every emitter gets wrong somewhere (the every-reader hazards and
/// more): white space at the ends, indicators, `: `/` #`, controls, lone
/// `-`, empty, number-like and YAML 1.1-ambiguous forms, document markers,
/// unicode, `\r`, `\t`, trailing newlines, strings of only newlines.
pub const AWKWARD: &[&str] = &[
    "",
    " ",
    "  a",
    "a ",
    "a  ",
    "\ta",
    "a\t",
    "#",
    "#a",
    "a #b",
    "a#b",
    "a: b",
    "a:",
    "a:b",
    ":",
    ":a",
    "-",
    "-a",
    "- a",
    "--",
    "---",
    "--- a",
    "...",
    "... a",
    "?",
    "?a",
    "? a",
    ",",
    ",a",
    "a,b",
    "[",
    "]",
    "{",
    "}",
    "[a]",
    "{a: b}",
    "&a",
    "*a",
    "!a",
    "!",
    "|",
    "|a",
    ">",
    ">a",
    "'",
    "'a",
    "\"",
    "\"a",
    "%",
    "%a",
    "@",
    "@a",
    "`",
    "`a",
    "a'b",
    "a\"b",
    "a\\b",
    "\\",
    "~",
    "null",
    "Null",
    "NULL",
    "true",
    "True",
    "TRUE",
    "false",
    "FALSE",
    "yes",
    "Yes",
    "YES",
    "no",
    "No",
    "NO",
    "on",
    "On",
    "ON",
    "off",
    "Off",
    "OFF",
    "y",
    "Y",
    "n",
    "N",
    "0",
    "00",
    "007",
    "0777",
    "-0",
    "+1",
    "123",
    "-123",
    "1_000",
    "1,000",
    "12:30",
    "1:20:30",
    "12:30.5",
    "0x1F",
    "0X1F",
    "0xZZ",
    "0o17",
    "0b1",
    "-0b1",
    "+0x1f",
    "0x_",
    "1e3",
    "1E3",
    "1.5e-3",
    "1.0",
    "1.",
    ".5",
    "-.5",
    "1.2.3",
    "1.1.1.1",
    ".",
    "..",
    ".inf",
    ".Inf",
    "-.inf",
    "+.inf",
    ".nan",
    ".NaN",
    "inf",
    "nan",
    "infinity",
    "2001-12-14",
    "2001-12-14t21:59:43.10-05:00",
    "2001-12-14 21:59:43.10 -5",
    "<<",
    "=",
    "é",
    "日本語",
    "😀",
    "a😀b",
    "\u{a0}",
    "a\u{a0}",
    "\u{85}",
    "\u{2028}",
    "a\u{2029}b",
    "\u{feff}a",
    "\u{ffff}",
    "\0",
    "\u{7}",
    "\u{1b}",
    "\u{7f}",
    "\u{9f}",
    "\r",
    "a\rb",
    "a\r\nb",
    "\t",
    "a\tb",
    "\n",
    "\n\n",
    "\n\n\n",
    "a\n",
    "a\nb",
    "a\nb\n",
    "a\nb\n\n",
    "a\n\n",
    "\na",
    "\n a",
    " a\nb",
    "  a\n b",
    "a\n b",
    "a \nb",
    "a\n\tb",
    "\ta\nb",
    "a\n \n",
    " \n",
    "a\n\n\nb",
    "- a\n- b",
    "a: 1\nb: 2",
    "# not a comment\nx",
    "--- \nx",
    "...\nx",
    "x\n...",
    "x\n---",
    "a\u{85}b\nc",
    "é\nü",
    // Every reader reads a plain string as a string: Ruby Psych reads
    // comma-grouped numbers, base 60 led by any digit, and `.inf`/`.nan` in any
    // case; Go yaml.v2/v3 strip every `_` before parsing a number.
    "1,2",
    "+1,000",
    "0,7",
    "1,000.5",
    "09:15",
    "00:30",
    "0:30",
    ".iNf",
    "+.INf",
    ".nAn",
    "+_1",
    "1e_5",
    "0_x1F",
    "1_e_5",
    // Psych reads null/booleans in any letter case and
    // `,` inside binary/hexadecimal digits.
    "tRUE",
    "nULL",
    "yEs",
    "oFF",
    "FaLsE",
    "0b1,0",
    "0x1,F",
    "0x,1F",
];

/// Settings used by the corpus.
pub fn serde_yaml() -> Settings {
    Compat::serde_yaml()
}

/// Reads a case's output back and compares it with its source value.
pub type Check = Box<dyn Fn(&str) -> Result<(), String>>;

/// A golden case: its name, the output (or the error code), and an
/// optional read-back check.
pub struct Case {
    pub name: String,
    pub output: Result<String, String>,
    /// Default layout (for the every-reader check).
    pub default_layout: bool,
    pub read_back: Option<Check>,
}

fn code(e: &oneyaml::Error) -> String {
    e.code().to_string()
}

fn value_case(name: &str, v: &Value, settings: &Settings) -> Case {
    let output = oneyaml::to_string_with(v, settings).map_err(|e| code(&e));
    let want = v.clone();
    let read = settings.clone();
    Case {
        name: name.to_owned(),
        output,
        default_layout: settings.get_emit().get_layout() == Layout::Default,
        read_back: Some(Box::new(move |out: &str| {
            let got: Value = oneyaml::from_str_with(out, &read).map_err(|e| e.to_string())?;
            if got == want {
                Ok(())
            } else {
                Err(format!("read back {got:?}, want {want:?}"))
            }
        })),
    }
}

fn typed_case<T>(name: &str, v: &T, settings: &Settings) -> Case
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug + Clone + 'static,
{
    let output = oneyaml::to_string_with(v, settings).map_err(|e| code(&e));
    let want = v.clone();
    let read = settings.clone();
    Case {
        name: name.to_owned(),
        output,
        default_layout: settings.get_emit().get_layout() == Layout::Default,
        read_back: Some(Box::new(move |out: &str| {
            let got: T = oneyaml::from_str_with(out, &read).map_err(|e| e.to_string())?;
            if got == want {
                Ok(())
            } else {
                Err(format!("read back {got:?}, want {want:?}"))
            }
        })),
    }
}

fn raw_case(name: &str, output: Result<String, oneyaml::Error>) -> Case {
    Case {
        name: name.to_owned(),
        output: output.map_err(|e| code(&e)),
        default_layout: !name.starts_with("serde_yaml"),
        read_back: None,
    }
}

pub fn yaml(src: &str) -> Value {
    oneyaml::from_str_with(src, &Compat::yaml_1_1()).unwrap()
}

pub fn map(entries: Vec<(Value, Value)>) -> Value {
    let mut m = Mapping::new();
    for (k, v) in entries {
        m.insert(k, v);
    }
    Value::Mapping(m)
}

pub fn seq(items: Vec<Value>) -> Value {
    Value::Sequence(Sequence::from(items))
}

pub fn tagged(tag: &str, v: Value) -> Value {
    Value::from(TaggedValue {
        tag: Tag::new(tag),
        value: v,
    })
}

fn s(v: &str) -> Value {
    Value::from(v)
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub enum Shape {
    Unit,
    Text(String),
    Count(i32),
    Flag(bool),
    Maybe(Option<i32>),
    List(Vec<u8>),
    Pair(i32, String),
    Point {
        x: f64,
        y: f64,
    },
    Nested(Box<Shape>),
    #[serde(rename = "a b")]
    Spaced(String),
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Inner {
    pub name: String,
    pub tags: Vec<String>,
    pub limits: BTreeMap<String, u32>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Service {
    pub name: String,
    pub port: u16,
    pub enabled: bool,
    pub ratio: f64,
    pub small: f32,
    pub big: i128,
    pub huge: u128,
    pub initial: char,
    pub note: Option<String>,
    pub missing: Option<String>,
    pub unit: (),
    pub pair: (i32, String),
    pub bytes: Vec<u8>,
    pub inner: Inner,
    pub shapes: Vec<Shape>,
    pub empty_list: Vec<i32>,
    pub empty_map: BTreeMap<String, i32>,
    pub script: String,
    pub id: Id,
    pub marker: Marker,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Id(pub u64);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Marker;

pub fn service() -> Service {
    let mut limits = BTreeMap::new();
    limits.insert("cpu".to_owned(), 2);
    limits.insert("memory: max".to_owned(), 512);
    Service {
        name: "web".to_owned(),
        port: 8080,
        enabled: true,
        ratio: 0.5,
        small: 1.1,
        big: i128::MIN,
        huge: u128::MAX,
        initial: 'y',
        note: Some("no".to_owned()),
        missing: None,
        unit: (),
        pair: (1, "one".to_owned()),
        bytes: vec![0, 255],
        inner: Inner {
            name: "inner".to_owned(),
            tags: vec!["a".to_owned(), "1.0".to_owned(), String::new()],
            limits,
        },
        shapes: vec![
            Shape::Unit,
            Shape::Text("hello".to_owned()),
            Shape::Text("on".to_owned()),
            Shape::Count(3),
            Shape::Flag(false),
            Shape::Maybe(None),
            Shape::Maybe(Some(7)),
            Shape::List(vec![1, 2]),
            Shape::List(vec![]),
            Shape::Pair(1, "x".to_owned()),
            Shape::Point { x: 1.0, y: -2.5 },
            Shape::Nested(Box::new(Shape::Count(1))),
            Shape::Nested(Box::new(Shape::Unit)),
            Shape::Spaced("s".to_owned()),
        ],
        empty_list: vec![],
        empty_map: BTreeMap::new(),
        script: "set -e\necho hi\n".to_owned(),
        id: Id(42),
        marker: Marker,
    }
}

/// Integers wider than 128 bits in every radix the reader keeps (`Number`
/// `==` is meaning-only).
pub const WIDE_INTEGERS: &[&str] = &[
    "0x1ffffffffffffffffffffffffffffffff",
    "0o7777777777777777777777777777777777777777777",
    "1000000000000000000000000000000000000000000",
    "-123456789012345678901234567890123456789012",
    "-0x1ffffffffffffffffffffffffffffffff",
    "-017777777777777777777777777777777777777777777",
    "1:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0",
];

/// A wide integer as a bare `Number` (read under `Compat::yaml_1_1()`).
pub fn wide_number(src: &str) -> Number {
    yaml(src).as_number().cloned().expect(src)
}

/// Every [`WIDE_INTEGERS`] entry as a bare `Number`.
pub fn wide_numbers() -> Vec<Number> {
    WIDE_INTEGERS.iter().map(|s| wide_number(s)).collect()
}

/// `Number` fields of a user type go through the
/// same hand-over as `Value::Number`, so wide integers stay numbers.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct WithNumber {
    pub n: Number,
    pub all: Vec<Number>,
    pub small: Number,
    pub maybe: Option<Number>,
}

pub fn with_number() -> WithNumber {
    WithNumber {
        n: wide_number(WIDE_INTEGERS[0]),
        all: wide_numbers(),
        small: Number::from(-7),
        maybe: Some(wide_number(WIDE_INTEGERS[2])),
    }
}

/// Every wide `Number` written as one document each through the
/// document `Serializer`.
pub fn wide_number_stream() -> Result<String, oneyaml::Error> {
    let mut out = Vec::new();
    let mut ser = oneyaml::Serializer::new(&mut out);
    for n in wide_numbers() {
        n.serialize(&mut ser)?;
    }
    ser.into_inner()?;
    Ok(String::from_utf8(out).unwrap())
}

/// A mapping of every awkward string to its index, and back.
fn awkward_map() -> Value {
    map(AWKWARD
        .iter()
        .enumerate()
        .map(|(i, a)| (s(a), Value::from(i)))
        .collect())
}

fn awkward_seq() -> Value {
    AWKWARD.iter().map(|a| s(a)).collect()
}

fn nested() -> Value {
    map(vec![
        (s("name"), s("web")),
        (s("ports"), seq(vec![Value::from(80), Value::from(443)])),
        (
            s("env"),
            map(vec![
                (s("A"), s("1")),
                (s("B"), map(vec![(s("c"), seq(vec![s("d")]))])),
            ]),
        ),
        (
            s("items"),
            seq(vec![
                map(vec![
                    (s("a"), Value::from(1)),
                    (s("b"), seq(vec![Value::from(2)])),
                ]),
                seq(vec![
                    seq(vec![Value::from(3), Value::from(4)]),
                    map(vec![(s("x"), Value::Null)]),
                ]),
                s("last"),
            ]),
        ),
        (s("block"), s("line 1\nline 2\n")),
    ])
}

fn complex_keys() -> Value {
    map(vec![
        (seq(vec![Value::from(1), Value::from(2)]), s("seq key")),
        (
            map(vec![(s("a"), Value::from(1))]),
            seq(vec![s("seq value")]),
        ),
        (s("multi\nline"), map(vec![(s("map"), s("value"))])),
        (seq(vec![]), s("empty seq key")),
        (map(vec![]), s("empty map key")),
        (Value::Null, s("null key")),
        (Value::from(1), s("int key")),
        (Value::from(1.5), s("float key")),
        (Value::from(true), s("bool key")),
        (s(&"k".repeat(1025)), s("long key")),
        (tagged("!t", seq(vec![s("x")])), s("tagged seq key")),
        (tagged("!t", s("x")), s("tagged key")),
    ])
}

fn tags() -> Value {
    map(vec![
        (s("scalar"), tagged("!color", s("red"))),
        (s("quoted"), tagged("!color", s("yes"))),
        (s("empty"), tagged("!e", s(""))),
        (s("block"), tagged("!script", s("a\nb\n"))),
        (s("seq"), tagged("!list", seq(vec![Value::from(1), s("a")]))),
        (s("map"), tagged("!obj", map(vec![(s("k"), s("v"))]))),
        (s("empty_seq"), tagged("!l", seq(vec![]))),
        (s("empty_map"), tagged("!m", map(vec![]))),
        (s("binary"), tagged("tag:yaml.org,2002:binary", s("AAEC"))),
        (s("global"), tagged("tag:example.com,2000:app/foo", s("x"))),
        (s("escaped"), tagged("!a!b c", s("x"))),
        (
            s("in_seq"),
            seq(vec![
                tagged("!i", seq(vec![Value::from(1)])),
                tagged("!j", map(vec![(s("k"), Value::from(2))])),
                tagged("!k", s("v")),
            ]),
        ),
    ])
}

fn deep_value(depth: usize) -> Value {
    let mut v = Value::from(1);
    for _ in 0..depth {
        v = seq(vec![v]);
    }
    v
}

#[derive(Debug)]
struct Refuses;

impl Serialize for Refuses {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("refused"))
    }
}

#[derive(Serialize)]
struct Flattened {
    a: i32,
    #[serde(flatten)]
    rest: BTreeMap<String, i32>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct Tree(pub Vec<Tree>);

pub fn tree(depth: usize) -> Tree {
    let mut t = Tree(vec![]);
    for _ in 0..depth {
        t = Tree(vec![t]);
    }
    t
}

fn stream(values: &[Value], settings: &Settings) -> Result<String, oneyaml::Error> {
    let mut out = Vec::new();
    let mut ser = oneyaml::Serializer::with_settings(&mut out, settings);
    for v in values {
        v.serialize(&mut ser)?;
    }
    ser.into_inner()?;
    Ok(String::from_utf8(out).unwrap())
}

fn documents() -> Vec<Value> {
    vec![
        map(vec![(s("a"), Value::from(1))]),
        seq(vec![s("x"), s("y")]),
        s("plain"),
        s("yes"),
        s("two\nlines"),
        Value::Null,
        seq(vec![]),
        tagged("!t", s("x")),
        tagged("!t", map(vec![(s("k"), s("v"))])),
    ]
}

/// Every golden case, in snapshot order.
pub fn cases() -> Vec<Case> {
    let d = Settings::new();
    let compat = serde_yaml();
    let mut c = vec![
        // Null / booleans.
        value_case("null", &Value::Null, &d),
        typed_case("null_unit", &(), &d),
        value_case("bool_true", &Value::from(true), &d),
        value_case("bool_false", &Value::from(false), &d),
        // Integers, including wide ones (Number::to_core_string).
        value_case(
            "int_small",
            &seq(vec![
                Value::from(0),
                Value::from(-1),
                Value::from(i64::MAX),
                Value::from(u64::MAX),
            ]),
            &d,
        ),
        value_case(
            "int_128",
            &seq(vec![Value::from(i128::MIN), Value::from(u128::MAX)]),
            &d,
        ),
        // `Number` `==` is meaning-only, so wide integers
        // compare as values whatever radix they were read in.
        value_case(
            "int_wide",
            &yaml(
                "- 0x1ffffffffffffffffffffffffffffffff\n- 0o7777777777777777777777777777777777777777777\n- 1000000000000000000000000000000000000000000\n- -0x1ffffffffffffffffffffffffffffffff\n- 1:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0\n- -017777777777777777777777777777777777777777777\n",
            ),
            &d,
        ),
        // A bare `Number` and `Number` fields are
        // written as `Value::Number` is (numbers, not quoted strings).
        typed_case("int_wide_number_hex", &wide_number(WIDE_INTEGERS[0]), &d),
        typed_case(
            "int_wide_number_decimal",
            &wide_number(WIDE_INTEGERS[3]),
            &d,
        ),
        typed_case("int_wide_number_field", &with_number(), &d),
        raw_case("stream_wide_numbers", wide_number_stream()),
        // Floats: shortest round trip, always `.` or an exponent.
        value_case(
            "float_forms",
            &seq([
                1.0,
                0.1,
                -2.5,
                1e15,
                1e16,
                1e300,
                1e-5,
                1.5e-7,
                5e-324,
                f64::MAX,
                123_456_789.125,
                -0.0,
                0.0,
            ]
            .into_iter()
            .map(Value::from)
            .collect()),
            &d,
        ),
        value_case(
            "float_special",
            &seq(vec![
                Value::from(f64::INFINITY),
                Value::from(f64::NEG_INFINITY),
                Value::from(f64::NAN),
            ]),
            &d,
        ),
        typed_case(
            "float_f32",
            &vec![1.1f32, 0.1, 3.4e38, 1e-7, 16_777_216.0],
            &d,
        ),
        // Strings: quoting (single; double only for escapes).
        value_case("quote_awkward_values", &awkward_seq(), &d),
        value_case("quote_awkward_keys", &awkward_map(), &d),
        value_case(
            "quote_double_style",
            &awkward_seq(),
            &Settings::new().emit(Emit::new().quote_style(QuoteStyle::Double)),
        ),
        value_case("quote_root_empty", &s(""), &d),
        value_case("quote_root_plain", &s("hello world"), &d),
        value_case("quote_root_yes", &s("yes"), &d),
        // Indentation.
        value_case("indent_default", &nested(), &d),
        value_case("indent_4", &nested(), &Settings::new().indent(4)),
        value_case("indent_1", &nested(), &Settings::new().indent(1)),
        value_case(
            "indent_sequences_off",
            &nested(),
            &Settings::new().emit(Emit::new().indent_sequences(false)),
        ),
        value_case(
            "indent_root_seq",
            &seq(vec![s("a"), seq(vec![s("b")]), map(vec![(s("c"), s("d"))])]),
            &d,
        ),
        // Block scalars.
        value_case(
            "block_chomping",
            &map(vec![
                (s("clip"), s("a\nb\n")),
                (s("strip"), s("a\nb")),
                (s("keep"), s("a\nb\n\n")),
                (s("keep_more"), s("a\n\n\n")),
            ]),
            &d,
        ),
        value_case(
            "block_indicator",
            &map(vec![
                (s("lead"), s(" a\nb")),
                (s("lead_empty"), s("\n  a\n")),
                (s("blank_line"), s("a\n\n b\n")),
                (s("in_seq"), seq(vec![s("  x\ny")])),
            ]),
            &d,
        ),
        value_case("block_root_indicator", &s("  indented\nfirst line\n"), &d),
        value_case("block_root", &s("line 1\nline 2\n"), &d),
        value_case(
            "block_indicator_indent_4",
            &map(vec![(s("k"), s(" a\nb"))]),
            &Settings::new().indent(4),
        ),
        value_case(
            "block_indicator_too_wide",
            &map(vec![(s("k"), s(" a\nb")), (s("plain"), s("a\nb"))]),
            &Settings::new().indent(10),
        ),
        value_case(
            "block_cannot",
            &seq(vec![
                s("\n"),
                s("\n\n"),
                s("a\r\nb"),
                s("a\u{85}b\nc"),
                s("x\n\u{1}"),
            ]),
            &d,
        ),
        value_case(
            "block_tabs_and_spaces",
            &seq(vec![s("a\n\tb"), s("\ta\nb"), s("a \nb"), s(" \n")]),
            &d,
        ),
        // Empty and nested empty collections.
        value_case("empty_seq", &seq(vec![]), &d),
        value_case("empty_map", &map(vec![]), &d),
        value_case(
            "empty_nested",
            &map(vec![
                (s("a"), map(vec![])),
                (s("b"), seq(vec![])),
                (s("c"), seq(vec![seq(vec![]), map(vec![])])),
                (s("d"), map(vec![(s("e"), seq(vec![]))])),
            ]),
            &d,
        ),
        // Complex keys.
        value_case("complex_keys", &complex_keys(), &d),
        // Scalar keys of any type are implicit (`1: x`,
        // `true: x`, `null: x`); `? ` only for collection keys.
        value_case(
            "keys_scalar_implicit",
            &map(vec![
                (Value::from(1), s("int")),
                (Value::from(-1.5), s("float")),
                (Value::from(true), s("bool")),
                (Value::Null, s("null")),
                (s("yes"), s("quoted string")),
                (Value::from(f64::NAN), s("nan")),
            ]),
            &d,
        ),
        value_case(
            "indent_max",
            &nested(),
            &Settings::new().indent(Emit::MAX_INDENT),
        ),
        // Tags.
        value_case("tag_values", &tags(), &d),
        value_case("tag_root_seq", &tagged("!list", seq(vec![s("a")])), &d),
        value_case("tag_root_scalar", &tagged("!x", s("1")), &d),
        // Serde shapes.
        typed_case("serde_service", &service(), &d),
        typed_case("serde_option_some", &Some(5), &d),
        typed_case("serde_option_none", &None::<i32>, &d),
        typed_case("serde_tuple", &(1, "a".to_owned(), true), &d),
        typed_case("serde_char", &'#', &d),
        typed_case("serde_bytes_vec", &vec![1u8, 2, 3], &d),
        typed_case(
            "serde_map_int_keys",
            &BTreeMap::from([(1, "a".to_owned()), (2, "b\nc".to_owned())]),
            &d,
        ),
        typed_case(
            "serde_map_tuple_keys",
            &BTreeMap::from([((1, 2), "a".to_owned())]),
            &d,
        ),
        typed_case("enum_unit", &Shape::Unit, &d),
        typed_case("enum_newtype_str", &Shape::Text("x".to_owned()), &d),
        typed_case("enum_newtype_int", &Shape::Count(1), &d),
        typed_case("enum_newtype_none", &Shape::Maybe(None), &d),
        typed_case("enum_newtype_seq", &Shape::List(vec![1, 2]), &d),
        typed_case("enum_tuple", &Shape::Pair(1, "a".to_owned()), &d),
        typed_case("enum_struct", &Shape::Point { x: 1.0, y: 2.0 }, &d),
        typed_case(
            "enum_nested",
            &Shape::Nested(Box::new(Shape::Text("t".to_owned()))),
            &d,
        ),
        typed_case("enum_renamed", &Shape::Spaced("s".to_owned()), &d),
        typed_case(
            "enum_in_map",
            &BTreeMap::from([
                ("a".to_owned(), Shape::Unit),
                ("b".to_owned(), Shape::Pair(2, "b".to_owned())),
                ("c".to_owned(), Shape::Point { x: 0.5, y: 0.0 }),
            ]),
            &d,
        ),
        // Streams.
        raw_case("stream_default", stream(&documents(), &d)),
        raw_case("stream_one_document", stream(&documents()[..1], &d)),
        raw_case("stream_empty", stream(&[], &d)),
        // Settings errors and value errors.
        raw_case(
            "error_indent_0",
            oneyaml::to_string_with(&1, &Settings::new().indent(0)),
        ),
        raw_case(
            "error_schema_json",
            oneyaml::to_string_with(&1, &Settings::new().schema(oneyaml::Schema::Json)),
        ),
        raw_case(
            "error_schema_failsafe",
            oneyaml::to_string_with(&1, &Settings::new().schema(oneyaml::Schema::Failsafe)),
        ),
        raw_case(
            "error_line_width",
            oneyaml::to_string_with(&1, &Settings::new().emit(Emit::new().line_width(Some(80)))),
        ),
        raw_case(
            "error_value_depth",
            oneyaml::to_string_with(&deep_value(9), &Settings::new().max_depth(8)),
        ),
        raw_case(
            "error_typed_depth",
            oneyaml::to_string_with(
                &tree(9),
                &Settings::new().limits(Limits::new().max_typed_depth(8)),
            ),
        ),
        raw_case("error_custom", oneyaml::to_string(&Refuses)),
        raw_case(
            "error_duplicate_key",
            oneyaml::to_string(&Flattened {
                a: 1,
                rest: BTreeMap::from([("a".to_owned(), 2)]),
            }),
        ),
        raw_case(
            "error_tagged_number",
            oneyaml::to_string(&tagged("!n", Value::from(1))),
        ),
        raw_case(
            "error_nested_tags",
            oneyaml::to_string(&tagged("!a", tagged("!b", s("x")))),
        ),
        raw_case("error_empty_tag", oneyaml::to_string(&tagged("", s("x")))),
        // §6.8.2.1 Example 6.25: `!<!>` and `!<$:?>` are invalid.
        raw_case("error_bare_tag", oneyaml::to_string(&tagged("!", s("x")))),
        raw_case(
            "error_tag_without_scheme",
            oneyaml::to_string(&tagged("$:?", s("x"))),
        ),
        raw_case(
            "error_bare_tag_key",
            oneyaml::to_string(&map(vec![(tagged("!", s("k")), s("v"))])),
        ),
        // Any indent is accepted by the type; outside
        // 1..=Emit::MAX_INDENT it is Y1012, never a panic or a blow-up.
        raw_case(
            "error_indent_too_wide",
            oneyaml::to_string_with(&nested(), &Settings::new().indent(Emit::MAX_INDENT + 1)),
        ),
        raw_case(
            "error_indent_usize_max",
            oneyaml::to_string_with(&s("a\nb\n"), &Settings::new().indent(usize::MAX)),
        ),
        raw_case(
            "error_core_tag",
            oneyaml::to_string(&tagged("tag:yaml.org,2002:str", s("x"))),
        ),
    ];
    // Layout::SerdeYaml (Compat::serde_yaml()).
    c.extend([
        value_case("serde_yaml_awkward_values", &awkward_seq(), &compat),
        value_case("serde_yaml_awkward_keys", &awkward_map(), &compat),
        value_case("serde_yaml_nested", &nested(), &compat),
        value_case(
            "serde_yaml_floats",
            &seq([1.0, 0.1, 1e15, 1e16, 1e300, 1e-5, 1e-7, 1.5e-7, -0.0]
                .into_iter()
                .map(Value::from)
                .collect()),
            &compat,
        ),
        value_case("serde_yaml_complex_keys", &complex_keys(), &compat),
        value_case("serde_yaml_tags", &tags(), &compat),
        value_case(
            "serde_yaml_blocks",
            &seq(vec![s("a\nb"), s(" a\nb"), s("\n"), s("a\n\n"), s("a \nb")]),
            &compat,
        ),
        value_case("serde_yaml_root_block_indicator", &s(" a\nb"), &compat),
        value_case(
            "serde_yaml_empty",
            &map(vec![(s("a"), seq(vec![])), (s("b"), map(vec![]))]),
            &compat,
        ),
        typed_case("serde_yaml_service", &service(), &compat),
        raw_case("serde_yaml_stream", stream(&documents(), &compat)),
    ]);
    c
}

/// A small deterministic generator (xorshift64*), so the property corpus
/// is reproducible without a dev-dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// The hazard alphabet of the string generator (the property gate):
/// white space, line breaks and whitespace-only lines, every indicator,
/// digits and number punctuation (`_ , : . e + -`), YAML 1.1 words, document
/// markers, NEL/LS/PS, `\r`, the BOM, and non-ASCII text.
pub const HAZARDS: &[&str] = &[
    " ", "  ", "\t", "\n", "\n\n", "\n\n\n", " \n", "  \n", "\t\n", "#", " #", ":", ": ", "-",
    "- ", "?", "? ", ",", "[", "]", "{", "}", "&", "*", "!", "|", ">", "'", "\"", "%", "@", "`",
    "\\", "0", "1", "7", "9", "00", "12", "_", ",", ":", ".", "e", "E", "+", "-", "x", "o", "b",
    "a", "y", "n", "on", "no", "inf", "Inf", "nan", ".inf", "null", "true", "~", "---", "...",
    "<<", "=", "\u{85}", "\u{2028}", "\u{2029}", "\r", "\r\n", "\u{feff}", "\u{a0}", "é", "😀",
    "\0", "\u{7f}",
];

/// A random string over [`HAZARDS`]; a quarter are block-scalar shapes:
/// leading whitespace-only lines, indented or tabbed content lines, and
/// several trailing line breaks.
pub fn gen_string(r: &mut Rng) -> String {
    if r.below(4) == 0 {
        return gen_block_shape(r);
    }
    (0..1 + r.below(6))
        .map(|_| HAZARDS[r.below(HAZARDS.len())])
        .collect()
}

/// `lead` whitespace-only lines, 1–3 content lines, `trail` line breaks.
pub fn gen_block_shape(r: &mut Rng) -> String {
    const BLANK: &[&str] = &["", " ", "  ", "\t", " \t"];
    const LEAD: &[&str] = &["", " ", "  ", "\t", "#", "- "];
    const BODY: &[&str] = &[
        "a", "b c", "1", "x: y", "#c", "é", "a ", "'q'", "...", "---",
    ];
    let mut out = String::new();
    for _ in 0..r.below(4) {
        out.push_str(BLANK[r.below(BLANK.len())]);
        out.push('\n');
    }
    for i in 0..1 + r.below(3) {
        if i > 0 {
            out.push_str(&"\n".repeat(1 + r.below(2)));
        }
        out.push_str(LEAD[r.below(LEAD.len())]);
        out.push_str(BODY[r.below(BODY.len())]);
    }
    out.push_str(&"\n".repeat(r.below(5)));
    out
}

/// A random tag with a spec-valid written form: a local `!…` tag or a
/// `tag:` URI over awkward ASCII tag characters (escaped when written; a
/// non-ASCII character has no written form that reads back), or one of the
/// usual shapes.
pub fn gen_tag(r: &mut Rng) -> String {
    const CHARS: &[&str] = &[
        "a", "Z", "0", "!", "%", " ", "_", ",", "[", "]", "{", "}", "/", ":", "#", "~", "<", ">",
        "\"", "'", "^", "\\", "|", "-", ".", "%41", "\t", "*", "&",
    ];
    let body: String = (0..1 + r.below(5))
        .map(|_| CHARS[r.below(CHARS.len())])
        .collect();
    match r.below(4) {
        0 | 1 => format!("!{body}"),
        2 => format!("tag:example.com,2000:{body}"),
        _ => [
            "!t",
            "!long-tag.name",
            "tag:example.com,2000:x",
            "!tt",
            "tag:yaml.org,2002:binary",
        ][r.below(5)]
        .to_owned(),
    }
}

fn gen_scalar(r: &mut Rng) -> Value {
    match r.below(14) {
        0 => Value::Null,
        1 => Value::from(r.below(2) == 0),
        2 => Value::from(r.next() as i64),
        3 => Value::from(r.next()),
        4 => Value::from(((r.next() as u128) << 64 | r.next() as u128) as i128),
        5 => Value::from(f64::from_bits(r.next())),
        6 => Value::from([0.5, -0.0, 1e16, 1e-7, f64::INFINITY, f64::NAN, 3.0][r.below(7)]),
        // Wide integers in every radix (`Number` `==` is meaning-only).
        7 => yaml(
            [
                "0x1ffffffffffffffffffffffffffffffff",
                "0o7777777777777777777777777777777777777777777",
                "-123456789012345678901234567890123456789012",
                "-0x1ffffffffffffffffffffffffffffffff",
                "-017777777777777777777777777777777777777777777",
                "1:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0:0",
            ][r.below(6)],
        ),
        8..=10 => Value::from(AWKWARD[r.below(AWKWARD.len())]),
        _ => Value::from(gen_string(r)),
    }
}

/// A random `Value` at most `depth` levels deep: every scalar kind, the
/// awkward strings, tags on strings and collections, any-`Value` keys.
pub fn gen_value(r: &mut Rng, depth: usize) -> Value {
    let choice = if depth == 0 { 0 } else { r.below(10) };
    match choice {
        0..=4 => gen_scalar(r),
        5 | 6 => (0..r.below(4)).map(|_| gen_value(r, depth - 1)).collect(),
        7 | 8 => {
            let mut m = Mapping::new();
            for _ in 0..r.below(4) {
                let key = if r.below(4) == 0 {
                    gen_value(r, depth - 1)
                } else {
                    gen_scalar(r)
                };
                m.insert(key, gen_value(r, depth - 1));
            }
            Value::Mapping(m)
        }
        _ => {
            let content = match r.below(4) {
                0 => Value::from(AWKWARD[r.below(AWKWARD.len())]),
                1 => Value::from(gen_string(r)),
                2 => (0..r.below(3)).map(|_| gen_value(r, depth - 1)).collect(),
                _ => {
                    let mut m = Mapping::new();
                    m.insert(gen_scalar(r), gen_value(r, depth - 1));
                    Value::Mapping(m)
                }
            };
            tagged(&gen_tag(r), content)
        }
    }
}

/// The property corpus: `n` generated values plus fixed edge cases.
pub fn property_values(n: usize) -> Vec<Value> {
    let mut r = Rng::new(0x5eed_0f0e_7a31);
    let mut out: Vec<Value> = (0..n).map(|_| gen_value(&mut r, 4)).collect();
    out.extend(AWKWARD.iter().map(|a| Value::from(*a)));
    out.push(awkward_map());
    out.push(complex_keys());
    out.push(tags());
    out.push(nested());
    out.push(deep_value(200));
    out
}

/// The settings the property gate runs under.
pub fn property_settings() -> Vec<(&'static str, Settings)> {
    vec![
        ("default", Settings::new()),
        ("serde_yaml", serde_yaml()),
        (
            "indent_4_double",
            Settings::new()
                .indent(4)
                .emit(Emit::new().indent(4).quote_style(QuoteStyle::Double)),
        ),
        (
            "indent_1_flat",
            Settings::new().emit(
                Emit::new()
                    .indent(1)
                    .indent_sequences(false)
                    .layout(Layout::Default),
            ),
        ),
    ]
}

/// A generated sweep of scalar-looking texts — signs × prefixes ×
/// digit shapes, plus the words readers type (booleans, nulls, `inf`/`nan`
/// spellings, merge and value keys, timestamps). Written as *strings*, each
/// must read back as the same string, in every layout, under the settings
/// it was written for.
pub fn scalar_sweep() -> Vec<String> {
    const SIGNS: [&str; 3] = ["", "-", "+"];
    const PREFIXES: [&str; 8] = ["", "0", "0x", "0o", "0b", "0X", "0O", "0B"];
    const DIGITS: [&str; 17] = [
        "1",
        "17",
        "8",
        "1F",
        "1_0",
        "1_",
        "_1",
        "1:30",
        "12:30:00",
        "1:30.5",
        "1.5",
        "1e3",
        "1.5e+3",
        ".5",
        "",
        "99999999999999999999999999999999999999999",
        "1,000",
    ];
    const WORDS: &[&str] = &[
        "true",
        "True",
        "TRUE",
        "tRUE",
        "false",
        "False",
        "FALSE",
        "yes",
        "Yes",
        "YES",
        "no",
        "No",
        "NO",
        "on",
        "On",
        "ON",
        "off",
        "Off",
        "OFF",
        "y",
        "Y",
        "n",
        "N",
        "null",
        "Null",
        "NULL",
        "nULL",
        "~",
        "nil",
        "None",
        ".inf",
        ".Inf",
        ".INF",
        "-.inf",
        "+.inf",
        ".iNf",
        ".nan",
        ".NaN",
        ".NAN",
        "inf",
        "nan",
        "NaN",
        "-inf",
        "infinity",
        "<<",
        "=",
        "-",
        "+",
        ".",
        "0.",
        "-0",
        "+0",
        "00",
        "1e400",
        "-1e400",
        "2001-12-14",
        "2001-12-14t21:59:43.10-05:00",
        "2001-12-14 21:59:43.10 -5",
        "12:60",
        "0:30",
        "190:20:30.15",
        "685_230.15",
        "0x_1F",
        "0b1_0",
        "0o_7",
    ];
    let mut out = Vec::new();
    for sign in SIGNS {
        for prefix in PREFIXES {
            for digits in DIGITS {
                out.push(format!("{sign}{prefix}{digits}"));
            }
        }
    }
    out.extend(WORDS.iter().map(|w| (*w).to_owned()));
    out.sort();
    out.dedup();
    out
}
