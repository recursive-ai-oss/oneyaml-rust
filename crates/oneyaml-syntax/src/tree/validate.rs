//! Well-formed edits: an edited tree always
//! agrees with its own text; validation reads the edit in its real
//! context; the edited tree's error set equals the re-parse's; `Limits`
//! apply to edits; validation cost is local; there is one parser, so one
//! interpretation).
//!
//! After an edit is applied, the text of its **validation window** is read
//! again through the one parser under the tree's settings, after a short
//! synthetic **context** that puts the parser in the state the window's
//! first element is read in, and the reading must be exactly the window's
//! elements, error nodes and diagnostics included. The rule is documented
//! in the module docs of [`super`] (*Well-formed edits*); this file
//! implements it. Everything here is O(depth + window), and a window is
//! bounded by the changed text plus the lines around it that can interact
//! with it.

use super::stored::Stored;
use std::borrow::Cow;

use oneyaml_core::code::{
    DEPTH_LIMIT_EXCEEDED, DOCUMENT_TOO_LARGE, DUPLICATE_DIRECTIVE, DUPLICATE_YAML_DIRECTIVE,
    SCALAR_TOO_LONG, TOO_MANY_DOCUMENTS, YAML_VERSION_UNSUPPORTED_MAJOR,
};
use oneyaml_core::{Code, Severity};

use super::edit::{DiagAt, Jump};
use super::{Node, SyntaxKind as K, SyntaxTree, TreeEditError, WalkEvent};
use crate::event::BlockHeader;

/// How far the trivia shortcut and the window builders walk over a run of
/// trivia before giving up on a shortcut (a constant, so a long run
/// of comments never makes an edit next to it cost the run).
const TRIVIA_WALK: usize = 64;

/// How far past the start of a peeked element's first token its peek reads
/// on that line: twice the parser's implicit-key window
/// (`KEY_WINDOW`, 1024 characters of up to 4 bytes), the farthest any
/// decision at a node's head looks ahead on its line. Without the bound,
/// an edit right before an entry written on one long line (`big: [0, 1,
/// …]`, 4M items) re-read that whole line (~1 s).
const PEEK_LOOKAHEAD: u64 = (crate::parser::KEY_WINDOW as u64).saturating_mul(2);

/// One step of the structural reading compared (as in
/// [`compare_trees`](super::compare_trees), offsets relative to the
/// window).
///
/// 40 bytes, not 168 (the rare parts are
/// boxed, and a token's text and a node's tag share a field: the kind
/// says which), and only our side of a window is materialised, the
/// re-read side being compared as it is read ([`Compare`]): a window of a
/// deep document held ~12 MB of items on each side, which fell out of
/// cache and made its cost grow faster than its size.
#[derive(Debug)]
enum Item<'t> {
    Enter {
        kind: K,
        /// A token's text; a node's resolved tag.
        text: Option<Cow<'t, str>>,
        header: Option<Box<BlockHeader>>,
    },
    Leave,
    Diagnostic(Stored),
    /// The element the window synchronises on: where it starts and its
    /// first token (how the rest of it reads depends on text after the
    /// window).
    Peek {
        at: usize,
        token: Option<(K, Cow<'t, str>)>,
    },
}

impl PartialEq for Item<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Enter { kind, text, header },
                Self::Enter {
                    kind: k2,
                    text: t2,
                    header: h2,
                },
            ) => kind == k2 && text == t2 && header == h2,
            (Self::Leave, Self::Leave) => true,
            (Self::Diagnostic(a), Self::Diagnostic(b)) => super::stored::same(a, b),
            (Self::Peek { at, token }, Self::Peek { at: a2, token: t2 }) => at == a2 && token == t2,
            _ => false,
        }
    }
}

/// Our side of a window, in order.
enum Part {
    /// Siblings, compared with their whole subtrees.
    Kids(Vec<u32>),
    /// The collection being read ends.
    Leave,
    /// Tokens after the collection, compared one by one wherever they sit
    /// (their placement is decided by text after the window, unchanged).
    Lone(Vec<u32>),
    /// The synchronising element.
    Peek(u32),
}

/// Where the re-read container is in the re-read tree.
#[derive(Clone, Copy)]
enum Shape {
    /// Stream windows: the re-read stream's children.
    Stream,
    /// The re-read document's root node, or (`skeleton`) the value of the
    /// synthetic entry the context wrote (flow windows inside block
    /// collections).
    Root {
        kind: K,
        skeleton: bool,
        /// A synthetic opening bracket was written before the window.
        open: bool,
        /// Block windows that restart inside the last entry's value: the
        /// kind of the deepest block collection they restart in, the
        /// value of the synthetic entry the context wrote.
        inner: Option<K>,
    },
}

/// The text and elements to re-read.
struct Window {
    /// Context written before the window (`%TAG` directives, `---`, a
    /// synthetic entry, the column).
    prefix: String,
    /// A flow collection's brackets, written back around the window.
    open: Option<char>,
    close: Option<char>,
    /// Bytes of the edited text re-read.
    start: u64,
    end: u64,
    ours: Vec<Part>,
    shape: Shape,
    /// The container's depth in the tree minus its depth in the re-read
    /// (`max_depth` counts from the tree's root).
    depth_offset: usize,
    /// The peek was cut short of its line's end
    /// ([`PEEK_LOOKAHEAD`]): the peeked element and the window's end before
    /// the peek, to re-read the whole line when the cut window does not
    /// read back (so no edit is refused that the whole line accepts).
    uncut: Option<(u32, u64)>,
    /// The flow collection a flow window reads in (`None` for
    /// other windows).
    flow: Option<u32>,
}

const fn is_block(k: K) -> bool {
    matches!(k, K::BlockMapping | K::BlockSequence)
}

const fn is_flow(k: K) -> bool {
    matches!(k, K::FlowMapping | K::FlowSequence)
}

const fn is_container(k: K) -> bool {
    is_block(k) || is_flow(k) || matches!(k, K::Stream)
}

/// Kinds the parser counts toward `max_depth` (a flow pair is the
/// single-pair mapping it starts).
const fn counts_depth(k: K) -> bool {
    is_block(k) || is_flow(k) || matches!(k, K::FlowPair)
}

/// Directive tokens (what may precede a document's `---`).
const fn is_directive_kind(k: K) -> bool {
    matches!(k, K::YamlDirective | K::TagDirective | K::ReservedDirective)
}

/// The codes of a document's directive errors, the parser's
/// soft errors among a document's directives (a duplicate `%YAML` or
/// `%TAG`, `%YAML 2.x`; `parser::stream`): zero-width at the end of their
/// directive, with that one label, never followed by an error region. No
/// other error of a document has these codes (`DIRECTIVE_WITHOUT_DOCUMENT`,
/// the other soft error, lies outside every document).
pub(super) const fn is_directive_error(code: Code) -> bool {
    matches!(
        code,
        DUPLICATE_DIRECTIVE | DUPLICATE_YAML_DIRECTIVE | YAML_VERSION_UNSUPPORTED_MAJOR
    )
}

const fn is_limit(code: Code) -> bool {
    matches!(
        code,
        DEPTH_LIMIT_EXCEEDED | SCALAR_TOO_LONG | DOCUMENT_TOO_LARGE | TOO_MANY_DOCUMENTS
    )
}

const fn open_of(kind: K) -> K {
    if matches!(kind, K::FlowMapping) {
        K::FlowMappingOpen
    } else {
        K::FlowSequenceOpen
    }
}

const fn close_of(kind: K) -> K {
    if matches!(kind, K::FlowMapping) {
        K::FlowMappingClose
    } else {
        K::FlowSequenceClose
    }
}

/// Tokens after which inserted trivia can never be read as part of them
/// (see [`SyntaxTree::trivia_only`]); plain scalars are decided there.
const fn closes(k: K) -> bool {
    !matches!(
        k,
        K::PlainScalar
            | K::LiteralScalar
            | K::FoldedScalar
            | K::BlockScalarHeader
            | K::ErrorText
            | K::Skipped
    ) && k.is_token()
}

fn relative(s: usize, origin: usize) -> usize {
    s.saturating_sub(origin)
}

/// The diagnostics attached to `x`, spans relative to `origin`.
fn push_diagnostics<'t>(tree: &'t SyntaxTree, x: Node<'t>, origin: usize, out: &mut impl Sink<'t>) {
    tree.for_each_diagnostic(x.slot(), |d, from, to| {
        let mut d = d.clone();
        d.shift(from, to);
        d.shift(origin, 0);
        out.put(Item::Diagnostic(d));
    });
}

fn push_enter<'t>(tree: &'t SyntaxTree, x: Node<'t>, origin: usize, out: &mut impl Sink<'t>) {
    let kind = x.kind();
    out.put(Item::Enter {
        kind,
        text: if kind.is_token() {
            Some(x.text())
        } else {
            x.resolved_tag()
        },
        header: x.block_header().map(|mut h| {
            h.span = oneyaml_core::Span::new(
                relative(h.span.start, origin),
                relative(h.span.end, origin),
            );
            Box::new(h)
        }),
    });
    push_diagnostics(tree, x, origin, out);
}

/// The reading of `n`'s subtree (pushed into one buffer, no
/// allocation per step).
fn push_subtree<'t>(tree: &'t SyntaxTree, n: Node<'t>, origin: usize, out: &mut impl Sink<'t>) {
    for step in n.walk() {
        match step {
            WalkEvent::Enter(x) => push_enter(tree, x, origin, out),
            WalkEvent::Leave(_) => out.put(Item::Leave),
        }
        if out.failed() {
            return;
        }
    }
}

/// An element read on its own (see [`Part::Lone`]).
fn push_lone<'t>(tree: &'t SyntaxTree, x: Node<'t>, origin: usize, out: &mut impl Sink<'t>) {
    let kind = x.kind();
    out.put(Item::Enter {
        kind,
        text: kind.is_token().then(|| x.text()),
        header: None,
    });
    push_diagnostics(tree, x, origin, out);
    if kind.is_token() {
        out.put(Item::Leave);
    }
}

/// Not its kind: the re-read, cut after the peeked element's first line,
/// may read that line as a smaller node (a mapping's properties as an
/// empty scalar's, a document's directives on their own).
fn push_peek<'t>(x: Node<'t>, origin: usize, out: &mut impl Sink<'t>) {
    let token = x
        .descendants()
        .find(|t| t.is_token())
        .map(|t| (t.kind(), t.text()));
    out.put(Item::Peek {
        at: relative(x.span().start, origin),
        token,
    });
}

/// Where the items of a reading go: our side is collected, the re-read
/// side compared with it as it is read.
trait Sink<'t> {
    fn put(&mut self, item: Item<'t>);
    /// A difference was found: the rest need not be read.
    fn failed(&self) -> bool;
}

impl<'t> Sink<'t> for Vec<Item<'t>> {
    fn put(&mut self, item: Item<'t>) {
        self.push(item);
    }

    fn failed(&self) -> bool {
        false
    }
}

/// Compares the items put with `ours`, in order.
struct Compare<'o, 't> {
    ours: &'o [Item<'t>],
    at: usize,
    same: bool,
}

impl<'o, 't> Compare<'o, 't> {
    fn new(ours: &'o [Item<'t>]) -> Self {
        Self {
            ours,
            at: 0,
            same: true,
        }
    }

    /// Every item was put, and each was ours.
    fn equal(&self) -> bool {
        self.same && self.at == self.ours.len()
    }
}

impl<'t> Sink<'t> for Compare<'_, 't> {
    fn put(&mut self, item: Item<'t>) {
        if self.same {
            self.same = self.ours.get(self.at) == Some(&item);
            self.at = self.at.saturating_add(1);
        }
    }

    fn failed(&self) -> bool {
        !self.same
    }
}

/// The re-read side: `node`'s children from `skip` on, until one starts at
/// or after `peek` (which is then peeked, and the walk stops: `true`).
fn push_children<'t>(
    tree: &'t SyntaxTree,
    node: Node<'t>,
    skip: usize,
    after: Option<Node<'t>>,
    origin: usize,
    peek: Option<usize>,
    out: &mut impl Sink<'t>,
) -> bool {
    let mut kids = node.children().skip(skip);
    if let Some(a) = after {
        for k in kids.by_ref() {
            if k == a {
                break;
            }
        }
    }
    for k in kids {
        // The element peeked has bytes: zero-width ones at its offset
        // (empty nodes, ends) come before it.
        let sp = k.span();
        if peek.is_some_and(|p| relative(sp.start, origin) >= p && !sp.is_empty()) {
            push_peek(k, origin, out);
            return true;
        }
        push_subtree(tree, k, origin, out);
        if out.failed() {
            return true;
        }
    }
    false
}

/// The value of the first entry of the synthetic context in `r`: the first
/// node after its first `:`, `-`, or `?`.
fn value_after_indicator(r: Node<'_>) -> Option<Node<'_>> {
    let mut seen = false;
    for k in r.children() {
        if seen && !k.is_token() {
            return Some(k);
        }
        seen |= matches!(k.kind(), K::MappingValue | K::SequenceEntry | K::MappingKey);
    }
    None
}

impl SyntaxTree {
    // ========================================================== entry point

    /// Checks that the edit just applied under `parent` (between `prev`
    /// and `next`, inserting `span`'s top-level elements, if any) is
    /// well-formed. `trivia_removed`: everything the edit removed was
    /// trivia tokens (vacuously true for an insertion); `comment_removed`:
    /// one of them was a comment.
    ///
    /// # Errors
    ///
    /// [`TreeEditError::NotWellFormed`]; [`TreeEditError::Limit`];
    /// [`TreeEditError::Internal`].
    pub(super) fn check_well_formed(
        &self,
        parent: u32,
        prev: Option<u32>,
        next: Option<u32>,
        span: Option<(u32, u32)>,
        removed: Removed,
    ) -> Result<(), TreeEditError> {
        // An edited tree always agrees with
        // its own text, recovered input included): an error region reads
        // as its document before it left the parser (C6), so a local
        // window shows that it reads the same only in one case (see
        // `before_errors`); other edits in that document, or next to it,
        // are refused.
        let recovered = if self.error_count() > 0 {
            self.touches_recovered(parent, prev, next, span)?
        } else {
            Recovered::No
        };
        if recovered == Recovered::Yes {
            return Err(TreeEditError::ReadOnly { kind: K::Error });
        }
        let own = matches!(recovered, Recovered::Document(_));
        // Where the edit is, in the edited text.
        let edit_at = match (span, next, prev) {
            (Some((f, _)), _, _) => self.bytes(f)?.0,
            (None, Some(n), _) => self.bytes(n)?.0,
            (None, None, Some(p)) => self.bytes(p)?.1,
            (None, None, None) => 0,
        };
        // The first element at or after the edit, in preorder.
        let head = match (span, next, prev) {
            (Some((f, _)), _, _) => Some(f),
            (None, Some(n), _) => Some(n),
            (None, None, Some(p)) => self.nav_after(p),
            (None, None, None) => self.nav_after(parent),
        };
        // (in a recovered document, only the window decides.)
        if !own
            && self.trivia_only(parent, prev, next, span, removed)?
            && !self.labels_cross(parent, head, edit_at, edit_at, removed)?
        {
            return Ok(());
        }
        // Trivia inserted at stream level right after
        // a document is read with that document's tail, not the whole
        // document.
        if parent == 0 && self.document_tail_reads(prev, next, span, removed, edit_at)? {
            return Ok(());
        }
        let first = prev.or(span.map(|s| s.0)).or(next);
        let last = next.or(span.map(|s| s.1)).or(prev);
        let (mut c, mut range) = self.container(parent, first, last)?;
        // A flow collection inside an implicit key is re-read as
        // the whole key, in its mapping (one line, 1024 characters, §8.2.2).
        let flow = self.kind_of(c).is_some_and(is_flow);
        if let Some(key) = flow
            .then(|| self.implicit_key_around(c))
            .transpose()?
            .flatten()
        {
            let p = self.nav_parent(key).ok_or(TreeEditError::Internal)?;
            (c, range) = self.container(p, Some(key), Some(key))?;
        }
        let kind = self.kind_of(c).ok_or(TreeEditError::Internal)?;
        let window = match (kind, range) {
            (K::Stream, range) => self.stream_window(parent, range, prev, next, span)?,
            (_, None) => return Err(TreeEditError::Internal),
            (K::FlowMapping | K::FlowSequence, Some((lo, hi))) => {
                let direct = parent == c;
                // An insertion right after a `,` starts after it (what
                // follows a `,` never changes the entry before it).
                let lo = match prev {
                    Some(p) if direct && lo == p && self.kind_of(p) == Some(K::FlowEntry) => {
                        span.map(|s| s.0).or(next).unwrap_or(lo)
                    }
                    _ => lo,
                };
                let after = if direct { next } else { self.nav_next(hi) };
                self.flow_window(c, kind, lo, after)?
            }
            (_, Some((lo, _))) => {
                let direct = parent == c;
                let (p, n) = if direct { (prev, next) } else { (None, None) };
                // The element after the edit starts a line both before
                // and after it: its line is untouched.
                let after_lb =
                    span.map(|s| s.1).or(prev).and_then(|x| self.kind_of(x)) == Some(K::LineBreak);
                let around = Around {
                    prev: p,
                    next: n,
                    direct,
                    next_line_kept: direct && removed.next_line_start && after_lb,
                    old_column: removed.old_column.filter(|_| direct),
                };
                self.block_window(c, kind, lo, around)?
            }
        };
        let Some(w) = window else {
            return Err(TreeEditError::NotWellFormed);
        };
        // A stream window never reads (or peeks) an element with
        // errors (see `touches_recovered`).
        if self.error_count() > 0 && self.window_recovered(&w)? {
            return Err(TreeEditError::ReadOnly { kind: K::Error });
        }
        if let Recovered::Document(d) = recovered {
            // The bytes this splice inserted (none known for the first
            // splice of a pair, whose second one inserts elsewhere).
            let changed = removed
                .later
                .is_none()
                .then(|| (edit_at, edit_at.saturating_add(removed.added)));
            if !self.before_errors(&w, d, changed, removed.line_break) {
                return Err(TreeEditError::ReadOnly { kind: K::Error });
            }
        }
        // A diagnostic after the window with a label before the edit (a
        // multi-line key's start, Y0007/Y0018) would be moved with its
        // element, wrongly; the window does not re-read it.
        if self.labels_cross(parent, head, edit_at, w.end, removed)? {
            return Err(TreeEditError::NotWellFormed);
        }
        self.reread_uncut(w)
    }

    /// How the edit relates to **recovered** input (see
    /// [`SyntaxTree::recovered`] and *Recovered input* in the [module
    /// docs](super)): it lies in a recovered document (decided by its
    /// window, [`SyntaxTree::before_errors`]), or next to a recovered
    /// stream child that could read it, or whose errors' labels or fixes
    /// reach it (refused), or neither. How an error region reads depends on
    /// everything its document read before it and, at its end, on what
    /// follows it (an unclosed flow collection or quote cut off by a
    /// marker, lines skipped up to the next document), and its diagnostics
    /// have labels and fixes far from their element (an unclosed `[`; a
    /// byte order mark that ended a document is reported over the line it
    /// starts). Inserted recovered stream children are decided by the
    /// stream window ([`SyntaxTree::window_recovered`]). O(depth +
    /// `LONG_RUN` + `TRIVIA_WALK` + log R + the neighbours' content
    /// errors, not their directive errors, see
    /// [`SyntaxTree::errors_reach`]); nothing for a tree without errors
    /// (the caller checks).
    fn touches_recovered(
        &self,
        parent: u32,
        prev: Option<u32>,
        next: Option<u32>,
        span: Option<(u32, u32)>,
    ) -> Result<Recovered, TreeEditError> {
        // Where the edit is in the current text.
        let at = match (span, next, prev) {
            (Some((f, _)), _, _) => self.bytes(f)?.0,
            (None, Some(n), _) => self.bytes(n)?.0,
            (None, None, Some(p)) => self.bytes(p)?.1,
            (None, None, None) => self.bytes(parent)?.1,
        };
        let doc = self.document_of(parent);
        let mut own = false;
        let (before, after) = match doc {
            Some(d) => {
                own = self.recovered(d)?;
                let after = match self.nav_next(d) {
                    Some(a) => self.content_after(a)?,
                    None => None,
                };
                (self.content_before(d)?, after)
            }
            None => {
                // (Inserted recovered stream children are read whole by the
                // stream window: see `window_recovered`.)
                let before = match prev {
                    Some(p) if self.kind_of(p).is_some_and(K::is_trivia) => {
                        self.content_before(p)?
                    }
                    p => p,
                };
                let after = match next {
                    Some(n) => self.content_after(n)?,
                    None => None,
                };
                (before, after)
            }
        };
        if let Some(b) = before {
            if self.recovered(b)? {
                // A `...` after a byte order mark on its line
                // ends `b` for the stream, not a quoted scalar in it.
                let ended = self
                    .last_token(b)
                    .is_some_and(|t| self.kind_of(t) == Some(K::DocumentEndMarker))
                    && self.ends_with_end_line(b)?;
                let closed = match doc {
                    // Inside a document that reads fresh: past its `---`,
                    // or starting a line after the `...` that ends `b`.
                    // (A document left without tokens is not.)
                    Some(d) => match self.first_token(d) {
                        Some(first) => {
                            let (fa, fe) = self.bytes(first)?;
                            (self.starts_line_with_marker(d)? && at >= fe)
                                || (ended && self.starts_line_at(fa))
                        }
                        None => false,
                    },
                    // After the `...` that ends it.
                    None => ended,
                };
                if !closed || self.errors_reach(b, Some(at), None) {
                    return Ok(Recovered::Yes);
                }
            } else {
                if let Some(bb) = self.content_before(b)? {
                    // Past a clean one too: a label over the line a
                    // misplaced mark starts covers that whole document.
                    if self.has_errors_within(bb) && self.errors_reach(bb, Some(at), None) {
                        return Ok(Recovered::Yes);
                    }
                }
                if self.recovered_reads_on(b, doc, at)? {
                    return Ok(Recovered::Yes);
                }
            }
        }
        if let Some(a) = after {
            if self.recovered(a)? {
                let start = self.bytes(a)?.0;
                // Read afresh: after its own `---`, or after the `...` that
                // ends the document before it (the edit's, before that
                // marker, or one the edit is not in).
                let fresh = match self.fresh_after(a)? {
                    Fresh::Marker => true,
                    Fresh::Ended { by, marker } => doc != Some(by) || at < marker,
                    Fresh::No => false,
                };
                if !fresh || self.errors_reach(a, None, Some(start)) {
                    return Ok(Recovered::Yes);
                }
            }
        }
        Ok(match doc {
            Some(d) if own => Recovered::Document(d),
            _ => Recovered::No,
        })
    }

    /// Some recovered stream child
    /// before clean stream child `b` (the content before the edit, which
    /// lies in document `doc` or at stream level, at byte `at`) could read
    /// the edit: no `---` or `...` at the start of a line lies between them.
    /// How an error region reads can depend on every line up to such a
    /// marker: an unclosed quoted scalar's scan reads on to the end of the
    /// text or to the first line starting with `---` or `...`. A byte order
    /// mark at the start of a line ends a document for the stream
    /// (production 211) but not a quoted scalar, where marks are
    /// allowed (section 5.2: `nb-json`), so in `'a`, a line break, then a
    /// mark and `---`, the `---` starts a document for the stream and is
    /// content for the quote; a `---` appended at the end changed the
    /// quote's error ("not closed" became "document marker inside a quoted
    /// scalar") while the edit was accepted.
    ///
    /// The walk: back from `b`, one stream child at a time, it
    /// answers "no" at a document starting with `---` at the start of a
    /// line, at a child ending with `...` at the start of a line
    /// ([`SyntaxTree::ends_with_end_line`]), or at the start of the text,
    /// and "yes" at the first recovered child once a document lies between
    /// it and the edit (the edit's own, or one walked over). Recovered
    /// children before any document is crossed are passed over: the edit
    /// is then at stream level after error regions, skipped lines or stray
    /// markers only, so its stream window does not start after a document
    /// there; it reads back to the document before (`back_to_document`),
    /// those children included, which are compared whole
    /// ([`SyntaxTree::window_recovered`]). But an older recovered child
    /// may still read on through them, so the walk goes on (stopping
    /// stopped at the first and answered "no": `"b`, a line break, a mark,
    /// `x`, `: ]` with `---` appended was accepted). At most
    /// [`TRIVIA_WALK`] stream children are walked; past them, the edit is
    /// taken to be read (refused). O(`TRIVIA_WALK` * (`LONG_RUN` + log R +
    /// depth)).
    fn recovered_reads_on(&self, b: u32, doc: Option<u32>, at: u64) -> Result<bool, TreeEditError> {
        // The edit's own document starts with `---` at the start of a line,
        // and the edit is past it.
        if let Some(d) = doc {
            if self.starts_line_with_marker(d)? {
                if let Some(f) = self.first_token(d) {
                    if at >= self.bytes(f)?.1 {
                        return Ok(false);
                    }
                }
            }
        }
        // A document lies between the walk and the edit.
        let mut crossed = doc.is_some();
        let mut c = b;
        for _ in 0..TRIVIA_WALK {
            if self.starts_line_with_marker(c)? {
                return Ok(false);
            }
            crossed |= self.kind_of(c) == Some(K::Document);
            let Some(p) = self.content_before(c)? else {
                return Ok(false);
            };
            // `p` ends with `...` at the start of a line: nothing before
            // it reads past it.
            if self.ends_with_end_line(p)? {
                return Ok(false);
            }
            // Before a document is crossed, a recovered child is read whole by
            // the window, but one further back may read on through it
            // (`"b⏎<BOM>x⏎: ]` with `---` appended: the quote reads past the
            // clean `x` and the error region), so the walk goes on past it.
            if crossed && self.recovered(p)? {
                return Ok(true);
            }
            c = p;
        }
        Ok(true)
    }

    /// Stream child `p` ends with `...` (or a stray one) at the start of a
    /// line, right after a line break (not after a byte order mark, which
    /// a quoted scalar reads as content, section 5.2): nothing before it
    /// reads past it. O(depth + log R).
    fn ends_with_end_line(&self, p: u32) -> Result<bool, TreeEditError> {
        let Some(t) = self.last_token(p) else {
            return Ok(false);
        };
        if !matches!(
            self.kind_of(t),
            Some(K::DocumentEndMarker | K::StrayDocumentEndMarker)
        ) {
            return Ok(false);
        }
        Ok(self.starts_line_at(self.bytes(t)?.0))
    }

    /// For an edit in recovered document `d`, the window `w`
    /// (block or flow) ends before every error of `d` with all its labels
    /// and fixes: they lie at or after its synchronising point, the start
    /// of the element it peeks or, when it reads to its collection's end,
    /// its own end (no error text among the trivia after the collection).
    /// Errors end their document's content (C6), so all of `d` up to there
    /// is text without errors: the window decides how it reads exactly as
    /// in a tree without errors, and the parser reaches the synchronising
    /// point in the state it was in before the edit (no state of the
    /// parser's that an error would show, an open bracket or a key's
    /// start, reaches back past it: no label does). Everything from there
    /// on, the error region included, reads the same, and every diagnostic
    /// of the region moves with its text. A flow collection the window lies
    /// in that is followed by `:` (an invalid implicit key: it spans lines)
    /// is taken when it spans lines outside `changed`, the bytes the edit
    /// inserted. O(log d + log R + content errors of `d` + the window's
    /// trailing tokens + depth).
    fn before_errors(
        &self,
        w: &Window,
        d: u32,
        changed: Option<(u64, u64)>,
        removed_break: bool,
    ) -> bool {
        if !matches!(w.shape, Shape::Root { .. }) {
            return false;
        }
        let errorish = |x: u32| {
            self.kind_of(x)
                .is_none_or(|k| matches!(k, K::Error | K::ErrorText | K::Skipped))
        };
        let sync = match w.ours.last() {
            Some(Part::Peek(p)) => match self.nav_rank(*p) {
                Some((_, at)) => at,
                None => return false,
            },
            _ => {
                let tail_errors = w.ours.iter().any(|part| match part {
                    Part::Lone(k) => k.iter().any(|&x| errorish(x)),
                    _ => false,
                });
                if tail_errors {
                    return false;
                }
                w.end
            }
        };
        // A flow collection followed by `:` on its line is an implicit key
        // whose reading (one line, 1024 characters, §8.2.2) the edit may
        // change; an invalid one is an error after the window that no
        // label ties to it. Each one the window lies in is checked.
        let mut x = w.flow;
        while let Some(c) = x {
            if self.in_key_position(c)
                || (self.colon_follows(c)
                    && !changed.is_some_and(|ch| self.multi_line_both(c, ch, removed_break)))
            {
                return false;
            }
            x = self.nav_parent(c).filter(|p| {
                self.kind_of(*p)
                    .is_some_and(|k| is_flow(k) || k == K::FlowPair)
            });
        }
        let (Some(last), Some((rank, _))) = (self.nav_last_leaf(d), self.nav_rank(d)) else {
            return false;
        };
        // A document `facts.rs` records
        // as mixed is refused in O(1) (its directive errors lie before the
        // window, so the scan below refused it too, after its content
        // errors).
        if self.mixed_directive_document(d) {
            return false;
        }
        let end = i128::from(sync);
        let wide = |s: usize| i128::try_from(s).unwrap_or(i128::MAX);
        let mut after = true;
        // Down to the first warning, or to the
        // first directive error (zero-width before `d`'s `---`, with its one
        // label there, and, `d` not being mixed, only directive errors
        // before it, `directive_codes_mark_directive_errors_only`): the
        // window lies in `d`'s content, after its `---`, so it changes how
        // no directive reads, and those errors stay as they are. Before, a
        // duplicate `%YAML` refused every edit of such a document.
        self.scan_diagnostics(last, false, rank, |x| {
            if x.diagnostic.severity() != Severity::Error || is_directive_error(x.diagnostic.code())
            {
                return false;
            }
            let start = wide(x.diagnostic.first_start())
                .saturating_sub(wide(x.from))
                .saturating_add(wide(x.to));
            after = i128::from(x.start) >= end && start >= end;
            after
        });
        after
    }

    /// Flow collection `x` spans lines both before and after the
    /// edit: it holds a line break outside the bytes the edit inserted
    /// (`changed`, current text; looked for within [`PEEK_LOOKAHEAD`] bytes
    /// on each side), or both the removed text (`removed_break`) and the
    /// inserted text hold one. Followed by `:`, it is then an invalid
    /// implicit key both times (§8.2.2: one line), and the parser reads
    /// that `:` the same. O(`PEEK_LOOKAHEAD` + inserted bytes).
    fn multi_line_both(&self, x: u32, changed: (u64, u64), removed_break: bool) -> bool {
        let (a, b) = changed;
        let inserted = removed_break
            && self
                .try_text_range(a, b)
                .is_ok_and(|t| t.contains(['\n', '\r']));
        inserted || self.multi_line_outside(x, changed)
    }

    /// The line break of [`SyntaxTree::multi_line_both`] outside `changed`.
    fn multi_line_outside(&self, x: u32, changed: (u64, u64)) -> bool {
        let Some((xs, xe)) = self.nav_bytes(x) else {
            return false;
        };
        let (a, b) = changed;
        let has_break = |from: u64, to: u64| {
            to > from
                && self
                    .try_text_range(from, to)
                    .is_ok_and(|t| t.contains(['\n', '\r']))
        };
        has_break(xs.max(a.saturating_sub(PEEK_LOOKAHEAD)), a.min(xe))
            || has_break(b.max(xs), xe.min(b.saturating_add(PEEK_LOOKAHEAD)))
    }

    /// Node `x` sits where a mapping's key does: in a block or
    /// flow mapping or a flow pair, not right after a `:` (a key, or what
    /// recovery left there: a multi-line flow collection where a key was
    /// expected, whose `:` never came). O(depth + `TRIVIA_WALK`).
    fn in_key_position(&self, x: u32) -> bool {
        let keyed = self
            .nav_parent(x)
            .and_then(|p| self.kind_of(p))
            .is_some_and(|k| matches!(k, K::BlockMapping | K::FlowMapping | K::FlowPair));
        if !keyed {
            return false;
        }
        // The content before `x` within [`TRIVIA_WALK`]
        // trivia siblings; past them, `x` is taken to be a key (the caller
        // then refuses), so a long run of comment lines before a flow
        // collection costs a constant, not the run.
        let mut at = self.nav_prev(x);
        for _ in 0..TRIVIA_WALK {
            let Some(p) = at else {
                return true;
            };
            match self.kind_of(p) {
                Some(k) if k.is_trivia() => at = self.nav_prev(p),
                k => return k != Some(K::MappingValue),
            }
        }
        true
    }

    /// The first token after `x` on its line, white space passed
    /// over, starts with `:` (or there is no telling within
    /// [`TRIVIA_WALK`] steps). O(depth + `TRIVIA_WALK`).
    fn colon_follows(&self, x: u32) -> bool {
        let mut at = self.nav_after(x);
        let mut steps = 0usize;
        while let Some(y) = at {
            steps = steps.saturating_add(1);
            if steps > TRIVIA_WALK {
                return true;
            }
            let Some(k) = self.kind_of(y) else {
                return true;
            };
            if !k.is_token() || k == K::Whitespace {
                at = self.nav_succ(y);
                continue;
            }
            return k == K::MappingValue || Node::new(self, y).text().starts_with(':');
        }
        false
    }

    /// `x` has errors in its content: an error node or skipped
    /// text (stream level), or a document whose content holds an error.
    /// Errors among a document's directives (`%YAML 2.0`, a duplicate
    /// `%TAG`) are not content errors: they are zero-width, their labels
    /// are at their own place, and nothing the document reads depends on
    /// them. O(log d + log R + `TRIVIA_WALK`).
    fn recovered(&self, x: u32) -> Result<bool, TreeEditError> {
        if !self.has_errors_within(x) {
            return Ok(false);
        }
        if self.kind_of(x) != Some(K::Document) {
            return Ok(true);
        }
        let (Some(last), Some((rank, _))) = (self.nav_last_leaf(x), self.nav_rank(x)) else {
            return Ok(false);
        };
        // The last diagnostic: content errors are the last ones of their
        // document (see `labels_cross`).
        let mut last_error = None;
        self.scan_diagnostics(last, false, rank, |d| {
            if d.diagnostic.severity() == Severity::Error {
                last_error = Some((d.rank, is_directive_error(d.diagnostic.code())));
            }
            false
        });
        let Some((e, directive_code)) = last_error else {
            return Ok(false);
        };
        // A directive error lies before the document's `---`; only a
        // document recorded with one has any (`facts.rs`, O(log)).
        let directive_errors = self
            .facts_at(x)
            .is_some_and(|(f, local, _)| f.directive_error(local));
        if !directive_errors {
            return Ok(true);
        }
        // The last error has no directive code (a failed
        // directive block before the `---`, `Y0001`, whose error region
        // may hold an unclosed quote that reads on past a mark and the
        // `---` after it, into the content), or an error without one
        // precedes a directive error (a mixed document, `facts.rs`): the
        // document is taken as recovered wherever its errors lie. Before,
        // only an error after the `---` counted, so `%YAML 2.0`, `c: x`, a
        // mark, `'q` and a mark before `---` read as clean, and `---`
        // appended after it was accepted while it changed the quote's
        // error.
        if !directive_code || self.mixed_directive_document(x) {
            return Ok(true);
        }
        // At most `TRIVIA_WALK` of its first children
        // (directives, trivia, errors); past them it is taken to have
        // content errors (its edits are refused).
        let mut c = self.nav_first(x);
        let mut steps = 0usize;
        while let Some(ch) = c {
            steps = steps.saturating_add(1);
            if steps > TRIVIA_WALK {
                break;
            }
            let k = self.kind_of(ch).ok_or(TreeEditError::Internal)?;
            if k == K::DocumentStartMarker {
                let (r, _) = self.nav_rank(ch).ok_or(TreeEditError::Internal)?;
                return Ok(e > r);
            }
            if !(k.is_trivia() || k == K::Error || is_directive_kind(k)) {
                break;
            }
            c = self.nav_next(ch);
        }
        Ok(true)
    }

    /// Some error attached in `x`'s subtree has a label or fix
    /// ending at or after `after`, or starting before `before` (current
    /// text). Every caller asks about a threshold outside `x`'s own bytes
    /// (`after` at or past its end, `before` at or before its start).
    /// Errors are the last diagnostics of their document's content (see
    /// [`SyntaxTree::labels_cross`]), so the scan stops at the first
    /// warning; and at the first directive error of a document
    /// ([`is_directive_error`]): those are zero-width at their own place,
    /// before the document's `---`, so they reach neither threshold, and
    /// every error before them has a directive code too, unless the
    /// document is recorded otherwise
    /// ([`SyntaxTree::mixed_directive_document`]).
    ///
    /// Before, the scan went on through every directive error (`Y1004`
    /// duplicates are errors, with no warning between), so an edit next to a
    /// document with 400k duplicate `%YAML` lines cost 3.7 ms, not 15 µs; the
    /// decision is the same (they never reached).
    ///
    /// A mixed document is taken to
    /// reach both thresholds, in O(1) (its edits and those next to it are
    /// refused). Scanning its errors in full would cost O(its directive
    /// errors): an edit next to one with 400k duplicate `%YAML` lines cost
    /// 4.15 ms, and whether its earlier errors' labels stay within its own
    /// lines is not proven. Such documents (a failed directive block,
    /// `Y0001`, kept with the next document's directives) are rare. O(log d
    /// + log R + content errors of `x`).
    fn errors_reach(&self, x: u32, after: Option<u64>, before: Option<u64>) -> bool {
        if self.mixed_directive_document(x) {
            return after.is_some() || before.is_some();
        }
        let (Some(last), Some((rank, _))) = (self.nav_last_leaf(x), self.nav_rank(x)) else {
            return false;
        };
        let document = self.kind_of(x) == Some(K::Document);
        let wide = |s: usize| i128::try_from(s).unwrap_or(i128::MAX);
        let mut found = false;
        self.scan_diagnostics(last, false, rank, |d| {
            if d.diagnostic.severity() != Severity::Error
                || (document && is_directive_error(d.diagnostic.code()))
            {
                return false;
            }
            let moved = |s: usize| {
                wide(s)
                    .saturating_sub(wide(d.from))
                    .saturating_add(wide(d.to))
            };
            let end = moved(d.diagnostic.last_end());
            let start = moved(d.diagnostic.first_start());
            found = after.is_some_and(|a| end >= i128::from(a))
                || before.is_some_and(|b| start < i128::from(b));
            !found
        });
        found
    }

    /// `x` is a document in which an error without
    /// a directive code ([`is_directive_error`]) comes before one with it
    /// (`facts.rs` records them: a failed directive block, `Y0001`, kept
    /// before later directives). In any other document, a backward scan of
    /// its errors may stop at the first one with a directive code; a mixed
    /// one is refused instead (see [`SyntaxTree::errors_reach`]). O(log).
    fn mixed_directive_document(&self, x: u32) -> bool {
        self.kind_of(x) == Some(K::Document)
            && self
                .facts_at(x)
                .is_some_and(|(f, local, _)| f.mixed_directive_error(local))
    }

    /// `x` is a document that starts with `---` at the start of
    /// a line (right after a line break, or at the start of the text): it
    /// reads the same whatever precedes it.
    fn starts_line_with_marker(&self, x: u32) -> Result<bool, TreeEditError> {
        if self.kind_of(x) != Some(K::Document) {
            return Ok(false);
        }
        let Some(t) = self.first_token(x) else {
            return Ok(false);
        };
        if self.kind_of(t) != Some(K::DocumentStartMarker) {
            return Ok(false);
        }
        Ok(self.starts_line_at(self.bytes(t)?.0))
    }

    /// How stream child `x` reads whatever precedes it: it is a
    /// document starting with `---` at the start of a line; or it starts a
    /// line right after a document ended by `...` (trivia between), which
    /// nothing before that marker can change. O(`LONG_RUN` + log R +
    /// depth).
    fn fresh_after(&self, x: u32) -> Result<Fresh, TreeEditError> {
        if self.starts_line_with_marker(x)? {
            return Ok(Fresh::Marker);
        }
        let Some(t) = self.first_token(x) else {
            return Ok(Fresh::No);
        };
        if !self.starts_line_at(self.bytes(t)?.0) {
            return Ok(Fresh::No);
        }
        let Some(b) = self.content_before(x)? else {
            return Ok(Fresh::No);
        };
        if self.kind_of(b) != Some(K::Document) {
            return Ok(Fresh::No);
        }
        match self.last_token(b) {
            Some(m) if self.kind_of(m) == Some(K::DocumentEndMarker) => Ok(Fresh::Ended {
                by: b,
                marker: self.bytes(m)?.0,
            }),
            _ => Ok(Fresh::No),
        }
    }

    /// Byte `at` of the current text starts a line.
    fn starts_line_at(&self, at: u64) -> bool {
        match at.checked_sub(1) {
            None => true,
            Some(b) => matches!(self.byte_at(b), Some(b'\n' | b'\r')),
        }
    }

    /// A stream window reads a recovered element it cannot
    /// decide, or peeks one that would not read the same whatever precedes
    /// it (see [`SyntaxTree::touches_recovered`]). A recovered stream child
    /// read whole (an inserted document with errors, an error region before
    /// the next document) is compared with its errors, so it is decided
    /// when the window starts where reading starts afresh (as every stream
    /// window does: after a document without errors, or at the start) and
    /// ends where it ends afresh (a document starting with `---` at the
    /// start of a line, peeked, or the end of the text), and its errors'
    /// labels and fixes lie inside the window. Trivia read one by one
    /// after the window's elements never hold a recovered element.
    /// O(window's stream children · (log + `TRIVIA_WALK` + their content
    /// errors)).
    fn window_recovered(&self, w: &Window) -> Result<bool, TreeEditError> {
        if !matches!(w.shape, Shape::Stream) {
            return Ok(false);
        }
        let ends_fresh = match w.ours.last() {
            Some(Part::Peek(p)) => self.fresh_after(*p)? != Fresh::No,
            _ => w.end >= self.nav_total().1,
        };
        for p in &w.ours {
            match p {
                Part::Kids(k) => {
                    for &x in k {
                        if self.recovered(x)?
                            && (!ends_fresh
                                || self.errors_reach(
                                    x,
                                    Some(w.end.saturating_add(1)),
                                    Some(w.start),
                                ))
                        {
                            return Ok(true);
                        }
                    }
                }
                Part::Lone(k) => {
                    for &x in k {
                        if self.recovered(x)? {
                            return Ok(true);
                        }
                    }
                }
                Part::Peek(x) => {
                    if self.recovered(*x)? {
                        let start = self.bytes(*x)?.0;
                        if self.fresh_after(*x)? == Fresh::No
                            || self.errors_reach(*x, None, Some(start))
                        {
                            return Ok(true);
                        }
                    }
                }
                Part::Leave => {}
            }
        }
        Ok(false)
    }

    /// [`SyntaxTree::reread`], then, when a cut peek did not read back, the
    /// window with its peek's whole line (exactly the check
    /// before the cut).
    ///
    /// A safety net, not a known need. What a re-read compares
    /// of the peeked element is its start and first token, which the
    /// parser decides within `KEY_WINDOW` bytes of that start (an implicit
    /// key is at most 1024 characters), and nothing after it; no input is
    /// known where the cut window is refused and the whole line accepted
    /// (none in 1.45M instrumented edits). The retry
    /// keeps that equivalence exact at the cost of a refusal next to a
    /// long line: such an edit re-reads the line
    /// (pinned by `a_refusal_next_to_a_long_line_reads_the_line`);
    /// accepted edits stay O(local).
    fn reread_uncut(&self, mut w: Window) -> Result<(), TreeEditError> {
        let first = self.reread(&w);
        let Some((p, base)) = w.uncut.take().filter(|_| first.is_err()) else {
            return first;
        };
        w.end = base.max(self.peek_end(p, false)?.0);
        self.reread(&w)
    }

    /// Some diagnostic of the edit's document, attached to an element
    /// starting at `after` or later, has a label that started before
    /// `at` before the edit (its spans move with its element, so that
    /// label would move with it). `head`: the first element at or after
    /// the edit.
    ///
    /// O(log d + log R), plus the diagnostics near the
    /// edit and the document's errors, never all of its diagnostics (100k
    /// `Y0028` warnings under `lenient_inline_colons` made every edit
    /// O(document)). Which diagnostics have labels before their element
    /// (see `SyntaxTree::diagnostics_of`, parser T3, C2SP): errors (a
    /// multi-line key's start, `Y0007`/`Y0018`; the inline `: `), and
    /// `Y0028` warnings, whose labels lie in the plain scalar right before
    /// their element (no element between). Such a warning crosses only
    /// when the edit lies between that scalar and its element, so its
    /// element is the first old one after the edit, which starts at `at`
    /// plus the bytes inserted: warnings are checked only from `after` up
    /// to there (the forward pass). Errors in a document's content end
    /// that content (what follows one up to the next document is its error
    /// region, C6), so no warning is attached after such an error: the
    /// document's content errors are its last diagnostics, read backwards
    /// from its end down to the first warning (the backward pass).
    /// The other errors, among a document's directives
    /// (`%YAML 2.0`, duplicates; warnings of later directives may follow
    /// them), are zero-width error nodes whose one label is at their own
    /// element, so they never cross and the backward pass need not reach
    /// them (`directive_errors_never_cross` checks both halves).
    fn labels_cross(
        &self,
        parent: u32,
        head: Option<u32>,
        at: u64,
        after: u64,
        removed: Removed,
    ) -> Result<bool, TreeEditError> {
        let Some(doc) = self.document_of(parent) else {
            return Ok(false);
        };
        let (doc_rank, _) = self.nav_rank(doc).ok_or(TreeEditError::Internal)?;
        let doc_end = doc_rank.saturating_add(self.nav_size(doc));
        // This splice's own shift, plus a later
        // splice's for the elements after it (the delta of the whole text
        // moved the elements between the two by the later one's bytes).
        let own = i128::from(removed.added).saturating_sub(i128::from(removed.removed));
        let delta = |rank: u64| match removed.later {
            Some((from, more)) if rank >= from => own.saturating_add(more),
            _ => own,
        };
        let at_wide = i128::from(at);
        let wide = |s: usize| i128::try_from(s).unwrap_or(i128::MAX);
        // A span start in the current text is `s - from + to`; before the
        // edit it was that minus `delta`.
        let crosses = |x: &DiagAt<'_>| {
            let before = |s: usize| {
                wide(s)
                    .saturating_sub(wide(x.from))
                    .saturating_add(wide(x.to))
                    .saturating_sub(delta(x.rank))
                    < at_wide
            };
            // Some label or fix starts before: the earliest does
            // (what a skeleton keeps).
            before(x.diagnostic.first_start())
        };
        let mut found = false;
        // Forward: everything attached from `after` to the first old
        // element after the edit.
        let edit_end = at.saturating_add(removed.added);
        if let Some(h) = head.filter(|_| after <= edit_end) {
            self.scan_diagnostics(h, true, doc_end, |x| {
                if x.start > edit_end {
                    return false;
                }
                if x.start >= after && crosses(&x) {
                    found = true;
                    return false;
                }
                true
            });
        }
        if found {
            return Ok(true);
        }
        // Backward: the document's errors, from its end down to the first
        // warning (or to `after`).
        if let Some(last) = self.nav_last_leaf(doc) {
            self.scan_diagnostics(last, false, doc_rank, |x| {
                if x.start < after || x.diagnostic.severity() != Severity::Error {
                    return false;
                }
                if crosses(&x) {
                    found = true;
                    return false;
                }
                true
            });
        }
        Ok(found)
    }

    /// Some error is attached inside `x`'s subtree. O(log d + log R):
    /// errors in a document's content end that content, so when `x` holds
    /// one, the last diagnostic inside it is an error (see
    /// [`SyntaxTree::labels_cross`]). An error
    /// among a document's directives (`%YAML 2.0`, a duplicate) does not
    /// end anything, and warnings of later directives follow it; such
    /// documents are recorded when their arena enters the tree
    /// (`facts.rs`), and the stream has errors exactly when it counts any.
    pub(super) fn has_errors_within(&self, x: u32) -> bool {
        match self.kind_of(x) {
            Some(K::Stream) => return self.error_count() > 0,
            Some(K::Document)
                if self
                    .facts_at(x)
                    .is_some_and(|(f, local, _)| f.directive_error(local)) =>
            {
                return true;
            }
            _ => {}
        }
        let (Some(last), Some((rank, _))) = (self.nav_last_leaf(x), self.nav_rank(x)) else {
            return false;
        };
        let mut found = false;
        self.scan_diagnostics(last, false, rank, |d| {
            found = d.diagnostic.severity() == Severity::Error;
            false
        });
        found
    }

    /// The nearest container at or above `parent` holding the edit, and
    /// the range of its children the edit touched (`None`: an empty
    /// stream).
    fn container(
        &self,
        parent: u32,
        first: Option<u32>,
        last: Option<u32>,
    ) -> Result<(u32, Option<(u32, u32)>), TreeEditError> {
        let (mut c, mut range) = (parent, first.zip(last));
        loop {
            let k = self.kind_of(c).ok_or(TreeEditError::Internal)?;
            if k == K::Stream || (is_container(k) && range.is_some()) {
                return Ok((c, range));
            }
            range = Some((c, c));
            c = self.nav_parent(c).ok_or(TreeEditError::Internal)?;
        }
    }

    /// The outermost implicit key (block or flow) that holds flow
    /// collection `c`, below the nearest block collection. O(depth).
    fn implicit_key_around(&self, c: u32) -> Result<Option<u32>, TreeEditError> {
        let mut key = None;
        let mut x = c;
        loop {
            let Some(p) = self.nav_parent(x) else {
                return Ok(key);
            };
            let pk = self.kind_of(p).ok_or(TreeEditError::Internal)?;
            let x_kind = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            let keyed = matches!(pk, K::BlockMapping | K::FlowMapping | K::FlowPair)
                && !x_kind.is_token()
                && self.next_content(x).and_then(|n| self.kind_of(n)) == Some(K::MappingValue)
                && self.prev_content(x).and_then(|n| self.kind_of(n)) != Some(K::MappingKey);
            if keyed {
                key = Some(x);
            }
            if !(is_flow(pk) || pk == K::FlowPair) {
                return Ok(key);
            }
            x = p;
        }
    }

    /// The next sibling that is not trivia.
    fn next_content(&self, x: u32) -> Option<u32> {
        let mut at = self.nav_next(x);
        while let Some(n) = at {
            if !self.kind_of(n)?.is_trivia() {
                return Some(n);
            }
            at = self.nav_next(n);
        }
        None
    }

    /// The previous sibling that is not trivia.
    fn prev_content(&self, x: u32) -> Option<u32> {
        let mut at = self.nav_prev(x);
        while let Some(n) = at {
            if !self.kind_of(n)?.is_trivia() {
                return Some(n);
            }
            at = self.nav_prev(n);
        }
        None
    }

    /// Collections from the root down to `x`, `x` included.
    fn depth_of(&self, x: u32) -> usize {
        let mut d = 0usize;
        let mut at = Some(x);
        while let Some(a) = at {
            if self.kind_of(a).is_some_and(counts_depth) {
                d = d.saturating_add(1);
            }
            at = self.nav_parent(a);
        }
        d
    }

    fn bytes(&self, x: u32) -> Result<(u64, u64), TreeEditError> {
        self.nav_bytes(x).ok_or(TreeEditError::Internal)
    }

    /// The first token of `x` (itself for a token). O(depth).
    fn first_token(&self, x: u32) -> Option<u32> {
        let mut at = x;
        while !self.kind_of(at)?.is_token() {
            at = self.nav_first(at)?;
        }
        Some(at)
    }

    /// The last token of `x` (itself for a token): the end of its chain of
    /// last children, when that is a token (`None` when it is an empty
    /// node). O(depth + log R), not a `nav_last` per
    /// level (O(depth²)).
    pub(super) fn last_token(&self, x: u32) -> Option<u32> {
        let leaf = self.nav_last_leaf(x)?;
        self.kind_of(leaf)?.is_token().then_some(leaf)
    }

    /// The last token of `x` with bytes (or the empty
    /// text of a block scalar), what reads the text after `x`: zero-width
    /// elements at its end (the empty value of a `? |` entry, ends) are
    /// passed over. `Some(None)`: `x` has no token with bytes; `None`: the
    /// walk gave up (the caller refuses). Before, the zero-width last token
    /// hid a block scalar ending `x`, whose indentation and chomping read
    /// the blank lines after it (`? |⏎␠␠` with `---⏎` inserted before the
    /// spaces was accepted, its header keeping an indentation the re-parse
    /// does not detect).
    ///
    /// The walk goes down the last children, and
    /// passes over a zero-width sibling (a node, with its whole subtree,
    /// or a token) in one step, at most [`TRIVIA_WALK`] of them in a row
    /// at one level; a node with bytes is entered (one of its tokens has
    /// bytes). A zero-width node holds no block scalar text: a block
    /// scalar's node holds its header, which has bytes. So the answer is
    /// the last such token in preorder, as a walk over every element finds
    /// it. Before, at most [`TRIVIA_WALK`] elements were walked in all, and
    /// a document ending with 31 or more nested empty keys (`? ? … a`,
    /// two zero-width elements per level) refused correct edits after it.
    /// O(depth · (log R + `TRIVIA_WALK`)).
    fn last_solid_token(&self, x: u32) -> Option<Option<u32>> {
        // The common case first, at the cost of `last_token` (the
        // clean edit path): the last leaf is a token with bytes.
        let Some(leaf) = self.nav_last_leaf(x) else {
            return Some(None);
        };
        let k = self.kind_of(leaf)?;
        if k.is_token() {
            let (a, b) = self.nav_bytes(leaf)?;
            if b > a || matches!(k, K::LiteralScalar | K::FoldedScalar) {
                return Some(Some(leaf));
            }
        }
        let mut at = x;
        // Zero-width siblings passed in a row at the current level.
        let mut run = 0usize;
        loop {
            let k = self.kind_of(at)?;
            let (a, b) = self.nav_bytes(at)?;
            if k.is_token() {
                // An empty block scalar's (zero-width) text is what reads
                // the lines after it: its kind says so.
                if b > a || matches!(k, K::LiteralScalar | K::FoldedScalar) {
                    return Some(Some(at));
                }
            } else if b > a {
                at = self.nav_last(at)?;
                run = 0;
                continue;
            }
            if at == x {
                return Some(None);
            }
            run = run.saturating_add(1);
            if run > TRIVIA_WALK {
                return None;
            }
            // The parent has bytes, so some sibling before `at` holds them
            // (`None` only if that does not hold: refused).
            at = self.nav_prev(at)?;
        }
    }

    /// Where a peek at `x` stops: the end of the line its first token ends
    /// on, without cutting a multi-line token (the line, not what
    /// follows it). `bounded`: in a tree without errors,
    /// at the first token boundary [`PEEK_LOOKAHEAD`] bytes or more past
    /// the first token's start (the parser decides how a node's head reads
    /// within one line and `KEY_WINDOW` bytes); a tree with errors reads
    /// the line (its diagnostics' labels are checked against the window's
    /// end, `labels_cross`).
    fn peek_end(&self, x: u32, bounded: bool) -> Result<(u64, bool), TreeEditError> {
        let t = self.first_token(x).ok_or(TreeEditError::Internal)?;
        let (start, mut end) = self.bytes(t)?;
        let limit =
            (bounded && self.error_count() == 0).then(|| start.saturating_add(PEEK_LOOKAHEAD));
        let mut at = self.nav_succ(t);
        while let Some(s) = at {
            let k = self.kind_of(s).ok_or(TreeEditError::Internal)?;
            if k.is_token() {
                if k == K::LineBreak {
                    break;
                }
                let text = Node::new(self, s).text();
                if text.contains(['\n', '\r']) {
                    break;
                }
                // The line goes on past the bound: cut here.
                if limit.is_some_and(|l| end >= l) {
                    return Ok((end, true));
                }
                end = self.bytes(s)?.1;
            }
            at = self.nav_succ(s);
        }
        Ok((end, false))
    }

    /// The window's end with peek `p` after `base` (the window's end before
    /// it), and the `uncut` record when the peek was cut.
    fn peek_window_end(
        &self,
        p: u32,
        base: u64,
    ) -> Result<(u64, Option<(u32, u64)>), TreeEditError> {
        let (end, cut) = self.peek_end(p, true)?;
        Ok((base.max(end), cut.then_some((p, base))))
    }

    /// `%TAG` directives of `c`'s document, as a document prefix, so the
    /// window's tags resolve as in place.
    fn directives_prefix(&self, c: u32) -> String {
        let mut ctx = self.tag_context(c);
        if !ctx.is_empty() {
            ctx.push_str("---\n");
        }
        ctx
    }

    // ===================================================== trivia shortcut

    /// Trivia edits at document or stream level stay
    /// local: an edit that only adds or removes white space, comments,
    /// and line breaks **inside** an existing run of trivia is well-formed
    /// exactly when the lines it touches still lex as those tokens, and
    /// neither the content before the run nor the content after it can
    /// read the change: the content token before the run is closed (a
    /// comment lies between, or it is a quoted scalar, a bracket, an
    /// indicator, a property, a marker, …; not a block scalar, and a plain
    /// scalar only while no comment is removed), and the line of the
    /// content after the run is untouched. The whole run is under one
    /// parent in the parsed tree (C4) and the edit keeps it there. Checked
    /// in O(line); `Ok(false)` falls back to the window.
    fn trivia_only(
        &self,
        parent: u32,
        prev: Option<u32>,
        next: Option<u32>,
        span: Option<(u32, u32)>,
        removed: Removed,
    ) -> Result<bool, TreeEditError> {
        if !removed.trivia_only {
            return Ok(false);
        }
        let is_run = |x: Option<u32>| {
            x.and_then(|x| self.kind_of(x))
                .is_some_and(|k| matches!(k, K::Whitespace | K::LineBreak | K::Comment))
        };
        // Everything inserted is trivia.
        if let Some((f, l)) = span {
            let mut at = Some(f);
            let mut steps = 0usize;
            loop {
                let Some(x) = at else { return Ok(false) };
                if !is_run(Some(x)) || self.has_diagnostics(x) {
                    return Ok(false);
                }
                if x == l {
                    break;
                }
                steps = steps.saturating_add(1);
                if steps > TRIVIA_WALK {
                    return Ok(false);
                }
                at = self.nav_next(x);
            }
        }
        // Inside an existing run (or at the start of the text or
        // of its first document, decided below).
        if !is_run(prev) && !is_run(next) && prev.is_some() {
            return Ok(false);
        }
        let stream = self.kind_of(parent) == Some(K::Stream);
        // Backwards: the start of the edit's line (its offset, the element
        // the slice starts with, and whether content ends right there),
        // and whether the content before the run is closed (a comment in
        // between closes anything).
        let mut at = prev;
        let mut steps = 0usize;
        let mut line: Option<(u64, Option<u32>, bool)> = None;
        let mut closed = false;
        let mut plain_end = false;
        loop {
            let Some(x) = at else {
                // The start of the first document, with
                // only trivia before it in the stream, is the start of a
                // line nothing before reads (a comment line removed above
                // a root: without this, the whole document was re-read).
                let first_doc = !stream
                    && self.kind_of(parent) == Some(K::Document)
                    && self.content_before(parent)?.is_none()
                    && self.nav_prev(parent).is_none();
                if !(stream || first_doc) {
                    return Ok(false);
                }
                if line.is_none() {
                    let at = if stream { 0 } else { self.bytes(parent)?.0 };
                    line = Some((at, self.nav_first(parent), false));
                }
                break;
            };
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            match k {
                K::Comment => closed = true,
                K::Whitespace => {}
                K::LineBreak => {
                    // The slice starts with this line break: a `\r` before
                    // an inserted `\n` would be one `\r\n` in place.
                    if line.is_none() {
                        line = Some((self.bytes(x)?.0, Some(x), false));
                    }
                }
                _ if k.is_trivia() => return Ok(false),
                _ => {
                    if line.is_none() {
                        line = Some((self.bytes(x)?.1, self.nav_next(x), true));
                    }
                    if !closed {
                        let t = self.last_token(x).ok_or(TreeEditError::Internal)?;
                        let tk = self.kind_of(t).ok_or(TreeEditError::Internal)?;
                        let (a, b) = self.bytes(t)?;
                        let plain_ok = tk == K::PlainScalar && !removed.comment;
                        // A comment removed after a plain
                        // scalar that ends its document (at stream level,
                        // in a tree without errors): no line after it can
                        // continue it when no content follows before the
                        // next document (checked below; a comment line
                        // never continues a plain scalar, section 7.3.3:
                        // `#` after white space starts a comment).
                        // Appending after a last item `- text # c` removes
                        // that comment first: without this, the last item
                        // was re-read whole.
                        plain_end = tk == K::PlainScalar
                            && removed.comment
                            && stream
                            && self.error_count() == 0
                            && !self.settings.get_lenient_inline_colons();
                        if b <= a || !(closes(tk) || plain_ok || plain_end) {
                            return Ok(false);
                        }
                        closed = true;
                    }
                }
            }
            if closed && line.is_some() {
                break;
            }
            steps = steps.saturating_add(1);
            if steps > TRIVIA_WALK {
                return Ok(false);
            }
            at = self.nav_prev(x);
        }
        let Some((s, first, after_content)) = line else {
            return Ok(false);
        };
        // Forwards: a line break before the next content (its line is
        // untouched), which ends the lexed slice. Or the edit's
        // own, when what follows the edit started a line before it and the
        // inserted text ends with a line break (a comment line inserted
        // right before a document: without this, every such insertion
        // re-read the document before and all the trivia between, so k
        // insertions cost O(k²)).
        // (§5.4) A `\r` break ends its line as a `\n` does.
        let own_break = match span {
            Some((_, l)) if removed.next_line_start && self.break_ends_line(l)? => Some(l),
            _ => None,
        };
        let mut at = if own_break.is_some() { None } else { next };
        let mut steps = 0usize;
        // Only white space since the edit (see below), and where the edit
        // is (a removal: where `next` starts).
        let mut ws_only = true;
        let edit_start = match next {
            Some(n) => self.bytes(n)?.0,
            None => self.nav_total().1,
        };
        let e = loop {
            if let Some(l) = own_break {
                break self.bytes(l)?.1;
            }
            let Some(x) = at else {
                if !stream {
                    return Ok(false);
                }
                break self.nav_total().1;
            };
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            if k == K::LineBreak {
                // A `\r` ending the slice could join a `\n` after it
                // (only then; a CR document's breaks are all `\r`).
                if !self.break_ends_line(x)? {
                    return Ok(false);
                }
                break self.bytes(x)?.1;
            }
            if !matches!(k, K::Whitespace | K::Comment) {
                // Whole lines removed right before content
                // that started a line before the edit and still does (only
                // its indentation between): its line is untouched, so the
                // slice ends at the edit (a comment line removed above an
                // entry re-read the collection).
                // Whole lines only when the removal also
                // starts a line (right after a line break, or at the start
                // of the text or of its first document): removing just the
                // line break after a comment ends with a line break too,
                // and joined the next line into the comment (`# c⏎a: 1`
                // became `# ca: 1`, accepted).
                let starts_line = match prev {
                    Some(p) => self.kind_of(p) == Some(K::LineBreak),
                    None => true,
                };
                let whole_lines =
                    span.is_none() && removed.next_line_start && ws_only && starts_line;
                if whole_lines {
                    break edit_start;
                }
                return Ok(false);
            }
            ws_only &= k == K::Whitespace;
            steps = steps.saturating_add(1);
            if steps > TRIVIA_WALK {
                return Ok(false);
            }
            at = self.nav_next(x);
        };
        // (See `plain_end` above: the next content, if any, is a document
        // starting with `---` at the start of a line, which ends the plain
        // scalar's document. O(LONG_RUN + log R).)
        if plain_end {
            let follows = match next {
                Some(n) => self.content_after(n)?,
                None => None,
            };
            if let Some(y) = follows {
                if !self.marker_start(y)? {
                    return Ok(false);
                }
            }
        }
        // The slice lexes, on its own, into exactly our tokens. A `#` right
        // after content is not a comment in place (§6.6), whatever it is
        // on its own.
        let slice = self.try_text_range(s, e)?;
        // A slice that starts with a line break right after
        // content (a same-line comment removed) lexes as in place too: the
        // content's line ends there either way (a closing token never ends
        // with a `\r` a `\n` could join). Without this, removing the
        // comment of `... # c` among N stray markers re-read the stretch up
        // to the next document: O(N) each.
        if after_content && !slice.starts_with([' ', '\t', '\n', '\r']) {
            return Ok(false);
        }
        let again = SyntaxTree::parse_with(slice.as_ref(), &self.settings);
        if !again.diagnostics.is_empty() {
            return Ok(false);
        }
        let mut theirs = again.root().descendants().filter(|n| n.is_token());
        let mut x = first;
        let mut steps = 0usize;
        loop {
            let Some(cur) = x else {
                return Ok(theirs.next().is_none());
            };
            if self.bytes(cur)?.0 >= e {
                return Ok(theirs.next().is_none());
            }
            let k = self.kind_of(cur).ok_or(TreeEditError::Internal)?;
            if !matches!(k, K::Whitespace | K::LineBreak | K::Comment) || self.has_diagnostics(cur)
            {
                return Ok(false);
            }
            let Some(t) = theirs.next() else {
                return Ok(false);
            };
            if t.kind() != k || t.text() != Node::new(self, cur).text() {
                return Ok(false);
            }
            steps = steps.saturating_add(1);
            if steps > TRIVIA_WALK.saturating_mul(4) {
                return Ok(false);
            }
            x = self.nav_next(cur);
        }
    }

    /// §5.4 `b-break` (CR, LF and CRLF alike on
    /// the edit path): `l` is a line break token that ends its line where
    /// it is, so text cut right after it lexes as it does in place. A
    /// `\n`-ended break always does; a `\r`-ended one unless a `\n`
    /// follows it (the two would read as one `\r\n`: the only hazard).
    /// O(log R).
    fn break_ends_line(&self, l: u32) -> Result<bool, TreeEditError> {
        if self.kind_of(l) != Some(K::LineBreak) {
            return Ok(false);
        }
        let text = Node::new(self, l).text();
        if text.ends_with('\n') {
            return Ok(true);
        }
        if !text.ends_with('\r') {
            return Ok(false);
        }
        let end = self.bytes(l)?.1;
        Ok(self.byte_at(end) != Some(b'\n'))
    }

    fn has_diagnostics(&self, x: u32) -> bool {
        let mut any = false;
        self.for_each_diagnostic(x, |_, _, _| any = true);
        any
    }

    // ============================================================ windows

    /// Documents: from the touched ones (or from the document before,
    /// when what the edit changed could continue it), to the start of the
    /// next document, which is peeked. Cheap: a document that starts with
    /// `---`, or follows one ended by `...`, at the start of a line and
    /// right after a document, is read without the one before; trivia
    /// after the last document read is taken only as far as its content
    /// could reach (see [`SyntaxTree::trailing`]).
    fn stream_window(
        &self,
        parent: u32,
        range: Option<(u32, u32)>,
        prev: Option<u32>,
        next: Option<u32>,
        span: Option<(u32, u32)>,
    ) -> Result<Option<Window>, TreeEditError> {
        let empty = |start: u64| Window {
            prefix: String::new(),
            open: None,
            close: None,
            start,
            end: start,
            ours: Vec::new(),
            shape: Shape::Stream,
            depth_offset: 0,
            uncut: None,
            flow: None,
        };
        let Some((lo, hi)) = range else {
            return Ok(Some(empty(0)));
        };
        // The first changed element (or the one right after a removal),
        // and the last changed one (none for a removal).
        let (head, last) = if parent == 0 {
            (span.map(|s| s.0).or(next), span.map(|s| s.1))
        } else {
            (Some(lo), Some(hi))
        };
        // Where to start: the head if it reads the same whatever precedes
        // it, else back to (and with) the document before it.
        let l = match head {
            Some(h) if self.starts_safely(h)? => Some(h),
            Some(h) => Some(self.back_to_document(h)),
            None => prev.map(|p| self.back_to_document(p)),
        };
        // The window starts a line (a stream child indented by white space
        // before it is read with that white space).
        let mut l = l;
        while let Some(x) = l {
            let t = self.first_token(x).unwrap_or(x);
            if self.column_at(self.bytes(t)?.0) == Some(0) {
                break;
            }
            match self.nav_prev(x) {
                Some(p) => l = Some(p),
                None => break,
            }
        }
        let mut kids = Vec::new();
        match (l, last) {
            (Some(l), Some(last)) => self.siblings(l, last, &mut kids),
            // A removal: from the document before up to the gap.
            (Some(l), None) if Some(l) != head => {
                if let Some(p) = prev {
                    self.siblings(l, p, &mut kids);
                }
            }
            _ => {}
        }
        // What follows: trivia the last content could read, then the next
        // element, peeked. The element peeked starts on a line the edit did
        // not touch (its first line's indentation is part of how it
        // reads), unless it starts with `---` right after a removal, and
        // does not start with an error (how an error region reads depends
        // on its later lines); otherwise it is read whole, and the one
        // after it is peeked.
        let mut first = true;
        let (tail, stop) = loop {
            let before = kids.last().copied().or(prev);
            let from = match kids.last() {
                Some(&k) => self.nav_next(k),
                None => head,
            };
            // (the last token with bytes reads what follows.)
            let last_tok = match before {
                Some(b) => match self.last_solid_token(b) {
                    Some(t) => t,
                    None => return Ok(None),
                },
                None => None,
            };
            let (mut tail, stop) = self.trailing(from, last_tok, None, false)?;
            // Stream children only (an error node's text is in its
            // subtree).
            tail.retain(|&x| self.nav_parent(x) == Some(0));
            let broke = tail.iter().any(|&t| self.kind_of(t) == Some(K::LineBreak));
            let Some(p) = stop else {
                break (tail, stop);
            };
            let untouched = broke || !first || (kids.is_empty() && self.marker_start(p)?);
            if untouched && !self.starts_with_error(p)? {
                break (tail, stop);
            }
            first = false;
            kids.append(&mut tail);
            kids.push(p);
        };
        let rest = match (tail.last(), stop) {
            (Some(&t), None) => self.nav_next(t),
            _ => None,
        };
        kids.extend(tail);
        // Past a cut in the trivia, the next element reads the same only
        // if it starts a document with `---`; otherwise (content after a
        // document, an orphan directive, an error region) its reading
        // depends on what precedes it: the run up to it is read, and it is
        // read whole or peeked as above. The
        // next element is found by jumping over the run
        // (`content_after`: O(LONG_RUN + log R)); the run is walked only
        // when it is read, i.e. when that element depends on it.
        let mut stop = stop;
        let mut at = rest;
        while let Some(x) = at {
            let Some(y) = self.content_after(x)? else {
                break;
            };
            if self.marker_start(y)? {
                break;
            }
            let mut t = Some(x);
            while let Some(s) = t.filter(|s| *s != y) {
                kids.push(s);
                t = self.nav_next(s);
            }
            if self.starts_with_error(y)? || self.kind_of(y) != Some(K::Document) {
                // Orphan directives and error regions are read whole, up
                // to the next document.
                kids.push(y);
                at = self.nav_next(y);
                continue;
            }
            stop = Some(y);
            break;
        }
        let start = match kids.first().copied().or(stop) {
            Some(f) => self.bytes(f)?.0,
            None => return Ok(Some(empty(self.nav_total().1))),
        };
        let mut end = match kids.last() {
            Some(&x) => self.bytes(x)?.1,
            None => start,
        };
        let mut ours = vec![Part::Kids(kids)];
        let mut uncut = None;
        if let Some(p) = stop {
            (end, uncut) = self.peek_window_end(p, end)?;
            ours.push(Part::Peek(p));
        }
        Ok(Some(Window {
            start,
            end,
            ours,
            uncut,
            ..empty(start)
        }))
    }

    /// Validation cost is local: stream-level
    /// trivia was inserted, or removed at the end of the text (the line
    /// break of an entry removed after a final block scalar), after a
    /// document `d`
    /// with only trivia between, in a tree without errors, and `d` ends
    /// with its root, a block collection `c` (no `...`). What the insertion
    /// can change is how `c`'s end reads (its last scalar, a block scalar
    /// or a plain one, could read the new text) and how the new tokens
    /// lex; what follows reads the same (after a document not ended by
    /// `...`, a tree without errors has only trivia and documents that
    /// start with `---` at the start of a line). That is exactly the
    /// window of an edit at the end of `c` ([`SyntaxTree::block_window`]:
    /// from the last entry of the deepest collection `c` ends with,
    /// through the trivia after `c` up to a comment at `c`'s column),
    /// provided it covers the inserted tokens (the one line break that
    /// may end a comment cut there lexes the same on its own). `true`
    /// when that window reads back as the tree; `false` falls back to the
    /// document-level window (which re-reads `d` whole: the gha-workflow
    /// outlier, 65 ms at 300 KB for one appended key). The inserted text
    /// must end its line (or the text), so that what follows it starts
    /// its line as before.
    fn document_tail_reads(
        &self,
        prev: Option<u32>,
        next: Option<u32>,
        span: Option<(u32, u32)>,
        removed: Removed,
        edit_at: u64,
    ) -> Result<bool, TreeEditError> {
        if self.error_count() > 0 || !removed.trivia_only {
            return Ok(false);
        }
        // Everything inserted is trivia without diagnostics.
        if let Some((f, l)) = span {
            let mut at = Some(f);
            let mut steps = 0usize;
            loop {
                let Some(x) = at else { return Ok(false) };
                let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
                if !k.is_trivia() || self.has_diagnostics(x) {
                    return Ok(false);
                }
                if x == l {
                    break;
                }
                steps = steps.saturating_add(1);
                if steps > TRIVIA_WALK {
                    return Ok(false);
                }
                at = self.nav_next(x);
            }
        }
        // Where the edit ends in the current text, and whether what follows
        // it keeps its line: the inserted text ends with a line break (so
        // the next line starts where it did, untouched), or the edit ends
        // the text. Otherwise it shares a line with what follows (a space
        // before `---` makes it content), which the window does not read.
        // (A removal is taken only at the end of the text.)
        let total = self.nav_total().1;
        // A `\r`-ended break as a `\n`-ended one.
        let (edit_end, ends_line) = match span {
            Some((_, l)) => (self.bytes(l)?.1, self.break_ends_line(l)?),
            None => (edit_at, false),
        };
        // A removal before the end of the text is taken
        // too, when the window reads past it (below): the element after the
        // gap is then re-read in place, and what lies past the window reads
        // the same (the window's trailing trivia is cut at a comment, or
        // reaches the end). Appending after a root mapping whose last entry
        // ends with footer lines moves those lines into the mapping and
        // removes them after it: without this, the whole document was
        // re-read (O(document) per append).
        let removal = span.is_none();
        if !(ends_line || edit_end >= total || removal) {
            return Ok(false);
        }
        // The document before the edit, with only trivia between.
        let d = match (span, next, prev) {
            (Some((f, _)), _, _) => self.content_before(f)?,
            (None, Some(n), _) => self.content_before(n)?,
            (None, None, Some(p)) if self.kind_of(p).is_some_and(K::is_trivia) => {
                self.content_before(p)?
            }
            (None, None, p) => p,
        };
        let Some(d) = d else {
            return Ok(false);
        };
        if self.kind_of(d) != Some(K::Document) {
            return Ok(false);
        }
        let Some(c) = self.nav_last(d) else {
            return Ok(false);
        };
        let Some(kind) = self.kind_of(c).filter(|k| is_block(*k)) else {
            return Ok(false);
        };
        let Some(last) = self.nav_last(c) else {
            return Ok(false);
        };
        let around = Around {
            prev: Some(last),
            next: None,
            direct: true,
            next_line_kept: false,
            old_column: None,
        };
        let Some(w) = self.block_window(c, kind, last, around)? else {
            return Ok(false);
        };
        // The changed bytes are contiguous, from after `d` (inside the
        // window) to `edit_end`; a final line break just past the window
        // (after a comment it cut at) lexes the same on its own.
        let covered = if removal {
            w.end > edit_at || edit_at >= total
        } else {
            w.end >= edit_end
                || (ends_line
                    && span.is_some_and(|(_, l)| self.bytes(l).is_ok_and(|(la, _)| la == w.end)))
                || (ends_line && self.whole_comment_lines_from(span, w.end)?)
        };
        if !covered {
            return Ok(false);
        }
        Ok(self.reread_uncut(w).is_ok())
    }

    /// The inserted siblings `span` that start at `from`
    /// or later are whole lines of trivia: line breaks, and white space and
    /// comments that start their line (after a line break, or white space
    /// after one). A window cut at a comment (see
    /// [`SyntaxTree::trailing`]) ends what the content before could read,
    /// so such lines after it read as they lex, on their own: comment lines
    /// whatever their column. O(the inserted elements).
    fn whole_comment_lines_from(
        &self,
        span: Option<(u32, u32)>,
        from: u64,
    ) -> Result<bool, TreeEditError> {
        let Some((f, l)) = span else {
            return Ok(false);
        };
        // The insertion starts inside the window, so everything from `from`
        // on is inserted text, lexed as it was in its fragment (never joined
        // with a line break before it: a `\n` after an old `\r`).
        if self.bytes(f)?.0 > from {
            return Ok(false);
        }
        let mut at = Some(f);
        // Whether the current line holds only white space so far, seeded
        // from what precedes the insertion (white
        // space right after a comment on its line is that comment's text,
        // §6.6, never a token of its own; a line break or a block scalar's
        // text ending with one starts a line).
        let mut line_start = self.nav_prev(f).is_some_and(|p| self.ends_with_break(p));
        while let Some(x) = at {
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            let (a, _) = self.bytes(x)?;
            if a >= from {
                match k {
                    K::LineBreak => line_start = true,
                    K::Whitespace if line_start => {}
                    K::Comment if line_start => line_start = false,
                    _ => return Ok(false),
                }
                if self.has_diagnostics(x) {
                    return Ok(false);
                }
            } else {
                line_start = k == K::LineBreak || (line_start && k == K::Whitespace);
            }
            if x == l {
                return Ok(true);
            }
            at = self.nav_next(x);
        }
        Ok(false)
    }

    /// From stream child `x` back to the document before it (included),
    /// or to the stream's first child: orphan directives and error regions
    /// in between are read with it.
    fn back_to_document(&self, x: u32) -> u32 {
        let mut l = x;
        while let Some(p) = self.nav_prev(l) {
            l = p;
            if self.kind_of(p) == Some(K::Document) {
                break;
            }
        }
        l
    }

    /// `x`'s first token is an error's (or skipped) text.
    fn starts_with_error(&self, x: u32) -> Result<bool, TreeEditError> {
        let t = self.first_token(x).ok_or(TreeEditError::Internal)?;
        Ok(matches!(self.kind_of(t), Some(K::ErrorText | K::Skipped)))
    }

    /// A document starting with `---` at the start of a line right after
    /// a document (or at the stream's start): it reads the same whatever
    /// precedes it (a directive does not: after a document not ended by
    /// `...` it is content).
    fn marker_start(&self, x: u32) -> Result<bool, TreeEditError> {
        if self.kind_of(x) != Some(K::Document) {
            return Ok(false);
        }
        let Some(t) = self.first_token(x) else {
            return Ok(false);
        };
        let tk = self.kind_of(t).ok_or(TreeEditError::Internal)?;
        Ok(tk == K::DocumentStartMarker
            && self.column_at(self.bytes(t)?.0) == Some(0)
            && !matches!(self.after_document(x)?, Before::Other))
    }

    /// What precedes stream child `x`, trivia passed over.
    /// A long run of trivia recorded by its arena
    /// (`facts.rs`) is jumped over in O(log R) per overlay run it spans,
    /// so the walk is O(runs crossed + `LONG_RUN` per short piece), never
    /// O(comments before the document); a bounded walk counted
    /// a longer run as [`Before::Other`], which pulled the whole previous
    /// document into the window.
    fn after_document(&self, x: u32) -> Result<Before, TreeEditError> {
        Ok(match self.content_before(x)? {
            None => Before::Start,
            Some(p) if self.kind_of(p) == Some(K::Document) => Before::Document(p),
            Some(_) => Before::Other,
        })
    }

    /// The last stream child before stream child `x` that is not trivia
    /// (`None`: only trivia precedes `x`).
    ///
    /// Edit cost is independent of edit
    /// history): a long run of one arena is jumped over by its facts, and
    /// the overlay runs that are all stream-level trivia by the treap's
    /// sums (`Seq::prev_solid`), so trivia that k edits inserted as k
    /// small arenas costs O(log R), not O(k). O(`LONG_RUN` + log R +
    /// depth) in all (the depth: climbing from the last element before
    /// the trivia to its stream child).
    pub(super) fn content_before(&self, x: u32) -> Result<Option<u32>, TreeEditError> {
        let mut at = self.nav_prev(x);
        while let Some(p) = at {
            let pk = self.kind_of(p).ok_or(TreeEditError::Internal)?;
            if !pk.is_trivia() {
                return Ok(Some(p));
            }
            let s = self.trivia_run_start(p);
            at = match self.jump_trivia(s, false)? {
                Jump::Past(y) => y,
                Jump::Step => self.nav_prev(s),
            };
        }
        Ok(None)
    }

    /// The first stream child at or after stream child `x` that is not
    /// trivia (`None`: only trivia follows). The mirror of
    /// [`SyntaxTree::content_before`].
    pub(super) fn content_after(&self, x: u32) -> Result<Option<u32>, TreeEditError> {
        let mut at = Some(x);
        while let Some(p) = at {
            let pk = self.kind_of(p).ok_or(TreeEditError::Internal)?;
            if !pk.is_trivia() {
                return Ok(Some(p));
            }
            let e = self.trivia_run_end(p);
            at = match self.jump_trivia(e, true)? {
                Jump::Past(y) => y,
                Jump::Step => self.nav_next(e),
            };
        }
        Ok(None)
    }

    /// A stream child that reads the same whatever precedes it, and
    /// whatever precedes it reads the same: it starts a line, right after
    /// a document without diagnostics (or at the stream's start: no
    /// orphan directive or error before it), and is a document starting
    /// with `---`, or follows a document ended by `...` (or none).
    fn starts_safely(&self, x: u32) -> Result<bool, TreeEditError> {
        let t = self.first_token(x).unwrap_or(x);
        if self.column_at(self.bytes(t)?.0) != Some(0) {
            return Ok(false);
        }
        // A byte order mark reads by what precedes it: a document prefix
        // holds one mark, so a second one right behind the first is
        // misplaced.
        if self.kind_of(t) == Some(K::ByteOrderMark) {
            return Ok(false);
        }
        let before = match self.after_document(x)? {
            Before::Other => return Ok(false),
            Before::Start => None,
            Before::Document(d) => Some(d),
        };
        // How an error region reads can depend on what follows it (e.g.
        // an unclosed quote at a `---` or at the end).
        // Nor does a block scalar ending it: blank lines after it are read
        // for its indentation and chomping.
        if let Some(d) = before {
            // Errors only (a warning, a `%YAML 1.1` or `Y0028`,
            // reads the same whatever follows the document: nothing it
            // covers reaches past its line), found in O(log d + log R).
            if self.has_errors_within(d) {
                return Ok(false);
            }
            // (the last token with bytes.)
            let Some(t) = self.last_solid_token(d) else {
                return Ok(false);
            };
            let t = t.and_then(|t| self.kind_of(t));
            if matches!(
                t,
                Some(K::LiteralScalar | K::FoldedScalar | K::BlockScalarHeader)
            ) {
                return Ok(false);
            }
        }
        if self.kind_of(x) == Some(K::Document) && self.kind_of(t) == Some(K::DocumentStartMarker) {
            return Ok(true);
        }
        match before {
            None => Ok(true),
            Some(d) => {
                let t = self.last_token(d).ok_or(TreeEditError::Internal)?;
                Ok(self.kind_of(t) == Some(K::DocumentEndMarker))
            }
        }
    }

    /// Trivia from `from` on (in preorder) that the content ending with
    /// token `last` could still read (a block scalar takes indented and
    /// blank lines, a plain scalar more indented lines): up to and
    /// including the first comment (after a block scalar: the first at
    /// column `max_col` or less, and none for `None`, document level). An error region (error nodes and the lines skipped after
    /// them) is never cut: its repairs depend on what precedes it (C6), so
    /// when one follows the run, the run and the region are taken whole.
    /// Returns the elements and the next other element, when reached
    /// (`None` also when the run was cut at a comment); walking past a
    /// cut costs O(run) steps, no text.
    fn trailing(
        &self,
        from: Option<u32>,
        last: Option<u32>,
        max_col: Option<usize>,
        skip_empty: bool,
    ) -> Result<(Vec<u32>, Option<u32>), TreeEditError> {
        let lk = last.and_then(|t| self.kind_of(t));
        let block_scalar = matches!(
            lk,
            Some(K::LiteralScalar | K::FoldedScalar | K::BlockScalarHeader)
        );
        let errorish = |k: K| matches!(k, K::Error | K::ErrorText | K::Skipped);
        let mut out = Vec::new();
        let mut at = from;
        let mut cut = false;
        while let Some(x) = at {
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            if errorish(k) {
                break;
            }
            if !k.is_trivia() {
                // An empty node (zero-width) is passed over: the re-read
                // side does the same, where cut text may end with one.
                let (a, b) = self.bytes(x)?;
                if !k.is_token() && b <= a && skip_empty {
                    // One that carries diagnostics (a
                    // `Y0028` of the plain scalar before it, attached to
                    // the empty value of an enclosing `? ` entry) is
                    // compared, with them (the re-read side does the
                    // same); passed over, a warning that should leave with
                    // an edited scalar stayed, moved.
                    if self.has_diagnostics(x) {
                        out.push(x);
                    }
                    at = self.nav_after(x);
                    continue;
                }
                return Ok((out, Some(x)));
            }
            out.push(x);
            if k == K::Comment {
                // Only a block scalar reads lines after a comment (an
                // indented `#` line is its content); anything else is
                // ended by the first comment, at any column. A
                // column test for every last token
                // walked, and re-read, a whole run of deeper comments
                // after a plain scalar.
                let ends = if block_scalar {
                    match max_col {
                        Some(m) => {
                            let col = self
                                .column_at(self.bytes(x)?.0)
                                .ok_or(TreeEditError::Internal)?;
                            col <= m
                        }
                        None => false,
                    }
                } else {
                    true
                };
                if ends {
                    cut = true;
                    at = self.nav_succ(x);
                    break;
                }
            }
            at = self.nav_succ(x);
        }
        // Past a cut: only trivia up to the next element, unless that is
        // an error region. A tree without errors has no
        // error region, so the run is not walked at all.
        if cut && self.error_count() == 0 {
            return Ok((out, None));
        }
        if cut {
            let mut look = at;
            while let Some(x) = look {
                let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
                if errorish(k) {
                    break;
                }
                if !k.is_trivia() {
                    return Ok((out, None));
                }
                look = self.nav_succ(x);
            }
            if look.is_none() {
                return Ok((out, None));
            }
        }
        // The rest of the run, then the error region, whole.
        while let Some(x) = at {
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            if !(k.is_trivia() || errorish(k)) {
                return Ok((out, Some(x)));
            }
            out.push(x);
            at = self.nav_succ(x);
        }
        Ok((out, None))
    }

    /// Flow entries (cut at `,`): from the entry the edit starts in to the
    /// first `,` after it, which is peeked (or the closing bracket), read
    /// after the context the collection sits in: `%TAG`s, and a synthetic
    /// block entry at the column of the nearest enclosing block collection
    /// (§7: flow lines are indented past it).
    fn flow_window(
        &self,
        c: u32,
        kind: K,
        lo: u32,
        after: Option<u32>,
    ) -> Result<Option<Window>, TreeEditError> {
        let first = self.nav_first(c);
        let mut l = lo;
        while let Some(p) = self.nav_prev(l) {
            if self.kind_of(p) == Some(K::FlowEntry) {
                break;
            }
            l = p;
        }
        let open = first != Some(l);
        // The bracket written back must be the collection's own: its first
        // child after its properties.
        if open {
            let mut lead = first;
            while let Some(x) = lead {
                let k = self.kind_of(x);
                if !k.is_some_and(|k| k.is_trivia() || matches!(k, K::Anchor | K::Tag)) {
                    break;
                }
                lead = self.nav_next(x);
            }
            if lead.and_then(|x| self.kind_of(x)) != Some(open_of(kind)) {
                return Ok(None);
            }
        }
        // Forwards from the first element after the edit.
        let mut ours_kids = Vec::new();
        let mut x = Some(l);
        let mut past = false;
        let mut peek = None;
        while let Some(s) = x {
            past |= Some(s) == after;
            if past && self.kind_of(s) == Some(K::FlowEntry) && Some(s) != Some(l) {
                peek = Some(s);
                break;
            }
            ours_kids.push(s);
            x = self.nav_next(s);
        }
        let ends = peek.is_none();
        if ends && ours_kids.last().and_then(|&k| self.kind_of(k)) != Some(close_of(kind)) {
            return Ok(None);
        }
        // Context: the nearest enclosing block collection's column.
        let mut prefix = self.directives_prefix(c);
        let mut skeleton = false;
        let mut at = self.nav_parent(c);
        while let Some(a) = at {
            let ak = self.kind_of(a).ok_or(TreeEditError::Internal)?;
            if is_block(ak) {
                let n = self.entry_column(a).ok_or(TreeEditError::Internal)?;
                prefix.extend(core::iter::repeat_n(' ', n));
                prefix.push_str("_: ");
                skeleton = true;
                break;
            }
            if matches!(ak, K::Document | K::Stream) {
                break;
            }
            at = self.nav_parent(a);
        }
        let reread_depth = if skeleton { 2 } else { 1 };
        let start = match ours_kids.first() {
            Some(&f) => self.bytes(f)?.0,
            None => return Ok(None),
        };
        let mut end = match ours_kids.last() {
            Some(&f) => self.bytes(f)?.1,
            None => start,
        };
        let mut ours = vec![Part::Kids(ours_kids)];
        if let Some(p) = peek {
            end = self.bytes(p)?.1;
            ours.push(Part::Peek(p));
        } else {
            ours.push(Part::Leave);
        }
        let bracket = |k: K| {
            if k == K::FlowMapping {
                ('{', '}')
            } else {
                ('[', ']')
            }
        };
        Ok(Some(Window {
            prefix,
            open: open.then_some(bracket(kind).0),
            close: peek.map(|_| bracket(kind).1),
            start,
            end,
            ours,
            shape: Shape::Root {
                kind,
                skeleton,
                open,
                inner: None,
            },
            depth_offset: self.depth_of(c).saturating_sub(reread_depth),
            uncut: None,
            flow: Some(c),
        }))
    }

    /// Block entries: from the entry the edit starts in (or, for an edit
    /// after the last entry's value, from the last entry of the deepest
    /// block collection that value ends with) to the next entry start or
    /// value after the edit, which is peeked; or, when none follows, to
    /// the end of the collection and the trivia after it that its content
    /// could read (see [`SyntaxTree::trailing`]).
    fn block_window(
        &self,
        c: u32,
        kind: K,
        lo: u32,
        around: Around,
    ) -> Result<Option<Window>, TreeEditError> {
        let Around {
            prev,
            next,
            direct,
            next_line_kept,
            old_column,
        } = around;
        let Some(column) = self.entry_column(c) else {
            return Ok(None);
        };
        // In place, a block collection must be indented past its parent
        // collection's entries (a sequence may share a mapping's column,
        // §8.2.1), or it would end its parent's entry instead.
        if let Some(parent) = self.nav_parent(c) {
            let pk = self.kind_of(parent);
            if matches!(pk, Some(K::BlockMapping | K::BlockSequence)) {
                let fits = match self.entry_column(parent) {
                    Some(p) => {
                        column > p
                            || (column == p
                                && kind == K::BlockSequence
                                && pk == Some(K::BlockMapping))
                    }
                    None => false,
                };
                if !fits {
                    return Ok(None);
                }
            }
        }
        // Where to start.
        let mut restart = self.entry_start(c, kind, column, lo);
        let mut inner = None;
        if direct {
            if let Some(p) = prev {
                let v = if self.kind_of(p).is_some_and(K::is_trivia) {
                    self.prev_content(p)
                } else {
                    Some(p)
                };
                if let Some(v) = v {
                    if self.is_value(v) {
                        match self.deepest_last(v)? {
                            Some((dk, ek)) => inner = Some((dk, ek, v)),
                            None => {
                                restart = match self.line_after_value(c, kind, column, v)? {
                                    Some(x0) => Some(x0),
                                    None => self.entry_start(c, kind, column, v),
                                };
                            }
                        }
                    }
                }
            }
        }
        let restart = restart.ok_or(TreeEditError::Internal)?;
        // Where to stop: the first entry start or value at or after `next`.
        let mut peek = None;
        let mut at = if direct { next } else { self.nav_next(lo) };
        let column_entry = self.first_entry(c);
        // A `:` passed since the edit with only trivia after it, and
        // whether a line break followed it.
        let mut colon: Option<bool> = None;
        let scan_start = at;
        // A line break passed since the edit: the line of what follows is
        // untouched (or the element right after the edit started a line
        // before and after it).
        let mut broke = false;
        let mut past_column_entry = false;
        while let Some(x) = at {
            // Never an error node: the repairs after it depend on what
            // precedes it (C6), so it is read whole.
            // Nor a zero-width element: the re-read peeks elements with
            // bytes (zero-width ones at the same offset come first).
            let (xa, xb) = self.bytes(x)?;
            // Nor the collection's first child: it starts an entry only by
            // being first, which the edit may have made it.
            // Nor the entry that sets the collection's column: the edit may
            // have moved it, so it is read, and the next one peeked.
            let error = self.kind_of(x) == Some(K::Error)
                || xb <= xa
                || self.nav_first(c) == Some(x)
                || column_entry == Some(x);
            // An entry start's line prefix is part of how it reads (after
            // `?` or not, its column); a value in the entry that sets the
            // column is not a synchronising point (the next entry is).
            let line_kept = broke || (Some(x) == scan_start && next_line_kept);
            if !error
                && ((line_kept && self.is_entry_start(c, kind, column, x))
                    || (!past_column_entry && self.is_value_sync(x, colon)?))
            {
                peek = Some(x);
                break;
            }
            broke |= self.kind_of(x) == Some(K::LineBreak);
            // The entry that sets a mapping's column was an
            // entry of this mapping before the edit too (it lies after the
            // edit: the removal of the old first entry made it the first
            // one). When the column is unchanged (its line was not touched,
            // or the mapping's column before the edit, in a tree without
            // errors, is this one), it reads as it did, up to its value,
            // which synchronises as any value after a `:` does and is
            // peeked, not read whole (moving a first entry away from a
            // large second one re-read that whole entry). A tree with
            // errors below the mapping may hold entries at other columns:
            // read as before.
            if column_entry == Some(x) {
                let same_column = kind == K::BlockMapping
                    && (line_kept || old_column == Some(column))
                    && !self.has_errors_within(c);
                past_column_entry |= !same_column;
            }
            colon = match self.kind_of(x) {
                Some(K::MappingValue) => Some(false),
                Some(K::LineBreak) => colon.map(|_| true),
                Some(k) if k.is_trivia() => colon,
                _ => None,
            };
            at = self.nav_next(x);
        }
        let from_c = match inner {
            Some((_, _, v)) => self.nav_next(v),
            None => Some(restart),
        };
        let mut kids = Vec::new();
        let mut x = from_c;
        while let Some(s) = x {
            if Some(s) == peek {
                break;
            }
            kids.push(s);
            x = self.nav_next(s);
        }
        let mut ours = Vec::new();
        let (start, prefix_col) = match inner {
            Some((_, ek, _)) => {
                let mut dk_kids = Vec::new();
                let mut y = Some(ek);
                while let Some(s) = y {
                    dk_kids.push(s);
                    y = self.nav_next(s);
                }
                ours.push(Part::Kids(dk_kids));
                ours.push(Part::Leave);
                let at = self.bytes(ek)?.0;
                (at, self.column_at(at).ok_or(TreeEditError::Internal)?)
            }
            None => {
                let at = self.bytes(restart)?.0;
                (at, self.column_at(at).ok_or(TreeEditError::Internal)?)
            }
        };
        let last_kid = kids.last().copied();
        ours.push(Part::Kids(kids));
        let mut end = match last_kid {
            Some(k) => self.bytes(k)?.1,
            None => match inner {
                Some((_, _, v)) => self.bytes(v)?.1,
                None => start,
            },
        };
        let mut uncut = None;
        if let Some(p) = peek {
            (end, uncut) = self.peek_window_end(p, end)?;
            ours.push(Part::Peek(p));
        } else {
            // What follows a block collection starts on a later line; on
            // the same line, the collection's last scalar would run into it.
            if !self.ends_its_line(c)? {
                return Ok(None);
            }
            // (the last token with bytes reads what follows.)
            let Some(last) = self.last_solid_token(c) else {
                return Ok(None);
            };
            let (tail, _) = self.trailing(self.nav_after(c), last, Some(column), true)?;
            if let Some(&t) = tail.last() {
                end = end.max(self.bytes(t)?.1);
            }
            ours.push(Part::Leave);
            ours.push(Part::Lone(tail));
        }
        let mut prefix = self.directives_prefix(c);
        let mut reread_depth = 1usize;
        // Read from its start, the collection sits after its parent's
        // indicator, as in place (its properties' lines and its first
        // line are read against the parent's column, §8.2.1).
        let parent_skeleton = if inner.is_none() && Some(restart) == self.nav_first(c) {
            self.parent_skeleton(c, prefix_col)?
        } else {
            None
        };
        let marker_line = if inner.is_none() && Some(restart) == self.nav_first(c) {
            self.marker_line(c)?
        } else {
            None
        };
        if let Some(skel) = &parent_skeleton {
            prefix.push_str(skel);
            reread_depth = 2;
        } else if let Some(same_line) = marker_line {
            // A document's root on its `---` line, or after it: the marker
            // is read too (§9.1.3: no block collection on that line).
            if !prefix.ends_with("---\n") {
                prefix.push_str("---\n");
            }
            if same_line {
                prefix.truncate(prefix.len().saturating_sub(1));
                if prefix_col < 4 {
                    return Ok(None);
                }
                prefix.extend(core::iter::repeat_n(' ', prefix_col.saturating_sub(3)));
            } else {
                prefix.extend(core::iter::repeat_n(' ', prefix_col));
            }
        } else {
            if inner.is_some() {
                prefix.extend(core::iter::repeat_n(' ', column));
                prefix.push_str(if kind == K::BlockMapping {
                    "_:\n"
                } else {
                    "-\n"
                });
            }
            prefix.extend(core::iter::repeat_n(' ', prefix_col));
        }
        let inner_kind = match inner {
            Some((dk, _, _)) => Some(self.kind_of(dk).ok_or(TreeEditError::Internal)?),
            None => None,
        };
        Ok(Some(Window {
            prefix,
            open: None,
            close: None,
            start,
            end,
            ours,
            shape: Shape::Root {
                kind,
                skeleton: parent_skeleton.is_some(),
                open: false,
                inner: inner_kind,
            },
            depth_offset: self.depth_of(c).saturating_sub(reread_depth),
            uncut,
            flow: None,
        }))
    }

    /// For value `v` of block collection `c` (an entry
    /// that is not a block collection, see [`SyntaxTree::deepest_last`]):
    /// the entry start `x0` of `c` on the line after `v`'s line, when what
    /// lies between is white space, a comment and the line break ending
    /// `v`'s line (no line in between), lexing on its own exactly as those
    /// tokens, and `v` ends with a token that can read nothing after it
    /// there: a closed one (a bracket, a quote, an alias, ...), or a plain
    /// scalar, which a line starting at `c`'s entry column cannot continue
    /// (§7.3.3: continuation lines are indented past the entry). `v`
    /// then ends its entry whatever follows its line, so the window may
    /// start at `x0` (as at any entry start before an edit), and `v`, the
    /// whole previous entry, is not re-read: appending after a last item
    /// written as one long flow collection re-read that item. `None`:
    /// read from `v`'s entry as before. O(depth + `TRIVIA_WALK` + the
    /// line's trivia).
    fn line_after_value(
        &self,
        c: u32,
        kind: K,
        column: usize,
        v: u32,
    ) -> Result<Option<u32>, TreeEditError> {
        let Some(t) = self.last_token(v) else {
            return Ok(None);
        };
        let tk = self.kind_of(t).ok_or(TreeEditError::Internal)?;
        let (ta, tb) = self.bytes(t)?;
        // Diagnostics about `v` may be attached after it (labels before
        // their element: errors, and `Y0028` warnings, whose plain scalar
        // reads under `lenient_inline_colons`, see `labels_cross`); `v` is
        // not re-read, so none may exist.
        let plain = tk == K::PlainScalar;
        let lenient = self.settings.get_lenient_inline_colons();
        if tb <= ta || !(closes(tk) || plain) || (plain && lenient) || self.error_count() > 0 {
            return Ok(None);
        }
        // White space and a comment, then the line break.
        let mut ours = Vec::new();
        let mut at = self.nav_next(v);
        let lb = loop {
            let Some(x) = at else { return Ok(None) };
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            if self.has_diagnostics(x) {
                return Ok(None);
            }
            match k {
                K::LineBreak => {
                    ours.push(x);
                    break x;
                }
                K::Whitespace | K::Comment => ours.push(x),
                _ => return Ok(None),
            }
            if ours.len() > TRIVIA_WALK {
                return Ok(None);
            }
            at = self.nav_next(x);
        };
        if !self.break_ends_line(lb)? {
            return Ok(None);
        }
        // The next line starts an entry of `c` at its column.
        // An edited tree agrees with its own text:
        // the window starts at `x0` and the re-read writes the line's
        // indentation as `column` spaces, so its real indentation is never
        // re-read. It must therefore be exactly those spaces: a tab of the
        // same byte length passes the column test (columns count bytes)
        // but reads differently in place (§6.1: tabs are never
        // indentation), and an edit that put one there was accepted.
        let mut x0 = self.nav_next(lb);
        if let Some(w) = x0.filter(|w| self.kind_of(*w) == Some(K::Whitespace)) {
            if self.has_diagnostics(w) || !Node::new(self, w).text().bytes().all(|b| b == b' ') {
                return Ok(None);
            }
            x0 = self.nav_next(w);
        }
        let Some(x0) = x0 else { return Ok(None) };
        let (xa, xb) = self.bytes(x0)?;
        if xb <= xa
            || self.kind_of(x0) == Some(K::Error)
            || !self.is_entry_start(c, kind, column, x0)
        {
            return Ok(None);
        }
        // The tokens after `v` through the line break lex on their own as
        // ours (a `#` right after `v` would not be a comment in place).
        let (_, end) = self.bytes(lb)?;
        let slice = self.try_text_range(tb, end)?;
        if !slice.starts_with([' ', '\t', '\n', '\r']) {
            return Ok(None);
        }
        let again = SyntaxTree::parse_with(slice.as_ref(), &self.settings);
        if !again.diagnostics.is_empty() {
            return Ok(None);
        }
        let mut theirs = again.root().descendants().filter(|n| n.is_token());
        for &x in &ours {
            let Some(y) = theirs.next() else {
                return Ok(None);
            };
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            if y.kind() != k || y.text() != Node::new(self, x).text() {
                return Ok(None);
            }
        }
        if theirs.next().is_some() {
            return Ok(None);
        }
        Ok(Some(x0))
    }

    /// For a document's root `c`: `Some(true)` when it is on its `---`
    /// line (only then does the marker change how it reads; O(1): the
    /// white space between them is one token).
    fn marker_line(&self, c: u32) -> Result<Option<bool>, TreeEditError> {
        if self.nav_parent(c).and_then(|p| self.kind_of(p)) != Some(K::Document) {
            return Ok(None);
        }
        let mut at = self.nav_prev(c);
        while let Some(x) = at {
            match self.kind_of(x).ok_or(TreeEditError::Internal)? {
                K::DocumentStartMarker => return Ok(Some(true)),
                K::Whitespace => {}
                _ => return Ok(None),
            }
            at = self.nav_prev(x);
        }
        Ok(None)
    }

    /// For block collection `c` whose first line is at column `col`: the
    /// synthetic entry of its parent block collection that puts `c` where
    /// it is: the parent's indicator (`_:`, `-`, `?`) at the parent's
    /// column, then `c` on the same line or the next, at `col`. `None`
    /// at document level, or when the indicator's line cannot be
    /// reproduced.
    fn parent_skeleton(&self, c: u32, col: usize) -> Result<Option<String>, TreeEditError> {
        let Some(p) = self.nav_parent(c) else {
            return Ok(None);
        };
        if !self.kind_of(p).is_some_and(is_block) {
            return Ok(None);
        }
        let Some(pcol) = self.entry_column(p) else {
            return Ok(None);
        };
        let mut same_line = true;
        let mut at = self.nav_prev(c);
        let indicator = loop {
            let Some(x) = at else { return Ok(None) };
            match self.kind_of(x).ok_or(TreeEditError::Internal)? {
                K::MappingValue => break "_:",
                K::SequenceEntry => break "-",
                K::MappingKey => break "?",
                K::LineBreak => same_line = false,
                K::Whitespace | K::Comment => {}
                _ => return Ok(None),
            }
            at = self.nav_prev(x);
        };
        let mut out = String::new();
        out.extend(core::iter::repeat_n(' ', pcol));
        out.push_str(indicator);
        if same_line {
            let used = pcol.saturating_add(indicator.len());
            if col <= used {
                return Ok(None);
            }
            out.extend(core::iter::repeat_n(' ', col.saturating_sub(used)));
        } else {
            out.push('\n');
            out.extend(core::iter::repeat_n(' ', col));
        }
        Ok(Some(out))
    }

    /// `x` starts an entry of block collection `c`: its first child, or a
    /// key, `?`, or `-` first on its line at the entry column.
    fn is_entry_start(&self, c: u32, kind: K, column: usize, x: u32) -> bool {
        if self.nav_first(c) == Some(x) {
            return true;
        }
        let Some(k) = self.kind_of(x) else {
            return false;
        };
        // A key (a block collection is never one) or `?`; `: v` with an
        // empty key starts with that key's zero-width scalar.
        let entry = if kind == K::BlockMapping {
            matches!(
                k,
                K::Scalar | K::Alias | K::FlowMapping | K::FlowSequence | K::MappingKey | K::Error
            )
        } else {
            matches!(k, K::SequenceEntry | K::Error)
        };
        entry && self.line_column(x) == Some(column)
    }

    /// The start of the entry of `c` holding child `x`. An entry whose key
    /// is empty (`: v` at the start of a line) is not one to start from:
    /// after an explicit key, that `:` is the key's value.
    fn entry_start(&self, c: u32, kind: K, column: usize, x: u32) -> Option<u32> {
        let mut at = x;
        loop {
            if self.is_entry_start(c, kind, column, at) {
                let empty = self.nav_bytes(at).is_some_and(|(a, b)| b <= a);
                if !empty || self.nav_first(c) == Some(at) {
                    return Some(at);
                }
            }
            at = self.nav_prev(at)?;
        }
    }

    /// `x` is a value: a node right after `:` or `-` (not an explicit key
    /// after `?`, whose entry goes on with its `:`).
    fn is_value(&self, x: u32) -> bool {
        self.kind_of(x).is_some_and(|k| !k.is_token())
            && self
                .prev_content(x)
                .and_then(|p| self.kind_of(p))
                .is_some_and(|k| matches!(k, K::MappingValue | K::SequenceEntry))
    }

    /// `x` is a value with bytes after a `:` that lies after the edit
    /// (`colon`: `Some(broke)`, with only trivia between, `broke` when a
    /// line break is among it): the parser reads it in the same state as
    /// before, whatever the key before was, once the key reads the same.
    /// A block collection must start on a later line (its own column
    /// matters; the untouched line keeps it); anything else on the
    /// `:`'s line is read against the mapping's column, not its own.
    fn is_value_sync(&self, x: u32, colon: Option<bool>) -> Result<bool, TreeEditError> {
        let (Some(k), Some(broke)) = (self.kind_of(x), colon) else {
            return Ok(false);
        };
        if k.is_token() || k == K::Error {
            return Ok(false);
        }
        let (a, b) = self.bytes(x)?;
        Ok(b > a && (broke || !is_block(k)))
    }

    /// For value `v` that is a block collection: the deepest block
    /// collection its last entries end with, and the start of that
    /// collection's last entry.
    ///
    /// The chain of last children is found once
    /// ([`SyntaxTree::last_chain`], O(depth)); the candidates along it
    /// (block collections, each the value of the one before) are cheap to
    /// test, and only the deepest one's entry start is looked for. That
    /// is O(depth) where one `nav_last`, one column, and one entry search
    /// per level cost O(depth²).
    fn deepest_last(&self, v: u32) -> Result<Option<(u32, u32)>, TreeEditError> {
        let chain = self.last_chain(v);
        // The collections `x` of the chain the old walk would visit: each
        // a block collection whose parent's last child it is and a value.
        let mut candidates: Vec<(u32, u32)> = Vec::new();
        let mut i = 0usize;
        while let Some(&x) = chain.get(i) {
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            if !is_block(k) {
                break;
            }
            let Some(&last) = chain.get(i.saturating_add(1)) else {
                break;
            };
            candidates.push((x, last));
            if !self.is_value(last) {
                break;
            }
            i = i.saturating_add(1);
        }
        // The deepest candidate whose entry start is found (the old walk
        // stopped at the first level where one was not, keeping the level
        // above; so the deepest found below no failure is the answer).
        // Without errors below `v`, every level has its entry start (the
        // shortcut below); with errors, each level is checked in turn.
        let clean = !self.has_errors_within(v);
        // The columns of all levels at once,
        // sharing line starts (one `column_at` per line, not per level:
        // nested entries on one line made each level's O(column) scan add
        // up to O(depth²) when an error forced the level-by-level walk).
        let columns = self.entry_columns(&candidates);
        let mut found = None;
        for (i, &(x, last)) in candidates.iter().enumerate() {
            let k = self.kind_of(x).ok_or(TreeEditError::Internal)?;
            let Some(col) = columns.get(i).copied().flatten() else {
                break;
            };
            let Some(es) = self.entry_start(x, k, col, last) else {
                break;
            };
            found = Some((x, es));
            // The levels in between only need to succeed; test the
            // deepest directly and stop when it does.
            if let Some(&(dx, dl)) = candidates.last().filter(|_| clean) {
                if dx != x {
                    let dk = self.kind_of(dx).ok_or(TreeEditError::Internal)?;
                    let deep = columns
                        .last()
                        .copied()
                        .flatten()
                        .and_then(|c| self.entry_start(dx, dk, c, dl));
                    if let Some(es) = deep {
                        return Ok(Some((dx, es)));
                    }
                }
            }
        }
        Ok(found)
    }

    /// [`SyntaxTree::entry_column`] of each candidate collection of
    /// [`SyntaxTree::deepest_last`] (each nested in the one before, so
    /// their first entries come in text order). Computed deepest first: a
    /// level whose first entry lies on the line of the one below it, at or
    /// after that line's start, gets its column by subtraction (no line
    /// break lies between), so each line is scanned once. O(depth + log R
    /// per level + the lines' prefixes).
    fn entry_columns(&self, candidates: &[(u32, u32)]) -> Vec<Option<usize>> {
        let mut out = vec![None; candidates.len()];
        // (line start, an offset on that line at or after every offset
        // computed from it)
        let mut line: Option<(u64, u64)> = None;
        for (i, &(x, _)) in candidates.iter().enumerate().rev() {
            let Some(at) = self
                .first_entry(x)
                .and_then(|e| self.nav_rank(e))
                .map(|(_, b)| b)
            else {
                continue;
            };
            let col = match line {
                Some((start, upto)) if start <= at && at <= upto => {
                    usize::try_from(at.saturating_sub(start)).ok()
                }
                _ => {
                    let col = self.column_at(at);
                    if let Some(c) = col.and_then(|c| u64::try_from(c).ok()) {
                        line = Some((at.saturating_sub(c), at));
                    }
                    col
                }
            };
            if let Some(slot) = out.get_mut(i) {
                *slot = col;
            }
        }
        out
    }

    /// `c`'s text ends a line: a line break follows before any content.
    fn ends_its_line(&self, c: u32) -> Result<bool, TreeEditError> {
        let last = self.last_token(c).map(|t| Node::new(self, t).text());
        if last.is_some_and(|t| t.ends_with(['\n', '\r'])) {
            return Ok(true);
        }
        let mut at = self.nav_after(c);
        while let Some(t) = at {
            let k = self.kind_of(t).ok_or(TreeEditError::Internal)?;
            if k == K::LineBreak {
                return Ok(true);
            }
            if k.is_token() && !k.is_trivia() {
                let (x, y) = self.bytes(t)?;
                if y > x {
                    return Ok(false);
                }
            }
            at = self.nav_succ(t);
        }
        Ok(true)
    }

    /// The siblings `l..=h`, pushed in order.
    fn siblings(&self, l: u32, h: u32, out: &mut Vec<u32>) {
        let mut x = Some(l);
        while let Some(s) = x {
            out.push(s);
            if s == h {
                break;
            }
            x = self.nav_next(s);
        }
    }

    /// The column of a block collection's first entry (after its
    /// properties).
    pub(super) fn entry_column(&self, c: u32) -> Option<usize> {
        self.column_at(self.nav_rank(self.first_entry(c)?)?.1)
    }

    /// A block collection's first child after its properties.
    fn first_entry(&self, c: u32) -> Option<u32> {
        let mut x = self.nav_first(c);
        while let Some(ch) = x {
            let k = self.kind_of(ch)?;
            if !(k.is_trivia() || matches!(k, K::Anchor | K::Tag)) {
                return Some(ch);
            }
            x = self.nav_next(ch);
        }
        None
    }

    /// The column of `x` when it starts a line after a sibling line break
    /// (and indentation).
    fn line_column(&self, x: u32) -> Option<usize> {
        let p = self.nav_prev(x)?;
        match self.kind_of(p)? {
            K::LineBreak => Some(0),
            K::Whitespace if self.nav_prev(p).is_some_and(|q| self.ends_with_break(q)) => {
                // Bytes, as the parser counts columns.
                Some(Node::new(self, p).text().len())
            }
            // After a value whose block scalar holds
            // the line break (an entry after `a: |` or `a: >-` text), `x`
            // starts its line too; not seeing it made a window start at
            // the entry before, which may be a whole large value.
            _ if self.ends_with_break(p) => Some(0),
            _ => None,
        }
    }

    /// `q` is a line break, or a node whose last token is a block
    /// scalar's text ending with one. O(depth + log R) for a node.
    fn ends_with_break(&self, q: u32) -> bool {
        match self.kind_of(q) {
            Some(K::LineBreak) => true,
            Some(k) if !k.is_token() => self
                .last_token(q)
                .filter(|t| matches!(self.kind_of(*t), Some(K::LiteralScalar | K::FoldedScalar)))
                .is_some_and(|t| Node::new(self, t).text().ends_with(['\n', '\r'])),
            _ => false,
        }
    }

    // ============================================================ re-read

    /// Re-reads the window in its context and compares.
    fn reread(&self, w: &Window) -> Result<(), TreeEditError> {
        super::cost::count_reread(w.end.saturating_sub(w.start));
        #[cfg(test)]
        REREADS.with(|n| n.set(n.get().saturating_add(1)));
        #[cfg(test)]
        REREAD_BYTES.with(|n| {
            let len = usize::try_from(w.end.saturating_sub(w.start)).unwrap_or(usize::MAX);
            n.set(n.get().saturating_add(len));
        });
        let limits = self.settings.get_limits();
        // Depth counts from the tree's root, not the window's.
        let max =
            limits
                .get_max_depth()
                .checked_sub(w.depth_offset)
                .ok_or(TreeEditError::Limit {
                    code: DEPTH_LIMIT_EXCEEDED,
                })?;
        // The window is re-read with every diagnostic's texts (a
        // window is local), compared with the tree's as `same_diagnostic`
        // says.
        let settings = self
            .settings
            .clone()
            .limits(limits.clone().max_depth(max).max_diagnostics(usize::MAX));
        let mut text = w.prefix.clone();
        if let Some(o) = w.open {
            text.push(o);
        }
        // The window's text is read once.
        let body = self.try_text_range(w.start, w.end)?;
        // A `\r\n` split by the window's ends would read as one break in
        // place.
        let before = w.start.checked_sub(1).and_then(|b| self.byte_at(b));
        let after = self.byte_at(w.end);
        if (before == Some(b'\r') && body.starts_with('\n'))
            || (after == Some(b'\n') && body.ends_with('\r'))
        {
            return Err(TreeEditError::NotWellFormed);
        }
        text.push_str(&body);
        drop(body);
        if let Some(c) = w.close {
            text.push(c);
        }
        let again = SyntaxTree::parse_with(text, &settings);
        let origin = usize::try_from(w.start).map_err(|_| TreeEditError::Internal)?;
        let origin_again = w.prefix.len().saturating_add(usize::from(w.open.is_some()));
        let mut ours = Vec::new();
        let mut peek_at = None;
        for part in &w.ours {
            match part {
                Part::Kids(k) => {
                    for &x in k {
                        push_subtree(self, Node::new(self, x), origin, &mut ours);
                    }
                }
                Part::Leave => ours.put(Item::Leave),
                Part::Lone(k) => {
                    for &x in k {
                        push_lone(self, Node::new(self, x), origin, &mut ours);
                    }
                }
                Part::Peek(p) => {
                    let n = Node::new(self, *p);
                    peek_at = Some(relative(n.span().start, origin));
                    push_peek(n, origin, &mut ours);
                }
            }
        }
        // Trivia after the collection sits where the parser puts it: at
        // the outermost level that continues after it, so outside error
        // regions no node but the stream ends with trivia (C4, "ends
        // early"). The lone comparison does not see placement, so it is
        // checked here (e.g. an edit that removes an error region that
        // kept its document open).
        let erroneous = ours
            .iter()
            .any(|i| matches!(i, Item::Diagnostic(d) if d.is_error()));
        if !erroneous {
            let mut checked = None;
            for part in &w.ours {
                let Part::Lone(k) = part else { continue };
                for &t in k {
                    let p = self.nav_parent(t);
                    if p == checked || p == Some(0) {
                        continue;
                    }
                    checked = p;
                    let last = p
                        .and_then(|p| self.nav_last(p))
                        .and_then(|l| self.kind_of(l));
                    if last.is_some_and(K::is_trivia) {
                        return Err(TreeEditError::NotWellFormed);
                    }
                }
            }
        }
        // A window that reaches the end of the text also reads
        // the warnings after the last element (attached to no element: a
        // `Y0028` whose scalar ends the text). Removing the line break
        // after such a scalar moved its warning there in the re-read,
        // while the tree dropped it with the line break.
        let at_end = w.end >= self.nav_total().1;
        if at_end {
            let base_len = self.kinds.len();
            let lo = self
                .diagnostics
                .partition_point(|(n, _)| (*n as usize) < base_len);
            let to = usize::try_from(self.nav_total().1).map_err(|_| TreeEditError::Internal)?;
            for (_, d) in self.diagnostics.get(lo..).unwrap_or_default() {
                let mut d = d.clone();
                d.shift(self.source.len(), to);
                d.shift(origin, 0);
                ours.put(Item::Diagnostic(d));
            }
        }
        // The re-read side is compared as it is read.
        let mut theirs = Compare::new(&ours);
        let placed = self.theirs(&again, w, origin_again, peek_at, &mut theirs);
        if placed.is_some() && at_end {
            let n = again.kinds.len();
            let lo = again
                .diagnostics
                .partition_point(|(i, _)| (*i as usize) < n);
            for (_, d) in again.diagnostics.get(lo..).unwrap_or_default() {
                let mut d = d.clone();
                d.shift(origin_again, 0);
                theirs.put(Item::Diagnostic(d));
            }
        }
        if placed.is_some() && theirs.equal() {
            return Ok(());
        }
        // A limit the re-read reports and the window does not have.
        let limit = again.diagnostics.iter().map(|(_, d)| d.code()).find(|c| {
            is_limit(*c)
                && !ours
                    .iter()
                    .any(|i| matches!(i, Item::Diagnostic(d) if d.code() == *c))
        });
        match limit {
            Some(code) => Err(TreeEditError::Limit { code }),
            None => Err(TreeEditError::NotWellFormed),
        }
    }

    /// The re-read side of the window, put into `out`; `None` when its
    /// container is not where the context put it.
    fn theirs<'a>(
        &self,
        again: &'a SyntaxTree,
        w: &Window,
        origin: usize,
        peek: Option<usize>,
        out: &mut impl Sink<'a>,
    ) -> Option<()> {
        let (kind, skeleton, open, inner) = match w.shape {
            Shape::Stream => {
                push_children(again, again.root(), 0, None, origin, peek, out);
                return Some(());
            }
            Shape::Root {
                kind,
                skeleton,
                open,
                inner,
            } => (kind, skeleton, open, inner),
        };
        let doc = again.root().children().find(|n| n.kind() == K::Document)?;
        let r = doc.children().find(|n| !n.is_token())?;
        let c = if skeleton {
            value_after_indicator(r)?
        } else {
            r
        };
        if c.kind() != kind {
            return None;
        }
        let ends = w.ours.iter().any(|p| matches!(p, Part::Lone(_)))
            || (w.ours.last().is_some_and(|p| matches!(p, Part::Leave)));
        let lone = w.ours.iter().any(|p| matches!(p, Part::Lone(_)));
        match inner {
            Some(ik) => {
                let d = value_after_indicator(c)?;
                if d.kind() != ik || d.span().start != w.prefix.len() {
                    return None;
                }
                if push_children(again, d, 0, None, origin, peek, out) {
                    return Some(());
                }
                out.put(Item::Leave);
                if push_children(again, c, 0, Some(d), origin, peek, out) {
                    return Some(());
                }
            }
            None => {
                if c.span().start != w.prefix.len() {
                    return None;
                }
                let skip = usize::from(open);
                if open && c.first_child().map(|k| k.kind()) != Some(open_of(kind)) {
                    return None;
                }
                if push_children(again, c, skip, None, origin, peek, out) {
                    return Some(());
                }
            }
        }
        if ends {
            out.put(Item::Leave);
        }
        if lone {
            let past = c.preorder_index().saturating_add(c.descendants().len());
            // As ours (see `trailing`): trivia and error regions, up to the
            // next other element (a skeleton's own rest, or nothing).
            // Zero-width nodes (an empty node the cut text ends with) are
            // passed over on both sides.
            for x in again.root().descendants().skip(past) {
                let k = x.kind();
                if !k.is_token() && x.span().is_empty() {
                    // (with its diagnostics, as ours.)
                    if again.has_diagnostics(x.slot()) {
                        push_lone(again, x, origin, out);
                        if out.failed() {
                            break;
                        }
                    }
                    continue;
                }
                if k.is_token() && x.span().is_empty() && k == K::PlainScalar {
                    continue;
                }
                if !(k.is_trivia() || matches!(k, K::Error | K::ErrorText | K::Skipped)) {
                    break;
                }
                push_lone(again, x, origin, out);
                if out.failed() {
                    break;
                }
            }
        }
        Some(())
    }
}

#[cfg(test)]
thread_local! {
    /// Test instrumentation: how many windows this thread
    /// re-read, so tests can pin which edits the trivia shortcut decides
    /// locally (`tree/tests.rs`). Not compiled outside tests.
    pub(super) static REREADS: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
    /// Test instrumentation: how many bytes of the edited text
    /// the windows re-read on this thread so far, so tests can pin that an
    /// edit's validation does not grow with the document.
    pub(super) static REREAD_BYTES: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

/// Where a block collection's edit is (see [`SyntaxTree::block_window`]).
#[derive(Clone, Copy)]
struct Around {
    /// The siblings before and after the edit, when the edit is directly
    /// under the collection (`direct`).
    prev: Option<u32>,
    next: Option<u32>,
    direct: bool,
    /// The element after the edit starts a line before and after it.
    next_line_kept: bool,
    /// The collection's entry column before the edit, when the edit is at
    /// its start (see [`Removed::old_column`]).
    old_column: Option<usize>,
}

/// How an edit relates to recovered input (see
/// [`SyntaxTree::touches_recovered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Recovered {
    /// Nothing recovered is involved.
    No,
    /// The edit lies in this document, which has errors in its content
    /// (its neighbours are not involved).
    Document(u32),
    /// The edit may change how recovered input reads: refused.
    Yes,
}

/// How a stream child reads whatever precedes it (see
/// [`SyntaxTree::fresh_after`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fresh {
    /// It starts with `---` at the start of a line.
    Marker,
    /// It starts a line after document `by`, ended by the `...` at byte
    /// `marker`.
    Ended { by: u32, marker: u64 },
    /// Neither.
    No,
}

/// What precedes a stream child (see [`SyntaxTree::after_document`]).
enum Before {
    /// The stream's start.
    Start,
    /// A document.
    Document(u32),
    /// Anything else: an orphan directive, an error region.
    Other,
}

/// What an edit removed, for the trivia shortcut.
#[derive(Clone, Copy, Debug)]
pub(super) struct Removed {
    /// Only trivia tokens (or nothing).
    pub(super) trivia_only: bool,
    /// A comment among them.
    pub(super) comment: bool,
    /// The element after the edit started a line before the edit (the
    /// last removed element, or the one before the edit, was a line
    /// break).
    pub(super) next_line_start: bool,
    /// Bytes the edit inserted.
    pub(super) added: u64,
    /// Bytes the edit removed.
    pub(super) removed: u64,
    /// A later splice of the same edit (`SyntaxTree::splice_pair`): the
    /// elements from this rank on moved by that many more bytes.
    pub(super) later: Option<(u64, i128)>,
    /// For an edit at the start of a block mapping (no element
    /// before it there) in a tree without errors, the mapping's entry
    /// column before the edit.
    pub(super) old_column: Option<usize>,
    /// The removed text holds a line break.
    pub(super) line_break: bool,
}
