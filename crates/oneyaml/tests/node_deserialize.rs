//! Per-node typed reads, `NodeRef::deserialize`.
//!
//! A node reads what the whole-document read reads at its position:
//! plain scalars' source text, aliases (to anchors inside or outside the node),
//! merge keys, spans, codes, and locations.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeMap;

use oneyaml::{
    Document, Limits, NodeRef, Settings, Spanned, Stream, Value, code, from_str, from_str_with,
    path,
};
use serde::Deserialize;

fn at<'d>(doc: &'d Document, p: &oneyaml::Path) -> NodeRef<'d> {
    doc.get_path(p).unwrap()
}

#[test]
fn a_node_reads_what_the_document_reads_there() {
    let src = "\
spec:
  port: 8080
  version: 1.10
  flags: [on, 'off', 0x1F]
  empty:
";
    let doc = Document::parse(src).unwrap();
    let spec = at(&doc, &path!["spec"]);
    // Plain scalars into strings keep their source text.
    let strings: BTreeMap<String, Value> = spec.deserialize().unwrap();
    assert_eq!(strings.len(), 4);
    let port: String = at(&doc, &path!["spec", "port"]).deserialize().unwrap();
    assert_eq!(port, "8080");
    let version: String = at(&doc, &path!["spec", "version"]).deserialize().unwrap();
    assert_eq!(version, "1.10");
    let flags: Vec<String> = at(&doc, &path!["spec", "flags"]).deserialize().unwrap();
    assert_eq!(flags, ["on", "off", "0x1F"]);
    let empty: Option<u8> = at(&doc, &path!["spec", "empty"]).deserialize().unwrap();
    assert_eq!(empty, None);
    // The node's `Value` is the document's `Value` at that path.
    let whole = doc.to_value().unwrap();
    assert_eq!(spec.deserialize::<Value>().unwrap(), whole["spec"]);
    // The root is the whole document.
    let root = doc.root().unwrap();
    assert_eq!(root.deserialize::<Value>().unwrap(), whole);
    assert_eq!(
        root.deserialize::<BTreeMap<String, BTreeMap<String, Value>>>()
            .unwrap(),
        doc.deserialize::<BTreeMap<String, BTreeMap<String, Value>>>()
            .unwrap()
    );
}

#[test]
fn errors_point_into_the_document() {
    let src = "a:\n  b:\n    - 1\n    - x\n";
    let doc = Document::parse(src).unwrap();
    let e = at(&doc, &path!["a"])
        .deserialize::<BTreeMap<String, Vec<u8>>>()
        .unwrap_err();
    assert_eq!(e.code(), code::TYPE_MISMATCH);
    // The path is below the node; the span and location are the document's.
    assert_eq!(e.path().unwrap().to_string(), "b[1]");
    assert_eq!(e.span().map(|s| &src[s.start..s.end]), Some("x"));
    let l = e.location().unwrap();
    assert_eq!((l.line(), l.column()), (4, 7));
    // `Spanned` gives document offsets.
    let b: Vec<Spanned<u8>> = at(&doc, &path!["a", "b"])
        .deserialize::<Vec<Spanned<Value>>>()
        .map(|v| v.into_iter().map(|s| Spanned::new(s.span(), 0u8)).collect())
        .unwrap();
    assert_eq!(&src[b[1].span().start..b[1].span().end], "x");
    let one: Spanned<u8> = at(&doc, &path!["a", "b", 0]).deserialize().unwrap();
    assert_eq!((*one, &src[one.span().start..one.span().end]), (1, "1"));
}

#[test]
fn aliases_to_anchors_outside_the_node() {
    let src = "\
base: &b {x: 1}
list: &l [*b, 2]
other: &b {x: 99}
use:
  first: *l
  second: *b
  inner: &i [3]
  again: *i
";
    let doc = Document::parse(src).unwrap();
    let whole = doc.to_value().unwrap();
    let node = at(&doc, &path!["use"]);
    let v: Value = node.deserialize().unwrap();
    assert_eq!(v, whole["use"]);
    // `*b` is the nearest `&b` before it (x: 99), and inside `*l` the one
    // before `list` (x: 1).
    assert_eq!(v["second"]["x"], Value::from(99));
    assert_eq!(v["first"][0]["x"], Value::from(1));
    // An alias node alone.
    let second: BTreeMap<String, u8> = at(&doc, &path!["use", "second"]).deserialize().unwrap();
    assert_eq!(second["x"], 99);
    let first: Value = at(&doc, &path!["use", "first"]).deserialize().unwrap();
    assert_eq!(first, whole["list"]);
    // Every node of the document reads its `Value`.
    for (k, want) in whole.as_mapping().unwrap() {
        let got: Value = at(&doc, &path![k.as_str().unwrap()]).deserialize().unwrap();
        assert_eq!(&got, want);
    }
}

#[test]
fn anchor_names_redefined_inside_the_node() {
    let src = "\
a: &x out
n:
  - *x
  - &x in
  - *x
";
    let doc = Document::parse(src).unwrap();
    let v: Vec<String> = at(&doc, &path!["n"]).deserialize().unwrap();
    assert_eq!(v, ["out", "in", "in"]);
}

#[test]
fn merge_keys_through_outside_anchors() {
    let settings = Settings::new().merge_keys(true);
    let src = "\
x-defaults: &d
  restart: always
  image: base
services:
  web:
    <<: *d
    image: web:1
";
    let doc = Document::parse_with(src, &settings).unwrap();
    #[derive(Debug, PartialEq, Deserialize)]
    struct Service {
        restart: String,
        image: String,
    }
    let web: Service = at(&doc, &path!["services", "web"]).deserialize().unwrap();
    assert_eq!(
        web,
        Service {
            restart: "always".into(),
            image: "web:1".into()
        }
    );
    let whole: Value = from_str_with(src, &settings).unwrap();
    let v: Value = at(&doc, &path!["services"]).deserialize().unwrap();
    assert_eq!(v, whole["services"]);
}

#[test]
fn alias_budget_applies_to_a_node() {
    // A billion-laughs chain outside the node, reached by one alias.
    let mut src = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
    let names = ["b", "c", "d", "e", "f", "g"];
    let mut prev = "a";
    for n in names {
        src.push_str(&format!("{n}: &{n} [*{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}, *{prev}]\n"));
        prev = n;
    }
    src.push_str("z: {bomb: *g}\n");
    let doc = Document::parse(src.clone()).unwrap();
    let untrusted = Settings::new().limits(Limits::untrusted());
    let doc_u = Document::parse_with(src.clone(), &untrusted).unwrap();
    let e = at(&doc_u, &path!["z"]).deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BUDGET_EXCEEDED);
    let e = at(&doc, &path!["z"]).deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BUDGET_EXCEEDED);
    // The whole-document read refuses it too.
    assert_eq!(
        from_str::<Value>(&src).unwrap_err().code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
    // A small expansion outside the node is fine.
    let small: Vec<Value> = at(&doc, &path!["b"]).deserialize().unwrap();
    assert_eq!(small.len(), 10);
}

#[test]
fn restricted_documents_and_bad_aliases() {
    // An error elsewhere does not stop a clean node; an error inside does.
    let parsed = Document::parse_recovering("good: {a: 1}\nbad: [1, 2\nlater: x\n");
    let doc = parsed.value();
    assert!(doc.has_errors());
    let good: BTreeMap<String, u8> = at(doc, &path!["good"]).deserialize().unwrap();
    assert_eq!(good["a"], 1);
    let err = doc.errors().next().unwrap();
    let e = err.deserialize::<Value>().unwrap_err();
    assert_eq!(e.kind(), oneyaml::ErrorKind::Syntax);

    // Undefined and recursive aliases: Y1015, as in the whole document.
    let parsed = Document::parse_recovering("a: *nope\nb: &r [1, *r]\n");
    let doc = parsed.value();
    let e = at(doc, &path!["a"]).deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
    let e = at(doc, &path!["b"]).deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
    let e = at(doc, &path!["b", 1]).deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::UNDEFINED_ALIAS);
}

#[test]
fn stream_documents_and_version_directives() {
    let stream = Stream::parse("a: &x 1\n---\nb: &x 2\nc: *x\n").unwrap();
    let doc = stream.document(1).unwrap();
    let c: u8 = doc.get("c").unwrap().deserialize().unwrap();
    assert_eq!(c, 2);
    // The document's `%YAML 1.1` decides, as for the whole document.
    let settings = Settings::new().honor_version_directive(true);
    let src = "%YAML 1.1\n---\nflags: {a: yes, b: 0b101}\n";
    let doc = Document::parse_with(src, &settings).unwrap();
    let flags: Value = at(&doc, &path!["flags"]).deserialize().unwrap();
    let whole: Value = from_str_with(src, &settings).unwrap();
    assert_eq!(flags, whole["flags"]);
    assert_eq!(flags["a"], Value::Bool(true));
}

/// Reads after edits see the edited text (the tree agrees with it).
#[test]
fn reads_after_edits() {
    let mut doc = Document::parse("a: &x {p: 1}\nb: [*x]\n").unwrap();
    doc.get_mut("a")
        .unwrap()
        .set(BTreeMap::from([("p", 2)]))
        .unwrap();
    let b: Vec<BTreeMap<String, u8>> = doc.get("b").unwrap().deserialize().unwrap();
    assert_eq!(b[0]["p"], 2);
    assert_eq!(
        doc.get("b").unwrap().deserialize::<Value>().unwrap(),
        doc.to_value().unwrap()["b"]
    );
}

/// A deep node reads on a small stack (its `Value` iteratively; a
/// recursive target is bounded by `max_typed_depth`).
#[test]
fn deep_nodes_on_a_small_stack() {
    let run = || {
        let depth = 1000;
        let src = format!("a: {}{}\n", "[".repeat(depth), "]".repeat(depth));
        let doc = Document::parse(src).unwrap();
        let v: Value = doc.get("a").unwrap().deserialize().unwrap();
        assert_eq!(v.depth(), depth);
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Tree(Vec<Tree>);
        let settings = Settings::new().limits(Limits::default().max_typed_depth(16));
        let doc = Document::parse_with(doc.to_string(), &settings).unwrap();
        let e = doc.get("a").unwrap().deserialize::<Tree>().unwrap_err();
        assert_eq!(e.code(), code::TYPED_DEPTH_EXCEEDED);
    };
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

/// A node read costs O(the node + what its aliases reach), never
/// O(document): the same read on documents 100× apart in size takes about
/// the same time (minimum of several runs; 4× allowed for cache effects,
/// as `document_scaling.rs`).
#[test]
fn node_reads_are_local() {
    use std::fmt::Write as _;
    use std::time::{Duration, Instant};

    fn source(items: usize) -> String {
        let mut s = String::from("defaults: &d {retries: 3, verbose: false}\nbig:\n");
        for i in 0..items {
            writeln!(s, "  - {{id: {i}, name: item number {i}}} # c").unwrap();
        }
        s.push_str("target:\n  port: 8080\n  opts: *d\n  tags: [a, b]\n");
        s
    }
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Target {
        port: String,
        opts: BTreeMap<String, Value>,
        tags: Vec<String>,
    }
    fn best(src: &str) -> Duration {
        let doc = Document::parse(src).unwrap();
        let node = doc.get("target").unwrap();
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for _ in 0..50 {
                let t: Target = node.deserialize().unwrap();
                assert_eq!(t.port, "8080");
            }
            min = min.min(start.elapsed());
        }
        min
    }
    let small = best(&source(1_000));
    let large = best(&source(100_000));
    eprintln!("node read x50: 1k items {small:?}, 100k items {large:?}");
    assert!(
        large <= small.saturating_mul(4) + Duration::from_millis(5),
        "a node read grew with the document: {small:?} vs {large:?}"
    );
}

/// The alias budget of a node read is the
/// document's, so a node read never refuses what the whole-document read
/// accepts; past the budget both refuse.
#[test]
fn node_reads_never_refuse_what_the_document_accepts() {
    // A known shape: one large anchor, aliased 8 times elsewhere.
    let mut src = String::from("a: &a [");
    src.push_str(&vec!["0"; 150_000].join(", "));
    src.push_str("]\nn: [*a, *a, *a, *a, *a, *a, *a, *a]\n");
    let doc = Document::parse(src.clone()).unwrap();
    assert!(doc.to_value().is_ok());
    let n: Vec<Vec<u8>> = doc.get("n").unwrap().deserialize().unwrap();
    assert_eq!(n.len(), 8);

    // Around the budget (untrusted: max(2 x own nodes, 100k)).
    let untrusted = Settings::new().limits(Limits::untrusted());
    let (mut accepted, mut refused) = (0, 0);
    for aliases in 1..=12 {
        let mut src = String::from("a: &a [");
        src.push_str(&vec!["0"; 20_000].join(", "));
        src.push_str("]\nn: [");
        src.push_str(&vec!["*a"; aliases].join(", "));
        src.push_str("]\n");
        let whole = from_str_with::<Value>(&src, &untrusted);
        let doc = Document::parse_with(src, &untrusted).unwrap();
        let node = doc.get("n").unwrap().deserialize::<Value>();
        match whole {
            Ok(v) => {
                assert_eq!(node.unwrap(), v["n"], "{aliases} aliases");
                accepted += 1;
            }
            Err(e) => {
                // Both directions: the node read refuses too.
                assert_eq!(e.code(), code::ALIAS_BUDGET_EXCEEDED);
                assert_eq!(
                    node.unwrap_err().code(),
                    code::ALIAS_BUDGET_EXCEEDED,
                    "{aliases} aliases"
                );
                refused += 1;
            }
        }
    }
    assert!(accepted > 0 && refused > 0, "{accepted} {refused}");

    // The budget counts incrementally: the budget at an alias is based on the
    // own nodes before it, so a large collection *after* the node does not
    // raise it (the same shape).
    let mut src = String::from("a: &a [");
    src.push_str(&vec!["0"; 1000].join(", "));
    src.push_str("]\nn: [");
    src.push_str(&vec!["*a"; 2000].join(", "));
    src.push_str("]\nbig: [");
    src.push_str(&vec!["0"; 1_000_000].join(", "));
    src.push_str("]\n");
    assert_eq!(
        from_str::<Value>(&src).unwrap_err().code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
    let doc = Document::parse(src).unwrap();
    assert_eq!(
        doc.to_value().unwrap_err().code(),
        code::ALIAS_BUDGET_EXCEEDED
    );
    let e = doc.get("n").unwrap().deserialize::<Value>().unwrap_err();
    assert_eq!(e.code(), code::ALIAS_BUDGET_EXCEEDED);
    // A large collection *before* the node raises it for both.
    let mut src = String::from("big: [");
    src.push_str(&vec!["0"; 300_000].join(", "));
    src.push_str("]\na: &a [");
    src.push_str(&vec!["0"; 1000].join(", "));
    src.push_str("]\nn: [");
    src.push_str(&vec!["*a"; 2000].join(", "));
    src.push_str("]\n");
    assert!(from_str::<oneyaml::Value>(&src).is_ok());
    let doc = Document::parse(src).unwrap();
    let n: Vec<Vec<u8>> = doc.get("n").unwrap().deserialize().unwrap();
    assert_eq!(n.len(), 2000);
}

/// The own-node count before a node is cached per edit
/// state, so repeated reads that need it cost O(log n), not O(document).
#[test]
fn prefix_counts_are_not_recomputed_per_read() {
    use std::time::{Duration, Instant};

    let source = |n: usize| {
        let mut s = String::from("a: &a [");
        s.push_str(&vec!["0"; 100].join(", "));
        s.push_str("]\nbig: [");
        s.push_str(&vec!["0"; n].join(", "));
        s.push_str("]\nn: [");
        s.push_str(&vec!["*a"; 50].join(", "));
        s.push_str("]\n");
        s
    };
    // The budget scales with the count only (no floor), so every read of
    // `n` needs the count before it.
    let settings = Settings::new().limits(Limits::default().alias_budget_ratio(10, 0));
    let per_read = |n: usize| {
        let doc = Document::parse_with(source(n), &settings).unwrap();
        let node = doc.get("n").unwrap();
        let _first: Value = node.deserialize().unwrap();
        (0..20)
            .map(|_| {
                let t = Instant::now();
                let v: Vec<Vec<u8>> = node.deserialize().unwrap();
                assert_eq!(v.len(), 50);
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let small = per_read(10_000);
    let large = per_read(1_000_000);
    eprintln!("read after the first: 10k {small:?}, 1M {large:?}");
    assert!(
        large <= small.saturating_mul(4) + Duration::from_millis(5),
        "{small:?} vs {large:?}"
    );
}

/// The scan for what a node's aliases reach is linear in
/// the node plus the reached content, so a node read costs about what the
/// whole-document read costs, whatever the nesting of the anchors it
/// reaches. Minimum of several runs; 4x plus 20 ms allowed.
#[test]
fn reaching_nested_and_deep_anchors_is_linear() {
    use serde::de::IgnoredAny;
    use std::time::{Duration, Instant};

    fn best(f: &dyn Fn()) -> Duration {
        (0..3)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .min()
            .unwrap()
    }
    // (1) Nested anchors, each aliased: `x: &a0 [0 x1000, &a1 [&a2 [...]]]`
    //     and `n: [*a0, ..., *a{d-1}]`.
    let nested = |d: usize| {
        let mut s = String::from("x: &a0 [");
        s.push_str(&vec!["0"; 1000].join(", "));
        s.push_str(", ");
        for i in 1..d {
            s.push_str(&format!("&a{i} ["));
        }
        s.push('0');
        s.push_str(&"]".repeat(d));
        s.push_str("\nn: [");
        let aliases: Vec<String> = (0..d).map(|i| format!("*a{i}")).collect();
        s.push_str(&aliases.join(", "));
        s.push_str("]\n");
        s
    };
    // (2) Many aliases to one deep anchor (refused by the budget in both).
    let deep = |aliases: usize| {
        let mut s = format!("x: &a {}0{}\nn: [", "[".repeat(1000), "]".repeat(1000));
        s.push_str(&vec!["*a"; aliases].join(", "));
        s.push_str("]\n");
        s
    };
    // (3) Many aliases deep inside the node (each alias's target
    //     is found through the known document, not its ancestors).
    let deep_aliases = |aliases: usize| {
        let mut s = format!("a: &a 1\nn: {}", "[".repeat(1000));
        s.push_str(&vec!["*a"; aliases].join(", "));
        s.push_str(&"]".repeat(1000));
        s.push('\n');
        s
    };
    let run = move || {
        for (name, src) in [
            ("nested d=800", nested(800)),
            ("deep x100k", deep(100_000)),
            ("deep aliases x40k", deep_aliases(40_000)),
        ] {
            let doc = Document::parse(src.clone()).unwrap();
            let node = doc.get("n").unwrap();
            let whole = best(&|| {
                let _ = from_str::<IgnoredAny>(&src);
            });
            let local = best(&|| {
                let _ = node.deserialize::<IgnoredAny>();
            });
            // Both agree on the outcome.
            assert_eq!(
                from_str::<IgnoredAny>(&src)
                    .map(|_| ())
                    .map_err(|e| e.code()),
                node.deserialize::<IgnoredAny>()
                    .map(|_| ())
                    .map_err(|e| e.code()),
                "{name}"
            );
            eprintln!("{name}: whole read {whole:?}, node read {local:?}");
            assert!(
                local <= whole.saturating_mul(4) + Duration::from_millis(20),
                "{name}: node read {local:?} vs whole read {whole:?}"
            );
        }
        // And the nested shape grows with its expansion, not faster.
        let doc = Document::parse(nested(200)).unwrap();
        let small = best(&|| {
            let _ = doc.get("n").unwrap().deserialize::<IgnoredAny>();
        });
        let doc = Document::parse(nested(800)).unwrap();
        let large = best(&|| {
            let _ = doc.get("n").unwrap().deserialize::<IgnoredAny>();
        });
        eprintln!("nested: d=200 {small:?}, d=800 {large:?}");
    };
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}
