//! The peak memory of reading each security-review trigger (alias expansion and
//! `%TAG` prefixes whose logical size is ≫ the input) is bounded by the input
//! plus the alias byte budget, at 1× and 10× the input, under
//! `Limits::default()` and `Limits::untrusted()`: nothing over the budget is
//! ever built. Without the byte budget, `untrusted()` peaked at 9.8 GB for a
//! 497 KB input.
//!
//! Measured per thread with `allocation-counter` (an `unsafe impl
//! GlobalAlloc` dev-dependency, so here: the only crate allowed one, as
//! `ser_allocations.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Document, Limits, Settings, Value, code};

fn alias_of_large_scalar(len: usize, uses: usize) -> String {
    let mut s = format!("a: &a {}\nb: [", "x".repeat(len));
    s.push_str(&vec!["*a"; uses].join(", "));
    s.push_str("]\n");
    s
}

fn alias_keyed_chains(len: usize, items: usize) -> String {
    let mut s = format!("k: &k {}\nm:\n", "x".repeat(len));
    for _ in 0..items {
        s.push_str("- {*k : {*k : {*k : 1}}}\n");
    }
    s
}

fn merge_chain(len: usize, levels: usize, width: usize) -> String {
    let mut s = format!("l0: &l0 {{k: {}}}\n", "x".repeat(len));
    for i in 1..=levels {
        let refs = vec![format!("*l{}", i - 1); width].join(", ");
        s.push_str(&format!("l{i}: &l{i} {{<<: [{refs}]}}\n"));
    }
    s
}

fn tag_prefix(len: usize, uses: usize) -> String {
    let mut s = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(len));
    for _ in 0..uses {
        s.push_str("- !e!x v\n");
    }
    s
}

/// Peak bytes held while `f` runs on this thread.
fn peak(f: impl FnOnce()) -> u64 {
    allocation_counter::measure(f).bytes_max
}

#[test]
fn triggers_peak_within_input_plus_budget() {
    for scale in [1usize, 10] {
        let len = 100_000 * scale;
        for (name, src, merge) in [
            (
                "alias of a large scalar",
                alias_of_large_scalar(len, 1000),
                false,
            ),
            ("alias-keyed chains", alias_keyed_chains(len, 400), false),
            ("merge chain", merge_chain(len, 3, 10), true),
            ("%TAG prefix x uses", tag_prefix(len, 1000), false),
        ] {
            for (profile, limits) in [
                ("default", Limits::default()),
                ("untrusted", Limits::untrusted()),
            ] {
                let budget = limits.get_alias_byte_budget(src.len()).unwrap();
                let s = Settings::new().limits(limits).merge_keys(merge);
                let value = peak(|| {
                    let e = oneyaml::from_str_with::<Value>(&src, &s).unwrap_err();
                    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
                });
                let doc = Document::parse_with(src.as_str(), &s).unwrap();
                let tree = peak(|| {
                    let e = doc.to_value().unwrap_err();
                    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
                });
                // The values built before the refusal (≤ the budget) plus
                // O(input) of text, tape, and events (measured: at most
                // budget + 1.4 × input).
                let budget = budget as u64;
                let bound = budget + budget / 8 + 4 * (src.len() as u64) + (1 << 20);
                println!(
                    "{name} {scale}x {profile}: input {} B, budget {budget} B, \
                     peak from_str {value} B, to_value {tree} B",
                    src.len()
                );
                assert!(
                    value <= bound,
                    "{name} {scale}x {profile}: {value} > {bound}"
                );
                assert!(tree <= bound, "{name} {scale}x {profile}: {tree} > {bound}");
            }
        }
    }
}
