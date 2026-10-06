//! Content the target type does not consume is still resolved and
//! checked (duplicate keys, tags, merge keys), so a typed read rejects
//! exactly what `from_str::<Value>` rejects, whatever the target ignores.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{
    Compat, Deserializer, DuplicateKeys, Schema, Settings, Value, code, from_str, from_str_with,
    from_str_with_diagnostics,
};
use serde::Deserialize;
use serde::de::IgnoredAny;

#[derive(Debug, Deserialize)]
struct Known {
    #[allow(dead_code)]
    known: i32,
}

#[test]
fn unknown_fields_are_checked() {
    // Three known cases: each fails as the `Value` path does.
    let cases = [
        ("known: 1\nextra: {a: 1, a: 2}\n", code::DUPLICATE_KEY),
        (
            "known: 1\nextra: !!int twelve\n",
            code::INVALID_SCALAR_FOR_TAG,
        ),
        (
            "known: 1\nextra: [!!map x]\n",
            code::COLLECTION_TAG_ON_SCALAR,
        ),
    ];
    for (src, c) in cases {
        assert_eq!(from_str::<Value>(src).unwrap_err().code(), c, "{src}");
        let e = from_str::<Known>(src).unwrap_err();
        assert_eq!(e.code(), c, "typed {src}");
        assert_eq!(
            e.path().unwrap().to_string().split('.').next(),
            Some("extra")
        );
    }
    assert_eq!(code::DUPLICATE_KEY.to_string(), "Y1013");
    assert_eq!(code::INVALID_SCALAR_FOR_TAG.to_string(), "Y1007");
    assert_eq!(code::COLLECTION_TAG_ON_SCALAR.to_string(), "Y1008");
    // Valid ignored content still reads.
    let k: Known = from_str("known: 1\nextra: {a: [1, {b: !x 2}], ? [c]: d}\n").unwrap();
    assert_eq!(k.known, 1);
}

/// Inline (flow) nodes: valid ones, and invalid ones under some settings.
const NODES: &[&str] = &[
    "1",
    "hello",
    "'q'",
    "{a: 1}",
    "[a, {b: c}]",
    "!custom {a: 1}",
    "{? [a]: 1}",
    "[&x 1, *x]",
    "{<<: {a: 1}, b: 2}",
    "{<<: [{a: 1}, {a: 2, c: 3}], a: 0}",
    "{a: 1, a: 2}",
    "{a: {b: 1, b: 2}}",
    "!!int twelve",
    "[!!map x]",
    "!!str [a]",
    "{[a]: 1, [a]: 2}",
    "{? {a: 1, a: 2}: x}",
    "{? {a: 1, b: 2}: x, ? {b: 2, a: 1}: y}",
    "{<<: 1}",
    "{<<: [{a: 1}, 2]}",
    "{<<: !t {a: 1}}",
    "{<<: {a: 1}, <<: {b: 1}}",
    "{x: {<<: {<<: {a: 1, a: 2}}}}",
    "[&a {k: 1}, {<<: *a, k: 2}]",
];

fn settings() -> Vec<(&'static str, Settings)> {
    vec![
        ("default", Settings::new()),
        ("merge", Settings::new().merge_keys(true)),
        (
            "last-wins",
            Settings::new().duplicate_keys(DuplicateKeys::LastWins),
        ),
        ("serde_yaml", Compat::serde_yaml()),
        ("json", Settings::new().schema(Schema::Json)),
    ]
}

/// The `Value` path's verdict: `None` when it accepts.
fn verdict(src: &str, s: &Settings) -> Option<oneyaml::Code> {
    from_str_with::<Value>(src, s).err().map(|e| e.code())
}

#[test]
fn ignoring_targets_agree_with_value() {
    for (name, s) in settings() {
        for node in NODES {
            let ctx = format!("{name}: {node}");
            // An unknown struct field.
            let src = format!("known: 1\nextra: {node}\n");
            let want = verdict(&src, &s);
            let got = from_str_with::<Known>(&src, &s).err().map(|e| e.code());
            assert_eq!(got, want, "unknown field, {ctx}");
            // `IgnoredAny` for the whole document.
            let want = verdict(node, &s);
            let got = from_str_with::<IgnoredAny>(node, &s)
                .err()
                .map(|e| e.code());
            assert_eq!(got, want, "IgnoredAny, {ctx}");
            // A `Value` field next to ignored ones sees the same verdict.
            #[derive(Deserialize)]
            struct WithValue {
                #[allow(dead_code)]
                v: Value,
            }
            let src = format!("v: 1\nextra: {node}\n");
            let got = from_str_with::<WithValue>(&src, &s).err().map(|e| e.code());
            assert_eq!(got, verdict(&src, &s), "Value field, {ctx}");
            // The rest of a fixed-size tuple (read early, then skipped): an
            // invalid rest fails with its own code before the length
            // error.
            let src = format!("[1, {node}]");
            let got = from_str_with::<(i32,)>(&src, &s).unwrap_err().code();
            let want = verdict(&src, &s).unwrap_or(code::INVALID_LENGTH);
            assert_eq!(got, want, "tuple rest, {ctx}");
            // A map stopped early by the target (a struct in a planned or
            // streamed mapping) is covered by the unknown field; an unread
            // document is skipped with the same checks.
            let src = format!("{node}\n--- 5\n");
            let first = verdict(&format!("{node}\n"), &s);
            let mut it = Deserializer::from_str_with(&src, &s);
            let _unread = it.next().unwrap();
            let second = it.next().unwrap();
            let got = u8::deserialize(second).err().map(|e| e.code());
            assert_eq!(got, first, "unread document, {ctx}");
        }
    }
}

#[test]
fn ignored_duplicates_warn_under_last_wins() {
    // Tolerated duplicates in ignored content still record their warning,
    // as the `Value` path does.
    let src = "known: 1\nextra: {a: 1, a: 2}\n";
    let s = Settings::new().duplicate_keys(DuplicateKeys::LastWins);
    let (_, w): (Known, _) = from_str_with_diagnostics(src, &s).unwrap();
    let (_, wv): (Value, _) = from_str_with_diagnostics(src, &s).unwrap();
    assert_eq!(w.len(), 1);
    assert_eq!(w, wv);
    assert_eq!(w[0].code, code::DUPLICATE_KEY_LAST_WINS);
}
