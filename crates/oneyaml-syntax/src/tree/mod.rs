//! The lossless syntax tree.
//!
//! [`SyntaxTree`] turns the lossless event stream into a tree that keeps
//! every byte of the input: parse → [`SyntaxTree::emit`] of an untouched
//! tree reproduces the input byte for byte. It is what `Document`, the edit
//! layer, comment attachment, and tooling build on.
//!
//! # Shape
//!
//! Elements are **nodes** (documents, collections, scalars, aliases, error
//! regions) and **tokens** (every byte: scalar text, indicators, brackets,
//! properties, white space, comments, …); see [`SyntaxKind`]. The tree
//! mirrors the event stream: a node per `…Start`/`…End` pair,
//! scalar, alias, and `Error` event, a token per event that covers bytes.
//! Two placements differ from the stream, both so that a node owns what
//! belongs to it:
//!
//! * a node's **properties** (`&a`, `!t`) and a block scalar's **header**,
//!   with the trivia between them, are children of that node (the stream
//!   emits them before the node's event, C3/C5);
//! * a document's **directives** are children of that document.
//!
//! Collections are flat: a mapping's children are its keys and values
//! (nodes) interleaved with `?`/`:`/`,` indicators and trivia, in source
//! order. Because the stream places every zero-width start and end "hugging
//! the content" (C4), no node begins or ends with white space, a line
//! break, or a comment: trivia always lies *between* nodes, at the
//! outermost level it can, which is what the comment attachment rules
//! decide over.
//!
//! **Errors (C6).** Every `Error` event becomes an
//! [`SyntaxKind::Error`] node holding the offending bytes; the lines skipped
//! while resynchronising follow it as [`SyntaxKind::Skipped`] tokens; the
//! repairs the parser synthesised (empty scalars, zero-width ends) are
//! ordinary nodes. Diagnostics are kept ([`SyntaxTree::diagnostics`],
//! [`Node::diagnostic`]).
//!
//! # Representation
//!
//! Our own arena, not `rowan`: the elements are stored **flat, in preorder**
//! (which is also source order), as parallel arrays of 9 bytes per element:
//!
//! * `kind`: one byte;
//! * `parent`: the parent's index (`u32`);
//! * `word` (`u32`): for a **token**, its start offset; for a **node**, the
//!   index one past its last descendant, so a subtree is the contiguous
//!   index range `index..word`.
//!
//! Nothing else is stored per element. Because tokens tile the source in
//! preorder (event contract C1), a token ends where the next token starts,
//! and a node spans from its first token to the end of its last one; a
//! zero-width node sits where the next element starts. First child, next
//! sibling, subtree skipping, and emitting are O(1) per step; spans are
//! O(1) except across a run of nodes that open at the same byte (bounded by
//! the grammar, in practice at most 4). Offsets beyond 4 GiB are carried by
//! a sparse list of the indices where a token's start crosses a multiple
//! of 2³² (empty for smaller inputs), so no input is out of range; the index
//! space holds up to about 4 billion elements, after which the rest of the
//! input becomes one [`SyntaxKind::Skipped`] token under an error node (the
//! tree still round-trips).
//!
//! What the tokens cannot say is kept sparsely: a block scalar's header
//! (chomping, detected indentation) and a tag's `%TAG` prefix, both exactly
//! as the event carried them; and the diagnostics. Scalar values, anchors,
//! and tags are read with the **same** readers the event stream uses,
//! e.g. [`Node::value`].
//!
//! # Handles
//!
//! [`Node`] is a borrowed view, cheap to copy. [`NodeId`] is a stable,
//! owned handle: element index, generation, and the issuing tree's stamp.
//! [`SyntaxTree::get`] returns [`StaleNode`] for a handle issued by another
//! tree (every tree, including a clone, has its own stamp) or whose
//! generation no longer matches its slot. A freshly built tree has every
//! slot at generation 0 and stores no generations at all. Edits never
//! reuse a slot: a removed or replaced element's handle stays stale, an
//! inserted element gets a new slot, and every other handle keeps
//! resolving to its element.
//!
//! **Forks**: [`SyntaxTree::fork`] copies a tree
//! *with* its handle identity. The fork gets a stamp of its own and
//! records its parent's stamp with the parent's next free slot; a slot
//! below that limit is named by the parent's stamp (or, below an earlier
//! limit, by an earlier ancestor's) in both trees, so a handle issued
//! before the fork resolves in both, to the same element, until a copy
//! removes it. Slots at or past the limit are allocated by each copy on
//! its own and named by that copy's stamp, so a handle issued after the
//! fork is stale in the other copy (never a wrong element). No per-copy
//! translation map exists: a chain of forks keeps one `(stamp, limit)`
//! pair per fork made after insertions whose range still holds a live
//! slot (dead ranges are dropped at the next fork), and
//! resolving is O(log) in their number.
//!
//! # Editing
//!
//! [`SyntaxTree::replace`], [`SyntaxTree::remove`],
//! [`SyntaxTree::insert_before`], [`SyntaxTree::insert_after`],
//! [`SyntaxTree::append_child`] (and their range forms) splice detached
//! [`Fragment`]s in and out;
//! [`SyntaxTree::replace_token`] rewrites one token;
//! [`SyntaxTree::replace_to_line_end`] and [`SyntaxTree::insert_line_after`]
//! edit through a line's end, placing its line break where the parser does
//! (a block scalar's text ends with its line break, other content's break
//! is a token at the outermost level that continues).
//! Fragments come from
//! text through the one parser ([`Fragment::parse_with`], or
//! [`SyntaxTree::parse_with`] then [`SyntaxTree::extract`]). An
//! edit never changes the parsed arrays: the first one adds a sparse
//! overlay, and each costs O(depth + log R + window) for R runs (a few per
//! edit), never O(document) below document level; reads stay local (see
//! `edit.rs`). An edited tree emits exactly the string splice of its edits.
//! Every edit returns `Result` and a refused edit changes nothing.
//!
//! **Reads of an edited tree**. A [`Node`] of an edited
//! tree carries the run it lies in, so sequential reads (iterating
//! children, descendants, a walk, kinds, token texts, [`SyntaxTree::emit`])
//! cost amortised O(1) per element, as in a parsed tree: a step inside a
//! run is the parsed arrays' arithmetic, and moving to the next run is one
//! treap step. Only a step that leaves a run reads the overlay's maps
//! (O(log R)), so a full read takes O(R) such lookups, whatever the size
//! of the document, and emit takes none (O(1) per run, and R is at most the
//! number of elements: O(n)). Random access (a span, a rank, a handle)
//! stays O(log R).
//!
//! # Well-formed edits
//!
//! An edited tree always agrees with its own text: the edit layer accepts
//! only **well-formed** edits and refuses the others with
//! [`TreeEditError::NotWellFormed`] (or [`TreeEditError::Limit`]). An
//! edit is well-formed when its **validation window**, read again through
//! the one parser under the tree's own [`SyntaxTree::settings`] **in the
//! window's real context**, reads as exactly the window's elements after
//! the edit: the same kinds, nesting, token texts, block scalar headers,
//! resolved tags, error nodes, and diagnostics (spans moved by the
//! window's offset). So the edited tree's error set equals its re-parse's;
//! an edit may sit next to an error region, but never changes how it
//! reads, and never removes or replaces an [`SyntaxKind::Error`] node
//! below document level (refused as `ReadOnly`). An accepted
//! edit inside a document thus keeps [`SyntaxTree::error_count`]. The
//! window is found from the edit's parent, going up past
//! scalars, aliases, error nodes, flow pairs, and documents to the nearest
//! **container**; a flow collection inside an implicit key (block or
//! flow) is replaced by the outermost such key, read in its mapping.
//!
//! **Recovered input** (the edited tree agrees with its
//! re-parse whole, diagnostics' labels and fixes included, with or
//! without errors; known exception: under `lenient_inline_colons`, a
//! document holding a failed directive block whose content ends in a
//! `Y0028` warning, see `compare_trees`). An error region reads as its document before it left
//! the parser (C6), what follows it may end it (a flow collection or quote
//! cut off by a marker), and its diagnostics have labels and fixes away
//! from their element (an unclosed `[`; a byte order mark that ended a
//! document is reported over the line it starts). A stream child is
//! **recovered** when it is an error node, or a document with an error in
//! its content (directive errors do not count). An edit that could change
//! how one reads, or move one of its labels, is refused with
//! [`TreeEditError::ReadOnly`] `{ kind: Error }`; accepted are:
//!
//! * in a recovered document, an edit whose block or flow window ends
//!   before every error of the document and every label and fix of them
//!   (at the element it peeks, or at its own end when it reads to its
//!   collection's end), and, for a flow window, outside any flow
//!   collection used as an implicit key the edit could change: none of the
//!   flow collections it lies in sits in a key's place (decided within
//!   `TRIVIA_WALK` trivia siblings, else taken as one), nor is followed by
//!   `:` on its line unless it spans lines both before and after the edit
//!   (an invalid key either way): the document is text without errors up
//!   to there, the window decides it as in a tree without errors, and the
//!   parser goes on from there as before the edit;
//! * next to a recovered stream child (trivia passed over), an edit it
//!   cannot read: one after it when it ends with `...` (or inside the next
//!   document, past its `---` at the start of a line), one before it when
//!   it starts with `---` at the start of a line or a line right after a
//!   document ended by `...` (before that marker); in each case when no
//!   label or fix of its errors (nor of the errors of the child before a
//!   clean neighbour: a mark's line) reaches the edit; and
//!   past clean documents only when a `---` or `...` right after a
//!   line break lies between (an unclosed quote reads on to such a line,
//!   past a byte order mark that ended its document for the stream, and
//!   past a marker after a mark), found within `TRIVIA_WALK` stream
//!   children walked back from the edit: recovered children met before
//!   any document are read whole by the edit's stream window and the walk
//!   goes on past them; the first one met past a document refuses;
//! * at stream level, a recovered stream child read whole by the window
//!   (inserted, or put back): its errors are compared, the window starts
//!   where reading starts afresh and ends where it does (a peeked
//!   document starting afresh, or the end of the text), and its labels
//!   lie inside the window.
//!
//! Directive errors are told from content errors only for a document
//! recorded with one (`facts.rs`), within `TRIVIA_WALK` of its first
//! children (past them it is taken as recovered), and only when its last
//! error has a directive code and it is not mixed (see below;
//! a failed directive block before the `---`, `Y0001`, may hold an
//! unclosed quote that reads on into the content); whether a neighbour's
//! errors reach an edit, and whether a window in a recovered document
//! ends before its errors, is decided from content errors only
//! (a directive error, `Y1004`, `Y0033` or `%YAML 2.x`, is zero-width
//! before its document's `---` and only directive errors precede it, so
//! the scans stop at the first). A document where an other error precedes
//! a directive error (a failed directive block kept with later
//! directives, recorded by `facts.rs`) is taken to reach every edit next
//! to it, and its own edits are refused (O(1), where scanning
//! its errors cost O(its directive errors)). The trivia shortcut and
//! the unvalidated comment replacement below are not taken there. In a
//! tree without errors none of these checks run (each edit records, in
//! O(removed), whether the removed text holds a line break, which only
//! they read).
//!
//! **Context.** Before the window's text the check writes what puts the
//! parser in the state the window's first element is read in: the
//! document's `%TAG` directives; for a flow collection, a synthetic block
//! entry at the column of the nearest enclosing block collection (flow
//! lines are indented past it, §7) and the collection's bracket; for a
//! block collection read from its start, a synthetic entry of its parent
//! collection (`_:`, `-`, `?`) at the parent's column, or the document's
//! `---` when it shares that line, and the collection's first line at its
//! column. `max_depth` is lowered by the window's depth below the tree's
//! root, so depth counts from the root.
//!
//! **Extent** (the changed text plus the lines that can interact
//! with it, never a neighbour's whole subtree):
//!
//! * **block mapping or sequence**: from the start of the entry the edit
//!   is in (a key, `?`, or `-` first on its line at the entry column, or
//!   the first child) or, for an edit after the last value of an entry,
//!   from the last entry of the deepest block collection that value ends
//!   with; to the first **synchronising element** after the edit: the
//!   next entry start, or a value right after a `:` that lies after the
//!   edit (a block collection only on a later line); never an error node,
//!   a zero-width element, or the entry that sets the column. Of that
//!   element, where it starts and its first token are compared (its first
//!   line is read). Without one, the window reaches the collection's end
//!   and takes the trivia after it up to a comment at the collection's
//!   column or less (nothing inside can read past that), or through an
//!   error region that follows; those tokens are compared one by one, and
//!   outside error regions no node but the stream may end with them (C4);
//!   an empty node among them is passed over unless it carries
//!   diagnostics, which are then compared (a `Y0028` of a
//!   lenient value attached to the empty value of an enclosing `? `
//!   entry, in any tree);
//! * **flow mapping or sequence**: from the entry the edit starts in (cut
//!   at `,`) to the next `,` after the edit, which is compared by where it
//!   starts, or to the closing bracket;
//! * **stream** (edits at document or stream level): from the documents
//!   the edit touches (or from the document before, unless the first
//!   starts a line right after a document without diagnostics and starts
//!   with `---` or follows `...`) to the next document, peeked when its
//!   first line is untouched and it does not start with an error, else
//!   read whole; trivia after the last document read is taken as for
//!   block collections (a comment ends what a plain scalar could read).
//!   What reads the text after a document or collection is its last
//!   token with bytes, or an empty block scalar's text (a
//!   zero-width last token, the empty value of a `? |` entry, hid a block
//!   scalar whose header detects its indentation from the blank lines
//!   after it); found down the last children, a zero-width sibling (with
//!   its subtree) passed over in one step, at most `TRIVIA_WALK` of them
//!   in a row at one level, else the edit is refused (O(depth),
//!   so nested empty keys at a document's end do not refuse it).
//!   Trivia inserted right after a document that ends with a block
//!   collection, in a tree without errors, is read with that collection's
//!   tail instead: the window of an edit at its end, when that window
//!   covers the inserted text (what follows such a document is
//!   trivia and documents starting with `---`, which read the same).
//!
//! **Warnings that leave.** A warning attached to the element right after
//! the removed elements, all of whose labels lie in the removed text (a
//! `Y0028` of a lenient `k: a: b` value, on the line break after it),
//! leaves with that text when the edit is refused with it: the edit is
//! then checked without it.
//!
//! **Shortcut.** An edit that only adds or removes white space, comments,
//! and line breaks inside an existing run of them is well-formed exactly
//! when the lines it touches lex, on their own, into those tokens, the
//! content before the run cannot read them (it is closed: a comment lies
//! between, or it is not a block scalar, and not a plain scalar when a
//! comment is removed), and the line of the content after it is
//! untouched: O(line), so trivia edits at document and stream level stay
//! local. Replacing a comment by a comment is always well-formed and is
//! not re-read in a tree without errors.
//!
//! **Limits.** `max_document_size` and, when documents are
//! inserted, `max_documents` are checked on the whole text before the
//! edit is applied (the tree keeps its document count, so this costs
//! O(removed), not O(documents)); `max_depth` (flow pairs count, as in
//! the parser) and `max_scalar_len` through the window.
//! A limit the re-read reports is [`TreeEditError::Limit`] with its code.
//!
//! **Cost.** O(depth + window); a window is local below document level,
//! and at document level O(the documents touched). Passing over a long run
//! of trivia after a cut costs O(run) steps without copying text (none
//! after a collection when the tree has no errors, hence no error region
//! that could follow the run). Diagnostics elsewhere in the document are
//! not visited: only those next to the edit and the document's errors,
//! which end its content, are checked for labels the edit would move. The
//! consequence, checked by the edit-script fuzz over arbitrary scripts and
//! by an edit sweep over the yaml-test-suite and the fuzz corpora,
//! recovered input included (`tests/reparse_sweep.rs`): after
//! every accepted edit, the whole text re-parses with exactly the edited
//! tree's diagnostics and structure ([`compare_trees`]). One exception,
//! by design (`max_diagnostics`): past the cap, the edited
//! tree may keep a diagnostic as a skeleton (code, severity, primary span,
//! label reach and a keyed fingerprint of its texts, labels and fixes)
//! where the re-parse has its texts; the two are equal on those fields,
//! which is what [`compare_trees`] compares for a skeleton.
//!
//! # Recursion
//!
//! Nothing here recurses: the builder keeps an explicit stack of open nodes,
//! traversals are index loops, and `Drop`, `Clone`, `Debug`, `PartialEq`,
//! and `Hash` work on the flat arrays.

mod arena;
mod attach;
mod build;
mod check;
mod cost;

/// Work counters for tests (behind the unstable `__internal-checks`
/// feature, outside the stability promise): the number of overlay
/// lookups, treap edges and compaction visits the tree has taken on this
/// thread since the last call. Tests use it to state that an edit costs
/// the same on a small and on a huge document, without measuring time.
#[cfg(feature = "__internal-checks")]
#[doc(hidden)]
pub fn take_work() -> u64 {
    cost::take_total()
}

/// Bytes of the edited text the tree has re-read on this thread to
/// validate edits since the last call (behind the unstable
/// `__internal-checks` feature, like [`take_work`]). A local edit re-reads
/// a number of bytes that depends on the edited region and its
/// neighbouring entries, never on the size of the document.
#[cfg(feature = "__internal-checks")]
#[doc(hidden)]
pub fn take_reread_bytes() -> u64 {
    cost::take_reread()
}

/// Bytes of scalar source text [`Node::value`] has decoded on this thread
/// since the last call (behind the unstable `__internal-checks` feature,
/// like [`take_work`]). A key lookup that can tell from the source text
/// that a key differs decodes nothing of it.
#[cfg(feature = "__internal-checks")]
#[doc(hidden)]
pub fn take_decoded_bytes() -> u64 {
    cost::take_decoded()
}
mod edit;
mod facts;
mod fragment;
mod kind;
mod line;
mod node;
mod runs;
mod seq;
mod slab;
mod stored;
mod validate;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use oneyaml_core::{Diagnostic, Settings, Span};

use crate::event::{BlockHeader, Event, Sourced};

pub use attach::{AttachRule, Attachment, CommentOwner};
pub use check::{TreeMismatch, check_tree, compare_trees};
pub use edit::{GroupMark, Inserted, Replaced, TreeEditError};
pub use fragment::Fragment;
pub use kind::SyntaxKind;
pub use node::{Ancestors, Children, Descendants, Node, Walk, WalkEvent};

use build::{Builder, Origin};
use stored::Stored;

/// `u32` index meaning "none" (the root's parent).
const NONE: u32 = u32::MAX;

/// 2³²: token starts are stored modulo this (see *Representation*).
const WRAP: u64 = 0x1_0000_0000;

/// What a node's event carried that its tokens cannot say (absolute
/// positions, exactly as the event had them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Extra {
    /// Start of the `%TAG` prefix the node's tag was resolved with.
    tag_prefix: Option<usize>,
    /// A block scalar's header.
    block: Option<BlockHeader>,
}

/// The stamp of the next tree built or cloned (see *Handles*).
///
/// 64 bits, so stamps never wrap in practice: a wrapped counter would give
/// a later tree an earlier tree's stamp, and that tree's stale handles
/// could then resolve into the wrong tree.
static NEXT_STAMP: AtomicU64 = AtomicU64::new(1);

/// A fresh stamp. Fails closed: once the counter is exhausted (after 2⁶⁴ − 1
/// trees), every new tree gets [`EXHAUSTED`], which resolves no handle.
///
/// The compare-exchange loop is what `AtomicU64::fetch_update` does; it is
/// written out because that method is deprecated on nightly (renamed
/// `try_update`, which is newer than the 1.85 MSRV) and docs.rs builds
/// with nightly.
fn new_stamp() -> u64 {
    let mut current = NEXT_STAMP.load(Ordering::Relaxed);
    loop {
        let Some(next) = current.checked_add(1) else {
            return EXHAUSTED;
        };
        match NEXT_STAMP.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(previous) => return previous,
            Err(actual) => current = actual,
        }
    }
}

/// The stamp of trees built after the stamp counter ran out.
const EXHAUSTED: u64 = 0;

/// A lossless syntax tree over an owned copy of its source (see the module
/// docs).
///
/// Build one with [`SyntaxTree::parse`], [`SyntaxTree::parse_with`], or
/// from an event stream with [`SyntaxTree::from_events`]. Building never
/// fails and never panics; problems in the input are
/// [`SyntaxKind::Error`] nodes and [`SyntaxTree::diagnostics`].
///
/// ```
/// use oneyaml_syntax::tree::{SyntaxKind, SyntaxTree};
///
/// let tree = SyntaxTree::parse("a: [1, 2] # c\n");
/// assert_eq!(tree.emit(), "a: [1, 2] # c\n");
/// let doc = tree.root().first_child().unwrap();
/// assert_eq!(doc.kind(), SyntaxKind::Document);
/// let map = doc.first_child().unwrap();
/// assert_eq!(map.kind(), SyntaxKind::BlockMapping);
/// assert_eq!(map.text(), "a: [1, 2]");
/// ```
///
/// Equality and hashing compare the source and the structure, not the
/// stamp that ties [`NodeId`]s to one tree instance.
pub struct SyntaxTree {
    source: String,
    kinds: Vec<SyntaxKind>,
    parents: Vec<u32>,
    words: Vec<u32>,
    /// Indices at which token starts cross a multiple of 2³², one entry
    /// per crossing; empty below 4 GiB.
    wraps: Vec<u32>,
    /// Sorted by node index.
    extras: Vec<(u32, Extra)>,
    /// Sorted by element index: an error's is its `Error` node, a
    /// warning's is the element that followed it.
    diagnostics: Vec<(u32, Stored)>,
    /// `Document` children of the stream, kept up to date by edits (so
    /// `max_documents` is checked against it in O(1)).
    documents: usize,
    /// Diagnostics of the parse kept as skeletons (past
    /// `Limits::max_diagnostics`); edits never change it.
    suppressed: usize,
    /// Error-severity diagnostics in the tree, kept up to date by edits
    /// (a tree without errors has no error region, which lets a
    /// window skip looking for one past a run of trivia).
    errors: usize,
    /// Slots whose generation is not 0; empty for a freshly built tree.
    ///
    /// The edit layer never bumps a generation,
    /// because it never reuses a slot (a removed element's slot is simply
    /// not live, and resolves to [`StaleNode`]); the map stays empty and
    /// allocates nothing. It is the one place a [`NodeId`]'s `generation`
    /// is checked against, kept for a later compaction that reuses slots
    /// (which would retire them here instead of leaving them dead).
    generations: BTreeMap<u32, u32>,
    stamp: u64,
    /// Handle identity across [`SyntaxTree::fork`]:
    /// `(stamp, limit)` of each ancestor this tree was forked
    /// from, oldest first, limits strictly increasing. Slots below the
    /// first limit are named by the first ancestor's stamp, slots from one
    /// limit to the next by the next ancestor's, and the rest by `stamp`.
    /// Empty (no allocation) for a tree that was never forked.
    ///
    /// Compacted at every fork: a pair whose range
    /// holds no live slot any more is dropped (its range joins the next
    /// owner's; its slots are dead for good, so no handle of either owner
    /// resolves there). So a tree keeps at most one pair per range of
    /// slots it still holds, bounded by its live content, never by the
    /// number of forks in its history.
    ancestry: Vec<(u64, u32)>,
    /// The first slot an overlay created from now on may issue: a
    /// rolled-back edit group that created the overlay drops it,
    /// and the slots the group issued must never be issued again. 0 until
    /// such a rollback.
    slot_floor: u32,
    /// Long stream-level trivia runs and documents with directive errors
    /// of the parsed arrays (`facts.rs`; empty, no allocation, for most
    /// trees).
    facts: facts::StreamFacts,
    /// The comment-run index of the parsed arrays (`runs.rs`;
    /// empty, no allocation, without a long run of comment lines).
    run_facts: runs::RunFacts,
    /// The settings the tree was parsed with; edits re-read text under
    /// them (never the defaults).
    settings: Settings,
    /// The edit overlay (`edit.rs`); `None` until the first edit, so the
    /// read path of a parsed tree is unchanged.
    edits: Option<Box<edit::Edits>>,
    /// The accepted edits of the open edit groups
    /// ([`SyntaxTree::begin_group`]), to be undone together; empty (no
    /// allocation) outside a group.
    group: edit::Group,
}

/// A stable handle to an element of one [`SyntaxTree`].
///
/// Resolve it with [`SyntaxTree::get`]; a handle that no longer names a
/// live element of that tree yields [`StaleNode`], never a wrong element.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId {
    tree: u64,
    index: u32,
    generation: u32,
}

/// A [`NodeId`] that names no live element of the tree it was used with:
/// issued by another tree, or its element was replaced or removed.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StaleNode {
    /// The handle that failed to resolve.
    pub id: NodeId,
}

impl core::fmt::Display for StaleNode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("stale node handle: the element no longer exists in this tree")
    }
}

impl std::error::Error for StaleNode {}

impl SyntaxTree {
    /// Parses `source` under the default [`Settings`] and builds its tree.
    ///
    /// Pass a `String` to hand over its buffer; a `&str` is copied.
    #[must_use]
    pub fn parse(source: impl Into<String>) -> Self {
        Self::parse_with(source, &Settings::new())
    }

    /// Parses `source` under `settings` and builds its tree from the pull
    /// parser's events as they come, without collecting them.
    #[must_use]
    pub fn parse_with(source: impl Into<String>, settings: &Settings) -> Self {
        let source = source.into();
        let parts = {
            let mut builder = Builder::new(source.len(), Origin::Parser)
                .max_diagnostics(settings.get_limits().get_max_diagnostics());
            for event in crate::parser::events_with(&source, settings) {
                builder.push(event.into_event());
            }
            builder.finish()
        };
        Self::from_parts(source, parts, settings.clone())
    }

    /// Builds the tree of an event stream, e.g. `list.sourced()` of an
    /// [`EventList`](crate::event::EventList) or
    /// [`events_with`](crate::parser::events_with), copying its source.
    ///
    /// A stream from the parser always satisfies the event contract, and
    /// its tree always round-trips. A stream that was filtered or reordered
    /// still builds without panicking; the tree then records a `Y9001`
    /// ([`INVALID_EVENT_STREAM`](oneyaml_core::code::INVALID_EVENT_STREAM))
    /// diagnostic, covers any bytes the stream skipped with
    /// [`SyntaxKind::Skipped`] tokens, and its spans follow the tokens it
    /// was given.
    #[must_use]
    pub fn from_events<'a, E, I>(events: I) -> Self
    where
        E: core::borrow::Borrow<Event<'a>>,
        I: IntoIterator<Item = Sourced<'a, E>>,
    {
        let mut events = events.into_iter().peekable();
        let source = events.peek().map_or("", Sourced::source);
        let mut builder = Builder::new(source.len(), Origin::Caller);
        for event in events {
            builder.push(event.event().clone());
        }
        let parts = builder.finish();
        Self::from_parts(source.to_owned(), parts, Settings::new())
    }

    fn from_parts(source: String, parts: build::Parts, settings: Settings) -> Self {
        let errors = parts
            .diagnostics
            .iter()
            .filter(|(_, d)| d.is_error())
            .count();
        let mut tree = Self {
            source,
            kinds: parts.kinds,
            parents: parts.parents,
            words: parts.words,
            wraps: parts.wraps,
            extras: parts.extras,
            diagnostics: parts.diagnostics,
            documents: parts.documents,
            suppressed: parts.suppressed,
            errors,
            generations: BTreeMap::new(),
            stamp: new_stamp(),
            ancestry: Vec::new(),
            slot_floor: 0,
            facts: facts::StreamFacts::default(),
            run_facts: runs::RunFacts::default(),
            settings,
            edits: None,
            group: edit::Group::default(),
        };
        let flat = tree.base_flat();
        let mut child = flat.first_child(0);
        let children = core::iter::from_fn(move || {
            let c = child?;
            child = flat.next_sibling(c);
            Some(c)
        });
        tree.facts = facts::StreamFacts::of(flat, children, tree.errors > 0);
        tree.run_facts = runs::RunFacts::of(tree.base_flat());
        tree
    }

    /// The settings the tree was parsed with ([`Settings::new`] for a tree
    /// built with [`SyntaxTree::from_events`]). Edits re-read text under
    /// them.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The source text the tree was built from (unchanged by edits: see
    /// [`SyntaxTree::emit`] for the current text).
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The [`SyntaxKind::Stream`] node spanning the whole input.
    #[must_use]
    pub fn root(&self) -> Node<'_> {
        // (O(1), without a hint; the iterators find theirs once.)
        Node::new(self, 0)
    }

    /// Number of elements (nodes and tokens).
    #[must_use]
    pub fn len(&self) -> usize {
        if self.edits.is_some() {
            return usize::try_from(self.nav_total().0).unwrap_or(usize::MAX);
        }
        self.kinds.len()
    }

    /// `true` when the tree has no elements, which never happens for a
    /// built tree (the root always exists).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        // The same count as [`SyntaxTree::len`].
        self.len() == 0
    }

    /// The element `id` names, or [`StaleNode`].
    ///
    /// # Errors
    ///
    /// [`StaleNode`] when `id` was issued by another tree or its element no
    /// longer exists.
    pub fn get(&self, id: NodeId) -> Result<Node<'_>, StaleNode> {
        // A handle names a slot of the tree, or of the
        // ancestor it was forked from before the fork; O(log A) for A
        // ancestors, O(1) for a tree never forked.
        let named = id.tree == self.owner(id.index)
            && id.tree != EXHAUSTED
            && self.stamp != EXHAUSTED
            && self.generation(id.index) == id.generation;
        // The run lookup that finds the slot live gives its
        // hint (one O(log R) lookup, as before).
        let hint = if !named {
            None
        } else if self.edits.is_some() {
            self.run_item(id.index).map(|(item, _, _)| item)
        } else {
            self.is_live(id.index).then_some(NONE)
        };
        match hint {
            Some(h) => Ok(Node::hinted(self, id.index, h)),
            None => Err(StaleNode { id }),
        }
    }

    /// The live element named by `slot` ([`Node::slot`]), or `None` once
    /// it was removed or replaced (or for a slot this tree never issued).
    /// The same element as [`SyntaxTree::get`] resolves the handle of an
    /// element of this tree to. O(1); O(log R) in an edited tree.
    #[must_use]
    pub fn node_at_slot(&self, slot: u32) -> Option<Node<'_>> {
        if slot == NONE {
            return None;
        }
        if self.edits.is_some() {
            // The lookup that finds the slot live gives its hint.
            return self
                .run_item(slot)
                .map(|(item, _, _)| Node::hinted(self, slot, item));
        }
        self.is_live(slot).then(|| Node::new(self, slot))
    }

    /// Every diagnostic, errors and warnings, in element order (see
    /// [`SyntaxTree::diagnostics_by_element`]). Borrowed from a parsed
    /// tree; an edited tree's are built with their spans moved to the
    /// edited text (O(d + D log R) for the D runs holding diagnostics, no
    /// cache). Complete: past `max_diagnostics` they are skeletons
    /// ([`SyntaxTree::is_skeleton`]).
    pub fn diagnostics(&self) -> impl ExactSizeIterator<Item = Cow<'_, Diagnostic>> + '_ {
        self.diagnostics_by_element().map(|(_, d)| d)
    }

    /// How many of the parse's diagnostics are kept as skeletons:
    /// a tree keeps the first `Limits::max_diagnostics + 1`
    /// diagnostics of its parse in stream order with their texts, and the
    /// first error wherever it is; every other one as a fixed-size
    /// **skeleton** (32 bytes, no heap). So the tree's record of what the
    /// parse found (every error, its position, its error node) is complete
    /// whatever the limit; only texts are not stored. Edits never change
    /// the count.
    #[must_use]
    pub const fn suppressed_diagnostics(&self) -> usize {
        self.suppressed
    }

    /// Whether `d` is a skeleton (see
    /// [`SyntaxTree::suppressed_diagnostics`]). Readers see a skeleton as
    /// a diagnostic with its code, severity, and primary span, and empty
    /// texts (no diagnostic with texts has an empty message), and nothing
    /// else: how the tree compares skeletons with re-read diagnostics is
    /// internal and never exposed.
    #[must_use]
    pub fn is_skeleton(d: &Diagnostic) -> bool {
        d.message.is_empty()
    }

    /// The element a diagnostic is attached to.
    fn diag_node(&self, i: u32) -> Option<Node<'_>> {
        let live = if self.edits.is_some() {
            i != NONE
        } else {
            (i as usize) < self.kinds.len()
        };
        live.then(|| Node::new(self, i))
    }

    /// Every diagnostic with the element it is attached to, in element
    /// order: an error's [`SyntaxKind::Error`] node, a warning's element
    /// that followed it in the event stream, or `None` for a warning after
    /// the last element. Consumers that read the tree as an event stream
    /// use this to see warnings where the stream reported them.
    ///
    /// Borrowed from a parsed tree. An edited tree's are built on each call
    /// from the arenas' own lists, spans moved to the edited text: O(d + D
    /// (log d + log R)) for d diagnostics, R runs, and the D runs holding
    /// any (the others are skipped by the treap's sums), and
    /// nothing is cached.
    pub fn diagnostics_by_element(
        &self,
    ) -> impl ExactSizeIterator<Item = (Option<Node<'_>>, Cow<'_, Diagnostic>)> + '_ {
        let source = match self.edits.as_deref() {
            Some(e) => DiagSource::Built(self.edited_diagnostics(e, None).into_iter()),
            None => DiagSource::Parsed(self.diagnostics.iter()),
        };
        DiagIter {
            tree: self,
            source,
            fill: &[],
        }
    }

    /// The diagnostics attached to elements of `node`'s subtree (see
    /// [`SyntaxTree::diagnostics_by_element`]), in element order. Found by
    /// binary search: O(log d + k) for k results, so reading one
    /// document of a stream does not scan the others' diagnostics; in an
    /// edited tree O(D' (log d + log R) + k) for the D' runs of the subtree
    /// holding diagnostics (never the subtree's other runs), with
    /// nothing rebuilt for the rest of the tree.
    pub fn diagnostics_within<'s>(
        &'s self,
        node: Node<'s>,
    ) -> impl ExactSizeIterator<Item = (Option<Node<'s>>, Cow<'s, Diagnostic>)> + 's {
        self.diagnostics_within_iter(node)
    }

    fn diagnostics_within_iter<'s>(&'s self, node: Node<'s>) -> DiagIter<'s> {
        let source = match self.edits.as_deref() {
            Some(e) => DiagSource::Built(self.edited_diagnostics(e, Some(node.slot())).into_iter()),
            None => {
                let first = node.preorder_index();
                let past = self.end_of(first);
                let lo = self
                    .diagnostics
                    .partition_point(|(n, _)| (*n as usize) < first);
                let hi = self
                    .diagnostics
                    .partition_point(|(n, _)| (*n as usize) < past)
                    .max(lo);
                DiagSource::Parsed(self.diagnostics.get(lo..hi).unwrap_or_default().iter())
            }
        };
        DiagIter {
            tree: self,
            source,
            fill: &[],
        }
    }

    /// [`SyntaxTree::diagnostics_within`], with each skeleton
    /// ([`SyntaxTree::is_skeleton`]) that a diagnostic of `fill` records
    /// read as that diagnostic, texts included: `fill`
    /// holds diagnostics with texts found by reading the same text again
    /// (with spans in this tree's text), **sorted by start**. A skeleton
    /// records a diagnostic when they have the same code, severity,
    /// primary span, label reach, and keyed fingerprint of everything the
    /// skeleton does not keep (as the tree compares them when it re-reads
    /// an edit's text), so a skeleton is never given another
    /// diagnostic's texts; one that none records stays a skeleton. Cost:
    /// as [`SyntaxTree::diagnostics_within`], plus O(log f) per skeleton.
    pub fn diagnostics_within_filled<'s>(
        &'s self,
        node: Node<'s>,
        fill: &'s [Diagnostic],
    ) -> impl ExactSizeIterator<Item = (Option<Node<'s>>, Cow<'s, Diagnostic>)> + 's {
        DiagIter {
            fill,
            ..self.diagnostics_within_iter(node)
        }
    }

    /// `true` when the tree has at least one error. O(1): the count is
    /// kept by every edit.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// A copy that shares this tree's **handle identity**:
    /// every [`NodeId`] issued before the fork, by this tree
    /// or the copy, resolves in both to the same element for as long as
    /// that copy keeps it, and [`Node::id`] of such an element is the same
    /// value in both. Elements inserted after the fork get handles of
    /// their own tree, stale in the other; an element removed from one
    /// copy is stale there only. Forks of forks work the same way.
    ///
    /// [`Clone`] gives an independent tree instead (no handle of the
    /// original resolves in it).
    ///
    /// Cost: O(n + L) for the parsed text and arrays (n) and the live
    /// edits (L: the inserted arenas some run still covers, the runs, the
    /// link overrides of live slots; never the edit history,
    /// since text removed by accepted edits is dropped with its arena),
    /// plus one `(stamp, slot)` pair of metadata per ancestor range that
    /// still holds a live slot (no per-copy translation map, pairs of
    /// ranges without live slots are dropped); resolving a handle is
    /// O(log A) for A such pairs.
    #[must_use]
    pub fn fork(&self) -> Self {
        let mut copy = self.clone();
        // Only the ranges that still hold a live
        // slot keep their pair (O(A log R)).
        let mut lo = 0u32;
        for &(stamp, limit) in &self.ancestry {
            if self.holds_live(lo, limit) {
                copy.ancestry.push((stamp, limit));
            }
            lo = limit;
        }
        let limit = self.next_slot();
        // An ancestor that issued no slot since the previous fork owns an
        // empty range: it is not recorded (chains of forks grow only
        // with the edits between them); nor one whose slots are all dead.
        if self.holds_live(lo, limit) {
            copy.ancestry.push((self.stamp, limit));
        }
        copy
    }

    /// The text of the tree as [`SyntaxTree::emit`] gives it, borrowed from
    /// the tree when it was not edited (nothing is copied) and built run by
    /// run when it was.
    #[must_use]
    pub fn text(&self) -> Cow<'_, str> {
        match self.edits.as_deref() {
            Some(e) => Cow::Owned(self.edited_emit(e)),
            None => Cow::Borrowed(&self.source),
        }
    }

    /// The text of the tree: its tokens' bytes in order. For a tree that
    /// was not edited this is exactly the input; for an edited tree,
    /// exactly the input with each edit's bytes spliced in. O(n) bytes
    /// copied, at memory speed: an untouched tree's text is one copy of its
    /// input, an edited tree's is copied run by run, O(1) per run beyond its
    /// bytes, with no map lookup (a run holds at least one element).
    #[must_use]
    pub fn emit(&self) -> String {
        if let Some(e) = self.edits.as_deref() {
            return self.edited_emit(e);
        }
        // The tokens of a tree that was not edited tile its input, so the
        // text is the input: one copy, no walk over the arena.
        self.source.clone()
    }

    // ------------------------------------------------------------ storage

    fn generation(&self, index: u32) -> u32 {
        self.generations.get(&index).copied().unwrap_or(0)
    }

    #[inline]
    fn kind_at(&self, i: usize) -> Option<SyntaxKind> {
        self.kinds.get(i).copied()
    }

    /// The parent index of `i`, if any.
    #[inline]
    fn parent_of(&self, i: usize) -> Option<usize> {
        match self.parents.get(i) {
            Some(&p) if p != NONE => Some(p as usize),
            _ => None,
        }
    }

    /// One past the last index of `i`'s subtree.
    #[inline]
    fn end_of(&self, i: usize) -> usize {
        match self.kind_at(i) {
            Some(k) if !k.is_token() => self.words.get(i).map_or(i, |w| *w as usize),
            _ => i.saturating_add(1),
        }
    }

    /// The start offset of token `i`.
    fn token_start(&self, i: usize) -> usize {
        let low = u64::from(self.words.get(i).copied().unwrap_or(0));
        if self.wraps.is_empty() {
            return usize::try_from(low).unwrap_or(usize::MAX);
        }
        let crossings = self.wraps.partition_point(|w| *w as usize <= i);
        let high = u64::try_from(crossings)
            .ok()
            .and_then(|c| c.checked_mul(WRAP));
        high.and_then(|h| h.checked_add(low))
            .and_then(|at| usize::try_from(at).ok())
            .unwrap_or(usize::MAX)
    }

    /// Where element `i` starts: its own start for a token, the start of
    /// the next token in preorder for a node (tiling, C1), or the end of
    /// the input.
    fn start_of(&self, mut i: usize) -> usize {
        loop {
            match self.kind_at(i) {
                None => return self.source.len(),
                Some(k) if k.is_token() => return self.token_start(i),
                Some(_) => i = i.saturating_add(1),
            }
        }
    }

    /// The span of element `i`.
    fn span_of(&self, i: usize) -> Span {
        let start = self.start_of(i);
        let end = self.start_of(self.end_of(i)).max(start);
        Span::new(start, end)
    }

    fn extra(&self, i: usize) -> Option<&Extra> {
        let at = self
            .extras
            .binary_search_by_key(&i, |(n, _)| *n as usize)
            .ok()?;
        self.extras.get(at).map(|(_, e)| e)
    }

    fn diagnostic_at(&self, i: usize) -> Option<Cow<'_, Diagnostic>> {
        if self.edits.is_some() {
            let slot = u32::try_from(i).ok()?;
            return pick_error(self.diagnostics_of(slot).into_iter());
        }
        let first = self.diagnostics.partition_point(|(n, _)| (*n as usize) < i);
        // A warning just before an error is attached to the same element
        // (the one that followed it): the error node's own diagnostic is
        // the error.
        let at_i = self
            .diagnostics
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(|(n, _)| *n as usize == i)
            .map(|(_, d)| d.view());
        pick_error(at_i)
    }

    fn id_of(&self, index: u32) -> NodeId {
        NodeId {
            tree: self.owner(index),
            index,
            generation: self.generation(index),
        }
    }

    /// The stamp that names `index` in this tree (see `ancestry`).
    fn owner(&self, index: u32) -> u64 {
        let k = self.ancestry.partition_point(|(_, l)| *l <= index);
        self.ancestry.get(k).map_or(self.stamp, |(s, _)| *s)
    }

    /// Some slot in `lo..hi` is live. O(log R).
    fn holds_live(&self, lo: u32, hi: u32) -> bool {
        if lo >= hi {
            return false;
        }
        let Some(e) = self.edits.as_deref() else {
            return (lo as usize) < self.kinds.len();
        };
        // Runs never overlap in slot space: the last run starting below
        // `hi` is the only one that can reach into the range from below.
        e.live_in(lo, hi)
    }

    /// The first slot the next insertion takes.
    fn next_slot(&self) -> u32 {
        match self.edits.as_deref() {
            Some(e) => e.next_slot(),
            None => u32::try_from(self.kinds.len())
                .unwrap_or(NONE)
                .max(self.slot_floor),
        }
    }
}

/// Where [`DiagIter`] reads from.
enum DiagSource<'t> {
    /// A parsed tree's list, borrowed.
    Parsed(core::slice::Iter<'t, (u32, Stored)>),
    /// An edited tree's, built for this call.
    Built(std::vec::IntoIter<(u32, Stored)>),
}

/// Diagnostics with the elements they are attached to.
struct DiagIter<'t> {
    tree: &'t SyntaxTree,
    source: DiagSource<'t>,
    /// Diagnostics with texts, sorted by start, to read for the
    /// skeletons they record (see [`SyntaxTree::diagnostics_within_filled`]).
    fill: &'t [Diagnostic],
}

impl<'t> DiagIter<'t> {
    /// `d` as readers see it, or, for a skeleton, the diagnostic of `fill`
    /// it records (O(log f + those starting where it starts)).
    fn seen(&self, d: &Stored) -> Cow<'t, Diagnostic> {
        if !self.fill.is_empty() && !matches!(d, Stored::Full(_)) {
            let at = d.span().start;
            let lo = self.fill.partition_point(|c| c.primary.span.start < at);
            let found = self
                .fill
                .get(lo..)
                .unwrap_or_default()
                .iter()
                .take_while(|c| c.primary.span.start == at)
                .find(|c| stored::records(d, c));
            if let Some(c) = found {
                return Cow::Borrowed(c);
            }
        }
        Cow::Owned(d.view().into_owned())
    }
}

impl<'t> Iterator for DiagIter<'t> {
    type Item = (Option<Node<'t>>, Cow<'t, Diagnostic>);

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.source {
            DiagSource::Parsed(it) => {
                let (i, d) = it.next()?;
                // (without `fill`, exactly the reads of the plain path,
                // at their cost.)
                let seen = if self.fill.is_empty() || matches!(d, Stored::Full(_)) {
                    d.view()
                } else {
                    self.seen(d)
                };
                Some((self.tree.diag_node(*i), seen))
            }
            DiagSource::Built(it) => {
                let (i, d) = it.next()?;
                let seen = if self.fill.is_empty() || matches!(d, Stored::Full(_)) {
                    Cow::Owned(d.view().into_owned())
                } else {
                    self.seen(&d)
                };
                Some((self.tree.diag_node(i), seen))
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.source {
            DiagSource::Parsed(it) => it.size_hint(),
            DiagSource::Built(it) => it.size_hint(),
        }
    }
}

impl ExactSizeIterator for DiagIter<'_> {}

/// The error among one element's diagnostics, else the first warning.
fn pick_error<'a>(at_i: impl Iterator<Item = Cow<'a, Diagnostic>>) -> Option<Cow<'a, Diagnostic>> {
    let mut fallback = None;
    for d in at_i {
        if d.severity == oneyaml_core::Severity::Error {
            return Some(d);
        }
        fallback = fallback.or(Some(d));
    }
    fallback
}

impl Clone for SyntaxTree {
    /// An independent copy with its own stamp: handles of the original do
    /// not resolve in the copy (they may diverge once edited). See
    /// [`SyntaxTree::fork`] for a copy that keeps them.
    fn clone(&self) -> Self {
        let mut copy = Self {
            source: self.source.clone(),
            kinds: self.kinds.clone(),
            parents: self.parents.clone(),
            words: self.words.clone(),
            wraps: self.wraps.clone(),
            extras: self.extras.clone(),
            diagnostics: self.diagnostics.clone(),
            documents: self.documents,
            suppressed: self.suppressed,
            errors: self.errors,
            generations: self.generations.clone(),
            stamp: new_stamp(),
            ancestry: Vec::new(),
            facts: self.facts.clone(),
            run_facts: self.run_facts.clone(),
            settings: self.settings.clone(),
            slot_floor: self.slot_floor,
            edits: self.edits.clone(),
            // An open group belongs to the tree it was opened on: the copy
            // starts with none (its accepted edits are simply kept).
            group: edit::Group::default(),
        };
        // The copy keeps the group's edits as accepted
        // ones, so it merges and releases what the group held back.
        self.settle_group_copy(&mut copy);
        copy
    }
}

impl PartialEq for SyntaxTree {
    /// Structural equality ([`compare_trees`]): the same elements in the
    /// same shape, token texts, block headers, resolved tags, and
    /// diagnostics. Not the stamp, the generations, or how edits laid the
    /// tree out, so an edited tree equals the tree parsed from its
    /// [`SyntaxTree::emit`] exactly when they agree structurally.
    fn eq(&self, other: &Self) -> bool {
        // Fast path: identical parsed arrays.
        let raw = self.edits.is_none()
            && other.edits.is_none()
            && self.source == other.source
            && self.kinds == other.kinds
            && self.parents == other.parents
            && self.words == other.words
            && self.wraps == other.wraps
            && self.extras == other.extras
            && self.diagnostics == other.diagnostics;
        raw || compare_trees(self, other).is_ok()
    }
}

impl Eq for SyntaxTree {}

impl core::hash::Hash for SyntaxTree {
    /// Hashes what [`PartialEq`] compares (a flat walk).
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        check::for_each_item(self, |item| item.hash(state));
    }
}

impl core::fmt::Debug for SyntaxTree {
    /// One line per element in preorder, `depth kind@start..end`, with the
    /// text of short tokens. Iterative, and O(n) output whatever the depth
    /// (depth is printed as a number, not as indentation).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(f, "SyntaxTree ({} elements)", self.len())?;
        let mut depth = 0usize;
        for event in self.root().walk() {
            match event {
                WalkEvent::Enter(node) => {
                    let span = node.span();
                    write!(f, "{depth} {:?}@{}..{}", node.kind(), span.start, span.end)?;
                    if node.is_token() && span.len() <= 40 {
                        write!(f, " {:?}", node.text())?;
                    }
                    f.write_str("\n")?;
                    depth = depth.saturating_add(1);
                }
                WalkEvent::Leave(_) => depth = depth.saturating_sub(1),
            }
        }
        for d in self.diagnostics() {
            writeln!(f, "{d}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod read_tests;
