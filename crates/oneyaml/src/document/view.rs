//! Typed, read-only views of a [`Document`]'s nodes.
//!
//! Views borrow the document and are cheap to copy. They carry no state
//! of their own beyond a position in the tree, so adding mutation later
//! (`NodeMut`, `get_mut`, …) needs no change to them. Every walk here is
//! a loop; aliases are followed one step (an alias never names an
//! alias), merge keys through an explicit stack.

use std::borrow::Cow;

use oneyaml_core::{Diagnostic, Path, Span};
use oneyaml_syntax::tree::{Node as TreeNode, SyntaxKind};

use super::key::{self, AsKey, KeyArg};
use super::lookup::KeyKind;
use super::path_arg::{self, IntoPath};
use super::{Document, EditError, NodeId};
use crate::construct::scalar::{is_merge_key, resolve, scalar_value};
use crate::construct::tree::yaml11_of;
use crate::error::Error;
use crate::resolve::{Resolved, Resolver};
use crate::value::Value;

/// The node a reader sees at `n`'s place (restricted mode): where
/// the parser replaced broken content with an empty node right after the
/// error, the error region itself.
pub(super) fn presented(n: TreeNode<'_>) -> TreeNode<'_> {
    // Every error node carries an error diagnostic (tree docs), so a tree
    // without errors (O(1)) has none to present: skip the span and sibling
    // reads (they cost O(log R) each in an edited tree).
    if !n.tree().has_errors() {
        return n;
    }
    if n.kind() == SyntaxKind::Scalar && n.span().is_empty() {
        if let Some(e) = n.prev_sibling().filter(|p| p.kind() == SyntaxKind::Error) {
            return e;
        }
    }
    n
}

/// What a node is.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// A block or flow mapping (or a single-pair flow mapping).
    Mapping,
    /// A block or flow sequence.
    Sequence,
    /// A scalar.
    Scalar,
    /// An alias (`*name`); [`NodeRef::alias_target`] is its anchored node.
    Alias,
    /// An error region of a restricted document: read-only; its
    /// [`NodeRef::diagnostic`] says what went wrong.
    Error,
}

/// How a scalar was written.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScalarStyle {
    /// Unquoted.
    Plain,
    /// `'…'`.
    SingleQuoted,
    /// `"…"`.
    DoubleQuoted,
    /// `|` block scalar.
    Literal,
    /// `>` block scalar.
    Folded,
}

/// A node of a [`Document`]: a mapping, sequence, scalar, alias, or error
/// region, with its span, properties, and handle.
///
/// # Reading
///
/// Lookups return `Option`: a missing key is not an error. A node has typed
/// accessors ([`as_str`](NodeRef::as_str), [`as_bool`](NodeRef::as_bool),
/// [`as_i64`](NodeRef::as_i64), [`as_u64`](NodeRef::as_u64),
/// [`as_f64`](NodeRef::as_f64), [`is_null`](NodeRef::is_null)) that read it
/// exactly as [`Value`](crate::Value) and serde do, its source
/// [`text`](NodeRef::text) and [`span`](NodeRef::span), and any
/// `Deserialize` type through [`deserialize`](NodeRef::deserialize):
///
/// ```
/// use oneyaml::Document;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let src = "# service\nname: web   # public name\nports: [80, 443]\nlimits: {cpu: 0.5}\n";
/// let doc = Document::parse(src)?;
/// assert_eq!(doc.to_string(), src); // byte-identical
/// assert_eq!(doc.get("name").and_then(|n| n.as_str()).as_deref(), Some("web"));
/// assert!(doc.get("replicas").is_none());
///
/// let port = doc.get_path("ports[1]").ok_or("no ports[1]")?;
/// assert_eq!(port.as_u64(), Some(443));
/// assert_eq!(&src[port.span().start..port.span().end], "443");
/// assert_eq!(doc.get_path("limits.cpu").and_then(|n| n.as_f64()), Some(0.5));
/// let ports: Vec<u16> = doc.get("ports").ok_or("no ports")?.deserialize()?;
/// assert_eq!(ports, [80, 443]);
///
/// // The whole meaning, through the same resolver as serde.
/// assert_eq!(doc.to_value()?, oneyaml::from_str::<oneyaml::Value>(src)?);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct NodeRef<'d> {
    doc: &'d Document,
    node: TreeNode<'d>,
}

impl core::fmt::Debug for NodeRef<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = self.span();
        write!(f, "NodeRef({:?}@{}..{})", self.kind(), s.start, s.end)
    }
}

impl PartialEq for NodeRef<'_> {
    /// The same node of the same document.
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.doc, other.doc) && self.node == other.node
    }
}

impl Eq for NodeRef<'_> {}

impl<'d> NodeRef<'d> {
    pub(super) fn new(doc: &'d Document, node: TreeNode<'d>) -> Self {
        Self { doc, node }
    }

    /// The syntax node underneath (for the edit layer).
    pub(super) fn tree_node(self) -> TreeNode<'d> {
        self.node
    }

    /// The document the node belongs to.
    pub(super) fn document(self) -> &'d Document {
        self.doc
    }

    fn at(self, node: TreeNode<'d>) -> Self {
        Self::new(self.doc, node)
    }

    /// The document's resolver (its `%YAML` directive included).
    pub(super) fn resolver(self) -> Resolver {
        let yaml11 = self
            .node
            .ancestors()
            .find(|a| a.kind() == SyntaxKind::Document)
            .is_some_and(yaml11_of);
        Resolver::for_document(self.doc.settings(), yaml11)
    }

    /// Whether this node is a merge key (`<<`) of its document, with
    /// `merge_keys` on (for the edit layer: two merge keys in one mapping
    /// are a duplicate key).
    ///
    /// # Errors
    ///
    /// `Y9000` for a tag that does not resolve.
    pub(super) fn is_merge_key(self) -> Result<bool, Error> {
        let r = self.resolver();
        if !r.merge_keys() {
            return Ok(false);
        }
        self.as_scalar().map_or(Ok(false), |s| s.is_merge_key(&r))
    }

    /// Whether a merge key may read this node: a mapping
    /// whose tag, if any, resolves to the plain mapping, as the merge
    /// resolver requires (`Shape::Map` in the serde planner, a merge source
    /// in the builder; anything else is `Y1016`). The same
    /// `resolve_collection` both consumers call.
    ///
    /// # Errors
    ///
    /// `Y9000` for a tag that does not resolve.
    pub(super) fn is_merge_source(self) -> Result<bool, Error> {
        if !matches!(
            self.node.kind(),
            SyntaxKind::BlockMapping | SyntaxKind::FlowMapping | SyntaxKind::FlowPair
        ) {
            return Ok(false);
        }
        let tag = self.doc.shared.tags.try_tag(self.node)?;
        Ok(matches!(
            self.resolver()
                .resolve_collection(tag.input(), crate::resolve::CollectionKind::Mapping),
            Ok(crate::resolve::ResolvedCollection::Plain)
        ))
    }

    /// This node read as any `T: Deserialize` (per-node typed read):
    /// exactly what [`crate::from_str`] reads for the same node in the
    /// whole document. It goes through the same event source,
    /// expander, resolver, and deserializer as [`Document::deserialize`],
    /// so plain scalars keep their source text for string targets,
    /// and anchors, aliases, merge keys, `Spanned<T>` spans, and error
    /// codes and locations are the whole-document read's. An alias inside
    /// the node to an anchor outside it reads that anchor's node.
    ///
    /// Cost: O(the node + the anchored nodes its aliases reach),
    /// never O(document); when an expansion outgrows the budget of the
    /// node's own nodes, O(log n) more to count the own nodes before it
    /// (a position list built once per edit state of the document).
    ///
    /// Limits: `max_depth` and `max_typed_depth` count from the
    /// node down. The alias budget is the whole read's at the same point:
    /// own nodes are counted incrementally, so at an alias it is based on
    /// the document's own nodes before that alias (those before the node
    /// plus the node's own up to it); what the node's aliases reach counts
    /// as expansion. Expansions by aliases *before* the node are not
    /// charged to it, so a node read may accept a node whose whole-document
    /// read had already spent its budget earlier; it never refuses what the
    /// whole-document read accepts. The alias byte budget follows
    /// the same rule: it scales with the bytes of the document from its
    /// start up to the same point, and only what the node itself expands
    /// (and its own `%TAG` tag uses) is charged. An alias whose whole
    /// expansion is over a budget is refused before any of it is replayed,
    /// so a refused read costs about the node plus what its
    /// aliases reach, never the budget.
    ///
    /// The read needs only the node and what its aliases reach: in a
    /// restricted document, a clean node reads even when another
    /// part of the document has errors; an error region inside the node
    /// (or an alias it cannot resolve) is the read's error.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// #[derive(Debug, PartialEq, serde::Deserialize)]
    /// #[serde(rename_all = "camelCase")]
    /// struct Port {
    ///     name: String,
    ///     container_port: u16,
    /// }
    /// let doc = Document::parse("\
    /// defaults: &http {name: http, containerPort: 8080}
    /// ports:
    ///   - *http
    ///   - name: 9090
    ///     containerPort: 9090
    /// ")
    /// .unwrap();
    /// let ports = doc.get_path(&path!["ports"]).unwrap();
    /// let ports: Vec<Port> = ports.deserialize().unwrap();
    /// assert_eq!(ports[0], Port { name: "http".into(), container_port: 8080 });
    /// // a plain `9090` read into a `String` is its text.
    /// assert_eq!(ports[1].name, "9090");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`crate::from_str`] for the node: type mismatches with the path
    /// *below this node* and the span in the document's text, limit
    /// errors, and errors inside the node or the anchors it reaches.
    pub fn deserialize<T: serde::de::DeserializeOwned>(self) -> Result<T, Error> {
        let doc = self.doc;
        if let Some(e) = doc.shared.pre_error(doc.index) {
            return Err(e);
        }
        let tree = self.node.tree();
        let document = self
            .node
            .ancestors()
            .find(|a| a.kind() == SyntaxKind::Document);
        let yaml11 = document.is_some_and(yaml11_of);
        let node = self.node;
        // The own nodes before the node (the whole read's count at
        // it), from the document's cached position list: O(log n).
        let before = move || document.map_or(0, |d| doc.nodes_before(d, node));
        let source = || {
            crate::construct::tree::NodeSource::new(
                tree,
                node,
                Box::new(before),
                yaml11,
                self.outside_anchors(&before),
                &doc.shared.tags,
            )
        };
        let lines = Some(&doc.shared.lines);
        // Warnings are discarded: at most 16 built.
        crate::de::read_discarding::<_, T>(source, || doc.shared.text(), doc.settings(), lines)
    }

    /// The anchored nodes outside this node that its aliases reach,
    /// directly or through other such anchors: in document order, none
    /// inside another (recording an anchored node records the anchors in
    /// it).
    ///
    /// Linear in the node plus the content its aliases reach. Every
    /// element is visited at most once: a target already met inside a
    /// scanned anchor is not scanned again, and a scan stops at an anchor
    /// scanned before (which is then nested, not kept). A target is found
    /// in O(log k) and classified in O(1) (see `inside`).
    ///
    /// The reached content counts against the alias budget the whole
    /// read has at the node's last alias (own nodes before the node plus
    /// the node's own). Reading the node replays all of it at least once,
    /// so this never refuses what the read itself (or the whole-document
    /// read) accepts. Explicit stacks, no recursion.
    fn outside_anchors(self, before: &dyn Fn() -> usize) -> Result<Vec<TreeNode<'d>>, Error> {
        use std::collections::HashSet;

        use crate::construct::tree::is_node_event;

        let document = self
            .node
            .ancestors()
            .find(|a| a.kind() == SyntaxKind::Document);

        let root = self.node;
        let root_at = root.preorder_index();
        // An alias's target is the nearest anchor *before* the alias. For an
        // alias inside the node, whatever precedes it in preorder outside
        // the node's subtree precedes the node itself (a subtree is
        // contiguous in preorder), so its target is inside exactly when its
        // position is at least the node's. The same holds for aliases in
        // reached content, which precedes the node, except inside an
        // ancestor of the node, which only a recursive alias reaches (the
        // expander reports it).
        // The target through the document already known, O(log k)
        // (`alias_target` would find the document per alias, O(depth)).
        let tree = root.tree();
        let anchors = self.doc.anchors();
        let doc_at = document.map(TreeNode::preorder_index);
        let target_in = |n: TreeNode<'d>| -> Option<TreeNode<'d>> {
            let name = n.alias_name()?;
            let id = anchors.nearest_before(tree, name, doc_at?, n.preorder_index())?;
            tree.get(id).ok()
        };
        let limits = self.doc.settings().get_limits();
        let mut own = 0usize;
        let mut reached = 0usize;
        let mut prefix: Option<usize> = None;
        let flat = limits.get_alias_budget(0) == limits.get_alias_budget(usize::MAX);
        // Scan roots (anchors reached from outside), anchors met inside a
        // scan, and scan roots met inside a later scan.
        let mut roots: Vec<TreeNode<'d>> = Vec::new();
        let mut scanned: HashSet<usize> = HashSet::new();
        let mut met: HashSet<usize> = HashSet::new();
        let mut nested: HashSet<usize> = HashSet::new();
        let mut queue: Vec<(TreeNode<'d>, bool)> = vec![(root, true)];
        while let Some((sub, is_root)) = queue.pop() {
            let mut stack = vec![sub];
            while let Some(n) = stack.pop() {
                let at = n.preorder_index();
                if n != sub && scanned.contains(&at) {
                    nested.insert(at);
                    continue;
                }
                if is_node_event(n.kind()) {
                    if is_root {
                        own = own.saturating_add(1);
                    } else {
                        reached = reached.saturating_add(1);
                        let over =
                            |own: usize| limits.get_alias_budget(own).filter(|&b| reached > b);
                        if over(own).is_some() {
                            // As the expander: with the own nodes before
                            // the node, asked once (unless the budget does
                            // not depend on the count).
                            let upto = if flat {
                                own
                            } else {
                                own.saturating_add(*prefix.get_or_insert_with(before))
                            };
                            if let Some(budget) = over(upto) {
                                return Err(crate::construct::expand::alias_budget_exceeded(
                                    budget,
                                    upto,
                                    Some(sub.span()),
                                ));
                            }
                        }
                        if n.anchor().is_some() {
                            met.insert(at);
                        }
                    }
                }
                if n.kind() == SyntaxKind::Alias {
                    // An undefined alias stays for the expander to report.
                    if let Some(t) = target_in(n) {
                        let t_at = t.preorder_index();
                        let known = scanned.contains(&t_at) || met.contains(&t_at);
                        if !known && t_at < root_at {
                            scanned.insert(t_at);
                            roots.push(t);
                            queue.push((t, false));
                        }
                    }
                }
                let mut child = n.first_child();
                while let Some(c) = child {
                    stack.push(c);
                    child = c.next_sibling();
                }
            }
        }
        roots.retain(|r| !nested.contains(&r.preorder_index()));
        roots.sort_by_key(|r| r.preorder_index());
        Ok(roots)
    }

    /// A stable handle to this node.
    #[must_use]
    pub fn id(self) -> NodeId {
        NodeId(self.node.id())
    }

    /// What the node is.
    #[must_use]
    pub fn kind(self) -> NodeKind {
        match self.node.kind() {
            SyntaxKind::BlockMapping | SyntaxKind::FlowMapping | SyntaxKind::FlowPair => {
                NodeKind::Mapping
            }
            SyntaxKind::BlockSequence | SyntaxKind::FlowSequence => NodeKind::Sequence,
            SyntaxKind::Scalar => NodeKind::Scalar,
            SyntaxKind::Alias => NodeKind::Alias,
            _ => NodeKind::Error,
        }
    }

    /// The source span (UTF-8 bytes), properties included.
    #[must_use]
    pub fn span(self) -> Span {
        self.node.span()
    }

    /// The source text of the node, exactly as written. Borrowed from the
    /// document's text; `Cow` because the syntax tree keeps no copies of
    /// text split by edits.
    #[must_use]
    pub fn text(self) -> Cow<'d, str> {
        self.node.text()
    }

    /// The anchor name (`&name`, without `&`).
    ///
    /// Properties and aliases, as written:
    ///
    /// ```
    /// use oneyaml::{Document, ScalarStyle, Value, path};
    ///
    /// let doc = Document::parse("base: &b !!str 80\nports: [*b, '443']\n").unwrap();
    /// let base = doc.get("base").unwrap();
    /// assert_eq!(base.anchor(), Some("b"));
    /// assert_eq!(base.tag().unwrap().unwrap(), "tag:yaml.org,2002:str");
    /// let ports = doc.get("ports").unwrap();
    /// let first = ports.get_index(0).unwrap();
    /// assert_eq!(first.alias_name(), Some("b"));
    /// assert_eq!(first.alias_target().unwrap().id(), base.id());
    /// assert_eq!(first.resolved().text(), "&b !!str 80");
    /// let second = ports.get_index(1).unwrap().as_scalar().unwrap();
    /// assert_eq!(second.style(), ScalarStyle::SingleQuoted);
    /// assert_eq!(second.resolved().unwrap(), Value::from("443"));
    /// ```
    #[must_use]
    pub fn anchor(self) -> Option<&'d str> {
        self.node.anchor().map(|a| a.name)
    }

    /// The tag as resolved from its `%TAG` handle (e.g.
    /// `tag:yaml.org,2002:str`), `!` for the non-specific tag; `None` when
    /// the node has no tag.
    ///
    /// O(1): each distinct tag of the document is resolved once and shared
    /// (a [`Tag`](crate::Tag) holds its `%TAG` prefix shared with
    /// the document's other tags, so reading it never copies the prefix).
    ///
    /// # Errors
    ///
    /// `Y9000` (internal error) should a tag property not resolve, which
    /// the parser rules out (never read as untagged instead).
    pub fn tag(self) -> Result<Option<crate::Tag>, Error> {
        self.doc.shared.tags.value_tag(self.node)
    }

    /// An alias's anchor name (without `*`).
    #[must_use]
    pub fn alias_name(self) -> Option<&'d str> {
        self.node.alias_name()
    }

    /// The node an alias refers to: the closest preceding node of the same
    /// document with that anchor (§3.2.2.2). `None` for other nodes and
    /// undefined aliases.
    #[must_use]
    pub fn alias_target(self) -> Option<NodeRef<'d>> {
        let name = self.node.alias_name()?;
        let doc = self
            .node
            .ancestors()
            .find(|a| a.kind() == SyntaxKind::Document)?;
        let tree = self.node.tree();
        // The anchors of this name, in source order (indexed once,
        // then maintained by edits): the last one of this document before
        // the alias, in O(log k).
        let id = self.doc.anchors().nearest_before(
            tree,
            name,
            doc.preorder_index(),
            self.node.preorder_index(),
        )?;
        tree.get(id).ok().map(|n| self.at(n))
    }

    /// This node, or an alias's target (one step: an alias never names an
    /// alias).
    #[must_use]
    pub fn resolved(self) -> NodeRef<'d> {
        self.alias_target().unwrap_or(self)
    }

    /// The diagnostic of an error region (`Cow`: see [`NodeRef::text`]).
    ///
    /// Also for an error region past `Limits::max_diagnostics`:
    /// the same code and spans, with the code's summary for a message (the
    /// tree keeps no texts past the limit). O(log n).
    #[must_use]
    pub fn diagnostic(self) -> Option<Cow<'d, Diagnostic>> {
        let d = self.node.diagnostic()?;
        if !oneyaml_syntax::tree::SyntaxTree::is_skeleton(&d) {
            return Some(d);
        }
        let limit = self.doc.settings().get_limits().get_max_diagnostics();
        Some(Cow::Owned(crate::diag_cap::filled(d.into_owned(), limit)))
    }

    /// The mapping view (aliases followed).
    #[must_use]
    pub fn as_mapping(self) -> Option<MappingRef<'d>> {
        let n = self.resolved();
        (n.kind() == NodeKind::Mapping).then_some(MappingRef(n))
    }

    /// The sequence view (aliases followed).
    #[must_use]
    pub fn as_sequence(self) -> Option<SequenceRef<'d>> {
        let n = self.resolved();
        (n.kind() == NodeKind::Sequence).then_some(SequenceRef(n))
    }

    /// The scalar view (aliases followed).
    #[must_use]
    pub fn as_scalar(self) -> Option<ScalarRef<'d>> {
        let n = self.resolved();
        (n.kind() == NodeKind::Scalar).then_some(ScalarRef(n))
    }

    /// The scalar's meaning (aliases followed), through the document's
    /// resolver and settings: what [`ScalarRef::resolved`] gives. `None`
    /// for a collection, an error region, an undefined alias, a scalar
    /// that does not resolve (`!!int x`), or a recovered scalar holding an
    /// error (`&a\n  &b x`, two anchors, reads as nothing,
    /// as `deserialize` refuses it).
    fn scalar_meaning(self) -> Option<Value> {
        self.typed_scalar()?.resolved().ok()
    }

    /// The scalar view for a typed read: `None` where the serde read of
    /// the node fails on an error inside it, found among the
    /// scalar's few children (its properties and text).
    fn typed_scalar(self) -> Option<ScalarRef<'d>> {
        let s = self.as_scalar()?;
        let holds_error = s.0.node.children().any(|c| c.kind() == SyntaxKind::Error);
        (!holds_error).then_some(s)
    }

    /// The string a scalar means (aliases followed), as
    /// [`Value::as_str`] reads it from this node's [`Value`]: `name:
    /// web` and `name: 'web'` give `web`; `port: 80` (an integer), `on: ~`
    /// (null), a collection, and a tagged scalar (`!secret x`, a
    /// [`Value::Tagged`]) give `None`.
    ///
    /// A `Cow` of the scalar's content ([`ScalarRef::value`]): borrowed
    /// from the document's text whenever the content is a slice of it
    /// (`web` above, or a quoted scalar with no escapes or line breaks);
    /// owned only when it had to be decoded (escapes, line folding, block
    /// scalar indentation). Nothing is allocated for a borrowed string.
    ///
    /// ```
    /// use std::borrow::Cow;
    /// use oneyaml::Document;
    ///
    /// let doc = Document::parse("name: web\nport: 8080\nmotd: \"hi\\tthere\"\n").unwrap();
    /// assert!(matches!(doc.get("name").unwrap().as_str(), Some(Cow::Borrowed("web"))));
    /// assert_eq!(doc.get("motd").unwrap().as_str().as_deref(), Some("hi\tthere"));
    /// assert_eq!(doc.get("port").unwrap().as_str(), None); // an integer
    /// ```
    #[must_use]
    pub fn as_str(self) -> Option<Cow<'d, str>> {
        self.typed_scalar()?.as_str()
    }

    /// The boolean a scalar means (aliases followed), as [`Value::as_bool`]
    /// reads it: `true`/`false` under the Core schema; `yes`/`on`
    /// only with `yaml11_booleans` (a string otherwise, so `None`).
    ///
    /// ```
    /// use oneyaml::{Compat, Document};
    ///
    /// let doc = Document::parse("debug: true\nlegacy: yes\n").unwrap();
    /// assert_eq!(doc.get("debug").unwrap().as_bool(), Some(true));
    /// assert_eq!(doc.get("legacy").unwrap().as_bool(), None); // YAML 1.2: a string
    /// let old = Document::parse_with("legacy: yes\n", &Compat::yaml_1_1()).unwrap();
    /// assert_eq!(old.get("legacy").unwrap().as_bool(), Some(true));
    /// ```
    #[must_use]
    pub fn as_bool(self) -> Option<bool> {
        self.scalar_meaning()?.as_bool()
    }

    /// The integer a scalar means, if it fits `i64` (aliases followed), as
    /// [`Value::as_i64`] reads it: `0x10` is 16; `1.0` and `'1'`
    /// give `None` (as does `-0x10`: the Core schema's hexadecimal has no
    /// sign, so it is a string).
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let doc = Document::parse("offset: -16\nmask: 0x10\nratio: 1.0\n").unwrap();
    /// assert_eq!(doc.get("offset").unwrap().as_i64(), Some(-16));
    /// assert_eq!(doc.get("mask").unwrap().as_i64(), Some(16));
    /// assert_eq!(doc.get("ratio").unwrap().as_i64(), None);
    /// ```
    #[must_use]
    pub fn as_i64(self) -> Option<i64> {
        self.scalar_meaning()?.as_i64()
    }

    /// The integer a scalar means, if it fits `u64` (aliases followed), as
    /// [`Value::as_u64`] reads it.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let doc = Document::parse("base: &p 8080\nports: [*p, -1]\n").unwrap();
    /// let ports = doc.get("ports").unwrap();
    /// assert_eq!(ports.get_index(0).unwrap().as_u64(), Some(8080)); // through the alias
    /// assert_eq!(ports.get_index(1).unwrap().as_u64(), None);
    /// ```
    #[must_use]
    pub fn as_u64(self) -> Option<u64> {
        self.scalar_meaning()?.as_u64()
    }

    /// The number a scalar means as `f64` (aliases followed), as
    /// [`Value::as_f64`] reads it: floats, `.inf`/`.nan`, and
    /// integers (see [`Number::as_f64`](crate::Number::as_f64)).
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let doc = Document::parse("ratio: 0.25\nlimit: .inf\nname: x\n").unwrap();
    /// assert_eq!(doc.get("ratio").unwrap().as_f64(), Some(0.25));
    /// assert_eq!(doc.get("limit").unwrap().as_f64(), Some(f64::INFINITY));
    /// assert_eq!(doc.get("name").unwrap().as_f64(), None);
    /// ```
    #[must_use]
    pub fn as_f64(self) -> Option<f64> {
        self.scalar_meaning()?.as_f64()
    }

    /// Whether a scalar means null (aliases followed), as
    /// [`Value::is_null`] says: `~`, `null`, and an empty value;
    /// `'null'` (quoted) is a string.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let doc = Document::parse("a: ~\nb:\nc: 'null'\n").unwrap();
    /// assert!(doc.get("a").unwrap().is_null());
    /// assert!(doc.get("b").unwrap().is_null());
    /// assert!(!doc.get("c").unwrap().is_null());
    /// ```
    #[must_use]
    pub fn is_null(self) -> bool {
        self.scalar_meaning().is_some_and(|v| v.is_null())
    }

    /// For a mapping entry's key, its value.
    pub(super) fn entry_value(self) -> Option<NodeRef<'d>> {
        value_after(self)
    }

    /// For a mapping entry's value, its key.
    pub(super) fn entry_key(self) -> Option<NodeRef<'d>> {
        let mut at = self.node.prev_sibling();
        while let Some(x) = at {
            if x.kind().is_yaml_node() {
                return Some(NodeRef::new(self.doc, presented(x)));
            }
            at = x.prev_sibling();
        }
        None
    }

    /// A mapping's entry for `key` (see [`MappingRef::get`]).
    #[must_use]
    pub fn get<K: AsKey>(self, key: K) -> Option<NodeRef<'d>> {
        self.as_mapping()?.get(key)
    }

    /// A sequence's item `index`.
    #[must_use]
    pub fn get_index(self, index: usize) -> Option<NodeRef<'d>> {
        self.as_sequence()?.get(index)
    }

    /// The node at `path` below this one; `None` when there is none
    /// (a read-only lookup, as `get`). Navigation follows aliases
    /// and merge keys as [`NodeRef::get`] does. To learn *why* a path
    /// names nothing, [`Document::get_path_mut`] reports it.
    ///
    /// ```
    /// use oneyaml::{Document, path};
    ///
    /// let doc = Document::parse("a:\n  b: [x, y]\n").unwrap();
    /// let a = doc.get("a").unwrap();
    /// assert_eq!(a.get_path(&path!["b", 1]).unwrap().text(), "y");
    /// assert!(a.get_path(&path!["b", 2]).is_none());
    /// ```
    #[must_use]
    pub fn get_path<P: IntoPath>(self, path: P) -> Option<NodeRef<'d>> {
        let path = path_arg::read(&path)?;
        self.walk(&path).ok()
    }

    /// [`NodeRef::get_path`] with the reason when the walk stops: a typed
    /// [`EditError`] naming the path where it stopped (for the edit API).
    pub(super) fn walk(self, path: &Path) -> Result<NodeRef<'d>, EditError> {
        let mut at = Path::new();
        let mut node = self;
        // A loop over the segments.
        for segment in path.segments() {
            if node.resolved().kind() == NodeKind::Error {
                return Err(EditError::ErrorNode { at });
            }
            node = match segment {
                oneyaml_core::PathSegment::Key(k) => {
                    let Some(m) = node.as_mapping() else {
                        return Err(EditError::NotAMapping { at });
                    };
                    at.push_key(k.clone());
                    m.segment(k, false)
                        .map_err(|e| e.located(&at))?
                        .ok_or_else(|| EditError::NotFound { at: at.clone() })?
                }
                oneyaml_core::PathSegment::Index(i) => {
                    let Some(s) = node.as_sequence() else {
                        return Err(EditError::NotASequence { at });
                    };
                    let Some(item) = s.get(*i) else {
                        let len = s.len();
                        return Err(EditError::IndexOutOfBounds { at, index: *i, len });
                    };
                    at.push_index(*i);
                    item
                }
                _ => return Err(EditError::NotFound { at }),
            };
        }
        Ok(node)
    }

    /// The YAML-node children (keys and values, or items), in order,
    /// error regions standing in for the nodes they broke.
    fn items(self) -> impl Iterator<Item = NodeRef<'d>> + 'd {
        let doc = self.doc;
        self.node
            .children()
            .filter(|c| c.kind().is_yaml_node())
            .map(move |c| NodeRef::new(doc, presented(c)))
    }
}

/// A mapping node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappingRef<'d>(NodeRef<'d>);

impl<'d> MappingRef<'d> {
    /// The mapping as a node.
    #[must_use]
    pub fn node(self) -> NodeRef<'d> {
        self.0
    }

    /// The entries as written, in order: duplicates and merge keys (`<<`)
    /// included.
    pub fn iter(self) -> impl Iterator<Item = (NodeRef<'d>, NodeRef<'d>)> + 'd {
        let mut items = self.0.items();
        core::iter::from_fn(move || Some((items.next()?, items.next()?)))
    }

    /// Number of entries as written. O(1) for a wide mapping (its lookup
    /// index), else O(entries).
    #[must_use]
    pub fn len(self) -> usize {
        let indexed = self
            .0
            .doc
            .with_lookup(self.0.node, false, |ix| Some(ix.len()));
        indexed.unwrap_or_else(|| self.0.items().count() / 2)
    }

    /// `true` without entries.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0.items().next().is_none()
    }

    /// Entry `i` (as [`MappingRef::iter`] gives it). O(log width) for a
    /// wide mapping, else O(i).
    pub(super) fn entry(self, i: usize) -> Option<(NodeRef<'d>, NodeRef<'d>)> {
        let (doc, tree) = (self.0.doc, self.0.node.tree());
        let indexed = doc.with_lookup(self.0.node, false, |ix| {
            if i >= ix.len() {
                return Some(None);
            }
            let k = i.checked_mul(2)?;
            let key = ix.item(k)?.node(tree)?;
            let value = ix.item(k.checked_add(1)?)?.node(tree)?;
            Some(Some((
                NodeRef::new(doc, presented(key)),
                NodeRef::new(doc, presented(value)),
            )))
        });
        indexed.unwrap_or_else(|| self.iter().nth(i))
    }

    /// The last of this mapping's own keys (merge keys aside) that `q`
    /// matches (the last one's value counts). Through the lookup
    /// index of a wide mapping (O(1) expected, plus its alias
    /// keys, which are read live), else (or `linear`) O(entries). The two
    /// make the same comparisons: the index only picks the candidates.
    fn own_key(self, r: &Resolver, q: KeyQuery<'_>, linear: bool) -> Option<NodeRef<'d>> {
        let (doc, tree) = (self.0.doc, self.0.node.tree());
        let indexed = (!linear)
            .then(|| {
                doc.with_lookup(self.0.node, false, |ix| {
                    let digest = match q {
                        KeyQuery::Str(k) => ix.digest(&Value::String(k.to_owned())),
                        KeyQuery::Value(v) => ix.digest(v),
                    };
                    // The bucket is in document order, so
                    // the governing key is the last that matches: O(log k)
                    // for k repeats (under `LastWins`); earlier members are
                    // read only after a digest collision.
                    let mut best: Option<(usize, TreeNode<'d>)> = None;
                    for id in ix.bucket_rev(digest) {
                        let n = id.node(tree)?;
                        if q.matches(NodeRef::new(doc, n), r) {
                            best = Some((n.preorder_index(), n));
                            break;
                        }
                    }
                    // Alias keys are read live (their meaning follows their
                    // anchored node), the last match first.
                    for id in ix.aliases_rev() {
                        let n = id.node(tree)?;
                        if q.matches(NodeRef::new(doc, n), r) {
                            let at = n.preorder_index();
                            if best.is_none_or(|(b, _)| at > b) {
                                best = Some((at, n));
                            }
                            break;
                        }
                    }
                    Some(best.map(|(_, n)| NodeRef::new(doc, n)))
                })
            })
            .flatten();
        indexed.unwrap_or_else(|| {
            let mut found = None;
            for (k, _) in self.iter() {
                if q.matches(k, r) {
                    found = Some(k);
                }
            }
            found
        })
    }

    /// The last merge key (`<<` with `merge_keys` on; the last one
    /// replaces earlier ones, as in keys.rs). As [`MappingRef::own_key`].
    fn last_merge(self, r: &Resolver, linear: bool) -> Option<NodeRef<'d>> {
        if !r.merge_keys() {
            return None;
        }
        let (doc, tree) = (self.0.doc, self.0.node.tree());
        let is_merge =
            |k: NodeRef<'_>| k.as_scalar().is_some_and(|s| lookup_hit(s.is_merge_key(r)));
        let indexed = (!linear)
            .then(|| {
                doc.with_lookup(self.0.node, false, |ix| {
                    // The last of each list (in document
                    // order) that is a merge key: O(log k) for k merge keys.
                    let mut best: Option<(usize, TreeNode<'d>)> = None;
                    for list in ix.merge_lists() {
                        for id in ix.rev(list) {
                            let n = id.node(tree)?;
                            if is_merge(NodeRef::new(doc, n)) {
                                let at = n.preorder_index();
                                if best.is_none_or(|(b, _)| at > b) {
                                    best = Some((at, n));
                                }
                                break;
                            }
                        }
                    }
                    Some(best.map(|(_, n)| NodeRef::new(doc, n)))
                })
            })
            .flatten();
        indexed.unwrap_or_else(|| {
            let mut found = None;
            for (k, _) in self.iter() {
                if is_merge(k) {
                    found = Some(k);
                }
            }
            found
        })
    }

    /// Whether one of the keys (merge keys aside) is a scalar whose
    /// resolved value is `key` (`~`, `null` and an empty key are
    /// one key). O(1) expected for a wide mapping, else
    /// O(entries).
    pub(super) fn has_key_value(self, key: &Value) -> bool {
        let r = self.0.resolver();
        self.own_key(&r, KeyQuery::Value(key), false).is_some()
    }

    /// Whether one of the keys is a merge key (`<<`, see
    /// [`NodeRef::is_merge_key`]). O(alias keys) for a wide mapping
    /// (with the lookup index), else O(entries).
    ///
    /// # Errors
    ///
    /// `Y9000` for a key tag that does not resolve.
    pub(super) fn has_merge_key(self) -> Result<bool, Error> {
        self.has_merge_key_in(false)
    }

    fn has_merge_key_in(self, linear: bool) -> Result<bool, Error> {
        let (doc, tree) = (self.0.doc, self.0.node.tree());
        let indexed = (!linear)
            .then(|| {
                doc.with_lookup(self.0.node, false, |ix| {
                    if ix.has_merges() {
                        return Some(Ok(true));
                    }
                    for id in ix.aliases_rev() {
                        match NodeRef::new(doc, id.node(tree)?).is_merge_key() {
                            Ok(false) => {}
                            found => return Some(found),
                        }
                    }
                    Some(Ok(false))
                })
            })
            .flatten();
        if let Some(found) = indexed {
            return found;
        }
        for (k, _) in self.iter() {
            if k.is_merge_key()? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The value of this mapping's **own** entry for `key` — the last one
    /// when repeated, the one whose value counts — ignoring keys
    /// merged in by `<<`. O(1) expected for a wide mapping,
    /// else O(entries).
    pub(super) fn own_value(self, key: &str) -> Option<NodeRef<'d>> {
        self.own_value_of(KeyQuery::Str(key))
    }

    /// [`MappingRef::own_value`] for any key.
    pub(super) fn own_value_of(self, q: KeyQuery<'_>) -> Option<NodeRef<'d>> {
        let r = self.0.resolver();
        value_after(self.own_key(&r, q, false)?)
    }

    /// The value for `key` (any [`AsKey`](crate::AsKey): `&str`, `String`,
    /// an integer, `bool`, a [`Value`]), with the meaning [`Value`] gives
    /// it: a string key matches when it resolves to that string (so
    /// `get("1")` does not match the integer key `1`, `get(1)` does), any
    /// other key when it resolves to an equal value; a repeated key's last
    /// value wins;
    /// with `merge_keys` on, keys merged in by `<<` are found too, own keys
    /// first. `None` when absent.
    ///
    /// Cost: O(1) expected per mapping searched when it is wide (its
    /// lookup index; plus its alias keys, read live), else
    /// O(its entries).
    #[must_use]
    pub fn get<K: AsKey>(self, key: K) -> Option<NodeRef<'d>> {
        let k = key::arg(&key);
        self.get_in(KeyQuery::of(&k), false)
    }

    /// Calls `f` with each of this mapping's own keys that a pointer token can
    /// name: a scalar key, whatever its style or tag and whether or not it
    /// resolves, with its text, or an alias key (`None`). Merge keys, collection
    /// keys and keys without a value are left out. One pass, in document order.
    pub(super) fn for_each_text_key(self, mut f: impl FnMut(NodeRef<'d>, Option<Cow<'d, str>>)) {
        let r = self.0.resolver();
        for (key, _) in self.iter() {
            if key.kind() == NodeKind::Alias {
                f(key, None);
                continue;
            }
            let Some(s) = key.as_scalar() else { continue };
            if r.merge_keys() && lookup_hit(s.is_merge_key(&r)) {
                continue;
            }
            f(key, Some(s.value()));
        }
    }

    /// Whether `key`, one of this mapping's own keys, is the one that counts
    /// for what it means: no other own key means the same, so a later
    /// duplicate (under `DuplicateKeys::LastWins`) does not override it.
    pub(super) fn governs(self, key: NodeRef<'d>) -> bool {
        let r = self.0.resolver();
        let Some(s) = key.as_scalar() else {
            return true;
        };
        let Ok(meaning) = s.resolved_with(&r) else {
            return true;
        };
        let query = match &meaning {
            Value::String(text) => KeyQuery::Str(text),
            other => KeyQuery::Value(other),
        };
        self.own_key(&r, query, false)
            .is_none_or(|k| k.node.id() == key.node.id())
    }

    /// The mappings this one merges in with `<<` (with `merge_keys` on), in
    /// the order a lookup searches them: earlier sources first.
    pub(super) fn merge_sources(self) -> Vec<MappingRef<'d>> {
        let r = self.0.resolver();
        let Some(v) = self.last_merge(&r, false).and_then(value_after) else {
            return Vec::new();
        };
        match v.as_sequence() {
            Some(seq) => seq.iter().filter_map(NodeRef::as_mapping).collect(),
            None => v.as_mapping().into_iter().collect(),
        }
    }

    /// The value a path segment names: for `key` as [`MappingRef::get`]
    /// (`own`: only this mapping's own entries, as the last segment of a
    /// set does), except that the empty segment goes by the key's text.
    ///
    /// The empty segment names the entry whose key has no content: a key
    /// written as nothing (`: v`, `[: v]`) is a scalar of empty text, though
    /// it reads as null, and so is a quoted empty key (`'': v`), whatever
    /// properties either has (`&a : v`, `!!null : v`). When both kinds are
    /// present the segment names two entries and the lookup is refused.
    pub(super) fn segment(self, key: &str, own: bool) -> Result<Option<NodeRef<'d>>, EditError> {
        if !key.is_empty() {
            return Ok(if own {
                self.own_value(key)
            } else {
                self.get_in(KeyQuery::Str(key), false)
            });
        }
        let (mut written, mut quoted) = (None, None);
        for (k, v) in self.iter() {
            let Some(s) = k.as_scalar().filter(|s| s.value().is_empty()) else {
                continue;
            };
            // (The last of a repeated key counts.)
            match s.style() {
                ScalarStyle::Plain => written = Some(v),
                _ => quoted = Some(v),
            }
        }
        match (written, quoted) {
            (Some(_), Some(_)) => Err(EditError::Unsupported {
                at: Path::new(),
                reason: "the empty key names two entries, one written as nothing and one quoted",
            }),
            (Some(v), None) | (None, Some(v)) => Ok(Some(v)),
            (None, None) if own => Ok(None),
            (None, None) => Ok(self.get_in(KeyQuery::Str(""), false)),
        }
    }

    fn get_in(self, key: KeyQuery<'_>, linear: bool) -> Option<NodeRef<'d>> {
        let resolver = self.0.resolver();
        // Mappings still to search, next first. A mapping's merge
        // closure is searched depth first in precedence order (own keys,
        // then earlier sources, keys.rs), so the first visit of a mapping
        // is its highest-precedence one and its whole closure is searched
        // before anything of lower precedence: skipping later visits is
        // exact, and the search is O(merge closure) however much a merge
        // DAG shares its sources. Also ends a merge cycle.
        let mut stack = vec![self];
        let mut seen = std::collections::HashSet::new();
        while let Some(m) = stack.pop() {
            if !seen.insert(m.0.node.id()) {
                continue;
            }
            let found = m.own_key(&resolver, key, linear).and_then(value_after);
            if found.is_some() {
                return found;
            }
            // Earlier sources take precedence: push them last.
            if let Some(v) = m.last_merge(&resolver, linear).and_then(value_after) {
                match v.as_sequence() {
                    Some(seq) => {
                        let items: Vec<_> = seq.iter().filter_map(NodeRef::as_mapping).collect();
                        stack.extend(items.into_iter().rev());
                    }
                    None => stack.extend(v.as_mapping()),
                }
            }
        }
        None
    }
}

/// What a key lookup asks for: a string key, or a key equal by
/// meaning to a value.
#[derive(Clone, Copy)]
pub(super) enum KeyQuery<'a> {
    Str(&'a str),
    Value(&'a Value),
}

impl<'a> KeyQuery<'a> {
    /// The query for key `k` of the public API.
    pub(super) fn of(k: &'a KeyArg<'_>) -> Self {
        match k {
            KeyArg::Str(s) => KeyQuery::Str(s),
            KeyArg::Value(v) => KeyQuery::Value(v.as_ref()),
        }
    }
}

impl KeyQuery<'_> {
    /// Whether own key `k` (merge keys aside) is the one asked for: the
    /// comparisons the linear scans have always made.
    fn matches(self, k: NodeRef<'_>, r: &Resolver) -> bool {
        k.as_scalar().is_some_and(|s| {
            !(r.merge_keys() && lookup_hit(s.is_merge_key(r)))
                && match self {
                    KeyQuery::Str(key) => s.is_string(r, key),
                    KeyQuery::Value(Value::String(key)) => s.is_string(r, key),
                    KeyQuery::Value(v) => s.resolved_with(r).ok().as_ref() == Some(v),
                }
        })
    }
}

/// A lookup's reading of a key check (`get`, `own_key`, the lookup
/// index): a key whose tag does not resolve (`Y9000`) matches no
/// lookup and is no merge key, as a key whose resolution fails (`!!int x`)
/// matches none. It is never read as untagged: the document's meaning
/// check reads the same tag through the construction layer
/// (`DocTags::try_tag`), so such a document already reports the `Y9000`
/// (`Document::parse` fails; `diagnostics()` lists it), and the `Result`
/// readers (`NodeRef::tag`, `ScalarRef::resolved`, the edits) return it.
fn lookup_hit(r: Result<bool, Error>) -> bool {
    r.unwrap_or(false)
}

/// The value of the entry whose key is `k`: the next YAML node (what
/// [`MappingRef::iter`] pairs with it), as presented. O(local).
fn value_after(k: NodeRef<'_>) -> Option<NodeRef<'_>> {
    let mut at = k.node.next_sibling();
    while let Some(x) = at {
        if x.kind().is_yaml_node() {
            return Some(NodeRef::new(k.doc, presented(x)));
        }
        at = x.next_sibling();
    }
    None
}

/// What mapping key `k` is to the lookup index (`lookup.rs`):
/// an alias (read live), a merge key, a scalar with its resolved meaning,
/// or a key no lookup matches.
pub(super) fn key_kind(k: NodeRef<'_>, r: &Resolver) -> KeyKind {
    if k.node.kind() == SyntaxKind::Alias {
        return KeyKind::Alias;
    }
    let Some(s) = k.as_scalar() else {
        return KeyKind::Never;
    };
    match s.is_merge_key(r) {
        Ok(true) if r.merge_keys() => return KeyKind::Merge,
        Ok(_) => {}
        // See `lookup_hit`: such a key matches no lookup.
        Err(_) => return KeyKind::Never,
    }
    s.resolved_with(r).map_or(KeyKind::Never, KeyKind::Value)
}

/// For `Document::check_lookup_indexes`: every lookup
/// of collection `n` through its index against the linear scan.
#[cfg(feature = "__internal-checks")]
pub(super) fn check_against_linear(n: NodeRef<'_>) -> Result<(), String> {
    match n.kind() {
        NodeKind::Mapping => {
            let m = MappingRef(n);
            let linear: Vec<(NodeRef<'_>, NodeRef<'_>)> = m.iter().collect();
            if m.len() != linear.len() {
                return Err(format!("len {} vs {}", m.len(), linear.len()));
            }
            for (i, e) in linear.iter().enumerate() {
                if m.entry(i) != Some(*e) {
                    return Err(format!("entry {i}: {:?} vs {e:?}", m.entry(i)));
                }
            }
            if m.entry(linear.len()).is_some() {
                return Err("an entry past the end".to_owned());
            }
            let r = n.resolver();
            let mut strings: Vec<String> = vec!["<<".into(), "".into(), "missing key".into()];
            let mut values = vec![Value::Null, Value::from(1)];
            for (k, _) in &linear {
                if let Some(s) = k.as_scalar() {
                    strings.push(s.value().into_owned());
                    values.extend(s.resolved_with(&r).ok());
                }
            }
            for q in &strings {
                let (a, b) = (
                    m.own_key(&r, KeyQuery::Str(q), false),
                    m.own_key(&r, KeyQuery::Str(q), true),
                );
                if a != b {
                    return Err(format!("own key {q:?}: {a:?} vs {b:?}"));
                }
                let (a, b) = (
                    m.get_in(KeyQuery::Str(q), false),
                    m.get_in(KeyQuery::Str(q), true),
                );
                if a != b {
                    return Err(format!("get {q:?}: {a:?} vs {b:?}"));
                }
            }
            for v in &values {
                let (a, b) = (
                    m.own_key(&r, KeyQuery::Value(v), false),
                    m.own_key(&r, KeyQuery::Value(v), true),
                );
                if a != b {
                    return Err(format!("key equal to {v:?}: {a:?} vs {b:?}"));
                }
            }
            let (a, b) = (m.last_merge(&r, false), m.last_merge(&r, true));
            if a != b {
                return Err(format!("last merge key: {a:?} vs {b:?}"));
            }
            let (a, b) = (
                m.has_merge_key_in(false).map_err(|e| e.code()),
                m.has_merge_key_in(true).map_err(|e| e.code()),
            );
            if a != b {
                return Err(format!("has a merge key: {a:?} vs {b:?}"));
            }
            Ok(())
        }
        NodeKind::Sequence => {
            let s = SequenceRef(n);
            let linear: Vec<NodeRef<'_>> = s.iter().collect();
            if s.len() != linear.len() {
                return Err(format!("len {} vs {}", s.len(), linear.len()));
            }
            for (i, x) in linear.iter().enumerate() {
                if s.get(i) != Some(*x) {
                    return Err(format!("item {i}: {:?} vs {x:?}", s.get(i)));
                }
            }
            if s.get(linear.len()).is_some() {
                return Err("an item past the end".to_owned());
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A sequence node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceRef<'d>(NodeRef<'d>);

impl<'d> SequenceRef<'d> {
    /// The sequence as a node.
    #[must_use]
    pub fn node(self) -> NodeRef<'d> {
        self.0
    }

    /// The items, in order.
    pub fn iter(self) -> impl Iterator<Item = NodeRef<'d>> + 'd {
        self.0.items()
    }

    /// Number of items. O(1) for a wide sequence (its lookup index),
    /// else O(items).
    #[must_use]
    pub fn len(self) -> usize {
        let indexed = self
            .0
            .doc
            .with_lookup(self.0.node, false, |ix| Some(ix.len()));
        indexed.unwrap_or_else(|| self.0.items().count())
    }

    /// `true` without items.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.0.items().next().is_none()
    }

    /// Item `index`. O(log width) for a wide sequence (its lookup index),
    /// else O(index).
    #[must_use]
    pub fn get(self, index: usize) -> Option<NodeRef<'d>> {
        let (doc, tree) = (self.0.doc, self.0.node.tree());
        let indexed = doc.with_lookup(self.0.node, false, |ix| {
            if index >= ix.len() {
                return Some(None);
            }
            let n = ix.item(index)?.node(tree)?;
            Some(Some(NodeRef::new(doc, presented(n))))
        });
        indexed.unwrap_or_else(|| self.0.items().nth(index))
    }
}

/// A scalar node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScalarRef<'d>(NodeRef<'d>);

impl<'d> ScalarRef<'d> {
    /// The scalar as a node.
    #[must_use]
    pub fn node(self) -> NodeRef<'d> {
        self.0
    }

    /// The content after escapes, folding, and chomping (§7, §8), before
    /// resolution: `'80'` and `80` both give `80`.
    #[must_use]
    pub fn value(self) -> Cow<'d, str> {
        self.0.node.value().unwrap_or(Cow::Borrowed(""))
    }

    /// How the scalar was written.
    #[must_use]
    pub fn style(self) -> ScalarStyle {
        match self.0.node.children().last().map(TreeNode::kind) {
            Some(SyntaxKind::SingleQuotedScalar) => ScalarStyle::SingleQuoted,
            Some(SyntaxKind::DoubleQuotedScalar) => ScalarStyle::DoubleQuoted,
            Some(SyntaxKind::LiteralScalar) => ScalarStyle::Literal,
            Some(SyntaxKind::FoldedScalar) => ScalarStyle::Folded,
            _ => ScalarStyle::Plain,
        }
    }

    /// The resolved value under the document's schema and settings (the
    /// one resolver): `80` is `Value::Number`, `'80'` a string.
    ///
    /// # Errors
    ///
    /// Resolution errors (e.g. `!!int x`), with their `Y1xxx` codes.
    pub fn resolved(self) -> Result<Value, Error> {
        self.resolved_with(&self.0.resolver())
    }

    /// The content, when the scalar resolves to a string: exactly when
    /// [`ScalarRef::resolved`] is `Ok(Value::String(content))`
    /// (`scalar_value` builds `Value::String` from `Resolved::Str` alone),
    /// without copying the content.
    fn as_str(self) -> Option<Cow<'d, str>> {
        let tag = self.0.doc.shared.tags.try_tag(self.0.node).ok()?;
        let value = self.value();
        let plain = self.style() == ScalarStyle::Plain;
        match resolve(&self.0.resolver(), &tag, plain, &value, self.0.span()) {
            Ok(Resolved::Str) => Some(value),
            _ => None,
        }
    }

    pub(super) fn resolved_with(self, r: &Resolver) -> Result<Value, Error> {
        let tag = self.0.doc.shared.tags.try_tag(self.0.node)?;
        let value = self.value();
        let plain = self.style() == ScalarStyle::Plain;
        let resolved = resolve(r, &tag, plain, &value, self.0.span())?;
        Ok(scalar_value(resolved, value, &tag))
    }

    /// Whether the scalar resolves to the string `key` (what
    /// `resolved_with(r) == Ok(Value::String(key))` says). A scalar
    /// resolves to a string only with its own content as the string, so
    /// the content is compared first and only an equal one is resolved
    /// (no resolution or allocation per non-matching key).
    /// The content is decoded once, and a match allocates
    /// nothing (`Resolved::Str` is the string of that content, as
    /// `scalar_value` builds it; a custom tag makes a tagged value).
    fn is_string(self, r: &Resolver, key: &str) -> bool {
        let value = self.value();
        if value != key {
            return false;
        }
        let Ok(tag) = self.0.doc.shared.tags.try_tag(self.0.node) else {
            return false;
        };
        let plain = self.style() == ScalarStyle::Plain;
        matches!(
            resolve(r, &tag, plain, &value, self.0.span()),
            Ok(Resolved::Str)
        )
    }

    /// Whether the scalar means the string `key`: its
    /// content is compared first, and the resolver is built only for an
    /// equal one (no allocation, as [`ScalarRef::is_string`]).
    pub(super) fn means_string(self, key: &str) -> bool {
        if self.value() != key {
            return false;
        }
        self.is_string(&self.0.resolver(), key)
    }

    /// # Errors
    ///
    /// `Y9000` for a tag that does not resolve.
    fn is_merge_key(self, r: &Resolver) -> Result<bool, Error> {
        let plain = self.style() == ScalarStyle::Plain;
        let tag = self.0.doc.shared.tags.try_tag(self.0.node)?;
        Ok(is_merge_key(r, &tag, plain, &self.value()))
    }
}
