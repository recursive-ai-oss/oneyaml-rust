//! Trivia edit differential (an edited tree
//! always agrees with its own text). Every removal of 1–N adjacent sibling
//! trivia tokens, and a set of trivia fragments inserted before and after
//! every token, over the yaml-test-suite inputs and adversarial tail shapes
//! (block roots ending with a cut comment, `|+`, `---` after), in LF, CRLF
//! and CR: every edit the syntax layer accepts must leave a tree equal to
//! the re-parse of its own text. Refusals are fine.
//!
//! The default run samples every second token of the suite inputs (every
//! token of the adversarial shapes) to stay well under a minute in debug
//! builds;
//! `ONEYAML_TRIVIA_DIFF_FULL=1` runs every token, fragment, and removal
//! length (up to `ONEYAML_TRIVIA_DIFF_N`, default 3).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::path::{Path, PathBuf};

use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxTree};

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Every `in.yaml` of the vendored suite, sorted (empty outside the
/// workspace).
fn suite_inputs() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    if !root.is_dir() {
        eprintln!("skipped: yaml-test-suite data not found");
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut dirs: Vec<PathBuf> = vec![root];
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                out.push((p.clone(), std::fs::read(&p).unwrap()));
            }
        }
    }
    out.sort();
    out.into_iter()
        .map(|(_, b)| String::from_utf8_lossy(&b).into_owned())
        .collect()
}

/// Block roots ending with a cut comment, `|+`, nested footers, and
/// documents after them (shapes found while hardening comment runs).
fn adversarial() -> Vec<String> {
    let bodies = [
        "a: 1\n# c\n",
        "a: 1\n  # c\n",
        "a:\n  b: 1\n  # f\n# g\n",
        "a:\n  b:\n    c: 1\n    # f\n  # g\n",
        "- a\n- b\n# c\n",
        "a: |+\n  x\n\n# c\n",
        "a: >-\n  x\n# c\n",
        "a: x # t\n# c\n",
        "? a\n: 1\n# c\n",
        "a: [x,\n  y]\n# c\n",
    ];
    let tails = ["", "# t\n", "--- y\n", "# t\n--- y\n", "\n# t\n", "...\n"];
    let mut out = Vec::new();
    for b in bodies {
        for t in tails {
            out.push(format!("{b}{t}"));
        }
        // Without the final line break.
        out.push(b.trim_end_matches('\n').to_owned());
    }
    out
}

/// The trivia fragments inserted (`{nl}` is the document's line break).
fn fragments(full: bool) -> Vec<&'static str> {
    let mut f = vec![
        "  {nl}",
        "\t{nl}",
        "  {nl}# z{nl}",
        "# z{nl}",
        "{nl}",
        " ",
        " # z",
        "{nl}  # z",
    ];
    if full {
        f.extend([
            "\t",
            "{nl}{nl}",
            "  # z{nl}",
            "{nl}# z{nl}  {nl}",
            " \t{nl}",
        ]);
    }
    f
}

#[derive(Default)]
struct Tally {
    edits: usize,
    accepted: usize,
    failures: Vec<String>,
}

fn check(t: &SyntaxTree, settings: &Settings, what: impl FnOnce() -> String, tally: &mut Tally) {
    tally.accepted += 1;
    let again = SyntaxTree::parse_with(t.emit(), settings);
    if *t != again && tally.failures.len() < 20 {
        tally
            .failures
            .push(format!("{}: accepted {:?}", what(), t.emit()));
    } else if *t != again {
        tally.failures.push(String::new());
    }
}

fn run(
    src: &str,
    settings: &Settings,
    full: bool,
    (stride, offset): (usize, usize),
    n_max: usize,
    tally: &mut Tally,
) {
    let tree = SyntaxTree::parse_with(src, settings);
    let nl = if src.contains("\r\n") {
        "\r\n"
    } else if src.contains('\r') {
        "\r"
    } else {
        "\n"
    };
    let tokens: Vec<(NodeId, bool)> = tree
        .root()
        .descendants()
        .filter(|n| n.is_token())
        .map(|n| (n.id(), n.kind().is_trivia()))
        .collect();
    // Removals of 1..=n_max adjacent sibling trivia tokens.
    for (i, (id, trivia)) in tokens.iter().enumerate() {
        if !trivia || (i + offset) % stride != 0 {
            continue;
        }
        let first = tree.get(*id).unwrap();
        let mut last = first;
        for n in 1..=n_max {
            if n > 1 {
                match last
                    .next_sibling()
                    .filter(|x| x.is_token() && x.kind().is_trivia())
                {
                    Some(x) => last = x,
                    None => break,
                }
            }
            let mut t = tree.fork();
            tally.edits += 1;
            if t.remove_range(first.id(), last.id()).is_ok() {
                check(
                    &t,
                    settings,
                    || format!("{src:?}: remove {n} from token {i}"),
                    tally,
                );
            }
        }
    }
    // Trivia fragments before and after every token.
    let frags: Vec<(String, Fragment)> = fragments(full)
        .into_iter()
        .filter_map(|f| {
            let text = f.replace("{nl}", nl);
            Fragment::parse_with(&text, settings)
                .ok()
                .map(|x| (text, x))
        })
        .collect();
    for (i, (id, _)) in tokens.iter().enumerate() {
        if (i + offset) % stride != 0 {
            continue;
        }
        for (text, f) in &frags {
            for before in [false, true] {
                let mut t = tree.fork();
                tally.edits += 1;
                let r = if before {
                    t.insert_before(*id, f.clone())
                } else {
                    t.insert_after(*id, f.clone())
                };
                if r.is_ok() {
                    let side = if before { "before" } else { "after" };
                    check(
                        &t,
                        settings,
                        || format!("{src:?}: insert {text:?} {side} token {i}"),
                        tally,
                    );
                }
            }
        }
    }
}

#[test]
fn accepted_trivia_edits_read_back_as_the_tree() {
    let full = std::env::var("ONEYAML_TRIVIA_DIFF_FULL").is_ok();
    let n_max = env("ONEYAML_TRIVIA_DIFF_N", 3);
    let settings = Settings::new();
    let mut tally = Tally::default();
    let start = std::time::Instant::now();
    // Adversarial shapes: every token, always.
    for src in adversarial() {
        for nl in ["\n", "\r\n", "\r"] {
            run(
                &src.replace('\n', nl),
                &settings,
                full,
                (1, 0),
                n_max,
                &mut tally,
            );
        }
    }
    // The suite: every token in full runs; by default every second, the
    // offset rotating with the input and line break.
    for (k, src) in suite_inputs().iter().enumerate() {
        for (j, nl) in ["\n", "\r\n", "\r"].into_iter().enumerate() {
            let src = src.replace('\n', nl);
            let stride = if full { 1 } else { 2 };
            run(
                &src,
                &settings,
                full,
                (stride, (k + j) % 2),
                n_max,
                &mut tally,
            );
        }
    }
    eprintln!(
        "trivia differential: {} edits, {} accepted, {} failures, {:?}",
        tally.edits,
        tally.accepted,
        tally.failures.len(),
        start.elapsed()
    );
    let shown: Vec<&String> = tally.failures.iter().filter(|f| !f.is_empty()).collect();
    assert!(
        tally.failures.is_empty(),
        "{} accepted edits whose tree is not its re-parse; first: {shown:#?}",
        tally.failures.len()
    );
    assert!(
        tally.accepted > 1000,
        "too few accepted edits: {}",
        tally.accepted
    );
}
