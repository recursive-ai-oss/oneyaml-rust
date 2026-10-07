//! The plain message of a diagnostic names the likely cause on its own:
//! many readers only ever see the one-line form (`?` in `main`, logs), never
//! the labels and help of the rendered snippet. Codes and spans are those of
//! the generic message.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use oneyaml_core::code as c;
use oneyaml_core::{Applicability, Code, Diagnostic, Settings};
use oneyaml_syntax::event::EventKind;

/// The diagnostics of `src`, in stream order.
fn diagnostics(src: &str) -> Vec<Diagnostic> {
    common::events(src, &Settings::default())
        .iter()
        .filter_map(|e| match e.kind() {
            EventKind::Error(d) | EventKind::Warning(d) => Some((**d).clone()),
            _ => None,
        })
        .collect()
}

/// The first error of `src`, which must have `code`.
fn first(src: &str, code: Code) -> Diagnostic {
    let d = diagnostics(src).into_iter().next().expect(src);
    assert_eq!(d.code, code, "{src:?}: {d:?}");
    d
}

const SEQ_CLOSER: &str =
    "the closing `]` of a flow sequence is not indented more than the sequence's block parent";
const MAP_CLOSER: &str =
    "the closing `}` of a flow mapping is not indented more than the mapping's block parent";
const SEQ_LINE: &str =
    "this line of a flow sequence is not indented more than the sequence's block parent";
const SEQ_OPEN: &str = "a flow sequence is still open at this line, which is not indented more than its block parent; a `]` may be missing before it";
const MAP_OPEN: &str = "a flow mapping is still open at this line, which is not indented more than its block parent; a `}` may be missing before it";

#[test]
fn an_under_indented_closer_is_not_called_unclosed() {
    // JSON-style YAML: the collection is closed, only its closer is short
    // of indentation. The fix indents the line (and then it parses).
    for (src, want, label, fixed) in [
        (
            "args: [\n  a,\n  b\n]\n",
            SEQ_CLOSER,
            "the flow sequence opened here",
            "args: [\n  a,\n  b\n ]\n",
        ),
        (
            "args: {\n  a: 1\n}\n",
            MAP_CLOSER,
            "the flow mapping opened here",
            "args: {\n  a: 1\n }\n",
        ),
        (
            "- [\n  a\n]\n",
            SEQ_CLOSER,
            "the flow sequence opened here",
            "- [\n  a\n ]\n",
        ),
        (
            "x:\n  k: [\n    a\n  ]\n",
            SEQ_CLOSER,
            "the flow sequence opened here",
            "x:\n  k: [\n    a\n   ]\n",
        ),
    ] {
        let d = first(src, c::INVALID_INDENTATION);
        assert_eq!(d.message, want, "{src:?}");
        assert!(!d.message.contains("unclosed") && !d.message.contains("still open"));
        assert_eq!(d.secondary[0].message, label, "{src:?}");
        assert!(
            d.help
                .as_deref()
                .unwrap()
                .contains("closing bracket included")
        );
        let fix = &d.suggestions[0];
        assert_eq!(fix.message, "indent the line");
        assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
        let out = fix.apply(src).unwrap();
        assert_eq!(out, fixed, "{src:?}");
        assert!(diagnostics(&out).is_empty(), "{out:?} still has errors");
    }
}

#[test]
fn an_under_indented_line_that_closes_the_collection_is_named_as_such() {
    for (src, fixed) in [
        ("k: [a,\nb]\n", "k: [a,\n b]\n"),
        ("a:\n  b: [1,\n  2]\n", "a:\n  b: [1,\n   2]\n"),
        // Brackets and quotes inside the line are counted and skipped.
        ("k: [a,\n[b], 'c]'\n]\n", ""),
    ] {
        let d = first(src, c::INVALID_INDENTATION);
        if fixed.is_empty() {
            // `[b], 'c]'` leaves the outer sequence open on its line.
            assert_eq!(d.message, SEQ_OPEN, "{src:?}");
            continue;
        }
        assert_eq!(d.message, SEQ_LINE, "{src:?}");
        assert_eq!(d.secondary[0].message, "the flow sequence opened here");
        let out = d.suggestions[0].apply(src).unwrap();
        assert_eq!(out, fixed, "{src:?}");
        assert!(diagnostics(&out).is_empty(), "{out:?} still has errors");
    }
    let d = first("k: {a: 1,\nb: [2]}\n", c::INVALID_INDENTATION);
    assert_eq!(
        d.message,
        "this line of a flow mapping is not indented more than the mapping's block parent"
    );
}

#[test]
fn a_collection_still_open_at_a_dedented_line_may_miss_its_bracket() {
    for (src, want) in [
        ("a: [1, 2\nb: c\n", SEQ_OPEN),
        ("a: {x: 1\nb: c\n", MAP_OPEN),
        // The innermost open collection is named.
        ("a: [1, {x: 2\nb: c\n", MAP_OPEN),
        ("- [a, b\n- c\n", SEQ_OPEN),
        // A `]` inside a nested collection or a comment does not close it.
        ("a: [1, 2\nb: [3]\n", SEQ_OPEN),
        ("a: [1, 2\nb # ]\n", SEQ_OPEN),
        // A closing bracket of the other kind is not this collection's
        // closer: the collection is still open.
        ("a: [1,\n}\n", SEQ_OPEN),
        ("a: {x: 1,\n]\n", MAP_OPEN),
    ] {
        let d = first(src, c::INVALID_INDENTATION);
        assert_eq!(d.message, want, "{src:?}");
        assert!(d.secondary[0].message.ends_with("is still open"), "{src:?}");
        assert_eq!(d.suggestions[0].message, "close the collection");
    }
    // A flow collection that reaches the end of the input keeps its own
    // message.
    let d = first("a: [1, 2\n", c::UNTERMINATED_FLOW_COLLECTION);
    assert_eq!(d.message, "flow collection is not closed");
    // Block indentation errors are unchanged.
    let d = first("a:\n  - 1\n - 2\n", c::INVALID_INDENTATION);
    assert_eq!(d.message, "content at an invalid indentation");
}

#[test]
fn a_quoted_scalar_open_at_a_dedented_line_may_miss_its_quote() {
    let d = first("a: 'abc\nb: c\n", c::QUOTED_SCALAR_INDENTATION);
    assert_eq!(
        d.message,
        "a single-quoted scalar is still open at a line that is not indented more than its parent; a closing `'` may be missing"
    );
    let d = first("a: \"abc\nb: c\n", c::QUOTED_SCALAR_INDENTATION);
    assert_eq!(
        d.message,
        "a double-quoted scalar is still open at a line that is not indented more than its parent; a closing `\"` may be missing"
    );
    // The other quoted-scalar indentation error keeps its message.
    let d = first("a: \"x\n\t\n  y\"\n", c::QUOTED_SCALAR_INDENTATION);
    assert_eq!(
        d.message,
        "an empty line with a tab must be indented like the scalar's content"
    );
}

#[test]
fn text_glued_to_a_closing_quote_names_the_cause() {
    // An inner quote only when the glued text runs to the same quote.
    for (src, want, help) in [
        (
            "key: 'it's'\n",
            "a `'` inside a single-quoted scalar ends it early",
            "inside single quotes, write `''` for a literal `'`",
        ),
        (
            "say: \"a \"b\" c\"\n",
            "a `\"` inside a double-quoted scalar ends it early",
            "inside double quotes, write `\\\"` for a literal `\"`",
        ),
        (
            "key: \"value\"# note\n",
            "a `#` right after a closing quote does not start a comment",
            "a comment needs white space before its `#` (§6.6)",
        ),
    ] {
        let d = first(src, c::UNEXPECTED_CONTENT);
        assert_eq!(d.message, want, "{src:?}");
        assert_eq!(d.help.as_deref(), Some(help), "{src:?}");
        assert!(!d.suggestions.is_empty(), "{src:?}");
    }
    // Other glued text is not taken for an inner quote.
    for src in ["key: 'a':b\n", "key: 'a'b c\n", "key: \"a\"[x]\n"] {
        let d = first(src, c::UNEXPECTED_CONTENT);
        assert_eq!(
            d.message, "unexpected text right after the closing quote of a quoted scalar",
            "{src:?}"
        );
        let help = d.help.as_deref().unwrap();
        assert!(!help.contains("write"), "{src:?}: {help}");
        assert!(d.suggestions.is_empty(), "{src:?}");
    }
    // Content after a node that is not glued to a quote keeps the generic
    // message.
    let d = first("a: [1, 2]]\n", c::UNEXPECTED_CONTENT);
    assert_eq!(d.message, "unexpected content after a node");
    let d = first("key: 'a' b\n", c::UNEXPECTED_CONTENT);
    assert_eq!(d.message, "unexpected content after a node");
}

#[test]
fn a_node_that_becomes_a_multi_line_key_says_so() {
    let want = "this plain scalar runs onto the next line and is followed by `:`, so it reads as a key, and a key must be on one line";
    let d = first("a: b\n  c: d\n", c::MULTILINE_IMPLICIT_KEY);
    assert_eq!(d.message, want);
    assert!(d.help.as_deref().unwrap().contains("nested entries"));
    // At the root there is no value to nest under: no help.
    let d = first("a\nb: c\n", c::MULTILINE_IMPLICIT_KEY);
    assert_eq!(d.message, want);
    assert!(d.help.is_none());
    // A key written over two lines keeps the key message.
    let d = first("x: 1\na\n b: c\n", c::MULTILINE_IMPLICIT_KEY);
    assert_eq!(d.message, "an implicit key must be on one line");
}
