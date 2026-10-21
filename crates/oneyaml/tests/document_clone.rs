//! `Document::clone` over `SyntaxTree::fork`:
//! cloning never makes handles stale on either copy, an edit
//! never invalidates handles to untouched nodes, and nothing grows with
//! the number of clone + edit cycles (no per-copy handle map).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod document_common;

mod work;

use std::fmt::Write as _;

use document_common::agrees_with_its_text;
use oneyaml::{Document, EditError, NodeId, Stream, path};

fn mapping(entries: usize) -> String {
    let mut s = String::from("target:\n  a: 1\n");
    for i in 0..entries {
        writeln!(s, "k{i}: {i} # c").unwrap();
    }
    s
}

/// The anchor index built before the copy is the copy's too (its handles
/// are the fork's): both copies answer from it, and edits to one leave
/// the other's answers unchanged.
#[test]
fn anchor_index_survives_the_copy() {
    let mut d = Document::parse("a: &x 1\nb: *x\nc: [*x]\n").unwrap();
    let a = d.id_of(&path!["a"]).unwrap();
    let before = d.aliases_of(a).unwrap();
    assert_eq!(before.len(), 2);
    let mut c = d.clone();
    c.set_anchor(a, Some("y")).unwrap();
    let pushed = c.get_mut("c").unwrap().push_alias(a).unwrap().id();
    assert_eq!(c.to_string(), "a: &y 1\nb: *y\nc: [*y, *y]\n");
    assert_eq!(c.aliases_of(a).unwrap().len(), 3);
    assert!(c.aliases_of(a).unwrap().contains(&pushed));
    assert_eq!(d.aliases_of(a).unwrap(), before);
    agrees_with_its_text(&c);
    // The original edits its own tree (no longer shared) with the index
    // it had: unchanged by the copy's edits.
    d.set_anchor(a, Some("z")).unwrap();
    assert_eq!(d.to_string(), "a: &z 1\nb: *z\nc: [*z]\n");
    assert_eq!(d.node(pushed).unwrap_err(), EditError::StaleNode);
    agrees_with_its_text(&d);
}

/// Restricted mode found before the copy protects the copy's error
/// regions too (the guard names nodes by handle; the fork keeps them).
#[test]
fn restricted_mode_survives_the_copy() {
    let mut d = Document::parse_recovering("good:\n  x: 1\nbad: @x\n").into_value();
    d.set_path(&path!["good", "x"], 2).unwrap();
    let keep = d.clone();
    // `d` shares its tree with `keep`: this edit forks it.
    let e = d.set_path(&path!["bad", "k"], 1).unwrap_err();
    assert!(matches!(e, EditError::ErrorNode { .. }), "{e:?}");
    d.set_path(&path!["good", "x"], 3).unwrap();
    assert_eq!(d.to_string(), "good:\n  x: 3\nbad: @x\n");
    assert_eq!(keep.to_string(), "good:\n  x: 2\nbad: @x\n");
    assert!(d.has_errors() && keep.has_errors());
    agrees_with_its_text(&d);
    agrees_with_its_text(&keep);
}

/// A snapshot of a stream document and the stream itself share handles
/// through forks on both sides, and a handle one of them issues after the
/// copy is stale in the other (never a wrong node).
#[test]
fn stream_snapshots_fork_both_ways() {
    let mut s = Stream::parse("a: 1\n---\nb: 2\nc: [3]\n").unwrap();
    let mut snap = s.document(1).unwrap();
    let c = snap.id_of(&path!["c"]).unwrap();
    let pushed = snap.get_mut("c").unwrap().push(4).unwrap().id();
    let mut live = s.document_mut(1).unwrap();
    assert_eq!(live.node(pushed).unwrap_err(), EditError::StaleNode);
    let other = live.get_mut("c").unwrap().push(5).unwrap().id();
    assert_eq!(live.node(c).unwrap().text(), "[3, 5]");
    assert_eq!(snap.node(other).unwrap_err(), EditError::StaleNode);
    assert_eq!(snap.node(c).unwrap().text(), "[3, 4]");
    assert_eq!(snap.node(pushed).unwrap().text(), "4");
    assert_eq!(s.to_string(), "a: 1\n---\nb: 2\nc: [3, 5]\n");
}

/// Resident set size in KiB (Linux; `None` elsewhere).
fn rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The fewest steps of five rounds of 200 `node()` lookups of `ids`.
fn lookup_time(doc: &Document, ids: &[NodeId]) -> u64 {
    let mut best = u64::MAX;
    for _ in 0..5 {
        work::reset();
        for _ in 0..200 {
            for id in ids {
                std::hint::black_box(doc.node(*id).unwrap());
            }
        }
        best = best.min(work::taken());
    }
    best
}

/// A known shape: 500 clone + edit cycles on a
/// 20k-entry mapping. Each generation is a clone of the previous one,
/// edited (so its tree is forked from a tree with insertions: the worst
/// case for the fork's ancestry), and the previous generation is dropped.
/// `node()` latency stays flat (the old composed maps were one binary
/// search, but grew by 32 bytes per node per copy while alive), and the
/// resident set does not grow beyond the two copies alive at a time.
#[test]
fn five_hundred_clone_edit_cycles_stay_flat() {
    const CYCLES: usize = 500;
    let mut doc = Document::parse(mapping(20_000)).unwrap();
    let ids: Vec<NodeId> = [0, 5_000, 10_000, 19_999]
        .iter()
        .map(|i| doc.id_of(&path![format!("k{i}")]).unwrap())
        .collect();
    let target = doc.id_of(&path!["target"]).unwrap();
    let mut a = doc.id_of(&path!["target", "a"]).unwrap();
    let mut early = (0u64, None);
    for cycle in 0..CYCLES {
        let mut next = doc.clone();
        // Replaces one value and inserts then removes an entry (insertions
        // into the tree each cycle), next to the 20k entries, through
        // handles (a key lookup reads its mapping's entries, O(width):
        // not what this measures).
        a = next.node_mut(a).unwrap().set(cycle).unwrap().id();
        let mut t = next.node_mut(target).unwrap();
        t.insert("extra", cycle).unwrap();
        let _ = t.remove("extra").unwrap();
        // Handles taken before any copy resolve in both generations.
        assert!(doc.node(target).is_ok() && next.node(target).is_ok());
        for id in &ids {
            assert!(doc.node(*id).is_ok() && next.node(*id).is_ok());
        }
        doc = next;
        if cycle == 49 {
            early = (lookup_time(&doc, &ids), rss_kib());
        }
    }
    let late = (lookup_time(&doc, &ids), rss_kib());
    let r = work::ratio("node()", early.0, late.0);
    eprintln!(
        "node(): {:?} steps after 50 cycles vs {:?} after {CYCLES} (ratio {r:.2}); RSS {:?} vs {:?} KiB",
        early.0, late.0, early.1, late.1
    );
    assert!(r < 3.0, "node() cost grows with clone cycles: {r:.2}");
    if let (Some(a), Some(b)) = (early.1, late.1) {
        // Two copies are alive at a time at both points; allow half of the
        // early resident set for allocator slack. A per-copy handle map
        // composed over the lineage (the old `handles.rs`) grows by one
        // entry per item node per cycle; 450 cycles of it are far past
        // this.
        let one_copy = a / 2;
        assert!(
            b <= a + one_copy,
            "RSS grows with clone cycles: {a} KiB after 50, {b} KiB after {CYCLES}"
        );
    }
    assert_eq!(doc.to_value().unwrap()["k10000"].as_u64(), Some(10_000));
    assert_eq!(
        doc.to_string(),
        mapping(20_000).replacen("a: 1", &format!("a: {}", CYCLES - 1), 1)
    );
}
