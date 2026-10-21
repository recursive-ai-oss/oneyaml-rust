//! Finding what a pointer names in a document.
//!
//! A token addresses a mapping member by the key's scalar text (the content
//! after quoting and escapes, before the schema gives it a type), so `80`,
//! `'80'` and `"80"` are all the token `80`, and `true` and `0x10` are the
//! tokens `true` and `0x10`. Two keys with the same text make the token
//! ambiguous. A key that is a collection has no scalar text and is not
//! addressable. With `merge_keys` on, the keys merged in by `<<` are found
//! too, as `Document::get_path` finds them; they belong to the merged
//! mapping, so a patch cannot change them (see `Loc::merged`).

use super::textindex::{Hit, Texts};
use super::{PatchError, PatchErrorKind};
use crate::document::{Document, MappingRef, NodeId, NodeKind, NodeRef};

/// What a pointer found.
pub(super) struct Loc {
    /// The node at the location, when there is one. An alias node is reported
    /// as itself, not as what it refers to.
    pub node: Option<NodeId>,
    /// For a member of a mapping, its key.
    pub key: Option<NodeId>,
    /// The node is the value of a key merged in by `<<`, not of one of the
    /// parent mapping's own keys. It belongs to the mapping that is merged
    /// in, which every user of that mapping shares, so it is read but not
    /// written.
    pub merged: bool,
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
/// none has, an error when two do.
pub(super) fn find_member<'d>(
    doc: &'d Document,
    map: MappingRef<'d>,
    token: &str,
    texts: &mut Texts,
) -> Result<Option<Member<'d>>, PatchError> {
    match texts.find(doc, map, token)? {
        Hit::Absent => Ok(None),
        Hit::Ambiguous => Err(ambiguous(token)),
        Hit::One { key, value, merged } => Ok(Some(Member { key, value, merged })),
    }
}

/// A member found by its key's text.
pub(super) struct Member<'d> {
    pub key: NodeId,
    pub value: NodeRef<'d>,
    pub merged: bool,
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

/// The refusal of a write that would land on a key merged in by `<<`.
pub(super) fn merged_error() -> PatchError {
    PatchError::new(
        PatchErrorKind::RefusedAlias,
        "the member comes from a mapping merged in with `<<`: writing it would change that \
         mapping for everything else that merges it, and a patch does not shadow it with a \
         key of its own",
    )
}

/// One step down from `node` by `token`, for the nodes before the last token.
fn descend<'d>(
    doc: &'d Document,
    node: NodeRef<'d>,
    token: &str,
    write: bool,
    texts: &mut Texts,
) -> Result<NodeRef<'d>, PatchError> {
    let node = container(node, write)?;
    if let Some(map) = node
        .as_mapping()
        .filter(|_| node.kind() == NodeKind::Mapping)
    {
        return match find_member(doc, map, token, texts)? {
            // A write goes into the node it reaches, and a merged-in node is
            // shared with the other users of the mapping it comes from.
            Some(m) if write && m.merged => Err(merged_error()),
            Some(m) => Ok(m.value),
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
    texts: &mut Texts,
) -> Result<Loc, PatchError> {
    let root = doc.root();
    let Some((last, before)) = tokens.split_last() else {
        return Ok(Loc {
            node: root.map(|r| r.id()),
            key: None,
            merged: false,
            parent: None,
        });
    };
    let Some(mut cur) = root else {
        return Err(path_error("the document is empty".into()));
    };
    for token in before {
        cur = descend(doc, cur, token, write, texts)?;
    }
    let cur = container(cur, write)?;
    if let Some(map) = cur.as_mapping().filter(|_| cur.kind() == NodeKind::Mapping) {
        let found = find_member(doc, map, last, texts)?;
        return Ok(Loc {
            node: found.as_ref().map(|m| m.value.id()),
            key: found.as_ref().map(|m| m.key),
            merged: found.is_some_and(|m| m.merged),
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
                key: None,
                merged: false,
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
            key: None,
            merged: false,
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
