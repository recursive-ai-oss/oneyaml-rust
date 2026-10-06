//! Tree-level edit-script fuzz: one interpretation, and
//! no panics.
//!
//! Random sequences of mutation calls over the yaml-test-suite inputs
//! (read from `crates/test-suite/data`) and generated documents, checked
//! against an **independent model**: the text as a `String` spliced by
//! hand for each accepted edit, and **every** element handle ever seen,
//! node or token (zero-width tokens included), with its kind and its
//! ancestors when it was first seen. An element dies with the subtree it
//! is in; a token's offset moves with the splices; a node's span is the
//! hull of its live tokens in the model. The splice ranges come from the
//! model, not from the tree. A refused edit must leave text and handles as
//! they were. After every step the full read consistency check runs.
//!
//! The **well-formedness implication** is checked after
//! every accepted edit of every script: the edited text re-parses (under
//! the tree's settings) with exactly the edited tree's diagnostics and
//! structure, errors or not (the error sets are equal).
//!
//! Three kinds of rounds:
//!
//! * **well-formed by construction** (inputs without errors): word
//!   replacement of single-line plain scalars, comment replacement,
//!   removing a sibling range and putting it back, replacing a node by a
//!   copy of itself, swapping a flow scalar for another plain scalar. The
//!   last three must be accepted; the re-parse is checked at the end;
//! * **arbitrary**: any primitive on any element with fragments from the
//!   tree itself, from earlier removals, and from parsed snippets,
//!   including edits the tree refuses;
//! * **diagnostics moved**: a generated document, `...`, then a suite
//!   input with errors; well-formed edits in the first document move every
//!   later diagnostic, and the result must equal its own re-parse,
//!   diagnostics and their spans included.
//!
//! §5.4 (CR, LF and CRLF are all `b-break` on
//! the edit path): every line break of a round's document and of the
//! snippets it inserts is written as LF, CRLF or CR, by blocks of ten
//! rounds (`(i / 10) % 3`; `TOKEN_TEXTS` keeps all three line breaks, so
//! documents get mixed ones too). The block index is independent
//! of `i % 2` and `i % 5`, which pick the round kind, so every kind meets
//! every line break; the coverage asserts count rounds per kind and per
//! line break. A dedicated test,
//! `the_same_script_costs_the_same_on_every_line_break`, runs the same
//! script over one document written with each line break and checks the
//! outcomes are the same and the cost is too.
//!
//! Deterministic and sectionable: round `i` is seeded by `i` alone;
//! `ONEYAML_EDIT_FUZZ_FROM` / `ONEYAML_EDIT_FUZZ_ROUNDS` pick the rounds
//! (default `0..5000`). Any section of at least 1,000 rounds passes the
//! coverage floors: the per-kind, per-line-break and `insert_line_after` floors
//! are rates per 1,000 rounds scaled to the section's length (so a longer
//! section needs no new floors), the others are minimums a 1,000-round section
//! meets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod edit_support;

use std::path::{Path, PathBuf};

use edit_support::{invariants, reparses};
use oneyaml_core::Settings;
use oneyaml_syntax::tree::{
    Fragment, Node, NodeId, SyntaxKind as K, SyntaxTree, TreeEditError, compare_trees,
};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut r = Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x2545_f491_4f6c_dd1d);
        r.next();
        r
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Every `in.yaml` of the vendored suite, sorted.
fn suite_inputs() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
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
    assert!(out.len() >= 350, "{}", out.len());
    out.into_iter()
        .map(|(_, b)| String::from_utf8_lossy(&b).into_owned())
        .collect()
}

/// `text` with every `\n` written as `eol`.
fn with_eol(text: &str, eol: &str) -> String {
    if eol == "\n" {
        text.to_owned()
    } else {
        text.replace('\n', eol)
    }
}

/// The line break of round `round`, and its index (LF 0, CRLF 1, CR 2).
/// No draw from the round's random stream. Chosen by
/// the block of ten rounds, so a block holds every residue of `round % 2`
/// and `round % 5` (the kind of the round) under one line break; an
/// earlier `round % 10 == 3 | 7` made every CR/CRLF round odd and never a
/// diagnostics round, so none of them was well-formed.
fn round_eol(round: usize) -> (&'static str, usize) {
    match (round / 10) % 3 {
        1 => ("\r\n", 1),
        2 => ("\r", 2),
        _ => ("\n", 0),
    }
}

/// A small valid document: nested block and flow collections, comments.
/// `text` with some `key: word` lines given a second `: ` (`key: word: z`),
/// a `Y0028` warning each under `lenient_inline_colons`.
fn lenient_values(rng: &mut Rng, text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        out.push_str(line);
        let simple = line.matches(": ").count() == 1
            && !line.contains(['[', '{', '|', '*', '&', '?', '#', '\'', '"', '~'])
            && line.ends_with(|c: char| c.is_alphanumeric());
        if simple && rng.below(2) == 0 {
            out.push_str(": z");
        }
        out.push('\n');
    }
    out
}

fn generate(rng: &mut Rng) -> String {
    // Non-ASCII: 2-, 3- and 4-byte characters and a combining
    // mark, so byte offsets and char counts differ everywhere.
    let words = [
        "a",
        "bb",
        "c1",
        "\u{e9}",
        "42",
        "true",
        "'q\u{1f600}'",
        "\"d\u{2603}\"",
        "~",
        "e\u{301}",
        "\u{1f600}x",
    ];
    let mut out = String::new();
    // No recursion, in tests too: an explicit stack of (indent, remaining
    // entries).
    let mut stack = vec![(0usize, 2 + rng.below(4))];
    while let Some((indent, left)) = stack.pop() {
        if left == 0 {
            continue;
        }
        stack.push((indent, left - 1));
        let pad = " ".repeat(indent);
        let key = format!("k{}", rng.below(1000));
        match rng.below(9) {
            0 if stack.len() < 5 => {
                out.push_str(&format!("{pad}{key}:\n"));
                stack.push((indent + 2, 1 + rng.below(3)));
            }
            1 => {
                let items: Vec<&str> = (0..rng.below(4))
                    .map(|_| words[rng.below(words.len())])
                    .collect();
                out.push_str(&format!("{pad}{key}: [{}]\n", items.join(", ")));
            }
            2 => out.push_str(&format!(
                "{pad}{key}: {{{}: {}}} # c{}\n",
                words[rng.below(3)],
                words[rng.below(words.len())],
                rng.below(9)
            )),
            3 => out.push_str(&format!(
                "{pad}{key}:\n{pad}  - {}\n{pad}  - {}\n",
                words[rng.below(9)],
                words[rng.below(9)]
            )),
            4 => out.push_str(&format!("{pad}{key}: |\n{pad}  line {}\n", rng.below(9))),
            5 => out.push_str(&format!("{pad}{key}:\n{pad}- {}\n", words[rng.below(9)])),
            6 => out.push_str(&format!(
                "{pad}? {key}\n{pad}: &a{} {}\n",
                rng.below(3),
                words[rng.below(9)]
            )),
            7 => out.push_str(&format!("{pad}{key}: *a{}\n", rng.below(3))),
            _ => out.push_str(&format!("{pad}{key}: {}\n", words[rng.below(words.len())])),
        }
    }
    out
}

/// One element the model knows.
#[derive(Clone)]
struct Tracked {
    id: NodeId,
    kind: K,
    /// Its ancestors when first seen (edits never move an element).
    ancestors: Vec<NodeId>,
    alive: bool,
    /// A token's text and offset (`None` while a zero-width token's place
    /// is ambiguous: an insertion at its very offset).
    token: Option<(String, Option<usize>)>,
}

/// The independent model: the text, and every element seen.
#[derive(Clone)]
struct Model {
    /// The line break the round writes.
    eol: &'static str,
    text: String,
    items: Vec<Tracked>,
    /// Places taken from the tree (elements with no bytes to place them).
    fallbacks: usize,
}

impl Model {
    fn new(tree: &SyntaxTree, eol: &'static str) -> Self {
        let mut m = Self {
            eol,
            text: tree.emit(),
            items: Vec::new(),
            fallbacks: 0,
        };
        m.track(tree, tree.root().id());
        m
    }

    /// Tracks every element under `id` (new elements).
    fn track(&mut self, tree: &SyntaxTree, id: NodeId) {
        let n = tree.get(id).unwrap();
        for t in n.descendants() {
            let token = t
                .is_token()
                .then(|| (t.text().into_owned(), Some(t.span().start)));
            self.items.push(Tracked {
                id: t.id(),
                kind: t.kind(),
                ancestors: t.ancestors().map(|a| a.id()).collect(),
                alive: true,
                token,
            });
        }
    }

    /// Tracks the inserted top-level elements `first..=last`.
    fn track_range(&mut self, tree: &SyntaxTree, first: NodeId, last: NodeId) {
        let mut at = tree.get(first).ok();
        while let Some(e) = at {
            self.track(tree, e.id());
            if e.id() == last {
                break;
            }
            at = e.next_sibling();
        }
    }

    /// `id` and everything under it die.
    fn kill(&mut self, id: NodeId) {
        for t in &mut self.items {
            if t.id == id || t.ancestors.contains(&id) {
                t.alive = false;
            }
        }
    }

    /// The model's byte range of a live element: a token's own, a node's
    /// hull of its live non-empty tokens.
    fn span(&self, id: NodeId) -> Option<(usize, usize)> {
        let mut hull: Option<(usize, usize)> = None;
        for t in &self.items {
            if !t.alive || !(t.id == id || t.ancestors.contains(&id)) {
                continue;
            }
            let Some((text, Some(s))) = &t.token else {
                continue;
            };
            if text.is_empty() && t.id != id {
                continue;
            }
            let (a, b) = (*s, s + text.len());
            hull = Some(hull.map_or((a, b), |(x, y)| (x.min(a), y.max(b))));
        }
        hull
    }

    /// Where `id` starts or ends, from the model; from the tree when the
    /// model cannot say (counted).
    fn place(&mut self, tree: &SyntaxTree, id: NodeId, end: bool) -> usize {
        match self.span(id) {
            Some((a, b)) => {
                if end {
                    b
                } else {
                    a
                }
            }
            None => {
                self.fallbacks += 1;
                let s = tree.get(id).unwrap().span();
                if end { s.end } else { s.start }
            }
        }
    }

    /// Bytes `a..b` become `with`.
    fn splice(&mut self, a: usize, b: usize, with: &str) {
        self.text.replace_range(a..b, with);
        let delta = with.len() as isize - (b - a) as isize;
        let shift = |s: usize| (s as isize + delta) as usize;
        for t in &mut self.items {
            if !t.alive {
                continue;
            }
            let Some((text, pos)) = &mut t.token else {
                continue;
            };
            let Some(s) = *pos else { continue };
            let e = s + text.len();
            if text.is_empty() {
                *pos = if s < a {
                    Some(s)
                } else if s > b || (s == b && b > a) {
                    Some(shift(s))
                } else if s == a && b > a {
                    Some(s)
                } else {
                    None
                };
            } else if s >= b && !(s == b && a == b && e == s) {
                *pos = Some(shift(s));
            } else {
                assert!(
                    e <= a,
                    "live token {:?} straddles an edit: {s}..{e} vs {a}..{b}",
                    t.kind
                );
            }
        }
    }

    fn check(&self, tree: &SyntaxTree, what: &str) {
        assert_eq!(tree.emit(), self.text, "bytes after {what}");
        for t in &self.items {
            if !t.alive {
                assert!(tree.get(t.id).is_err(), "dead handle resolves after {what}");
                continue;
            }
            let n = tree
                .get(t.id)
                .unwrap_or_else(|e| panic!("live handle stale after {what}: {e}"));
            assert_eq!(n.kind(), t.kind, "{what}");
            match &t.token {
                Some((text, pos)) => {
                    assert_eq!(n.text(), text.as_str(), "{what}");
                    if let Some(s) = pos {
                        assert_eq!(n.span().start, *s, "{:?} after {what}", t.kind);
                    }
                }
                None => {
                    if let Some((a, b)) = self.span(t.id) {
                        assert_eq!((n.span().start, n.span().end), (a, b), "{n:?} after {what}");
                        assert_eq!(n.text(), &self.text[a..b], "{what}");
                    }
                }
            }
        }
    }
}

fn ids(tree: &SyntaxTree) -> Vec<NodeId> {
    tree.root().descendants().map(|n| n.id()).collect()
}

/// A fragment for arbitrary scripts.
fn some_fragment(
    rng: &mut Rng,
    tree: &SyntaxTree,
    pool: &mut Vec<Fragment>,
    eol: &str,
) -> Option<Fragment> {
    const SNIPPETS: &[&str] = &[
        "x",
        "\n",
        " ",
        "# c",
        "[a, b]",
        "{k: v}",
        "k: v",
        "- a\n- b",
        "'q'",
        "&a x",
        "*a",
        "--- d\n",
        "|\n  lit\n",
        "!!str s",
        "\n  ",
        "",
        ", ",
        ": ",
        "- ",
        "\n- c",
        "\nk2: v2",
        "|+\n  keep\n",
        " # c",
        "\t",
        "...\n",
        "--- ",
        "\"a\n b\"",
        "!t x",
        ">-\n  f\n",
        "? a\n: b",
        "\n  - n",
        "k:",
        "\n\n",
        // Non-ASCII.
        "\u{e9}",
        "\u{e9}: \u{e9}",
        "\n\u{e9}: \u{1f600}",
        "\n- e\u{301}",
        "[\u{2603}, \u{1f600}]",
        "# \u{f1}",
        "|\n  \u{1f600} l\n",
        "'\u{e9}\n  \u{1f600}'",
        "&\u{e9} x",
        "*\u{e9}",
        "\n  \u{e9}: \u{e9}",
    ];
    match rng.below(3) {
        0 => {
            let all = ids(tree);
            let x = all[rng.below(all.len())];
            let n = tree.get(x).unwrap();
            let mut last = n;
            for _ in 0..rng.below(3) {
                if let Some(s) = last.next_sibling() {
                    last = s;
                }
            }
            tree.extract(x, last.id()).ok()
        }
        1 if !pool.is_empty() => Some(pool.swap_remove(rng.below(pool.len()))),
        _ => {
            let text = with_eol(SNIPPETS[rng.below(SNIPPETS.len())], eol);
            Fragment::parse_with(&text, tree.settings()).ok()
        }
    }
}

/// Replacement texts per token kind (some ill-formed in most places).
const TOKEN_TEXTS: &[(K, &[&str])] = &[
    (
        K::PlainScalar,
        &[
            "y",
            "zz",
            "a b",
            "",
            "x\ny",
            "k: v",
            "[",
            "-",
            "?x",
            "\u{e9}",
            "\u{1f600}",
            "e\u{301}",
            "\u{e9}: \u{e9}",
            "a\u{2603}b",
        ],
    ),
    (
        K::SingleQuotedScalar,
        &[
            "'s'",
            "'a\n  b'",
            "''",
            "'\u{e9}'",
            "'\u{1f600}\n  e\u{301}'",
        ],
    ),
    (
        K::DoubleQuotedScalar,
        &["\"\\t\"", "\"\"", "\"a\n b\"", "\"\u{2603}\""],
    ),
    (K::Comment, &["# n", "#", "# x # y", "# \u{e9}\u{1f600}"]),
    (K::Whitespace, &["  ", "   ", " ", "\t", "    "]),
    (K::LineBreak, &["\n", "\r\n", "\r"]),
    (K::Anchor, &["&b", "&a", "&\u{e9}"]),
    (K::AliasName, &["*b", "*a", "*\u{e9}"]),
];

/// An accepted edit: bytes `a..b` became the text, and the inserted
/// top-level elements (first, last).
type Applied = (usize, usize, String, Option<(NodeId, NodeId)>);

/// One arbitrary step. Returns what was done, for messages.
fn arbitrary_step(
    rng: &mut Rng,
    tree: &mut SyntaxTree,
    m: &mut Model,
    pool: &mut Vec<Fragment>,
    tally: &mut Tally,
) -> String {
    let op = rng.below(8);
    let all = ids(tree);
    let tokens: Vec<NodeId> = tree
        .root()
        .descendants()
        .filter(|t| TOKEN_TEXTS.iter().any(|(k, _)| *k == t.kind()))
        .map(|t| t.id())
        .collect();
    // replace_token: half the time on a replaceable token.
    let x = if op == 7 && !tokens.is_empty() && rng.below(2) == 0 {
        tokens[rng.below(tokens.len())]
    } else {
        all[rng.below(all.len())]
    };
    let n = tree.get(x).unwrap();
    let kind = n.kind();
    let before = m.text.clone();
    let targeted = if (3..=6).contains(&op) && rng.below(2) == 0 {
        entry_copy(rng, tree)
    } else {
        None
    };
    let result: Result<Applied, TreeEditError> = if let Some((e, parent, pos, f)) = targeted {
        let t = f.text().to_owned();
        let next = tree.get(e).unwrap().next_sibling().map(|s| s.id());
        let (at, r) = match (op, next) {
            (3, Some(s)) => (m.place(tree, s, false), tree.insert_before(s, f)),
            (3 | 4, _) => (m.place(tree, e, true), tree.insert_after(e, f)),
            (5, _) => (m.place(tree, e, true), tree.insert_child(parent, pos, f)),
            _ => (m.place(tree, parent, true), tree.append_child(parent, f)),
        };
        r.map(|i| (at, at, t, Some((i.first, i.last))))
    } else {
        match op {
            0 => {
                let (a, b) = (m.place(tree, x, false), m.place(tree, x, true));
                tree.remove(x).map(|f| {
                    pool.push(f);
                    (a, b, String::new(), None)
                })
            }
            1 => {
                let mut last = n;
                for _ in 0..rng.below(4) {
                    if let Some(s) = last.next_sibling() {
                        last = s;
                    }
                }
                let last = last.id();
                let (a, b) = (m.place(tree, x, false), m.place(tree, last, true));
                let killed = range_ids(tree, x, last);
                tree.remove_range(x, last).map(|f| {
                    pool.push(f);
                    for k in killed {
                        m.kill(k);
                    }
                    (a, b, String::new(), None)
                })
            }
            2 => match match rng.below(3) {
                // A copy of itself, a plain word for a scalar, or anything.
                0 => tree.extract(x, x).ok(),
                1 if kind == K::Scalar => Fragment::parse_with("w", tree.settings()).ok(),
                _ => some_fragment(rng, tree, pool, m.eol),
            } {
                Some(f) => {
                    let t = f.text().to_owned();
                    let (a, b) = (m.place(tree, x, false), m.place(tree, x, true));
                    tree.replace(x, f).map(|r| {
                        pool.push(r.removed);
                        (a, b, t, Some((r.inserted.first, r.inserted.last)))
                    })
                }
                None => Err(TreeEditError::EmptyFragment),
            },
            3 | 4 => match some_fragment(rng, tree, pool, m.eol) {
                Some(f) => {
                    let t = f.text().to_owned();
                    let at = m.place(tree, x, op == 4);
                    let r = if op == 3 {
                        tree.insert_before(x, f)
                    } else {
                        tree.insert_after(x, f)
                    };
                    r.map(|i| (at, at, t, Some((i.first, i.last))))
                }
                None => Err(TreeEditError::EmptyFragment),
            },
            5 => {
                let kids: Vec<NodeId> = n.children().map(|c| c.id()).collect();
                let pos = rng.below(kids.len() + 2);
                let at = match kids.get(pos) {
                    Some(c) => m.place(tree, *c, false),
                    None => m.place(tree, x, !kids.is_empty()),
                };
                match some_fragment(rng, tree, pool, m.eol) {
                    Some(f) => {
                        let t = f.text().to_owned();
                        tree.insert_child(x, pos, f)
                            .map(|i| (at, at, t, Some((i.first, i.last))))
                    }
                    None => Err(TreeEditError::EmptyFragment),
                }
            }
            6 => match some_fragment(rng, tree, pool, m.eol) {
                Some(f) => {
                    let t = f.text().to_owned();
                    let at = m.place(tree, x, n.first_child().is_some());
                    tree.append_child(x, f)
                        .map(|i| (at, at, t, Some((i.first, i.last))))
                }
                None => Err(TreeEditError::EmptyFragment),
            },
            _ => {
                // Mostly a text of the token's own kind, sometimes any.
                let own = TOKEN_TEXTS.iter().find(|(k, _)| *k == kind);
                let texts = match own {
                    Some((_, texts)) if rng.below(4) != 0 => texts,
                    _ => TOKEN_TEXTS[rng.below(TOKEN_TEXTS.len())].1,
                };
                // (Line breaks keep all three texts: mixed documents.)
                let t = texts[rng.below(texts.len())];
                let t = if texts.contains(&"\r") {
                    t.to_owned()
                } else {
                    with_eol(t, m.eol)
                };
                let t = t.as_str();
                let (a, b) = (m.place(tree, x, false), m.place(tree, x, true));
                tree.replace_token(x, t)
                    .map(|id| (a, b, t.to_owned(), Some((id, id))))
            }
        }
    };
    let what = format!(
        "op {op} on {kind:?} {:?} (text before: {before:?})",
        m.span(x)
    );
    match result {
        Ok((a, b, with, new)) => {
            tally.accepted[op] += 1;
            if op != 1 && (op <= 2 || op == 7) {
                m.kill(x);
            }
            if op <= 2 || op == 7 {
                assert!(tree.get(x).is_err(), "{what}: the removed element is stale");
            }
            m.splice(a, b, &with);
            if let Some((first, last)) = new {
                m.track_range(tree, first, last);
            }
            implication(tree, tally, &what);
        }
        Err(e) => {
            assert!(
                !matches!(e, TreeEditError::Internal),
                "{what}: internal error"
            );
            if e == TreeEditError::NotWellFormed {
                tally.refused_ill_formed += 1;
            }
            assert_eq!(m.text, before);
        }
    }
    what
}

/// A copy of one collection entry with the separator before it (`, b`,
/// `\nk: v`, `\n- x`), and where a copy would fit: after its last element
/// `e` (the element, the collection, and the child position after `e`).
fn entry_copy(rng: &mut Rng, tree: &SyntaxTree) -> Option<(NodeId, NodeId, usize, Fragment)> {
    let seps: Vec<Node<'_>> = tree
        .root()
        .descendants()
        .filter(|n| {
            matches!(n.kind(), K::FlowEntry | K::LineBreak)
                && n.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        K::BlockMapping | K::BlockSequence | K::FlowMapping | K::FlowSequence
                    )
                })
        })
        .collect();
    let s = *seps.get(rng.below(seps.len().max(1)))?;
    let parent = s.parent()?;
    let mut e = s.next_sibling()?;
    while let Some(n) = e.next_sibling() {
        if n.kind() == s.kind()
            || n.kind() == K::FlowSequenceClose
            || n.kind() == K::FlowMappingClose
        {
            break;
        }
        e = n;
    }
    let pos = parent.children().position(|c| c == e)? + 1;
    let f = tree.extract(s.id(), e.id()).ok()?;
    Some((e.id(), parent.id(), pos, f))
}

/// The siblings `first..=last`.
fn range_ids(tree: &SyntaxTree, first: NodeId, last: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut at = tree.get(first).ok();
    while let Some(e) = at {
        out.push(e.id());
        if e.id() == last {
            break;
        }
        at = e.next_sibling();
    }
    out
}

/// Texts that end their line, for the line-end primitives:
/// every chomping, plain and flow content, entries, non-ASCII.
const LINE_SNIPPETS: &[&str] = &[
    "x\n",
    "|\n  l\n",
    "|-\n  l\n",
    "|+\n  l\n\n",
    ">\n  f\n  g\n",
    "|\n l\n",
    "[a, \u{e9}]\n",
    "'q'\n",
    "\u{1f600}\n",
    "k: v\n",
    "k: |\n  v\n",
    "- |-\n  e\u{301}\n",
    "- x\n",
    "\u{e9}: |+\n  \u{1f600}\n\n",
    "|\n    deep\n",
    "x\r\n",
    "\n",
    "x",
];

/// `replace_to_line_end` or `insert_line_after` on a random
/// YAML node with a text that ends its line; the model splices the text
/// in place of (or after) the node's line through its line break.
fn line_step(rng: &mut Rng, tree: &mut SyntaxTree, m: &mut Model, tally: &mut Tally) -> String {
    let nodes: Vec<NodeId> = tree
        .root()
        .descendants()
        .filter(|n| n.kind().is_yaml_node())
        .map(|n| n.id())
        .collect();
    if nodes.is_empty() {
        return "line: skip".into();
    }
    let x = nodes[rng.below(nodes.len())];
    let snippet = with_eol(LINE_SNIPPETS[rng.below(LINE_SNIPPETS.len())], m.eol);
    let snippet = snippet.as_str();
    let Ok(mut f) = Fragment::parse_with(snippet, tree.settings()) else {
        return "line: skip".into();
    };
    let collection = f
        .root_kinds()
        .next()
        .is_some_and(|k| matches!(k, K::BlockMapping | K::BlockSequence));
    if collection && rng.below(2) == 0 {
        f = f.unwrap_root(0).unwrap();
    }
    let text = f.text().to_owned();
    let insert = rng.below(2) == 0;
    // `insert_line_after` needs a node whose line ends right
    // after it, and new lines (written from column 0) that fit where they
    // land; few random nodes allow either. For an insertion, move to the
    // first node from the drawn one on (in preorder, wrapping) that ends
    // its line in a column-0 block collection the text's shape fits
    // (an entry `k: v` in a mapping, an item `- x` in a sequence), else to
    // the first that ends its line, else keep it. No draw is added or
    // removed, so every other step's random stream is unchanged; the
    // splice ranges still come from the model.
    let x = if insert {
        let kinds: Vec<K> = f.root_kinds().collect();
        let want = if kinds.first() == Some(&K::SequenceEntry) {
            Some(K::BlockSequence)
        } else if kinds.contains(&K::MappingValue) {
            Some(K::BlockMapping)
        } else {
            None
        };
        let at_line_start =
            |pos: usize| pos == 0 || m.text.get(..pos).is_some_and(|t| t.ends_with(['\n', '\r']));
        let score = |id: NodeId| {
            let n = tree.get(id).unwrap();
            let ends_line = n.text().ends_with(['\n', '\r'])
                || m.text
                    .get(n.span().end..)
                    .is_some_and(|t| t.starts_with(['\n', '\r']));
            let fits = n
                .parent()
                .is_some_and(|p| Some(p.kind()) == want && at_line_start(p.span().start));
            match (ends_line, fits) {
                (true, true) => 2,
                (true, false) => 1,
                _ => 0,
            }
        };
        let at = nodes.iter().position(|n| *n == x).unwrap_or(0);
        let order = || nodes[at..].iter().chain(&nodes[..at]).copied();
        order()
            .find(|id| score(*id) == 2)
            .or_else(|| order().find(|id| score(*id) == 1))
            .unwrap_or(x)
    } else {
        x
    };
    let before = m.text.clone();
    let (a, e) = (m.place(tree, x, false), m.place(tree, x, true));
    let inner = tree.get(x).unwrap().text().ends_with(['\n', '\r']);
    let tail = &m.text[e..];
    let brk = if inner {
        0
    } else if tail.starts_with("\r\n") {
        2
    } else if tail.starts_with(['\n', '\r']) {
        1
    } else {
        0
    };
    // The model's line break token after the node, if any.
    let lb = (brk > 0)
        .then(|| {
            m.items.iter().find(|t| {
                t.alive
                    && t.kind == K::LineBreak
                    && matches!(&t.token, Some((_, Some(s))) if *s == e)
            })
        })
        .flatten()
        .map(|t| t.id);
    let what = format!(
        "line op {} {snippet:?} on {:?} (text before: {before:?})",
        if insert { "insert" } else { "replace" },
        m.span(x)
    );
    let r = if insert {
        tree.insert_line_after(x, f)
    } else {
        tree.replace_to_line_end(x, x, f).map(|r| r.inserted)
    };
    match r {
        Ok(ins) => {
            tally.line_accepted[usize::from(insert)] += 1;
            if let Some(l) = lb {
                if tree.get(l).is_err() {
                    m.kill(l);
                }
            }
            let at = if insert {
                let end = e + brk;
                m.splice(end, end, &text);
                end
            } else {
                m.kill(x);
                m.splice(a, e + brk, &text);
                a
            };
            m.track_range(tree, ins.first, ins.last);
            // The final line break, placed at an outer level, is not in
            // `ins`: every new token in the new text is tracked.
            let known: Vec<NodeId> = m.items.iter().map(|t| t.id).collect();
            let new: Vec<NodeId> = tree
                .root()
                .descendants()
                .filter(|t| {
                    t.is_token()
                        && t.span().start >= at
                        && t.span().end <= at + text.len()
                        && !known.contains(&t.id())
                })
                .map(|t| t.id())
                .collect();
            for t in new {
                m.track(tree, t);
            }
            implication(tree, tally, &what);
        }
        Err(err) => {
            assert!(
                !matches!(err, TreeEditError::Internal),
                "{what}: internal error"
            );
            assert_eq!(m.text, before, "{what}");
            assert_eq!(tree.emit(), before, "{what}: refused but changed");
        }
    }
    what
}

/// Counts for the report and the coverage asserts.
#[derive(Default)]
struct Tally {
    accepted: [usize; 8],
    /// Accepted `replace_to_line_end`, `insert_line_after`.
    line_accepted: [usize; 2],
    refused_ill_formed: usize,
    /// Accepted edits whose text re-parsed cleanly.
    implications: usize,
    /// Accepted edits whose text re-parsed with errors (compared too).
    with_errors: usize,
}

/// After every accepted edit,
/// the edited tree agrees with its own text: the same error set (codes,
/// severities, spans) as the re-parse, and the same structure, whether
/// or not the text has errors (an edit next to an error region must not
/// change how that region reads). Once, this returned early when
/// the re-parse had errors, so an accepted edit that introduced errors
/// went unseen.
fn implication(tree: &SyntaxTree, tally: &mut Tally, what: &str) {
    let again = SyntaxTree::parse_with(tree.emit(), tree.settings());
    let errors = |t: &SyntaxTree| -> Vec<_> {
        t.diagnostics()
            .map(|d| (d.code, d.severity, d.primary.span))
            .collect()
    };
    let (ours, theirs) = (errors(tree), errors(&again));
    if ours != theirs {
        panic!(
            "{what}: accepted, but the edited tree's diagnostics {ours:?} differ from its \
             re-parse's {theirs:?}\ntext {:?}",
            tree.emit()
        );
    }
    if again.has_errors() {
        tally.with_errors += 1;
    } else {
        tally.implications += 1;
    }
    if let Err(e) = compare_trees(&again, tree) {
        panic!(
            "{what}: accepted, but differs from its re-parse: {e}\ntext {:?}",
            tree.emit()
        );
    }
}

/// A plain single-line non-empty scalar token (not a lenient `a: b`,
/// whose `Y0028` warning a word in its place would drop: refused).
fn plain_word_token(n: &Node<'_>) -> bool {
    n.kind() == K::PlainScalar
        && !n.text().is_empty()
        && !n.text().contains(['\n', '\r'])
        && !n.text().contains(": ")
}

/// One well-formed step (see the module docs) on elements under `scope`.
fn wellformed_step(
    rng: &mut Rng,
    tree: &mut SyntaxTree,
    m: &mut Model,
    words: &[String],
    scope: NodeId,
    tally: &mut Tally,
) -> String {
    let all: Vec<_> = tree.get(scope).unwrap().descendants().skip(1).collect();
    if all.is_empty() {
        return "skip (empty)".into();
    }
    let op = rng.below(5);
    let what = format!("well-formed op {op}");
    match op {
        0 => {
            let cands: Vec<_> = all
                .iter()
                .filter(|n| plain_word_token(n))
                .map(|n| n.id())
                .collect();
            if let Some(&id) = cands.get(rng.below(cands.len().max(1))) {
                // Half the words non-ASCII.
                let w = ["w", "w\u{e9}\u{1f600}"][rng.below(2)];
                let w = format!("{w}{}", rng.below(100));
                let (a, b) = (m.place(tree, id, false), m.place(tree, id, true));
                // A plain word in place of another: refused only where the
                // longer key would change the text's structure (never here).
                let new = tree
                    .replace_token(id, &w)
                    .unwrap_or_else(|e| panic!("{e}: {w} at {a} in {:?}", tree.emit()));
                m.kill(id);
                m.splice(a, b, &w);
                m.track(tree, new);
            }
        }
        1 => {
            let cands: Vec<_> = all
                .iter()
                .filter(|n| n.kind() == K::Comment)
                .map(|n| n.id())
                .collect();
            if let Some(&id) = cands.get(rng.below(cands.len().max(1))) {
                let c = format!("# edited \u{f1} {}", rng.below(100));
                let (a, b) = (m.place(tree, id, false), m.place(tree, id, true));
                let new = tree.replace_token(id, &c).unwrap();
                m.kill(id);
                m.splice(a, b, &c);
                m.track(tree, new);
            }
        }
        2 => {
            // Remove a sibling range and put it back where it was.
            let n = all[rng.below(all.len())];
            let Some(parent) = n.parent() else {
                return "skip".into();
            };
            let mut last = n;
            for _ in 0..rng.below(3) {
                if let Some(s) = last.next_sibling() {
                    last = s;
                }
            }
            let (first, last_id) = (n.id(), last.id());
            let prev = n.prev_sibling().map(|p| p.id());
            let parent = parent.id();
            let (a, b) = (m.place(tree, first, false), m.place(tree, last_id, true));
            let killed = range_ids(tree, first, last_id);
            let Ok(f) = tree.remove_range(first, last_id) else {
                return "refused".into();
            };
            for k in killed {
                m.kill(k);
            }
            let text = f.text().to_owned();
            m.splice(a, b, "");
            m.check(tree, &what);
            implication(tree, tally, &what);
            let ins = match prev {
                Some(p) => tree.insert_after(p, f),
                None => tree.insert_child(parent, 0, f),
            }
            .unwrap_or_else(|e| panic!("{e}: putting back {text:?} at {a} in {:?}", tree.emit()));
            m.splice(a, a, &text);
            m.track_range(tree, ins.first, ins.last);
        }
        3 => {
            // Replace a YAML node by a copy of itself.
            let cands: Vec<_> = all.iter().filter(|n| n.kind().is_yaml_node()).collect();
            if let Some(n) = cands.get(rng.below(cands.len().max(1))) {
                let id = n.id();
                let copy = tree.extract(id, id).unwrap();
                let text = copy.text().to_owned();
                let (a, b) = (m.place(tree, id, false), m.place(tree, id, true));
                let span = n.span();
                let r = tree
                    .replace(id, copy)
                    .unwrap_or_else(|e| panic!("{e}: {span:?} {text:?} in {:?}", tree.emit()));
                assert_eq!(r.removed.text(), text);
                m.kill(id);
                m.splice(a, b, &text);
                m.track(tree, r.inserted.first);
            }
        }
        _ => {
            // A flow sequence's plain scalar becomes another plain word.
            let cands: Vec<_> = all
                .iter()
                .filter(|n| {
                    n.kind() == K::Scalar
                        && n.parent().is_some_and(|p| p.kind() == K::FlowSequence)
                        && n.children().count() == 1
                        && n.first_child().is_some_and(|t| plain_word_token(&t))
                })
                .map(|n| n.id())
                .collect();
            if let Some(&id) = cands.get(rng.below(cands.len().max(1))) {
                let w = &words[rng.below(words.len())];
                let f = Fragment::parse_with(w, tree.settings()).unwrap();
                let (a, b) = (m.place(tree, id, false), m.place(tree, id, true));
                let r = tree
                    .replace(id, f)
                    .unwrap_or_else(|e| panic!("{e}: {w} at {a} in {:?}", tree.emit()));
                m.kill(id);
                m.splice(a, b, w);
                m.track(tree, r.inserted.first);
            }
        }
    }
    implication(tree, tally, &what);
    what
}

/// The least well-formed and diagnostics-moved rounds per line
/// break per 1,000 rounds of a section (measured: 102..118 well-formed and
/// 66..68 moved per line break per 1,000 rounds). Every floor
/// below scales with the section's length ([`per_section`]), so any
/// section of at least 1,000 rounds passes.
const WF_PER_EOL: usize = 80;
const MOVED_PER_EOL: usize = 50;
/// The least arbitrary rounds per line break per 1,000 rounds
/// (measured: 139..176 per line break per 1,000 rounds over rounds
/// 0..50,000).
const ARBITRARY_PER_EOL: usize = 50;
/// The least accepted `insert_line_after` edits per 1,000 rounds.
/// About half the measured rate. Measured over rounds
/// 0..20,000 in 1,000-round sections (trivia lines are
/// accepted at the parser's place): 19..44, mean 31 (157 over 0..5,000).
const INSERT_LINE_PER_K: usize = 15;

/// A floor given per 1,000 rounds, for a section of `len` rounds.
fn per_section(rate: usize, len: usize) -> usize {
    rate * len / 1000
}

#[test]
fn edit_scripts_match_the_splice_model() {
    let inputs = suite_inputs();
    let with_errors: Vec<&String> = inputs
        .iter()
        .filter(|s| SyntaxTree::parse(s.as_str()).has_errors())
        .collect();
    let words: Vec<String> = (0..20)
        .map(|i| {
            if i % 2 == 0 {
                format!("v{i}")
            } else {
                format!("v\u{2603}{i}e\u{301}")
            }
        })
        .collect();
    let from = env("ONEYAML_EDIT_FUZZ_FROM", 0);
    let rounds = env("ONEYAML_EDIT_FUZZ_ROUNDS", 5000);
    let (mut wellformed, mut arbitrary, mut moved, mut steps) = (0usize, 0usize, 0usize, 0usize);
    let mut tally = Tally::default();
    let mut edited = 0usize;
    let mut fallbacks = 0usize;
    let (mut forks, mut fork_steps) = (0usize, 0usize);
    let (mut forks_of_forks, mut third_steps) = (0usize, 0usize);
    // Rounds written with LF, CRLF, CR, per kind:
    // `[well-formed, diagnostics-moved, arbitrary][line break]`.
    let mut eol_rounds = [0usize; 3];
    let mut kind_eol = [[0usize; 3]; 3];
    for round in from..rounds {
        let mut rng = Rng::new(round as u64);
        let diag_round = round % 5 == 4;
        // Some generated rounds read `k: a: b` values under
        // `lenient_inline_colons`, so edits meet `Y0028` warnings (whose
        // labels lie before their element).
        let lenient = !diag_round && round % 3 == 2 && round % 7 < 3;
        let src = if diag_round {
            let clean = generate(&mut rng);
            let bad = with_errors[rng.below(with_errors.len())];
            format!("{clean}...\n{bad}")
        } else if lenient {
            let text = generate(&mut rng);
            lenient_values(&mut rng, &text)
        } else if round % 3 == 2 {
            generate(&mut rng)
        } else {
            inputs[rng.below(inputs.len())].clone()
        };
        let settings = if lenient {
            Settings::new().lenient_inline_colons(true)
        } else {
            Settings::new()
        };
        let (eol, eol_ix) = round_eol(round);
        let src = with_eol(&src, eol);
        eol_rounds[eol_ix] += 1;
        let mut tree = SyntaxTree::parse_with(src.as_str(), &settings);
        // The scope of well-formed edits: the whole stream, or the clean
        // first document of a diagnostics round.
        let mut scope = Some(tree.root().id());
        if diag_round {
            let first = tree.root().first_child().unwrap();
            let clean_first = first.kind() == K::Document
                && tree.diagnostics_within(first).len() == 0
                && tree.has_errors();
            scope = clean_first.then(|| first.id());
        }
        let well = scope.is_some() && (diag_round || (!tree.has_errors() && round % 2 == 0));
        let mut m = Model::new(&tree, eol);
        let mut pool = Vec::new();
        let n_steps = 1 + rng.below(8);
        // One round in four forks the
        // tree mid-script and edits both copies on (the fork with its own
        // random stream, so the other rounds are unchanged), each checked
        // against its own copy of the model: every handle issued before
        // the fork resolves in both copies until that copy removes its
        // element, and handles issued after it by one copy are stale in
        // the other.
        let mut fork_rng = Rng::new(round as u64 ^ 0x0f0f_5eed_f0f0_5eed);
        let fork_at = (round % 4 == 1).then(|| fork_rng.below(n_steps));
        let mut side: Option<(SyntaxTree, Model, Vec<Fragment>, usize)> = None;
        // In half of those rounds the fork is forked
        // again at a later step (a fork of a fork: a second fork in the
        // round), and the third copy is edited on too, with a random
        // stream of its own (so the other copies' scripts are unchanged).
        let mut third_rng = Rng::new(round as u64 ^ 0x3c3c_5eed_c3c3_5eed);
        let refork_at = fork_at
            .filter(|_| round % 8 == 1)
            .map(|f| f + 1 + third_rng.below(n_steps.saturating_sub(f).max(1)));
        let mut third: Option<(SyntaxTree, Model, Vec<Fragment>, usize)> = None;
        for step in 0..n_steps {
            if Some(step) == refork_at {
                if let Some((b, bm, bpool, _)) = side.as_ref() {
                    let c = b.fork();
                    let cm = bm.clone();
                    cm.check(
                        &c,
                        &format!("round {round}: the fork of the fork of {src:?}"),
                    );
                    // Handles of the original issued after the first fork
                    // stay stale in the second.
                    let first_len = side.as_ref().map_or(0, |s| s.3);
                    for t in &m.items[first_len..] {
                        assert!(c.get(t.id).is_err(), "round {round}: original's handle");
                    }
                    third = Some((c, cm, bpool.clone(), bm.items.len()));
                    forks_of_forks += 1;
                }
            }
            if Some(step) == fork_at {
                let b = tree.fork();
                let bm = m.clone();
                bm.check(&b, &format!("round {round}: the fork of {src:?}"));
                side = Some((b, bm, pool.clone(), m.items.len()));
                forks += 1;
            }
            let what = match scope {
                Some(scope) if well => {
                    wellformed_step(&mut rng, &mut tree, &mut m, &words, scope, &mut tally)
                }
                // One step in four edits at a line's end.
                _ if rng.below(4) == 0 => line_step(&mut rng, &mut tree, &mut m, &mut tally),
                _ => arbitrary_step(&mut rng, &mut tree, &mut m, &mut pool, &mut tally),
            };
            let what = format!("round {round} step {step} {what} on {src:?}");
            m.check(&tree, &what);
            invariants(&tree);
            steps += 1;
            if let Some((b, bm, bpool, fork_len)) = side.as_mut() {
                let r = &mut fork_rng;
                let what = match scope {
                    Some(scope) if well => wellformed_step(r, b, bm, &words, scope, &mut tally),
                    _ if r.below(4) == 0 => line_step(r, b, bm, &mut tally),
                    _ => arbitrary_step(r, b, bm, bpool, &mut tally),
                };
                let what = format!("round {round} step {step} (fork) {what} on {src:?}");
                bm.check(b, &what);
                invariants(b);
                // The original is untouched by the fork's edits.
                m.check(&tree, &what);
                for t in &m.items[*fork_len..] {
                    assert!(
                        b.get(t.id).is_err(),
                        "original's new handle in the fork: {what}"
                    );
                }
                for t in &bm.items[*fork_len..] {
                    assert!(
                        tree.get(t.id).is_err(),
                        "fork's new handle in the original: {what}"
                    );
                }
                fork_steps += 1;
            }
            if let (Some((c, cm, cpool, c_len)), Some((b, bm, _, _))) =
                (third.as_mut(), side.as_ref())
            {
                let r = &mut third_rng;
                let what = match scope {
                    Some(scope) if well => wellformed_step(r, c, cm, &words, scope, &mut tally),
                    _ if r.below(4) == 0 => line_step(r, c, cm, &mut tally),
                    _ => arbitrary_step(r, c, cm, cpool, &mut tally),
                };
                let what = format!("round {round} step {step} (fork of fork) {what} on {src:?}");
                cm.check(c, &what);
                invariants(c);
                // The first fork and the original are untouched by it.
                bm.check(b, &what);
                m.check(&tree, &what);
                for t in &cm.items[*c_len..] {
                    assert!(
                        b.get(t.id).is_err(),
                        "third's new handle in the fork: {what}"
                    );
                    assert!(
                        tree.get(t.id).is_err(),
                        "third's new handle in the original: {what}"
                    );
                }
                for t in &bm.items[*c_len..] {
                    assert!(
                        c.get(t.id).is_err(),
                        "fork's new handle in the third: {what}"
                    );
                }
                third_steps += 1;
            }
        }
        if let Some((c, _, _, _)) = &third {
            if well && !diag_round {
                reparses(c, &settings);
            }
        }
        if let Some((b, _, _, _)) = &side {
            if well && diag_round {
                let again = SyntaxTree::parse(b.emit());
                if let Err(e) = compare_trees(&again, b) {
                    panic!("round {round}: the fork's moved diagnostics differ: {e}");
                }
            } else if well {
                reparses(b, &settings);
            }
        }
        fallbacks += m.fallbacks;
        if well && diag_round {
            // Diagnostics after the edits moved with their text.
            let again = SyntaxTree::parse(tree.emit());
            if let Err(e) = compare_trees(&again, &tree) {
                panic!(
                    "round {round}: moved diagnostics differ: {e}\n{:?}",
                    tree.emit()
                );
            }
            moved += 1;
            kind_eol[1][eol_ix] += 1;
        } else if well {
            reparses(&tree, &settings);
            wellformed += 1;
            kind_eol[0][eol_ix] += 1;
            edited += usize::from(tree.is_edited());
        } else {
            arbitrary += 1;
            kind_eol[2][eol_ix] += 1;
        }
    }
    eprintln!(
        "edit fuzz rounds {from}..{rounds}: {wellformed} well-formed ({edited} edited, re-parse \
         checked), {moved} diagnostics-moved, {arbitrary} arbitrary, {steps} steps; arbitrary \
         edits accepted per primitive (remove, remove_range, replace, insert_before, \
         insert_after, insert_child, append_child, replace_token): {:?}; line-end edits \
         accepted (replace_to_line_end, insert_line_after): {:?}; refused as not \
         well-formed: {}; implications checked: {} clean, {} with errors; places from the tree: \
         {fallbacks}; forked mid-script: {forks} rounds, {fork_steps} steps on the forks; forks of forks: {forks_of_forks} rounds, {third_steps} steps on them; rounds by line break (LF, CRLF, CR): {eol_rounds:?}; per kind (well-formed, diagnostics-moved, arbitrary) × line break (LF, CRLF, CR): {kind_eol:?}",
        tally.accepted,
        tally.line_accepted,
        tally.refused_ill_formed,
        tally.implications,
        tally.with_errors
    );
    // Every primitive is exercised, not only refused.
    let len = rounds.saturating_sub(from);
    if len >= 1000 {
        assert!(
            tally.accepted.iter().all(|a| *a > 20),
            "{:?}",
            tally.accepted
        );
        assert!(tally.line_accepted[0] > 20, "{:?}", tally.line_accepted);
        assert!(
            tally.line_accepted[1] >= per_section(INSERT_LINE_PER_K, len),
            "insert_line_after: {:?}",
            tally.line_accepted
        );
        assert!(edited * 4 > wellformed * 3, "{edited} of {wellformed}");
        assert!(moved > 50, "{moved}");
        assert!(tally.refused_ill_formed > 20);
        assert!(tally.implications > 1000);
        assert!(
            forks > 100 && fork_steps > 200,
            "{forks} forks, {fork_steps} steps"
        );
        assert!(eol_rounds.iter().all(|n| *n >= 50), "{eol_rounds:?}");
        // Every kind of round meets every line break.
        let [wf, mv, arb] = kind_eol;
        assert!(
            wf.iter().all(|n| *n >= per_section(WF_PER_EOL, len)),
            "well-formed: {kind_eol:?}"
        );
        assert!(
            mv.iter().all(|n| *n >= per_section(MOVED_PER_EOL, len)),
            "moved: {kind_eol:?}"
        );
        assert!(
            arb.iter()
                .all(|n| *n >= per_section(ARBITRARY_PER_EOL, len)),
            "arbitrary: {kind_eol:?}"
        );
        assert!(
            forks_of_forks > 50 && third_steps > 50,
            "{forks_of_forks} forks of forks, {third_steps} steps"
        );
    }
}

/// One step of [`the_same_script_costs_the_same_on_every_line_break`]:
/// what it was and whether it was accepted, the same for every line
/// break (a refusal's reason included).
type Outcome = (&'static str, Result<(), TreeEditError>);

/// A script over `tree` (written with `eol`) drawn from `seed`: comment lines
/// inserted before the second document and appended at the end of the stream
/// (the two shapes that once re-read whole documents), plain words replaced in
/// the first document, and line-end edits; the same draws whatever the line
/// break (the trees have the same shape). Returns the outcomes.
fn eol_script(tree: &mut SyntaxTree, seed: u64, eol: &'static str) -> Vec<Outcome> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::new();
    let comment = Fragment::parse(&with_eol("# d\n", eol)).unwrap();
    for _ in 0..24 {
        match rng.below(4) {
            0 => {
                let doc = tree
                    .root()
                    .children()
                    .filter(|x| x.kind() == K::Document)
                    .nth(1)
                    .unwrap()
                    .id();
                let r = tree.insert_before(doc, comment.clone()).map(|_| ());
                out.push(("comment before a document", r));
            }
            1 => {
                let root = tree.root().id();
                let r = tree.append_child(root, comment.clone()).map(|_| ());
                out.push(("comment at the end", r));
            }
            2 => {
                let first = tree.root().first_child().unwrap();
                let words: Vec<NodeId> = first
                    .descendants()
                    .filter(plain_word_token)
                    .map(|n| n.id())
                    .collect();
                let id = words[rng.below(words.len())];
                let w = format!("w{}", rng.below(100));
                let r = tree.replace_token(id, &w).map(|_| ());
                out.push(("word", r));
            }
            _ => {
                let first = tree.root().first_child().unwrap();
                let values: Vec<NodeId> = first
                    .descendants()
                    .filter(|n| n.kind() == K::Scalar && !n.span().is_empty())
                    .map(|n| n.id())
                    .collect();
                let id = values[rng.below(values.len())];
                let snippet = LINE_SNIPPETS[rng.below(LINE_SNIPPETS.len())];
                let f = Fragment::parse(&with_eol(snippet, eol));
                let r = match f {
                    Ok(f) if rng.below(2) == 0 => tree.insert_line_after(id, f).map(|_| ()),
                    Ok(f) => tree.replace_to_line_end(id, id, f).map(|_| ()),
                    Err(e) => Err(e),
                };
                out.push(("line end", r));
            }
        }
    }
    out
}

/// §5.4 (line breaks are CR, LF or CRLF alike on
/// the edit path; validation cost is local): one round type of the fuzz
/// runs the same script over one document written with LF, with CRLF, and
/// with CR. The outcomes are the same, every edited tree equals its
/// re-parse, and the script costs about the same (the fast paths
/// once accepted only `\n`-ended line breaks, so the CR scripts
/// re-read whole documents).
///
/// The document is a large first document (generated mappings, ~40 KB)
/// and a second one ending with a block scalar, so a slow path costs
/// O(document) against a fast path's O(line). Timing as in
/// `edit_retention.rs`: the minimum of several runs, the line breaks
/// alternating.
#[test]
fn the_same_script_costs_the_same_on_every_line_break() {
    let rounds = env("ONEYAML_EDIT_FUZZ_EOL_ROUNDS", 12);
    let mut times = [std::time::Duration::ZERO; 3];
    let mut accepted = 0usize;
    for round in 0..rounds {
        let mut rng = Rng::new(0xe01 ^ round as u64);
        let mut first = String::new();
        let mut i = 0usize;
        while first.len() < 40_000 {
            // Distinct keys per piece (a generated piece is a mapping at
            // column 0; the pieces continue it).
            let piece = generate(&mut rng);
            first.push_str(&piece.replace("k", &format!("p{i}k")));
            i += 1;
        }
        let src = format!("{first}--- b\n---\nlast: |\n  x\n");
        let lf = SyntaxTree::parse(src.as_str());
        assert!(!lf.has_errors(), "{src:?}");
        let mut outcomes: Option<Vec<Outcome>> = None;
        let eols: [&'static str; 3] = ["\n", "\r\n", "\r"];
        let trees: Vec<SyntaxTree> = eols
            .iter()
            .map(|e| SyntaxTree::parse(with_eol(&src, e)))
            .collect();
        let mut best = [std::time::Duration::MAX; 3];
        for rep in 0..3 {
            for (k, &eol) in eols.iter().enumerate() {
                let mut t = trees[k].clone();
                let start = std::time::Instant::now();
                let got = eol_script(&mut t, round as u64, eol);
                best[k] = best[k].min(start.elapsed());
                if rep == 0 {
                    reparses(&t, &Settings::new());
                    match &outcomes {
                        None => {
                            accepted += got.iter().filter(|(_, r)| r.is_ok()).count();
                            outcomes = Some(got);
                        }
                        Some(want) => assert_eq!(&got, want, "round {round}, {eol:?}"),
                    }
                }
            }
        }
        for (t, b) in times.iter_mut().zip(best) {
            *t += b;
        }
    }
    let ratio = |k: usize| times[k].as_secs_f64() / times[0].as_secs_f64().max(1e-9);
    eprintln!(
        "the same scripts ({rounds} rounds, {accepted} edits accepted): LF {:?}, CRLF {:?} \
         ({:.2}×), CR {:?} ({:.2}×)",
        times[0],
        times[1],
        ratio(1),
        times[2],
        ratio(2)
    );
    assert!(accepted > rounds * 10, "{accepted}");
    for (k, name) in [(1, "CRLF"), (2, "CR")] {
        assert!(
            ratio(k) < 2.0,
            "{name} scripts cost {:.2}× the LF ones",
            ratio(k)
        );
    }
}

/// The UTF-8 inputs (at most 20 KB) of the committed `edit` fuzz corpus
/// and its regressions (`crates/fuzz`), sorted.
fn fuzz_edit_inputs() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fuzz");
    let mut out = Vec::new();
    for d in ["corpus/edit", "regressions/edit"] {
        let Ok(rd) = std::fs::read_dir(root.join(d)) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let Ok(text) = String::from_utf8(std::fs::read(&p).unwrap()) else {
                continue;
            };
            if text.len() <= 20_000 {
                out.push(text);
            }
        }
    }
    out
}

/// The trivia texts an edit puts in place of white space `ws` (same
/// length with tabs, one space more or less, a tab added).
fn white_space_variants(ws: &str) -> Vec<String> {
    let l = ws.len();
    let mut out = vec!["\t".repeat(l), " ".repeat(l + 1), format!("{ws}\t")];
    if l > 1 {
        out.push(format!("{}\t", " ".repeat(l - 1)));
        out.push(format!("\t{}", " ".repeat(l - 1)));
        out.push(" ".repeat(l - 1));
    }
    if l > 2 {
        out.push(format!("{}\t ", " ".repeat(l - 2)));
    }
    out
}

/// An edited tree always agrees with its own
/// text): trivia edits where the validation shortcuts decide what is
/// re-read, swept over every input of the suite, the committed `edit`
/// fuzz corpus, and generated documents, under the default and the
/// lenient settings. At every line start (the indentation before an
/// entry, or the line break itself) and around the trivia between a
/// value and its line's end: the white space replaced by tab mixes of
/// the same byte length and by longer or shorter runs, removed, and
/// white space inserted before and after it; a space or tab inserted
/// after each line break. Every accepted edit must equal its re-parse
/// structurally (`implication`: every token, white space included, and
/// the diagnostics). An earlier shortcut past the line after a value
/// synthesised that line's indentation and never re-read it: 619 edits
/// of this sweep that put a tab there were accepted and read back
/// differently (the oracle of its own tests dropped white space).
#[test]
fn indentation_and_line_trivia_edits_read_back() {
    let mut inputs = suite_inputs();
    inputs.extend(fuzz_edit_inputs());
    for seed in 0..200u64 {
        inputs.push(generate(&mut Rng::new(seed ^ 0x48b)));
    }
    let mut tally = Tally::default();
    let (mut accepted, mut refused, mut edits) = (0usize, 0usize, 0usize);
    for src in &inputs {
        for settings in [Settings::new(), Settings::new().lenient_inline_colons(true)] {
            let base = SyntaxTree::parse_with(src.as_str(), &settings);
            let tokens: Vec<Node<'_>> =
                base.root().descendants().filter(|n| n.is_token()).collect();
            for (i, t) in tokens.iter().enumerate() {
                let prev = i.checked_sub(1).map(|p| tokens[p].kind());
                let next = tokens.get(i + 1).map(|n| n.kind());
                let ops: Vec<(&str, String)> = match t.kind() {
                    // Indentation, or trivia ending a line.
                    K::Whitespace
                        if prev == Some(K::LineBreak)
                            || matches!(next, Some(K::LineBreak | K::Comment)) =>
                    {
                        let mut ops: Vec<(&str, String)> = white_space_variants(&t.text())
                            .into_iter()
                            .map(|v| ("replace", v))
                            .collect();
                        ops.push(("remove", String::new()));
                        for v in [" ", "\t"] {
                            ops.push(("before", v.into()));
                            ops.push(("after", v.into()));
                        }
                        ops
                    }
                    K::LineBreak => [" ", "\t", "  ", " \t"]
                        .into_iter()
                        .map(|v| ("after", v.into()))
                        .collect(),
                    _ => continue,
                };
                for (op, v) in ops {
                    edits += 1;
                    let mut tree = base.fork();
                    let id = t.id();
                    let r = match op {
                        "replace" => tree.replace_token(id, &v).map(|_| ()),
                        "remove" => tree.remove(id).map(|_| ()),
                        _ => match Fragment::parse_with(&v, &settings) {
                            Ok(f) if op == "before" => tree.insert_before(id, f).map(|_| ()),
                            Ok(f) => tree.insert_after(id, f).map(|_| ()),
                            Err(e) => Err(e),
                        },
                    };
                    let what = format!("{op} {v:?} at {:?} in {src:?}", t.span());
                    match r {
                        Ok(()) => {
                            accepted += 1;
                            implication(&tree, &mut tally, &what);
                        }
                        Err(e) => {
                            assert!(!matches!(e, TreeEditError::Internal), "{what}");
                            assert_eq!(tree.emit(), base.emit(), "{what}: refused but changed");
                            refused += 1;
                        }
                    }
                }
            }
        }
    }
    eprintln!(
        "indentation and line trivia: {} inputs, {edits} edits, {accepted} accepted \
         ({} clean, {} with errors), {refused} refused",
        inputs.len(),
        tally.implications,
        tally.with_errors
    );
    // Measured: 71,978 edits, 14,640 accepted, 57,338 refused.
    assert!(
        accepted > 7_000 && refused > 25_000,
        "{accepted} / {refused}"
    );
}
