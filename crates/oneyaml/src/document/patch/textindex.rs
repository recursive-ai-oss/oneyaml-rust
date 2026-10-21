//! The text index: which key of a mapping a pointer token names.
//!
//! A token names the key whose scalar text it is (the content after quoting
//! and escapes, before the schema gives it a type), so the keys `80`, `'80'`
//! and `"80"` all answer to the token `80`, whatever tag a key carries. An alias
//! key has the text of the node it names. Every mapping,
//! narrow or wide, is looked up the same way: through an index from the
//! digest of a key's text to the keys with it, built on first use and kept up
//! to date by the patch's own edits, so a token costs a hash lookup however
//! wide the mapping is and the answer never depends on the width.
//!
//! Two keys with the text make the token ambiguous, and so does a key that a
//! later duplicate of the same meaning overrides (`16: a` followed by
//! `0x10: b` under `DuplicateKeys::LastWins` means `{16: b}`, so `/16` must
//! not read `a`). Alias keys are read live, because what they name can change
//! under an edit; the reading, and the walk of a mapping's merge sources,
//! are charged to a work budget so a patch cannot make them quadratic.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};

use super::{PatchError, PatchErrorKind};
use crate::document::{Document, MappingRef, NodeId, NodeKind, NodeRef};

/// Steps a patch may spend on alias keys and merge sources without regard to
/// the document's size. A step is one alias key read or one merged mapping
/// searched; a file that merges a handful of mappings needs a few per
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

/// What reading a key found.
enum KeyText {
    /// The node is gone: the index is stale.
    Gone,
    /// An alias that names no scalar, or a key that is not one.
    None,
    Text(String),
}

/// What a lookup in one mapping found.
enum Own {
    Absent,
    Ambiguous,
    One(NodeId),
}

/// What a lookup through a mapping and its merge sources found.
pub(super) enum Hit<'d> {
    Absent,
    Ambiguous,
    One {
        key: NodeId,
        value: NodeRef<'d>,
        /// The key is in a mapping merged in with `<<`, not the one asked.
        merged: bool,
    },
}

/// The text indexes of the mappings one patch (or one read) looks at, and the
/// work it has spent.
pub(super) struct Texts {
    maps: HashMap<NodeId, MapIndex>,
    hasher: RandomState,
    spent: usize,
    budget: Option<usize>,
    /// Keys put into indexes, and keys read to confirm a candidate, for the
    /// tests that pin the work a lookup does.
    #[cfg(test)]
    pub(super) built: usize,
    #[cfg(test)]
    pub(super) reads: usize,
}

impl Texts {
    /// For a patch to `doc`: the steps are bounded by the floor plus twice the
    /// document's length in bytes, unless the document's limits are off.
    pub(super) fn new(doc: &Document) -> Self {
        let limits = doc.settings().get_limits();
        let budget = limits
            .get_alias_byte_budget(0)
            .map(|_| WORK_FLOOR.saturating_add(doc.span().len().saturating_mul(2)));
        Self::with_budget(budget)
    }

    /// For a read: never refuses.
    pub(super) fn unlimited() -> Self {
        Self::with_budget(None)
    }

    fn with_budget(budget: Option<usize>) -> Self {
        Self {
            maps: HashMap::new(),
            hasher: RandomState::new(),
            spent: 0,
            budget,
            #[cfg(test)]
            built: 0,
            #[cfg(test)]
            reads: 0,
        }
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

    fn digest(&self, text: &str) -> u64 {
        self.hasher.hash_one(text)
    }

    fn build(&mut self, map: MappingRef<'_>) {
        let mut ix = MapIndex::default();
        let hasher = &self.hasher;
        #[cfg(test)]
        let mut built = 0usize;
        map.for_each_text_key(|key, text| {
            #[cfg(test)]
            {
                built = built.saturating_add(1);
            }
            match text {
                None => ix.aliases.push(key.id()),
                Some(text) => {
                    let d = hasher.hash_one(&*text);
                    match ix.by_text.remove(&d) {
                        None => {
                            ix.by_text.insert(d, Bucket::One(key.id()));
                        }
                        Some(Bucket::One(first)) => {
                            ix.by_text.insert(d, Bucket::Many(vec![first, key.id()]));
                        }
                        Some(Bucket::Many(mut v)) => {
                            v.push(key.id());
                            ix.by_text.insert(d, Bucket::Many(v));
                        }
                    }
                }
            }
        });
        #[cfg(test)]
        {
            self.built = self.built.saturating_add(built);
        }
        self.maps.insert(map.node().id(), ix);
    }

    /// The text of the key `id`.
    fn key_text(doc: &Document, id: NodeId) -> KeyText {
        let Ok(key) = doc.node(id) else {
            return KeyText::Gone;
        };
        let scalar = if key.kind() == NodeKind::Alias {
            key.alias_target().and_then(NodeRef::as_scalar)
        } else {
            key.as_scalar()
        };
        scalar.map_or(KeyText::None, |s| KeyText::Text(s.value().into_owned()))
    }

    /// The key of `map` with the text `token`.
    fn own<'d>(
        &mut self,
        doc: &'d Document,
        map: MappingRef<'d>,
        token: &str,
    ) -> Result<Own, PatchError> {
        // A stale entry (the edit layer rewrote the mapping) is rebuilt once.
        for attempt in 0..2 {
            if !self.maps.contains_key(&map.node().id()) {
                self.build(map);
            }
            let d = self.digest(token);
            let aliases_len = self
                .maps
                .get(&map.node().id())
                .map_or(0, |i| i.aliases.len());
            self.charge(aliases_len)?;
            let Some(ix) = self.maps.get(&map.node().id()) else {
                break;
            };
            let candidates = ix.by_text.get(&d).map_or(&[][..], Bucket::keys);
            let mut found: Vec<NodeId> = Vec::new();
            let mut stale = false;
            #[cfg(test)]
            let mut reads = 0usize;
            for &id in candidates.iter().chain(ix.aliases.iter()) {
                // Two keys with the text are enough to call it ambiguous.
                if found.len() >= 2 {
                    break;
                }
                #[cfg(test)]
                {
                    reads = reads.saturating_add(1);
                }
                match Self::key_text(doc, id) {
                    KeyText::Text(text) if text == token => {
                        if !found.contains(&id) {
                            found.push(id);
                        }
                    }
                    KeyText::Text(_) | KeyText::None => {}
                    KeyText::Gone => stale = true,
                }
            }
            #[cfg(test)]
            {
                self.reads = self.reads.saturating_add(reads);
            }
            if stale && attempt == 0 {
                self.maps.remove(&map.node().id());
                continue;
            }
            return Ok(match found.as_slice() {
                [] => Own::Absent,
                [one] => {
                    let governs = doc.node(*one).is_ok_and(|k| map.governs(k));
                    if governs {
                        Own::One(*one)
                    } else {
                        Own::Ambiguous
                    }
                }
                _ => Own::Ambiguous,
            });
        }
        Err(PatchError::new(
            PatchErrorKind::Edit,
            "internal error: a mapping's key index could not be rebuilt",
        ))
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
                Own::Ambiguous => return Ok(Hit::Ambiguous),
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
            let sources = m.merge_sources();
            self.charge(sources.len())?;
            stack.extend(sources.into_iter().rev());
        }
        Ok(Hit::Absent)
    }

    /// A member with the key `token` and the value `value` was added to `map`.
    pub(super) fn added(&mut self, doc: &Document, map: NodeId, value: NodeId) {
        let Some(ix) = self.maps.get_mut(&map) else {
            return;
        };
        let key = doc
            .node(value)
            .ok()
            .and_then(NodeRef::entry_key)
            .and_then(|k| Some((k.id(), k.as_scalar()?.value().into_owned())));
        let Some((id, text)) = key else {
            self.maps.remove(&map);
            return;
        };
        let d = self.hasher.hash_one(text.as_str());
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

    /// The member of `map` with the key `key` (whose text is `token`) was
    /// removed.
    pub(super) fn removed(&mut self, map: NodeId, token: &str, key: NodeId) {
        let d = self.hasher.hash_one(token);
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
}
