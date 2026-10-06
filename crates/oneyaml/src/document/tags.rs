//! Resolved tags of one document's tree, for node reads (kept
//! small and cheap).
//!
//! Every node of the tree with a `%TAG`-prefixed tag maps to its
//! [`Tag`](crate::Tag), built once per distinct tag by one [`TagCache`]
//! (a tag shares its `%TAG` prefix, decoded once per directive,
//! and holds its own suffix, so the table costs O(the distinct suffixes)
//! plus one copy of each prefix, and needs no bound). Equal tags of the
//! table are one allocation. [`NodeRef::tag`](super::NodeRef::tag), scalar
//! resolution, key lookups, per-node reads, `Document::to_value` and
//! `Document::deserialize` read it: a use costs O(the property's own
//! text). Other tags (default handles, verbatim) cost only their own text
//! and are built per call.
//!
//! As the other caches of `Shared` (`node_events`): built lazily, on the
//! first read of a `%TAG`-prefixed tag (a document without one never
//! builds it), and
//! set-once (`OnceLock`), so readers share it without a lock. Its map is
//! keyed by tree handles only (`IdMap`). A copy of the tree
//! (`Shared::copy`) starts a new table.
//!
//! Unlike `node_events` it is **not** dropped by an edit (rebuilding it is
//! O(tree), which would make every edit of a tagged document O(document), a
//! cost edits must not pay). It stays exact because an edit never changes the
//! tag of a node it keeps: a node keeps its handle and its tag token until it
//! is removed (tag tokens are read-only to the syntax layer's edits), and a
//! node an edit writes has a new handle. Such a node is not in the table and is
//! resolved per call, in O(its own tag text): the edit layer never writes a
//! `%TAG`-prefixed tag, because it refuses to write any tag into a document
//! that has `%TAG` directives (`EditError::Unsupported`, "a tag written into a
//! document with %TAG directives", for new values and for moved or re-inserted
//! entries alike), and refuses to re-insert a [`Removed`](super::Removed) taken
//! from another document (`EditError::WrongDocument`), whose tags were resolved
//! under other directives. `tests/tag_prefix_sharing.rs` pins both refusals as
//! this module's precondition.

use std::sync::OnceLock;

use oneyaml_syntax::tree::{Node as TreeNode, SyntaxTree};

use super::carry::IdMap;
use crate::construct::{Tag, TagCache, TagText};
use crate::error::Error;
use crate::value::Tag as ValueTag;

/// The resolved tags of a tree (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct DocTags {
    table: OnceLock<IdMap<ValueTag>>,
}

/// Every node of `tree` with a `%TAG`-prefixed tag, and its tag, each
/// distinct tag built once. O(tree + distinct suffixes + prefixes).
fn build(tree: &SyntaxTree) -> IdMap<ValueTag> {
    let mut cache = TagCache::default();
    let mut out = IdMap::default();
    for n in tree.root().descendants() {
        if !n.kind().is_yaml_node() {
            continue;
        }
        let Some(t) = n.tag_source().filter(|t| t.prefix_key().is_some()) else {
            continue;
        };
        // A tag that does not resolve is left out: `try_tag` then resolves
        // it on its own and reports the internal error.
        if let Ok(Tag::Named(t)) = cache.tag(Some(t)) {
            out.insert(n.id(), t.value_tag());
        }
    }
    out
}

impl DocTags {
    /// A fresh (empty, unbuilt) table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Node `n`'s tag as the construction layer reads it (the text
    /// every other reader gives the same node). O(the property's text)
    /// once the table is built (O(tree), on the first `%TAG`-prefixed tag
    /// read).
    ///
    /// # Errors
    ///
    /// `Y9000` (internal) for a tag property that does not resolve, which
    /// the parser rules out.
    pub(crate) fn try_tag<'d>(&'d self, n: TreeNode<'d>) -> Result<Tag<'d>, Error> {
        let source = n.tag_source();
        if source.as_ref().is_some_and(|t| t.prefix_key().is_some()) {
            let table = self.table.get_or_init(|| build(n.tree()));
            if let Some(t) = table.get(&n.id()) {
                return Ok(Tag::Named(TagText::new(t.clone())));
            }
        }
        TagCache::one(source)
    }

    /// Node `n`'s resolved tag (`NodeRef::tag`): `None` when untagged.
    ///
    /// # Errors
    ///
    /// `Y9000` as [`DocTags::try_tag`].
    pub(crate) fn value_tag(&self, n: TreeNode<'_>) -> Result<Option<ValueTag>, Error> {
        Ok(match self.try_tag(n)? {
            Tag::Named(t) => Some(t.value_tag()),
            Tag::NonSpecific => Some(ValueTag::new("!")),
            Tag::Absent => None,
        })
    }
}
