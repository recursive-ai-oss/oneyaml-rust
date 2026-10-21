//! Tier-2 errors of the `Document` API: small, typed, matchable.

use oneyaml_core::{Diagnostic, Path};

use super::NodeId;

/// Why a [`Document`](crate::Document) navigation or edit failed.
/// Every variant says where it happened or what refused it; a
/// failed edit changes nothing.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EditError {
    /// A [`NodeId`](crate::NodeId) that names no node of this document:
    /// issued by another document, or its node no longer exists (removed
    /// or replaced by an edit).
    StaleNode,
    /// No mapping entry with that key.
    #[non_exhaustive]
    NotFound {
        /// The path of the missing entry.
        at: Path,
    },
    /// A key segment met a node that is not a mapping.
    #[non_exhaustive]
    NotAMapping {
        /// The path of that node.
        at: Path,
    },
    /// An index segment met a node that is not a sequence.
    #[non_exhaustive]
    NotASequence {
        /// The path of that node.
        at: Path,
    },
    /// An index past the end of a sequence.
    #[non_exhaustive]
    IndexOutOfBounds {
        /// The path of the sequence.
        at: Path,
        /// The index asked for.
        index: usize,
        /// The sequence's length.
        len: usize,
    },
    /// The walk met an error region of a restricted document.
    #[non_exhaustive]
    ErrorNode {
        /// The path of the error region.
        at: Path,
    },
    /// The edit touches (or is next to) an error region of a restricted
    /// document, or the document cannot be edited at all (its input had
    /// encoding errors).
    #[non_exhaustive]
    ReadOnly {
        /// The path of the node the edit was aimed at (empty when unknown).
        at: Path,
    },
    /// The syntax layer refused the edit: its text would not read back as
    /// the edited tree ("an edited tree always agrees with its own
    /// text"), e.g. a block value moved into a flow collection. Nothing
    /// was changed.
    ///
    /// One documented limitation is also refused this way rather than
    /// as [`EditError::Unsupported`]: under
    /// `lenient_inline_colons`, an edit whose validation window holds a
    /// `Y0028` warning attached to an element the edit moves, even when
    /// its result would be well formed (see [`crate::Document`],
    /// *Editing in depth*).
    #[non_exhaustive]
    NotWellFormed {
        /// The path of the node the edit was aimed at (empty when unknown).
        at: Path,
    },
    /// This version cannot make that edit (the reason says which part);
    /// nothing was changed.
    #[non_exhaustive]
    Unsupported {
        /// The path of the node the edit was aimed at (empty when unknown).
        at: Path,
        /// What is not supported.
        reason: &'static str,
    },
    /// The mapping already has an entry with that key.
    #[non_exhaustive]
    KeyExists {
        /// The path of the existing entry.
        at: Path,
    },
    /// The value or the edit was rejected with a coded diagnostic: a limit
    /// (`Y2005` `max_depth`, `Y2007` `max_typed_depth`, `Y2002`, `Y2006`),
    /// an output setting (`Y1012`, `Y1017`), or a value YAML cannot
    /// represent (`Y3015`), or a serde error (`Y30xx`).
    #[non_exhaustive]
    Rejected {
        /// The diagnostic, with its stable `Y####` code.
        diagnostic: Box<Diagnostic>,
    },
    /// the anchor is referenced by aliases outside what the
    /// edit removes; remove or replace them first
    /// ([`Document::aliases_of`](crate::Document::aliases_of)).
    #[non_exhaustive]
    AnchorInUse {
        /// The anchor name.
        anchor: String,
        /// The aliases that refer to it, in document order.
        aliases: Vec<NodeId>,
    },
    /// an alias can only refer to an anchored node.
    #[non_exhaustive]
    NotAnchored {
        /// The path of the node without an anchor (empty when unknown).
        at: Path,
    },
    /// the edit would place an alias before its anchor
    /// (or where its anchor is not defined), which YAML cannot express.
    #[non_exhaustive]
    AliasBeforeAnchor {
        /// The anchor name.
        anchor: String,
    },
    /// the anchor name is already used in this document.
    #[non_exhaustive]
    DuplicateAnchor {
        /// The anchor name.
        anchor: String,
    },
    /// The anchor name is not valid: YAML 1.2.2 production 102 allows any
    /// printable character except white space, line breaks, the byte
    /// order mark, and `,` `[` `]` `{` `}`, and at least one.
    #[non_exhaustive]
    InvalidAnchorName {
        /// The name asked for.
        name: String,
    },
    /// A [`Removed`](crate::Removed) node was re-inserted into another
    /// document than the one it was removed from.
    WrongDocument,
    /// Text given to a comment setter cannot be a comment:
    /// it holds a line break, the byte order mark, or another character
    /// YAML 1.2.2 production 75 (`c-nb-comment-text`) does not allow, or
    /// U+0085, U+2028 or U+2029, which some readers take for line breaks
    /// (every-reader principle). The text is refused, never
    /// mangled.
    #[non_exhaustive]
    InvalidComment {
        /// The text given (the view form, without `#`).
        text: String,
    },
    /// A comment written there would not belong to the owner it was
    /// written for when the text is read again, or a
    /// comment next to it would change owner: e.g. a footer of a root
    /// collection at column 0 (rule 7 makes it the document footer), a
    /// document footer before a next document's `---` (rule 1), or a
    /// leading comment of a root mapping (rule 3 gives it to the first
    /// entry). Nothing was changed; [`Document::attach_comment`](crate::Document::attach_comment)
    /// re-owns an existing comment explicitly instead.
    #[non_exhaustive]
    CommentReattaches {
        /// The rule that would decide the owner there.
        rule: crate::CommentRule,
    },
    /// A path string given to a path method is not a path (see
    /// [`IntoPath`](crate::IntoPath) and [`Path::parse`]). Nothing was
    /// changed.
    ///
    /// ```
    /// use oneyaml::{Document, EditError};
    ///
    /// let mut doc = Document::parse("a: 1\n").unwrap();
    /// let err = doc.set_path("a..b", 2).unwrap_err();
    /// assert!(matches!(err, EditError::InvalidPath { offset: 2, .. }));
    /// ```
    #[non_exhaustive]
    InvalidPath {
        /// The UTF-8 byte offset in the path string where it went wrong.
        offset: usize,
        /// What went wrong.
        reason: &'static str,
    },
    /// An internal invariant failed (a bug; `Y9000`).
    Internal,
}

impl EditError {
    /// The path where the walk or edit stopped, when the variant names
    /// one. (The struct-like variants are `#[non_exhaustive]`, match
    /// them with `..`.)
    #[must_use]
    pub fn at(&self) -> Option<&Path> {
        match self {
            Self::NotFound { at }
            | Self::NotAMapping { at }
            | Self::NotASequence { at }
            | Self::IndexOutOfBounds { at, .. }
            | Self::ErrorNode { at }
            | Self::ReadOnly { at }
            | Self::NotWellFormed { at }
            | Self::Unsupported { at, .. }
            | Self::KeyExists { at }
            | Self::NotAnchored { at } => Some(at),
            Self::StaleNode
            | Self::Rejected { .. }
            | Self::AnchorInUse { .. }
            | Self::AliasBeforeAnchor { .. }
            | Self::DuplicateAnchor { .. }
            | Self::InvalidAnchorName { .. }
            | Self::WrongDocument
            | Self::InvalidComment { .. }
            | Self::CommentReattaches { .. }
            | Self::InvalidPath { .. }
            | Self::Internal => None,
        }
    }

    /// The coded diagnostic of [`EditError::Rejected`]: e.g. a value
    /// deeper than the document's `max_depth` (a limit diagnostic, whose
    /// help names the setting to raise).
    ///
    /// ```
    /// use oneyaml::{Document, Settings, code, path};
    ///
    /// let settings = Settings::new().max_depth(4);
    /// let mut doc = Document::parse_with("a: 1\n", &settings).unwrap();
    /// let deep = vec![vec![vec![vec![vec![1]]]]];
    /// let err = doc.set_path(&path!["b"], &deep).unwrap_err();
    /// let d = err.diagnostic().expect("a coded refusal");
    /// assert_eq!(d.code, code::VALUE_DEPTH_EXCEEDED);
    /// assert_eq!(doc.to_string(), "a: 1\n"); // nothing changed
    /// ```
    #[must_use]
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Self::Rejected { diagnostic } => Some(diagnostic),
            _ => None,
        }
    }

    pub(crate) fn rejected(e: &crate::Error) -> Self {
        Self::Rejected {
            diagnostic: Box::new(e.diagnostic().clone()),
        }
    }

    /// Fills in an empty `at` (errors raised below the path-aware layer).
    pub(crate) fn located(mut self, path: &Path) -> Self {
        match &mut self {
            Self::ReadOnly { at }
            | Self::NotWellFormed { at }
            | Self::Unsupported { at, .. }
            | Self::NotAnchored { at }
                if at.is_empty() =>
            {
                *at = path.clone();
            }
            _ => {}
        }
        self
    }
}

impl core::fmt::Display for EditError {
    /// Text from the input or the caller (anchor names,
    /// comment text, path keys) is shown bounded (a segment, a path of at
    /// most 8 KiB); the fields keep it whole.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use crate::error::{MAX_SEGMENT_CHARS, segment, short_chars, shown_path};
        let at = |p: &Path| shown_path(p);
        match self {
            Self::StaleNode => f.write_str("stale node handle: no such node in this document"),
            Self::NotFound { at: p } => write!(f, "no entry at {}", at(p)),
            Self::NotAMapping { at: p } => write!(f, "not a mapping at {}", at(p)),
            Self::NotASequence { at: p } => write!(f, "not a sequence at {}", at(p)),
            Self::IndexOutOfBounds { at: p, index, len } => {
                write!(f, "index {index} out of bounds at {} (length {len})", at(p))
            }
            Self::ErrorNode { at: p } => write!(f, "syntax error region at {}", at(p)),
            Self::ReadOnly { at: p } => write!(f, "read-only (error region nearby) at {}", at(p)),
            Self::NotWellFormed { at: p } => {
                write!(
                    f,
                    "the edit at {} would not read back as the edited tree",
                    at(p)
                )
            }
            Self::Unsupported { at: p, reason } => {
                write!(f, "unsupported edit at {}: {reason}", at(p))
            }
            Self::KeyExists { at: p } => write!(f, "an entry already exists at {}", at(p)),
            Self::Rejected { diagnostic } => write!(f, "{diagnostic}"),
            Self::AnchorInUse { anchor, aliases } => write!(
                f,
                "anchor `{}` is still referenced by {} alias(es)",
                segment(anchor),
                aliases.len()
            ),
            Self::NotAnchored { at: p } => write!(f, "the node at {} has no anchor", at(p)),
            Self::AliasBeforeAnchor { anchor } => {
                write!(
                    f,
                    "an alias `*{}` would come before its anchor",
                    segment(anchor)
                )
            }
            Self::DuplicateAnchor { anchor } => {
                write!(f, "the anchor name `{}` is already in use", segment(anchor))
            }
            Self::InvalidAnchorName { name } => {
                write!(f, "`{}` is not a valid anchor name", segment(name))
            }
            Self::WrongDocument => f.write_str("the removed node belongs to another document"),
            Self::InvalidComment { text } => write!(
                f,
                "{} cannot be a comment",
                short_chars(format_args!("{text:?}"), MAX_SEGMENT_CHARS)
            ),
            Self::CommentReattaches { rule } => write!(
                f,
                "the comment would belong to another owner there (rule {}: {})",
                rule.number(),
                rule.summary()
            ),
            Self::InvalidPath { offset, reason } => {
                write!(f, "invalid path at byte {offset}: {reason}")
            }
            Self::Internal => f.write_str("internal error in the edit layer (a bug)"),
        }
    }
}

impl std::error::Error for EditError {}
