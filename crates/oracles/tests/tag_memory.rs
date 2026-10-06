//! The memory a read uses for tags never grows
//! with the number of uses, and a distinct tag costs its suffix, not its
//! `%TAG` prefix. Peak heap bytes (`allocation-counter`, a dev dependency
//! whose counting allocator lives in this crate only) for the
//! known triggers: distinct tags of a 1 MB `%TAG` prefix, rotated,
//! under `Limits::untrusted()` (once 1.58 GB for 20 distinct
//! tags × 8,000 uses, 4.7 GB for 40).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Document, Limits, Settings, Stream, Value};

const MB: u64 = 1 << 20;

fn rotating(distinct: usize, uses: usize) -> String {
    let mut s = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(1_000_000));
    for i in 0..uses {
        s.push_str(&format!("- !e!x{} v\n", i % distinct));
    }
    s
}

fn peak(f: impl FnOnce()) -> u64 {
    allocation_counter::measure(f).bytes_max
}

/// Every path holds one decoded copy of the prefix plus what the
/// input's size costs anyway, whatever the number of distinct tags (16 to
/// 100) and uses (8,000 to 160,000). O(distinct × prefix) would be 16 MB
/// to 100 MB more; O(uses × prefix) gigabytes.
///
/// These values repeat the 1 MB prefix per use (8 GB of logical
/// text and more), over `untrusted()`'s alias byte budget, so the preset
/// now refuses the reads that build them (`Y2009`, the expected new
/// refusal, at the use that goes over: see `byte_budget_memory.rs`); the
/// memory measured here is of the reads with the byte budget lifted.
#[test]
fn rotating_tags_peak_memory_is_bounded() {
    let preset = Settings::new().limits(Limits::untrusted());
    let refused = oneyaml::from_str_with::<Value>(&rotating(16, 8000), &preset).unwrap_err();
    assert_eq!(refused.code(), oneyaml::code::ALIAS_BYTE_BUDGET_EXCEEDED);
    let s = Settings::new().limits(Limits::untrusted().alias_byte_budget(usize::MAX));
    for (distinct, uses) in [
        (16, 8000),
        (20, 8000),
        (40, 8000),
        (100, 16_000),
        (40, 160_000),
    ] {
        let src = rotating(distinct, uses);
        let input = src.len() as u64;
        let ignored = peak(|| {
            oneyaml::from_str_with::<serde::de::IgnoredAny>(&src, &s).unwrap();
        });
        let value = peak(|| {
            drop(oneyaml::from_str_with::<Value>(&src, &s).unwrap());
        });
        let document = peak(|| {
            drop(Document::parse_with(src.as_str(), &s).unwrap());
        });
        let stream = peak(|| {
            drop(Stream::parse_with(src.as_str(), &s).unwrap());
        });
        let doc = Document::parse_with(src.as_str(), &s).unwrap();
        let to_value = peak(|| drop(doc.to_value().unwrap()));
        let walk = peak(|| {
            for i in doc.root().unwrap().as_sequence().unwrap().iter() {
                assert!(!i.tag().unwrap().unwrap().is_empty());
                let _ = i.as_scalar().unwrap().resolved().unwrap();
            }
        });
        println!(
            "{distinct} x {uses} (input {} KB): IgnoredAny {} KB, Value {} KB, Document {} KB, \
             Stream {} KB, to_value {} KB, tag walk {} KB",
            input >> 10,
            ignored >> 10,
            value >> 10,
            document >> 10,
            stream >> 10,
            to_value >> 10,
            walk >> 10,
        );
        // A read: a copy of the prefix (1 MB) and the parser's buffers.
        assert!(
            ignored < 3 * MB,
            "{distinct} x {uses}: IgnoredAny {ignored}"
        );
        // Values, trees: a few hundred bytes per item beside the prefix.
        let per_item = 400 * uses as u64;
        for (name, bytes) in [
            ("Value", value),
            ("Document", document),
            ("Stream", stream),
            ("to_value", to_value),
        ] {
            assert!(
                bytes < 3 * MB + 3 * input + per_item,
                "{distinct} x {uses}: {name} {bytes}"
            );
        }
        // The walk: the table (built on the first read) and nothing per use.
        assert!(
            walk < 2 * MB + per_item / 4,
            "{distinct} x {uses}: walk {walk}"
        );
    }
}

/// A `Document`'s node reads: nothing retained per read.
#[test]
fn node_reads_peak_memory_is_bounded() {
    let src = rotating(20, 4000);
    let doc = Document::parse(src.as_str()).unwrap();
    let items: Vec<_> = doc.root().unwrap().as_sequence().unwrap().iter().collect();
    let tags = peak(|| {
        for _ in 0..3 {
            for i in &items {
                assert!(i.tag().unwrap().is_some());
                let _ = i.as_scalar().unwrap().resolved().unwrap();
            }
        }
    });
    let reads = peak(|| {
        let values: Vec<Value> = items.iter().map(|i| i.deserialize().unwrap()).collect();
        assert_eq!(values.len(), 4000);
    });
    println!("tags {} KB, reads {} KB", tags >> 10, reads >> 10);
    assert!(tags < 2 * MB, "tags {tags}");
    assert!(reads < 8 * MB, "reads {reads}");
}
