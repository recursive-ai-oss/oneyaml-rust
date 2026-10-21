//! The `Document` edit-script fuzz: seeded
//! random sequences of edit API calls — every operation, awkward keys and
//! values, stale handles, restricted documents, anchors and aliases —
//! over the yaml-test-suite inputs (valid and invalid) and generated
//! documents. After every call:
//!
//! * nothing panics;
//! * a refused call changed nothing (the text is unchanged);
//! * an accepted call keeps both readers in agreement: `Value::try_from(&doc)`
//!   equals `from_str::<Value>(&doc.to_string())` (value or first error code),
//!   and parsing the text gives the document's own diagnostics;
//! * handles follow a model: handles of nodes outside the edited
//!   collection still resolve to the same text, and handles the model
//!   knows were removed or replaced are [`EditError::StaleNode`];
//! * **meaning**: the value at the edited path is
//!   what was set (or the alias target's value), and — for documents
//!   without anchors, aliases, or merge keys, read from the tree (it is
//!   never a text search, so `'a*b'` keeps the check on) — the whole
//!   `Value` equals the model's (the value before, with exactly that
//!   change; a removal removes exactly its entry; a move reorders;
//!   `set_anchor` changes nothing). An accepted edit never turns a
//!   meaning into an error, except where a merge key reads an
//!   anchored node through an alias and the edit replaces that node by
//!   one that cannot be merged (`Y1016`), where an edit inside a complex
//!   key (directly or through an alias in it) makes two keys equal
//!   (`Y1013`), and where content taken while the meaning had an error is
//!   moved back in (counted and reported);
//! * **comments**: for every call
//!   that replaces no existing value, the comments of the document plus
//!   those held by live [`Removed`] values are conserved;
//! * **comment identity against the rulebook** (comment edit semantics,
//!   `docs/comment-attachment.md`), read before the call with explicit owners
//!   set aside: a removal's [`Removed::comment_ids`] are exactly the comments
//!   the rulebook gives the removed entry (for `remove_keep_comments`, less its
//!   leading ones, which must then be the next entry's leading comments or the
//!   collection's footer, the document footer for a root collection: the call
//!   is the explicit request); every other comment keeps its handle and owner,
//!   or is rewritten where the parser puts it (same text and owner, a new
//!   handle); a moved entry's comments come back owned inside it, a re-inserted
//!   one's inside the new entry; a replaced value's comments inside its text go
//!   with it, its own leading and trailing ones outside stay, owned inside the
//!   new value; nothing is made up; the edited text, parsed again, explains
//!   every comment as the document does; a move carries every explicit owner
//!   (random `attach_comment` calls are interleaved).
//!
//! The corpus adds CR and CRLF versions of the generated documents and
//! documents that redefine anchor names.
//!
//! Sectionable to fit a time budget: `ONEYAML_EDIT_FUZZ_SECTION=i/n` runs
//! the i-th of n slices of the corpus, `ONEYAML_EDIT_FUZZ_STEPS` sets the
//! calls per document (default 10), `ONEYAML_EDIT_FUZZ_ROUNDS` the number
//! of generated documents (default 150), `ONEYAML_EDIT_FUZZ_SEED` the seed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod documented;
pub(crate) mod generator;
mod owners;

use documented::DOCUMENTED_UNSUPPORTED;
use generator::Rng;
use oneyaml::{
    CommentId, CommentOwner, Document, EditError, NodeId, NodeKind, NodeRef, Path, Removed,
    ScalarStyle, Settings, Span, Value,
};

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn section() -> (usize, usize) {
    std::env::var("ONEYAML_EDIT_FUZZ_SECTION")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once('/')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .filter(|(i, n)| *n > 0 && i < n)
        .unwrap_or((0, 1))
}

const KEYS: &[&str] = &[
    "a", "b", "k", "new key", "<<", "x: y", "#", "", "1", "true", "- z", "é", "a,b", "[q]",
];

const STRINGS: &[&str] = &[
    "",
    " a",
    "a ",
    "a: b",
    "#",
    " #x",
    "- x",
    "[",
    "]",
    "{",
    "'",
    "\"",
    "\n",
    "a\nb",
    "a\n",
    // A keep (`|+`) block scalar.
    "a\n\n",
    " a\nb",
    "yes",
    "~",
    "null",
    "0x1F",
    "1_000",
    "é",
    "\t",
    "---",
    "...",
    "*a",
    "&a",
    "!t",
    "%x",
    "@",
    "a,b",
    "12:30",
    "2001-12-14",
    "<<",
    "=",
    ".inf",
    "x",
    "plain text",
];

fn value(rng: &mut Rng, depth: usize) -> Value {
    match rng.below(if depth > 1 { 6 } else { 9 }) {
        0 => Value::Null,
        1 => Value::Bool(rng.below(2) == 0),
        2 => Value::from(rng.below(1000) as i64 - 500),
        3 => Value::from((rng.below(1000) as f64) / 8.0),
        4 | 5 => Value::from(STRINGS[rng.below(STRINGS.len())]),
        6 => {
            let n = rng.below(3);
            Value::from((0..n).map(|_| value(rng, depth + 1)).collect::<Vec<_>>())
        }
        7 => {
            let mut m = oneyaml::Mapping::new();
            for _ in 0..rng.below(3) {
                m.insert(
                    Value::from(KEYS[rng.below(KEYS.len())]),
                    value(rng, depth + 1),
                );
            }
            Value::Mapping(m)
        }
        _ => Value::Tagged(Box::new(oneyaml::TaggedValue {
            tag: oneyaml::Tag::new("!t"),
            value: Value::from(STRINGS[rng.below(STRINGS.len())]),
        })),
    }
}

/// A step of a path into a [`Value`].
#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Key(Value),
    Idx(usize),
}

/// A path into a [`Value`] (`None` for keys and nodes under merge keys).
type VPath = Option<Vec<Seg>>;

fn vget<'v>(mut v: &'v Value, path: &[Seg]) -> Option<&'v Value> {
    for s in path {
        v = match s {
            Seg::Key(k) => v.untag_ref().as_mapping()?.get(k)?,
            Seg::Idx(i) => v.untag_ref().as_sequence()?.get(*i)?,
        };
    }
    Some(v)
}

fn vget_mut<'v>(mut v: &'v mut Value, path: &[Seg]) -> Option<&'v mut Value> {
    for s in path {
        v = match s {
            Seg::Key(k) => v.untag_mut().as_mapping_mut()?.get_mut(k)?,
            Seg::Idx(i) => v.untag_mut().as_sequence_mut()?.get_mut(*i)?,
        };
    }
    Some(v)
}

fn of_path(p: &Path) -> Vec<Seg> {
    p.segments()
        .iter()
        .filter_map(|s| match s {
            oneyaml::PathSegment::Key(k) => Some(Seg::Key(Value::from(k.as_str()))),
            oneyaml::PathSegment::Index(i) => Some(Seg::Idx(*i)),
            _ => None,
        })
        .collect()
}

/// A node of the document: handle, kind, text, span, value path, and
/// whether it lies inside a mapping key (itself a key, or in one).
type NodeInfo = (NodeId, NodeKind, String, Span, VPath, bool);

/// Every YAML node of the document (keys included), with handles, texts,
/// spans and value paths; an explicit stack, no recursion.
fn nodes(doc: &Document) -> Vec<NodeInfo> {
    let mut out = Vec::new();
    let mut stack: Vec<(NodeRef<'_>, VPath, bool)> = doc
        .root()
        .into_iter()
        .map(|r| (r, Some(Vec::new()), false))
        .collect();
    while let Some((n, path, in_key)) = stack.pop() {
        out.push((
            n.id(),
            n.kind(),
            n.text().into_owned(),
            n.span(),
            path.clone(),
            in_key,
        ));
        match n.kind() {
            NodeKind::Mapping => {
                if let Some(m) = n.as_mapping() {
                    for (k, v) in m.iter() {
                        let key = k
                            .as_scalar()
                            .filter(|_| k.kind() == NodeKind::Scalar)
                            .and_then(|s| s.resolved().ok())
                            .filter(|kv| kv.as_str() != Some("<<"));
                        let vp = match (&path, key) {
                            (Some(p), Some(kv)) => {
                                let mut p = p.clone();
                                p.push(Seg::Key(kv));
                                Some(p)
                            }
                            _ => None,
                        };
                        stack.push((k, None, true));
                        stack.push((v, vp, in_key));
                    }
                }
            }
            NodeKind::Sequence => {
                if let Some(s) = n.as_sequence() {
                    for (i, item) in s.iter().enumerate() {
                        let ip = path.as_ref().map(|p| {
                            let mut p = p.clone();
                            p.push(Seg::Idx(i));
                            p
                        });
                        stack.push((item, ip, in_key));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether one change can show in several places of the meaning, read
/// from the tree: no anchor, no alias, no merge key (a plain
/// `<<` key, or one tagged `!!merge`). An explicit stack, no recursion.
fn plain(doc: &Document) -> bool {
    let mut stack: Vec<NodeRef<'_>> = doc.root().into_iter().collect();
    while let Some(n) = stack.pop() {
        if n.anchor().is_some() || n.kind() == NodeKind::Alias {
            return false;
        }
        if let Some(m) = n.as_mapping().filter(|_| n.kind() == NodeKind::Mapping) {
            for (k, v) in m.iter() {
                let merge = k.as_scalar().is_some_and(|s| {
                    (s.style() == ScalarStyle::Plain && s.value() == "<<")
                        || k.tag().unwrap().is_some_and(|t| t.ends_with(":merge"))
                });
                if merge {
                    return false;
                }
                stack.push(k);
                stack.push(v);
            }
        } else if let Some(q) = n.as_sequence().filter(|_| n.kind() == NodeKind::Sequence) {
            stack.extend(q.iter());
        }
    }
    true
}

/// How the whole value changes (`None`: the model cannot say).
type Change = Box<dyn FnOnce(&mut Value) -> Option<()>>;

/// What a call should do to the meaning (see the module docs).
#[derive(Default)]
struct Model {
    /// The value at this path afterwards (a value, or the value another
    /// path has afterwards).
    at: Option<(Vec<Seg>, Want)>,
    /// The whole value afterwards: the value before, changed so.
    whole: Option<Change>,
    /// The call replaces no existing value: comments are conserved.
    keeps_comments: bool,
    /// The call moves content back in that was taken while the meaning
    /// had an error: it may carry that error (e.g. an invalid merge value
    /// or duplicate keys inside it).
    moves_tainted: bool,
    /// The call edits inside a mapping key, or the document has an alias
    /// inside a key (an edit of the anchored node changes that key): it
    /// may make two keys equal.
    in_key: bool,
    /// What the comment identity accounting needs.
    acct: Acct,
}

/// The comment edit rules (`docs/comment-attachment.md`): what a
/// call does to comments, for the identity accounting (see the module
/// docs).
#[derive(Default)]
struct Acct {
    /// A removal from this collection (the removed entry is the one whose
    /// value's handle went stale, or the only one when the collection was
    /// rewritten).
    removal: Option<NodeId>,
    /// The `Removed` the call returned: its comment handles.
    taken: Option<Vec<CommentId>>,
    /// `remove_keep_comments`: the removed entry's leading comments stay.
    keep_lead: bool,
    /// A replacement: the replaced node, and its new handle.
    replaced: Option<(NodeId, Option<NodeId>)>,
    /// A rewrite of a node (`set_anchor`): old and new handle.
    rewritten: Option<(NodeId, NodeId)>,
    /// A move: (collection, from, to).
    moved: Option<(NodeId, usize, usize)>,
    /// A re-insertion into this collection (appended): the comment
    /// handles of the `Removed` it brings back.
    brought: Option<(NodeId, Vec<CommentId>)>,
}

impl Acct {
    /// The call, for the exact owner accounting.
    fn op(
        &self,
        carried: &std::collections::HashMap<Vec<CommentId>, Vec<owners::Carried>>,
    ) -> owners::Op {
        if let Some(coll) = self.removal {
            return owners::Op::Remove {
                coll,
                taken: self.taken.clone().unwrap_or_default(),
                keep_lead: self.keep_lead,
            };
        }
        if let Some((old, new)) = self.replaced {
            return owners::Op::Replace { old, new };
        }
        if let Some((old, new)) = self.rewritten {
            return owners::Op::Rewrite { old, new };
        }
        if let Some((coll, from, to)) = self.moved {
            return owners::Op::Move { coll, from, to };
        }
        if let Some((coll, ids)) = &self.brought {
            let carried = if ids.is_empty() {
                Vec::new()
            } else {
                carried
                    .get(ids)
                    .cloned()
                    .unwrap_or_else(|| panic!("no record of the Removed's comments {ids:?}"))
            };
            return owners::Op::Reinsert {
                coll: *coll,
                index: None,
                carried,
            };
        }
        owners::Op::Plain
    }
}

/// An entry of a collection: key, value, span from the key (or item)
/// through the value, the value's span.
type EntryInfo = (Option<NodeId>, NodeId, Span, Span);

/// The entries of collection `c`.
fn entries_of(doc: &Document, c: NodeId) -> Vec<EntryInfo> {
    let Ok(n) = doc.node(c) else {
        return Vec::new();
    };
    if let Some(m) = n.as_mapping().filter(|m| m.node() == n) {
        return m
            .iter()
            .map(|(k, v)| {
                let e = Span::new(k.span().start, v.span().end);
                (Some(k.id()), v.id(), e, v.span())
            })
            .collect();
    }
    if let Some(q) = n.as_sequence().filter(|q| q.node() == n) {
        return q
            .iter()
            .map(|v| (None, v.id(), v.span(), v.span()))
            .collect();
    }
    Vec::new()
}

/// Every comment by handle, with its **rulebook** owner (explicit owners
/// set aside: the rules decide what a node edit takes), text, and the
/// owner's span.
type Snap = Vec<(CommentId, CommentOwner, String, Option<Span>, u8)>;

/// A comment as the rules read it: text, rule, owner span.
type Shape = (usize, String, u8, Option<(usize, usize)>);

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

/// Exact comment-owner accounting (see `owners/mod.rs`), then
/// The edited text, parsed again, explains every comment as the
/// document does (explicit owners set aside, which the text cannot carry).
fn account(
    before: &owners::Snap,
    bs: &owners::Shape,
    doc: &Document,
    op: &owners::Op,
    ctx: &str,
    t: &mut Tally,
) -> Vec<owners::Carried> {
    let report = owners::check(before, bs, doc, op, ctx);
    if let owners::Op::Remove { taken, .. } = op {
        t.taken_checks += 1;
        t.taken_comments += taken.len();
    }
    t.moved_comments += report.moved;
    let text = doc.to_string();
    let again = Document::parse_recovering_with(text.clone(), doc.settings()).into_value();
    let shape = |s: &Snap| -> Vec<Shape> {
        s.iter()
            .map(|(_, _, x, sp, r)| (0, x.clone(), *r, sp.map(|s| (s.start, s.end))))
            .collect()
    };
    assert_eq!(
        shape(&rule_snap(&again)),
        shape(&rule_snap(doc)),
        "the re-parse owns the comments otherwise: {ctx}\nafter: {text:?}"
    );
    t.identity_checks += 1;
    report.carried
}

enum Want {
    Value(Value),
    Path(Vec<Seg>),
    /// Anything (the node exists).
    Exists,
}

fn same() -> Model {
    Model {
        whole: Some(Box::new(|_| Some(()))),
        keeps_comments: true,
        ..Model::default()
    }
}

/// Comments of the document's text (tokens) plus those held by `removed`.
fn comment_count(doc: &Document, removed: &[Removed]) -> usize {
    let own = doc
        .syntax()
        .root()
        .descendants()
        .filter(|n| n.kind() == oneyaml_syntax::tree::SyntaxKind::Comment)
        .count();
    own + removed.iter().map(|r| r.comments().len()).sum::<usize>()
}

fn pick<T: Clone>(rng: &mut Rng, v: &[T]) -> Option<T> {
    if v.is_empty() {
        None
    } else {
        Some(v[rng.below(v.len())].clone())
    }
}

/// Every collection's lookup index (built here whatever
/// its width, then maintained by the calls that follow, accepted or
/// rolled back) agrees with the linear scans.
fn lookups(doc: &Document, ctx: &str) {
    if let Err(e) = doc.check_lookup_indexes() {
        panic!("a lookup index went stale: {e}\n{ctx}");
    }
}

/// The edited syntax tree equals a fresh
/// parse of its text under its own settings, whole (structure, token
/// texts, diagnostics with every label and fix), recovered input
/// included.
fn whole_tree(tree: &oneyaml_syntax::tree::SyntaxTree, ctx: &str) {
    let again = oneyaml_syntax::tree::SyntaxTree::parse_with(tree.emit(), tree.settings());
    if let Err(e) = oneyaml_syntax::tree::compare_trees(tree, &again) {
        panic!(
            "the edited tree differs from its re-parse ({e}): {ctx}\ntext: {:?}",
            tree.emit()
        );
    }
}

/// Both readers agree after an accepted edit (see the module docs).
fn agrees_with_its_text(doc: &Document, ctx: &str) {
    whole_tree(doc.syntax(), ctx);
    let text = doc.to_string();
    let s = doc.settings();
    let a = Value::try_from(doc).map_err(|e| e.code());
    let b = oneyaml::from_str_with::<Value>(&text, s).map_err(|e| e.code());
    assert_eq!(a, b, "same meaning: {ctx}\ntext: {text:?}");
    if doc.has_errors() {
        let re = Document::parse_recovering_with(text.clone(), s);
        assert_eq!(
            re.value().diagnostics(),
            doc.diagnostics(),
            "same diagnostics: {ctx}\ntext: {text:?}"
        );
    } else {
        let re = Document::parse_with(text.clone(), s)
            .unwrap_or_else(|e| panic!("edited text does not parse ({e}): {ctx}\ntext: {text:?}"));
        assert_eq!(
            re.diagnostics(),
            doc.diagnostics(),
            "same diagnostics: {ctx}\ntext: {text:?}"
        );
    }
}

#[derive(Default)]
pub(crate) struct Tally {
    /// Failures reported in `ONEYAML_EDIT_FUZZ_COLLECT` mode.
    collected: usize,
    /// Refusals by kind, and an example of each.
    kinds: std::collections::BTreeMap<String, (usize, String)>,
    noop: usize,
    docs: usize,
    ok: usize,
    refused: usize,
    stale_checked: usize,
    comment_checks: usize,
    at_checks: usize,
    whole_checks: usize,
    /// Accepted edits that turned a meaning into an error where anchors or
    /// merge keys allow it (reported).
    invalidated: usize,
    /// Identity accounting checks, `Removed` exactness checks,
    /// comments rewritten or carried and found again, explicit owners
    /// carried by moves.
    identity_checks: usize,
    taken_checks: usize,
    taken_comments: usize,
    moved_comments: usize,
    explicit_moves: usize,
}

/// Whether `tag`, on a scalar, still gives exactly `v` when read back: a
/// custom tag on a string, a core tag on the type it names.
fn tag_describes(tag: &oneyaml::Tag, v: &Value) -> bool {
    const CORE: &str = "tag:yaml.org,2002:";
    let core = |name: &str| *tag == format!("{CORE}{name}");
    match v {
        Value::String(_) => core("str") || !tag.starts_with(CORE),
        Value::Bool(_) => core("bool"),
        Value::Null => core("null"),
        Value::Number(n) => core(if n.is_integer() { "int" } else { "float" }),
        _ => false,
    }
}

/// The meaning oracle (see the module docs).
fn check_meaning(
    doc: &Document,
    before: Option<Value>,
    plain: bool,
    model: Model,
    ctx: &str,
    t: &mut Tally,
) {
    let text = doc.to_string();
    let mut after = match Value::try_from(doc) {
        Ok(after) => after,
        Err(e) => {
            // An accepted edit that turns a meaning into an error never passes
            // silently. The tolerated cases (reported, design questions): the
            // node a merge key reads through an alias is replaced by one that
            // cannot be merged (editing an anchored node edits what its aliases
            // denote); an edit inside a complex key (directly, or through an
            // alias in it) makes it equal to another key; content taken from an
            // erroring state is moved back in with its error.
            if before.is_some() {
                let merge = !plain && e.code() == oneyaml::code::INVALID_MERGE_VALUE;
                let key = model.in_key && e.code() == oneyaml::code::DUPLICATE_KEY;
                // An oracle fix: a node that aliases
                // repeat made larger can take the `Value` read past its
                // alias budget (nodes or bytes); budgets apply to `Value`
                // and serde only, never to `Document` edits.
                let budget = matches!(
                    e.code(),
                    oneyaml::code::ALIAS_BUDGET_EXCEEDED
                        | oneyaml::code::ALIAS_BYTE_BUDGET_EXCEEDED
                );
                assert!(
                    merge || key || budget || model.moves_tainted,
                    "accepted edit made the meaning an error ({}): {ctx}\nafter: {text:?}",
                    e.code()
                );
                t.invalidated += 1;
            }
            return;
        }
    };
    // A scalar set to a value keeps its tag when the tag still describes the
    // value (a custom tag on a string, `!!str` on a string, ...): the node
    // then reads as that value under its tag, which is the value set.
    if let Some((path, Want::Value(v))) = &model.at {
        let kept = match vget(&after, path) {
            Some(Value::Tagged(t)) => t.value == *v && tag_describes(&t.tag, v),
            _ => false,
        };
        if kept {
            if let Some(slot) = vget_mut(&mut after, path) {
                *slot = v.clone();
            }
        }
    }
    if let Some((path, want)) = model.at {
        let got = vget(&after, &path);
        match want {
            Want::Value(v) => {
                assert_eq!(got, Some(&v), "value at {path:?}: {ctx}\nafter: {text:?}")
            }
            Want::Path(p) => {
                assert_eq!(
                    got,
                    vget(&after, &p),
                    "alias at {path:?}: {ctx}\nafter: {text:?}"
                );
            }
            Want::Exists => assert!(got.is_some(), "nothing at {path:?}: {ctx}\nafter: {text:?}"),
        }
        t.at_checks += 1;
    }
    // The whole value, where no alias, anchor or merge key can make one
    // change show in several places (`plain`, read from the tree before).
    if let (Some(mut expected), Some(change), true) = (before, model.whole, plain) {
        if change(&mut expected).is_some() {
            assert_eq!(after, expected, "meaning: {ctx}\nafter: {text:?}");
            t.whole_checks += 1;
        }
    }
}

/// One random call. Returns (description, the collection or node it
/// changes (its span, for the handle model), the result, handles it made
/// stale, the meaning model).
#[allow(clippy::too_many_lines)]
fn step(
    doc: &mut Document,
    rng: &mut Rng,
    removed: &mut Vec<Removed>,
    stale: &[NodeId],
    tainted: &std::collections::HashSet<String>,
) -> (
    String,
    Option<Span>,
    Result<(), EditError>,
    Vec<NodeId>,
    Model,
) {
    let all = nodes(doc);
    let maps: Vec<_> = all
        .iter()
        .filter(|n| n.1 == NodeKind::Mapping)
        .cloned()
        .collect();
    let seqs: Vec<_> = all
        .iter()
        .filter(|n| n.1 == NodeKind::Sequence)
        .cloned()
        .collect();
    let colls: Vec<_> = maps.iter().chain(&seqs).cloned().collect();
    let op = rng.below(17);
    let v = value(rng, 0);
    let key = KEYS[rng.below(KEYS.len())];
    let parent_span = |id: NodeId, doc: &Document| -> Option<Span> {
        let n = doc.node(id).ok()?;
        let sp = n.span();
        // The collection holding it (or the node itself at the root).
        all.iter()
            .filter(|c| matches!(c.1, NodeKind::Mapping | NodeKind::Sequence))
            .filter(|c| c.3.start <= sp.start && sp.end <= c.3.end && c.0 != id)
            .min_by_key(|c| c.3.len())
            .map(|c| c.3)
            .or(Some(sp))
    };
    match op {
        0 => {
            let Some(n) = pick(rng, &all) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let span = parent_span(n.0, doc);
            let r = doc
                .node_mut(n.0)
                .and_then(|mut m| m.set(&v).map(|m| m.id()));
            let mut model = at_and_whole(n.4.clone(), v.clone(), false);
            // A node set to the value it has stays (no
            // replacement, its handle still resolves).
            let kept = r.as_ref().is_ok_and(|id| *id == n.0);
            model.acct.replaced = (!kept).then(|| (n.0, r.as_ref().ok().copied()));
            let gone = if kept { vec![] } else { vec![n.0] };
            let r = r.map(|_| ());
            (format!("set {:?} = {v:?}", n.2), span, r, gone, model)
        }
        1 => {
            let sc: Vec<_> = all
                .iter()
                .filter(|n| n.1 == NodeKind::Scalar)
                .cloned()
                .collect();
            let Some(n) = pick(rng, &sc) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let s = STRINGS[rng.below(STRINGS.len())];
            let span = parent_span(n.0, doc);
            let r = doc
                .node_mut(n.0)
                .and_then(|mut m| m.set_scalar(s).map(|m| m.id()));
            let mut model = at_and_whole(n.4.clone(), Value::from(s), false);
            // As for `set`.
            let kept = r.as_ref().is_ok_and(|id| *id == n.0);
            model.acct.replaced = (!kept).then(|| (n.0, r.as_ref().ok().copied()));
            let gone = if kept { vec![] } else { vec![n.0] };
            let r = r.map(|_| ());
            (
                format!("set_scalar {:?} = {s:?}", n.2),
                span,
                r,
                gone,
                model,
            )
        }
        2 => {
            let Some(m) = pick(rng, &maps) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let entries = entries_of(doc, m.0);
            let r = doc
                .node_mut(m.0)
                .and_then(|mut x| x.insert(key, &v).map(|n| n.id()));
            let mut model = match (&m.4, key) {
                (_, "<<") | (None, _) => Model::default(),
                (Some(p), _) => {
                    let mut p = p.clone();
                    p.push(Seg::Key(Value::from(key)));
                    at_and_whole(Some(p), v.clone(), false)
                }
            };
            // (A rewritten single-pair mapping: every old node is stale.)
            model.acct.rewritten = r.as_ref().ok().and_then(|_| rewritten_map(doc, &m));
            if model.acct.rewritten.is_none() {
                model.acct.replaced = replaced_value(doc, &entries, r.as_ref().ok().copied());
            }
            let r = r.map(|_| ());
            (
                format!("insert {key:?}: {v:?} into {:?}", m.2),
                Some(m.3),
                r,
                vec![],
                model,
            )
        }
        3 => {
            let Some(m) = pick(rng, &maps) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let mut out = None;
            let r = doc.node_mut(m.0).and_then(|mut x| x.remove(key)).map(|rm| {
                out = Some(rm);
            });
            let mut model = removal(m.4.clone(), Seg::Key(Value::from(key)));
            if let Some(rm) = out {
                model.acct.removal = Some(m.0);
                model.acct.taken = Some(taken_of(&rm));
                removed.push(rm);
            }
            (
                format!("remove {key:?} from {:?}", m.2),
                Some(m.3),
                r,
                vec![],
                model,
            )
        }
        4 => {
            let Some(s) = pick(rng, &seqs) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let i = rng.below(4);
            let which = rng.below(4);
            let mut out = None;
            let entries = entries_of(doc, s.0);
            let mut new = None;
            let r = doc.node_mut(s.0).and_then(|mut x| match which {
                0 => x.push(&v).map(|_| ()),
                1 => x.insert_at(i, &v).map(|_| ()),
                2 => x.remove_at(i).map(|rm| out = Some(rm)),
                _ => x.replace_at(i, &v).map(|n| new = Some(n.id())),
            });
            let mut acct = Acct::default();
            if let Some(rm) = out {
                acct.removal = Some(s.0);
                acct.taken = Some(taken_of(&rm));
                removed.push(rm);
            }
            // (An item set to the value it has stays.)
            if new.is_some() {
                acct.replaced = entries
                    .get(i)
                    .filter(|e| Some(e.1) != new)
                    .map(|e| (e.1, new));
            }
            let mut model = match (&s.4, which) {
                (None, _) => Model::default(),
                (Some(p), 0) => {
                    let (p, v) = (p.clone(), v.clone());
                    Model {
                        whole: Some(Box::new(move |x| {
                            vget_mut(x, &p)?.untag_mut().as_sequence_mut()?.push(v);
                            Some(())
                        })),
                        keeps_comments: true,
                        ..Model::default()
                    }
                }
                (Some(p), 1) => {
                    let (p, v) = (p.clone(), v.clone());
                    Model {
                        whole: Some(Box::new(move |x| {
                            let seq = vget_mut(x, &p)?.untag_mut().as_sequence_mut()?;
                            seq.insert_at(i, v).ok()
                        })),
                        keeps_comments: true,
                        ..Model::default()
                    }
                }
                (Some(p), 2) => removal(Some(p.clone()), Seg::Idx(i)),
                (Some(p), _) => {
                    let mut p = p.clone();
                    p.push(Seg::Idx(i));
                    at_and_whole(Some(p), v.clone(), false)
                }
            };
            model.acct = acct;
            (
                format!("seq op {which} at {i} ({v:?}) on {:?}", s.2),
                Some(s.3),
                r,
                vec![],
                model,
            )
        }
        5 => {
            let Some(c) = pick(rng, &colls) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let (a, b) = (rng.below(4), rng.below(4));
            let moved = entries_of(doc, c.0).get(a).map(|_| (c.0, a, b));
            let r = doc.node_mut(c.0).and_then(|mut x| x.move_entry(a, b));
            let mut model = match (&c.4, c.1) {
                (Some(p), NodeKind::Sequence) => {
                    let p = p.clone();
                    Model {
                        whole: Some(Box::new(move |x| {
                            let seq = vget_mut(x, &p)?.untag_mut().as_sequence_mut()?;
                            let m = seq.remove_at(a)?;
                            seq.insert_at(b, m).ok()
                        })),
                        keeps_comments: true,
                        ..Model::default()
                    }
                }
                // Mapping equality ignores order.
                (Some(_), _) => same(),
                (None, _) => Model {
                    keeps_comments: true,
                    ..Model::default()
                },
            };
            if a != b {
                model.acct.moved = moved;
            }
            (
                format!("move {a} -> {b} in {:?}", c.2),
                Some(c.3),
                r,
                vec![],
                model,
            )
        }
        6 => {
            let Some(n) = pick(rng, &all) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let span = parent_span(n.0, doc);
            let holder = holder_of(doc, &all, n.0);
            let mut out = None;
            let r = doc.remove_node(n.0).map(|rm| out = Some(rm));
            let mut model = match &n.4 {
                Some(p) if !p.is_empty() => {
                    let mut parent = p.clone();
                    let last = parent.pop().unwrap();
                    removal(Some(parent), last)
                }
                _ => Model {
                    keeps_comments: true,
                    ..Model::default()
                },
            };
            if let (Some(rm), Some(h)) = (out, holder) {
                model.acct.removal = Some(h.0);
                model.acct.taken = Some(taken_of(&rm));
                removed.push(rm);
            }
            (format!("remove_node {:?}", n.2), span, r, vec![n.0], model)
        }
        7 => {
            let Some(c) = pick(rng, &colls) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let Some(rm) = removed.pop() else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let desc = format!("insert_removed {:?} into {:?}", rm, c.2);
            let moves_tainted = tainted.contains(rm.text());
            let brought = rm.comment_ids().to_vec();
            let keep = rm.clone();
            // A refusal hands the `Removed` back.
            let r = match doc.node_mut(c.0) {
                Ok(mut x) => match x.insert_removed(rm) {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        let (e, back) = e.into_parts();
                        assert_eq!(back, keep, "the refused Removed comes back as it was");
                        removed.push(back);
                        Err(e)
                    }
                },
                Err(e) => {
                    removed.push(keep);
                    Err(e)
                }
            };
            let model = Model {
                keeps_comments: true,
                moves_tainted,
                acct: Acct {
                    brought: Some((c.0, brought)),
                    ..Acct::default()
                },
                ..Model::default()
            };
            (desc, Some(c.3), r, vec![], model)
        }
        8 => {
            let Some(n) = pick(rng, &all) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let name = ["a", "b", "zz", "x1"][rng.below(4)];
            let name = (rng.below(3) != 0).then_some(name);
            let span = parent_span(n.0, doc);
            let r = doc.set_anchor(n.0, name);
            let mut model = same();
            if let Ok(new) = r {
                model.acct.rewritten = Some((n.0, new));
            }
            (
                format!("set_anchor {name:?} on {:?}", n.2),
                span.map(|_| Span::new(0, usize::MAX)),
                r.map(|_| ()),
                vec![],
                model,
            )
        }
        9 => {
            let anchored: Vec<_> = all
                .iter()
                .filter(|n| doc.node(n.0).is_ok_and(|x| x.anchor().is_some()))
                .cloned()
                .collect();
            let Some(t) = pick(rng, &anchored) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let Some(c) = pick(rng, &colls) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let r = doc.node_mut(c.0).and_then(|mut x| {
                if c.1 == NodeKind::Mapping {
                    x.insert_alias(key, t.0).map(|_| ())
                } else {
                    x.push_alias(t.0).map(|_| ())
                }
            });
            let mut model = match (&c.4, &t.4, c.1) {
                (Some(p), Some(tp), NodeKind::Mapping) if key != "<<" => {
                    let mut p = p.clone();
                    p.push(Seg::Key(Value::from(key)));
                    Model {
                        at: Some((p, Want::Path(tp.clone()))),
                        keeps_comments: true,
                        ..Model::default()
                    }
                }
                _ => Model {
                    keeps_comments: true,
                    ..Model::default()
                },
            };
            // An oracle fix: an alias inserted into a
            // single-pair flow mapping rewrites it as `insert` does
            // (`rewritten_map`).
            if r.is_ok() && c.1 == NodeKind::Mapping {
                model.acct.rewritten = rewritten_map(doc, &c);
            }
            (
                format!("alias to {:?} in {:?}", t.2, c.2),
                Some(c.3),
                r,
                vec![],
                model,
            )
        }
        10 | 11 => {
            let mut p = Path::new();
            for _ in 0..=rng.below(3) {
                if rng.below(3) == 0 {
                    p.push_index(rng.below(3));
                } else {
                    p.push_key(KEYS[rng.below(4)]);
                }
            }
            let existed = doc.get_path(&p).is_some();
            let old = doc.get_path(&p).map(|n| (n.id(), n.span()));
            // The nodes along the path before (creating below a
            // scalar or null replaces it).
            let prefixes: Vec<(Path, NodeId)> = (0..=p.segments().len())
                .filter_map(|k| {
                    let mut q = Path::new();
                    for sg in &p.segments()[..k] {
                        match sg {
                            oneyaml::PathSegment::Key(x) => q.push_key(x.clone()),
                            oneyaml::PathSegment::Index(i) => q.push_index(*i),
                            _ => {}
                        }
                    }
                    doc.get_path(&q).map(|n| (q, n.id()))
                })
                .collect();
            // As `rewritten_map`: creating a key below a
            // single-pair flow mapping (`[k: v]`) rewrites it as
            // `{k: v, new: x}`, a new mapping at the same place.
            let shape = owners::Shape::of(doc);
            let pairs: Vec<(Path, NodeId)> = prefixes
                .iter()
                .filter(|(q, id)| q.segments().len() < p.segments().len() && shape.is_pair(*id))
                .cloned()
                .collect();
            let r = if op == 10 {
                doc.set_path(&p, &v).map(|n| Some(n.id()))
            } else {
                doc.get_path_or_create(&p).map(|_| None)
            };
            let rewritten = pairs.iter().rev().find_map(|(q, id)| {
                let n = doc.get_path(q)?;
                (r.is_ok() && doc.node(*id).is_err() && n.kind() == NodeKind::Mapping)
                    .then(|| (*id, n.id()))
            });
            let replaced = match (&r, old) {
                _ if rewritten.is_some() => None,
                (Ok(Some(new)), Some((id, _))) if doc.node(id).is_err() => Some((id, Some(*new))),
                _ if r.is_ok() => prefixes
                    .iter()
                    .find(|(_, id)| doc.node(*id).is_err())
                    .map(|(q, id)| (*id, doc.get_path(q).map(|n| n.id()))),
                _ => None,
            };
            let r = r.map(|_| ());
            let mut model = match (op, existed) {
                (10, _) => at_and_whole(Some(of_path(&p)), v.clone(), false),
                (_, true) => same(),
                _ => Model {
                    at: Some((of_path(&p), Want::Exists)),
                    keeps_comments: true,
                    ..Model::default()
                },
            };
            model.acct.replaced = replaced;
            model.acct.rewritten = rewritten;
            (
                format!("path op {op} {p} = {v:?}"),
                Some(Span::new(0, usize::MAX)),
                r,
                vec![],
                model,
            )
        }
        12 => {
            let Some(id) = pick(rng, stale) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let r = match doc.node_mut(id) {
                Err(EditError::StaleNode) => Ok(()),
                other => panic!("stale handle resolved: {:?}", other.map(|m| m.id())),
            };
            ("stale".into(), None, r, vec![], Model::default())
        }
        13 => {
            let Some(m) = pick(rng, &maps) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let occupied = doc
                .node_mut(m.0)
                .and_then(|x| x.entry(key))
                .is_ok_and(|e| e.is_occupied());
            let r = doc
                .node_mut(m.0)
                .and_then(|x| x.entry(key))
                .and_then(|e| e.or_insert(&v))
                .map(|_| ());
            let mut model = match (&m.4, occupied, key) {
                (_, true, _) => same(),
                (_, _, "<<") | (None, _, _) => Model::default(),
                (Some(p), false, _) => {
                    let mut p = p.clone();
                    p.push(Seg::Key(Value::from(key)));
                    at_and_whole(Some(p), v.clone(), true)
                }
            };
            if r.is_ok() && !occupied {
                model.acct.rewritten = rewritten_map(doc, &m);
            }
            (
                format!("entry {key:?} or_insert {v:?}"),
                Some(m.3),
                r,
                vec![],
                model,
            )
        }
        14 => {
            let old = doc.root().map(|n| n.id());
            let r = doc.set_root(&v).map(|n| n.id());
            let mut model = at_and_whole(Some(Vec::new()), v.clone(), false);
            // (A root set to the value it has stays.)
            model.acct.replaced = old
                .filter(|id| r.as_ref().ok() != Some(id))
                .map(|id| (id, r.as_ref().ok().copied()));
            (
                format!("set_root {v:?}"),
                Some(Span::new(0, usize::MAX)),
                r.map(|_| ()),
                vec![],
                model,
            )
        }
        15 => {
            // `remove_keep_comments`: the entry's leading
            // comments stay, re-homed to the next entry or the footer.
            let Some(n) = pick(rng, &all) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let span = parent_span(n.0, doc);
            let holder = holder_of(doc, &all, n.0);
            let mut out = None;
            let r = doc.remove_keep_comments(n.0).map(|rm| out = Some(rm));
            let mut model = match &n.4 {
                Some(p) if !p.is_empty() => {
                    let mut parent = p.clone();
                    let last = parent.pop().unwrap();
                    removal(Some(parent), last)
                }
                _ => Model {
                    keeps_comments: true,
                    ..Model::default()
                },
            };
            if let (Some(rm), Some(h)) = (out, holder) {
                model.acct.removal = Some(h.0);
                model.acct.taken = Some(taken_of(&rm));
                model.acct.keep_lead = true;
                removed.push(rm);
            }
            (
                format!("remove_keep_comments {:?}", n.2),
                span,
                r,
                vec![n.0],
                model,
            )
        }
        _ => {
            // Explicit owners, for the node edits to carry.
            let comments = doc.comment_ids();
            let (Some(c), Some(n)) = (pick(rng, &comments), pick(rng, &all)) else {
                return ("noop".into(), None, Ok(()), vec![], Model::default());
            };
            let owner = match rng.below(3) {
                0 => CommentOwner::Leading(n.0),
                1 => CommentOwner::Trailing(n.0),
                _ => CommentOwner::Footer(n.0),
            };
            let r = doc.attach_comment(c, owner);
            ("noop".into(), None, r.or(Ok(())), vec![], same())
        }
    }
}

/// The value at `path` is `v` afterwards, and the whole value is the value
/// before with `v` set (or inserted) there; `keeps`: nothing was replaced.
fn at_and_whole(path: VPath, v: Value, keeps: bool) -> Model {
    let Some(p) = path else {
        return Model::default();
    };
    let (q, w) = (p.clone(), v.clone());
    Model {
        at: Some((p, Want::Value(v))),
        whole: Some(Box::new(move |x| {
            let Some((last, parent)) = q.split_last() else {
                *x = w;
                return Some(());
            };
            let slot = vget_mut(x, parent)?.untag_mut();
            match last {
                Seg::Key(k) => {
                    slot.as_mapping_mut()?.insert(k.clone(), w);
                }
                Seg::Idx(i) => {
                    let seq = slot.as_sequence_mut()?;
                    if *i == seq.len() {
                        seq.push(w);
                    } else {
                        *seq.get_mut(*i)? = w;
                    }
                }
            }
            Some(())
        })),
        keeps_comments: keeps,
        ..Model::default()
    }
}

/// Entry `last` of the collection at `parent` is gone afterwards.
fn removal(parent: VPath, last: Seg) -> Model {
    let Some(p) = parent else {
        return Model {
            keeps_comments: true,
            ..Model::default()
        };
    };
    Model {
        whole: Some(Box::new(move |x| {
            let slot = vget_mut(x, &p)?.untag_mut();
            match last {
                Seg::Key(k) => slot.as_mapping_mut()?.remove(&k).map(|_| ()),
                Seg::Idx(i) => slot.as_sequence_mut()?.remove_at(i).map(|_| ()),
            }
        })),
        keeps_comments: true,
        ..Model::default()
    }
}

/// `src` with comment lines (at random columns, some after blank lines)
/// and same-line comments injected.
fn commented(rng: &mut Rng, src: &str) -> String {
    let mut lines: Vec<String> = src.lines().map(str::to_owned).collect();
    for l in &mut lines {
        let plain = !l.contains(['#', '\'', '"', '|', '>', '[', '{']) && !l.trim().is_empty();
        if plain && rng.below(4) == 0 {
            l.push_str(" # t");
        }
    }
    for _ in 0..rng.below(8) {
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

/// The handles of a `Removed`'s comments.
fn taken_of(rm: &Removed) -> Vec<CommentId> {
    rm.comment_ids().to_vec()
}

/// The collection holding node `n` as a value or item, with its entries.
fn holder_of(doc: &Document, all: &[NodeInfo], n: NodeId) -> Option<(NodeId, Vec<EntryInfo>)> {
    all.iter()
        .filter(|c| matches!(c.1, NodeKind::Mapping | NodeKind::Sequence))
        .map(|c| (c.0, entries_of(doc, c.0)))
        .find(|(_, es)| es.iter().any(|e| e.1 == n))
}

/// The value of `entries` an insert replaced (its handle went stale), and
/// the new value.
/// An insertion into a single-pair flow mapping (`[k: v]`)
/// rewrites it as `{k: v, new: x}`, a new node at the same place: its
/// comments are owned by the corresponding nodes of the new mapping (as
/// for a re-insertion there, `owners::Op::Reinsert`).
fn rewritten_map(doc: &Document, m: &NodeInfo) -> Option<(NodeId, NodeId)> {
    if doc.node(m.0).is_ok() {
        return None;
    }
    nodes(doc)
        .into_iter()
        .find(|n| n.1 == NodeKind::Mapping && n.3.start == m.3.start)
        .map(|n| (m.0, n.0))
}

fn replaced_value(
    doc: &Document,
    entries: &[EntryInfo],
    new: Option<NodeId>,
) -> Option<(NodeId, Option<NodeId>)> {
    new?;
    entries
        .iter()
        .find(|e| doc.node(e.1).is_err())
        .map(|e| (e.1, new))
}

/// `ONEYAML_EDIT_FUZZ_COLLECT` (triage only): report each document's
/// first failure and go on, failing at the end.
fn run(name: &str, doc: Document, rng: &mut Rng, steps: usize, t: &mut Tally) {
    if std::env::var("ONEYAML_EDIT_FUZZ_COLLECT").is_err() {
        return run_one(name, doc, rng, steps, t);
    }
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_one(name, doc, rng, steps, t);
    }));
    if let Err(e) = r {
        let m = e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|x| (*x).to_owned()))
            .unwrap_or_default();
        eprintln!("FAIL {}", m.replace('\n', "\n     "));
        t.collected += 1;
    }
}

pub(crate) fn run_one(name: &str, mut doc: Document, rng: &mut Rng, steps: usize, t: &mut Tally) {
    t.docs += 1;
    let mut removed = Vec::new();
    let mut stale: Vec<NodeId> = Vec::new();
    let mut tainted = std::collections::HashSet::new();
    // The comments each live `Removed` carries, by its handles.
    let mut carried: std::collections::HashMap<Vec<CommentId>, Vec<owners::Carried>> =
        std::collections::HashMap::new();
    for i in 0..steps {
        lookups(
            &doc,
            &format!("{name} before step {i}: {:?}", doc.to_string()),
        );
        let before = doc.to_string();
        let snapshot = nodes(&doc);
        let meaning_before = Value::try_from(&doc).ok();
        let plain_before = plain(&doc);
        let comments_before = comment_count(&doc, &removed);
        let owners_before = owners::snap(&doc);
        let shape_before = owners::Shape::of(&doc);
        let explicit = |d: &Document| {
            d.explain_comments()
                .unwrap()
                .iter()
                .filter(|e| e.rule() == oneyaml::CommentRule::Explicit)
                .count()
        };
        let explicit_before = explicit(&doc);
        let held = removed.len();
        let (desc, touched, r, gone, mut model) =
            step(&mut doc, rng, &mut removed, &stale, &tainted);
        // A `Removed` taken while the meaning had an error may carry it.
        if removed.len() > held && meaning_before.is_none() {
            if let Some(rm) = removed.last() {
                tainted.insert(rm.text().to_owned());
            }
        }
        // The edited collection lies inside a mapping key, or an alias
        // does (its anchored node's edits change that key).
        model.in_key = snapshot.iter().any(|n| n.5 && n.1 == NodeKind::Alias)
            || touched.is_some_and(|t| {
                snapshot
                    .iter()
                    .any(|n| n.5 && n.3.start <= t.start && t.end <= n.3.end)
            });
        let ctx = format!("{name} step {i}: {desc}\nbefore: {before:?}");
        let comments_after = comment_count(&doc, &removed);
        if std::env::var("ONEYAML_EDIT_FUZZ_TRACE").is_ok_and(|t| t == name) {
            eprintln!("TRACE {ctx}\n  -> {r:?} {:?}", doc.to_string());
        }
        if r.is_err() || model.keeps_comments {
            // Nothing destroyed silently.
            assert_eq!(
                comments_before,
                comments_after,
                "comments lost or made up: {ctx}\nafter: {:?}",
                doc.to_string()
            );
            t.comment_checks += 1;
        }
        match r {
            Ok(()) => {
                if desc == "noop" {
                    t.noop += 1;
                    continue;
                }
                if desc == "stale" {
                    t.stale_checked += 1;
                    continue;
                }
                t.ok += 1;
                agrees_with_its_text(&doc, &ctx);
                // Comment identity accounting against the
                // rulebook.
                let op = model.acct.op(&carried);
                let out = account(&owners_before, &shape_before, &doc, &op, &ctx, t);
                if let owners::Op::Remove { taken, .. } = &op {
                    if !taken.is_empty() {
                        carried.insert(taken.clone(), out);
                    }
                }
                // A move carries every explicit owner.
                if model.acct.moved.is_some() {
                    assert_eq!(
                        explicit(&doc),
                        explicit_before,
                        "explicit owner lost: {ctx}"
                    );
                    t.explicit_moves += explicit_before;
                }
                check_meaning(&doc, meaning_before, plain_before, model, &ctx, t);
                // Handles outside the edited region still resolve, with
                // the same text.
                if let Some(span) = touched {
                    for (id, _, text, sp, _, _) in &snapshot {
                        let disjoint = sp.end < span.start || span.end < sp.start;
                        if disjoint && !gone.contains(id) {
                            let now = doc.node(*id).unwrap_or_else(|e| {
                                panic!(
                                    "unrelated handle {text:?} lost ({e}): {ctx}\nafter: {:?}",
                                    doc.to_string()
                                )
                            });
                            assert_eq!(now.text(), text.as_str(), "{ctx}");
                        }
                    }
                }
                for id in gone {
                    if !stale.contains(&id) {
                        stale.push(id);
                    }
                }
            }
            Err(e) => {
                t.refused += 1;
                let kind = match &e {
                    // By reason, checked against the documented
                    // list below.
                    EditError::Unsupported { reason, .. } => format!("Unsupported: {reason}"),
                    _ => {
                        let kind = format!("{e:?}");
                        kind.split([' ', '{', '(']).next().unwrap_or("").to_owned()
                    }
                };
                let slot = t.kinds.entry(kind).or_insert((0, String::new()));
                slot.0 += 1;
                if slot.0 <= 4 {
                    slot.1.push_str(&format!(
                        "
    e.g. {e:?} {ctx}"
                    ));
                }
                assert_eq!(
                    doc.to_string(),
                    before,
                    "refused edit ({e}) changed the text: {ctx}"
                );
                let _ = e.to_string();
            }
        }
        for id in &stale {
            assert_eq!(doc.node(*id).err(), Some(EditError::StaleNode), "{ctx}");
        }
    }
    lookups(
        &doc,
        &format!("{name} after the last step: {:?}", doc.to_string()),
    );
}

#[test]
fn edit_scripts_over_the_suite_and_generated_documents() {
    if std::env::var("ONEYAML_EDIT_FUZZ_COLLECT").is_ok() {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let steps = env("ONEYAML_EDIT_FUZZ_STEPS", 10);
    let rounds = env("ONEYAML_EDIT_FUZZ_ROUNDS", 150);
    let seed = env("ONEYAML_EDIT_FUZZ_SEED", 0x5eed) as u64;
    let (part, parts) = section();
    let cases = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    assert!(cases.len() > 300);
    let mut t = Tally::default();
    let settings = [Settings::new(), Settings::new().merge_keys(true)];
    for (ci, case) in cases.iter().enumerate() {
        if ci % parts != part {
            continue;
        }
        let Ok(src) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        for (si, s) in settings.iter().enumerate() {
            let mut rng = Rng(seed ^ (ci as u64).wrapping_mul(0x9e37_79b9) ^ si as u64);
            let doc = match Document::parse_with(src.clone(), s) {
                Ok(d) => d,
                Err(_) => Document::parse_recovering_with(src.clone(), s).into_value(),
            };
            run(&case.id, doc, &mut rng, steps, &mut t);
        }
    }
    let mut g = Rng(seed.wrapping_add(1));
    for r in 0..rounds {
        if r % parts != part {
            continue;
        }
        let src = generator::generate(&mut g);
        // CR and CRLF documents (section 5.4).
        let src = match r % 3 {
            1 => src.replace('\n', "\r\n"),
            2 => src.replace('\n', "\r"),
            _ => src,
        };
        let s = Settings::new().merge_keys(true);
        let doc = match Document::parse_with(src.clone(), &s) {
            Ok(d) => d,
            Err(_) => Document::parse_recovering_with(src.clone(), &s).into_value(),
        };
        let mut rng = Rng(seed ^ (r as u64) << 20);
        run(&format!("generated {r}"), doc, &mut rng, steps, &mut t);
    }
    // Generated documents with comment lines, blank lines, and
    // same-line comments injected (the rulebook decides their owners; the
    // identity accounting checks every edit against it), with CR and CRLF
    // line breaks.
    let mut g = Rng(seed.wrapping_add(7));
    for r in 0..rounds {
        if r % parts != part {
            continue;
        }
        let plain_src = generator::generate(&mut g);
        let src = commented(&mut g, &plain_src);
        let src = match r % 3 {
            1 => src.replace('\n', "\r\n"),
            2 => src.replace('\n', "\r"),
            _ => src,
        };
        let s = Settings::new().merge_keys(true);
        let doc = match Document::parse_with(src.clone(), &s) {
            Ok(d) => d,
            Err(_) => Document::parse_recovering_with(src.clone(), &s).into_value(),
        };
        let mut rng = Rng(seed ^ (r as u64) << 22);
        run(&format!("commented {r}"), doc, &mut rng, steps, &mut t);
    }
    // Documents that redefine anchor names, with CR and CRLF
    // line breaks.
    let anchored = [
        "a: &x 1\nb: *x\nc: &x 2\nd: *x\nl: [*x, &x 3, *x]\nm:\n  - &y {k: *x}\n  - *y\n",
        "l:\n  - &x a # one\n  - *x\n  - &x b\n  - *x\n# end\nk: &x c\n",
        "base: &b {p: 1}\nsvc:\n  <<: *b\n  # svc comment\n  q: 2\nother: &b [1]\nuse: *b\n",
    ];
    for (ai, src) in anchored.iter().enumerate() {
        for (bi, nl) in ["\n", "\r\n", "\r"].iter().enumerate() {
            let src = src.replace('\n', nl);
            for round in 0..20 {
                if (ai * 60 + bi * 20 + round) % parts != part {
                    continue;
                }
                let s = Settings::new().merge_keys(true);
                let doc = Document::parse_with(src.clone(), &s).unwrap();
                let mut rng = Rng(seed ^ ((ai * 100 + bi * 30 + round) as u64) << 24);
                run(
                    &format!("anchored {ai}/{bi}/{round}"),
                    doc,
                    &mut rng,
                    steps,
                    &mut t,
                );
            }
        }
    }
    // Block scalar entries at the ends of
    // block collections, after entries that end their line, followed by
    // comment lines (their moves lost the comment; their removal was
    // refused), with CR and CRLF line breaks.
    let block_ends = [
        "k: |\n  text\n# c\nm: |\n  t2\n# c2\n",
        "o:\n  k: |\n    s\n  m: |\n    t\n  # c2\nz: 1\n",
        "- |\n  a\n# c1\n- |\n  b\n# c2\n",
        "k: 1\nm: |\n  t2\n# c2\nz: 3\n",
        "steps:\n  - name: x\n    run: |\n      echo hi\n  - name: y\n    run: >+\n      a\n\n",
        "data:\n  a: 1 # one\n  s.sh: |-\n    echo\n  # tail\nk: 2\n",
        "o:\n  - v0\n  - |+\n    x\n\n  - v2\nz: 1\n",
    ];
    for (bi, src) in block_ends.iter().enumerate() {
        for (ni, nl) in ["\n", "\r\n", "\r"].iter().enumerate() {
            let src = src.replace('\n', nl);
            for round in 0..20 {
                if (bi * 60 + ni * 20 + round) % parts != part {
                    continue;
                }
                let doc = Document::parse(src.as_str()).unwrap();
                let mut rng = Rng(seed ^ ((bi * 100 + ni * 30 + round) as u64) << 28);
                run(
                    &format!("block ends {bi}/{ni}/{round}"),
                    doc,
                    &mut rng,
                    steps,
                    &mut t,
                );
            }
        }
    }
    eprintln!(
        "edit fuzz: {} documents, {} accepted, {} refused, {} stale checks, {} no-ops; \
         meaning checks: {} at a path, {} whole; {} comment checks; \
         {} accepted edits made a meaning an error (anchors or merge keys); \
         identity accounting: {} checks, {} Removed exactness checks ({} comments taken), \
         {} comments rewritten or carried, {} explicit owners carried by moves",
        t.docs,
        t.ok,
        t.refused,
        t.stale_checked,
        t.noop,
        t.at_checks,
        t.whole_checks,
        t.comment_checks,
        t.invalidated,
        t.identity_checks,
        t.taken_checks,
        t.taken_comments,
        t.moved_comments,
        t.explicit_moves
    );
    for (k, (n, example)) in &t.kinds {
        eprintln!("  refused {k}: {n}");
        if std::env::var("ONEYAML_EDIT_FUZZ_VERBOSE").is_ok() {
            eprintln!("{example}");
        }
    }
    assert!(t.ok > 0);
    // Every `Unsupported` the fuzz meets is one documented on
    // `Document` (its documented limitations, and refusals of
    // requests YAML cannot express); anything else is a bug.
    for k in t.kinds.keys() {
        if let Some(reason) = k.strip_prefix("Unsupported: ") {
            assert!(
                DOCUMENTED_UNSUPPORTED.contains(&reason),
                "undocumented Unsupported refusal: {reason}"
            );
        }
    }
    // Every oracle actually ran.
    let _ = std::panic::take_hook();
    assert_eq!(t.collected, 0, "failures collected");
    assert!(t.at_checks > 0, "no meaning check at a path ran");
    assert!(t.whole_checks > 0, "no whole-meaning check ran");
    assert!(t.comment_checks > 0, "no comment-count check ran");
    // The identity accounting ran, on removals, and saw comments
    // move.
    assert!(t.identity_checks > 0, "no identity accounting ran");
    assert!(t.taken_checks > 0, "no Removed exactness check ran");
    assert!(t.moved_comments > 0, "no comment was rewritten or carried");
    assert!(t.taken_comments > 0, "no removal took a comment");
    assert!(
        !t.kinds.contains_key("Internal"),
        "never a panic: Internal refusals: {:?}",
        t.kinds.get("Internal")
    );
}
