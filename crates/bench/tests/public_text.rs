//! Everything the generators publish reads as plain user-facing text.
//!
//! `bench update`, `bench readme` and `oracles --readme` write into the
//! READMEs, `docs/*.md` and the benchmark charts. None of it may carry the
//! project's internal process identifiers (decision IDs, the private
//! planning documents, work-unit numbers, review jargon):
//! [`oracles::public_text::internal_reference`] defines them. Two checks:
//!
//! 1. every generated region committed in the tree, and both charts;
//! 2. every string the generators can emit, read from their sources (code
//!    lines outside comments and `#[cfg(test)]` modules), so a new label
//!    is caught before it is ever measured and written.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::{Path, PathBuf};

use oracles::public_text::{generated_regions, internal_reference};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    out.sort();
    out
}

#[test]
fn committed_generated_text_is_plain() {
    let root = root();
    let mut docs = vec![
        root.join("README.md"),
        root.join("crates/oneyaml/README.md"),
    ];
    let mut md: Vec<PathBuf> = std::fs::read_dir(root.join("docs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    md.sort();
    docs.extend(md);
    let mut regions = 0usize;
    let mut found = Vec::new();
    for path in &docs {
        let text = std::fs::read_to_string(path).unwrap();
        for (name, body) in generated_regions(&text) {
            regions = regions.saturating_add(1);
            if let Some(id) = internal_reference(body) {
                found.push(format!("{} `{name}`: {id:?}", path.display()));
            }
        }
    }
    // The README summary and chart, the conformance line and the
    // performance tables: the check must not pass vacuously.
    assert!(regions >= 20, "only {regions} generated regions found");
    for svg in [
        "docs/img/bench-summary-light.svg",
        "docs/img/bench-summary-dark.svg",
    ] {
        let text = std::fs::read_to_string(root.join(svg)).unwrap();
        if let Some(id) = internal_reference(&text) {
            found.push(format!("{svg}: {id:?}"));
        }
    }
    assert!(
        found.is_empty(),
        "internal identifiers in generated text:\n{}",
        found.join("\n")
    );
}

/// The code of a source file that can reach generated output: everything
/// before its `#[cfg(test)]` module, without `//` comments (doc comments
/// included; a `//` right after `:` is a URL, not a comment).
fn code_lines(text: &str) -> Vec<(usize, String)> {
    let code = text.split("\n#[cfg(test)]").next().unwrap_or_default();
    code.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let b = line.as_bytes();
            let cut = b
                .windows(2)
                .enumerate()
                .find(|&(k, w)| w == b"//" && (k == 0 || b[k.saturating_sub(1)] != b':'))
                .map_or(line.len(), |(k, _)| k);
            let kept = line[..cut].trim();
            (!kept.is_empty()).then(|| (i.saturating_add(1), kept.to_owned()))
        })
        .collect()
}

#[test]
fn generator_strings_are_plain() {
    let root = root();
    let mut found = Vec::new();
    let mut scanned = 0usize;
    for dir in ["crates/bench/src", "crates/oracles/src"] {
        for path in rs_files(&root.join(dir)) {
            // The checker itself spells out what it looks for.
            if path.ends_with("oracles/src/public_text.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (line, code) in code_lines(&text) {
                scanned = scanned.saturating_add(1);
                if let Some(id) = internal_reference(&code) {
                    found.push(format!("{}:{line}: {id:?} in `{code}`", path.display()));
                }
            }
        }
    }
    assert!(scanned > 1000, "only {scanned} code lines scanned");
    assert!(
        found.is_empty(),
        "internal identifiers in generator code:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_source_scan_sees_labels_and_skips_comments() {
    let src = "/// Q4.4 in a doc comment\nconst A: &str = \"Q4.4 ≤ 1.25×\"; // NN-3\n\
               let u = \"https://example.com/x\";\n#[cfg(test)]\nmod tests { \"unit 7\" }\n";
    let lines = code_lines(src);
    assert_eq!(
        lines,
        [
            (2, "const A: &str = \"Q4.4 ≤ 1.25×\";".to_owned()),
            (3, "let u = \"https://example.com/x\";".to_owned()),
        ]
    );
    assert_eq!(internal_reference(&lines[0].1).as_deref(), Some("Q4.4"));
}
