//! Stable vocabulary types for [oneyaml](https://docs.rs/oneyaml): spans,
//! diagnostic codes, diagnostics, line indexes, paths, input encodings, and
//! settings.
//!
//! This crate has no runtime dependencies and no behaviour beyond trivial
//! constructors and accessors. `oneyaml` re-exports everything here, so
//! most users depend on `oneyaml` alone. The resource-limit profiles,
//! including [`Limits::untrusted()`] for input from the network, are
//! documented on [`Limits`]; YAML 1.1 behaviours such as merge keys are
//! opt-in [`Settings`].
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod code;
mod diagnostic;
mod emit;
mod encoding;
mod limits;
mod line_index;
mod path;
mod settings;
mod span;

pub use code::{Code, CodeRange};
pub use diagnostic::{Applicability, Diagnostic, Label, Replacement, Severity, Suggestion};
pub use emit::{Emit, Layout, QuoteStyle};
pub use encoding::Encoding;
pub use limits::Limits;
pub use line_index::{ColumnUnit, LineCol, LineIndex};
#[doc(hidden)]
pub use path::__private;
pub use path::{ParsePathError, Path, PathSegment};
pub use settings::{Compat, DuplicateKeys, Schema, Settings};
pub use span::Span;
