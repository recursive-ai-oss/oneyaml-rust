//! `Deserializer for Value` (`from_value`, `&Value`, `Value`).
//!
//! `from_value::<T>(from_str::<Value>(s)?)` reads what
//! `from_str::<T>(s)` reads — the same values, the same codes and paths —
//! except the one documented difference (a `Value` has no source text, so
//! the plain-scalar-to-string reading has nothing to read: `Y3000`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;

use oneyaml::{
    Code, Limits, Mapping, Settings, Spanned, Tag, TaggedValue, Value, code, from_str,
    from_str_with, from_value, from_value_with,
};
use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};

type Outcome<T> = Result<T, (Code, Option<String>)>;

fn outcome<T>(r: Result<T, oneyaml::Error>) -> Outcome<T> {
    r.map_err(|e| (e.code(), e.path().map(ToString::to_string)))
}

/// `from_str::<T>` and `from_value::<T>` of the same input agree: equal
/// values, or the same code and path.
fn agree<T: DeserializeOwned + PartialEq + Debug>(src: &str) -> Outcome<T> {
    let direct = outcome(from_str::<T>(src));
    let v: Value = from_str(src).unwrap();
    let via = outcome(from_value::<T>(v.clone()));
    assert_eq!(direct, via, "from_str vs from_value: {src:?}");
    let borrowed = outcome(T::deserialize(&v));
    assert_eq!(direct, borrowed, "from_str vs &Value: {src:?}");
    let owned = outcome(T::deserialize(v));
    assert_eq!(direct, owned, "from_str vs Value: {src:?}");
    direct
}

#[derive(Debug, PartialEq, Deserialize)]
struct Container {
    name: String,
    image: Option<String>,
    ports: Vec<Port>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Port {
    container_port: u16,
    protocol: Option<Protocol>,
}

#[derive(Debug, PartialEq, Deserialize)]
enum Protocol {
    #[serde(rename = "TCP")]
    Tcp,
    #[serde(rename = "UDP")]
    Udp,
}

#[derive(Debug, PartialEq, Deserialize)]
enum Shape {
    Unit,
    Circle(f64),
    Point(i32, i32),
    Rect { w: u32, h: u32 },
    Tags(Vec<String>),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum IntOrString {
    Int(i64),
    Str(String),
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "kind")]
enum Object {
    Service { name: String },
    Secret { data: BTreeMap<String, String> },
}

#[derive(Debug, PartialEq, Deserialize)]
struct Flat {
    name: String,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Strict {
    a: u8,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Unit;

#[derive(Debug, PartialEq, Deserialize)]
struct Meters(f64);

#[test]
fn typed_reads_agree_with_from_str() {
    let k8s = "name: web\nimage: nginx:1.25\nports:\n  - containerPort: 80\n    protocol: TCP\n  - containerPort: 9090\nenv:\n  MODE: prod\n";
    assert!(agree::<Container>(k8s).is_ok());
    assert!(agree::<Value>(k8s).is_ok());
    assert!(
        agree::<Vec<Shape>>(
            "[Unit, {Circle: 1.5}, {Point: [1, -2]}, {Rect: {w: 3, h: 4}}, {Tags: [a, b]}]"
        )
        .is_ok()
    );
    // Tags as enum variants: scalars carry a string, collections content.
    assert!(
        agree::<Vec<Shape>>("[!Unit , !Circle 1.5, !Point [1, 2], !Rect {w: 1, h: 2}]").is_err()
    );
    assert!(agree::<Vec<Shape>>("[!Unit , !Tags [a], !Rect {w: 1, h: 2}]").is_ok());
    assert!(agree::<Vec<IntOrString>>("[1, x, '2']").is_ok());
    assert!(
        agree::<Vec<Object>>("[{kind: Service, name: web}, {kind: Secret, data: {k: v}}]").is_ok()
    );
    assert!(agree::<Flat>("name: a\nx: 1\ny: [2]\n").is_ok());
    assert!(agree::<Flat>("name: a\nz: !t q\n").is_err());
    assert!(agree::<(u8, String, Option<bool>)>("[1, a, ~]").is_ok());
    assert!(agree::<Unit>("~").is_ok());
    assert!(agree::<Meters>("2.5").is_ok());
    assert!(agree::<char>("x").is_ok());
    assert!(agree::<Option<Option<u8>>>("~").is_ok());
    assert!(agree::<Vec<f64>>("[1, 1.5, -.inf, 0x10]").is_ok());
    assert!(agree::<i128>("-170141183460469231731687303715884105728").is_ok());
    assert!(agree::<u128>("340282366920938463463374607431768211455").is_ok());
    assert!(agree::<HashMap<i64, bool>>("{1: true, 0x2: false}").is_ok());
    assert!(agree::<BTreeMap<String, Value>>("a: 1\nb: [x]\n").is_ok());
    assert!(agree::<()>("null").is_ok());
    assert!(agree::<String>("'quoted'").is_ok());
    assert!(agree::<serde_json::Value>("a: [1, 2.5, true, null, x]\n").is_ok());
    // Ignored content (the target reads nothing of it).
    let _ = agree_ignored("a: [1, {b: !x c}]\n");
}

fn agree_ignored(src: &str) -> Outcome<()> {
    let direct = outcome(from_str::<IgnoredAny>(src).map(|_| ()));
    let via = outcome(from_value::<IgnoredAny>(from_str(src).unwrap()).map(|_| ()));
    assert_eq!(direct, via);
    direct
}

#[test]
fn errors_agree_in_code_and_path() {
    let cases: &[(&str, Code, &str)] = &[
        (
            "ports: [{containerPort: 99999}]\nname: a\n",
            code::INTEGER_OUT_OF_RANGE,
            "ports[0].containerPort",
        ),
        (
            "name: a\nports: [{containerPort: '80'}]\n",
            code::TYPE_MISMATCH,
            "ports[0].containerPort",
        ),
        (
            "name: a\nports: [{containerPort: 1, protocol: SCTP}]\n",
            code::UNKNOWN_VARIANT,
            "ports[0].protocol",
        ),
        ("name: a\n", code::MISSING_FIELD, ""),
        (
            "name: a\nports: [{containerPort: .inf}]\n",
            code::TYPE_MISMATCH,
            "ports[0].containerPort",
        ),
        ("name: [a]\nports: []\n", code::TYPE_MISMATCH, "name"),
    ];
    for (src, want, path) in cases {
        let got = agree::<Container>(src).unwrap_err();
        assert_eq!(got.0, *want, "{src}");
        assert_eq!(got.1.as_deref().unwrap_or(""), *path, "{src}");
    }
    assert_eq!(
        agree::<Strict>("{a: 1, b: 2}").unwrap_err().0,
        code::UNKNOWN_FIELD
    );
    assert_eq!(
        agree::<(u8, u8)>("[1, 2, 3]").unwrap_err().0,
        code::INVALID_LENGTH
    );
    assert_eq!(
        agree::<(u8, u8)>("[1]").unwrap_err().0,
        code::INVALID_LENGTH
    );
    // Map-style enums: one key only, and it is a scalar.
    assert_eq!(agree::<Shape>("{}").unwrap_err().0, code::INVALID_LENGTH);
    assert_eq!(
        agree::<Shape>("{Unit: ~, Circle: 1}").unwrap_err().0,
        code::INVALID_LENGTH
    );
    assert_eq!(
        agree::<Shape>("{[a]: 1}").unwrap_err().0,
        code::TYPE_MISMATCH
    );
    assert_eq!(
        agree::<Shape>("{Rect: {w: 1, h: x}}").unwrap_err(),
        (code::TYPE_MISMATCH, Some("Rect.h".to_owned()))
    );
    assert_eq!(
        agree::<Shape>("!Rect {w: 1}").unwrap_err().1.as_deref(),
        Some("Rect")
    );
    assert_eq!(
        agree::<Shape>("!Unit x").unwrap_err().0,
        code::TYPE_MISMATCH
    );
    assert_eq!(agree::<u8>("!x 1").unwrap_err().0, code::TYPE_MISMATCH);
    assert_eq!(
        agree::<Vec<u8>>("!x [1]").unwrap_err().0,
        code::TYPE_MISMATCH
    );
    assert_eq!(agree::<u8>("[1]").unwrap_err().0, code::TYPE_MISMATCH);
    assert_eq!(
        agree::<Vec<u8>>("{a: 1}").unwrap_err().0,
        code::TYPE_MISMATCH
    );
    assert_eq!(agree::<bool>("'true'").unwrap_err().0, code::TYPE_MISMATCH);
    assert_eq!(
        agree::<u64>("-1").unwrap_err().0,
        code::INTEGER_OUT_OF_RANGE
    );
    assert_eq!(agree::<f64>("1").unwrap(), 1.0);
    let wide = "123456789012345678901234567890123456789012345678901234567890";
    assert_eq!(
        agree::<f64>(wide).unwrap_err().0,
        code::INTEGER_OUT_OF_RANGE
    );
    assert_eq!(agree::<IgnoredAnyEq>(wide), Ok(IgnoredAnyEq));
    assert_eq!(
        agree::<serde_json::Value>(wide).unwrap_err().0,
        code::INTEGER_OUT_OF_RANGE
    );
}

/// `IgnoredAny` with `PartialEq` (for `agree`).
#[derive(Debug, PartialEq)]
struct IgnoredAnyEq;

impl<'de> Deserialize<'de> for IgnoredAnyEq {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(d).map(|_| IgnoredAnyEq)
    }
}

/// The one documented difference (module docs of `de/value.rs`): a string
/// target reads a plain scalar's *source text*; a `Value` has none.
#[test]
fn resolved_scalars_never_become_strings() {
    let src = "port: 8080\nversion: 1.10\ndebug: true\nnothing: ~\n";
    let direct: BTreeMap<String, String> = from_str(src).unwrap();
    assert_eq!(direct["version"], "1.10");
    let v: Value = from_str(src).unwrap();
    let e = from_value::<BTreeMap<String, String>>(v.clone()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert_eq!(e.path().unwrap().to_string(), "port");
    assert_eq!(
        e.to_string(),
        "invalid type: integer `8080`, expected a string at port"
    );
    for (field, what) in [
        ("version", "floating point `1.1`"),
        ("debug", "boolean `true`"),
        ("nothing", "unit value"),
    ] {
        let e = from_value::<String>(v.get(field).unwrap().clone()).unwrap_err();
        assert_eq!(e.code(), code::TYPE_MISMATCH);
        assert_eq!(
            e.to_string(),
            format!("invalid type: {what}, expected a string")
        );
    }
    // Keys likewise.
    let e = from_value::<BTreeMap<String, u8>>(from_str("1: 2").unwrap()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    // Strings stay strings, never numbers or bools (both ways).
    let e = from_value::<u16>(Value::from("8080")).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    // The same rule as an explicit core tag in the source.
    assert_eq!(
        from_str::<String>("!!int 8080").unwrap_err().code(),
        from_value::<String>(from_str("8080").unwrap())
            .unwrap_err()
            .code()
    );
}

#[test]
fn errors_have_paths_but_no_spans() {
    let v: Value = from_str("a:\n  - {b: 1}\n  - {b: x}\n").unwrap();
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct A {
        a: Vec<B>,
    }
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct B {
        b: u8,
    }
    let e = from_value::<A>(v).unwrap_err();
    assert_eq!(e.path().unwrap().to_string(), "a[1].b");
    assert_eq!(e.span(), None);
    assert_eq!(e.location(), None);
    assert_eq!(
        e.to_string(),
        "invalid type: string \"x\", expected u8 at a[1].b"
    );
    // Out of range names the number (canonical form: no source text).
    let e = from_value::<u8>(from_str("0x1FF").unwrap()).unwrap_err();
    assert_eq!(e.to_string(), "integer 511 is out of range for u8");
    assert_eq!(e.span(), None);
}

#[test]
fn spanned_needs_source_text() {
    let v: Value = from_str("port: 8080").unwrap();
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct P {
        port: Spanned<u16>,
    }
    let e = from_value::<P>(v.clone()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert_eq!(e.path().unwrap().to_string(), "port");
    assert!(e.to_string().contains("has no source spans"), "{e}");
    let e = from_value::<Option<Spanned<Value>>>(v).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
}

#[test]
fn borrowed_and_owned_deserializers() {
    #[derive(Deserialize)]
    struct Env<'a> {
        name: &'a str,
        #[serde(borrow)]
        value: std::borrow::Cow<'a, str>,
    }
    let v: Value = from_str("name: LOG_LEVEL\nvalue: info\n").unwrap();
    let env = Env::deserialize(&v).unwrap();
    assert_eq!(env.name, "LOG_LEVEL");
    assert!(matches!(env.value, std::borrow::Cow::Borrowed("info")));
    // An owned `Value` cannot lend its strings.
    assert_eq!(u8::deserialize(Value::from(3)).unwrap(), 3);
    assert!(<&str>::deserialize(Value::from("x")).is_err());
    // A `Value` target takes the value itself (or a clone).
    let back = Value::deserialize(&v).unwrap();
    assert_eq!(back, v);
    assert_eq!(from_value::<Value>(v.clone()).unwrap(), v);
    assert_eq!(Value::deserialize(v.clone()).unwrap(), v);
}

#[test]
fn programmatic_values() {
    // Tagged content that YAML text cannot give (a tagged number): the
    // variant's content is read as it is.
    #[derive(Debug, PartialEq, Deserialize)]
    enum E {
        #[serde(rename = "x")]
        X(u8),
    }
    let t = Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("!x"),
        value: Value::from(5),
    }));
    assert_eq!(from_value::<E>(t.clone()).unwrap(), E::X(5));
    assert_eq!(from_value::<u8>(t).unwrap_err().code(), code::TYPE_MISMATCH);
    // Non-string keys in typed maps.
    let mut m = Mapping::new();
    m.insert(Value::from(true), Value::from(1));
    let r: HashMap<bool, u8> = from_value(Value::Mapping(m)).unwrap();
    assert_eq!(r[&true], 1);
}

/// A recursive target type: each typed level counts (`Y2007`), exactly as
/// the streaming deserializer counts it.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Tree(Vec<Tree>);

fn nested(depth: usize) -> String {
    let mut s = "[".repeat(depth);
    s.push_str(&"]".repeat(depth));
    s
}

#[test]
fn typed_depth_matches_the_streaming_reader() {
    let run = || {
        for depth in [127, 128, 129, 200] {
            let src = nested(depth);
            let direct = from_str::<Tree>(&src).map(|_| ()).map_err(|e| e.code());
            let via = from_value::<Tree>(from_str(&src).unwrap())
                .map(|_| ())
                .map_err(|e| e.code());
            assert_eq!(direct, via, "depth {depth}");
        }
        let deep = from_str::<Value>(&nested(200)).unwrap();
        let e = from_value::<Tree>(deep.clone()).unwrap_err();
        assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
        let s = Settings::new().limits(Limits::default().max_typed_depth(300));
        assert!(from_value_with::<Tree>(deep, &s).is_ok());
    };
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

/// A 1,000,000-deep `Value` (sequences, or a chain of tags
/// that only a programmatic value can hold) is refused on a small stack.
#[test]
fn deep_values_on_a_small_stack() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    enum Chain {
        #[serde(rename = "t")]
        T(Box<Chain>),
        #[serde(rename = "end")]
        End,
    }
    let run = || {
        let mut seq = Value::Null;
        for _ in 0..1_000_000 {
            seq = Value::from(vec![seq]);
        }
        let e = from_value::<Value>(seq.clone()).unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
        let e = from_value::<IgnoredAny>(seq.clone()).unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
        let e = Tree::deserialize(&seq).unwrap_err();
        assert_eq!(e.code(), code::VALUE_DEPTH_EXCEEDED);
        // A debug build takes ~7 KiB of stack per typed level (see the
        // `max_typed_depth` docs).
        let shallow = Limits::default().max_typed_depth(16);
        let unlimited = Settings::new().limits(Limits::unlimited().max_typed_depth(16));
        let e = from_value_with::<Tree>(seq, &unlimited).unwrap_err();
        assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);

        let mut tags = Value::from("end");
        for _ in 0..1_000_000 {
            tags = Value::Tagged(Box::new(TaggedValue {
                tag: Tag::new("!t"),
                value: tags,
            }));
        }
        let typed = Settings::new().limits(shallow);
        let e = from_value_with::<Chain>(tags.clone(), &typed).unwrap_err();
        assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
        assert!(from_value::<IgnoredAny>(tags.clone()).is_ok());
        assert_eq!(from_value::<Value>(tags.clone()).unwrap(), tags);
    };
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

/// Options and newtypes re-enter the same node
/// through the target's own `Deserialize`; each re-entry counts a typed
/// level (`Y2007`) on every entry point, so a type that recurses without
/// consuming input is refused, never a stack overflow.
#[test]
fn same_node_reentry_is_bounded() {
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct R(Option<Box<R>>);
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct N(Box<N>);
    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct S(Spanned<Box<S>>);
    fn check<T: DeserializeOwned + Debug>(settings: &Settings) {
        for src in ["1", "x", "[1]", "{a: 1}", "!t x", "!t [1]"] {
            let direct = from_str_with::<T>(src, settings).unwrap_err();
            assert_eq!(direct.code(), code::TYPED_DEPTH_EXCEEDED, "{src}");
            let v: Value = from_str(src).unwrap();
            let e = from_value_with::<T>(v, settings).unwrap_err();
            assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED, "{src}");
            let doc = oneyaml::Document::parse_with(src, settings).unwrap();
            let e = doc.root().unwrap().deserialize::<T>().unwrap_err();
            assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED, "{src}");
        }
    }
    // Default limits on the 8 MiB stack a probe once overflowed.
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(|| {
            let s = Settings::new();
            check::<R>(&s);
            check::<N>(&s);
        })
        .unwrap()
        .join()
        .unwrap();
    // A small stack with a small bound.
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            let s = Settings::new().limits(Limits::default().max_typed_depth(16));
            check::<R>(&s);
            check::<N>(&s);
            let e = from_str_with::<S>("1", &s).unwrap_err();
            assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
            let doc = oneyaml::Document::parse_with("[1]", &s).unwrap();
            let e = doc.root().unwrap().deserialize::<S>().unwrap_err();
            assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A re-entry whose visitor returns without
/// reading the node (legal serde) leaves no free pass: the next reader of
/// that node pays its typed level. `W` ignores its `Some` deserializer
/// (`WErr` first swallows an error from it); `Deep` reads one `W`, then
/// recurses into itself for each further element.
mod unread_reentry {
    use super::*;
    use serde::de::{SeqAccess, Visitor};

    pub struct W;
    pub struct WErr;

    macro_rules! ignoring_option {
        ($t:ident, $swallow:expr) => {
            impl<'de> Deserialize<'de> for $t {
                fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                    struct V;
                    impl<'de> Visitor<'de> for V {
                        type Value = $t;
                        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                            f.write_str("anything")
                        }
                        fn visit_none<E>(self) -> Result<$t, E> {
                            Ok($t)
                        }
                        fn visit_some<D: serde::Deserializer<'de>>(
                            self,
                            d: D,
                        ) -> Result<$t, D::Error> {
                            if $swallow {
                                let _ = u8::deserialize(d);
                            }
                            Ok($t)
                        }
                    }
                    d.deserialize_option(V)
                }
            }
        };
    }
    ignoring_option!(W, false);
    ignoring_option!(WErr, true);

    pub struct Deep<T>(std::marker::PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Deserialize<'de> for Deep<T> {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V<T>(std::marker::PhantomData<T>);
            impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
                type Value = Deep<T>;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a sequence")
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Deep<T>, A::Error> {
                    let _ = a.next_element::<T>()?;
                    while a.next_element::<Deep<T>>()?.is_some() {}
                    Ok(Deep(std::marker::PhantomData))
                }
            }
            d.deserialize_seq(V(std::marker::PhantomData))
        }
    }
}

#[test]
fn unread_reentries_leave_no_free_pass() {
    use unread_reentry::{Deep, W, WErr};

    fn outcome<T>(r: Result<T, oneyaml::Error>) -> Result<(), Code> {
        r.map(|_| ()).map_err(|e| e.code())
    }
    /// Every entry point returns (never overflows) and they agree: the
    /// node `W` ignored is consumed, never re-read by the next element.
    fn probe<T: DeserializeOwned>(src: &str, settings: &Settings) -> Result<(), Code> {
        let direct = outcome(from_str_with::<Deep<T>>(src, settings));
        let doc = oneyaml::Document::parse_with(src, settings).unwrap();
        let node = outcome(doc.root().unwrap().deserialize::<Deep<T>>());
        assert_eq!(direct, node, "node read");
        let v: Value = from_str_with(src, settings).unwrap();
        let via = outcome(from_value_with::<Deep<T>>(v, settings));
        assert_eq!(direct, via, "from_value");
        direct
    }
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            let s = Settings::new().limits(Limits::default().max_typed_depth(16));
            assert_eq!(probe::<W>(&nested(1000), &s), Ok(()));
            // Each `W` ignores a whole deep item; the chain of `Deep`
            // items still counts one level each.
            let mut chain = String::new();
            for _ in 0..1000 {
                chain.push_str("[[1], ");
            }
            chain.push_str(&"]".repeat(1000));
            assert_eq!(probe::<W>(&chain, &s), Err(code::TYPED_DEPTH_EXCEEDED));
            // A swallowed error before reading: never a crash, and the same
            // outcome on every entry point (`from_str`, a node read,
            // `from_value`). A type mismatch leaves the read in
            // step (the refused node is skipped), so swallowing it reads on
            // exactly as `W`, which never reads the node.
            for src in [
                nested(1000).as_str(),
                chain.as_str(),
                "[[1]]",
                "[{a: 1}, 3]",
            ] {
                assert_eq!(probe::<WErr>(src, &s), probe::<W>(src, &s), "{src}");
            }
            assert_eq!(probe::<WErr>("[[1]]", &s), Ok(()));
            // Swallowing the input's own error poisons the read (Y3018),
            // as `from_value` cannot see it (the `Value` read refuses it).
            let src = "[{a: 1, a: 2}, [1]]";
            assert_eq!(
                outcome(from_str_with::<Deep<WErr>>(src, &s)),
                Err(code::READ_AFTER_IGNORED_ERROR)
            );
            // An ignored error for a scalar leaves nothing unread: no error.
            assert_eq!(probe::<WErr>("[x, [y]]", &s), Ok(()));
            assert_eq!(probe::<W>("[[1]]", &s), Ok(()));
        })
        .unwrap()
        .join()
        .unwrap();
    // Default limits, the probe's depths: 1000 on 8 MiB, and 200k with
    // `max_depth` raised.
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(|| {
            assert_eq!(probe::<W>(&nested(1000), &Settings::new()), Ok(()));
            let deep = Settings::new().limits(Limits::default().max_depth(1_000_000));
            assert_eq!(probe::<W>(&nested(200_000), &deep), Ok(()));
            let mut chain = String::new();
            for _ in 0..200_000 {
                chain.push_str("[[1], ");
            }
            chain.push_str(&"]".repeat(200_000));
            let e = from_str_with::<Deep<W>>(&chain, &deep).map(|_| ());
            assert_eq!(e.map_err(|e| e.code()), Err(code::TYPED_DEPTH_EXCEEDED));
            assert_eq!(
                probe::<WErr>(&chain, &deep),
                Err(code::TYPED_DEPTH_EXCEEDED)
            );
        })
        .unwrap()
        .join()
        .unwrap();
}
