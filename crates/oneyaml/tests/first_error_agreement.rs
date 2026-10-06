//! Every consumer reports the same first error at
//! the same place, and a stream's diagnostics each belong to
//! exactly one document.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::{BTreeMap, HashMap};

use oneyaml::{Document, Error, Settings, Stream, Value, code, from_str};
use serde::de::IgnoredAny;

/// (code, primary start, path, secondary starts) of an error.
fn shape(e: &Error) -> (oneyaml::Code, usize, String, Vec<usize>) {
    let d = e.diagnostic();
    (
        e.code(),
        d.primary.span.start,
        e.path().map(ToString::to_string).unwrap_or_default(),
        d.secondary.iter().map(|l| l.span.start).collect(),
    )
}

/// Every consumer's error for `src`: `Value`, `IgnoredAny`, typed maps,
/// strict `Document`, and `Document::to_value` of the recovered tree.
fn every_consumer(src: &str) -> Vec<(&'static str, Error)> {
    let doc = Document::parse_recovering(src).into_value();
    vec![
        ("Value", from_str::<Value>(src).unwrap_err()),
        ("IgnoredAny", from_str::<IgnoredAny>(src).unwrap_err()),
        (
            "HashMap<Value, Value>",
            from_str::<HashMap<Value, Value>>(src).unwrap_err(),
        ),
        (
            "BTreeMap<String, IgnoredAny>",
            from_str::<BTreeMap<String, IgnoredAny>>(src).unwrap_err(),
        ),
        ("Document::parse", Document::parse(src).unwrap_err()),
        ("Document::to_value", doc.to_value().unwrap_err()),
    ]
}

#[test]
fn typed_map_complex_key_reports_the_meaning_error_first() {
    // Major 2 (fuzz #29032): the complex key `{a: !!map f: b ...` holds a
    // meaning error (3:2) before a syntax error (3:3). Typed maps used to
    // buffer the whole key first and so met the syntax error first.
    let src = "x: 1\n{a: !!map\n f: b\n";
    for (who, e) in every_consumer(src) {
        assert_eq!(e.code(), code::COLLECTION_TAG_ON_SCALAR, "{who}: {e}");
        let at = e.location().unwrap();
        assert_eq!((at.line(), at.column()), (3, 2), "{who}: {e}");
    }
}

#[test]
fn opaque_key_identities_read_as_the_alias() {
    // Minor 4: strict checking never expands `*n` unboundedly; its key
    // identity must not leak the internal tag into paths or messages.
    // Within the copy budget an alias key means its anchor's
    // value (`Anchored::full`), so the path is the one serde shows
    // (`top[40].*n.k` only past the budget: `same_meaning.rs`,
    // `past_the_copy_budget_an_alias_key_stays_opaque`).
    for (src, want) in [
        (
            "a: &n [1]\ntop: {? [*n] : {k: !!int x}}",
            "top.\"Sequence [Sequence [Number(1)]]\".k",
        ),
        (
            "a: &n [1]\ntop: {? *n : {k: !!int x}}",
            "top.\"Sequence [Number(1)]\".k",
        ),
        (
            "a: &n {b: 1}\ntop: {? *n : {k: !!int x}}",
            "top.\"Mapping {String(\\\"b\\\"): Number(1)}\".k",
        ),
    ] {
        let e = Document::parse(src).unwrap_err();
        assert_eq!(e.code(), code::INVALID_SCALAR_FOR_TAG, "{src:?}");
        let path = e.path().unwrap().to_string();
        assert_eq!(path, want, "{src:?}");
        let serde_path = from_str::<Value>(src)
            .unwrap_err()
            .path()
            .unwrap()
            .to_string();
        assert_eq!(path, serde_path, "{src:?}");
        assert!(!e.to_string().contains("oneyaml.invalid"), "{e}");
        // serde expands the alias: same code, same place.
        let serde = from_str::<Value>(src).unwrap_err();
        assert_eq!(serde.code(), e.code());
        assert_eq!(serde.diagnostic().primary.span, e.diagnostic().primary.span);
    }
}

#[test]
fn a_duplicate_through_an_alias_is_reported_at_the_alias() {
    // Minor 5: the duplicate is the alias `*n0` (1:25); the first
    // definition is the anchored `x` (1:16), as a secondary label.
    let src = "top: {? {? &n0 x : 1, ? *n0 : 2} : 3}";
    for (who, e) in every_consumer(src) {
        // `IgnoredAny` reports no path at all (a follow-up, not this fix).
        let path = if who == "IgnoredAny" { "" } else { "top" };
        assert_eq!(
            shape(&e),
            (code::DUPLICATE_KEY, 24, path.to_owned(), vec![15]),
            "{who}: {e}"
        );
    }
    // A scalar alias as a typed value: its type error points at the
    // alias (1:14), not at the anchor.
    #[derive(Debug, serde::Deserialize)]
    #[allow(dead_code)]
    struct T {
        a: String,
        b: u32,
    }
    let e = from_str::<T>("a: &x foo\nb: *x\n").unwrap_err();
    assert_eq!(e.diagnostic().primary.span.start, 13, "{e}");
}

#[test]
fn trailing_diagnostics_belong_to_the_last_document() {
    // Minor 7: W4TN truncated; the malformed `%YAML` after the last
    // document belongs to it.
    let src = "%YAML 1.2\n--- |\n%!PS-Adobe-2.0\n...\n%YAML";
    let st = Stream::parse_recovering(src).into_value();
    assert!(st.has_errors());
    assert_eq!(st.len(), 1);
    let last = st.document(0).unwrap();
    assert!(last.has_errors());
    assert_eq!(last.diagnostics(), st.diagnostics());
    // Between documents: the next one's.
    let src = "a\n...\n%YAML\n---\nb\n";
    let st = Stream::parse_recovering(src).into_value();
    assert!(st.has_errors());
    let owners: Vec<bool> = st.documents().map(|d| d.has_errors()).collect();
    assert_eq!(owners.iter().filter(|o| **o).count(), 1, "{owners:?}");
    assert!(!owners[0], "{owners:?}");
    // No documents: only the stream has them.
    let st = Stream::parse_recovering("%YAML").into_value();
    assert!(st.is_empty() || st.documents().any(|d| d.has_errors()));
    assert!(st.has_errors());
    assert!(!Stream::parse("a\n--- b\n").unwrap().has_errors());
}

#[test]
fn every_located_diagnostic_has_exactly_one_owner() {
    // Minor 7: decoding diagnostics are located, so each belongs to the
    // document holding it, not to every document.
    let bytes = b"a: \xff\n--- b\n--- c: \xfe\n...\n# \x80\n";
    let st = Stream::from_slice_recovering(bytes, &Settings::new()).into_value();
    assert_eq!(st.len(), 3);
    let per_doc: Vec<usize> = st
        .documents()
        .map(|d| {
            d.diagnostics()
                .iter()
                .filter(|x| x.code == code::INVALID_UTF8)
                .count()
        })
        .collect();
    assert_eq!(per_doc, [1, 0, 2]);
    for d in &st.diagnostics() {
        let owners = st
            .documents()
            .filter(|doc| doc.diagnostics().contains(d))
            .count();
        assert_eq!(owners, 1, "{d:?}");
    }
    // A document's value reports its own decoding errors only.
    assert!(st.document(0).unwrap().to_value().is_err());
    assert_eq!(
        st.document(1).unwrap().to_value().unwrap(),
        Value::from("b")
    );
    // Settings diagnostics describe every document's settings: each lists
    // them.
    let s = Settings::new()
        .schema(oneyaml::Schema::Json)
        .yaml11_int_forms(true);
    let st = Stream::parse_recovering_with("1\n--- 2\n", &s).into_value();
    assert_eq!(st.diagnostics().len(), 1);
    assert_eq!(st.diagnostics()[0].code, code::MEANINGLESS_SETTINGS);
    for d in st.documents() {
        assert_eq!(d.diagnostics(), st.diagnostics());
    }
}
