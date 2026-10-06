//! Remove → re-insert at the same position restores the bytes (the comment edit
//! rules, and a byte-identical round trip): over every yaml-test-suite input
//! (valid ones, LF, CRLF and CR) and generated documents with comment lines,
//! blank lines and same-line comments injected, every entry and item of every
//! collection is removed ([`Document::remove_node`]) and put back at its index
//! ([`NodeMut::insert_removed_at`]) on a copy of the document; the text must be
//! the original byte for byte, and the removed entry's comments
//! ([`Removed::comments`]) must be exactly those the rulebook gives it.
//!
//! Every successful removal is also checked against the rulebook: its
//! `Removed::comment_ids` are exactly the comments whose owner lies inside
//! the entry (read with `Document::explain_comments` before the removal),
//! in order.
//!
//! Not round trips, and counted apart (each a documented refusal or
//! rewrite): the only entry of a collection (the collection becomes
//! `{}`/`[]`, a flow collection a block entry does not go back into),
//! removals refused with a documented class (an anchor an alias outside
//! still uses, a key that is a collection, comments between a collection's
//! properties and its only entry, a flow entry next to other owners'
//! comments the flow layout cannot keep; any other refused removal fails
//! the test), and re-insertions refused with a documented
//! `EditError::Unsupported` (a flow entry with comments around it or
//! another owner's comments before its `,`; a tag in a `%TAG` document;
//! an item appended back after comments past a `? ` key's empty value), or
//! by the conservative anchor checks (`DuplicateAnchor`,
//! `AliasBeforeAnchor`: a name defined more than once in the document).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod generator;

use generator::Rng;
use oneyaml::{Document, EditError, NodeId, NodeKind, Settings};

/// Every collection of the document, with its entries' value handles.
fn collections(doc: &Document) -> Vec<(NodeId, Vec<NodeId>)> {
    let mut out = Vec::new();
    let mut stack: Vec<_> = doc.root().into_iter().collect();
    // A loop, not recursion; aliases are not followed.
    while let Some(n) = stack.pop() {
        if n.kind() == NodeKind::Alias {
            continue;
        }
        if let Some(m) = n.as_mapping().filter(|m| m.node() == n) {
            let mut vs = Vec::new();
            for (k, v) in m.iter() {
                stack.push(k);
                stack.push(v);
                vs.push(v.id());
            }
            out.push((n.id(), vs));
        } else if let Some(s) = n.as_sequence().filter(|s| s.node() == n) {
            let vs: Vec<NodeId> = s.iter().map(|v| v.id()).collect();
            stack.extend(s.iter());
            out.push((n.id(), vs));
        }
    }
    out
}

/// The documented limitations a re-insertion may meet (see
/// `Document`'s docs).
/// Kept in sync with the "Comments through node edits" section (the
/// limitation classes S3 and S6 are the ones a re-insertion at the same
/// place can meet).
const DOCUMENTED: &[&str] = &[
    // 0.2 limitation, and limitation S3 (flow layout).
    "a flow entry with comments around it cannot be re-inserted yet",
    // 0.2 limitation.
    "a tag written into a document with %TAG directives",
    // Limitation S6: appending an item back into a `? ` key written
    // without `:` whose last item has a same-line comment.
    "appending after comments that lie past an empty value (a `? ` key written without `:`)",
];

#[derive(Default, Debug)]
struct Tally {
    round_trips: usize,
    with_comments: usize,
    only_entries: usize,
    /// Removals refused, by class (each documented).
    removal_refused: std::collections::BTreeMap<&'static str, usize>,
    /// Removals whose `Removed::comment_ids` were checked against the
    /// rulebook.
    rulebook_checks: usize,
    reinsert_refused: usize,
    anchor_refused: usize,
    failures: Vec<String>,
}

fn sweep(name: &str, src: &str, settings: &Settings, t: &mut Tally) {
    let Ok(base) = Document::parse_with(src, settings) else {
        return;
    };
    for (coll, values) in collections(&base) {
        let n = values.len();
        for (i, v) in values.iter().enumerate() {
            let mut d = base.clone();
            if n == 1 {
                t.only_entries += 1;
                continue;
            }
            // The comments the rulebook gives the entry (explicit owners
            // aside: a parsed document has none), in document order.
            let want = owned_by_entry(&base, coll, *v);
            let r = match d.remove_node(*v) {
                Ok(r) => r,
                // Only classified refusals.
                Err(e) => {
                    match removal_class(&e) {
                        Some(c) => *t.removal_refused.entry(c).or_default() += 1,
                        None => t.failures.push(format!(
                            "{name}: entry {i} of {coll:?}: removal {e:?}: {src:?}"
                        )),
                    }
                    continue;
                }
            };
            if r.comment_ids() != want.as_slice() {
                t.failures.push(format!(
                    "{name}: entry {i} of {coll:?}: took {:?}, the rulebook gives {want:?}: {src:?}",
                    r.comment_ids()
                ));
            }
            t.rulebook_checks += 1;
            let has = !r.comments().is_empty();
            let back = d.node_mut(coll).and_then(|mut c| {
                c.insert_removed_at(i, r)
                    .map(|n| n.id())
                    .map_err(|e| e.into_error())
            });
            match back {
                Ok(_) => {
                    let text = d.to_string();
                    if text.as_bytes() == src.as_bytes() {
                        t.round_trips += 1;
                        t.with_comments += usize::from(has);
                    } else {
                        t.failures.push(format!(
                            "{name}: entry {i} of {coll:?}: {src:?}\n  -> {text:?}"
                        ));
                    }
                }
                Err(EditError::Unsupported { reason, .. }) => {
                    assert!(
                        DOCUMENTED.contains(&reason),
                        "{name}: entry {i}: undocumented refusal {reason}: {src:?}"
                    );
                    t.reinsert_refused += 1;
                }
                // Conservatively, an anchor re-inserted before
                // another anchor of its name, or with aliases of its name
                // after it that it did not own, is refused (documented on
                // `NodeMut::insert_removed`).
                Err(EditError::DuplicateAnchor { .. } | EditError::AliasBeforeAnchor { .. }) => {
                    t.anchor_refused += 1;
                }
                Err(e) => t
                    .failures
                    .push(format!("{name}: entry {i} of {coll:?}: {e:?}: {src:?}")),
            }
        }
    }
}

/// The documented class of a refused removal (`None`: not documented).
fn removal_class(e: &EditError) -> Option<&'static str> {
    match e {
        // An anchor an alias outside still uses.
        EditError::AnchorInUse { .. } => Some("anchor in use"),
        EditError::Unsupported { reason, .. } => match *reason {
            "removing an entry with a collection key" => Some("collection key (0.2 limitation)"),
            "comments of other owners between a collection's properties and its only entry" => {
                Some("comments between properties and an only entry (0.3 limitation)")
            }
            // Limitation S3.
            "removing a flow entry next to comments of other owners that the flow layout cannot \
             keep" => Some("flow layout (0.3 limitation, S3)"),
            _ => None,
        },
        _ => None,
    }
}

/// The handles of the comments the rulebook gives the entry of `coll`
/// whose value is `v` (owners inside the entry, the collection's own
/// excepted), in document order.
fn owned_by_entry(doc: &Document, coll: NodeId, v: NodeId) -> Vec<oneyaml::CommentId> {
    let (Ok(c), Ok(value)) = (doc.node(coll), doc.node(v)) else {
        return Vec::new();
    };
    let start = c
        .as_mapping()
        .filter(|m| m.node() == c)
        .and_then(|m| {
            m.iter()
                .find(|(_, x)| x.id() == v)
                .map(|(k, _)| k.span().start)
        })
        .unwrap_or(value.span().start);
    let (lo, hi) = (start, value.span().end);
    let Ok(all) = doc.explain_comments() else {
        return Vec::new();
    };
    all.iter()
        .filter(|e| {
            let node = match e.owner() {
                oneyaml::CommentOwner::Leading(n)
                | oneyaml::CommentOwner::Trailing(n)
                | oneyaml::CommentOwner::Footer(n) => n,
                _ => return false,
            };
            node != coll && e.owner_span().is_some_and(|s| lo <= s.start && s.end <= hi)
        })
        .map(|e| e.id())
        .collect()
}

/// `src` with comment lines (random columns, some after blank lines) and
/// same-line comments injected.
fn commented(rng: &mut Rng, src: &str) -> String {
    let mut lines: Vec<String> = src.lines().map(str::to_owned).collect();
    for l in &mut lines {
        let plain = !l.contains(['#', '\'', '"', '|', '>', '[', '{']) && !l.trim().is_empty();
        if plain && rng.below(3) == 0 {
            l.push_str(" # t");
        }
    }
    for _ in 0..rng.below(10) {
        let at = rng.below(lines.len() + 1);
        let col = [0, 1, 2, 4, 6][rng.below(5)];
        lines.insert(at, format!("{}# c{at}", " ".repeat(col)));
        if rng.below(3) == 0 {
            lines.insert(at, String::new());
        }
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[test]
fn remove_then_reinsert_at_the_same_place_restores_the_bytes() {
    let cases = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    assert!(cases.len() > 300);
    let mut t = Tally::default();
    let settings = Settings::new();
    for case in &cases {
        let Ok(src) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        for nl in ["\n", "\r\n", "\r"] {
            let src = src.replace('\n', nl);
            sweep(&case.id, &src, &settings, &mut t);
        }
    }
    let mut g = Rng(0x31_31);
    let merge = Settings::new().merge_keys(true);
    for r in 0..300 {
        let plain = generator::generate(&mut g);
        let src = commented(&mut g, &plain);
        let src = match r % 3 {
            1 => src.replace('\n', "\r\n"),
            2 => src.replace('\n', "\r"),
            _ => src,
        };
        sweep(&format!("commented {r}"), &src, &merge, &mut t);
    }
    eprintln!(
        "re-insertion round trips: {} (of them {} with comments), {} only entries, {} removals \
         checked against the rulebook, removals refused {:?}, {} re-insertions refused \
         (documented), {} refused by the anchor rules, {} failures",
        t.round_trips,
        t.with_comments,
        t.only_entries,
        t.rulebook_checks,
        t.removal_refused,
        t.reinsert_refused,
        t.anchor_refused,
        t.failures.len()
    );
    for f in t.failures.iter().take(20) {
        eprintln!("  {f}");
    }
    assert!(t.failures.is_empty(), "{} failures", t.failures.len());
    assert!(t.round_trips > 1000, "{}", t.round_trips);
    assert!(t.with_comments > 50, "{}", t.with_comments);
}
