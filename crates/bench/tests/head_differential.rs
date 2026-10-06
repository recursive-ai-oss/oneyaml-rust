//! The edit differential against a recorded baseline (no regressions: an edit
//! accepted by the baseline is still accepted, with its comments placed per the
//! rulebook). The calls of `crates/oneyaml/examples/edit_differential.rs`
//! (remove, remove and re-insert, moves, inserts, replacements, anchor edits
//! over every entry of the yaml-test-suite inputs and of commented variants,
//! LF, CRLF and CR) run on the current code; every call refused now must have
//! been refused by the baseline (`data/edit-differential-baseline.tsv`,
//! recorded as `examples/edit_differential.rs` says), or be listed below in a
//! documented class (a refusal where the rulebook shows the baseline's result
//! was wrong and no correct result is expressible yet). The list is exact: a
//! listed call that is accepted again fails the test too, so it shrinks as the
//! classes are closed.
//!
//! Classes:
//!
//! * **S1** — comment lines whose column gives them to a deeper
//!   collection's footer once the entry between is removed or moved (rule
//!   6 decides by column: a collection's footer, or the next entry's
//!   leading comments, after the last entry of a deeper collection); the
//!   old result silently re-owned them.
//! * **S2** — at the top of a document, rule 3 gives the comment lines to
//!   the root node when the new first entry has an empty key.
//! * **S3** — flow entries with comments around them moved to a place
//!   whose layout cannot keep each comment's owner (not on a line of its
//!   own, or another entry's comment before its `,`).
//! * **S4** — anchor edits next to comment lines on a node's properties
//!   line (the comment would change owner, or leave the slot).
//! * **S5** — comments of other owners between a collection's properties
//!   and its only entry.
//! * **S6** — block scalars and explicit keys next to comment lines whose
//!   re-read would change a meaning.
//!
//! The broadened call set (middle moves, re-insertion into another
//! collection `xins`, `remove_keep_comments`) adds these classes:
//!
//! * **P1** — a block entry with comments re-inserted into a flow
//!   collection (the baseline's `Removed` dropped the
//!   comments).
//! * **P2** — the S1/S2 re-owning on the new calls (`xins`, middle moves).
//! * **P3** — the S3 flow layout on the new calls.
//! * **P4** — a replacement whose entry comment lies inside the replaced
//!   value and cannot be written back on the new value's line
//!   (`HEADER_COMMENT_LIMIT`; the baseline deleted the comment silently).
//! * **P5** — middle moves of a block scalar next to blank lines and
//!   comments (H2RW, MZX3), refused `NotWellFormed`.
//! * **P6** — the S4 anchor removal family.
//! * **P7** — removing the only entry of a nested collection with a
//!   comment on its properties line (`NESTED_COLLAPSE`).
//! * **S5** on `xins`.
//!
//! The column-0 flow comment lines of the corpus
//! extras (`f0*`) moved or re-inserted elsewhere (P3), root anchoring next
//! to header lines or on an empty root (S4), root anchor removal (P6), and
//! **P8** — anchoring a root mapping whose first entry has an empty key
//! below comment lines. On the unmodified (`/p/`) inputs, the baseline's
//! results for every listed call re-owned `# c`;
//! the re-insertions at the same place that the baseline got right are fixed.
//!
//! Each class is refused with a specific error documented on `Document` (an
//! `Unsupported` reason or `CommentReattaches`) and nothing changed: the test
//! checks the errors against [`DOCUMENTED_ERRORS`].
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    dead_code
)]

use std::collections::{BTreeMap, HashMap, HashSet};

#[path = "../../oneyaml/examples/edit_differential.rs"]
mod diff;

const S1: &str = "S1 footer column";
const S2: &str = "S2 rule 3 root";
const S3: &str = "S3 flow layout";
const S4: &str = "S4 anchor next to comment lines";
const S5: &str = "S5 properties and only entry";
const S6: &str = "S6 block scalar or explicit key";
const P1: &str = "P1 block entry with comments into flow";
const P2: &str = "P2 re-owning on the new calls";
const P3: &str = "P3 flow layout on the new calls";
const P4: &str = "P4 entry comment inside a replaced value";
const P5: &str = "P5 block scalar middle move";
const P6: &str = "P6 anchor removal";
const P7: &str = "P7 nested collapse with a properties-line comment";
const P8: &str = "P8 root anchor above an empty key";

/// Calls refused now and accepted by the baseline, each in a documented class
/// (`*`: both `lf` and `crlf`).
const DOCUMENTED_REFUSALS: &[(&str, &str)] = &[
    ("3ALJ/c2/*#0:mv0>1", S1),
    ("3ALJ/c2/*#0:rm1", S1),
    ("3ALJ/c2/*#0:rt1", S1),
    ("4ABK/c2/*#0:mv1>2", S3),
    ("7ZZ5/c2/*#0:mv0>2", S1),
    ("8UDB/c2/*#0:mv4>0", S3),
    ("8UDB/c2/*#0:rt4", S3),
    ("93JH/c1/*#0:mv0>1", S1),
    ("93JH/c1/*#0:rm1", S1),
    ("93JH/c1/*#0:rt1", S1),
    ("9FMG/c2/*#0:mv0>1", S1),
    ("9FMG/c2/*#0:mv1>0", S1),
    ("9FMG/c2/*#0:rm1", S1),
    ("9FMG/c2/*#0:rt1", S1),
    ("BU8L/c2/*#1:rm0", S5),
    ("DBG4/c1/*#1:mv2>0", S3),
    ("DBG4/c1/*#1:mv2>4", S3),
    ("DBG4/c1/*#1:rm4", S3),
    ("DBG4/c2/*#1:mv2>0", S3),
    ("DBG4/c2/*#1:mv2>4", S3),
    ("J7PZ/c1/*#0:mv0>2", S1),
    ("JR7V/c2/*#0:mv2>7", S1),
    ("KK5P/c1/*#10:ins1", S6),
    ("KK5P/c1/*#10:pushml", S6),
    ("KK5P/c1/*#9:anc0", S4),
    ("KMK3/c1/*#0:mv0>1", S1),
    ("KMK3/c1/*#0:rm1", S1),
    ("KMK3/c1/*#0:rt1", S1),
    ("KMK3/c2/*#0:mv0>1", S1),
    ("KMK3/c2/*#0:rm1", S1),
    ("KMK3/c2/*#0:rt1", S1),
    ("M2N8/00/c2/*#1:anc0", S1),
    ("M2N8/00/c2/*#1:ml0", S1),
    ("M2N8/00/c2/*#1:set0", S1),
    ("M2N8/01/c1/*#0:anc0", S1),
    ("M2N8/01/c1/*#0:ml0", S1),
    ("M2N8/01/c1/*#0:set0", S1),
    ("M2N8/01/c2/*#0:anc0", S1),
    ("M2N8/01/c2/*#0:ml0", S1),
    ("M2N8/01/c2/*#0:set0", S1),
    ("M7NX/c1/*#1:mv0>2", S3),
    ("RLU9/c1/*#0:mv0>1", S1),
    ("RLU9/c1/*#0:mv1>0", S1),
    ("RLU9/c1/*#0:rm1", S1),
    ("RLU9/c1/*#0:rt1", S1),
    ("RLU9/c2/*#0:mv0>1", S1),
    ("RLU9/c2/*#0:mv1>0", S1),
    ("RLU9/c2/*#0:rm1", S1),
    ("RLU9/c2/*#0:rt1", S1),
    ("S3PD/c1/*#0:mv0>2", S2),
    ("S3PD/c1/*#0:rm0", S2),
    ("S3PD/c1/*#0:rt0", S2),
    ("S9E8/c1/*#0:mv0>1", S1),
    ("S9E8/c1/*#0:rm1", S1),
    ("S9E8/c1/*#0:rt1", S1),
    ("S9E8/c2/*#0:mv0>1", S1),
    ("S9E8/c2/*#0:mv1>0", S1),
    ("S9E8/c2/*#0:rm1", S1),
    ("S9E8/c2/*#0:rt1", S1),
    ("U3XV/c2/*#0:mv0>6", S1),
    ("U3XV/c2/*#0:unanc4", S4),
    ("XW4D/c1/*#0:unanc4", S4),
    ("ZF4X/c2/*#1:mv0>1", S3),
    // The broadened differential.
    ("57H4/c1/*#0:xins", P2),
    ("6BCT/c1/*#0:xins", P2),
    ("6HB6/c1/*#1:ml1", P4),
    ("6HB6/c2/*#1:ml1", P4),
    ("6HB6/p/*#1:ml1", P4),
    ("8UDB/c2/*#0:mv4>2", P3),
    ("BU8L/c2/*#0:ml0", P4),
    ("BU8L/c2/*#0:set0", P4),
    ("BU8L/c2/*#1:xins", S5),
    ("CFD4/c2/*#0:xins", P1),
    ("DBG4/c1/*#0:xins", P1),
    ("DBG4/c2/*#0:xins", P1),
    ("DBG4/p/*#0:xins", P1),
    ("DFF7/c2/*#0:mv0>1", P3),
    ("EHF6/c2/*#0:ml0", P4),
    ("EHF6/c2/*#0:set0", P4),
    ("H2RW/c2/*#0:mv2>1", P5),
    ("JR7V/c1/*#6:xins", P1),
    ("JR7V/c2/*#6:xins", P1),
    ("M7NX/c1/*#1:mv0>1", P3),
    ("M7NX/c1/*#1:xins", P3),
    ("MZX3/c1/*#0:mv3>2", P5),
    ("S3PD/c1/*#0:mv0>1", P2),
    ("S3PD/c1/*#0:mv2>1", P2),
    ("U3XV/c1/*#0:unanc4", P6),
    ("U3XV/c2/*#0:xins", P2),
    ("UDR7/c1/*#0:xins", P1),
    ("XW4D/c2/*#0:unanc4", P6),
    ("XW4D/c2/*#1:rm0", P7),
    ("XW4D/c2/*#1:xins", P7),
    ("XW4D/p/*#0:unanc4", P6),
    ("Y79Y/002/c1/*#1:xins", P3),
    ("YD5X/c2/*#0:xins", P1),
    ("ZF4X/c1/*#0:xins", P1),
    // Root anchoring and column-0 flow comment lines.
    ("3R3P/c1/*#r:unancroot", P6),
    ("3R3P/c2/*#r:unancroot", P6),
    ("6BFJ/c1/*#r:unancroot", P6),
    ("DK95/07/c1/*#r:ancroot", S4),
    ("DK95/07/c2/*#r:ancroot", S4),
    ("FTA2/c2/*#r:unancroot", P6),
    ("MUS6/02/c2/*#r:ancroot", S4),
    ("MUS6/03/c1/*#r:ancroot", S4),
    ("MUS6/04/c2/*#r:ancroot", S4),
    ("MUS6/05/c1/*#r:ancroot", S4),
    ("MUS6/05/c2/*#r:ancroot", S4),
    ("MUS6/06/c1/*#r:ancroot", S4),
    ("NHX8/c1/*#r:ancroot", P8),
    ("NHX8/c2/*#r:ancroot", P8),
    ("SM9W/01/c2/*#r:ancroot", P8),
    ("f0a/c1/*#1:mv0>1", P3),
    ("f0a/c1/*#1:xins", P3),
    ("f0a/c2/*#1:mv0>1", P3),
    ("f0a/c2/*#1:xins", P3),
    ("f0a/p/*#1:mv0>1", P3),
    ("f0a/p/*#1:xins", P3),
    ("f0b/c1/*#1:mv0>1", P3),
    ("f0b/c1/*#1:mv0>2", P3),
    ("f0b/c2/*#1:mv0>1", P3),
    ("f0b/c2/*#1:mv0>2", P3),
    ("f0b/c2/*#1:rt0", P3),
    ("f0b/p/*#1:mv0>1", P3),
    ("f0b/p/*#1:mv0>2", P3),
    ("f0c/c1/*#1:mv0>1", P3),
    ("f0c/c1/*#1:rt0", P3),
    ("f0c/c1/*#1:xins", P3),
    ("f0c/c2/*#1:mv0>1", P3),
    ("f0c/c2/*#1:rt0", P3),
    ("f0c/c2/*#1:xins", P3),
    ("f0c/p/*#1:mv0>1", P3),
    ("f0c/p/*#1:xins", P3),
    ("f0d/c1/*#3:mv1>0", P3),
    ("f0d/c1/*#3:mv1>2", P3),
    ("f0d/c2/*#3:mv1>0", P3),
    ("f0d/c2/*#3:mv1>2", P3),
    ("f0d/p/*#3:mv1>0", P3),
    ("f0d/p/*#3:mv1>2", P3),
    ("f0e/c1/*#3:rt0", P3),
    ("f0e/c1/*#3:xins", P3),
    ("f0e/c2/*#3:rt0", P3),
    ("f0e/c2/*#3:xins", P3),
    ("f0e/p/*#3:rt0", P3),
    ("f0e/p/*#3:xins", P3),
    ("f0f/c1/*#1:rt0", P3),
    ("f0f/c2/*#1:rt0", P3),
    ("f0f/p/*#1:rt0", P3),
    // Fuzz finding E4: inserting into a
    // single-pair flow mapping (`[ a: b ]`, rewritten as `{a: b, new: 1}`)
    // next to the pair's comments. Before (and in the baseline) the accepted
    // result gave the pair's leading and trailing comments to the new
    // `{…}` item (`# t -> Trailing(foo)` became `Trailing({foo: bar, …})`;
    // CN3R `xins` left `# t` on the emptied `{}`): S3 on `add`/`addml`, P3
    // on `xins`.
    ("4FJ6/c2/*#1:add", S3),
    ("4FJ6/c2/*#1:addml", S3),
    ("CN3R/c1/*#3:xins", P3),
    ("CN3R/c1/*#4:add", S3),
    ("CN3R/c1/*#4:addml", S3),
    ("L9U5/c1/*#2:add", S3),
    ("L9U5/c1/*#2:addml", S3),
    ("LQZ7/c2/*#2:add", S3),
    ("LQZ7/c2/*#2:addml", S3),
    ("QF4Y/c1/*#1:add", S3),
    ("QF4Y/c1/*#1:addml", S3),
];

#[test]
fn no_regression_against_the_baseline() {
    let dir = test_suite::default_data_dir();
    let now = diff::run(&dir);
    let data = include_str!("../data/edit-differential-baseline.tsv");
    let calls: usize = data
        .lines()
        .find_map(|l| l.split("Calls: ").nth(1))
        .and_then(|n| n.trim_end_matches('.').parse().ok())
        .unwrap();
    assert_eq!(
        now.len(),
        calls,
        "the call set changed: regenerate the baseline"
    );
    let refused_then: HashMap<&str, &str> = data
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('\t'))
        .collect();
    let listed: HashMap<&str, &str> = DOCUMENTED_REFUSALS.iter().copied().collect();
    let mut regressions = Vec::new();
    let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut improved = 0usize;
    // Each class is refused with a specific,
    // documented error.
    let mut errors: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    // `remove_keep_comments` (`keep<i>`) does not exist at
    // the baseline (`NA` there): not compared; its refusals are tallied, and
    // never `Internal`.
    let mut keep: BTreeMap<&str, usize> = BTreeMap::new();
    for (k, v) in &now {
        if k.contains(":keep") {
            assert!(v != "E:Internal", "{k}: an Internal refusal");
            *keep.entry(v.as_str()).or_default() += 1;
            continue;
        }
        let then = refused_then.get(k.as_str());
        if v == "OK" {
            improved += usize::from(then.is_some());
            continue;
        }
        if then.is_some() {
            continue;
        }
        let norm = k
            .replace("/crlf#", "/*#")
            .replace("/lf#", "/*#")
            .replace("/cr#", "/*#");
        match listed.get(norm.as_str()) {
            Some(c) => {
                *classes.entry(c).or_default() += 1;
                *errors.entry((c, v.as_str())).or_default() += 1;
                seen.insert(norm);
            }
            None => regressions.push(format!("{k}: accepted by the baseline, now {v}")),
        }
    }
    let stale: Vec<_> = DOCUMENTED_REFUSALS
        .iter()
        .filter(|(k, _)| !seen.contains(*k))
        .collect();
    eprintln!(
        "edit differential: {} calls, {} refused by the baseline and accepted now, {} \
         regressions outside the documented list, documented classes {classes:?}",
        now.len(),
        improved,
        regressions.len()
    );
    let shown = if std::env::var("ONEYAML_DIFF_ALL").is_ok() {
        usize::MAX
    } else {
        20
    };
    for r in regressions.iter().take(shown) {
        eprintln!("  {r}");
    }
    for ((c, e), n) in &errors {
        eprintln!("  {c}: {e}: {n}");
    }
    eprintln!("  remove_keep_comments outcomes: {keep:?}");
    // Each class's own errors.
    let undocumented: Vec<_> = errors
        .keys()
        .filter(|ce| !DOCUMENTED_ERRORS.contains(ce))
        .collect();
    assert!(
        undocumented.is_empty(),
        "documented calls refused with an error outside the documented ones: {undocumented:?}"
    );
    assert!(regressions.is_empty(), "{} regressions", regressions.len());
    assert!(
        stale.is_empty(),
        "listed but accepted now (shrink the list): {stale:?}"
    );
}

/// The errors the documented classes may be refused with (each a
/// specific error, documented on `Document`).
const DOCUMENTED_ERRORS: &[(&str, &str)] = &[
    // S1: rule 6 gives the comment lines to a deeper collection's footer.
    (S1, "E:CommentReattaches:CollectionFooter"),
    // S2: rule 3 gives them to the root node.
    (S2, "E:CommentReattaches:FirstNodeLeading"),
    // S3: flow layout.
    (S3, "E:CommentReattaches:Flow"),
    // Fuzz finding E4: the pair's same-line comment (`[ foo: bar # t ]`).
    (S3, "E:CommentReattaches:Trailing"),
    (
        S3,
        "E:Unsupported:a flow entry with comments around it cannot be re-inserted yet",
    ),
    (
        S3,
        "E:Unsupported:removing a flow entry next to comments of other owners that the flow \
         layout cannot keep",
    ),
    // S4: anchor edits next to comment lines.
    (S4, "E:CommentReattaches:Trailing"),
    (
        S4,
        "E:Unsupported:removing the anchor of a node whose properties stand on a line of their \
         own above comment lines",
    ),
    // S5.
    (
        S5,
        "E:Unsupported:comments of other owners between a collection's properties and its only \
         entry",
    ),
    // S6.
    (
        S6,
        "E:Unsupported:appending after comments that lie past an empty value (a `? ` key \
         written without `:`)",
    ),
    // P1: the 0.2 reason.
    (
        P1,
        "E:Unsupported:block content or comments cannot move into a flow collection",
    ),
    // P2: the S1/S2 rules.
    (P2, "E:CommentReattaches:CollectionFooter"),
    (P2, "E:CommentReattaches:FirstNodeLeading"),
    (P2, "E:CommentReattaches:Leading"),
    // P3: the S3 errors.
    (P3, "E:CommentReattaches:Flow"),
    (
        P3,
        "E:Unsupported:a flow entry with comments around it cannot be re-inserted yet",
    ),
    // P4: `HEADER_COMMENT_LIMIT`.
    (
        P4,
        "E:Unsupported:the entry's comment inside the replaced value cannot be written back \
         there",
    ),
    // P5: block scalars beside blank lines and comments.
    (P5, "E:NotWellFormed"),
    // P6: the S4 family.
    (P6, "E:CommentReattaches:Leading"),
    // S4 on root anchoring (an empty root, header lines).
    (S4, "E:CommentReattaches:DocumentFooter"),
    // P6 on root anchor removal.
    (P6, "E:CommentReattaches:FirstNodeLeading"),
    (P6, "E:CommentReattaches:Trailing"),
    (
        P6,
        "E:Unsupported:removing the anchor of a node whose properties stand on a line of their \
         own above comment lines",
    ),
    // P8.
    (P8, "E:CommentReattaches:ExplicitValue"),
    // P7: `NESTED_COLLAPSE`.
    (
        P7,
        "E:Unsupported:the footer comments of a nested collection whose only entry is removed \
         have no owner",
    ),
];
