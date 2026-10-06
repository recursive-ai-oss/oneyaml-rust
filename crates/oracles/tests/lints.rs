//! The oracles crate cannot inherit `[workspace.lints]` (it must lower
//! `unsafe_code` from `forbid` to `deny`), so its lint tables are a hand
//! copy. This keeps the copy in sync with the workspace and keeps the
//! `unsafe` exception confined to `src/libyaml.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// `(table, key) -> value` for the `[<prefix>.rust]` / `[<prefix>.clippy]`
/// tables of a Cargo manifest (`key = "value"` lines only, which is all the
/// lint tables use).
fn lint_tables(manifest: &str, prefix: &str) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    let mut table: Option<String> = None;
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('.'))
                .filter(|group| matches!(*group, "rust" | "clippy"))
                .map(str::to_owned);
            continue;
        }
        if let Some(group) = &table {
            let (key, value) = line.split_once('=').expect("key = value");
            let value = value.trim().trim_matches('"').to_owned();
            out.insert((group.clone(), key.trim().to_owned()), value);
        }
    }
    out
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Lints the oracles crate adds beyond the workspace (stricter, never
/// looser), with their required level.
const EXTRA: [(&str, &str, &str); 1] = [("clippy", "undocumented_unsafe_blocks", "deny")];

#[test]
fn lints_match_the_workspace_except_unsafe_code() {
    let root = crate_dir().join("../../Cargo.toml");
    let workspace = lint_tables(&fs::read_to_string(root).unwrap(), "workspace.lints");
    let ours = lint_tables(
        &fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap(),
        "lints",
    );
    assert!(!workspace.is_empty(), "no [workspace.lints] found");
    let unsafe_code = ("rust".to_owned(), "unsafe_code".to_owned());
    assert_eq!(
        workspace.get(&unsafe_code).map(String::as_str),
        Some("forbid")
    );
    assert_eq!(ours.get(&unsafe_code).map(String::as_str), Some("deny"));
    let mut expected = workspace.clone();
    expected.insert(unsafe_code, "deny".to_owned());
    for (group, key, level) in EXTRA {
        expected.insert((group.to_owned(), key.to_owned()), level.to_owned());
    }
    assert_eq!(ours, expected);
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    // Test code: the crate's source tree is a few levels deep.
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_libyaml_adapter_allows_unsafe_code() {
    // Built so this file does not match itself.
    let needle = concat!("allow(", "unsafe_code)");
    let mut files = Vec::new();
    rust_files(&crate_dir().join("src"), &mut files);
    rust_files(&crate_dir().join("tests"), &mut files);
    let mut hits: Vec<String> = files
        .iter()
        .filter(|f| fs::read_to_string(f).unwrap().contains(needle))
        .map(|f| {
            f.strip_prefix(crate_dir())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    hits.sort();
    assert_eq!(hits, ["src/libyaml.rs"]);
}
