//! The repository README's Quick start is a copy of the crate README's,
//! which runs as a doctest. This keeps the copy from drifting: the code
//! blocks under the two `## Quick start` headings must be identical.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::path::Path;

/// The first fenced code block (with its info string) after the
/// `## Quick start` heading of `text`.
fn quick_start(text: &str, what: &str) -> String {
    let start = text
        .find("\n## Quick start\n")
        .unwrap_or_else(|| panic!("{what}: no `## Quick start` heading"));
    let section = &text[start + 1..];
    let section = match section[1..].find("\n## ") {
        Some(end) => &section[..end + 1],
        None => section,
    };
    let open = section
        .find("\n```")
        .unwrap_or_else(|| panic!("{what}: no code block under `## Quick start`"));
    let block = &section[open + 1..];
    let body_start = block.find('\n').unwrap() + 1;
    let close = block[body_start..]
        .find("\n```")
        .unwrap_or_else(|| panic!("{what}: unclosed code block under `## Quick start`"));
    block[..body_start + close + 4].to_owned()
}

#[test]
fn root_quick_start_matches_the_doctested_crate_readme() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap();
    let crate_block = quick_start(&read("crates/oneyaml/README.md"), "crate README");
    let root_block = quick_start(&read("README.md"), "README.md");
    assert!(crate_block.starts_with("```rust\n"), "{crate_block}");
    assert_eq!(
        root_block, crate_block,
        "README.md's Quick start differs from crates/oneyaml/README.md's (the doctested one); \
         copy it over"
    );
}

#[test]
fn the_extraction_sees_a_difference() {
    let a = "# a\n\n## Quick start\n\nText.\n\n```rust\nlet x = 1;\n```\n\n## Next\n";
    let b = a.replace("let x = 1;", "let x = 2;");
    assert_eq!(quick_start(a, "a"), "```rust\nlet x = 1;\n```");
    assert_ne!(quick_start(a, "a"), quick_start(&b, "b"));
}
