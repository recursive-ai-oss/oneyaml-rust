//! Every public API is used in a
//! real-world example: every `pub fn` of the public surface is called in
//! an example (`examples/*.rs`) or in a doc example (a fenced code block in
//! a `///`/`//!` comment of `src/`, or in `README.md`, which is the
//! crate's doc).
//!
//! The check covers the whole public surface: the `Document`/`Stream` edit and
//! comment API, the serializer, the conversions and
//! diagnostics (`from_value`, the `serde_json` conversions, `Error::render`,
//! the `Render` trait, the `miette` feature's `DiagnosticReport`; scanned
//! whatever features are on: the check reads text), and
//! the whole public surface: every file of `src/` (the handle API, `key.rs`,
//! `Value`/`Mapping`/`Sequence`/`Number`/`Tag`, `Spanned`, the `from_*`
//! entry points) and the `oneyaml-core` vocabulary `oneyaml` re-exports
//! (`Settings`, `Limits`, `Emit`, `Path`, `Diagnostic`, `Code`, `LineIndex`,
//! `Span`), whose doc examples count too.
//!
//! The check is by name, mechanically: `name(` must appear as a call
//! (`.name(`, `::name(`, or `name(` after a non-identifier character) in
//! that code. A name listed in [`NOT_CALLED`] is exempt, with the reason.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Public functions that are not called by name, and why.
const NOT_CALLED: &[(&str, &str)] = &[
    (
        "index_into",
        "`#[doc(hidden)]` method of the sealed `ValueIndex` trait; used through `Value::get` and `value[..]`",
    ),
    (
        "index_into_mut",
        "`#[doc(hidden)]` method of the sealed `ValueIndex` trait; used through `Value::get_mut`",
    ),
    (
        "same_allocation",
        "`#[doc(hidden)]` test hook of `Tag` behind the unstable `__internal-checks` feature, outside the stability promise",
    ),
    (
        "shares_prefix_with",
        "`#[doc(hidden)]` test hook of `Tag` behind the unstable `__internal-checks` feature, outside the stability promise",
    ),
    (
        "push_segment",
        "`oneyaml_core::__private` (`#[doc(hidden)]`): support for the `path!` macro, used through it",
    ),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The `oneyaml-core` sources (re-exported by `oneyaml`).
fn core_src() -> PathBuf {
    root().join("../oneyaml-core/src")
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

/// The names of the `pub fn`s declared in `files` (not `pub(super)` or
/// `pub(crate)`), and of the methods declared in a `pub trait`, outside
/// `#[cfg(test)]` files.
fn pub_fns(files: &[PathBuf]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in files {
        if f.file_name().is_some_and(|n| n == "tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(f).unwrap();
        let mut in_pub_trait = false;
        for line in text.lines() {
            // A `pub trait` block ends at its closing `}` in column 0.
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

/// The code of the doc examples in `text`: fenced blocks of `///` and
/// `//!` comments (or of a Markdown file when `markdown`).
fn doc_code(text: &str, markdown: bool) -> String {
    let mut code = String::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim_start();
        let body = if markdown {
            Some(t)
        } else {
            t.strip_prefix("///").or_else(|| t.strip_prefix("//!"))
        };
        let Some(body) = body else {
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

/// The example code: `examples/*.rs`, and the doc examples of `src/`,
/// `README.md` and `oneyaml-core`'s `src/`.
fn example_code() -> String {
    let src = root().join("src");
    let mut code = String::new();
    let mut examples = Vec::new();
    rs_files(&root().join("examples"), &mut examples);
    for f in &examples {
        code.push_str(&std::fs::read_to_string(f).unwrap());
    }
    let mut all_src = Vec::new();
    rs_files(&src, &mut all_src);
    rs_files(&core_src(), &mut all_src);
    for f in &all_src {
        code.push_str(&doc_code(&std::fs::read_to_string(f).unwrap(), false));
    }
    code.push_str(&doc_code(
        &std::fs::read_to_string(root().join("README.md")).unwrap(),
        true,
    ));
    code
}

/// One editing style: the `NodeMut` edits that mirror an
/// id-taking `Document` method, and those that return a handle to the
/// node they create, are each used *on a handle* in an example: called
/// right after `?` or `.unwrap()` (a chain), which the by-name check
/// above cannot tell from the `Document` method of the same name.
const CHAINED: &[&str] = &[
    "set",
    "set_scalar",
    "set_anchor",
    "set_leading",
    "set_trailing",
    "push_footer",
    "clear_comments",
    "set_blank_lines_before",
    "into_key",
    "push",
    "insert",
    "insert_at",
    "replace_at",
    "push_alias",
    "insert_alias",
    "insert_merge",
    "insert_removed",
    "insert_removed_at",
];

fn chained(code: &str, name: &str) -> bool {
    let pat = format!(".{name}(");
    code.match_indices(&pat).any(|(i, _)| {
        let before = code[..i].trim_end();
        before.ends_with('?') || before.ends_with(".unwrap()")
    })
}

#[test]
fn node_mut_edits_are_used_through_handles() {
    let code = example_code();
    let edit = std::fs::read_to_string(root().join("src/document/edit.rs")).unwrap();
    let missing: Vec<&str> = CHAINED
        .iter()
        .copied()
        .filter(|n| !chained(&code, n))
        .collect();
    assert!(
        missing.is_empty(),
        "NodeMut edits used on no handle in an example: {missing:?}"
    );
    // Each is a `NodeMut` method (the list stays exact).
    let node_mut = edit
        .split("impl<'d> NodeMut<'d> {")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    for n in CHAINED {
        assert!(
            node_mut.contains(&format!("pub fn {n}("))
                || node_mut.contains(&format!("pub fn {n}<")),
            "{n} is not a NodeMut method"
        );
    }
}

#[test]
fn every_public_fn_is_used_in_an_example() {
    let src = root().join("src");
    let mut declared = Vec::new();
    rs_files(&src, &mut declared);
    // The modules named by earlier units are still there.
    for file in [
        "document/edit.rs",
        "document/view.rs",
        "document/key.rs",
        "ser/mod.rs",
        "de/api.rs",
        "de/value.rs",
        "value/json.rs",
        "error.rs",
        "render.rs",
        "miette_impl.rs",
        "spanned.rs",
    ] {
        let f = src.join(file);
        assert!(declared.contains(&f), "{}", f.display());
    }
    // In the workspace, the re-exported `oneyaml-core` too (a packaged crate
    // has no sibling sources; `oneyaml-core` is then checked here, in the
    // workspace, before it is published).
    let in_workspace = core_src().is_dir();
    if in_workspace {
        rs_files(&core_src(), &mut declared);
    } else {
        eprintln!(
            "{} not found: oneyaml-core not checked",
            core_src().display()
        );
    }
    let names = pub_fns(&declared);
    let floor = if in_workspace { 250 } else { 150 };
    assert!(names.len() > floor, "found {} names", names.len());

    let code = example_code();

    let exempt: BTreeSet<&str> = NOT_CALLED.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !exempt.contains(n.as_str()) && !called(&code, n))
        .collect();
    assert!(
        missing.is_empty(),
        "public functions used in no example or doc example: {missing:?}"
    );
    // The exemptions stay exact.
    for (n, _) in NOT_CALLED {
        assert!(names.contains(*n) || !in_workspace, "stale exemption {n}");
    }
}

/// The test-only checks are compiled only with the unstable
/// `__internal-checks` feature (the attribute among those right above
/// their `pub fn` line), so they are not part of the 1.0 surface.
#[test]
fn test_only_checks_are_behind_the_internal_feature() {
    let gated = [
        ("src/document/lookup.rs", "check_lookup_indexes"),
        ("src/value/tag.rs", "same_allocation"),
        ("src/value/tag.rs", "shares_prefix_with"),
    ];
    for (file, name) in gated {
        let text = std::fs::read_to_string(root().join(file)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.trim_start().starts_with(&format!("pub fn {name}(")))
            .unwrap_or_else(|| panic!("{name} not found in {file}"));
        let gate = lines[..at]
            .iter()
            .rev()
            .take_while(|l| {
                let l = l.trim_start();
                l.starts_with("#[") || l.starts_with("///")
            })
            .any(|l| l.trim() == "#[cfg(feature = \"__internal-checks\")]");
        assert!(gate, "{name} is not behind `__internal-checks`");
    }
}
