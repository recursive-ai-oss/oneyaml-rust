//! Applying an RFC 7396 merge patch.

use std::collections::{HashMap, HashSet};

use super::apply::{Run, insert_member, remove_node, replace_node};
use super::locate::{Scan, ambiguous, find_member};
use super::{PatchError, PatchErrorKind};
use crate::document::{Document, EditError, NodeId, NodeKind, NodeRef};
use crate::value::{Mapping, Value};

/// What the target of a merge looks like.
enum Target {
    /// A mapping to merge into.
    Mapping,
    /// An alias of a mapping: merging into it would change the anchored node
    /// for every user of the anchor.
    AliasToMapping,
    /// Anything else, or nothing: replaced.
    Other,
}

fn target_of(node: NodeRef<'_>) -> Target {
    match node.kind() {
        NodeKind::Mapping => Target::Mapping,
        NodeKind::Alias
            if node
                .alias_target()
                .is_some_and(|t| t.kind() == NodeKind::Mapping) =>
        {
            Target::AliasToMapping
        }
        _ => Target::Other,
    }
}

fn refused_alias() -> PatchError {
    PatchError::new(
        PatchErrorKind::RefusedAlias,
        "the target is an alias; merging into it would change the anchored node for every alias of it",
    )
}

/// Every member name of every object in the patch must be a string, as a JSON
/// object's are; and the patch may not nest deeper than the document may.
fn check_shape(doc: &Document, patch: &Value) -> Result<(), PatchError> {
    let max = doc.settings().get_limits().get_max_depth();
    if patch.depth() > max {
        return Err(PatchError::new(
            PatchErrorKind::Limit,
            format!("the patch nests deeper than {max} levels (Limits::max_depth)"),
        ));
    }
    let mut stack = vec![patch];
    while let Some(v) = stack.pop() {
        match v {
            Value::Mapping(m) => {
                for (k, x) in m.iter() {
                    if !matches!(k, Value::String(_)) {
                        return Err(PatchError::new(
                            PatchErrorKind::Invalid,
                            "the member names of a merge patch must be strings",
                        ));
                    }
                    stack.push(x);
                }
            }
            Value::Sequence(items) => stack.extend(items.iter()),
            Value::Tagged(t) => stack.push(&t.value),
            _ => {}
        }
    }
    Ok(())
}

/// The patch with every `null` member of every object removed, which is how a
/// patch merges into a target that is not an object (RFC 7396 section 2).
fn without_nulls(patch: &Value) -> Value {
    let mut out = patch.clone();
    let mut stack: Vec<&mut Value> = vec![&mut out];
    while let Some(v) = stack.pop() {
        if let Value::Mapping(m) = v {
            let nulls: Vec<Value> = m
                .iter()
                .filter(|(_, x)| x.is_null())
                .map(|(k, _)| k.clone())
                .collect();
            for k in &nulls {
                m.remove(k);
            }
            stack.extend(m.values_mut());
        }
    }
    out
}

/// The value node of `map`'s member `key`.
fn member(
    doc: &Document,
    map: NodeId,
    key: &str,
    scan: &mut Scan,
) -> Result<Option<NodeId>, PatchError> {
    let node = doc.node(map)?;
    let Some(m) = node.as_mapping() else {
        return Err(PatchError::new(
            PatchErrorKind::Edit,
            "internal error: a merge frame is not a mapping",
        ));
    };
    Ok(find_member(m, key, scan)?.map(|(_, v)| v.id()))
}

/// What an index of a mapping's members holds for a name.
#[derive(Clone, Copy)]
enum Found {
    One(NodeId),
    /// Two keys have the text.
    Many,
}

/// A patch mapping with more members than this finds its members through one
/// pass over the document's mapping, not one pass each.
const INDEX_MIN: usize = 16;

/// The members of `map` whose key text is a name of `patch`, found in one pass.
fn index_members<'p>(
    doc: &Document,
    map: NodeId,
    patch: &'p Mapping,
    scan: &mut Scan,
) -> Result<HashMap<&'p str, Found>, PatchError> {
    let wanted: HashSet<&'p str> = patch.keys().filter_map(Value::as_str).collect();
    let mut index: HashMap<&'p str, Found> = HashMap::new();
    let node = doc.node(map)?;
    let Some(m) = node.as_mapping() else {
        return Err(PatchError::new(
            PatchErrorKind::Edit,
            "internal error: a merge frame is not a mapping",
        ));
    };
    let mut seen = 0usize;
    for (key, value) in m.iter() {
        seen = seen.saturating_add(1);
        let Some(scalar) = key.as_scalar() else {
            continue;
        };
        let text = scalar.value();
        let Some(name) = wanted.get(text.as_ref()) else {
            continue;
        };
        let found = match index.get(name) {
            Some(_) => Found::Many,
            None => Found::One(value.id()),
        };
        index.insert(name, found);
    }
    scan.spend(seen)?;
    Ok(index)
}

/// A frame's member `key`: from its index when it has one, else by a pass.
fn frame_member(
    doc: &Document,
    frame: &Frame<'_>,
    key: &str,
    scan: &mut Scan,
) -> Result<Option<NodeId>, PatchError> {
    match &frame.index {
        Some(index) => match index.get(key) {
            Some(Found::One(id)) => Ok(Some(*id)),
            Some(Found::Many) => Err(ambiguous(key)),
            None => Ok(None),
        },
        None => member(doc, frame.map, key, scan),
    }
}

/// A new frame merging `patch` into the mapping `map`.
fn new_frame<'p>(
    doc: &Document,
    map: NodeId,
    name: &'p str,
    patch: &'p Mapping,
    scan: &mut Scan,
) -> Result<Frame<'p>, PatchError> {
    let index = if patch.len() > INDEX_MIN {
        Some(index_members(doc, map, patch, scan)?)
    } else {
        None
    };
    Ok(Frame {
        map,
        name,
        patch,
        next: 0,
        gone: Vec::new(),
        index,
    })
}

/// One mapping of the patch being merged into one mapping of the document.
struct Frame<'p> {
    /// The document's mapping.
    map: NodeId,
    /// The name of the member this frame merges into (`""` for the root).
    name: &'p str,
    patch: &'p Mapping,
    next: usize,
    /// The names to delete when the frame ends.
    gone: Vec<&'p str>,
    /// Where the patch's names are in the document's mapping, for a patch
    /// mapping with many members. Nothing in it goes stale: a name is looked up
    /// once, before the edit that changes it, and an added member is not a
    /// name the patch has again.
    index: Option<HashMap<&'p str, Found>>,
}

fn pointer_of(stack: &[Frame<'_>], last: Option<&str>) -> String {
    let mut s = String::new();
    for f in stack.iter().skip(1) {
        push_token(&mut s, f.name);
    }
    if let Some(t) = last {
        push_token(&mut s, t);
    }
    s
}

fn push_token(out: &mut String, token: &str) {
    out.push('/');
    for c in token.chars() {
        match c {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            c => out.push(c),
        }
    }
}

/// Merges `patch` into `doc`. The caller makes the whole of it atomic.
pub(super) fn apply_merge(doc: &mut Document, patch: &Value) -> Result<(), PatchError> {
    check_shape(doc, patch)?;
    let mut run = Run::new(doc);
    let Value::Mapping(top) = patch else {
        // A patch that is not an object is the result.
        doc.set_root(patch)?;
        return run.check_size(doc);
    };
    let root = doc.root().map(|r| (r.id(), target_of(r)));
    let root = match root {
        Some((id, Target::Mapping)) => id,
        Some((_, Target::AliasToMapping)) => {
            return if top.is_empty() {
                Ok(())
            } else {
                Err(refused_alias())
            };
        }
        _ => {
            doc.set_root(without_nulls(patch))?;
            return run.check_size(doc);
        }
    };
    let first = new_frame(doc, root, "", top, &mut run.scan)?;
    let mut stack = vec![first];
    while !stack.is_empty() {
        step(doc, &mut run.scan, &mut stack).map_err(|(e, key)| {
            let pointer = pointer_of(&stack, key);
            e.at_default(&pointer)
        })?;
    }
    run.check_size(doc)
}

/// Does the next piece of work of the innermost frame: merges its next
/// member, or, when it has none left, deletes its `null` members and ends it.
/// The error says which member it was working on.
fn step<'p>(
    doc: &mut Document,
    scan: &mut Scan,
    stack: &mut Vec<Frame<'p>>,
) -> Result<(), (PatchError, Option<&'p str>)> {
    let Some(top) = stack.last_mut() else {
        return Ok(());
    };
    let map = top.map;
    let Some((key, value)) = top.patch.get_index(top.next) else {
        // Deletions come last: a mapping that loses its last member would
        // become `{}` before the new members were added to it.
        let gone = std::mem::take(&mut top.gone);
        let frame = stack.pop();
        for name in gone {
            // A member that is not there is already gone. A mapping that lost
            // its last member and was rewritten as `{}` has no more members.
            let found = match &frame {
                Some(f) => frame_member(doc, f, name, scan),
                None => Ok(None),
            };
            let found = match found {
                Ok(f) => f,
                Err(e) if matches!(e.source_edit(), Some(EditError::StaleNode)) => break,
                Err(e) => return Err((e, Some(name))),
            };
            if let Some(id) = found {
                remove_node(doc, id).map_err(|e| (e, Some(name)))?;
            }
        }
        return Ok(());
    };
    top.next = top.next.saturating_add(1);
    let Value::String(name) = key else {
        return Err((
            PatchError::new(PatchErrorKind::Invalid, "a member name is not a string"),
            None,
        ));
    };
    let name: &'p str = name.as_str();
    let fail = |e: PatchError| (e, Some(name));
    match value {
        Value::Null => top.gone.push(name),
        Value::Mapping(inner) => {
            let found = frame_member(doc, top, name, scan).map_err(fail)?;
            let kind = match found {
                Some(id) => Some((id, target_of(doc.node(id).map_err(|e| fail(e.into()))?))),
                None => None,
            };
            match kind {
                Some((id, Target::Mapping)) => {
                    let frame = new_frame(doc, id, name, inner, scan).map_err(fail)?;
                    stack.push(frame);
                }
                Some((_, Target::AliasToMapping)) => {
                    if !inner.is_empty() {
                        return Err(fail(refused_alias()));
                    }
                }
                Some((id, Target::Other)) => {
                    replace_node(doc, id, &without_nulls(value)).map_err(fail)?;
                }
                None => insert_member(doc, map, name, &without_nulls(value)).map_err(fail)?,
            }
        }
        other => {
            let found = frame_member(doc, top, name, scan).map_err(fail)?;
            match found {
                Some(id) => replace_node(doc, id, other),
                None => insert_member(doc, map, name, other),
            }
            .map_err(fail)?;
        }
    }
    Ok(())
}
