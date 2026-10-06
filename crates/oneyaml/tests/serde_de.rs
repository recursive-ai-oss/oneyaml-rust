//! Serde deserializer: entry points, string coercions, enums, flatten,
//! untagged, borrowing, `Spanned`, errors.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::{BTreeMap, HashMap};

use oneyaml::{Compat, ErrorKind, Settings, Spanned, Value, code, from_str, from_str_with};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Port {
    name: String,
    port: u16,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Spec {
    ports: Vec<Port>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Manifest {
    spec: Spec,
}

#[test]
fn struct_and_error_path_and_location() {
    let ok: Manifest = from_str("spec:\n  ports:\n    - {name: a, port: 80}\n").unwrap();
    assert_eq!(ok.spec.ports[0].port, 80);
    let src = "spec:\n  ports:\n    - name: a\n      port: 1\n    - name: b\n      port: 2\n    - name: c\n      port: 99999\n";
    let e = from_str::<Manifest>(src).unwrap_err();
    assert_eq!(e.code(), code::INTEGER_OUT_OF_RANGE);
    assert_eq!(e.kind(), ErrorKind::Data);
    assert_eq!(
        e.to_string(),
        "integer 99999 is out of range for u16 at spec.ports[2].port (8:13)"
    );
    assert_eq!(e.path().unwrap().to_string(), "spec.ports[2].port");
    let loc = e.location().unwrap();
    assert_eq!((loc.line(), loc.column()), (8, 13));
    assert_eq!(
        &src[e.span().unwrap().start..e.span().unwrap().end],
        "99999"
    );
    // Missing field: located at the mapping.
    let e = from_str::<Port>("name: a\n").unwrap_err();
    assert_eq!(e.code(), code::MISSING_FIELD);
    assert_eq!(e.to_string(), "missing field `port` (1:1)");
}

#[test]
fn plain_scalars_into_strings_use_source_text() {
    #[derive(Deserialize)]
    struct S {
        a: String,
        b: String,
        c: String,
        d: String,
        e: String,
        f: String,
    }
    let s: S = from_str("a: 8080\nb: 1.10\nc: true\nd: ~\ne:\nf: 0x1F\n").unwrap();
    assert_eq!(
        [s.a, s.b, s.c, s.d, s.e, s.f],
        ["8080", "1.10", "true", "~", "", "0x1F"]
    );
    // Tagged core types are not plain text any more.
    let e = from_str::<String>("!!int 8080").unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert_eq!(from_str::<String>("!!str 8080").unwrap(), "8080");
    // `Value` sees the resolved type (same node, one interpretation).
    assert_eq!(from_str::<Value>("8080").unwrap(), Value::from(8080));
}

#[test]
fn quoted_never_number_or_bool() {
    for (src, what) in [("'123'", "u16"), ("\"123\"", "u16"), ("'true'", "bool")] {
        let e = if what == "bool" {
            from_str::<bool>(src).unwrap_err()
        } else {
            from_str::<u16>(src).unwrap_err()
        };
        assert_eq!(e.code(), code::TYPE_MISMATCH, "{src}");
        assert!(e.to_string().starts_with("invalid type: string"), "{e}");
    }
    assert_eq!(from_str::<String>("'123'").unwrap(), "123");
}

#[test]
fn floats_never_integers_and_ranges() {
    for src in [".inf", "-.inf", ".nan", "1.0", "1e3"] {
        assert_eq!(
            from_str::<i64>(src).unwrap_err().code(),
            code::TYPE_MISMATCH,
            "{src}"
        );
    }
    assert_eq!(
        from_str::<u8>("256").unwrap_err().code(),
        code::INTEGER_OUT_OF_RANGE
    );
    assert_eq!(
        from_str::<u64>("-1").unwrap_err().code(),
        code::INTEGER_OUT_OF_RANGE
    );
    assert_eq!(from_str::<i8>("-128").unwrap(), -128);
    assert_eq!(from_str::<u128>(&u128::MAX.to_string()).unwrap(), u128::MAX);
    assert_eq!(from_str::<i128>(&i128::MIN.to_string()).unwrap(), i128::MIN);
    let wide = "1".repeat(50);
    assert_eq!(
        from_str::<u128>(&wide).unwrap_err().code(),
        code::INTEGER_OUT_OF_RANGE
    );
    assert!(
        from_str::<Value>(&wide)
            .unwrap()
            .as_number()
            .unwrap()
            .is_big_integer()
    );
    assert_eq!(from_str::<f64>("3").unwrap(), 3.0);
    assert!(from_str::<f64>(".nan").unwrap().is_nan());
    assert_eq!(from_str::<f32>("0.5").unwrap(), 0.5);
    assert_eq!(
        from_str::<f64>(&wide).unwrap_err().code(),
        code::INTEGER_OUT_OF_RANGE
    );
    assert_eq!(
        from_str::<f64>("abc").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
}

#[test]
fn bools_only_from_the_schema() {
    assert!(from_str::<bool>("true").unwrap());
    assert!(!from_str::<bool>("False").unwrap());
    assert_eq!(
        from_str::<bool>("yes").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    assert_eq!(
        from_str::<bool>("1").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    let y11 = Settings::new().yaml11_booleans(true);
    assert!(from_str_with::<bool>("yes", &y11).unwrap());
    assert!(!from_str_with::<bool>("off", &Compat::yaml_1_1()).unwrap());
}

#[test]
fn options_units_and_chars() {
    assert_eq!(from_str::<Option<u8>>("~").unwrap(), None);
    assert_eq!(from_str::<Option<u8>>("").unwrap(), None);
    assert_eq!(from_str::<Option<u8>>("3").unwrap(), Some(3));
    assert_eq!(
        from_str::<Option<String>>("''").unwrap(),
        Some(String::new())
    );
    assert_eq!(from_str::<Option<Vec<u8>>>("[1]").unwrap(), Some(vec![1]));
    from_str::<()>("null").unwrap();
    assert_eq!(from_str::<()>("1").unwrap_err().code(), code::TYPE_MISMATCH);
    assert_eq!(from_str::<char>("x").unwrap(), 'x');
    assert_eq!(
        from_str::<char>("xy").unwrap_err().code(),
        code::INVALID_VALUE
    );
    assert_eq!(from_str::<(u8, String)>("[1, a]").unwrap(), (1, "a".into()));
    assert_eq!(
        from_str::<(u8, u8)>("[1, 2, 3]").unwrap_err().code(),
        code::INVALID_LENGTH
    );
    assert_eq!(
        from_str::<(u8, u8, u8)>("[1, 2]").unwrap_err().code(),
        code::INVALID_LENGTH
    );
    assert_eq!(
        from_str::<Vec<u8>>("{a: 1}").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    assert_eq!(
        from_str::<HashMap<String, u8>>("[1]").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    assert_eq!(
        from_str::<String>("[1]").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
}

#[test]
fn borrowing_deserialization() {
    #[derive(Deserialize)]
    struct B<'a> {
        name: &'a str,
        #[serde(borrow)]
        tags: Vec<&'a str>,
    }
    let src = String::from("name: web\ntags: [a, 'b']\n");
    let b: B<'_> = from_str(&src).unwrap();
    assert_eq!(b.name, "web");
    let range = src.as_ptr() as usize..src.as_ptr() as usize + src.len();
    assert!(
        range.contains(&(b.name.as_ptr() as usize)),
        "borrowed, not copied"
    );
    assert_eq!(b.tags, ["a", "b"]);
    // A value that needs decoding cannot be borrowed: a clear error.
    let e = from_str::<&str>("\"a\\tb\"").unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert_eq!(from_str::<String>("\"a\\tb\"").unwrap(), "a\tb");
}

#[derive(Debug, Deserialize, PartialEq)]
enum Shape {
    Unit,
    Circle(f64),
    Point(i32, i32),
    Rect { w: u32, h: u32 },
}

#[test]
fn enums_all_representations() {
    assert_eq!(from_str::<Shape>("Unit").unwrap(), Shape::Unit);
    assert_eq!(from_str::<Shape>("{Unit: ~}").unwrap(), Shape::Unit);
    assert_eq!(
        from_str::<Shape>("Circle: 1.5").unwrap(),
        Shape::Circle(1.5)
    );
    assert_eq!(
        from_str::<Shape>("Point: [1, 2]").unwrap(),
        Shape::Point(1, 2)
    );
    assert_eq!(
        from_str::<Shape>("Rect: {w: 1, h: 2}").unwrap(),
        Shape::Rect { w: 1, h: 2 }
    );
    // YAML tags name the variant.
    assert_eq!(from_str::<Shape>("!Unit").unwrap(), Shape::Unit);
    assert_eq!(
        from_str::<Shape>("!Point [3, 4]").unwrap(),
        Shape::Point(3, 4)
    );
    assert_eq!(
        from_str::<Shape>("!Rect {w: 5, h: 6}").unwrap(),
        Shape::Rect { w: 5, h: 6 }
    );
    let e = from_str::<Shape>("Hexagon").unwrap_err();
    assert_eq!(e.code(), code::UNKNOWN_VARIANT);
    assert!(e.to_string().contains("expected one of `Unit`"), "{e}");
    assert_eq!(
        from_str::<Shape>("!Nope 1").unwrap_err().code(),
        code::UNKNOWN_VARIANT
    );
    assert_eq!(
        from_str::<Shape>("{}").unwrap_err().code(),
        code::INVALID_LENGTH
    );
    assert_eq!(
        from_str::<Shape>("{Unit: ~, Circle: 1}")
            .unwrap_err()
            .code(),
        code::INVALID_LENGTH
    );
    assert_eq!(
        from_str::<Shape>("[Unit]").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    // Custom-tagged scalar content is a string (never re-resolved).
    #[derive(Debug, Deserialize, PartialEq)]
    enum Ref {
        Ref(String),
        Num(u16),
    }
    assert_eq!(from_str::<Ref>("!Ref 12").unwrap(), Ref::Ref("12".into()));
    assert_eq!(
        from_str::<Ref>("!Num 12").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    // Tagged variants with struct content from a scalar are an error.
    assert_eq!(
        from_str::<Shape>("!Rect x").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    assert_eq!(
        from_str::<Shape>("!Unit x").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    assert_eq!(
        from_str::<Shape>("!Point x").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    // Tagged collections are enums, not plain collections.
    assert_eq!(
        from_str::<Vec<u8>>("!x [1]").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
}

#[test]
fn internally_and_adjacently_tagged_and_untagged() {
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(tag = "type")]
    enum Internal {
        A { x: u8 },
        B,
    }
    assert_eq!(
        from_str::<Internal>("type: A\nx: 3").unwrap(),
        Internal::A { x: 3 }
    );
    assert_eq!(from_str::<Internal>("{type: B}").unwrap(), Internal::B);
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(tag = "t", content = "c")]
    enum Adjacent {
        N(u8),
    }
    assert_eq!(
        from_str::<Adjacent>("{t: N, c: 7}").unwrap(),
        Adjacent::N(7)
    );
    // A tested requirement: untagged enums via deserialize_any.
    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(untagged)]
    enum Untagged {
        Int(i64),
        Float(f64),
        Bool(bool),
        Text(String),
        List(Vec<Untagged>),
        Map(BTreeMap<String, Untagged>),
        Null(()),
    }
    let v: Untagged = from_str("[1, 1.5, true, x, '2', ~, {k: [3]}]").unwrap();
    assert_eq!(
        v,
        Untagged::List(vec![
            Untagged::Int(1),
            Untagged::Float(1.5),
            Untagged::Bool(true),
            Untagged::Text("x".into()),
            Untagged::Text("2".into()),
            Untagged::Null(()),
            Untagged::Map([("k".into(), Untagged::List(vec![Untagged::Int(3)]))].into()),
        ])
    );
}

#[test]
fn flatten_is_supported() {
    // A tested requirement.
    #[derive(Debug, Deserialize, PartialEq)]
    struct Meta {
        name: String,
        replicas: u32,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    struct Obj {
        kind: String,
        #[serde(flatten)]
        meta: Meta,
        #[serde(flatten)]
        rest: BTreeMap<String, Value>,
    }
    let o: Obj = from_str("kind: Deployment\nname: web\nreplicas: 3\nextra: [1, a]\n").unwrap();
    assert_eq!(
        o.meta,
        Meta {
            name: "web".into(),
            replicas: 3
        }
    );
    assert_eq!(
        o.rest["extra"],
        Value::from(vec![Value::from(1), Value::from("a")])
    );
    // Quoted numbers stay strings through flatten's buffering too.
    let e = from_str::<Obj>("kind: D\nname: w\nreplicas: '3'\n").unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
}

#[test]
fn deserialize_any_and_ignored() {
    #[derive(Debug, Deserialize)]
    struct Small {
        a: u8,
    }
    // Unknown fields are skipped (iteratively), whatever their shape.
    let s: Small = from_str("x: {y: [1, {z: 2}]}\na: 1\nw: !t [x]\n").unwrap();
    assert_eq!(s.a, 1);
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Strict {
        #[allow(dead_code)]
        a: u8,
    }
    assert_eq!(
        from_str::<Strict>("a: 1\nb: 2").unwrap_err().code(),
        code::UNKNOWN_FIELD
    );
    let j: serde_json::Value = from_str("{a: [1, -2, 1.5, true, ~, x]}").unwrap();
    assert_eq!(j, serde_json::json!({"a": [1, -2, 1.5, true, null, "x"]}));
    // An integer too wide for any Rust type.
    let wide = "9".repeat(60);
    assert_eq!(
        from_str::<serde_json::Value>(&wide).unwrap_err().code(),
        code::INTEGER_OUT_OF_RANGE
    );
    // i128/u128 through deserialize_any (serde's untagged buffering has no
    // 128-bit integers, so a direct visitor checks them).
    struct Wide(String);
    impl<'de> Deserialize<'de> for Wide {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl serde::de::Visitor<'_> for V {
                type Value = Wide;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("an integer")
                }
                fn visit_i128<E>(self, v: i128) -> Result<Wide, E> {
                    Ok(Wide(format!("i128 {v}")))
                }
                fn visit_u128<E>(self, v: u128) -> Result<Wide, E> {
                    Ok(Wide(format!("u128 {v}")))
                }
                fn visit_i64<E>(self, v: i64) -> Result<Wide, E> {
                    Ok(Wide(format!("i64 {v}")))
                }
                fn visit_u64<E>(self, v: u64) -> Result<Wide, E> {
                    Ok(Wide(format!("u64 {v}")))
                }
            }
            d.deserialize_any(V)
        }
    }
    assert_eq!(
        from_str::<Wide>(&i128::MIN.to_string()).unwrap().0,
        format!("i128 {}", i128::MIN)
    );
    assert_eq!(
        from_str::<Wide>(&u128::MAX.to_string()).unwrap().0,
        format!("u128 {}", u128::MAX)
    );
    assert_eq!(from_str::<Wide>("-5").unwrap().0, "i64 -5");
    assert_eq!(from_str::<Wide>("5").unwrap().0, "u64 5");
}

#[test]
fn spanned_values() {
    #[derive(Deserialize)]
    struct S {
        a: Spanned<u16>,
        b: Spanned<Vec<u8>>,
        c: Spanned<Value>,
        m: HashMap<Spanned<String>, u8>,
    }
    let src = "a: 80\nb: [1, 2]\nc: {x: 1}\nm: {k: 1}\n";
    let s: S = from_str(src).unwrap();
    let text = |sp: oneyaml::Span| &src[sp.start..sp.end];
    assert_eq!(*s.a, 80);
    assert_eq!(text(s.a.span()), "80");
    assert_eq!(text(s.b.span()), "[1, 2]");
    assert_eq!(text(s.c.span()), "{x: 1}");
    let (k, _) = s.m.iter().next().unwrap();
    assert_eq!(text(k.span()), "k");
    assert_eq!(s.a.clone().into_inner(), 80);
    assert!(format!("{:?}", s.a).starts_with("Spanned(3..5"));
    // Other formats cannot provide spans.
    assert!(serde_json::from_str::<Spanned<u8>>("1").is_err());
}

#[test]
fn empty_input_and_multiple_documents() {
    assert_eq!(from_str::<Value>("").unwrap(), Value::Null);
    assert_eq!(
        from_str::<Option<Port>>("# only a comment\n").unwrap(),
        None
    );
    let e = from_str::<Value>("a\n---\nb\n").unwrap_err();
    assert_eq!(e.code(), code::MULTIPLE_DOCUMENTS);
    assert_eq!(e.location().unwrap().line(), 2);
    assert_eq!(from_str::<u8>("--- 1\n...\n").unwrap(), 1);
}

#[test]
fn syntax_and_limit_errors_surface_with_codes() {
    let e = from_str::<Value>("a: [1\n").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Syntax);
    assert!(e.location().is_some());
    let deep = format!("{}{}", "[".repeat(20), "]".repeat(20));
    let e = from_str_with::<Value>(&deep, &Settings::new().max_depth(10)).unwrap_err();
    assert_eq!(e.code(), code::DEPTH_LIMIT_EXCEEDED);
    assert_eq!(e.kind(), ErrorKind::Limit);
    assert!(
        e.diagnostic()
            .help
            .as_deref()
            .unwrap()
            .contains("max_depth")
    );
}

#[test]
fn custom_and_duplicate_field_codes() {
    // A target's own error (here `IpAddr`'s parser) is Y3004.
    let e = from_str::<std::net::IpAddr>("not-an-ip").unwrap_err();
    assert_eq!(e.code(), code::CUSTOM);
    assert_eq!(e.location().unwrap().column(), 1);
    // Two different keys naming one field.
    #[derive(Debug, Deserialize)]
    struct A {
        #[serde(alias = "b")]
        #[allow(dead_code)]
        a: u8,
    }
    assert_eq!(
        from_str::<A>("{a: 1, b: 2}").unwrap_err().code(),
        code::DUPLICATE_FIELD
    );
}
