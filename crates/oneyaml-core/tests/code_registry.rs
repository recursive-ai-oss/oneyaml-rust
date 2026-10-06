//! Code-registry stability: the registry equals the frozen snapshot,
//! and library crates never mint codes with `Code::new`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};

use oneyaml_core::code::REGISTRY;

fn registry_lines() -> Vec<String> {
    REGISTRY
        .iter()
        .map(|info| {
            let status = if info.deprecated {
                "deprecated"
            } else {
                "active"
            };
            format!(
                "{}\t{}\t{}\t{}\t{}",
                info.code, info.name, info.severity, status, info.summary
            )
        })
        .collect()
}

#[test]
fn registry_matches_frozen_snapshot() {
    let snapshot = include_str!("codes.snapshot");
    let frozen: Vec<&str> = snapshot
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    let current = registry_lines();
    for (i, line) in frozen.iter().enumerate() {
        assert_eq!(
            current.get(i).map(String::as_str),
            Some(*line),
            "registry entry {i} differs from tests/codes.snapshot: existing codes are \
             append-only (only `active` -> `deprecated` may change)"
        );
    }
    assert_eq!(
        current.len(),
        frozen.len(),
        "every registry entry must be appended to tests/codes.snapshot"
    );
}

/// Library `.rs` files with inline `#[cfg(test)]` modules cut off, and
/// dedicated test files skipped.
fn library_sources(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    // Explicit worklist: no recursion, even in tests.
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "tests") {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                let text = fs::read_to_string(&path).unwrap();
                let library = text.split("#[cfg(test)]").next().unwrap_or("").to_owned();
                out.push((path, library));
            }
        }
    }
}

/// Whether this crate is being tested inside the oneyaml workspace (its
/// manifest two levels down from a `[workspace]` of `crates/*`), rather
/// than from its unpacked package. Inside, workspace data must be there:
/// a missing file fails the test instead of skipping it silently.
fn in_workspace() -> bool {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    std::fs::read_to_string(root)
        .is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
}

#[test]
fn library_code_never_mints_codes() {
    // The sibling crates' sources exist only in the workspace: the
    // published package checks nothing here, and says so.
    if !in_workspace() {
        eprintln!("skipped: not inside the oneyaml workspace (sibling crates absent)");
        return;
    }
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut sources = Vec::new();
    for krate in ["oneyaml-core", "oneyaml-syntax", "oneyaml"] {
        library_sources(&crates.join(krate).join("src"), &mut sources);
    }
    assert!(sources.len() >= 3, "found the library sources");
    for (path, text) in sources {
        assert!(
            !text.contains("Code::new("),
            "{} calls Code::new outside tests; emit a registered constant instead",
            path.display()
        );
    }
}
