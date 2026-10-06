//! Rendering: `render(src)` on `Diagnostic` and `Error` draws a
//! rustc-style snippet with carets that line up (tabs, wide and combining
//! characters, CRLF/CR), multi-line and multi-line-apart labels, spans at
//! the end of the input, and huge lines cut around the span; it never
//! panics on any span/source combination and costs the lines
//! shown, not the input.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use oneyaml::{
    Applicability, Code, Diagnostic, Document, Label, LineIndex, Render, Replacement, Span,
    Suggestion, Value, code,
};

fn diag(span: Span, label: &str) -> Diagnostic {
    Diagnostic::error(code::CUSTOM, "boom", Label::new(span, label))
}

/// The marker row right after the source row `row` (e.g. `"1 | ab"`).
fn markers_after<'a>(out: &'a str, row: &str) -> &'a str {
    let mut lines = out.lines();
    lines
        .find(|l| *l == row)
        .unwrap_or_else(|| panic!("{row:?} in\n{out}"));
    lines.next().unwrap()
}

#[test]
fn display_stays_compact() {
    let d = diag(Span::new(0, 1), "x");
    assert!(!d.to_string().contains('\n'));
    let err = oneyaml::from_str::<Value>("a: b: c\n").unwrap_err();
    assert_eq!(err.to_string().lines().count(), 1);
}

#[test]
fn layout_of_a_simple_snippet() {
    let src = "key: value\nother: 1\n";
    let d = diag(Span::new(5, 10), "this one")
        .with_help("do something")
        .with_path(oneyaml::Path::new().with_key("key"));
    assert_eq!(
        d.render(src),
        "error[Y3004]: boom\n\
         \x20--> 1:6\n\
         \x20 |\n\
         1 | key: value\n\
         \x20 |      ^^^^^ this one\n\
         \x20 |\n\
         \x20 = help: do something\n\
         \x20 = path: key\n"
    );
}

#[test]
fn carets_line_up_after_tabs_wide_and_combining_characters() {
    // A tab shows as four spaces.
    let src = "\tk: v\n";
    let out = diag(Span::new(4, 5), "").render(src);
    assert_eq!(markers_after(&out, "1 |     k: v"), "  |        ^");
    // CJK and emoji are two columns wide.
    let src = "k: 中文\u{1F600}x\n";
    let x = src.find('x').unwrap();
    let out = diag(Span::new(x, x + 1), "").render(src);
    assert_eq!(
        markers_after(&out, "1 | k: 中文\u{1F600}x"),
        "  |          ^"
    );
    // A span over wide characters is as wide as they are.
    let out = diag(Span::new(3, x), "").render(src);
    assert_eq!(
        markers_after(&out, "1 | k: 中文\u{1F600}x"),
        "  |    ^^^^^^"
    );
    // Combining marks take no column.
    let src = "e\u{301}e\u{301}x\n";
    let x = src.find('x').unwrap();
    let out = diag(Span::new(x, x + 1), "").render(src);
    assert_eq!(markers_after(&out, "1 | e\u{301}e\u{301}x"), "  |   ^");
    // The header column counts chars, like `Error`'s `Display`.
    assert!(out.contains("--> 1:5"), "{out}");
}

#[test]
fn unsafe_characters_are_neutralised() {
    let src = "a: \u{202e}bc\u{7}d\n";
    let out = diag(Span::new(3, src.len() - 1), "").render(src);
    assert!(out.contains("1 | a: \u{fffd}bc\u{fffd}d"), "{out}");
    assert!(!out.contains('\u{202e}') && !out.contains('\u{7}'));
    assert_eq!(
        markers_after(&out, "1 | a: \u{fffd}bc\u{fffd}d"),
        "  |    ^^^^^"
    );
}

#[test]
fn every_line_break_style_ends_a_line() {
    for src in [
        "a: 1\nb: 2\nc: 3\n",
        "a: 1\r\nb: 2\r\nc: 3\r\n",
        "a: 1\rb: 2\rc: 3\r",
    ] {
        let b = src.find('b').unwrap();
        let out = diag(Span::new(b, b + 4), "here").render(src);
        assert!(out.contains("--> 2:1"), "{out}");
        assert!(out.contains("2 | b: 2\n"), "{out:?}");
        assert!(!out.contains('\r'), "{out:?}");
        assert_eq!(markers_after(&out, "2 | b: 2"), "  | ^^^^ here");
        // A span that includes its line break stays on its line.
        let out = diag(Span::new(b, b + 5 + usize::from(src.contains("\r\n"))), "").render(src);
        assert!(!out.contains("3 |"), "{out}");
    }
}

#[test]
fn multi_line_labels_and_distant_lines() {
    let src = "a: |\n  x\n  y\nb: 1\n";
    let out = diag(Span::new(3, 12), "the block").render(src);
    assert!(out.contains("1 | a: |\n  |    ^\n"), "{out}");
    assert!(out.contains("...\n3 |   y\n  | ^^^ the block\n"), "{out}");

    // Labels 11 lines apart: a two-digit gutter and `...` between.
    let mut src = String::from("first: 1\n");
    for k in 0..10 {
        src.push_str(&format!("k{k}: v\n"));
    }
    src.push_str("first: 2\n");
    let second = src.rfind("first").unwrap();
    let d = diag(Span::new(second, second + 5), "again")
        .with_secondary(Label::new(Span::new(0, 5), "first here"));
    let out = d.render(&src);
    assert!(
        out.contains(
            " 1 | first: 1\n   | ----- first here\n  ...\n12 | first: 2\n   | ^^^^^ again\n"
        ),
        "{out}"
    );
    assert!(out.contains("  --> 12:1"), "{out}");
}

#[test]
fn labels_on_one_line_share_it_or_get_their_own_rows() {
    let src = "a: b: c\n";
    let d = diag(Span::new(3, 7), "").with_secondary(Label::new(Span::new(4, 5), "colon"));
    let out = d.render(src);
    // Overlapping: one row per label, so neither hides the other.
    assert!(
        out.contains("1 | a: b: c\n  |    ^^^^\n  |     - colon\n"),
        "{out}"
    );
    // Apart, with one message: one row.
    let d = diag(Span::new(0, 1), "").with_secondary(Label::new(Span::new(6, 7), "c"));
    let out = d.render(src);
    assert!(out.contains("  | ^     - c\n"), "{out}");
}

#[test]
fn spans_at_the_end_and_out_of_range() {
    let src = "a: [1, 2\n";
    let out = diag(Span::empty(src.len()), "end").render(src);
    assert!(out.contains("--> 2:1\n  |\n2 |\n  | ^ end\n"), "{out}");
    // Past the end, inverted, and inside a multi-byte character: clamped.
    let src = "é中";
    for span in [
        Span::new(99, 120),
        Span::new(3, 1),
        Span::new(1, 4),
        Span::empty(usize::MAX),
    ] {
        let out = diag(span, "x").render(src);
        assert!(out.contains("1 | é中"), "{span:?}: {out}");
    }
    // An empty source.
    let out = diag(Span::new(0, 5), "").render("");
    assert!(out.contains("--> 1:1"), "{out}");
}

#[test]
fn huge_lines_are_cut_around_the_span() {
    let mut src = "k: ".to_owned();
    src.push_str(&"y".repeat(1_000_000));
    let at = src.len();
    src.push_str("@z");
    src.push_str(&"w".repeat(1_000_000));
    src.push('\n');
    let d = diag(Span::new(at, at + 1), "reserved");
    let index = LineIndex::new(&src);
    let out = d.render_indexed(&src, &index);
    assert!(out.len() < 1_000, "{} bytes:\n{out}", out.len());
    assert!(out.contains("...yyyyyyyyyyyyyyyyyyyyyyyy@zwww"), "{out}");
    assert!(out.contains("www...\n"), "{out}");
    assert!(out.contains(&format!("--> 1:{}", at + 1)), "{out}");
    let row = out.lines().find(|l| l.starts_with("1 |")).unwrap();
    let marker = markers_after(&out, row);
    assert_eq!(
        marker.find('^').unwrap() - 4,
        row.find('@').unwrap() - 4,
        "{out}"
    );
    assert!(d.render(&src).len() < 1_000);
}

#[test]
fn rendering_cost_does_not_grow_with_the_input() {
    // O(lines shown). A label in the middle of a 64 MiB single line,
    // with an index built once, costs about what it costs on a short line.
    fn time(src: &str, index: &LineIndex, d: &Diagnostic) -> std::time::Duration {
        (0..5)
            .map(|_| {
                let t = std::time::Instant::now();
                let out = d.render_indexed(src, index);
                std::hint::black_box(out);
                t.elapsed()
            })
            .min()
            .unwrap()
    }
    let small = "x".repeat(4_096);
    let big = "x".repeat(64 << 20);
    let (is, ib) = (LineIndex::new(&small), LineIndex::new(&big));
    let ds = diag(Span::new(2_048, 2_050), "here");
    let db = diag(Span::new(32 << 20, (32 << 20) + 2), "here");
    let (ts, tb) = (time(&small, &is, &ds), time(&big, &ib, &db));
    // Linear cost would be 16,384 times slower.
    assert!(
        tb < ts * 50 + std::time::Duration::from_millis(2),
        "{ts:?} vs {tb:?}"
    );
    // An `Error` needs no index: it knows its line and column.
    let mut src: String = (0..200_000).map(|k| format!("k{k}: v\n")).collect();
    src.push_str("x: [1\n");
    let err = oneyaml::from_str::<Value>(&src).unwrap_err();
    let out = err.render(&src);
    assert!(out.contains("200002 |"), "{out}");
}

#[test]
fn suggestions_are_summarised() {
    let src = "a: b\n";
    let edit = |span, text: &str| Replacement::new(span, text);
    let d = diag(Span::new(3, 4), "")
        .with_suggestion(Suggestion::new(
            "insert",
            vec![edit(Span::empty(4), "!")],
            Applicability::MachineApplicable,
        ))
        .with_suggestion(Suggestion::new(
            "remove",
            vec![edit(Span::new(3, 4), "")],
            Applicability::MaybeIncorrect,
        ))
        .with_suggestion(Suggestion::new(
            "two",
            vec![edit(Span::empty(0), "x"), edit(Span::empty(4), "y")],
            Applicability::MaybeIncorrect,
        ))
        .with_suggestion(Suggestion::new(
            "long",
            vec![edit(Span::new(3, 99), &"z\n".repeat(50))],
            Applicability::MaybeIncorrect,
        ));
    let out = d.render(src);
    assert!(
        out.contains("= suggestion: insert: insert `!` at 1:5\n"),
        "{out}"
    );
    assert!(
        out.contains("= suggestion (may be incorrect): remove: remove `b`\n"),
        "{out}"
    );
    assert!(
        out.contains("= suggestion (may be incorrect): two (2 edits)\n"),
        "{out}"
    );
    assert!(out.contains("replace `b\\n` with `z\\nz\\n"), "{out}");
    assert!(out.contains("…`\n"), "{out}");
}

#[test]
fn errors_render_like_their_diagnostics() {
    // `Error::render` numbers lines from its own location; the indexed
    // form uses a `LineIndex`: the same output, on every invalid suite
    // case and on each serde error shape.
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    let mut n = 0;
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|f| f == "in.yaml") {
                let Ok(src) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if let Err(err) = oneyaml::from_str::<Value>(&src) {
                    let a = err.render(&src);
                    let b = err.render_indexed(&src, &LineIndex::new(&src));
                    assert_eq!(a, b, "{path:?}");
                    assert_eq!(a, oneyaml::Render::render(&err, &src));
                    assert!(a.starts_with(&format!("error[{}]", err.code())));
                    n += 1;
                }
            }
        }
    }
    assert!(n > 90, "{n} error cases");

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
    let src = "ports:\n  - port: 80\n  - port: 99999\n";
    let err = oneyaml::from_str::<Spec>(src).unwrap_err();
    let out = err.render(src);
    assert_eq!(out, err.render_indexed(src, &LineIndex::new(src)));
    assert!(
        out.starts_with("error[Y3001]: integer 99999 is out of range for u16"),
        "{out}"
    );
    assert!(
        out.contains("3 |   - port: 99999\n  |           ^^^^^\n"),
        "{out}"
    );
    assert!(out.contains("= path: ports[1].port\n"), "{out}");
    // A suggestion far along a line longer than the capped
    // scans is located exactly by `Error::render` too.
    let mut src = "k: [".to_owned();
    src.push_str(&"ab, ".repeat(600));
    src.push_str("z # note\n");
    let err = oneyaml::from_str::<Value>(&src).unwrap_err();
    let out = err.render(&src);
    assert_eq!(out, err.render_indexed(&src, &LineIndex::new(&src)));
    assert!(out.contains("insert `]` at 1:2406"), "{out}");
    // After the anchor on its line (a bad escape far along a long quoted
    // scalar), and before it on an earlier long line (a flow collection
    // cut by a dedent).
    for src in [
        format!("k: \"{}\\q\"\n", "x".repeat(3_000)),
        format!("k: [{}z\nb: 1\n", "ab, ".repeat(600)),
    ] {
        let err = oneyaml::from_str::<Value>(&src).unwrap_err();
        let out = err.render(&src);
        assert_eq!(out, err.render_indexed(&src, &LineIndex::new(&src)));
        assert!(out.contains("insert `"), "{out}");
    }
    // An error with no position shows no snippet.
    let err = oneyaml::from_reader::<_, Value>(Broken).unwrap_err();
    assert_eq!(
        err.render("anything"),
        format!("error[{}]: {}\n", err.code(), err.diagnostic().message)
    );
}

struct Broken;
impl std::io::Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n.max(1) as u64)).unwrap()
    }
}

const PIECES: &[&str] = &[
    "a",
    "k",
    ":",
    " ",
    ": ",
    "\n",
    "\r",
    "\r\n",
    "\t",
    "é",
    "中",
    "\u{1F600}",
    "\u{301}",
    "#",
    "'",
    "\"",
    "\\",
    "[",
    "]",
    "{",
    "}",
    "- ",
    "@",
    "`",
    "\u{202e}",
    "\u{7}",
    "\u{feff}",
    "&a",
    "*a",
    "!t",
    "|",
    ">",
    "---",
    "...",
    "%",
];

fn random_text(rng: &mut Rng, max: usize) -> String {
    let n = rng.below(max);
    (0..n).map(|_| PIECES[rng.below(PIECES.len())]).collect()
}

fn random_span(rng: &mut Rng, len: usize) -> Span {
    let pick = |rng: &mut Rng| match rng.below(8) {
        0 => usize::MAX - rng.below(3),
        1 => len + rng.below(4),
        _ => rng.below(len + 1),
    };
    Span::new(pick(rng), pick(rng))
}

#[test]
fn rendering_never_panics() {
    // Random sources, random (bad) spans, mismatched indexes.
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..3_000 {
        let src = random_text(&mut rng, 40);
        let other = random_text(&mut rng, 40);
        let mut d = Diagnostic::warning(
            Code::new(3004).unwrap(),
            random_text(&mut rng, 6),
            Label::new(random_span(&mut rng, src.len()), random_text(&mut rng, 3)),
        );
        for _ in 0..rng.below(4) {
            d = d.with_secondary(Label::new(
                random_span(&mut rng, src.len()),
                random_text(&mut rng, 3),
            ));
        }
        if rng.below(2) == 0 {
            d = d.with_help(random_text(&mut rng, 5));
        }
        for _ in 0..rng.below(3) {
            let edits = (0..rng.below(3))
                .map(|_| {
                    Replacement::new(random_span(&mut rng, src.len()), random_text(&mut rng, 3))
                })
                .collect();
            d = d.with_suggestion(Suggestion::new("fix", edits, Applicability::MaybeIncorrect));
        }
        let out = d.render(&src);
        assert!(out.starts_with("warning[Y3004]"), "{round}");
        let _ = d.render_indexed(&src, &LineIndex::new(&other));
        let _ = d.render(&other);

        // The real pipeline: every diagnostic and every error of the
        // random text, against its own source and a different one.
        let parsed = Document::parse_recovering(src.as_str());
        let index = LineIndex::new(&src);
        for d in parsed.diagnostics() {
            let a = d.render(&src);
            assert_eq!(a, d.render_indexed(&src, &index), "{src:?}");
            let _ = d.render(&other);
        }
        if let Err(err) = oneyaml::from_str::<Value>(&src) {
            assert_eq!(
                err.render(&src),
                err.render_indexed(&src, &index),
                "{src:?}"
            );
            let _ = err.render(&other);
            let _ = err.render("");
        }
    }
}
