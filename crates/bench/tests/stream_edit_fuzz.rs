//! The `Stream` edit-script fuzz:
//! seeded random edit scripts over generated multi-document streams —
//! parsed strictly (when they parse) and with recovery, errors at document
//! ends and at boundaries (an unclosed `[` cut off by the next `---`,
//! meaning errors, lenient `k: a: b` lines), LF/CRLF/CR, and
//! `lenient_inline_colons` on and off — with `document_mut` switches,
//! stream clones, and document snapshots mid-script. The stream edit layer
//! produced every recent bug; the `Document` fuzz (`edit_fuzz.rs`) covers
//! one document only.
//!
//! After every call:
//!
//! * nothing panics, and no call returns `EditError::Internal`;
//!   every `EditError::Unsupported` is one documented on `Document`
//!   (`documented/mod.rs`, shared with `edit_fuzz.rs`);
//! * a refused call leaves the stream's text byte-identical, and a refused
//!   re-insertion hands its `Removed` back unchanged;
//! * an accepted call keeps one meaning: `diagnostics()`, `has_errors()`, and
//!   each document's `diagnostics()`, `has_errors()` and `to_value()`
//!   equal those of a re-parse of the text in the same mode;
//! * an accepted call never removes or replaces an error node (error
//!   regions are read-only; the count of `Document::errors` over the
//!   documents is unchanged);
//! * clones and snapshots are independent: editing one never changes the
//!   other's text;
//! * comments are conserved:
//!   after every call that replaces no existing value (all but `set_path`
//!   and `set_root`), accepted or refused, the stream's comments plus those
//!   held by the pool of `Removed` values are as many as before;
//! * **comment identity against the rulebook** (its edit semantics,
//!   `docs/comment-attachment.md`; the stream form of `edit_fuzz.rs`'s
//!   accounting), read with explicit owners set aside, after every accepted
//!   call: the other documents' comments keep their handles, owners and texts
//!   (stream edits are local); in the edited document, the document header and
//!   footer never change as a side effect (the same texts, in order; a removal
//!   from the root collection puts in front of the footer exactly what the call
//!   itself gives it — `remove_keep_comments` of the last entry its leading
//!   comments, the call being the explicit request, and the only entry the
//!   collection's own footers, rule 7); for calls that replace no value, a
//!   removal's [`Removed::comment_ids`] are comments of the document that are
//!   gone afterwards, and every other comment keeps its handle and text or is
//!   rewritten (same text, same header/footer role, a new handle); a
//!   re-insertion brings back exactly its `Removed`'s comments, none as header
//!   or footer; nothing is made up; and the edited stream, parsed again,
//!   explains every comment of the document as it does (same text, rule, and
//!   owner span).
//!
//! The generator builds the block-scalar shape on purpose (a block scalar entry
//! followed by a comment line, then moved, appended after, removed and
//! re-inserted) and the error-node shape (an error node whose diagnostic the
//! next document owns, then an edit over it); and the block-scalar-entry
//! shapes: the last entry of a block collection holding a block scalar after
//! another entry that ends its line, followed by a comment line (whose moves
//! lost that comment), and block scalar entries of every chomping and style at
//! the ends of nested collections (whose removal was refused).
//!
//! Sectionable (the release acceptance run is 50k rounds in sections):
//! `ONEYAML_STREAM_FUZZ_SECTION=i/n` runs every n-th round from the i-th,
//! `ONEYAML_STREAM_FUZZ_ROUNDS` (default 400), `ONEYAML_STREAM_FUZZ_STEPS`
//! (calls per stream, default 12), `ONEYAML_STREAM_FUZZ_SEED`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod documented;
pub(crate) mod generator;
pub(crate) mod known;
mod owners;

use std::collections::{BTreeMap, HashMap, HashSet};

use documented::DOCUMENTED_UNSUPPORTED;
use generator::Rng;
use oneyaml::{
    CommentId, CommentOwner, Document, EditError, NodeKind, Path, Removed, Settings, Severity,
    Span, Stream, Value,
};

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn section() -> (usize, usize) {
    std::env::var("ONEYAML_STREAM_FUZZ_SECTION")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once('/')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .filter(|(i, n)| *n > 0 && i < n)
        .unwrap_or((0, 1))
}

/// Document bodies (each ends with a line break). The first ones are the
/// refused-edit shapes: the block-scalar shape (block scalar, comment line) and
/// the error-node shape (an unclosed flow collection cut off by the next
/// document).
const BODIES: &[&str] = &[
    "k: |\n  text\n# c\nm: 2\n# tail\n",
    "k: |\n  text\n# c\nm: 2\n",
    "- |\n  text\n# c\n- 2\n- 3\n",
    "a: [1, 2\n",
    // The error-node shape exactly: the next document owns the diagnostic and
    // has a meaning error that the recovering checks skip.
    "a: [1, 2\n---\nt: !!int x\nu: 2\n",
    "a: {p: 1\n",
    "t: !!int x\nu: 2\n",
    "a: 1\nb:\n  - x\n  - y # c\nc: {p: 1, q: [2, 3]}\n",
    "a: &x 1\nb: *x\nl: [*x, 2]\n",
    "k: x: z\nb: 1\n",
    "b: 1\nk: x: z\n",
    "# only a comment\n",
    "key: >\n  folded\n  text\n\n# gap\nz: 3\n",
    "? [a, b]\n: 1\nc: 2\n",
    "- a\n- b: |+\n    keep\n\n# after\n- c\n",
    "a: 'unclosed\n",
    "x: 1\n...\n",
    "s:\n- 1\n- 2\n# end\n",
    "plain scalar\n",
    // Block scalar entries.
    "k: |\n  text\n# c\nm: |\n  t2\n# c2\n",
    "o:\n  k: |\n    s\n  m: |\n    t\n  # c2\nz: 1\n",
    "- |\n  a\n- |\n  b\n# c2\n",
    "- |\n  a\n# c1\n- |\n  b\n# c2\n",
    "k: 1\nm: |\n  t2\n# c2\nz: 3\n",
    "steps:\n  - name: x\n    run: |\n      echo hi\n  - name: y\n    run: >-\n      a\n      b\n",
    "data:\n  a: 1 # one\n  s.sh: |+\n    echo\n\n# end\n",
    "s:\n  - a\n  - |-\n    b\n  # tail\nz: 1\n",
];

const SEPARATORS: &[&str] = &["---\n", "--- # sep\n", "...\n---\n", "\n---\n"];

const KEYS: &[&str] = &["a", "k", "m", "new", "z", "x: y", "#", ""];

fn value(rng: &mut Rng) -> Value {
    match rng.below(10) {
        // A keep (`|+`) block scalar.
        9 => Value::from("k\n\n"),
        0 => Value::Null,
        1 => Value::from(rng.below(100) as u64),
        2 => Value::from("l1\nl2"),
        3 => Value::from("a: b"),
        4 => Value::from(true),
        5 => Value::from("text\n"),
        6 => oneyaml::from_str("{p: 1}").unwrap(),
        7 => oneyaml::from_str("[1, 2]").unwrap(),
        _ => Value::from("x"),
    }
}

/// A generated stream: 1–4 documents, sometimes with a generated
/// (merge-heavy) document among them.
fn stream_text(rng: &mut Rng) -> String {
    let n = 1 + rng.below(4);
    let mut out = String::new();
    if rng.below(3) == 0 {
        out.push_str("---\n");
    }
    for i in 0..n {
        if i > 0 {
            out.push_str(SEPARATORS[rng.below(SEPARATORS.len())]);
        }
        if rng.below(8) == 0 {
            let g = generator::generate(rng);
            if !g.contains("---") && !g.contains("...") && !g.contains('%') {
                out.push_str(&g);
                if !g.ends_with('\n') {
                    out.push('\n');
                }
                continue;
            }
        }
        out.push_str(BODIES[rng.below(BODIES.len())]);
    }
    out
}

/// Paths of document `i` (up to depth 3): (path, kind, entries).
fn paths(s: &Stream, i: usize) -> Vec<(Path, NodeKind, usize)> {
    let Some(d) = s.document(i) else {
        return Vec::new();
    };
    let Some(root) = d.root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![(Path::new(), root, 0usize)];
    while let Some((p, n, depth)) = stack.pop() {
        let len = n
            .as_mapping()
            .map(|m| m.len())
            .or_else(|| n.as_sequence().map(|q| q.len()))
            .unwrap_or(0);
        out.push((p.clone(), n.kind(), len));
        if depth >= 3 {
            continue;
        }
        if let Some(m) = n.as_mapping() {
            for (k, v) in m.iter() {
                if let Some(k) = k.as_scalar() {
                    stack.push((p.clone().with_key(k.value().into_owned()), v, depth + 1));
                }
            }
        } else if let Some(q) = n.as_sequence() {
            for (j, v) in q.iter().enumerate() {
                stack.push((p.clone().with_index(j), v, depth + 1));
            }
        }
    }
    out
}

/// Error nodes over every document.
fn error_nodes(s: &Stream) -> usize {
    s.documents().map(|d| d.errors().count()).sum()
}

fn codes(r: Result<Value, oneyaml::Error>) -> Result<Value, oneyaml::Code> {
    r.map_err(|e| e.code())
}

/// The edited syntax tree equals a fresh
/// parse of its text under its own settings, whole (structure, token
/// texts, diagnostics with every label and fix), recovered input
/// included. `false` when they differ as the one documented
/// exception (`known/mod.rs`), which the caller then stops checking.
fn whole_tree(tree: &oneyaml_syntax::tree::SyntaxTree, ctx: &str) -> bool {
    let again = oneyaml_syntax::tree::SyntaxTree::parse_with(tree.emit(), tree.settings());
    if let Err(e) = oneyaml_syntax::tree::compare_trees(tree, &again) {
        if known::excused(tree, &again) {
            return false;
        }
        panic!(
            "the edited tree differs from its re-parse ({e}): {ctx}\ntext: {:?}",
            tree.emit()
        );
    }
    true
}

/// Both readers agree after an accepted edit: the stream reads as its text
/// re-parsed in the same mode.
fn check_reparse(s: &Stream, recovering: bool, ctx: &str) {
    if !whole_tree(s.syntax(), ctx) {
        return;
    }
    let text = s.to_string();
    let settings = s.settings();
    let rec = Stream::parse_recovering_with(text.clone(), settings).into_value();
    assert_eq!(s.len(), rec.len(), "documents: {ctx}\n{text:?}");
    if recovering {
        assert_eq!(
            s.diagnostics(),
            rec.diagnostics(),
            "diagnostics: {ctx}\n{text:?}"
        );
        assert_eq!(
            s.has_errors(),
            rec.has_errors(),
            "has_errors: {ctx}\n{text:?}"
        );
        for i in 0..s.len() {
            let (a, b) = (s.document(i).unwrap(), rec.document(i).unwrap());
            assert_eq!(a.diagnostics(), b.diagnostics(), "doc {i}: {ctx}\n{text:?}");
            assert_eq!(a.has_errors(), b.has_errors(), "doc {i}: {ctx}\n{text:?}");
        }
    } else {
        match Stream::parse_with(text.clone(), settings) {
            Ok(st) => {
                assert_eq!(
                    s.diagnostics(),
                    st.diagnostics(),
                    "diagnostics: {ctx}\n{text:?}"
                );
                assert!(!s.has_errors(), "has_errors: {ctx}\n{text:?}");
                for i in 0..s.len() {
                    let (a, b) = (s.document(i).unwrap(), st.document(i).unwrap());
                    assert_eq!(a.diagnostics(), b.diagnostics(), "doc {i}: {ctx}\n{text:?}");
                    assert!(!a.has_errors(), "doc {i}: {ctx}\n{text:?}");
                }
            }
            Err(e) => {
                // An accepted meaning error: the first error
                // listed is the one the strict parse fails with.
                let first = s
                    .diagnostics()
                    .into_iter()
                    .find(|d| d.severity == Severity::Error);
                assert_eq!(first.map(|d| d.code), Some(e.code()), "{ctx}\n{text:?}");
                assert!(s.has_errors(), "has_errors: {ctx}\n{text:?}");
            }
        }
    }
    for i in 0..s.len() {
        let (a, b) = (s.document(i).unwrap(), rec.document(i).unwrap());
        assert_eq!(
            codes(a.to_value()),
            codes(b.to_value()),
            "value {i}: {ctx}\n{text:?}"
        );
    }
}

#[derive(Default)]
pub(crate) struct Tally {
    /// Exact owner checks, and the comments each live
    /// `Removed` carries (by its handles).
    owner_checks: usize,
    carried: HashMap<Vec<CommentId>, Vec<owners::Carried>>,
    streams: usize,
    ok: usize,
    refused: BTreeMap<String, usize>,
    clones: usize,
    snapshots: usize,
    b1: usize,
    /// Comment-count checks run.
    comment_checks: usize,
    /// Identity checks run, and comments followed through them.
    identity_checks: usize,
    taken_comments: usize,
    rewritten_comments: usize,
    brought_comments: usize,
}

/// Every comment of a document by handle, with its **rulebook** owner
/// (explicit owners set aside), text, owner span and rule.
type Snap = Vec<(CommentId, CommentOwner, String, Option<Span>, u8)>;

/// A comment as the rules read it: text, rule, owner span.
type Shape = (String, u8, Option<(usize, usize)>);

fn rule_snap(doc: &Document) -> Snap {
    let mut c = doc.clone();
    for id in c.comment_ids() {
        let _ = c.detach_comment(id);
    }
    c.explain_comments()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e.id(),
                e.owner(),
                e.text().to_owned(),
                e.owner_span(),
                e.rule().number(),
            )
        })
        .collect()
}

fn snaps(s: &Stream) -> Vec<Snap> {
    s.documents().map(|d| rule_snap(&d)).collect()
}

/// The role a comment plays for the header/footer rule.
fn role(o: CommentOwner) -> u8 {
    match o {
        CommentOwner::DocumentHeader => 1,
        CommentOwner::DocumentFooter => 2,
        _ => 0,
    }
}

/// What an accepted call did with comments, for [`account`].
enum Acct {
    /// Nothing taken or brought.
    Plain,
    /// A removal: the comment handles its `Removed` took, and the texts
    /// of the leading comments `remove_keep_comments` of a root
    /// collection's last entry gives to the document footer (the call is
    /// the explicit request), in order.
    Taken(Vec<CommentId>, Vec<String>),
    /// A re-insertion: the comment texts it brought back.
    Brought(Vec<String>),
    /// A replacement (`set_path`, `set_root`): only the header/footer and
    /// re-parse checks apply.
    Replaced,
}

/// Identity accounting of one accepted call on document `i`.
fn account(before: &[Snap], s: &Stream, i: usize, acct: &Acct, ctx: &str, t: &mut Tally) {
    let after = snaps(s);
    let text = s.to_string();
    assert_eq!(before.len(), after.len(), "documents: {ctx}");
    for (j, (b, a)) in before.iter().zip(&after).enumerate() {
        if j == i {
            continue;
        }
        let key = |x: &Snap| -> Vec<(CommentId, CommentOwner, String, u8)> {
            x.iter()
                .map(|(id, o, tx, _, r)| (*id, *o, tx.clone(), *r))
                .collect()
        };
        assert_eq!(
            key(b),
            key(a),
            "document {j} changed its comments: {ctx}\n{text:?}"
        );
    }
    let (b, a) = (&before[i], &after[i]);
    // The header and footer change only through explicit calls (as here:
    // `remove_keep_comments` of a root collection's last entry is one).
    let to_footer: &[String] = match acct {
        Acct::Taken(_, texts) => texts,
        _ => &[],
    };
    for r in [1, 2] {
        let of = |x: &Snap| -> Vec<String> {
            x.iter()
                .filter(|c| role(c.1) == r)
                .map(|c| c.2.clone())
                .collect()
        };
        let want = if r == 2 {
            to_footer.iter().cloned().chain(of(b)).collect()
        } else {
            of(b)
        };
        assert_eq!(
            want,
            of(a),
            "the document {} changed as a side effect: {ctx}\n{text:?}",
            if r == 1 { "header" } else { "footer" }
        );
    }
    if !matches!(acct, Acct::Replaced) {
        let after_ids: HashMap<CommentId, &str> = a
            .iter()
            .map(|(id, _, tx, _, _)| (*id, tx.as_str()))
            .collect();
        let before_ids: HashSet<CommentId> = b.iter().map(|c| c.0).collect();
        let taken: HashSet<CommentId> = match acct {
            Acct::Taken(ids, _) => ids.iter().copied().collect(),
            _ => HashSet::new(),
        };
        for id in &taken {
            assert!(
                before_ids.contains(id),
                "a taken comment was not the document's: {ctx}"
            );
            assert!(
                !after_ids.contains_key(id),
                "a taken comment stayed: {ctx}\n{text:?}"
            );
        }
        t.taken_comments += taken.len();
        let mut back = Vec::new();
        for (id, o, tx, _, _) in b {
            if taken.contains(id) {
                continue;
            }
            match after_ids.get(id) {
                Some(now) => assert_eq!(now, tx, "a comment changed its text: {ctx}"),
                None => back.push((tx.clone(), role(*o))),
            }
        }
        let mut fresh: Vec<(&str, u8)> = a
            .iter()
            .filter(|c| !before_ids.contains(&c.0))
            .map(|c| (c.2.as_str(), role(c.1)))
            .collect();
        let mut to_footer: Vec<&str> = to_footer.iter().map(String::as_str).collect();
        for (tx, r) in back {
            let rehomed = r == 0 && to_footer.contains(&tx.as_str());
            let want = if rehomed { 2 } else { r };
            if rehomed {
                if let Some(q) = to_footer.iter().position(|x| *x == tx) {
                    to_footer.remove(q);
                }
            }
            let Some(p) = fresh.iter().position(|f| f.0 == tx && f.1 == want) else {
                panic!("comment {tx:?} lost or re-owned: {ctx}\n{text:?}\nfresh: {fresh:?}");
            };
            fresh.remove(p);
            t.rewritten_comments += 1;
        }
        if let Acct::Brought(texts) = acct {
            for tx in texts {
                let Some(p) = fresh.iter().position(|f| f.0 == tx && f.1 == 0) else {
                    panic!("brought comment {tx:?} missing or re-owned: {ctx}\n{text:?}");
                };
                fresh.remove(p);
                t.brought_comments += 1;
            }
        }
        assert!(
            fresh.is_empty(),
            "comments made up: {fresh:?}: {ctx}\n{text:?}"
        );
    }
    // The re-parsed stream explains the document's comments alike.
    let again = Stream::parse_recovering_with(text.clone(), s.settings()).into_value();
    let shape = |x: &Snap| -> Vec<Shape> {
        x.iter()
            .map(|(_, _, tx, sp, r)| (tx.clone(), *r, sp.map(|s| (s.start, s.end))))
            .collect()
    };
    let re = again.document(i).map(|d| rule_snap(&d)).unwrap_or_default();
    assert_eq!(
        shape(&re),
        shape(a),
        "the re-parse owns the comments otherwise: {ctx}\n{text:?}"
    );
    t.identity_checks += 1;
}

/// Comments of the stream's text (tokens) plus those held by `pool`
/// (every comment is accounted for).
fn comment_count(s: &Stream, pool: &[Removed]) -> usize {
    let own = s
        .syntax()
        .root()
        .descendants()
        .filter(|n| n.kind() == oneyaml_syntax::tree::SyntaxKind::Comment)
        .count();
    own + pool.iter().map(|r| r.comments().len()).sum::<usize>()
}

fn kind(e: &EditError) -> String {
    let s = format!("{e:?}");
    s.split([' ', '{', '(']).next().unwrap_or("").to_owned()
}

/// The texts of the comments a removal of the root collection's entry at `p`
/// gives to the document footer, in order: with `keep`, the leading comments of
/// the last entry (as the call asks); when it is the only entry, the root
/// collection's own footers (rule 7).
fn to_document_footer(d: &Document, p: &Path, keep: bool) -> Vec<String> {
    let Some(v) = d.get_path(p) else {
        return Vec::new();
    };
    let Some(root) = d.root() else {
        return Vec::new();
    };
    let (owner, only, entry) = if let Some(m) = root.as_mapping().filter(|m| m.node() == root) {
        match m.iter().last() {
            Some((k, last)) if last.id() == v.id() => {
                (k.id(), m.iter().count() == 1, (Some(k.id()), v.id()))
            }
            _ => return Vec::new(),
        }
    } else if let Some(sq) = root.as_sequence().filter(|sq| sq.node() == root) {
        match sq.iter().last() {
            Some(last) if last.id() == v.id() => (v.id(), sq.iter().count() == 1, (None, v.id())),
            _ => return Vec::new(),
        }
    } else {
        return Vec::new();
    };
    // Fuzz finding E6 (an oracle fix, no library change): a
    // flow root keeps its braces and its footer when its only entry
    // leaves (`{⏎  # f⏎}`); only a block root becomes `{}`/`[]`.
    let flow = root
        .text()
        .split_whitespace()
        .find(|w| !w.starts_with(['&', '!']))
        .is_some_and(|w| w.starts_with(['{', '[']));
    let only = only && !flow;
    // The leading comments written above the entry (one written
    // inside it, after `? `, goes with it: `owners::Shape::above_entry`).
    let shape = owners::Shape::of(d);
    rule_snap(d)
        .into_iter()
        .filter(|c| {
            (keep && c.1 == CommentOwner::Leading(owner) && shape.above_entry(entry, c.0))
                || (only && c.1 == CommentOwner::Footer(root.id()))
        })
        .map(|c| c.2)
        .collect()
}

/// The first `k` segments of `p`.
fn prefix(p: &Path, k: usize) -> Path {
    let mut q = Path::new();
    for sg in p.segments().iter().take(k) {
        match sg {
            oneyaml::PathSegment::Key(x) => q.push_key(x.clone()),
            oneyaml::PathSegment::Index(i) => q.push_index(*i),
            _ => {}
        }
    }
    q
}

/// What `set_path` replaced: the deepest node along the path
/// before that is gone (`prefixes`, deepest first), and the node at its
/// path now; nothing gone: an insertion.
fn replaced(
    d: &Document,
    pairs: &HashSet<oneyaml::NodeId>,
    prefixes: &[(Path, oneyaml::NodeId)],
    p: &Path,
    new: oneyaml::NodeId,
) -> owners::Op {
    let gone = prefixes.iter().rev().find(|(_, id)| d.node(*id).is_err());
    // As `edit_fuzz::rewritten_map`: creating a key inside a
    // single-pair flow mapping (`[k: v]`) rewrites it as `{k: v, new: x}`,
    // a new mapping at the same place whose nodes correspond to the old.
    if let Some((q, old)) = gone.filter(|(q, _)| q.segments().len() < p.segments().len()) {
        if pairs.contains(old) {
            if let Some(n) = d.get_path(q) {
                if n.kind() == NodeKind::Mapping {
                    return owners::Op::Rewrite {
                        old: *old,
                        new: n.id(),
                    };
                }
            }
        }
    }
    match gone {
        Some((q, old)) => owners::Op::Replace {
            old: *old,
            new: if q.segments().len() == p.segments().len() {
                Some(new)
            } else {
                d.get_path(q).map(|n| n.id())
            },
        },
        None => owners::Op::Plain,
    }
}

/// One call on document `i` of `s`; `Err` = refused. Returns a label.
fn call(
    s: &mut Stream,
    i: usize,
    rng: &mut Rng,
    pool: &mut Vec<Removed>,
    t: &mut Tally,
) -> (String, Result<(), EditError>, Acct, owners::Op) {
    let ps = paths(s, i);
    let block = s.document(i).is_some_and(|d| d.to_string().contains('|'));
    let pick = |rng: &mut Rng, want: fn(&(Path, NodeKind, usize)) -> bool| {
        let c: Vec<_> = ps.iter().filter(|p| want(p)).cloned().collect();
        (!c.is_empty()).then(|| c[rng.below(c.len())].clone())
    };
    let is_coll = |p: &(Path, NodeKind, usize)| {
        matches!(p.1, NodeKind::Mapping | NodeKind::Sequence) && p.2 > 0
    };
    let mut d = s.document_mut(i).unwrap();
    match rng.below(11) {
        0 | 1 => {
            let p = match pick(rng, |_| true) {
                Some((p, ..)) if rng.below(3) > 0 => p,
                Some((p, NodeKind::Mapping, _)) => p.with_key(KEYS[rng.below(KEYS.len())]),
                _ => Path::new().with_key(KEYS[rng.below(KEYS.len())]),
            };
            let v = value(rng);
            // The node the call replaces (the value at the path,
            // or the first node along it that creating below replaces).
            let prefixes: Vec<(Path, oneyaml::NodeId)> = (0..=p.segments().len())
                .rev()
                .filter_map(|k| {
                    let q = prefix(&p, k);
                    d.get_path(&q).map(|n| (q, n.id()))
                })
                .collect();
            let shape = owners::Shape::of(&d);
            let pairs: HashSet<oneyaml::NodeId> = prefixes
                .iter()
                .map(|(_, id)| *id)
                .filter(|id| shape.is_pair(*id))
                .collect();
            let r = d.set_path(&p, &v).map(|n| n.id());
            let op = match &r {
                Ok(new) => replaced(&d, &pairs, &prefixes, &p, *new),
                Err(_) => owners::Op::Plain,
            };
            (
                format!("set_path {p} = {v:?}"),
                r.map(|_| ()),
                Acct::Replaced,
                op,
            )
        }
        2 | 3 => {
            let Some((p, ..)) = pick(rng, |p| !p.0.segments().is_empty()) else {
                return ("noop".into(), Ok(()), Acct::Plain, owners::Op::Plain);
            };
            // `remove_keep_comments` too (its leading comments
            // stay, re-homed).
            let keep = rng.below(3) == 0;
            // The leading comments of a root collection's
            // last entry go to the document footer; and the footers of a
            // root collection its only entry leaves (it becomes `{}`/`[]`,
            // which owns none: rule 7, the documented fallback).
            let to_footer: Vec<String> = if p.segments().len() == 1 {
                to_document_footer(&d, &p, keep)
            } else {
                Vec::new()
            };
            // (The path may lead through an alias or a merge key: the
            // collection is the node's own parent.)
            let coll = d
                .get_path(&p)
                .and_then(|n| owners::Shape::of(&d).parent(n.id()));
            let r = if keep {
                d.get_path_mut(&p)
                    .map(|n| n.id())
                    .and_then(|id| d.remove_keep_comments(id))
            } else {
                d.remove_path(&p)
            };
            let mut acct = Acct::Plain;
            let mut op = owners::Op::Plain;
            let r = r.map(|rm| {
                acct = Acct::Taken(rm.comment_ids().to_vec(), to_footer);
                op = owners::Op::Remove {
                    coll: coll.unwrap(),
                    taken: rm.comment_ids().to_vec(),
                    keep_lead: keep,
                };
                pool.push(rm);
            });
            let name = if keep {
                "remove_keep_comments"
            } else {
                "remove_path"
            };
            (format!("{name} {p}"), r, acct, op)
        }
        4 | 5 => {
            let Some(rm) = pool.pop() else {
                return ("noop".into(), Ok(()), Acct::Plain, owners::Op::Plain);
            };
            let target = pick(rng, |p| {
                matches!(p.1, NodeKind::Mapping | NodeKind::Sequence)
            })
            .map_or(Path::new(), |p| p.0);
            let desc = format!("insert_removed {:?} into {target}", rm.text());
            let keep = rm.clone();
            let brought = rm.comments().to_vec();
            let ids = rm.comment_ids().to_vec();
            // (The node the edit goes into: an alias on the path resolves.)
            let coll = d.get_path(&target).map(|n| n.resolved().id());
            let at = rng.below(4);
            let r = match d.get_path_mut(&target) {
                Ok(mut n) => {
                    let r = if at == 0 {
                        n.insert_removed(rm)
                    } else {
                        n.insert_removed_at(at - 1, rm)
                    };
                    r.map(|_| ()).map_err(|e| {
                        let (e, back) = e.into_parts();
                        assert_eq!(back, keep, "the refused Removed comes back");
                        pool.push(back);
                        e
                    })
                }
                Err(e) => {
                    pool.push(keep);
                    Err(e)
                }
            };
            let op = match coll {
                Some(coll) if r.is_ok() => owners::Op::Reinsert {
                    coll,
                    index: at.checked_sub(1),
                    carried: if ids.is_empty() {
                        Vec::new()
                    } else {
                        t.carried
                            .get(&ids)
                            .cloned()
                            .unwrap_or_else(|| panic!("no record of the Removed's comments"))
                    },
                },
                _ => owners::Op::Plain,
            };
            (desc, r, Acct::Brought(brought), op)
        }
        6 | 7 => {
            // The block-scalar shape: move an entry holding a block scalar
            // (else any entry).
            let Some((p, _, len)) = pick(rng, is_coll) else {
                return ("noop".into(), Ok(()), Acct::Plain, owners::Op::Plain);
            };
            let from = rng.below(len);
            let to = if rng.below(2) == 0 {
                len - 1
            } else {
                rng.below(len)
            };
            t.b1 += usize::from(block);
            let coll = d.get_path(&p).map(|n| n.id());
            let r = d.get_path_mut(&p).and_then(|mut n| n.move_entry(from, to));
            let op = match coll {
                Some(coll) if from != to => owners::Op::Move { coll, from, to },
                _ => owners::Op::Plain,
            };
            (format!("move_entry {p} {from}->{to}"), r, Acct::Plain, op)
        }
        8 => {
            let Some((p, ..)) = pick(rng, |p| p.1 == NodeKind::Sequence) else {
                return ("noop".into(), Ok(()), Acct::Plain, owners::Op::Plain);
            };
            let v = value(rng);
            let r = d.get_path_mut(&p).and_then(|mut n| n.push(&v).map(|_| ()));
            (format!("push {p} {v:?}"), r, Acct::Plain, owners::Op::Plain)
        }
        9 => {
            let Some((p, ..)) = pick(rng, |p| p.1 != NodeKind::Alias) else {
                return ("noop".into(), Ok(()), Acct::Plain, owners::Op::Plain);
            };
            let name = ["n1", "x", "a0"][rng.below(3)];
            let name = (rng.below(3) > 0).then_some(name);
            let old = d.get_path_mut(&p).map(|n| n.id());
            let r = old.clone().and_then(|id| {
                d.node_mut(id)
                    .and_then(|mut n| n.set_anchor(name).map(|n| n.id()))
            });
            let op = match (old, &r) {
                (Ok(old), Ok(new)) if old != *new => owners::Op::Rewrite { old, new: *new },
                _ => owners::Op::Plain,
            };
            (
                format!("set_anchor {p} {name:?}"),
                r.map(|_| ()),
                Acct::Plain,
                op,
            )
        }
        _ => {
            let v = value(rng);
            let old = d.root().map(|n| n.id());
            let r = d.set_root(&v).map(|n| n.id());
            // (A root set to the value it has stays.)
            let op = match (old, &r) {
                (Some(old), Ok(new)) if old != *new => owners::Op::Replace {
                    old,
                    new: Some(*new),
                },
                _ => owners::Op::Plain,
            };
            (format!("set_root {v:?}"), r.map(|_| ()), Acct::Replaced, op)
        }
    }
}

/// Every document's lookup indexes (built here for every
/// collection, whatever its width, then maintained by the calls that
/// follow through the stream, accepted or rolled back) agree with the
/// linear scans.
fn lookups(s: &Stream, ctx: &str) {
    for i in 0..s.len() {
        if let Some(Err(e)) = s.document(i).map(|d| d.check_lookup_indexes()) {
            panic!("a lookup index went stale (document {i}): {e}\n{ctx}");
        }
    }
}

pub(crate) fn run(
    src: &str,
    settings: &Settings,
    recovering: bool,
    rng: &mut Rng,
    steps: usize,
    t: &mut Tally,
) {
    let mut s = if recovering {
        Stream::parse_recovering_with(src.to_owned(), settings).into_value()
    } else {
        match Stream::parse_with(src.to_owned(), settings) {
            Ok(s) => s,
            Err(_) => return,
        }
    };
    t.streams += 1;
    let mut pool = Vec::new();
    // A clone taken mid-script, with its text then: never changed by edits
    // of `s` (and it must still read as its own text).
    let mut frozen: Option<(Stream, String)> = None;
    let mut log = Vec::new();
    for _ in 0..steps {
        if s.is_empty() {
            break;
        }
        match rng.below(12) {
            0 => {
                t.clones += 1;
                let c = s.clone();
                if rng.below(2) == 0 {
                    // Go on editing the clone; the original is frozen.
                    let text = s.to_string();
                    frozen = Some((std::mem::replace(&mut s, c), text));
                } else {
                    let text = c.to_string();
                    frozen = Some((c, text));
                }
            }
            1 => {
                // A snapshot edited on its own never changes the stream.
                t.snapshots += 1;
                let i = rng.below(s.len());
                let before = s.to_string();
                if let Some(mut snap) = s.document(i) {
                    let _ = snap.set_path(Path::new().with_key("snap"), 1);
                }
                assert_eq!(s.to_string(), before, "a snapshot edit changed the stream");
            }
            _ => {}
        }
        lookups(
            &s,
            &format!("{src:?} before: {:?}\n{}", s.to_string(), log.join("\n")),
        );
        let i = rng.below(s.len());
        let before = s.to_string();
        let errors_before = error_nodes(&s);
        let comments_before = comment_count(&s, &pool);
        let snaps_before = snaps(&s);
        let (owners_before, shape_before) = s
            .document(i)
            .map(|d| (owners::snap(&d), owners::Shape::of(&d)))
            .unwrap_or_default();
        let (desc, r, acct, op) = call(&mut s, i, rng, &mut pool, t);
        // Only a replacement may drop comments (those
        // inside the replaced value).
        let replaces = desc.starts_with("set_path") || desc.starts_with("set_root");
        log.push(format!("doc {i}: {desc} -> {r:?}"));
        let ctx = || format!("{src:?} (recovering: {recovering})\n{}", log.join("\n"));
        if !replaces || r.is_err() {
            t.comment_checks += 1;
            assert_eq!(
                comment_count(&s, &pool),
                comments_before,
                "comments lost or made up: {}\nafter: {:?}",
                ctx(),
                s.to_string()
            );
        }
        match r {
            Ok(()) => {
                t.ok += 1;
                assert_eq!(
                    error_nodes(&s),
                    errors_before,
                    "an error node went: {}",
                    ctx()
                );
                check_reparse(&s, recovering, &ctx());
                // (`set_root` of an empty document adds none: the count
                // stays.)
                if s.len() == snaps_before.len() {
                    account(&snaps_before, &s, i, &acct, &ctx(), t);
                    // Exact owners (see `owners/mod.rs`).
                    if let Some(d) = s.document(i) {
                        let rep = owners::check(&owners_before, &shape_before, &d, &op, &ctx());
                        t.owner_checks += 1;
                        if let owners::Op::Remove { taken, .. } = &op {
                            if !taken.is_empty() {
                                t.carried.insert(taken.clone(), rep.carried);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                assert!(
                    !matches!(e, EditError::Internal),
                    "Internal refusal: {}",
                    ctx()
                );
                assert_eq!(s.to_string(), before, "refused but changed: {}", ctx());
                // Every `Unsupported` is a documented one (the
                // list `edit_fuzz.rs` checks too).
                if let EditError::Unsupported { reason, .. } = &e {
                    assert!(
                        DOCUMENTED_UNSUPPORTED.contains(reason),
                        "undocumented Unsupported refusal {reason:?}: {}",
                        ctx()
                    );
                }
                *t.refused.entry(kind(&e)).or_default() += 1;
            }
        }
        if let Some((c, text)) = &frozen {
            assert_eq!(&c.to_string(), text, "a clone changed: {}", ctx());
        }
    }
    lookups(
        &s,
        &format!(
            "{src:?} at the end: {:?}\n{}",
            s.to_string(),
            log.join("\n")
        ),
    );
    if let Some((c, _)) = &frozen {
        check_reparse(c, recovering, "the frozen clone");
    }
}

#[test]
fn stream_edit_scripts() {
    let rounds = env("ONEYAML_STREAM_FUZZ_ROUNDS", 400);
    let steps = env("ONEYAML_STREAM_FUZZ_STEPS", 12);
    let seed = env("ONEYAML_STREAM_FUZZ_SEED", 0x5712) as u64;
    let (part, parts) = section();
    let mut t = Tally::default();
    for r in 0..rounds {
        if r % parts != part {
            continue;
        }
        let mut rng = Rng(seed ^ ((r as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)));
        let src = stream_text(&mut rng);
        let src = match r % 3 {
            1 => src.replace('\n', "\r\n"),
            2 => src.replace('\n', "\r"),
            _ => src,
        };
        let settings = Settings::new().lenient_inline_colons(r % 2 == 1);
        run(&src, &settings, false, &mut rng, steps, &mut t);
        run(&src, &settings, true, &mut rng, steps, &mut t);
    }
    eprintln!(
        "stream fuzz: {} streams, {} accepted, {} clones, {} snapshots, {} B1-shaped moves, {} comment checks, {} identity checks ({} taken, {} rewritten, {} brought back); refused {:?}",
        t.streams,
        t.ok,
        t.clones,
        t.snapshots,
        t.b1,
        t.comment_checks,
        t.identity_checks,
        t.taken_comments,
        t.rewritten_comments,
        t.brought_comments,
        t.refused
    );
    if rounds >= 100 {
        assert!(t.ok > rounds, "too few accepted edits: {}", t.ok);
        assert!(t.b1 > 0 && t.clones > 0 && t.snapshots > 0);
        assert!(
            t.comment_checks > rounds,
            "{} comment checks",
            t.comment_checks
        );
        assert!(
            t.identity_checks > rounds,
            "{} identity checks",
            t.identity_checks
        );
        assert!(t.taken_comments > 0 && t.brought_comments > 0);
    }
}
