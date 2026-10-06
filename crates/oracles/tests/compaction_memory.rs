//! Memory compaction of the
//! linear residuals a security review accepted, measured under
//! `Limits::untrusted()` at 1× and 10×:
//!
//! 1. strict `Document::from_slice` on invalid bytes stops at the first
//!    invalid run (as `from_slice` does for serde);
//! 2. invalid decode runs are stored compactly (offsets and one pool of their
//!    bytes, no per-run allocation; the input still round-trips byte for byte);
//! 3. directives waiting for their `---` are written into the arena as
//!    they come (no buffered copy);
//! 4. a diagnostic in the tree's list takes 32 bytes (a skeleton 24);
//! 5. the lookup index of a wide collection holds 4-byte slots.
//!
//! Each bound below is under the review's measurement (in the comments),
//! so it fails at the old constants. Measured per thread with
//! `allocation-counter` (an `unsafe impl GlobalAlloc` dev-dependency, so
//! here: the only crate allowed one, as `ser_allocations.rs`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Document, Limits, Settings, path};

fn peak(f: impl FnOnce()) -> u64 {
    allocation_counter::measure(f).bytes_max
}

fn report(what: &str, input: usize, bytes: u64, max_ratio: u64) {
    let ratio = bytes as f64 / input as f64;
    eprintln!(
        "{what}: {} KB input, {} KB, {ratio:.1}x (bound {max_ratio}x)",
        input / 1000,
        bytes / 1000
    );
    assert!(
        bytes <= max_ratio * input as u64,
        "{what}: {bytes} bytes for {input} input bytes ({ratio:.1}x) over {max_ratio}x"
    );
}

fn untrusted() -> Settings {
    Settings::new().limits(Limits::untrusted())
}

#[test]
fn invalid_bytes() {
    let s = untrusted();
    for scale in [1usize, 10] {
        let bytes = b"\x80a".repeat(210_000 * scale);
        // 1. The review measured ~63x: every run was kept to return `Err`.
        report(
            &format!("{scale}x invalid bytes Document::from_slice"),
            bytes.len(),
            peak(|| {
                let e = Document::from_slice_with(&bytes, &s).unwrap_err();
                assert_eq!(e.code(), oneyaml::code::INVALID_UTF8);
            }),
            2,
        );
        report(
            &format!("{scale}x invalid bytes Stream::from_slice"),
            bytes.len(),
            peak(|| {
                assert!(oneyaml::Stream::from_slice_with(&bytes, &s).is_err());
            }),
            2,
        );
        // 2. The review measured ~86x: each run kept its own bytes (now
        // ~17-21x: 12 bytes a run, the text, and the tree).
        report(
            &format!("{scale}x invalid bytes Document::from_slice_recovering"),
            bytes.len(),
            peak(|| {
                let p = Document::from_slice_recovering(&bytes, &s);
                assert!(p.has_errors());
            }),
            32,
        );
    }
}

#[test]
fn orphan_and_dense_directives() {
    let s = untrusted();
    for scale in [1usize, 10] {
        let n = 30_000 * scale;
        // 3. The review measured 29-46x for orphan directives (now ~11-12x).
        let src = "%YAML 1.2\n...\n".repeat(n);
        report(
            &format!("{scale}x orphans Document::parse_recovering"),
            src.len(),
            peak(|| {
                let p = Document::parse_recovering_with(src.as_str(), &s);
                assert!(p.has_errors());
            }),
            16,
        );
        report(
            &format!("{scale}x orphans Stream::parse_recovering"),
            src.len(),
            peak(|| {
                let _ = oneyaml::Stream::parse_recovering_with(src.as_str(), &s);
            }),
            16,
        );
        // 4. The review measured 54-102x for dense warnings (`%A`, now
        // ~32-37x) and 29-35x for longer lines (now ~11-13x).
        for (line, bound) in [("%A\n", 48), ("%FOO bar\n", 20)] {
            let src = format!("{}--- a\n", line.repeat(3 * n));
            report(
                &format!("{scale}x {line:?} Document::parse_recovering"),
                src.len(),
                peak(|| {
                    let p = Document::parse_recovering_with(src.as_str(), &s);
                    assert!(!p.has_errors());
                }),
                bound,
            );
        }
    }
}

fn flat_mapping(n: usize) -> String {
    let mut s = String::with_capacity(n * 16);
    for i in 0..n {
        s.push_str(&format!("key{i:06}: v{i}\n"));
    }
    s
}

fn flat_sequence(n: usize) -> String {
    let mut s = String::with_capacity(n * 10);
    for i in 0..n {
        s.push_str(&format!("- i{i:06}\n"));
    }
    s
}

/// Item 5: a looked-up 100k flat mapping (and sequence) stays within the 8×
/// retained budget: the tree plus its lookup index. Uncompacted:
/// 12.9x for the mapping, 8.7x for the sequence (now ~7.3x and ~7.1x).
#[test]
fn looked_up_wide_collections_stay_within_the_retained_budget() {
    let s = untrusted();
    for (what, src, probe) in [
        ("mapping", flat_mapping(100_000), 0),
        ("sequence", flat_sequence(100_000), 1),
    ] {
        let info = allocation_counter::measure(|| {
            let doc = Document::parse_with(src.as_str(), &s).unwrap();
            if probe == 0 {
                let v = doc.get_path(&path!["key050000"]).unwrap();
                assert_eq!(v.text(), "v50000");
            } else {
                let v = doc.get_path(&path![50_000]).unwrap();
                assert_eq!(v.text(), "i050000");
            }
            std::mem::forget(doc);
        });
        report(
            &format!("looked-up 100k flat {what}: retained"),
            src.len(),
            info.bytes_current.try_into().unwrap(),
            8,
        );
    }
    // The index alone: the same document with and without a lookup.
    let src = flat_mapping(100_000);
    let plain = allocation_counter::measure(|| {
        std::mem::forget(Document::parse_with(src.as_str(), &s).unwrap());
    });
    let looked = allocation_counter::measure(|| {
        let doc = Document::parse_with(src.as_str(), &s).unwrap();
        assert!(doc.get_path(&path!["key000001"]).is_some());
        std::mem::forget(doc);
    });
    let index = looked.bytes_current - plain.bytes_current;
    let per_entry = index as f64 / 100_000.0;
    eprintln!("lookup index: {index} bytes, {per_entry:.1} per entry");
    // Two 16-byte treap nodes plus a ~17-byte key entry, ~49 per entry
    // (before: two 32-byte nodes and the key's entry, ~149 per entry).
    assert!(per_entry <= 56.0, "{per_entry:.1} bytes per entry");
}

/// Allocations of `f` (after a warm-up run): count and bytes.
fn allocations(f: impl Fn()) -> (u64, u64) {
    f();
    let info = allocation_counter::measure(f);
    (info.count_total, info.bytes_total)
}

/// Allocation gate: a strict `from_slice` of valid input
/// decodes it once. In UTF-8 it allocates exactly what `parse` of the same
/// text does (the text is borrowed, then owned once); in UTF-16 at most
/// the decoded text's buffer more (sized from the input: decoding
/// reserves its length). An earlier first pass decoded UTF-16
/// input into a throwaway `String` (one more allocation of that size),
/// which this gate fails.
#[test]
fn strict_from_slice_decodes_once() {
    let s = Settings::new();
    let text = flat_mapping(2_000);
    let utf16: Vec<u8> = [0xff, 0xfe]
        .into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let with_bom = format!("\u{feff}{text}");
    let parse = |t: &str| {
        let t = t.to_owned();
        let s = s.clone();
        move || {
            std::hint::black_box(Document::parse_with(t.as_str(), &s).unwrap());
        }
    };
    let (p16c, p16b) = allocations(parse(&with_bom));
    let (dc, db) = allocations(parse(&text));
    let (sc, sb) = allocations(|| {
        std::hint::black_box(oneyaml::Stream::parse_with(text.as_str(), &s).unwrap());
    });
    for (what, (pc, pb), (c, b)) in [
        (
            "Document",
            (dc, db),
            allocations(|| {
                std::hint::black_box(Document::from_slice_with(text.as_bytes(), &s).unwrap());
            }),
        ),
        (
            "Stream",
            (sc, sb),
            allocations(|| {
                std::hint::black_box(
                    oneyaml::Stream::from_slice_with(text.as_bytes(), &s).unwrap(),
                );
            }),
        ),
    ] {
        eprintln!("UTF-8 {what}::from_slice: {c} allocations, {b} bytes (parse: {pc}, {pb})");
        assert!(
            c <= pc && b <= pb,
            "UTF-8 {what}: {c}/{b} vs parse {pc}/{pb}"
        );
    }
    let (c, b) = allocations(|| {
        std::hint::black_box(Document::from_slice_with(&utf16, &s).unwrap());
    });
    eprintln!("UTF-16 Document::from_slice: {c} allocations, {b} bytes (parse: {p16c}, {p16b})");
    assert!(c <= p16c, "UTF-16: {c} allocations vs parse {p16c}");
    assert!(
        b <= p16b + utf16.len() as u64,
        "UTF-16: {b} bytes vs parse {p16b} + {}",
        utf16.len()
    );
}
