//! Every public API is used, also for `-syntax`'s own semver track: every `pub
//! fn` of `oneyaml-syntax` (and every method of a `pub trait`) is called
//! somewhere that exercises it: this crate's tests (`tests/*.rs`, the
//! `#[cfg(test)]` modules and `tests.rs` files of `src/`), its examples, a doc
//! example in `src/`, or `oneyaml`'s sources, the crate's consumer (`-syntax`
//! is reached through `oneyaml`).
//!
//! The check is by name, mechanically, as `oneyaml`'s `api_examples.rs`:
//! `name(` must appear as a call (`.name(`, `::name(`, or `name(` after a
//! non-identifier character). A name in [`NOT_CALLED`] is exempt, with the
//! reason; the list stays exact.
//!
//! `oneyaml`'s sources are read from the workspace (`../oneyaml/src`).
//! Outside it (a packaged crate) that half of the evidence is absent, so
//! the test only prints what it could not account for.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Public functions that are not called by name, and why.
const NOT_CALLED: &[(&str, &str)] = &[];

/// Public functions only this crate's tests use, kept public for
/// them but `#[doc(hidden)]` (outside the documented surface). We
/// kept these visible, as genuinely useful to users: `Decoded::encoder`
/// (writes text back in the input's encoding), `Decoded::into_text` (the
/// text without a copy), `Event::node_info` (a node event's properties
/// whatever its kind), `Fragment::unwrap_root` and `Fragment::root_kinds`
/// (how a parsed collection's entries go into another collection); we
/// also kept `EventList::get_sourced` (the O(1) indexed sourced read
/// the docs present) and `DirectiveKind::reserved` (as its siblings
/// `yaml` and `tag`, the one constructor of its variant).
const TEST_ONLY: &[&str] = &[
    "event",
    "has_bom",
    "has_no_properties",
    "heap_bytes",
    "insert_child",
    "root_count",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

fn is_test_file(f: &Path) -> bool {
    f.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "tests.rs" || n.ends_with("_tests.rs"))
}

/// The names of the `pub fn`s declared in `files` (not `pub(super)` or
/// `pub(crate)`), and of the methods declared in a `pub trait`.
fn pub_fns(files: &[PathBuf]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in files.iter().filter(|f| !is_test_file(f)) {
        let text = std::fs::read_to_string(f).unwrap();
        let mut in_pub_trait = false;
        for line in text.lines() {
            if line.starts_with("pub trait ") {
                in_pub_trait = true;
            } else if line.starts_with('}') {
                in_pub_trait = false;
            }
            let t = line.trim_start();
            let rest = t
                .strip_prefix("pub fn ")
                .or_else(|| t.strip_prefix("pub const fn "))
                .or_else(|| t.strip_prefix("fn ").filter(|_| in_pub_trait));
            if let Some(rest) = rest {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                out.insert(name);
            }
        }
    }
    out
}

/// The code of the doc examples in `text` (fenced blocks of `///`/`//!`).
fn doc_code(text: &str) -> String {
    let mut code = String::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim_start();
        let Some(body) = t.strip_prefix("///").or_else(|| t.strip_prefix("//!")) else {
            inside = false;
            continue;
        };
        let body = body.trim_start();
        if body.starts_with("```") {
            inside = !inside;
            continue;
        }
        if inside {
            code.push_str(body);
            code.push('\n');
        }
    }
    code
}

fn called(code: &str, name: &str) -> bool {
    let pat = format!("{name}(");
    code.match_indices(&pat).any(|(i, _)| {
        code[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
    })
}

/// This crate's own uses: integration tests, examples, unit tests (the
/// `tests.rs` files and the inline `#[cfg(test)] mod tests` of `src/`),
/// and doc examples.
fn own_uses(src_files: &[PathBuf]) -> String {
    let mut code = String::new();
    let mut files = Vec::new();
    rs_files(&root().join("tests"), &mut files);
    rs_files(&root().join("examples"), &mut files);
    for f in &files {
        code.push_str(&std::fs::read_to_string(f).unwrap());
    }
    for f in src_files {
        let text = std::fs::read_to_string(f).unwrap();
        if is_test_file(f) {
            code.push_str(&text);
            continue;
        }
        if let Some(i) = text.find("#[cfg(test)]\nmod tests {") {
            code.push_str(&text[i..]);
        }
        code.push_str(&doc_code(&text));
    }
    code
}

#[test]
fn every_public_fn_is_used_by_a_test_or_by_oneyaml() {
    let mut src = Vec::new();
    rs_files(&root().join("src"), &mut src);
    let names = pub_fns(&src);
    assert!(names.len() > 100, "found {} names", names.len());
    let own = own_uses(&src);
    let consumer_dir = root().join("../oneyaml/src");
    let mut consumer = String::new();
    let mut files = Vec::new();
    rs_files(&consumer_dir, &mut files);
    for f in &files {
        consumer.push_str(&std::fs::read_to_string(f).unwrap());
    }
    for (n, _) in NOT_CALLED {
        assert!(names.contains(*n), "stale exemption {n}");
    }
    let exempt: BTreeSet<&str> = NOT_CALLED.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !exempt.contains(n.as_str()))
        .filter(|n| !called(&own, n) && !called(&consumer, n))
        .collect();
    if !consumer_dir.is_dir() {
        eprintln!(
            "{} not found (outside the workspace); not accounted for by this crate alone: {missing:?}",
            consumer_dir.display()
        );
        return;
    }
    assert!(
        missing.is_empty(),
        "public functions of oneyaml-syntax used by no test, example, doc example, or oneyaml: {missing:?}"
    );
}

/// Every [`TEST_ONLY`] function is declared `#[doc(hidden)]` (the
/// attribute among those right above its `pub fn` line), and no other
/// code than this crate's tests calls it (`oneyaml` included).
#[test]
fn test_only_functions_are_hidden_and_unused_outside_tests() {
    let mut src = Vec::new();
    rs_files(&root().join("src"), &mut src);
    let mut hidden = BTreeSet::new();
    for f in src.iter().filter(|f| !is_test_file(f)) {
        let text = std::fs::read_to_string(f).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let Some(rest) = t
                .strip_prefix("pub fn ")
                .or_else(|| t.strip_prefix("pub const fn "))
            else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let attrs = lines[..i]
                .iter()
                .rev()
                .take_while(|l| {
                    let l = l.trim_start();
                    l.starts_with("#[") || l.starts_with("///")
                })
                .any(|l| l.trim() == "#[doc(hidden)]");
            if attrs {
                hidden.insert(name);
            }
        }
    }
    let mut consumer = String::new();
    let mut files = Vec::new();
    rs_files(&root().join("../oneyaml/src"), &mut files);
    for f in &files {
        consumer.push_str(&std::fs::read_to_string(f).unwrap());
    }
    for n in TEST_ONLY {
        assert!(hidden.contains(*n), "{n} is not #[doc(hidden)]");
        assert!(!called(&consumer, n), "oneyaml calls test-only {n}");
    }
}
