//! The syntax tree as a [`Source`].
//!
//! `Document` → `Value` reads the tree the parser's events built, as the
//! same semantic events [`super::feed::Feed`] reads from the parser: the
//! tree mirrors the event stream (a node per `…Start`/`…End` pair, scalar,
//! alias, and `Error` event), so a preorder walk with enter and leave steps
//! replays it. Two placements differ from the stream, and the walk undoes
//! them: a node's properties (and a document's directives) are its first
//! children in the tree but came before its event in the stream, so they
//! are read — errors, warnings, `%YAML` — before the node's event is
//! returned.
//!
//! The walk is the tree's own iterative [`Walk`] (no recursion, no
//! stack; O(1) per step). The consistency test checks that this source and
//! the parser's feed yield identical events, spans included, on the whole
//! corpus.

use std::borrow::Cow;

use oneyaml_core::{Diagnostic, Severity, Span};
use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind, SyntaxTree, Walk, WalkEvent};

use super::feed::is_yaml11;
use super::{Ev, Node, Source, Tag, TagCache};
use crate::error::Error;

/// Semantic events read from a [`SyntaxTree`] (or one document of it).
pub(crate) struct TreeSource<'a> {
    tree: &'a SyntaxTree,
    walk: Walk<'a>,
    /// Warnings with the preorder index of the element they precede.
    /// `(index, start, warning)`, the warning `None` for a
    /// skeleton (counted only), so this list costs 24 bytes per entry
    /// whatever the cap.
    warnings: Vec<Warning<'a>>,
    next_warning: usize,
    collected: crate::diag_cap::Capped,
    /// The most recently entered element (the last descendant of a node
    /// when that node is left).
    last_entered: Option<TreeNode<'a>>,
    yaml11_next: bool,
    end: usize,
    done: bool,
    /// Each distinct tag resolved once.
    tags: TagCache<'a>,
}

/// A warning the replay will meet: its element's preorder index, its
/// start, and the warning (`None`: a skeleton).
type Warning<'a> = (usize, usize, Option<Box<Cow<'a, Diagnostic>>>);

/// Kinds that may precede a YAML node's content among its children (its
/// properties, header, and what lies between them).
fn is_node_prefix(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Anchor
            | SyntaxKind::Tag
            | SyntaxKind::BlockScalarHeader
            | SyntaxKind::Whitespace
            | SyntaxKind::LineBreak
            | SyntaxKind::Comment
            | SyntaxKind::Error
            | SyntaxKind::Skipped
    )
}

/// Kinds that may precede a document's `---` among its children. A stray
/// `...` after directives (`%X\n...\n---`, whose error the document
/// adopts) is one of them (a fuzz finding: without it the replay
/// skipped the document's prefix, and its error, as the parser's order
/// has it before the `DocStart`).
fn is_document_prefix(k: SyntaxKind) -> bool {
    is_node_prefix(k)
        || matches!(
            k,
            SyntaxKind::YamlDirective
                | SyntaxKind::TagDirective
                | SyntaxKind::ReservedDirective
                | SyntaxKind::ByteOrderMark
                | SyntaxKind::StrayDocumentEndMarker
        )
}

/// The version of a `%YAML` directive token.
fn directive_version(text: &str) -> Option<&str> {
    text.strip_prefix("%YAML")?.split_whitespace().next()
}

/// Whether a `Document` node carries a `%YAML 1.1` (or 1.0) directive:
/// the `yaml11` of its `DocStart`.
pub(crate) fn yaml11_of(doc: TreeNode<'_>) -> bool {
    let mut yaml11 = false;
    for c in doc.children().take_while(|c| is_document_prefix(c.kind())) {
        if c.kind() == SyntaxKind::YamlDirective {
            yaml11 = directive_version(&c.text()).is_some_and(is_yaml11);
        }
    }
    yaml11
}

/// The span of a `Document` node's `DocStart` event: its `---`, or empty
/// at its start for an implicit document.
pub(crate) fn doc_start_span(n: TreeNode<'_>) -> Span {
    n.children()
        .find(|c| !is_document_prefix(c.kind()))
        .filter(|c| c.kind() == SyntaxKind::DocumentStartMarker)
        .map_or_else(|| Span::empty(n.span().start), TreeNode::span)
}

fn error_at(n: TreeNode<'_>) -> Error {
    // Past `max_diagnostics` the tree keeps the error's skeleton
    // (code, severity, spans): the same error, found in O(log n), with its
    // code's summary for a message.
    let limit = n.tree().settings().get_limits().get_max_diagnostics();
    match n.diagnostic() {
        Some(d) => Error::from_diagnostic(crate::diag_cap::filled(d.into_owned(), limit)),
        None => {
            // Every error node carries its diagnostic (tree docs).
            debug_assert!(false, "error node without a diagnostic");
            Error::internal("error node without a diagnostic", Some(n.span()))
        }
    }
}

impl<'a> TreeSource<'a> {
    /// Reads the whole tree.
    pub(crate) fn new(tree: &'a SyntaxTree) -> Self {
        Self::over(tree, tree.root())
    }

    /// Reads the subtree of `root` (the stream, or one document), then
    /// `StreamEnd`.
    pub(crate) fn over(tree: &'a SyntaxTree, root: TreeNode<'a>) -> Self {
        // A document's warnings: those attached inside it, found by binary
        // search (O(log n) per document, not a scan of the stream's).
        let attached: Box<dyn Iterator<Item = _>> = if root.kind() == SyntaxKind::Stream {
            Box::new(tree.diagnostics_by_element())
        } else {
            Box::new(tree.diagnostics_within(root))
        };
        let warnings = attached
            .filter(|(_, d)| d.severity == Severity::Warning)
            .map(|(n, d)| {
                let at = n.map_or(usize::MAX, TreeNode::preorder_index);
                let start = d.primary.span.start;
                let keep = (!SyntaxTree::is_skeleton(&d)).then(|| Box::new(d));
                (at, start, keep)
            })
            .collect();
        Self {
            tree,
            walk: root.walk(),
            warnings,
            next_warning: 0,
            collected: crate::diag_cap::Capped::with_keep(usize::MAX),
            last_entered: None,
            yaml11_next: false,
            end: tree.source().len(),
            done: false,
            tags: TagCache::default(),
        }
    }

    /// Reads tags through `doc`, the document's table (node
    /// reads share each `%TAG`-prefixed tag, never resolving it again).
    pub(crate) fn with_doc_tags(mut self, doc: &'a crate::document::tags::DocTags) -> Self {
        self.tags = TagCache::with_doc(doc);
        self
    }

    /// Collects the warnings the stream reported before element `index`
    /// (inclusive: a warning is attached to the element that followed it).
    #[inline]
    fn see(&mut self, index: usize) {
        while let Some((at, start, d)) = self.warnings.get(self.next_warning) {
            if *at > index {
                break;
            }
            // Built only when kept; a skeleton (past
            // `max_diagnostics`, the tree kept no texts) is counted.
            match d {
                Some(d) if self.collected.wants_warning(*start) => {
                    self.collected.push(d.as_ref().clone().into_owned());
                }
                _ => self.collected.count(*start),
            }
            self.next_warning = self.next_warning.saturating_add(1);
        }
    }

    fn stream_end(&mut self) -> Ev<'a> {
        self.done = true;
        self.see(usize::MAX);
        Ev::new(Node::StreamEnd, Span::empty(self.end))
    }

    fn fail(&mut self, e: Error) -> Error {
        self.done = true;
        e
    }

    /// Reads a node's leading children (as the stream did before the
    /// node's event); returns its first other child and which properties
    /// it has (so the properties are read only when present:
    /// `Node::anchor`/`Node::tag` scan these same children again).
    /// The node's tag, read only when a tag token was seen.
    fn tag_of(&mut self, props: &Prefix<'a>, n: TreeNode<'a>) -> Result<Tag<'a>, Error> {
        if props.tag {
            self.tags.node(n).map_err(|e| self.fail(e))
        } else {
            Ok(Tag::Absent)
        }
    }

    fn prefix(&mut self, n: TreeNode<'a>, document: bool) -> Result<Prefix<'a>, Error> {
        let mut out = Prefix::default();
        for c in n.children() {
            let k = c.kind();
            let leading = if document {
                is_document_prefix(k)
            } else {
                is_node_prefix(k)
            };
            if !leading {
                out.first = Some(c);
                return Ok(out);
            }
            self.see(c.preorder_index());
            match k {
                SyntaxKind::Error => return Err(self.fail(error_at(c))),
                SyntaxKind::YamlDirective => {
                    if let Some(v) = directive_version(&c.text()) {
                        self.yaml11_next = is_yaml11(v);
                    }
                }
                SyntaxKind::Anchor => out.anchor = true,
                SyntaxKind::Tag => out.tag = true,
                _ => {}
            }
        }
        Ok(out)
    }

    fn enter(&mut self, n: TreeNode<'a>, kind: SyntaxKind) -> Result<Option<Ev<'a>>, Error> {
        // Spans and properties are read only where an
        // event needs them; most elements are tokens that give none.
        let node = match kind {
            SyntaxKind::Error => return Err(self.fail(error_at(n))),
            SyntaxKind::YamlDirective
                if n.parent().is_some_and(|p| p.kind() == SyntaxKind::Stream) =>
            {
                if let Some(v) = directive_version(&n.text()) {
                    self.yaml11_next = is_yaml11(v);
                }
                return Ok(None);
            }
            SyntaxKind::Document => {
                // Only an explicit document adopted what preceded its
                // `---` (directives and the trivia, warnings, and errors
                // around them); an implicit one starts with its content.
                let explicit = n
                    .children()
                    .find(|c| !is_document_prefix(c.kind()))
                    .is_some_and(|c| c.kind() == SyntaxKind::DocumentStartMarker);
                if explicit {
                    self.prefix(n, true)?;
                }
                let at = doc_start_span(n);
                let yaml11 = std::mem::take(&mut self.yaml11_next);
                return Ok(Some(Ev::new(Node::DocStart { yaml11 }, at)));
            }
            SyntaxKind::Scalar => {
                let props = self.prefix(n, false)?;
                // A scalar's text token is its last child (tree docs).
                let last = n.last_child();
                let text = last.map_or_else(|| n.span(), TreeNode::span);
                let Some(value) = n.value() else {
                    debug_assert!(false, "scalar value unreadable");
                    let e = Error::internal("scalar value unreadable", Some(text));
                    return Err(self.fail(e));
                };
                let plain = last.is_some_and(|t| t.kind() == SyntaxKind::PlainScalar);
                let node = Node::Scalar {
                    value,
                    plain,
                    tag: self.tag_of(&props, n)?,
                };
                return Ok(Some(Ev {
                    node,
                    anchor: props.anchor_of(n),
                    span: text,
                    replayed: false,
                }));
            }
            SyntaxKind::BlockMapping
            | SyntaxKind::FlowMapping
            | SyntaxKind::FlowPair
            | SyntaxKind::BlockSequence
            | SyntaxKind::FlowSequence => {
                let props = self.prefix(n, false)?;
                let at = match props.first {
                    Some(m)
                        if matches!(
                            m.kind(),
                            SyntaxKind::FlowMappingOpen | SyntaxKind::FlowSequenceOpen
                        ) =>
                    {
                        m.span()
                    }
                    Some(m) => Span::empty(m.span().start),
                    None => Span::empty(n.span().end),
                };
                let tag = self.tag_of(&props, n)?;
                let node = if matches!(kind, SyntaxKind::BlockSequence | SyntaxKind::FlowSequence) {
                    Node::SeqStart { tag }
                } else {
                    Node::MapStart { tag }
                };
                return Ok(Some(Ev {
                    node,
                    anchor: props.anchor_of(n),
                    span: at,
                    replayed: false,
                }));
            }
            SyntaxKind::Alias => match n.alias_name() {
                Some(name) => Node::Alias { name },
                None => {
                    debug_assert!(false, "alias name unreadable");
                    let e = Error::internal("alias name unreadable", Some(n.span()));
                    return Err(self.fail(e));
                }
            },
            _ => return Ok(None),
        };
        Ok(Some(Ev::new(node, n.span())))
    }

    fn leave(&mut self, n: TreeNode<'a>, kind: SyntaxKind) -> Option<Ev<'a>> {
        let node = match kind {
            SyntaxKind::Stream => return Some(self.stream_end()),
            SyntaxKind::Document => Node::DocEnd,
            SyntaxKind::BlockMapping | SyntaxKind::FlowMapping | SyntaxKind::FlowPair => {
                Node::MapEnd
            }
            SyntaxKind::BlockSequence | SyntaxKind::FlowSequence => Node::SeqEnd,
            _ => return None,
        };
        // The last descendant, if it is this node's closing bracket.
        let close = self
            .last_entered
            .filter(|l| {
                matches!(
                    l.kind(),
                    SyntaxKind::FlowMappingClose
                        | SyntaxKind::FlowSequenceClose
                        | SyntaxKind::DocumentEndMarker
                )
            })
            .filter(|l| l.parent() == Some(n))
            .map(TreeNode::span);
        let at = close.unwrap_or_else(|| Span::empty(n.span().end));
        Some(Ev::new(node, at))
    }
}

/// What [`TreeSource::prefix`] read: a node's first child after its
/// leading children, and whether an anchor or a tag token is among them.
#[derive(Default)]
struct Prefix<'a> {
    first: Option<TreeNode<'a>>,
    anchor: bool,
    tag: bool,
}

impl<'a> Prefix<'a> {
    /// The node's anchor name, read only when an anchor token was seen
    /// among the children `Node::anchor` reads (the same ones).
    fn anchor_of(&self, n: TreeNode<'a>) -> Option<&'a str> {
        if self.anchor {
            n.anchor().map(|a| a.name)
        } else {
            None
        }
    }
}

/// A node of a tree to read again (the value of an alias key in
/// `Document`'s strict check, built only when an alias needs it).
#[derive(Clone, Copy)]
pub(crate) struct Replay<'a> {
    tree: &'a SyntaxTree,
    node: TreeNode<'a>,
}

impl<'a> Replay<'a> {
    /// The node of `tree` in `slot` ([`Replay::slot`]), while it lives.
    pub(crate) fn at(tree: &'a SyntaxTree, slot: u32) -> Option<Self> {
        tree.node_at_slot(slot).map(|node| Self { tree, node })
    }

    /// The tree the node belongs to.
    pub(crate) fn tree(self) -> &'a SyntaxTree {
        self.tree
    }

    /// The node's 4-byte name in its tree (what an anchor
    /// keeps to be read again, instead of this handle).
    pub(crate) fn slot(self) -> u32 {
        self.node.slot()
    }

    /// Where the node starts.
    pub(crate) fn start(self) -> usize {
        self.node.span().start
    }

    /// The node's events (its properties' warnings are not collected).
    pub(crate) fn source(self) -> TreeSource<'a> {
        TreeSource::over(self.tree, self.node)
    }
}

impl<'a> Source<'a> for TreeSource<'a> {
    fn replay_last(&self) -> Option<Replay<'a>> {
        self.last_entered.map(|node| Replay {
            tree: self.tree,
            node,
        })
    }

    fn next_event(&mut self) -> Result<Ev<'a>, Error> {
        if self.done {
            return Ok(self.stream_end());
        }
        loop {
            let Some(step) = self.walk.next() else {
                return Ok(self.stream_end());
            };
            let ev = match step {
                WalkEvent::Enter(n) => {
                    self.last_entered = Some(n);
                    self.see(n.preorder_index());
                    let kind = n.kind();
                    self.enter(n, kind)?
                }
                WalkEvent::Leave(n) => {
                    let kind = n.kind();
                    self.leave(n, kind)
                }
                // `WalkEvent` is `#[non_exhaustive]`.
                _ => None,
            };
            if let Some(ev) = ev {
                return Ok(ev);
            }
        }
    }

    fn take_warnings(&mut self) -> (Vec<Diagnostic>, crate::diag_cap::Dropped) {
        self.collected.take()
    }

    fn collect_warnings(&mut self, warnings: crate::diag_cap::Capped) {
        self.collected = warnings;
    }
}

/// One node of a document read as a document of its own: `DocStart` (with
/// the document's `%YAML` version), the node's events, `DocEnd`,
/// `StreamEnd` (a per-node read, `NodeRef::deserialize`).
///
/// Aliases inside the node that name anchors outside it are resolved
/// through [`Source::take_preload`]: the anchored nodes they reach (listed
/// by the caller, in document order, none inside another) are read here
/// and recorded by the expander before the node, so an alias means the
/// same node it means in the whole document. The cost is the node plus
/// what its aliases reach.
pub(crate) struct NodeSource<'a> {
    tree: &'a SyntaxTree,
    /// The document's tag table.
    doc_tags: &'a crate::document::tags::DocTags,
    /// How many own nodes of the document precede the node (the prefix
    /// budget), computed only when an expansion needs it.
    nodes_before: Box<dyn Fn() -> usize + 'a>,
    inner: TreeSource<'a>,
    /// The anchored nodes outside the node its aliases reach, or the
    /// error found while listing them (reported as the read's first error).
    anchors: Option<Result<Vec<TreeNode<'a>>, Error>>,
    yaml11: bool,
    span: Span,
    /// Where the node's document starts ([`Source::document_start`]).
    doc_start: usize,
    /// 0: `DocStart` next; 1: the node; 2: `DocEnd` given, `StreamEnd`
    /// from now on.
    stage: u8,
}

impl<'a> NodeSource<'a> {
    pub(crate) fn new(
        tree: &'a SyntaxTree,
        node: TreeNode<'a>,
        nodes_before: Box<dyn Fn() -> usize + 'a>,
        yaml11: bool,
        anchors: Result<Vec<TreeNode<'a>>, Error>,
        doc_tags: &'a crate::document::tags::DocTags,
    ) -> Self {
        Self {
            tree,
            doc_tags,
            nodes_before,
            inner: TreeSource::over(tree, node).with_doc_tags(doc_tags),
            anchors: Some(anchors),
            yaml11,
            span: node.span(),
            doc_start: node
                .ancestors()
                .find(|a| a.kind() == SyntaxKind::Document)
                .map_or(0, |d| doc_start_span(d).start),
            stage: 0,
        }
    }
}

impl<'a> Source<'a> for NodeSource<'a> {
    fn next_event(&mut self) -> Result<Ev<'a>, Error> {
        match self.stage {
            0 => {
                self.stage = 1;
                let doc = Node::DocStart {
                    yaml11: self.yaml11,
                };
                Ok(Ev::new(doc, Span::empty(self.span.start)))
            }
            1 => {
                let ev = self.inner.next_event()?;
                if ev.node == Node::StreamEnd {
                    self.stage = 2;
                    return Ok(Ev::new(Node::DocEnd, Span::empty(self.span.end)));
                }
                Ok(ev)
            }
            _ => Ok(Ev::new(
                Node::StreamEnd,
                Span::empty(self.tree.source().len()),
            )),
        }
    }

    fn take_warnings(&mut self) -> (Vec<Diagnostic>, crate::diag_cap::Dropped) {
        self.inner.take_warnings()
    }

    fn collect_warnings(&mut self, warnings: crate::diag_cap::Capped) {
        self.inner.collect_warnings(warnings);
    }

    fn take_preload(&mut self) -> Result<Vec<Ev<'a>>, Error> {
        let mut out = Vec::new();
        let anchors = self.anchors.take().unwrap_or_else(|| Ok(Vec::new()))?;
        for anchor in anchors {
            let mut src = TreeSource::over(self.tree, anchor).with_doc_tags(self.doc_tags);
            loop {
                let ev = src.next_event()?;
                if ev.node == Node::StreamEnd {
                    break;
                }
                out.push(ev);
            }
        }
        Ok(out)
    }

    fn nodes_before(&mut self) -> Option<usize> {
        Some((self.nodes_before)())
    }

    fn document_start(&mut self) -> Option<usize> {
        Some(self.doc_start)
    }
}

/// Whether a tree node is one [`TreeSource`] reads as a node event (what
/// the expander counts as an own node: scalars, collections, aliases).
pub(crate) fn is_node_event(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Scalar
            | SyntaxKind::BlockMapping
            | SyntaxKind::FlowMapping
            | SyntaxKind::FlowPair
            | SyntaxKind::BlockSequence
            | SyntaxKind::FlowSequence
            | SyntaxKind::Alias
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::construct::feed::Feed;
    use oneyaml_core::Settings;

    type Trace<'a> = (Vec<Ev<'a>>, Option<oneyaml_core::Code>, Vec<Diagnostic>);

    fn drain<'a>(mut s: impl Source<'a>) -> Trace<'a> {
        let mut out = Vec::new();
        loop {
            match s.next_event() {
                Ok(e) if e.node == Node::StreamEnd => {
                    out.push(e);
                    return (out, None, s.take_warnings().0);
                }
                Ok(e) => out.push(e),
                Err(e) => return (out, Some(e.code()), s.take_warnings().0),
            }
        }
    }

    /// The suite's inputs (two fixed levels, no recursion).
    fn suite() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-suite/data/yaml-test-suite");
        let mut out = Vec::new();
        let mut dirs: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            let mut cases = vec![d.clone()];
            if !d.join("in.yaml").is_file() {
                cases = std::fs::read_dir(&d)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .filter(|p| p.is_dir())
                    .collect();
            }
            for c in cases {
                if let Ok(bytes) = std::fs::read(c.join("in.yaml")) {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    out.push((c.display().to_string(), text));
                }
            }
        }
        out
    }

    /// Whether this crate is being tested inside the oneyaml workspace (its
    /// manifest two levels down from a `[workspace]` of `crates/*`), rather
    /// than from its unpacked package. Inside, workspace data must be there:
    /// a missing file fails the test instead of skipping it silently.
    fn in_workspace() -> bool {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        std::fs::read_to_string(root)
            .is_ok_and(|t| t.contains("[workspace]") && t.contains("\"crates/*\""))
    }

    fn assert_same(name: &str, src: &str, settings: &Settings) {
        let feed = Feed::new(oneyaml_syntax::events_with(src, settings), src.len());
        let tree = SyntaxTree::parse_with(src, settings);
        let a = drain(feed);
        let b = drain(TreeSource::new(&tree));
        assert_eq!(a, b, "{name}: {src:?}");
    }

    #[test]
    fn tree_replays_the_parser_events_on_the_suite() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-suite/data/yaml-test-suite");
        if !root.is_dir() {
            // The published package has no suite data (it lives in the
            // workspace); inside the workspace, it must be there.
            assert!(
                !in_workspace(),
                "yaml-test-suite data missing at {} inside the workspace",
                root.display()
            );
            eprintln!(
                "skipped: yaml-test-suite data not found at {}",
                root.display()
            );
            return;
        }
        let cases = suite();
        assert!(cases.len() > 300);
        let settings = Settings::new();
        for (name, src) in &cases {
            assert_same(name, src, &settings);
            // Every truncation, at character boundaries.
            for (cut, _) in src.char_indices() {
                assert_same(name, &src[..cut], &settings);
            }
        }
    }

    #[test]
    fn directives_warnings_and_documents() {
        let s = Settings::new();
        for src in [
            "%YAML 1.1\n--- a\n...\n%YAML 1.2\n--- b\n",
            "%YAML 1.1\n# c\n--- !!str &x a\n--- *x\n",
            "&a [1, {b: *a}]\n",
            "--- |2-\n  x\n...\n",
            "a: [b, c: d]\n",
            "%FOO bar\n--- x\n",
            "",
            "# only\n",
        ] {
            assert_same(src, src, &s);
        }
        assert_eq!(directive_version("%YAML 1.1"), Some("1.1"));
        assert_eq!(directive_version("%TAG ! x"), None);
    }

    #[test]
    fn properties_are_read_only_where_present() {
        // `prefix` gates `Node::anchor`/`Node::tag`; every place
        // a property can stand, next to comments, headers, and errors.
        let s = Settings::new();
        for src in [
            "&a !t x\n",
            "!t &a x\n",
            "- &a\n  # c\n  x\n- !!str\n  y\n",
            "a: &m !!map\n  b: 1\nc: *m\n",
            "--- &s !!seq\n- 1\n",
            "k: &b |2-\n   x\n",
            "k: !t >+ # c\n  x\n\n",
            "[&a a, !t b, &c !u {d: e}, ? &k k : &v v]\n",
            "{&a a: &b b, !t c: d}\n",
            "&a\n",
            "!t\n",
            "- &a !t @x\n",
            "%TAG !e! tag:e.com,2000:\n--- !e!x &a [!e!y z]\n",
            "a: &x 1\nb: \"q\"\nc: 'r'\nd: *x\n",
        ] {
            assert_same(src, src, &s);
        }
    }

    #[test]
    fn one_document_of_a_stream() {
        let tree = SyntaxTree::parse("a\n--- b\n--- [c]\n");
        let docs: Vec<_> = tree
            .root()
            .children()
            .filter(|n| n.kind() == SyntaxKind::Document)
            .collect();
        let (evs, err, _) = drain(TreeSource::over(&tree, docs[2]));
        assert!(err.is_none());
        assert!(matches!(evs[0].node, Node::DocStart { .. }));
        assert!(matches!(evs[1].node, Node::SeqStart { .. }));
        assert_eq!(evs.last().unwrap().node, Node::StreamEnd);
    }
}
