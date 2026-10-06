//! What each element of the syntax tree is.

/// The kind of a syntax tree element: an interior **node** or a leaf
/// **token** ([`SyntaxKind::is_token`]).
///
/// Tokens cover source bytes, and in document order they tile the source:
/// every byte belongs to exactly one token. A token may be
/// zero-width only when it is the text of an empty scalar
/// ([`SyntaxKind::PlainScalar`]) or the text of an [`SyntaxKind::Error`]
/// that points at a position ([`SyntaxKind::ErrorText`]). Nodes cover the
/// bytes of their tokens; a node's span runs from its first token to its
/// last.
///
/// Stored as one byte per element.
#[non_exhaustive]
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SyntaxKind {
    // ------------------------------------------------------------ nodes
    /// The whole input (`l-yaml-stream`, production 211); always the root.
    /// Holds documents and the bytes between them: byte order marks,
    /// comments, blank lines, stray `...` markers.
    Stream,
    /// One document (productions 206-210): its directives, its `---` and
    /// `...` markers when written, the trivia between them and the root,
    /// and the root node. Trivia after an implicitly ended document belongs
    /// to the [`SyntaxKind::Stream`] (event contract C4, "ends early").
    Document,
    /// A block mapping (production 187): its properties, then keys, values,
    /// `?`/`:` indicators and the trivia between them, flat.
    BlockMapping,
    /// A flow mapping `{ … }` (production 140).
    FlowMapping,
    /// A single-pair mapping inside a flow sequence, `[ a: b ]`
    /// (productions 150-151).
    FlowPair,
    /// A block sequence (production 183): `-` indicators, entries, and the
    /// trivia between them, flat.
    BlockSequence,
    /// A flow sequence `[ … ]` (production 137).
    FlowSequence,
    /// A scalar node: its properties (and block scalar header), then
    /// exactly one scalar text token, always its last child.
    Scalar,
    /// An alias node: exactly one [`SyntaxKind::AliasName`] token.
    Alias,
    /// A syntax error (event contract C6): exactly one
    /// [`SyntaxKind::ErrorText`] token. Read-only for the edit layer; the
    /// bytes skipped while resynchronising follow it as
    /// [`SyntaxKind::Skipped`] tokens.
    Error,

    // ------------------------------------------------------------ tokens
    /// A plain scalar's text (may be zero-width: an empty node).
    PlainScalar,
    /// A single-quoted scalar's text, quotes included.
    SingleQuotedScalar,
    /// A double-quoted scalar's text, quotes included.
    DoubleQuotedScalar,
    /// A literal block scalar's content lines (header excluded, C5).
    LiteralScalar,
    /// A folded block scalar's content lines (header excluded, C5).
    FoldedScalar,
    /// `*name`.
    AliasName,
    /// A `%YAML` directive.
    YamlDirective,
    /// A `%TAG` directive.
    TagDirective,
    /// A reserved directive.
    ReservedDirective,
    /// `---` of an explicit document start.
    DocumentStartMarker,
    /// `...` ending a document.
    DocumentEndMarker,
    /// `...` that ends no document (e.g. at the start of a stream).
    StrayDocumentEndMarker,
    /// `{`.
    FlowMappingOpen,
    /// `}`.
    FlowMappingClose,
    /// `[`.
    FlowSequenceOpen,
    /// `]`.
    FlowSequenceClose,
    /// U+FEFF between documents.
    ByteOrderMark,
    /// A run of spaces and/or tabs.
    Whitespace,
    /// One line break (`\n`, `\r\n`, or `\r`).
    LineBreak,
    /// `#` to the end of the line.
    Comment,
    /// `-` block sequence entry indicator.
    SequenceEntry,
    /// `?` mapping key indicator.
    MappingKey,
    /// `:` mapping value indicator.
    MappingValue,
    /// `,` flow entry separator.
    FlowEntry,
    /// `&name` anchor property.
    Anchor,
    /// A tag property.
    Tag,
    /// A block scalar header (`|`, `>-`, `|2+`, …).
    BlockScalarHeader,
    /// The offending bytes of an [`SyntaxKind::Error`] (may be zero-width).
    ErrorText,
    /// One line of bytes skipped while resynchronising after an error.
    Skipped,
}

impl SyntaxKind {
    /// `true` for leaf tokens, `false` for interior nodes.
    #[must_use]
    pub const fn is_token(self) -> bool {
        (self as u8) >= (Self::PlainScalar as u8)
    }

    /// `true` for presentation-only tokens that carry no meaning: white
    /// space, line breaks, comments, and byte order marks.
    #[must_use]
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineBreak | Self::Comment | Self::ByteOrderMark
        )
    }

    /// `true` for the node kinds that hold a YAML node's content: mappings,
    /// sequences, scalars, and aliases.
    #[must_use]
    pub const fn is_yaml_node(self) -> bool {
        matches!(
            self,
            Self::BlockMapping
                | Self::FlowMapping
                | Self::FlowPair
                | Self::BlockSequence
                | Self::FlowSequence
                | Self::Scalar
                | Self::Alias
        )
    }
}

#[cfg(test)]
mod tests {
    use super::SyntaxKind;

    #[test]
    fn nodes_and_tokens() {
        for k in [
            SyntaxKind::Stream,
            SyntaxKind::Document,
            SyntaxKind::BlockMapping,
            SyntaxKind::FlowMapping,
            SyntaxKind::FlowPair,
            SyntaxKind::BlockSequence,
            SyntaxKind::FlowSequence,
            SyntaxKind::Scalar,
            SyntaxKind::Alias,
            SyntaxKind::Error,
        ] {
            assert!(!k.is_token(), "{k:?}");
            assert!(!k.is_trivia(), "{k:?}");
        }
        for k in [
            SyntaxKind::PlainScalar,
            SyntaxKind::Skipped,
            SyntaxKind::Comment,
        ] {
            assert!(k.is_token(), "{k:?}");
        }
        assert!(SyntaxKind::Comment.is_trivia());
        assert!(!SyntaxKind::Anchor.is_trivia());
        assert!(SyntaxKind::Alias.is_yaml_node());
        assert!(!SyntaxKind::Document.is_yaml_node());
        assert_eq!(core::mem::size_of::<SyntaxKind>(), 1);
    }
}
