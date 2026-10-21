//! [`Document`] and [`Stream`]: the lossless view of YAML, and its edit
//! API (`edit.rs`).
//!
//! A `Document` owns the syntax tree the parser's events built, so
//! an untouched document emits its input byte for byte, and its
//! meaning is read through the **same** construction layer and resolver as
//! serde: [`Value::try_from`] replays the tree as the parser's events
//! (`construct::tree`) into the deserializer's own entry point.
//!
//! Nothing here recurses on input: navigation is index loops over
//! the arena, emitting is a flat token loop, and the value is built by the
//! iterative builder.
//!
//! # `std`-only shared state
//!
//! The public data structures are `no_std`-ready, but the shared
//! state of a parse (`Shared`, behind an `Arc`) and of a `Document` uses
//! `std` synchronisation for its caches, so that reads through `&self`
//! can build them and a `Document` stays `Send + Sync`. Not changed yet;
//! what is `std`-only:
//!
//! * `std::sync::OnceLock`: `Shared::anchors`, `Shared::lines`,
//!   `Shared::node_events`, `Shared::cap_view`, the `%TAG` table of
//!   `tags::DocTags`;
//! * `std::sync::Mutex` (with `PoisonError` handling):
//!   `Shared::counts`, `Shared::cap_index` and `Shared::lookups`;
//! * `std::collections::HashMap`/`HashSet` with `RandomState` in the
//!   caches (`lookup.rs`, `anchors.rs`; `carry.rs`'s `IdMap`), and the
//!   per-process keys of `oneyaml-syntax`'s diagnostic fingerprints
//!   (a `OnceLock<RandomState>`);
//! * `std::sync::atomic` counters (`Document` identities,
//!   `Document::error_neighbour`): these are in `core` already.
//!
//! A later `no_std` + `alloc` flag (additive) would need: `Arc` from `alloc`
//! (already there); `core::cell::OnceCell` for the `OnceLock`s and a `RefCell`
//! (or a spin lock) for the `Mutex`es, which makes `Document` `!Sync` under
//! that flag, or a `critical-section`-style lock behind the flag (a new
//! dependency, which needs its own case); `BTreeMap`/ `BTreeSet` in place of
//! the hash maps and sets (the project uses no third-party hasher crates, and a
//! `no_std` `RandomState` needs a seed source), with the lookup index's O(1)
//! digest table becoming O(log n); and a seed for the treap priorities and
//! fingerprint keys from the caller or a fixed key (hash flooding resistance
//! then rests on the treap and the `BTreeMap`s, never on secrecy).

use std::sync::{Arc, OnceLock};

use oneyaml_core::Encoding;
use oneyaml_core::{Code, Diagnostic, LineIndex, Path, Settings, Severity, Span, code};

use crate::diag_cap::Dropped;
use oneyaml_syntax::decode::{InvalidRun, InvalidRuns};
use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind, SyntaxTree};

#[cfg(test)]
use crate::construct::Node as Ev;
use crate::construct::Source;
use crate::construct::settings::read_diagnostics;
use crate::construct::tree::TreeSource;
use crate::error::Error;
use crate::value::Value;

mod anchors;
mod cap_index;
mod carry;
mod comment_edit;
mod comments;
mod edit;
mod error;
mod key;
mod lookup;
mod patch;
mod path_arg;
mod place;
mod render;
mod stream;
pub(crate) mod tags;
mod view;

pub use comments::{
    Comment, CommentExplanation, CommentId, CommentOwner, CommentRule, Comments, NO_COMMENTS,
};
pub use edit::{Entry, NodeMut, ReinsertError, Removed};
pub use error::EditError;
pub use key::AsKey;
pub use patch::{MergePatch, Patch, PatchError, PatchErrorKind, PatchOp, Pointer};
pub use path_arg::IntoPath;
pub use stream::{DocumentMut, Stream};
pub use view::{MappingRef, NodeKind, NodeRef, ScalarRef, ScalarStyle, SequenceRef};

/// How the input was encoded, to give its bytes back.
#[derive(Clone, Debug)]
struct Origin {
    /// Always a codec this version has (writing never fails).
    encoder: oneyaml_syntax::Encoder,
    /// Compact (offsets and one pool of the bytes).
    invalid: InvalidRuns,
}

/// A syntax-tree handle (never in a public signature).
type TreeId = oneyaml_syntax::tree::NodeId;

/// How a parse checked its input: what [`Shared::fresh_diagnostics`]
/// repeats after edits (the same checks as parsing the edited text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// `parse`: the first error fails; `single`: one document.
    Strict { single: bool },
    /// `parse_recovering`: every error is a diagnostic.
    Recovering { single: bool },
}

/// What the documents of one parse share (its `std`-only caches: see the
/// module docs).
struct Shared {
    tree: SyntaxTree,
    settings: Settings,
    /// Decoding diagnostics (in source order), then settings diagnostics,
    /// then the tree's and the meaning checks', in source order.
    diagnostics: Vec<Diagnostic>,
    /// How many of `diagnostics` come from decoding (the first ones).
    decoded: usize,
    /// How many of `diagnostics` come before any event (decoding,
    /// settings).
    pre: usize,
    /// Each document's node, in order. A document's span is read from its
    /// node on demand (O(log R): never rebuilt per
    /// edit); the list itself changes only when a document is added (the
    /// first document of an empty input).
    doc_ids: Vec<TreeId>,
    /// How many documents (from the first) the stream-wide meaning checks
    /// of a recovering parse reach: they stop at the first syntax error
    /// (in the document holding it). Each document's
    /// meaning is then checked on its own, O(own region). Accepted edits
    /// inside a document never create, remove, or move a syntax error:
    /// the edited tree's error set is its re-parse's, and
    /// the syntax layer refuses to remove an error node;
    /// restricted mode refuses edits next to one. As a second line
    /// of defence, it is recomputed whenever the tree's error count
    /// changes (`errors`, checked after every edit in O(1)).
    checked: usize,
    /// The tree's error count `checked` was computed for.
    errors: usize,
    /// The indices (ascending) of the documents whose subtree holds an
    /// error node, whichever document owns its diagnostic (an
    /// unclosed `[` cut off by the next `---` is an error node of its own
    /// document, reported by the next one). Found with `checked`, and
    /// recomputed with it: accepted edits never create or remove an error
    /// node. Empty, with no allocation, for a clean tree.
    error_docs: Vec<usize>,
    origin: Origin,
    /// Anchors and aliases by name (`anchors.rs`): built once, on first
    /// use, then maintained by every edit.
    anchors: OnceLock<anchors::Index>,
    /// The line break new text uses: the document's own (LF,
    /// CRLF or CR, never a hard-coded `\n`).
    nl: &'static str,
    /// The source's line index, built on the first error located.
    lines: OnceLock<LineIndex>,
    /// The positions (preorder) of the elements the expander counts as own
    /// nodes, in order: a per-node read finds how many precede its node in its
    /// document in O(log n) (the per-node alias budget's prefix). Built on the
    /// first read that needs it, dropped by every edit. `u32`: a tree has at
    /// most `u32::MAX` elements (`Y2006`).
    node_events: OnceLock<Vec<u32>>,
    /// The resolved `%TAG`-prefixed tags, for node reads:
    /// built on the first read that needs them, kept across edits (they
    /// cannot change them, see `tags.rs`), rebuilt for a copy.
    tags: tags::DocTags,
    /// How the input was checked.
    mode: Mode,
    /// The tree was edited: `diagnostics` describe the source; diagnostics
    /// are then rebuilt from the tree on demand.
    edited: bool,
    /// The syntax diagnostics attached after the last element (see
    /// `SyntaxTree::diagnostics_by_element`), with their parse spans. Edits
    /// never add or remove them and their spans move with the end of the
    /// text, so the last document reads them from here in O(k) (never a
    /// rebuild of the stream's diagnostics).
    trailing: Vec<Diagnostic>,
    /// Every error of the parse (syntax, meaning; decoding ones
    /// are `origin.invalid`) as
    /// `(start, end, code)`, sorted: complete whatever `max_diagnostics`,
    /// so `has_errors`, restricted mode, and reads never depend on the
    /// cap (the cap bounds only the stored lists).
    error_marks: Vec<(usize, usize, Code)>,
    /// The settings diagnostics, complete (a few; listed by every
    /// document).
    settings_diags: Vec<Diagnostic>,
    /// What the collectors of the parse did not keep (decoding, the
    /// tree's skeletons), added to the meaning checks' when the cap is
    /// applied.
    dropped: Dropped,
    /// How many diagnostics the parse found in all, and whether its list
    /// was capped.
    found: usize,
    capped: bool,
    /// Locality: the diagnostic counts of the edited
    /// documents, so a document's `diagnostics()` after edits knows in
    /// O(edited documents since the last read) whether the stream's
    /// list reaches the cap, and stays O(document) when it does not.
    counts: std::sync::Mutex<Counts>,
    /// The edited stream's diagnostics under the cap, built on the first
    /// read that needs them, dropped by every edit, and shared with the
    /// counts' record of what it showed (`cap_index::Shown`), never copied.
    cap_view: OnceLock<Arc<[Diagnostic]>>,
    /// The documents' diagnostic counts, for
    /// `cap_view` in O(log d + what it shows) (`cap_index.rs`): built on
    /// the first capped read after an edit (O(stream), once), then kept
    /// by every edit (the edited document marked, O(log)); `None` for a
    /// stream that never needed it (no cost under the cap).
    /// (Boxed: 16 bytes in every parse.)
    cap_index: std::sync::Mutex<Option<Box<cap_index::CapIndex>>>,
    /// The lookup indexes of wide collections
    /// (`lookup.rs`), built by reads on first use, maintained by every
    /// edit (a `Mutex`: reads build them through `&self`).
    lookups: std::sync::Mutex<lookup::Indexes>,
}

impl core::fmt::Debug for Shared {
    /// The state, never the caches (lookup indexes, line index,
    /// tag table, …): deterministic and bounded.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Shared")
            .field("documents", &self.doc_ids.len())
            .field("span", &self.tree.root().span())
            .field("edited", &self.edited)
            .field("mode", &self.mode)
            .field("encoding", &self.origin.encoder.encoding())
            .finish_non_exhaustive()
    }
}

/// The diagnostic counts of edited documents (see `Shared::counts`).
#[derive(Clone, Debug, Default)]
struct Counts {
    /// Per document: its count before its first edit, and its current one.
    docs: std::collections::BTreeMap<usize, (usize, usize)>,
    /// Edited documents whose current count is stale.
    stale: std::collections::BTreeSet<usize>,
    /// A document was edited before its count was taken: counts unknown.
    unknown: bool,
    /// The sum of current minus before, over `docs`.
    delta: i128,
}

/// `d` with every span moved by `to - from` (a diagnostic after the last
/// element when the text's end moves from `from` to `to`; the syntax
/// layer's own shift for those).
///
/// **Source of truth:** `shift_diagnostic`/`shift_span` in
/// `oneyaml-syntax`'s `tree/arena.rs`, which are not public; this is a copy
/// of them and must move spans exactly as they do (`diagnostics()`
/// agrees with the tree's). Pinned by
/// `tests::trailing_warnings_move_as_the_syntax_layer_moves_them`.
fn moved_to_end(d: &Diagnostic, from: usize, to: usize) -> Diagnostic {
    let mv = |s: &mut oneyaml_core::Span| {
        let shift = |x: usize| {
            if to >= from {
                x.saturating_add(to.saturating_sub(from))
            } else {
                x.saturating_sub(from.saturating_sub(to))
            }
        };
        *s = oneyaml_core::Span::new(shift(s.start), shift(s.end));
    };
    let mut d = d.clone();
    mv(&mut d.primary.span);
    for l in &mut d.secondary {
        mv(&mut l.span);
    }
    for sg in &mut d.suggestions {
        for e in &mut sg.edits {
            mv(&mut e.span);
        }
    }
    d
}

/// The code of an invalid run in input decoded as `e`.
fn invalid_code(e: Encoding) -> Code {
    match e {
        Encoding::Utf16Le | Encoding::Utf16Be => code::INVALID_UTF16,
        Encoding::Utf32Le | Encoding::Utf32Be => code::INVALID_UTF32,
        _ => code::INVALID_UTF8,
    }
}

/// The line break a text uses: its first one (LF when it has none).
fn line_break_of(text: &str) -> &'static str {
    match text.find(['\n', '\r']) {
        Some(i) if text.get(i..).is_some_and(|t| t.starts_with("\r\n")) => "\r\n",
        Some(i) if text.get(i..).is_some_and(|t| t.starts_with('\r')) => "\r",
        _ => "\n",
    }
}

/// How many documents the stream-wide meaning checks of a recovering parse
/// reach (see `Shared::checked`): through the one holding the first syntax
/// error (they stop there), all of them without one.
fn checked_documents(tree: &SyntaxTree, docs: &[TreeId]) -> usize {
    let first_error = tree
        .diagnostics_by_element()
        .filter(|(_, d)| d.severity == Severity::Error)
        .map(|(n, _)| n.map_or(usize::MAX, TreeNode::preorder_index))
        .min();
    let Some(at) = first_error else {
        return docs.len();
    };
    docs.iter()
        .filter(|id| tree.get(**id).is_ok_and(|d| d.preorder_index() <= at))
        .count()
}

/// The indices of the documents whose subtree holds an error node (see
/// `Shared::error_docs`). O(1) for a tree without errors (every error node
/// carries an error diagnostic, so the tree's O(1) count rules them out);
/// otherwise one walk of the documents, O(n), as parsing (iterative).
fn documents_with_error_nodes(tree: &SyntaxTree, docs: &[TreeId]) -> Vec<usize> {
    if !tree.has_errors() {
        return Vec::new();
    }
    docs.iter()
        .enumerate()
        .filter(|(_, id)| {
            tree.get(**id)
                .is_ok_and(|d| d.descendants().any(|n| n.kind() == SyntaxKind::Error))
        })
        .map(|(i, _)| i)
        .collect()
}

impl Shared {
    /// A copy with its own tree, for a document that edits a tree it
    /// shares (copy-on-write). The tree is **forked**
    /// ([`SyntaxTree::fork`]): every handle issued before the copy resolves
    /// in both trees to the same element, with no translation map. O(n) for
    /// the tree, O(1) handle metadata; the
    /// anchor index is kept (its handles are the fork's too).
    fn copy(&self) -> Self {
        Self {
            tree: self.tree.fork(),
            settings: self.settings.clone(),
            diagnostics: self.diagnostics.clone(),
            decoded: self.decoded,
            pre: self.pre,
            doc_ids: self.doc_ids.clone(),
            checked: self.checked,
            errors: self.errors,
            error_docs: self.error_docs.clone(),
            origin: self.origin.clone(),
            anchors: self.anchors.clone(),
            lines: self.lines.clone(),
            // An edit follows the copy and would drop it (O(1) instead).
            node_events: OnceLock::new(),
            tags: tags::DocTags::new(),
            nl: self.nl,
            mode: self.mode,
            edited: self.edited,
            trailing: self.trailing.clone(),
            error_marks: self.error_marks.clone(),
            settings_diags: self.settings_diags.clone(),
            dropped: self.dropped,
            found: self.found,
            capped: self.capped,
            counts: std::sync::Mutex::new(self.counts.lock().map(|c| c.clone()).unwrap_or_else(
                |_| Counts {
                    unknown: true,
                    ..Counts::default()
                },
            )),
            cap_view: OnceLock::new(),
            // The same tree: the counts stay exact (O(d), the tree's
            // copy is O(n) anyway).
            cap_index: std::sync::Mutex::new(
                self.cap_index
                    .lock()
                    .ok()
                    .and_then(|ix| ix.as_ref().cloned()),
            ),
            // A fork keeps every handle: the indexes stay exact.
            lookups: std::sync::Mutex::new(
                self.lookups
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
        }
    }

    fn build(
        text: String,
        settings: &Settings,
        origin: Origin,
        mut diags: Vec<Diagnostic>,
        mut dropped: Dropped,
    ) -> Self {
        diags.sort_by_key(|d| d.primary.span.start);
        let decoded = diags.len();
        let settings_diags = match read_diagnostics(settings) {
            Ok(w) => w,
            Err(e) => vec![e.diagnostic().clone()],
        };
        diags.extend(settings_diags.iter().cloned());
        let tree = SyntaxTree::parse_with(text, settings);
        let pre = diags.len();
        // Every syntax error is marked (complete; decoding errors
        // are the invalid runs, complete too); the tree's skeletons are
        // counted, not listed.
        let mut error_marks: Vec<(usize, usize, Code)> = Vec::new();
        for d in tree.diagnostics() {
            if d.severity == Severity::Error {
                error_marks.push((d.primary.span.start, d.primary.span.end, d.code));
            }
            if SyntaxTree::is_skeleton(&d) {
                dropped = dropped.and(Dropped::one(d.primary.span.start));
            } else {
                diags.push(d.into_owned());
            }
        }
        if let Some(events) = diags.get_mut(pre..) {
            events.sort_by_key(|d| d.primary.span.start);
        }
        let doc_ids: Vec<TreeId> = tree
            .root()
            .children()
            .filter(|n| n.kind() == SyntaxKind::Document)
            .map(TreeNode::id)
            .collect();
        let checked = checked_documents(&tree, &doc_ids);
        let errors = tree.error_count();
        let error_docs = documents_with_error_nodes(&tree, &doc_ids);
        // Parsed tree: its borrowed list, in element order (O(d) once, as
        // `checked_documents`).
        let trailing = tree
            .diagnostics_by_element()
            .filter(|(n, _)| n.is_none())
            .map(|(_, d)| d.into_owned())
            .collect();
        let nl = line_break_of(tree.source());
        Self {
            tree,
            settings: settings.clone(),
            diagnostics: diags,
            decoded,
            pre,
            doc_ids,
            checked,
            errors,
            error_docs,
            origin,
            anchors: OnceLock::new(),
            lines: OnceLock::new(),
            node_events: OnceLock::new(),
            tags: tags::DocTags::new(),
            nl,
            mode: Mode::Strict { single: true },
            edited: false,
            trailing,
            error_marks,
            settings_diags,
            dropped,
            found: 0,
            capped: false,
            counts: std::sync::Mutex::new(Counts::default()),
            cap_view: OnceLock::new(),
            cap_index: std::sync::Mutex::new(None),
            lookups: std::sync::Mutex::new(lookup::Indexes::default()),
        }
    }

    fn from_str(text: String, settings: &Settings) -> Self {
        let origin = Origin {
            encoder: oneyaml_syntax::Encoder::default(),
            invalid: InvalidRuns::default(),
        };
        Self::build(text, settings, origin, Vec::new(), Dropped::default())
    }

    /// A strict read of `bytes`, decoded once: at the
    /// first invalid run decoding stops and that run's error is returned,
    /// exactly as [`Shared::pre_error`] reports it from a whole decode (its
    /// location only reads the text before it: nothing else
    /// is held).
    fn from_slice_strict(bytes: &[u8], settings: &Settings) -> Result<Self, Error> {
        match oneyaml_syntax::decode_strict(bytes) {
            Ok(decoded) => Ok(Self::from_decoded(decoded, settings)),
            Err(e) => {
                let (d, text) = *e;
                Err(Error::from_diagnostic(d).finish(Some(&text), Vec::new()))
            }
        }
    }

    fn from_slice(bytes: &[u8], settings: &Settings) -> Self {
        // Decoding builds at most `max_diagnostics + 1`
        // diagnostics; the runs are always complete.
        let keep = settings
            .get_limits()
            .get_max_diagnostics()
            .saturating_add(1);
        Self::from_decoded(oneyaml_syntax::decode_with_limit(bytes, keep), settings)
    }

    fn from_decoded(decoded: oneyaml_syntax::Decoded<'_>, settings: &Settings) -> Self {
        let (diags, invalid, encoder, text) = decoded.into_parts();
        let first_dropped = invalid.get(diags.len()).map(|r| r.text.start);
        let dropped = Dropped {
            count: invalid.len().saturating_sub(diags.len()),
            first: first_dropped,
        };
        let origin = Origin { encoder, invalid };
        Self::build(text.into_owned(), settings, origin, diags, dropped)
    }

    /// The current text: the source, or the edited tree's text (O(n)).
    fn text(&self) -> std::borrow::Cow<'_, str> {
        if self.edited {
            std::borrow::Cow::Owned(self.tree.emit())
        } else {
            std::borrow::Cow::Borrowed(self.tree.source())
        }
    }

    /// After an accepted edit: the line index is dropped. O(1): the
    /// documents' spans are read from their nodes on
    /// demand, and the anchor index was maintained by the edit.
    fn after_edit(&mut self) {
        self.edited = true;
        self.lines = OnceLock::new();
        self.cap_view = OnceLock::new();
        self.node_events = OnceLock::new();
        // (`tags` stays valid: see `tags.rs`.)
        // The meaning checks' reach follows the
        // syntax errors; O(1) unless they changed.
        if self.tree.error_count() != self.errors {
            self.checked = checked_documents(&self.tree, &self.doc_ids);
            self.errors = self.tree.error_count();
            self.error_docs = documents_with_error_nodes(&self.tree, &self.doc_ids);
            // (The meaning checks' reach moved: every count may change.)
            self.drop_cap_index();
        }
    }

    /// The documents' diagnostic counts are not known any more
    /// (rebuilt on the next capped read).
    fn drop_cap_index(&mut self) {
        self.cap_index = std::sync::Mutex::new(None);
    }

    /// After a document was added (the first of an empty input): the
    /// document list is read again (O(documents), once).
    fn documents_changed(&mut self) {
        self.cap_view = OnceLock::new();
        self.drop_cap_index();
        self.doc_ids = self.documents().map(TreeNode::id).collect();
        self.checked = checked_documents(&self.tree, &self.doc_ids);
        self.errors = self.tree.error_count();
        self.error_docs = documents_with_error_nodes(&self.tree, &self.doc_ids);
    }

    /// Document `i`'s span, read from its node (O(log R)).
    fn doc_span(&self, i: usize) -> Option<Span> {
        let id = self.doc_ids.get(i)?;
        self.tree.get(*id).ok().map(TreeNode::span)
    }

    /// The text region of document node `d`: the bytes
    /// `Document::to_string` writes for a document of a stream.
    ///
    /// The documents' regions partition the stream's text, consecutive and
    /// in order, and each holds exactly the comments the rulebook gives its
    /// document (`docs/comment-attachment.md` section 4, which already
    /// partitions the comments): everything before the first document is
    /// the first document's, everything after the last the last one's, and
    /// the stretch of stream-level text between two documents is split at
    /// a line start ([`Shared::split_between`]). O(the document and the
    /// stretches around it), never the rest of the stream.
    fn region_of(&self, d: TreeNode<'_>) -> Span {
        let is_doc = |n: &TreeNode<'_>| n.kind() == SyntaxKind::Document;
        let mut prev = d.prev_sibling();
        while let Some(p) = prev.filter(|p| !is_doc(p)) {
            prev = p.prev_sibling();
        }
        let mut next = d.next_sibling();
        while let Some(n) = next.filter(|n| !is_doc(n)) {
            next = n.next_sibling();
        }
        let root = self.tree.root().span();
        let start = prev.map_or(root.start, |p| self.split_between(p, d));
        let end = next.map_or(root.end, |n| self.split_between(d, n));
        Span::new(start, end.max(start))
    }

    /// Where the stream-level text between documents `prev` and `next` is
    /// split: right after the line holding the last thing
    /// `prev` owns there (its last content line, and its trailing and
    /// footer comments), and never after the first thing `next` owns
    /// (its header comments, a stray `...` before it). The blank lines in
    /// between go with `next` (rule 8: blank lines travel with what
    /// follows them). Owners are the rulebook's, decided for the stretch
    /// in one pass (`comment_attachments_between`, O(the stretch)).
    fn split_between(&self, prev: TreeNode<'_>, next: TreeNode<'_>) -> usize {
        let tree = &self.tree;
        let mine: std::collections::BTreeSet<TreeId> = tree
            .comment_attachments_between(Some(prev), Some(next))
            .into_iter()
            .filter(|(_, a)| a.document() == Some(prev))
            .map(|(c, _)| c.id())
            .collect();
        let mut pos = prev.span().end;
        // Still on `prev`'s last line: its last token with bytes does not
        // end the line (a block scalar's text ends with its line break).
        let mut last = comments::last_token(prev);
        while let Some(t) = last.filter(|t| t.span().is_empty()) {
            last = place::prev_token(t);
        }
        let mut on_line = last.is_some_and(|t| !comments::ends_line(t));
        let mut first_next = None;
        let mut at = prev.next_sibling();
        // Loops over the stretch's elements and their tokens.
        'stretch: while let Some(top) = at.filter(|t| *t != next) {
            for t in top.descendants().filter(|t| t.is_token()) {
                if t.kind() == SyntaxKind::Comment {
                    if mine.contains(&t.id()) {
                        pos = t.span().end;
                        on_line = true;
                    } else {
                        first_next = Some(t.span().start);
                        break 'stretch;
                    }
                } else if comments::ends_line(t) {
                    if on_line {
                        pos = t.span().end;
                        on_line = false;
                    }
                } else if comments::visible(t) {
                    first_next = Some(t.span().start);
                    break 'stretch;
                }
            }
            at = top.next_sibling();
        }
        let pos = pos.min(next.span().start);
        first_next.map_or(pos, |f| pos.min(f))
    }

    /// The anchor index (built on first use, O(n) once).
    fn anchor_index(&self) -> &anchors::Index {
        self.anchors
            .get_or_init(|| anchors::Index::build(&self.tree))
    }

    /// The diagnostics of the edited tree, built exactly as parsing its
    /// text builds them: the settings diagnostics, the tree's (the
    /// syntax layer moves them with the edits), and the meaning checks of
    /// the parse mode, in source order. O(n); only edited documents use it.
    ///
    /// Under the diagnostics cap, exactly as parsing the edited
    /// text caps them: when the parse itself dropped syntax diagnostics
    /// (which the tree then lacks), the edited text is parsed again
    /// (O(n), only for such input).
    fn fresh_diagnostics(&self) -> Vec<Diagnostic> {
        if self.tree.suppressed_diagnostics() > 0 {
            return self.reparsed().diagnostics;
        }
        let mut out: Vec<Diagnostic> = self
            .diagnostics
            .get(..self.pre)
            .unwrap_or_default()
            .to_vec();
        let pre = out.len();
        out.extend(self.tree.diagnostics().map(std::borrow::Cow::into_owned));
        let found = self.meaning();
        out.extend(found.list);
        if let Some(events) = out.get_mut(pre..) {
            events.sort_by_key(|d| d.primary.span.start);
        }
        crate::diag_cap::cap(&mut out, found.dropped, self.limit());
        out
    }

    /// `Limits::max_diagnostics`.
    fn limit(&self) -> usize {
        self.settings.get_limits().get_max_diagnostics()
    }

    /// The meaning checks of the whole tree in the parse mode, as a parse
    /// of its text runs them: the kept diagnostics (in no particular
    /// order) and how many more were found.
    fn meaning(&self) -> Found {
        let mut events = TreeSource::new(&self.tree);
        let mut out = Vec::new();
        let found = match self.mode {
            Mode::Strict { single } => {
                let found = crate::de::check_unexpanded(&mut events, &self.settings, single, false);
                out.extend(found.errors.first().map(|e| e.diagnostic().clone()));
                found
            }
            Mode::Recovering { single } => {
                let found = crate::de::check_unexpanded(&mut events, &self.settings, false, true);
                out.extend(found.errors.iter().map(|e| e.diagnostic().clone()));
                if single {
                    out.extend(self.multiple_documents());
                }
                found
            }
        };
        out.extend(found.warnings);
        Found {
            list: out,
            dropped: found.suppressed,
            dropped_errors: found.dropped_errors,
        }
    }

    /// The current text parsed again, with the same settings and checks:
    /// its diagnostics are those of a parse of the edited text, so both
    /// readers agree. O(n).
    fn reparsed(&self) -> Shared {
        #[cfg(test)]
        tests::count_reparse();
        let mut s = Shared::from_str(self.text().into_owned(), &self.settings);
        s.mode = self.mode;
        let found = s.meaning();
        s.add_diagnostics(found);
        s
    }

    /// Whether the edited stream's diagnostics reach the cap: the parse's
    /// did, or the parse's count plus what edits changed exceeds it
    /// (exact, from the edited documents' counts, so a stream
    /// with few diagnostics never pays O(stream)). Otherwise nothing is
    /// capped, and a document reads its own diagnostics locally.
    /// O(the documents edited since the last read).
    fn cap_possible(&self) -> bool {
        if self.capped || self.tree.suppressed_diagnostics() > 0 {
            return true;
        }
        let Ok(mut c) = self.counts.lock() else {
            return true;
        };
        let stale = std::mem::take(&mut c.stale);
        for j in stale {
            let now = self.complete_count(j);
            if let Some(e) = c.docs.get_mut(&j) {
                let was = e.1;
                e.1 = now;
                c.delta = c
                    .delta
                    .saturating_add(i128::try_from(now).unwrap_or(i128::MAX))
                    .saturating_sub(i128::try_from(was).unwrap_or(i128::MAX));
            } else {
                c.unknown = true;
            }
        }
        if c.unknown {
            return true;
        }
        let total = i128::try_from(self.found)
            .unwrap_or(i128::MAX)
            .saturating_add(c.delta);
        total > i128::try_from(self.limit()).unwrap_or(i128::MAX)
    }

    /// How many diagnostics document `i` has now, kept or not.
    /// O(document).
    fn complete_count(&self, i: usize) -> usize {
        let (list, dropped) = self.edited_doc_events(i);
        list.len().saturating_add(dropped.count)
    }

    /// Before the first edit of document `i` (its restricted-mode guard is
    /// found then): records its count.
    ///
    /// The first edit is local too: while the
    /// stream is unedited and its parse kept every diagnostic, the count
    /// is the parse's own list for the document's region (what
    /// [`Document::diagnostics`] of the untouched document shows, the
    /// settings ones aside), two binary searches: O(log n). Otherwise (the
    /// stream was edited, so the list's spans are the source's, or the
    /// parse was capped) its whole list is read, O(document), as before.
    fn record_count(&self, i: usize) {
        if let Ok(mut c) = self.counts.lock() {
            c.docs.entry(i).or_insert_with(|| {
                let n = self
                    .parsed_count(i)
                    .unwrap_or_else(|| self.complete_count(i));
                (n, n)
            });
        }
    }

    /// [`Shared::complete_count`] of document `i` read from the parse's
    /// list (see [`Shared::record_count`]); `None` when that list cannot
    /// give it: the stream was edited, the parse was capped or decoded
    /// invalid input (an edited tree has no decoding diagnostics).
    fn parsed_count(&self, i: usize) -> Option<usize> {
        if self.edited
            || self.capped
            || self.decoded > 0
            || self.tree.suppressed_diagnostics() > 0
            || self.dropped.count > 0
        {
            return None;
        }
        let events = self.diagnostics.get(self.pre..)?;
        Some(self.owned(events, i).len())
    }

    /// After an accepted edit of document `i`: its count is stale.
    fn mark_edited(&mut self, i: usize) {
        if let Ok(c) = self.counts.get_mut() {
            if c.docs.contains_key(&i) {
                c.stale.insert(i);
            } else {
                c.unknown = true;
            }
        }
        // Its counts are read again on the next capped read.
        let poisoned = match self.cap_index.get_mut() {
            Ok(Some(ix)) => {
                ix.mark(i);
                false
            }
            Ok(None) => false,
            Err(_) => true,
        };
        if poisoned {
            self.drop_cap_index();
        }
    }

    /// The edited stream's diagnostics under the cap: built
    /// once per edit state, as a parse of the edited text would cap them.
    /// From the documents' counts (`cap_index.rs`), in
    /// O(log d + the documents holding what it shows) once the counts are
    /// built (O(stream), on the first such read); by reading every
    /// document only when they cannot be had.
    ///
    /// When the parse kept some syntax diagnostics as skeletons (no
    /// texts), the list is still read from the counts: a parse of the
    /// edited text keeps the texts of the first `max_diagnostics + 1`
    /// syntax diagnostics (in source order) and of the first error, which
    /// is all it lists of them, and those are what the counted list shows.
    /// A shown skeleton takes its texts from its document's text read
    /// again on its own ([`Shared::doc_head_filled`]); only when
    /// one cannot be filled so (or the counts cannot be had) is the
    /// edited text parsed again, O(n), as before.
    ///
    /// A stream of one document parses its whole text
    /// again at once: its region is the whole text,
    /// so reading it again for skeleton texts costs that parse anyway,
    /// and the counts would only add their own reads to it.
    fn capped_view(&self) -> &[Diagnostic] {
        self.cap_view.get_or_init(|| {
            if self.tree.suppressed_diagnostics() > 0 {
                if self.doc_ids.len() <= 1 {
                    return self.reparsed().diagnostics.into();
                }
                return self.capped_indexed().unwrap_or_else(|| {
                    #[cfg(test)]
                    tests::count_fallback();
                    self.reparsed().diagnostics.into()
                });
            }
            self.capped_indexed()
                .unwrap_or_else(|| self.capped_scan().into())
        })
    }

    /// [`Shared::capped_view`] read from every document (O(stream)): the
    /// reference the counted read equals.
    fn capped_scan(&self) -> Vec<Diagnostic> {
        let keep = self.limit().saturating_add(1);
        let mut out: Vec<Diagnostic> = self
            .diagnostics
            .get(..self.pre)
            .unwrap_or_default()
            .to_vec();
        let mut extra = Dropped::default();
        let mut error_kept = out.iter().any(|d| d.severity == Severity::Error);
        for i in 0..self.doc_ids.len() {
            let (own, dropped) = self.edited_doc_events(i);
            extra = extra.and(dropped);
            for d in own {
                let error = d.severity == Severity::Error;
                if out.len() < keep || (error && !error_kept) {
                    error_kept |= error;
                    out.push(d);
                } else {
                    extra = extra.and(Dropped::one(d.primary.span.start));
                }
            }
        }
        crate::diag_cap::cap(&mut out, extra, self.limit());
        out
    }

    /// [`Shared::capped_scan`]'s list, from the documents' counts (see
    /// `cap_index.rs`): the pre-event diagnostics, then the first `keep` of the
    /// documents' (read from the documents holding them, found by count), then,
    /// when none of those is an error, the first error after them (read from
    /// the first document holding one after them), with every other diagnostic
    /// counted; then the cap, exactly as the scan applies it. O(log d + those
    /// documents), after the refresh of the documents edited since the last
    /// read; a document the last list showed and no edit touched is taken from
    /// that list (moved to where its region is now), not read
    /// (`cap_index::Shown`). `None` when the counts cannot be had, or (past the
    /// cap on syntax diagnostics) a shown skeleton could not be filled: the
    /// caller scans or parses the edited text again.
    ///
    /// Defensively (`debug_assert!`), a document whose list does not match
    /// its count, or the counts pointing into a document's middle, also
    /// give `None`: in release the caller then runs the reference
    /// algorithm (the scan of every document, or the re-parse), so the
    /// list is still exact and no `Y9000` is needed.
    fn capped_indexed(&self) -> Option<Arc<[Diagnostic]>> {
        let limit = self.limit();
        let keep = limit.saturating_add(1);
        // The meaning checks the counts read, kept for the lists below
        // (never checked twice per read).
        let memo = Memo::new(self, keep);
        let mut slot = self.cap_index.lock().ok()?;
        if slot.is_none() {
            *slot = cap_index::CapIndex::build(self.doc_ids.len(), &memo).map(Box::new);
        }
        let ix = slot.as_mut()?;
        if !ix.refresh(&memo) {
            *slot = None;
            return None;
        }
        // What the last list showed (its stale documents dropped by the
        // refresh); replaced by what this one shows.
        let prev = std::mem::take(&mut ix.shown);
        let ix: &cap_index::CapIndex = ix;
        let is_error = |d: &Diagnostic| d.severity == Severity::Error;
        let mut out: Vec<Diagnostic> = self
            .diagnostics
            .get(..self.pre)
            .unwrap_or_default()
            .to_vec();
        let mut error_kept = out.iter().any(is_error);
        let total = ix.total();
        // Document `j`'s first `need` diagnostics (and its first error
        // when `want_error`), its count checked (with the texts of the
        // skeletons among them, when the parse kept some): never
        // its whole list, and from the last
        // list when it showed them.
        let skeletons = self.tree.suppressed_diagnostics() > 0;
        let events = |j: usize, need: usize, want_error: bool| {
            if let Some(head) = self.seen_head(&prev, ix, j, need, want_error) {
                return Some(head);
            }
            let meaning = match memo.take(j) {
                Some((m, _)) => m,
                None if ix.meaning_empty(j) => Vec::new(),
                None => self.doc_meaning_of(j).0,
            };
            let head = if skeletons {
                self.doc_head_filled(j, &meaning, need)
            } else {
                self.doc_head(j, &meaning, &[], need)
            };
            let ok = head.n == ix.leaf_n(j);
            debug_assert!(ok, "document {j}: its diagnostics do not match its count");
            ok.then_some(head)
        };
        let push = keep.saturating_sub(out.len()).min(total.n);
        let mut taken = 0usize;
        // What each document read put in `out`.
        let mut seen: Vec<Placed> = Vec::new();
        // The first document not read, and the last one read when it has
        // more than was listed (its first error, if known).
        let mut next = 0usize;
        let mut rest: Option<(usize, Option<Option<Diagnostic>>)> = None;
        while taken < push {
            let (j, before) = ix.nth_diagnostic(taken)?;
            if before != 0 {
                debug_assert!(false, "document {j} read from its middle");
                return None;
            }
            let want = push.saturating_sub(taken);
            let head = events(j, want, false)?;
            let n = want.min(head.n);
            let at = out.len();
            let error = head.list.iter().take(n).position(is_error);
            for d in head.list.into_iter().take(n) {
                error_kept |= is_error(&d);
                out.push(d);
            }
            seen.push(Placed {
                doc: j,
                at,
                len: n,
                error: error.map(|e| at.saturating_add(e)),
            });
            // (When its first error was listed, `error_kept` is set.)
            rest = (head.n > n).then(|| (seen.len().saturating_sub(1), head.first_error));
            taken = taken.saturating_add(n);
            next = j.saturating_add(1);
        }
        let mut error_pushed = false;
        if !error_kept && total.errors > 0 {
            let mut error = None;
            let mut placed = None;
            if let Some((k, known)) = rest {
                let doc = seen.get(k).map_or(usize::MAX, |p| p.doc);
                error = match known {
                    Some(e) => e,
                    None => events(doc, 0, true)?.first_error.flatten(),
                };
                placed = Some(k);
            }
            if error.is_none() {
                if let Some(j) = ix.first_error_from(next) {
                    error = events(j, 0, true)?.first_error.flatten();
                    seen.push(Placed {
                        doc: j,
                        at: out.len(),
                        len: 0,
                        error: None,
                    });
                    placed = Some(seen.len().saturating_sub(1));
                }
            }
            if let Some(e) = error {
                if let Some(p) = placed.and_then(|k| seen.get_mut(k)) {
                    p.error = Some(out.len());
                }
                out.push(e);
                error_pushed = true;
            }
        }
        // Every diagnostic not listed is counted, with the least start
        // the meaning checks did not keep. (The scan also takes the least
        // start of the unlisted diagnostics; it only places the summary
        // when the list is no longer than the limit, which it never is
        // with diagnostics left unlisted: the first `keep` were listed.)
        let unlisted = total
            .n
            .saturating_sub(push)
            .saturating_sub(usize::from(error_pushed));
        let first = total
            .dropped
            .first
            .or_else(|| (unlisted > 0).then(|| out.last().map(|d| d.primary.span.start))?);
        let extra = Dropped {
            count: total.dropped.count.saturating_add(unlisted),
            first,
        };
        let at_limit = out.get(limit).cloned();
        let cut = crate::diag_cap::cap(&mut out, extra, limit);
        if skeletons && out.iter().any(SyntaxTree::is_skeleton) {
            // (A shown skeleton not filled: nothing recorded.)
            return None;
        }
        let list: Arc<[Diagnostic]> = out.into();
        let shown = self.shown_record(&list, &seen, cut, at_limit);
        drop(prev);
        if let Some(ix) = slot.as_mut() {
            ix.shown = shown;
        }
        Some(list)
    }

    /// What `list` (the capped list just built from `seen`) shows of each
    /// document, for the next read (`cap_index::Shown`): where
    /// [`crate::diag_cap::cap`] left each document's entries (`list[..limit]`
    /// stay, `list[limit]` became the summary and is kept aside as the
    /// spare, the first error after it moved behind the summary).
    fn shown_record(
        &self,
        list: &Arc<[Diagnostic]>,
        seen: &[Placed],
        cut: Option<crate::diag_cap::Cut>,
        at_limit: Option<Diagnostic>,
    ) -> cap_index::Shown {
        let place = |p: usize| match cut {
            None => Some(p),
            Some(c) if p < c.limit => Some(p),
            Some(c) if c.error == Some(p) => c.limit.checked_add(1),
            Some(_) => None,
        };
        let mut docs = std::collections::BTreeMap::new();
        for p in seen {
            let end = p.at.saturating_add(p.len);
            let (len, spare) = match cut {
                None => (p.len, false),
                Some(c) if end <= c.limit => (p.len, false),
                Some(c) if p.at <= c.limit && end == c.limit.saturating_add(1) => {
                    (c.limit.saturating_sub(p.at), true)
                }
                // (Not a place the cap leaves: not recorded.)
                Some(_) => continue,
            };
            let error = match p.error.and_then(place) {
                Some(at) => cap_index::ErrorAt::At(at),
                None => cap_index::ErrorAt::Unknown,
            };
            docs.insert(
                p.doc,
                cap_index::Seen {
                    start: self.region_start(p.doc),
                    at: p.at,
                    len,
                    spare,
                    error,
                },
            );
        }
        cap_index::Shown {
            list: Some(Arc::clone(list)),
            spare: cut.and(at_limit),
            docs,
        }
    }

    /// Document `j`'s first `need` diagnostics (and its first error when
    /// `want_error`) as the last capped list showed them, moved to where
    /// its region starts now; `None` when that list did not show as many
    /// (or the error), or `j` was edited since (a read
    /// never reads an untouched document whose entries it already showed).
    /// O(log d + what it takes).
    fn seen_head(
        &self,
        prev: &cap_index::Shown,
        ix: &cap_index::CapIndex,
        j: usize,
        need: usize,
        want_error: bool,
    ) -> Option<Head> {
        let seen = prev.docs.get(&j)?;
        let list = prev.list.as_deref()?;
        let n = ix.leaf_n(j);
        let take = need.min(n);
        let avail = seen.len.saturating_add(usize::from(seen.spare));
        if avail < take {
            return None;
        }
        let now = self.region_start(j);
        let moved = |d: &Diagnostic| {
            if now == seen.start {
                d.clone()
            } else {
                moved_to_end(d, seen.start, now)
            }
        };
        let error = if ix.leaf_errors(j) == 0 {
            Some(None)
        } else {
            match seen.error {
                cap_index::ErrorAt::At(at) => Some(Some(moved(list.get(at)?))),
                cap_index::ErrorAt::Unknown if want_error => return None,
                cap_index::ErrorAt::Unknown => None,
            }
        };
        let own = list.get(seen.at..seen.at.checked_add(seen.len)?)?;
        let mut out: Vec<Diagnostic> = own.iter().take(take).map(moved).collect();
        if out.len() < take {
            out.push(moved(prev.spare.as_ref()?));
        }
        Some(Head {
            list: out,
            first_error: error,
            n,
        })
    }

    /// Document `i`'s part of [`Shared::capped_view`]: the settings ones,
    /// then those its region owns.
    fn capped_doc(&self, i: usize) -> Vec<Diagnostic> {
        let all = self.capped_view();
        let pre = self.pre.min(all.len());
        let (settings, events) = all.split_at(pre);
        let mut out = settings
            .get(self.decoded.min(pre)..)
            .unwrap_or_default()
            .to_vec();
        out.extend_from_slice(self.owned(events, i));
        out
    }

    /// The meaning checks of document `i` (node `doc`), read on its own in the
    /// parse mode: its warnings, then its first error (strict) or every error
    /// (recovering; a meaning error skips the rest of the document, checking
    /// stops at its first syntax error). None for a document past the first
    /// syntax error of a recovering stream, which the stream-wide check never
    /// reaches (`checked`). O(document), never the rest of the stream.
    fn doc_meaning(&self, i: usize, doc: TreeNode<'_>) -> (Vec<Diagnostic>, Dropped) {
        #[cfg(test)]
        tests::count_doc_read();
        if i >= self.checked {
            return (Vec::new(), Dropped::default());
        }
        let mut events = TreeSource::over(&self.tree, doc);
        let recover = matches!(self.mode, Mode::Recovering { .. });
        let found = crate::de::check_unexpanded(&mut events, &self.settings, false, recover);
        let mut out = found.warnings;
        if recover {
            out.extend(found.errors.iter().map(|e| e.diagnostic().clone()));
        } else {
            out.extend(found.errors.first().map(|e| e.diagnostic().clone()));
        }
        (out, found.suppressed)
    }

    /// The syntax layer's diagnostics document `i` owns (see
    /// [`Document::diagnostics`]), in element order. Read from the
    /// documents before and after it and the stream-level elements between
    /// them only (a diagnostic is attached to the element it describes or
    /// to the one that followed it, so where it is located is never past a
    /// neighbouring document), never from the rest of the stream.
    /// O(own region + the neighbours' diagnostics).
    fn region_tree_diagnostics(&self, i: usize) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        self.region_tree_each(i, &[], |d| out.push(d.into_owned()));
        out
    }

    /// How many of the syntax layer's diagnostics document `i` owns, and
    /// how many of them are errors ([`Shared::region_tree_diagnostics`]
    /// counted, never listed).
    ///
    /// `bounds`: where its region and the next one start, when known
    /// (the counts' build reads each boundary once).
    fn region_tree_counts(&self, i: usize, bounds: Option<(usize, usize)>) -> (usize, usize) {
        let (mut n, mut errors) = (0usize, 0usize);
        let bounds = bounds
            .unwrap_or_else(|| (self.region_start(i), self.region_start(i.saturating_add(1))));
        self.region_tree_each_within(i, bounds, &[], |d| {
            n = n.saturating_add(1);
            errors = errors.saturating_add(usize::from(d.severity == Severity::Error));
        });
        (n, errors)
    }

    /// Calls `f` with each of [`Shared::region_tree_diagnostics`], in
    /// order, each skeleton that a diagnostic of `fill` (sorted by start)
    /// records read as it (see
    /// `SyntaxTree::diagnostics_within_filled`).
    fn region_tree_each<'s>(
        &'s self,
        i: usize,
        fill: &'s [Diagnostic],
        f: impl FnMut(std::borrow::Cow<'s, Diagnostic>),
    ) {
        let bounds = (self.region_start(i), self.region_start(i.saturating_add(1)));
        self.region_tree_each_within(i, bounds, fill, f);
    }

    /// [`Shared::region_tree_each`] with the region `(from, to)`.
    fn region_tree_each_within<'s>(
        &'s self,
        i: usize,
        (from, to): (usize, usize),
        fill: &'s [Diagnostic],
        mut f: impl FnMut(std::borrow::Cow<'s, Diagnostic>),
    ) {
        #[cfg(test)]
        tests::count_doc_read();
        let owned = |d: &Diagnostic| from <= d.primary.span.start && d.primary.span.start < to;
        let tree = &self.tree;
        let first = match i.checked_sub(1).and_then(|p| self.doc_ids.get(p)) {
            Some(id) => tree.get(*id).ok(),
            None => tree.root().first_child(),
        };
        let stop = self.doc_ids.get(i.saturating_add(1)).copied();
        let mut at = first;
        while let Some(n) = at {
            tree.diagnostics_within_filled(n, fill)
                .map(|(_, d)| d)
                .filter(|d| owned(d))
                .for_each(&mut f);
            if Some(n.id()) == stop {
                break;
            }
            at = n.next_sibling();
        }
        if stop.is_none() {
            // Attached after the last element: the last document's,
            // read from the parse's list, spans moved
            // from the end of the source to the end of the edited text (as
            // the syntax layer moves them), O(k), never a rebuild of the
            // stream's diagnostics.
            let base = tree.source().len();
            let end = if self.edited {
                tree.root().span().end
            } else {
                base
            };
            self.trailing
                .iter()
                .map(|d| moved_to_end(d, base, end))
                .filter(|d| owned(d))
                .for_each(|d| f(std::borrow::Cow::Owned(d)));
        }
    }

    /// Document `i`'s diagnostics after edits, the settings ones aside: its
    /// own syntax diagnostics and meaning checks, in source order, as
    /// [`Shared::fresh_diagnostics`] orders them. O(own region).
    ///
    /// With how many more meaning diagnostics were found.
    ///
    /// Its errors are complete: the tree keeps every syntax
    /// error (past the cap, as a skeleton), and a document has at most one
    /// meaning error, which the collectors always keep.
    fn edited_doc_events(&self, i: usize) -> (Vec<Diagnostic>, Dropped) {
        let mut out = self.region_tree_diagnostics(i);
        let (meaning, suppressed) = self.doc_meaning_of(i);
        out.extend(meaning);
        out.sort_by_key(|d| d.primary.span.start);
        (out, suppressed)
    }

    /// [`Shared::doc_meaning`] of document `i` (none past the documents).
    fn doc_meaning_of(&self, i: usize) -> (Vec<Diagnostic>, Dropped) {
        match self.doc_ids.get(i).and_then(|id| self.tree.get(*id).ok()) {
            Some(doc) => self.doc_meaning(i, doc),
            None => (Vec::new(), Dropped::default()),
        }
    }

    /// What [`Shared::capped_indexed`] reads of document `i`'s list (as
    /// [`Shared::edited_doc_events`] orders it, with `meaning` its meaning
    /// checks): its first `need` diagnostics, its first error, and how
    /// many it lists, found in one pass over its region's diagnostics that
    /// keeps at most `need + 1` of them (never the whole
    /// list, which past the cap is one skeleton per diagnostic). Each
    /// skeleton a diagnostic of `fill` records is read as it. O(the region
    /// × log need).
    fn doc_head(&self, i: usize, meaning: &[Diagnostic], fill: &[Diagnostic], need: usize) -> Head {
        // The sort key of `edited_doc_events` (a stable sort by start of
        // the region's diagnostics, then the meaning checks'): the start,
        // then the place in that sequence.
        let mut heap: std::collections::BinaryHeap<Keyed> = std::collections::BinaryHeap::new();
        let mut error: Option<Keyed> = None;
        let mut seq = 0usize;
        let mut n = 0usize;
        let mut take = |d: std::borrow::Cow<'_, Diagnostic>| {
            let key = (d.primary.span.start, seq);
            seq = seq.saturating_add(1);
            n = n.saturating_add(1);
            let first_error =
                d.severity == Severity::Error && error.as_ref().is_none_or(|e| key < e.key);
            let in_head =
                need > 0 && (heap.len() < need || heap.peek().is_some_and(|top| key < top.key));
            if in_head {
                let d = d.into_owned();
                if first_error {
                    error = Some(Keyed { key, d: d.clone() });
                }
                if heap.len() >= need {
                    heap.pop();
                }
                heap.push(Keyed { key, d });
            } else if first_error {
                error = Some(Keyed {
                    key,
                    d: d.into_owned(),
                });
            }
        };
        self.region_tree_each(i, fill, &mut take);
        meaning
            .iter()
            .for_each(|d| take(std::borrow::Cow::Borrowed(d)));
        Head {
            list: heap.into_sorted_vec().into_iter().map(|k| k.d).collect(),
            first_error: Some(error.map(|k| k.d)),
            n,
        }
    }

    /// [`Shared::doc_head`] with the texts of the skeletons it shows
    /// (see `cap_index.rs`): when the parse kept some of the syntax layer's
    /// diagnostics as skeletons and one of the first `need`, or the first
    /// error, is one (only then; the others are counted, never
    /// shown), document `i`'s region text is read again on its own, parsed
    /// with the same settings **and the same cap** (the
    /// re-read builds texts for at most `max_diagnostics + 1` diagnostics
    /// and the first error, as any parse, never one per diagnostic of the
    /// region), and each skeleton one of them records (same code,
    /// severity, spans and fingerprint:
    /// `SyntaxTree::diagnostics_within_filled`) takes its texts. A
    /// skeleton none records stays one (the caller then parses the whole
    /// text again). O(the region).
    ///
    /// The cap is enough for what [`Shared::capped_indexed`] shows of a
    /// document: at most its first `max_diagnostics + 1` (in source
    /// order), whose syntax ones are among the first `max_diagnostics + 1`
    /// of its region's, and its first error; the region's own parse keeps
    /// the texts of exactly those (the earliest starting, and the first
    /// error) when it finds the region's diagnostics as the whole text
    /// does. Where it does not (a region read on its own can differ from
    /// the whole text), a shown skeleton stays unfilled and the caller
    /// falls back to parsing the whole edited text again (`capped_view`),
    /// so the list is exact either way.
    fn doc_head_filled(&self, i: usize, meaning: &[Diagnostic], need: usize) -> Head {
        let head = self.doc_head(i, meaning, &[], need);
        let shown = head.list.iter().any(SyntaxTree::is_skeleton)
            || head
                .first_error
                .as_ref()
                .and_then(Option::as_ref)
                .is_some_and(SyntaxTree::is_skeleton);
        if !shown {
            return head;
        }
        let Some((start, text)) = self.region_text(i) else {
            return head;
        };
        drop(head);
        #[cfg(test)]
        tests::count_fill();
        let again = SyntaxTree::parse_with(text, &self.settings);
        // (The re-read's own skeletons fill nothing: not copied.)
        let mut fill: Vec<Diagnostic> = again
            .diagnostics()
            .filter(|d| !SyntaxTree::is_skeleton(d))
            .map(|d| moved_to_end(&d, 0, start))
            .collect();
        drop(again);
        fill.sort_by_key(|d| d.primary.span.start);
        self.doc_head(i, meaning, &fill, need)
    }

    /// Document `i`'s region and its text (as `Document::to_string`
    /// writes it). O(the region and the stretches around it).
    fn region_text(&self, i: usize) -> Option<(usize, String)> {
        let d = self.doc_ids.get(i).and_then(|id| self.tree.get(*id).ok())?;
        let r = self.region_of(d);
        let is_doc = |n: &TreeNode<'_>| n.kind() == SyntaxKind::Document;
        let mut first = d;
        while let Some(p) = first.prev_sibling().filter(|p| !is_doc(p)) {
            first = p;
        }
        let mut text = String::new();
        let mut at = Some(first);
        while let Some(top) = at {
            if top != d && is_doc(&top) {
                break;
            }
            for n in top.descendants().filter(|n| n.is_token()) {
                let s = n.span();
                if r.start <= s.start && s.end <= r.end {
                    text.push_str(&n.text());
                }
            }
            at = top.next_sibling();
        }
        Some((r.start, text))
    }

    /// A stream's diagnostics after edits: the pre-event ones, then each
    /// document's ([`Shared::edited_doc_events`]; the regions are
    /// consecutive, so this is source order). O(stream).
    ///
    /// Under the diagnostics cap when it can apply (see
    /// [`Shared::capped_view`]).
    fn edited_stream_diagnostics(&self) -> Vec<Diagnostic> {
        if self.doc_ids.is_empty() {
            return self.fresh_diagnostics();
        }
        if self.cap_possible() {
            return self.capped_view().to_vec();
        }
        let mut out: Vec<Diagnostic> = self
            .diagnostics
            .get(..self.pre)
            .unwrap_or_default()
            .to_vec();
        for i in 0..self.doc_ids.len() {
            out.extend(self.edited_doc_events(i).0);
        }
        out
    }

    /// The source's line index, built once, on the first error located
    /// (then O(log n) per error, not a scan of the source).
    fn lines(&self) -> &LineIndex {
        self.lines.get_or_init(|| LineIndex::new(&self.text()))
    }

    /// A decoding or settings error: before any event. For document `i`
    /// of a stream, only its own decoding errors count (see
    /// [`Document::diagnostics`]).
    ///
    /// Decided from the invalid runs (always complete)
    /// and the settings diagnostics, never from the capped list.
    fn pre_error(&self, index: Option<usize>) -> Option<Error> {
        let d = match self.runs(index).next() {
            Some(r) => oneyaml_syntax::run_diagnostic(self.origin.encoder.encoding(), &r),
            None => self
                .settings_diags
                .iter()
                .find(|d| d.severity == Severity::Error)?
                .clone(),
        };
        let e = Error::from_diagnostic(d).locate(self.lines());
        Some(e.finish(Some(&self.text()), Vec::new()))
    }

    /// The invalid runs document `i`'s region holds (`None`: all). O(log n).
    fn runs(&self, index: Option<usize>) -> impl ExactSizeIterator<Item = InvalidRun<'_>> + '_ {
        let runs = &self.origin.invalid;
        let (lo, hi) = match index {
            None => (0, runs.len()),
            Some(i) => {
                let (from, to) = (self.region_start(i), self.region_start(i.saturating_add(1)));
                let lo = runs.first_at(from);
                (lo, runs.first_at(to).max(lo))
            }
        };
        runs.iter().skip(lo).take(hi.saturating_sub(lo))
    }

    /// Whether the settings diagnostics hold an error (every document's).
    fn settings_error(&self) -> bool {
        self.settings_diags
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// The errors of the parse document `i`'s region holds (complete,
    /// whatever the cap); `None`: the whole input. O(log n +
    /// the region's errors).
    fn marks(&self, index: Option<usize>) -> &[(usize, usize, Code)] {
        let all = self.error_marks.as_slice();
        let Some(i) = index else {
            return all;
        };
        let (from, to) = (self.region_start(i), self.region_start(i.saturating_add(1)));
        let lo = all.partition_point(|m| m.0 < from);
        let hi = all.partition_point(|m| m.0 < to).max(lo);
        all.get(lo..hi).unwrap_or_default()
    }

    fn documents(&self) -> impl Iterator<Item = TreeNode<'_>> + '_ {
        self.tree
            .root()
            .children()
            .filter(|n| n.kind() == SyntaxKind::Document)
    }

    /// Strict parsing: the first error of any kind in source order — decoding
    /// or settings, syntax, a second document when `single`, or meaning
    /// (resolution, collection tags, duplicate keys, merge values, undefined
    /// and recursive aliases; checked without alias expansion) — with the
    /// warnings before it. On success, the meaning checks' warnings (tolerated
    /// duplicates), to keep.
    ///
    /// The warnings are kept under the cap (with how many more
    /// were found); an error's warnings end with the summary when there
    /// were more than the cap before it.
    fn strict_check(&self, single: bool) -> Result<Found, Error> {
        if let Some(e) = self.pre_error(None) {
            return Err(e);
        }
        let mut events = TreeSource::new(&self.tree);
        let found = crate::de::check_unexpanded(&mut events, &self.settings, single, false);
        match found.errors.into_iter().next() {
            Some(e) => {
                // The tree's skeletons before the error are
                // counted by the replay, which reads them in O(1) each.
                let (mut w, tree_dropped) = events.take_warnings();
                w.extend(found.warnings);
                w.sort_by_key(|d| d.primary.span.start);
                let extra = found.suppressed.and(tree_dropped);
                crate::diag_cap::cap(&mut w, extra, self.limit());
                Err(e.finish(Some(self.tree.source()), w))
            }
            None => Ok(Found {
                list: found.warnings,
                dropped: found.suppressed,
                dropped_errors: found.dropped_errors,
            }),
        }
    }

    /// Recovering parsing: the meaning checks' warnings and errors (each
    /// meaning error skips the rest of its document; checking stops at the
    /// first syntax error, which the tree reports itself), with how many
    /// more were found than kept.
    fn recovering_check(&self) -> Found {
        let mut events = TreeSource::new(&self.tree);
        let found = crate::de::check_unexpanded(&mut events, &self.settings, false, true);
        let mut out = found.warnings;
        out.extend(found.errors.iter().map(|e| e.diagnostic().clone()));
        Found {
            list: out,
            dropped: found.suppressed,
            dropped_errors: found.dropped_errors,
        }
    }

    /// The lowest source offset document `i` owns: where its region
    /// starts ([`Document::span`]: diagnostics follow the
    /// regions); `usize::MAX` past the last document. O(the
    /// stream-level text between documents `i - 1` and `i`).
    fn region_start(&self, i: usize) -> usize {
        if i >= self.doc_ids.len() {
            return usize::MAX;
        }
        let Some(p) = i.checked_sub(1) else {
            return 0;
        };
        let node = |j: usize| self.doc_ids.get(j).and_then(|id| self.tree.get(*id).ok());
        match (node(p), node(i)) {
            (Some(prev), Some(this)) => self.split_between(prev, this),
            // (Every listed document is in the tree; a fallback, never a
            // panic.)
            _ => self.doc_span(i).map_or(usize::MAX, |s| s.start),
        }
    }

    /// The part of `list` (sorted by start) that document `i` owns: a
    /// contiguous run, found by binary search.
    fn owned<'s>(&self, list: &'s [Diagnostic], i: usize) -> &'s [Diagnostic] {
        let (from, to) = (self.region_start(i), self.region_start(i.saturating_add(1)));
        let lo = list.partition_point(|d| d.primary.span.start < from);
        let hi = list.partition_point(|d| d.primary.span.start < to).max(lo);
        list.get(lo..hi).unwrap_or_default()
    }

    /// Adds diagnostics after the pre-event ones, keeping source order,
    /// then applies the diagnostics cap to the whole list:
    /// `suppressed` more were found by the meaning checks, and the tree's
    /// own count. Called once per parse.
    ///
    /// The meaning errors (kept or not) join the complete error
    /// record first.
    fn add_diagnostics(&mut self, found: Found) {
        for d in found.list.iter().filter(|d| d.severity == Severity::Error) {
            self.error_marks
                .push((d.primary.span.start, d.primary.span.end, d.code));
        }
        for (code, span) in &found.dropped_errors {
            self.error_marks.push((span.start, span.end, *code));
        }
        self.error_marks
            .sort_unstable_by_key(|m| (m.0, m.1, m.2.get()));
        self.diagnostics.extend(found.list);
        if let Some(events) = self.diagnostics.get_mut(self.pre..) {
            events.sort_by_key(|d| d.primary.span.start);
        }
        let extra = found.dropped.and(self.dropped);
        self.found = self.diagnostics.len().saturating_add(extra.count);
        let limit = self.limit();
        if let Some(cut) = crate::diag_cap::cap(&mut self.diagnostics, extra, limit) {
            self.decoded = cut.before(self.decoded);
            self.pre = cut.before(self.pre);
            self.capped = true;
        }
    }

    /// A second document, for a `Document` (one document): the error
    /// [`crate::from_str`] reports at its start, whatever comes before it.
    fn multiple_documents(&self) -> Option<Diagnostic> {
        let second = self.documents().nth(1)?;
        let e = Error::new(
            code::MULTIPLE_DOCUMENTS,
            "the input has more than one document",
            Some(crate::construct::tree::doc_start_span(second)),
        )
        .with_help("use oneyaml::Stream to read each document");
        Some(e.diagnostic().clone())
    }

    /// The first error, in the order the event stream reports it, with the
    /// warnings before it; `single`: a second document is an error, as for
    /// [`crate::from_str`]. The reference order the tests hold the
    /// strict check to.
    #[cfg(test)]
    fn first_error(&self, single: bool) -> Option<Error> {
        let src = self.tree.source();
        if let Some(e) = self.pre_error(None) {
            return Some(e);
        }
        if !self.tree.has_errors() && (!single || self.documents().nth(1).is_none()) {
            return None;
        }
        // Rare path: replay the events to order errors exactly as a strict
        // consumer meets them.
        let mut events = TreeSource::new(&self.tree);
        let mut docs = 0usize;
        loop {
            match events.next_event() {
                Ok(e) => match e.node {
                    Ev::StreamEnd => return None,
                    Ev::DocStart { .. } if single && docs == 1 => {
                        let e = Error::new(
                            code::MULTIPLE_DOCUMENTS,
                            "the input has more than one document",
                            Some(e.span),
                        )
                        .with_help("use oneyaml::Stream to read each document");
                        return Some(e.finish(Some(src), events.take_warnings().0));
                    }
                    Ev::DocStart { .. } => docs = docs.saturating_add(1),
                    _ => {}
                },
                Err(e) => return Some(e.finish(Some(src), events.take_warnings().0)),
            }
        }
    }
}

/// A document's counts for `cap_index.rs`, as
/// [`Shared::edited_doc_events`] lists them; the non-empty meaning checks
/// read for them are kept for the read that needs them (so
/// a document's meaning is checked once per read, not once for its count
/// and again for its list), at most `keep` diagnostics in all (the
/// cap bounds what is held; past it a document's checks are read again
/// when listed).
struct Memo<'s> {
    shared: &'s Shared,
    /// The last region boundary read: where document `.0`'s region
    /// starts (the build reads the documents in order).
    boundary: std::cell::Cell<Option<(usize, usize)>>,
    /// Diagnostics that may still be kept.
    room: std::cell::Cell<usize>,
    kept: std::cell::RefCell<std::collections::BTreeMap<usize, (Vec<Diagnostic>, Dropped)>>,
}

impl<'s> Memo<'s> {
    fn new(shared: &'s Shared, keep: usize) -> Self {
        Self {
            shared,
            boundary: std::cell::Cell::new(None),
            room: std::cell::Cell::new(keep),
            kept: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        }
    }

    /// Document `j`'s meaning checks, if kept (taken: read once).
    fn take(&self, j: usize) -> Option<(Vec<Diagnostic>, Dropped)> {
        self.kept.try_borrow_mut().ok()?.remove(&j)
    }
}

impl cap_index::Parts for Memo<'_> {
    fn syntax_part(&self, i: usize) -> cap_index::Agg {
        let s = self.shared;
        let from = match self.boundary.get() {
            Some((j, at)) if j == i => at,
            _ => s.region_start(i),
        };
        let next = i.saturating_add(1);
        let to = s.region_start(next);
        self.boundary.set(Some((next, to)));
        let (n, errors) = s.region_tree_counts(i, Some((from, to)));
        cap_index::Agg {
            n,
            errors,
            dropped: Dropped::default(),
        }
    }

    fn meaning_part(&self, i: usize) -> cap_index::Agg {
        let (list, dropped) = self.shared.doc_meaning_of(i);
        let agg = cap_index::Agg {
            n: list.len(),
            errors: list
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count(),
            dropped,
        };
        // (Empty lists are not kept: the counts say a document's meaning
        // checks found nothing.)
        if let Some(room) = self
            .room
            .get()
            .checked_sub(list.len())
            .filter(|_| !list.is_empty())
        {
            if let Ok(mut kept) = self.kept.try_borrow_mut() {
                self.room.set(room);
                kept.insert(i, (list, dropped));
            }
        }
        agg
    }
}

/// Where a document's entries went in the capped list being built
/// (see [`Shared::shown_record`]).
struct Placed {
    doc: usize,
    at: usize,
    len: usize,
    /// Where its first error is in it.
    error: Option<usize>,
}

/// What the capped read takes from one document ([`Shared::doc_head`]).
struct Head {
    /// Its first diagnostics, in order.
    list: Vec<Diagnostic>,
    /// Its first error (`None`: not known; `Some(None)`: it has none).
    first_error: Option<Option<Diagnostic>>,
    /// How many diagnostics it lists.
    n: usize,
}

/// A diagnostic with its sort key, ordered by the key only.
struct Keyed {
    key: (usize, usize),
    d: Diagnostic,
}

impl PartialEq for Keyed {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Keyed {}

impl PartialOrd for Keyed {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Keyed {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

/// What a meaning check found: the kept diagnostics, what was
/// not kept, and the code and span of every error not kept.
struct Found {
    list: Vec<Diagnostic>,
    dropped: Dropped,
    dropped_errors: Vec<(Code, Span)>,
}

/// The result of a recovering parse: always a value, with every
/// diagnostic, errors included. Check [`Parsed::has_errors`].
#[must_use]
#[derive(Clone, Debug)]
pub struct Parsed<T> {
    value: T,
    diagnostics: Vec<Diagnostic>,
}

impl<T> Parsed<T> {
    /// The parsed value.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// Takes the parsed value.
    pub fn into_value(self) -> T {
        self.value
    }

    /// Every diagnostic, errors and warnings, under `Limits::max_diagnostics`
    /// (see [`Document::diagnostics`]: the first `max_diagnostics`, the
    /// first error, and a [`code::DIAGNOSTICS_SUPPRESSED`] summary).
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// `true` when at least one diagnostic is an error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    /// Every diagnostic as a snippet of `src` (the text that was parsed),
    /// separated by blank lines, with one [`LineIndex`] for all of them:
    /// O(n) once, then O(lines shown) per diagnostic.
    #[must_use]
    pub fn render_diagnostics(&self, src: &str) -> String {
        let index = LineIndex::new(src);
        let rendered: Vec<String> = self
            .diagnostics
            .iter()
            .map(|d| crate::Render::render_indexed(d, src, &index))
            .collect();
        rendered.join("\n")
    }
}

/// A stable handle to a node of a [`Document`]. Resolve it with
/// [`Document::node`]; a handle that names no node of that document yields
/// [`EditError::StaleNode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(oneyaml_syntax::tree::NodeId);

/// One YAML document, lossless and editable.
///
/// # The core path
///
/// [`Document::parse`] → [`Document::get`] / [`Document::get_path`] and
/// the typed accessors of [`NodeRef`] ([`as_str`](NodeRef::as_str),
/// [`as_u64`](NodeRef::as_u64), …) → [`Document::set_path`] /
/// [`Document::get_mut`] and [`NodeMut::push`] / [`Document::remove_path`] →
/// [`to_string`](ToString::to_string):
///
/// ```
/// use oneyaml::Document;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let src = "name: web  # the name\nports: [80, 443]\nold: true\n";
/// let mut doc = Document::parse(src)?;
/// assert_eq!(doc.to_string(), src); // byte-identical
/// assert_eq!(doc.get("name").and_then(|n| n.as_str()).as_deref(), Some("web"));
/// assert_eq!(doc.get_path("ports[1]").and_then(|n| n.as_u64()), Some(443));
///
/// doc.set_path("name", "api")?;
/// doc.get_mut("ports")?.push(8080)?;
/// let _old = doc.remove_path("old")?;
/// assert_eq!(doc.to_string(), "name: api  # the name\nports: [80, 443, 8080]\n");
/// # Ok(())
/// # }
/// ```
///
/// **Reading** is `Option`-based ([`Document::get`],
/// [`Document::get_path`]: a lookup that finds nothing is not an error).
/// The typed accessors read a node as [`Value`] and serde do;
/// [`NodeRef::deserialize`] reads a node as any `Deserialize` type, and
/// [`Document::to_value`] the whole meaning ([`NodeRef`] has a worked
/// example). There is no `Index` on `Document` (it would hide a meaning
/// error behind `Null`):
///
/// ```compile_fail,E0608
/// let doc = oneyaml::Document::parse("a: 1").unwrap();
/// let _ = &doc["a"];
/// ```
///
/// **Editing**: [`Document::set_path`], [`Document::get_path_mut`]
/// and [`NodeMut`] (`insert`, `remove`, `push`, `set`, …), each returning
/// `Result<_, EditError>`; a refused edit changes nothing. Paths
/// are strings or [`Path`]s ([`IntoPath`](crate::IntoPath)); values are
/// anything `Serialize`, by value or by reference. New content is laid
/// out like its neighbours, the rest of the text is kept byte for
/// byte, and the edited document means what its text means. [`NodeMut`]
/// shows edits in depth, [`Removed`] moving an entry.
///
/// # Opt-in: comments, streams, anchors, diagnostics
///
/// * **Comments** are kept by every edit, each with the owner the
///   [comment attachment rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/comment-attachment.md) gives it: see
///   [Comments](#comments) below.
/// * **Streams**: a file of several documents is a [`Stream`]; each of
///   its documents is edited through [`Stream::document_mut`].
/// * **Anchors and aliases**: editing an anchored node is seen through
///   every alias: see [Anchors and aliases](#anchors-and-aliases) below.
/// * **Diagnostics**: [`Document::diagnostics`] (warnings included),
///   [`Document::render_diagnostics`]. [`Document::parse_recovering`]
///   never fails: a document built over input with errors is
///   **restricted**: its error regions are [`NodeKind::Error`] nodes;
///   edits next to an error region are refused ([`EditError::ReadOnly`]),
///   edits elsewhere work.
/// * **Handles**: [`NodeId`]s name nodes across edits; the id-taking
///   methods of `Document` (`set_leading(id, …)`, `set_anchor(id, …)`,
///   `remove_node(id)`) are the base layer under [`NodeMut`], for handles
///   found by [`Document::explain_comments`], [`Document::aliases_of`] and
///   the like.
///
/// # Comments
///
/// Every comment belongs to an owner (an entry, an item, a collection's
/// footer, or the document header or footer) decided by the numbered rules
/// of the [comment attachment rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/comment-attachment.md). Edits carry comments with
/// their owner: a removed entry takes its own comments (listed on the
/// [`Removed`]), a moved entry brings them along, and
/// [`Document::explain_comment`] says which rule decided:
///
/// ```
/// use oneyaml::{CommentOwner, CommentRule, Document, path};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let src = "ports:\n  - 80\n  # - 8443  (TLS at the ingress)\n  - 9090\n";
/// let mut doc = Document::parse(src)?;
///
/// // Rule 4: the comment line belongs to the item below it...
/// let id = doc.comment_ids()[0];
/// let why = doc.explain_comment(id)?;
/// let metrics = doc.id_of(&path!["ports", 1]).ok_or("no ports[1]")?;
/// assert_eq!(why.rule(), CommentRule::Leading);
/// assert_eq!(why.owner(), CommentOwner::Leading(metrics));
///
/// // ...so removing `9090` takes it along (`Removed::comments`), while
/// // `remove_keep_comments` leaves it, as the footer of `ports` (rule 6).
/// let mut scratch = doc.clone();
/// let removed = scratch.remove_node(metrics)?;
/// assert_eq!(removed.comments(), ["# - 8443  (TLS at the ingress)"]);
/// let removed = doc.remove_keep_comments(metrics)?;
/// assert!(removed.comments().is_empty());
/// assert_eq!(doc.to_string(), "ports:\n  - 80\n  # - 8443  (TLS at the ingress)\n");
/// # Ok(())
/// # }
/// ```
///
/// The comment API reads and writes comments by owner:
/// [`Document::comments`] (leading, trailing, footer),
/// [`Document::header`] and [`Document::footer`]; on a [`NodeMut`] (or with
/// an id on `Document`) [`NodeMut::set_leading`], [`NodeMut::set_trailing`],
/// [`NodeMut::push_footer`], [`NodeMut::clear_comments`] and
/// [`NodeMut::set_blank_lines_before`]; and [`Document::set_header`] and
/// [`Document::set_footer`]. Comment lines are any iterable of strings
/// (`["a"]`, a `Vec<String>`); [`NO_COMMENTS`](crate::NO_COMMENTS) removes
/// them. [`Document::attach_comment`] and [`Document::detach_comment`]
/// re-own a comment explicitly when the rules got it wrong (in the
/// `Document` only: the text cannot carry it). Replacing a value keeps its
/// entry's comments:
///
/// ```
/// use oneyaml::{Document, path};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut doc = Document::parse("# app\n\nimage: web:1.4   # pinned\n")?;
/// doc.set_path(&path!["image"], "web:1.5")?
///     .set_leading(["bumped by release tooling"])?; // the entry's comment
/// // The entry's comments belong to its key.
/// let image = doc.get_mut("image")?.into_key()?.id();
/// assert_eq!(doc.comments(image)?.trailing()[0].text(), "pinned");
/// assert_eq!(
///     doc.to_string(),
///     "# app\n\n# bumped by release tooling\nimage: web:1.5   # pinned\n"
/// );
/// # Ok(())
/// # }
/// ```
///
/// An entry is named by its key ([`NodeRef::key`], [`NodeMut::into_key`]);
/// on a `NodeMut` to a mapping value, the comment edits act on its entry,
/// and the id-taking `Document` methods act on exactly the node they are
/// given. An edit that would give a comment a different owner when the text
/// is read again is refused ([`EditError::CommentReattaches`]), so the
/// owners you see are the owners the text has. The comment layouts that are
/// refused are listed under *Editing in depth* below.
///
/// # Anchors and aliases
///
/// `Document` never expands aliases: [`NodeRef::alias_target`] is the
/// anchored node, and the typed accessors read through it. Editing an
/// anchored node changes what every alias of it means; removing one that an
/// alias still uses is refused ([`EditError::AnchorInUse`]).
/// [`NodeMut::set_anchor`], [`NodeMut::push_alias`] and
/// [`NodeMut::insert_alias`] write anchors and aliases, and
/// [`Document::aliases_of`] finds the aliases of a node:
///
/// ```
/// use oneyaml::{Document, EditError};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut doc = Document::parse("base: &port 8080\nweb: {port: *port}\n")?;
/// assert_eq!(doc.get_path("web.port").and_then(|n| n.as_u64()), Some(8080));
/// doc.set_path("base", 9090)?; // the anchor stays
/// assert_eq!(doc.to_value()?["web"]["port"].as_u64(), Some(9090));
/// let base = doc.id_of("base").ok_or("no base")?;
/// let refused = doc.remove_path("base"); // `*port` still uses it
/// assert!(matches!(refused, Err(EditError::AnchorInUse { .. })));
/// assert_eq!(doc.aliases_of(base)?.len(), 1);
/// # Ok(())
/// # }
/// ```
///
/// # Editing in depth
///
/// **Limitations** (each refusal changes nothing).
/// These are refused with [`EditError::Unsupported`], whose `reason`
/// names them:
///
/// * replacing a mapping key in place (edit the entry: remove it and
///   insert the new one);
/// * inserting or removing an entry whose key is a collection;
/// * moving a [`Removed`] between block and flow context when it would
///   need re-rendering: block content (multi-line block text, or an entry
///   with comments, including its same-line comment) into a flow
///   collection; and re-inserting a flow entry whose removal took comments
///   after it (re-rendering moved content in the target style is a later
///   phase);
/// * re-inserting a [`Removed`] where its text cannot be re-indented (a
///   line of it less indented than the new column allows): the same
///   later phase;
/// * writing a **tag** into a document that has `%TAG` directives (the
///   tag's meaning would depend on them).
///
/// Also `Unsupported`, as refusals of the request itself rather than
/// limitations: an alias inside its own anchored node, and an anchor on an
/// alias (neither can be written in YAML); a [`Removed`] entry into a
/// sequence or item into a mapping; removing the root (use
/// [`Document::set_root`]) or a key (remove its entry); `set_scalar` on a
/// collection; and, defensively, a path segment of a kind this version
/// does not know.
///
/// One more limitation is refused as [`EditError::NotWellFormed`], not
/// `Unsupported` (the syntax layer cannot tell it from a malformed edit):
/// under `lenient_inline_colons`, an edit whose
/// validation window holds a `Y0028` warning attached to an element the
/// edit moves may be refused even when its result would be well formed (the syntax layer's check of where
/// diagnostics attach is conservative there): e.g. appending an entry
/// after `k: x: z` at the end of `b: 1⏎k: x: z⏎`. Relaxing this is a
/// later-phase item.
///
/// Also [`EditError::NotWellFormed`]:
/// an unterminated last line of spaces after a kept (`|+`) block
/// scalar is one more line of that scalar's value, so an edit that would
/// change which scalar reads it, other than as meant, is refused, nothing
/// changed: e.g. `insert("z", 1)` into `k: |+⏎  a⏎␠` (it would change `k`
/// from `"a\n\n"` to `"a\n"`), or moving `k` of `a: 1⏎k: |+⏎  x⏎␠` first.
/// The check is on values, once per edit: re-inserting the removed `k` at
/// its place, anchoring it, or rewriting comments that leave its value as
/// it was are accepted. A new multi-line value written right before such a
/// line is written double-quoted instead of `|+`, so it keeps its own
/// value; removing the entry after a kept scalar (`z` of `k: |+⏎  x⏎⏎z: 1⏎␠`)
/// removes the final line of spaces too, so `k` keeps its value; comment
/// writes that would change it are refused `Unsupported`, as in 0.3.
///
/// **Comments through node edits** (the
/// [rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/comment-attachment.md) decides every owner).
/// A removal takes
/// exactly the comments the rulebook gives the entry — its leading comment
/// lines, its same-line comment, those inside it, and the footers of the
/// collections inside it — with the blank lines they carry (rule 8), into
/// its [`Removed`]; every other comment stays with its owner (comment
/// lines after a block scalar are the next entry's leading comments, rule
/// 4, or a footer, rules 6 and 7). [`Document::remove_keep_comments`]
/// leaves the leading comments to the next entry or the collection's
/// footer (the document footer for a root collection's last entry). A
/// move carries the entry's comments; an insertion adds none; the
/// document header and footer never change as a side effect (the two
/// document-footer cases under **Fallbacks** below follow from the call
/// itself). A re-inserted entry brings its comments back around it,
/// and [`NodeMut::insert_removed_at`] back at the original position gives
/// back the original bytes (checked over the yaml-test-suite and generated
/// commented documents). Every node edit checks the owners of the comments
/// around it afterwards: an edit after which a comment would read
/// back with another owner is refused with
/// [`EditError::CommentReattaches`]; one after which comment lines would
/// read as content (or content as comments: e.g. a leading comment line at
/// column 2 below a `|2-` scalar moved above it) with
/// [`EditError::NotWellFormed`]. Each refusal changes nothing.
///
/// * **Replacing a value** keeps the entry's comments, and the old value's
///   inner comments go with it (the leading comments of its first item,
///   comment lines between the key and the value). A comment the entry (or
///   the node) owns that stands inside the old value's first line is
///   written back on the new value's line: `top: &n # c⏎  k: v⏎` set to 7
///   becomes `top: &n 7 # c⏎`; a new value on lines of its own leaves it on
///   the key's (or dash's) line: `- &b one # c⏎` set to `[1, 2]` becomes
///   `- &b # c⏎  - 1⏎  - 2⏎`. Where it cannot be written back (more than
///   one such comment, one not on the first line, or no place for it: a
///   flow context, or text after the value) the replacement is refused
///   with `Unsupported` ("the entry's comment inside the replaced value
///   cannot be written back there"): e.g. setting `key` in
///   `key: &a⏎  # c1⏎  !!map # t⏎  a: b⏎`.
/// * **Inserting into a flow collection** writes the new entry right after
///   the previous entry, so a comment before the next entry's `,` stays
///   that entry's: inserting `5` at 1 into `[ word1⏎# comment⏎, word2]⏎`
///   gives `[ word1, 5⏎# comment⏎, word2]⏎`.
/// * **A new anchor** goes on the key's (or dash's) line, above comment
///   lines that belong to the node's first entry: anchoring `k` in
///   `k:⏎  # about a⏎  a: 1⏎` gives `k: &x⏎  # about a⏎  a: 1⏎` (after
///   `---`, on a line of its own below it; a root without `---`, on a
///   line of its own above those comment lines, rule 12: `# about a⏎a: 1⏎`
///   gives `&x⏎# about a⏎a: 1⏎`).
/// * **An only entry leaving a collection** turns it into `{}`/`[]` on the
///   parent's line. Its properties stay, and so does its tag, except the
///   Core defaults `!!map`/`!!seq`, which the flow form states anyway:
///   `k: !!set⏎  ? a⏎` becomes `k: !!set {}⏎`, `k: !!map⏎  a: 1⏎` becomes
///   `k: {}⏎`. A **nested** collection's footer comments move inside the
///   new flow collection, still its footer: removing `a` from
///   `k:⏎  a: 1⏎  # footer k⏎z: 1⏎` gives `k: {⏎  # footer k⏎  }⏎z: 1⏎`
///   (they never go to a sibling). When they cannot (they are not whole
///   lines after the entry, or the properties' line has a comment too) the
///   removal is refused (see **P7** below).
///
/// **Fallbacks** (each tried only when the first layout would re-own a
/// comment, and each still checked): an insertion is retried without
/// the blank lines it would add when they cannot be written there (after a
/// `|+` scalar they would be its content), and then with a re-inserted
/// entry's leading comment lines written at the entry's column (where
/// their own column would give them to a deeper collection's footer, rule
/// 6); a removal is retried with the next entry's leading comment lines
/// re-indented to that entry's column, for the same reason; when an only
/// entry leaves a root collection (it becomes `{}`/`[]`, which owns no
/// footer), the collection's footers become the document footer (rule 7).
/// [`Document::remove_keep_comments`] of a root collection's last entry
/// gives its leading comments to the document footer (the call is the
/// explicit request).
///
/// **Comment-layout limitations** (comments through node edits; each refusal
/// changes nothing, and names its class):
///
/// * **S1 — footer by column** (rule 6): comment lines whose column gives
///   them to a deeper collection's footer once the entry between is
///   removed or moved, even after the fallbacks: `CommentReattaches`.
///   E.g. removing `- s` from `- - a⏎  - b⏎ # c⏎    # d⏎- s⏎`.
/// * **S2 — rule 3 at the top**: the new first entry of a root collection
///   has an empty key, so the comment lines above it would be the root
///   node's: `CommentReattaches`. E.g. removing `a: 1` from
///   `a: 1⏎# c⏎: x⏎`.
/// * **S3 — flow layout**: a flow entry with comments around it moved or
///   re-inserted where the layout cannot keep each comment's owner (not on
///   a line of its own, or another entry's comment before its `,`):
///   `Unsupported` ("a flow entry with comments around it cannot be
///   re-inserted yet"), or `CommentReattaches`; removing a flow entry next
///   to other owners' comments the layout cannot keep: `Unsupported`
///   ("removing a flow entry next to comments of other owners that the
///   flow layout cannot keep"). E.g. removing `c` from
///   `- [ a, b, # t⏎  c ]⏎`. Inserting into a single-pair mapping inside
///   a flow sequence (`[ a: b ]`, written back as `{a: b, new: 1}`) when
///   the pair has leading or same-line comments, which the new `{…}` item
///   would own: `CommentReattaches`. E.g. inserting into
///   `foo: bar` in `[⏎foo: bar # t⏎]⏎`.
/// * **S4 — anchors next to comment lines**: setting or removing an anchor
///   whose properties line holds or borders a comment that would change
///   owner: `CommentReattaches`, or `Unsupported` ("removing the anchor of
///   a node whose properties stand on a line of their own above comment
///   lines"). E.g. removing `&n` from `top: # t⏎  &n # u⏎  k: v⏎`.
///   Setting or removing an anchor that would make an empty root visible
///   or invisible (turning the document header into the footer, or back)
///   is refused with `CommentReattaches` too.
/// * **S5 — properties and an only entry**: comments of other owners
///   between a collection's properties and its only entry, which is
///   removed: `Unsupported` ("comments of other owners between a
///   collection's properties and its only entry"). E.g. removing `a: b`
///   from `key: &x⏎  # c1⏎  !!map # t⏎  a: b⏎`.
/// * **S6 — block scalars and explicit keys**: comment lines whose re-read
///   next to a block scalar or a `? ` key would change a meaning:
///   `NotWellFormed` when the text would read comment lines as content (see
///   above); appending to a sequence that is a `? ` key written without
///   `:` whose last item has a same-line comment: `Unsupported`
///   ("appending after comments that lie past an empty value (a `? ` key
///   written without `:`)"). E.g. pushing onto `- a` in `? - a # t⏎`.
///
/// The same classes, met by moves to a middle index and by re-inserting a
/// [`Removed`] into another collection, and the refusals that keep
/// comments the 0.2 edits deleted:
///
/// * **P1 — a block entry with comments into a flow collection**: the 0.2
///   limitation above, `Unsupported` ("block content or comments cannot
///   move into a flow collection"; the 0.2 [`Removed`] dropped the
///   comments). E.g. re-inserting `a` from `a: 1 # t⏎b: {c: 2}⏎` into `b`.
/// * **P2 — S1/S2 re-owning on moves and re-insertions**:
///   `CommentReattaches`. E.g. moving entry 2 to index 1 in
///   `a: 1⏎  # c⏎: # e⏎ # d⏎"q":⏎- e⏎`.
/// * **P3 — S3 flow layout on moves and re-insertions**: `Unsupported` ("a
///   flow entry with comments around it cannot be re-inserted yet") or
///   `CommentReattaches`. E.g. moving entry 0 to index 1 in
///   `[⏎# c⏎  b, c, d]⏎`.
/// * **P4 — the entry's comment inside a replaced value** that cannot be
///   written back (see **Replacing a value** above): `Unsupported`.
/// * **P5 — block scalars next to blank lines and comments** moved to a
///   middle index, where a comment line would read as scalar content:
///   `NotWellFormed` — the error names no
///   reason; this class is recognised by its shape. E.g. moving `t` to
///   index 1 in `a: 1⏎⏎  # c⏎⏎b: 2⏎⏎t: |⏎  x⏎`.
/// * **P6 — removing an anchor** whose own line holds a comment that would
///   change owner (the S4 family): `CommentReattaches`. E.g. removing `&n`
///   from `k:⏎  &n # t⏎  a: 1⏎`.
/// * **P7 — a nested collapse with a comment on the properties' line**:
///   `Unsupported` ("the footer comments of a nested collection whose only
///   entry is removed have no owner"). E.g. removing `x: y` from
///   `e:⏎ &n # p⏎ - x: y⏎  # f⏎z: 1⏎`.
/// * **P8 — anchoring a root mapping whose first entry has an empty key**
///   below comment lines: the anchor's line would give them to that
///   entry's value: `CommentReattaches`. E.g. anchoring the root of
///   `# c⏎: x⏎`.
/// * **S5** applies to re-insertions too (the entry removed first). The
///   S3/P3 flow limitation covers moving, or re-inserting elsewhere, a
///   flow item with comment lines at column 0 above it inside a block
///   collection (`- [⏎# c⏎  b, c]`); removing it, and putting it back at
///   its place, work. S4 and P6 cover anchoring or un-anchoring a **root**
///   next to header lines or on an empty root.
///
/// Also refused with `Unsupported` (a refusal of the request):
/// `remove_keep_comments` of a collection's only entry or of a flow entry
/// that has leading comments (they have no entry or block footer to go
/// to).
///
/// Setting the value of an entry written without `:` works:
/// `? a` alone (as in `!!set`) becomes `? a⏎: value` (the key line's
/// comment stays on it), and `{a, b}` becomes `{a: value, b}`.
///
/// **Meaning errors accepted**: an edit whose text is
/// well formed but whose *meaning* is an error is accepted and reported
/// through [`Document::diagnostics`] and [`Document::has_errors`] (both
/// readers agree on the erroring text): replacing the node a merge
/// key reads (through an alias) with a non-mapping (`Y1016`), and making a
/// complex key equal to a sibling key (`Y1013`). Refusing such edits is a
/// later-phase refinement.
///
/// **Restricted mode and edits**: what restricted mode protects
/// is found once per document, from its own diagnostics (its own region
/// of a [`Stream`]) and the error nodes of its own subtree (whichever
/// document owns their diagnostic: an unclosed `[` cut off by the next
/// `---` is reported by the next document), before its first
/// edit, and kept (by a stream across
/// switches between its documents, and by clones). It is not refreshed by
/// later edits: accepted edits never create, remove, or change a syntax
/// error region, and a meaning error an accepted edit creates is reported
/// ([`Document::diagnostics`], [`Document::has_errors`]) without making
/// its region read-only, so the next edit can repair it (re-checking the
/// meaning on every edit would make each edit O(document)).
///
/// ```
/// use oneyaml::{Document, path};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut doc = Document::parse("name: web # the name\nports:\n    - 80\n")?;
/// doc.set_path(&path!["name"], "api")?;
/// doc.get_mut("ports")?.push(&443)?;
/// assert_eq!(doc.to_string(), "name: api # the name\nports:\n    - 80\n    - 443\n");
/// # Ok(())
/// # }
/// ```
pub struct Document {
    shared: Arc<Shared>,
    /// The `Document` node; `None` for an empty stream.
    doc: Option<oneyaml_syntax::tree::NodeId>,
    /// `None`: the whole input (`Document::parse`); `Some(i)`: document
    /// `i` of a [`Stream`].
    index: Option<usize>,
    /// This document's identity, for [`Removed`] (a fresh one per parse
    /// and per clone).
    id: u64,
    /// What restricted mode protects, found on the first edit.
    guard: Option<edit::Guard>,
    /// The anchor-index changes of the open edit group (empty
    /// outside an edit, no allocation).
    ix_log: Vec<edit::IxOp>,
    /// Explicit comment owners ([`Document::attach_comment`]), indexed by
    /// comment and by owner. `None`, with no allocation, until the first
    /// explicit attachment (nothing added to parsing); shared by clones and
    /// stream snapshots (O(1)), copied on the first change after they diverge.
    /// `BTreeMap`s, for a later `no_std` flag.
    explicit: Option<Arc<comments::Explicit>>,
    /// The explicit-owner changes of the open edit group, to be reverted
    /// with it (a fuzz finding: a refused edit dropped an
    /// `attach_comment` owner). Empty outside an edit.
    explicit_log: Vec<comments::ExplicitUndo>,
    /// The final-space-line value check of the open edit group (empty
    /// outside an edit, no allocation).
    tail: edit::TailGuard,
    /// The open edit touches a node right next to
    /// an error region, so at its end the document's errors are checked
    /// against its text (set by `check_region`, which reads `&self`).
    error_neighbour: std::sync::atomic::AtomicBool,
}

impl core::fmt::Debug for Document {
    /// What the document is — which document of its input, its
    /// span, whether it was edited, its encoding and settings — never its
    /// caches (lookup indexes, the cached `Value`, logs) or per-process
    /// identities: deterministic, and bounded whatever the input.
    ///
    /// ```
    /// let doc = oneyaml::Document::parse("a: 1\n").unwrap();
    /// let shown = format!("{doc:?}");
    /// assert!(shown.starts_with("Document { index: None, span: Span { start: 0, end: 5 }"));
    /// ```
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Document")
            .field("index", &self.index)
            .field("span", &self.span())
            .field("edited", &self.shared.edited)
            .field("encoding", &self.encoding())
            .field("settings", self.settings())
            .finish_non_exhaustive()
    }
}

/// The identity of the next document built or cloned.
static NEXT_DOCUMENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn document_id() -> u64 {
    NEXT_DOCUMENT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Clone for Document {
    /// An independent copy. O(1): the copies
    /// share the tree until one of them is edited, which then copies it
    /// (O(n), copy-on-write), so editing one never changes the other.
    ///
    /// **Handles.** Every handle taken before the clone resolves in both
    /// copies, before and after their edits, until its own node is removed
    /// or replaced in that copy; a handle taken from one copy after they
    /// diverged names nothing in the other ([`EditError::StaleNode`]).
    ///
    /// **Cost.** Cloning is O(1). The first edit of a copy whose tree is
    /// still shared copies the tree, O(n), as a syntax-tree *fork* that
    /// keeps handle identity itself: no per-copy handle map, O(1) handle
    /// metadata per copy, and resolving a handle stays O(log) in the
    /// number of forks of its lineage made after insertions (16 bytes
    /// each). Nothing grows with the number of clones that are no longer
    /// alive.
    ///
    /// A [`Removed`] belongs to the copy it was taken from.
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            doc: self.doc,
            index: self.index,
            id: document_id(),
            // The same tree, and a fork keeps its handles: what restricted
            // mode protects is the same in the copy (not found again, which
            // costs a meaning check of an edited document, O(n)).
            guard: self.guard.clone(),
            ix_log: Vec::new(),
            explicit_log: Vec::new(),
            tail: edit::TailGuard::default(),
            error_neighbour: std::sync::atomic::AtomicBool::new(false),
            explicit: self.explicit.clone(),
        }
    }
}

impl Document {
    /// The `Document` node of `shared` this document stands for.
    fn whole(shared: Shared) -> Self {
        let doc = shared.documents().next().map(TreeNode::id);
        Self {
            shared: Arc::new(shared),
            doc,
            index: None,
            id: document_id(),
            guard: None,
            ix_log: Vec::new(),
            explicit_log: Vec::new(),
            tail: edit::TailGuard::default(),
            error_neighbour: std::sync::atomic::AtomicBool::new(false),
            explicit: None,
        }
    }

    /// Document `index` of a stream's `shared` (shared until its first
    /// edit).
    fn of_stream(
        shared: &Arc<Shared>,
        doc: Option<oneyaml_syntax::tree::NodeId>,
        index: Option<usize>,
    ) -> Self {
        Self {
            shared: Arc::clone(shared),
            doc,
            index,
            id: document_id(),
            guard: None,
            ix_log: Vec::new(),
            explicit_log: Vec::new(),
            tail: edit::TailGuard::default(),
            error_neighbour: std::sync::atomic::AtomicBool::new(false),
            explicit: None,
        }
    }

    fn strict(mut shared: Shared) -> Result<Self, Error> {
        let found = shared.strict_check(true)?;
        shared.add_diagnostics(found);
        shared.mode = Mode::Strict { single: true };
        Ok(Self::whole(shared))
    }

    fn recovering(mut shared: Shared) -> Parsed<Self> {
        shared.mode = Mode::Recovering { single: true };
        let mut found = shared.recovering_check();
        // A recovering parse reports every error, so a second
        // document is always one, in source order with the rest.
        found.list.extend(shared.multiple_documents());
        shared.add_diagnostics(found);
        let diagnostics = shared.diagnostics.clone();
        Parsed {
            value: Self::whole(shared),
            diagnostics,
        }
    }

    /// Parses one document with default [`Settings`]. Strict: any error
    /// fails; warnings are kept ([`Document::diagnostics`]). An empty input
    /// is an empty document; more than one document is
    /// [`code::MULTIPLE_DOCUMENTS`] (use [`Stream`]).
    ///
    /// Strict parsing also checks the meaning, as [`crate::from_str`]
    /// does, **without expanding aliases** (a `Document` never
    /// expands them): invalid tagged scalars (`!!int x`), a collection tag
    /// on a scalar, duplicate keys, invalid merge values, and undefined or
    /// recursive aliases ([`code::UNDEFINED_ALIAS`]) are errors, in source
    /// order. An alias stands for its anchor's shape, and as a
    /// key (or inside one, merged into one included: `? {<<: *a}`) it means
    /// its anchor's value: a scalar's, and a collection's within the alias
    /// budgets `from_str` has, past which (where
    /// `from_str` refuses) it equals only other aliases of the same anchor.
    /// What only expansion reveals — the alias budget, depth after
    /// expansion, or a duplicate past that copy budget — is reported when
    /// the meaning is read ([`Document::to_value`]).
    ///
    /// Pass a `String` to hand over its buffer; a `&str` is copied.
    ///
    /// # Errors
    ///
    /// The first syntax, limit, settings, or meaning error, as
    /// [`crate::from_str`] reports it (see above for what needs
    /// expansion).
    pub fn parse(src: impl Into<String>) -> Result<Self, Error> {
        Self::parse_with(src, &Settings::new())
    }

    /// [`Document::parse`] with explicit [`Settings`].
    ///
    /// # Errors
    ///
    /// As [`Document::parse`].
    pub fn parse_with(src: impl Into<String>, settings: &Settings) -> Result<Self, Error> {
        Self::strict(Shared::from_str(src.into(), settings))
    }

    /// Parses one document, recovering from errors: never fails.
    /// Syntax errors and the meaning errors [`Document::parse`] checks are
    /// [`Parsed::diagnostics`] (a meaning error skips the rest of its
    /// document's checks; meaning is checked up to the first syntax
    /// error). The broken regions become read-only error nodes; the rest
    /// of the document can still be read and edited, and
    /// [`Parsed::render_diagnostics`] draws every diagnostic rustc-style,
    /// with its suggested fix.
    ///
    /// ```
    /// use oneyaml::{Document, NodeKind, Settings, code};
    ///
    /// let parsed = Document::parse_recovering("a: [1, 2\nb: 3\n");
    /// assert!(parsed.has_errors());
    /// let doc = parsed.into_value();
    /// // The error region is read-only; the diagnostics say what happened.
    /// let region = doc.errors().next().unwrap();
    /// assert_eq!(region.kind(), NodeKind::Error);
    /// assert!(doc.diagnostics().iter().any(|d| d.severity == oneyaml::Severity::Error));
    /// let lenient = Settings::new().lenient_inline_colons(true);
    /// let doc = Document::parse_recovering_with("title: The farm: a story\n", &lenient);
    /// assert!(!doc.has_errors());
    /// assert_eq!(doc.value().diagnostics()[0].code, code::LENIENT_INLINE_COLON);
    /// assert_eq!(doc.value().settings(), &lenient);
    /// ```
    pub fn parse_recovering(src: impl Into<String>) -> Parsed<Self> {
        Self::parse_recovering_with(src, &Settings::new())
    }

    /// [`Document::parse_recovering`] with explicit [`Settings`].
    pub fn parse_recovering_with(src: impl Into<String>, settings: &Settings) -> Parsed<Self> {
        Self::recovering(Shared::from_str(src.into(), settings))
    }

    /// Parses bytes in UTF-8, UTF-16, or UTF-32 (§5.2). Spans refer to the
    /// decoded UTF-8 text; [`Document::to_bytes`] gives the input back.
    ///
    /// ```
    /// use oneyaml::{Document, Encoding, Settings, path};
    ///
    /// // `a: 1` in UTF-16LE, with a byte order mark.
    /// let bytes = [0xFF, 0xFE, b'a', 0, b':', 0, b' ', 0, b'1', 0, b'\n', 0];
    /// let mut doc = Document::from_slice(&bytes).unwrap();
    /// assert_eq!(doc.encoding(), Encoding::Utf16Le);
    /// assert_eq!(doc.to_bytes(), bytes); // byte-identical, in the input's encoding
    /// doc.set_path(&path!["a"], &2).unwrap();
    /// assert_eq!(doc.to_string(), "\u{feff}a: 2\n");
    /// assert_eq!(doc.to_bytes()[8], b'2'); // written back in UTF-16LE
    /// let merged = Document::from_slice_with(&bytes, &Settings::new().merge_keys(true)).unwrap();
    /// assert!(merged.settings().get_merge_keys());
    /// // Invalid bytes are errors; recovering keeps them, read-only.
    /// let parsed = Document::from_slice_recovering(b"a: \xff\n", &Settings::new());
    /// assert!(parsed.has_errors());
    /// assert_eq!(parsed.value().to_bytes(), b"a: \xff\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::parse`]; invalid encoding is `Y0029`–`Y0031`.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        Self::from_slice_with(bytes, &Settings::new())
    }

    /// [`Document::from_slice`] with explicit [`Settings`].
    ///
    /// # Errors
    ///
    /// As [`Document::from_slice`].
    pub fn from_slice_with(bytes: &[u8], settings: &Settings) -> Result<Self, Error> {
        Self::strict(Shared::from_slice_strict(bytes, settings)?)
    }

    /// [`Document::from_slice`], recovering from errors (decoding errors
    /// included): never fails.
    pub fn from_slice_recovering(bytes: &[u8], settings: &Settings) -> Parsed<Self> {
        Self::recovering(Shared::from_slice(bytes, settings))
    }

    /// The settings the document was parsed with.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.shared.settings
    }

    /// Every diagnostic of this document: decoding, settings, syntax
    /// errors (restricted mode) and warnings. For [`Document::parse`],
    /// every diagnostic of the input.
    ///
    /// **Ownership in a [`Stream`].** A diagnostic belongs to the document
    /// whose **region** ([`Document::span`]) holds its start: the
    /// regions are consecutive, half-open, and cover the input, so a
    /// diagnostic at a boundary belongs to the document that starts there
    /// (e.g. an unclosed `[` cut off by the next `---` is reported at that
    /// `---`); one in the text between two documents belongs to the one
    /// whose region holds it (an invalid byte in the earlier document's
    /// trailing comment is the earlier document's; a malformed directive
    /// before the next `---` the next one's); anything before the first
    /// document is the first one's, anything after the last (e.g. a
    /// trailing `%YAML` with no document) the last one's. A document's
    /// diagnostics are thus those of its own text (checked over the
    /// yaml-test-suite: each document's text parsed alone, recovering,
    /// reports the same codes at the same relative spans, the stream-wide
    /// meaning-check cut-off aside).
    /// So every located diagnostic, decoding ones included, belongs to
    /// exactly one document. Settings diagnostics describe the
    /// [`Settings`] every document was read with, so each document lists
    /// them. A stream without documents has only
    /// [`Stream::diagnostics`].
    ///
    /// Order: decoding, then settings, then syntax and meaning
    /// diagnostics in source order. O(log n + own + the stream-level text
    /// around the document).
    ///
    /// After edits, the diagnostics are those of the edited text, built as
    /// parsing it builds them: its syntax diagnostics (the syntax
    /// layer moves them with the edits) and its meaning checks. For a
    /// document of a [`Stream`], both are read from **its own region**
    /// ("stream edits are local too"): O(the
    /// document), never O(stream); its meaning is checked as the document
    /// by itself (for a strict parse: its warnings and its first meaning
    /// error; for a recovering one: every meaning error, and none past the
    /// stream's first syntax error, as a recovering parse reports them).
    /// For [`Document::parse`], O(n).
    ///
    /// **The cap** (`Limits::max_diagnostics`): one budget per
    /// stream. The stream's list keeps its first `max_diagnostics`
    /// diagnostics, the first error wherever it is, and one
    /// [`code::DIAGNOSTICS_SUPPRESSED`] summary in the place of the first
    /// suppressed one; a document lists the part of that list its region
    /// owns (the summary included, if it is located there), so a document
    /// past the point where suppression starts lists nothing but a first
    /// error. **Meaning never depends on the cap**: reads,
    /// [`Document::has_errors`], restricted mode, and which edits are
    /// accepted are decided from a complete record of the parse (every
    /// error, kept or not); an error region past the cap still gives its
    /// code and spans ([`NodeRef::diagnostic`], with the code's summary for
    /// a message). After edits, the list equals a parse of the edited text
    /// with the same limits; when the stream's diagnostics (counted
    /// exactly, O(the documents edited since the last read)) exceed the
    /// cap, a document's list is read
    /// from the stream's, built once per edit state from the documents'
    /// diagnostic counts (the counts are built once, O(stream),
    /// on the first such read, 16 bytes per document). A read costs
    /// O(log d) per document holding an entry it keeps or its first error,
    /// plus reading the documents edited since the last read (and their
    /// neighbours' syntax counts); an untouched document whose entries the
    /// last list showed is not read again. When the parse kept
    /// syntax diagnostics past the cap without their texts, a document
    /// read for one it shows has its own text read again under the cap;
    /// only when that cannot give them, and for a stream of one document,
    /// is the edited text parsed again.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        if self.shared.edited {
            return self.edited_diagnostics();
        }
        self.own_diagnostics().cloned().collect()
    }

    /// Every [`Document::diagnostics`] as a snippet of the document's text
    /// ([`Render`](crate::Render)), separated by blank lines, all with the
    /// document's one line index: O(n) once, then O(lines shown) per
    /// diagnostic (calling `render` per diagnostic builds an
    /// index each time).
    ///
    /// ```
    /// let parsed = oneyaml::Document::parse_recovering("%YAML 1.1\n--- a: b: c\n");
    /// let text = parsed.value().render_diagnostics();
    /// assert!(text.starts_with("warning[Y1000]")); // `%YAML 1.1`
    /// assert!(text.contains("error["));
    /// ```
    #[must_use]
    pub fn render_diagnostics(&self) -> String {
        let text = self.shared.text();
        let index = self.shared.lines();
        let rendered: Vec<String> = self
            .diagnostics()
            .iter()
            .map(|d| crate::Render::render_indexed(d, &text, index))
            .collect();
        rendered.join("\n")
    }

    /// [`Document::diagnostics`] of an edited document.
    fn edited_diagnostics(&self) -> Vec<Diagnostic> {
        let s = &self.shared;
        let Some(i) = self.index else {
            return s.fresh_diagnostics();
        };
        // (An edited tree has no decoding diagnostics: they make it
        // read-only.)
        if s.cap_possible() {
            // The stream's cap decides what this document keeps.
            return s.capped_doc(i);
        }
        let settings = s.diagnostics.get(s.decoded..s.pre).unwrap_or_default();
        let mut out = settings.to_vec();
        out.extend(s.edited_doc_events(i).0);
        out
    }

    /// This document's diagnostics, borrowed (see
    /// [`Document::diagnostics`]): three binary searches, never a scan of
    /// the stream.
    fn own_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> + '_ {
        let s = &self.shared;
        let all = s.diagnostics.as_slice();
        let decoded = all.get(..s.decoded).unwrap_or_default();
        let settings = all.get(s.decoded..s.pre).unwrap_or_default();
        let events = all.get(s.pre..).unwrap_or_default();
        let (decoded, events) = match self.index {
            None => (decoded, events),
            Some(i) => (s.owned(decoded, i), s.owned(events, i)),
        };
        decoded.iter().chain(settings).chain(events)
    }

    /// `true` when the document has errors (restricted mode): its
    /// own subtree holds an error node ([`Document::errors`] is not empty),
    /// or it owns an error diagnostic (the ownership rule of
    /// [`Document::diagnostics`]).
    ///
    /// **In a [`Stream`] this can be `true` while this document's
    /// [`Document::diagnostics`] holds no error**:
    /// an error region and the diagnostic describing it can fall in
    /// different documents. In `a: [1, 2⏎---⏎…` the unclosed `[` is cut
    /// off by the `---`: the first document holds the error node (it ends
    /// there), but the diagnostic is located at the `---`, where the next
    /// document starts, so the ownership rule gives it to the next
    /// document. Both documents then have errors: the first because its
    /// contents are an error region, the next because it owns the
    /// diagnostic. Ownership follows the documents' regions
    /// ([`Document::span`]), which tile the input, so every
    /// diagnostic still belongs to exactly one document.
    ///
    /// Cost: the error-node check is O(log documents) (found once per
    /// parse); the diagnostic check costs what [`Document::diagnostics`]
    /// costs (O(log n) untouched, O(the document) after edits).
    #[must_use]
    pub fn has_errors(&self) -> bool {
        let s = &self.shared;
        if self
            .index
            .is_some_and(|i| s.error_docs.binary_search(&i).is_ok())
            || s.settings_error()
        {
            return true;
        }
        // From the complete record, never the capped
        // list: the same answer whatever `max_diagnostics`.
        let error = |d: &Diagnostic| d.severity == Severity::Error;
        match (self.index, s.edited) {
            (None, true) => s.tree.has_errors() || s.meaning().list.iter().any(error),
            (Some(i), true) => s.edited_doc_events(i).0.iter().any(error),
            (index, false) => !s.marks(index).is_empty() || s.runs(index).len() > 0,
        }
    }

    /// This document's errors, complete (whatever
    /// `max_diagnostics`): code and span. For restricted mode.
    fn own_errors(&self) -> Vec<(Code, Span)> {
        let s = &self.shared;
        let mut out: Vec<(Code, Span)> = s
            .settings_diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| (d.code, d.primary.span))
            .collect();
        let key = |d: &Diagnostic| (d.code, d.primary.span);
        match (self.index, s.edited) {
            (None, true) => {
                out.extend(
                    s.tree
                        .diagnostics()
                        .filter(|d| d.severity == Severity::Error)
                        .map(|d| key(&d)),
                );
                let found = s.meaning();
                out.extend(
                    found
                        .list
                        .iter()
                        .filter(|d| d.severity == Severity::Error)
                        .map(key),
                );
                out.extend(found.dropped_errors);
            }
            (Some(i), true) => out.extend(
                s.edited_doc_events(i)
                    .0
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .map(key),
            ),
            (index, false) => {
                let code = invalid_code(s.origin.encoder.encoding());
                out.extend(s.runs(index).map(|r| (code, r.text)));
                out.extend(s.marks(index).iter().map(|m| (m.2, Span::new(m.0, m.1))));
            }
        }
        out
    }

    /// The syntax tree underneath: the escape hatch for tooling.
    ///
    /// ```
    /// let doc = oneyaml::Document::parse("a: 1 # one\n").unwrap();
    /// assert_eq!(doc.syntax().emit(), doc.to_string());
    /// ```
    ///
    /// **Outside the stability promise**: `oneyaml-syntax` has its own
    /// semver track. For a document of a [`Stream`], the tree is the whole
    /// stream's.
    #[must_use]
    pub fn syntax(&self) -> &SyntaxTree {
        &self.shared.tree
    }

    fn doc_node(&self) -> Option<TreeNode<'_>> {
        self.shared.tree.get(self.doc?).ok()
    }

    /// The span of this document's text: the whole input for
    /// [`Document::parse`]; for a document of a [`Stream`], its **region**
    /// of the stream's text, which is exactly what
    /// `Document::to_string` and [`Document::to_bytes`] write.
    ///
    /// **A document's region** holds the document with its own comments
    /// and line breaks: its header comments (before its `---`, or before
    /// its root), its directives and `---`, its content, the trailing
    /// comment of its last line, its footer comments, and its `...` —
    /// exactly the comments [`Document::explain_comments`] reports
    /// (rulebook section 4 gives every comment between two documents to
    /// one of them). The regions of a stream's documents are consecutive
    /// and cover its text: the first document's starts at the start of
    /// the text, the last one's ends at its end, and between two documents
    /// the text is split at the start of the line after the last line the
    /// earlier document owns; blank lines after that line go with the next
    /// document (rule 8: blank lines travel with what follows them), as
    /// does a stray `...` between them. So each document's text parses
    /// again, on its own, to the same comments with the same owners and
    /// blank lines, and the same bytes (checked over the
    /// yaml-test-suite), before and after edits. In recovered input whose
    /// documents begin or end with trivia of their neighbour (rulebook
    /// section 5) the split is best effort.
    ///
    /// **Coordinates.** Every span a document reports — [`NodeRef::span`],
    /// [`Comment::span`], [`CommentExplanation::span`] and
    /// [`CommentExplanation::owner_span`], diagnostics — is a byte offset
    /// in the text of the whole input (for a stream document, the
    /// stream's `Stream::to_string`); subtract `span().start` to index
    /// the document's own `Document::to_string`.
    ///
    /// Cost: O(1) for [`Document::parse`]; for a stream document, O(the
    /// comments and stream-level text between it and its neighbours).
    ///
    /// ```
    /// let stream = oneyaml::Stream::parse("# head\n\na: 1 # t\n---\nb: 2 # u\n# foot\n").unwrap();
    /// let second = stream.document(1).unwrap();
    /// assert_eq!(second.to_string(), "---\nb: 2 # u\n# foot\n");
    /// let region = second.span();
    /// assert_eq!(&stream.to_string()[region.start..region.end], second.to_string());
    /// // Spans are the stream's: shift by the region's start for the
    /// // document's own text.
    /// let u = &second.explain_comments().unwrap()[0];
    /// let own = u.span().start - region.start;
    /// assert_eq!(&second.to_string()[own..own + 3], "# u");
    /// ```
    #[must_use]
    pub fn span(&self) -> Span {
        if self.index.is_none() {
            return self.shared.tree.root().span();
        }
        self.doc_node()
            .map_or(Span::empty(0), |d| self.shared.region_of(d))
    }

    /// The root node; `None` for an empty stream.
    #[must_use]
    pub fn root(&self) -> Option<NodeRef<'_>> {
        let doc = self.doc_node()?;
        doc.children()
            .find(|c| c.kind().is_yaml_node())
            .map(|n| NodeRef::new(self, view::presented(n)))
    }

    /// The error regions of a restricted document, in source
    /// order: read-only [`NodeKind::Error`] nodes, each with its
    /// [`NodeRef::diagnostic`]. Navigation meets them where they replaced
    /// a node (e.g. the value of `a: 'unclosed`).
    pub fn errors(&self) -> impl Iterator<Item = NodeRef<'_>> + '_ {
        let top = if self.index.is_none() {
            Some(self.shared.tree.root())
        } else {
            self.doc_node()
        };
        top.into_iter()
            .flat_map(TreeNode::descendants)
            .filter(|n| n.kind() == SyntaxKind::Error)
            .map(|n| NodeRef::new(self, n))
    }

    /// The root mapping's entry for `key` (any [`AsKey`]: `&str`,
    /// `String`, an integer, `bool`, a [`Value`]; see [`NodeRef::get`]).
    ///
    /// For the resolved meaning at a key (aliases expanded, scalars
    /// typed), read [`Document::to_value`] or [`NodeRef::deserialize`].
    #[must_use]
    pub fn get<K: AsKey>(&self, key: K) -> Option<NodeRef<'_>> {
        self.root()?.get(key)
    }

    /// The node at `path` from the root; `None` when there is none.
    /// A read-only lookup, so an `Option`, as `get`;
    /// [`Document::get_path_mut`] says why a path names nothing.
    ///
    /// `path` is a path string (`"spec.ports[0]"`, see
    /// [`IntoPath`](crate::IntoPath)) or a [`Path`]; a malformed path
    /// string names nothing (`None`).
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let doc = Document::parse("spec:\n  ports: [80, 443]\n").unwrap();
    /// assert_eq!(doc.get_path("spec.ports[1]").unwrap().as_u64(), Some(443));
    /// assert_eq!(doc.get_path("spec.ports[1]"), doc.get_path(&path!["spec", "ports", 1]));
    /// assert!(doc.get_path("spec.ports[9]").is_none());
    /// ```
    #[must_use]
    pub fn get_path<P: IntoPath>(&self, path: P) -> Option<NodeRef<'_>> {
        self.root()?.get_path(path)
    }

    /// [`Document::get_path`] with the reason when the walk stops (for the
    /// edit API): a typed [`EditError`] naming the path prefix where it
    /// stopped.
    pub(super) fn walk_path(&self, path: &Path) -> Result<NodeRef<'_>, EditError> {
        let root = self
            .root()
            .ok_or_else(|| EditError::NotFound { at: Path::new() })?;
        root.walk(path)
    }

    /// The handle of the node at `path`; `None` when there is none (as
    /// [`Document::get_path`]).
    #[must_use]
    pub fn id_of<P: IntoPath>(&self, path: P) -> Option<NodeId> {
        self.get_path(path).map(|n| n.id())
    }

    /// The node `id` names.
    ///
    /// # Errors
    ///
    /// [`EditError::StaleNode`] when `id` names no node of this document.
    pub fn node(&self, id: NodeId) -> Result<NodeRef<'_>, EditError> {
        let stale = EditError::StaleNode;
        let n = self.shared.tree.get(id.0).map_err(|_| stale.clone())?;
        let inside = self
            .doc_node()
            .is_some_and(|d| n.ancestors().any(|a| a == d));
        if !inside || !(n.kind().is_yaml_node() || n.kind() == SyntaxKind::Error) {
            return Err(stale);
        }
        Ok(NodeRef::new(self, n))
    }

    /// The document's meaning, read through the same construction layer
    /// and resolver as serde: equal to `from_str::<Value>` of the
    /// same input, errors included. Also [`Value::try_from`].
    ///
    /// # Errors
    ///
    /// As [`crate::from_str`] with a [`Value`] target.
    pub fn to_value(&self) -> Result<Value, Error> {
        let tree = &self.shared.tree;
        let settings = &self.shared.settings;
        if let Some(e) = self.shared.pre_error(self.index) {
            return Err(e);
        }
        let doc = self.doc_node().filter(|_| self.index.is_some());
        let source = || {
            match doc {
                Some(d) => TreeSource::over(tree, d),
                None => TreeSource::new(tree),
            }
            .with_doc_tags(&self.shared.tags)
        };
        let lines = Some(&self.shared.lines);
        // The text (the whole stream's, which spans refer
        // to) is read only to locate an error: a read is O(this document).
        // Warnings discarded: at most 16 built.
        crate::de::read_discarding::<_, Value>(source, || self.shared.text(), settings, lines)
    }

    /// The document's bytes: exactly the input for an untouched document,
    /// in its original encoding (invalid bytes restored).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let text = self.to_string();
        let origin = &self.shared.origin;
        // The text is the source from `base` on (a stream document's
        // span); each invalid run inside it gets its original bytes back.
        let base = if self.index.is_none() {
            0
        } else {
            self.span().start
        };
        let end = base.saturating_add(text.len());
        let mut out = Vec::with_capacity(text.len());
        let mut at = 0usize;
        // The runs are in source order; binary-search this
        // document's instead of filtering the whole stream's.
        let lo = origin.invalid.first_at(base);
        let inside = origin
            .invalid
            .iter()
            .skip(lo)
            .take_while(|r| r.text.start <= end)
            .filter(|r| r.text.end <= end);
        for run in inside {
            let start = run.text.start.saturating_sub(base);
            let before = text.get(at..start).unwrap_or("");
            out.extend(origin.encoder.encode(before));
            out.extend_from_slice(run.bytes);
            at = run.text.end.saturating_sub(base);
        }
        out.extend(origin.encoder.encode(text.get(at..).unwrap_or("")));
        out
    }

    /// The encoding the input was read in.
    #[must_use]
    pub fn encoding(&self) -> Encoding {
        self.shared.origin.encoder.encoding()
    }

    /// How many own nodes (as the expander counts them) precede `node` in
    /// `document` (the alias budget at a point is based on the own
    /// nodes read before it). O(log n) once the position list is built
    /// (O(n), once per edit state, shared by every read).
    fn nodes_before(&self, document: TreeNode<'_>, node: TreeNode<'_>) -> usize {
        let list = self.shared.node_events.get_or_init(|| {
            self.shared
                .tree
                .root()
                .descendants()
                .enumerate()
                .filter(|(_, n)| crate::construct::tree::is_node_event(n.kind()))
                // Always fits: tree positions are `u32` (`Y2006` caps them).
                .filter_map(|(i, _)| u32::try_from(i).ok())
                .collect()
        });
        let before =
            |at: usize| list.partition_point(|&p| usize::try_from(p).is_ok_and(|p| p < at));
        before(node.preorder_index()).saturating_sub(before(document.preorder_index()))
    }

    /// The anchor index (built once, then maintained by edits).
    fn anchors(&self) -> &anchors::Index {
        self.shared.anchor_index()
    }

    /// This document's position range `(start, end)` in the tree (anchors
    /// are per document, YAML 1.2.2 section 3.2.2.2); the whole tree for
    /// an empty stream.
    fn doc_range(&self) -> (usize, usize) {
        match self.doc_node() {
            Some(d) => (
                d.preorder_index(),
                place::next_token(d).map_or(usize::MAX, TreeNode::preorder_index),
            ),
            None => (0, usize::MAX),
        }
    }
}

impl core::fmt::Display for Document {
    /// The document's text: exactly the input for an untouched document;
    /// for a document of a [`Stream`], its region of the stream's
    /// text (see [`Document::span`]), edited or not. a flat loop
    /// over the tokens; O(the document's region).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let tree = &self.shared.tree;
        let Some(d) = self.doc_node().filter(|_| self.index.is_some()) else {
            if self.index.is_none() {
                for n in tree.root().descendants() {
                    if n.is_token() {
                        f.write_str(&n.text())?;
                    }
                }
            }
            return Ok(());
        };
        let r = self.shared.region_of(d);
        // The stream's elements from the one after the previous document
        // (or the first) through the one before the next document (or the
        // last): the region lies inside them.
        let is_doc = |n: &TreeNode<'_>| n.kind() == SyntaxKind::Document;
        let mut first = d;
        while let Some(p) = first.prev_sibling().filter(|p| !is_doc(p)) {
            first = p;
        }
        let mut at = Some(first);
        while let Some(top) = at {
            if top != d && is_doc(&top) {
                break;
            }
            // The tokens tile the text, so a token starts
            // where the one before ended: one span per stream element, not
            // an O(log R) span per token of an edited tree.
            let mut pos = top.span().start;
            for n in top.descendants().filter(|n| n.is_token()) {
                let text = n.text();
                let start = pos;
                pos = pos.saturating_add(text.len());
                if r.start <= start && pos <= r.end {
                    f.write_str(&text)?;
                }
            }
            at = top.next_sibling();
        }
        Ok(())
    }
}

impl TryFrom<&Document> for Value {
    type Error = Error;

    /// [`Document::to_value`].
    fn try_from(doc: &Document) -> Result<Value, Error> {
        doc.to_value()
    }
}

#[cfg(test)]
mod tests;
