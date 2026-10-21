//! Equal collection keys: putting a removed entry whose key is a
//! sequence or a mapping back into a mapping must not make a duplicate key.
//!
//! A key is compared by a 64-bit digest of its meaning: scalars by their
//! resolved value (`[1]` and `[0x1]` agree), sequences in order, mappings
//! whatever the order of their entries, tags included, and an alias by the
//! digest of the node it names (so `[*s]` and `[x]` are one key when `s` is
//! `&s x`). The digest of an anchored node is computed
//! once per cache and reused by every alias to it, so an alias is never
//! expanded: a key that names a 10,000-item anchor costs O(its own text)
//! however many keys do.
//!
//! **Cost.** Reading the digests of a mapping's collection keys is
//! O(the text of those keys plus the anchored nodes they name, each once),
//! done once per edit state. The cache is then kept up to date by the two
//! operations that change a mapping's collection keys (removing and
//! putting back an entry), so a run of N removals and re-insertions into K
//! collection keys costs O(N + K), each operation O(the size of its own
//! key). Any other edit drops the cache, to be read again on next use.
//!
//! **Memory.** One digest and one count per distinct key (a hash map),
//! plus one digest per anchored node a key names, for up to four mappings;
//! dropped with any other edit and never copied by a clone. A digest collision (2⁻⁶⁴ per pair)
//! would refuse a re-insertion that is not a duplicate; a duplicate is
//! never missed except through an alias redefined in between (aliases
//! compare by what they name when the key is read).
//!
//! Loops only, no recursion: the key's nesting depth comes from the input.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

use oneyaml_syntax::tree::NodeId as TreeId;

use super::view::{NodeKind, NodeRef};
use crate::value::Value;

/// The digests of the collection keys of one mapping, for one edit state.
pub(super) struct CKeys {
    /// The mapping.
    mapping: TreeId,
    /// The edit state (see `Document::serial`) the digests describe.
    serial: u64,
    /// Each distinct key digest and how many keys have it.
    counts: HashMap<u64, u32>,
    /// The digest of each anchored node a key names.
    anchored: HashMap<TreeId, u64>,
}

// Nodes digested, in tests (work units, not time).
#[cfg(test)]
thread_local! {
    pub(super) static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One hasher family for the process, so a digest taken now equals one
/// taken later, and nothing about it is predictable from outside.
fn state() -> &'static std::collections::hash_map::RandomState {
    static S: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    S.get_or_init(std::collections::hash_map::RandomState::new)
}

fn hash_of(parts: &[u64]) -> u64 {
    let mut h = state().build_hasher();
    for p in parts {
        h.write_u64(*p);
    }
    h.finish()
}

fn hash_value(v: &Value) -> u64 {
    state().hash_one(v)
}

fn hash_text(s: &str) -> u64 {
    state().hash_one(s)
}

/// A collection being digested.
struct Frame<'d> {
    node: NodeRef<'d>,
    /// The node whose digest this is going to be memoised for (an anchored
    /// node reached through an alias).
    memo: Option<TreeId>,
    map: bool,
    items: Box<dyn Iterator<Item = NodeRef<'d>> + 'd>,
    /// Entries: the sum of the entry digests (order does not matter).
    /// Items: a running digest in order.
    acc: u64,
    count: u64,
    key: Option<u64>,
}

fn frame<'d>(node: NodeRef<'d>, memo: Option<TreeId>) -> Option<Frame<'d>> {
    let (map, items): (bool, Box<dyn Iterator<Item = NodeRef<'d>> + 'd>) = match node.kind() {
        NodeKind::Mapping => {
            let m = node.as_mapping()?;
            (true, Box::new(m.iter().flat_map(|(k, v)| [k, v])))
        }
        NodeKind::Sequence => (false, Box::new(node.as_sequence()?.iter())),
        _ => return None,
    };
    Some(Frame {
        node,
        memo,
        map,
        items,
        acc: 0,
        count: 0,
        key: None,
    })
}

fn tag_digest(n: NodeRef<'_>) -> u64 {
    match n.tag() {
        Ok(Some(t)) => hash_text(&format!("{t:?}")),
        Ok(None) => 0,
        Err(_) => 1,
    }
}

/// A leaf's digest, or `None` for a collection.
fn leaf(n: NodeRef<'_>) -> Option<u64> {
    #[cfg(test)]
    WORK.with(|w| w.set(w.get().saturating_add(1)));
    match n.kind() {
        NodeKind::Scalar => {
            let s = n.as_scalar()?;
            Some(hash_of(&[
                2,
                tag_digest(n),
                match s.resolved() {
                    Ok(v) => hash_value(&v),
                    Err(_) => hash_text(&s.value()),
                },
            ]))
        }
        NodeKind::Sequence | NodeKind::Mapping => None,
        // An error region: never equal to anything.
        _ => Some(hash_of(&[4, hash_text(&format!("{:?}", n.id()))])),
    }
}

impl CKeys {
    fn new(mapping: TreeId, serial: u64) -> Self {
        Self {
            mapping,
            serial,
            counts: HashMap::new(),
            anchored: HashMap::new(),
        }
    }

    /// The digest of `root`, with an explicit stack.
    pub(super) fn digest(&mut self, root: NodeRef<'_>) -> u64 {
        let mut open: HashSet<TreeId> = HashSet::new();
        let mut stack: Vec<Frame<'_>> = Vec::new();
        let mut done: Option<u64> = None;
        // Starts with the root as the node to enter.
        let mut enter = Some(root);
        loop {
            if let Some(n) = enter.take() {
                // An alias: the anchored node's digest, once.
                let (n, memo) = if n.kind() == NodeKind::Alias {
                    match n.alias_target() {
                        Some(t) => {
                            let id = t.tree_node().id();
                            if let Some(d) = self.anchored.get(&id) {
                                done = Some(*d);
                                continue;
                            }
                            if !open.insert(id) {
                                // Names itself: opaque.
                                done = Some(hash_of(&[5, hash_text(&format!("{id:?}"))]));
                                continue;
                            }
                            (t, Some(id))
                        }
                        None => {
                            done =
                                Some(hash_of(&[6, hash_text(n.alias_name().unwrap_or_default())]));
                            continue;
                        }
                    }
                } else {
                    (n, None)
                };
                match leaf(n) {
                    Some(d) => {
                        if let Some(id) = memo {
                            open.remove(&id);
                            self.anchored.insert(id, d);
                        }
                        done = Some(d);
                    }
                    None => match frame(n, memo) {
                        Some(f) => stack.push(f),
                        None => {
                            done = Some(0);
                        }
                    },
                }
            }
            if let Some(d) = done.take() {
                match stack.last_mut() {
                    None => return d,
                    Some(f) if f.map => match f.key.take() {
                        None => f.key = Some(d),
                        Some(k) => {
                            f.acc = f.acc.wrapping_add(hash_of(&[k, d]));
                            f.count = f.count.saturating_add(1);
                        }
                    },
                    Some(f) => {
                        f.acc = hash_of(&[f.acc, d]);
                        f.count = f.count.saturating_add(1);
                    }
                }
            }
            let Some(f) = stack.last_mut() else {
                return 0;
            };
            match f.items.next() {
                Some(c) => enter = Some(c),
                None => {
                    let Some(f) = stack.pop() else {
                        return 0;
                    };
                    #[cfg(test)]
                    WORK.with(|w| w.set(w.get().saturating_add(1)));
                    let d = hash_of(&[
                        if f.map { 7 } else { 8 },
                        tag_digest(f.node),
                        f.acc,
                        f.count,
                    ]);
                    if let Some(id) = f.memo {
                        open.remove(&id);
                        self.anchored.insert(id, d);
                    }
                    done = Some(d);
                }
            }
        }
    }

    /// Reads the collection keys of `m`.
    fn read(&mut self, m: NodeRef<'_>) {
        let Some(map) = m.as_mapping() else {
            return;
        };
        for (k, _) in map.iter() {
            if matches!(k.kind(), NodeKind::Sequence | NodeKind::Mapping) {
                let d = self.digest(k);
                let c = self.counts.entry(d).or_insert(0);
                *c = c.saturating_add(1);
            }
        }
    }
}

/// How many mappings' digests a document keeps at once.
const KEEP: usize = 4;

/// What a document keeps between edits for [`CKeys`]: the digests of up to
/// [`KEEP`] mappings (entries move between a few mappings at a time).
#[derive(Default)]
pub(super) struct Cache(pub(super) std::sync::Mutex<Vec<CKeys>>);

impl Cache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<CKeys>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Runs `f` on the digests of mapping `m`'s collection keys, reading
    /// them first when they are not kept for this edit state.
    pub(super) fn with<R>(
        &self,
        m: NodeRef<'_>,
        serial: u64,
        f: impl FnOnce(&mut CKeys) -> R,
    ) -> R {
        let id = m.tree_node().id();
        let mut g = self.lock();
        g.retain(|c| c.serial == serial);
        let at = match g.iter().position(|c| c.mapping == id) {
            Some(at) => at,
            None => {
                let mut c = CKeys::new(id, serial);
                // What the kept mappings learned of anchored nodes holds for
                // this one (the same edit state).
                if let Some(other) = g.first() {
                    c.anchored.clone_from(&other.anchored);
                }
                c.read(m);
                if g.len() >= KEEP {
                    g.remove(0);
                }
                g.push(c);
                g.len().saturating_sub(1)
            }
        };
        match g.get_mut(at) {
            Some(c) => f(c),
            None => f(&mut CKeys::new(id, serial)),
        }
    }

    /// After an edit that changed only the collection keys of `mapping`
    /// (one key `digest` added or removed, defining no anchor): what was
    /// kept for edit state `before` is current for `now`, the mapping's
    /// with that key counted. Anything kept for another state goes.
    pub(super) fn moved(&self, mapping: TreeId, before: u64, now: u64, digest: u64, add: bool) {
        let mut g = self.lock();
        g.retain(|c| c.serial == before);
        for c in g.iter_mut() {
            c.serial = now;
            if c.mapping != mapping {
                continue;
            }
            if add {
                let n = c.counts.entry(digest).or_insert(0);
                *n = n.saturating_add(1);
            } else {
                match c.counts.get_mut(&digest) {
                    Some(n) if *n > 1 => *n = n.saturating_sub(1),
                    Some(_) => {
                        c.counts.remove(&digest);
                    }
                    None => {}
                }
            }
        }
    }

    /// Forgets everything.
    pub(super) fn clear(&self) {
        self.lock().clear();
    }
}

impl CKeys {
    /// Whether a key with `digest` is in the mapping.
    pub(super) fn contains(&self, digest: u64) -> bool {
        self.counts.contains_key(&digest)
    }
}

/// The digest of `k` alone, with no cache (a key taken from a flow pair,
/// whose mapping is not the one the entry goes back into).
pub(super) fn digest_once(k: NodeRef<'_>) -> u64 {
    CKeys::new(k.tree_node().id(), 0).digest(k)
}
