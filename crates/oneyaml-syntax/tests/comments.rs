//! Comment attachment (`docs/comment-attachment.md`): every comment
//! of every yaml-test-suite case (valid and invalid) gets exactly one
//! owner, deterministically, the same from the one-comment and the
//! whole-tree entry points, and the documents of a stream partition the
//! comments; deep nesting on a small stack; edited trees;
//! linear cost.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use std::fs;
use std::path::{Path, PathBuf};

use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::tree::{AttachRule, Attachment, CommentOwner, Node, SyntaxKind, SyntaxTree};

fn in_workspace() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    fs::read_to_string(root).is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
}

/// Every `in.yaml` of the vendored suite (inside the workspace, missing
/// data fails the test).
fn suite_inputs() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    if !root.is_dir() {
        assert!(
            !in_workspace(),
            "yaml-test-suite data missing at {}",
            root.display()
        );
        eprintln!("skipped: yaml-test-suite data not found");
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if p.file_name().is_some_and(|n| n == "in.yaml") {
                let text = fs::read_to_string(&p).unwrap();
                out.push((p, text));
            }
        }
    }
    out.sort();
    out
}

fn doc_of(n: Node<'_>) -> Option<Node<'_>> {
    n.ancestors().find(|a| a.kind() == SyntaxKind::Document)
}

/// The parser's recovery placeholder as rulebook section 5 defines it: an
/// empty scalar right after an error region that has no bytes **and** is
/// the last child of its collection. (an empty scalar after a
/// skipped byte order mark that is followed by more of its collection,
/// e.g. the empty key of `\u{feff}: # c` inside a mapping, is a real node.)
fn is_placeholder(n: Node<'_>) -> bool {
    n.kind() == SyntaxKind::Scalar
        && n.span().is_empty()
        && n.prev_sibling()
            .is_some_and(|p| p.kind() == SyntaxKind::Error)
        && n.next_sibling().is_none()
}

fn is_collection(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::BlockMapping
            | SyntaxKind::BlockSequence
            | SyntaxKind::FlowMapping
            | SyntaxKind::FlowSequence
            | SyntaxKind::FlowPair
    )
}

/// The owner is the kind of element its variant says, and the document
/// the attachment names is the owner's.
fn check_owner(what: &str, c: Node<'_>, a: &Attachment<'_>) {
    let owner_doc = match a.owner() {
        CommentOwner::DocumentHeader(d) | CommentOwner::DocumentFooter(d) => match d.kind() {
            SyntaxKind::Document => Some(d),
            SyntaxKind::Stream => {
                assert!(
                    matches!(a.owner(), CommentOwner::DocumentHeader(_)),
                    "{what}: stream footer for {c:?}"
                );
                None
            }
            k => panic!("{what}: header/footer owner {k:?} for {c:?}"),
        },
        CommentOwner::Leading(n) | CommentOwner::Trailing(n) => {
            assert!(
                n.kind().is_yaml_node() || n.kind() == SyntaxKind::Error,
                "{what}: node owner {n:?} for {c:?}"
            );
            // Rulebook section 5: never the empty placeholder
            // right after an error region (navigation presents the region).
            assert!(!is_placeholder(n), "{what}: placeholder owner for {c:?}");
            doc_of(n)
        }
        CommentOwner::Footer(n) => {
            assert!(
                is_collection(n.kind()),
                "{what}: footer owner {n:?} for {c:?}"
            );
            doc_of(n)
        }
        o => panic!("{what}: unknown owner {o:?}"),
    };
    assert_eq!(a.document(), owner_doc, "{what}: document of {c:?}");
    // Rule 5 exactly for same-line comments; blank lines only above
    // own-line ones.
    if a.rule() == AttachRule::Trailing {
        assert_eq!(a.blank_lines_before(), 0, "{what}: {c:?}");
    }
}

/// Rule 13: within one run of comment lines (no content
/// between them), each owner's own-line comments are contiguous: once the
/// run leaves an owner, no later comment of the run returns to it.
fn check_runs(what: &str, tree: &SyntaxTree, all: &[(Node<'_>, Attachment<'_>)]) {
    // The run of each comment: the last content token before it.
    let mut last_visible = None;
    let mut run_of = std::collections::HashMap::new();
    for t in tree.root().descendants().filter(|n| n.is_token()) {
        if t.kind() == SyntaxKind::Comment {
            run_of.insert(t, last_visible);
        } else if !t.kind().is_trivia() && !t.span().is_empty() {
            last_visible = Some(t.preorder_index());
        }
    }
    let mut current_run = None;
    let mut left: Vec<CommentOwner<'_>> = Vec::new();
    let mut previous: Option<CommentOwner<'_>> = None;
    for (c, a) in all {
        if a.rule() == AttachRule::Trailing {
            continue;
        }
        let run = run_of[c];
        if Some(run) != current_run {
            current_run = Some(run);
            left.clear();
            previous = None;
        }
        let owner = a.owner();
        if let Some(p) = previous.filter(|p| *p != owner) {
            left.push(p);
        }
        assert!(
            !left.contains(&owner),
            "{what}: {:?} returns to an owner its run already left ({owner:?})",
            c.text()
        );
        previous = Some(owner);
    }
}

/// Every invariant of the attachment API on one tree.
fn check(what: &str, tree: &SyntaxTree) -> usize {
    let comments: Vec<Node<'_>> = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .collect();
    let all = tree.comment_attachments();
    assert_eq!(
        all.len(),
        comments.len(),
        "{what}: one attachment per comment"
    );
    for ((c, a), want) in all.iter().zip(&comments) {
        assert_eq!(c, want, "{what}: source order");
        assert_eq!(
            tree.comment_attachment(*c).as_ref(),
            Some(a),
            "{what}: the one-comment and whole-tree answers differ for {c:?} {:?}",
            c.text()
        );
        check_owner(what, *c, a);
        // The run of every comment, decided at once, agrees
        // with the pass (the comment API reads and checks by run).
        let run = tree.comment_run_attachments(*c);
        assert!(
            run.iter().any(|(r, ra)| r == c && ra == a),
            "{what}: the run of {c:?} {:?} disagrees: {run:?}",
            c.text()
        );
        for (r, ra) in &run {
            let pos = all.iter().position(|(x, _)| x == r);
            assert_eq!(
                pos.map(|i| &all[i].1),
                Some(ra),
                "{what}: run member {r:?} {:?}",
                r.text()
            );
        }
    }
    // In a tree without errors, a comment inside a document's
    // node is that document's (the comment API's O(depth) membership
    // check rests on it).
    if !tree.has_errors() {
        for (c, a) in &all {
            if let Some(d) = doc_of(*c) {
                assert_eq!(
                    a.document(),
                    Some(d),
                    "{what}: {c:?} {:?} inside a clean document",
                    c.text()
                );
            }
        }
    }
    // Not a comment, or not this tree's: no run.
    assert!(tree.comment_run_attachments(tree.root()).is_empty());
    // Deterministic.
    assert_eq!(tree.comment_attachments(), all, "{what}: deterministic");
    check_runs(what, tree, &all);
    // The documents partition the comments (a comment without a document
    // only in an input without documents).
    let docs: Vec<Node<'_>> = tree
        .root()
        .children()
        .filter(|n| n.kind() == SyntaxKind::Document)
        .collect();
    let mut owned = 0;
    for d in &docs {
        let mine = tree.document_comment_attachments(*d);
        let want: Vec<_> = all
            .iter()
            .filter(|(_, a)| a.document() == Some(*d))
            .copied()
            .collect();
        assert_eq!(mine, want, "{what}: comments of {d:?}");
        owned += mine.len();
    }
    let orphans = all.iter().filter(|(_, a)| a.document().is_none()).count();
    assert!(
        orphans == 0 || docs.is_empty(),
        "{what}: comments without a document"
    );
    assert_eq!(owned + orphans, all.len(), "{what}: partition");
    // Every stretch between documents, decided at once, agrees
    // with the pass.
    let mut prev = None;
    let mut bounds: Vec<(Option<Node<'_>>, Option<Node<'_>>)> = Vec::new();
    for d in docs.iter().copied().map(Some).chain(core::iter::once(None)) {
        bounds.push((prev, d));
        prev = d;
    }
    for (b, e) in bounds {
        for (c, a) in tree.comment_attachments_between(b, e) {
            let pos = all.iter().position(|(x, _)| *x == c);
            assert_eq!(
                pos.map(|i| &all[i].1),
                Some(&a),
                "{what}: between {b:?} {e:?}"
            );
            assert!(doc_of(c).is_none(), "{what}: {c:?} is inside a document");
        }
    }
    if let (Some(f), Some(l)) = (docs.first(), docs.last()) {
        if f != l {
            // Reversed bounds: nothing.
            assert!(
                tree.comment_attachments_between(Some(*l), Some(*f))
                    .is_empty()
            );
        }
    }
    assert!(
        tree.comment_attachments_between(Some(tree.root()), None)
            .is_empty()
    );
    // Not a comment, or not this tree's: no attachment.
    assert!(tree.comment_attachment(tree.root()).is_none());
    assert!(tree.document_comment_attachments(tree.root()).is_empty());
    all.len()
}

#[test]
fn every_suite_comment_has_exactly_one_owner() {
    let inputs = suite_inputs();
    if inputs.is_empty() {
        return;
    }
    let mut comments = 0;
    let mut rules = std::collections::BTreeSet::new();
    for (path, text) in &inputs {
        let tree = SyntaxTree::parse(text.as_str());
        assert_eq!(tree.emit().as_bytes(), text.as_bytes(), "byte-identical");
        comments += check(&path.display().to_string(), &tree);
        for (_, a) in tree.comment_attachments() {
            rules.insert(a.rule().number());
        }
    }
    assert!(inputs.len() > 300, "{} cases", inputs.len());
    assert!(comments > 100, "{comments} comments");
    // The suite exercises the core rules.
    for r in [1, 2, 3, 4, 5, 6, 7, 9] {
        assert!(
            rules.contains(&r),
            "rule {r} never fired over the suite: {rules:?}"
        );
    }
}

#[test]
fn another_trees_comment_has_no_attachment() {
    let a = SyntaxTree::parse("a: 1 # c\n");
    let b = SyntaxTree::parse("a: 1 # c\n");
    let c = a
        .root()
        .descendants()
        .find(|n| n.kind() == SyntaxKind::Comment)
        .unwrap();
    assert!(a.comment_attachment(c).is_some());
    assert!(b.comment_attachment(c).is_none());
    let d = a.root().first_child().unwrap();
    assert!(b.document_comment_attachments(d).is_empty());
}

#[test]
fn stream_documents_share_out_their_comments() {
    let src = "# h1\na: 1\n  # f1\n# before two\n---\n# h2\n\nb: [1, # t\n  2]\n...\n# after end\n--- \n# empty\n";
    let tree = SyntaxTree::parse(src);
    assert!(!tree.has_errors());
    check("stream", &tree);
    let docs: Vec<_> = tree
        .root()
        .children()
        .filter(|n| n.kind() == SyntaxKind::Document)
        .collect();
    let texts = |d| {
        tree.document_comment_attachments(d)
            .iter()
            .map(|(c, a)| format!("{} r{}", c.text(), a.rule().number()))
            .collect::<Vec<_>>()
    };
    // `  # f1` is deeper than the root but before a `---`: rule 1 (a
    // comment before `---` is always the next document's header).
    assert_eq!(texts(docs[0]), ["# h1 r3"]);
    assert_eq!(
        texts(docs[1]),
        ["# f1 r1", "# before two r1", "# h2 r2", "# t r5"]
    );
    assert_eq!(texts(docs[2]), ["# after end r1", "# empty r2"]);
}

/// Nothing recurses on depth; on a 128 KiB stack.
fn on_small_stack(src: String) {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let settings = Settings::new().limits(Limits::unlimited());
            let tree = SyntaxTree::parse_with(src.as_str(), &settings);
            let all = tree.comment_attachments();
            assert!(!all.is_empty());
            // The one-comment entry point on the deepest comments.
            for (c, a) in all.iter().take(3).chain(all.iter().rev().take(3)) {
                assert_eq!(tree.comment_attachment(*c).as_ref(), Some(a));
            }
            for d in tree.root().children() {
                let _ = tree.document_comment_attachments(d);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_nesting_on_a_small_stack() {
    const DEPTH: usize = 1_000_000;
    // Flow: a trailing comment at the deepest point, own-line comments on
    // the way out.
    let mut flow = "[".repeat(DEPTH);
    flow.push_str(" # deepest\n");
    flow.push_str(&"]\n# out\n".repeat(3));
    flow.push_str(&"]".repeat(DEPTH - 3));
    flow.push_str(" # end\n# footer\n");
    on_small_stack(flow);
    // Block: `- - - … x # c` then comments closing every level.
    let mut block = "- ".repeat(DEPTH);
    block.push_str("x # deepest\n# after\n  # deeper\n");
    on_small_stack(block);
}

#[test]
fn edited_trees_are_read_the_same_way() {
    let src = "# head\n\na: 1 # one\n# lead b\nb:\n  - x\n  # foot\nc: |\n  text\n# lead d\nd: 4\n";
    let mut tree = SyntaxTree::parse(src);
    let before: Vec<_> = tree
        .comment_attachments()
        .iter()
        .map(|(c, a)| (c.id(), a.rule()))
        .collect();
    // Rewrite every plain scalar and one comment (a comment for a comment
    // is always well formed).
    let scalars: Vec<_> = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::PlainScalar && !n.span().is_empty())
        .map(|n| (n.id(), format!("{}{}", n.text(), n.text())))
        .collect();
    for (id, text) in scalars {
        tree.replace_token(id, &text).unwrap();
    }
    let foot = tree
        .root()
        .descendants()
        .find(|n| n.text() == "# foot")
        .unwrap()
        .id();
    let new_foot = tree.replace_token(foot, "#foot, rewritten").unwrap();
    assert!(tree.is_edited());
    check("edited", &tree);
    let after: Vec<_> = tree
        .comment_attachments()
        .iter()
        .map(|(c, a)| (c.id(), a.rule()))
        .collect();
    // The same rules; every comment keeps its handle but the replaced one.
    assert_eq!(
        before.iter().map(|x| x.1).collect::<Vec<_>>(),
        after.iter().map(|x| x.1).collect::<Vec<_>>()
    );
    for (b, a) in before.iter().zip(&after) {
        if b.0 == foot {
            assert_eq!(a.0, new_foot);
            assert!(tree.get(foot).is_err(), "the replaced comment is stale");
        } else {
            assert_eq!(a.0, b.0);
        }
    }
    // The edited tree reads as its re-parse.
    let reparsed = SyntaxTree::parse(tree.emit());
    let rules = |t: &SyntaxTree| {
        t.comment_attachments()
            .iter()
            .map(|(c, a)| (c.text().into_owned(), a.rule(), a.blank_lines_before()))
            .collect::<Vec<_>>()
    };
    assert_eq!(rules(&tree), rules(&reparsed));
}

/// One pass is O(n): long comment runs after deep block nesting (a
/// run's context is computed once), same-line comments on directive,
/// `---` and `...` lines (decided from the element's kind, never by
/// scanning the document's children), and comments around stray `...`
/// markers between documents (their neighbouring documents are tabled once
/// per pass, never found by walking the stream per comment).
#[test]
fn whole_document_pass_is_linear() {
    fn deep(n: usize) -> String {
        let mut s = String::new();
        let depth = 40;
        for i in 0..n / 100 {
            for d in 0..depth {
                s.push_str(&" ".repeat(d * 2));
                s.push_str(&format!("k{i}_{d}:\n"));
            }
            s.push_str(&" ".repeat(depth * 2));
            s.push_str("v # t\n");
            for j in 0..depth {
                s.push_str(&" ".repeat((depth - j) * 2));
                s.push_str("# c\n\n");
            }
        }
        s
    }
    fn tags(n: usize) -> String {
        let mut s: String = (0..n)
            .map(|i| format!("%TAG !t{i}! tag:x{i}: # c\n"))
            .collect();
        s.push_str("--- a\n");
        s
    }
    type Make = fn(usize) -> String;
    let shapes: [(&str, Make); 7] = [
        ("deep block", deep),
        ("%TAG lines", tags),
        ("reserved directives", |n| "%FOO bar # c\n".repeat(n)),
        ("stray ... lines", |n| "... # c\n".repeat(n)),
        ("document then stray ... lines", |n| {
            format!("a\n{}", "... # c\n".repeat(n))
        }),
        ("document then own-line comments after stray ...", |n| {
            format!("a\n{}", "...\n# c\n".repeat(n))
        }),
        ("own-line comments after stray ...", |n| {
            "...\n# c\n".repeat(n)
        }),
    ];
    fn best(src: &str) -> f64 {
        let tree = SyntaxTree::parse(src);
        (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let n = tree.comment_attachments().len();
                assert!(n > 0);
                t.elapsed().as_secs_f64()
            })
            .fold(f64::INFINITY, f64::min)
    }
    for (name, make) in shapes {
        let (small, large) = (make(2_000), make(20_000));
        let (a, b) = (best(&small), best(&large));
        let ratio = b / a.max(1e-9);
        // 10x the input: linear is ~10, quadratic ~100.
        assert!(
            ratio < 35.0,
            "{name}: 10x input took {ratio:.1}x ({a:.6}s -> {b:.6}s)"
        );
        // The per-document pass of a stream costs the same.
        let tree = SyntaxTree::parse(large.as_str());
        let docs: Vec<_> = tree
            .root()
            .children()
            .filter(|n| n.kind() == SyntaxKind::Document)
            .collect();
        let t = std::time::Instant::now();
        for d in docs {
            let _ = tree.document_comment_attachments(d);
        }
        let per_doc = t.elapsed().as_secs_f64();
        assert!(
            per_doc < b * 35.0 + 0.05,
            "{name}: per-document passes took {per_doc:.6}s"
        );
    }
}

/// Runs cross no nesting (rule 13) — the known shapes.
#[test]
fn owners_only_move_outward_within_a_run() {
    let cases: [(&str, &[&str]); 3] = [
        // An inner footer after its parent's footer joins the parent's.
        (
            "a:\n  b:\n    c: 1\n  # c2\n    # c4\n",
            &["# c2 r6", "# c4 r13"],
        ),
        // A footer after the next node's leading comment joins it.
        (
            "a:\n  - 1\n# lead b\n  # - 2\nb: 1\n",
            &["# lead b r4", "# - 2 r13"],
        ),
        // The same at the document's end, after a blank line.
        (
            "a:\n  - 1\n# lead b\n\n  # - 2\n",
            &["# lead b r7", "# - 2 r13"],
        ),
    ];
    for (src, want) in cases {
        let tree = SyntaxTree::parse(src);
        check(src, &tree);
        let got: Vec<String> = tree
            .comment_attachments()
            .iter()
            .map(|(c, a)| format!("{} r{}", c.text(), a.rule().number()))
            .collect();
        assert_eq!(got, want, "{src:?}");
        // A rule-13 comment has the owner of the comment before it.
        let all = tree.comment_attachments();
        for w in all.windows(2) {
            if w[1].1.rule() == AttachRule::RunOrder {
                assert_eq!(w[1].1.owner(), w[0].1.owner(), "{src:?}");
                assert_eq!(w[1].1.document(), w[0].1.document(), "{src:?}");
            }
        }
    }
}

/// A content token inside a stream-level
/// error region; the one-comment path finds the documents around the
/// region, as the pass does.
#[test]
fn stream_level_error_regions_agree() {
    for src in [
        "#\n&\n---",
        "#\n&\n---\na: 1\n",
        "# c\n,\n---\na: 1 # t\n",
        "a\n...\n# c\n]\n# d\n---\nb\n",
        "a\n...\n}\n# c\n",
        "# c\n{\n",
    ] {
        let tree = SyntaxTree::parse(src);
        check(src, &tree);
    }
}

/// A small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Pieces YAML is made of, comments and line shapes weighted up.
const PIECES: &[&str] = &[
    "a",
    "b",
    "k: ",
    "- ",
    "? ",
    ": ",
    ":",
    "[",
    "]",
    "{",
    "}",
    ", ",
    ",",
    "# c",
    "# d",
    "#",
    "\n",
    "\n",
    "\n",
    "\n  ",
    "\n    ",
    "\n# c\n",
    "\n  # c\n",
    "\n\n",
    "  ",
    "---",
    "--- ",
    "...",
    "&x ",
    "&",
    "!t ",
    "*x",
    "|",
    "|+",
    ">-",
    "'q'",
    "\"q\"",
    "%YAML 1.2",
    "%TAG ! t:",
    "%FOO",
    "\t",
    "\r\n",
    "x: |\n  t\n",
    "- - ",
    "<<: *x",
    "\u{feff}",
];

/// Inputs for the differential fuzz: random pieces, and suite inputs with
/// comment lines inserted at random places and columns.
fn fuzz_inputs(n: usize) -> Vec<String> {
    let suite = suite_inputs();
    // `ONEYAML_FUZZ_SEED` picks another deterministic sequence.
    let seed = std::env::var("ONEYAML_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15_u64);
    let mut rng = Rng(seed | 1);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if i % 2 == 0 || suite.is_empty() {
            let len = 1 + rng.below(24);
            out.push((0..len).map(|_| PIECES[rng.below(PIECES.len())]).collect());
        } else {
            let base = &suite[rng.below(suite.len())].1;
            let mut lines: Vec<String> = base.lines().map(str::to_owned).collect();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(lines.len() + 1);
                let col = [0, 1, 2, 4, 6][rng.below(5)];
                let blank = if rng.below(3) == 0 { "\n" } else { "" };
                lines.insert(at, format!("{blank}{}# f", " ".repeat(col)));
            }
            out.push(lines.join("\n") + "\n");
        }
    }
    out
}

/// The one-comment path and the passes agree on every comment
/// (and every other invariant of `check` holds, rule 13 included) over a
/// deterministic fuzz of generated and comment-injected suite inputs.
#[test]
fn single_path_agrees_with_the_pass_fuzz() {
    // `ONEYAML_FUZZ_CASES` raises the count for a longer local sweep.
    let n = std::env::var("ONEYAML_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12_000);
    let inputs = fuzz_inputs(n);
    let mut comments = 0;
    for src in &inputs {
        let tree = SyntaxTree::parse(src.as_str());
        comments += check(&format!("{src:?}"), &tree);
    }
    assert!(comments > n, "{comments} comments");
}

/// The section 5 check still recognises the parser's real
/// recovery placeholder (so narrowing it did not blind it), and does not
/// mistake the empty key after a skipped byte order mark for one.
#[test]
fn placeholder_check_follows_section_5() {
    let placeholders = |src: &str| {
        let tree = SyntaxTree::parse(src);
        tree.root()
            .descendants()
            .filter(|n| is_placeholder(*n))
            .count()
    };
    for src in [
        "k: v\nz: @x # c\n",
        "- a\n- b: @x\n  # c\n",
        "a:\n  b: @x\n  # c\n",
    ] {
        assert_eq!(placeholders(src), 1, "{src:?}");
    }
    assert_eq!(placeholders("<<: *x---'q'\n\n\u{feff}: # c"), 0);
    assert_eq!(placeholders("k: v\n\u{feff}: # c"), 0);
}
