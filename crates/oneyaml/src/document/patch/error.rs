//! Why a patch failed.

use core::fmt;

use crate::EditError;

/// What kind of failure a [`PatchError`] is.
///
/// The kinds other than [`Edit`](PatchErrorKind::Edit) say what is wrong with
/// the patch or the location; a caller is expected to act on them. Each kind
/// has a stable [`name`](PatchErrorKind::name).
///
/// ```
/// use oneyaml::{Document, Patch, PatchErrorKind};
///
/// let mut doc = Document::parse("a: 1\n").unwrap();
/// let patch = Patch::from_json(r#"[{"op": "test", "path": "/a", "value": 2}]"#).unwrap();
/// let err = doc.apply_patch(&patch).unwrap_err();
/// assert_eq!(err.kind(), PatchErrorKind::Test);
/// assert_eq!(err.kind().name(), "patch-test");
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PatchErrorKind {
    /// A pointer names nothing: a missing key or index, a step into a scalar,
    /// a document index past the end of the stream. (`patch-path`)
    Path,
    /// A `test` operation found a different value. (`patch-test`)
    Test,
    /// The patch itself is not well formed: an unknown operation, a missing
    /// member, a malformed pointer, a move into the moved value's own
    /// descendant. (`patch-invalid`)
    Invalid,
    /// The operation would write through an alias, or remove an anchored node
    /// that an alias still refers to, or merge into an alias, or change a key
    /// that exists only in a mapping merged in by `<<`. (`refused-alias`)
    RefusedAlias,
    /// Two keys of a mapping have the same text as one pointer token, or a key is
    /// overridden by a later key with the same meaning. (`refused-ambiguous`)
    RefusedAmbiguous,
    /// A limit was reached: `Limits::max_depth` (a value or a patch nested too
    /// deeply), `Limits::alias_budget` (aliases written out by a copy or move,
    /// counted in nodes), `Limits::alias_byte_budget` (the bytes a patch
    /// copies, aliases written out included), `Limits::max_document_size`
    /// (the document after a write), or the patch's bound on the alias keys
    /// and merged mappings it searches to find its locations. (`limit`)
    Limit,
    /// The edit layer refused the edit the operation asks for. This happens
    /// when the result would not read back as the edited value, when a comment
    /// would change owner, when the edit is one the edit layer does not do (see
    /// [`EditError`](crate::EditError)), and when part of the document is
    /// read-only because it has parse errors. The patch is valid and the
    /// location exists; the document's text cannot take the change. Nothing
    /// was changed. (`patch-edit`)
    Edit,
}

impl PatchErrorKind {
    /// The kind's stable name: `patch-path`, `patch-test`, `patch-invalid`,
    /// `refused-alias`, `refused-ambiguous`, `limit` or `patch-edit`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            PatchErrorKind::Path => "patch-path",
            PatchErrorKind::Test => "patch-test",
            PatchErrorKind::Invalid => "patch-invalid",
            PatchErrorKind::RefusedAlias => "refused-alias",
            PatchErrorKind::RefusedAmbiguous => "refused-ambiguous",
            PatchErrorKind::Limit => "limit",
            PatchErrorKind::Edit => "patch-edit",
        }
    }
}

/// How much of a pointer an error keeps. A pointer in a patch can be any
/// length, and an error should not copy all of it.
const POINTER_KEPT: usize = 200;

/// A failed patch. When it comes from applying a patch, the document is
/// exactly as it was before the call.
///
/// The error says which kind of failure it is ([`PatchError::kind`]), which
/// operation failed ([`PatchError::op_index`], [`PatchError::operation`]) and
/// where ([`PatchError::pointer`]).
///
/// ```
/// use oneyaml::{Document, Patch, PatchErrorKind};
///
/// let mut doc = Document::parse("a: 1\nb: 2\n").unwrap();
/// let patch = Patch::from_json(
///     r#"[{"op": "replace", "path": "/a", "value": 9},
///         {"op": "remove", "path": "/zz"}]"#,
/// )
/// .unwrap();
/// let err = doc.apply_patch(&patch).unwrap_err();
/// assert_eq!(err.kind(), PatchErrorKind::Path);
/// assert_eq!(err.op_index(), Some(1));
/// assert_eq!(err.operation(), Some("remove"));
/// assert_eq!(err.pointer(), Some("/zz"));
/// assert_eq!(doc.to_string(), "a: 1\nb: 2\n"); // the first operation was undone
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchError(Box<Inner>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Inner {
    kind: PatchErrorKind,
    index: Option<usize>,
    op: Option<&'static str>,
    pointer: Option<String>,
    message: String,
    source: Option<EditError>,
}

impl PatchError {
    pub(crate) fn new(kind: PatchErrorKind, message: impl Into<String>) -> Self {
        Self(Box::new(Inner {
            kind,
            index: None,
            op: None,
            pointer: None,
            message: message.into(),
            source: None,
        }))
    }

    /// Names the pointer the failure is about (shortened when very long).
    pub(crate) fn at(mut self, pointer: &str) -> Self {
        self.0.pointer = Some(shorten(pointer));
        self
    }

    /// Names the operation the failure belongs to.
    pub(crate) fn in_op(mut self, index: usize, op: &'static str) -> Self {
        self.0.index = Some(index);
        self.0.op = Some(op);
        self
    }

    /// Names the pointer unless an inner step already did.
    pub(crate) fn at_default(self, pointer: &str) -> Self {
        if self.0.pointer.is_some() {
            self
        } else {
            self.at(pointer)
        }
    }

    /// Names the operation by its index only (its name is not known).
    pub(crate) fn at_index(mut self, index: usize) -> Self {
        self.0.index = Some(index);
        self
    }

    /// The edit-layer error this one comes from, if any.
    pub(crate) fn source_edit(&self) -> Option<&EditError> {
        self.0.source.as_ref()
    }

    pub(crate) fn caused_by(mut self, source: EditError) -> Self {
        self.0.source = Some(source);
        self
    }

    /// The kind of failure.
    #[must_use]
    pub fn kind(&self) -> PatchErrorKind {
        self.0.kind
    }

    /// The index of the failing operation in the patch; `None` when the
    /// failure is not about one operation (a merge patch, a document index
    /// past the end of a stream).
    #[must_use]
    pub fn op_index(&self) -> Option<usize> {
        self.0.index
    }

    /// The failing operation's name (`add`, `remove`, `replace`, `move`,
    /// `copy` or `test`).
    #[must_use]
    pub fn operation(&self) -> Option<&str> {
        self.0.op
    }

    /// The pointer the failure is about, as written in the patch. A pointer
    /// longer than 200 bytes is cut there and ends in `…`.
    #[must_use]
    pub fn pointer(&self) -> Option<&str> {
        self.0.pointer.as_deref()
    }

    /// A one-line explanation, without the operation or the pointer (which
    /// [`Display`](fmt::Display) adds).
    ///
    /// ```
    /// use oneyaml::{Document, Patch};
    ///
    /// let mut doc = Document::parse("a: 1\n").unwrap();
    /// let patch = Patch::from_json(r#"[{"op": "remove", "path": "/nope"}]"#).unwrap();
    /// let error = doc.apply_patch(&patch).unwrap_err();
    /// assert!(!error.message().is_empty());
    /// assert!(error.to_string().contains(error.message()));
    /// ```
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0.message
    }
}

fn shorten(pointer: &str) -> String {
    if pointer.len() <= POINTER_KEPT {
        return pointer.to_string();
    }
    let mut end = POINTER_KEPT;
    while end > 0 && !pointer.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut s = pointer.get(..end).unwrap_or("").to_string();
    s.push('…');
    s
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let e = &*self.0;
        write!(f, "{}", e.kind.name())?;
        if let (Some(i), Some(op)) = (e.index, e.op) {
            write!(f, ": operation {i} ({op})")?;
        }
        if let Some(p) = &e.pointer {
            write!(f, " at {p:?}")?;
        }
        write!(f, ": {}", e.message)
    }
}

impl std::error::Error for PatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0
            .source
            .as_ref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}
