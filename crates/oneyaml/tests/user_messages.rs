//! Messages a new user meets first: a YAML 1.1 boolean word read into a
//! `bool`, and parse errors whose plain `Display` must name the likely
//! cause without the rendered labels.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Applicability, Compat, Document, Settings, Value, code};
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Config {
    debug: bool,
}

#[derive(Debug, Deserialize, PartialEq)]
struct Maybe {
    debug: Option<bool>,
}

const TRUE_WORDS: [&str; 8] = ["y", "Y", "yes", "Yes", "YES", "on", "On", "ON"];
const FALSE_WORDS: [&str; 8] = ["n", "N", "no", "No", "NO", "off", "Off", "OFF"];

fn words() -> impl Iterator<Item = (&'static str, bool)> {
    TRUE_WORDS
        .iter()
        .map(|w| (*w, true))
        .chain(FALSE_WORDS.iter().map(|w| (*w, false)))
}

#[test]
fn every_yaml11_bool_word_explains_the_yaml12_reading() {
    for (word, meant) in words() {
        let src = format!("name: app\ndebug: {word}\n");
        let err = oneyaml::from_str::<Config>(&src).unwrap_err();
        assert_eq!(err.code(), code::TYPE_MISMATCH, "{word}");
        assert_eq!(
            err.to_string(),
            format!(
                "invalid type: string \"{word}\", expected a boolean; YAML 1.2 reads `{word}` as a string (its booleans are `true` and `false`) at debug (2:8)"
            )
        );
        let d = err.diagnostic();
        let help = d.help.as_deref().unwrap();
        assert!(
            help.contains(&format!("write `{meant}` instead of `{word}`")),
            "{help}"
        );
        assert!(help.contains("Settings::yaml11_booleans(true)"), "{help}");
        assert!(help.contains("Compat::yaml_1_1()"), "{help}");
        // The fix writes the 1.2 boolean in place; it needs review.
        let fix = &d.suggestions[0];
        assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
        let fixed = fix.apply(&src).unwrap();
        assert_eq!(fixed, format!("name: app\ndebug: {meant}\n"));
        assert_eq!(
            oneyaml::from_str::<Config>(&fixed).unwrap(),
            Config { debug: meant }
        );
        // The help also shows in the rendered snippet.
        assert!(err.render(&src).contains("= help: write `"), "{word}");
        // `Option<bool>` reads the same node.
        let err = oneyaml::from_str::<Maybe>(&src).unwrap_err();
        assert!(err.diagnostic().help.is_some(), "{word}");
        // The suggested settings do read it.
        for s in [Settings::new().yaml11_booleans(true), Compat::yaml_1_1()] {
            assert_eq!(
                oneyaml::from_str_with::<Config>(&src, &s).unwrap(),
                Config { debug: meant }
            );
        }
        // A per-node read of a `Document` says the same.
        let doc = Document::parse(&src).unwrap();
        let err = doc.get("debug").unwrap().deserialize::<bool>().unwrap_err();
        assert!(err.to_string().contains("YAML 1.2 reads"), "{word}");
    }
}

#[test]
fn the_hint_only_covers_plain_untagged_yaml11_words() {
    for src in [
        // Quoted: written as a string on purpose.
        "debug: 'yes'\n",
        "debug: \"off\"\n",
        // Explicitly a string.
        "debug: !!str yes\n",
        // Not a YAML 1.1 bool word.
        "debug: yEs\n",
        "debug: enabled\n",
    ] {
        let err = oneyaml::from_str::<Config>(src).unwrap_err();
        assert_eq!(err.code(), code::TYPE_MISMATCH, "{src}");
        assert!(!err.to_string().contains("YAML 1.2"), "{src}: {err}");
        let d = err.diagnostic();
        assert!(d.help.is_none() && d.suggestions.is_empty(), "{src}");
    }
    // A string target reads the word as text, as before.
    #[derive(Deserialize)]
    struct Text {
        debug: String,
    }
    assert_eq!(
        oneyaml::from_str::<Text>("debug: yes\n").unwrap().debug,
        "yes"
    );
    // A `Value` has no source presentation: no hint, same code.
    let v: Value = oneyaml::from_str("debug: yes\n").unwrap();
    let err = oneyaml::from_value::<Config>(v).unwrap_err();
    assert_eq!(err.code(), code::TYPE_MISMATCH);
    assert!(err.diagnostic().help.is_none());
}

#[test]
fn a_fix_is_offered_only_when_the_span_is_the_word() {
    // An anchor before the word: the span still covers just the word.
    let src = "debug: &d yes\n";
    let err = oneyaml::from_str::<Config>(src).unwrap_err();
    let fixed = err.diagnostic().suggestions[0].apply(src).unwrap();
    assert_eq!(fixed, "debug: &d true\n");
}

#[test]
fn a_dedented_line_inside_a_flow_collection_names_the_cause() {
    // A forgotten bracket.
    let err = Document::parse("a: [1, 2\nb: c\n").unwrap_err();
    assert_eq!(err.code(), code::INVALID_INDENTATION);
    assert_eq!(
        err.to_string(),
        "a flow sequence is still open at this line, which is not indented more than its block parent; a `]` may be missing before it at a[2] (2:1)"
    );
    let err = oneyaml::from_str::<Value>("a: {x: 1\nb: c\n").unwrap_err();
    assert_eq!(
        err.to_string(),
        "a flow mapping is still open at this line, which is not indented more than its block parent; a `}` may be missing before it at a (2:1)"
    );
    // JSON-style YAML whose closer is short of indentation: not unclosed.
    let src = "args: [\n  a,\n  b\n]\n";
    let err = Document::parse(src).unwrap_err();
    assert_eq!(
        err.to_string(),
        "the closing `]` of a flow sequence is not indented more than the sequence's block parent at args[2] (4:1)"
    );
    let fixed = err.diagnostic().suggestions[0].apply(src).unwrap();
    assert_eq!(fixed, "args: [\n  a,\n  b\n ]\n");
    assert!(Document::parse(&fixed).is_ok());
}

#[test]
fn quote_mistakes_are_named_in_the_plain_message() {
    let err = Document::parse("a: 'abc\nb: c\n").unwrap_err();
    assert_eq!(
        err.to_string(),
        "a single-quoted scalar is still open at a line that is not indented more than its parent; a closing `'` may be missing at a (1:4)"
    );
    let err = Document::parse("key: 'it's'\n").unwrap_err();
    assert_eq!(
        err.to_string(),
        "a `'` inside a single-quoted scalar ends it early (1:10)"
    );
}

#[test]
fn error_codes_debug_as_their_y_form() {
    let err = Document::parse("a: [1, 2\nb: c\n").unwrap_err();
    assert_eq!(format!("{:?}", err.code()), "Y0005");
    assert!(format!("{:?}", err.diagnostic()).contains("code: Y0005"));
}
