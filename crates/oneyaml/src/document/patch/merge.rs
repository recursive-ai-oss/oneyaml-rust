//! Applying an RFC 7396 merge patch.

use super::apply::{Run, insert_member, remove_node, replace_node};
use super::locate::{find_member, merged_error};
use super::textindex::Texts;
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
    without_nulls_with(patch, &mut |_| {})
}

/// [`without_nulls`], telling a test about each member it looks at (`true` when
/// the member is kept), so the work can be checked to be linear in the size of
/// the patch rather than timed.
#[cfg(test)]
pub(super) fn without_nulls_noting(patch: &Value, note: &mut impl FnMut(bool)) -> Value {
    without_nulls_with(patch, note)
}

fn without_nulls_with(patch: &Value, note: &mut impl FnMut(bool)) -> Value {
    let mut out = patch.clone();
    let mut stack: Vec<&mut Value> = vec![&mut out];
    while let Some(v) = stack.pop() {
        if let Value::Mapping(m) = v {
            // One pass that builds the mapping without its nulls. Removing
            // them one by one from the mapping would move every later member
            // each time.
            let old = std::mem::take(m);
            let mut kept = Mapping::with_capacity(old.len());
            for (k, x) in old {
                let keep = !x.is_null();
                note(keep);
                if keep {
                    kept.insert(k, x);
                }
            }
            *m = kept;
            stack.extend(m.values_mut());
        }
    }
    out
}

/// The value node of `map`'s member `key`, and whether it comes from a mapping
/// merged in by `<<`.
fn member(
    doc: &Document,
    texts: &mut Texts,
    map: NodeId,
    key: &str,
) -> Result<Option<(NodeId, NodeId, bool)>, PatchError> {
    let node = doc.node(map)?;
    let Some(m) = node.as_mapping() else {
        return Err(PatchError::new(
            PatchErrorKind::Edit,
            "internal error: a merge frame is not a mapping",
        ));
    };
    Ok(find_member(doc, m, key, texts)?.map(|m| (m.value.id(), m.key, m.merged)))
}

/// A new frame merging `patch` into the mapping `map`.
fn new_frame<'p>(map: NodeId, name: &'p str, patch: &'p Mapping) -> Frame<'p> {
    Frame {
        map,
        name,
        patch,
        next: 0,
        gone: Vec::new(),
    }
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
    let first = new_frame(root, "", top);
    let mut stack = vec![first];
    while !stack.is_empty() {
        step(doc, &mut run.texts, &mut stack).map_err(|(e, key)| {
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
    texts: &mut Texts,
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
                Some(f) => member(doc, texts, f.map, name),
                None => Ok(None),
            };
            let found = match found {
                Ok(f) => f,
                Err(e) if matches!(e.source_edit(), Some(EditError::StaleNode)) => break,
                Err(e) => return Err((e, Some(name))),
            };
            match found {
                // The member is in a mapping merged in by `<<`; deleting it
                // would delete it for everything else that uses that mapping.
                Some((_, _, true)) => return Err((merged_error(), Some(name))),
                Some((id, key, false)) => {
                    remove_node(doc, id).map_err(|e| (e, Some(name)))?;
                    if let Some(f) = &frame {
                        texts.removed(f.map, name, key);
                    }
                }
                None => {}
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
            let found = member(doc, texts, top.map, name).map_err(fail)?;
            let kind = match found {
                Some((id, _, merged)) => Some((
                    id,
                    merged,
                    target_of(doc.node(id).map_err(|e| fail(e.into()))?),
                )),
                None => None,
            };
            match kind {
                // An empty object changes nothing, wherever the member is.
                Some((_, _, Target::Mapping | Target::AliasToMapping)) if inner.is_empty() => {}
                Some((_, true, _)) => return Err(fail(merged_error())),
                Some((id, false, Target::Mapping)) => {
                    stack.push(new_frame(id, name, inner));
                }
                Some((_, false, Target::AliasToMapping)) => {
                    return Err(fail(refused_alias()));
                }
                Some((id, false, Target::Other)) => {
                    replace_node(doc, id, &without_nulls(value)).map_err(fail)?;
                }
                None => {
                    insert_member(doc, texts, map, name, &without_nulls(value)).map_err(fail)?
                }
            }
        }
        other => {
            let found = member(doc, texts, top.map, name).map_err(fail)?;
            match found {
                Some((_, _, true)) => return Err(fail(merged_error())),
                Some((id, _, false)) => replace_node(doc, id, other),
                None => insert_member(doc, texts, map, name, other),
            }
            .map_err(fail)?;
        }
    }
    Ok(())
}
