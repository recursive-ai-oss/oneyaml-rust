//! The `Document` edit API's performance bounds: `set_path`, `insert`, and
//! `remove` next to a large subtree cost the same on a 100 KB and a 10 MB
//! document — never O(document).
//!
//! Method (as `oneyaml-syntax/tests/edit_scaling.rs`, robust against
//! wall-clock noise): the script runs several times on a parsed document
//! (parsing is not timed) and the minimum is kept; the documents
//! differ 100× in size, so an O(document) step would make the large run
//! tens of times slower, while the bound allows 4× for cache effects.
//!
//! Scope of the claim: an edit's cost is local to the collection it
//! changes; finding a key or a position in a wide collection is O(log
//! width) through its lookup index (`document/lookup.rs`), so
//! the `wide_*` tests grow the edited collection itself (10k vs 100k
//! entries). Reported with `--nocapture`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use oneyaml::{Document, EditError, Limits, Settings, path};

fn source(items: usize) -> String {
    let mut s = String::from("big:\n");
    for i in 0..items {
        writeln!(s, "  - {{id: {i}, name: item number {i}}} # c").unwrap();
    }
    s.push_str("target:\n  a: 1\n  b: [x, y]\nafter: [1, 2]\n");
    s
}

fn script(doc: &mut Document) {
    for i in 0..20 {
        doc.set_path(&path!["target", "a"], i).unwrap();
        let key = format!("c{i}");
        let mut t = doc.get_mut("target").unwrap();
        t.insert(&key, vec![i, i]).unwrap();
        let _ = t.remove(&key).unwrap();
        let mut b = doc.get_path_mut(&path!["target", "b"]).unwrap();
        b.push(i).unwrap();
        let _ = b.remove_at(2).unwrap();
    }
}

fn best(src: &str, settings: &Settings) -> Duration {
    let mut min = Duration::MAX;
    // Parsed once (not timed); the script leaves the document as it
    // found it, apart from the syntax layer's edit overlay.
    let mut doc = Document::parse_with(src, settings).unwrap();
    for _ in 0..3 {
        let start = Instant::now();
        script(&mut doc);
        min = min.min(start.elapsed());
        assert_eq!(doc.to_value().unwrap()["target"]["a"].as_u64(), Some(19));
    }
    min
}

#[test]
fn edits_do_not_scale_with_the_document() {
    let settings = Settings::new().limits(Limits::unlimited());
    let small = source(2_500);
    let large = source(250_000);
    assert!(small.len() > 100_000 && large.len() > 10_000_000);
    let a = best(&small, &settings);
    let b = best(&large, &settings);
    let ratio = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!(
        "edit script: {:?} on {} KB, {:?} on {} MB (ratio {ratio:.2})",
        a,
        small.len() / 1000,
        b,
        large.len() / 1_000_000
    );
    assert!(ratio < 4.0, "edits scale with the document: {ratio:.2}");
}

/// The minimum over three runs of `script` on a document parsed from
/// `src` (not timed); `script` leaves the document as it found it.
fn best_of(src: &str, script: &dyn Fn(&mut Document)) -> Duration {
    let settings = Settings::new().limits(Limits::unlimited());
    let mut doc = Document::parse_with(src, &settings).unwrap();
    let mut min = Duration::MAX;
    for _ in 0..3 {
        let start = Instant::now();
        script(&mut doc);
        min = min.min(start.elapsed());
    }
    min
}

fn ratio(name: &str, small: &str, large: &str, script: &dyn Fn(&mut Document)) {
    assert!(small.len() > 100_000 && large.len() > 10_000_000);
    let a = best_of(small, script);
    let b = best_of(large, script);
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("{name}: {a:?} vs {b:?} (ratio {r:.2})");
    assert!(r < 4.0, "{name} scales with the document: {r:.2}");
}

/// Aliases into a small list of a document full of
/// anchors and aliases (the anchor index is maintained, never rebuilt
/// per edit; the alias checks are binary searches).
#[test]
fn alias_edits_do_not_scale_with_the_document() {
    let src = |items: usize| {
        let mut s = String::from("big:\n");
        for i in 0..items {
            writeln!(s, "  - &a{i} {{id: {i}}}\n  - *a{i}").unwrap();
        }
        s.push_str("anchor: &t 1\nlist: [0]\n");
        s
    };
    let script = |doc: &mut Document| {
        let t = doc.id_of(&path!["anchor"]).unwrap();
        let mut list = doc.get_mut("list").unwrap();
        for _ in 0..10 {
            list.push_alias(t).unwrap();
        }
        for _ in 0..10 {
            let _ = list.remove_at(1).unwrap();
        }
        assert_eq!(doc.aliases_of(t).unwrap().len(), 0);
    };
    ratio("push_alias", &src(4_000), &src(400_000), &script);
}

/// Replacing the last item of a long flat sequence
/// finds its slot from its neighbours, not from the whole sequence.
#[test]
fn replacing_in_a_long_sequence_does_not_scale() {
    let src = |items: usize| {
        let mut s = String::from("flat:\n");
        for i in 0..items {
            writeln!(s, "  - {i}").unwrap();
        }
        s
    };
    // The last item is found once per document, untimed.
    let timed = |src: &str| {
        let settings = Settings::new().limits(Limits::unlimited());
        let mut doc = Document::parse_with(src, &settings).unwrap();
        let flat = doc.get("flat").unwrap().as_sequence().unwrap();
        let mut id = flat.iter().last().unwrap().id();
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for i in 0..20 {
                id = doc.node_mut(id).unwrap().set(7 + i).unwrap().id();
            }
            min = min.min(start.elapsed());
        }
        min
    };
    let (small, large) = (src(12_000), src(1_200_000));
    assert!(small.len() > 100_000 && large.len() > 10_000_000);
    let (a, b) = (timed(&small), timed(&large));
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("set last: {a:?} vs {b:?} (ratio {r:.2})");
    assert!(r < 4.0, "set last scales with the document: {r:.2}");
}

/// The minimum over three runs of `script` on a document parsed from
/// `src` (not timed; `prepare` runs untimed too, e.g. to build the anchor
/// index); `script` leaves the document as it found it.
fn best_prepared(
    src: &str,
    prepare: &dyn Fn(&mut Document),
    script: &dyn Fn(&mut Document),
) -> Duration {
    let settings = Settings::new().limits(Limits::unlimited());
    let mut doc = Document::parse_with(src, &settings).unwrap();
    prepare(&mut doc);
    let mut min = Duration::MAX;
    for _ in 0..3 {
        let start = Instant::now();
        script(&mut doc);
        min = min.min(start.elapsed());
    }
    min
}

/// Cost against the number `k` of repeated names: 10× more must cost
/// about 10× (linear), never 100× (one list operation per name;
/// before the fix, 103× measured at 20k, 35× at 10k). The bound
/// allows 2× noise.
fn linear(name: &str, src: &dyn Fn(usize) -> String, script: &dyn Fn(&mut Document)) {
    let prepare = |d: &mut Document| {
        // Builds the anchor index (once, O(n)), outside the timing.
        let a = d.id_of(&path!["anchor"]).unwrap();
        let _ = d.aliases_of(a).unwrap();
    };
    let a = best_prepared(&src(1_000), &prepare, script);
    let b = best_prepared(&src(10_000), &prepare, script);
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("{name}: {a:?} for 1k vs {b:?} for 10k (ratio {r:.2}, linear is 10)");
    assert!(
        r < 20.0,
        "{name} is not linear in the repeated names: {r:.2}"
    );
}

/// Renaming an anchor with many aliases
/// moves them in one splice per list.
#[test]
fn renaming_an_anchor_with_many_aliases_is_linear() {
    let src = |k: usize| {
        let mut s = String::from("anchor: &x 1\nlist:\n");
        for _ in 0..k {
            s.push_str("  - *x\n");
        }
        s
    };
    let script = |doc: &mut Document| {
        let a = doc.id_of(&path!["anchor"]).unwrap();
        doc.set_anchor(a, Some("y")).unwrap();
        doc.set_anchor(a, Some("x")).unwrap();
    };
    linear("rename", &src, &script);
}

/// Removing and moving back a subtree
/// that holds one anchor name many times (valid YAML, section 3.2.2.2),
/// in a document holding it many times more, with aliases after it.
#[test]
fn moving_a_subtree_of_same_name_anchors_is_linear() {
    let src = |k: usize| {
        let mut s = String::from("anchor: &x 0\nbefore:\n");
        for i in 0..k {
            writeln!(s, "  - &x {i}").unwrap();
        }
        s.push_str("sub:\n");
        for i in 0..k {
            writeln!(s, "  - &x {i}\n  - *x").unwrap();
        }
        s.push_str("last: 1\n");
        s
    };
    let script = |doc: &mut Document| {
        let r = doc.remove_path(&path!["sub"]).unwrap();
        let mut root = doc.root_mut().unwrap();
        root.insert_removed_at(2, r).unwrap();
    };
    linear("move subtree", &src, &script);
}

/// The empty values of a flow set (`{a, b}`)
/// are found from their neighbours, never by counting the mapping.
#[test]
fn removing_from_a_long_flow_set_does_not_scale() {
    let src = |items: usize| {
        // One entry per line (`-syntax` validation reads the edited lines).
        let mut s = String::from("set: {\n");
        for i in 0..items {
            writeln!(s, "  k{i},").unwrap();
        }
        s.push_str("  end}\n");
        s
    };
    // The handles of the values near the end, taken untimed.
    let timed = |src: &str| {
        let settings = Settings::new().limits(Limits::unlimited());
        let mut doc = Document::parse_with(src, &settings).unwrap();
        let set = doc.get("set").unwrap().as_mapping().unwrap();
        let n = set.len();
        let ids: Vec<_> = set.iter().skip(n - 61).map(|(_, v)| v.id()).collect();
        let mut ids = ids.into_iter();
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for _ in 0..20 {
                let _ = doc.remove_node(ids.next().unwrap()).unwrap();
            }
            min = min.min(start.elapsed());
        }
        assert_eq!(doc.get("set").unwrap().as_mapping().unwrap().len(), n - 60);
        min
    };
    let (small, large) = (src(15_000), src(1_500_000));
    assert!(small.len() > 100_000 && large.len() > 10_000_000);
    let (a, b) = (timed(&small), timed(&large));
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("flow set removals: {a:?} vs {b:?} (ratio {r:.2})");
    assert!(r < 4.0, "removing from a flow set scales with it: {r:.2}");
}

/// Stream edits are local too:
/// editing every document of a stream through `Stream::document_mut`
/// (switching documents, the first edit of each, and `has_errors`) costs
/// the same per document for 200 and 2,000 documents. Before the fix every
/// switch re-checked the whole stream: 2.7 ms/document at 200, 32.7 at
/// 2,000 (release), quadratic in all.
#[test]
fn editing_every_document_of_a_stream_is_linear() {
    use oneyaml::Stream;
    let per_doc = |k: usize| {
        let src = "---\na: 1\nb: 2\n".repeat(k);
        let mut min = Duration::MAX;
        for round in 0..3u64 {
            let mut s = Stream::parse(src.as_str()).unwrap();
            let start = Instant::now();
            for i in 0..k {
                let mut d = s.document_mut(i).unwrap();
                d.set_path(&path!["b"], round + 3).unwrap();
                assert!(!d.has_errors());
            }
            min = min.min(start.elapsed());
            assert_eq!(
                s.document(k - 1).unwrap().to_value().unwrap()["b"].as_u64(),
                Some(round + 3)
            );
        }
        min.as_secs_f64() / k as f64
    };
    let (a, b) = (per_doc(200), per_doc(2_000));
    let r = b / a.max(1e-12);
    eprintln!(
        "every document: {:.3} ms/doc at 200, {:.3} ms/doc at 2000 (ratio {r:.2})",
        a * 1e3,
        b * 1e3
    );
    assert!(r < 3.0, "editing a document scales with the stream: {r:.2}");
}

/// An accepted edit no longer reads every document's
/// span (before the fix, 0.033 ms/edit at 500 documents, 1.07 at 20,000):
/// edits of one document cost the same in a stream of 1,000 and of
/// 100,000 documents.
#[test]
fn an_edit_does_not_scale_with_the_number_of_documents() {
    use oneyaml::Stream;
    let timed = |k: usize| {
        let src = "---\na: 1\nb: 2\n".repeat(k);
        let mut s = Stream::parse(src).unwrap();
        let mut d = s.document_mut(k / 2).unwrap();
        // The first edit finds the document's guard (O(document)).
        d.set_path(&path!["b"], 0).unwrap();
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for i in 0..50u64 {
                d.set_path(&path!["b"], i).unwrap();
            }
            min = min.min(start.elapsed());
        }
        min
    };
    let (a, b) = (timed(1_000), timed(100_000));
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("edits in a stream: {a:?} at 1k documents vs {b:?} at 100k (ratio {r:.2})");
    assert!(
        r < 4.0,
        "an edit scales with the number of documents: {r:.2}"
    );
}

/// After an edit, the **last** document's diagnostics
/// no longer rebuild the stream's (before the fix, 64 µs at 1,000 documents,
/// 7.9 ms at 100,000: the diagnostics after the last element were found by
/// `diagnostics_by_element` over the whole edited tree). Reading them costs
/// the same in a stream of 1,000 and of 100,000 documents, each holding a
/// warning (so the stream's diagnostics grow with it), and so does a
/// middle document's.
#[test]
fn the_last_documents_diagnostics_do_not_scale_with_the_stream() {
    use oneyaml::Stream;
    let lenient = Settings::new().lenient_inline_colons(true);
    let timed = |k: usize, which: fn(usize) -> usize| {
        // Each document has a `Y0028` warning; the last one's is attached
        // after the last element (its scalar ends the text).
        let mut src = "---\na: 1\nk: x: z\n".repeat(k);
        src.push_str("---\nk: x: z");
        let mut s = Stream::parse_with(src, &lenient).unwrap();
        s.document_mut(0).unwrap().set_path(&path!["a"], 2).unwrap();
        let d = s.document(which(k)).unwrap();
        assert_eq!(d.diagnostics().len(), 1);
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            for _ in 0..50 {
                assert_eq!(d.diagnostics().len(), 1);
            }
            min = min.min(start.elapsed());
        }
        min
    };
    for (name, which) in [
        ("last", (|k| k) as fn(usize) -> usize),
        ("middle", |k| k / 2),
    ] {
        let (a, b) = (timed(1_000, which), timed(100_000, which));
        let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        eprintln!(
            "{name} document's diagnostics: {a:?} at 1k documents vs {b:?} at 100k (ratio {r:.2})"
        );
        assert!(
            r < 4.0,
            "the {name} document's diagnostics scale with the stream: {r:.2}"
        );
    }
}

/// Appending to a block sequence
/// whose last item is a folded block scalar, then removing the item
/// again. Before the fix: 4.0 ms at 1k items, 565 ms at 100k (release; the
/// `-syntax` window and `NodeMut::push` counting the
/// sequence's items, both fixed).
#[test]
fn appending_after_a_block_scalar_does_not_scale() {
    let src = |items: usize| {
        let mut s = String::from("s:\n");
        for i in 0..items {
            writeln!(s, "  - v{i}").unwrap();
        }
        s.push_str("  - >-\n    folded\n");
        s
    };
    let script = |doc: &mut Document| {
        for _ in 0..20 {
            let id = doc.get_mut("s").unwrap().push("x").unwrap().id();
            let _ = doc.remove_node(id).unwrap();
        }
    };
    ratio(
        "push after a folded scalar",
        &src(12_000),
        &src(1_200_000),
        &script,
    );
}

/// An edit touching a name costs O(log k) in that name's k occurrences (bound
/// documented in `anchors.rs`; a chunked list), no longer an O(k) `Vec` splice.
/// Measured per push_alias + remove (release): 0.016 / 0.055 / 0.295 ms at k =
/// 1k / 100k / 1M before (a 16 MB memmove at 1M), 0.015 / 0.017 / 0.021 ms
/// after. 1k against 400k: the bound allows 3×. With the old `Vec` the ratio
/// was about 8× in a release build but only 1.4× in a debug build (the edit's
/// fixed cost is larger there, the memmove is not), so this test guards release
/// builds; the index-level `one_alias_in_a_long_list_costs_log_time`
/// (`document/anchors.rs`) guards debug ones too (32× before, about 2× after).
#[test]
fn many_aliases_of_one_name_cost_log_time() {
    let src = |k: usize| {
        let mut s = String::from("anchor: &x 1\nlist: [0]\nbig:\n");
        for _ in 0..k {
            s.push_str("  - *x\n");
        }
        s
    };
    let prepare = |d: &mut Document| {
        let a = d.id_of(&path!["anchor"]).unwrap();
        let _ = d.aliases_of(a).unwrap();
    };
    let script = |doc: &mut Document| {
        let a = doc.id_of(&path!["anchor"]).unwrap();
        let mut list = doc.get_mut("list").unwrap();
        for _ in 0..20 {
            list.push_alias(a).unwrap();
            let _ = list.remove_at(1).unwrap();
        }
    };
    let a = best_prepared(&src(1_000), &prepare, &script);
    let b = best_prepared(&src(400_000), &prepare, &script);
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("one name 1k vs 400k times: {a:?} vs {b:?} (ratio {r:.2})");
    assert!(r < 3.0, "one name's lists cost more than log k: {r:.2}");
}

/// Removing the first entry of a root or nested collection when it and the next
/// entry have leading comment lines is local. The comment lines above a root
/// are removed or re-inserted at the start of the document's text, which the
/// syntax layer's trivia shortcut now decides from the lines around (before:
/// the whole document was re-read, so draining a commented mapping from the
/// front was quadratic).
#[test]
fn first_entries_with_leading_comments_do_not_scale() {
    let root = |items: usize| {
        let mut s = String::from("# lead 0\nk0: 0\n# lead 1\nk1: 1\n");
        for i in 2..items {
            writeln!(s, "k{i}: value number {i}").unwrap();
        }
        s
    };
    let nested = |items: usize| {
        let mut s = String::from("top:\n  # lead 0\n  k0: 0\n  # lead 1\n  k1: 1\n");
        for i in 2..items {
            writeln!(s, "  k{i}: value number {i}").unwrap();
        }
        s
    };
    // Remove the first entry (timed) and put it back (not timed: finding
    // the style of a new entry in its collection is O(width), as at
    // the baseline), ten times; the document is as it was after each round
    // (byte-exact re-insertion). The minimum of three runs.
    let timed = |src: &str, at: &[&str]| {
        let mut d =
            Document::parse_with(src, &Settings::new().limits(Limits::unlimited())).unwrap();
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let mut total = Duration::ZERO;
            for _ in 0..10 {
                let (coll, first) = {
                    let mut n = d.root().unwrap();
                    for k in at {
                        n = n.as_mapping().unwrap().get(k).unwrap();
                    }
                    let m = n.as_mapping().unwrap();
                    (n.id(), m.iter().next().unwrap().1.id())
                };
                let start = Instant::now();
                let r = d.remove_node(first).unwrap();
                total += start.elapsed();
                d.node_mut(coll).unwrap().insert_removed_at(0, r).unwrap();
            }
            min = min.min(total);
        }
        min
    };
    for (name, small, large, at) in [
        ("root", root(5_000), root(500_000), &[][..]),
        ("nested", nested(5_000), nested(500_000), &["top"][..]),
    ] {
        assert!(small.len() > 100_000 && large.len() > 10_000_000);
        let (a, b) = (timed(&small, at), timed(&large, at));
        let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        eprintln!(
            "{name} first-entry removals with leading comments: {a:?} vs {b:?} (ratio {r:.2})"
        );
        assert!(
            r < 4.0,
            "{name}: removing the first entry scales with the document: {r:.2}"
        );
    }
}

/// Removing a collection's only entry
/// is linear in that entry (its comments are matched with a set, not a
/// list scan: before, 128k commented lines took 2.65 s).
#[test]
fn removing_an_only_entry_is_linear_in_it() {
    let src = |items: usize| {
        let mut s = String::from("k:\n  a:\n");
        for i in 0..items {
            writeln!(s, "    b{i}: 1 # c{i}").unwrap();
        }
        s.push_str("z: 1\n");
        s
    };
    let timed = |src: &str| {
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let mut d = Document::parse(src).unwrap();
            let id = d.id_of(&path!["k", "a"]).unwrap();
            let start = Instant::now();
            let r = d.remove_node(id).unwrap();
            min = min.min(start.elapsed());
            assert_eq!(r.comments().len(), src.matches("# c").count());
        }
        min
    };
    let (a, b) = (timed(&src(10_000)), timed(&src(100_000)));
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("only-entry removal: {a:?} vs {b:?} for 10x the entry (ratio {r:.2})");
    assert!(
        r < 30.0,
        "removing an only entry is superlinear in it: {r:.2}"
    );
}

/// Found by the `edits` benchmark (openapi-10m's script went
/// from 0.57 M to 227 M instructions): appending to a root mapping whose
/// last entry is huge, then replacing and removing the new entry, never
/// lists that entry's nodes (the comments of the window are tested for
/// being inside it by walking up from their owners), and the last entry's
/// same-line comment after a closing `}` is moved before the new entry
/// goes in (removed after it, it followed the new plain scalar, which the
/// syntax layer re-read with the whole last entry: 873 ms at 250k items).
#[test]
fn appending_after_a_huge_last_entry_does_not_scale() {
    let src = |n: usize| {
        let mut s = String::from("a: 1\nbig:\n");
        for i in 0..n {
            writeln!(s, "  k{i}:\n    - {{id: {i}, name: item {i}}} # c").unwrap();
        }
        s
    };
    let (small, large) = (src(2_500), src(250_000));
    ratio("append after a huge last entry", &small, &large, &|doc| {
        for i in 0..20 {
            let mut r = doc.root_mut().unwrap();
            r.insert("bench_edit_key", "x").unwrap();
            doc.set_path(&path!["bench_edit_key"], i).unwrap();
            let _ = doc.root_mut().unwrap().remove("bench_edit_key").unwrap();
        }
    });
    // The huge last entry ends with footer
    // comments (its own collection's, and nested ones'): appending after
    // it relocates no footer and re-reads nothing of it.
    type Shape = (&'static str, fn(usize) -> String);
    let shapes: [Shape; 4] = [
        ("a mapping footer", |n| {
            let mut s = String::from("k0: 0\nbig:\n");
            for i in 0..n {
                writeln!(s, "  b{i}: {i}").unwrap();
            }
            s.push_str("  # footer big\n");
            s
        }),
        ("nested footers", |n| {
            let mut s = String::from("k0: 0\nbig:\n  inner:\n");
            for i in 0..n {
                writeln!(s, "    b{i}: {i}").unwrap();
            }
            s.push_str("    # footer inner\n  # footer big\n");
            s
        }),
        ("a sequence footer", |n| {
            let mut s = String::from("k0: 0\nbig:\n");
            for i in 0..n {
                writeln!(s, "  - {i}").unwrap();
            }
            s.push_str("  # footer big\n");
            s
        }),
        ("a footer and a comment line after it", |n| {
            let mut s = String::from("k0: 0\nbig:\n");
            for i in 0..n {
                writeln!(s, "  b{i}: {i}").unwrap();
            }
            s.push_str("  # footer big\n# document footer\n");
            s
        }),
    ];
    for (name, src) in shapes {
        let (small, large) = (src(12_000), src(1_200_000));
        let name = format!("append after a huge last entry with {name}");
        ratio(&name, &small, &large, &|doc| {
            for i in 0..20 {
                doc.root_mut().unwrap().insert("new", 1).unwrap();
                doc.set_path(&path!["new"], i).unwrap();
                let _ = doc.root_mut().unwrap().remove("new").unwrap();
            }
        });
    }
    // A root sequence whose last item holds a mapping with a footer.
    let src = |n: usize| {
        let mut s = String::from("- 0\n- big:\n");
        for i in 0..n {
            writeln!(s, "    b{i}: {i}").unwrap();
        }
        s.push_str("    # footer big\n");
        s
    };
    let (small, large) = (src(10_000), src(1_000_000));
    ratio(
        "push after a huge last item with a footer",
        &small,
        &large,
        &|doc| {
            for _ in 0..20 {
                doc.root_mut().unwrap().push(1).unwrap();
                let n = doc.root().unwrap().as_sequence().unwrap().len();
                let _ = doc.remove_path(&path![n - 1]).unwrap();
            }
        },
    );
}

/// A document ending in a long
/// unterminated line of spaces. The E5 check walked back over that whole
/// line on every edit; now it is
/// the `Document` layer's tail check, once per edit, which finds the line
/// from the last tokens and reads its bytes only after a kept `|+` scalar.
#[test]
fn edits_before_a_long_final_space_line_do_not_scale() {
    let src = |spaces: usize| {
        let mut s = String::from("a: 1\nb: [x]\nk: |+\n  kept\n\nz: 2\n");
        s.push_str(&" ".repeat(spaces));
        s
    };
    let script = |doc: &mut Document| {
        for i in 0..20 {
            doc.set_path(&path!["a"], i).unwrap();
            let mut b = doc.get_path_mut(&path!["b"]).unwrap();
            b.push(i).unwrap();
            let _ = b.remove_at(1).unwrap();
            doc.set_path(&path!["k"], "v").unwrap();
            doc.set_path(&path!["k"], "kept\n\n").unwrap();
        }
    };
    ratio(
        "edits before a long final space line",
        &src(100_001),
        &src(10_000_001),
        &script,
    );
}

/// The tail check (fuzz finding E5) reads no value. A
/// document ending in a kept scalar and a final line of spaces (the tail
/// reader): edits far from it cost the same whether the scalar is 1 KB or
/// 1 MB (reading its value twice per edit cost 2.5 ms per edit at 1 MB).
#[test]
fn edits_far_from_a_large_kept_scalar_do_not_scale() {
    let src = |bytes: usize| {
        let mut s = String::from("a: 1\nb: [x]\nk: |+\n");
        while s.len() < bytes {
            s.push_str("  some kept text of the scalar\n");
        }
        s.push_str("\n ");
        s
    };
    let script = |doc: &mut Document| {
        for i in 0..20 {
            doc.set_path(&path!["a"], i).unwrap();
            let mut b = doc.get_path_mut(&path!["b"]).unwrap();
            b.push(i).unwrap();
            let _ = b.remove_at(1).unwrap();
        }
    };
    let (small, large) = (src(1_000), src(1_000_000));
    let a = best_of(&small, &script);
    let b = best_of(&large, &script);
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("edits far from a kept scalar (1 KB vs 1 MB): {a:?} vs {b:?} (ratio {r:.2})");
    let doc = Document::parse(large.as_str()).unwrap();
    assert!(
        doc.to_value().unwrap()["k"]
            .as_str()
            .is_some_and(|v| v.ends_with("\n\n\n"))
    );
    assert!(
        r < 4.0,
        "edits far from a kept scalar scale with it: {r:.2}"
    );
}

/// Key- and index-addressed
/// operations on a **wide** collection cost O(log width), never
/// O(width): the per-call time of `script` on a root collection of 10k
/// and of 100k entries. `script` runs once untimed first (its first
/// lookup builds the collection's lookup index, O(width) once), then the
/// minimum of three timed runs is kept; 10× the entries must cost far
/// less than 10× (linear), the bound allows 3× for cache effects.
fn wide(name: &str, src: fn(usize) -> String, script: &dyn Fn(&mut Document, usize) -> usize) {
    wide_prepared(name, src, &|_, _| {}, script);
}

/// [`wide`] with `prepare` run once, untimed, before the first call.
fn wide_prepared(
    name: &str,
    src: fn(usize) -> String,
    prepare: &dyn Fn(&mut Document, usize),
    script: &dyn Fn(&mut Document, usize) -> usize,
) {
    let make = |n: usize| {
        let settings = Settings::new().limits(Limits::unlimited());
        Document::parse_with(src(n), &settings).unwrap()
    };
    wide_doc(name, &make, prepare, script);
}

/// [`wide_prepared`] over documents built by `make` (other settings, or
/// recovering parses).
fn wide_doc(
    name: &str,
    make: &dyn Fn(usize) -> Document,
    prepare: &dyn Fn(&mut Document, usize),
    script: &dyn Fn(&mut Document, usize) -> usize,
) {
    let per_call = |n: usize| {
        let mut doc = make(n);
        prepare(&mut doc, n);
        let calls = script(&mut doc, n).max(1);
        let mut min = Duration::MAX;
        for _ in 0..3 {
            let start = Instant::now();
            script(&mut doc, n);
            min = min.min(start.elapsed());
        }
        min / u32::try_from(calls).unwrap()
    };
    let (a, b) = (per_call(10_000), per_call(100_000));
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("wide {name}: {a:?}/call at 10k entries, {b:?}/call at 100k (ratio {r:.2})");
    assert!(r < 3.0, "wide {name} scales with the width: {r:.2}");
}

fn wide_map(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "k{i}: v{i}").unwrap();
    }
    s
}

fn wide_seq(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        writeln!(s, "- v{i}").unwrap();
    }
    s
}

#[test]
fn wide_mapping_set_path_last_key() {
    wide("set_path last key", wide_map, &|doc, n| {
        let p = path![format!("k{}", n - 1)];
        for i in 0..20 {
            doc.set_path(&p, i).unwrap();
        }
        assert_eq!(doc.get(format!("k{}", n - 1)).unwrap().text(), "19");
        20
    });
}

#[test]
fn wide_mapping_insert_and_remove_a_new_key() {
    wide("insert + remove a new key", wide_map, &|doc, n| {
        for i in 0..20 {
            doc.root_mut().unwrap().insert("new", i).unwrap();
            let _ = doc.root_mut().unwrap().remove("new").unwrap();
        }
        assert_eq!(doc.root().unwrap().as_mapping().unwrap().len(), n);
        20
    });
}

#[test]
fn wide_mapping_remove_and_reinsert_in_the_middle() {
    wide(
        "remove + insert_removed_at the middle",
        wide_map,
        &|doc, n| {
            let key = format!("k{}", n / 2);
            for _ in 0..20 {
                let r = doc.root_mut().unwrap().remove(&key).unwrap();
                doc.root_mut().unwrap().insert_removed_at(n / 2, r).unwrap();
            }
            // (Where it went back is checked in `wide_lookup_index.rs`:
            // `iter().nth` is a linear walk.)
            assert_eq!(doc.get(&key).unwrap().text(), format!("v{}", n / 2));
            20
        },
    );
}

#[test]
fn wide_mapping_move_entry() {
    wide("move_entry last <-> first", wide_map, &|doc, n| {
        for _ in 0..10 {
            doc.root_mut().unwrap().move_entry(n - 1, 0).unwrap();
            doc.root_mut().unwrap().move_entry(0, n - 1).unwrap();
        }
        assert_eq!(
            doc.get(format!("k{}", n - 1)).unwrap().text(),
            format!("v{}", n - 1)
        );
        20
    });
}

/// Reads after an edit (the tree is edited, the index maintained):
/// `Document::get`, `NodeRef::get`, `get_path`, `len`, and a scalar's
/// resolved value (in place of `Index`, removed from
/// `Document`).
#[test]
fn wide_mapping_reads() {
    let edit = |doc: &mut Document, _: usize| doc.set_path(&path!["k0"], "w0").map(|_| ()).unwrap();
    wide_prepared(
        "get / get_path / len / resolved",
        wide_map,
        &edit,
        &|doc, n| {
            let mut found = 0;
            for j in 0..2_000usize {
                let k = format!("k{}", (j * 7_919) % n);
                found += usize::from(doc.get(&k).is_some());
                found += usize::from(doc.get_path(&path![k.clone()]).is_some());
                found += usize::from(doc.root().unwrap().get(&k).is_some());
                let resolved = doc
                    .get(k.as_str())
                    .and_then(|n| n.as_scalar()?.resolved().ok());
                found += usize::from(resolved.as_ref().and_then(|v| v.as_str()).is_some());
                assert!(doc.get("missing").is_none());
                assert_eq!(doc.root().unwrap().as_mapping().unwrap().len(), n);
            }
            assert_eq!(found, 8_000);
            2_000
        },
    );
}

#[test]
fn wide_sequence_replace_at() {
    wide("seq replace_at(n/2)", wide_seq, &|doc, n| {
        for i in 0..20 {
            doc.root_mut().unwrap().replace_at(n / 2, i).unwrap();
        }
        assert_eq!(doc.get_path(&path![n / 2]).unwrap().text(), "19");
        20
    });
}

#[test]
fn wide_sequence_insert_at_and_remove_at() {
    wide("seq insert_at + remove_at(n/2)", wide_seq, &|doc, n| {
        for i in 0..20 {
            doc.root_mut().unwrap().insert_at(n / 2, i).unwrap();
            let _ = doc.root_mut().unwrap().remove_at(n / 2).unwrap();
        }
        assert_eq!(doc.root().unwrap().as_sequence().unwrap().len(), n);
        20
    });
}

/// Removing and re-inserting the
/// middle item of a wide sequence of aliases to one anchor costs the same
/// at 10k and 100k items once the list's lookup index exists (the probe's
/// growth, 0.026 ms at 1k to 0.15 ms at 100k over 20 rounds, was that
/// index's one O(width) build on its first positional lookup).
#[test]
fn wide_sequence_remove_and_reinsert_an_alias_in_the_middle() {
    fn src(n: usize) -> String {
        let mut s = String::from("anchor: &x 1\nbig:\n");
        for _ in 0..n {
            s.push_str("  - *x\n");
        }
        s
    }
    wide_prepared(
        "alias item remove_at + insert_removed_at(n/2)",
        src,
        &|doc, _| {
            let a = doc.id_of(&path!["anchor"]).unwrap();
            let _ = doc.aliases_of(a).unwrap();
        },
        &|doc, n| {
            for _ in 0..20 {
                let r = doc.get_mut("big").unwrap().remove_at(n / 2).unwrap();
                doc.get_mut("big")
                    .unwrap()
                    .insert_removed_at(n / 2, r)
                    .unwrap();
            }
            let a = doc.id_of(&path!["anchor"]).unwrap();
            assert_eq!(doc.aliases_of(a).unwrap().len(), n);
            20
        },
    );
}

#[test]
fn wide_sequence_move_entry_and_reads() {
    wide("seq move_entry + get_index", wide_seq, &|doc, n| {
        for _ in 0..10 {
            doc.root_mut().unwrap().move_entry(n / 2, 0).unwrap();
            doc.root_mut().unwrap().move_entry(0, n / 2).unwrap();
            for j in 0..10 {
                let i = (j * 7_919) % n;
                assert!(doc.root().unwrap().get_index(i).is_some());
            }
        }
        assert_eq!(
            doc.get_path(&path![n / 2]).unwrap().text(),
            format!("v{}", n / 2)
        );
        20
    });
}

// ----------------------------------------------- minified JSON and flow

/// Minified JSON, one line.
fn json_object(n: usize) -> String {
    let mut s = String::from("{");
    for i in 0..n {
        write!(s, "\"k{i}\":\"v{i}\",").unwrap();
    }
    s.push_str("\"o\":{\"x\":1}}\n");
    s
}

fn json_array(n: usize) -> String {
    let mut s = String::from("[");
    for i in 0..n {
        write!(s, "\"v{i}\",").unwrap();
    }
    s.push_str("\"last\"]\n");
    s
}

/// Edits at the end of a single-line flow
/// collection never walk the line back to its start (no column is needed
/// for a single-line flow entry).
#[test]
fn minified_json_object_edits() {
    wide("json remove + reinsert last key", json_object, &|doc, n| {
        let key = format!("k{}", n - 1);
        for _ in 0..10 {
            let r = doc.root_mut().unwrap().remove(&key).unwrap();
            doc.root_mut().unwrap().insert_removed_at(n - 1, r).unwrap();
        }
        20
    });
    wide("json move_entry near the end", json_object, &|doc, n| {
        for _ in 0..10 {
            doc.root_mut().unwrap().move_entry(n - 1, n - 2).unwrap();
            doc.root_mut().unwrap().move_entry(n - 2, n - 1).unwrap();
        }
        20
    });
    wide(
        "json set_anchor on the last value",
        json_object,
        &|doc, n| {
            let p = path![format!("k{}", n - 1)];
            for _ in 0..10 {
                let id = doc.id_of(&p).unwrap();
                let id = doc.set_anchor(id, Some("a")).unwrap();
                doc.set_anchor(id, None).unwrap();
            }
            20
        },
    );
    wide(
        "json remove_keep_comments + reinsert",
        json_object,
        &|doc, n| {
            let p = path![format!("k{}", n - 1)];
            for _ in 0..10 {
                let id = doc.id_of(&p).unwrap();
                let r = doc.remove_keep_comments(id).unwrap();
                doc.root_mut().unwrap().insert_removed_at(n - 1, r).unwrap();
            }
            20
        },
    );
    wide("json set_path last key", json_object, &|doc, n| {
        let p = path![format!("k{}", n - 1)];
        for i in 0..20 {
            doc.set_path(&p, i).unwrap();
        }
        20
    });
    wide(
        "json insert into a nested object",
        json_object,
        &|doc, _| {
            for i in 0..10 {
                doc.get_mut("o").unwrap().insert("y", i).unwrap();
                let _ = doc.get_mut("o").unwrap().remove("y").unwrap();
            }
            20
        },
    );
}

#[test]
fn minified_json_array_edits() {
    wide("json array remove_at + insert_at", json_array, &|doc, n| {
        for i in 0..10 {
            let _ = doc.root_mut().unwrap().remove_at(n - 1).unwrap();
            doc.root_mut().unwrap().insert_at(n - 1, i).unwrap();
        }
        20
    });
    wide(
        "json array replace_at + move_entry",
        json_array,
        &|doc, n| {
            for i in 0..10 {
                doc.root_mut().unwrap().replace_at(n - 1, i).unwrap();
                doc.root_mut().unwrap().move_entry(n - 1, n - 2).unwrap();
            }
            20
        },
    );
}

/// Restricted documents. A wide mapping
/// with a duplicate key (Y1013, a meaning error) near its start: edits
/// elsewhere in it never scan it for error regions. A wide nested mapping
/// in a document with a syntax error after it: its lookups use the index,
/// and so do edits in it.
#[test]
fn restricted_wide_mapping() {
    let limits = || Settings::new().limits(Limits::unlimited());
    let meaning = move |n: usize| {
        let mut s = String::from("k0: dup\n");
        s.push_str(&wide_map(n));
        let d = Document::parse_recovering_with(s, &limits()).into_value();
        assert!(d.has_errors());
        d
    };
    let none = |_: &mut Document, _: usize| {};
    wide_doc(
        "restricted (Y1013) set_path + insert/remove",
        &meaning,
        &none,
        &|doc, n| {
            let p = path![format!("k{}", n - 1)];
            for i in 0..10 {
                doc.set_path(&p, i).unwrap();
                doc.root_mut().unwrap().insert("new", i).unwrap();
                let _ = doc.root_mut().unwrap().remove("new").unwrap();
            }
            30
        },
    );
    let syntax = move |n: usize| {
        let mut s = String::from("m:\n");
        for i in 0..n {
            writeln!(s, "  k{i}: v{i}").unwrap();
        }
        s.push_str("bad: x: y\n");
        let d = Document::parse_recovering_with(s, &limits()).into_value();
        assert!(d.has_errors() && d.errors().count() == 1);
        d
    };
    wide_doc(
        "restricted (syntax) get / len",
        &syntax,
        &none,
        &|doc, n| {
            let m = doc.get("m").unwrap().as_mapping().unwrap();
            for j in 0..1_000usize {
                let k = format!("k{}", (j * 7_919) % n);
                assert!(m.get(&k).is_some());
                assert_eq!(m.len(), n);
            }
            1_000
        },
    );
    wide_doc(
        "restricted (syntax) edits in a clean mapping",
        &syntax,
        &none,
        &|doc, n| {
            let p = path!["m", format!("k{}", n - 1)];
            for i in 0..10 {
                doc.set_path(&p, i).unwrap();
                doc.get_mut("m").unwrap().insert("new", i).unwrap();
                let _ = doc.get_mut("m").unwrap().remove("new").unwrap();
            }
            30
        },
    );
}

/// Under `DuplicateKeys::LastWins`, a key repeated n
/// times (and n merge keys) costs O(log), not O(repeats), per lookup or
/// edit.
#[test]
fn last_wins_duplicates() {
    use oneyaml::DuplicateKeys;
    let settings = || {
        Settings::new()
            .limits(Limits::unlimited())
            .duplicate_keys(DuplicateKeys::LastWins)
    };
    let dups = move |n: usize| {
        let s: String = (0..n).map(|i| format!("a: {i}\n")).collect();
        Document::parse_with(s, &settings()).unwrap()
    };
    let none = |_: &mut Document, _: usize| {};
    wide_doc(
        "LastWins get / set_path / remove",
        &dups,
        &none,
        &|doc, n| {
            for i in 0..10 {
                assert!(doc.get("a").is_some());
                doc.set_path(&path!["a"], i).unwrap();
                // (The governing one goes; the one before it governs then.)
                let _ = doc.root_mut().unwrap().remove("a").unwrap();
            }
            let _ = n;
            30
        },
    );
    let merges = move |n: usize| {
        let mut s = String::from("b: &b {x: 1}\n");
        for _ in 0..n {
            s.push_str("<<: *b\n");
        }
        Document::parse_with(s, &settings().merge_keys(true)).unwrap()
    };
    wide_doc("LastWins merge keys get", &merges, &none, &|doc, _| {
        for _ in 0..1_000 {
            assert_eq!(doc.get("x").unwrap().text(), "1");
        }
        1_000
    });
}

// ----------------------------------------- multi-line flow entries

/// Multi-line flow entries chained after one long
/// line (`{k0: v, …, m0: [a,⏎ b], m1: [a,⏎ b], …}`): removing the first
/// pulls the next onto the long line; each removal (and its re-insertion
/// at the end) costs O(the entry), never O(the line).
const CHAIN: usize = 24;

fn chained_map(n: usize) -> String {
    let mut s = String::from("{");
    for i in 0..n {
        write!(s, "k{i}: v, ").unwrap();
    }
    for j in 0..CHAIN {
        write!(s, "m{j}: [a,\n b], ").unwrap();
    }
    s.push_str("end: 1}\n");
    s
}

fn chained_seq(n: usize) -> String {
    let mut s = String::from("[");
    for i in 0..n {
        write!(s, "v{i}, ").unwrap();
    }
    for _ in 0..CHAIN {
        s.push_str("[a,\n b], ");
    }
    s.push_str("end]\n");
    s
}

#[test]
fn chained_multi_line_flow_entries() {
    use std::cell::Cell;
    let step = Cell::new(0usize);
    let reset = |_: &mut Document, _: usize| step.set(0);
    let make = |n: usize| {
        Document::parse_with(chained_map(n), &Settings::new().limits(Limits::unlimited())).unwrap()
    };
    wide_doc(
        "chained flow mapping: remove + re-insert",
        &make,
        &reset,
        &|doc, n| {
            for _ in 0..10 {
                let key = format!("m{}", step.get() % CHAIN);
                step.set(step.get() + 1);
                let r = doc.root_mut().unwrap().remove(&key).unwrap();
                doc.root_mut()
                    .unwrap()
                    .insert_removed_at(n + CHAIN, r)
                    .unwrap();
            }
            20
        },
    );
    let make = |n: usize| {
        Document::parse_with(chained_seq(n), &Settings::new().limits(Limits::unlimited())).unwrap()
    };
    let none = |_: &mut Document, _: usize| {};
    wide_doc(
        "chained flow sequence: remove_at + re-insert",
        &make,
        &none,
        &|doc, n| {
            for _ in 0..10 {
                let r = doc.root_mut().unwrap().remove_at(n).unwrap();
                doc.root_mut()
                    .unwrap()
                    .insert_removed_at(n + CHAIN - 1, r)
                    .unwrap();
            }
            20
        },
    );
}

/// The same chained entries moved from the long line
/// into a block mapping (re-indented from the column `Removed` carries,
/// never found again at re-insertion), then dropped there: O(the entry)
/// per round, whatever the line before them.
#[test]
fn chained_flow_entries_into_block_context() {
    use std::cell::Cell;
    let step = Cell::new(0usize);
    let reset = |_: &mut Document, _: usize| step.set(0);
    let make = |n: usize| {
        let mut s = String::from("blk:\n  x: 1\nflow: {");
        for i in 0..n {
            write!(s, "k{i}: v, ").unwrap();
        }
        for j in 0..48 {
            write!(s, "m{j}: [a,\n b], ").unwrap();
        }
        s.push_str("end: 1}\n");
        Document::parse_with(s, &Settings::new().limits(Limits::unlimited())).unwrap()
    };
    wide_doc(
        "chained flow entry into block context",
        &make,
        &reset,
        &|doc, _| {
            for _ in 0..10 {
                let key = format!("m{}", step.get());
                step.set(step.get() + 1);
                let r = doc.get_mut("flow").unwrap().remove(&key).unwrap();
                doc.get_mut("blk").unwrap().insert_removed(r).unwrap();
                let b = doc.get_path(&path!["blk", key.clone(), 1]).unwrap();
                assert_eq!(b.text(), "b");
                let _ = doc.get_mut("blk").unwrap().remove(&key).unwrap();
            }
            30
        },
    );
}

/// Refused edits in recovered input cost O(log), not
/// O(width): an insertion into, and a removal from, a wide mapping holding
/// an error region; a root written into an input whose only content is
/// an error region (after a long run of comment lines).
#[test]
fn refusals_in_recovered_input() {
    let limits = || Settings::new().limits(Limits::unlimited());
    let errored = move |n: usize| {
        let mut s = wide_map(n);
        s.push_str("bad: x: y\n");
        Document::parse_recovering_with(s, &limits()).into_value()
    };
    let none = |_: &mut Document, _: usize| {};
    wide_doc(
        "refused insert/remove next to an error region",
        &errored,
        &none,
        &|doc, n| {
            for _ in 0..10 {
                let e = doc.root_mut().unwrap().insert("new", 1).unwrap_err();
                assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
                let k = format!("k{}", n / 2);
                let e = doc.root_mut().unwrap().remove(&k).unwrap_err();
                assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
            }
            20
        },
    );
    let error_root = move |n: usize| {
        let s = format!("{}]\n", "# c\n".repeat(n));
        Document::parse_recovering_with(s, &limits()).into_value()
    };
    wide_doc(
        "refused root over an error region",
        &error_root,
        &none,
        &|doc, _| {
            for i in 0..10 {
                let e = doc.set_root(i).unwrap_err();
                assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
                let e = doc.set_path(&path!["k"], i).unwrap_err();
                assert!(matches!(e, EditError::ReadOnly { .. }), "{e:?}");
            }
            20
        },
    );
}

/// The minimum over five fresh documents parsed from `src` (not timed)
/// of the time `script` takes on one (for edits that are not undone),
/// after `warm` (not timed: a document's first edit sets up its edit
/// overlay, and touches memory the parse left cold).
fn best_fresh(src: &str, warm: &dyn Fn(&mut Document), script: &dyn Fn(&mut Document)) -> Duration {
    let settings = Settings::new().limits(Limits::unlimited());
    let mut min = Duration::MAX;
    for _ in 0..5 {
        let mut doc = Document::parse_with(src, &settings).unwrap();
        warm(&mut doc);
        let start = Instant::now();
        script(&mut doc);
        min = min.min(start.elapsed());
    }
    min
}

/// `move_entry` of a root mapping's first entry (under a
/// header comment, or not) past a large second entry. Removing the first
/// entry made the second one set the mapping's column, and the syntax
/// layer re-read that whole entry (17x from 10k to 100k nested entries,
/// 6 ms vs 104 ms per round): its column is unchanged, so its value is
/// peeked.
#[test]
fn moving_a_first_entry_past_a_large_one_does_not_scale() {
    for head in ["# header\n\n", ""] {
        let src = |n: usize| {
            let mut s = String::from(head);
            s.push_str("k0: 0\nbig:\n");
            for i in 0..n {
                writeln!(s, "  b{i}: {i}").unwrap();
            }
            s.push_str("z: 1\n");
            s
        };
        let (small, large) = (src(10_000), src(1_000_000));
        let script = |doc: &mut Document| {
            for _ in 0..10 {
                doc.root_mut().unwrap().move_entry(0, 1).unwrap();
                doc.root_mut().unwrap().move_entry(1, 0).unwrap();
            }
        };
        ratio(
            &format!("move_entry first [{head:?}]"),
            &small,
            &large,
            &script,
        );
        let mut d = Document::parse(small.as_str()).unwrap();
        script(&mut d);
        assert_eq!(d.to_string(), small);
    }
}

/// Setting and removing a root block collection's anchor
/// is a token edit above its first entry: before, the whole root was
/// rewritten (47 ms at 10k entries, linear). The node keeps its handle.
#[test]
fn a_root_anchor_does_not_scale() {
    type Src = fn(usize) -> String;
    let shapes: [(&str, Src); 4] = [
        ("mapping", wide_map),
        ("sequence", wide_seq),
        ("mapping under a header", |n| {
            format!("# header\n\n{}", wide_map(n))
        }),
        ("sequence after ---", |n| format!("---\n{}", wide_seq(n))),
    ];
    for (name, src) in shapes {
        let (small, large) = (src(20_000), src(2_000_000));
        let script = |doc: &mut Document| {
            for _ in 0..10 {
                let r = doc.root().unwrap().id();
                let a = doc.set_anchor(r, Some("a")).unwrap();
                doc.set_anchor(a, None).unwrap();
            }
        };
        ratio(
            &format!("root set_anchor ({name})"),
            &small,
            &large,
            &script,
        );
        let mut d = Document::parse(small.as_str()).unwrap();
        script(&mut d);
        assert_eq!(d.to_string(), small);
        // The root keeps its handle (a token edit).
        let r = d.root().unwrap().id();
        assert_eq!(d.set_anchor(r, Some("a")).unwrap(), r);
        assert_eq!(d.root().unwrap().anchor(), Some("a"));
        assert_eq!(d.set_anchor(r, None).unwrap(), r);
    }
}

/// Pushing onto a root sequence whose last item has a
/// same-line comment. Small items cost the same at every size now;
/// a last item written as one long plain scalar or flow collection
/// was re-read whole (to remove its comment, then to check the new
/// item): 1.7 ms vs 136 ms at 10k vs 100k flow items, before.
#[test]
fn pushing_after_a_commented_last_item_does_not_scale() {
    // Many small items.
    let src = |n: usize| {
        let mut s = wide_seq(n);
        s.push_str("- last # c\n");
        s
    };
    ratio(
        "push after `- last # c`",
        &src(20_000),
        &src(2_000_000),
        &|doc| {
            for _ in 0..10 {
                doc.root_mut().unwrap().push(1).unwrap();
                let n = doc.root().unwrap().as_sequence().unwrap().len();
                let _ = doc.root_mut().unwrap().remove_at(n - 1).unwrap();
            }
        },
    );
    // One long last item (removing the pushed item after it again still
    // re-reads that item: timed on fresh documents, the push alone).
    type Src = fn(usize) -> String;
    let shapes: [(&str, Src); 4] = [
        ("flow sequence", |n| {
            let items: Vec<String> = (0..n).map(|i| i.to_string()).collect();
            format!("- 0\n- [{}] # c\n", items.join(", "))
        }),
        ("plain scalar", |n| {
            let words: Vec<String> = (0..n).map(|i| format!("w{i}")).collect();
            format!("- 0\n- {} # c\n", words.join(" "))
        }),
        ("quoted scalar", |n| {
            let words: Vec<String> = (0..n).map(|i| format!("w{i}")).collect();
            format!("- 0\n- '{}' # c\n", words.join(" "))
        }),
        ("flow mapping, no comment", |n| {
            let items: Vec<String> = (0..n).map(|i| format!("k{i}: {i}")).collect();
            format!("- 0\n- {{{}}}\n", items.join(", "))
        }),
    ];
    for (name, src) in shapes {
        let (small, large) = (src(20_000), src(2_000_000));
        assert!(small.len() > 100_000 && large.len() > 10_000_000);
        let push = |doc: &mut Document| {
            doc.root_mut().unwrap().push(1).unwrap();
        };
        let warm = |doc: &mut Document| {
            doc.set_path(&path![0], 0).unwrap();
        };
        let a = best_fresh(&small, &warm, &push);
        let b = best_fresh(&large, &warm, &push);
        let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        eprintln!("push after a long {name}: {a:?} vs {b:?} (ratio {r:.2})");
        // The bound allows 10x, not 4x: on a fresh 16 MB document the
        // push meets memory the parse left cold (measured 2-4x in wall
        // time with the same instruction count, 163k vs 165k Ir at 20k vs
        // 2M words); the re-read item cost 100x before. The syntax layer's
        // tests pin the bytes re-read (`long_scalar_*` in `tree/tests.rs`).
        assert!(r < 10.0, "push after a long {name} scales: {r:.2}");
        let mut d = Document::parse(small.as_str()).unwrap();
        push(&mut d);
        assert_eq!(d.to_string(), format!("{small}- 1\n"));
    }
}

/// An edit right before an entry written
/// on one long line (`big: [0, 1, …]`) peeked at that whole line: 1.0 s
/// per edit at 4M items, 0.26 s at 1M (linear in the line). The peek now
/// reads a bounded prefix of the line.
#[test]
fn editing_next_to_a_long_flow_line_does_not_scale() {
    let src = |n: usize| {
        let items: Vec<String> = (0..n).map(|i| (i % 10).to_string()).collect();
        format!("a: 1\nbig: [{}]\nz: 2\n", items.join(", "))
    };
    let (small, large) = (src(40_000), src(4_000_000));
    ratio("edit before a long flow line", &small, &large, &|doc| {
        for i in 0..10 {
            doc.set_path(&path!["a"], i).unwrap();
            doc.set_path(&path!["z"], i).unwrap();
        }
        doc.set_path(&path!["a"], 1).unwrap();
        doc.set_path(&path!["z"], 2).unwrap();
    });
}

/// Moving an entry whose anchor has k aliases after it (out and back). The
/// check that a moved alias still follows its anchor compares the recorded runs
/// of aliases with the ranks of the name's list, O(log k); only recording the k
/// referenced aliases in the `Removed` is O(k), a copy. Measured (release,
/// `move_entry(0, 1)` then `(1, 0)`): 0.187 / 15.4 / 73.7 ms at k = 1k / 100k /
/// 400k before (hash sets and copies of all k), 0.052 / 0.141 / 0.288 ms after.
/// 1k against 200k: the bound allows 20× (a linear cost would be 200×).
#[test]
fn moving_an_anchor_with_many_aliases_costs_a_copy() {
    let src = |k: usize| {
        let mut s = String::from("anchor: &x 1\nlist: [0]\nbig:\n");
        for _ in 0..k {
            s.push_str("  - *x\n");
        }
        s
    };
    let prepare = |d: &mut Document| {
        let a = d.id_of(&path!["anchor"]).unwrap();
        let _ = d.aliases_of(a).unwrap();
    };
    let script = |doc: &mut Document| {
        let mut root = doc.root_mut().unwrap();
        for _ in 0..5 {
            root.move_entry(0, 1).unwrap();
            root.move_entry(1, 0).unwrap();
        }
    };
    let a = best_prepared(&src(1_000), &prepare, &script);
    let b = best_prepared(&src(200_000), &prepare, &script);
    let r = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("move with 1k vs 200k aliases: {a:?} vs {b:?} (ratio {r:.2})");
    assert!(r < 20.0, "a move's alias checks are not O(log k): {r:.2}");
}

/// The first edit of a `Stream` document is local
/// too. Before, it ran that document's whole meaning check to count its
/// diagnostics (`Shared::record_count`; 2.07 G Ir at 10 MB
/// against ~0.6 M for every later edit). Each run parses afresh (not
/// timed) and times one edit, the first.
#[test]
fn first_stream_edit_does_not_scale_with_the_document() {
    use oneyaml::Stream;
    let settings = Settings::new().limits(Limits::unlimited());
    let first = |src: &str| {
        let mut min = Duration::MAX;
        for _ in 0..2 {
            let mut s = Stream::parse_with(src, &settings).unwrap();
            let start = Instant::now();
            s.document_mut(0)
                .unwrap()
                .set_path(&path!["target", "a"], 7)
                .unwrap();
            min = min.min(start.elapsed());
            assert_eq!(
                s.document(0).unwrap().to_value().unwrap()["target"]["a"].as_u64(),
                Some(7)
            );
        }
        min
    };
    let small = source(2_500);
    let large = source(250_000);
    let (a, b) = (first(&small), first(&large));
    let ratio = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
    eprintln!("first stream edit: {a:?} on 100 KB, {b:?} on 10 MB (ratio {ratio:.2})");
    assert!(
        ratio < 4.0,
        "the first stream edit scales with the document: {ratio:.2}"
    );
}
