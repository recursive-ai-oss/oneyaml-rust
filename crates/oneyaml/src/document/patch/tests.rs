//! Tests of how much work patches do, counted rather than timed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use super::merge::without_nulls_noting;
use crate::document::view::LINEAR_KEY_READS;
use crate::{Document, Mapping, MergePatch, Patch, PatchOp, Value};

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

#[test]
fn a_wide_null_heavy_merge_patch_reads_no_keys_one_by_one_from_a_wide_mapping() {
    // A mapping of 2,000 members and a patch that deletes 1,500 of them,
    // sets 100 and names 1,500 that are not there: 3,100 lookups on a wide
    // mapping, none of which may read the mapping's keys one by one.
    let mut src = String::new();
    for i in 0..2_000 {
        src.push_str(&format!("k{i}: {i}\n"));
    }
    let mut doc = Document::parse(src).unwrap();
    let mut patch = Mapping::new();
    for i in 0..1_500 {
        patch.insert(Value::from(format!("k{i}")), Value::Null);
        patch.insert(Value::from(format!("absent{i}")), Value::Null);
    }
    for i in 1_900..2_000 {
        patch.insert(Value::from(format!("k{i}")), Value::from("set"));
    }
    LINEAR_KEY_READS.with(|c| c.set(0));
    doc.apply_merge_patch(&MergePatch::new(Value::from(patch)))
        .unwrap();
    assert_eq!(LINEAR_KEY_READS.with(std::cell::Cell::get), 0);
    assert_eq!(
        doc.get_pointer("/k1999").unwrap().as_str().as_deref(),
        Some("set")
    );
    assert!(doc.get_pointer("/k0").is_err());
    assert_eq!(doc.get_pointer("/k1600").unwrap().as_i64(), Some(1600));
}

#[test]
fn many_operations_on_a_wide_mapping_read_no_keys_one_by_one() {
    let n = 1_500;
    let mut src = String::new();
    for i in 0..n {
        src.push_str(&format!("k{i:05}: v\n"));
    }
    let mut doc = Document::parse(src).unwrap();
    let mut ops: Vec<PatchOp> = (0..n)
        .map(|i| PatchOp::replace(format!("/k{i:05}"), Value::from("w")))
        .collect();
    ops.extend((0..n).map(|i| PatchOp::test(format!("/k{i:05}"), Value::from("w"))));
    ops.extend((0..n).map(|i| PatchOp::remove(format!("/k{i:05}"))));
    LINEAR_KEY_READS.with(|c| c.set(0));
    doc.apply_patch(&Patch::new(ops)).unwrap();
    assert_eq!(LINEAR_KEY_READS.with(std::cell::Cell::get), 0);
    assert_eq!(doc.to_string(), "{}\n");
}

#[test]
fn a_narrow_mapping_is_read_in_full_but_the_reads_are_bounded_by_its_width() {
    // Under the width at which a mapping is indexed (32 entries), a token
    // reads at most that many keys.
    let mut src = String::new();
    for i in 0..30 {
        src.push_str(&format!("k{i}: v\n"));
    }
    let doc = Document::parse(src).unwrap();
    LINEAR_KEY_READS.with(|c| c.set(0));
    for i in 0..30 {
        doc.get_pointer(&format!("/k{i}")).unwrap();
    }
    let reads = LINEAR_KEY_READS.with(std::cell::Cell::get);
    assert!(reads > 0 && reads <= 30 * 30);
}
