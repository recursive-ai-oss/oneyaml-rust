//! Finding what a pointer names in a document.
//!
//! A token addresses a mapping member by the key's scalar text (the content
//! after quoting and escapes, before the schema gives it a type), so `80`,
//! `'80'` and `"80"` are all the token `80`, and `true` and `0x10` are the
//! tokens `true` and `0x10`. Two keys with the same text make the token
//! ambiguous. A key that is a collection has no scalar text and is not
//! addressable.

use super::{PatchError, PatchErrorKind};
use crate::document::{Document, MappingRef, NodeId, NodeKind, NodeRef};

/// How many mapping entries a patch may look at in all. A pointer token is
/// matched by reading the keys of a mapping, so a patch of many operations
/// on a very wide mapping would otherwise cost the operations times the
/// width.
pub(super) struct Scan {
    used: usize,
    max: usize,
}

impl Scan {
    pub(super) fn new(max: usize) -> Self {
        Self { used: 0, max }
    }

    /// A scan that never refuses, for reads outside a patch.
    pub(super) fn unlimited() -> Self {
        Self::new(usize::MAX)
    }

    /// Accounts for `entries` more entries looked at.
    pub(super) fn spend(&mut self, entries: usize) -> Result<(), PatchError> {
        self.used = self.used.saturating_add(entries);
        if self.used > self.max {
            return Err(PatchError::new(
                PatchErrorKind::Limit,
                "the patch looks at too many mapping entries to find its locations; \
                 use fewer operations on wide mappings",
            ));
        }
        Ok(())
    }
}

/// What a pointer found.
pub(super) struct Loc {
    /// The node at the location, when there is one. An alias node is reported
    /// as itself, not as what it refers to.
    pub node: Option<NodeId>,
    /// The collection the last token stepped into; `None` for the empty
    /// pointer.
    pub parent: Option<Parent>,
}

/// The collection holding a location.
pub(super) struct Parent {
    pub id: NodeId,
    pub is_map: bool,
    /// For a sequence: the index the token names; `usize::MAX` for `-`.
    pub index: usize,
    /// For a sequence: the index is the sequence's length, so an item can be
    /// added there.
    pub at_end: bool,
}

/// A token shortened for a message.
fn shown(token: &str) -> String {
    const KEPT: usize = 48;
    let mut s: String = token.chars().take(KEPT).collect();
    if token.chars().nth(KEPT).is_some() {
        s.push('…');
    }
    format!("{s:?}")
}

fn path_error(message: String) -> PatchError {
    PatchError::new(PatchErrorKind::Path, message)
}

/// The array index a token names: digits only, no leading zero (RFC 6901
/// section 4). `None` for anything else, and for an index too large for
/// `usize`.
fn array_index(token: &str) -> Option<usize> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if token.len() > 1 && token.starts_with('0') {
        return None;
    }
    token.parse::<usize>().ok()
}

/// The member of `map` whose key has the scalar text `token`: `Ok(None)` when
/// none has, an error when two do. Linear in the mapping's width.
pub(super) fn find_member<'d>(
    map: MappingRef<'d>,
    token: &str,
    scan: &mut Scan,
) -> Result<Option<(NodeRef<'d>, NodeRef<'d>)>, PatchError> {
    let mut found = None;
    let mut seen = 0usize;
    for (key, value) in map.iter() {
        seen = seen.saturating_add(1);
        let Some(scalar) = key.as_scalar() else {
            continue;
        };
        if scalar.value() != token {
            continue;
        }
        if found.is_some() {
            return Err(ambiguous(token));
        }
        found = Some((key, value));
    }
    scan.spend(seen)?;
    Ok(found)
}

/// Two keys match `token`.
pub(super) fn ambiguous(token: &str) -> PatchError {
    PatchError::new(
        PatchErrorKind::RefusedAmbiguous,
        format!("two keys match the token {}", shown(token)),
    )
}

/// The node a step goes through: for a write, an alias is refused (it would
/// change every user of its anchor); for a read it is followed.
fn container<'d>(node: NodeRef<'d>, write: bool) -> Result<NodeRef<'d>, PatchError> {
    if node.kind() != NodeKind::Alias {
        return Ok(node);
    }
    if write {
        return Err(PatchError::new(
            PatchErrorKind::RefusedAlias,
            "the location is below an alias; writing there would change the anchored node for every alias of it",
        ));
    }
    node.alias_target()
        .ok_or_else(|| path_error("the location is below an alias that refers to no anchor".into()))
}

/// One step down from `node` by `token`, for the nodes before the last token.
fn descend<'d>(
    node: NodeRef<'d>,
    token: &str,
    write: bool,
    scan: &mut Scan,
) -> Result<NodeRef<'d>, PatchError> {
    let node = container(node, write)?;
    if let Some(map) = node
        .as_mapping()
        .filter(|_| node.kind() == NodeKind::Mapping)
    {
        return match find_member(map, token, scan)? {
            Some((_, value)) => Ok(value),
            None => Err(path_error(format!("no member {}", shown(token)))),
        };
    }
    if let Some(seq) = node
        .as_sequence()
        .filter(|_| node.kind() == NodeKind::Sequence)
    {
        return array_index(token)
            .and_then(|i| seq.get(i))
            .ok_or_else(|| path_error(format!("no item {}", shown(token))));
    }
    Err(path_error(format!(
        "cannot step to {} from a value that is neither a mapping nor a sequence",
        shown(token)
    )))
}

/// Resolves `tokens`. With `write`, nothing on the way may be an alias (the
/// location is a place an edit goes through); the node found last may be one.
/// Without it, aliases are followed on the way.
pub(super) fn locate(
    doc: &Document,
    tokens: &[String],
    write: bool,
    scan: &mut Scan,
) -> Result<Loc, PatchError> {
    let root = doc.root();
    let Some((last, before)) = tokens.split_last() else {
        return Ok(Loc {
            node: root.map(|r| r.id()),
            parent: None,
        });
    };
    let Some(mut cur) = root else {
        return Err(path_error("the document is empty".into()));
    };
    for token in before {
        cur = descend(cur, token, write, scan)?;
    }
    let cur = container(cur, write)?;
    if let Some(map) = cur.as_mapping().filter(|_| cur.kind() == NodeKind::Mapping) {
        let node = find_member(map, last, scan)?.map(|(_, v)| v.id());
        return Ok(Loc {
            node,
            parent: Some(Parent {
                id: cur.id(),
                is_map: true,
                index: 0,
                at_end: false,
            }),
        });
    }
    if let Some(seq) = cur
        .as_sequence()
        .filter(|_| cur.kind() == NodeKind::Sequence)
    {
        if last == "-" {
            return Ok(Loc {
                node: None,
                parent: Some(Parent {
                    id: cur.id(),
                    is_map: false,
                    index: usize::MAX,
                    at_end: true,
                }),
            });
        }
        let Some(index) = array_index(last) else {
            return Err(path_error(format!(
                "{} is not an index of a sequence",
                shown(last)
            )));
        };
        let node = seq.get(index);
        let at_end = node.is_none()
            && match index.checked_sub(1) {
                None => true,
                Some(prev) => seq.get(prev).is_some(),
            };
        return Ok(Loc {
            node: node.map(|n| n.id()),
            parent: Some(Parent {
                id: cur.id(),
                is_map: false,
                index,
                at_end,
            }),
        });
    }
    Err(path_error(format!(
        "cannot step to {} from a value that is neither a mapping nor a sequence",
        shown(last)
    )))
}
