//! Parser unit tests: every stream is checked against the event contract, the
//! semantic events are compared in yaml-test-suite-like notation, and every
//! diagnostic code the parser emits has a negative test.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml_core::Code;
use oneyaml_core::code as c;

use oneyaml_core::{Limits, Settings};

use super::{parse, parse_with};
use crate::event::{CollectionStyle, Event, EventKind, EventList, ScalarStyle, check_contract};

/// Parses, asserts the contract, and checks the spans reproduce the input.
fn events(src: &str) -> EventList<'_> {
    let events = parse(src);
    if let Err(err) = check_contract(src, &events) {
        panic!("{src:?}: {err}\n{events:#?}");
    }
    let rebuilt: String = events.sourced().map(|e| e.text().unwrap()).collect();
    assert_eq!(rebuilt.as_bytes(), src.as_bytes());
    events
}

/// Semantic events in compact notation, one string per event.
fn tree(src: &str) -> String {
    let mut out = Vec::new();
    for e in events(src) {
        let props = {
            let mut s = String::new();
            if let Some(a) = e.anchor() {
                s.push_str(&format!(" &{}", a.name));
            }
            if let Some(t) = e.tag() {
                s.push_str(&format!(" <{}>", t.resolved));
            }
            s
        };
        out.push(match e.kind() {
            EventKind::StreamStart | EventKind::StreamEnd => continue,
            EventKind::DocumentStart { explicit } => {
                if *explicit { "+DOC ---" } else { "+DOC" }.to_owned()
            }
            EventKind::DocumentEnd { explicit } => {
                if *explicit { "-DOC ..." } else { "-DOC" }.to_owned()
            }
            EventKind::MappingStart { style, .. } => {
                let flow = match style {
                    CollectionStyle::Block => "+MAP",
                    CollectionStyle::Flow => "+MAP {}",
                    _ => "+PAIR",
                };
                format!("{flow}{props}")
            }
            EventKind::SequenceStart { style, .. } => {
                let flow = if *style == CollectionStyle::Block {
                    ""
                } else {
                    " []"
                };
                format!("+SEQ{flow}{props}")
            }
            EventKind::MappingEnd => "-MAP".to_owned(),
            EventKind::SequenceEnd => "-SEQ".to_owned(),
            EventKind::Scalar { style, .. } => {
                let style = match style {
                    ScalarStyle::Plain => ':',
                    ScalarStyle::SingleQuoted => '\'',
                    ScalarStyle::DoubleQuoted => '"',
                    ScalarStyle::Literal => '|',
                    _ => '>',
                };
                let value = e.value().unwrap();
                format!("=VAL{props} {style}{}", value.replace('\n', "\\n"))
            }
            EventKind::Alias => format!("=ALI *{}", e.alias_name().unwrap()),
            EventKind::Error(d) => format!("!{}", d.code),
            EventKind::Warning(d) => format!("~{}", d.code),
            _ => continue,
        });
    }
    out.join(" | ")
}

/// The diagnostic codes, in stream order.
fn codes(src: &str) -> Vec<Code> {
    events(src)
        .iter()
        .filter_map(|e| e.diagnostic().map(|d| d.code))
        .collect()
}

#[test]
fn empty_and_trivia_only_streams() {
    assert_eq!(tree(""), "");
    assert_eq!(tree("# c\n\n  \n"), "");
    assert_eq!(tree("...\n"), "");
    assert_eq!(tree("\u{feff}a"), "+DOC | =VAL :a | -DOC");
}

#[test]
fn block_mappings_and_sequences() {
    assert_eq!(
        tree("a: b\nc: d\n"),
        "+DOC | +MAP | =VAL :a | =VAL :b | =VAL :c | =VAL :d | -MAP | -DOC"
    );
    assert_eq!(
        tree("a:\n  - b\n  - c\nd:\n- e\n"),
        "+DOC | +MAP | =VAL :a | +SEQ | =VAL :b | =VAL :c | -SEQ | =VAL :d | +SEQ | =VAL :e | -SEQ | -MAP | -DOC"
    );
    assert_eq!(
        tree("- - a\n  - b\n- k: v\n  l: w\n"),
        "+DOC | +SEQ | +SEQ | =VAL :a | =VAL :b | -SEQ | +MAP | =VAL :k | =VAL :v | =VAL :l | =VAL :w | -MAP | -SEQ | -DOC"
    );
    assert_eq!(
        tree("a:\n  b:\n    c: d\ne: f"),
        "+DOC | +MAP | =VAL :a | +MAP | =VAL :b | +MAP | =VAL :c | =VAL :d | -MAP | -MAP | =VAL :e | =VAL :f | -MAP | -DOC"
    );
}

#[test]
fn explicit_keys_and_empty_nodes() {
    assert_eq!(
        tree("? a\n: b\n"),
        "+DOC | +MAP | =VAL :a | =VAL :b | -MAP | -DOC"
    );
    assert_eq!(
        tree("? a\n? b\n"),
        "+DOC | +MAP | =VAL :a | =VAL : | =VAL :b | =VAL : | -MAP | -DOC"
    );
    assert_eq!(
        tree("? - a\n: - b\n"),
        "+DOC | +MAP | +SEQ | =VAL :a | -SEQ | +SEQ | =VAL :b | -SEQ | -MAP | -DOC"
    );
    assert_eq!(
        tree("a:\nb:\n"),
        "+DOC | +MAP | =VAL :a | =VAL : | =VAL :b | =VAL : | -MAP | -DOC"
    );
    assert_eq!(
        tree(": v\n"),
        "+DOC | +MAP | =VAL : | =VAL :v | -MAP | -DOC"
    );
    assert_eq!(
        tree("-\n- a\n"),
        "+DOC | +SEQ | =VAL : | =VAL :a | -SEQ | -DOC"
    );
    assert_eq!(tree("---\n"), "+DOC --- | =VAL : | -DOC");
    assert_eq!(tree("--- \n...\n"), "+DOC --- | =VAL : | -DOC ...");
}

#[test]
fn documents_and_markers() {
    assert_eq!(
        tree("a\n---\nb\n...\n"),
        "+DOC | =VAL :a | -DOC | +DOC --- | =VAL :b | -DOC ..."
    );
    assert_eq!(
        tree("--- a\n--- b"),
        "+DOC --- | =VAL :a | -DOC | +DOC --- | =VAL :b | -DOC"
    );
    assert_eq!(
        tree("a\n...\nb\n"),
        "+DOC | =VAL :a | -DOC ... | +DOC | =VAL :b | -DOC"
    );
    assert_eq!(
        tree("a: 1\r\nb: 2\r"),
        "+DOC | +MAP | =VAL :a | =VAL :1 | =VAL :b | =VAL :2 | -MAP | -DOC"
    );
}

#[test]
fn plain_scalars() {
    assert_eq!(
        tree("a b\n  c\n\n  d # e\n"),
        "+DOC | =VAL :a b c\\nd | -DOC"
    );
    assert_eq!(
        tree("k: a\n b\nl: c"),
        "+DOC | +MAP | =VAL :k | =VAL :a b | =VAL :l | =VAL :c | -MAP | -DOC"
    );
    assert_eq!(
        tree("a:b: c#d"),
        "+DOC | +MAP | =VAL :a:b | =VAL :c#d | -MAP | -DOC"
    );
    assert_eq!(
        tree("- a\n  - b"),
        "+DOC | +SEQ | =VAL :a - b | -SEQ | -DOC"
    );
}

#[test]
fn quoted_scalars() {
    assert_eq!(
        tree("'a''b': \"c\\td\""),
        "+DOC | +MAP | =VAL 'a'b | =VAL \"c\td | -MAP | -DOC"
    );
    assert_eq!(
        tree("k: 'a\n  b\n\n  c'"),
        "+DOC | +MAP | =VAL :k | =VAL 'a b\\nc | -MAP | -DOC"
    );
    assert_eq!(tree("\"a\\\n  b\\u00e9\""), "+DOC | =VAL \"abé | -DOC");
}

#[test]
fn properties_aliases_and_tags() {
    assert_eq!(
        tree("&a k: *a"),
        "+DOC | +MAP | =VAL &a :k | =ALI *a | -MAP | -DOC"
    );
    assert_eq!(
        tree("&m\n&k k: v"),
        "+DOC | +MAP &m | =VAL &k :k | =VAL :v | -MAP | -DOC"
    );
    assert_eq!(
        tree("- !!str\n- &x !t\n  - a"),
        "+DOC | +SEQ | =VAL <tag:yaml.org,2002:str> : | +SEQ &x <!t> | =VAL :a | -SEQ | -SEQ | -DOC"
    );
    assert_eq!(
        tree("!!null : a"),
        "+DOC | +MAP | =VAL <tag:yaml.org,2002:null> : | =VAL :a | -MAP | -DOC"
    );
    assert_eq!(tree("! a"), "+DOC | =VAL <!> :a | -DOC");
    assert_eq!(tree("!<tag:x%21> a"), "+DOC | =VAL <tag:x!> :a | -DOC");
    assert_eq!(
        tree("%TAG !e! tag:e.com,2000:\n--- !e!a%20b x"),
        "+DOC --- | =VAL <tag:e.com,2000:a b> :x | -DOC"
    );
    // ns-anchor-char includes `:` (production 102).
    assert_eq!(tree("&a: x"), "+DOC | =VAL &a: :x | -DOC");
}

#[test]
fn directives_and_warnings() {
    assert_eq!(codes("%YAML 1.0\n---\na"), [c::YAML_VERSION_1_0]);
    assert_eq!(codes("%YAML 1.1\n---\na"), [c::YAML_VERSION_1_1]);
    assert_eq!(codes("%YAML 1.3\n---\na"), [c::YAML_VERSION_NEWER_MINOR]);
    assert_eq!(codes("%YAML 1.2\n---\na"), []);
    assert_eq!(codes("%FOO bar\n---\na"), [c::RESERVED_DIRECTIVE_IGNORED]);
    assert_eq!(
        codes("%YAML 1.2 # c\n%TAG !a! b\n---\na\n...\n%YAML 1.2\n---\nb"),
        []
    );
}

/// The first error code, asserting the stream conforms (C6 R1).
fn first_error(src: &str) -> Option<Code> {
    events(src)
        .iter()
        .find(|e| e.is_error())
        .map(|e| e.diagnostic().unwrap().code)
}

#[test]
fn every_error_code_is_reachable() {
    let long_key = format!("{}: v\n", "k".repeat(1025));
    let cases: &[(&str, Code)] = &[
        ("%YAML 1.2\n", c::DIRECTIVE_WITHOUT_DOCUMENT),
        ("%YAML 1.2\na\n", c::DIRECTIVE_WITHOUT_DOCUMENT),
        ("%YAML 1.2\n...\n", c::DIRECTIVE_WITHOUT_DOCUMENT),
        ("%YAML\n---\n", c::MALFORMED_DIRECTIVE),
        ("%YAML 1.2 x\n---\n", c::MALFORMED_DIRECTIVE),
        ("%TAG !x a\n---\n", c::MALFORMED_DIRECTIVE),
        ("% x\n---\n", c::MALFORMED_DIRECTIVE),
        ("--- a # c\n%YAML 1.2\n---\n", c::DIRECTIVE_IN_DOCUMENT),
        ("---\n%YAML 1.2\n---\n", c::DIRECTIVE_IN_DOCUMENT),
        ("a\nb: c\n", c::MULTILINE_IMPLICIT_KEY),
        ("'a'\nb\n", c::CONTENT_AFTER_DOCUMENT),
        ("... x\n", c::CONTENT_AFTER_DOCUMENT),
        ("a:\n  b: c\n d: e\n", c::INVALID_INDENTATION),
        ("a: b\n- c\n", c::INVALID_INDENTATION),
        ("- - a\n - b\n", c::INVALID_INDENTATION),
        ("--- a: b\n", c::BLOCK_COLLECTION_NOT_ALLOWED),
        ("a: - b\n", c::BLOCK_COLLECTION_NOT_ALLOWED),
        ("a: b: c\n", c::INLINE_COLON_IN_PLAIN_VALUE),
        ("&x - a\n", c::BLOCK_COLLECTION_NOT_ALLOWED),
        ("a: 'b': c\n", c::BLOCK_COLLECTION_NOT_ALLOWED),
        ("'a\n b': c\n", c::MULTILINE_IMPLICIT_KEY),
        ("k:\n  x\n  y: z\n", c::MULTILINE_IMPLICIT_KEY),
        ("@a\n", c::INVALID_NODE_START),
        ("a: `b\n", c::INVALID_NODE_START),
        ("'a\n", c::UNTERMINATED_QUOTED_SCALAR),
        ("\"a\n---\n\"\n", c::UNTERMINATED_QUOTED_SCALAR),
        ("\"a\u{1}\"\n", c::INVALID_QUOTED_CHARACTER),
        ("'a\u{1b}'\n", c::INVALID_QUOTED_CHARACTER),
        ("k: \"a\n\t\n b\"\n", c::QUOTED_SCALAR_INDENTATION),
        ("k: 'a\n \t\n\t\n b'\n", c::QUOTED_SCALAR_INDENTATION),
        ("\"\\q\"\n", c::INVALID_ESCAPE),
        ("\"\\uD800\"\n", c::INVALID_ESCAPE),
        ("k: \"a\nb\"\n", c::QUOTED_SCALAR_INDENTATION),
        ("& a\n", c::INVALID_ANCHOR),
        ("* a\n", c::INVALID_ANCHOR),
        ("&a[ b\n", c::INVALID_ANCHOR),
        ("!!\n", c::INVALID_TAG),
        ("!a\"b\" c\n", c::INVALID_TAG),
        ("!<> a\n", c::INVALID_TAG),
        ("&a *b\n", c::ALIAS_WITH_PROPERTIES),
        ("!!str\n*b\n", c::ALIAS_WITH_PROPERTIES),
        ("&a &b c\n", c::DUPLICATE_PROPERTY),
        ("!a !b c\n", c::DUPLICATE_PROPERTY),
        ("&a\n&b c\n", c::DUPLICATE_PROPERTY),
        ("&a\n&b\nc\n", c::DUPLICATE_PROPERTY),
        ("a:\n\tb: c\n", c::TAB_INDENTATION),
        ("- a\n\t- b\n", c::TAB_INDENTATION),
        ("-\t- a\n", c::TAB_INDENTATION),
        ("\tk: v\n", c::TAB_INDENTATION),
        ("a:\n  b: 1\n\t c: 2\n", c::TAB_INDENTATION),
        ("'a' b\n", c::UNEXPECTED_CONTENT),
        ("- 'a' b\n", c::UNEXPECTED_CONTENT),
        ("a: 'b' c\n", c::UNEXPECTED_CONTENT),
        ("\"a\"#c\n", c::UNEXPECTED_CONTENT),
        ("a: 1\nb\n", c::MISSING_MAPPING_COLON),
        ("a: 1\n&x\n", c::MISSING_MAPPING_COLON),
        ("a: |0\n  x\n", c::INVALID_BLOCK_HEADER),
        ("a: |++\n", c::INVALID_BLOCK_HEADER),
        ("a: > x\n", c::INVALID_BLOCK_HEADER),
        ("a: >#c\n", c::INVALID_BLOCK_HEADER),
        ("a: |\n   \n  x\n", c::BLOCK_SCALAR_INDENTATION),
        ("a: |\n\t\nb: 1\n", c::TAB_INDENTATION),
        ("[a, b\n", c::UNTERMINATED_FLOW_COLLECTION),
        ("{a: [b\n---\n", c::UNTERMINATED_FLOW_COLLECTION),
        ("k: [a,\nb]\n", c::INVALID_INDENTATION),
        ("[a\n b\n# c\n c]\n", c::MISSING_FLOW_SEPARATOR),
        (r#"{a: 1 "b": 2}"#, c::MISSING_FLOW_SEPARATOR),
        ("[, a]\n", c::UNEXPECTED_FLOW_INDICATOR),
        ("[a, , b]\n", c::UNEXPECTED_FLOW_INDICATOR),
        ("[a}\n", c::UNEXPECTED_FLOW_INDICATOR),
        ("[a\n: b]\n", c::MULTILINE_IMPLICIT_KEY),
        ("{a: b\n c: d}\n", c::MISSING_FLOW_SEPARATOR),
        ("{a: b c: d}\n", c::MISSING_FLOW_SEPARATOR),
        ("[a\n b: c]\n", c::MULTILINE_IMPLICIT_KEY),
        ("a: 1\nb\n c: 2\n", c::MULTILINE_IMPLICIT_KEY),
        ("k: v\n%YAML 1.2\n---\n", c::DIRECTIVE_IN_DOCUMENT),
        ("a:\n  b: c\n%YAML 1.2\n", c::DIRECTIVE_IN_DOCUMENT),
        ("[&a *b]\n", c::ALIAS_WITH_PROPERTIES),
        ("[!!a !!b c]\n", c::DUPLICATE_PROPERTY),
        ("[@]\n", c::INVALID_NODE_START),
        ("[a] b\n", c::UNEXPECTED_CONTENT),
        ("a: [b]: c\n", c::BLOCK_COLLECTION_NOT_ALLOWED),
        ("[a\n]: b\n", c::MULTILINE_IMPLICIT_KEY),
        (long_key.as_str(), c::IMPLICIT_KEY_TOO_LONG),
        ("%YAML 2.0\n---\na\n", c::YAML_VERSION_UNSUPPORTED_MAJOR),
        ("%YAML 1.2\n%YAML 1.2\n---\n", c::DUPLICATE_YAML_DIRECTIVE),
        ("# a\u{7f}\n", c::INVALID_COMMENT_CHARACTER),
        ("%TAG !a! x\n%TAG !a! y\n---\n", c::DUPLICATE_DIRECTIVE),
        ("!e!x a\n", c::UNDECLARED_TAG_HANDLE),
        ("%TAG !e! x\n---\na\n--- !e!x b\n", c::UNDECLARED_TAG_HANDLE),
        ("  \u{feff}k: v\n", c::MISPLACED_BYTE_ORDER_MARK),
    ];
    for (src, code) in cases {
        assert_eq!(first_error(src), Some(*code), "{src:?}");
    }
    // Limit codes, under the limits named.
    let tiny = Limits::new()
        .max_depth(1)
        .max_scalar_len(2)
        .max_document_size(9)
        .max_documents(1);
    let limit_cases: &[(&str, Code)] = &[
        ("- - a\n", c::DEPTH_LIMIT_EXCEEDED),
        ("[[a]]\n", c::DEPTH_LIMIT_EXCEEDED),
        ("abc\n", c::SCALAR_TOO_LONG),
        ("k: 'abc'\n", c::SCALAR_TOO_LONG),
        ("|\n abc\n", c::SCALAR_TOO_LONG),
        ("[abc]\n", c::SCALAR_TOO_LONG),
        ("1234567890", c::DOCUMENT_TOO_LARGE),
        ("a\n--- b\n", c::TOO_MANY_DOCUMENTS),
    ];
    let settings = Settings::new().limits(tiny);
    for (src, code) in limit_cases {
        let evs = parse_with(src, &settings);
        check_contract(src, &evs).unwrap();
        let first = evs
            .iter()
            .find(|e| e.is_error())
            .map(|e| e.diagnostic().unwrap().code);
        assert_eq!(first, Some(*code), "{src:?}");
    }
    // Decoding codes (§5.2), from `decode`.
    let decode_cases: &[(&[u8], Code)] = &[
        (b"a\xFF", c::INVALID_UTF8),
        (b"a\x00\x00\xD8", c::INVALID_UTF16),
        (b"a\x00\x00\x00\x00\xD8\x00\x00", c::INVALID_UTF32),
    ];
    for (bytes, code) in decode_cases {
        let d = crate::decode(bytes);
        assert_eq!(d.diagnostics().first().map(|d| d.code), Some(*code));
    }
    // Every error code the parser and decoder can emit is covered above.
    // (Codes of the other layers are checked registry-wide by `oneyaml`'s
    // `tests/code_coverage.rs`, which sees every layer.)
    let emitted = parser_emitted_codes();
    assert!(emitted.contains(&c::INVALID_UTF8) && emitted.contains(&c::TAB_INDENTATION));
    for info in c::REGISTRY.iter().filter(|i| {
        i.severity == oneyaml_core::Severity::Error && !i.deprecated && emitted.contains(&i.code)
    }) {
        assert!(
            cases.iter().any(|(_, code)| *code == info.code)
                || limit_cases.iter().any(|(_, code)| *code == info.code)
                || decode_cases.iter().any(|(_, code)| *code == info.code)
                || internal_codes().contains(&info.code),
            "{} has no negative test",
            info.code
        );
    }
}

/// The codes the parser and decoder can emit: every registry constant
/// named in their library sources (`src/parser/`, `src/decode.rs`; test
/// modules cut off). Derived from the code, so a newly emitted code needs
/// a case in `every_error_code_is_reachable` without anyone updating a
/// list.
fn parser_emitted_codes() -> Vec<Code> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = vec![root.join("decode.rs")];
    for entry in std::fs::read_dir(root.join("parser")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|n| n != "tests.rs")
        {
            files.push(path);
        }
    }
    let mut text = String::new();
    for f in files {
        let src = std::fs::read_to_string(f).unwrap();
        text.push_str(src.split("#[cfg(test)]").next().unwrap_or(""));
        text.push('\n');
    }
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    c::REGISTRY
        .iter()
        .filter(|info| {
            text.match_indices(info.name).any(|(at, m)| {
                let before = at.checked_sub(1).and_then(|i| text.as_bytes().get(i));
                let after = text.as_bytes().get(at.saturating_add(m.len()));
                !before.is_some_and(|b| is_ident(*b)) && !after.is_some_and(|b| is_ident(*b))
            })
        })
        .map(|info| info.code)
        .collect()
}

/// Drains a parser to the end, as `Events` does.
fn drain<'a>(mut p: super::Parser<'a>) -> Vec<Event<'a>> {
    let mut out = Vec::new();
    loop {
        if let Some(e) = p.out.pop(p.src) {
            out.push(e.into_event());
            continue;
        }
        if matches!(p.state, super::State::Done) {
            return out;
        }
        p.fill();
    }
}

/// The release-build outcomes of the parser's `debug_assert!`
/// invariants, driven directly (no input breaks them). Their codes count
/// as reachable for `every_error_code_is_reachable`.
fn internal_codes() -> Vec<Code> {
    let mut codes = Vec::new();
    // `internal_error`: owed events, then the error, then the pending bytes
    // as skipped trivia, so the stream still tiles.
    let src = "a b\n";
    let mut p = super::Parser::new(src, &Settings::new());
    p.pos = 2;
    p.defer(EventKind::plain());
    p.internal_error("test");
    let evs = drain(p);
    let rebuilt: String = evs.iter().map(|e| e.source(src).unwrap()).collect();
    assert_eq!(rebuilt, src);
    let err = evs.iter().position(Event::is_error).unwrap();
    assert!(matches!(
        evs[err.checked_sub(1).unwrap()].kind(),
        EventKind::Scalar { .. }
    ));
    assert_eq!(evs[err].span(), crate::Span::empty(0));
    assert!(matches!(
        evs[err.checked_add(1).unwrap()].kind(),
        EventKind::Trivia(crate::event::TriviaKind::Skipped)
    ));
    codes.extend(evs.iter().filter_map(|e| e.diagnostic()).map(|d| d.code));
    // `fail_internal`: a Y9000 error with the usual repair and
    // resynchronisation, so the stream is contract-valid.
    let src = "k: v\n--- b\n";
    let mut p = super::Parser::new(src, &Settings::new());
    p.fail_internal("test", false);
    let evs = drain(p);
    check_contract(src, &evs).unwrap();
    let d = evs.iter().find_map(Event::diagnostic).unwrap();
    assert!(d.message.starts_with("internal error"), "{}", d.message);
    assert!(d.help.as_deref().is_some_and(|h| h.contains("bug")));
    codes.push(d.code);
    assert!(codes.iter().all(|c| *c == c::INTERNAL_ERROR));
    codes
}

#[test]
fn internal_invariants_report_y9000() {
    assert_eq!(internal_codes().len(), 2);
    // Only trivia can be lexed as a trivia run; the caller turns `None`
    // into a Y9000 event.
    assert_eq!(super::scan::lex_trivia("ab", 0, 2), None);
    assert_eq!(super::scan::lex_trivia(" a", 1, 1), None);
    assert!(super::scan::lex_trivia(" #c\n", 0, 4).is_some());
}

/// Never a panic, in release builds too: a non-trivia byte queued as trivia
/// becomes a Y9000 error covering the run, not a mislabelled trivia event. (In
/// debug builds the `debug_assert!` fires instead; run with `--release`.)
#[cfg(not(debug_assertions))]
#[test]
fn release_trivia_invariant_is_an_error_event() {
    let src = "ab";
    let mut out = super::Out::default();
    out.trivia(0, 2);
    let e = out.pop(src).unwrap().into_event();
    assert_eq!(e.span(), crate::Span::new(0, 2));
    assert_eq!(e.diagnostic().map(|d| d.code), Some(c::INTERNAL_ERROR));
    assert!(out.pop(src).is_none());
}

#[test]
fn recovery_resumes_at_the_next_document() {
    // The error document is repaired and closed; the next one parses.
    assert_eq!(
        tree("a: 'b' c\nd: e\n---\nf\n"),
        "+DOC | +MAP | =VAL :a | =VAL 'b | !Y0017 | -MAP | -DOC | +DOC --- | =VAL :f | -DOC"
    );
    // An error before any node of an implicit document drops the document.
    assert_eq!(tree("@a\n--- b\n"), "!Y0008 | +DOC --- | =VAL :b | -DOC");
    // A missing value is repaired right after the error (C6 R2); properties of
    // the rejected head are part of the error's bytes, those on an earlier
    // line are carried by the repair node.
    assert_eq!(
        tree("k: &a @\n"),
        "+DOC | +MAP | =VAL :k | !Y0008 | =VAL : | -MAP | -DOC"
    );
    assert_eq!(
        tree("k: &a\n  @\n"),
        "+DOC | +MAP | =VAL :k | !Y0008 | =VAL &a : | -MAP | -DOC"
    );
    // At document level a `%` line continues a plain scalar (ns-plain-char).
    assert_eq!(tree("--- a\n%b"), "+DOC --- | =VAL :a %b | -DOC");
    // Soft errors do not stop the parse.
    assert_eq!(
        tree("%YAML 2.0\n--- a"),
        "!Y1002 | +DOC --- | =VAL :a | -DOC"
    );
}

#[test]
fn errors_cover_one_line_and_skipped_lines_follow() {
    let src = "a: 'b' c d\n  e\nf\n";
    let evs = events(src);
    let err = evs.sourced().find(|e| e.is_error()).unwrap();
    assert_eq!(err.text(), Some("c d"));
    let skipped: Vec<&str> = evs
        .sourced()
        .filter(|e| {
            matches!(
                e.kind(),
                EventKind::Trivia(crate::event::TriviaKind::Skipped)
            )
        })
        .map(|e| e.text().unwrap())
        .collect();
    assert_eq!(skipped, ["  e", "f"]);
}

// ---------------------------------------------------------------- recovery

#[test]
fn block_scalars() {
    assert_eq!(
        tree("a: |\n  x\n   y\n\n  z\nb: >-\n  p\n  q\n\n  r\n"),
        "+DOC | +MAP | =VAL :a | =VAL |x\\n y\\n\\nz\\n | =VAL :b | =VAL >p q\\nr | -MAP | -DOC"
    );
    assert_eq!(
        tree("- |+\n  k\n\n- |2-\n   i\n"),
        "+DOC | +SEQ | =VAL |k\\n\\n | =VAL | i | -SEQ | -DOC"
    );
    assert_eq!(tree("--- |\n"), "+DOC --- | =VAL | | -DOC");
    assert_eq!(
        tree("--- >\nfolded\ntext\n"),
        "+DOC --- | =VAL >folded text\\n | -DOC"
    );
    assert_eq!(
        tree("k: !!str &a | # c\n  v\n# after\n"),
        "+DOC | +MAP | =VAL :k | =VAL &a <tag:yaml.org,2002:str> |v\\n | -MAP | -DOC"
    );
    assert_eq!(
        tree("k: &a\n  |\n  v"),
        "+DOC | +MAP | =VAL :k | =VAL &a |v\\n | -MAP | -DOC"
    );
    // A block scalar is never an implicit key.
    assert_eq!(first_error("|\n x\n: v\n"), Some(c::CONTENT_AFTER_DOCUMENT));
}

#[test]
fn block_scalar_header_payload_matches() {
    let src = "k: >2+\n   a\n";
    let evs = events(src);
    let s = evs.sourced().find(|e| e.block_header().is_some()).unwrap();
    let h = s.block_header().unwrap();
    assert_eq!((h.indent_indicator, h.content_indent), (Some(2), 2));
    assert_eq!(&src[h.span.start..h.span.end], ">2+");
    assert_eq!(s.text(), Some("   a\n"));
    assert_eq!(s.value().unwrap(), " a\n");
}

#[test]
fn flow_collections() {
    assert_eq!(
        tree("[a, 'b', \"c\", [d], {e: f}]"),
        "+DOC | +SEQ [] | =VAL :a | =VAL 'b | =VAL \"c | +SEQ [] | =VAL :d | -SEQ | +MAP {} | =VAL :e | =VAL :f | -MAP | -SEQ | -DOC"
    );
    assert_eq!(
        tree("{a, b: , : c, ? d}"),
        "+DOC | +MAP {} | =VAL :a | =VAL : | =VAL :b | =VAL : | =VAL : | =VAL :c | =VAL :d | =VAL : | -MAP | -DOC"
    );
    assert_eq!(
        tree("[a: b, ? c, : d, \"e\":f]"),
        "+DOC | +SEQ [] | +PAIR | =VAL :a | =VAL :b | -MAP | +PAIR | =VAL :c | =VAL : | -MAP | +PAIR | =VAL : | =VAL :d | -MAP | +PAIR | =VAL \"e | =VAL :f | -MAP | -SEQ | -DOC"
    );
    assert_eq!(
        tree("[a:b, c:, http://x]"),
        "+DOC | +SEQ [] | =VAL :a:b | +PAIR | =VAL :c | =VAL : | -MAP | =VAL :http://x | -SEQ | -DOC"
    );
    assert_eq!(
        tree("[&x !!str , *x]"),
        "+DOC | +SEQ [] | =VAL &x <tag:yaml.org,2002:str> : | =ALI *x | -SEQ | -DOC"
    );
    assert_eq!(tree("[]\n"), "+DOC | +SEQ [] | -SEQ | -DOC");
    assert_eq!(tree("{ }"), "+DOC | +MAP {} | -MAP | -DOC");
}

#[test]
fn multi_line_flow_and_flow_in_block() {
    assert_eq!(
        tree("k: [ a\n  b, # c\n  {x: y}\n  ]\nl: 1"),
        "+DOC | +MAP | =VAL :k | +SEQ [] | =VAL :a b | +MAP {} | =VAL :x | =VAL :y | -MAP | -SEQ | =VAL :l | =VAL :1 | -MAP | -DOC"
    );
    assert_eq!(
        tree("{ multi\n  line: v }"),
        "+DOC | +MAP {} | =VAL :multi line | =VAL :v | -MAP | -DOC"
    );
    assert_eq!(
        tree("- [a, b]\n- {c: d}"),
        "+DOC | +SEQ | +SEQ [] | =VAL :a | =VAL :b | -SEQ | +MAP {} | =VAL :c | =VAL :d | -MAP | -SEQ | -DOC"
    );
}

#[test]
fn flow_collections_as_block_keys() {
    assert_eq!(
        tree("[a, b]: c\n{d: e}: f"),
        "+DOC | +MAP | +SEQ [] | =VAL :a | =VAL :b | -SEQ | =VAL :c | +MAP {} | =VAL :d | =VAL :e | -MAP | =VAL :f | -MAP | -DOC"
    );
    assert_eq!(
        tree("&m\n&k [x]: v"),
        "+DOC | +MAP &m | +SEQ [] &k | =VAL :x | -SEQ | =VAL :v | -MAP | -DOC"
    );
    assert_eq!(tree("&s\n[x]"), "+DOC | +SEQ [] &s | =VAL :x | -SEQ | -DOC");
}

#[test]
fn flow_recovery_closes_open_collections() {
    assert_eq!(
        tree("[a, {b: [c"),
        "+DOC | +SEQ [] | =VAL :a | +MAP {} | =VAL :b | +SEQ [] | =VAL :c | !Y0022 | -SEQ | -MAP | -SEQ | -DOC"
    );
    assert_eq!(
        tree("k: {a: \n---\nx"),
        "+DOC | +MAP | =VAL :k | +MAP {} | =VAL :a | !Y0022 | =VAL : | -MAP | -MAP | -DOC | +DOC --- | =VAL :x | -DOC"
    );
    assert_eq!(
        tree("{? [x"),
        "+DOC | +MAP {} | +SEQ [] | =VAL :x | !Y0022 | -SEQ | =VAL : | -MAP | -DOC"
    );
    assert_eq!(
        tree("[a: [b"),
        "+DOC | +SEQ [] | +PAIR | =VAL :a | +SEQ [] | =VAL :b | !Y0022 | -SEQ | -MAP | -SEQ | -DOC"
    );
}

// ---------------------------------------------------------------- error codes

#[test]
fn one_error_per_problem_and_next_document_survives() {
    // Review finding: a failed node must not be reported a second time.
    let src = "k: &a\n \t&b \"x\": y\n--- z\n";
    let evs = events(src);
    assert_eq!(evs.iter().filter(|e| e.is_error()).count(), 1);
    assert!(tree(src).ends_with("+DOC --- | =VAL :z | -DOC"));
}

#[test]
fn implicit_key_length_limit() {
    // Productions 154-155: at most 1024 characters, properties and white
    // space before `:` included; counted in characters, not bytes.
    let key = |n: usize| "k".repeat(n);
    assert_eq!(first_error(&format!("{}: v", key(1024))), None);
    assert_eq!(first_error(&format!("{} : v", key(1023))), None);
    assert_eq!(
        first_error(&format!("{}  : v", key(1023))),
        Some(c::IMPLICIT_KEY_TOO_LONG)
    );
    assert_eq!(first_error(&format!("&a {}: v", key(1021))), None);
    assert_eq!(
        first_error(&format!("&a {}: v", key(1022))),
        Some(c::IMPLICIT_KEY_TOO_LONG)
    );
    assert_eq!(first_error(&format!("{}: v", "é".repeat(1024))), None);
    assert_eq!(
        first_error(&format!("a: 1\n'{}': v", key(1023))),
        Some(c::IMPLICIT_KEY_TOO_LONG)
    );
    // Flow collections as block keys and flow pairs are limited too.
    assert_eq!(
        first_error(&format!("[{}]: v", key(1023))),
        Some(c::IMPLICIT_KEY_TOO_LONG)
    );
    assert_eq!(
        first_error(&format!("[{}: v]", key(1025))),
        Some(c::IMPLICIT_KEY_TOO_LONG)
    );
    assert_eq!(first_error(&format!("[{}: v]", key(1024))), None);
    // Flow mapping keys are not implicit-key limited (production 144).
    assert_eq!(first_error(&format!("{{{}: v}}", key(2000))), None);
    // Explicit keys are never limited.
    assert_eq!(first_error(&format!("? {}\n: v", key(2000))), None);
}

#[test]
fn tabs_in_empty_lines_of_multi_line_scalars() {
    // l-empty (production 70) needs the scalar's indentation before a tab.
    assert_eq!(
        tree("k: \"a\n  \t\n b\""),
        "+DOC | +MAP | =VAL :k | =VAL \"a\\nb | -MAP | -DOC"
    );
    assert_eq!(tree("\"a\n\t\n b\""), "+DOC | =VAL \"a\\nb | -DOC");
    assert_eq!(
        tree("k: a\n  \t\n  b"),
        "+DOC | +MAP | =VAL :k | =VAL :a\\nb | -MAP | -DOC"
    );
    // Too little indentation: the plain scalar ends before that line...
    assert_eq!(first_error("k: a\n\t\n b\n"), Some(c::INVALID_INDENTATION));
    // ...and a tab-only separator line between nodes stays valid.
    assert_eq!(first_error("k: a\n\t\nl: b\n"), None);
}

#[test]
fn quoted_values_borrow_unless_changed() {
    let value = |src: &'static str| -> bool {
        parse(src)
            .into_iter()
            .any(|e| matches!(e.value(), Some(std::borrow::Cow::Borrowed(v)) if !v.is_empty()))
    };
    assert!(value("'plain text'"));
    assert!(value("\"plain text\""));
    assert!(!value("'it''s'"));
    assert!(!value("\"a\\tb\""));
    assert!(!value("\"a\n b\""));
    assert_eq!(tree("\"a \\\\ \n b\""), "+DOC | =VAL \"a \\ b | -DOC");
    assert_eq!(tree("'a  \n b ''c'''"), "+DOC | =VAL 'a b 'c' | -DOC");
}

// ---------------------------------------------------------------- flow keys

#[test]
fn multi_line_flow_key_is_rejected_at_its_closing_bracket() {
    // T3: the contents are committed, the completing `]`/`}` is not.
    assert_eq!(
        tree("[23\n]: 42\n"),
        "+DOC | +SEQ [] | =VAL :23 | !Y0007 | -SEQ | -DOC"
    );
    assert_eq!(
        tree("k: {a: b,\n  c: d}: v\n"),
        "+DOC | +MAP | =VAL :k | +MAP {} | =VAL :a | =VAL :b | =VAL :c | =VAL :d | !Y0007 | -MAP | -MAP | -DOC"
    );
    // A multi-line flow collection that is not a key is unaffected, and so
    // is a nested one.
    assert_eq!(first_error("[a,\n b]\n"), None);
    assert_eq!(
        first_error("[[a,\n b]: c]\n"),
        Some(c::MULTILINE_IMPLICIT_KEY)
    );
}

// ---------------------------------------------------------------- more errors

/// The first error's diagnostic.
fn first_diagnostic(src: &str) -> Option<oneyaml_core::Diagnostic> {
    events(src)
        .iter()
        .find(|e| e.is_error())
        .and_then(|e| e.diagnostic().cloned())
}

#[test]
fn multi_line_flow_key_points_at_its_opening_bracket() {
    // The C2SP rule's Y0007 has a secondary label at the key's `[`/`{`.
    let d = first_diagnostic("[23\n]: 42\n").unwrap();
    assert_eq!(d.code, c::MULTILINE_IMPLICIT_KEY);
    assert_eq!(d.secondary.len(), 1);
    assert_eq!(d.secondary[0].span, oneyaml_core::Span::new(0, 1));
    let src = "k: &a {a: b,\n  c: d}: v\n";
    let d = first_diagnostic(src).unwrap();
    assert_eq!(d.code, c::MULTILINE_IMPLICIT_KEY);
    assert_eq!(d.secondary.len(), 1);
    assert_eq!(d.secondary[0].span, oneyaml_core::Span::new(6, 7));
    // Other Y0007s (not the C2SP rule) carry no such label.
    let d = first_diagnostic("[a\n: b]\n").unwrap();
    assert_eq!(d.code, c::MULTILINE_IMPLICIT_KEY);
    assert!(d.secondary.is_empty());
    let d = first_diagnostic("a\nb: c\n").unwrap();
    assert_eq!(d.code, c::MULTILINE_IMPLICIT_KEY);
    assert!(d.secondary.is_empty());
}

#[test]
fn valid_multi_line_flow_that_is_not_an_implicit_key() {
    // Regression guards for the C2SP rule: a multi-line flow collection is
    // fine as a flow mapping key (production 144), as an explicit key, and
    // as a block mapping value.
    assert_eq!(
        tree("{[a,\n b]: c}\n"),
        "+DOC | +MAP {} | +SEQ [] | =VAL :a | =VAL :b | -SEQ | =VAL :c | -MAP | -DOC"
    );
    assert_eq!(
        tree("? [a,\n b]\n: c\n"),
        "+DOC | +MAP | +SEQ [] | =VAL :a | =VAL :b | -SEQ | =VAL :c | -MAP | -DOC"
    );
    assert_eq!(
        tree("k: [a,\n  b]\nl: v\n"),
        "+DOC | +MAP | =VAL :k | +SEQ [] | =VAL :a | =VAL :b | -SEQ | =VAL :l | =VAL :v | -MAP | -DOC"
    );
}

/// A failed directive block declares nothing: its `%TAG` handles (and
/// `%YAML`) do not reach the next document (§6.8).
#[test]
fn failed_directive_block_does_not_leak_tags() {
    let src = "%TAG !e! tag:a:\n%YAML 1.2\nfoo\n---\n!e!x v\n";
    let evs = events(src);
    let codes: Vec<Code> = evs
        .iter()
        .filter_map(|e| e.diagnostic())
        .map(|d| d.code)
        .collect();
    // Y0001 for the block, then the undeclared handle; no duplicate %YAML.
    assert_eq!(
        codes,
        [c::DIRECTIVE_WITHOUT_DOCUMENT, c::UNDECLARED_TAG_HANDLE]
    );
    assert!(
        evs.sourced()
            .all(|e| e.tag().is_none_or(|t| t.resolved != "tag:a:x"))
    );
    // The same handle declared by a good block still applies.
    let src = "%TAG !e! tag:a:\n--- !e!x v\n";
    let evs = events(src);
    let tag = evs.sourced().find_map(|e| e.tag()).unwrap();
    assert_eq!(tag.resolved, "tag:a:x");
    // A failed block's %YAML does not make the next block's a duplicate.
    let src = "%YAML 1.2\nfoo\n%YAML 1.2\n--- a\n";
    let codes: Vec<Code> = events(src)
        .iter()
        .filter_map(|e| e.diagnostic())
        .map(|d| d.code)
        .collect();
    assert_eq!(codes, [c::DIRECTIVE_WITHOUT_DOCUMENT]);
}

/// Timing convention T3: a multi-line flow collection in a block key slot
/// with no `:` is reported where the scan stopped (here the end of input),
/// with a secondary label on the key.
#[test]
fn missing_colon_after_multi_line_flow_key_labels_the_key() {
    let src = "'k' : ---\n[\n  'x' : value]\n";
    let evs = events(src);
    let d = evs.iter().find_map(|e| e.diagnostic()).unwrap();
    assert_eq!(d.code, c::MISSING_MAPPING_COLON);
    assert_eq!(d.secondary.len(), 1);
    let key = d.secondary[0].span;
    assert_eq!(&src[key.start..key.end], "[\n  'x' : value]");
}

/// The `%YAML 1.0` warning (Y1006) says what happens to the document.
#[test]
fn yaml_1_0_warning_reflects_honor_version_directive() {
    let src = "%YAML 1.0\n---\na\n";
    let message = |settings: &Settings| {
        let evs = parse_with(src, settings);
        check_contract(src, &evs).unwrap();
        let d = evs.iter().find_map(|e| e.diagnostic()).unwrap().clone();
        assert_eq!(d.code, c::YAML_VERSION_1_0);
        assert!(evs.iter().filter(|e| e.is_warning()).count() == 1);
        d.message
    };
    let default = message(&Settings::new());
    assert!(default.contains("parsed as YAML 1.2"), "{default}");
    let honored = message(&Settings::new().honor_version_directive(true));
    assert!(!honored.contains("parsed as YAML 1.2"), "{honored}");
    assert!(honored.contains("honor_version_directive"), "{honored}");
    assert!(honored.contains("1.1 preset"), "{honored}");
}

/// Productions 211 and 202: any number of document prefixes may precede a
/// document, and each may start with a byte order mark, so marks in a row
/// (directly behind each other, on lines of their own, or before a later
/// document) are valid and read as the single-mark form.
#[test]
fn several_marks_before_a_document_are_valid() {
    for (src, marks) in [
        ("\u{feff}\u{feff}a: 1\n", 2),
        ("\u{feff}\u{feff}\u{feff}a: 1\n", 3),
        ("\u{feff}\n\u{feff}\na: 1\n", 2),
        ("\u{feff}\u{feff}", 2),
        ("a: 1\n...\n\u{feff}\u{feff}b: 2\n", 2),
        ("a\n...\n\u{feff}\u{feff}--- b\n", 2),
        ("a\n\u{feff}\u{feff}\n\n", 2),
    ] {
        assert_eq!(first_error(src), None, "{src:?}");
        assert_eq!(bom_trivia(src), marks, "{src:?}");
        // The same events as with the marks removed.
        let plain = src.replace('\u{feff}', "");
        assert_eq!(tree(src), tree(&plain), "{src:?}");
    }
    // The marks hold no column: the text after them starts at column 0.
    assert_eq!(
        tree("\u{feff}\u{feff}k:\n  v\n"),
        "+DOC | +MAP | =VAL :k | =VAL :v | -MAP | -DOC"
    );
    // A mark anywhere else is still misplaced.
    for src in [
        "a: 1 \u{feff}\n",
        "\u{feff}  \u{feff}a: 1\n",
        "a: 1\n \u{feff}b: 2\n",
    ] {
        assert_eq!(
            first_error(src),
            Some(c::MISPLACED_BYTE_ORDER_MARK),
            "{src:?}"
        );
    }
}

/// The `ByteOrderMark` trivia events of `src`.
fn bom_trivia(src: &str) -> usize {
    events(src)
        .iter()
        .filter(|e| {
            matches!(
                e.kind(),
                EventKind::Trivia(crate::event::TriviaKind::ByteOrderMark)
            )
        })
        .count()
}

/// §5.2, production 202: a byte order mark is trivia only at the
/// very start of a line that begins the stream or a document prefix.
#[test]
fn byte_order_marks_at_a_document_prefix_are_trivia() {
    for (src, expected, marks) in [
        ("\u{feff}a", "+DOC | =VAL :a | -DOC", 1),
        ("\u{feff}--- a\n", "+DOC --- | =VAL :a | -DOC", 1),
        ("\u{feff}%YAML 1.2\n--- a\n", "+DOC --- | =VAL :a | -DOC", 1),
        ("\u{feff}# c\n\u{feff}a\n", "+DOC | =VAL :a | -DOC", 2),
        (
            "a\n...\n\u{feff}--- b\n",
            "+DOC | =VAL :a | -DOC ... | +DOC --- | =VAL :b | -DOC",
            1,
        ),
        (
            "a\n...\n\u{feff}b\n",
            "+DOC | =VAL :a | -DOC ... | +DOC | =VAL :b | -DOC",
            1,
        ),
        // Columns count after a leading mark.
        (
            "\u{feff}k:\n  v\n",
            "+DOC | +MAP | =VAL :k | =VAL :v | -MAP | -DOC",
            1,
        ),
    ] {
        assert_eq!(tree(src), expected, "{src:?}");
        assert_eq!(bom_trivia(src), marks, "{src:?}");
    }
}

/// A mark anywhere else is a `MISPLACED_BYTE_ORDER_MARK` error
/// covering the mark; the parse goes on as if it were absent.
#[test]
fn misplaced_byte_order_marks_are_skipped_errors() {
    let e = c::MISPLACED_BYTE_ORDER_MARK;
    for (src, expected) in [
        // After white space at the start of the stream.
        (
            "  \u{feff}k: v",
            format!("!{e} | +DOC | +MAP | =VAL :k | =VAL :v | -MAP | -DOC"),
        ),
        // After `---`, `-`, `:` and properties on a line.
        ("--- \u{feff}a", format!("+DOC --- | !{e} | =VAL :a | -DOC")),
        (
            "- \u{feff}x",
            format!("+DOC | +SEQ | !{e} | =VAL :x | -SEQ | -DOC"),
        ),
        (
            "k: \u{feff}v",
            format!("+DOC | +MAP | =VAL :k | !{e} | =VAL :v | -MAP | -DOC"),
        ),
        ("&a \u{feff}v", format!("+DOC | !{e} | =VAL &a :v | -DOC")),
        (
            "&a \u{feff} !!str \u{feff}\u{feff} v",
            format!("+DOC | !{e} | !{e} | !{e} | =VAL &a <tag:yaml.org,2002:str> :v | -DOC"),
        ),
        (
            "&a \u{feff} : v",
            format!("+DOC | +MAP | !{e} | =VAL &a : | =VAL :v | -MAP | -DOC"),
        ),
        (
            "&a \u{feff}\nk: v",
            format!("+DOC | !{e} | +MAP &a | =VAL :k | =VAL :v | -MAP | -DOC"),
        ),
        // Not at the start of a line inside a document.
        (
            "a:\n  b\n  \u{feff}\n",
            format!("+DOC | +MAP | =VAL :a | =VAL :b | !{e} | -MAP | -DOC"),
        ),
        // After indentation inside a document (at column 0 a mark ends
        // the document instead: `a_mark_after_a_document_starts_a_prefix`).
        (
            "a:\n  x: 1\n  \u{feff}b: 2\n",
            format!(
                "+DOC | +MAP | =VAL :a | +MAP | =VAL :x | =VAL :1 | !{e} | =VAL :b | =VAL :2 | -MAP | -MAP | -DOC"
            ),
        ),
        // In a flow collection.
        (
            "[a, \u{feff}b: c]",
            format!(
                "+DOC | +SEQ [] | =VAL :a | !{e} | +PAIR | =VAL :b | =VAL :c | -MAP | -SEQ | -DOC"
            ),
        ),
        // Between directives and `---`, and after `...` on its line.
        (
            "%YAML 1.2\n\u{feff}--- a\n",
            format!("!{e} | +DOC --- | =VAL :a | -DOC"),
        ),
        (
            "a\n... \u{feff}\n",
            format!("+DOC | =VAL :a | -DOC ... | !{e}"),
        ),
    ] {
        assert_eq!(tree(src), expected, "{src:?}");
        assert_eq!(bom_trivia(src), 0, "{src:?}");
    }
}

/// The line's columns and leading spaces are counted without a
/// skipped mark (the parser used to keep the spaces before a mark but
/// count columns from after it).
#[test]
fn a_skipped_mark_is_no_column() {
    let e = c::MISPLACED_BYTE_ORDER_MARK;
    // `k` stays in the nested mapping: indented by 2, not 5 or 0.
    assert_eq!(
        tree("a:\n  x: 1\n  \u{feff}k: v\n"),
        format!(
            "+DOC | +MAP | =VAL :a | +MAP | =VAL :x | =VAL :1 | !{e} | =VAL :k | =VAL :v | -MAP | -MAP | -DOC"
        )
    );
    // Spaces on both sides of the mark are indentation: 4 here.
    assert_eq!(
        tree("a:\n  x:\n  \u{feff}  k: v\n"),
        format!(
            "+DOC | +MAP | =VAL :a | +MAP | =VAL :x | !{e} | +MAP | =VAL :k | =VAL :v | -MAP | -MAP | -MAP | -DOC"
        )
    );
    // A compact mapping after `- ` starts at column 2.
    assert_eq!(
        tree("- \u{feff}k: v\n  l: w\n"),
        format!(
            "+DOC | +SEQ | !{e} | +MAP | =VAL :k | =VAL :v | =VAL :l | =VAL :w | -MAP | -SEQ | -DOC"
        )
    );
    // In flow context, spaces after a leading mark still indent the line.
    assert_eq!(
        tree("k: [a,\n\u{feff} b]\n"),
        format!("+DOC | +MAP | =VAL :k | +SEQ [] | =VAL :a | !{e} | =VAL :b | -SEQ | -MAP | -DOC")
    );
}

/// Production 211, §9.1.1: a mark at the start of a line after
/// a complete document without `...` ends it and starts a document prefix;
/// only a document starting with `---` (or nothing) may follow.
#[test]
fn a_mark_after_a_document_starts_a_prefix() {
    for (src, expected) in [
        (
            "--- a\n\u{feff}--- b\n",
            "+DOC --- | =VAL :a | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        (
            "a\n\u{feff}--- b\n",
            "+DOC | =VAL :a | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        (
            "--- a\n\u{feff}# c\n--- b\n",
            "+DOC --- | =VAL :a | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        ("a\n\u{feff}", "+DOC | =VAL :a | -DOC"),
        ("a\n\u{feff}\n\u{feff}\n\n", "+DOC | =VAL :a | -DOC"),
        // Open block collections and empty nodes close before the mark.
        (
            "k:\n  - x\n\u{feff}--- b\n",
            "+DOC | +MAP | =VAL :k | +SEQ | =VAL :x | -SEQ | -MAP | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        (
            "k:\n\u{feff}--- b\n",
            "+DOC | +MAP | =VAL :k | =VAL : | -MAP | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        (
            "k: |\n  x\n\u{feff}--- b\n",
            "+DOC | +MAP | =VAL :k | =VAL |x\\n | -MAP | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        (
            "--- |\nx\n\u{feff}--- b\n",
            "+DOC --- | =VAL |x\\n | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        (
            "---\n\u{feff}--- b\n",
            "+DOC --- | =VAL : | -DOC | +DOC --- | =VAL :b | -DOC",
        ),
        // Production 211: `...` after the prefix is
        // `l-document-suffix` between documents, after which
        // `l-any-document` (a bare one too) may follow.
        ("a\n\u{feff}...\n", "+DOC | =VAL :a | -DOC"),
        (
            "a\n\u{feff}...\nb\n",
            "+DOC | =VAL :a | -DOC | +DOC | =VAL :b | -DOC",
        ),
    ] {
        assert_eq!(tree(src), expected, "{src:?}");
        assert_eq!(first_error(src), None, "{src:?}");
    }
    // One mark per line: each line starts a prefix. (Two marks on one line
    // are one too many, production 202; see `a_second_mark_in_a_prefix_is_misplaced`.)
    assert_eq!(bom_trivia("a\n\u{feff}\n\u{feff}\n\n"), 2);
    // A bare document or a directive after such a prefix keeps its error
    // (the document before was not ended with `...`).
    for (src, code) in [
        ("a\n\u{feff}b\n", c::CONTENT_AFTER_DOCUMENT),
        ("k: v\n\u{feff}b: 2\n", c::CONTENT_AFTER_DOCUMENT),
        ("a\n\u{feff}%YAML 1.2\n--- b\n", c::DIRECTIVE_IN_DOCUMENT),
        ("a\n\u{feff}... x\n", c::CONTENT_AFTER_DOCUMENT),
    ] {
        assert_eq!(codes(src), vec![code], "{src:?}");
    }
    // Recovery resumes at such a mark after an error.
    assert_eq!(
        tree("a: @\n\u{feff}--- b\n"),
        "+DOC | +MAP | =VAL :a | !Y0008 | =VAL : | -MAP | -DOC | +DOC --- | =VAL :b | -DOC"
    );
}

/// Production 27 (`nb-char` excludes the mark): a mark inside a
/// comment or block scalar content is reported with its own span, right
/// after the token, which keeps its bytes; the block scalar's value keeps
/// the mark. Quoted scalars may hold one (`nb-json`).
#[test]
fn marks_inside_comments_and_block_scalars() {
    let e = c::MISPLACED_BYTE_ORDER_MARK;
    for (src, expected, marks) in [
        (
            "k: v # a\u{feff}b\u{feff}\n",
            format!("+DOC | +MAP | =VAL :k | =VAL :v | !{e} | !{e} | -MAP | -DOC"),
            vec![8, 12],
        ),
        (
            "\u{feff}# \u{feff}\na\n",
            format!("!{e} | +DOC | =VAL :a | -DOC"),
            vec![5],
        ),
        (
            "[a, # \u{feff}\n b]\n",
            format!("+DOC | +SEQ [] | =VAL :a | !{e} | =VAL :b | -SEQ | -DOC"),
            vec![6],
        ),
        (
            "k: |\n  x\u{feff}y\n",
            format!("+DOC | +MAP | =VAL :k | =VAL |x\u{feff}y\\n | !{e} | -MAP | -DOC"),
            vec![8],
        ),
        (
            "k: > # \u{feff}\n  \u{feff}\n",
            format!("+DOC | +MAP | =VAL :k | =VAL >\u{feff}\\n | !{e} | !{e} | -MAP | -DOC"),
            vec![7, 13],
        ),
        (
            "k: \"a\u{feff}b\"\nl: 'c\u{feff}'\n",
            "+DOC | +MAP | =VAL :k | =VAL \"a\u{feff}b | =VAL :l | =VAL 'c\u{feff} | -MAP | -DOC"
                .to_owned(),
            vec![],
        ),
    ] {
        assert_eq!(tree(src), expected, "{src:?}");
        let spans: Vec<usize> = events(src)
            .iter()
            .filter_map(|ev| ev.diagnostic())
            .map(|d| {
                assert_eq!(d.code, e, "{src:?}");
                assert_eq!(d.primary.span.len(), 3, "{src:?}");
                d.primary.span.start
            })
            .collect();
        assert_eq!(spans, marks, "{src:?}");
    }
}

/// A mark right after a plain scalar's text ends the scalar (it
/// is no `ns-char`); the text after it is new content, so a further error
/// follows (documented on `MISPLACED_BYTE_ORDER_MARK`).
#[test]
fn a_mark_ends_a_plain_scalar() {
    assert_eq!(
        codes("k: a\u{feff}b\n"),
        vec![c::MISPLACED_BYTE_ORDER_MARK, c::UNEXPECTED_CONTENT]
    );
}

/// The error after a document ended by a mark points at
/// that invisible mark, and recovery reads what follows as the next (bare)
/// document instead of dropping it.
#[test]
fn a_bare_document_after_a_prefix_is_reported_and_kept() {
    let y4 = c::CONTENT_AFTER_DOCUMENT;
    for (src, expected) in [
        (
            "a: 1\n\u{feff}b: 2\nc: 3\n",
            format!(
                "+DOC | +MAP | =VAL :a | =VAL :1 | -MAP | -DOC | !{y4} | +DOC | +MAP | =VAL :b | =VAL :2 | =VAL :c | =VAL :3 | -MAP | -DOC"
            ),
        ),
        (
            "k:\n- a\n\u{feff}- b\n",
            format!(
                "+DOC | +MAP | =VAL :k | +SEQ | =VAL :a | -SEQ | -MAP | -DOC | !{y4} | +DOC | +SEQ | =VAL :b | -SEQ | -DOC"
            ),
        ),
    ] {
        assert_eq!(tree(src), expected, "{src:?}");
    }
    // The primary label starts at the marks that start the offending line
    // (so its location counts no invisible character); a secondary label
    // and the help name the mark that ended the document.
    for (src, mark, primary, directive) in [
        ("a: 1\n\u{feff}b: 2\n", 5, (5, 12), false),
        ("a\n\u{feff}# c\nb\n", 2, (9, 10), false),
        ("a\n\u{feff}\n\u{feff}b\n", 2, (6, 10), false),
        ("a\n\u{feff}%YAML 1.2\n", 2, (2, 14), true),
    ] {
        let evs = events(src);
        let d = evs
            .iter()
            .find_map(|e| e.diagnostic())
            .unwrap_or_else(|| panic!("{src:?}: no diagnostic"));
        let code = if directive {
            c::DIRECTIVE_IN_DOCUMENT
        } else {
            c::CONTENT_AFTER_DOCUMENT
        };
        assert_eq!(d.code, code, "{src:?}");
        assert_eq!(
            (d.primary.span.start, d.primary.span.end.min(primary.1)),
            primary,
            "{src:?}"
        );
        assert_eq!(d.secondary.len(), 1, "{src:?}");
        assert_eq!(d.secondary[0].span.start, mark, "{src:?}");
        assert_eq!(d.secondary[0].span.len(), 3, "{src:?}");
        assert!(d.help.as_deref().unwrap().contains("U+FEFF"), "{src:?}");
    }
}

/// A mark that makes another construct fail is reported
/// too (`MISPLACED_BYTE_ORDER_MARK`, its own span), right after that
/// construct's error, whose label comes first (the first error event
/// is the first diagnostic by position); a mark between an implicit key
/// and its `:` is skipped as if absent, with only its own error.
#[test]
fn a_mark_is_reported_with_the_error_it_causes() {
    let e = c::MISPLACED_BYTE_ORDER_MARK;
    for (src, want) in [
        (
            "a: 1\nSecond \u{feff}x: y\n",
            vec![c::MISSING_MAPPING_COLON, e],
        ),
        ("- | \u{feff}# x\n  a\n", vec![c::INVALID_BLOCK_HEADER, e]),
        ("- |\u{feff}\n  a\n", vec![c::INVALID_BLOCK_HEADER, e]),
        (
            "- |\n    \n \u{feff} a\n",
            vec![c::BLOCK_SCALAR_INDENTATION, e],
        ),
        (
            "%YAML \u{feff}1.2\n--- a\n",
            vec![c::MALFORMED_DIRECTIVE, e],
        ),
        (
            "%YAML 1.2\u{feff}\n--- a\n",
            vec![c::MALFORMED_DIRECTIVE, e],
        ),
        ("%FOO \u{feff}bar\n--- a\n", vec![c::MALFORMED_DIRECTIVE, e]),
        ("'k' \u{feff}: v\n", vec![e]),
        ("*x \u{feff}: v\n", vec![e]),
        ("a: 1\nb \u{feff} : 2\n", vec![e]),
    ] {
        assert_eq!(codes(src), want, "{src:?}");
        let evs = events(src);
        let d = evs
            .iter()
            .filter_map(|x| x.diagnostic())
            .find(|d| d.code == e)
            .unwrap();
        assert_eq!(
            src.get(d.primary.span.start..d.primary.span.end),
            Some("\u{feff}"),
            "{src:?}"
        );
    }
    assert_eq!(
        tree("'k' \u{feff}: v\n"),
        format!("+DOC | +MAP | =VAL 'k | !{e} | =VAL :v | -MAP | -DOC")
    );
    assert_eq!(
        tree("a: 1\nb \u{feff} : 2\n"),
        format!("+DOC | +MAP | =VAL :a | =VAL :1 | =VAL :b | !{e} | =VAL :2 | -MAP | -DOC")
    );
    // A reserved directive's parameters are `ns-char`s too.
    assert_eq!(
        codes("%FOO bar\n--- a\n"),
        vec![c::RESERVED_DIRECTIVE_IGNORED]
    );
}

/// The byte-level pre-check of a comment never hides a character that is not
/// printable, at any length and position.
#[test]
fn the_comment_pre_check_agrees_with_the_character_check() {
    let chars = [
        'a',
        ' ',
        '\t',
        '\u{1}',
        '\u{1f}',
        '\u{7f}',
        '\u{80}',
        '\u{85}',
        '\u{9f}',
        '\u{a0}',
        '\u{e9}',
        '\u{feff}',
        '\u{fffe}',
        '\u{ffff}',
        '\u{1F600}',
    ];
    for len in 0..12 {
        for pos in 0..len.max(1) {
            for c in chars {
                let mut s = String::from("#");
                s.push_str(&"a".repeat(pos));
                s.push(c);
                s.push_str(&"b".repeat(len - pos));
                let got =
                    super::comment_char_diagnostic(&s, 0, s.len()).map(|d| d.primary.span.start);
                let want = s
                    .char_indices()
                    .find(|(_, ch)| !super::scan::is_printable(*ch))
                    .map(|(k, _)| k);
                assert_eq!(got, want, "{s:?}");
            }
        }
    }
}
