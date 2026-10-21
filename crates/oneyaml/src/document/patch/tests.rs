//! Tests of how much work patches do, counted rather than timed, and of the
//! key index against a plain scan.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::apply::{Run, apply_ops_in};
use super::merge::{apply_merge_in, without_nulls_noting};
use super::textindex::{Hit, Texts};
use super::{PatchError, PatchErrorKind, PatchOp, ReadCache};
use crate::{Document, DuplicateKeys, Limits, Mapping, NodeId, NodeKind, NodeRef, Settings, Value};

/// An object of `nulls` null members and `kept` others, nested `depth` deep.
fn null_heavy(nulls: usize, kept: usize, depth: usize) -> Value {
    let mut m = Mapping::new();
    for i in 0..nulls {
        m.insert(Value::from(format!("n{i}")), Value::Null);
    }
    for i in 0..kept {
        m.insert(Value::from(format!("v{i}")), Value::from(i as i64));
    }
    if depth > 0 {
        m.insert(
            Value::from("inner"),
            null_heavy(nulls, kept, depth.saturating_sub(1)),
        );
    }
    Value::from(m)
}

#[test]
fn removing_nulls_visits_each_member_once_and_writes_each_kept_one_once() {
    for scale in [1_000usize, 4_000, 16_000] {
        let patch = null_heavy(scale, scale / 10, 2);
        let (mut visited, mut kept) = (0usize, 0usize);
        let out = without_nulls_noting(&patch, &mut |keep| {
            visited += 1;
            kept += usize::from(keep);
        });
        // Three nested objects, each with `scale` nulls and `scale / 10` kept
        // members, and the `inner` member of the first two.
        let members = 3 * (scale + scale / 10) + 2;
        assert_eq!(visited, members, "scale {scale}");
        assert_eq!(kept, 3 * (scale / 10) + 2, "scale {scale}");
        let m = out.as_mapping().unwrap();
        assert_eq!(m.len(), scale / 10 + 1);
        assert!(m.iter().all(|(_, v)| !v.is_null()));
    }
}

fn lastwins() -> Settings {
    Settings::new().duplicate_keys(DuplicateKeys::LastWins)
}

/// What a lookup found, without the borrow.
#[derive(Debug, PartialEq)]
enum Found {
    Absent,
    Ambiguous,
    One(NodeId),
}

fn lookup(doc: &Document, texts: &mut Texts, token: &str) -> Found {
    let map = doc.root().unwrap().as_mapping().unwrap();
    match texts.find(doc, map, token).unwrap() {
        Hit::Absent => Found::Absent,
        Hit::Ambiguous(_) => Found::Ambiguous,
        Hit::One { key, merged, .. } => {
            assert!(!merged);
            Found::One(key)
        }
    }
}

#[test]
fn an_index_is_built_once_and_a_lookup_reads_one_key() {
    let n = 5_000;
    let mut src = String::new();
    for i in 0..n {
        src.push_str(&format!("k{i}: v\n"));
    }
    let doc = Document::parse(src).unwrap();
    let mut texts = Texts::unlimited();
    for i in 0..n {
        assert!(matches!(
            lookup(&doc, &mut texts, &format!("k{i}")),
            Found::One(_)
        ));
        assert_eq!(
            lookup(&doc, &mut texts, &format!("absent{i}")),
            Found::Absent
        );
    }
    assert_eq!(texts.built, n);
    assert_eq!(texts.reads, n);
}

#[test]
fn keys_with_one_text_are_not_all_read_to_say_a_token_is_ambiguous() {
    // 3,000 keys that are all the text `0x10` (one meaning, repeated) and one
    // key `16` that they override.
    let n = 3_000;
    let mut src = String::from("16: first\n");
    for _ in 0..n {
        src.push_str("0x10: again\n");
    }
    let doc = Document::parse_with(src, &lastwins()).unwrap();
    let mut texts = Texts::unlimited();
    for _ in 0..n {
        assert_eq!(lookup(&doc, &mut texts, "0x10"), Found::Ambiguous);
        assert_eq!(lookup(&doc, &mut texts, "16"), Found::Ambiguous);
    }
    // At most two keys are read to see two of them, and one to see that the
    // key `16` is overridden.
    assert!(texts.reads <= 3 * n, "{}", texts.reads);
}

#[test]
fn a_key_overridden_by_later_spellings_of_its_meaning_costs_one_read() {
    // `16` and `n` more spellings of the same integer, each its own text (an
    // implicit key may be 1,024 characters at most).
    let n = 900;
    let mut src = String::from("16: first\n");
    for i in 0..n {
        src.push_str(&format!("0x{}10: again\n", "0".repeat(i + 1)));
    }
    let doc = Document::parse_with(src, &lastwins()).unwrap();
    let mut texts = Texts::unlimited();
    for _ in 0..n {
        assert_eq!(lookup(&doc, &mut texts, "16"), Found::Ambiguous);
    }
    assert_eq!(texts.reads, n);
}

// ---- the index against a plain scan -----------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A mapping of `entries` keys written in the shapes a token has to see
/// through: plain, quoted, in another radix, tagged, repeated, and aliases.
fn generated(rng: &mut Rng, entries: usize, merging: bool) -> String {
    let mut src = String::new();
    if merging {
        src.push_str("base: &base {m1: 1, m2: 2}\n");
    }
    src.push_str("anchors: {a: &x0 0, b: &x1 1, c: &x2 text}\n");
    for i in 0..entries {
        let n = rng.below(6);
        let key = match rng.below(14) {
            0 => format!("{n}"),
            1 => format!("0x{n:x}"),
            2 => format!("'{n}'"),
            3 => format!("\"{n}\""),
            4 => format!("!local {n}"),
            5 => format!("!!str {n}"),
            6 => format!("!!int \"{n}\""),
            7 => format!("*x{}", rng.below(3)),
            8 => "true".to_string(),
            9 => "~".to_string(),
            10 => "''".to_string(),
            11 => format!("k{n}"),
            12 => "text".to_string(),
            _ => format!("u{i}"),
        };
        src.push_str(&format!("{key} : v{i}\n"));
    }
    if merging {
        src.push_str("<<: *base\n");
    }
    src
}

/// The text a key answers to, or `None` for a key that has none.
fn key_text(key: NodeRef<'_>) -> Option<String> {
    if key.kind() == NodeKind::Alias {
        return Some(key.alias_target()?.as_scalar()?.value().into_owned());
    }
    Some(key.as_scalar()?.value().into_owned())
}

/// The meaning of a key, for finding the keys it overrides.
fn key_meaning(key: NodeRef<'_>) -> Option<Value> {
    if key.kind() == NodeKind::Alias {
        return key.alias_target()?.as_scalar()?.resolved().ok();
    }
    key.as_scalar()?.resolved().ok()
}

/// What a plain scan of the mapping says a token names.
fn scan(doc: &Document, token: &str, merging: bool) -> Found {
    let map = doc.root().unwrap().as_mapping().unwrap();
    let keys: Vec<NodeRef<'_>> = map
        .iter()
        .map(|(k, _)| k)
        .filter(|k| !(merging && k.is_merge_key().unwrap_or(false)))
        .collect();
    let named: Vec<NodeRef<'_>> = keys
        .iter()
        .copied()
        .filter(|k| key_text(*k).as_deref() == Some(token))
        .collect();
    match named.as_slice() {
        [] => Found::Absent,
        [one] => {
            let overridden = key_meaning(*one).is_some_and(|m| {
                keys.iter()
                    .skip_while(|k| k.id() != one.id())
                    .skip(1)
                    .any(|k| key_meaning(*k).as_ref() == Some(&m))
            });
            if overridden {
                Found::Ambiguous
            } else {
                Found::One(one.id())
            }
        }
        _ => Found::Ambiguous,
    }
}

#[test]
fn the_index_agrees_with_a_plain_scan_for_every_key() {
    let mut rng = Rng(0xA5A5_1234_DEAD_BEEF);
    let mut checked = 0usize;
    for round in 0..120 {
        // Narrow and wide mappings go through the same code.
        let entries = [3, 9, 20, 40, 90, 160][round % 6];
        let merging = round % 3 == 0;
        let src = generated(&mut rng, entries, merging);
        let settings = lastwins().merge_keys(merging);
        let Ok(doc) = Document::parse_with(src.clone(), &settings) else {
            continue;
        };
        let map = doc.root().unwrap().as_mapping().unwrap();
        let mut tokens: Vec<String> = map.iter().filter_map(|(k, _)| key_text(k)).collect();
        tokens.extend(["absent".to_string(), "99".to_string(), "m1".to_string()]);
        let mut texts = Texts::unlimited();
        for token in &tokens {
            let want = scan(&doc, token, merging);
            let got = {
                let map = doc.root().unwrap().as_mapping().unwrap();
                match texts.find(&doc, map, token).unwrap() {
                    Hit::Absent => Found::Absent,
                    Hit::Ambiguous(_) => Found::Ambiguous,
                    Hit::One { key, merged, .. } => {
                        if merged {
                            // Only the merged-in keys are not own keys.
                            assert!(token == "m1" || token == "m2", "{token}\n{src}");
                            assert_eq!(want, Found::Absent, "{token}\n{src}");
                            continue;
                        }
                        Found::One(key)
                    }
                }
            };
            assert_eq!(got, want, "token {token:?} in\n{src}");
            checked += 1;
        }
    }
    assert!(checked > 2_000, "{checked}");
}

// ---- the work of whole patches and of reads -----------------------------------

/// A mapping of `n` keys `k0`..`k{n-1}`.
fn wide(n: usize) -> String {
    (0..n).map(|i| format!("k{i}: {i}\n")).collect()
}

/// Applies `ops` as `Document::apply_patch` does, and says how many keys the
/// patch put into indexes and how many key texts it read.
fn counted(doc: &mut Document, ops: &[PatchOp]) -> (Result<(), PatchError>, usize, usize) {
    doc.prepare().unwrap();
    let mut run = Run::new(doc);
    let result = doc.atomic(|d| apply_ops_in(d, &mut run, ops));
    (result, run.texts.built, run.texts.reads)
}

#[test]
fn many_operations_on_a_wide_mapping_index_it_once_and_read_a_key_each() {
    for n in [1_000usize, 4_000] {
        let mut doc = Document::parse(wide(n)).unwrap();
        let m = n / 2;
        let mut ops = Vec::new();
        for i in 0..m {
            ops.push(PatchOp::add(format!("/n{i}"), Value::from(1)));
            ops.push(PatchOp::remove(format!("/k{i}")));
            ops.push(PatchOp::replace(format!("/k{}", n - 1 - i), Value::from(2)));
            ops.push(PatchOp::test(format!("/n{i}"), Value::from(1)));
        }
        let (result, built, reads) = counted(&mut doc, &ops);
        result.unwrap();
        // The index of the root: every key once, then every key added.
        assert_eq!(built, n + m, "width {n}");
        // One key is read to confirm a name that is there (the remove, the
        // replace and the test); a name that is not there reads none.
        assert_eq!(reads, 3 * m, "width {n}");
        assert!(reads <= ops.len());
    }
}

#[test]
fn a_wide_merge_patch_indexes_the_mapping_once_and_reads_a_key_per_member() {
    for n in [1_000usize, 4_000] {
        let mut doc = Document::parse(wide(n)).unwrap();
        let m = n / 2;
        let mut patch = Mapping::new();
        for i in 0..m {
            patch.insert(Value::from(format!("k{i}")), Value::Null);
            patch.insert(Value::from(format!("n{i}")), Value::from(1));
            patch.insert(Value::from(format!("k{}", n - 1 - i)), Value::from(2));
        }
        let patch = Value::from(patch);
        doc.prepare().unwrap();
        let mut run = Run::new(&doc);
        doc.atomic(|d| apply_merge_in(d, &mut run, &patch)).unwrap();
        assert_eq!(run.texts.built, n + m, "width {n}");
        // The replaced keys and the deleted ones are each read once.
        assert_eq!(run.texts.reads, 2 * m, "width {n}");
        assert_eq!(doc.get("k0"), None);
        assert_eq!(
            doc.get(format!("k{}", n - 1).as_str()).unwrap().as_i64(),
            Some(2)
        );
    }
}

/// A mapping `m` with `k` alias keys that each name an anchored scalar of
/// `len` bytes, and a key `b`.
fn aliased(k: usize, len: usize) -> String {
    let mut src = String::new();
    for i in 0..k {
        src.push_str(&format!("a{i}: &k{i} {i}{}\n", "y".repeat(len)));
    }
    src.push_str("m:\n");
    for i in 0..k {
        src.push_str(&format!("  *k{i} : {i}\n"));
    }
    src.push_str("  b: 1\n");
    src
}

#[test]
fn the_text_of_an_alias_key_is_read_once_however_many_operations_look_at_it() {
    let (k, ops) = (6usize, 400usize);
    let mut doc = Document::parse(aliased(k, 4_000)).unwrap();
    let mut list = Vec::new();
    for i in 0..ops {
        // Writes in the mapping between the reads do not make the alias keys
        // be read again.
        match i % 4 {
            0 => list.push(PatchOp::test("/m/b", Value::from(1))),
            1 => list.push(PatchOp::add(format!("/m/x{i}"), Value::from(i as i64))),
            2 => list.push(PatchOp::replace("/m/b", Value::from(1))),
            _ => list.push(PatchOp::remove(format!("/m/x{}", i - 2))),
        }
    }
    let (result, _, reads) = counted(&mut doc, &list);
    result.unwrap();
    // Each alias key once. Then one key is read to confirm each name that is
    // there: `m` and `b` for a test or a replace, `m` alone for an add (the
    // new name is not there), `m` and the added member for a remove.
    assert_eq!(reads, k + 7 * (ops / 4));
}

#[test]
fn an_alias_key_is_read_again_when_the_patch_changes_what_it_names() {
    // Replacing the anchored value changes the key's text, in the same patch.
    let src = "a: &k old\nm:\n  *k : 1\n  b: 2\n";
    let mut doc = Document::parse(src).unwrap();
    let ops = [
        PatchOp::test("/m/old", Value::from(1)),
        PatchOp::replace("/a", Value::from("new")),
        PatchOp::test("/m/new", Value::from(1)),
        PatchOp::test("/m/b", Value::from(2)),
    ];
    let (result, _, reads) = counted(&mut doc, &ops);
    result.unwrap();
    // The alias key is read for `old`, again for `new` because the patch
    // replaced what it names, and not for `b`. The other reads are the
    // names that are there: `m`, `a`, `m`, then `m` and `b`.
    assert_eq!(reads, 2 + 5, "{reads}");
    let mut doc = Document::parse(src).unwrap();
    let stale = [
        PatchOp::test("/m/old", Value::from(1)),
        PatchOp::replace("/a", Value::from("new")),
        PatchOp::test("/m/old", Value::from(1)),
    ];
    let (result, _, _) = counted(&mut doc, &stale);
    assert_eq!(result.unwrap_err().kind(), PatchErrorKind::Path);
}

#[test]
fn alias_keys_naming_large_values_are_read_once_under_the_untrusted_limits() {
    let (k, len, ops) = (8usize, 250_000usize, 600usize);
    let src = aliased(k, len);
    let settings = Settings::new().limits(Limits::untrusted());
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let list: Vec<PatchOp> = (0..ops)
        .map(|_| PatchOp::test("/m/b", Value::from(1)))
        .collect();
    let (result, _, reads) = counted(&mut doc, &list);
    result.unwrap();
    // Each alias key once; `m` and `b` are read for each test.
    assert_eq!(reads, k + 2 * ops);
}

#[test]
fn reading_alias_keys_is_charged_by_length_to_the_alias_byte_budget() {
    let src = aliased(2, 40_000);
    let settings = Settings::new().limits(Limits::default().alias_byte_budget(100_000));
    let mut doc = Document::parse_with(src.clone(), &settings).unwrap();
    let (result, _, _) = counted(&mut doc, &[PatchOp::test("/m/b", Value::from(1))]);
    // 80,000 bytes of keys read once: within the budget.
    result.unwrap();
    // Each `add` into the mapping compares the new key with all of them.
    let adds: Vec<PatchOp> = (0..5)
        .map(|i| PatchOp::add(format!("/m/n{i}"), Value::from(1)))
        .collect();
    let mut doc = Document::parse_with(src.clone(), &settings).unwrap();
    let (result, _, _) = counted(&mut doc, &adds);
    assert_eq!(result.unwrap_err().kind(), PatchErrorKind::Limit);
    // And the failed patch left the document as it was.
    let mut doc = Document::parse_with(src.clone(), &settings).unwrap();
    let r = doc.apply_patch(&crate::Patch::new(adds));
    assert_eq!(r.unwrap_err().kind(), PatchErrorKind::Limit);
    assert_eq!(doc.to_string(), src);
}

#[test]
fn reading_pointers_builds_the_index_of_a_wide_mapping_once() {
    let n = 20_000;
    let doc = Document::parse(wide(n)).unwrap();
    let before = ReadCache::built_on_thread();
    for i in 0..1_000 {
        let node = doc.get_pointer(&format!("/k{}", i * 7)).unwrap();
        assert_eq!(node.as_i64(), Some(i * 7));
    }
    assert_eq!(ReadCache::built_on_thread() - before, n);
    // An edit drops what was kept; the next read builds again, once.
    let mut doc = doc;
    doc.set_path("k0", 5).unwrap();
    let before = ReadCache::built_on_thread();
    for i in 0..100 {
        doc.get_pointer(&format!("/k{i}")).unwrap();
    }
    assert_eq!(ReadCache::built_on_thread() - before, n);
    assert_eq!(doc.get_pointer("/k0").unwrap().as_i64(), Some(5));
    // A clone keeps its own state: editing it does not show in the original.
    let mut copy = doc.clone();
    copy.set_path("k1", 6).unwrap();
    assert_eq!(copy.get_pointer("/k1").unwrap().as_i64(), Some(6));
    assert_eq!(doc.get_pointer("/k1").unwrap().as_i64(), Some(1));
}
