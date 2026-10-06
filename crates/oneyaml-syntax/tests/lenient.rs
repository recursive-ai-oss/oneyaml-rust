//! `lenient_inline_colons` and the `INLINE_COLON_IN_PLAIN_VALUE`
//! diagnosis: exactly which inputs are covered, the machine-applicable
//! quoting fix, the always-recorded warning, and no effect on anything else.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::{diagnostics, events, first_code, first_error, tree};
use oneyaml_core::code::{
    BLOCK_COLLECTION_NOT_ALLOWED, INLINE_COLON_IN_PLAIN_VALUE, LENIENT_INLINE_COLON,
    MULTILINE_IMPLICIT_KEY, SCALAR_TOO_LONG, TAB_INDENTATION,
};
use oneyaml_core::{Applicability, Limits, Settings, Severity, Span};

const STRICT: Settings = Settings::new();
const LENIENT: Settings = Settings::new().lenient_inline_colons(true);

/// The error by default, its fix, and what the fixed text parses to.
fn fixed(src: &str) -> (String, Applicability) {
    let d = first_error(src, &STRICT).unwrap();
    assert_eq!(d.code, INLINE_COLON_IN_PLAIN_VALUE, "{src:?}");
    let s = d.suggestions.first().unwrap();
    (s.apply(src).unwrap(), s.applicability)
}

#[test]
fn zcz6_is_an_error_with_an_explanation_and_a_fix() {
    let src = "a: b: c: d\n";
    let d = first_error(src, &STRICT).unwrap();
    assert_eq!(d.code, INLINE_COLON_IN_PLAIN_VALUE);
    assert!(
        d.message
            .contains("`: ` inside an unquoted value starts a nested mapping")
    );
    // The secondary label points at the `: ` that would start the mapping.
    assert_eq!(d.secondary.len(), 1);
    assert_eq!(d.secondary[0].span, Span::new(4, 5));
    assert!(d.help.as_deref().unwrap().contains("quote the whole value"));
    let fix = &d.suggestions[0];
    assert_eq!(fix.applicability, Applicability::MachineApplicable);
    assert_eq!(fix.edits.len(), 1);
    assert_eq!(fix.edits[0].span, Span::new(3, 10));
    assert_eq!(fix.edits[0].text, "'b: c: d'");
    // The fixed text is valid YAML with the value the user meant.
    let out = fix.apply(src).unwrap();
    assert_eq!(out, "a: 'b: c: d'\n");
    assert_eq!(
        tree(&out, &STRICT),
        "+DOC | +MAP | =VAL :a | =VAL 'b: c: d | -MAP | -DOC"
    );
    // The default stream is unchanged apart from the code: the error sits
    // where Y0006 did and is repaired the same way.
    assert_eq!(
        tree(src, &STRICT),
        "+DOC | +MAP | =VAL :a | !Y0027 | =VAL : | -MAP | -DOC"
    );
}

#[test]
fn zcz6_is_one_string_and_a_warning_when_lenient() {
    let src = "a: b: c: d\n";
    assert_eq!(
        tree(src, &LENIENT),
        "+DOC | +MAP | =VAL :a | =VAL :b: c: d | ~Y0028 | -MAP | -DOC"
    );
    let w = diagnostics(src, &LENIENT);
    assert_eq!(w.len(), 1);
    assert_eq!(
        (w[0].code, w[0].severity),
        (LENIENT_INLINE_COLON, Severity::Warning)
    );
    assert_eq!(w[0].primary.span, Span::new(4, 5));
    // The warning offers the same fix, to make the file portable.
    let err = first_error(src, &STRICT).unwrap();
    assert_eq!(w[0].suggestions, err.suggestions);
    // The warning is zero-width and follows the scalar.
    let evs = events(src, &LENIENT);
    let at = evs.iter().position(|e| e.is_warning()).unwrap();
    assert!(evs[at].span().is_empty());
    assert_eq!(evs.get_sourced(at - 1).unwrap().text(), Some("b: c: d"));
}

#[test]
fn covered_inputs() {
    // (input, lenient tree, fixed text)
    let cases = [
        (
            "title: The farm: a banana story\n",
            "+DOC | +MAP | =VAL :title | =VAL :The farm: a banana story | ~Y0028 | -MAP | -DOC",
            "title: 'The farm: a banana story'\n",
        ),
        // Sequence-entry values: a compact mapping's value.
        (
            "- a: b: c\n",
            "+DOC | +SEQ | +MAP | =VAL :a | =VAL :b: c | ~Y0028 | -MAP | -SEQ | -DOC",
            "- a: 'b: c'\n",
        ),
        // A value after an empty key.
        (
            ": b: c\n",
            "+DOC | +MAP | =VAL : | =VAL :b: c | ~Y0028 | -MAP | -DOC",
            ": 'b: c'\n",
        ),
        // Properties on the value's line stay properties.
        (
            "a: &x !!str b: c\n",
            "+DOC | +MAP | =VAL :a | =VAL &x <tag:yaml.org,2002:str> :b: c | ~Y0028 | -MAP | -DOC",
            "a: &x !!str 'b: c'\n",
        ),
        // A trailing `:`, a tab after `:`, and a comment after the value.
        (
            "a: b:\n",
            "+DOC | +MAP | =VAL :a | =VAL :b: | ~Y0028 | -MAP | -DOC",
            "a: 'b:'\n",
        ),
        (
            "a: b:\tc\n",
            "+DOC | +MAP | =VAL :a | =VAL :b:\tc | ~Y0028 | -MAP | -DOC",
            "a: 'b:\tc'\n",
        ),
        (
            "a: b: c  # note\n",
            "+DOC | +MAP | =VAL :a | =VAL :b: c | ~Y0028 | -MAP | -DOC",
            "a: 'b: c'  # note\n",
        ),
        // Quotes inside are doubled by the fix; indicators are just text.
        (
            "a: it's: [x] - &y\n",
            "+DOC | +MAP | =VAL :a | =VAL :it's: [x] - &y | ~Y0028 | -MAP | -DOC",
            "a: 'it''s: [x] - &y'\n",
        ),
        // Continuation lines fold as for any plain scalar.
        (
            "a: b: c\n  d\ne: f\n",
            "+DOC | +MAP | =VAL :a | =VAL :b: c d | ~Y0028 | =VAL :e | =VAL :f | -MAP | -DOC",
            "a: 'b: c\n  d'\ne: f\n",
        ),
        // Nested mapping values, and several in one document.
        (
            "k:\n  a: b: c\n  d: e: f\n",
            "+DOC | +MAP | =VAL :k | +MAP | =VAL :a | =VAL :b: c | ~Y0028 | =VAL :d | =VAL :e: f | ~Y0028 | -MAP | -MAP | -DOC",
            "k:\n  a: 'b: c'\n  d: e: f\n",
        ),
    ];
    for (src, lenient, fixed_text) in cases {
        assert_eq!(tree(src, &LENIENT), lenient, "{src:?}");
        let (out, applicability) = fixed(src);
        assert_eq!(out, fixed_text, "{src:?}");
        assert_eq!(applicability, Applicability::MachineApplicable, "{src:?}");
        // Applied fixes (repeatedly, one error at a time) give the lenient
        // values, with no diagnostics at all.
        let mut text = out;
        while first_code(&text, &STRICT) == Some(INLINE_COLON_IN_PLAIN_VALUE) {
            text = fixed(&text).0;
        }
        let expect = lenient.replace(" | ~Y0028", "");
        let got = tree(&text, &STRICT).replace(" '", " :");
        assert_eq!(got, expect, "{src:?} fixed to {text:?}");
        assert!(diagnostics(&text, &STRICT).is_empty(), "{text:?}");
    }
}

#[test]
fn continuation_colons_stay_errors() {
    // Only the value's first line is lenient: a `: ` on a continuation line
    // still makes a multi-line implicit key, and the fix says it is partial.
    let src = "a: b: c\n  d: e\n";
    assert_eq!(first_code(src, &LENIENT), Some(MULTILINE_IMPLICIT_KEY));
    assert_eq!(fixed(src).1, Applicability::MaybeIncorrect);
}

#[test]
fn excluded_inputs_are_unchanged() {
    // (input, first error code by default). With the setting on, the
    // stream is identical: nothing outside a same-line plain block value.
    let cases = [
        ("--- a: b\n", Some(BLOCK_COLLECTION_NOT_ALLOWED)),
        ("--- &x a: b\n", Some(BLOCK_COLLECTION_NOT_ALLOWED)),
        ("a: 'b': c\n", Some(BLOCK_COLLECTION_NOT_ALLOWED)),
        ("a: \"b\": c\n", Some(BLOCK_COLLECTION_NOT_ALLOWED)),
        ("a: [b]: c\n", Some(BLOCK_COLLECTION_NOT_ALLOWED)),
        ("a: - b\n", Some(BLOCK_COLLECTION_NOT_ALLOWED)),
        ("a:\n\tb: c\n", Some(TAB_INDENTATION)),
        ("-\tb: c\n", Some(TAB_INDENTATION)),
        ("a: b\n  c: d\n", Some(MULTILINE_IMPLICIT_KEY)),
        ("{a: b: c}\n", first_code("{a: b: c}\n", &STRICT)),
        ("a: [b: c: d]\n", first_code("a: [b: c: d]\n", &STRICT)),
        ("[a, b: c: d]\n", first_code("[a, b: c: d]\n", &STRICT)),
        // Valid input with colons: unaffected.
        ("a: b:c\n", None),
        ("a: 'b: c'\n", None),
        ("a:\n  b: c\n", None),
        ("a: &x\n  b: c\n", None),
        ("- a: b\n", None),
        ("? a: b\n: c: d\n", None),
        ("a: http://x.y/z\n", None),
        ("a: b #: c\n", None),
        ("[a: b]\n", None),
        ("{a: b}\n", None),
        ("a: b\n", None),
    ];
    for (src, code) in cases {
        assert_eq!(first_code(src, &STRICT), code, "{src:?}");
        assert_ne!(code, Some(INLINE_COLON_IN_PLAIN_VALUE));
        assert_eq!(tree(src, &LENIENT), tree(src, &STRICT), "{src:?}");
        assert!(
            diagnostics(src, &LENIENT)
                .iter()
                .all(|d| d.code != LENIENT_INLINE_COLON),
            "{src:?}"
        );
    }
    // The flow inputs above are errors.
    assert!(first_code("{a: b: c}\n", &STRICT).is_some());
    assert!(first_code("a: [b: c: d]\n", &STRICT).is_some());
}

#[test]
fn lenient_scalars_obey_the_scalar_limit() {
    let s = LENIENT.limits(Limits::new().max_scalar_len(4));
    assert_eq!(first_code("a: b: c\n", &s), None);
    assert_eq!(first_code("a: b: cd\n", &s), Some(SCALAR_TOO_LONG));
}
