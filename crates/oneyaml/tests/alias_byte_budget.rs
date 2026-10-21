//! The alias **byte** budget (`Limits::alias_byte_budget`,
//! `Y2009`) and refusals without replay.
//!
//! * Every security-review trigger, at 1× and 10× its input, is refused
//!   by every reading path under `Limits::default()` and
//!   `Limits::untrusted()`, and quickly (refused at the alias or tag use
//!   that goes over, never after building the excess).
//! * At the budget boundary (exactly at it; one byte over) every path
//!   agrees; a node read never refuses what the whole read accepts.
//! * Output written from an accepted `Value` (emitter, `Debug`) is bounded
//!   by the input plus the bytes charged.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt;

use oneyaml::{Code, Deserializer, Document, Limits, Settings, Stream, Value, code, path};
use serde::Deserialize;
use serde::de::{self, EnumAccess, IgnoredAny, VariantAccess, Visitor};

fn with(limits: Limits) -> Settings {
    Settings::new().limits(limits)
}

fn profiles() -> [(&'static str, Limits); 2] {
    [
        ("default", Limits::default()),
        ("untrusted", Limits::untrusted()),
    ]
}

// ============================================================ triggers

/// A large anchored scalar (`len` bytes) and `uses` aliases of it.
fn alias_of_large_scalar(len: usize, uses: usize) -> String {
    let mut s = format!("a: &a {}\nb: [", "x".repeat(len));
    s.push_str(&vec!["*a"; uses].join(", "));
    s.push_str("]\n");
    s
}

/// Alias-keyed chains: `items` mappings nesting three keys that are
/// aliases of one `len`-byte key.
fn alias_keyed_chains(len: usize, items: usize) -> String {
    let mut s = format!("k: &k {}\nm:\n", "x".repeat(len));
    for _ in 0..items {
        s.push_str("- {*k : {*k : {*k : 1}}}\n");
    }
    s
}

/// A merge chain: `l0` holds one `len`-byte value, each next level merges
/// `width` aliases of the one before (`merge_keys` on).
fn merge_chain(len: usize, levels: usize, width: usize) -> String {
    let mut s = format!("l0: &l0 {{k: {}}}\n", "x".repeat(len));
    for i in 1..=levels {
        let refs = vec![format!("*l{}", i - 1); width].join(", ");
        s.push_str(&format!("l{i}: &l{i} {{<<: [{refs}]}}\n"));
    }
    s
}

/// A `len`-byte `%TAG` prefix used `uses` times.
fn tag_prefix(len: usize, uses: usize) -> String {
    let mut s = format!("%TAG !e! tag:x,1:{}\n---\n", "a".repeat(len));
    for _ in 0..uses {
        s.push_str("- !e!x v\n");
    }
    s
}

struct Trigger {
    name: &'static str,
    src: String,
    merge_keys: bool,
}

/// The triggers at `scale` (1 or 10): each charges about 100 MB at 1× and
/// 1 GB at 10× from at most 1 MB of input, over both profiles' budgets.
fn triggers(scale: usize) -> Vec<Trigger> {
    let len = 100_000 * scale;
    vec![
        Trigger {
            name: "alias of a large scalar",
            src: alias_of_large_scalar(len, 1000),
            merge_keys: false,
        },
        Trigger {
            name: "alias-keyed chains",
            src: alias_keyed_chains(len, 400),
            merge_keys: false,
        },
        Trigger {
            name: "merge chain",
            src: merge_chain(len, 3, 10),
            merge_keys: true,
        },
        Trigger {
            name: "%TAG prefix x uses",
            src: tag_prefix(len, 1000),
            merge_keys: false,
        },
    ]
}

/// What each reading path gives for `src` under `settings`: `Ok` or the
/// error code.
fn every_path(src: &str, settings: &Settings) -> Vec<(&'static str, Result<(), Code>)> {
    let code = |r: Result<(), oneyaml::Error>| r.map_err(|e| e.code());
    let mut out = vec![
        (
            "from_str Value",
            code(oneyaml::from_str_with::<Value>(src, settings).map(drop)),
        ),
        (
            "from_str IgnoredAny",
            code(oneyaml::from_str_with::<IgnoredAny>(src, settings).map(drop)),
        ),
        (
            "Deserializer",
            code(
                Deserializer::from_str_with(src, settings)
                    .try_for_each(|d| Value::deserialize(d).map(drop)),
            ),
        ),
    ];
    let doc = Document::parse_with(src, settings).unwrap();
    out.push(("Document::to_value", code(doc.to_value().map(drop))));
    out.push((
        "Document::deserialize",
        code(doc.deserialize::<Value>().map(drop)),
    ));
    let stream = Stream::parse_with(src, settings).unwrap();
    out.push((
        "Stream document",
        code(stream.documents().try_for_each(|d| d.to_value().map(drop))),
    ));
    out
}

#[test]
#[ignore = "slow: runs in CI"]
fn security_review_triggers_are_refused_at_1x_and_10x() {
    for scale in [1, 10] {
        for t in triggers(scale) {
            for (profile, limits) in profiles() {
                let s = with(limits).merge_keys(t.merge_keys);
                for (path, got) in every_path(&t.src, &s) {
                    assert_eq!(
                        got,
                        Err(code::ALIAS_BYTE_BUDGET_EXCEEDED),
                        "{} {scale}x, {profile}: {path}",
                        t.name
                    );
                }
                // Refused at the use that goes over: the work is about
                // the input (at most 1 MB) and the budget, never the
                // 100 MB / 1 GB the input would expand to.
                // Lifting only the byte budget, the same reads would expand
                // that much (the trigger is real), except where the node
                // budget stops them first: checked on the cheapest path.
                if scale == 1 {
                    let lifted = with(Limits::untrusted().alias_byte_budget(usize::MAX))
                        .merge_keys(t.merge_keys);
                    let r = oneyaml::from_str_with::<IgnoredAny>(&t.src, &lifted);
                    assert!(r.is_ok(), "{}: {:?}", t.name, r.err());
                }
            }
        }
    }
}

// ======================================================= joined names

/// A hand-written visitor that takes any enum and reads its variant name
/// as a `String` (`deserialize_any`): a `%TAG` tag naming no variant is
/// joined, and charged.
#[derive(Debug)]
struct Named(String);

impl<'de> Deserialize<'de> for Named {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Named;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a tagged value")
            }
            fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Named, A::Error> {
                let (name, rest): (String, _) = data.variant()?;
                let _: String = rest.newtype_variant()?;
                Ok(Named(name))
            }
        }
        d.deserialize_any(V)
    }
}

/// The typed enum with `#[serde(other)]` reads a tag naming no variant
/// through the joined name too.
#[derive(Debug, Deserialize, PartialEq)]
enum Shape {
    Square,
    #[serde(other)]
    Other,
}

#[test]
fn joined_variant_names_are_charged() {
    // 50 uses of a 10,008-byte prefix (`tag:x,1:` + 10,000): the read
    // itself charges the 50 prefixes; each joined name (`tag:x,1:a…ax`) is
    // 10,009 bytes more.
    let prefix = 10_000;
    let src = tag_prefix(prefix, 50);
    let own = 50 * ("tag:x,1:".len() + prefix);
    let joined = 50 * ("tag:x,1:".len() + prefix + 1);
    let fits = |budget: usize| with(Limits::default().alias_byte_budget(budget));
    // The `Value` read needs only the prefixes.
    assert!(oneyaml::from_str_with::<Value>(&src, &fits(own)).is_ok());
    let e = oneyaml::from_str_with::<Value>(&src, &fits(own - 1)).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    // The input has no alias: the message names what expanded.
    assert!(
        e.diagnostic().message.starts_with(
            "expansion (alias copies and `%TAG` prefixes repeated per use) \
             materialises more than 500399 bytes"
        ),
        "{e}"
    );
    assert!(
        e.diagnostic()
            .help
            .as_deref()
            .unwrap()
            .starts_with("raise Limits::alias_byte_budget")
    );
    // The joined names are charged on top, exactly.
    let names: Vec<Named> = oneyaml::from_str_with(&src, &fits(own + joined)).unwrap();
    assert_eq!(names.len(), 50);
    assert_eq!(names[0].0.len(), joined / 50);
    let e = oneyaml::from_str_with::<Vec<Named>>(&src, &fits(own + joined - 1)).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    assert_eq!(e.path().unwrap().to_string(), "[49]");
    // Every typed path agrees, and so does a node read of the item.
    let doc = Document::parse_with(src.as_str(), &fits(own + joined - 1)).unwrap();
    assert_eq!(
        doc.deserialize::<Vec<Named>>().unwrap_err().code(),
        code::ALIAS_BYTE_BUDGET_EXCEEDED
    );
    assert!(
        doc.get_path(&path![49])
            .unwrap()
            .deserialize::<Named>()
            .is_ok()
    );
    // `#[serde(other)]` (a unit variant: empty content) joins the name of
    // a tag that names no variant, charged the same.
    let src = tag_prefix(prefix, 50).replace("!e!x v", "!e!x ''");
    let shapes: Vec<Shape> = oneyaml::from_str_with(&src, &fits(own + joined)).unwrap();
    assert!(shapes.iter().all(|s| *s == Shape::Other));
    let e = oneyaml::from_str_with::<Vec<Shape>>(&src, &fits(own + joined - 1)).unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    // `from_value` charges nothing: the `Value` was bounded when read.
    let v: Value = oneyaml::from_str_with(&src, &fits(own)).unwrap();
    assert!(oneyaml::from_value::<Vec<Named>>(v).is_ok());
    // At 1× and 10× under the profiles, the read itself is refused.
    for scale in [1, 10] {
        let src = tag_prefix(100_000 * scale, 1000);
        for (profile, limits) in profiles() {
            let e = oneyaml::from_str_with::<Vec<Named>>(&src, &with(limits)).unwrap_err();
            assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED, "{profile}");
        }
    }
}

// ========================================================== one interpretation

/// Every path of a document whose charges are all inside node `n`:
/// whole reads and the node read agree exactly at the boundary.
#[test]
fn every_path_agrees_at_the_boundary() {
    // `n` copies `a` (6 bytes) twice and `t` (3 bytes + a 9-byte tag:
    // `tag:x,1:` is a `%TAG` prefix, so each own use of it costs 8 more).
    let src = "%TAG !e! tag:x,1:\n---\na: &a abcdef\nt: &t !e!q xyz\nn: [*a, *a, *t]\n";
    // Own uses: `!e!q` once (8). Replays: 6 + 6 + (3 + 9) = 24.
    let exact = 8 + 24;
    for budget in [exact, exact - 1] {
        let s = with(Limits::default().alias_byte_budget(budget));
        let want = if budget == exact {
            Ok(())
        } else {
            Err(code::ALIAS_BYTE_BUDGET_EXCEEDED)
        };
        for (path, got) in every_path(src, &s) {
            assert_eq!(got, want, "budget {budget}: {path}");
        }
        // The node read is charged its own expansion only (24): it
        // accepts at `exact - 1` too, never refusing what the whole read
        // accepts.
        let doc = Document::parse_with(src, &s).unwrap();
        assert!(doc.get("n").unwrap().deserialize::<Value>().is_ok());
    }
    let s = with(Limits::default().alias_byte_budget(23));
    let doc = Document::parse_with(src, &s).unwrap();
    let e = doc.get("n").unwrap().deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    // At the third alias, as the whole read with a budget 8 larger.
    assert_eq!(&src[e.span().unwrap().start..e.span().unwrap().end], "*t");
    let s = with(Limits::default().alias_byte_budget(31));
    let e = oneyaml::from_str_with::<Value>(src, &s).unwrap_err();
    assert_eq!(&src[e.span().unwrap().start..e.span().unwrap().end], "*t");
}

/// The ratio budget scales with the bytes of the document read so far,
/// measured from the document's start in every path: a node read has the
/// whole read's budget at the same place (the prefix rule).
#[test]
fn ratio_budgets_agree_across_paths() {
    // Each copy is 10 bytes; the budget at an alias is the bytes of the
    // document up to its end (from `---`, or the content's start).
    let body = "a: &a 0123456789\nn: [*a, *a, *a, *a]\n";
    for (lead, docs) in [("", 1), ("x: 1\n---\n", 2)] {
        let src = format!("{lead}{body}");
        let s = with(Limits::default().alias_byte_budget_ratio(1, 0));
        // The aliases end at 23, 27, 31, 35 (+ 4 after `---\n`): the
        // first three fit (10 ≤ 23, 20 ≤ 27, 30 ≤ 31), the fourth
        // (40 > 35) does not.
        let whole = Stream::parse_with(src.as_str(), &s).unwrap();
        assert_eq!(whole.len(), docs);
        let last = whole.document(docs - 1).unwrap();
        let e = last.to_value().unwrap_err();
        assert_eq!(e.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
        let e2 = last.get("n").unwrap().deserialize::<Value>().unwrap_err();
        assert_eq!((e2.code(), e2.span()), (e.code(), e.span()));
        let e3 = Deserializer::from_str_with(&src, &s)
            .map(Value::deserialize)
            .find_map(Result::err)
            .unwrap();
        assert_eq!((e3.code(), e3.span()), (e.code(), e.span()));
        let at = e.span().unwrap();
        assert_eq!(&src[at.start..at.end], "*a");
        assert_eq!(at.start, src.rfind("*a").unwrap());
        // Three aliases fit.
        let ok = format!("{lead}a: &a 0123456789\nn: [*a, *a, *a]\n");
        assert!(
            oneyaml::Deserializer::from_str_with(&ok, &s).all(|d| Value::deserialize(d).is_ok())
        );
        let doc = Stream::parse_with(ok.as_str(), &s)
            .unwrap()
            .document(docs - 1)
            .unwrap();
        assert!(doc.get("n").unwrap().deserialize::<Value>().is_ok());
    }
}

// ============================================================== output

/// Output written from an accepted `Value` is bounded by the input plus
/// the bytes charged (the logical size is bounded at read time).
#[test]
fn output_is_bounded_by_input_plus_budget() {
    for (src, charged) in [
        // 100 copies of 1,000 bytes.
        (alias_of_large_scalar(1000, 100), 100 * 1000),
        // 100 uses of a 10,008-byte prefix (`tag:x,1:` + 10,000).
        (tag_prefix(10_000, 100), 100 * 10_008),
    ] {
        let s = with(Limits::default().alias_byte_budget(charged));
        let v: Value = oneyaml::from_str_with(&src, &s).unwrap();
        let bound = src.len() + charged + 4096;
        let emitted = oneyaml::to_string(&v).unwrap();
        assert!(emitted.len() <= bound, "{} > {bound}", emitted.len());
        let debug = format!("{v:?}");
        assert!(debug.len() <= 2 * bound, "{} > {}", debug.len(), 2 * bound);
        // One byte less is refused.
        let s = with(Limits::default().alias_byte_budget(charged - 1));
        assert_eq!(
            oneyaml::from_str_with::<Value>(&src, &s)
                .unwrap_err()
                .code(),
            code::ALIAS_BYTE_BUDGET_EXCEEDED
        );
    }
}

// ====================================================== refusal without replay

/// 300 refused node reads of a 2 KB document (22 s when each
/// replayed the expansion up to the budget before refusing).
#[test]
fn refused_node_reads_are_cheap() {
    let mut src = String::from("l0: &l0 [x, x, x, x, x, x, x, x, x, x]\n");
    for i in 1..7 {
        let refs = vec![format!("*l{}", i - 1); 10].join(", ");
        src.push_str(&format!("l{i}: &l{i} [{refs}]\n"));
    }
    src.push_str("big: &big ");
    src.push_str(&"y".repeat(1000));
    src.push('\n');
    for i in 0..150 {
        src.push_str(&format!("n{i}: [*l6]\nb{i}: [*big, *big]\n"));
    }
    for (profile, limits) in profiles() {
        let s = with(limits.alias_byte_budget(1500));
        let doc = Document::parse_with(src.as_str(), &s).unwrap();
        let mut codes = Vec::new();
        for i in 0..150 {
            for (key, want) in [
                (format!("n{i}"), code::ALIAS_BUDGET_EXCEEDED),
                (format!("b{i}"), code::ALIAS_BYTE_BUDGET_EXCEEDED),
            ] {
                let e = doc.get(&key).unwrap().deserialize::<Value>().unwrap_err();
                assert_eq!(e.code(), want, "{profile} {key}");
                codes.push(e.code());
            }
        }
        assert_eq!(codes.len(), 300);
    }
}

/// 1,000 documents, each over its own byte budget at a `%TAG` use: each
/// refused where it goes over, the per-document bound being
/// `max_documents` × the budget (`untrusted()`, document size lifted to
/// hold the stream).
#[test]
#[ignore = "slow: runs in CI"]
fn a_thousand_documents_are_each_refused() {
    let doc = tag_prefix(20_000, 500);
    let src = vec![doc.as_str(); 1000].join("...\n");
    let s = with(Limits::untrusted().max_document_size(usize::MAX));
    let stream = Stream::parse_with(src.as_str(), &s).unwrap();
    assert_eq!(stream.len(), 1000);
    let refused = stream
        .documents()
        .filter(|d| d.to_value().unwrap_err().code() == code::ALIAS_BYTE_BUDGET_EXCEEDED)
        .count();
    assert_eq!(refused, 1000);
    let mut it = Deserializer::from_str_with(&src, &s);
    let first = Value::deserialize(it.next().unwrap()).unwrap_err();
    assert_eq!(first.code(), code::ALIAS_BYTE_BUDGET_EXCEEDED);
    assert!(it.next().is_none());
}
