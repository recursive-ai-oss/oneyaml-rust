//! A local edit costs O(depth + log R + local size), never
//! O(document), and reads of an edited tree cost what they cost before
//! the edit (O(1) steps, O(depth) climbs), never O(depth²).
//!
//! Method, chosen to be robust against wall-clock noise: each script runs
//! on fresh trees several times (building the tree and locating the
//! targets are not timed) and the **minimum** is kept, which filters out
//! scheduling noise. Two sizes are compared:
//!
//! * shapes that grow in **breadth** (a flat document, many comments, many
//!   diagnostics) differ 100× in size, so any O(document) step would make
//!   the large run tens of times slower, while the bound allows 4× for
//!   cache effects of the larger arena;
//! * shapes that grow in **depth** (a block chain, tagged flow nesting)
//!   differ 8× in depth: an O(depth) script runs about 8× slower, an
//!   O(depth²) one about 64×; the bound is 24×.
//!
//! Reported with `--nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::sync::Mutex;
use std::time::{Duration, Instant};

use oneyaml_core::{Limits, Settings};
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind as K, SyntaxTree, WalkEvent};

/// The timed tests run one at a time (the harness runs tests in
/// parallel, which would add noise to each other's timings).
static TIMING: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The minimum over 5 runs of `run` on a fresh `setup()` (not timed).
fn best<T>(setup: impl Fn() -> T, run: impl Fn(&mut T) -> usize) -> (Duration, usize) {
    let mut min = Duration::MAX;
    let mut sink = 0;
    for _ in 0..5 {
        let mut x = setup();
        let start = Instant::now();
        sink = run(&mut x);
        min = min.min(start.elapsed());
    }
    (min, sink)
}

fn ratio(small: Duration, large: Duration) -> f64 {
    large.as_secs_f64() / small.as_secs_f64().max(1e-9)
}

fn unlimited() -> Settings {
    Settings::new().limits(Limits::unlimited())
}

// ================================================================ breadth

fn document(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 64);
    let mut i = 0usize;
    while out.len() < bytes {
        out.push_str(&format!("k{i}: [a{i}, b, {{c: d}}] # note\n"));
        i += 1;
    }
    out
}

/// Handles of 64 scalars spread evenly over the document.
fn targets(tree: &SyntaxTree) -> Vec<NodeId> {
    let all: Vec<NodeId> = tree
        .root()
        .descendants()
        .filter(|n| n.kind() == K::Scalar && n.value().is_some_and(|v| v.starts_with('a')))
        .map(|n| n.id())
        .collect();
    (0..64).map(|k| all[k * (all.len() - 1) / 63]).collect()
}

/// The timed script: per target, a token replacement, an insertion after
/// it, reads of the edited and neighbouring elements, and the removal of
/// the insertion.
fn script(tree: &mut SyntaxTree, targets: &[NodeId], extra: &Fragment) -> usize {
    let mut sink = 0usize;
    for &id in targets {
        let tok = tree.get(id).unwrap().first_child().unwrap().id();
        let new = tree.replace_token(tok, "zz").unwrap();
        let ins = tree.insert_after(id, extra.clone()).unwrap();
        let n = tree.get(id).unwrap();
        let seq = n.parent().unwrap();
        sink += n.span().start
            + n.text().len()
            + n.preorder_index()
            + seq.span().end
            + seq.children().count()
            + seq.text().len()
            + tree.get(new).unwrap().span().end
            + n.next_sibling().unwrap().span().start
            + usize::from(n.value().as_deref() == Some("zz"));
        let _ = tree.remove_range(ins.first, ins.last).unwrap();
        sink += tree.get(id).unwrap().span().end;
    }
    sink
}

fn flat(src: &str) -> Duration {
    let tree = SyntaxTree::parse(src);
    let t = SyntaxTree::parse("[x, yy]");
    let seq = t.root().first_child().unwrap().first_child().unwrap();
    let kids: Vec<_> = seq.children().collect();
    let extra = t.extract(kids[2].id(), kids[4].id()).unwrap();
    assert_eq!(extra.text(), ", yy");
    // The string splice of the same edits (the insertions are removed).
    let mut expected = src.to_owned();
    for id in targets(&tree).iter().rev() {
        let sp = tree.get(*id).unwrap().span();
        expected.replace_range(sp.start..sp.end, "zz");
    }
    let (min, _) = best(
        || {
            // A clone has its own handles: locate the targets anew.
            let copy = tree.clone();
            let targets = targets(&copy);
            (copy, targets)
        },
        |(copy, targets)| {
            let sink = script(copy, targets, &extra);
            assert_eq!(copy.emit(), expected);
            sink
        },
    );
    min
}

#[test]
fn local_edits_do_not_scale_with_the_document() {
    let _serial = serial();
    let small = document(100 * 1024);
    let large = document(10 * 1024 * 1024);
    // Warm up allocator and caches.
    let _ = flat(&small);
    let (t_small, t_large) = (flat(&small), flat(&large));
    let r = ratio(t_small, t_large);
    eprintln!(
        "flat: 64 × (replace_token + insert_after + reads + remove_range): {} KB: {t_small:?}, \
         {} KB: {t_large:?}, ratio {r:.2} (O(document) would be ~100)",
        small.len() / 1024,
        large.len() / 1024
    );
    assert!(r < 4.0, "edit cost grew with the document: {r:.2}×");
}

/// A document with `n` comment lines before its
/// content, and a `%TAG` directive; 64 edits that each need the tag
/// context (a tagged node replaced by a copy of itself), 64 token
/// replacements, 64 comment replacements.
fn comments(n: usize) -> Duration {
    let src = format!(
        "%TAG !e! tag:e,2000:\n--- \n{}a: !e!t [1]\n",
        "# c\n".repeat(n)
    );
    let tree = SyntaxTree::parse(src);
    assert!(!tree.has_errors());
    let (min, _) = best(
        || {
            let t = tree.clone();
            let seq = t
                .root()
                .descendants()
                .find(|x| x.kind() == K::FlowSequence)
                .unwrap()
                .id();
            (t, seq)
        },
        |(t, seq)| {
            let mut sink = 0;
            for i in 0..64 {
                let copy = t.extract(*seq, *seq).unwrap();
                let r = t.replace(*seq, copy).unwrap();
                *seq = r.inserted.first;
                let one = t.get(r.inserted.first).unwrap();
                let tok = one.children().nth(3).unwrap().first_child().unwrap().id();
                let new = t.replace_token(tok, &format!("{i}")).unwrap();
                sink += t.get(new).unwrap().span().start;
                sink += t
                    .get(r.inserted.first)
                    .unwrap()
                    .tag()
                    .unwrap()
                    .resolved
                    .len();
            }
            sink
        },
    );
    min
}

/// `n` documents with errors after a clean one;
/// edits in the clean one, each followed by local diagnostic queries.
fn diagnostics(n: usize) -> Duration {
    let src = format!("[x, y]\n{}", "--- a: b: c\n".repeat(n));
    let tree = SyntaxTree::parse(src);
    assert!(tree.diagnostics().len() >= n);
    let (min, _) = best(
        || tree.clone(),
        |t| {
            let mut sink = 0;
            for i in 0..64 {
                let first = t.root().first_child().unwrap();
                let seq = first.first_child().unwrap();
                let tok = seq.children().nth(1).unwrap().first_child().unwrap().id();
                let _ = t.replace_token(tok, &format!("x{i}")).unwrap();
                let first = t.root().first_child().unwrap();
                sink += t.diagnostics_within(first).len();
                let second = core::iter::successors(first.next_sibling(), |x| x.next_sibling())
                    .find(|x| x.kind() == K::Document)
                    .unwrap();
                let err = second.descendants().find(|x| x.kind() == K::Error).unwrap();
                sink += err.diagnostic().unwrap().primary.span.start;
                sink += t.diagnostics_within(second).len();
            }
            sink
        },
    );
    min
}

#[test]
fn comments_and_diagnostics_elsewhere_cost_nothing() {
    let _serial = serial();
    for (what, f) in [
        ("comments", comments as fn(usize) -> Duration),
        ("diagnostics", diagnostics),
    ] {
        let _ = f(1_000);
        let (small, large) = (f(1_000), f(100_000));
        let r = ratio(small, large);
        eprintln!("{what}: 1k: {small:?}, 100k: {large:?}, ratio {r:.2} (O(n) would be ~100)");
        assert!(r < 4.0, "{what}: edit cost grew with the document: {r:.2}×");
    }
}

// ================================================================== depth

/// A block chain `- - - … a` of depth `d` (the
/// shape whose reads were O(depth²) after an edit: no closing bracket ends
/// the nesting). An edit at the bottom, then reads that climb.
fn chain(d: usize) -> (Duration, usize) {
    let src = format!("{}a\n", "- ".repeat(d));
    let tree = SyntaxTree::parse_with(src, &unlimited());
    assert!(!tree.has_errors());
    let entry = innermost_entry(d);
    best(
        || tree.clone(),
        |t| {
            let a = t
                .root()
                .descendants()
                .find(|x| x.kind() == K::PlainScalar)
                .unwrap()
                .id();
            let b = t.replace_token(a, "bb").unwrap();
            let n = t.get(b).unwrap();
            let inner = n.parent().unwrap().parent().unwrap();
            let outer = t.root().first_child().unwrap().first_child().unwrap();
            let mut sink = n.span().start + inner.span().start + inner.text().len();
            sink += outer.span().end + outer.text().len();
            sink += inner.descendants().count();
            sink += inner
                .walk()
                .filter(|s| matches!(s, WalkEvent::Leave(_)))
                .count();
            sink += t.root().descendants().filter(|x| x.is_token()).count();
            // An insertion and a removal at the bottom.
            let inner = inner.id();
            let ins = t.append_child(inner, entry.clone()).unwrap();
            sink += t.get(ins.last).unwrap().span().start;
            let _ = t.remove_range(ins.first, ins.last).unwrap();
            assert_eq!(t.emit().len(), 2 * d + 3);
            sink
        },
    )
}

/// `\n- c`, indented as a new last entry of the innermost sequence of a
/// depth-`d` chain, cut from a parsed chain.
fn innermost_entry(d: usize) -> Fragment {
    let text = format!("{}a\n{}- c\n", "- ".repeat(d), " ".repeat(2 * (d - 1)));
    let t = SyntaxTree::parse_with(text, &unlimited());
    let seq = t
        .root()
        .descendants()
        .filter(|x| x.kind() == K::BlockSequence)
        .last()
        .unwrap();
    let kids: Vec<_> = seq.children().collect();
    assert_eq!(kids.len(), 8);
    t.extract(kids[3].id(), kids[7].id()).unwrap()
}

/// `!t [` nested `d` deep; extracting and replacing
/// the outer sequence copies it once, O(size), not O(tags × depth).
fn tagged(d: usize) -> (Duration, usize) {
    let src = format!("{}{}\n", "!t [".repeat(d), "]".repeat(d));
    let tree = SyntaxTree::parse_with(src, &unlimited());
    assert!(!tree.has_errors());
    best(
        || tree.clone(),
        |t| {
            let outer = t.root().first_child().unwrap().first_child().unwrap().id();
            let copy = t.extract(outer, outer).unwrap();
            let len = copy.len();
            let r = t.replace(outer, copy).unwrap();
            len + r.removed.len()
        },
    )
}

/// An entry appended to the root of
/// a block chain `- - - … a` of depth `d`, after its deep last entry,
/// then removed. The validation window starts at the last entry of the
/// deepest collection the chain ends with; finding it walked the chain
/// with one O(depth) `nav_last` and one O(column) column per level
/// (O(depth²)); now it follows the chain of last children once.
fn append_after_chain(d: usize) -> (Duration, usize) {
    let src = format!("{}a\n", "- ".repeat(d));
    let tree = SyntaxTree::parse_with(src, &unlimited());
    assert!(!tree.has_errors());
    let t = SyntaxTree::parse("- x\n- z");
    let seq = t.root().first_child().unwrap().first_child().unwrap();
    let kids: Vec<_> = seq.children().collect();
    let entry = t.extract(kids[3].id(), kids[6].id()).unwrap();
    assert_eq!(entry.text(), "\n- z");
    best(
        || tree.clone(),
        |t| {
            let root = t.root().first_child().unwrap().first_child().unwrap().id();
            let ins = t.append_child(root, entry.clone()).unwrap();
            let at = t.get(ins.last).unwrap().span().start;
            let _ = t.remove_range(ins.first, ins.last).unwrap();
            at
        },
    )
}

#[test]
fn deep_edits_and_reads_are_linear_in_depth() {
    let _serial = serial();
    for (what, f) in [
        ("block chain", chain as fn(usize) -> (Duration, usize)),
        ("tagged flow", tagged),
        ("appended after a block chain", append_after_chain),
    ] {
        let _ = f(2_000);
        // The small and the large depth alternate, each keeping
        // its minimum, so a burst of load on the host slows both rather
        // than only the one it happens to hit (the tagged shape read
        // 24-28x under load, 10x alone, one measurement after the other).
        // 7 alternations of 5 runs each (35 samples
        // per depth, was 15), and the tagged shape itself fixed: its
        // comparison items were 168 bytes and materialised on both sides
        // (~12 MB at 16k, out of cache: 11-14x for 8x the depth, 25x under
        // load); now 40 bytes, one side, ~8x (instruction counts were
        // linear throughout: 45.5M at 2k, 349M at 16k).
        let (mut small, mut large) = (Duration::MAX, Duration::MAX);
        for _ in 0..7 {
            small = small.min(f(2_000).0);
            large = large.min(f(16_000).0);
        }
        let r = ratio(small, large);
        eprintln!(
            "{what}: depth 2k: {small:?}, 16k: {large:?}, ratio {r:.2} (linear ~8, quadratic ~64)"
        );
        assert!(r < 24.0, "{what}: cost grew faster than depth: {r:.2}×");
    }
}

// ============================================== next to large things

/// The scalar token whose text is `text`.
fn plain(t: &SyntaxTree, text: &str) -> NodeId {
    t.root()
        .descendants()
        .find(|x| x.kind() == K::PlainScalar && x.text() == text)
        .unwrap()
        .id()
}

/// `n` lines of a nested mapping, as the value of `key`.
fn big(key: &str, n: usize) -> String {
    let mut out = format!("{key}:\n");
    for i in 0..n {
        out.push_str(&format!("  k{i}: v\n"));
    }
    out
}

/// 64 edits in `src`, each done and undone by
/// `script` on the element `find` locates (not timed), on fresh clones;
/// the minimum over 5 runs.
fn timed(
    src: &str,
    find: fn(&SyntaxTree) -> NodeId,
    script: fn(&mut SyntaxTree, &mut NodeId, usize) -> usize,
) -> Duration {
    let tree = SyntaxTree::parse(src);
    assert!(!tree.has_errors(), "{}", &src[..src.len().min(200)]);
    let (min, _) = best(
        || {
            let t = tree.clone();
            let id = find(&t);
            (t, id)
        },
        |(t, id)| {
            let mut sink = 0;
            for i in 0..64 {
                sink += script(t, id, i);
            }
            sink + t.len()
        },
    );
    let mut t = tree.clone();
    let mut id = find(&t);
    for i in 0..64 {
        let _ = script(&mut t, &mut id, i);
    }
    // The edited tree agrees with its text.
    assert_eq!(SyntaxTree::parse(t.emit()), t);
    min
}

/// Replaces token `id` by `9` and back.
fn flip(t: &mut SyntaxTree, id: &mut NodeId, i: usize) -> usize {
    *id = t
        .replace_token(*id, if i % 2 == 0 { "9" } else { "1" })
        .unwrap();
    t.get(*id).unwrap().span().start
}

/// `a: 1` before a large entry: the window stops at the next entry's
/// first line (before the fix it re-read the whole neighbour: 102× at 100×).
fn before_large(n: usize) -> Duration {
    timed(
        &format!("a: 1\n{}z: 2\n", big("b", n)),
        |t| plain(t, "1"),
        flip,
    )
}

/// `a: 1` after a large entry: the window starts at its own entry.
fn after_large(n: usize) -> Duration {
    timed(&format!("{}a: 1\n", big("b", n)), |t| plain(t, "1"), flip)
}

/// An entry appended after a large last entry, then removed: the window
/// starts at the last entry of the deepest collection it ends with.
fn append_after_large(n: usize) -> Duration {
    timed(
        &big("b", n),
        |t| t.root().first_child().unwrap().first_child().unwrap().id(),
        |t, map, _| {
            let f = Fragment::parse("\nz: 1").unwrap().unwrap_root(1).unwrap();
            let ins = t.append_child(*map, f).unwrap();
            let at = t.get(ins.last).unwrap().span().start;
            let _ = t.remove_range(ins.first, ins.last).unwrap();
            at
        },
    )
}

/// The middle comment.
fn mid_comment(t: &SyntaxTree) -> NodeId {
    let comments: Vec<NodeId> = t
        .root()
        .descendants()
        .filter(|x| x.kind() == K::Comment)
        .map(|x| x.id())
        .collect();
    comments[comments.len() / 2]
}

/// Inserts `# x` and a line break before comment `mid`, then removes
/// them.
fn comment_in_run(t: &mut SyntaxTree, mid: &mut NodeId, _: usize) -> usize {
    let f = Fragment::parse("# x\n").unwrap();
    let ins = t.insert_before(*mid, f).unwrap();
    let at = t.get(ins.first).unwrap().span().start;
    let _ = t.remove_range(ins.first, ins.last).unwrap();
    at
}

/// A comment line inserted before a document-level comment (between
/// `---` and the root), then removed (before the fix: 103× at 100×).
fn document_comments(n: usize) -> Duration {
    let src = format!("--- \n{}a: 1\n", "# c\n".repeat(n));
    timed(&src, mid_comment, comment_in_run)
}

/// The same at stream level, after the last document (before the fix: 109×).
fn stream_comments(n: usize) -> Duration {
    let src = format!("a: 1\n{}", "# c\n".repeat(n));
    timed(&src, mid_comment, comment_in_run)
}

/// A document's root replaced among `n` documents: O(that document).
fn documents(n: usize) -> Duration {
    let src: String = (0..n).map(|i| format!("--- d{i}\n")).collect();
    timed(
        &src,
        |t| {
            let n = t.root().children().count() / 4;
            let mid = format!("d{n}");
            plain(t, &mid)
        },
        |t, id, i| {
            *id = t.replace_token(*id, &format!("e{i}")).unwrap();
            t.get(*id).unwrap().span().start
        },
    )
}

#[test]
fn edits_next_to_large_neighbours_and_trivia_runs_stay_local() {
    let _serial = serial();
    for (what, f) in [
        (
            "before a large entry",
            before_large as fn(usize) -> Duration,
        ),
        ("after a large entry", after_large),
        ("appended after a large entry", append_after_large),
        ("document-level comment run", document_comments),
        ("stream-level comment run", stream_comments),
        ("one document among many", documents),
    ] {
        let _ = f(1_000);
        let (small, large) = (f(1_000), f(100_000));
        let r = ratio(small, large);
        eprintln!("{what}: 1k: {small:?}, 100k: {large:?}, ratio {r:.2} (O(n) would be ~100)");
        assert!(r < 4.0, "{what}: edit cost grew with the document: {r:.2}×");
    }
}

// ======================================================= more shapes

/// [`timed`] under `settings`.
fn timed_with(
    settings: &Settings,
    src: &str,
    find: fn(&SyntaxTree) -> NodeId,
    script: fn(&mut SyntaxTree, &mut NodeId, usize) -> usize,
) -> Duration {
    let tree = SyntaxTree::parse_with(src, settings);
    assert!(!tree.has_errors(), "{}", &src[..src.len().min(200)]);
    let (min, _) = best(
        || {
            let t = tree.clone();
            let id = find(&t);
            (t, id)
        },
        |(t, id)| {
            let mut sink = 0;
            for i in 0..64 {
                sink += script(t, id, i);
            }
            sink + t.len()
        },
    );
    let mut t = tree.clone();
    let mut id = find(&t);
    for i in 0..64 {
        let _ = script(&mut t, &mut id, i);
    }
    // The edited tree agrees with its text, diagnostics included.
    assert_eq!(SyntaxTree::parse_with(t.emit(), settings), t);
    min
}

/// A finite `max_documents`, so the document count is checked on every
/// document edit (the default, `usize::MAX`, could skip it).
fn many_documents() -> Settings {
    Settings::new().limits(Limits::new().max_documents(10_000_000))
}

/// A document inserted among `n` documents, and
/// removed, 64 times. Counting the documents per insertion made each one
/// O(n).
fn insert_document(n: usize) -> Duration {
    let src = "--- a\n".repeat(n);
    timed_with(
        &many_documents(),
        &src,
        |t| {
            let docs: Vec<NodeId> = t
                .root()
                .children()
                .filter(|x| x.kind() == K::Document)
                .map(|x| x.id())
                .collect();
            docs[docs.len() / 2]
        },
        |t, doc, _| {
            let f = Fragment::parse("--- z\n").unwrap();
            let ins = t.insert_before(*doc, f).unwrap();
            let at = t.get(ins.first).unwrap().span().start;
            let _ = t.remove_range(ins.first, ins.last).unwrap();
            at
        },
    )
}

/// 65 comments between a large document and `--- b`;
/// `b` replaced. The walk back to the document gave up after 64 trivia
/// elements, so the window took the whole large document.
fn after_comment_run(n: usize) -> Duration {
    let src = format!("{}{}--- b\n", big("a", n), "# c\n".repeat(65));
    timed(&src, |t| plain(t, "b"), flip)
}

/// A previous document with a warning (`%YAML 1.1`) made the
/// next document's start unsafe, so its window took the previous
/// document whole.
fn after_warned_document(n: usize) -> Duration {
    let src = format!("%YAML 1.1\n---\n{}--- b\n", big("a", n));
    let tree = SyntaxTree::parse(src.as_str());
    assert_eq!(tree.diagnostics().len(), 1);
    timed(&src, |t| plain(t, "b"), flip)
}

/// A long run of comments indented deeper than the mapping
/// after its last entry, whose value is a plain scalar: the first comment
/// ends what the scalar can read, so the window stops there.
fn deeper_comments(n: usize) -> Duration {
    let src = format!("a:\n  b: 1\n{}z: 2\n", "    # c\n".repeat(n));
    timed(&src, |t| plain(t, "1"), flip)
}

/// `n` lines with a `Y0028` warning each under
/// `lenient_inline_colons`; an edit at the top checked every warning of
/// the document, twice.
fn many_warnings(n: usize) -> Duration {
    let settings = Settings::new().lenient_inline_colons(true);
    let mut src = String::from("x: 1\n");
    for i in 0..n {
        src.push_str(&format!("k{i}: a: b\n"));
    }
    let tree = SyntaxTree::parse_with(src.as_str(), &settings);
    assert_eq!(tree.diagnostics().len(), n);
    timed_with(&settings, &src, |t| plain(t, "1"), flip)
}

#[test]
fn unit_21e_edits_stay_local() {
    let _serial = serial();
    for (what, f) in [
        (
            "a document inserted among many",
            insert_document as fn(usize) -> Duration,
        ),
        ("a document after 65 comments", after_comment_run),
        ("a document after a warned document", after_warned_document),
        ("comments deeper than the mapping", deeper_comments),
        ("a document of lenient warnings", many_warnings),
    ] {
        let _ = f(1_000);
        let (small, large) = (f(1_000), f(100_000));
        let r = ratio(small, large);
        eprintln!("{what}: 1k: {small:?}, 100k: {large:?}, ratio {r:.2} (O(n) would be ~100)");
        assert!(r < 4.0, "{what}: edit cost grew with the document: {r:.2}×");
    }
}

/// `n` documents replaced by one at stream level. The
/// document count skipped the removed ones with a linear search per
/// document (O(n²): 10¹⁰ comparisons at 100k); now the removal is linear.
fn replace_all_documents(n: usize) -> (Duration, usize) {
    let src = "--- a\n".repeat(n);
    let tree = SyntaxTree::parse_with(src, &many_documents());
    best(
        || tree.clone(),
        |t| {
            let docs: Vec<NodeId> = t
                .root()
                .children()
                .filter(|x| x.kind() == K::Document)
                .map(|x| x.id())
                .collect();
            let f = Fragment::parse("--- z").unwrap();
            let r = t.replace_range(docs[0], docs[docs.len() - 1], f).unwrap();
            assert_eq!(t.emit(), "--- z\n");
            r.removed.len()
        },
    )
}

#[test]
fn replacing_many_documents_is_linear() {
    let _serial = serial();
    let _ = replace_all_documents(2_000);
    let (small, _) = replace_all_documents(2_000);
    let (large, _) = replace_all_documents(32_000);
    let r = ratio(small, large);
    eprintln!(
        "replace all documents: 2k: {small:?}, 32k: {large:?}, ratio {r:.2} (linear ~16, \
         quadratic ~256)"
    );
    assert!(
        r < 64.0,
        "replacing documents grew faster than linearly: {r:.2}×"
    );
}

/// Kept as a shape: a space inserted
/// before a comment that follows a deep block chain, then removed. The
/// trivia shortcut reads the last token of the content before the run
/// (the chain's bottom): O(depth), where one `nav_last` per level was
/// O(depth²).
fn comment_after_chain(d: usize) -> (Duration, usize) {
    let src = format!("{}a\n# c\n", "- ".repeat(d));
    let tree = SyntaxTree::parse_with(src, &unlimited());
    assert!(!tree.has_errors());
    best(
        || tree.clone(),
        |t| {
            // An edit at the bottom first, so the reads below run on an
            // edited tree.
            let a = t
                .root()
                .descendants()
                .find(|x| x.kind() == K::PlainScalar)
                .unwrap()
                .id();
            let _ = t.replace_token(a, "bb").unwrap();
            let c = t
                .root()
                .descendants()
                .find(|x| x.kind() == K::Comment)
                .unwrap()
                .id();
            let ins = t.insert_before(c, Fragment::parse(" ").unwrap()).unwrap();
            let at = t.get(ins.first).unwrap().span().start;
            let _ = t.remove_range(ins.first, ins.last).unwrap();
            assert_eq!(t.emit().len(), 2 * d + 7);
            at
        },
    )
}

#[test]
fn a_comment_after_a_deep_chain_is_edited_in_linear_time() {
    let _serial = serial();
    let _ = comment_after_chain(2_000);
    let (small, _) = comment_after_chain(2_000);
    let (large, _) = comment_after_chain(16_000);
    let r = ratio(small, large);
    eprintln!(
        "comment after a block chain: depth 2k: {small:?}, 16k: {large:?}, ratio {r:.2} \
         (linear ~8, quadratic ~64)"
    );
    assert!(r < 24.0, "cost grew faster than depth: {r:.2}×");
}

// ======================================================= forks and history

/// An entry appended to the root of a block chain of
/// depth `d` (`- - … &x &y a`) whose bottom is an error (two anchors),
/// then removed. With an error below the value, `deepest_last` checks every
/// level, and each level's column was an O(column) scan of the one line
/// they all share: O(depth²) (1.0 ms at 1k, 213 ms at 16k).
fn append_after_erroneous_chain(d: usize) -> (Duration, usize) {
    let src = format!("{}&x &y a\n", "- ".repeat(d));
    let tree = SyntaxTree::parse_with(src, &unlimited());
    assert!(tree.has_errors());
    let t = SyntaxTree::parse("- x\n- z");
    let seq = t.root().first_child().unwrap().first_child().unwrap();
    let kids: Vec<_> = seq.children().collect();
    let entry = t.extract(kids[3].id(), kids[6].id()).unwrap();
    assert_eq!(entry.text(), "\n- z");
    best(
        || tree.clone(),
        |t| {
            let root = t.root().first_child().unwrap().first_child().unwrap().id();
            match t.append_child(root, entry.clone()) {
                Ok(ins) => {
                    let at = t.get(ins.last).unwrap().span().start;
                    let _ = t.remove_range(ins.first, ins.last).unwrap();
                    at
                }
                Err(e) => format!("{e:?}").len(),
            }
        },
    )
}

#[test]
fn an_error_in_a_deep_chain_keeps_appending_linear() {
    let _serial = serial();
    let _ = append_after_erroneous_chain(2_000);
    let (small, _) = append_after_erroneous_chain(2_000);
    let (large, _) = append_after_erroneous_chain(16_000);
    let r = ratio(small, large);
    eprintln!(
        "appended after an erroneous chain: depth 2k: {small:?}, 16k: {large:?}, ratio {r:.2} \
         (linear ~8, quadratic ~64)"
    );
    assert!(r < 24.0, "cost grew faster than depth: {r:.2}×");
}

/// `n` comment lines between a document and `--- b`;
/// `b` replaced 64 times. The walk back to the document before stepped
/// over every comment (0.72 ms at 1k, 562 ms at 1M); the run is now
/// jumped over. `after_comment_run` fixes the run at 65 lines; this one
/// grows it.
fn after_growing_comment_run(n: usize) -> Duration {
    let src = format!("a: 1\n{}--- b\n", "# c\n".repeat(n));
    timed(&src, |t| plain(t, "b"), flip)
}

/// The same with the run cut by edits into overlay runs first (a comment
/// replaced in the middle, one near each end): the jump crosses each
/// piece in O(log R).
fn after_edited_comment_run(n: usize) -> Duration {
    let src = format!("a: 1\n{}--- b\n", "# c\n".repeat(n));
    let tree = SyntaxTree::parse(src.as_str());
    let mut edited = tree.clone();
    let comments: Vec<NodeId> = edited
        .root()
        .children()
        .filter(|x| x.kind() == K::Comment)
        .map(|x| x.id())
        .collect();
    for at in [3, comments.len() / 2, comments.len() - 3] {
        let _ = edited.replace_token(comments[at], "# d").unwrap();
    }
    let (min, _) = best(
        || {
            let t = edited.fork();
            let id = plain(&t, "b");
            (t, id)
        },
        |(t, id)| {
            let mut sink = 0;
            for i in 0..64 {
                sink += flip(t, id, i);
            }
            sink
        },
    );
    min
}

#[test]
fn unit_21f_root_edits_after_long_comment_runs_stay_local() {
    let _serial = serial();
    for (what, f) in [
        (
            "a document after a growing comment run",
            after_growing_comment_run as fn(usize) -> Duration,
        ),
        (
            "a document after an edited comment run",
            after_edited_comment_run,
        ),
    ] {
        let _ = f(1_000);
        let (small, large) = (f(1_000), f(100_000));
        let r = ratio(small, large);
        eprintln!("{what}: 1k: {small:?}, 100k: {large:?}, ratio {r:.2} (O(n) would be ~100)");
        assert!(r < 4.0, "{what}: edit cost grew with the document: {r:.2}×");
    }
}

/// `has_errors` on an edited tree with `n` warnings
/// built and cloned every diagnostic; it is a count now.
fn has_errors_after_an_edit(n: usize) -> Duration {
    let settings = Settings::new().lenient_inline_colons(true);
    let mut src = String::from("x: 1\n");
    for i in 0..n {
        src.push_str(&format!("k{i}: a: b\n"));
    }
    let mut tree = SyntaxTree::parse_with(src.as_str(), &settings);
    let one = plain(&tree, "1");
    let _ = tree.replace_token(one, "2").unwrap();
    assert!(tree.is_edited());
    let (min, hits) = best(|| (), |()| (0..256).filter(|_| !tree.has_errors()).count());
    assert_eq!(hits, 256);
    min
}

#[test]
fn has_errors_is_constant_time() {
    let _serial = serial();
    let _ = has_errors_after_an_edit(1_000);
    let (small, large) = (
        has_errors_after_an_edit(1_000),
        has_errors_after_an_edit(100_000),
    );
    let r = ratio(small, large);
    eprintln!("has_errors of an edited tree: 1k: {small:?}, 100k: {large:?}, ratio {r:.2}");
    assert!(r < 4.0, "has_errors grew with the diagnostics: {r:.2}×");
}

// ============================================================ recovered input

/// 64 replacements of the scalar `x`
/// (and back) in recovered input `src`, accepted or refused, on fresh
/// clones; the minimum over 5 runs. Every state agrees with its text.
fn recovered_flips(src: &str) -> Duration {
    let tree = SyntaxTree::parse(src);
    assert!(tree.has_errors(), "{}", &src[..src.len().min(200)]);
    let script = |t: &mut SyntaxTree, id: &mut NodeId, i: usize| {
        if let Ok(new) = t.replace_token(*id, if i % 2 == 0 { "z" } else { "x" }) {
            *id = new;
        }
        t.get(*id).unwrap().span().start
    };
    let (min, _) = best(
        || {
            let t = tree.clone();
            let id = plain(&t, "x");
            (t, id)
        },
        |(t, id)| {
            let mut sink = 0;
            for i in 0..64 {
                sink += script(t, id, i);
            }
            sink + t.len()
        },
    );
    let mut t = tree.clone();
    let mut id = plain(&t, "x");
    for i in 0..64 {
        let _ = script(&mut t, &mut id, i);
    }
    assert_eq!(SyntaxTree::parse(t.emit()), t);
    min
}

/// A flow collection after `n` comment lines inside a flow mapping, in a
/// document with an error after it: deciding whether the collection sits
/// in a key's place walked every comment line before it (4.73 ms at
/// 400k lines, 32 µs once fixed).
fn flow_after_comments(n: usize) -> Duration {
    recovered_flips(&format!("k: {{a:{}\n [x, y]}}\nb: ]\n", "\n # c".repeat(n)))
}

/// A document with a directive error and `n` comment lines among its
/// directives, and a content error: telling the two errors apart walked
/// the document's first children.
fn directives_then_error(n: usize) -> Duration {
    recovered_flips(&format!(
        "%YAML 1.2\n%YAML 1.2\n{}---\nk: [x, y]\nb: ]\n",
        "# c\n".repeat(n)
    ))
}

/// Probe p13: the edit is in a clean
/// document ended by `...`; the next document has `n - 1` duplicate
/// `%YAML` errors (`Y1004`) and a content error. Whether that document's
/// errors reach the edit scanned every directive error (3.74 ms at 400k).
fn duplicate_yaml_after(n: usize) -> Duration {
    recovered_flips(&format!(
        "c: x\n...\n{}---\nb: [y\n",
        "%YAML 1.2\n".repeat(n)
    ))
}

/// Probe p14: as [`duplicate_yaml_after`], with duplicate
/// `%TAG` handles.
fn duplicate_tag_after(n: usize) -> Duration {
    recovered_flips(&format!(
        "c: x\n...\n{}---\nb: [y\n",
        "%TAG !t! tag:t,2000:\n".repeat(n)
    ))
}

/// Probe p11b: the document with the directive errors is two
/// documents before the edit's (the one between has no errors).
fn duplicate_yaml_two_before(n: usize) -> Duration {
    recovered_flips(&format!(
        "{}---\nb: [y\n--- \nq: 1\n---\nc: x\n",
        "%YAML 1.2\n".repeat(n)
    ))
}

/// The next document is one
/// `facts.rs` records as mixed (a failed directive block, `Y0001`, kept
/// with the `n` duplicate `%YAML` lines after it) and has a content
/// error. Whether its errors reach the edit scanned every directive error
/// (4.15 ms at 400k, 13 µs at 1k); a mixed document is now taken to reach
/// it, in O(1).
fn mixed_yaml_after(n: usize) -> Duration {
    recovered_flips(&format!(
        "c: x\n...\n%YAML 0.2\n----\n...\n{}---\nb: [\n",
        "%YAML 1.2\n".repeat(n)
    ))
}

/// As [`mixed_yaml_after`], with duplicate `%TAG` handles.
fn mixed_tag_after(n: usize) -> Duration {
    recovered_flips(&format!(
        "c: x\n...\n%TAG !t! a\n----\n...\n{}--- [y\n",
        "%TAG !t! a\n".repeat(n)
    ))
}

/// The mixed document is two documents before the edit's.
fn mixed_yaml_two_before(n: usize) -> Duration {
    recovered_flips(&format!(
        "%YAML 0.2\n----\n...\n{}---\nb: [y\n--- \nq: 1\n---\nc: x\n",
        "%YAML 1.2\n".repeat(n)
    ))
}

#[test]
fn edits_in_recovered_documents_stay_local() {
    let _serial = serial();
    for (what, f) in [
        (
            "flow collection after a comment run",
            flow_after_comments as fn(usize) -> Duration,
        ),
        ("comment run among directives", directives_then_error),
        (
            "duplicate %YAML directives in the next document (p13)",
            duplicate_yaml_after,
        ),
        (
            "duplicate %TAG directives in the next document (p14)",
            duplicate_tag_after,
        ),
        (
            "duplicate %YAML directives two documents before (p11b)",
            duplicate_yaml_two_before,
        ),
        (
            "a mixed document with duplicate %YAML next (48h5)",
            mixed_yaml_after,
        ),
        (
            "a mixed document with duplicate %TAG next (48h5)",
            mixed_tag_after,
        ),
        (
            "a mixed document two documents before (48h5)",
            mixed_yaml_two_before,
        ),
    ] {
        let _ = f(1_000);
        let (small, large) = (f(1_000), f(100_000));
        let r = ratio(small, large);
        eprintln!("{what}: 1k: {small:?}, 100k: {large:?}, ratio {r:.2} (O(n) would be ~100)");
        assert!(r < 4.0, "{what}: edit cost grew with the document: {r:.2}×");
    }
}
