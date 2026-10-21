//! Strict `Document::parse` builds an anchored
//! collection's value only when an alias of it stands as a key (or inside
//! one), so an anchored section that no alias key uses costs what the same
//! section without the anchor costs (the review measured 90x against 14x
//! when every anchored collection's value was built). Measured per thread
//! with `allocation-counter` (an `unsafe impl GlobalAlloc` dev-dependency,
//! so here: the only crate allowed one, as `ser_allocations.rs`).
//!
//! Nor does an anchor no alias key uses cost anything per
//! anchor. Documents with many distinct anchor names peak where they did
//! before the alias-key replay existed ; the replay bookkeeping
//! (a replay handle and a definition history entry per name, ~225 B)
//! raised them by 40-70%.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss
)]

use std::fmt::Write as _;

use oneyaml::Document;

fn peak(f: impl FnOnce()) -> u64 {
    allocation_counter::measure(f).bytes_max
}

fn section(anchor: &str, entries: usize) -> String {
    let mut s = format!("root: {anchor}\n");
    for i in 0..entries {
        writeln!(s, "  k{i}: [v{i}, {{a: {i}}}]").unwrap();
    }
    s.push_str("other: *r\n");
    s
}

#[test]
fn an_anchored_section_without_alias_keys_costs_no_more() {
    for entries in [10_000usize, 100_000] {
        let plain = section("&r", entries).replace("other: *r\n", "other: 1\n");
        let plain = plain.replacen("root: &r\n", "root:\n", 1);
        let anchored = section("&r", entries);
        let base = peak(|| {
            Document::parse(plain.as_str()).unwrap();
        });
        let with = peak(|| {
            Document::parse(anchored.as_str()).unwrap();
        });
        let ratio = with as f64 / base as f64;
        eprintln!(
            "{entries} entries: {} KB input, {} KB plain, {} KB anchored \
             ({ratio:.2}x, {:.1}x input)",
            anchored.len() / 1000,
            base / 1000,
            with / 1000,
            with as f64 / anchored.len() as f64
        );
        // The anchor and one alias add a few bytes, never a copy of the
        // section (which would be several times its size).
        assert!(
            with <= base + base / 20 + 4096,
            "{entries}: {with} bytes anchored vs {base} plain"
        );
        assert!(with <= 20 * anchored.len() as u64, "{with}");
    }
}

/// `- &a{i} v{i}`: a distinct scalar anchor per item.
fn scalars(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "- &a{i} v{i}").unwrap();
    }
    s
}

/// `- &a{i} [v{i}]`: a distinct flow-collection anchor per item.
fn flows(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "- &a{i} [v{i}]").unwrap();
    }
    s
}

/// Anchors nested in anchored collections, three distinct names per item.
fn nested(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "- &a{i}\n  k: &b{i} {{x: &c{i} v{i}}}").unwrap();
    }
    s
}

/// One name redefined per item (a reference point: one name, so the replay
/// bookkeeping cost little here).
fn redefined(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "- &a v{i}").unwrap();
    }
    s
}

/// Every name defined twice: `- &a{i} v{i}` then `- &a{i} w{i}` (or flow
/// collections), so each keeps its previous definition.
fn pairs(n: usize, flow: bool) -> String {
    let mut s = String::new();
    for i in 0..n {
        if flow {
            writeln!(s, "- &a{i} [v{i}]\n- &a{i} [w{i}]").unwrap();
        } else {
            writeln!(s, "- &a{i} v{i}\n- &a{i} w{i}").unwrap();
        }
    }
    s
}

/// Peak bytes of a strict parse of `src`, at most `max_tenths` tenths of
/// its size.
fn bounded(what: &str, src: &str, max_tenths: u64) {
    let bytes = peak(|| {
        Document::parse(src).unwrap();
    });
    let input = src.len() as u64;
    eprintln!(
        "{what}: {} KB input, {} KB peak, {:.2}x input (bound {:.1}x)",
        input / 1000,
        bytes / 1000,
        bytes as f64 / input as f64,
        max_tenths as f64 / 10.0
    );
    assert!(
        bytes * 10 <= max_tenths * input,
        "{what}: {bytes} bytes peak for {input} bytes of input"
    );
}

#[test]
#[ignore = "slow: runs in CI"]
fn many_distinct_anchors_without_alias_keys_cost_what_they_did() {
    // Bounds: the peak before alias-key replays (measured with these inputs:
    // 22.97x, 28.78x, 36.24x, 13.18x) plus 5%, rounded. An earlier tree peaked
    // at 39.15x, 43.35x, 57.06x (and 13.18x).
    bounded("300k scalar anchors", &scalars(300_000), 241);
    bounded("300k flow anchors", &flows(300_000), 302);
    bounded("100k nested anchors", &nested(100_000), 381);
    bounded("300k redefinitions", &redefined(300_000), 139);
    // Every name defined twice (before: 14.25x, 17.72x; with
    // definition histories: 15.62x and +7%, a history entry per name).
    bounded("150k scalar pairs", &pairs(150_000, false), 150);
    bounded("150k flow pairs", &pairs(150_000, true), 187);
}

/// A laughs chain (`a6` expands to ~1.1e7 nodes), then `k` distinct
/// anchors over it, each used once as an alias key.
fn replay_bomb(k: usize) -> String {
    let mut s = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
    for n in 1..=6 {
        let p = n - 1;
        let items = vec![format!("*a{p}"); 10].join(", ");
        writeln!(s, "a{n}: &a{n} [{items}]").unwrap();
    }
    for i in 0..k {
        writeln!(s, "b{i}: &b{i} [*a6 ]").unwrap();
    }
    s.push_str("m:\n");
    for i in 0..k {
        writeln!(s, "- {{? *b{i} : 1}}").unwrap();
    }
    s
}

/// As [`replay_bomb`], each anchor a mapping merged into a complex key
/// (`? {<<: *b0}`: the merged entries are its identity).
fn merge_bomb(k: usize) -> String {
    replay_bomb(k)
        .lines()
        .map(|l| {
            if let Some(rest) = l.strip_prefix("- {? *b") {
                format!("- {{? {{<<: *b{}\n", rest.replacen(" : 1}", "} : 1}", 1))
            } else if l.starts_with('b') {
                format!("{}\n", l.replace("[*a6 ]", "{k: *a6}"))
            } else {
                format!("{l}\n")
            }
        })
        .collect()
}

/// As [`replay_bomb`], but each anchor fits the node budget and passes
/// the byte budget (big scalars), so its replay is refused part-way.
fn byte_bomb(k: usize) -> String {
    let mut s = format!("a0: &a0 {}\n", "x".repeat(10_000));
    for n in 1..=5 {
        let p = n - 1;
        let items = vec![format!("*a{p}"); 10].join(", ");
        writeln!(s, "a{n}: &a{n} [{items}]").unwrap();
    }
    for i in 0..k {
        writeln!(s, "b{i}: &b{i} [*a5 ]").unwrap();
    }
    s.push_str("m:\n");
    for i in 0..k {
        writeln!(s, "- {{? *b{i} : 1}}").unwrap();
    }
    s
}

/// One alias key of an anchor of 100 MB of scalar bytes in 202 nodes (a
/// 500 KB scalar, aliased 200 times): within the node budget, far past
/// the byte budget.
fn wide_byte_bomb() -> String {
    let items = vec!["*a0"; 200].join(", ");
    format!(
        "a0: &a0 {}\na1: &a1 [{items}]\nb: &b [*a1 ]\nm:\n- {{? *b : 1}}\n",
        "x".repeat(500_000)
    )
}

/// The alias-key replays of one strict check read
/// at most the copy budget in all, however many distinct anchors are used
/// as keys past it (it was k × budget: 58 s for k = 300). The work is
/// measured as allocations against the same input without alias keys.
#[test]
fn distinct_over_budget_alias_keys_cost_one_budget() {
    use oneyaml::{Limits, Settings, code};
    let untrusted = Settings::new().limits(Limits::untrusted());
    for (what, src, s) in [
        ("nodes, default", replay_bomb(300), Settings::new()),
        ("nodes, untrusted", replay_bomb(1000), untrusted.clone()),
        ("bytes, default", byte_bomb(300), Settings::new()),
        ("bytes, untrusted", byte_bomb(1000), untrusted.clone()),
        ("bytes 500 KB, default", wide_byte_bomb(), Settings::new()),
        (
            "merged, default",
            merge_bomb(300),
            Settings::new().merge_keys(true),
        ),
        (
            "merged, untrusted",
            merge_bomb(1000),
            untrusted.merge_keys(true),
        ),
    ] {
        let plain = src.replace("{? *b", "{? b").replace("{<<: *b", "{x: b");
        let base = allocation_counter::measure(|| {
            Document::parse_with(plain.as_str(), &s).unwrap();
        });
        let with = allocation_counter::measure(|| {
            Document::parse_with(src.as_str(), &s).unwrap();
        });
        // Where the `Value` read refuses at its alias budget.
        let e = oneyaml::from_str_with::<oneyaml::Value>(&src, &s).unwrap_err();
        assert!(
            [
                code::ALIAS_BUDGET_EXCEEDED,
                code::ALIAS_BYTE_BUDGET_EXCEEDED
            ]
            .contains(&e.code()),
            "{what}: {e}"
        );
        eprintln!(
            "{what}: {} B input, {} allocations ({} without alias keys), \
             {} KB allocated ({} KB), {} KB peak ({} KB)",
            src.len(),
            with.count_total,
            base.count_total,
            with.bytes_total / 1000,
            base.bytes_total / 1000,
            with.bytes_max / 1000,
            base.bytes_max / 1000,
        );
        // One budget of replay at most: 1M nodes (default), 100k
        // (untrusted), each an allocation or two; bytes at most the byte
        // budget (64 MiB, 8 MiB) copied once.
        let nodes = if what.ends_with("default") {
            1_000_000
        } else {
            100_000
        };
        let bytes = if what.ends_with("default") {
            64 << 20
        } else {
            8 << 20
        };
        assert!(
            with.count_total <= base.count_total + 3 * nodes,
            "{what}: {} allocations vs {}",
            with.count_total,
            base.count_total
        );
        assert!(
            with.bytes_total <= base.bytes_total + 3 * bytes + 200 * nodes,
            "{what}: {} bytes vs {}",
            with.bytes_total,
            base.bytes_total
        );
        // An alias key over the byte budget copies nothing (at
        // first, the 20 KB byte bomb peaked at 68 MB against 282 KB
        // without alias keys, and copied 74 MB in all); replays refused at
        // the node budget are not kept either.
        assert!(
            with.bytes_max <= base.bytes_max + base.bytes_max / 10 + (64 << 10),
            "{what}: {} bytes peak vs {}",
            with.bytes_max,
            base.bytes_max
        );
        if what.starts_with("bytes") {
            assert!(
                with.bytes_total <= base.bytes_total + base.bytes_total / 2 + (1 << 20),
                "{what}: {} bytes vs {}",
                with.bytes_total,
                base.bytes_total
            );
        }
    }
}
