//! Tests of how much work patches do, counted rather than timed, and of the
//! key index against a plain scan.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::merge::without_nulls_noting;
use super::textindex::{Hit, Texts};
use crate::{Document, DuplicateKeys, Mapping, NodeId, NodeKind, NodeRef, Settings, Value};

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
        Hit::Ambiguous => Found::Ambiguous,
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
                    Hit::Ambiguous => Found::Ambiguous,
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
