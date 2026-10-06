//! The targets added by the second campaign, for the areas the
//! first fuzz targets did not reach directly:
//!
//! * `tree_edit`: the `-syntax` edit layer itself (edit
//!   validation, recovered input, and what lies underneath) through the
//!   edit battery of `oneyaml-syntax/tests/reparse_sweep.rs`, single
//!   and chained, each accepted edit compared whole with a re-parse;
//! * `cap`: capped diagnostics (`max_diagnostics`), the
//!   `bench/tests/cap_invariance.rs` outcomes and the capped lists
//!   against the uncapped one, before and after edits;
//! * `handles`: the handle API: the equal-value set is a
//!   byte no-op through `NodeMut::set`, the parent's `NodeMut::insert` on
//!   the node's key and `NodeMut::replace_at` on its index, and
//!   `Document::set_root` (not `set_scalar` or `set_path`, which the
//!   `edit` target's scripts cover), and a mapping value's handle edits
//!   its entry's comments.
//!
//! Each panics exactly when an invariant breaks (test tooling).
#![allow(clippy::panic)]

use oneyaml::{
    Compat, Diagnostic, Document, EditError, Limits, NodeId, NodeKind, NodeRef, ScalarStyle,
    Settings, Severity, Stream, Value, code,
};
use oneyaml_syntax::tree::{NodeId as TreeId, SyntaxKind, SyntaxTree, compare_trees};

use crate::reuse::{cap_invariance, known, reparse_sweep};
use crate::targets::{fnv, text};

/// A xorshift64 generator seeded from the input's hash, so a replayed
/// input replays its choices.
struct Rng(u64);

impl Rng {
    fn new(data: &[u8]) -> Self {
        Self(fnv(data) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        let n = u64::try_from(n.max(1)).unwrap_or(u64::MAX);
        usize::try_from(self.next().checked_rem(n).unwrap_or(0)).unwrap_or(0)
    }
}

/// `tree` equals a fresh parse of its text under its own
/// settings, whole, except for the documented exception (`known`).
fn tree_agrees(tree: &SyntaxTree, what: &dyn Fn() -> String) -> bool {
    let again = SyntaxTree::parse_with(tree.emit(), tree.settings());
    if let Err(e) = compare_trees(tree, &again) {
        if known::excused(tree, &again) {
            return false;
        }
        panic!(
            "the edited tree differs from its re-parse ({e}): {}\ntext: {:?}",
            what(),
            tree.emit()
        );
    }
    true
}

/// Elements sampled per setting by [`tree_edit`].
const TREE_ELEMENTS: usize = 6;

/// Chained edits per setting by [`tree_edit`].
const TREE_CHAIN: usize = 8;

/// **`tree_edit`**: a battery of edits (removal, copies, moves, rolled back
/// groups, inserts of white space, tabs, breaks, comment lines, entries and
/// markers, token rewrites, node replacements) at sampled elements of the
/// input's tree, under the default settings and `lenient_inline_colons`, each
/// on a fork of the parsed tree; then a chain of such edits on one tree (edit
/// history). Every accepted edit leaves a tree equal (`compare_trees`) to a
/// re-parse of its text, with or without errors (see *Recovered input* in the
/// `-syntax` tree docs); no edit is `Internal`.
pub fn tree_edit(data: &[u8]) {
    let src = text(data);
    let mut rng = Rng::new(data);
    for s in [Settings::new(), Settings::new().lenient_inline_colons(true)] {
        let base = SyntaxTree::parse_with(src.as_ref(), &s);
        let all = elements(&base);
        if all.is_empty() {
            continue;
        }
        for _ in 0..TREE_ELEMENTS.min(all.len()) {
            let Some((id, kind, tok)) = all.get(rng.below(all.len())) else {
                continue;
            };
            for e in reparse_sweep::edits(*kind, tok) {
                let mut tree = base.fork();
                match reparse_sweep::apply(&mut tree, *id, &e, &s) {
                    Some(true) => {
                        tree_agrees(&tree, &|| {
                            format!("{e:?} at {kind:?} {tok:?}\nsource {src:?}")
                        });
                    }
                    Some(false) => unchanged(&base, &tree, &e, &|| {
                        format!("{e:?} at {kind:?} {tok:?}\nsource {src:?}")
                    }),
                    None => {}
                }
            }
        }
        // A chain: each accepted edit is the next one's starting point.
        let mut tree = base.fork();
        let mut done = Vec::new();
        for _ in 0..TREE_CHAIN {
            let now = elements(&tree);
            let Some((id, kind, tok)) = now.get(rng.below(now.len())).cloned() else {
                break;
            };
            let es = reparse_sweep::edits(kind, &tok);
            let Some(e) = es.get(rng.below(es.len())) else {
                continue;
            };
            let before = tree.fork();
            if reparse_sweep::apply(&mut tree, id, e, &s) != Some(true) {
                unchanged(&before, &tree, e, &|| {
                    format!("chain {done:?} then {e:?} at {kind:?} {tok:?}\nsource {src:?}")
                });
                continue;
            }
            done.push(format!("{e:?} at {kind:?} {tok:?}"));
            if !tree_agrees(&tree, &|| format!("chain {done:?}\nsource {src:?}")) {
                break;
            }
        }
    }
}

/// A refused edit leaves the tree as it
/// was: its text, and the whole tree (`compare_trees`). Every battery
/// edit is one call except `mv` (a removal, then an insertion that may be
/// refused after the removal was accepted), which is not checked.
fn unchanged(before: &SyntaxTree, after: &SyntaxTree, e: &str, what: &dyn Fn() -> String) {
    if e == "mv" {
        return;
    }
    assert_eq!(
        after.emit(),
        before.emit(),
        "a refused edit changed the text: {}",
        what()
    );
    if let Err(x) = compare_trees(before, after) {
        panic!("a refused edit changed the tree ({x}): {}", what());
    }
}

/// Every element below the root: its handle, kind and token text.
fn elements(t: &SyntaxTree) -> Vec<(TreeId, SyntaxKind, String)> {
    t.root()
        .descendants()
        .skip(1)
        .map(|n| {
            let tok = if n.is_token() {
                n.text().into_owned()
            } else {
                String::new()
            };
            (n.id(), n.kind(), tok)
        })
        .collect()
}

/// The fields a capped list keeps for an entry: code, severity, primary
/// span, and message.
type Key = (oneyaml::Code, Severity, usize, usize, String);

fn key(d: &Diagnostic) -> Key {
    (
        d.code,
        d.severity,
        d.primary.span.start,
        d.primary.span.end,
        d.message.clone(),
    )
}

fn keys(list: &[Diagnostic]) -> Vec<Key> {
    list.iter().map(key).collect()
}

/// `capped` is `full` (every diagnostic, no cap)
/// reported under `max_diagnostics = limit`: all of it when it fits;
/// else its first `limit` entries, the first error when none of those is
/// one (its code, severity and span; its message may be its code's
/// summary), and one `Y2010` summary counting exactly what is left out,
/// placed at the first left out.
fn capped_as_documented(capped: &[Diagnostic], full: &[Diagnostic], limit: usize, what: &str) {
    if full.len() <= limit {
        assert_eq!(keys(capped), keys(full), "{what}: under the cap");
        return;
    }
    let summaries: Vec<&Diagnostic> = capped
        .iter()
        .filter(|d| d.code == code::DIAGNOSTICS_SUPPRESSED)
        .collect();
    let [summary] = summaries.as_slice() else {
        panic!("{what}: {} summaries\n{capped:#?}", summaries.len());
    };
    let rest: Vec<&Diagnostic> = capped
        .iter()
        .filter(|d| d.code != code::DIAGNOSTICS_SUPPRESSED)
        .collect();
    let prefix = full.get(..limit).unwrap_or_default();
    let first_error = full
        .iter()
        .position(|d| d.severity == Severity::Error)
        .filter(|i| *i >= limit);
    let want = prefix
        .len()
        .saturating_add(usize::from(first_error.is_some()));
    assert_eq!(rest.len(), want, "{what}: kept entries\n{capped:#?}");
    for (got, exp) in rest.iter().zip(prefix) {
        assert_eq!(key(got), key(exp), "{what}: kept prefix");
    }
    if let (Some(i), Some(got)) = (first_error, rest.last()) {
        let exp = full.get(i).unwrap_or_else(|| panic!("{what}"));
        assert_eq!(
            (got.code, got.severity, got.primary.span),
            (exp.code, exp.severity, exp.primary.span),
            "{what}: the first error"
        );
    }
    let left = full.len().saturating_sub(rest.len());
    let noun = if left == 1 {
        "diagnostic"
    } else {
        "diagnostics"
    };
    assert_eq!(
        summary.message,
        format!("{left} more {noun} suppressed"),
        "{what}: summary"
    );
    assert_eq!(summary.severity, Severity::Warning, "{what}: summary");
    let at = full.get(limit).map(|d| d.primary.span.start);
    assert_eq!(
        Some(summary.primary.span.start),
        at,
        "{what}: summary position"
    );
}

/// The caps [`cap`] reads under, beside the uncapped read.
const CAPS: [usize; 4] = [0, 1, 2, 16];

/// The stream and each document of `st`, capped, against `full` read
/// with no cap.
fn stream_capped(st: &Stream, full: &Stream, limit: usize, what: &str) {
    capped_as_documented(
        &st.diagnostics(),
        &full.diagnostics(),
        limit,
        &format!("{what}: stream"),
    );
    assert_eq!(st.len(), full.len(), "{what}: documents");
    assert_eq!(st.has_errors(), full.has_errors(), "{what}: has_errors");
    for (i, (a, b)) in st.documents().zip(full.documents()).enumerate() {
        assert_eq!(a.has_errors(), b.has_errors(), "{what}: doc {i} has_errors");
        let ra = a.to_value().map_err(|e| (e.code(), e.span()));
        let rb = b.to_value().map_err(|e| (e.code(), e.span()));
        assert_eq!(ra, rb, "{what}: doc {i} value");
    }
}

/// Edit steps per input in [`cap`].
const CAP_STEPS: usize = 4;

/// **cap**: meaning never depends on `max_diagnostics` (the
/// `cap_invariance` outcomes: reads, strict and recovering parses,
/// `has_errors`, error regions, a sample edit script per document, under
/// caps 0, 1, 2, 16, the profile's and none); the capped lists of a
/// recovering `Stream` and `Document` are the uncapped ones cut as
/// documented ([`capped_as_documented`]); and a short edit script run on
/// a capped and an uncapped copy of the stream is accepted alike, after
/// which the capped stream's diagnostics equal a re-parse of its text
/// with the same limits and the uncapped list cut as documented.
pub fn cap(data: &[u8]) {
    let src = text(data);
    let mut rng = Rng::new(data);
    // `untrusted()` limits (as `cap_invariance` reads the adversarial
    // corpus): an expansion bomb within the default alias budget costs
    // seconds per read instrumented, and `outcome` reads ~30 times.
    let base = match rng.below(3) {
        0 => Settings::new(),
        1 => Compat::serde_yaml(),
        _ => Settings::new().lenient_inline_colons(true).merge_keys(true),
    }
    .limits(Limits::untrusted());
    cap_invariance::same("fuzz", &base, &|t| cap_invariance::outcome(&src, t));
    let with = |n: usize| base.clone().limits(Limits::untrusted().max_diagnostics(n));
    let full_s = with(usize::MAX);
    let full = Stream::parse_recovering_with(src.as_ref(), &full_s).into_value();
    let full_doc = Document::parse_recovering_with(src.as_ref(), &full_s).into_value();
    let n = CAPS.get(rng.below(CAPS.len())).copied().unwrap_or_default();
    let s = with(n);
    let mut st = Stream::parse_recovering_with(src.as_ref(), &s).into_value();
    stream_capped(&st, &full, n, "parsed");
    let doc = Document::parse_recovering_with(src.as_ref(), &s).into_value();
    capped_as_documented(&doc.diagnostics(), &full_doc.diagnostics(), n, "document");
    let mut full = full;
    for step in 0..CAP_STEPS {
        if st.is_empty() {
            break;
        }
        let i = rng.below(st.len());
        let op = rng.below(4);
        let pick = rng.next();
        let a = cap_edit(&mut st, i, op, pick);
        let b = cap_edit(&mut full, i, op, pick);
        let what = format!("step {step}: doc {i} op {op}\nsource {src:?}");
        assert_eq!(
            a.as_ref().map_err(edit_kind),
            b.as_ref().map_err(edit_kind),
            "{what}: accepted alike"
        );
        assert_eq!(st.to_string(), full.to_string(), "{what}: text");
        if a.is_err() {
            continue;
        }
        let text = st.to_string();
        let again = Stream::parse_recovering_with(text.as_str(), &s).into_value();
        assert_eq!(
            keys(&st.diagnostics()),
            keys(&again.diagnostics()),
            "{what}: capped diagnostics vs a re-parse\ntext {text:?}"
        );
        for (j, (x, y)) in st.documents().zip(again.documents()).enumerate() {
            assert_eq!(
                keys(&x.diagnostics()),
                keys(&y.diagnostics()),
                "{what}: doc {j} diagnostics vs a re-parse\ntext {text:?}"
            );
        }
        stream_capped(&st, &full, n, &what);
    }
}

/// The first word of an edit error's `Debug` form: its kind.
fn edit_kind(e: &EditError) -> String {
    let s = format!("{e:?}");
    s.split(['(', ' ', '{'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// One edit of [`cap`] on document `i`: insert, remove, replace or
/// set-root, at a node chosen by `pick`.
fn cap_edit(st: &mut Stream, i: usize, op: usize, pick: u64) -> Result<(), EditError> {
    let Some(d) = st.document(i) else {
        return Ok(());
    };
    let items: Vec<NodeId> = d.root().map(|r| children(r)).unwrap_or_default();
    let kind = d.root().map(|r| r.kind());
    let at = usize::try_from(pick).unwrap_or(0);
    let item = at
        .checked_rem(items.len())
        .and_then(|j| items.get(j))
        .copied();
    let Some(mut dm) = st.document_mut(i) else {
        return Ok(());
    };
    match (op, kind, item) {
        (0, Some(NodeKind::Mapping), _) => dm
            .root_mut()
            .and_then(|mut m| m.insert("zz_cap", 1).map(|_| ())),
        (0, Some(NodeKind::Sequence), _) => dm.root_mut().and_then(|mut m| m.push(1).map(|_| ())),
        (1, _, Some(id)) => dm.remove_node(id).map(|_| ()),
        (2, _, Some(id)) => dm.node_mut(id).and_then(|mut m| m.set("zz").map(|_| ())),
        _ => dm.set_root([1, 2]).map(|_| ()),
    }
}

/// The values of a mapping, or the items of a sequence.
fn children(n: NodeRef<'_>) -> Vec<NodeId> {
    if let Some(m) = n.as_mapping() {
        m.iter().map(|(_, v)| v.id()).collect()
    } else if let Some(q) = n.as_sequence() {
        q.iter().map(|v| v.id()).collect()
    } else {
        Vec::new()
    }
}

/// Both readers agree after an accepted `Document` edit: the tree equals a
/// re-parse of its text (bar the known exception), the meaning and the
/// diagnostics equal a re-parse's; refusals are never `Internal`.
fn doc_agrees(doc: &Document, what: &dyn Fn() -> String) {
    if !tree_agrees(doc.syntax(), what) {
        return;
    }
    let text = doc.to_string();
    let s = doc.settings();
    let a = doc.to_value().map_err(|e| e.code());
    let b = oneyaml::from_str_with::<Value>(&text, s).map_err(|e| e.code());
    assert_eq!(a, b, "same meaning: {}\ntext {text:?}", what());
    let re = Document::parse_recovering_with(text.as_str(), s).into_value();
    assert_eq!(
        re.diagnostics(),
        doc.diagnostics(),
        "same diagnostics: {}\ntext {text:?}",
        what()
    );
}

fn not_internal<T>(r: &Result<T, EditError>, what: &dyn Fn() -> String) {
    if let Err(EditError::Internal) = r {
        panic!("EditError::Internal: {}", what());
    }
}

/// A node of the document as [`handles`] visits it.
struct Visit {
    id: NodeId,
    /// The value is read through its node (`NodeRef::deserialize`).
    value: Option<Value>,
    /// The node is or holds an alias, a merge key (with
    /// `merge_keys` on: an untagged plain `<<`, or any key tagged
    /// `!!merge`; see [`merge_key`]) or repeated keys, where the equal-value no-op does not
    /// apply.
    exempt: bool,
    /// The parent collection and the entry's key or index.
    parent: Option<(NodeId, Slot)>,
    /// The entry's key node, for a mapping value.
    key: Option<NodeId>,
}

#[derive(Clone)]
enum Slot {
    Key(Value),
    Index(usize),
}

/// A node [`visits`] has still to visit: the node, its parent collection
/// and slot, and its key node (for a mapping value).
type Pending<'d> = (NodeRef<'d>, Option<(NodeId, Slot)>, Option<NodeId>);

/// Values with more nodes are not set by [`handles`].
const HANDLE_VALUE_NODES: usize = 512;

/// Nodes [`handles`] visits per input.
const HANDLE_NODES: usize = 48;

/// Every node (up to [`HANDLE_NODES`], preorder, keys included, not
/// through aliases); an explicit stack, no recursion.
fn visits(doc: &Document, merge_keys: bool) -> Vec<Visit> {
    let Some(root) = doc.root() else {
        return Vec::new();
    };
    // A node holding aliases is read (expanded) only when the whole
    // document reads as a small value: each read of an expansion bomb
    // costs up to the alias budget (a cost of the oracle, not of the library).
    let small = doc
        .to_value()
        .is_ok_and(|v| crate::targets::nodes_at_most(&v, HANDLE_VALUE_NODES));
    let mut out = Vec::new();
    let mut stack: Vec<Pending<'_>> = vec![(root, None, None)];
    while let Some((n, parent, key)) = stack.pop() {
        if out.len() >= HANDLE_NODES {
            break;
        }
        let (exempt, aliased) = exempt(n, merge_keys);
        out.push(Visit {
            id: n.id(),
            value: (small || !aliased)
                .then(|| n.deserialize::<Value>().ok())
                .flatten(),
            exempt,
            parent,
            key,
        });
        if n.alias_name().is_some() {
            continue;
        }
        if let Some(m) = n.as_mapping() {
            let entries: Vec<_> = m.iter().collect();
            for (k, v) in entries.into_iter().rev() {
                let slot = k.deserialize::<Value>().ok().map(Slot::Key);
                stack.push((v, slot.map(|s| (n.id(), s)), Some(k.id())));
                stack.push((k, None, None));
            }
        } else if let Some(q) = n.as_sequence() {
            let items: Vec<_> = q.iter().enumerate().collect();
            for (j, v) in items.into_iter().rev() {
                stack.push((v, Some((n.id(), Slot::Index(j))), None));
            }
        }
    }
    out
}

/// The tag `!!merge` resolves to (`oneyaml`'s `resolve::MERGE_TAG`).
const MERGE_TAG: &str = "tag:yaml.org,2002:merge";

/// Whether key `k` is a merge key under `merge_keys`, exactly as
/// `oneyaml` decides it (`Resolver::is_merge_key`, which is crate-private;
/// mirrored here): an untagged plain `<<`, or any scalar whose
/// tag resolves to `tag:yaml.org,2002:merge` (`!!merge`, or a `%TAG`
/// handle naming it), whatever its text or quoting (`!!merge '<<'`,
/// `!!merge x`). Neither a quoted untagged `'<<'`, a non-specific `! <<`,
/// nor a local tag such as `!x:merge` is one: the equal-value no-op
/// covers those, as it covers every key with `merge_keys` off.
fn merge_key(k: NodeRef<'_>, merge_keys: bool) -> bool {
    let Some(s) = k.as_scalar().filter(|_| merge_keys) else {
        return false;
    };
    match k.tag() {
        Ok(None) => s.style() == ScalarStyle::Plain && s.value() == "<<",
        Ok(Some(t)) => t == MERGE_TAG,
        // (A tag that does not resolve is `Y9000`, which the parser rules
        // out: not a case for this oracle; exempted.)
        Err(_) => true,
    }
}

/// Whether the subtree of `n` holds an alias, a merge key (see
/// [`merge_key`]), or two keys of one mapping with equal values, and
/// whether it holds an alias (or is too large to tell), by an explicit
/// stack.
fn exempt(n: NodeRef<'_>, merge_keys: bool) -> (bool, bool) {
    let mut stack = vec![n];
    let mut seen = 0usize;
    let mut other = false;
    while let Some(n) = stack.pop() {
        seen = seen.saturating_add(1);
        if seen > 4096 || n.alias_name().is_some() {
            return (true, true);
        }
        if let Some(m) = n.as_mapping() {
            let mut ks: Vec<Value> = Vec::new();
            for (k, v) in m.iter() {
                other |= merge_key(k, merge_keys);
                if k.alias_name().is_none()
                    && let Ok(kv) = k.deserialize::<Value>()
                {
                    other |= ks.contains(&kv);
                    ks.push(kv);
                }
                stack.push(k);
                stack.push(v);
            }
        } else if let Some(q) = n.as_sequence() {
            stack.extend(q.iter());
        }
    }
    (other, false)
}

/// The settings [`handles`] reads under.
/// (`untrusted()` limits throughout: each node is read on its own, so an
/// expansion bomb within the default budget costs the fuzzer seconds.)
fn handle_settings(pick: usize) -> Settings {
    match pick {
        0 => Settings::new(),
        1 => Settings::new().merge_keys(true),
        2 => Settings::new()
            .merge_keys(true)
            .duplicate_keys(oneyaml::DuplicateKeys::LastWins),
        _ => Settings::new().lenient_inline_colons(true),
    }
    .limits(Limits::untrusted())
}

/// Comment line sets for [`handles`].
const LINES: [&[&str]; 4] = [&[], &["c"], &["two", "lines"], &["#x", ""]];

/// **handles**: for each node, setting it to the value
/// it has (through `NodeMut::set`, the parent's `insert` on its key, the
/// parent's `replace_at`, and `set_root`) is accepted as a byte no-op that
/// keeps the handle, unless it is or holds an alias, a merge key (with
/// `merge_keys` on, as the library decides it: [`merge_key`]) or repeated
/// keys (then any accepted result keeps one meaning); a refused call
/// changes nothing. For each mapping value, the comment edits of its
/// handle act on its entry: `set_leading`, `set_trailing` and
/// `set_blank_lines_before` equal the base layer on the key, and
/// `clear_comments` equals clearing the key's and then the value's own
/// comments, as one edit. Every accepted edit keeps one meaning.
pub fn handles(data: &[u8]) {
    let src = text(data).into_owned();
    let mut rng = Rng::new(data);
    let s = handle_settings(rng.below(4));
    let doc = match Document::parse_with(src.as_str(), &s) {
        Ok(d) => d,
        Err(_) => Document::parse_recovering_with(src.as_str(), &s).into_value(),
    };
    let before = doc.to_string();
    let merge_keys = s.get_merge_keys();
    for v in visits(&doc, merge_keys) {
        // A value read through aliases is written expanded: past
        // `HANDLE_VALUE_NODES` that is the oracle's cost, not the edit's.
        let Some(value) = v
            .value
            .as_ref()
            .filter(|x| crate::targets::nodes_at_most(x, HANDLE_VALUE_NODES))
        else {
            continue;
        };
        let ctx = |how: &str| format!("{how} {:?} = {value:?}\nsource {src:?}", v.id);
        // Through the node's handle.
        let mut c = doc.clone();
        let r = c
            .node_mut(v.id)
            .and_then(|mut m| m.set(value).map(|m| m.id()));
        equal_set(&c, &before, &r, v.id, v.exempt, &|| ctx("set"));
        // Through the parent: `insert` on the key, `replace_at` the index.
        if let Some((p, slot)) = &v.parent {
            // Through the parent, its repeated keys count too (which own
            // entry `insert` names is the last one, LastWins' meaning).
            let exempt = v.exempt || doc.node(*p).is_ok_and(|p| exempt(p, merge_keys).0);
            let mut c = doc.clone();
            let r = c.node_mut(*p).and_then(|mut m| match slot {
                Slot::Key(k) => m.insert(k, value).map(|n| n.id()),
                Slot::Index(j) => m.replace_at(*j, value).map(|n| n.id()),
            });
            equal_set(&c, &before, &r, v.id, exempt, &|| ctx("parent"));
        } else if doc.root().is_some_and(|r| r.id() == v.id) {
            let mut c = doc.clone();
            let r = c.set_root(value).map(|n| n.id());
            equal_set(&c, &before, &r, v.id, v.exempt, &|| ctx("set_root"));
        }
        if let Some(k) = v.key {
            entry_comments(&doc, v.id, k, &mut rng, &src);
        }
    }
}

/// The equal-value set's oracle (see [`handles`]).
fn equal_set(
    c: &Document,
    before: &str,
    r: &Result<NodeId, EditError>,
    id: NodeId,
    exempt: bool,
    what: &dyn Fn() -> String,
) {
    not_internal(r, what);
    match r {
        Err(_) => assert_eq!(c.to_string(), before, "a refused set changed: {}", what()),
        Ok(new) if !exempt => {
            assert_eq!(
                c.to_string(),
                before,
                "an equal-value set changed bytes: {}",
                what()
            );
            assert_eq!(*new, id, "an equal-value set moved the handle: {}", what());
        }
        Ok(_) => doc_agrees(c, what),
    }
}

/// The value-handle comment edits of the entry `key: value` (see
/// [`handles`]) against the id-taking base layer.
fn entry_comments(doc: &Document, value: NodeId, key: NodeId, rng: &mut Rng, src: &str) {
    let lines = LINES.get(rng.below(LINES.len())).copied().unwrap_or(&[]);
    let trailing = ["t", "", "#t"].get(rng.below(3)).copied();
    let trailing = trailing.filter(|_| rng.below(4) != 0);
    let blanks = rng.below(3);
    let what = |op: &str| format!("{op} on value {value:?} (key {key:?})\nsource {src:?}");
    let pair = |op: &str,
                via: &dyn Fn(&mut Document) -> Result<(), EditError>,
                base: &dyn Fn(&mut Document) -> Result<(), EditError>| {
        let (mut a, mut b) = (doc.clone(), doc.clone());
        let (ra, rb) = (via(&mut a), base(&mut b));
        let w = || what(op);
        not_internal(&ra, &w);
        not_internal(&rb, &w);
        assert_eq!(
            ra.as_ref().map_err(edit_kind),
            rb.as_ref().map_err(edit_kind),
            "value handle vs base layer: {}",
            w()
        );
        assert_eq!(
            a.to_string(),
            b.to_string(),
            "value handle vs base layer: {}",
            w()
        );
        if ra.is_ok() {
            doc_agrees(&a, &w);
        } else {
            assert_eq!(
                a.to_string(),
                doc.to_string(),
                "a refused edit changed: {}",
                w()
            );
        }
    };
    pair(
        "set_leading",
        &|d| d.node_mut(value)?.set_leading(lines).map(|_| ()),
        &|d| d.set_leading(key, lines).map(|_| ()),
    );
    pair(
        "set_trailing",
        &|d| d.node_mut(value)?.set_trailing(trailing).map(|_| ()),
        &|d| d.set_trailing(key, trailing).map(|_| ()),
    );
    pair(
        "set_blank_lines_before",
        &|d| d.node_mut(value)?.set_blank_lines_before(blanks),
        &|d| d.set_blank_lines_before(key, blanks),
    );
    // `clear_comments`: the key's, then the value's own, as one edit.
    let (mut a, mut b) = (doc.clone(), doc.clone());
    let ra = a.node_mut(value).and_then(|mut m| m.clear_comments());
    let w = || what("clear_comments");
    not_internal(&ra, &w);
    let rb = b.clear_comments(key).and_then(|mut t| {
        t.extend(b.clear_comments(value)?);
        Ok(t)
    });
    not_internal(&rb, &w);
    match (&ra, &rb) {
        (Ok(x), Ok(y)) => {
            assert_eq!(x, y, "clear_comments texts: {}", w());
            assert_eq!(a.to_string(), b.to_string(), "clear_comments: {}", w());
            doc_agrees(&a, &w);
        }
        (Err(_), Err(_)) => {
            assert_eq!(
                a.to_string(),
                doc.to_string(),
                "a refused clear changed: {}",
                w()
            );
        }
        _ => panic!("clear_comments {ra:?} vs base layer {rb:?}: {}", w()),
    }
}
