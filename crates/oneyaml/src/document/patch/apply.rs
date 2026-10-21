//! Applying RFC 6902 operations.

use super::locate::{Loc, locate, merged_error};
use super::{PatchError, PatchErrorKind, PatchOp, Pointer};
use crate::document::{Document, EditError, NodeId};
use crate::value::Value;

/// What one patch may write, and what it has written so far. The text a patch
/// copies is charged to the document's alias byte budget, because one short
/// operation can name a large value, and a few of them double the document
/// each time.
pub(super) struct Run {
    budget: Option<usize>,
    copied: usize,
    max_document: usize,
}

/// How many times a copied byte counts against the alias byte budget.
const COPY_WEIGHT: usize = 3;

/// The length of the document's text in bytes.
fn document_len(doc: &Document) -> usize {
    doc.span().len()
}

impl Run {
    pub(super) fn new(doc: &Document) -> Self {
        let limits = doc.settings().get_limits();
        let len = document_len(doc);
        Self {
            budget: limits.get_alias_byte_budget(len),
            copied: 0,
            max_document: limits.get_max_document_size(),
        }
    }

    /// Accounts for `bytes` about to be written by copying. A copied value is
    /// held in memory as a value while it is written, which takes several
    /// times its text, so each byte counts [`COPY_WEIGHT`] times against the
    /// alias byte budget: the budget then bounds the memory a patch of copies
    /// uses as well as the text it writes. Aliases written out count the same.
    fn charge(&mut self, bytes: usize) -> Result<(), PatchError> {
        self.copied = self
            .copied
            .saturating_add(bytes.saturating_mul(COPY_WEIGHT));
        match self.budget {
            Some(budget) if self.copied > budget => Err(PatchError::new(
                PatchErrorKind::Limit,
                format!(
                    "the patch copies too much: more than {budget} bytes of budget \
                     (Limits::alias_byte_budget), counting each copied byte {COPY_WEIGHT} times \
                     and aliases written out as copied"
                ),
            )),
            _ => Ok(()),
        }
    }

    /// After a write: the document may not be longer than its size limit.
    pub(super) fn check_size(&self, doc: &Document) -> Result<(), PatchError> {
        if document_len(doc) > self.max_document {
            return Err(PatchError::new(
                PatchErrorKind::Limit,
                format!(
                    "the patched document is longer than {} bytes (Limits::max_document_size)",
                    self.max_document
                ),
            ));
        }
        Ok(())
    }
}

impl From<EditError> for PatchError {
    /// The patch error an edit-layer error stands for: an anchor still in use
    /// is [`PatchErrorKind::RefusedAlias`], a missing node or index is
    /// [`PatchErrorKind::Path`], a limit is [`PatchErrorKind::Limit`], and
    /// what the edit layer cannot do is [`PatchErrorKind::Edit`].
    fn from(e: EditError) -> Self {
        let (kind, message) = match &e {
            EditError::AnchorInUse { anchor, .. } => (
                PatchErrorKind::RefusedAlias,
                format!(
                    "the anchor `{}` is still used by an alias outside what the operation removes",
                    crate::error::segment(anchor)
                ),
            ),
            EditError::NotFound { .. }
            | EditError::NotAMapping { .. }
            | EditError::NotASequence { .. }
            | EditError::IndexOutOfBounds { .. } => (PatchErrorKind::Path, e.to_string()),
            EditError::Rejected { diagnostic } => {
                let limit = (2000..3000).contains(&diagnostic.code.get());
                (
                    if limit {
                        PatchErrorKind::Limit
                    } else {
                        PatchErrorKind::Invalid
                    },
                    e.to_string(),
                )
            }
            _ => (PatchErrorKind::Edit, e.to_string()),
        };
        PatchError::new(kind, message).caused_by(e)
    }
}

/// Applies `ops` in order to `doc`. The caller makes the whole of it atomic.
pub(super) fn apply_ops(doc: &mut Document, ops: &[PatchOp]) -> Result<(), PatchError> {
    let mut run = Run::new(doc);
    for (i, op) in ops.iter().enumerate() {
        apply_op(doc, &mut run, op).map_err(|e| e.in_op(i, op.name()))?;
    }
    Ok(())
}

fn apply_op(doc: &mut Document, run: &mut Run, op: &PatchOp) -> Result<(), PatchError> {
    match op {
        PatchOp::Add { path, value } => {
            let to = Pointer::parse(path)?;
            add(doc, to.tokens(), value).map_err(|e| e.at_default(path))?;
            run.check_size(doc).map_err(|e| e.at_default(path))
        }
        PatchOp::Replace { path, value } => {
            let to = Pointer::parse(path)?;
            replace(doc, to.tokens(), value).map_err(|e| e.at_default(path))?;
            run.check_size(doc).map_err(|e| e.at_default(path))
        }
        PatchOp::Remove { path } => {
            let at = Pointer::parse(path)?;
            remove(doc, at.tokens()).map_err(|e| e.at_default(path))
        }
        PatchOp::Test { path, value } => {
            let at = Pointer::parse(path)?;
            test(doc, at.tokens(), value).map_err(|e| e.at_default(path))
        }
        PatchOp::Copy { from, path } => {
            let source = Pointer::parse(from)?;
            let to = Pointer::parse(path)?;
            copy(doc, run, source.tokens(), to.tokens()).map_err(|e| e.at_default(path))?;
            run.check_size(doc).map_err(|e| e.at_default(path))
        }
        PatchOp::Move { from, path } => {
            let source = Pointer::parse(from)?;
            let to = Pointer::parse(path)?;
            relocate(doc, run, source.tokens(), to.tokens()).map_err(|e| e.at_default(path))?;
            run.check_size(doc).map_err(|e| e.at_default(path))
        }
    }
}

// ---- the edits ----------------------------------------------------------

/// Replaces the node `id`, an alias node included, by `value`.
pub(super) fn replace_node(
    doc: &mut Document,
    id: NodeId,
    value: &Value,
) -> Result<(), PatchError> {
    doc.node_mut(id)?.set(value)?;
    Ok(())
}

/// Adds a new member `key` to the mapping `map`.
pub(super) fn insert_member(
    doc: &mut Document,
    map: NodeId,
    key: &str,
    value: &Value,
) -> Result<(), PatchError> {
    doc.node_mut(map)?.insert(key, value)?;
    Ok(())
}

/// The `add` operation at `tokens`.
fn add(doc: &mut Document, tokens: &[String], value: &Value) -> Result<(), PatchError> {
    let Some(last) = tokens.last() else {
        doc.set_root(value)?;
        return Ok(());
    };
    let Loc {
        node,
        merged,
        parent,
    } = locate(doc, tokens, true)?;
    let parent = parent.ok_or_else(internal)?;
    if parent.is_map {
        if merged {
            // The member exists only in a merged-in mapping: adding it is
            // replacing it there.
            return Err(merged_error());
        }
        return match node {
            // Adding a member that is there replaces its value.
            Some(id) => replace_node(doc, id, value),
            None => insert_member(doc, parent.id, last, value),
        };
    }
    if parent.at_end {
        doc.node_mut(parent.id)?.push(value)?;
        return Ok(());
    }
    if node.is_none() {
        return Err(PatchError::new(
            PatchErrorKind::Path,
            "the index is past the end of the sequence",
        ));
    }
    doc.node_mut(parent.id)?.insert_at(parent.index, value)?;
    Ok(())
}

fn internal() -> PatchError {
    PatchError::new(
        PatchErrorKind::Edit,
        "internal error: a location has no parent",
    )
}

fn replace(doc: &mut Document, tokens: &[String], value: &Value) -> Result<(), PatchError> {
    if tokens.is_empty() {
        doc.set_root(value)?;
        return Ok(());
    }
    let loc = locate(doc, tokens, true)?;
    if loc.merged {
        return Err(merged_error());
    }
    match loc.node {
        Some(id) => replace_node(doc, id, value),
        None => Err(PatchError::new(
            PatchErrorKind::Path,
            "there is nothing to replace at the location",
        )),
    }
}

fn remove(doc: &mut Document, tokens: &[String]) -> Result<(), PatchError> {
    if tokens.is_empty() {
        return Err(PatchError::new(
            PatchErrorKind::Invalid,
            "the whole document cannot be removed; replace it instead",
        ));
    }
    let loc = locate(doc, tokens, true)?;
    if loc.merged {
        return Err(merged_error());
    }
    match loc.node {
        Some(id) => remove_node(doc, id),
        None => Err(PatchError::new(
            PatchErrorKind::Path,
            "there is nothing to remove at the location",
        )),
    }
}

pub(super) fn remove_node(doc: &mut Document, id: NodeId) -> Result<(), PatchError> {
    // The removed text is dropped; what the edit layer returns is not needed.
    let _removed = doc.remove_node(id)?;
    Ok(())
}

/// The value at `tokens`, reading through aliases, and the length in bytes of
/// the source text it was read from.
fn read(doc: &Document, tokens: &[String]) -> Result<(Value, usize), PatchError> {
    let loc = locate(doc, tokens, false)?;
    let Some(id) = loc.node else {
        if tokens.is_empty() {
            return Ok((Value::Null, 0));
        }
        return Err(PatchError::new(
            PatchErrorKind::Path,
            "there is nothing at the location",
        ));
    };
    let node = doc.node(id)?.resolved();
    let size = node.span().len();
    let value = node.deserialize::<Value>().map_err(|e| {
        let kind = if e.kind() == crate::ErrorKind::Limit {
            PatchErrorKind::Limit
        } else {
            PatchErrorKind::Invalid
        };
        PatchError::new(kind, format!("the value cannot be read: {e}"))
    })?;
    Ok((value, size))
}

fn test(doc: &Document, tokens: &[String], expected: &Value) -> Result<(), PatchError> {
    // A value that cannot be read (a limit, a tag that does not resolve) is an
    // error of its own kind; one that reads differently is a failed test.
    let (actual, _) = read(doc, tokens)?;
    if &actual == expected {
        Ok(())
    } else {
        Err(PatchError::new(
            PatchErrorKind::Test,
            "the value at the location is not the expected value",
        ))
    }
}

/// An estimate of the bytes a value takes written out: the length of its text
/// and a few bytes for each node. Iterative: a value can nest as deep as its
/// builder allowed.
fn value_bytes(value: &Value) -> usize {
    let mut total = 0usize;
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        let own = match v {
            Value::Null => 4,
            Value::Bool(_) => 5,
            Value::Number(_) => 8,
            Value::String(s) => s.len().saturating_add(1),
            Value::Sequence(items) => {
                stack.extend(items.iter());
                2
            }
            Value::Mapping(m) => {
                for (k, x) in m.iter() {
                    stack.push(k);
                    stack.push(x);
                }
                2
            }
            Value::Tagged(t) => {
                stack.push(&t.value);
                t.tag.to_string().len()
            }
        };
        total = total.saturating_add(own);
    }
    total
}

fn copy(
    doc: &mut Document,
    run: &mut Run,
    from: &[String],
    to: &[String],
) -> Result<(), PatchError> {
    // Copying a value onto its own mapping key changes nothing. Onto its own
    // sequence index it is an add (RFC 6902 section 4.5): a second copy goes
    // there.
    if from == to && !parent_is_sequence(doc, from)? {
        read(doc, from)?;
        return Ok(());
    }
    // Charge the source text before reading the value, so a refused copy costs
    // little, then what writing aliases out adds to it.
    let size = source_size(doc, from)?;
    run.charge(size)?;
    let (value, _) = read(doc, from)?;
    run.charge(value_bytes(&value).saturating_sub(size))?;
    add(doc, to, &value)
}

/// The length of the source text at `tokens` (the node the pointer names,
/// aliases followed), without reading its value.
fn source_size(doc: &Document, tokens: &[String]) -> Result<usize, PatchError> {
    let loc = locate(doc, tokens, false)?;
    let Some(id) = loc.node else {
        if tokens.is_empty() {
            return Ok(0);
        }
        return Err(PatchError::new(
            PatchErrorKind::Path,
            "there is nothing at the location",
        ));
    };
    Ok(doc.node(id)?.resolved().span().len())
}

/// Whether the location's parent is a sequence (false for the whole document).
fn parent_is_sequence(doc: &Document, tokens: &[String]) -> Result<bool, PatchError> {
    if tokens.is_empty() {
        return Ok(false);
    }
    Ok(locate(doc, tokens, false)?
        .parent
        .is_some_and(|p| !p.is_map))
}

fn relocate(
    doc: &mut Document,
    run: &mut Run,
    from: &[String],
    to: &[String],
) -> Result<(), PatchError> {
    // A value cannot go into itself (RFC 6902 section 4.4).
    if to.len() > from.len() && to.starts_with(from) {
        return Err(PatchError::new(
            PatchErrorKind::Invalid,
            "a value cannot be moved into itself",
        ));
    }
    // The source is removed, so nothing on the way to it may be an alias.
    let source = locate(doc, from, true)?;
    if source.merged {
        return Err(merged_error());
    }
    let Some(source_id) = source.node else {
        if from.is_empty() && to.is_empty() {
            return Ok(());
        }
        return Err(PatchError::new(
            PatchErrorKind::Path,
            "there is nothing to move at the source",
        ));
    };
    if from == to {
        return Ok(());
    }
    if from.is_empty() {
        // The whole document goes somewhere inside itself.
        return Err(PatchError::new(
            PatchErrorKind::Invalid,
            "a value cannot be moved into itself",
        ));
    }
    // Only used to choose the order. A destination that does not exist yet
    // may exist once the source is gone (a sequence index counts from what is
    // left), and add reports a real error.
    let destination = locate(doc, to, true).ok();
    let (value, size) = read(doc, from)?;
    run.charge(value_bytes(&value).saturating_sub(size))?;
    // The RFC removes first. When both places are in mappings and the
    // destination is not above the source, writing first gives the same result
    // without leaving a mapping empty (and so `{}`) in between.
    let above = to.len() <= from.len() && from.starts_with(to);
    let write_first = !above
        && destination
            .as_ref()
            .and_then(|d| d.parent.as_ref())
            .is_some_and(|p| p.is_map)
        && source.parent.as_ref().is_some_and(|p| p.is_map);
    if write_first {
        add(doc, to, &value)?;
        remove_node(doc, source_id)
    } else {
        remove_node(doc, source_id)?;
        add(doc, to, &value)
    }
}
