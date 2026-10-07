//! Render goldens: the rustc-style snippets of every polished
//! input, one case per code range, and the line-break, wide-character,
//! UTF-16 and long-line layouts, against `tests/render_golden.snapshot`.
//!
//! **Approved by the owner (2026-09-30)**: changes to rendered output are
//! reviewed changes. To regenerate after an approved
//! change, run with `ONEYAML_BLESS=1` and review the diff. Each case also
//! records the primary position in UTF-16 columns (the LSP unit) from
//! `LineIndex`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;

use oneyaml::{
    ColumnUnit, Diagnostic, Document, Label, LineIndex, Render, Settings, Span, Value, code,
};

const SNAPSHOT: &str = "tests/render_golden.snapshot";

/// How a case gets its diagnostics.
enum Via {
    /// `Document::parse_recovering_with`: every diagnostic.
    Parse(Settings),
    /// A strict typed read: `Error::render`.
    Serde(fn(&str) -> oneyaml::Error),
    /// A hand-built diagnostic (layouts the parser never produces).
    Built(fn(&str) -> Diagnostic),
    /// UTF-16LE bytes of the text, decoded by `Document::from_slice_recovering`.
    Utf16,
}

fn cases() -> Vec<(&'static str, String, Via)> {
    let parse = || Via::Parse(Settings::default());
    let mut long = "key: [".to_owned();
    long.push_str(&"long, ".repeat(60));
    long.push_str("@x, ");
    long.push_str(&"tail, ".repeat(60));
    long.push_str("]\n");
    vec![
        // The polish backlog.
        ("backlog_glued_colon", "a: 1\nb:2\n".into(), parse()),
        (
            "backlog_single_quote_inside",
            "key: 'it's'\n".into(),
            parse(),
        ),
        (
            "backlog_double_quote_inside",
            "say: \"a \"b\" c\"\n".into(),
            parse(),
        ),
        (
            "backlog_comment_glued",
            "key: \"value\"# note\n".into(),
            parse(),
        ),
        (
            "backlog_windows_path",
            "path: \"C:\\path\"\n".into(),
            parse(),
        ),
        (
            "backlog_windows_path_valid_escape",
            "path: \"C:\\data\\new\"\n".into(),
            parse(),
        ),
        (
            "backlog_escaped_apostrophe",
            "q: \"it\\'s\"\n".into(),
            parse(),
        ),
        ("backlog_reserved_at", "user: @admin\n".into(), parse()),
        (
            "backlog_reserved_backtick",
            "run: `make` # build\n".into(),
            parse(),
        ),
        ("backlog_reserved_key", "@type: x\n".into(), parse()),
        ("backlog_unclosed_bracket_eof", "a: [1, 2\n".into(), parse()),
        (
            "backlog_unclosed_nested_eof",
            "a: [1, {b: 2\n".into(),
            parse(),
        ),
        (
            "backlog_unclosed_bracket_dedent",
            "a: [1, 2\nb: 3\n".into(),
            parse(),
        ),
        (
            "backlog_closer_dedent",
            "args: [\n  a,\n  b\n]\n".into(),
            parse(),
        ),
        (
            "backlog_inline_colon",
            "title: The farm: a story\n".into(),
            parse(),
        ),
        (
            "backlog_flow_windows_path",
            "k: [\"C:\\path\", b]\n".into(),
            parse(),
        ),
        ("backlog_flow_reserved", "{a: @x, b: y}\n".into(), parse()),
        ("backlog_tab_indent", "a:\n\t- b\n".into(), parse()),
        (
            "backlog_unterminated_quote",
            "a: \"abc\nb: 1\n".into(),
            parse(),
        ),
        // One per code range.
        (
            "range_schema_duplicate_key",
            "a: 1\nb: 2\nc: 3\na: 4\n".into(),
            parse(),
        ),
        (
            "range_schema_warning",
            "%YAML 1.1\n---\na: yes\n".into(),
            parse(),
        ),
        (
            "range_limits_depth",
            "a:\n  b:\n    c: 1\n".into(),
            Via::Parse(Settings::new().max_depth(2)),
        ),
        (
            "range_serde_path",
            "ports:\n  - port: 80\n  - port: 99999\n".into(),
            Via::Serde(|src| {
                #[derive(Debug, serde::Deserialize)]
                #[allow(dead_code)]
                struct Spec {
                    ports: Vec<Port>,
                }
                #[derive(Debug, serde::Deserialize)]
                #[allow(dead_code)]
                struct Port {
                    port: u16,
                }
                oneyaml::from_str::<Spec>(src).unwrap_err()
            }),
        ),
        (
            "range_serde_syntax_error",
            "a: [1, 2\n".into(),
            Via::Serde(|src| oneyaml::from_str::<Value>(src).unwrap_err()),
        ),
        // Line breaks, widths, encodings, long lines.
        ("crlf", "a: 1\r\nb:2\r\nc: 3\r\n".into(), parse()),
        ("cr_only", "a: 1\rb: [1\r".into(), parse()),
        (
            "wide_characters",
            "名前: 中文\u{1F600}: x\n".into(),
            parse(),
        ),
        (
            "combining_marks",
            "cafe\u{301}: e\u{301}: x\n".into(),
            parse(),
        ),
        ("tab_before_span", "k:\t\"C:\\p\"\n".into(), parse()),
        ("bidi_override", "k: \u{202e}abc: x\n".into(), parse()),
        ("utf16_input", "k: [\u{e9}, \u{1F600}\n".into(), Via::Utf16),
        ("long_line", long, parse()),
        (
            "multi_line_label",
            "a: |\n  x\n  y\nb: 1\n".into(),
            Via::Built(|src| {
                let end = src.find("b:").unwrap();
                Diagnostic::warning(
                    code::CUSTOM,
                    "a block scalar",
                    Label::new(Span::new(3, end), "spans three lines"),
                )
                .with_secondary(Label::new(Span::new(end, end + 1), "next key"))
            }),
        ),
        (
            "empty_input_eof",
            String::new(),
            Via::Built(|_| {
                Diagnostic::error(code::CUSTOM, "nothing here", Label::new(Span::empty(0), ""))
            }),
        ),
    ]
}

fn utf16(index: &LineIndex, span: Span) -> String {
    match index.line_col(span.start, ColumnUnit::Utf16) {
        Some(p) => format!("# utf16 {}:{}\n", p.line, p.column),
        None => "# utf16 -\n".to_owned(),
    }
}

fn render_case(src: &str, via: &Via) -> String {
    let mut out = String::new();
    let index = LineIndex::new(src);
    let diagnostics = match via {
        Via::Parse(settings) => Document::parse_recovering_with(src, settings)
            .diagnostics()
            .to_vec(),
        Via::Built(f) => vec![f(src)],
        Via::Serde(f) => {
            let err = f(src);
            out.push_str(&utf16(&index, err.span().unwrap_or_default()));
            out.push_str(&err.render(src));
            assert_eq!(err.render(src), err.render_indexed(src, &index));
            return out;
        }
        Via::Utf16 => {
            let bytes: Vec<u8> = src.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let bom = [0xff, 0xfe].into_iter().chain(bytes).collect::<Vec<u8>>();
            let parsed = Document::from_slice_recovering(&bom, &Settings::default());
            let text = parsed.value().syntax().source().to_owned();
            let index = LineIndex::new(&text);
            for d in parsed.diagnostics() {
                out.push_str(&utf16(&index, d.primary.span));
                out.push_str(&d.render_indexed(&text, &index));
            }
            return out;
        }
    };
    assert!(!diagnostics.is_empty(), "{src:?} has no diagnostics");
    for d in &diagnostics {
        out.push_str(&utf16(&index, d.primary.span));
        let rendered = d.render(src);
        assert_eq!(rendered, d.render_indexed(src, &index));
        out.push_str(&rendered);
    }
    out
}

fn render_all() -> String {
    let mut out = String::from(
        "# oneyaml rendered diagnostics. Approved baseline (2026-09-30).\n\
         # Each case: `=== name`, `# input` (Rust-escaped), `# utf16 line:col`, the snippet.\n",
    );
    for (name, src, via) in cases() {
        let _ = writeln!(out, "=== {name}");
        let _ = writeln!(out, "# input {src:?}");
        out.push_str(&render_case(&src, &via));
    }
    out
}

fn parse(snapshot: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut current: Option<(String, String)> = None;
    for line in snapshot.split_inclusive('\n') {
        if let Some(name) = line.strip_prefix("=== ") {
            if let Some((n, body)) = current.take() {
                map.insert(n, body);
            }
            current = Some((name.trim_end_matches('\n').to_owned(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
        }
    }
    if let Some((n, body)) = current {
        map.insert(n, body);
    }
    map
}

#[test]
fn rendered_diagnostics_match_the_snapshot() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    let rendered = render_all();
    if std::env::var_os("ONEYAML_BLESS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    let frozen = parse(&std::fs::read_to_string(&path).expect("tests/render_golden.snapshot"));
    let current = parse(&rendered);
    let mut diffs = Vec::new();
    for (name, body) in &current {
        match frozen.get(name) {
            Some(f) if f.as_bytes() == body.as_bytes() => {}
            Some(f) => diffs.push(format!("{name}:\n--- snapshot\n{f}--- now\n{body}")),
            None => diffs.push(format!("{name}: not in the snapshot")),
        }
    }
    for name in frozen.keys() {
        if !current.contains_key(name) {
            diffs.push(format!("{name}: in the snapshot but no longer a case"));
        }
    }
    assert!(
        diffs.is_empty(),
        "rendered output changed:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn case_names_are_unique_and_cover_every_code_range() {
    let cases = cases();
    let mut names: Vec<&str> = cases.iter().map(|c| c.0).collect();
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), n);
    let all = render_all();
    for code in ["[Y00", "[Y10", "[Y20", "[Y30"] {
        assert!(all.contains(code), "no {code}xx case");
    }
}
