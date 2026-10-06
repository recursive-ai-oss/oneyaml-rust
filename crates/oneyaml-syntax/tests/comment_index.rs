//! The comment-run index (`tree/runs.rs`) decides exactly as
//! the run walk it stands in for. Over inputs with long runs of comment
//! lines (amplified yaml-test-suite and fuzz-corpus inputs, generated
//! shapes with stray `...` markers, nested footers, block scalars, byte
//! order marks, recovered input), and over trees edited inside those runs
//! (comment lines inserted, removed, rewritten; groups rolled back; clones
//! and forks), every comment's [`SyntaxTree::comment_attachment`] and
//! [`SyntaxTree::comment_document`] equal the whole-tree pass, and every
//! block collection's [`SyntaxTree::last_footer_comment`] equals the one
//! read from the pass. Every accepted edit leaves a tree equal (the whole
//! tree, trivia included) to a fresh parse of its text.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use std::fs;
use std::path::Path;

use oneyaml_syntax::tree::{AttachRule, Attachment, CommentOwner, Node, SyntaxKind, SyntaxTree};

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
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Every file under `dir` (recursively), sorted; empty when it is missing.
fn files(dir: &Path, name: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(d) = pending.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries {
            let p = e.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            } else if name.is_none_or(|n| p.file_name().is_some_and(|f| f == n)) {
                if let Ok(t) = fs::read(&p) {
                    out.push((p, String::from_utf8_lossy(&t).into_owned()));
                }
            }
        }
    }
    out.sort();
    out.into_iter().map(|(_, t)| t).collect()
}

fn crates() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

fn suite() -> Vec<String> {
    let v = files(
        &crates().join("test-suite/data/yaml-test-suite"),
        Some("in.yaml"),
    );
    assert!(v.len() > 300, "yaml-test-suite data missing");
    v
}

fn corpus() -> Vec<String> {
    let v = files(&crates().join("fuzz/corpus"), None);
    assert!(v.len() > 1000, "fuzz corpora missing");
    v
}

/// `src` with its comment lines repeated (`k` times each, at random
/// columns around their own, blank lines and stray `...` lines among them
/// when `strays`), so that its runs are longer than the index's
/// threshold.
fn amplify(src: &str, rng: &mut Rng, k: usize, strays: bool) -> String {
    let mut out = String::new();
    for line in src.split_inclusive('\n') {
        out.push_str(line);
        let t = line.trim_start_matches([' ', '\t']);
        if !t.starts_with('#') {
            continue;
        }
        let col = line.len() - t.len();
        let body = t.trim_end_matches(['\n', '\r']);
        let nl = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
        if !line.ends_with('\n') {
            out.push_str(nl);
        }
        for i in 0..k {
            match rng.below(16) {
                0 => out.push_str(nl),
                1 if strays => {
                    out.push_str("...");
                    out.push_str(nl);
                }
                _ => {
                    let c = (col + rng.below(5)).saturating_sub(2);
                    out.push_str(&" ".repeat(c));
                    out.push_str(body);
                    out.push_str(&format!(" {i}"));
                    out.push_str(nl);
                }
            }
        }
    }
    out
}

/// Generated shapes: nested block collections closing into one long run
/// of comment lines at every column, stray `...` stretches, block scalars,
/// several documents.
fn shapes(rng: &mut Rng, n: usize) -> Vec<String> {
    let mut out = Vec::new();
    for _ in 0..n {
        let mut s = String::new();
        if rng.below(4) == 0 {
            s.push('\u{feff}');
        }
        let docs = 1 + rng.below(3);
        for d in 0..docs {
            if d > 0 || rng.below(3) == 0 {
                s.push_str(["---\n", "--- # m\n", "...\n---\n", ""][rng.below(4)]);
            }
            let depth = 1 + rng.below(4);
            for i in 0..depth {
                let pad = " ".repeat(2 * i);
                match rng.below(5) {
                    0 => s.push_str(&format!("{pad}- s{i}\n{pad}-\n")),
                    1 if i + 1 == depth => s.push_str(&format!("{pad}t: |\n{pad}  x\n")),
                    2 => s.push_str(&format!("{pad}- k{i}: v\n{pad}  m:\n")),
                    _ => s.push_str(&format!("{pad}k{i}: v\n{pad}m{i}:\n")),
                }
            }
            s.push_str(&format!("{}leaf: 1\n", " ".repeat(2 * depth)));
            let lines = 20 + rng.below(60);
            for i in 0..lines {
                match rng.below(12) {
                    0 => s.push('\n'),
                    1 => s.push_str("...\n"),
                    2 => s.push_str("  \n"),
                    _ => {
                        let col = rng.below(2 * depth + 3);
                        s.push_str(&format!("{}# c{i}\n", " ".repeat(col)));
                    }
                }
            }
            match rng.below(4) {
                0 => s.push_str("tail: 2\n"),
                1 => s.push_str("[x, # f\n  y]\n"),
                _ => {}
            }
        }
        out.push(s);
    }
    out
}

fn is_block(k: SyntaxKind) -> bool {
    matches!(k, SyntaxKind::BlockMapping | SyntaxKind::BlockSequence)
}

fn visible(t: Node<'_>) -> bool {
    t.is_token() && !t.kind().is_trivia() && !t.span().is_empty()
}

/// [`SyntaxTree::last_footer_comment`] read from the pass: the comments
/// after the collection's last content token, up to the next one, while
/// their owner is a footer within it.
fn footer_from_pass<'t>(
    x: Node<'t>,
    tokens: &[Node<'t>],
    all: &std::collections::HashMap<Node<'t>, Attachment<'t>>,
) -> Option<Node<'t>> {
    let span = x.span();
    let within = |y: Node<'_>| {
        let s = y.span();
        span.start <= s.start && s.end <= span.end
    };
    let end = x.preorder_index() + x.descendants().len();
    let p = tokens
        .iter()
        .rposition(|t| visible(*t) && t.preorder_index() < end)?;
    if tokens[p].preorder_index() < x.preorder_index() {
        return None;
    }
    let mut last = None;
    for t in &tokens[p + 1..] {
        if visible(*t) {
            break;
        }
        if t.kind() != SyntaxKind::Comment {
            continue;
        }
        let a = all[t];
        if a.rule() == AttachRule::Trailing {
            continue;
        }
        match a.owner() {
            CommentOwner::Footer(y) if within(y) => last = Some(*t),
            _ => break,
        }
    }
    last
}

/// Every index answer on `tree` equals the pass. Returns the comments
/// checked.
fn check(what: &str, tree: &SyntaxTree) -> usize {
    let all = tree.comment_attachments();
    for (c, a) in &all {
        assert_eq!(
            tree.comment_attachment(*c).as_ref(),
            Some(a),
            "{what}: comment_attachment of {c:?} {:?}",
            c.text()
        );
        assert_eq!(
            tree.comment_document(*c),
            Some(a.document()),
            "{what}: comment_document of {c:?} {:?}",
            c.text()
        );
    }
    assert_eq!(tree.comment_document(tree.root()), None);
    let map: std::collections::HashMap<_, _> = all.iter().copied().collect();
    let tokens: Vec<Node<'_>> = tree.root().descendants().filter(|n| n.is_token()).collect();
    for x in tree.root().descendants() {
        if is_block(x.kind()) {
            assert_eq!(
                tree.last_footer_comment(x),
                footer_from_pass(x, &tokens, &map),
                "{what}: last footer comment of {x:?}"
            );
        } else {
            assert_eq!(tree.last_footer_comment(x), None);
        }
    }
    all.len()
}

/// The fragment of `text`'s stream children.
fn fragment(text: &str) -> oneyaml_syntax::tree::Fragment {
    let t = SyntaxTree::parse(text);
    let kids: Vec<_> = t.root().children().collect();
    t.extract(kids[0].id(), kids[kids.len() - 1].id()).unwrap()
}

/// The white space before comment `c` on its line, else `c`.
fn line_start(c: Node<'_>) -> Node<'_> {
    c.prev_sibling()
        .filter(|w| w.kind() == SyntaxKind::Whitespace)
        .unwrap_or(c)
}

/// The edits of [`edit_and_check`]'s `long` mode:
/// a fragment of 22 to 45 comment lines (a long run of its
/// own arena, so the fragment index path, its segments and notes are
/// read), cuts inside such runs (a comment's indentation rewritten, to
/// the same or another width: the next piece starts at a comment whose
/// line began in the piece before; the note's line lies before its piece,
/// and its column may be stale), removals of several whole
/// lines across pieces, and every comment of a run rewritten (the shape
/// that once missed this). `None` when the shape is not there.
fn long_edit(tree: &mut SyntaxTree, rng: &mut Rng, step: usize) -> Option<bool> {
    let breaks: Vec<_> = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::LineBreak)
        .map(Node::id)
        .collect();
    let comments: Vec<_> = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .map(Node::id)
        .collect();
    if breaks.is_empty() || comments.is_empty() {
        return None;
    }
    Some(match rng.below(4) {
        0 => {
            let mut text = String::new();
            for i in 0..22 + rng.below(24) {
                match rng.below(9) {
                    0 => text.push('\n'),
                    _ => {
                        text.push_str(&" ".repeat(rng.below(9)));
                        text.push_str(&format!(
                            "# l{step}.{i}
"
                        ));
                    }
                }
            }
            let at = breaks[rng.below(breaks.len())];
            tree.insert_after(at, fragment(&text)).is_ok()
        }
        1 => {
            // Mostly a comment of an inserted long fragment.
            let inserted: Vec<_> = comments
                .iter()
                .copied()
                .filter(|c| tree.get(*c).unwrap().text().starts_with("# l"))
                .collect();
            let pool = if inserted.is_empty() || rng.below(4) == 0 {
                &comments
            } else {
                &inserted
            };
            let c = tree.get(pool[rng.below(pool.len())]).unwrap();
            let w = line_start(c);
            if w == c {
                return None;
            }
            // The same width or another: with another, the arena's column
            // of the comment is stale and the tree's is read.
            let (w, len) = (w.id(), w.text().len());
            let width = if rng.below(2) == 0 {
                len
            } else {
                1 + rng.below(8)
            };
            tree.replace_token(w, &" ".repeat(width)).is_ok()
        }
        2 => {
            let c = tree.get(comments[rng.below(comments.len())]).unwrap();
            let first = line_start(c).id();
            let want = 2 + rng.below(6);
            let (mut last, mut lines) = (c, 0);
            while lines < want {
                let Some(x) = last.next_sibling() else { break };
                last = x;
                if x.kind() == SyntaxKind::LineBreak {
                    lines += 1;
                }
            }
            if lines < want {
                return None;
            }
            let last = last.id();
            tree.remove_range(first, last).is_ok()
        }
        _ => {
            let c = tree.get(comments[rng.below(comments.len())]).unwrap();
            let run: Vec<_> = tree
                .comment_run_attachments(c)
                .into_iter()
                .map(|(c, _)| (c.id(), c.text().into_owned()))
                .collect();
            let mut ok = false;
            for (c, text) in run {
                ok |= tree.replace_token(c, &text).is_ok();
            }
            ok
        }
    })
}

/// Random edits inside the runs of `tree`: comment lines inserted after a
/// line break (at a random column), removed, rewritten; some in a group
/// that is rolled back; with `long`, also [`long_edit`]'s. Every state is
/// checked.
fn edit_and_check(
    what: &str,
    src: &str,
    rng: &mut Rng,
    steps: usize,
    long: bool,
) -> (usize, usize) {
    // Recovered input (it parses with errors) is compared whole
    // too: an edit in or next to a document with errors is refused, so the
    // diagnostics' secondary labels and fixes never move with an element
    // after the edit (comparing such trees without them once missed
    // `[ a\n# c\n...\n` with the comment rewritten to `# r10`).
    let mut tree = SyntaxTree::parse(src);
    let (mut accepted, mut checked) = (0, 0);
    for step in 0..steps {
        let breaks: Vec<_> = tree
            .root()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::LineBreak)
            .map(Node::id)
            .collect();
        let comments: Vec<_> = tree
            .root()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::Comment)
            .map(Node::id)
            .collect();
        let group = rng.below(5) == 0;
        let mark = group.then(|| tree.begin_group());
        let op = if long { rng.below(7) } else { rng.below(3) };
        let ok = match op {
            3.. => long_edit(&mut tree, rng, step).unwrap_or(false),
            0 if !breaks.is_empty() => {
                let at = breaks[rng.below(breaks.len())];
                let col = rng.below(9);
                let text = format!("{}# e{step}\n", " ".repeat(col));
                let frag_tree = SyntaxTree::parse(text.as_str());
                let kids: Vec<_> = frag_tree.root().children().collect();
                let frag = frag_tree
                    .extract(kids[0].id(), kids[kids.len() - 1].id())
                    .unwrap();
                tree.insert_after(at, frag).is_ok()
            }
            1 if !comments.is_empty() => {
                let c = comments[rng.below(comments.len())];
                tree.replace_token(c, &format!("# r{step}")).is_ok()
            }
            _ if !comments.is_empty() => {
                // A comment line: its white space, the comment, its break.
                let c = tree.get(comments[rng.below(comments.len())]).unwrap();
                let first = c
                    .prev_sibling()
                    .filter(|w| w.kind() == SyntaxKind::Whitespace)
                    .unwrap_or(c);
                match c.next_sibling() {
                    Some(b) if b.kind() == SyntaxKind::LineBreak => {
                        let (f, b) = (first.id(), b.id());
                        tree.remove_range(f, b).is_ok()
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        if let Some(m) = mark {
            if rng.below(2) == 0 {
                tree.rollback_group(m).unwrap();
            } else {
                tree.end_group(m).unwrap();
            }
        }
        if ok {
            accepted += 1;
        }
        // The whole tree equals a fresh parse of its text (trivia
        // included), and every index answer equals the pass.
        let text = tree.emit();
        let fresh = SyntaxTree::parse(text.as_str());
        assert!(
            tree == fresh,
            "{what}: step {step}: the edited tree differs from a fresh parse of {text:?}"
        );
        checked += check(&format!("{what} step {step}"), &tree);
        if step % 7 == 3 {
            let copy = tree.fork();
            check(&format!("{what} fork {step}"), &copy);
            let copy = tree.clone();
            check(&format!("{what} clone {step}"), &copy);
        }
    }
    (accepted, checked)
}

#[test]
fn index_agrees_with_the_pass_on_amplified_suite_inputs() {
    let mut rng = Rng(0x48C0_0001);
    let mut comments = 0;
    for (i, src) in suite().iter().enumerate() {
        if !src.contains('#') {
            continue;
        }
        let k = 24 + rng.below(40);
        let s = amplify(src, &mut rng, k, i % 2 == 0);
        comments += check(&format!("suite {i}: {s:?}"), &SyntaxTree::parse(s.as_str()));
        // Unamplified too (short runs read element by element).
        check(
            &format!("suite {i} plain"),
            &SyntaxTree::parse(src.as_str()),
        );
    }
    assert!(comments > 2_000, "{comments}");
}

#[test]
fn index_agrees_with_the_pass_on_amplified_fuzz_corpora() {
    let mut rng = Rng(0x48C0_0002);
    let mut comments = 0;
    for (i, src) in corpus().iter().enumerate() {
        check(
            &format!("corpus {i} plain"),
            &SyntaxTree::parse(src.as_str()),
        );
        if !src.contains('#') {
            continue;
        }
        let s = amplify(src, &mut rng, 24, i % 3 == 0);
        comments += check(
            &format!("corpus {i}: {s:?}"),
            &SyntaxTree::parse(s.as_str()),
        );
    }
    assert!(comments > 10_000, "{comments}");
}

#[test]
fn index_agrees_with_the_pass_on_generated_shapes() {
    let mut rng = Rng(0x48C0_0003);
    let mut comments = 0;
    for (i, s) in shapes(&mut rng, 400).iter().enumerate() {
        comments += check(&format!("shape {i}: {s:?}"), &SyntaxTree::parse(s.as_str()));
    }
    assert!(comments > 10_000, "{comments}");
}

#[test]
fn index_agrees_with_the_pass_after_edits_inside_long_runs() {
    let mut rng = Rng(0x48C0_0004);
    let mut inputs = shapes(&mut rng, 40);
    let suite = suite();
    for _ in 0..40 {
        let src = &suite[rng.below(suite.len())];
        if src.contains('#') {
            inputs.push(amplify(src, &mut rng, 30, true));
        }
    }
    let (mut accepted, mut checked) = (0, 0);
    for (i, s) in inputs.iter().enumerate() {
        let (a, c) = edit_and_check(&format!("edited {i}: {s:?}"), s, &mut rng, 25, false);
        accepted += a;
        checked += c;
    }
    assert!(accepted > 500, "{accepted} accepted edits");
    assert!(checked > 50_000, "{checked} comments checked");
}

/// The same over edits that insert
/// fragments longer than the index's threshold (their own arenas' long
/// runs), cut them, remove several lines across pieces, roll groups back,
/// fork and clone, and rewrite every comment of a run — on generated
/// shapes, amplified suite inputs, and the long runs of a nested footer.
#[test]
fn index_agrees_with_the_pass_after_long_fragment_edits() {
    let mut rng = Rng(0x48C2_0002);
    let mut inputs = shapes(&mut rng, 30);
    let suite = suite();
    for _ in 0..30 {
        let src = &suite[rng.below(suite.len())];
        if src.contains('#') {
            inputs.push(amplify(src, &mut rng, 30, true));
        }
    }
    let mut footer = String::from("a:\n  b:\n    c: 1\n");
    for i in 0..120 {
        footer.push_str(&format!("{}# f{i}\n", " ".repeat(i % 6)));
    }
    footer.push_str("z: 1\n");
    inputs.push(footer);
    // Recovered inputs stay (error regions between documents, header and
    // footer owners in one run, `Error` nodes among siblings); see
    // `edit_and_check` on how they are compared with a fresh parse.
    let recovered = inputs
        .iter()
        .filter(|s| SyntaxTree::parse(s.as_str()).has_errors())
        .count();
    assert!(recovered >= 5, "{recovered} recovered inputs");
    let (mut accepted, mut checked) = (0, 0);
    for (i, s) in inputs.iter().enumerate() {
        let (a, c) = edit_and_check(&format!("long-edited {i}: {s:?}"), s, &mut rng, 30, true);
        accepted += a;
        checked += c;
    }
    assert!(accepted > 500, "{accepted} accepted edits");
    assert!(checked > 50_000, "{checked} comments checked");
}
