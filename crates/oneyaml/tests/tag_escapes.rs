//! Percent escapes in tags are read and written one to one: two spellings
//! that differ in a kept escape are two tags, a tag read from a document is
//! written back as it was read, and an enum variant named with non-ASCII
//! characters is written and read as its escaped form.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml::{
    Compat, Document, Tag, TaggedValue, Value, code, from_str, from_str_with, to_string,
};
use oneyaml::{from_value, to_string_with};
use serde::{Deserialize, Serialize};

fn tagged(tag: &str, v: Value) -> Value {
    Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new(tag),
        value: v,
    }))
}

/// `%25C3%25A9` spells the text `%C3%A9`; `%C3%A9` is the escape of a
/// non-ASCII character. They are different tags, so these keys are not
/// duplicates.
#[test]
fn an_escaped_percent_and_a_kept_escape_are_different_tags() {
    let src = "{!a%25C3%25A9 x: 1, !a%C3%A9 x: 2}\n";
    let v: Value = from_str(src).unwrap();
    assert_eq!(v.as_mapping().unwrap().len(), 2);
    let doc = Document::parse(src).unwrap();
    assert_eq!(doc.to_value().unwrap(), v);
    let a = tagged("!a%25C3%25A9", Value::from("x"));
    let b = tagged("!a%C3%A9", Value::from("x"));
    assert_ne!(a, b);
    let m = v.as_mapping().unwrap();
    assert_eq!(m.get(&a).and_then(Value::as_i64), Some(1));
    assert_eq!(m.get(&b).and_then(Value::as_i64), Some(2));
}

/// The same through a `%TAG` prefix and a verbatim tag.
#[test]
fn the_prefix_and_the_suffix_keep_a_percent_escape() {
    let one: Value = from_str("%TAG !e! tag:x,1:%25\n--- !e!41 a\n").unwrap();
    let two: Value = from_str("--- !<tag:x,1:%2541> a\n").unwrap();
    let three: Value = from_str("--- !<tag:x,1:A> a\n").unwrap();
    assert_eq!(one, two);
    assert_ne!(one, three);
    // The escape is kept as written, not read as `%41`.
    let Value::Tagged(t) = &one else {
        panic!("{one:?}")
    };
    assert_eq!(t.tag.to_string(), "tag:x,1:%2541");
}

/// A tag read from a document is written as it was read, and reads back as
/// the same tag.
#[test]
fn tags_are_written_back_as_they_were_read() {
    for (src, written) in [
        (
            "%TAG !e! tag:x,1:%C3\n--- !e!%A9 a\n",
            "!<tag:x,1:%C3%A9> a\n",
        ),
        ("--- !a%25C3%25A9 a\n", "!a%25C3%25A9 a\n"),
        ("--- !a%C3%A9 a\n", "!a%C3%A9 a\n"),
        ("--- !<tag:x,1:%25> a\n", "!<tag:x,1:%25> a\n"),
        ("--- !a%41 a\n", "!aA a\n"),
    ] {
        let v: Value = from_str(src).unwrap();
        let out = to_string(&v).unwrap();
        assert_eq!(out, written, "{src:?}");
        assert_eq!(from_str::<Value>(&out).unwrap(), v, "{src:?}");
    }
}

/// A tag in memory with a `%` no reader produces (a bare one, or the escape
/// of an ASCII character, which readers decode) reads back as another tag,
/// so the writer refuses it.
#[test]
fn a_tag_with_a_percent_no_reader_produces_is_refused() {
    for tag in ["!a%", "!a%4", "!a%41", "tag:x,1:%7e", "!%zz"] {
        let v = tagged(tag, Value::from("a"));
        assert_eq!(
            to_string(&v).unwrap_err().code(),
            code::UNREPRESENTABLE_VALUE,
            "{tag:?}"
        );
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
enum Names {
    #[serde(rename = "caf\u{e9}")]
    Cafe(String),
    #[serde(rename = "50%")]
    Percent(String),
    #[serde(rename = "a%41")]
    Escapeish(String),
    Plain(String),
}

/// A variant named with non-ASCII characters is written as its escaped
/// form and read back, in the layout that writes variants as tags; a `%` in
/// a name is escaped, so a name that looks like an escape reads back as
/// itself.
#[test]
fn variant_names_with_non_ascii_characters_round_trip_as_tags() {
    let s = Compat::serde_yaml();
    for (v, written) in [
        (Names::Cafe("v".into()), "!caf%C3%A9 v\n"),
        (Names::Percent("v".into()), "!50%25 v\n"),
        (Names::Escapeish("v".into()), "!a%2541 v\n"),
        (Names::Plain("v".into()), "!Plain v\n"),
    ] {
        let out = to_string_with(&v, &s).unwrap();
        assert_eq!(out, written);
        assert_eq!(from_str_with::<Names>(&out, &s).unwrap(), v);
        let value: Value = from_str(&out).unwrap();
        // The value built from the enum is the one the text reads as.
        assert_eq!(oneyaml::to_value(&v).unwrap(), value);
        assert_eq!(from_value::<Names>(value).unwrap(), v);
        let doc = Document::parse(&out).unwrap();
        assert_eq!(doc.deserialize::<Names>().unwrap(), v);
    }
}

/// What the previous release wrote for such a variant reads back.
#[test]
fn the_escaped_form_of_a_variant_name_is_read() {
    let s = Compat::serde_yaml();
    assert_eq!(
        from_str_with::<Names>("!caf%C3%A9 v\n", &s).unwrap(),
        Names::Cafe("v".into())
    );
    // Not a variant of the enum: the error names the decoded text.
    let err = from_str_with::<Names>("!caf%C3%A8 v\n", &s).unwrap_err();
    assert!(err.to_string().contains("caf\u{e8}"), "{err}");
}
