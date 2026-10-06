//! [`Stream`]: every document of one input, sharing one syntax tree
//! (a stream-level parse).

use std::sync::Arc;

use oneyaml_core::{Diagnostic, Settings};
use oneyaml_syntax::tree::SyntaxTree;
use serde::Serialize;

use super::{
    AsKey, CommentId, CommentOwner, Document, EditError, IntoPath, NodeId, NodeMut, Parsed,
    Removed, Shared, document_id,
};
use crate::error::Error;

/// A YAML stream: every document of one input, sharing one syntax tree.
///
/// [`Document::parse`](crate::Document::parse) reads a single document; a
/// file with several (`---` between them) is a `Stream`. Each document is
/// read and edited on its own, and the stream writes its bytes back as one
/// file.
///
/// ```
/// let stream = oneyaml::Stream::parse("a: 1\n---\nb: 2\n").unwrap();
/// assert_eq!(stream.len(), 2);
/// let second = stream.document(1).unwrap();
/// assert_eq!(second.get("b").unwrap().text(), "2");
/// assert_eq!(stream.to_string(), "a: 1\n---\nb: 2\n");
/// ```
///
/// **Editing** goes through the stream:
/// [`Stream::document_mut`] gives document `i` for editing in place (O(1),
/// no copy), and the stream's text is the edited text.
///
/// ```
/// let mut stream = oneyaml::Stream::parse("a: 1\n---\nb: 2\n").unwrap();
/// let mut second = stream.document_mut(1).unwrap();
/// let b = second.id_of(&oneyaml::path!["b"]).unwrap();
/// second.node_mut(b).unwrap().set(&3).unwrap();
/// assert_eq!(stream.to_string(), "a: 1\n---\nb: 3\n");
/// ```
///
/// For serde, [`Deserializer::from_str`](crate::Deserializer::from_str)
/// iterates over the documents, one value each:
///
/// ```
/// use serde::Deserialize;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// #[derive(Deserialize)]
/// struct Kind { kind: String }
///
/// let src = "kind: Deployment\nreplicas: 2\n---\nkind: Service  # public\n";
/// let kinds: Vec<String> = oneyaml::Deserializer::from_str(src)
///     .map(|d| Kind::deserialize(d).map(|k| k.kind))
///     .collect::<Result<_, _>>()?;
/// assert_eq!(kinds, ["Deployment", "Service"]);
/// # Ok(())
/// # }
/// ```
pub struct Stream {
    /// The stream as one document: owns the tree; [`Stream::document_mut`]
    /// points it at one document.
    editor: Document,
    /// Each document's identity (for [`crate::Removed`]).
    ids: Vec<u64>,
    /// What restricted mode protects in each document but the one the
    /// editor points at (which holds its own): found from that document's
    /// region on its first edit and kept across switches (switching
    /// documents is O(1), never a re-check of the
    /// stream).
    guards: Vec<Option<super::edit::Guard>>,
}

impl Clone for Stream {
    /// An independent copy (see [`Document`]'s `Clone`: O(1) until one
    /// of the copies is edited; handles taken before resolve in both).
    fn clone(&self) -> Self {
        let ids: Vec<u64> = self.ids.iter().map(|_| document_id()).collect();
        let mut editor = self.editor.clone();
        // The editor keeps pointing
        // at the same document under that document's new identity, so the
        // clone's next `document_mut` is not a switch and keeps the guard
        // found before the first edit (a meaning error an accepted edit
        // created stays editable, and no guard is rebuilt).
        if let Some(id) = editor.index.and_then(|i| ids.get(i)) {
            editor.id = *id;
        }
        Self {
            editor,
            ids,
            // A fork keeps the handles the guards hold (as `Document`'s
            // clone keeps its guard).
            guards: self.guards.clone(),
        }
    }
}

impl core::fmt::Debug for Stream {
    /// How many documents, the span, whether it was edited, and
    /// the settings; never the caches: deterministic and bounded.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = self.shared();
        f.debug_struct("Stream")
            .field("documents", &s.doc_ids.len())
            .field("span", &s.tree.root().span())
            .field("edited", &s.edited)
            .field("settings", &s.settings)
            .finish_non_exhaustive()
    }
}

impl Stream {
    fn new(shared: Shared) -> Self {
        let ids = shared.doc_ids.iter().map(|_| document_id()).collect();
        let guards = shared.doc_ids.iter().map(|_| None).collect();
        Self {
            editor: Document::of_stream(&Arc::new(shared), None, None),
            ids,
            guards,
        }
    }

    fn shared(&self) -> &Shared {
        &self.editor.shared
    }

    fn strict(mut shared: Shared) -> Result<Self, Error> {
        let found = shared.strict_check(false)?;
        shared.add_diagnostics(found);
        shared.mode = super::Mode::Strict { single: false };
        Ok(Self::new(shared))
    }

    fn recovering(mut shared: Shared) -> Parsed<Self> {
        shared.mode = super::Mode::Recovering { single: false };
        let found = shared.recovering_check();
        shared.add_diagnostics(found);
        let diagnostics = shared.diagnostics.clone();
        Parsed {
            value: Self::new(shared),
            diagnostics,
        }
    }

    /// Parses every document with default [`Settings`]. Strict, with the
    /// meaning checks of [`Document::parse`] (no alias expansion).
    ///
    /// # Errors
    ///
    /// The first syntax, limit, settings, or meaning error.
    pub fn parse(src: impl Into<String>) -> Result<Self, Error> {
        Self::parse_with(src, &Settings::new())
    }

    /// [`Stream::parse`] with explicit [`Settings`].
    ///
    /// # Errors
    ///
    /// As [`Stream::parse`].
    pub fn parse_with(src: impl Into<String>, settings: &Settings) -> Result<Self, Error> {
        Self::strict(Shared::from_str(src.into(), settings))
    }

    /// Parses every document, recovering from errors: never fails. Each
    /// document owns the diagnostics of its own region (see
    /// [`Document::diagnostics`]).
    ///
    /// ```
    /// use oneyaml::{Settings, Stream};
    ///
    /// let parsed = Stream::parse_recovering("a: 1\n---\nb: [2\n");
    /// assert!(parsed.has_errors());
    /// let stream = parsed.into_value();
    /// assert!(!stream.document(0).unwrap().has_errors());
    /// assert!(stream.document(1).unwrap().has_errors());
    /// assert_eq!(stream.diagnostics().len(), 1);
    /// let merge = Settings::new().merge_keys(true);
    /// let parsed = Stream::parse_recovering_with("a: 1\n", &merge);
    /// assert_eq!(parsed.value().settings(), &merge);
    /// assert_eq!(parsed.value().syntax().emit(), "a: 1\n");
    /// ```
    pub fn parse_recovering(src: impl Into<String>) -> Parsed<Self> {
        Self::parse_recovering_with(src, &Settings::new())
    }

    /// [`Stream::parse_recovering`] with explicit [`Settings`].
    pub fn parse_recovering_with(src: impl Into<String>, settings: &Settings) -> Parsed<Self> {
        Self::recovering(Shared::from_str(src.into(), settings))
    }

    /// Parses bytes in any YAML encoding (§5.2) with default [`Settings`],
    /// as [`Document::from_slice`] does for one document.
    ///
    /// ```
    /// use oneyaml::{Settings, Stream};
    ///
    /// let s = Stream::from_slice(b"a\n---\nb\n").unwrap();
    /// assert_eq!(s.document(1).unwrap().to_bytes(), b"---\nb\n");
    /// let s = Stream::from_slice_with(b"a\n", &Settings::new()).unwrap();
    /// assert_eq!(s.to_bytes(), b"a\n");
    /// let parsed = Stream::from_slice_recovering(b"a\n--- \xfe\n", &Settings::new());
    /// assert!(parsed.has_errors());
    /// assert_eq!(parsed.value().to_bytes(), b"a\n--- \xfe\n");
    /// ```
    ///
    /// # Errors
    ///
    /// As [`Stream::parse`]; invalid encoding is `Y0029`–`Y0031`.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, Error> {
        Self::from_slice_with(bytes, &Settings::new())
    }

    /// [`Stream::from_slice`] with explicit [`Settings`].
    ///
    /// # Errors
    ///
    /// As [`Stream::from_slice`].
    pub fn from_slice_with(bytes: &[u8], settings: &Settings) -> Result<Self, Error> {
        // The first decoding error fails before the rest
        // is decoded (in the one decoding pass).
        Self::strict(Shared::from_slice_strict(bytes, settings)?)
    }

    /// [`Stream::from_slice_with`], recovering from errors: never fails.
    pub fn from_slice_recovering(bytes: &[u8], settings: &Settings) -> Parsed<Self> {
        Self::recovering(Shared::from_slice(bytes, settings))
    }

    /// Number of documents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shared().doc_ids.len()
    }

    /// `true` for a stream without documents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shared().doc_ids.is_empty()
    }

    /// Document `i`: a snapshot sharing this stream's tree (O(1)). Its
    /// text (`Document::to_string`, [`Document::to_bytes`]) is its region
    /// of the stream's text, comments and line breaks included (see
    /// [`Document::span`]; O(the region)).
    ///
    /// Editing the snapshot never changes the stream: its first edit
    /// copies the tree (O(stream), as for a [`Clone`]d [`Document`]). To
    /// edit the stream, use [`Stream::document_mut`]. Handles taken from a
    /// snapshot resolve in the stream's own documents too (and the
    /// reverse), until their node is removed or replaced there.
    #[must_use]
    pub fn document(&self, i: usize) -> Option<Document> {
        let doc = *self.shared().doc_ids.get(i)?;
        let mut d = Document::of_stream(&self.editor.shared, Some(doc), Some(i));
        // Explicit comment owners made through `document_mut`
        // (keyed by comment handle; reads keep each document's own).
        d.explicit.clone_from(&self.editor.explicit);
        Some(d)
    }

    /// Document `i` of the stream, for editing in place: every edit
    /// changes this stream (O(local), no copy unless a snapshot from
    /// [`Stream::document`] or a clone still shares the tree). Handles
    /// taken from any document of the stream resolve here. A
    /// [`Removed`](crate::Removed) taken from one document goes back only
    /// into the same document (anchors are per document).
    ///
    /// The result is a [`DocumentMut`] guard: it
    /// reads as a [`Document`] and has its edit methods, but the document
    /// cannot be swapped out of the stream (no `DerefMut`).
    pub fn document_mut(&mut self, i: usize) -> Option<DocumentMut<'_>> {
        let doc = *self.shared().doc_ids.get(i)?;
        let id = *self.ids.get(i)?;
        let e = &mut self.editor;
        if e.index != Some(i) || e.doc != Some(doc) || e.id != id {
            // Each document keeps its own guard (O(1)
            // per switch; the next edit never re-checks the stream).
            let mine = self.guards.get_mut(i).and_then(Option::take);
            let theirs = std::mem::replace(&mut e.guard, mine);
            if let Some(slot) = e.index.and_then(|j| self.guards.get_mut(j)) {
                *slot = theirs;
            }
            e.doc = Some(doc);
            e.index = Some(i);
            e.id = id;
        }
        Some(DocumentMut { doc: e })
    }

    /// The documents, in order (snapshots, see [`Stream::document`]).
    pub fn documents(&self) -> impl ExactSizeIterator<Item = Document> + '_ {
        (0..self.len())
            .filter_map(|i| self.document(i))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// Every diagnostic of the stream: decoding, settings, then syntax and
    /// meaning diagnostics in source order. Each located one also belongs
    /// to exactly one document (see [`Document::diagnostics`]).
    ///
    /// After edits ([`Stream::document_mut`]), the diagnostics of the
    /// edited text, built as parsing it builds them (they and [`Stream::has_errors`] describe the
    /// edited state):
    /// each document's, as [`Document::diagnostics`] reads
    /// them from its own region, in order (so a strict stream lists each
    /// document's first meaning error, the first of which is the one
    /// [`Stream::parse`] of the edited text fails with), in O(n);
    /// untouched, a copy of those of the parse. When they exceed
    /// `Limits::max_diagnostics`, the capped list is read from the
    /// documents' diagnostic counts: O(log d + the documents
    /// holding its entries and its first error + the documents edited
    /// since the last read), the counts built once, O(n), on the first
    /// such read.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let s = self.shared();
        if s.edited {
            return s.edited_stream_diagnostics();
        }
        s.diagnostics.clone()
    }

    /// `true` when the stream has at least one error (then so does at
    /// least one of its documents, unless it has none): exactly when
    /// [`Stream::diagnostics`] holds one, edited or not.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        let s = self.shared();
        let error = |d: &Diagnostic| d.severity == oneyaml_core::Severity::Error;
        if s.edited {
            // A capped list keeps the first error: complete for this.
            return s.edited_stream_diagnostics().iter().any(error);
        }
        // The complete record, never the capped list.
        s.settings_error() || !s.error_marks.is_empty() || !s.origin.invalid.is_empty()
    }

    /// The settings the stream was parsed with.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.shared().settings
    }

    /// The syntax tree (outside the stability promise, see
    /// [`Document::syntax`]).
    #[must_use]
    pub fn syntax(&self) -> &SyntaxTree {
        &self.shared().tree
    }

    /// The stream's bytes: exactly the input when untouched.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        Document::of_stream(&self.editor.shared, None, None).to_bytes()
    }
}

impl core::fmt::Display for Stream {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for n in self.shared().tree.root().descendants() {
            if n.is_token() {
                f.write_str(&n.text())?;
            }
        }
        Ok(())
    }
}

/// Document `i` of a [`Stream`], for editing in place: the guard
/// [`Stream::document_mut`] returns.
///
/// It reads as a [`Document`] (`Deref`) and has the `Document` edit
/// methods that do not go through a node handle, each changing the
/// stream; node edits go through the [`NodeMut`] its `node_mut`,
/// `root_mut`, `get_mut` and `get_path_mut` give (one editing
/// style). It keeps the id-taking comment edits that act on exactly the
/// node they get (`set_leading`, `set_trailing`, `clear_comments`,
/// `set_blank_lines_before`): on a mapping value's handle `NodeMut`
/// acts on its entry, so they are the way to a value's own comments;
/// `set_anchor` and `push_footer` act on the node either
/// way, so the handle covers them. It has no
/// `DerefMut`: the document it edits cannot be replaced
/// (`std::mem::replace`, `std::mem::swap`, assignment), so an edit through
/// it never reaches the stream's other documents.
///
/// ```
/// use oneyaml::{Stream, path};
///
/// let mut stream = Stream::parse("a: 1\n---\nb: 2\n---\nc: 3\n").unwrap();
/// let mut second = stream.document_mut(1).unwrap();
/// assert_eq!(second.get("b").unwrap().text(), "2"); // reads as a Document
/// second.set_path(&path!["b"], &20).unwrap();
/// second.get_mut("b").unwrap().set_trailing(Some("was 2")).unwrap();
/// assert_eq!(stream.to_string(), "a: 1\n---\nb: 20 # was 2\n---\nc: 3\n");
/// ```
///
/// Swapping the document out does not compile:
///
/// ```compile_fail,E0596
/// let mut stream = oneyaml::Stream::parse("a: 1\n---\nb: 2\n").unwrap();
/// let other = oneyaml::Document::parse("x: 1").unwrap();
/// let mut second = stream.document_mut(1).unwrap();
/// let _ = std::mem::replace(&mut *second, other);
/// ```
#[derive(Debug)]
pub struct DocumentMut<'s> {
    doc: &'s mut Document,
}

impl core::ops::Deref for DocumentMut<'_> {
    type Target = Document;

    fn deref(&self) -> &Document {
        self.doc
    }
}

impl DocumentMut<'_> {
    /// [`Document::node_mut`].
    ///
    /// # Errors
    ///
    /// As [`Document::node_mut`].
    pub fn node_mut(&mut self, id: NodeId) -> Result<NodeMut<'_>, EditError> {
        self.doc.node_mut(id)
    }

    /// [`Document::root_mut`].
    ///
    /// # Errors
    ///
    /// As [`Document::root_mut`].
    pub fn root_mut(&mut self) -> Result<NodeMut<'_>, EditError> {
        self.doc.root_mut()
    }

    /// [`Document::get_mut`].
    ///
    /// # Errors
    ///
    /// As [`Document::get_mut`].
    pub fn get_mut<K: AsKey>(&mut self, key: K) -> Result<NodeMut<'_>, EditError> {
        self.doc.get_mut(key)
    }

    /// [`Document::get_path_mut`].
    ///
    /// # Errors
    ///
    /// As [`Document::get_path_mut`].
    pub fn get_path_mut<P: IntoPath>(&mut self, path: P) -> Result<NodeMut<'_>, EditError> {
        self.doc.get_path_mut(path)
    }

    /// [`Document::set_path`].
    ///
    /// # Errors
    ///
    /// As [`Document::set_path`].
    pub fn set_path<P: IntoPath, T: Serialize>(
        &mut self,
        path: P,
        value: T,
    ) -> Result<NodeMut<'_>, EditError> {
        self.doc.set_path(path, value)
    }

    /// [`Document::get_path_or_create`].
    ///
    /// # Errors
    ///
    /// As [`Document::get_path_or_create`].
    pub fn get_path_or_create<P: IntoPath>(&mut self, path: P) -> Result<NodeMut<'_>, EditError> {
        self.doc.get_path_or_create(path)
    }

    /// [`Document::set_root`].
    ///
    /// # Errors
    ///
    /// As [`Document::set_root`].
    pub fn set_root<T: Serialize>(&mut self, value: T) -> Result<NodeMut<'_>, EditError> {
        self.doc.set_root(value)
    }

    /// [`Document::remove_path`].
    ///
    /// # Errors
    ///
    /// As [`Document::remove_path`].
    pub fn remove_path<P: IntoPath>(&mut self, path: P) -> Result<Removed, EditError> {
        self.doc.remove_path(path)
    }

    /// [`Document::remove_node`].
    ///
    /// # Errors
    ///
    /// As [`Document::remove_node`].
    pub fn remove_node(&mut self, id: NodeId) -> Result<Removed, EditError> {
        self.doc.remove_node(id)
    }

    /// [`Document::remove_keep_comments`]: the leading comments written
    /// above the entry stay, with the next entry, the collection's footer,
    /// or — for the last entry of the document's root collection — the
    /// document footer; one written inside the entry
    /// (after `?`, `-` or its properties) goes with the [`Removed`].
    ///
    /// # Errors
    ///
    /// As [`Document::remove_keep_comments`].
    pub fn remove_keep_comments(&mut self, id: NodeId) -> Result<Removed, EditError> {
        self.doc.remove_keep_comments(id)
    }

    /// [`Document::set_leading`]: the base layer, acting on exactly node
    /// `id` (on a mapping value's handle, [`NodeMut::set_leading`] acts on
    /// its entry; this reaches a value's own leading comment).
    ///
    /// # Errors
    ///
    /// As [`Document::set_leading`].
    pub fn set_leading<I>(&mut self, id: NodeId, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.doc.set_leading(id, lines)
    }

    /// [`Document::set_trailing`]: the base layer, on exactly node `id`.
    ///
    /// # Errors
    ///
    /// As [`Document::set_trailing`].
    pub fn set_trailing(
        &mut self,
        id: NodeId,
        text: Option<&str>,
    ) -> Result<Option<CommentId>, EditError> {
        self.doc.set_trailing(id, text)
    }

    /// [`Document::clear_comments`]: the base layer, on exactly node `id`.
    ///
    /// # Errors
    ///
    /// As [`Document::clear_comments`].
    pub fn clear_comments(&mut self, id: NodeId) -> Result<Vec<String>, EditError> {
        self.doc.clear_comments(id)
    }

    /// [`Document::set_blank_lines_before`]: the base layer, on exactly
    /// node `id`.
    ///
    /// # Errors
    ///
    /// As [`Document::set_blank_lines_before`].
    pub fn set_blank_lines_before(&mut self, id: NodeId, n: usize) -> Result<(), EditError> {
        self.doc.set_blank_lines_before(id, n)
    }

    /// [`Document::set_comment_blank_lines`].
    ///
    /// # Errors
    ///
    /// As [`Document::set_comment_blank_lines`].
    pub fn set_comment_blank_lines(&mut self, id: CommentId, n: usize) -> Result<(), EditError> {
        self.doc.set_comment_blank_lines(id, n)
    }

    /// [`Document::set_header`].
    ///
    /// # Errors
    ///
    /// As [`Document::set_header`].
    pub fn set_header<I>(&mut self, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.doc.set_header(lines)
    }

    /// [`Document::set_footer`].
    ///
    /// # Errors
    ///
    /// As [`Document::set_footer`].
    pub fn set_footer<I>(&mut self, lines: I) -> Result<Vec<CommentId>, EditError>
    where
        I: IntoIterator,
        I::Item: AsRef<str>,
    {
        self.doc.set_footer(lines)
    }

    /// [`Document::attach_comment`] (the explicit owner is kept by the
    /// stream).
    ///
    /// # Errors
    ///
    /// As [`Document::attach_comment`].
    pub fn attach_comment(&mut self, id: CommentId, owner: CommentOwner) -> Result<(), EditError> {
        self.doc.attach_comment(id, owner)
    }

    /// [`Document::detach_comment`].
    ///
    /// # Errors
    ///
    /// As [`Document::detach_comment`].
    pub fn detach_comment(&mut self, id: CommentId) -> Result<Option<CommentOwner>, EditError> {
        self.doc.detach_comment(id)
    }
}
