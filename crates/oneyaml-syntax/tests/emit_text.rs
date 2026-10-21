//! The text of a tree that was not edited is its input, copied whole: the
//! same bytes as the tokens in order, for every input (valid or not); and the
//! work counters of decoding and of validating an edit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};

use oneyaml_core::Settings;
use oneyaml_syntax::tree::{
    Fragment, Node, SyntaxKind, SyntaxTree, take_decoded_bytes, take_reread_bytes,
};

fn suite_inputs() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    let mut out = Vec::new();
    let mut pending: Vec<PathBuf> = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                out.push(String::from_utf8_lossy(&fs::read(&p).unwrap()).into_owned());
            }
        }
    }
    out
}

fn token_walk(tree: &SyntaxTree) -> String {
    let mut out = String::new();
    for n in tree.root().descendants() {
        if n.is_token() {
            out.push_str(&n.text());
        }
    }
    out
}

#[test]
fn the_text_of_an_untouched_tree_is_its_input_and_its_tokens() {
    let mut inputs = suite_inputs();
    assert!(inputs.len() > 300, "yaml-test-suite data missing");
    inputs.extend(
        [
            "",
            "\n",
            "\u{feff}a: 1\n",
            "a: 1\r\nb: 2\r\n",
            "a: 1\rb: 2\r",
            "a: 'unclosed\n",
            "? [a\n",
            "\u{feff}\u{feff}a\n",
            "%YAML 1.1\n---\na\n...\n%TAG ! tag:x,2000:\n---\n!a b\n",
            "a: \u{fffd}\n",
            "- &a x\n- *a\n- *missing\n",
            "key: |\n  text\n\n\n",
            "\t\n",
        ]
        .map(str::to_owned),
    );
    for src in inputs {
        for lenient in [false, true] {
            let tree = SyntaxTree::parse_with(
                src.clone(),
                &Settings::new().lenient_inline_colons(lenient),
            );
            assert_eq!(tree.emit(), src, "{src:?}");
            assert_eq!(token_walk(&tree), src, "{src:?}");
        }
    }
}

fn scalar<'t>(tree: &'t SyntaxTree, text: &str) -> Node<'t> {
    tree.root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Scalar && n.text() == text)
        .unwrap()
}

/// Decoding a scalar counts the bytes of its source text; validating an
/// edit counts the bytes of the edited text it re-reads: the edited region and
/// its neighbours, whatever the size of the rest.
#[test]
fn decoding_and_validation_are_counted() {
    let mut src = String::from("a: 1\nbig: hello world\n");
    for i in 0..5_000 {
        src.push_str(&format!("k{i}: {i}\n"));
    }
    let mut tree = SyntaxTree::parse(src);
    let _ = take_decoded_bytes();
    assert_eq!(scalar(&tree, "hello world").value().unwrap(), "hello world");
    assert_eq!(take_decoded_bytes(), 11);
    let one = scalar(&tree, "1").id();
    let _ = take_reread_bytes();
    let _ = tree.replace(one, Fragment::parse("2").unwrap()).unwrap();
    let read = take_reread_bytes();
    assert!(read > 0 && read < 40, "{read} bytes re-read");
}
