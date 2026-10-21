//! The comment-API edit-script fuzz:
//! seeded random sequences of comment calls — `set_leading`,
//! `set_trailing`, `push_footer`, `clear_comments`, `set_header`,
//! `set_footer`, `attach_comment`, `detach_comment`, with valid and
//! invalid text — over the yaml-test-suite inputs (valid and invalid, with
//! comment lines injected at random places and columns), generated
//! documents, and CR/CRLF variants. After every call:
//!
//! * nothing panics;
//! * a refused call changed nothing: the text, and every explanation;
//! * **comment accounting by identity** (nothing destroyed
//!   silently"): the comments after are exactly those before,
//!   minus the ones the rulebook path (`explain_comments`, read before
//!   the call, not the API under test) gives the owners the call
//!   replaces, plus the ids it returned; `clear_comments`' reported texts
//!   are exactly those removed; every surviving comment keeps its owner
//!   (except the one an `attach_comment`/`detach_comment` re-owned);
//! * **untouched bytes**: the content tokens (everything but white
//!   space, line breaks, and comments) are exactly as before, and so is
//!   the meaning;
//! * **round trip**: the edited text, parsed again, re-emits byte
//!   for byte and explains every comment exactly as the edited document
//!   does — except comments with an explicit owner (`attach_comment`),
//!   which the text cannot carry (documented);
//! * a new comment belongs to the owner it was written for;
//! * in a tree without errors, every comment inside a document's node is
//!   that document's (the invariant `attach_comment`'s fast path rests
//!   on), checked after every accepted call.
//!
//! Streams are fuzzed through `Stream::document_mut`, with node edits
//! (`move_entry`, `remove_node`, `set`) interleaved: they change the
//! document (checked by a re-parse, the membership invariant, and every
//! explicit owner still naming a live node), and exercise the pruning of
//! explicit owners across node edits.
//!
//! `ONEYAML_COMMENT_FUZZ_STEPS` sets the calls per document (default 12),
//! `ONEYAML_COMMENT_FUZZ_SEED` the seed, `ONEYAML_COMMENT_FUZZ_SECTION=i/n`
//! runs the i-th of n slices of the corpus, `ONEYAML_COMMENT_FUZZ_TRACE=Kind`
//! prints the calls refused with that `EditError` variant,
//! `ONEYAML_COMMENT_FUZZ_DOC=name` every call on that document.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

pub(crate) mod generator;

use generator::Rng;
use std::collections::{BTreeMap, BTreeSet};

use oneyaml::{
    CommentId, CommentOwner, Document, DocumentMut, EditError, NodeId, Settings, Stream,
};
use oneyaml_syntax::tree::SyntaxKind;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn section() -> (usize, usize) {
    std::env::var("ONEYAML_COMMENT_FUZZ_SECTION")
        .ok()
        .and_then(|s| {
            let (a, b) = s.split_once('/')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .filter(|(i, n)| *n > 0 && i < n)
        .unwrap_or((0, 1))
}

/// View texts: ordinary, awkward, and invalid ones (refused, never
/// mangled).
const TEXTS: &[&str] = &[
    "note",
    "",
    " indented",
    "\ttab",
    "trailing  ",
    "- commented: item",
    "#",
    "é ünïcødé",
    "a: b # c",
    "---",
    "two\nlines",
    "cr\r",
    "bom\u{feff}",
    "bell\u{7}",
    "nel\u{85}",
    "ls\u{2028}ps\u{2029}",
];

/// The comment tokens of the document's text.
fn comment_count(doc: &Document) -> usize {
    doc.syntax()
        .root()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::Comment)
        .count()
}

/// The content tokens (everything but white space, line breaks,
/// comments): what comment calls must leave byte for byte.
fn content(doc: &Document) -> Vec<(SyntaxKind, String)> {
    doc.syntax()
        .root()
        .descendants()
        .filter(|n| n.is_token() && !n.kind().is_trivia())
        .map(|n| (n.kind(), n.text().into_owned()))
        .collect()
}

type Shape = (usize, String, String, Option<(usize, usize)>, u8);

/// Every explanation without handles (owners by span), explicit ones
/// marked.
fn shapes(doc: &Document) -> Vec<(Shape, bool)> {
    doc.explain_comments()
        .unwrap()
        .iter()
        .map(|e| {
            let owner = match e.owner() {
                CommentOwner::DocumentHeader => "header",
                CommentOwner::DocumentFooter => "footer",
                CommentOwner::Leading(_) => "leading",
                CommentOwner::Trailing(_) => "trailing",
                CommentOwner::Footer(_) => "collection footer",
                _ => "other",
            };
            let explicit = e.rule() == oneyaml::CommentRule::Explicit;
            (
                (
                    e.span().start,
                    e.text().to_owned(),
                    owner.to_owned(),
                    e.owner_span().map(|s| (s.start, s.end)),
                    e.rule().number(),
                ),
                explicit,
            )
        })
        .collect()
}

/// The nodes of the document: every node and key, each once.
fn nodes(doc: &Document) -> Vec<NodeId> {
    let mut out = Vec::new();
    let Some(root) = doc.root() else { return out };
    let mut stack = vec![root];
    // A loop, not recursion; aliases are not followed (each node once).
    while let Some(n) = stack.pop() {
        out.push(n.id());
        if n.kind() == oneyaml::NodeKind::Alias {
            continue;
        }
        if let Some(m) = n.as_mapping() {
            if m.node() == n {
                for (k, v) in m.iter() {
                    out.push(k.id());
                    stack.push(v);
                }
            }
        }
        if let Some(s) = n.as_sequence() {
            if s.node() == n {
                stack.extend(s.iter());
            }
        }
    }
    out
}

#[derive(Default, Debug)]
pub(crate) struct Tally {
    docs: usize,
    ok: usize,
    refused: usize,
    new_comments: usize,
    removed: usize,
    reparsed: usize,
    streams: usize,
    stream_ok: usize,
    node_edits: usize,
    by_error: std::collections::BTreeMap<String, usize>,
}

fn pick<T: Copy>(rng: &mut Rng, v: &[T]) -> Option<T> {
    (!v.is_empty()).then(|| v[rng.below(v.len())])
}

fn lines(rng: &mut Rng) -> Vec<&'static str> {
    (0..rng.below(3))
        .map(|_| TEXTS[rng.below(TEXTS.len())])
        .collect()
}

/// What a call was aimed at, for the owner check.
enum Aim {
    Owner(CommentOwner),
    None,
}

/// The comment calls, on a [`Document`] or through a stream's
/// [`DocumentMut`] (the stream path is fuzzed too).
trait Ops {
    fn doc(&self) -> &Document;
    fn set_leading(&mut self, n: NodeId, l: &[&str]) -> Result<Vec<CommentId>, EditError>;
    fn set_trailing(&mut self, n: NodeId, t: Option<&str>) -> Result<Option<CommentId>, EditError>;
    fn push_footer(&mut self, n: NodeId, t: &str) -> Result<CommentId, EditError>;
    fn clear_comments(&mut self, n: NodeId) -> Result<Vec<String>, EditError>;
    fn set_header(&mut self, l: &[&str]) -> Result<Vec<CommentId>, EditError>;
    fn set_footer(&mut self, l: &[&str]) -> Result<Vec<CommentId>, EditError>;
    fn attach_comment(&mut self, c: CommentId, o: CommentOwner) -> Result<(), EditError>;
    fn detach_comment(&mut self, c: CommentId) -> Result<Option<CommentOwner>, EditError>;
}

macro_rules! ops {
    // `$node`: how a node edit reaches node `n`: the base layer (by id,
    // exactly that node) of a `Document` or of a stream's guard.
    ($t:ty, |$s:ident, $n:ident| $node:expr) => {
        impl Ops for $t {
            fn doc(&self) -> &Document {
                self
            }
            fn set_leading(&mut self, n: NodeId, l: &[&str]) -> Result<Vec<CommentId>, EditError> {
                let ($s, $n) = (self, n);
                $node.set_leading(l)
            }
            fn set_trailing(
                &mut self,
                n: NodeId,
                t: Option<&str>,
            ) -> Result<Option<CommentId>, EditError> {
                let ($s, $n) = (self, n);
                $node.set_trailing(t)
            }
            fn push_footer(&mut self, n: NodeId, t: &str) -> Result<CommentId, EditError> {
                let ($s, $n) = (self, n);
                $node.push_footer(t)
            }
            fn clear_comments(&mut self, n: NodeId) -> Result<Vec<String>, EditError> {
                let ($s, $n) = (self, n);
                $node.clear_comments()
            }
            fn set_header(&mut self, l: &[&str]) -> Result<Vec<CommentId>, EditError> {
                <$t>::set_header(self, l)
            }
            fn set_footer(&mut self, l: &[&str]) -> Result<Vec<CommentId>, EditError> {
                <$t>::set_footer(self, l)
            }
            fn attach_comment(&mut self, c: CommentId, o: CommentOwner) -> Result<(), EditError> {
                <$t>::attach_comment(self, c, o)
            }
            fn detach_comment(&mut self, c: CommentId) -> Result<Option<CommentOwner>, EditError> {
                <$t>::detach_comment(self, c)
            }
        }
    };
}

/// The `Document` base layer: the id-taking methods.
struct ById<'a>(&'a mut Document, NodeId);

impl ById<'_> {
    fn set_leading(self, l: &[&str]) -> Result<Vec<CommentId>, EditError> {
        self.0.set_leading(self.1, l)
    }
    fn set_trailing(self, t: Option<&str>) -> Result<Option<CommentId>, EditError> {
        self.0.set_trailing(self.1, t)
    }
    fn push_footer(self, t: &str) -> Result<CommentId, EditError> {
        self.0.push_footer(self.1, t)
    }
    fn clear_comments(self) -> Result<Vec<String>, EditError> {
        self.0.clear_comments(self.1)
    }
}

ops!(Document, |d, n| ById(d, n));
ops!(DocumentMut<'_>, |d, n| ByGuard(d, n));

/// A stream document's guard: its id-taking comment edits (exactly the
/// node, as the base layer), and `push_footer` through its handle
/// (the guard has no copy of that one).
struct ByGuard<'a, 's>(&'a mut DocumentMut<'s>, NodeId);

impl ByGuard<'_, '_> {
    fn set_leading(self, l: &[&str]) -> Result<Vec<CommentId>, EditError> {
        self.0.set_leading(self.1, l)
    }
    fn set_trailing(self, t: Option<&str>) -> Result<Option<CommentId>, EditError> {
        self.0.set_trailing(self.1, t)
    }
    fn push_footer(self, t: &str) -> Result<CommentId, EditError> {
        self.0.node_mut(self.1)?.push_footer(t)
    }
    fn clear_comments(self) -> Result<Vec<String>, EditError> {
        self.0.clear_comments(self.1)
    }
}

/// One random comment call, and what it is expected to do.
struct Call {
    desc: String,
    r: Result<Vec<CommentId>, EditError>,
    /// The owners whose comments (as read before the call) it replaces.
    removes: Vec<CommentOwner>,
    /// The owner it writes new comments for.
    aim: Aim,
    /// The comment it re-owns (`attach_comment`, `detach_comment`).
    reowned: Option<CommentId>,
    /// `clear_comments`' reported texts.
    reported: Option<Vec<String>>,
}

fn call(desc: String, r: Result<Vec<CommentId>, EditError>) -> Call {
    Call {
        desc,
        r,
        removes: Vec::new(),
        aim: Aim::None,
        reowned: None,
        reported: None,
    }
}

fn step<T: Ops>(target: &mut T, rng: &mut Rng) -> Call {
    let ns = nodes(target.doc());
    let comments = target.doc().comment_ids();
    let node = pick(rng, &ns);
    let text = TEXTS[rng.below(TEXTS.len())];
    let noop = || call("noop".to_owned(), Ok(Vec::new()));
    match (rng.below(8), node) {
        (0, Some(n)) => {
            let ls = lines(rng);
            Call {
                removes: vec![CommentOwner::Leading(n)],
                aim: Aim::Owner(CommentOwner::Leading(n)),
                ..call(
                    format!("set_leading {n:?} {ls:?}"),
                    target.set_leading(n, &ls),
                )
            }
        }
        (1, Some(n)) => {
            let some = rng.below(4) != 0;
            let r = target.set_trailing(n, some.then_some(text));
            Call {
                removes: vec![CommentOwner::Trailing(n)],
                aim: Aim::Owner(CommentOwner::Trailing(n)),
                ..call(
                    format!("set_trailing {n:?} {:?}", some.then_some(text)),
                    r.map(|x| x.into_iter().collect()),
                )
            }
        }
        (2, Some(n)) => Call {
            aim: Aim::Owner(CommentOwner::Footer(n)),
            ..call(
                format!("push_footer {n:?} {text:?}"),
                target.push_footer(n, text).map(|c| vec![c]),
            )
        },
        (3, Some(n)) => {
            let r = target.clear_comments(n);
            let reported = r.as_ref().ok().cloned();
            Call {
                removes: vec![
                    CommentOwner::Leading(n),
                    CommentOwner::Trailing(n),
                    CommentOwner::Footer(n),
                ],
                reported,
                ..call(format!("clear_comments {n:?}"), r.map(|_| Vec::new()))
            }
        }
        (4, _) => {
            let ls = lines(rng);
            Call {
                removes: vec![CommentOwner::DocumentHeader],
                aim: Aim::Owner(CommentOwner::DocumentHeader),
                ..call(format!("set_header {ls:?}"), target.set_header(&ls))
            }
        }
        (5, _) => {
            let ls = lines(rng);
            Call {
                removes: vec![CommentOwner::DocumentFooter],
                aim: Aim::Owner(CommentOwner::DocumentFooter),
                ..call(format!("set_footer {ls:?}"), target.set_footer(&ls))
            }
        }
        (6, Some(n)) => {
            let owner = match rng.below(5) {
                0 => CommentOwner::Leading(n),
                1 => CommentOwner::Trailing(n),
                2 => CommentOwner::Footer(n),
                3 => CommentOwner::DocumentHeader,
                _ => CommentOwner::DocumentFooter,
            };
            match pick(rng, &comments) {
                Some(c) => Call {
                    reowned: Some(c),
                    ..call(
                        format!("attach_comment {c:?} {owner:?}"),
                        target.attach_comment(c, owner).map(|()| Vec::new()),
                    )
                },
                None => noop(),
            }
        }
        (7, _) => match pick(rng, &comments) {
            Some(c) => Call {
                reowned: Some(c),
                ..call(
                    format!("detach_comment {c:?}"),
                    target.detach_comment(c).map(|_| Vec::new()),
                )
            },
            None => noop(),
        },
        _ => noop(),
    }
}

/// Every comment of a document by handle: its owner (explicit ones
/// applied) and stored text, as the rulebook path explains them.
type Owners = BTreeMap<CommentId, (CommentOwner, String)>;

fn owners(doc: &Document) -> Owners {
    doc.explain_comments()
        .unwrap()
        .iter()
        .map(|e| (e.id(), (e.owner(), e.text().to_owned())))
        .collect()
}

/// Accounting by identity (see the module docs). Returns the
/// number of comments removed.
fn identity(before: &Owners, after: &Owners, c: &Call, new: &[CommentId], ctx: &str) -> usize {
    let removed: BTreeSet<CommentId> = before
        .iter()
        .filter(|(_, (o, _))| c.removes.contains(o))
        .map(|(id, _)| *id)
        .collect();
    if let Some(reported) = &c.reported {
        let mut want: Vec<&str> = removed.iter().map(|id| before[id].1.as_str()).collect();
        let mut got: Vec<&str> = reported.iter().map(String::as_str).collect();
        want.sort_unstable();
        got.sort_unstable();
        assert_eq!(got, want, "clear_comments reported other comments: {ctx}");
    }
    let mut expected: BTreeSet<CommentId> = before
        .keys()
        .filter(|id| !removed.contains(id))
        .copied()
        .collect();
    expected.extend(new.iter().copied());
    let got: BTreeSet<CommentId> = after.keys().copied().collect();
    assert_eq!(
        got, expected,
        "comments removed or added other than expected: {ctx}"
    );
    for (id, (owner, text)) in before {
        if removed.contains(id) || Some(*id) == c.reowned {
            continue;
        }
        let (now, now_text) = &after[id];
        assert_eq!(now, owner, "a surviving comment changed owner: {ctx}");
        assert_eq!(now_text, text, "a surviving comment changed text: {ctx}");
    }
    removed.len()
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

/// In a tree without errors, every comment inside document
/// `j`'s node is one of document `j`'s comments (by span).
fn membership(tree: &oneyaml_syntax::tree::SyntaxTree, docs: &[Document], ctx: &str) {
    if tree.has_errors() {
        return;
    }
    let nodes: Vec<_> = tree
        .root()
        .children()
        .filter(|n| n.kind() == SyntaxKind::Document)
        .collect();
    for (d, doc) in nodes.iter().zip(docs) {
        let own: BTreeSet<usize> = doc
            .explain_comments()
            .unwrap()
            .iter()
            .map(|e| e.span().start)
            .collect();
        for c in d.descendants().filter(|n| n.kind() == SyntaxKind::Comment) {
            assert!(
                own.contains(&c.span().start),
                "a comment inside a clean document is not its own: {:?}: {ctx}",
                c.text()
            );
        }
    }
}

/// Every explicit owner still names a live node (pruning across edits).
fn explicit_owners_live(doc: &Document, ctx: &str) {
    for e in doc.explain_comments().unwrap() {
        if e.rule() == oneyaml::CommentRule::Explicit {
            if let CommentOwner::Leading(n) | CommentOwner::Trailing(n) | CommentOwner::Footer(n) =
                e.owner()
            {
                assert!(doc.node(n).is_ok(), "an explicit owner is gone: {ctx}");
            }
        }
    }
}

/// One random node edit through a stream's document.
fn node_step(d: &mut DocumentMut<'_>, rng: &mut Rng) -> (String, Result<(), EditError>) {
    let ns = nodes(d);
    let Some(n) = pick(rng, &ns) else {
        return ("noop".to_owned(), Ok(()));
    };
    match rng.below(3) {
        0 => {
            let (from, to) = (rng.below(3), rng.below(3));
            (
                format!("move_entry {n:?} {from} {to}"),
                d.node_mut(n).and_then(|mut m| m.move_entry(from, to)),
            )
        }
        1 => (format!("remove_node {n:?}"), d.remove_node(n).map(|_| ())),
        _ => (
            format!("set {n:?}"),
            d.node_mut(n).and_then(|mut m| m.set("v").map(|_| ())),
        ),
    }
}

/// A refused call: counted, never `Internal`.
fn refused(e: &EditError, ctx: &str, t: &mut Tally) {
    t.refused += 1;
    let kind = format!("{e:?}");
    let kind = kind.split([' ', '{', '(']).next().unwrap_or("").to_owned();
    if std::env::var("ONEYAML_COMMENT_FUZZ_TRACE").is_ok_and(|k| k == kind) {
        eprintln!("TRACE {ctx}");
    }
    *t.by_error.entry(kind).or_default() += 1;
    assert!(!matches!(e, EditError::Internal), "internal error: {ctx}");
}

/// The explanations of `mine` that are not explicit, and those of
/// `theirs` (a re-parse) at the same places.
fn round_trip(mine: &Document, theirs: &Document, ctx: &str) {
    let own: Vec<Shape> = shapes(mine)
        .into_iter()
        .filter(|(_, explicit)| !explicit)
        .map(|(s, _)| s)
        .collect();
    let explicit_at: Vec<usize> = shapes(mine)
        .into_iter()
        .filter(|(_, explicit)| *explicit)
        .map(|(s, _)| s.0)
        .collect();
    let other: Vec<Shape> = shapes(theirs)
        .into_iter()
        .map(|(s, _)| s)
        .filter(|s| !explicit_at.contains(&s.0))
        .collect();
    assert_eq!(own, other, "owners differ after a re-parse: {ctx}");
}

/// Every collection's lookup index (built here whatever
/// its width, then maintained by the calls that follow, accepted or
/// rolled back) agrees with the linear scans.
fn lookups(doc: &Document, ctx: &str) {
    if let Err(e) = doc.check_lookup_indexes() {
        panic!("a lookup index went stale: {e}\n{ctx}");
    }
}

pub(crate) fn run(name: &str, mut doc: Document, rng: &mut Rng, steps: usize, t: &mut Tally) {
    t.docs += 1;
    let recovering = doc.has_errors();
    for i in 0..steps {
        lookups(
            &doc,
            &format!("{name} before step {i}: {:?}", doc.to_string()),
        );
        let before = doc.to_string();
        let before_shapes = shapes(&doc);
        let before_owners = owners(&doc);
        let before_content = content(&doc);
        let before_value = doc.to_value().map_err(|e| e.code());
        let count = comment_count(&doc);
        let c = step(&mut doc, rng);
        let ctx = format!("{name} step {i}: {}\nbefore: {before:?}", c.desc);
        let after = doc.to_string();
        if std::env::var("ONEYAML_COMMENT_FUZZ_DOC").is_ok_and(|d| d == name) {
            eprintln!("STEP {ctx}\n  -> {:?} {after:?}\n  {:?}", c.r, shapes(&doc));
        }
        match &c.r {
            Err(e) => {
                refused(e, &ctx, t);
                assert_eq!(after, before, "a refused call changed the text: {ctx}");
                assert_eq!(
                    shapes(&doc),
                    before_shapes,
                    "a refused call changed owners: {ctx}"
                );
            }
            Ok(new) => {
                if c.desc == "noop" {
                    continue;
                }
                t.ok += 1;
                t.new_comments += new.len();
                let ctx = format!("{ctx}\nafter: {after:?}");
                // Nothing destroyed silently, by identity.
                let removed = identity(&before_owners, &owners(&doc), &c, new, &ctx);
                t.removed += removed;
                assert_eq!(
                    comment_count(&doc),
                    count - removed + new.len(),
                    "comments lost or made up: {ctx}"
                );
                // Only trivia changed, so the meaning did not.
                assert_eq!(content(&doc), before_content, "content changed: {ctx}");
                assert_eq!(
                    doc.to_value().map_err(|e| e.code()),
                    before_value,
                    "meaning changed: {ctx}"
                );
                // New comments belong where they were written.
                if let Aim::Owner(o) = c.aim {
                    for id in new {
                        let e = doc.explain_comment(*id).unwrap();
                        assert_eq!(e.owner(), o, "new comment owner: {ctx}");
                    }
                }
                membership(doc.syntax(), std::slice::from_ref(&doc), &ctx);
                whole_tree(doc.syntax(), &ctx);
                // The round trip: both readers agree.
                let again = if recovering {
                    Document::parse_recovering_with(after.clone(), doc.settings()).into_value()
                } else {
                    Document::parse_with(after.clone(), doc.settings())
                        .unwrap_or_else(|e| panic!("the edited text does not parse ({e}): {ctx}"))
                };
                assert_eq!(
                    again.to_string().as_bytes(),
                    after.as_bytes(),
                    "byte-identical: {ctx}"
                );
                round_trip(&doc, &again, &ctx);
                t.reparsed += 1;
            }
        }
    }
}

/// [`run`] over the documents of a stream, each call through
/// [`Stream::document_mut`], with node edits interleaved;
/// the checks are the stream's: its text, each document's comments
/// by identity, content, meaning, and explanations against a re-parse of
/// the stream, and the membership invariant.
pub(crate) fn run_stream(name: &str, mut s: Stream, rng: &mut Rng, steps: usize, t: &mut Tally) {
    t.streams += 1;
    let recovering = s.has_errors();
    let tokens = |s: &Stream, comments: bool| {
        s.syntax()
            .root()
            .descendants()
            .filter(|n| n.is_token())
            .filter(|n| {
                if comments {
                    n.kind() == SyntaxKind::Comment
                } else {
                    !n.kind().is_trivia()
                }
            })
            .map(|n| n.text().into_owned())
            .collect::<Vec<_>>()
    };
    for i in 0..steps {
        let n = s.len();
        if n == 0 {
            return;
        }
        let di = rng.below(n);
        let before = s.to_string();
        let docs = |s: &Stream| -> Vec<Document> { (0..n).filter_map(|j| s.document(j)).collect() };
        for (j, d) in docs(&s).iter().enumerate() {
            lookups(
                d,
                &format!("{name} before step {i} (document {j}): {before:?}"),
            );
        }
        let before_shapes: Vec<_> = docs(&s).iter().map(shapes).collect();
        let before_owners: Vec<Owners> = docs(&s).iter().map(owners).collect();
        let before_values: Vec<_> = docs(&s)
            .iter()
            .map(|d| d.to_value().map_err(|e| e.code()))
            .collect();
        let before_content = tokens(&s, false);
        let count = tokens(&s, true).len();
        // One call in four is a node edit.
        if rng.below(4) == 0 {
            let (desc, r) = {
                let mut d = s.document_mut(di).unwrap();
                node_step(&mut d, rng)
            };
            let after = s.to_string();
            let ctx = format!(
                "{name} step {i} (document {di}): {desc}\nbefore: {before:?}\nafter: {after:?}"
            );
            match r {
                Err(e) => {
                    refused(&e, &ctx, t);
                    assert_eq!(after, before, "a refused node edit changed the text: {ctx}");
                    let now: Vec<_> = docs(&s).iter().map(shapes).collect();
                    assert_eq!(
                        now, before_shapes,
                        "a refused node edit changed owners: {ctx}"
                    );
                }
                Ok(()) if desc == "noop" => {}
                Ok(()) => {
                    t.node_edits += 1;
                    let again = if recovering {
                        Stream::parse_recovering(after.clone()).into_value()
                    } else {
                        Stream::parse(after.clone()).unwrap_or_else(|e| {
                            panic!("the edited stream does not parse ({e}): {ctx}")
                        })
                    };
                    assert_eq!(
                        again.to_string().as_bytes(),
                        after.as_bytes(),
                        "byte-identical: {ctx}"
                    );
                    assert_eq!(again.len(), n, "{ctx}");
                    let now = docs(&s);
                    membership(s.syntax(), &now, &ctx);
                    whole_tree(s.syntax(), &ctx);
                    for (j, d) in now.iter().enumerate() {
                        explicit_owners_live(d, &ctx);
                        round_trip(
                            d,
                            &again.document(j).unwrap(),
                            &format!("{ctx} (document {j})"),
                        );
                    }
                }
            }
            continue;
        }
        let c = {
            let mut d = s.document_mut(di).unwrap();
            step(&mut d, rng)
        };
        let after = s.to_string();
        let ctx = format!(
            "{name} step {i} (document {di}): {}\nbefore: {before:?}",
            c.desc
        );
        if std::env::var("ONEYAML_COMMENT_FUZZ_DOC").is_ok_and(|d| d == name) {
            let now: Vec<_> = docs(&s).iter().map(shapes).collect();
            eprintln!("STEP {ctx}\n  -> {:?} {after:?}\n  {now:?}", c.r);
        }
        match &c.r {
            Err(e) => {
                refused(e, &ctx, t);
                assert_eq!(after, before, "a refused call changed the text: {ctx}");
                let now: Vec<_> = docs(&s).iter().map(shapes).collect();
                assert_eq!(now, before_shapes, "a refused call changed owners: {ctx}");
            }
            Ok(new) => {
                if c.desc == "noop" {
                    continue;
                }
                t.ok += 1;
                t.stream_ok += 1;
                t.new_comments += new.len();
                let ctx = format!("{ctx}\nafter: {after:?}");
                let now = docs(&s);
                let mut removed = 0;
                for (j, d) in now.iter().enumerate() {
                    let after_owners = owners(d);
                    if j == di {
                        removed = identity(&before_owners[j], &after_owners, &c, new, &ctx);
                    } else {
                        assert_eq!(
                            after_owners, before_owners[j],
                            "another document's comments changed: {ctx}"
                        );
                    }
                }
                t.removed += removed;
                assert_eq!(
                    tokens(&s, true).len(),
                    count - removed + new.len(),
                    "comments lost or made up: {ctx}"
                );
                assert_eq!(tokens(&s, false), before_content, "content changed: {ctx}");
                let values: Vec<_> = now
                    .iter()
                    .map(|d| d.to_value().map_err(|e| e.code()))
                    .collect();
                assert_eq!(values, before_values, "meaning changed: {ctx}");
                if let Aim::Owner(o) = c.aim {
                    for id in new {
                        assert_eq!(
                            now[di].explain_comment(*id).unwrap().owner(),
                            o,
                            "new comment owner: {ctx}"
                        );
                    }
                }
                membership(s.syntax(), &now, &ctx);
                whole_tree(s.syntax(), &ctx);
                let again = if recovering {
                    Stream::parse_recovering(after.clone()).into_value()
                } else {
                    Stream::parse(after.clone())
                        .unwrap_or_else(|e| panic!("the edited stream does not parse ({e}): {ctx}"))
                };
                assert_eq!(
                    again.to_string().as_bytes(),
                    after.as_bytes(),
                    "byte-identical: {ctx}"
                );
                assert_eq!(again.len(), n, "{ctx}");
                for (j, d) in now.iter().enumerate() {
                    round_trip(
                        d,
                        &again.document(j).unwrap(),
                        &format!("{ctx} (document {j})"),
                    );
                }
                t.reparsed += 1;
            }
        }
    }
}

/// `src` with comment lines injected at random lines and columns.
fn with_comments(rng: &mut Rng, src: &str) -> String {
    let mut lines: Vec<String> = src.lines().map(str::to_owned).collect();
    for _ in 0..rng.below(4) {
        let at = rng.below(lines.len() + 1);
        let col = [0, 1, 2, 4][rng.below(4)];
        lines.insert(at, format!("{}# c{at}", " ".repeat(col)));
    }
    let mut out = lines.join("\n");
    if src.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[test]
#[ignore = "slow: runs in CI"]
fn comment_scripts_over_the_suite_and_generated_documents() {
    let steps = env("ONEYAML_COMMENT_FUZZ_STEPS", 12);
    let seed = env("ONEYAML_COMMENT_FUZZ_SEED", 0xc0_ffee) as u64;
    let (part, parts) = section();
    let cases = test_suite::load_suite(&test_suite::default_data_dir()).unwrap();
    assert!(cases.len() > 300);
    let mut t = Tally::default();
    for (ci, case) in cases.iter().enumerate() {
        if ci % parts != part {
            continue;
        }
        let Ok(src) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        let mut rng = Rng(((seed ^ (ci as u64).wrapping_mul(0x9e37_79b9)) << 1) | 1);
        for variant in 0..2 {
            let src = if variant == 0 {
                src.clone()
            } else {
                with_comments(&mut rng, &src)
            };
            let doc = match Document::parse(src.clone()) {
                Ok(d) => d,
                Err(_) => Document::parse_recovering(src.clone()).into_value(),
            };
            run(
                &format!("{} v{variant}", case.id),
                doc,
                &mut rng,
                steps,
                &mut t,
            );
        }
    }
    let mut g = Rng((seed.wrapping_add(7) << 1) | 1);
    for r in 0..120usize {
        if r % parts != part {
            continue;
        }
        let base = generator::generate(&mut g);
        let src = with_comments(&mut g, &base);
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
        let mut rng = Rng(((seed ^ ((r as u64) << 20)) << 1) | 1);
        run(&format!("generated {r}"), doc, &mut rng, steps, &mut t);
    }
    // Hand-written documents with every kind of owner.
    let rich = [
        "# header\n\n# lead a\na: 1 # trailing a\nb:\n  - x\n  # footer of b\nc: 3 # trailing c\n# document footer\n",
        "%YAML 1.2\n# among directives\n--- # on the marker\nk: [ # open\n  # lead x\n  x, # after x\n  y\n  # end\n  ] # close\n... # on the end\n# after\n",
        "a:\n  b:\n    - 1\n    # inner\n  # outer\nc: |\n  text\n# lead d\nd: 1\n",
        "? a # key\n# before colon\n: b\nl:\n- 1\n- 2\n\n\n# spaced\nz: {p: 1,\n  q: 2}\n",
        "a: 1 # t\n... # c1\n... # c2\n# c3\n...\n  # c4\n... # c5\n",
    ];
    for (ri, src) in rich.iter().enumerate() {
        for (ni, nl) in ["\n", "\r\n", "\r"].iter().enumerate() {
            for round in 0..10usize {
                if (ri * 30 + ni * 10 + round) % parts != part {
                    continue;
                }
                let doc = Document::parse(src.replace('\n', nl)).unwrap();
                let mut rng = Rng(((seed ^ ((ri * 100 + ni * 30 + round) as u64) << 24) << 1) | 1);
                run(
                    &format!("rich {ri}/{ni}/{round}"),
                    doc,
                    &mut rng,
                    steps,
                    &mut t,
                );
            }
        }
    }
    // Streams, every call through `Stream::document_mut`: the
    // suite's multi-document inputs (with comment lines injected), and
    // hand-written streams.
    let streams = [
        "# h0\na: 1 # t\n---\n# h1\nb:\n  - 2\n  # f\n...\n# after\n--- c\n# end\n",
        "%YAML 1.2\n--- # m\nk: |+\n  text\n# x\n...\n---\nl: [ # o\n  1,\n  ]\n  ",
        "- a\n---\n- b\n# c\n---\n",
        // Stray `...` markers with comments between documents.
        "a\n... # c1\n... # c2\n# c3\n...\n  # c4\n... # c5\n--- b\n# c6\n...\n... # c7\n",
    ];
    for (ci, case) in cases.iter().enumerate() {
        if ci % parts != part {
            continue;
        }
        let Ok(src) = String::from_utf8(case.input.clone()) else {
            continue;
        };
        if !src.contains("---") && !src.contains("...") {
            continue;
        }
        let mut rng = Rng(((seed ^ (ci as u64).wrapping_mul(0x51_7cc1)) << 1) | 1);
        let src = with_comments(&mut rng, &src);
        let s = match Stream::parse(src.clone()) {
            Ok(s) => s,
            Err(_) => Stream::parse_recovering(src).into_value(),
        };
        run_stream(&format!("stream {}", case.id), s, &mut rng, steps, &mut t);
    }
    for (si, src) in streams.iter().enumerate() {
        for round in 0..20usize {
            if (si * 20 + round) % parts != part {
                continue;
            }
            let mut rng = Rng(((seed ^ ((si * 50 + round) as u64) << 28) << 1) | 1);
            let s = Stream::parse(*src).unwrap();
            run_stream(&format!("stream {si}/{round}"), s, &mut rng, steps, &mut t);
        }
    }
    eprintln!("{t:#?}");
    if parts == 1 {
        assert!(t.stream_ok > 300, "{t:?}");
        assert!(t.node_edits > 100, "{t:?}");
        assert!(t.ok > 2_000, "{t:?}");
        assert!(t.new_comments > 1_000, "{t:?}");
        assert!(t.removed > 200, "{t:?}");
        assert!(t.refused > 200, "{t:?}");
    }
}
