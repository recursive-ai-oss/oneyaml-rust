//! Interning a tag costs the same however many spellings
//! of its text were seen. Each `%XX` escape pattern of one suffix is a
//! distinct spelling (a distinct `TagKey`) of one text, so a read meets
//! one new spelling per item; before this unit, each was compared against
//! every spelling seen before (quadratic: k = 180,000 took 15 s).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use oneyaml::{Document, Limits, Settings, Tag, Value, from_str_with};

/// 42 characters.
const SUFFIX: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOP";

/// `k` items, each the same tag `!e!` + [`SUFFIX`] spelled with its own
/// `%XX` escape pattern (the bits of the item's number pick which
/// characters are escaped).
fn spellings(k: usize) -> String {
    let mut s = String::from("%TAG !e! tag:x,1:\n---\n");
    for i in 0..k {
        s.push_str("- !e!");
        for (bit, c) in SUFFIX.bytes().enumerate() {
            if (i >> bit) & 1 == 1 {
                write!(s, "%{c:02X}").unwrap();
            } else {
                s.push(char::from(c));
            }
        }
        s.push_str(" v\n");
    }
    s
}

fn best(f: impl Fn()) -> Duration {
    let mut min = Duration::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        f();
        min = min.min(t.elapsed());
    }
    min
}

fn settings() -> Settings {
    // (The large input is 2.6 MB; `untrusted()` allows 1 MiB documents.)
    Settings::new().limits(Limits::untrusted().max_document_size(8 << 20))
}

/// Every spelling reads as the one tag, on every path.
#[test]
fn every_spelling_is_one_tag() {
    let src = spellings(64);
    let want = Tag::new(format!("tag:x,1:{SUFFIX}"));
    let v: Value = from_str_with(&src, &settings()).unwrap();
    let seq = v.as_sequence().unwrap();
    assert_eq!(seq.len(), 64);
    let first = &seq.get(0).unwrap().as_tagged().unwrap().tag;
    for item in seq.iter() {
        let t = &item.as_tagged().unwrap().tag;
        assert_eq!(*t, want);
        // One read: one instance per text.
        assert!(t.same_allocation(first));
    }
    let doc = Document::parse_with(src.as_str(), &settings()).unwrap();
    assert_eq!(doc.to_value().unwrap(), v);
    for n in doc.root().unwrap().as_sequence().unwrap().iter() {
        assert_eq!(n.tag().unwrap().unwrap(), want);
    }
}

/// The known trigger at k and 10k: linear (about 10×), never the
/// quadratic 100×.
#[test]
fn spellings_scale_linearly() {
    let (small, large) = (spellings(3_000), spellings(30_000));
    let s = settings();
    type Run<'a> = Box<dyn Fn(&str) + 'a>;
    let paths: Vec<(&str, Run<'_>)> = vec![
        (
            "Value",
            Box::new(|src: &str| drop(from_str_with::<Value>(src, &s).unwrap())),
        ),
        (
            "IgnoredAny",
            Box::new(|src: &str| {
                from_str_with::<serde::de::IgnoredAny>(src, &s).unwrap();
            }),
        ),
        (
            "Document::parse + to_value",
            Box::new(|src: &str| {
                let d = Document::parse_with(src, &s).unwrap();
                drop(d.to_value().unwrap());
            }),
        ),
        (
            "node tags",
            Box::new(|src: &str| {
                let d = Document::parse_with(src, &s).unwrap();
                for n in d.root().unwrap().as_sequence().unwrap().iter() {
                    assert!(!n.tag().unwrap().unwrap().is_empty());
                }
            }),
        ),
    ];
    for (name, f) in &paths {
        let a = best(|| f(&small));
        let b = best(|| f(&large));
        let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        println!("spellings 3k/30k, {name}: {a:?} vs {b:?} ({r:.2}x)");
        assert!(r < 25.0, "{name}: {r:.2}x");
    }
}
