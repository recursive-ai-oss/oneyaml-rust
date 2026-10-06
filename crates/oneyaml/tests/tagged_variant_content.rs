//! `Settings::resolve_tagged_variant_content` (an opt-in
//! setting, off by default).
//!
//! `serde_yaml` writes a newtype variant around a number, bool, or null as
//! a tagged scalar (`!Square 2.0`). Under the setting (on in
//! `Compat::serde_yaml()` only), a typed enum reads a locally tagged plain
//! scalar's content resolved under Core, and `Layout::SerdeYaml` writes
//! such variants that way. `Value`, untyped reads, quoted content, and the
//! default settings are unchanged.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{
    Compat, Document, Emit, Layout, Settings, TaggedValue, Value, code, from_str, from_str_with,
    from_value, from_value_with, path, to_string, to_string_with,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Shape {
    Unit,
    Circle { r: f64 },
    Square(f64),
    Pair(i32, i32),
    Named(String),
    Enabled(bool),
    Count(u32),
    Maybe(Option<u8>),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Outer {
    Wrap(Shape),
    Many(Vec<Shape>),
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Drawing {
    shapes: Vec<Shape>,
    #[serde(default)]
    extra: Option<Shape>,
}

fn on() -> Settings {
    Settings::new().resolve_tagged_variant_content(true)
}

fn tagged(tag: &str, text: &str) -> Value {
    Value::Tagged(Box::new(TaggedValue {
        tag: oneyaml::Tag::new(tag),
        value: Value::from(text),
    }))
}

#[test]
fn the_setting_is_off_by_default_and_on_in_the_serde_yaml_preset_only() {
    assert!(!Settings::new().get_resolve_tagged_variant_content());
    assert!(Compat::serde_yaml().get_resolve_tagged_variant_content());
    assert!(!Compat::yaml_1_1().get_resolve_tagged_variant_content());
}

#[test]
fn typed_enums_resolve_a_tagged_plain_scalar_under_core() {
    for s in [on(), Compat::serde_yaml()] {
        let read = |src: &str| from_str_with::<Shape>(src, &s).unwrap();
        assert_eq!(read("!Square 2.0"), Shape::Square(2.0));
        assert_eq!(read("!Square 2"), Shape::Square(2.0));
        assert_eq!(read("!Square .inf"), Shape::Square(f64::INFINITY));
        assert_eq!(read("!Enabled true"), Shape::Enabled(true));
        assert_eq!(read("!Count 0x10"), Shape::Count(16));
        assert_eq!(read("!Maybe ~"), Shape::Maybe(None));
        assert_eq!(read("!Maybe null"), Shape::Maybe(None));
        assert_eq!(read("!Maybe 7"), Shape::Maybe(Some(7)));
        assert_eq!(read("!Named hello"), Shape::Named("hello".into()));
        // A plain scalar into a string target is its text.
        assert_eq!(read("!Named 123"), Shape::Named("123".into()));
        assert_eq!(read("!Named 1.10"), Shape::Named("1.10".into()));
        // The other forms are unchanged.
        assert_eq!(read("!Circle {r: 1.5}"), Shape::Circle { r: 1.5 });
        assert_eq!(read("!Pair [1, 2]"), Shape::Pair(1, 2));
        assert_eq!(read("Square: 2.0"), Shape::Square(2.0));
        assert_eq!(read("Unit"), Shape::Unit);
    }
    // The document's 1.1 settings apply to the content, as to an untagged
    // plain scalar (`Compat::serde_yaml()` has `yaml11_int_forms`).
    let compat = Compat::serde_yaml();
    assert_eq!(
        from_str_with::<Shape>("!Count 0b101", &compat).unwrap(),
        Shape::Count(5)
    );
    assert_eq!(
        from_str_with::<Shape>("!Count 0b101", &on())
            .unwrap_err()
            .code(),
        code::TYPE_MISMATCH
    );
}

#[test]
fn errors_in_resolved_content_keep_their_codes_and_spans() {
    let e = from_str_with::<Shape>("- !Count 70000000000\n", &on()).unwrap_err();
    // (A sequence is not a `Shape`; read the element.)
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    let e = from_str_with::<Vec<Shape>>("- !Count 70000000000\n", &on()).unwrap_err();
    assert_eq!(e.code(), code::INTEGER_OUT_OF_RANGE);
    // The content's own span (after `- !Count `).
    assert_eq!(e.span().map(|s| s.start), Some(9));
    let e = from_str_with::<Vec<Shape>>("- !Enabled 1\n", &on()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    // A unit variant still takes no content.
    let e = from_str_with::<Shape>("!Unit 1", &on()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert_eq!(from_str_with::<Shape>("!Unit", &on()).unwrap(), Shape::Unit);
}

#[test]
fn quoted_content_stays_a_string() {
    for src in ["!Square '2.0'", "!Square \"2.0\"", "!Square |\n  2.0\n"] {
        let e = from_str_with::<Shape>(src, &on()).unwrap_err();
        assert_eq!(e.code(), code::TYPE_MISMATCH, "{src}");
    }
    assert_eq!(
        from_str_with::<Shape>("!Named '2.0'", &on()).unwrap(),
        Shape::Named("2.0".into())
    );
}

#[test]
fn only_local_tags_name_a_resolved_variant() {
    // A verbatim local tag is local; global and `%TAG`-expanded tags are
    // not, and their content stays a string.
    assert_eq!(
        from_str_with::<Shape>("!<!Square> 2.0", &on()).unwrap(),
        Shape::Square(2.0)
    );
    for src in [
        "!<tag:example.com,2000:Square> 2.0",
        "%TAG !e! tag:example.com,2000:\n--- !e!Square 2.0\n",
        "%TAG ! tag:example.com,2000:\n--- !Square 2.0\n",
    ] {
        // No variant is named by a global tag (Y3003), and its content
        // stays a string.
        assert!(from_str_with::<Shape>(src, &on()).is_err(), "{src}");
        let v: Value = from_str_with(src, &on()).unwrap();
        assert_eq!(v.untag_ref(), &Value::from("2.0"), "{src}");
    }
}

#[test]
fn with_the_setting_off_nothing_changes() {
    for s in [
        Settings::new(),
        Compat::serde_yaml().resolve_tagged_variant_content(false),
    ] {
        let e = from_str_with::<Shape>("!Square 2.0", &s).unwrap_err();
        assert_eq!(e.code(), code::TYPE_MISMATCH);
        assert_eq!(
            from_str_with::<Shape>("!Named 123", &s).unwrap(),
            Shape::Named("123".into())
        );
        assert!(from_str_with::<Shape>("!Enabled true", &s).is_err());
    }
    assert!(from_str::<Shape>("!Square 2.0").is_err());
}

#[test]
fn values_and_untyped_reads_keep_the_tagged_string() {
    let src = "a: !Square 2.0\nb: !Enabled true\n";
    for s in [Settings::new(), on(), Compat::serde_yaml()] {
        let v: Value = from_str_with(src, &s).unwrap();
        assert_eq!(v["a"], tagged("!Square", "2.0"));
        assert_eq!(v["b"], tagged("!Enabled", "true"));
    }
    // `deserialize_any` (an untyped target) is unchanged by the setting.
    // (`serde_json::Value` refuses a tagged scalar either way.)
    let json = |s: &Settings| {
        from_str_with::<serde_json::Value>(src, s).map_err(|e| (e.code(), e.to_string()))
    };
    assert_eq!(json(&Settings::new()), json(&on()));
    let any =
        |s: &Settings| from_str_with::<std::collections::BTreeMap<String, Value>>(src, s).unwrap();
    assert_eq!(any(&Settings::new()), any(&on()));
    // The document's `Value` too.
    let doc = Document::parse_with(src, &on()).unwrap();
    assert_eq!(doc.to_value().unwrap()["a"], tagged("!Square", "2.0"));
}

#[test]
fn from_value_agrees_with_from_str_under_the_setting() {
    // The `Value` deserializer resolves the tagged string's content
    // in a typed enum under the same setting.
    let cases: &[(&str, Shape)] = &[
        ("!Square 2.0", Shape::Square(2.0)),
        ("!Enabled true", Shape::Enabled(true)),
        ("!Count 0x10", Shape::Count(16)),
        ("!Named 123", Shape::Named("123".into())),
        ("!Named hello", Shape::Named("hello".into())),
        ("!Circle {r: 1.5}", Shape::Circle { r: 1.5 }),
    ];
    for s in [on(), Compat::serde_yaml()] {
        for (src, want) in cases {
            let direct: Shape = from_str_with(src, &s).unwrap();
            let v: Value = from_str_with(src, &s).unwrap();
            let via: Shape = from_value_with(v, &s).unwrap();
            assert_eq!(&direct, want, "{src}");
            assert_eq!(&via, want, "{src}");
        }
    }
    let v = tagged("!Square", "2.0");
    assert_eq!(
        from_value_with::<Shape>(v.clone(), &on()).unwrap(),
        Shape::Square(2.0)
    );
    // Through a `Value`, content never resolves to null.
    let v_null: Value = from_str_with("!Maybe ~", &on()).unwrap();
    assert_eq!(
        from_str_with::<Shape>("!Maybe ~", &on()).unwrap(),
        Shape::Maybe(None)
    );
    assert_eq!(
        from_value_with::<Shape>(v_null, &on()).unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    // The documented difference: a `Value` keeps no style, so quoted
    // content resolves here while `from_str_with` refuses it.
    assert!(from_str_with::<Shape>("!Square '2.0'", &on()).is_err());
    let quoted: Value = from_str_with("!Square '2.0'", &on()).unwrap();
    assert_eq!(quoted, v);
    assert_eq!(
        from_value_with::<Shape>(quoted, &on()).unwrap(),
        Shape::Square(2.0)
    );
    // Off (and `from_value`, default settings): the content is a string.
    let e = from_value::<Shape>(v.clone()).unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    assert!(from_value_with::<Shape>(v.clone(), &Settings::new()).is_err());
    // A `Value` target and untyped targets keep the tagged string.
    let back: Value = from_value_with(v.clone(), &on()).unwrap();
    assert_eq!(back, v);
    // Errors of the resolved content carry their code and path.
    let e = from_value_with::<Drawing>(from_str::<Value>("shapes: [!Count -1]").unwrap(), &on())
        .unwrap_err();
    assert_eq!(e.code(), code::INTEGER_OUT_OF_RANGE);
    assert_eq!(e.span(), None);
    assert!(e.to_string().ends_with("at shapes[0]"), "{e}");
}

#[test]
fn node_reads_follow_the_documents_settings() {
    let src = "shapes:\n  - !Square 2.0\n  - !Enabled true\nextra: !Maybe 3\n";
    let doc = Document::parse_with(src, &on()).unwrap();
    let first: Shape = doc
        .get_path(&path!["shapes", 0])
        .unwrap()
        .deserialize()
        .unwrap();
    assert_eq!(first, Shape::Square(2.0));
    let whole: Drawing = doc.deserialize().unwrap();
    assert_eq!(
        whole,
        Drawing {
            shapes: vec![Shape::Square(2.0), Shape::Enabled(true)],
            extra: Some(Shape::Maybe(Some(3))),
        }
    );
    let plain = Document::parse(src).unwrap();
    assert!(
        plain
            .get_path(&path!["shapes", 0])
            .unwrap()
            .deserialize::<Shape>()
            .is_err()
    );
}

#[test]
fn nested_enums_and_options_around_the_variant() {
    let s = on();
    assert_eq!(
        from_str_with::<Outer>("Wrap: !Square 2.0", &s).unwrap(),
        Outer::Wrap(Shape::Square(2.0))
    );
    assert_eq!(
        from_str_with::<Outer>("!Many [!Square 1, !Enabled false, Unit]", &s).unwrap(),
        Outer::Many(vec![Shape::Square(1.0), Shape::Enabled(false), Shape::Unit])
    );
    assert_eq!(
        from_str_with::<Option<Shape>>("!Square 2.0", &s).unwrap(),
        Some(Shape::Square(2.0))
    );
    assert_eq!(from_str_with::<Option<Shape>>("~", &s).unwrap(), None);
    let d: Drawing = from_str_with("shapes: []\nextra: !Enabled true\n", &s).unwrap();
    assert_eq!(d.extra, Some(Shape::Enabled(true)));
    // The same through `from_value_with`.
    let v: Value = from_str_with("!Many [!Square 1, !Enabled false]", &s).unwrap();
    assert_eq!(
        from_value_with::<Outer>(v, &s).unwrap(),
        Outer::Many(vec![Shape::Square(1.0), Shape::Enabled(false)])
    );
    let v: Value = from_str_with("!Square 2.0", &s).unwrap();
    assert_eq!(
        from_value_with::<Option<Shape>>(v, &s).unwrap(),
        Some(Shape::Square(2.0))
    );
}

#[test]
fn the_serde_yaml_layout_writes_scalar_variants_as_tagged_scalars() {
    let compat = Compat::serde_yaml();
    let w = |v: &dyn erased::Ser| v.write(&compat);
    assert_eq!(w(&Shape::Square(2.0)), "!Square 2.0\n");
    assert_eq!(w(&Shape::Enabled(true)), "!Enabled true\n");
    assert_eq!(w(&Shape::Count(16)), "!Count 16\n");
    assert_eq!(w(&Shape::Maybe(None)), "!Maybe null\n");
    assert_eq!(w(&Shape::Named("x".into())), "!Named x\n");
    assert_eq!(w(&Outer::Wrap(Shape::Square(2.0))), "Wrap: !Square 2.0\n");
    let d = Drawing {
        shapes: vec![Shape::Square(2.0), Shape::Enabled(false), Shape::Unit],
        extra: Some(Shape::Maybe(Some(1))),
    };
    let text = w(&d);
    assert_eq!(
        text,
        "shapes:\n- !Square 2.0\n- !Enabled false\n- Unit\nextra: !Maybe 1\n"
    );
    // It reads back.
    assert_eq!(from_str_with::<Drawing>(&text, &compat).unwrap(), d);
    // A `Value` inside the variant is written the same way.
    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    enum Holds {
        V(Value),
    }
    let h = Holds::V(Value::from(2.5));
    let text = w(&h);
    assert_eq!(text, "!V 2.5\n");
    assert_eq!(from_str_with::<Holds>(&text, &compat).unwrap(), h);
}

#[test]
fn without_the_setting_or_the_layout_the_map_form_is_kept() {
    // The default layout never changes (goldens).
    assert_eq!(to_string(&Shape::Square(2.0)).unwrap(), "Square: 2.0\n");
    assert_eq!(
        to_string_with(&Shape::Square(2.0), &on()).unwrap(),
        "Square: 2.0\n"
    );
    // The `serde_yaml` layout read back without the setting: `!Square 2.0`
    // would not read back, so the map form stays.
    let layout_only = Compat::serde_yaml().resolve_tagged_variant_content(false);
    assert_eq!(
        to_string_with(&Shape::Square(2.0), &layout_only).unwrap(),
        "Square: 2.0\n"
    );
    let layout_only = Settings::new().emit(Emit::new().layout(Layout::SerdeYaml));
    assert_eq!(
        to_string_with(&Shape::Enabled(true), &layout_only).unwrap(),
        "Enabled: true\n"
    );
    // `to_value` builds the meaning-level map form in every case.
    assert_eq!(
        oneyaml::to_value_with(Shape::Square(2.0), &Compat::serde_yaml()).unwrap(),
        from_str::<Value>("Square: 2.0").unwrap()
    );
}

/// `to_string_with` over a trait object, for the table above.
mod erased {
    pub trait Ser {
        fn write(&self, s: &oneyaml::Settings) -> String;
    }
    impl<T: serde::Serialize> Ser for T {
        fn write(&self, s: &oneyaml::Settings) -> String {
            oneyaml::to_string_with(self, s).unwrap()
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Texts {
    Opt(Option<String>),
    Str(String),
    Maybe(Option<u8>),
}

/// In the `Value` path a tagged string's
/// content never resolves to null, so null-like text is never lost.
#[test]
fn the_value_path_never_resolves_tagged_content_to_null() {
    let s = Compat::serde_yaml();
    let via_value = |src: &str| {
        let v: Value = from_str_with(src, &s).unwrap();
        from_value_with::<Texts>(v, &s)
    };
    for (src, text) in [("!Opt 'null'", "null"), ("!Opt ''", ""), ("!Opt '~'", "~")] {
        let want = Texts::Opt(Some(text.into()));
        assert_eq!(from_str_with::<Texts>(src, &s).unwrap(), want, "{src}");
        assert_eq!(via_value(src).unwrap(), want, "{src}");
    }
    // A plain null is `None` when read from text; its `Value` is the same
    // tagged string as the quoted one, so it keeps the text there.
    assert_eq!(
        from_str_with::<Texts>("!Opt null", &s).unwrap(),
        Texts::Opt(None)
    );
    assert_eq!(
        via_value("!Opt null").unwrap(),
        Texts::Opt(Some("null".into()))
    );
    // `!Maybe ''` into `Option<u8>` is an error on both paths (never `None`).
    assert!(from_str_with::<Texts>("!Maybe ''", &s).is_err());
    assert_eq!(
        via_value("!Maybe ''").unwrap_err().code(),
        code::TYPE_MISMATCH
    );
    // Bool and number content still resolve, and string targets read text.
    assert_eq!(via_value("!Maybe 7").unwrap(), Texts::Maybe(Some(7)));
    assert_eq!(via_value("!Opt 12").unwrap(), Texts::Opt(Some("12".into())));
    assert_eq!(via_value("!Str true").unwrap(), Texts::Str("true".into()));
}

/// Text, then `Value`, then typed, over the compat serializer's own output
/// for `Option`/`String` newtype variants holding null-like strings.
#[test]
fn compat_output_round_trips_through_a_value() {
    let s = Compat::serde_yaml();
    let mut cases = Vec::new();
    for t in [
        "null", "Null", "NULL", "~", "", " ", "true", "12", "1.5", ".nan", "x",
    ] {
        cases.push(Texts::Opt(Some(t.into())));
        cases.push(Texts::Str(t.into()));
    }
    cases.push(Texts::Opt(None));
    cases.push(Texts::Maybe(Some(3)));
    for c in &cases {
        let text = to_string_with(c, &s).unwrap();
        let direct: Texts = from_str_with(&text, &s).unwrap();
        assert_eq!(&direct, c, "{text:?}");
        let v: Value = from_str_with(&text, &s).unwrap();
        let via: Texts = from_value_with(v, &s).unwrap();
        if *c == Texts::Opt(None) {
            // Written `!Opt null`; its `Value` keeps the text (never null).
            assert_eq!(via, Texts::Opt(Some("null".into())), "{text:?}");
        } else {
            assert_eq!(&via, c, "{text:?}");
        }
    }
    // As one document.
    let text = to_string_with(&cases, &s).unwrap();
    let back: Vec<Texts> = from_str_with(&text, &s).unwrap();
    assert_eq!(back, cases);
}
