//! `NodeRef`'s typed accessors (`as_str`, `as_bool`, `as_i64`, `as_u64`,
//! `as_f64`, `is_null`) read every node as the `Value` of that node does — the
//! `Value` read through serde (`NodeRef::deserialize::<Value>`, the same
//! reading `from_str` makes), not through the accessors' own path — over every
//! yaml-test-suite case and a corpus of scalar forms, under the default
//! settings, `Compat::yaml_1_1()`, the JSON and Failsafe schemas,
//! `honor_version_directive`, `merge_keys` and `Compat::serde_yaml()` (and the
//! handle API), and over the inputs as `Document::parse_recovering` reads them
//! (every node that is not an error node).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};

use oneyaml::{Compat, Document, NodeKind, NodeRef, Schema, Settings, Stream, Value};

fn in_workspace() -> bool {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    fs::read_to_string(root).is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
}

/// Every `in.yaml` of the vendored suite (none outside the workspace).
fn suite() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-suite/data/yaml-test-suite");
    if !root.is_dir() {
        assert!(!in_workspace(), "yaml-test-suite data missing");
        eprintln!("skipped: yaml-test-suite data not found");
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut pending: Vec<PathBuf> = vec![root.clone()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                pending.push(p);
            }
        }
        let input = dir.join("in.yaml");
        if input.is_file() {
            let name = dir.strip_prefix(&root).unwrap().display().to_string();
            out.push((name, fs::read_to_string(input).unwrap()));
        }
    }
    out.sort();
    out
}

/// Scalar forms the suite has few of: numbers in every radix, 1.1-only
/// forms, tags (core, custom, non-specific), quoting, aliases.
const FORMS: &str = "\
a: 0x1F
b: 0o17
c: 017
d: 0b101
e: 1_000
f: 190:20:30
g: yes
h: Off
i: ~
j: null
k:
l: .inf
m: -.INF
n: .nan
o: 1e3
p: 18446744073709551615
q: 18446744073709551616
r: -9223372036854775808
s: -9223372036854775809
t: !!str 80
u: !!int '80'
v: !!float 1
w: !custom x
x: ! 80
y: '80'
z: \"tab\\there\"
aa: &n 42
ab: *n
ac: !!null ''
ad: !!bool true
ae: [1, 'two', 3.0, ~, true]
af: {k: v}
ag: |
  block
ah: +12
ai: -0
aj: 1.
ak: .5
al: !!binary aGk=
am: TRUE
an: 0x
";

fn same(name: &str, node: NodeRef<'_>, want: &Value) {
    let at = format!("{name} {node:?}");
    assert_eq!(node.as_str().as_deref(), want.as_str(), "as_str {at}");
    assert_eq!(node.as_bool(), want.as_bool(), "as_bool {at}");
    assert_eq!(node.as_i64(), want.as_i64(), "as_i64 {at}");
    assert_eq!(node.as_u64(), want.as_u64(), "as_u64 {at}");
    assert_eq!(
        node.as_f64().map(f64::to_bits),
        want.as_f64().map(f64::to_bits),
        "as_f64 {at}"
    );
    assert_eq!(node.is_null(), want.is_null(), "is_null {at}");
}

/// Every node of `doc` (keys included), checked; returns how many. Error
/// nodes (of a recovered document) are not compared, only seen to read
/// as nothing.
fn sweep(name: &str, doc: &Document) -> usize {
    let mut checked = 0usize;
    let mut stack: Vec<NodeRef<'_>> = doc.root().into_iter().collect();
    // An explicit stack.
    while let Some(n) = stack.pop() {
        match n.kind() {
            NodeKind::Mapping => {
                for (k, v) in n.as_mapping().unwrap().iter() {
                    stack.push(k);
                    stack.push(v);
                }
            }
            NodeKind::Sequence => stack.extend(n.as_sequence().unwrap().iter()),
            NodeKind::Error => {
                assert_eq!(n.as_str(), None, "{name} {n:?}");
                assert_eq!((n.as_bool(), n.as_u64()), (None, None), "{name} {n:?}");
                assert!(!n.is_null(), "{name} {n:?}");
                continue;
            }
            _ => {}
        }
        match n.deserialize::<Value>() {
            Ok(v) => same(name, n, &v),
            // What serde cannot read has no meaning to read either.
            Err(e) => {
                assert_eq!(n.as_str(), None, "{name} {n:?}: {e}");
                assert_eq!(n.as_bool(), None, "{name} {n:?}: {e}");
                assert_eq!(n.as_i64(), None, "{name} {n:?}: {e}");
                assert_eq!(n.as_u64(), None, "{name} {n:?}: {e}");
                assert_eq!(n.as_f64(), None, "{name} {n:?}: {e}");
                assert!(!n.is_null(), "{name} {n:?}: {e}");
            }
        }
        checked = checked.saturating_add(1);
    }
    checked
}

/// The suite and the scalar forms (also under `%YAML 1.1`).
fn inputs() -> Vec<(String, String)> {
    let mut inputs = suite();
    inputs.push(("forms".into(), FORMS.into()));
    inputs.push(("forms-1.1".into(), format!("%YAML 1.1\n---\n{FORMS}")));
    inputs
}

/// Every input that parses strictly under `settings`, every document.
fn run(settings: &Settings) -> usize {
    let mut checked = 0usize;
    for (name, src) in &inputs() {
        let Ok(stream) = Stream::parse_with(src.as_str(), settings) else {
            continue;
        };
        for doc in stream.documents() {
            checked = checked.saturating_add(sweep(name, &doc));
        }
    }
    checked
}

#[test]
fn typed_reads_match_value_default() {
    let n = run(&Settings::new());
    assert!(n > 1500 || !in_workspace(), "only {n} nodes checked");
}

#[test]
fn typed_reads_match_value_yaml_1_1() {
    let n = run(&Compat::yaml_1_1());
    assert!(n > 1500 || !in_workspace(), "only {n} nodes checked");
}

/// The other schemas and the settings that change a reading.
fn settings_swept() -> [(&'static str, Settings); 7] {
    [
        ("default", Settings::new()),
        ("yaml_1_1", Compat::yaml_1_1()),
        ("json", Settings::new().schema(Schema::Json)),
        ("failsafe", Settings::new().schema(Schema::Failsafe)),
        (
            "honor_version_directive",
            Settings::new().honor_version_directive(true),
        ),
        ("merge_keys", Settings::new().merge_keys(true)),
        ("serde_yaml", Compat::serde_yaml()),
    ]
}

#[test]
fn typed_reads_match_value_json() {
    // Strict JSON refuses most plain scalars (`hello`), so fewer inputs
    // parse; the recovering sweep below reads the rest.
    let n = run(&Settings::new().schema(Schema::Json));
    assert!(n > 100 || !in_workspace(), "only {n} nodes checked");
}

#[test]
fn typed_reads_match_value_failsafe() {
    let n = run(&Settings::new().schema(Schema::Failsafe));
    assert!(n > 1500 || !in_workspace(), "only {n} nodes checked");
}

#[test]
fn typed_reads_match_value_honor_version_directive() {
    let n = run(&Settings::new().honor_version_directive(true));
    assert!(n > 1500 || !in_workspace(), "only {n} nodes checked");
}

#[test]
fn typed_reads_match_value_merge_keys() {
    let n = run(&Settings::new().merge_keys(true));
    assert!(n > 1500 || !in_workspace(), "only {n} nodes checked");
}

#[test]
fn typed_reads_match_value_serde_yaml() {
    let n = run(&Compat::serde_yaml());
    assert!(n > 1500 || !in_workspace(), "only {n} nodes checked");
}

/// Every input read by `Document::parse_recovering` (and `_with`, under
/// each swept setting), errors and all: the nodes that are not error nodes
/// read as their `Value`.
#[test]
fn typed_reads_match_value_recovered() {
    let inputs = inputs();
    for (label, settings) in settings_swept() {
        let mut checked = 0usize;
        let mut recovered = 0usize;
        for (name, src) in &inputs {
            let parsed = if label == "default" {
                Document::parse_recovering(src.as_str())
            } else {
                Document::parse_recovering_with(src.as_str(), &settings)
            };
            if parsed.has_errors() {
                recovered = recovered.saturating_add(1);
            }
            let name = format!("{label} {name}");
            checked = checked.saturating_add(sweep(&name, parsed.value()));
        }
        assert!(
            (checked > 1500 && recovered > 50) || !in_workspace(),
            "{label}: only {checked} nodes checked, {recovered} recovered inputs"
        );
    }
}

#[test]
fn typed_reads_by_hand() {
    let doc = Document::parse(FORMS).unwrap();
    let get = |k: &str| doc.get(k).unwrap();
    assert_eq!(get("a").as_u64(), Some(31));
    assert_eq!(get("c").as_u64(), Some(17)); // Core: decimal 17
    assert_eq!(get("g").as_bool(), None);
    assert_eq!(get("g").as_str().as_deref(), Some("yes"));
    assert!(get("i").is_null() && get("j").is_null() && get("k").is_null());
    assert_eq!(get("p").as_u64(), Some(u64::MAX));
    assert_eq!(get("p").as_i64(), None);
    assert_eq!(get("q").as_u64(), None);
    assert_eq!(get("r").as_i64(), Some(i64::MIN));
    assert_eq!(get("t").as_str().as_deref(), Some("80"));
    assert_eq!(get("u").as_u64(), Some(80));
    assert_eq!(get("v").as_f64(), Some(1.0));
    // A custom tag makes a `Value::Tagged`: no accessor sees through it.
    assert_eq!(get("w").as_str(), None);
    assert_eq!(get("x").as_str().as_deref(), Some("80"));
    assert_eq!(get("y").as_u64(), None);
    assert_eq!(get("z").as_str().as_deref(), Some("tab\there"));
    assert_eq!(get("ab").as_u64(), Some(42)); // the alias's target
    assert_eq!(get("ae").as_str(), None); // a collection
    assert_eq!(get("ag").as_str().as_deref(), Some("block\n"));

    let old = Document::parse_with(FORMS, &Compat::yaml_1_1()).unwrap();
    let get = |k: &str| old.get(k).unwrap();
    assert_eq!(get("c").as_u64(), Some(15)); // legacy octal
    assert_eq!(get("d").as_u64(), Some(5));
    assert_eq!(get("f").as_u64(), Some(685_230));
    assert_eq!(get("g").as_bool(), Some(true));
    assert_eq!(get("h").as_bool(), Some(false));
}

#[test]
fn typed_reads_of_a_restricted_document() {
    let parsed = Document::parse_recovering("a: 'unclosed\nb: 2\n");
    let doc = parsed.value();
    let a = doc.get("a").unwrap();
    assert_eq!(a.kind(), NodeKind::Error);
    assert_eq!((a.as_str(), a.as_u64(), a.is_null()), (None, None, false));
}

/// A recovered scalar holding an error (two anchors, suite
/// 4JVG) reads as nothing, as its `Value` read fails.
#[test]
fn typed_reads_of_a_scalar_holding_an_error() {
    let parsed = Document::parse_recovering("top: &a\n  &b val\n");
    assert!(parsed.has_errors());
    let top = parsed.value().get("top").unwrap();
    assert_eq!(top.kind(), NodeKind::Scalar);
    assert!(top.deserialize::<Value>().is_err());
    assert_eq!(
        (top.as_str(), top.as_u64(), top.is_null()),
        (None, None, false)
    );
}

/// `as_str` borrows the content when it is a slice of the
/// text, and owns it only when it was decoded.
#[test]
fn as_str_borrows_what_it_can() {
    use std::borrow::Cow;
    let doc = Document::parse(
        "plain: web\nsq: 'web'\ndq: \"web\"\nesc: \"a\\tb\"\nfold: a\n  b\nblock: |\n  x\n",
    )
    .unwrap();
    let get = |k: &str| doc.get(k).unwrap().as_str().unwrap();
    assert!(matches!(get("plain"), Cow::Borrowed("web")));
    assert!(matches!(get("sq"), Cow::Borrowed("web")));
    assert!(matches!(get("dq"), Cow::Borrowed("web")));
    assert!(matches!(get("esc"), Cow::Owned(s) if s == "a\tb"));
    assert!(matches!(get("fold"), Cow::Owned(s) if s == "a b"));
    assert_eq!(get("block"), "x\n");
    // Through an alias: the anchored scalar's content, still borrowed.
    let doc = Document::parse("a: &n name\nb: *n\n").unwrap();
    assert!(matches!(
        doc.get("b").unwrap().as_str(),
        Some(Cow::Borrowed("name"))
    ));
}

#[test]
fn typed_reads_of_an_undefined_alias() {
    let parsed = Document::parse_recovering("a: *nowhere\n");
    let a = parsed.value().get("a").unwrap();
    assert_eq!((a.as_str(), a.as_u64(), a.is_null()), (None, None, false));
}
