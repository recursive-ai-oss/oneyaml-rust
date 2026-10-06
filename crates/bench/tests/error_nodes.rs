//! An invariant guard (restricted mode): **every
//! `SyntaxKind::Error` node carries an error diagnostic**, so a tree with
//! no error diagnostics (`SyntaxTree::has_errors() == false`) has no error
//! node.
//!
//! `oneyaml::document::view::presented` relies on it: it returns early,
//! without the span and sibling reads, on a tree without errors, which is
//! only correct when no error node can hide there. This test checks the
//! invariant, and the early return's exact precondition (no error-node
//! sibling before an empty scalar in an error-free tree), on:
//!
//! 1. every yaml-test-suite case, valid and invalid, and every truncation
//!    of it at a character boundary, under several reading settings;
//! 2. deterministic mutations of the suite (the fuzz alphabet of
//!    `doc_diagnostics.rs`);
//! 3. the adversarial corpus under `Limits::untrusted()`, and the suite
//!    under tight limits (limit errors are error nodes too);
//! 4. edited trees: random edit scripts (remove, replace, insert, token
//!    replacement) over the suite, checked after every accepted edit;
//! 5. the trees `Document`/`Stream` keep (`syntax()`), strict and
//!    recovering.
//!
//! It fails if an error node without an error diagnostic ever appears.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Compat, Document, DuplicateKeys, Limits, Settings, Stream};
use oneyaml_core::Severity;
use oneyaml_syntax::SyntaxTree;
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind};

/// Counts, so the checks are not vacuous.
#[derive(Default, Debug)]
struct Tally {
    trees: usize,
    with_error_nodes: usize,
    error_nodes: usize,
    edited: usize,
    edited_with_error_nodes: usize,
}

/// Checks the invariant on `tree`; `what` names the input on failure.
fn check(tree: &SyntaxTree, what: &str, tally: &mut Tally) -> usize {
    tally.trees += 1;
    let mut errors_here = 0usize;
    for n in tree.root().descendants() {
        if n.kind() != SyntaxKind::Error {
            continue;
        }
        errors_here += 1;
        assert!(
            tree.has_errors(),
            "{what}: an error node at {:?} in a tree without errors\ntext {:?}",
            n.span(),
            tree.emit()
        );
        let d = n.diagnostic().unwrap_or_else(|| {
            panic!(
                "{what}: the error node at {:?} has no diagnostic\ntext {:?}",
                n.span(),
                tree.emit()
            )
        });
        assert_eq!(
            d.severity,
            Severity::Error,
            "{what}: the error node at {:?} carries a {:?}\ntext {:?}",
            n.span(),
            d,
            tree.emit()
        );
    }
    if !tree.has_errors() {
        // What `presented` skips: an empty scalar right after an error node.
        for n in tree.root().descendants() {
            if n.kind() == SyntaxKind::Scalar && n.span().is_empty() {
                assert!(
                    n.prev_sibling()
                        .is_none_or(|p| p.kind() != SyntaxKind::Error),
                    "{what}: an empty scalar after an error node in an error-free tree"
                );
            }
        }
    }
    if errors_here > 0 {
        tally.with_error_nodes += 1;
        tally.error_nodes += errors_here;
    }
    errors_here
}

fn settings() -> Vec<(&'static str, Settings)> {
    vec![
        ("default", Settings::new()),
        (
            "last_wins",
            Settings::new()
                .duplicate_keys(DuplicateKeys::LastWins)
                .merge_keys(true),
        ),
        ("yaml_1_1", Compat::yaml_1_1()),
        (
            "honor_lenient",
            Settings::new()
                .honor_version_directive(true)
                .lenient_inline_colons(true),
        ),
        (
            "tight_limits",
            Settings::new().limits(
                Limits::new()
                    .max_depth(3)
                    .max_scalar_len(8)
                    .max_document_size(64)
                    .max_documents(2),
            ),
        ),
    ]
}

/// xorshift64*, deterministic.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n.max(1)
    }
}

const ALPHABET: &[&str] = &[
    "-", "?", ":", ": ", ",", "[", "]", "{", "}", "#", "&a ", "*a", "!", "!!int ", "!!str ",
    "!!map ", "|", ">", "'", "\"", "%", "@", " ", "  ", "\n", "\t", "\r\n", "---", "...", "<<",
    "<<: ", "x", "k1: ", "1", "0x1", "\\", "\u{feff}", "é", "~", "null", ".inf", "08",
];

fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut s = src.to_owned();
    for _ in 0..=rng.below(3) {
        let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
        let at = bounds[rng.below(bounds.len())];
        let end = bounds.iter().copied().find(|&b| b > at).unwrap_or(at);
        match rng.below(3) {
            0 => s.insert_str(at, ALPHABET[rng.below(ALPHABET.len())]),
            1 => s.replace_range(at..end, ""),
            _ => s.replace_range(at..end, ALPHABET[rng.below(ALPHABET.len())]),
        }
    }
    s
}

fn suite() -> Vec<(String, String)> {
    let suite = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    assert!(suite.len() >= 402, "{}", suite.len());
    suite
        .iter()
        .map(|c| (c.id.clone(), String::from_utf8_lossy(&c.input).into_owned()))
        .collect()
}

#[test]
fn error_nodes_carry_error_diagnostics_in_parsed_trees() {
    let texts = suite();
    let mut tally = Tally::default();
    for (label, s) in settings() {
        for (id, src) in &texts {
            check(
                &SyntaxTree::parse_with(src.as_str(), &s),
                &format!("{label}/{id}"),
                &mut tally,
            );
            for (cut, _) in src.char_indices().skip(1) {
                let t = SyntaxTree::parse_with(&src[..cut], &s);
                check(&t, &format!("{label}/{id}@{cut}"), &mut tally);
            }
        }
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ label.len() as u64);
        for round in 0..3000 {
            let base = rng.below(texts.len());
            let src = mutate(&mut rng, &texts[base].1);
            check(
                &SyntaxTree::parse_with(src, &s),
                &format!("{label}/m{round}"),
                &mut tally,
            );
        }
    }
    for a in test_suite::adversarial::corpus() {
        let s = Settings::new()
            .limits(Limits::untrusted())
            .merge_keys(a.merge_keys);
        check(
            &SyntaxTree::parse_with(a.input.as_str(), &s),
            &format!("adversarial/{}", a.name),
            &mut tally,
        );
    }
    eprintln!("{tally:?}");
    assert!(tally.with_error_nodes >= 10_000, "{tally:?}");
}

#[test]
fn error_nodes_carry_error_diagnostics_in_document_trees() {
    let mut tally = Tally::default();
    for (label, s) in settings() {
        for (id, src) in suite() {
            let what = format!("{label}/{id}");
            let d = Document::parse_recovering_with(&src, &s);
            check(
                d.value().syntax(),
                &format!("{what} Document recovering"),
                &mut tally,
            );
            let st = Stream::parse_recovering_with(&src, &s);
            check(
                st.value().syntax(),
                &format!("{what} Stream recovering"),
                &mut tally,
            );
            if let Ok(d) = Document::parse_with(&src, &s) {
                check(d.syntax(), &format!("{what} Document strict"), &mut tally);
            }
        }
    }
    eprintln!("{tally:?}");
    assert!(tally.with_error_nodes >= 500, "{tally:?}");
}

/// Snippets for replacements and insertions, broken ones included.
const SNIPPETS: &[&str] = &[
    "x", "a: 1", "[1, 2]", "{a: b}", "- x", "'q'", "\"d\"", "[", "{a", "a: b: c", "&a x", "*a",
    "!!int x", "|\n  t\n", ": x", "? k", "- - x", "\"\\q\"", "#c",
];

#[test]
fn error_nodes_carry_error_diagnostics_in_edited_trees() {
    let texts = suite();
    let mut tally = Tally::default();
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for (label, s) in settings()
        .into_iter()
        .filter(|(l, _)| matches!(*l, "default" | "honor_lenient" | "tight_limits"))
    {
        for (id, src) in &texts {
            let mut tree = SyntaxTree::parse_with(src.as_str(), &s);
            let mut pool: Vec<Fragment> = Vec::new();
            for step in 0..40 {
                let all: Vec<NodeId> = tree.root().descendants().map(|n| n.id()).collect();
                if all.is_empty() {
                    break;
                }
                let x = all[rng.below(all.len())];
                let snippet = SNIPPETS[rng.below(SNIPPETS.len())];
                let fragment = || Fragment::parse_with(snippet, &s).ok();
                let accepted = match rng.below(5) {
                    0 => tree.remove(x).map(|f| pool.push(f)).is_ok(),
                    1 => fragment().is_some_and(|f| tree.replace(x, f).is_ok()),
                    2 => fragment().is_some_and(|f| tree.insert_after(x, f).is_ok()),
                    3 => match pool.pop() {
                        Some(f) => tree.insert_after(x, f).is_ok(),
                        None => false,
                    },
                    _ => tree.replace_token(x, snippet).is_ok(),
                };
                if accepted {
                    tally.edited += 1;
                    if check(&tree, &format!("{label}/{id} step {step}"), &mut tally) > 0 {
                        tally.edited_with_error_nodes += 1;
                    }
                }
            }
        }
    }
    eprintln!("{tally:?}");
    assert!(tally.edited >= 2_000, "{tally:?}");
    assert!(tally.edited_with_error_nodes >= 200, "{tally:?}");
}
