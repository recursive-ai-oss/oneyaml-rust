//! A new or moved block scalar appended after the last entry's footer
//! comment lines is written as a block scalar, with the footer lines kept
//! above it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Patch, Stream, path};

const ADD_MULTI: &str = r#"[{"op":"add","path":"/n","value":"multi\nline"}]"#;

fn patched(src: &str, patch: &str) -> String {
    let mut s = Stream::parse(src).unwrap();
    s.apply_patch(0, &Patch::from_json(patch).unwrap()).unwrap();
    s.to_string()
}

#[test]
fn append_after_footer_lines_writes_a_literal_block() {
    assert_eq!(
        patched("k:\n  - a\n  # note\n", ADD_MULTI),
        "k:\n  - a\n  # note\nn: |-\n  multi\n  line\n"
    );
    // Several comment lines and a blank line between stay together.
    assert_eq!(
        patched("k:\n  - a\n\n  # one\n  # two\n", ADD_MULTI),
        "k:\n  - a\n\n  # one\n  # two\nn: |-\n  multi\n  line\n"
    );
    // Without a final line break after the comment.
    assert_eq!(
        patched("k:\n  - a\n  # note", ADD_MULTI),
        "k:\n  - a\n  # note\nn: \"multi\\nline\""
    );
}

#[test]
fn append_after_footer_lines_of_a_nested_collection() {
    let mut d = Document::parse("a:\n  b:\n    - x\n    # c\nz: 1\n").unwrap();
    d.set_path(&path!["a", "n"], "multi\nline").unwrap();
    assert_eq!(
        d.to_string(),
        "a:\n  b:\n    - x\n    # c\n  n: |-\n    multi\n    line\nz: 1\n"
    );
    let mut d = Document::parse("k:\n  - a\n  # note\n").unwrap();
    d.set_path(&path!["n"], "multi\nline").unwrap();
    assert_eq!(
        d.to_string(),
        "k:\n  - a\n  # note\nn: |-\n  multi\n  line\n"
    );
}

#[test]
fn line_breaks_of_the_document_are_kept() {
    for nl in ["\r\n", "\r"] {
        let src = "k:\n  - a\n  # note\n".replace('\n', nl);
        let want = "k:\n  - a\n  # note\nn: |-\n  multi\n  line\n".replace('\n', nl);
        assert_eq!(patched(&src, ADD_MULTI), want, "{nl:?}");
    }
}

/// Moving a block scalar to the end, past the footer lines of the last
/// entry, keeps it a block scalar.
#[test]
fn moving_a_literal_past_footer_lines_keeps_the_block() {
    let src = "a: |\n  t\n  u\nk:\n  - a\n  # note\n";
    assert_eq!(
        patched(src, r#"[{"op":"move","from":"/a","path":"/z"}]"#),
        "k:\n  - a\n  # note\nz: |\n  t\n  u\n"
    );
}

/// A comment before a `---` is the next document's header, so the insert
/// leaves it where it is, below the new entry; a literal block above an
/// indented comment would read it as its text, so the string is quoted.
#[test]
fn a_comment_before_a_marker_stays_the_next_headers() {
    assert_eq!(
        patched("k:\n  - a\n  # note\n---\nx: 1\n", ADD_MULTI),
        "k:\n  - a\nn: \"multi\\nline\"\n  # note\n---\nx: 1\n"
    );
}

#[test]
fn the_edited_text_reads_back() {
    let out = patched("k:\n  - a\n  # note\n", ADD_MULTI);
    let again = Stream::parse(out.clone()).unwrap();
    assert_eq!(again.to_string(), out);
    let v: oneyaml::Value = oneyaml::from_str(&out).unwrap();
    assert_eq!(v["n"].as_str(), Some("multi\nline"));
}
