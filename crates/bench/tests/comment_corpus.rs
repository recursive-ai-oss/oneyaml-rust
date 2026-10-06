//! Comment attachment over the benchmark corpus: every
//! comment of every corpus shape (the real-world shapes —
//! Kubernetes manifests, a GitHub Actions workflow, docker-compose with
//! merge keys — and the stress shapes) gets exactly one owner, the
//! whole-document pass agrees with the one-comment entry point, and each
//! stream document's comments partition the stream's (linear cost is
//! pinned in `oneyaml-syntax/tests/comments.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use bench::corpus::CASES;
use oneyaml::{Document, Stream};
use oneyaml_syntax::tree::SyntaxKind;

fn check(name: &str, src: &str) -> usize {
    let tree = oneyaml_syntax::SyntaxTree::parse(src);
    let comments = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .count();
    let all = tree.comment_attachments();
    assert_eq!(all.len(), comments, "{name}");
    for (c, a) in &all {
        assert_eq!(
            tree.comment_attachment(*c).as_ref(),
            Some(a),
            "{name}: {c:?}"
        );
    }
    // Through the public API: the stream's documents share them out.
    if let Ok(stream) = Stream::parse(src) {
        let owned: usize = stream
            .documents()
            .map(|d| d.explain_comments().unwrap().len())
            .sum();
        if !stream.is_empty() {
            assert_eq!(owned, comments, "{name}: stream partition");
        }
    }
    if let Ok(d) = Document::parse(src) {
        assert_eq!(d.explain_comments().unwrap().len(), comments, "{name}");
    }
    comments
}

#[test]
fn every_corpus_comment_has_one_owner() {
    let mut total = 0;
    for case in CASES.iter().filter(|c| c.base <= 128 * 1024) {
        // Every shape at its base size (the multi-megabyte variants repeat
        // the same shapes).
        let src = case.generate(1);
        total += check(case.name, &src);
    }
    assert!(total > 0, "the corpus has comments");
}

#[test]
fn freeze_gate_shapes_at_their_benchmark_size() {
    for name in ["k8s-manifest", "gha-workflow", "compose-merge"] {
        let case = bench::corpus::find(name).unwrap();
        let src = case.generate(1);
        check(name, &src);
    }
}
