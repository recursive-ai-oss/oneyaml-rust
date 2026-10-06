//! The public API-leak check. `oneyaml`'s stable surface names
//! no `oneyaml-syntax` type; `Document::syntax()`/`Stream::syntax()` are
//! the only escape hatches (outside the stability promise).
//!
//! A source scan, cheap and dependency-free: every `pub` item signature
//! (functions up to their body, types, fields, constants, re-exports) of
//! the library, `#[doc(hidden)]` items and `#[cfg(test)]` code excluded,
//! must not mention `oneyaml_syntax` or a name imported from it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::fs;
use std::path::{Path, PathBuf};

/// The escape hatches: the only public functions allowed to name syntax
/// types.
const ALLOWED: &[&str] = &["fn syntax("];

fn sources() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.extension().is_some_and(|e| e == "rs")
                && p.file_name().is_some_and(|n| n != "tests.rs")
            {
                out.push(p);
            }
        }
    }
    out
}

/// Names a file imports from `oneyaml_syntax` (aliases included).
fn syntax_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("use oneyaml_syntax") {
        let stmt = &rest[at..];
        let end = stmt.find(';').unwrap_or(stmt.len());
        for word in stmt[..end].split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            let is_type = word.chars().next().is_some_and(char::is_uppercase);
            if is_type {
                names.push(word.to_owned());
            }
        }
        rest = &stmt[end..];
    }
    names
}

fn mentions(sig: &str, name: &str) -> bool {
    sig.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| w == name)
}

/// Public item signatures: (line, signature), `#[doc(hidden)]` skipped.
fn public_signatures(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        let public = t.starts_with("pub ") && !t.starts_with("pub(");
        let hidden = i > 0 && lines[i - 1].trim() == "#[doc(hidden)]";
        if public && !hidden {
            // A signature ends at its body or at `;` / `,` (fields).
            let mut sig = String::new();
            let mut j = i;
            while j < lines.len() {
                sig.push_str(lines[j]);
                sig.push('\n');
                let l = lines[j].trim_end();
                if l.ends_with('{') || l.ends_with(';') || l.ends_with(',') || l.ends_with('}') {
                    break;
                }
                j += 1;
            }
            // A tuple struct's private fields are not API.
            if let Some(open) = sig
                .find('(')
                .filter(|_| sig.trim_start().starts_with("pub struct"))
            {
                if !sig[open..].contains("pub ") {
                    sig.truncate(open);
                }
            }
            out.push((i + 1, sig));
            i = j;
        }
        i += 1;
    }
    out
}

#[test]
fn stable_api_names_no_syntax_type() {
    let files = sources();
    assert!(files.len() > 10, "found the library sources");
    let mut leaks = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        // Test modules (`#[cfg(test)] mod …`) cut off; other `cfg(test)`
        // items are private helpers and scanned anyway.
        let library = text.split("#[cfg(test)]\nmod ").next().unwrap_or("");
        let names = syntax_names(library);
        for (line, sig) in public_signatures(library) {
            if ALLOWED.iter().any(|a| sig.contains(a)) {
                continue;
            }
            let leak = sig.contains("oneyaml_syntax") || names.iter().any(|n| mentions(&sig, n));
            if leak {
                leaks.push(format!("{}:{line}: {}", file.display(), sig.trim()));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "syntax types in the stable API:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn the_scan_finds_leaks() {
    // The checker itself: a leak in each form is found, the escape hatch
    // and hidden items are not.
    let src = "use oneyaml_syntax::{Encoding, tree::SyntaxTree as Tree};\n\
               pub fn a() -> Encoding {\n}\n\
               pub fn syntax(&self) -> &Tree {\n}\n\
               #[doc(hidden)]\npub fn hidden() -> Tree {\n}\n\
               pub use oneyaml_syntax as syntax;\n\
               pub struct S {\n    pub field: Tree,\n}\n\
               pub fn fine() -> u8 {\n}\n\
               pub struct Handle(Tree);\n\
               pub struct Open(pub Tree);\n";
    let names = syntax_names(src);
    assert!(names.contains(&"Encoding".to_owned()) && names.contains(&"Tree".to_owned()));
    let flagged: Vec<usize> = public_signatures(src)
        .into_iter()
        .filter(|(_, sig)| !ALLOWED.iter().any(|a| sig.contains(a)))
        .filter(|(_, sig)| sig.contains("oneyaml_syntax") || names.iter().any(|n| mentions(sig, n)))
        .map(|(l, _)| l)
        .collect();
    assert_eq!(flagged, vec![2, 9, 11, 16], "{flagged:?}");
}
