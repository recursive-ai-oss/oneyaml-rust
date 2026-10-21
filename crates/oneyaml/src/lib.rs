#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// docs.rs only (`--cfg docsrs`, Cargo.toml `[package.metadata.docs.rs]`):
// items behind a feature show its badge. Stable builds never set `docsrs`.
#![cfg_attr(docsrs, feature(doc_cfg))]

pub use oneyaml_core::*;

mod construct;
mod de;
mod diag_cap;
mod document;
mod emit;
mod error;
#[cfg(feature = "miette")]
mod miette_impl;
mod render;
mod resolve;
mod ser;
mod spanned;
mod value;

pub use de::{
    Deserializer, from_reader, from_reader_with, from_slice, from_slice_with, from_str,
    from_str_with, from_str_with_diagnostics, from_value, from_value_with,
};
pub use document::{
    AsKey, Comment, CommentExplanation, CommentId, CommentOwner, CommentRule, Comments, Document,
    DocumentMut, EditError, Entry, IntoPath, MappingRef, MergePatch, NO_COMMENTS, NodeId, NodeKind,
    NodeMut, NodeRef, Parsed, Patch, PatchError, PatchErrorKind, PatchOp, Pointer, ReinsertError,
    Removed, ScalarRef, ScalarStyle, SequenceRef, Stream,
};
pub use error::{Error, ErrorKind, Location};
#[cfg(feature = "miette")]
#[cfg_attr(docsrs, doc(cfg(feature = "miette")))]
pub use miette_impl::DiagnosticReport;
pub use render::Render;
pub use ser::{
    DocumentCompound, Serializer, to_string, to_string_with, to_value, to_value_with, to_writer,
    to_writer_with,
};
pub use spanned::Spanned;
pub use value::{
    Mapping, MappingIntoIter, MappingIter, Number, Sequence, Tag, TaggedValue, Value, ValueIndex,
};
