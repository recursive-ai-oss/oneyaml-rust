//! The text index: which key of a mapping a pointer token names.
//!
//! A token names the key whose scalar text it is (the content after quoting
//! and escapes, before the schema gives it a type), so the keys `80`, `'80'`
//! and `"80"` all answer to the token `80`, whatever tag a key carries. An alias
//! key has the text of the node it names. Every mapping, narrow or wide, is
//! looked up the same way: through an index from the digest of a key's text to
//! the keys with it, built on first use and kept up to date by the patch's own
//! edits, so a token costs a hash lookup however wide the mapping is and the
//! answer never depends on the width.
//!
//! Two keys with the same text make the token ambiguous, and so does a key
//! that a later duplicate of the same meaning overrides (`16: a` followed by
//! `0x10: b` under `DuplicateKeys::LastWins` means `{16: b}`, so `/16` must
//! not read `a`).
//!
//! Alias keys are the exception to "built once", because what they name can
//! change under an edit. The text of an alias key is read at most once per
//! patch and remembered together with the node it was read from. The patch's
//! own removals and replacements forget the texts that came from the nodes
//! they touch, and a remembered text is used only while its alias still names
//! the same node. Every read is charged by its length in bytes to the
//! document's alias byte budget, and the walk over the alias keys and merged
//! mappings of each lookup is charged in steps, so the work of a patch is
//! bounded by those budgets and not by the number of its operations times the
//! size of what its alias keys name.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};

use super::{PatchError, PatchErrorKind};
use crate::DuplicateKeys;
use crate::document::{Document, MappingRef, NodeId, NodeKind, NodeRef};

/// Steps a patch may spend on alias keys and merged mappings without regard to
/// the document's size. A step is one alias key looked at or one merged
/// mapping searched; a file that merges a handful of mappings needs a few per
/// operation.
const WORK_FLOOR: usize = 1024 * 1024;

#[derive(Debug)]
enum Bucket {
    One(NodeId),
    Many(Vec<NodeId>),
}

impl Bucket {
    fn keys(&self) -> &[NodeId] {
        match self {
            Self::One(k) => std::slice::from_ref(k),
            Self::Many(v) => v,
        }
    }
}

/// The keys of one mapping.
#[derive(Debug, Default)]
struct MapIndex {
    by_text: HashMap<u64, Bucket>,
    aliases: Vec<NodeId>,
}

/// What reading an alias key found, and the node it was read from.
#[derive(Debug)]
struct AliasMemo {
    /// The node the alias named when it was read; `None` for an alias that
    /// names nothing.
    target: Option<NodeId>,
    /// The text of the scalar it names; `None` when that is not a scalar.
    text: Option<String>,
}

/// Why a token is ambiguous.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cause {
    /// Two keys have the same text.
    SameText,
    /// One key has it, and a later key with the same meaning overrides that
    /// key.
    Overridden,
}

/// What a lookup in one mapping found.
enum Own {
    Absent,
    Ambiguous(Cause),
    One(NodeId),
}

/// Whether a key has a token's text.
enum Match {
    /// The node is gone: the index is stale.
    Gone,
    Yes,
    No,
}

/// What a lookup through a mapping and its merge sources found.
pub(super) enum Hit<'d> {
    Absent,
    Ambiguous(Cause),
    One {
        key: NodeId,
        value: NodeRef<'d>,
        /// The key is in a mapping merged in with `<<`, not the one asked.
        merged: bool,
    },
}

/// The text indexes of the mappings one patch (or a run of reads) looks at, and
/// the work it has spent.
pub(super) struct Texts {
    maps: HashMap<NodeId, MapIndex>,
    /// Alias keys that were read, by the key.
    aliases: HashMap<NodeId, AliasMemo>,
    /// The mappings a mapping merges in, by the mapping (only with merge keys
    /// on), until the next write.
    sources: HashMap<NodeId, Vec<NodeId>>,
    hasher: RandomState,
    /// Keys put into indexes, for the cap on how much a cache of reads keeps.
    indexed: usize,
    spent: usize,
    budget: Option<usize>,
    bytes_spent: usize,
    byte_budget: Option<usize>,
    /// Whether the document may hold keys that override each other, once
    /// asked.
    duplicates: Option<bool>,
    /// Keys put into indexes, and key texts read, for the tests that pin the
    /// work a lookup does.
    #[cfg(test)]
    pub(super) built: usize,
    #[cfg(test)]
    pub(super) reads: usize,
}

#[cfg(test)]
thread_local! {
    /// Keys put into indexes by this thread, whichever `Texts` did it.
    pub(super) static BUILT_ON_THREAD: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Texts {
    /// For a patch to `doc`: the steps are bounded by the floor plus twice the
    /// document's length in bytes, and the bytes read from alias keys by the
    /// document's alias byte budget, unless its limits are off.
    pub(super) fn new(doc: &Document) -> Self {
        let limits = doc.settings().get_limits();
        let len = doc.span().len();
        let budget = limits
            .get_alias_byte_budget(0)
            .map(|_| WORK_FLOOR.saturating_add(len.saturating_mul(2)));
        Self::with_budgets(budget, limits.get_alias_byte_budget(len))
    }

    /// For a read: never refuses.
    pub(super) fn unlimited() -> Self {
        Self::with_budgets(None, None)
    }

    fn with_budgets(budget: Option<usize>, byte_budget: Option<usize>) -> Self {
        Self {
            maps: HashMap::new(),
            aliases: HashMap::new(),
            sources: HashMap::new(),
            hasher: RandomState::new(),
            indexed: 0,
            spent: 0,
            budget,
            bytes_spent: 0,
            byte_budget,
            duplicates: None,
            #[cfg(test)]
            built: 0,
            #[cfg(test)]
            reads: 0,
        }
    }

    /// How many keys the indexes hold.
    pub(super) fn indexed(&self) -> usize {
        self.indexed
    }

    fn charge(&mut self, units: usize) -> Result<(), PatchError> {
        self.spent = self.spent.saturating_add(units);
        match self.budget {
            Some(b) if self.spent > b => Err(PatchError::new(
                PatchErrorKind::Limit,
                format!(
                    "the patch looks through too many alias keys and merged mappings to find its \
                     locations (more than {b} steps); use fewer operations on mappings that merge \
                     many others"
                ),
            )),
            _ => Ok(()),
        }
    }

    fn charge_bytes(&mut self, bytes: usize) -> Result<(), PatchError> {
        self.bytes_spent = self.bytes_spent.saturating_add(bytes);
        match self.byte_budget {
            Some(b) if self.bytes_spent > b => Err(PatchError::new(
                PatchErrorKind::Limit,
                format!(
                    "the patch reads too much text from alias keys to find its locations \
                     (more than {b} bytes, Limits::alias_byte_budget); use fewer operations on \
                     mappings whose keys are aliases of large values"
                ),
            )),
            _ => Ok(()),
        }
    }

    fn digest(&self, text: &str) -> u64 {
        self.hasher.hash_one(text)
    }

    fn file(&mut self, ix: &mut MapIndex, id: NodeId, text: &str) {
        self.indexed = self.indexed.saturating_add(1);
        #[cfg(test)]
        {
            self.built = self.built.saturating_add(1);
            BUILT_ON_THREAD.with(|c| c.set(c.get().saturating_add(1)));
        }
        let d = self.hasher.hash_one(text);
        match ix.by_text.remove(&d) {
            None => {
                ix.by_text.insert(d, Bucket::One(id));
            }
            Some(Bucket::One(first)) => {
                ix.by_text.insert(d, Bucket::Many(vec![first, id]));
            }
            Some(Bucket::Many(mut v)) => {
                v.push(id);
                ix.by_text.insert(d, Bucket::Many(v));
            }
        }
    }

    fn build(&mut self, map: MappingRef<'_>) {
        let mut ix = MapIndex::default();
        let mut keys: Vec<(NodeId, Option<String>)> = Vec::new();
        map.for_each_text_key(|key, text| keys.push((key.id(), text.map(|t| t.into_owned()))));
        for (id, text) in keys {
            match text {
                None => ix.aliases.push(id),
                Some(text) => self.file(&mut ix, id, &text),
            }
        }
        self.maps.insert(map.node().id(), ix);
    }

    /// Whether the non-alias key `id` has the text `token`.
    fn key_matches(doc: &Document, id: NodeId, token: &str) -> Match {
        let Ok(key) = doc.node(id) else {
            return Match::Gone;
        };
        if key.as_scalar().is_some_and(|s| s.value() == token) {
            Match::Yes
        } else {
            Match::No
        }
    }

    /// Whether the alias key `id` has the text `token`. The text is read when
    /// the alias has not been read, or names another node than it did, and
    /// the read is charged by the length of what it names.
    fn alias_matches(
        &mut self,
        doc: &Document,
        id: NodeId,
        token: &str,
    ) -> Result<Match, PatchError> {
        let Ok(key) = doc.node(id) else {
            return Ok(Match::Gone);
        };
        let target = key.alias_target();
        let target_id = target.map(NodeRef::id);
        if let Some(memo) = self.aliases.get(&id).filter(|m| m.target == target_id) {
            return Ok(if memo.text.as_deref() == Some(token) {
                Match::Yes
            } else {
                Match::No
            });
        }
        // The length of the source is charged before it is decoded, so a
        // refused read costs nothing.
        self.charge_bytes(target.map_or(0, |t| t.span().len()))?;
        #[cfg(test)]
        {
            self.reads = self.reads.saturating_add(1);
        }
        let text = target
            .and_then(NodeRef::as_scalar)
            .map(|s| s.value().into_owned());
        let hit = text.as_deref() == Some(token);
        self.aliases.insert(
            id,
            AliasMemo {
                target: target_id,
                text,
            },
        );
        Ok(if hit { Match::Yes } else { Match::No })
    }

    /// The bytes of the sources the alias keys `ids` name, which is what a
    /// read of all of them (by the edit layer's own key comparisons) costs.
    fn charge_alias_sources(&mut self, doc: &Document, ids: &[NodeId]) -> Result<(), PatchError> {
        let total = ids.iter().fold(0usize, |sum, id| {
            let len = doc
                .node(*id)
                .ok()
                .and_then(NodeRef::alias_target)
                .map_or(0, |t| t.span().len());
            sum.saturating_add(len)
        });
        self.charge(ids.len())?;
        self.charge_bytes(total)
    }

    /// Whether keys of the document can override each other: with
    /// `DuplicateKeys::LastWins`, or where an error has been recovered from.
    fn may_override(&mut self, doc: &Document) -> bool {
        *self.duplicates.get_or_insert_with(|| {
            doc.settings().get_duplicate_keys() != DuplicateKeys::Error || doc.has_errors()
        })
    }

    /// Whether `key`, the one key of `map` with a token's text, is the one
    /// that counts for what it means.
    ///
    /// The edit layer decides this by comparing the key with every alias key
    /// of the mapping afresh, so where there are alias keys the bytes that
    /// reads takes are charged, and where the document cannot hold keys that
    /// override each other (the default duplicate-key setting, and no
    /// recovered error) the question is not asked.
    fn governs(
        &mut self,
        doc: &Document,
        map: MappingRef<'_>,
        ix: &MapIndex,
        key: NodeId,
    ) -> Result<bool, PatchError> {
        let Ok(key) = doc.node(key) else {
            return Ok(false);
        };
        if !ix.aliases.is_empty() {
            if !self.may_override(doc) {
                return Ok(true);
            }
            self.charge_alias_sources(doc, &ix.aliases)?;
        }
        Ok(map.governs(key))
    }

    /// The key of `map` with the text `token`.
    fn own<'d>(
        &mut self,
        doc: &'d Document,
        map: MappingRef<'d>,
        token: &str,
    ) -> Result<Own, PatchError> {
        let mid = map.node().id();
        // A stale entry (the edit layer rewrote the mapping) is rebuilt once.
        for attempt in 0..2 {
            if !self.maps.contains_key(&mid) {
                self.build(map);
            }
            let Some(ix) = self.maps.remove(&mid) else {
                break;
            };
            let found = self.scan(doc, &ix, token);
            let out = match found {
                Err(e) => Err(e),
                Ok((_, true)) if attempt == 0 => continue,
                Ok((found, _)) => match found.as_slice() {
                    [] => Ok(Own::Absent),
                    [one] => self.governs(doc, map, &ix, *one).map(|g| {
                        if g {
                            Own::One(*one)
                        } else {
                            Own::Ambiguous(Cause::Overridden)
                        }
                    }),
                    _ => Ok(Own::Ambiguous(Cause::SameText)),
                },
            };
            self.maps.insert(mid, ix);
            return out;
        }
        Err(PatchError::new(
            PatchErrorKind::Edit,
            "internal error: a mapping's key index could not be rebuilt",
        ))
    }

    /// The keys of an index with the text `token` (two are enough), and
    /// whether the index named a node that is gone.
    fn scan(
        &mut self,
        doc: &Document,
        ix: &MapIndex,
        token: &str,
    ) -> Result<(Vec<NodeId>, bool), PatchError> {
        self.charge(ix.aliases.len())?;
        let d = self.digest(token);
        let candidates = ix.by_text.get(&d).map_or(&[][..], Bucket::keys);
        let mut found: Vec<NodeId> = Vec::new();
        let mut stale = false;
        for &id in candidates {
            // Two keys with the same text are enough to call it ambiguous.
            if found.len() >= 2 {
                break;
            }
            #[cfg(test)]
            {
                self.reads = self.reads.saturating_add(1);
            }
            match Self::key_matches(doc, id, token) {
                Match::Yes => {
                    if !found.contains(&id) {
                        found.push(id);
                    }
                }
                Match::No => {}
                Match::Gone => stale = true,
            }
        }
        for &id in &ix.aliases {
            if found.len() >= 2 {
                break;
            }
            match self.alias_matches(doc, id, token)? {
                Match::Yes => {
                    if !found.contains(&id) {
                        found.push(id);
                    }
                }
                Match::No => {}
                Match::Gone => stale = true,
            }
        }
        Ok((found, stale))
    }

    /// The mappings `m` merges in with `<<`, in the order a lookup searches
    /// them. They are remembered until the patch next writes, because finding
    /// them reads the mapping's alias keys.
    fn merge_sources<'d>(
        &mut self,
        doc: &'d Document,
        m: MappingRef<'d>,
    ) -> Result<Vec<MappingRef<'d>>, PatchError> {
        let id = m.node().id();
        if !m.node().resolver().merge_keys() {
            return Ok(Vec::new());
        }
        if let Some(ids) = self.sources.get(&id) {
            let found: Vec<MappingRef<'d>> = ids
                .iter()
                .filter_map(|i| doc.node(*i).ok().and_then(NodeRef::as_mapping))
                .collect();
            self.charge(found.len())?;
            return Ok(found);
        }
        let aliases = self
            .maps
            .get(&id)
            .map(|ix| ix.aliases.clone())
            .unwrap_or_default();
        self.charge_alias_sources(doc, &aliases)?;
        let found = m.merge_sources();
        self.charge(found.len())?;
        self.sources
            .insert(id, found.iter().map(|s| s.node().id()).collect());
        Ok(found)
    }

    /// The member `token` names in `map`: its own key, or, failing that, a
    /// key of a mapping merged in with `<<`, searched in the order a lookup
    /// uses.
    pub(super) fn find<'d>(
        &mut self,
        doc: &'d Document,
        map: MappingRef<'d>,
        token: &str,
    ) -> Result<Hit<'d>, PatchError> {
        let root = map.node().id();
        let mut stack = vec![map];
        let mut seen = HashSet::new();
        while let Some(m) = stack.pop() {
            if !seen.insert(m.node().id()) {
                continue;
            }
            self.charge(1)?;
            match self.own(doc, m, token)? {
                Own::Ambiguous(cause) => return Ok(Hit::Ambiguous(cause)),
                Own::One(key) => {
                    let value = doc.node(key).ok().and_then(NodeRef::entry_value);
                    if let Some(value) = value {
                        return Ok(Hit::One {
                            key,
                            value,
                            merged: m.node().id() != root,
                        });
                    }
                }
                Own::Absent => {}
            }
            let sources = self.merge_sources(doc, m)?;
            stack.extend(sources.into_iter().rev());
        }
        Ok(Hit::Absent)
    }

    /// A member with the value `value` was added to `map`.
    pub(super) fn added(&mut self, doc: &Document, map: NodeId, value: NodeId) {
        // A new member can be a merge key or an item of the merged sequence.
        self.sources.clear();
        if !self.maps.contains_key(&map) {
            return;
        }
        let key = doc
            .node(value)
            .ok()
            .and_then(NodeRef::entry_key)
            .and_then(|k| Some((k.id(), k.as_scalar()?.value().into_owned())));
        let Some((id, text)) = key else {
            self.maps.remove(&map);
            return;
        };
        let Some(mut ix) = self.maps.remove(&map) else {
            return;
        };
        self.file(&mut ix, id, &text);
        self.maps.insert(map, ix);
    }

    /// The member of `map` with the key `key` (whose text is `token`) was
    /// removed.
    pub(super) fn removed(&mut self, map: NodeId, token: &str, key: NodeId) {
        let d = self.hasher.hash_one(token);
        self.aliases.remove(&key);
        let Some(ix) = self.maps.get_mut(&map) else {
            return;
        };
        ix.aliases.retain(|k| *k != key);
        match ix.by_text.remove(&d) {
            None => {}
            Some(Bucket::One(k)) if k == key => {}
            Some(Bucket::One(k)) => {
                ix.by_text.insert(d, Bucket::One(k));
            }
            Some(Bucket::Many(mut v)) => {
                v.retain(|k| *k != key);
                match v.as_slice() {
                    [] => {}
                    [one] => {
                        ix.by_text.insert(d, Bucket::One(*one));
                    }
                    _ => {
                        ix.by_text.insert(d, Bucket::Many(v));
                    }
                }
            }
        }
    }

    /// The node `node` is about to be removed or replaced: the texts read from
    /// it, or from anything under it, are forgotten, and so are the merged
    /// mappings found so far.
    pub(super) fn will_change(&mut self, doc: &Document, node: NodeId) -> Result<(), PatchError> {
        self.sources.clear();
        if self.aliases.is_empty() {
            return Ok(());
        }
        // One walk up from each distinct node an alias key was read from.
        self.charge(self.aliases.len())?;
        let hit = |target: NodeId| match doc.node(target) {
            Err(_) => true,
            Ok(t) => target == node || t.tree_node().ancestors().any(|a| a.id() == node.0),
        };
        self.aliases.retain(|_, memo| !memo.target.is_some_and(hit));
        Ok(())
    }

    /// A sequence changed (an item added): the merged mappings found so far
    /// may have.
    pub(super) fn sequence_changed(&mut self) {
        self.sources.clear();
    }

    /// The whole document was replaced: nothing found so far holds.
    pub(super) fn reset(&mut self) {
        self.maps.clear();
        self.aliases.clear();
        self.sources.clear();
    }

    /// An edit adds a member to `map`, which compares the new key with all
    /// the alias keys of the mapping: charge what that reads.
    pub(super) fn will_insert(&mut self, doc: &Document, map: NodeId) -> Result<(), PatchError> {
        if !self.maps.contains_key(&map) {
            if let Some(m) = doc.node(map).ok().and_then(NodeRef::as_mapping) {
                if m.node().kind() == NodeKind::Mapping {
                    self.build(m);
                }
            }
        }
        let aliases = self
            .maps
            .get(&map)
            .map(|ix| ix.aliases.clone())
            .unwrap_or_default();
        if aliases.is_empty() {
            return Ok(());
        }
        self.charge_alias_sources(doc, &aliases)
    }
}
