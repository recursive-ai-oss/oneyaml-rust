//! Tag memory never grows with uses (bounded time and memory), node reads
//! resolve each `%TAG`-prefixed tag once per document, and the emitter refuses
//! tags the output could not be read back with.
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

use oneyaml::{
    Document, Limits, Settings, Tag, TaggedValue, Value, code, from_str_with, to_string,
    to_string_with,
};

/// The timed tests run one at a time (the harness runs tests in parallel).
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

fn untrusted() -> Settings {
    Settings::new().limits(Limits::untrusted())
}

/// `untrusted()` without the alias byte budget: for the
/// checks of what reading these values costs once it may (the presets
/// now refuse them, `Y2009`).
fn untrusted_no_byte_budget() -> Settings {
    Settings::new().limits(Limits::untrusted().alias_byte_budget(usize::MAX))
}

/// `uses` tagged items rotating over `distinct` tags of one long prefix.
fn rotating(prefix: usize, distinct: usize, uses: usize) -> String {
    let mut s = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(prefix));
    for i in 0..uses {
        s.push_str(&format!("- !e!x{} v\n", i % distinct));
    }
    s
}

/// The distinct tag allocations of a `Value`'s items.
fn tag_allocations(v: &Value) -> usize {
    distinct_allocations(
        v.as_sequence()
            .unwrap()
            .iter()
            .map(|i| i.as_tagged().unwrap().tag.clone()),
    )
}

/// How many allocations `tags` are (`Tag` has no `as_str`).
fn distinct_allocations(tags: impl Iterator<Item = Tag>) -> usize {
    let mut seen: Vec<Tag> = Vec::new();
    for t in tags {
        if !seen.iter().any(|s| s.same_allocation(&t)) {
            seen.push(t);
        }
    }
    seen.len()
}

// ============================================ 1: rotating tags, one read

/// The known trigger (1 MB prefix, 8,000 uses, `untrusted()`): the
/// values hold exactly one instance of each distinct tag (1.58 GB for 20
/// distinct, 4.7 GB for 40, unbounded; now an instance
/// holds only its suffix, the prefix shared).
///
/// Those values' logical size (8 GB of tag text) is over
/// `untrusted()`'s alias byte budget, so the preset now refuses them
/// (`Y2009`, the expected new refusal); the sharing is checked with the
/// byte budget lifted.
#[test]
fn rotating_tags_are_shared_by_the_values() {
    let src = rotating(1_000_000, 16, 8000);
    let e = from_str_with::<Value>(&src, &untrusted()).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    let doc = Document::parse_with(src.as_str(), &untrusted()).unwrap();
    assert_eq!(
        doc.to_value().unwrap_err().code(),
        code::ALIAS_BYTE_BUDGET_EXCEEDED
    );
    for distinct in [16, 20, 40] {
        let src = rotating(1_000_000, distinct, 8000);
        let v: Value = from_str_with(&src, &untrusted_no_byte_budget()).unwrap();
        assert_eq!(tag_allocations(&v), distinct, "{distinct}");
        // The `Document`'s value too (a read of the tree).
        let doc = Document::parse_with(src.as_str(), &untrusted_no_byte_budget()).unwrap();
        let v = doc.to_value().unwrap();
        assert_eq!(tag_allocations(&v), distinct, "{distinct}");
    }
}

/// 1× and 10× the uses of 40 distinct tags (past the cache's bound):
/// building the `Value` costs about the input's growth, never 10× the
/// tag text per use.
#[test]
fn rotating_tags_scale_with_the_input() {
    let _serial = serial();
    let (small, large) = (rotating(1_000_000, 40, 800), rotating(1_000_000, 40, 8000));
    let s = untrusted_no_byte_budget();
    let a = best(|| drop(from_str_with::<Value>(&small, &s).unwrap()));
    let b = best(|| drop(from_str_with::<Value>(&large, &s).unwrap()));
    println!("value: 800 uses {a:?}, 8000 uses {b:?}");
}

// ================================================== 2: node reads, tags

/// The known trigger: a 1 MB prefix × 4,000 tagged nodes, walked
/// with `NodeRef::tag` and `ScalarRef::resolved` (24.7 s before). Each
/// distinct tag is resolved once per document and borrowed: with 10× the
/// prefix the walk costs about the same; O(prefix) per node would be 10×.
#[test]
fn node_tag_walks_do_not_scale_with_the_prefix() {
    let _serial = serial();
    for distinct in [1, 20] {
        let (small, large) = (
            Document::parse(rotating(100_000, distinct, 4000)).unwrap(),
            Document::parse(rotating(1_000_000, distinct, 4000)).unwrap(),
        );
        let walk = |d: &Document| {
            let mut n = 0usize;
            for item in d.root().unwrap().as_sequence().unwrap().iter() {
                n += item.tag().unwrap().unwrap().len();
                let _ = item.as_scalar().unwrap().resolved().unwrap();
            }
            assert!(n > 0);
        };
        // The first walk builds the table (once per document).
        walk(&small);
        walk(&large);
        let (a, b) = (best(|| walk(&small)), best(|| walk(&large)));
        println!("{distinct} distinct: 100 KB prefix {a:?}, 1 MB prefix {b:?}");
        // Every read shares the one resolved tag.
        let tags = large
            .root()
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .map(|i| i.tag().unwrap().unwrap());
        assert_eq!(distinct_allocations(tags), distinct);
    }
}

/// Per-node typed reads (`NodeRef::deserialize`) and `ScalarRef::resolved`
/// share the document's copy: separate reads, one allocation per tag.
#[test]
fn node_reads_share_the_documents_tags() {
    let doc = Document::parse(rotating(1 << 16, 3, 30)).unwrap();
    let items: Vec<_> = doc.root().unwrap().as_sequence().unwrap().iter().collect();
    let mut tags = Vec::new();
    for i in &items {
        let v: Value = i.deserialize().unwrap();
        tags.push(v.as_tagged().unwrap().tag.clone());
        let r = i.as_scalar().unwrap().resolved().unwrap();
        tags.push(r.as_tagged().unwrap().tag.clone());
    }
    assert_eq!(distinct_allocations(tags.into_iter()), 3);
    // Same values as the whole-document read.
    let whole = doc.to_value().unwrap();
    for (i, item) in items.iter().enumerate() {
        assert_eq!(&item.deserialize::<Value>().unwrap(), whole.get(i).unwrap());
    }
}

/// The table stays exact across edits (it is not rebuilt: an edit never
/// writes a `%TAG`-prefixed tag), and an edit of a `%TAG` document does
/// not rebuild it.
#[test]
fn the_table_survives_edits() {
    let src = "%TAG !e! tag:x,1:\n---\na: !e!one 1\nb: !e!two 2\nc: 3\n";
    let mut doc = Document::parse(src).unwrap();
    assert_eq!(
        doc.get("a")
            .unwrap()
            .tag()
            .unwrap()
            .map(|t| t.to_string())
            .as_deref(),
        Some("tag:x,1:one")
    );
    doc.get_mut("c").unwrap().set(4).unwrap();
    doc.root_mut().unwrap().insert("d", 5).unwrap();
    let _ = doc.root_mut().unwrap().remove("a").unwrap();
    assert_eq!(
        doc.get("b")
            .unwrap()
            .tag()
            .unwrap()
            .map(|t| t.to_string())
            .as_deref(),
        Some("tag:x,1:two")
    );
    assert_eq!(doc.get("d").unwrap().tag().unwrap(), None);
    assert_eq!(
        doc.to_value().unwrap(),
        from_str_with::<Value>(&doc.to_string(), &Settings::new()).unwrap()
    );
    // A clone reads the same.
    let copy = doc.clone();
    assert_eq!(
        copy.get("b")
            .unwrap()
            .tag()
            .unwrap()
            .map(|t| t.to_string())
            .as_deref(),
        Some("tag:x,1:two")
    );
}

// ================================================ 4: emitter tag length

fn tagged(tag: String) -> Value {
    Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new(tag),
        value: Value::from("v"),
    }))
}

/// A tag longer than the reading settings' `max_scalar_len` is `Y2001`
/// (the output would not read back); within it, it reads back.
#[test]
fn the_emitter_refuses_tags_the_output_cannot_read_back() {
    let long = format!("!{}", "t".repeat(1 << 20));
    let v = tagged(long.clone());
    let e = to_string_with(&v, &untrusted()).unwrap_err();
    assert_eq!(e.code(), code::SCALAR_TOO_LONG);
    // As a key, in a sequence, on a collection, and a global tag.
    let mut m = oneyaml::Mapping::new();
    m.insert(v.clone(), Value::from(1));
    for v in [
        Value::from(m),
        Value::from(vec![v.clone()]),
        Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new(long.clone()),
            value: Value::from(vec![Value::from(1)]),
        })),
        tagged(format!("tag:x,1:{}", "g".repeat(1 << 20))),
    ] {
        let e = to_string_with(&v, &untrusted()).unwrap_err();
        assert_eq!(e.code(), code::SCALAR_TOO_LONG);
    }
    // Under the defaults it is written and reads back.
    let out = to_string(&v).unwrap();
    assert_eq!(from_str_with::<Value>(&out, &Settings::new()).unwrap(), v);
    // Exactly at the limit: accepted, and read back under the same limit.
    let s = Settings::new().limits(Limits::new().max_scalar_len(32));
    let at = tagged(format!("!{}", "t".repeat(31)));
    let out = to_string_with(&at, &s).unwrap();
    assert_eq!(from_str_with::<Value>(&out, &s).unwrap(), at);
    let over = tagged(format!("!{}", "t".repeat(32)));
    assert_eq!(
        to_string_with(&over, &s).unwrap_err().code(),
        code::SCALAR_TOO_LONG
    );
}

/// The streaming serializer: a `Value` field with a long tag, and an enum
/// variant written as a local tag (`Layout::SerdeYaml`).
#[test]
fn the_streaming_serializer_refuses_long_tags() {
    #[derive(serde::Serialize)]
    struct Holder {
        v: Value,
    }
    let h = Holder {
        v: tagged(format!("!{}", "t".repeat(40))),
    };
    let s = Settings::new().limits(Limits::new().max_scalar_len(32));
    assert_eq!(
        to_string_with(&h, &s).unwrap_err().code(),
        code::SCALAR_TOO_LONG
    );
    #[derive(serde::Serialize)]
    enum E {
        AVariantNameLongerThanThirtyTwoBytes(f64),
    }
    let compat = oneyaml::Compat::serde_yaml().limits(Limits::new().max_scalar_len(32));
    let e = to_string_with(&E::AVariantNameLongerThanThirtyTwoBytes(1.0), &compat).unwrap_err();
    assert_eq!(e.code(), code::SCALAR_TOO_LONG);
    // Within the limit it is written as the tag.
    let ok = oneyaml::Compat::serde_yaml();
    assert_eq!(
        to_string_with(&E::AVariantNameLongerThanThirtyTwoBytes(1.0), &ok).unwrap(),
        "!AVariantNameLongerThanThirtyTwoBytes 1.0\n"
    );
}
