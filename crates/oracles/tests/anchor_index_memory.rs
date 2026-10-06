//! Building `Document`'s anchor index (on its first
//! use, here `aliases_of` of an anchor without aliases) holds nothing but
//! the lists it makes: bytes allocated, peak and allocation count per
//! anchor or alias, deterministic (allocation sizes do not depend on
//! timing). With per-occurrence allocations, for one name used 200,000 times:
//! 8.79 MB allocated (43.9 bytes per anchor or alias, one allocation per
//! occurrence), a peak of 6.29 MB here (31.5; reallocation included), in
//! 200,023 allocations; with a whole-tree name list
//! and per-name `Vec`s copied into chunks, 11.8 MB net under DHAT. Now
//! 3.41 MB (17.0) and a 3.31 MB peak (16.5) in 1,594 allocations.
//!
//! Measured per thread with `allocation-counter` (an `unsafe impl
//! GlobalAlloc` dev-dependency, so here: the only crate allowed one, as
//! `byte_budget_memory.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]

use oneyaml::{Document, Limits, Settings, path};

/// `k` aliases spread over `names` anchors, and an anchor `none` without
/// aliases.
fn doc(k: usize, names: usize) -> Document {
    let mut s = String::from("none: &none 0\n");
    for i in 0..names {
        s.push_str(&format!("a{i}: &n{i} 1\n"));
    }
    s.push_str("list:\n");
    for i in 0..k {
        s.push_str(&format!("  - *n{}\n", i % names));
    }
    Document::parse_with(s, &Settings::new().limits(Limits::unlimited())).unwrap()
}

/// (bytes allocated, peak bytes, allocations) of building the index.
fn build(d: &Document) -> (u64, u64, u64) {
    let id = d.id_of(&path!["none"]).unwrap();
    let info = allocation_counter::measure(|| {
        assert!(d.aliases_of(id).unwrap().is_empty());
    });
    (info.bytes_total, info.bytes_max, info.count_total)
}

#[test]
fn building_the_anchor_index_holds_only_its_lists() {
    // One name used 200,000 times: chunked lists filled in place.
    let k = 200_000;
    let ids = (k + 2) as f64;
    let (total, peak, count) = build(&doc(k, 1));
    eprintln!("one name: {total} B allocated, peak {peak} B, {count} allocations");
    assert!(
        (total as f64) <= 17.5 * ids + 16_384.0,
        "{:.2} B allocated per anchor/alias",
        total as f64 / ids
    );
    assert!(
        (peak as f64) <= 17.0 * ids + 16_384.0,
        "{:.2} B peak per anchor/alias",
        peak as f64 / ids
    );
    assert!(count <= (k / 64) as u64, "{count} allocations");
    // 1,000 names used 100 times each: `Vec`s, one key per name.
    let (k, names) = (100_000, 1_000);
    let ids = (k + names + 1) as f64;
    let (total, peak, count) = build(&doc(k, names));
    eprintln!("1,000 names: {total} B allocated, peak {peak} B, {count} allocations");
    assert!(
        (total as f64) <= 44.0 * ids,
        "{:.2} B allocated per anchor/alias",
        total as f64 / ids
    );
    assert!(
        (peak as f64) <= 23.5 * ids,
        "{:.2} B peak per anchor/alias",
        peak as f64 / ids
    );
    assert!(count <= 12 * names as u64, "{count} allocations");
}
