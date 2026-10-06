//! Parser limits: each limit's exact boundary, its `Y2xxx` code, its
//! help naming the setting and current value, contract-valid
//! recovery, and no effect under `Limits::unlimited()`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use common::{events, first_code, first_error, tree};
use oneyaml_core::code::{
    DEPTH_LIMIT_EXCEEDED, DOCUMENT_TOO_LARGE, SCALAR_TOO_LONG, TOO_MANY_DOCUMENTS,
};
use oneyaml_core::{Code, Limits, Settings};
use oneyaml_syntax::event::{EventKind, TriviaKind};

fn with(limits: Limits) -> Settings {
    Settings::new().limits(limits)
}

/// `ok` parses without error and `bad` fails first with `code`, whose help
/// names `setting` and `value`.
fn boundary(settings: &Settings, ok: &str, bad: &str, code: Code, setting: &str, value: usize) {
    assert_eq!(first_code(ok, settings), None, "{ok:?}");
    let d = first_error(bad, settings).expect("an error for the over-limit input");
    assert_eq!(d.code, code, "{bad:?}");
    let help = format!("raise Limits::{setting} (currently {value})");
    assert_eq!(d.help.as_deref(), Some(help.as_str()), "{bad:?}");
    // Nothing is limited under `unlimited()`.
    assert_eq!(first_code(bad, &with(Limits::unlimited())), None, "{bad:?}");
}

fn collections(src: &str, settings: &Settings) -> usize {
    events(src, settings)
        .iter()
        .filter(|e| {
            matches!(
                e.kind(),
                EventKind::MappingStart { .. } | EventKind::SequenceStart { .. }
            )
        })
        .count()
}

#[test]
fn max_depth_counts_block_and_flow_together() {
    let s = with(Limits::new().max_depth(3));
    let d = |ok: &str, bad: &str| boundary(&s, ok, bad, DEPTH_LIMIT_EXCEEDED, "max_depth", 3);
    d("- - - a\n", "- - - - a\n");
    d("a:\n  b:\n    c: d\n", "a:\n  b:\n    c:\n      d: e\n");
    d("? ? ? a\n", "? ? ? ? a\n");
    d("[[[a]]]\n", "[[[[a]]]]\n");
    d("{a: {b: {c: d}}}\n", "{a: {b: {c: {d: e}}}}\n");
    // Mixed: block collections plus flow collections inside them.
    d("- - [a]\n", "- - [[a]]\n");
    d("k:\n  - {a: b}\n", "k:\n  - {a: [b]}\n");
    // A flow pair is a mapping: `[a: b]` has depth 2, its key's
    // collections one more.
    d("[[a: b]]\n", "[[[a: b]]]\n");
    d("[[a]: b]\n", "[[[a]]: b]\n");
    d("[? [a] : b]\n", "[? [[a]] : b]\n");
    // A flow key of a new block mapping sits inside that mapping.
    d("[[a]]: b\n", "[[[a]]]: b\n");
    d("- [a]: b\n", "- [[a]]: b\n");
    // Explicit and compact forms.
    d("- ? a\n  : - b\n", "- ? a\n  : - - b\n");
    assert_eq!(collections("[[a]: b]\n", &s), 3);
    assert_eq!(collections("- - [a]\n", &s), 3);
}

#[test]
fn max_depth_zero_and_recovery() {
    let s = with(Limits::new().max_depth(0));
    assert_eq!(first_code("a\n", &s), None);
    assert_eq!(first_code("- a\n", &s), Some(DEPTH_LIMIT_EXCEEDED));
    assert_eq!(first_code("[a]\n", &s), Some(DEPTH_LIMIT_EXCEEDED));
    assert_eq!(first_code("a: b\n", &s), Some(DEPTH_LIMIT_EXCEEDED));
    // The error is repaired and the next document parses (C6).
    let s = with(Limits::new().max_depth(1));
    assert_eq!(
        tree("a:\n  - b\n---\nc\n", &s),
        "+DOC | +MAP | =VAL :a | !Y2000 | =VAL : | -MAP | -DOC | +DOC | =VAL :c | -DOC"
    );
    assert_eq!(
        tree("[a, [b]]\n--- x\n", &s),
        "+DOC | +SEQ | =VAL :a | !Y2000 | -SEQ | -DOC | +DOC | =VAL :x | -DOC"
    );
}

#[test]
fn max_scalar_len_measures_values_in_bytes() {
    let s = with(Limits::new().max_scalar_len(3));
    let d = |ok: &str, bad: &str| boundary(&s, ok, bad, SCALAR_TOO_LONG, "max_scalar_len", 3);
    d("abc\n", "abcd\n");
    d("k: abc\n", "k: abcd\n");
    d("abc: v\n", "abcd: v\n");
    d("k: a\n  b\n", "k: a\n  bc\n");
    d("k: 'abc'\n", "k: 'abcd'\n");
    d("k: \"abc\"\n", "k: \"abcd\"\n");
    d("k: |-\n  abc\n", "k: |\n  abc\n");
    d("k: >-\n  a\n  b\n", "k: >-\n  ab\n  c\n");
    d("[abc]\n", "[abcd]\n");
    d("{abc: 'def'}\n", "{abc: 'defg'}\n");
    d("[\"a\n b\"]\n", "[\"a\n bc\"]\n");
    // The value after escapes and folding counts, not the source bytes.
    assert_eq!(first_code("\"\\x41\\u0042\\U00000043\"\n", &s), None);
    assert_eq!(first_code("'a''b'\n", &s), None);
    // A multi-byte character counts all its bytes.
    assert_eq!(first_code("é\n", &s), None);
    assert_eq!(first_code("éé\n", &s), Some(SCALAR_TOO_LONG));
    // Recovery: the next document parses.
    assert_eq!(
        tree("k: |\n  abcdef\n--- x\n", &s),
        "+DOC | +MAP | =VAL :k | !Y2001 | =VAL : | -MAP | -DOC | +DOC | =VAL :x | -DOC"
    );
}

#[test]
fn max_document_size_rejects_the_whole_input() {
    let s = with(Limits::new().max_document_size(10));
    let d = |ok: &str, bad: &str| {
        boundary(&s, ok, bad, DOCUMENT_TOO_LARGE, "max_document_size", 10);
    };
    d("a: b\nc: d\n", "a: b\nc: de\n");
    d("", "---\n---\n---\n");
    // Nothing is parsed: one error, then only skipped lines, even across
    // document markers.
    let src = "a: b\n---\nc: d\n...\n";
    assert_eq!(tree(src, &s), "!Y2002");
    let evs = events(src, &s);
    let skipped = evs
        .iter()
        .filter(|e| matches!(e.kind(), EventKind::Trivia(TriviaKind::Skipped)))
        .count();
    assert_eq!(skipped, 3);
    // A leading document marker gets a zero-width error before it (R4).
    let evs = events("--- abcdefghij\n", &s);
    let err = evs.iter().find(|e| e.is_error()).unwrap();
    assert!(err.span().is_empty());
}

#[test]
fn max_documents_stops_the_stream() {
    let s = with(Limits::new().max_documents(2));
    let d = |ok: &str, bad: &str| boundary(&s, ok, bad, TOO_MANY_DOCUMENTS, "max_documents", 2);
    d("a\n--- b\n", "a\n--- b\n--- c\n");
    d("---\n---\n", "---\n---\n---\n");
    d("a\n...\nb\n...\n", "a\n...\nb\n...\nc\n");
    d(
        "%YAML 1.2\n---\na\n...\n%YAML 1.2\n---\nb\n",
        "a\n--- b\n...\n%YAML 1.2\n---\nc\n",
    );
    // Comments, blank lines, and a lone `...` are not documents.
    assert_eq!(
        first_code("# c\n\n...\n# d\n", &with(Limits::new().max_documents(0))),
        None
    );
    // Everything after the limit is skipped, document markers included.
    assert_eq!(
        tree("a\n--- b\n--- c\n--- d\n", &s),
        "+DOC | =VAL :a | -DOC | +DOC | =VAL :b | -DOC | !Y2003"
    );
}

#[test]
fn profiles_accept_normal_input() {
    let src = "a: [b, {c: d}]\n---\n- |\n  text\n- 'q'\n";
    for limits in [Limits::default(), Limits::untrusted(), Limits::unlimited()] {
        assert_eq!(first_code(src, &with(limits)), None);
    }
}

/// The tree keeps the first `max_diagnostics + 1`
/// diagnostics of its parse with their texts, and the first error wherever
/// it is; every other one as its skeleton (code, severity, spans). The
/// record stays complete (every diagnostic, every error node with its
/// diagnostic), the event stream is not capped, and the tree still
/// round-trips.
#[test]
fn the_tree_keeps_texts_for_its_first_diagnostics_and_skeletons_after() {
    use oneyaml_syntax::tree::{SyntaxKind, SyntaxTree};
    let src = format!("{}[\n---\n[\n", "- a: b: c\n".repeat(5));
    let s = Settings::new()
        .lenient_inline_colons(true)
        .limits(Limits::default().max_diagnostics(2));
    let all = SyntaxTree::parse_with(src.as_str(), &Settings::new().lenient_inline_colons(true));
    let capped = SyntaxTree::parse_with(src.as_str(), &s);
    assert_eq!(capped.emit(), src);
    assert_eq!(all.suppressed_diagnostics(), 0);
    let full: Vec<_> = all.diagnostics().collect();
    let kept: Vec<_> = capped.diagnostics().collect();
    assert_eq!(kept.len(), full.len());
    let first_error = full
        .iter()
        .position(|d| d.severity == oneyaml_core::Severity::Error)
        .unwrap();
    for (i, (k, f)) in kept.iter().zip(&full).enumerate() {
        if i < 3 || i == first_error {
            assert_eq!(k, f, "{i}");
        } else {
            assert!(SyntaxTree::is_skeleton(k), "{i}");
            assert_eq!(
                (k.code, k.severity, k.primary.span),
                (f.code, f.severity, f.primary.span)
            );
        }
    }
    assert_eq!(capped.suppressed_diagnostics(), full.len() - 4);
    assert_eq!(capped.error_count(), all.error_count());
    let errors = |t: &SyntaxTree| {
        t.root()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::Error)
            .map(|n| n.diagnostic().map(|d| (d.code, d.primary.span)))
            .collect::<Vec<_>>()
    };
    assert_eq!(errors(&all), errors(&capped));
    // Events are not capped.
    let warnings = events(&src, &s)
        .iter()
        .filter(|e| matches!(e.kind(), EventKind::Warning(_)))
        .count();
    assert_eq!(warnings, 5);
}

/// An edit whose window holds a diagnostic the tree keeps as a
/// skeleton is accepted exactly when it is without the cap. The depth-limit
/// error's help names the window's own, reduced `max_depth` when the window
/// is re-read, so its re-read differs from the tree's only in text: the
/// skeleton's fingerprint must tell (comparing code and spans alone accepts
/// edits the uncapped tree refuses). Every token is replaced by its own
/// text, one at a time.
#[test]
fn edits_next_to_skeletons_are_decided_as_without_the_cap() {
    use oneyaml_syntax::tree::SyntaxTree;
    for src in [
        "%FOO\n--- 'x\n---\n- - - - - 1\n  - b\n",
        "%FOO\n--- 'x\n---\na:\n  b:\n    c:\n      d: 1\n  e: 2\n",
    ] {
        let outcomes = |limit: usize| {
            let s = with(Limits::default().max_depth(3).max_diagnostics(limit));
            let tree = SyntaxTree::parse_with(src, &s);
            let tokens: Vec<_> = tree
                .root()
                .descendants()
                .filter(|n| n.is_token())
                .map(|n| (n.id(), n.text().into_owned()))
                .collect();
            let skeletons = tree
                .diagnostics()
                .filter(|d| SyntaxTree::is_skeleton(d))
                .count();
            let mut accepted: Vec<bool> = tokens
                .into_iter()
                .map(|(id, text)| tree.fork().replace_token(id, &text).is_ok())
                .collect();
            // An edit in a document with errors in its content is
            // refused (`ReadOnly`) before any window is read, whatever the
            // cap; such a document is re-read with its errors when the
            // stream window reads it whole: each document removed and put
            // back where it was.
            let docs: Vec<_> = tree
                .root()
                .children()
                .filter(|n| n.kind() == oneyaml_syntax::tree::SyntaxKind::Document)
                .map(|n| (n.id(), n.next_sibling().map(|x| x.id())))
                .collect();
            for (d, next) in docs {
                let mut t = tree.fork();
                let root = t.root().id();
                let Ok(f) = t.remove(d) else {
                    accepted.push(false);
                    continue;
                };
                let back = match next {
                    Some(x) => t.insert_before(x, f).is_ok(),
                    None => t.append_child(root, f).is_ok(),
                };
                if back {
                    assert_eq!(t.emit(), src);
                }
                accepted.push(back);
            }
            (accepted, skeletons)
        };
        let (want, none) = outcomes(usize::MAX);
        let (got, some) = outcomes(0);
        assert_eq!(none, 0);
        assert!(some > 0, "{src:?}: the depth error is a skeleton");
        assert!(want.contains(&false) && want.contains(&true));
        assert_eq!(got, want, "{src:?}");
    }
}

/// `diagnostics_within_filled` reads a skeleton as the
/// diagnostic with texts that it records (found by reading the same text
/// again without the cap), and never as one it does not record (same
/// code, severity and span, other texts).
#[test]
fn skeletons_are_filled_only_with_what_they_record() {
    use oneyaml_syntax::tree::SyntaxTree;
    let src = "a: b: c\nd: e: f\ng: h: i\nj: [k\n";
    let lenient = Settings::new().lenient_inline_colons(true);
    let capped = SyntaxTree::parse_with(
        src,
        &lenient.clone().limits(Limits::default().max_diagnostics(0)),
    );
    let full = SyntaxTree::parse_with(src, &lenient);
    assert!(capped.suppressed_diagnostics() > 0);
    let mut fill: Vec<_> = full
        .diagnostics()
        .map(std::borrow::Cow::into_owned)
        .collect();
    fill.sort_by_key(|d| d.primary.span.start);
    assert!(fill.iter().all(|d| !SyntaxTree::is_skeleton(d)));
    let root = capped.root();
    let plain: Vec<_> = capped
        .diagnostics_within(root)
        .map(|(_, d)| d.into_owned())
        .collect();
    assert!(plain.iter().any(SyntaxTree::is_skeleton));
    let filled: Vec<_> = capped
        .diagnostics_within_filled(root, &fill)
        .map(|(_, d)| d.into_owned())
        .collect();
    let mut want = fill.clone();
    want.sort_by_key(|d| d.primary.span.start);
    let mut got = filled.clone();
    got.sort_by_key(|d| d.primary.span.start);
    assert_eq!(got, want);
    // Forged texts at the same places fill nothing.
    let forged: Vec<_> = fill
        .iter()
        .cloned()
        .map(|mut d| {
            d.message.push('!');
            d
        })
        .collect();
    let kept: Vec<_> = capped
        .diagnostics_within_filled(root, &forged)
        .map(|(_, d)| d.into_owned())
        .collect();
    assert_eq!(kept, plain);
    // Without candidates: `diagnostics_within`.
    let none: Vec<_> = capped
        .diagnostics_within_filled(root, &[])
        .map(|(_, d)| d.into_owned())
        .collect();
    assert_eq!(none, plain);
}
