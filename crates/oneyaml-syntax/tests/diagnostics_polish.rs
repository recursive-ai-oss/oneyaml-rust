//! Diagnostic polish: the common-mistake inputs get a label
//! at the culprit, help, and fixes; codes are unchanged (the suite's
//! expected codes still hold) and every machine-applicable fix yields text
//! that parses without errors.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod common;

use oneyaml_core::code as c;
use oneyaml_core::{Applicability, Code, Diagnostic, Settings, Span};
use oneyaml_syntax::event::EventKind;

/// The diagnostics of `src`, in stream order (contract checked).
fn diagnostics(src: &str) -> Vec<Diagnostic> {
    common::events(src, &Settings::default())
        .iter()
        .filter_map(|e| match e.kind() {
            EventKind::Error(d) | EventKind::Warning(d) => Some((**d).clone()),
            _ => None,
        })
        .collect()
}

/// The first diagnostic, which must have `code`.
fn first(src: &str, code: Code) -> Diagnostic {
    let d = diagnostics(src).into_iter().next().expect(src);
    assert_eq!(d.code, code, "{src:?}: {d:?}");
    d
}

/// Applies suggestion `k` and asserts the result.
fn fixed(src: &str, d: &Diagnostic, k: usize, want: &str, how: Applicability) {
    let s = &d.suggestions[k];
    assert_eq!(s.applicability, how, "{src:?}");
    let out = s.apply(src).unwrap();
    assert_eq!(out, want, "{src:?}");
    if how == Applicability::MachineApplicable {
        assert!(diagnostics(&out).is_empty(), "{out:?} still has errors");
    }
}

#[test]
fn glued_colon_points_at_the_colon_and_offers_the_space() {
    let src = "a: 1\nb:2\n";
    let d = first(src, c::MISSING_MAPPING_COLON);
    // The report is where the key's text ends, the first character that
    // cannot belong to a key; the label marks the glued colon.
    assert_eq!(d.primary.span, Span::new(8, 8));
    assert_eq!(d.secondary[0].span, Span::new(6, 7));
    assert!(d.help.as_deref().unwrap().contains("space"));
    fixed(src, &d, 0, "a: 1\nb: 2\n", Applicability::MaybeIncorrect);
    // Without a glued `:`, the generic help and no fix.
    let src = "a: 1\nb\n";
    let d = first(src, c::MISSING_MAPPING_COLON);
    assert!(d.secondary.is_empty() && d.suggestions.is_empty());
    assert!(d.help.as_deref().unwrap().contains("`key: value`"));
    // A leading `:` is not a glued key/value separator.
    let d = first("a: 1\n:x\n", c::MISSING_MAPPING_COLON);
    assert!(d.suggestions.is_empty());
    // The valid forms are untouched.
    assert!(diagnostics("b:2\n").is_empty());
    assert!(diagnostics("a: 1\nb: 2\n").is_empty());
}

#[test]
fn a_quote_inside_a_quoted_scalar() {
    let src = "key: 'it's'\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    assert_eq!(d.secondary[0].span, Span::new(8, 9));
    assert_eq!(d.secondary[0].message, "the quoted scalar ends here");
    assert!(d.help.as_deref().unwrap().contains("''"));
    fixed(
        src,
        &d,
        0,
        "key: 'it''s'\n",
        Applicability::MachineApplicable,
    );

    let src = "'it's' # c\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    fixed(
        src,
        &d,
        0,
        "'it''s' # c\n",
        Applicability::MachineApplicable,
    );

    // Already doubled quotes stay doubled.
    let src = "- 'a''b'c'\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    fixed(
        src,
        &d,
        0,
        "- 'a''b''c'\n",
        Applicability::MachineApplicable,
    );

    let src = "key: \"say \"hi\"\"\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    assert!(d.help.as_deref().unwrap().contains("\\\""));
    fixed(
        src,
        &d,
        0,
        "key: \"say \\\"hi\\\"\"\n",
        Applicability::MachineApplicable,
    );
    // Escapes already in the text are kept as they are.
    let src = "k: \"a\\\"b\"c\"\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    fixed(
        src,
        &d,
        0,
        "k: \"a\\\"b\\\"c\"\n",
        Applicability::MachineApplicable,
    );
}

#[test]
fn quoted_scalar_followed_by_other_content() {
    // Not glued to the quote: no quoting fix (the code is unchanged).
    let d = first("k: 'x' y\n", c::UNEXPECTED_CONTENT);
    assert!(d.suggestions.is_empty() && d.secondary.is_empty());
    // Glued, but the rest does not end with the quote: label and help only.
    let d = first("k: 'it's\n", c::UNEXPECTED_CONTENT);
    assert_eq!(d.secondary.len(), 1);
    assert!(d.suggestions.is_empty());
    // A multi-line quoted scalar is not rewritten.
    let d = first("k: 'a\n  b'c'\n", c::UNEXPECTED_CONTENT);
    assert!(d.suggestions.is_empty());
    // Content after a plain scalar keeps the plain diagnostic.
    let d = first("- [a] b\n", c::UNEXPECTED_CONTENT);
    assert!(d.suggestions.is_empty());
}

#[test]
fn comment_glued_to_a_quoted_scalar() {
    // yaml-test-suite SU5Z's shape.
    let src = "key: \"value\"# invalid comment\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    assert!(d.help.as_deref().unwrap().contains("white space"));
    fixed(
        src,
        &d,
        0,
        "key: \"value\" # invalid comment\n",
        Applicability::MachineApplicable,
    );
}

#[test]
fn windows_paths_in_double_quotes() {
    let src = "path: \"C:\\path\"\n";
    let d = first(src, c::INVALID_ESCAPE);
    assert_eq!(d.secondary[0].span, Span::new(9, 11));
    assert_eq!(d.secondary[0].message, "`\\p` is not an escape");
    assert!(d.help.as_deref().unwrap().contains("single quotes"));
    fixed(
        src,
        &d,
        0,
        "path: \"C:\\\\path\"\n",
        Applicability::MachineApplicable,
    );
    fixed(
        src,
        &d,
        1,
        "path: 'C:\\path'\n",
        Applicability::MaybeIncorrect,
    );

    // Several bad escapes are all fixed; a valid one rules out single
    // quotes (they would change its meaning).
    let src = "p: \"C:\\a\\qx\\zy\\n\"\n";
    let d = first(src, c::INVALID_ESCAPE);
    assert_eq!(d.suggestions.len(), 1);
    fixed(
        src,
        &d,
        0,
        "p: \"C:\\a\\\\qx\\\\zy\\n\"\n",
        // `\a` and `\n` may have been meant literally too.
        Applicability::MaybeIncorrect,
    );
    // A `'` rules out single quotes too.
    let d = first("p: \"it's \\q\"\n", c::INVALID_ESCAPE);
    assert_eq!(d.suggestions.len(), 1);
    // Unclosed on its line: the fix covers that line only.
    let src = "p: \"a\\q\n  b\"\n";
    let d = first(src, c::INVALID_ESCAPE);
    assert_eq!(d.suggestions.len(), 1);
    fixed(
        src,
        &d,
        0,
        "p: \"a\\\\q\n  b\"\n",
        Applicability::MaybeIncorrect,
    );
}

#[test]
fn an_escaped_apostrophe_loses_its_backslash() {
    // yaml-test-suite HRE5's shape.
    let src = "q: \"it\\'s\"\n";
    let d = first(src, c::INVALID_ESCAPE);
    assert!(d.help.as_deref().unwrap().contains("needs no escape"));
    assert_eq!(d.suggestions.len(), 1);
    fixed(
        src,
        &d,
        0,
        "q: \"it's\"\n",
        Applicability::MachineApplicable,
    );
}

#[test]
fn reserved_indicators() {
    let src = "a: @foo\n";
    let d = first(src, c::INVALID_NODE_START);
    assert_eq!(d.secondary[0].span, Span::new(3, 4));
    assert!(d.help.as_deref().unwrap().contains("reserved"));
    fixed(src, &d, 0, "a: '@foo'\n", Applicability::MachineApplicable);

    let src = "a: `cmd` # run it\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(
        src,
        &d,
        0,
        "a: '`cmd`' # run it\n",
        Applicability::MachineApplicable,
    );

    // As a key, only the key is quoted.
    let src = "@k: v\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(src, &d, 0, "'@k': v\n", Applicability::MachineApplicable);
    let src = "m:\n  @k: v\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(
        src,
        &d,
        0,
        "m:\n  '@k': v\n",
        Applicability::MachineApplicable,
    );

    // A value holding `: ` may have meant a mapping.
    let src = "a: @b: c\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(src, &d, 0, "a: '@b: c'\n", Applicability::MaybeIncorrect);

    // Other invalid starts keep the plain diagnostic.
    let d = first("a: ,\n", c::INVALID_NODE_START);
    assert!(d.suggestions.is_empty());
}

#[test]
fn unclosed_flow_collections() {
    let src = "a: [1, 2\n";
    let d = first(src, c::UNTERMINATED_FLOW_COLLECTION);
    assert_eq!(d.secondary[0].span, Span::new(3, 4));
    assert_eq!(d.secondary[0].message, "this `[` is never closed");
    fixed(src, &d, 0, "a: [1, 2]\n", Applicability::MachineApplicable);

    let src = "a: [1, {b: 2 # note\n";
    let d = first(src, c::UNTERMINATED_FLOW_COLLECTION);
    assert_eq!(d.secondary[0].message, "this `{` is never closed");
    fixed(
        src,
        &d,
        0,
        "a: [1, {b: 2}] # note\n",
        Applicability::MachineApplicable,
    );

    let src = "- {a: [x,\n";
    let d = first(src, c::UNTERMINATED_FLOW_COLLECTION);
    fixed(
        src,
        &d,
        0,
        "- {a: [x,]}\n",
        Applicability::MachineApplicable,
    );

    // A dedented line: the bracket may close later, so maybe.
    let src = "a: [1, 2\nb: 3\n";
    let d = first(src, c::INVALID_INDENTATION);
    assert_eq!(d.secondary[0].span, Span::new(3, 4));
    assert!(d.help.as_deref().unwrap().contains("indented more"));
    fixed(
        src,
        &d,
        0,
        "a: [1, 2]\nb: 3\n",
        Applicability::MaybeIncorrect,
    );

    // A document marker inside the collection.
    let src = "[a\n---\n";
    let d = first(src, c::UNTERMINATED_FLOW_COLLECTION);
    assert_eq!(d.secondary[0].span, Span::new(0, 1));
    assert_eq!(
        d.suggestions[0].applicability,
        Applicability::MaybeIncorrect
    );
}

#[test]
fn a_missing_closing_quote() {
    // The next line is not indented: the scalar cannot go on.
    let src = "a: \"abc\nb: 1\n";
    let d = first(src, c::QUOTED_SCALAR_INDENTATION);
    assert_eq!(d.secondary[0].span, Span::new(3, 4));
    assert_eq!(d.secondary[0].message, "the quoted scalar starts here");
    assert!(d.help.as_deref().unwrap().contains("closing quote"));
    fixed(
        src,
        &d,
        0,
        "a: \"abc\"\nb: 1\n",
        Applicability::MaybeIncorrect,
    );
    assert!(diagnostics("a: \"abc\"\nb: 1\n").is_empty());
    // Runs into the end of the input: the quote goes before what would
    // be a comment outside the quotes.
    let src = "a: 'abc # note\n";
    let d = first(src, c::UNTERMINATED_QUOTED_SCALAR);
    assert!(d.help.as_deref().unwrap().contains("closing quote"));
    fixed(
        src,
        &d,
        0,
        "a: 'abc' # note\n",
        Applicability::MaybeIncorrect,
    );
    assert!(diagnostics("a: 'abc' # note\n").is_empty());
}

#[test]
fn default_help_by_code() {
    let d = first("a:\n\t- b\n", c::TAB_INDENTATION);
    assert!(d.help.as_deref().unwrap().contains("spaces"));
    let d = first("a: \"abc\n", c::UNTERMINATED_QUOTED_SCALAR);
    assert!(d.help.as_deref().unwrap().contains("closing quote"));
    // Limit help still names the setting.
    let settings = Settings::new().max_depth(1);
    let events = common::events("a:\n  b: 1\n", &settings);
    let d = events
        .iter()
        .find_map(|e| match e.kind() {
            EventKind::Error(d) => Some(d.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(d.code, c::DEPTH_LIMIT_EXCEEDED);
    assert!(d.help.as_deref().unwrap().contains("Limits::max_depth"));
}

#[test]
fn existing_codes_are_unchanged() {
    // The yaml-test-suite shapes the polish touches, with their codes.
    for (src, code) in [
        ("key: \"value\"# invalid comment\n", c::UNEXPECTED_CONTENT), // SU5Z
        ("\"quoted \\' scalar\"\n", c::INVALID_ESCAPE),               // HRE5
        ("---\n[\n---\n", c::UNTERMINATED_FLOW_COLLECTION),           // N782
        ("---\n[ a, b, c ] ]\n", c::UNEXPECTED_CONTENT),              // 4H7K
        ("key: [ a, b\n", c::UNTERMINATED_FLOW_COLLECTION),           // 6JTT-like
    ] {
        let _ = first(src, code);
    }
}

// ---------------------------------------------------------------- more polish

#[test]
fn fixes_that_would_not_parse_are_not_offered() {
    // A backslash before the last quote would escape it.
    let d = first("k: \"a\"b\\\"\n", c::UNEXPECTED_CONTENT);
    assert!(d.suggestions.is_empty(), "{d:?}");
    // A bad escape in the glued rest would stay.
    let d = first("k: \"a\"b\\q\"\n", c::UNEXPECTED_CONTENT);
    assert!(d.suggestions.is_empty(), "{d:?}");
    // The closing quote is never placed after a lone backslash.
    let d = first("a: \"abc\\\nb: 1\n", c::QUOTED_SCALAR_INDENTATION);
    assert!(d.suggestions.is_empty(), "{d:?}");
    let src = "a: \"abc\\\\\nb: 1\n"; // an escaped backslash is fine
    let d = first(src, c::QUOTED_SCALAR_INDENTATION);
    fixed(
        src,
        &d,
        0,
        "a: \"abc\\\\\"\nb: 1\n",
        Applicability::MaybeIncorrect,
    );
}

#[test]
fn continued_values_are_only_maybe() {
    for (src, code, want) in [
        (
            "a: @foo\n  bar\n",
            c::INVALID_NODE_START,
            "a: '@foo'\n  bar\n",
        ),
        ("a: `x\n  y`\n", c::INVALID_NODE_START, "a: '`x'\n  y`\n"),
        (
            "k: 'it's'\n  more\n",
            c::UNEXPECTED_CONTENT,
            "k: 'it''s'\n  more\n",
        ),
        (
            "k: \"a\"#c\n  more\n",
            c::UNEXPECTED_CONTENT,
            "k: \"a\" #c\n  more\n",
        ),
        (
            "k: \"C:\\p\"\n  more\n",
            c::INVALID_ESCAPE,
            "k: \"C:\\\\p\"\n  more\n",
        ),
        ("k: \"C:\\p\" x\n", c::INVALID_ESCAPE, "k: \"C:\\\\p\" x\n"),
        ("@foo\nbar\n", c::INVALID_NODE_START, "'@foo'\nbar\n"),
    ] {
        let d = first(src, code);
        fixed(src, &d, 0, want, Applicability::MaybeIncorrect);
    }
    // Blank lines and comments do not continue a value; a dedent does not.
    for (src, code, want) in [
        (
            "a: @foo\n\n  # c\nb: 1\n",
            c::INVALID_NODE_START,
            "a: '@foo'\n\n  # c\nb: 1\n",
        ),
        ("- 'it's'\n- x\n", c::UNEXPECTED_CONTENT, "- 'it''s'\n- x\n"),
        (
            "s:\n  - @x\n  - y\n",
            c::INVALID_NODE_START,
            "s:\n  - '@x'\n  - y\n",
        ),
        (
            "k: \"C:\\p\" # c\n---\nx\n",
            c::INVALID_ESCAPE,
            "k: \"C:\\\\p\" # c\n---\nx\n",
        ),
        ("\"C:\\p\": v\n", c::INVALID_ESCAPE, "\"C:\\\\p\": v\n"),
    ] {
        let d = first(src, code);
        fixed(src, &d, 0, want, Applicability::MachineApplicable);
    }
}

#[test]
fn polish_in_flow_context() {
    let src = "k: [\"C:\\path\", b]\n";
    let d = first(src, c::INVALID_ESCAPE);
    assert_eq!(d.secondary[0].message, "`\\p` is not an escape");
    fixed(
        src,
        &d,
        0,
        "k: [\"C:\\\\path\", b]\n",
        Applicability::MachineApplicable,
    );
    fixed(
        src,
        &d,
        1,
        "k: ['C:\\path', b]\n",
        Applicability::MaybeIncorrect,
    );

    let src = "[@x]\n";
    let d = first(src, c::INVALID_NODE_START);
    assert_eq!(d.secondary[0].span, Span::new(1, 2));
    fixed(src, &d, 0, "['@x']\n", Applicability::MachineApplicable);
    let src = "{a: @x, b: y}\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(
        src,
        &d,
        0,
        "{a: '@x', b: y}\n",
        Applicability::MachineApplicable,
    );
    let src = "{@k: v}\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(src, &d, 0, "{'@k': v}\n", Applicability::MachineApplicable);
    // Running to the end of the line in flow context: it may go on.
    let src = "[@x\n]\n";
    let d = first(src, c::INVALID_NODE_START);
    fixed(src, &d, 0, "['@x'\n]\n", Applicability::MaybeIncorrect);
}

// ------------------------------------------- property: fixes are fixes

/// The errors of `src` without the contract check (the checked parse runs
/// once per fixed text below).
fn errors(src: &str) -> Vec<Diagnostic> {
    diagnostics(src)
        .into_iter()
        .filter(|d| d.severity == oneyaml_core::Severity::Error)
        .collect()
}

/// The value of the scalar starting at `at` in `src`.
fn scalar_at(src: &str, at: usize) -> Option<String> {
    let events = common::events(src, &Settings::default());
    events.sourced().find_map(|e| match e.kind() {
        EventKind::Scalar { .. }
            if e.event().span().start == at && !e.event().span().is_empty() =>
        {
            e.value().map(|v| v.into_owned())
        }
        _ => None,
    })
}

/// Every machine-applicable fix of the first error of `src` removes it:
/// the fixed text has no error on or before that line (shifted by the
/// edit), and, where the fix's intent is knowable, the fixed scalar has
/// the intended value. `strict`: the fixed text must have no error at
/// all (the input has only that one error). Returns the fixes checked.
fn check_fixes(src: &str, strict: bool) -> usize {
    let errs = errors(src);
    let Some(d) = errs.first() else {
        return 0;
    };
    let mut checked = 0;
    for s in &d.suggestions {
        let out = s
            .apply(src)
            .unwrap_or_else(|| panic!("{src:?}: {s:?} does not apply"));
        if s.applicability != Applicability::MachineApplicable {
            continue;
        }
        checked += 1;
        // Where the fixed text ends, in `out`.
        let last = s.edits.last().unwrap();
        let delta = out.len() as isize - src.len() as isize;
        let fixed_end = (last.span.end as isize + delta) as usize;
        let after = errors(&out);
        if let Some(e) = after.first() {
            // Strict: nothing is left. Otherwise, whatever is left starts
            // after the fixed text: a later problem, not this one.
            // An error right at the end of the fixed text is
            // unrelated only if it is also there when the fixed node is a
            // plain word instead.
            let start = e.primary.span.start;
            let edit_start = s.edits[0].span.start;
            let unrelated_at_end = start == fixed_end && {
                let probe = format!("{}w{}", &out[..edit_start], &out[fixed_end..]);
                errors(&probe)
                    .first()
                    .is_some_and(|p| p.code == e.code && p.primary.span.start == edit_start + 1)
            };
            assert!(
                !strict && (start > fixed_end || unrelated_at_end),
                "{src:?} --[{}]--> {out:?} still fails: {e:?}",
                s.message
            );
        }
        // The intended values.
        let edit = &s.edits[0];
        let original = &src[edit.span.start..edit.span.end];
        let want = match s.message.as_str() {
            // (Multi-line values fold the same quoted or plain.)
            "quote the value" if !original.contains(['\n', '\r']) => Some(original.to_owned()),
            "double the inner `'`" => Some(original[1..original.len() - 1].replace("''", "'")),
            "escape the inner `\"`" if !original.contains('\\') => {
                Some(original[1..original.len() - 1].to_owned())
            }
            _ => None,
        };
        if let Some(want) = want {
            let got = scalar_at(&out, edit.span.start);
            assert_eq!(got.as_deref(), Some(want.as_str()), "{src:?} -> {out:?}");
        }
        if s.message == "make the backslashes literal" {
            // No skip. A machine-applicable fix keeps only
            // `\\` and `\"` as escapes, so the intended value is the
            // text between the quotes read with every other backslash
            // literal (`\'` loses its backslash).
            // The report points inside the scalar; its opening quote is the
            // first one on that line.
            let line = src[..d.primary.span.start]
                .rfind(['\n', '\r'])
                .map_or(0, |k| k + 1);
            let q = line + src[line..].find('"').unwrap();
            let mut want = String::new();
            let mut chars = src[q + 1..].chars();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => match chars.next().unwrap() {
                        n @ ('\\' | '"' | '\'') => want.push(n),
                        n => {
                            assert!(
                                !"0abtnvfreN_LPxuU/ \t".contains(n),
                                "{src:?}: a valid `\\{n}` in a machine-applicable fix"
                            );
                            want.push('\\');
                            want.push(n);
                        }
                    },
                    c => want.push(c),
                }
            }
            let got = scalar_at(&out, q);
            assert_eq!(got.as_deref(), Some(want.as_str()), "{src:?} -> {out:?}");
        }
    }
    checked
}

#[test]
fn machine_applicable_fixes_fix_every_suite_case() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    let mut stack = vec![root];
    let (mut cases, mut checked) = (0, 0);
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|f| f == "in.yaml") {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                cases += 1;
                let strict = errors(&src).len() == 1;
                checked += check_fixes(&src, strict);
            }
        }
    }
    assert!(cases > 300, "{cases}");
    assert!(checked >= 3, "only {checked} fixes on the suite");
}

/// xorshift64*.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PIECES: &[&str] = &[
    "a", "k", "é", "中", ":", ": ", " ", "  ", "\n", "\n  ", "\r\n", "\t", "#", " #", "'", "''",
    "\"", "\\", "\\q", "\\n", "\\'", "\\\"", "@", "`", "[", "]", "{", "}", ", ", "- ", "? ", "&a ",
    "*a", "!t ", "---\n", "...\n", "|\n", "b:2", "\\\\", "\\t", "\u{1}", "\u{7f}", "\u{85}", ": [",
    "{a: ",
];

#[test]
fn machine_applicable_fixes_fix_generated_text() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let (mut checked, mut offered) = (0, 0);
    for _ in 0..200_000 {
        let n = 1 + rng.below(24);
        let src: String = (0..n).map(|_| PIECES[rng.below(PIECES.len())]).collect();
        let before = checked;
        checked += check_fixes(&src, false);
        offered += usize::from(checked > before);
    }
    assert!(
        checked > 500,
        "only {checked} fixes checked ({offered} inputs)"
    );
}

#[test]
fn polish_regressions() {
    // Valid escapes other than `\\`/`\"` make the literal-backslash fix
    // uncertain: the value would silently keep them as escapes.
    for src in ["path: \"C:\\data\\new\"\n", "p: \"C:\\temp\\q\"\n"] {
        let d = first(src, c::INVALID_ESCAPE);
        assert_eq!(
            d.suggestions[0].applicability,
            Applicability::MaybeIncorrect,
            "{src:?}"
        );
    }
    // `\\` and `\"` are certain: machine-applicable, with the value checked.
    let src = "p: \"a\\\\b\\qc\\\"d\"\n";
    let d = first(src, c::INVALID_ESCAPE);
    fixed(
        src,
        &d,
        0,
        "p: \"a\\\\b\\\\qc\\\"d\"\n",
        Applicability::MachineApplicable,
    );
    assert_eq!(check_fixes(src, true), 1);
    assert_eq!(
        scalar_at("p: \"a\\\\b\\\\qc\\\"d\"\n", 3).as_deref(),
        Some("a\\b\\qc\"d")
    );

    // Flow: a `:` after a value, and `[`/`{` after a node, do not end it.
    for (src, want) in [
        ("{a: @b: c}\n", "{a: '@b: c'}\n"),
        ("[k: @x: y]\n", "[k: '@x: y']\n"),
        ("{a: {b: @c: d}}\n", "{a: {b: '@c: d'}}\n"),
        ("[@x[1]]\n", "['@x'[1]]\n"),
        ("{a: @x{}}\n", "{a: '@x'{}}\n"),
    ] {
        let d = first(src, c::INVALID_NODE_START);
        fixed(src, &d, 0, want, Applicability::MaybeIncorrect);
    }
    // Keys in flow still end at their `:`.
    for (src, want) in [
        ("[@x: y]\n", "['@x': y]\n"),
        ("{a: 1, @k: v}\n", "{a: 1, '@k': v}\n"),
    ] {
        let d = first(src, c::INVALID_NODE_START);
        fixed(src, &d, 0, want, Applicability::MachineApplicable);
    }
    // A value followed by `:` in flow is not a clean end for the escape fix.
    let d = first("{a: \"C:\\p\": b}\n", c::INVALID_ESCAPE);
    assert_eq!(
        d.suggestions[0].applicability,
        Applicability::MaybeIncorrect
    );
    let src = "{\"C:\\p\": b}\n";
    let d = first(src, c::INVALID_ESCAPE);
    fixed(
        src,
        &d,
        0,
        "{\"C:\\\\p\": b}\n",
        Applicability::MachineApplicable,
    );
}

#[test]
fn inline_colon_fix_with_a_control_character_is_only_maybe() {
    let src = "k: a: b\u{1}c\n";
    for d in diagnostics(src) {
        for s in &d.suggestions {
            if s.apply(src).is_some_and(|out| !errors(&out).is_empty()) {
                assert_eq!(s.applicability, Applicability::MaybeIncorrect, "{d:?}");
            }
        }
    }
}

/// Item 7: a misplaced byte order mark is never content outside
/// quotes (`Y0032`, with its own "remove it" fix), so a quoting fix on its
/// line never carries U+FEFF into the suggested text, where it would be
/// invisible and become part of the value.
#[test]
fn quoting_fixes_leave_a_misplaced_mark_out() {
    for (src, code, want) in [
        // `quote_fix`: an inline colon, a reserved indicator.
        // (The mark ends the plain scalar; the fix quotes the
        // glued text after it too, before a comment.)
        (
            "k: a: b\u{feff}c\n",
            c::INLINE_COLON_IN_PLAIN_VALUE,
            "k: 'a: bc'\n",
        ),
        (
            "k: a: b\u{feff}c d  # e\n",
            c::INLINE_COLON_IN_PLAIN_VALUE,
            "k: 'a: bc d'  # e\n",
        ),
        ("k: @a\u{feff}b\n", c::INVALID_NODE_START, "k: '@ab'\n"),
        // `after_quote`: a quote glued to the closing quote.
        ("k: 'it's\u{feff}'\n", c::UNEXPECTED_CONTENT, "k: 'it''s'\n"),
        (
            "k: \"say \"hi\u{feff}\"\"\n",
            c::UNEXPECTED_CONTENT,
            "k: \"say \\\"hi\\\"\"\n",
        ),
    ] {
        let all = diagnostics(src);
        assert!(
            all.iter().any(|d| d.code == c::MISPLACED_BYTE_ORDER_MARK),
            "{src:?}: {all:?}"
        );
        let d = all.iter().find(|d| d.code == code).expect(src);
        assert!(!d.suggestions.is_empty(), "{src:?}: {d:?}");
        for s in all.iter().flat_map(|d| &d.suggestions) {
            for e in &s.edits {
                assert!(!e.text.contains('\u{feff}'), "{src:?}: {s:?}");
            }
        }
        let fixed = d.suggestions[0].apply(src).unwrap();
        assert_eq!(fixed, want, "{src:?}");
        assert!(errors(&fixed).is_empty(), "{fixed:?}");
    }
    // A mark inside the original quotes is the value's (`nb-json`): kept.
    let src = "k: 'i\u{feff}t's'\n";
    let d = first(src, c::UNEXPECTED_CONTENT);
    assert_eq!(d.suggestions[0].apply(src).unwrap(), "k: 'i\u{feff}t''s'\n");
}
