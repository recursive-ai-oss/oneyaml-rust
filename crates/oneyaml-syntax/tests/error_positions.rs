//! Where an error is reported, and the inputs that must be errors (or must
//! not be): the primary label starts at the first character that cannot
//! belong, not at the start of the construct around it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use oneyaml_core::{Settings, code as c};

/// `(input, code, start of the primary label)`.
#[test]
fn the_primary_label_starts_at_the_first_character_that_cannot_belong() {
    let long_key = format!("{}: v\n", "a".repeat(1025));
    let cases: Vec<(String, oneyaml_core::Code, usize)> = vec![
        ("&a[b]\n".into(), c::INVALID_ANCHOR, 2),
        ("{:{0}}\n".into(), c::MISSING_FLOW_SEPARATOR, 2),
        ("{? a:{}}\n".into(), c::MISSING_FLOW_SEPARATOR, 5),
        ("{a:{}}\n".into(), c::MISSING_FLOW_SEPARATOR, 3),
        ("{!t :{}}\n".into(), c::MISSING_FLOW_SEPARATOR, 5),
        ("a: \"\\xZZ\"\n".into(), c::INVALID_ESCAPE, 6),
        ("a: \"x\\qy\"\n".into(), c::INVALID_ESCAPE, 6),
        ("a: \"\\u12\"\n".into(), c::INVALID_ESCAPE, 8),
        ("a: \"abc\n".into(), c::UNTERMINATED_QUOTED_SCALAR, 8),
        ("a: 'abc".into(), c::UNTERMINATED_QUOTED_SCALAR, 7),
        ("'a''b".into(), c::UNTERMINATED_QUOTED_SCALAR, 5),
        ("a: |0\n x\n".into(), c::INVALID_BLOCK_HEADER, 4),
        ("a: b: c\n".into(), c::INLINE_COLON_IN_PLAIN_VALUE, 5),
        (long_key, c::IMPLICIT_KEY_TOO_LONG, 1026),
        ("a: 1\nb\nc: 2\n".into(), c::MISSING_MAPPING_COLON, 6),
        ("a: 1\n- b\n".into(), c::INVALID_INDENTATION, 6),
        ("a: b\r c: d\r".into(), c::MULTILINE_IMPLICIT_KEY, 8),
        ("a: b\r\n c: d\r\n".into(), c::MULTILINE_IMPLICIT_KEY, 9),
        ("a: b\n c: d\n".into(), c::MULTILINE_IMPLICIT_KEY, 8),
        ("a: &x &y b\n".into(), c::DUPLICATE_PROPERTY, 6),
        ("!a%2 x\n".into(), c::INVALID_TAG, 4),
        ("!<a%zz> x\n".into(), c::INVALID_TAG, 4),
        ("- !<!> foo\n".into(), c::INVALID_TAG, 5),
        ("- !<$:?> bar\n".into(), c::INVALID_TAG, 4),
        (
            "a: 1\n!!map\n!!str b: c\n".into(),
            c::MISSING_MAPPING_COLON,
            10,
        ),
        ("a: \"x\n\"\nb: 1\n".into(), c::QUOTED_SCALAR_INDENTATION, 6),
        ("- 'x\n'\n".into(), c::QUOTED_SCALAR_INDENTATION, 5),
        (
            "a:\n  b: \"x\n  y\"\n".into(),
            c::QUOTED_SCALAR_INDENTATION,
            13,
        ),
        ("# c\u{7f}\na: 1\n".into(), c::INVALID_COMMENT_CHARACTER, 3),
        ("# c\u{9f}\na: 1\n".into(), c::INVALID_COMMENT_CHARACTER, 3),
        ("a: 1 #\u{1}\n".into(), c::INVALID_COMMENT_CHARACTER, 6),
        (
            "a: [1, # x\u{80}\n 2]\n".into(),
            c::INVALID_COMMENT_CHARACTER,
            10,
        ),
        (
            "k: | # c\u{7f}\n a\n".into(),
            c::INVALID_COMMENT_CHARACTER,
            8,
        ),
        (
            "%YAML 1.2\n%YAML 1.2\n---\n".into(),
            c::DUPLICATE_YAML_DIRECTIVE,
            10,
        ),
    ];
    for (src, code, start) in cases {
        let d = common::first_error(&src, &Settings::new()).unwrap_or_else(|| panic!("{src:?}"));
        assert_eq!(
            (d.code, d.primary.span.start),
            (code, start),
            "{src:?}: {d:?}"
        );
    }
}

/// A byte order mark is no column for characters, so the same text with and
/// without it reports the same column.
#[test]
fn a_leading_byte_order_mark_does_not_move_the_reported_column() {
    use oneyaml_core::{ColumnUnit, LineIndex};
    let with = "\u{feff}a: \"x\"y\n";
    let without = "a: \"x\"y\n";
    let at = |src: &str| {
        let d = common::first_error(src, &Settings::new()).unwrap();
        let lc = LineIndex::new(src)
            .line_col(d.primary.span.start, ColumnUnit::Char)
            .unwrap();
        (lc.line, lc.column)
    };
    assert_eq!(at(with), at(without));
}

/// A second `%YAML` has its own code; a repeated `%TAG` keeps the old one.
#[test]
fn a_second_yaml_directive_and_a_repeated_tag_directive_have_their_own_codes() {
    let d = common::first_error("%YAML 1.2\n%YAML 1.2\n---\n", &Settings::new()).unwrap();
    assert_eq!(d.code, c::DUPLICATE_YAML_DIRECTIVE);
    let d = common::first_error("%TAG !a! x\n%TAG !a! y\n---\n", &Settings::new()).unwrap();
    assert_eq!(d.code, c::DUPLICATE_DIRECTIVE);
}

/// A flow collection glued to a property or to the `:` of a plain, explicit
/// or empty key is an error; after a quoted (JSON-like) key it is not.
#[test]
fn flow_collections_need_separation() {
    for src in [
        "!a[b]\n",
        "k: !a{b: c}\n",
        "{a:{}}\n",
        "{:{0}}\n",
        "{? a:{}}\n",
    ] {
        assert!(
            common::first_error(src, &Settings::new()).is_some(),
            "{src:?}"
        );
    }
    for src in ["{\"a\":{}}\n", "{a: {}}\n", "!a [b]\n", "{'a':[]}\n"] {
        assert!(
            common::first_error(src, &Settings::new()).is_none(),
            "{src:?}"
        );
    }
}

/// Content after a plain root is an error inside the still-open document:
/// the document ends only at its end marker or the next document.
#[test]
fn a_document_stays_open_when_content_follows_a_plain_root() {
    let t = common::tree("word1  # comment\nword2\n", &Settings::new());
    let error = t.find('!').expect(&t);
    let end = t.find("-DOC").expect(&t);
    assert!(error < end, "{t}");
    let t = common::tree(
        "---\nscalar1 # comment\n%YAML 1.2\n---\nscalar2\n",
        &Settings::new(),
    );
    let error = t.find('!').expect(&t);
    let end = t.find("-DOC").expect(&t);
    assert!(error < end, "{t}");
}

/// A line of only a tab after a block scalar, at the end, is trailing space.
#[test]
fn a_tab_only_line_after_a_block_scalar_is_not_an_error() {
    for src in [
        "k: |\n a\n\t\n",
        "k: |\n a\n\t\n# c\n",
        "k: |\n a\n \t\n",
        // Before a document marker too.
        "k: |\n a\n\t\n---\nb\n",
        "k: |\n a\n\t\n...\n",
    ] {
        assert!(
            common::first_error(src, &Settings::new()).is_none(),
            "{src:?}"
        );
    }
    // Content after it is still content.
    assert!(common::first_error("k: |\n a\n\t\nz\n", &Settings::new()).is_some());
}

/// `%`-escapes in a tag: ASCII ones are decoded, except `%25`; those of
/// non-ASCII bytes and `%25` stay as written, because tag characters are
/// compared as presented and a decoded `%25` would equal them.
#[test]
fn tag_escapes_of_non_ascii_bytes_are_kept() {
    let t = common::tree("%TAG !e! tag:x,1:\n--- !e!%C3%A9 a\n", &Settings::new());
    assert!(t.contains("<tag:x,1:%C3%A9>"), "{t}");
    let t = common::tree("!a%21b c\n", &Settings::new());
    assert!(t.contains("<!a!b>"), "{t}");
    // `%25` is kept too, so it never reads as the kept escapes it spells.
    let spelled = common::tree("!a%25C3%25A9 c\n", &Settings::new());
    let kept = common::tree("!a%C3%A9 c\n", &Settings::new());
    assert!(spelled.contains("<!a%25C3%25A9>"), "{spelled}");
    assert!(kept.contains("<!a%C3%A9>"), "{kept}");
}

/// Comment text is printable characters: a tab, U+0085 and the rest of the
/// printable range are fine.
#[test]
fn printable_characters_in_a_comment_are_accepted() {
    for src in [
        "# a\tb\na: 1\n",
        "# \u{85} \u{a0} \u{e9} \u{1F600}\na: 1\n",
        "a: 1 # \u{fffd}\n",
        "a: [1, # x\n 2]\n",
    ] {
        assert!(
            common::first_error(src, &Settings::new()).is_none(),
            "{src:?}"
        );
    }
}

/// The errors in one comment come in the order of the characters they
/// report, whichever kind they are, in block and in flow context.
#[test]
fn errors_in_one_comment_keep_the_order_of_their_characters() {
    for src in [
        "a: 1 # \u{7f} \u{feff}\n",
        "a: 1 # \u{feff} \u{7f}\n",
        "a: [1, # \u{7f} \u{feff}\n 2]\n",
        "a: [1, # \u{feff} \u{7f}\n 2]\n",
        "a: | # \u{7f} \u{feff}\n x\n",
        "a: | # \u{feff} \u{7f}\n x\n",
    ] {
        let starts: Vec<usize> = common::diagnostics(src, &Settings::new())
            .iter()
            .map(|d| d.primary.span.start)
            .collect();
        assert_eq!(starts.len(), 2, "{src:?}");
        assert!(
            starts.windows(2).all(|w| w[0] < w[1]),
            "{src:?}: {starts:?}"
        );
    }
}
