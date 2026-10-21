//! RFC 6902 JSON Patch, RFC 7396 merge patch and RFC 6901 pointers, applied to
//! a lossless [`Document`](crate::Document).
//!
//! A patch is applied as a whole: when any operation fails, the document is
//! byte for byte what it was. The edits go through the same edit layer as
//! [`Document::set_path`](crate::Document::set_path) and friends, so a patch
//! changes only the text of the values it names and keeps comments, layout and
//! the style of everything else.

use crate::Value;
use crate::document::{Document, NodeRef};

mod apply;
mod error;
mod locate;
mod merge;
mod pointer;
#[cfg(test)]
mod tests;
mod textindex;

/// The text indexes that [`Document::get_pointer`] builds, kept until the next
/// edit so that reading a wide mapping's keys is not repeated for every read.
#[derive(Default)]
pub(super) struct ReadCache {
    slot: std::sync::Mutex<Option<textindex::Texts>>,
}

impl ReadCache {
    /// Takes the indexes out (to use them without holding the lock).
    fn take(&self) -> Option<textindex::Texts> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Keeps `texts` for the next read, unless they index more keys than a
    /// document of `len` bytes can hold at one per eight bytes: a cache of
    /// every key of a document of tiny entries would take more memory than it
    /// is worth, and such a read builds its index again.
    fn keep(&self, texts: textindex::Texts, len: usize) {
        if texts.indexed() > (len / 8).saturating_add(1024) {
            return;
        }
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(texts);
    }

    /// Forgets the indexes: the document was edited.
    pub(super) fn clear(&mut self) {
        *self
            .slot
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// The bytes the kept indexes use for what they remember of alias-key
    /// reads, if any are kept (tests).
    #[cfg(test)]
    fn kept_memo_bytes(&self) -> Option<usize> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(textindex::Texts::memo_bytes)
    }

    /// Keys put into indexes on this thread since it began (tests).
    #[cfg(test)]
    fn built_on_thread() -> usize {
        textindex::BUILT_ON_THREAD.with(std::cell::Cell::get)
    }
}

pub use error::{PatchError, PatchErrorKind};
pub use pointer::Pointer;

/// One RFC 6902 operation, with its pointers as text and its operand as a
/// [`Value`].
///
/// The operands of `add`, `replace` and `test` are [`Value`]s, so a patch
/// can be built without JSON text. A number maps to a YAML type by how JSON
/// writes it: an integer has no decimal point or exponent, so `Value::from(1)`
/// and `Value::from(1.0)` differ (and a `test` tells them apart).
///
/// ```
/// use oneyaml::{Document, Patch, PatchOp, Value};
///
/// let mut doc = Document::parse("a: 1\nb: [x]\n").unwrap();
/// let patch = Patch::new(vec![
///     PatchOp::replace("/a", Value::from(2)),
///     PatchOp::add("/b/-", Value::from("w")),
///     PatchOp::copy("/a", "/c"),
/// ]);
/// doc.apply_patch(&patch).unwrap();
/// assert_eq!(doc.to_string(), "a: 2\nb: [x, w]\nc: 2\n");
/// ```
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum PatchOp {
    /// Adds a value at `path`: a new member of a mapping (which replaces the
    /// member when its key is there), or an item inserted at an index of a
    /// sequence (`-` appends).
    Add {
        /// The pointer of the location.
        path: String,
        /// The value to add.
        value: Value,
    },
    /// Removes the value at `path`.
    Remove {
        /// The pointer of the location.
        path: String,
    },
    /// Replaces the value at `path`, which must exist.
    Replace {
        /// The pointer of the location.
        path: String,
        /// The new value.
        value: Value,
    },
    /// Removes the value at `from` and adds it at `path`.
    Move {
        /// The pointer the value is taken from.
        from: String,
        /// The pointer it is added at.
        path: String,
    },
    /// Adds a copy of the value at `from` at `path`.
    Copy {
        /// The pointer the value is read from.
        from: String,
        /// The pointer it is added at.
        path: String,
    },
    /// Fails the patch unless the value at `path` equals `value`, by the YAML
    /// data model under the document's schema.
    Test {
        /// The pointer of the location.
        path: String,
        /// The value expected there.
        value: Value,
    },
}

impl PatchOp {
    /// An `add` operation.
    #[must_use]
    pub fn add(path: impl Into<String>, value: Value) -> Self {
        Self::Add {
            path: path.into(),
            value,
        }
    }

    /// A `remove` operation.
    #[must_use]
    pub fn remove(path: impl Into<String>) -> Self {
        Self::Remove { path: path.into() }
    }

    /// A `replace` operation.
    #[must_use]
    pub fn replace(path: impl Into<String>, value: Value) -> Self {
        Self::Replace {
            path: path.into(),
            value,
        }
    }

    /// A `move` operation.
    #[must_use]
    pub fn move_value(from: impl Into<String>, path: impl Into<String>) -> Self {
        Self::Move {
            from: from.into(),
            path: path.into(),
        }
    }

    /// A `copy` operation.
    #[must_use]
    pub fn copy(from: impl Into<String>, path: impl Into<String>) -> Self {
        Self::Copy {
            from: from.into(),
            path: path.into(),
        }
    }

    /// A `test` operation.
    #[must_use]
    pub fn test(path: impl Into<String>, value: Value) -> Self {
        Self::Test {
            path: path.into(),
            value,
        }
    }

    /// The operation's name as RFC 6902 writes it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Add { .. } => "add",
            Self::Remove { .. } => "remove",
            Self::Replace { .. } => "replace",
            Self::Move { .. } => "move",
            Self::Copy { .. } => "copy",
            Self::Test { .. } => "test",
        }
    }
}

/// An RFC 6902 patch: a list of [`PatchOp`]s applied in order.
///
/// ```
/// use oneyaml::{Document, Patch};
///
/// let patch = Patch::from_json(
///     r#"[{"op": "add", "path": "/tags/-", "value": "new"},
///         {"op": "replace", "path": "/port", "value": 9090}]"#,
/// )
/// .unwrap();
/// let mut doc = Document::parse("port: 80  # http\ntags:\n  - old\n").unwrap();
/// doc.apply_patch(&patch).unwrap();
/// assert_eq!(doc.to_string(), "port: 9090  # http\ntags:\n  - old\n  - new\n");
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    ops: Vec<PatchOp>,
}

impl Patch {
    /// A patch of these operations.
    #[must_use]
    pub fn new(ops: Vec<PatchOp>) -> Self {
        Self { ops }
    }

    /// The operations, in order.
    ///
    /// ```
    /// use oneyaml::{Patch, PatchOp, Value};
    ///
    /// let patch = Patch::new(vec![
    ///     PatchOp::test("/a", Value::from(1)),
    ///     PatchOp::move_value("/a", "/b"),
    /// ]);
    /// let names: Vec<&str> = patch.ops().iter().map(PatchOp::name).collect();
    /// assert_eq!(names, ["test", "move"]);
    /// ```
    #[must_use]
    pub fn ops(&self) -> &[PatchOp] {
        &self.ops
    }

    /// Reads a patch from JSON text: an array of objects, each with an `op`
    /// and a `path` (a `from` for `move` and `copy`, a `value` for `add`,
    /// `replace` and `test`). Members an operation does not use are ignored.
    ///
    /// The text is read with oneyaml's own reader (JSON is YAML), so a number
    /// is an integer when it has no decimal point or exponent and a float when
    /// it has either, and the reader's limits apply. Text that is YAML but not
    /// JSON, such as `[{op: remove, path: /a}]`, is read too.
    ///
    /// # Errors
    ///
    /// [`PatchErrorKind::Invalid`] when the text cannot be read, is not an
    /// array of operations, names an operation that does not exist, or
    /// leaves out a member the operation needs (the error names the
    /// operation's index); [`PatchErrorKind::Limit`] when the reader's limits
    /// are exceeded.
    pub fn from_json(text: &str) -> Result<Self, PatchError> {
        let value = read_value(text)?;
        Self::from_value(value)
    }

    /// Reads a patch from a [`Value`] that holds an array of operation
    /// objects, as [`Patch::from_json`] does from text.
    ///
    /// # Errors
    ///
    /// [`PatchErrorKind::Invalid`] as for [`Patch::from_json`].
    pub fn from_value(value: Value) -> Result<Self, PatchError> {
        let Value::Sequence(items) = value else {
            return Err(PatchError::new(
                PatchErrorKind::Invalid,
                "a patch is an array of operations",
            ));
        };
        let mut ops = Vec::with_capacity(items.len());
        for (i, item) in items.into_vec().into_iter().enumerate() {
            ops.push(op_from_value(item).map_err(|(e, name)| match name {
                Some(n) => e.in_op(i, n),
                None => e.at_index(i),
            })?);
        }
        Ok(Self { ops })
    }
}

impl From<Vec<PatchOp>> for Patch {
    fn from(ops: Vec<PatchOp>) -> Self {
        Self { ops }
    }
}

impl FromIterator<PatchOp> for Patch {
    fn from_iter<I: IntoIterator<Item = PatchOp>>(iter: I) -> Self {
        Self {
            ops: iter.into_iter().collect(),
        }
    }
}

/// An RFC 7396 merge patch: a [`Value`] merged into the document. A member
/// that is `null` deletes the member of the same name, an object member is
/// merged into the mapping it meets, and any other value, a sequence
/// included, replaces what is there. A patch that is not an object replaces
/// the whole document.
///
/// ```
/// use oneyaml::{Document, MergePatch};
///
/// let patch = MergePatch::from_json(r#"{"a": {"b": "d", "c": null}, "z": [1, 2]}"#).unwrap();
/// let mut doc = Document::parse("a:\n  b: c  # kept comment\n  c: gone\n").unwrap();
/// doc.apply_merge_patch(&patch).unwrap();
/// assert_eq!(doc.to_string(), "a:\n  b: d  # kept comment\nz:\n  - 1\n  - 2\n");
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct MergePatch {
    value: Value,
}

impl MergePatch {
    /// A merge patch from a value. The names of its members must be strings
    /// (checked when the patch is applied, as a JSON object's are).
    #[must_use]
    pub fn new(value: Value) -> Self {
        Self { value }
    }

    /// Reads a merge patch from JSON text (with oneyaml's own reader, as
    /// [`Patch::from_json`] does).
    ///
    /// # Errors
    ///
    /// [`PatchErrorKind::Invalid`] when the text cannot be read;
    /// [`PatchErrorKind::Limit`] when the reader's limits are exceeded.
    pub fn from_json(text: &str) -> Result<Self, PatchError> {
        Ok(Self {
            value: read_value(text)?,
        })
    }

    /// The patch value.
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }
}

impl From<Value> for MergePatch {
    fn from(value: Value) -> Self {
        Self { value }
    }
}

/// The text read as one value.
fn read_value(text: &str) -> Result<Value, PatchError> {
    crate::from_str::<Value>(text).map_err(|e| {
        let kind = if e.kind() == crate::ErrorKind::Limit {
            PatchErrorKind::Limit
        } else {
            PatchErrorKind::Invalid
        };
        PatchError::new(kind, format!("the patch text cannot be read: {e}"))
    })
}

type OpResult = Result<PatchOp, (PatchError, Option<&'static str>)>;

fn op_from_value(item: Value) -> OpResult {
    let invalid = |m: &str, name: Option<&'static str>| {
        (
            PatchError::new(PatchErrorKind::Invalid, m.to_string()),
            name,
        )
    };
    let Value::Mapping(mut m) = item else {
        return Err(invalid("an operation is an object", None));
    };
    let mut take = |name: &str| m.remove(&Value::from(name));
    let name = match take("op") {
        Some(Value::String(s)) => s,
        Some(_) => return Err(invalid("`op` must be a string", None)),
        None => return Err(invalid("an operation has no `op` member", None)),
    };
    let known: Option<&'static str> = match name.as_str() {
        "add" => Some("add"),
        "remove" => Some("remove"),
        "replace" => Some("replace"),
        "move" => Some("move"),
        "copy" => Some("copy"),
        "test" => Some("test"),
        _ => None,
    };
    let Some(op) = known else {
        return Err(invalid(
            &format!("unknown operation {}", shown(&name)),
            None,
        ));
    };
    let mut text = |member: &str| -> Result<String, (PatchError, Option<&'static str>)> {
        match take(member) {
            Some(Value::String(s)) => Ok(s),
            Some(_) => Err(invalid(&format!("`{member}` must be a string"), Some(op))),
            None => Err(invalid(
                &format!("the {op} operation has no `{member}` member"),
                Some(op),
            )),
        }
    };
    let path = text("path")?;
    let result = match op {
        "remove" => PatchOp::Remove { path },
        "move" => PatchOp::Move {
            from: text("from")?,
            path,
        },
        "copy" => PatchOp::Copy {
            from: text("from")?,
            path,
        },
        _ => {
            let Some(value) = take("value") else {
                return Err(invalid(
                    &format!("the {op} operation has no `value` member"),
                    Some(op),
                ));
            };
            match op {
                "add" => PatchOp::Add { path, value },
                "replace" => PatchOp::Replace { path, value },
                _ => PatchOp::Test { path, value },
            }
        }
    };
    Ok(result)
}

/// A name from the patch text, bounded for a message.
fn shown(name: &str) -> String {
    const KEPT: usize = 40;
    let mut s: String = name.chars().take(KEPT).collect();
    if name.chars().nth(KEPT).is_some() {
        s.push('…');
    }
    format!("{s:?}")
}

impl Document {
    /// Applies an RFC 6902 patch to this document, all or nothing: when an
    /// operation fails, including a `test` that does not match, the document
    /// is exactly as it was and the error says which operation failed and why.
    ///
    /// Operations are applied in order, each seeing the result of those before
    /// it.
    ///
    /// # Pointers
    ///
    /// A pointer token names a mapping member by the scalar text of its key:
    /// `/80` finds the key `80`, `'80'` and `"80"` alike, and `/true` finds the
    /// key `true`, whatever tag a key carries. An alias key has the text of the
    /// node it names. A key that is a collection cannot be addressed. Two keys
    /// with the same text are refused as ambiguous
    /// ([`PatchErrorKind::RefusedAmbiguous`]), and so is a key that a later
    /// duplicate of the same meaning overrides (with `DuplicateKeys::LastWins`,
    /// `16: a` followed by `0x10: b` means `{16: b}`, so `/16` is not read as
    /// `a`). With `Settings::merge_keys` on, the keys merged in by `<<` are
    /// found as [`Document::get_path`] finds them, own keys first.
    ///
    /// Keys are found through an index from the text of each key of a mapping to
    /// its keys, built the first time a patch looks at the mapping and kept up
    /// to date by the patch's own edits, so a lookup costs a hash lookup however
    /// wide the mapping is, and what a token names never depends on the
    /// mapping's width. Building it reads the mapping's keys once per patch.
    /// The text of an alias key is read once per patch too, and read again only
    /// if the patch removes or replaces the node it names or the alias comes to
    /// name another; the bytes read are charged to the document's alias byte
    /// budget ([`Limits::alias_byte_budget`](crate::Limits::alias_byte_budget)),
    /// and so are the bytes of the alias keys of a mapping that an `add` into
    /// it, a lookup that must compare a key with every alias key (with
    /// `DuplicateKeys::LastWins`), or a search of the mappings it merges in
    /// has to read afresh. Looking at each alias key and merged mapping of a
    /// lookup is counted in steps, and a patch that takes more than about a
    /// million of them, plus twice the document's length in bytes, fails with
    /// [`PatchErrorKind::Limit`] (a file that merges a handful of mappings uses
    /// a few per operation). `Limits::unlimited()` removes both bounds.
    ///
    /// Reads (`test`, the `from` of `copy`) follow aliases. A write below an
    /// alias is refused with [`PatchErrorKind::RefusedAlias`], and so is a write
    /// that lands on a key merged in by `<<` (it belongs to the merged mapping,
    /// which everything else that merges it shares): `add`, `replace`,
    /// `remove` and the source of `move` all refuse it. Replacing the alias
    /// node itself is allowed, and removing or moving an anchored node that an
    /// alias still refers to is refused. `add` writes new members at the end of
    /// their mapping in the style of their siblings, and what a patch does not
    /// name stays byte for byte. The empty pointer is the whole document:
    /// `replace` and `add` set the root and keep the document's markers and
    /// header and footer comments.
    ///
    /// # `test`
    ///
    /// `test` compares by the YAML data model under the Core schema, which
    /// departs from RFC 6902 section 4.6: `1` is not `1.0`, and `0x1F` is `31`.
    /// A mapping key in a compared value matches a JSON object key only if it
    /// is a string under the Core schema, so the integer key `1` does not match
    /// `"1"`. (A pointer token, above, matches by text: `/1` addresses the key
    /// `1` too.)
    ///
    /// # Limits
    ///
    /// `copy` and `move` write the value they read: an alias inside it is
    /// written out as the value it stands for. The bytes a patch copies are
    /// charged to the document's alias byte budget
    /// ([`Limits::alias_byte_budget`](crate::Limits::alias_byte_budget), for a
    /// document of this length; by default the larger of ten times its length
    /// and 64 MiB), **three times each**, because a copied value is held in
    /// memory as a value while it is written and takes several times its text:
    /// the budget then bounds the memory a patch of copies uses as well as the
    /// text it writes. A `copy` charges the length of the text it reads, and a
    /// `copy` or `move` that writes aliases out also charges the size of the
    /// written-out value beyond that text, estimated as the length of its
    /// strings plus a few bytes per other node, at the same weight. At the
    /// default limits a patch can therefore copy about 21 MiB of text in all
    /// (a third of 64 MiB; a third of ten times the document when that is
    /// larger), and a patch that goes over fails with a limit error and
    /// changes nothing. After every write the document may not be longer than
    /// `Limits::max_document_size`, and a value nested deeper than
    /// `Limits::max_depth` is refused.
    ///
    /// # Atomicity
    ///
    /// The operations run inside one edit group of the syntax tree: every
    /// accepted edit records an undo entry, and a failure undoes them latest
    /// first. No copy of the document is made, so a patch costs what its edits
    /// cost plus one small record per edit, and a failed patch costs the same
    /// again to undo. A patch of `test` operations only reads.
    ///
    /// ```
    /// use oneyaml::{Document, Patch, PatchErrorKind};
    ///
    /// let mut doc = Document::parse("a: 1  # one\nb: 2\n").unwrap();
    /// let patch = Patch::from_json(
    ///     r#"[{"op": "replace", "path": "/a", "value": 10},
    ///         {"op": "add", "path": "/c", "value": [1, 2]}]"#,
    /// )
    /// .unwrap();
    /// doc.apply_patch(&patch).unwrap();
    /// assert_eq!(doc.to_string(), "a: 10  # one\nb: 2\nc:\n  - 1\n  - 2\n");
    ///
    /// // A later failure undoes the earlier edits.
    /// let bad = Patch::from_json(
    ///     r#"[{"op": "remove", "path": "/a"}, {"op": "remove", "path": "/zz"}]"#,
    /// )
    /// .unwrap();
    /// let before = doc.to_string();
    /// assert_eq!(doc.apply_patch(&bad).unwrap_err().kind(), PatchErrorKind::Path);
    /// assert_eq!(doc.to_string(), before);
    /// ```
    ///
    /// # Errors
    ///
    /// A [`PatchError`] of the kind that says why: [`PatchErrorKind::Path`],
    /// [`Test`](PatchErrorKind::Test), [`Invalid`](PatchErrorKind::Invalid),
    /// [`RefusedAlias`](PatchErrorKind::RefusedAlias),
    /// [`RefusedAmbiguous`](PatchErrorKind::RefusedAmbiguous),
    /// [`Limit`](PatchErrorKind::Limit) or [`Edit`](PatchErrorKind::Edit).
    pub fn apply_patch(&mut self, patch: &Patch) -> Result<(), PatchError> {
        if patch
            .ops()
            .iter()
            .all(|op| matches!(op, PatchOp::Test { .. }))
        {
            // Nothing is written, so nothing needs undoing (and a document
            // that shares its tree is not copied).
            return apply::apply_ops(self, patch.ops());
        }
        self.prepare()?;
        self.atomic(|d| apply::apply_ops(d, patch.ops()))
    }

    /// Applies an RFC 7396 merge patch to this document, all or nothing.
    ///
    /// A `null` member deletes the member of the same name (deletions in a
    /// mapping are applied after its other members, and deleting what is not
    /// there is not an error); an object member is merged into the mapping it
    /// meets, or, where there is none, written whole without its `null`
    /// members; any other value, a sequence included, replaces what is there.
    /// A patch that is not an object replaces the document's root. Merging a
    /// non-empty object into an alias is refused
    /// ([`PatchErrorKind::RefusedAlias`]); merging an empty one changes
    /// nothing. The names of the patch's members must be strings. Members are
    /// found by the scalar text of their keys, as pointer tokens are (see
    /// [`Document::apply_patch`]); with `merge_keys` on, a patch that would set
    /// or delete a member that exists only in a mapping merged in by `<<` is
    /// refused with [`PatchErrorKind::RefusedAlias`], because the mapping is
    /// shared. Deleting a name that no mapping has is not an error.
    ///
    /// Members the patch adds go at the end of their mapping in the style of
    /// their siblings; a `null` in the document stays unless the patch names
    /// it; comments and layout outside the changed values stay.
    ///
    /// ```
    /// use oneyaml::{Document, MergePatch};
    ///
    /// let mut doc = Document::parse("# config\n\na: 1\nb: {x: 1}\n").unwrap();
    /// let patch = MergePatch::from_json(r#"{"a": null, "b": {"z": 2}}"#).unwrap();
    /// doc.apply_merge_patch(&patch).unwrap();
    /// assert_eq!(doc.to_string(), "# config\n\nb: {x: 1, z: 2}\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Document::apply_patch`]; the error names no operation.
    pub fn apply_merge_patch(&mut self, patch: &MergePatch) -> Result<(), PatchError> {
        self.prepare()?;
        self.atomic(|d| merge::apply_merge(d, patch.value()))
    }

    /// The node a pointer names, for reading. Aliases are followed, on the
    /// way and at the end (the node returned is the anchored one).
    ///
    /// The first read after an edit builds the key index of each mapping on its
    /// way (once, O(its width)); the indexes are kept until the document is next
    /// edited, so further reads cost a hash lookup per token. A document whose
    /// entries are so short that its indexes would take more memory than one
    /// key per eight bytes of text does not keep them.
    ///
    /// ```
    /// use oneyaml::Document;
    ///
    /// let doc = Document::parse("a/b: {\"1\": [x, y]}\ncopy: &c [z]\nalias: *c\n").unwrap();
    /// let node = doc.get_pointer("/a~1b/1/1").unwrap();
    /// assert_eq!(node.as_str().as_deref(), Some("y"));
    /// assert_eq!(doc.get_pointer("/alias/0").unwrap().as_str().as_deref(), Some("z"));
    /// assert!(doc.get_pointer("/nope").is_err());
    /// ```
    ///
    /// # Errors
    ///
    /// [`PatchErrorKind::Invalid`] for text that is not a pointer;
    /// [`PatchErrorKind::Path`] when it names nothing;
    /// [`PatchErrorKind::RefusedAmbiguous`] when two keys match a token.
    pub fn get_pointer(&self, pointer: &str) -> Result<NodeRef<'_>, PatchError> {
        self.get_pointer_tokens(&Pointer::parse(pointer)?)
            .map_err(|e| e.at_default(pointer))
    }

    /// [`Document::get_pointer`] for a pointer already read.
    ///
    /// # Errors
    ///
    /// [`PatchErrorKind::Path`] and [`PatchErrorKind::RefusedAmbiguous`], as
    /// for [`Document::get_pointer`].
    pub fn get_pointer_tokens(&self, pointer: &Pointer) -> Result<NodeRef<'_>, PatchError> {
        // The indexes of the mappings on the way are kept until the document is
        // edited, so a run of reads builds each index once.
        let mut texts = self
            .shared
            .pointer_reads
            .take()
            .unwrap_or_else(textindex::Texts::unlimited);
        let loc = locate::locate(self, pointer.tokens(), false, &mut texts);
        self.shared.pointer_reads.keep(texts, self.span().len());
        let loc = loc?;
        let id = loc.node.ok_or_else(|| {
            PatchError::new(PatchErrorKind::Path, "there is nothing at the location")
        })?;
        Ok(self.node(id)?.resolved())
    }
}
