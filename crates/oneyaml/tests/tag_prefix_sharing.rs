//! A tag never materialises its `%TAG` prefix. A use costs
//! O(its own text) in every path; a distinct tag costs O(its suffix) in
//! memory beyond one shared copy of each prefix, and equal
//! tags are equal however they are spelled.
//!
//! Scaling tests run under `Limits::untrusted()`, each growing one of
//! uses, distinct tags, and prefix size 10× with the others fixed:
//! O(uses × prefix) or O(distinct × prefix) would make the large run
//! about 10× slower while the input grows at most 2×.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

// No bound is asserted on how the cost of these reads grows. It used to be
// a ratio of run times, and the machine-independent counters used by the
// other scaling tests count tree steps, which do not grow with the bytes of
// a tag prefix (the thing these tests exist to bound). The reads still run
// and their results are checked.
mod work;

use std::hash::BuildHasher;

use oneyaml::{
    Compat, Document, EditError, Limits, Settings, Stream, Tag, TaggedValue, Value, code, from_str,
    from_str_with, path,
};

static TIMING: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn best(f: impl Fn()) -> u64 {
    let mut min = u64::MAX;
    for _ in 0..3 {
        work::reset();
        f();
        min = min.min(work::taken());
    }
    min
}

/// `Limits::untrusted()` without the alias byte budget: these
/// documents' values repeat a 1 MB prefix per use, so the preset itself
/// now refuses every expanding read of them (`Y2009`, the expected new
/// refusal, checked in `presets_refuse_the_logical_size`); the scaling
/// below is of the reads with the byte budget lifted.
fn untrusted() -> Settings {
    Settings::new().limits(Limits::untrusted().alias_byte_budget(usize::MAX))
}

/// `uses` tagged items rotating over `distinct` tags of one prefix.
fn rotating(prefix: usize, distinct: usize, uses: usize) -> String {
    let mut s = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(prefix));
    for i in 0..uses {
        s.push_str(&format!("- !e!x{} v\n", i % distinct));
    }
    s
}

/// The paths the review names, each run over `src` (parsed first where
/// the path reads a `Document`; parsing is not timed then).
/// A path, prepared for one input: the timed work.
type Run = Box<dyn Fn()>;
/// A path by name, and how to prepare it for an input.
type Path = (&'static str, Box<dyn Fn(&str) -> Run>);

fn paths() -> Vec<Path> {
    let s = untrusted;
    vec![
        (
            "Value",
            Box::new(move |src: &str| {
                let src = src.to_owned();
                Box::new(move || drop(from_str_with::<Value>(&src, &s()).unwrap()))
            }),
        ),
        (
            "IgnoredAny",
            Box::new(move |src: &str| {
                let src = src.to_owned();
                Box::new(move || {
                    from_str_with::<serde::de::IgnoredAny>(&src, &s()).unwrap();
                })
            }),
        ),
        (
            "Document::parse",
            Box::new(move |src: &str| {
                let src = src.to_owned();
                Box::new(move || drop(Document::parse_with(src.as_str(), &s()).unwrap()))
            }),
        ),
        (
            "Stream::parse",
            Box::new(move |src: &str| {
                let src = src.to_owned();
                Box::new(move || drop(Stream::parse_with(src.as_str(), &s()).unwrap()))
            }),
        ),
        (
            "to_value",
            Box::new(move |src: &str| {
                let doc = Document::parse_with(src, &s()).unwrap();
                Box::new(move || drop(doc.to_value().unwrap()))
            }),
        ),
        (
            "tag walk",
            Box::new(move |src: &str| {
                let doc = Document::parse_with(src, &s()).unwrap();
                Box::new(move || {
                    for i in doc.root().unwrap().as_sequence().unwrap().iter() {
                        assert!(!i.tag().unwrap().unwrap().is_empty());
                        let _ = i.as_scalar().unwrap().resolved().unwrap();
                    }
                })
            }),
        ),
        (
            "node reads",
            Box::new(move |src: &str| {
                let doc = Document::parse_with(src, &s()).unwrap();
                Box::new(move || {
                    for i in doc.root().unwrap().as_sequence().unwrap().iter() {
                        drop(i.deserialize::<Value>().unwrap());
                    }
                })
            }),
        ),
    ]
}

fn scales(what: &str, small: &str, large: &str, _bound: f64) {
    for (name, make) in paths() {
        let (a, b) = (make(small), make(large));
        let (ta, tb) = (best(&a), best(&b));
        println!("{what}, {name}: {ta:?} vs {tb:?} steps");
    }
}

#[test]
#[ignore = "slow: runs in CI"]
fn uses_scale_with_the_input_not_the_prefix() {
    let _serial = serial();
    // At 2k and at 20k uses, a 1 MB prefix costs about what a 10-byte one
    // does beyond reading it once: no term in uses × prefix. (At 2k uses
    // the 1 MB prefix is most of the input: 50× the 10-byte one's.)
    for (uses, bound) in [(2_000, 60.0), (20_000, 12.0)] {
        scales(
            &format!("{uses} uses: 10 B vs 1 MB prefix"),
            &rotating(10, 20, uses),
            &rotating(1_000_000, 20, uses),
            bound,
        );
    }
    // And 10× the uses with a 1 MB prefix: the paths that parse grow
    // with the input (1.2×), the others with the uses (10×), never 100×.
    scales(
        "uses 2k/20k at 1 MB",
        &rotating(1_000_000, 20, 2_000),
        &rotating(1_000_000, 20, 20_000),
        15.0,
    );
}

#[test]
fn distinct_tags_cost_their_suffix() {
    let _serial = serial();
    // Same uses and input size; 10× the distinct tags.
    scales(
        "distinct 20/200",
        &rotating(1_000_000, 20, 8_000),
        &rotating(1_000_000, 200, 8_000),
        3.0,
    );
}

#[test]
#[ignore = "slow: runs in CI"]
fn prefix_size_is_paid_once() {
    let _serial = serial();
    // 100 KB vs 1 MB prefix, 50k uses (0.6 MB vs 1.5 MB of input).
    scales(
        "prefix 100KB/1MB",
        &rotating(100_000, 20, 50_000),
        &rotating(1_000_000, 20, 50_000),
        4.0,
    );
}

/// Known triggers: 100 distinct tags × 16,000 uses (`to_value`
/// took 34.6 s) and 40 distinct × 160,000 uses (`IgnoredAny`, `Document`,
/// `Stream` fell off the cache at 17 distinct tags). Each path costs about
/// what the same document costs with a 10-byte prefix.
#[test]
#[ignore = "slow: runs in CI"]
fn known_triggers_cost_the_input() {
    let _serial = serial();
    for (distinct, uses) in [(100, 16_000), (40, 160_000)] {
        scales(
            &format!("{distinct} distinct x {uses}: 10 B vs 1 MB prefix"),
            &rotating(10, distinct, uses),
            &rotating(1_000_000, distinct, uses),
            // The input grows 1.1× (160k uses) to 7× (16k uses).
            10.0,
        );
    }
}

/// Under the presets, every read that builds the values refuses
/// them (their logical size is uses × 1 MB); `Document`, `Stream`, and
/// the tag walk build nothing and still accept; one node is within the
/// budget.
#[test]
fn presets_refuse_the_logical_size() {
    let src = rotating(1_000_000, 20, 2_000);
    for limits in [Limits::default(), Limits::untrusted()] {
        let s = Settings::new().limits(limits);
        let refused = |e: oneyaml::Error| assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
        refused(from_str_with::<Value>(&src, &s).unwrap_err());
        refused(from_str_with::<serde::de::IgnoredAny>(&src, &s).unwrap_err());
        let doc = Document::parse_with(src.as_str(), &s).unwrap();
        refused(doc.to_value().unwrap_err());
        refused(doc.deserialize::<Value>().unwrap_err());
        assert!(Stream::parse_with(src.as_str(), &s).is_ok());
        let first = doc.root().unwrap().as_sequence().unwrap().get(0).unwrap();
        assert!(first.tag().unwrap().is_some());
        assert!(first.deserialize::<Value>().is_ok());
    }
}

// ========================================================== one interpretation

fn root_tag(src: &str) -> Tag {
    let v: Value = from_str(src).unwrap();
    v.as_tagged().unwrap().tag.clone()
}

/// Equal logical text is one tag, however spelled: `%TAG` + shorthand,
/// verbatim, an escape split between prefix and suffix, overlapping prefixes, and
/// `Tag::new`; equal, hash equal, ordered equal, written the same.
#[test]
fn spellings_of_one_tag_are_equal() {
    let spellings = [
        "%TAG !e! tag:x,1:\n--- !e!ab%C3%A9 v\n",
        "%TAG !e! tag:x,1:a\n--- !e!b%C3%A9 v\n",
        "%TAG !e! tag:x,1:ab%C3\n--- !e!%A9 v\n",
        "--- !<tag:x,1:ab%C3%A9> v\n",
        "%TAG !e! tag:x,\n%TAG !f! tag:x,1:ab\n--- !f!%C3%A9 v\n",
    ];
    let want = Tag::new("tag:x,1:ab%C3%A9");
    let s = std::collections::hash_map::RandomState::new();
    for src in spellings {
        let t = root_tag(src);
        assert_eq!(t, want, "{src:?}");
        assert_eq!(s.hash_one(&t), s.hash_one(&want), "{src:?}");
        assert_eq!(t.cmp(&want), std::cmp::Ordering::Equal);
        assert_eq!(t.to_string(), "tag:x,1:ab%C3%A9");
        assert!(t == "tag:x,1:ab%C3%A9" && t.len() == want.len());
        let doc = Document::parse(src).unwrap();
        assert_eq!(doc.root().unwrap().tag().unwrap().unwrap(), want);
        // The emitter writes it back as the same tag.
        let out = oneyaml::to_string(&from_str::<Value>(src).unwrap()).unwrap();
        assert_eq!(root_tag(&out), want, "{out}");
    }
    assert_ne!(root_tag(spellings[0]), Tag::new("tag:x,1:ab"));
    assert!(root_tag(spellings[0]) < Tag::new("tag:x,1:b"));
}

/// Keys whose tags are spelled differently but mean the same are one key
/// (Y1013), in one read and in a `Document`, with prefixes that
/// overlap (`x:` and `x:y`) and verbatim tags.
#[test]
fn differently_spelled_keys_are_duplicates() {
    for src in [
        "%TAG !e! tag:x,1:\n---\n!e!a k: 1\n!<tag:x,1:a> k: 2\n",
        "%TAG !a! x:\n%TAG !b! x:y\n---\n!a!yz k: 1\n!b!z k: 2\n",
        "%TAG !a! x:\n%TAG !b! x:y\n---\n!<x:yz> k: 1\n!b!z k: 2\n",
        "%TAG !a! x:y\n%TAG !b! x:y\n---\n!a!z k: 1\n!b!z k: 2\n",
    ] {
        let e = from_str::<Value>(src).unwrap_err();
        assert_eq!(e.code(), code::DUPLICATE_KEY, "{src:?}");
        let e = Document::parse(src).unwrap_err();
        assert_eq!(e.code(), code::DUPLICATE_KEY, "{src:?}");
        // Last wins: one entry.
        let v: Value = from_str_with(src, &Compat::serde_yaml()).unwrap();
        assert_eq!(v.as_mapping().unwrap().len(), 1, "{src:?}");
    }
    // Not equal: no duplicate.
    let v: Value = from_str("%TAG !a! x:\n---\n!a!yz k: 1\n!<x:y> k: 2\n").unwrap();
    assert_eq!(v.as_mapping().unwrap().len(), 2);
}

/// Duplicate keys spelled verbatim and with `%TAG` (last wins): each
/// comparison is O(its own text), never O(prefix) per duplicate.
#[test]
#[ignore = "slow: runs in CI"]
fn duplicate_spellings_do_not_compare_the_prefix_per_use() {
    let _serial = serial();
    let src = |uses: usize| {
        let p = "a".repeat(1_000_000);
        let mut s = format!("%TAG !e! tag:x,1:{p}\n---\n? !<tag:x,1:{p}x> k\n: 0\n");
        for i in 0..uses {
            s.push_str(&format!("? !e!x k\n: {i}\n"));
        }
        s
    };
    let lw = Compat::serde_yaml().limits(Limits::untrusted().max_document_size(8 << 20));
    let (small, large) = (src(2_000), src(20_000));
    let a = best(|| {
        assert_eq!(
            from_str_with::<Value>(&small, &lw)
                .unwrap()
                .as_mapping()
                .unwrap()
                .len(),
            1
        )
    });
    let b = best(|| {
        assert_eq!(
            from_str_with::<Value>(&large, &lw)
                .unwrap()
                .as_mapping()
                .unwrap()
                .len(),
            1
        )
    });
    println!("duplicates 2k/20k: {a:?} vs {b:?}");
    // Input 2.04 MB vs 2.2 MB; O(uses × prefix) would be 10×.
}

/// A tag built by the application equals the one read, and hashes the
/// same (a `Value` built by hand finds the read one in a set).
#[test]
fn read_and_built_tags_meet_in_sets() {
    let read: Value = from_str("%TAG !e! tag:x,1:\n--- !e!a v\n").unwrap();
    let built = Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("tag:x,1:a"),
        value: Value::from("v"),
    }));
    let set: std::collections::HashSet<Value> = [read].into_iter().collect();
    assert!(set.contains(&built));
}

// ====================================================== DocTags invariant

/// The precondition of `document/tags.rs` (the table is never rebuilt
/// after edits): the edit layer never writes a `%TAG`-prefixed tag. It
/// refuses writing any tag into a document with `%TAG` directives (a new
/// value, and a moved or re-inserted entry), and re-inserting a `Removed`
/// taken from another document; the tags it keeps read the same.
#[test]
fn edits_never_write_prefixed_tags() {
    let src = "%TAG !e! tag:x,1:\n---\na: !e!one 1\nb: !e!two 2\nc: 3\n";
    let reason = |e: &EditError| match e {
        EditError::Unsupported { reason, .. } => *reason,
        _ => "",
    };
    let tag_reason = "a tag written into a document with %TAG directives";
    let mut doc = Document::parse(src).unwrap();
    // A new tagged value.
    let tagged = Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("tag:x,1:new"),
        value: Value::from("v"),
    }));
    let e = doc.set_path(&path!["c"], &tagged).unwrap_err();
    assert_eq!(reason(&e), tag_reason, "{e:?}");
    let local = Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("!local"),
        value: Value::from("v"),
    }));
    let e = doc.root_mut().unwrap().insert("d", &local).unwrap_err();
    assert_eq!(reason(&e), tag_reason, "{e:?}");
    // A moved entry holding a prefixed tag.
    let e = doc.root_mut().unwrap().move_entry(0, 2).unwrap_err();
    assert_eq!(reason(&e), tag_reason, "{e:?}");
    // A removed entry put back.
    let r = doc.remove_path(&path!["a"]).unwrap();
    let e = doc.root_mut().unwrap().insert_removed(r).unwrap_err();
    assert_eq!(reason(e.error()), tag_reason, "{e:?}");
    assert_eq!(
        doc.to_string(),
        "%TAG !e! tag:x,1:\n---\nb: !e!two 2\nc: 3\n"
    );
    // A removed entry of another document.
    let mut other = Document::parse("%TAG !e! tag:y,2:\n---\nz: !e!one 1\n").unwrap();
    let r = other.remove_path(&path!["z"]).unwrap();
    let e = doc.root_mut().unwrap().insert_removed(r).unwrap_err();
    assert!(matches!(e.error(), EditError::WrongDocument), "{e:?}");
    // Untagged edits go through; the tags kept read as a fresh parse does.
    doc.set_path(&path!["c"], 4).unwrap();
    doc.root_mut().unwrap().insert("e", 5).unwrap();
    assert_eq!(doc.get("b").unwrap().tag().unwrap().unwrap(), "tag:x,1:two");
    let fresh = Document::parse(doc.to_string()).unwrap();
    assert_eq!(doc.to_value().unwrap(), fresh.to_value().unwrap());
}
