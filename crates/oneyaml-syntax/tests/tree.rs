//! The syntax tree: a million levels of nesting on a
//! small stack (build, emit, traverse, `Debug`, `Eq`, `Hash`, `Clone`,
//! `Drop`), generated garbage and truncations (a fuzz run), and the
//! byte-exact round trip through every text encoding.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use std::hash::{BuildHasher, RandomState};

use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::tree::{SyntaxKind, SyntaxTree, WalkEvent, check_tree};
use oneyaml_syntax::{Encoding, decode, encode, parse_with};

const DEPTH: usize = 1_000_000;

/// Discards what it is given, so `Debug` of a huge tree costs no memory.
struct Sink(usize);

impl core::fmt::Write for Sink {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

/// Everything the tree offers, on a 128 KiB stack: any recursion on depth
/// would overflow it.
fn deep(src: String, expect_depth: usize) {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let settings = Settings::new().limits(Limits::unlimited());
            let tree = SyntaxTree::parse_with(src.as_str(), &settings);
            assert!(!tree.has_errors());
            assert_eq!(tree.emit().as_bytes(), src.as_bytes());
            let events = parse_with(&src, &settings);
            check_tree(&events, &tree).unwrap();
            drop(events);
            let mut depth = 0usize;
            let mut max = 0usize;
            for step in tree.root().walk() {
                match step {
                    WalkEvent::Enter(n) if n.kind().is_yaml_node() => {
                        depth += 1;
                        max = max.max(depth);
                    }
                    WalkEvent::Leave(n) if n.kind().is_yaml_node() => depth -= 1,
                    _ => {}
                }
            }
            assert_eq!(max, expect_depth);
            // The innermost node's ancestors and spans.
            let leaf = tree
                .root()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::Scalar)
                .last()
                .unwrap();
            assert!(leaf.ancestors().count() >= expect_depth);
            let outer = tree.root().first_child().unwrap().first_child().unwrap();
            assert_eq!(outer.text(), src.trim_end());
            let _ = outer.last_child().unwrap().prev_sibling();
            let mut sink = Sink(0);
            core::fmt::write(&mut sink, format_args!("{tree:?}")).unwrap();
            assert!(sink.0 > src.len());
            let copy = tree.clone();
            assert_eq!(copy, tree);
            let s = RandomState::new();
            assert_eq!(s.hash_one(&copy), s.hash_one(&tree));
            drop(copy);
            drop(tree);
        })
        .unwrap()
        .join()
        .expect("no panic or stack overflow");
}

#[test]
fn a_million_flow_levels() {
    deep(
        format!("{}x{}\n", "[".repeat(DEPTH), "]".repeat(DEPTH)),
        DEPTH + 1,
    );
}

#[test]
fn a_million_block_levels() {
    deep(format!("{}x\n", "- ".repeat(DEPTH)), DEPTH + 1);
}

#[test]
fn a_million_flow_mapping_levels() {
    deep(
        format!("{}x{}\n", "{a: ".repeat(DEPTH), "}".repeat(DEPTH)),
        DEPTH + 1,
    );
}

/// A tiny deterministic generator (no dev-dependency needed).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as usize
    }
}

const PIECES: &[&str] = &[
    "a",
    "b",
    " ",
    "  ",
    "\t",
    "\n",
    "\r\n",
    "\r",
    ":",
    ": ",
    "-",
    "- ",
    "?",
    "? ",
    "#",
    " #c",
    "&x",
    "*x",
    "!",
    "!!str",
    "!e!t",
    "!<u>",
    "'",
    "''",
    "\"",
    "\\",
    "\\n",
    "\\x4",
    "---",
    "...",
    "%YAML 1.2",
    "%YAML 1.1",
    "%TAG !e! p",
    "%",
    "|",
    ">",
    "|-",
    ">+2",
    "[",
    "]",
    "{",
    "}",
    ",",
    "@",
    "`",
    "\u{feff}",
    "é",
    "\u{1}",
    "\u{a0}",
    "k: v",
    "\n  ",
    "\n- ",
    "\n? ",
    "\n: ",
];

fn settings_under_test() -> [Settings; 3] {
    let tight = Limits::new()
        .max_depth(2)
        .max_scalar_len(3)
        .max_document_size(40)
        .max_documents(2);
    [
        Settings::new(),
        Settings::new().lenient_inline_colons(true),
        Settings::new().limits(tight),
    ]
}

/// A fuzz run: garbage from a fixed seed, under every setting the parser
/// reads, must build a tree that agrees with its stream and round-trips.
#[test]
fn generated_garbage_round_trips() {
    let mut rng = Lcg(0x7ee5);
    let settings = settings_under_test();
    for n in 0..30_000 {
        let len = rng.next() % 32;
        let src: String = (0..len)
            .map(|_| PIECES[rng.next() % PIECES.len()])
            .collect();
        let s = &settings[n % settings.len()];
        let events = parse_with(&src, s);
        let tree = SyntaxTree::from_events(events.sourced());
        if let Err(err) = check_tree(&events, &tree) {
            panic!("{src:?}: {err}\n{tree:?}");
        }
    }
}

#[test]
fn every_truncation_round_trips() {
    let docs = [
        "%YAML 1.2\n%TAG !e! tag:e,2000:\n--- !e!a &x\nk: 'v''w'\n? - \"q\\t\n  r\"\n: *x\n...\n",
        "a:\n  - b\n  -\n    c: d # e\n  - &y !!str\nf: |\n  g\n",
        "- - x\n  - 'y\n\n    z'\n- ? k\n  : v\n#end\r\n",
        "[a, &b {c: d, ? e}, [f: g], !t ]\n--- |+2\n   x\n\n...\n# t\n",
    ];
    for s in settings_under_test() {
        for doc in docs {
            for (i, _) in doc.char_indices().chain([(doc.len(), ' ')]) {
                let src = &doc[..i];
                let events = parse_with(src, &s);
                let tree = SyntaxTree::from_events(events.sourced());
                if let Err(err) = check_tree(&events, &tree) {
                    panic!("{src:?}: {err}");
                }
            }
        }
    }
}

/// A byte-identical round trip end to end: bytes → decode → tree → emit →
/// re-encode gives the original bytes in every encoding, with and without a
/// byte order mark.
#[test]
fn every_encoding_round_trips_through_the_tree() {
    const SAMPLE: &str =
        "a: é\r\nb: [\"\u{1F600}\", 'ü']\n# ✓\n...\n\u{feff}--- |\n  x\u{10FFFF}\n";
    for enc in [
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Utf32Le,
        Encoding::Utf32Be,
    ] {
        for bom in [false, true] {
            let text = if bom {
                format!("\u{feff}{SAMPLE}")
            } else {
                SAMPLE.to_owned()
            };
            let bytes = encode(&text, enc).unwrap();
            let decoded = decode(&bytes);
            let tree = SyntaxTree::parse(decoded.text());
            assert!(!tree.has_errors(), "{enc:?}");
            assert_eq!(
                encode(&tree.emit(), decoded.encoding()).unwrap(),
                bytes,
                "{enc:?} {bom}"
            );
            assert_eq!(
                tree.root().first_child().unwrap().kind(),
                if bom {
                    SyntaxKind::ByteOrderMark
                } else {
                    SyntaxKind::Document
                }
            );
        }
    }
}
