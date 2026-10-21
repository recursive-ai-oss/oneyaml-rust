//! **Edit cost is independent
//! of edit history**. The k-th edit on a tree costs what the first one
//! costs (amortised O(depth + log R + local) for R runs), and reads after
//! many edits cost O(log R) or O(local), never O(edits).
//!
//! Method, chosen to be robust against wall-clock noise and against cache
//! effects of a larger document: every comparison runs on the **same
//! document**, so only the edit history differs. A script of `n` edits
//! runs on fresh copies of the tree (copying and locating the targets is
//! not timed); the minimum of several runs is kept, and the small and the
//! large script alternate, so a burst of load hits both. The per-edit cost
//! of 10k edits is compared with that of the first 1k: flat is ~1, an
//! O(R) step per edit makes it ~5.5 (the mean history is 5.5× longer),
//! and the bound is 2.5×.
//!
//! Reported with `--nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]

mod work;

use std::sync::Mutex;

use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Fragment, NodeId, SyntaxKind as K, SyntaxTree};

/// The timed tests run one at a time.
static TIMING: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `small` and `large` alternately `rounds` times, each on a fresh
/// `setup()` (not timed), and returns the minimum time of each.
fn alternate<T>(
    rounds: usize,
    setup: impl Fn() -> T,
    small: impl Fn(&mut T),
    large: impl Fn(&mut T),
) -> (u64, u64) {
    let (mut a, mut b) = (u64::MAX, u64::MAX);
    for _ in 0..rounds {
        let mut x = setup();
        work::reset();
        small(&mut x);
        a = a.min(work::taken());
        let mut x = setup();
        work::reset();
        large(&mut x);
        b = b.min(work::taken());
    }
    (a, b)
}

/// Per-edit cost of `large_n` edits over that of `small_n`.
fn per_edit_ratio(small: u64, small_n: usize, large: u64, large_n: usize) -> f64 {
    let s = small as f64 / small_n as f64;
    let l = large as f64 / large_n as f64;
    l / s.max(1e-12)
}

const SMALL: usize = 1_000;
const LARGE: usize = 10_000;
const BOUND: f64 = 2.5;

/// The three line breaks (§5.4; the tests whose edits move
/// or sit next to line breaks run over each).
const EOLS: [&str; 3] = ["\n", "\r\n", "\r"];

/// `anchor: &x 1` and a list of `k` aliases `*x`.
fn aliases(k: usize) -> String {
    let mut s = String::from("anchor: &x 1\nlist:\n");
    for _ in 0..k {
        s.push_str("  - *x\n");
    }
    s
}

fn alias_tokens(t: &SyntaxTree) -> Vec<NodeId> {
    t.root()
        .descendants()
        .filter(|n| n.kind() == K::AliasName)
        .map(|n| n.id())
        .collect()
}

fn anchor_token(t: &SyntaxTree) -> NodeId {
    t.root()
        .descendants()
        .find(|n| n.kind() == K::Anchor)
        .unwrap()
        .id()
}

/// (a) `n` sequential `replace_token` calls on one tree: the probe of a
/// sequential-edit probe (2k/4k/8k calls took 0.14 s / 0.71 s / 1.60 s, each
/// quarter slower than the one before). The backward diagnostic scan of the
/// well-formedness check walked every run back to the document's start.
#[test]
#[ignore = "slow: runs in CI"]
fn replace_token_cost_does_not_grow_with_edit_count() {
    let _serial = serial();
    let tree = SyntaxTree::parse(aliases(LARGE));
    let edit = |n: usize| {
        move |(t, ids): &mut (SyntaxTree, Vec<NodeId>)| {
            for (i, id) in ids.iter().take(n).enumerate() {
                let name = if i % 2 == 0 { "*y" } else { "*z" };
                let _ = t.replace_token(*id, name).unwrap();
            }
        }
    };
    let (small, large) = alternate(
        3,
        || {
            let t = tree.clone();
            let ids = alias_tokens(&t);
            (t, ids)
        },
        edit(SMALL),
        edit(LARGE),
    );
    let r = per_edit_ratio(small, SMALL, large, LARGE);
    eprintln!(
        "replace_token: first {SMALL}: {small:?}, {LARGE}: {large:?}, per-edit ratio {r:.2} \
         (flat ~1, O(R) per edit ~5.5)"
    );
    assert!(r < BOUND, "per-edit cost grew with the edit count: {r:.2}×");
}

/// `- e0 … - e{k-1}` and the fragment `\n- z` cut from a parsed list,
/// every line break `eol`.
fn entries(k: usize, eol: &str) -> (String, Fragment) {
    let mut s = String::new();
    for i in 0..k {
        s.push_str(&format!("- e{i}{eol}"));
    }
    let t = SyntaxTree::parse(format!("- x{eol}- z"));
    let seq = t.root().first_child().unwrap().first_child().unwrap();
    let kids: Vec<_> = seq.children().collect();
    let entry = t.extract(kids[3].id(), kids[6].id()).unwrap();
    assert_eq!(entry.text(), format!("{eol}- z"));
    (s, entry)
}

/// The scalar nodes of the list's entries.
fn entry_values(t: &SyntaxTree) -> Vec<NodeId> {
    t.root()
        .descendants()
        .filter(|n| n.kind() == K::Scalar)
        .map(|n| n.id())
        .collect()
}

/// (b) `n` sequential `insert_after` / `remove_range` pairs on one tree:
/// a new entry after the i-th, then its removal (each pair leaves a cut
/// behind, so R grows with the history). Over each line break.
#[test]
#[ignore = "slow: runs in CI"]
fn insert_remove_pairs_cost_does_not_grow_with_edit_count() {
    let _serial = serial();
    for eol in EOLS {
        insert_remove_pairs_with(eol);
    }
}

fn insert_remove_pairs_with(eol: &str) {
    let (src, entry) = entries(LARGE, eol);
    let tree = SyntaxTree::parse(src.as_str());
    let edit = |n: usize| {
        let entry = entry.clone();
        move |(t, ids): &mut (SyntaxTree, Vec<NodeId>)| {
            for id in ids.iter().take(n) {
                let ins = t.insert_after(*id, entry.clone()).unwrap();
                let _ = t.remove_range(ins.first, ins.last).unwrap();
            }
        }
    };
    let (small, large) = alternate(
        3,
        || {
            let t = tree.clone();
            let ids = entry_values(&t);
            (t, ids)
        },
        edit(SMALL),
        edit(LARGE),
    );
    let r = per_edit_ratio(small, SMALL, large, LARGE);
    eprintln!(
        "{eol:?}: insert_after + remove_range: first {SMALL}: {small:?}, {LARGE}: {large:?}, \
         per-pair ratio {r:.2} (flat ~1)"
    );
    assert!(
        r < BOUND,
        "{eol:?}: per-pair cost grew with the edit count: {r:.2}×"
    );
    // And the text is the original one (every insertion was removed).
    let mut t = tree.clone();
    let ids = entry_values(&t);
    for id in ids.iter().take(64) {
        let ins = t.insert_after(*id, entry.clone()).unwrap();
        let _ = t.remove_range(ins.first, ins.last).unwrap();
    }
    assert_eq!(t.emit().as_bytes(), src.as_bytes());
}

/// The same document with a warning at its start (`Y0028` under
/// `lenient_inline_colons`): the backward scan stops at a warning, the
/// forward one reads only the edit's neighbourhood; neither may cross the
/// runs of the history. Over each line break (the warning is
/// attached to the line break after its scalar).
#[test]
#[ignore = "slow: runs in CI"]
fn edits_next_to_diagnostics_do_not_grow_with_edit_count() {
    let _serial = serial();
    for eol in EOLS {
        edits_next_to_diagnostics_with(eol);
    }
}

fn edits_next_to_diagnostics_with(eol: &str) {
    let settings = Settings::new().lenient_inline_colons(true);
    let src = format!("w: a: b\n{}", aliases(LARGE)).replace('\n', eol);
    let tree = SyntaxTree::parse_with(src, &settings);
    assert!(!tree.has_errors());
    assert!(tree.diagnostics().len() > 0);
    let edit = |n: usize| {
        move |(t, ids): &mut (SyntaxTree, Vec<NodeId>)| {
            for id in ids.iter().take(n) {
                let _ = t.replace_token(*id, "*y").unwrap();
            }
        }
    };
    let (small, large) = alternate(
        3,
        || {
            let t = tree.clone();
            let ids = alias_tokens(&t);
            (t, ids)
        },
        edit(SMALL),
        edit(LARGE),
    );
    let r = per_edit_ratio(small, SMALL, large, LARGE);
    eprintln!(
        "{eol:?}: replace_token after a warning: first {SMALL}: {small:?}, {LARGE}: {large:?}, \
         per-edit ratio {r:.2}"
    );
    assert!(
        r < BOUND,
        "{eol:?}: per-edit cost grew with the edit count: {r:.2}×"
    );
}

/// (c) After `n` edits, reads: `get`, `span`, `text`, `value`, parent
/// climbs, and the diagnostics of the document cost the same after 10k
/// edits as after 1k (O(log R) or O(local) each); `emit` and the root's
/// `text()` are O(n + R): linear in the runs the edits made, and within
/// 1.5× a parse of the text (best of 11; see the margins below).
#[test]
#[ignore = "slow: runs in CI"]
fn reads_after_many_edits_stay_cheap() {
    let _serial = serial();
    let settings = Settings::new().lenient_inline_colons(true);
    let src = format!("w: a: b\n{}", aliases(LARGE));
    let base = SyntaxTree::parse_with(src, &settings);
    let edited = |n: usize| {
        let mut t = base.clone();
        let ids = alias_tokens(&t);
        let mut new = Vec::with_capacity(n);
        for id in ids.iter().take(n) {
            new.push(t.replace_token(*id, "*y").unwrap());
        }
        // Read targets spread over the whole list, edited or not.
        let all = alias_tokens(&t);
        let targets: Vec<NodeId> = (0..256).map(|k| all[k * (all.len() - 1) / 255]).collect();
        (t, targets)
    };
    let (small_tree, small_targets) = edited(SMALL);
    let (large_tree, large_targets) = edited(LARGE);
    let reads = |t: &SyntaxTree, targets: &[NodeId]| {
        let mut sink = 0usize;
        let doc = t.root().first_child().unwrap();
        // Each target is read several times in a row: the repeats run
        // with warm caches, so what is compared is the work a read does
        // (O(log R) against O(R)), not how far apart in memory 30k runs
        // lie from 3k (a cold read of a larger treap misses more).
        for id in targets {
            for _ in 0..16 {
                let n = t.get(*id).unwrap();
                let alias = n.parent().unwrap();
                sink += n.span().start
                    + n.text().len()
                    + alias.span().end
                    + alias.alias_name().map_or(0, str::len)
                    + alias.parent().unwrap().kind() as usize
                    + n.preorder_index();
            }
            // The document's diagnostics: the runs without any are
            // skipped (O(log R) each), not visited.
            let (_, d) = t.diagnostics_within(doc).next().unwrap();
            sink += d.primary.span.start + usize::from(t.has_errors());
        }
        sink
    };
    let mut rs = (u64::MAX, u64::MAX);
    for _ in 0..7 {
        work::reset();
        std::hint::black_box(reads(&small_tree, &small_targets));
        rs.0 = rs.0.min(work::taken());
        work::reset();
        std::hint::black_box(reads(&large_tree, &large_targets));
        rs.1 = rs.1.min(work::taken());
    }
    let r = work::ratio("scaling", rs.0, rs.1);
    eprintln!(
        "reads after {SMALL} edits: {:?}, after {LARGE}: {:?}, ratio {r:.2} (O(R) would be ~10)",
        rs.0, rs.1
    );
    // Reads are O(log R): ~1.3× the instructions for 10× the runs, ~2×
    // the time (a larger treap and link map miss more even when warm);
    // O(R) would be ≥ 10×.
    assert!(r < 4.0, "reads grew with the edit count: {r:.2}×");
    // Emitting and the root's text: O(n + R) (R: the runs the edits made,
    // up to one per edit). The check counts steps, so it needs no margin
    // for load: the emit after 10k edits against the emit after 1k, on the
    // same text (only the edit history differs, as in the rest of this file).
    // Anything quadratic in R is about 100×; the bound is 20×.
    let text = large_tree.emit();
    assert_eq!(large_tree.root().text(), text);
    assert_eq!(small_tree.emit().len(), text.len());
    let emit_of = |t: &SyntaxTree| {
        work::reset();
        let out = t.emit();
        let root = t.root().text().len();
        let took = work::taken();
        assert_eq!(out.len(), root);
        took
    };
    let (mut emit_small, mut emit) = (u64::MAX, u64::MAX);
    for _ in 0..11 {
        emit_small = emit_small.min(emit_of(&small_tree));
        emit = emit.min(emit_of(&large_tree));
    }
    let history = work::ratio("scaling", emit_small, emit);
    eprintln!(
        "emit + root text after {SMALL} edits: {emit_small:?} steps, after {LARGE}: {emit:?} \
         (ratio {history:.2})"
    );
    assert!(
        history < 20.0,
        "emit grew with the edit history: {emit_small:?} after {SMALL} edits, \
         {emit:?} after {LARGE} ({history:.2}×)"
    );
    // The edited tree is the tree of its text.
    assert_eq!(large_tree, SyntaxTree::parse_with(text, &settings));
}

/// (d) The rename shape of `oneyaml`'s
/// `renaming_an_anchor_with_many_aliases_is_linear` at the `-syntax`
/// level: the anchor and every alias token replaced (twice: away and
/// back), 1k aliases against 10k: linear is ~10, the bound is 20 (the
/// same as the document-level test).
#[test]
#[ignore = "slow: runs in CI"]
fn renaming_an_anchor_with_many_aliases_is_linear() {
    let _serial = serial();
    let rename = |t: &mut SyntaxTree| {
        for (anchor, alias) in [("&y", "*y"), ("&x", "*x")] {
            let a = anchor_token(t);
            let _ = t.replace_token(a, anchor).unwrap();
            for id in alias_tokens(t) {
                let _ = t.replace_token(id, alias).unwrap();
            }
        }
    };
    let small = SyntaxTree::parse(aliases(SMALL));
    let large = SyntaxTree::parse(aliases(LARGE));
    let (mut a, mut b) = (u64::MAX, u64::MAX);
    for _ in 0..3 {
        let mut t = small.clone();
        work::reset();
        rename(&mut t);
        a = a.min(work::taken());
        assert_eq!(t.emit(), aliases(SMALL));
        let mut t = large.clone();
        work::reset();
        rename(&mut t);
        b = b.min(work::taken());
        assert_eq!(t.emit(), aliases(LARGE));
    }
    let r = work::ratio("scaling", a, b);
    eprintln!("rename: {SMALL} aliases: {a:?}, {LARGE}: {b:?}, ratio {r:.2} (linear ~10)");
    assert!(r < 20.0, "rename is not linear in the aliases: {r:.2}×");
}
