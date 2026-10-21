//! What key lookups and edit validation read, counted instead of timed:
//!
//! * a lookup of a short key never decodes a long key it passes;
//! * a wide mapping's index is built after a few reads, not on the first,
//!   and `is_indexed` says so without building it;
//! * the text re-read to validate an edit depends on the edited region, not
//!   on the size of the document;
//! * keys are found by the string they mean, however they are written.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::fmt::Write as _;

use oneyaml::{Compat, Document, Settings, Value};
use oneyaml_syntax::tree::{take_decoded_bytes, take_reread_bytes};

/// A document with one explicit key of `big` bytes (a double-quoted scalar),
/// then `n` short entries `k0: 0` ….
fn with_long_key(big: usize, n: usize) -> String {
    let mut s = String::from("? \"");
    s.push_str(&"a".repeat(big));
    s.push_str("\"\n: huge\n");
    for i in 0..n {
        writeln!(s, "k{i}: {i}").unwrap();
    }
    s
}

#[test]
fn a_lookup_does_not_decode_a_long_key_it_passes() {
    // Narrow (a scan of the entries on every lookup) and wide (a lookup
    // index, built after a few reads: the build digests every key, the long
    // one too, once); 4 MB key.
    for (n, reads) in [(5usize, 12usize), (200, 40)] {
        let mut doc = Document::parse(with_long_key(4_000_000, n)).unwrap();
        for _ in 0..12 {
            let _ = doc.get("k1");
        }
        if n > 100 {
            assert!(doc.root().unwrap().as_mapping().unwrap().is_indexed());
        }
        let _ = take_decoded_bytes();
        for r in 0..reads {
            let k = format!("k{}", r % n);
            assert!(doc.get(k.as_str()).is_some());
            assert!(doc.get("absent").is_none());
        }
        let read = take_decoded_bytes();
        assert!(
            read < 4_000,
            "{n} short keys: {read} bytes decoded by {reads} lookups"
        );
        // An edit looks its key up too, and checks nothing of the long key.
        doc.set_path("k3", 77).unwrap();
        let _ = doc.remove_path("k1").unwrap();
        let read = take_decoded_bytes();
        assert!(
            read < 4_000,
            "{n} short keys: {read} bytes decoded by edits"
        );
        assert_eq!(doc.get("k3").unwrap().text(), "77");
        assert!(doc.get("k1").is_none());
    }
}

#[test]
fn a_long_key_is_still_found_by_what_it_means() {
    // The long key is the string of its content, found by that string
    // (decoding it, as the comparison needs), and no other.
    let text = "b".repeat(5_000);
    let src =
        format!("? '{text}'\n: one\n? \"{text}c\\x64\"\n: two\n? |\n  {text}\n: three\nk: 4\n");
    let doc = Document::parse(src).unwrap();
    assert_eq!(doc.get(text.as_str()).unwrap().text(), "one");
    assert_eq!(doc.get(format!("{text}cd")).unwrap().text(), "two");
    assert_eq!(doc.get(format!("{text}\n")).unwrap().text(), "three");
    assert!(doc.get(format!("{text}c")).is_none());
    assert!(doc.get(&text[1..]).is_none());
    assert_eq!(doc.get("k").unwrap().text(), "4");
    assert_eq!(doc.check_lookup_indexes().unwrap(), 1);
}

#[test]
fn a_wide_mapping_is_indexed_after_a_few_reads() {
    let mut s = String::new();
    for i in 0..100 {
        writeln!(s, "k{i}: {i}").unwrap();
    }
    let doc = Document::parse(s).unwrap();
    let root = doc.root().unwrap().as_mapping().unwrap();
    assert!(!root.is_indexed());
    assert_eq!(root.get("k99").unwrap().text(), "99");
    assert!(!root.is_indexed(), "one read is a scan");
    assert!(!root.is_indexed(), "asking builds nothing");
    for _ in 0..20 {
        assert_eq!(root.get("k50").unwrap().text(), "50");
    }
    assert!(root.is_indexed());
    // A narrow mapping never has one.
    let small = Document::parse("a: 1\nb: 2\n").unwrap();
    let m = small.root().unwrap().as_mapping().unwrap();
    for _ in 0..30 {
        assert!(m.get("a").is_some());
    }
    assert!(!m.is_indexed());
}

/// `n` records as the benchmark's `records` shape writes them.
fn records(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        write!(
            s,
            "- id: {i}\n  name: Person {i}\n  email: p{i}@example.com\n  active: true\n  \
             score: {i}.5\n  tags:\n    - a{i}\n    - b\n  address:\n    city: Town\n    \
             zip: '{i:05}'\n    geo:\n      lat: 1.5\n      lng: 2.5\n  note: ~\n"
        )
        .unwrap();
    }
    s
}

/// Mapping of `n` entries, then a nested section, as a Helm values file.
fn values(n: usize) -> String {
    let mut s = String::from("global:\n  registry: example.com\n");
    for i in 0..n {
        write!(
            s,
            "\nsvc{i}:\n  # -- Whether svc{i} is enabled\n  enabled: true\n  replicaCount: 2\n  \
             image:\n    repository: example.com/svc{i}\n    tag: \"\"\n  resources: {{}}\n  \
             annotations: {{}}\n"
        )
        .unwrap();
    }
    s
}

fn wide(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "key{i}: plain text {i}").unwrap();
    }
    s
}

/// One edit of a script.
type Step = fn(&mut Document);

/// The bytes re-read to check each edit of `steps` on a document, one edit
/// at a time.
fn reread(src: String, steps: &[Step]) -> Vec<u64> {
    let mut doc = Document::parse(src).unwrap();
    let _ = take_reread_bytes();
    steps
        .iter()
        .map(|step| {
            step(&mut doc);
            take_reread_bytes()
        })
        .collect()
}

const RECORDS: &[Step] = &[
    |d| _ = d.set_path("[1].name", "Renamed Person").unwrap(),
    |d| _ = d.set_path("[2].score", 99.5).unwrap(),
    |d| _ = d.set_path("[2].active", false).unwrap(),
    |d| _ = d.remove_path("[3].address.geo").unwrap(),
    |d| _ = d.root_mut().unwrap().push(7).unwrap(),
];

const VALUES: &[Step] = &[
    |d| _ = d.set_path("svc0.replicaCount", 5).unwrap(),
    |d| _ = d.set_path("svc0.enabled", false).unwrap(),
    |d| _ = d.set_path("global.registry", "mirror.example.com").unwrap(),
    |d| _ = d.root_mut().unwrap().insert("added", 1).unwrap(),
];

const WIDE: &[Step] = &[
    |d| _ = d.set_path("key0", 42).unwrap(),
    |d| _ = d.remove_path("key3").unwrap(),
    |d| _ = d.set_path("key7", "x y").unwrap(),
    |d| _ = d.root_mut().unwrap().insert("added", 1).unwrap(),
];

#[test]
fn validating_an_edit_reads_the_edited_region_not_the_document() {
    // The same script on a small and on a document 100 times its size: each
    // edit re-reads the same few dozen bytes.
    for (kind, steps, small, large) in [
        ("records", RECORDS, records(20), records(2_000)),
        ("values", VALUES, values(10), values(1_000)),
        ("wide", WIDE, wide(100), wide(10_000)),
    ] {
        let a = reread(small, steps);
        let b = reread(large, steps);
        assert_eq!(a.len(), b.len(), "{kind}");
        assert!(a.len() >= 4, "{kind}: {a:?}");
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!(
                *y < 200,
                "{kind} edit {i}: re-read {y} bytes of a large document"
            );
            assert!(
                y.saturating_sub(*x) <= 16,
                "{kind} edit {i}: {x} bytes in the small document, {y} in the large"
            );
        }
    }
}

/// The keys of the key-shape documents below, written every way a plain,
/// single-quoted, double-quoted, tagged and multi-line key can be.
const KEYS: &[&str] = &[
    "abc",
    "Abc",
    "k_1",
    "n",
    "no",
    "null",
    "~",
    "true",
    "yes",
    "on",
    "off",
    "12",
    "0x1F",
    "1.5",
    ".inf",
    "a b",
    "é",
    "x y z",
    "'abc'",
    "'it''s'",
    "''",
    "'12'",
    "'a\\b'",
    "'<<'",
    "\"abc\"",
    "\"a\\x41\"",
    "\"tab\\there\"",
    "\"\\u00e9\"",
    "\"12\"",
    "\"a b\"",
    "\"\"",
    "\"<<\"",
    "&anc named",
    "!!str 12",
    "!!str true",
    "!local foo",
    "!!int 12",
    "? multi\n  line",
    "? \"fold\n  ed\"",
    "? 'sq\n\n  x'",
    "? |\n  lit",
    "? >\n  fol\n  ded",
    "? abc\n  def",
    "? \"a  \n  b\"",
    "? 'a  \n  b'",
    "? a  \n  b",
    "? a\t \n  b",
    "? \"a\t\t\n  b\"",
    "? \"a  \n\n  b\"",
    "? \"a \n  b c\"",
    "? 'a  \n\n  b'",
    "? \"a  \r\n  b\"",
    "? 'abc'\n",
    "? \"a\\\n  b\"",
];

/// A document with every key of `keys` (explicit ones keep their `?`), each
/// with its own value, preceded by `filler` entries.
fn key_document(keys: &[&str], filler: usize) -> String {
    let mut s = String::new();
    for i in 0..filler {
        writeln!(s, "zz{i:03}: {i}").unwrap();
    }
    for (i, k) in keys.iter().enumerate() {
        if k.starts_with("? ") {
            writeln!(s, "{}\n: v{i}", k.trim_end()).unwrap();
        } else {
            writeln!(s, "{k}: v{i}").unwrap();
        }
    }
    s
}

/// What a string key is in the document's `Value`.
fn oracle(v: &Value, key: &str) -> Option<Value> {
    v.as_mapping()?.get(&Value::String(key.to_owned())).cloned()
}

fn variants(base: &str) -> Vec<String> {
    let mut out = vec![base.to_owned(), format!("{base}x"), format!("x{base}")];
    out.push(format!("{base} "));
    out.push(base.to_uppercase());
    out.push(
        base.chars()
            .take(base.chars().count().saturating_sub(1))
            .collect(),
    );
    out.push(base.replace('\n', " "));
    out
}

#[test]
fn keys_are_found_by_the_string_they_mean_however_written() {
    let settings = [
        Settings::new(),
        Settings::new().merge_keys(true),
        Settings::new().yaml11_booleans(true),
        Compat::yaml_1_1(),
        Compat::serde_yaml(),
        Settings::new().schema(oneyaml::Schema::Failsafe),
    ];
    let mut parsed = 0usize;
    for (si, s) in settings.iter().enumerate() {
        for filler in [0usize, 80] {
            // Each key alone (so one bad key does not hide the rest) and all
            // together.
            let mut docs: Vec<String> = KEYS.iter().map(|k| key_document(&[k], filler)).collect();
            docs.push(key_document(KEYS, filler));
            for src in docs {
                let Ok(doc) = Document::parse_with(src.clone(), s) else {
                    continue;
                };
                let Ok(value) = doc.to_value() else { continue };
                parsed += 1;
                let mut queries: Vec<String> = ["<<", "", "abc", "12", "null", "true"]
                    .map(str::to_owned)
                    .into();
                if let Some(m) = value.as_mapping() {
                    for (k, _) in m {
                        if let Value::String(k) = k {
                            queries.extend(variants(k));
                        }
                    }
                }
                // Enough reads for a wide mapping to be indexed part way
                // through: the scan and the index answer alike.
                for round in 0..2 {
                    for q in &queries {
                        let got = doc
                            .get(q.as_str())
                            .map(|n| n.deserialize::<Value>().unwrap());
                        assert_eq!(
                            got,
                            oracle(&value, q),
                            "settings {si}, key {q:?} in {src:?}"
                        );
                    }
                    if filler > 0 && round == 0 {
                        for _ in 0..12 {
                            let _ = doc.get("zz000");
                        }
                    }
                }
                if filler > 0 {
                    assert!(
                        doc.root().unwrap().as_mapping().unwrap().is_indexed(),
                        "{src:?}"
                    );
                }
                doc.check_lookup_indexes().unwrap();
            }
        }
    }
    assert!(parsed > 200, "only {parsed} key documents parsed");
}

/// White space before a line break is dropped by the fold, so these keys
/// are `a b` and `a\nb`: found by that, in block and flow keys, LF and CRLF,
/// and an edit through the path does not add a second key.
#[test]
fn keys_folded_over_trailing_white_space_are_found() {
    let cases: &[(&str, &str)] = &[
        ("? \"a  \n  b\"\n: 1\n", "a b"),
        ("? 'a  \n  b'\n: 1\n", "a b"),
        ("? a  \n  b\n: 1\n", "a b"),
        ("? a\t \n  b\n: 1\n", "a b"),
        ("? \"a\t\t\n  b\"\n: 1\n", "a b"),
        ("? \"a  \n\n  b\"\n: 1\n", "a\nb"),
        ("{ ? \"a  \n b\" : 1 }\n", "a b"),
        ("? \"a  \r\n  b\"\n: 1\n", "a b"),
        ("? a  \r\n  b\n: 1\n", "a b"),
    ];
    for (src, key) in cases {
        for filler in [0usize, 80] {
            let mut text = String::new();
            // (a flow mapping stays alone)
            if filler > 0 && !src.starts_with('{') {
                for i in 0..filler {
                    writeln!(text, "zz{i:03}: {i}").unwrap();
                }
            }
            text.push_str(src);
            let mut doc = Document::parse(text.as_str()).unwrap();
            for _ in 0..12 {
                assert_eq!(doc.get(*key).unwrap().text(), "1", "{src:?} for {key:?}");
            }
            doc.set_path(&oneyaml::path![*key], 2).unwrap();
            assert_eq!(doc.get(*key).unwrap().text(), "2", "{src:?}");
            assert!(
                doc.diagnostics().is_empty(),
                "{src:?}: {:?}",
                doc.diagnostics()
            );
        }
    }
}

/// Many long keys that share a long prefix (explicit keys): once the index
/// stands, a lookup compares and decodes the candidate it finds, about the
/// bytes of the query, never the prefix of every key of the mapping.
#[test]
fn lookups_among_many_long_keys_cost_the_query_not_the_mapping() {
    let (n, k) = (4_000usize, 1_105usize);
    let prefix = "x".repeat(1_100);
    let mut src = String::new();
    for i in 0..n {
        writeln!(src, "? {prefix}{i:05}\n: {i}").unwrap();
    }
    let mut doc = Document::parse(src).unwrap();
    // Scanning keys this long costs about what the build does: the index
    // comes after one scan, not eight.
    for _ in 0..2 {
        let _ = doc.get(format!("{prefix}00000").as_str());
    }
    assert!(doc.root().unwrap().as_mapping().unwrap().is_indexed());
    let _ = (take_decoded_bytes(), Document::take_key_compared_bytes());
    let lookups = 200u64;
    for i in 0..lookups {
        let key = format!("{prefix}{:05}", i * 19);
        assert_eq!(doc.get(key.as_str()).unwrap().text(), (i * 19).to_string());
        assert!(doc.get(format!("{prefix}9{:04}", i).as_str()).is_none());
    }
    let work = take_decoded_bytes() + Document::take_key_compared_bytes();
    let per_lookup = work / (2 * lookups);
    assert!(
        per_lookup <= 3 * k as u64,
        "{per_lookup} bytes compared or decoded per lookup among {n} keys"
    );
    // The edits do the same lookups.
    for i in 0..50 {
        doc.set_path(format!("{prefix}{:05}", i * 37).as_str(), 7)
            .unwrap();
    }
    let work = take_decoded_bytes() + Document::take_key_compared_bytes();
    assert!(
        work / 50 <= 12 * k as u64,
        "{} bytes compared or decoded per edit among {n} keys",
        work / 50
    );
}
