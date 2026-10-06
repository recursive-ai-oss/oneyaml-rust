//! Lossless YAML 1.2.2 decoder, lexer, parser, event stream, and syntax tree
//! underneath [oneyaml](https://docs.rs/oneyaml).
//!
//! For tooling (formatters, language servers, linters). Most users want
//! `oneyaml`'s `Document` and serde API instead. **This crate has its own
//! semver track** and may break more often than `oneyaml`.
//!
//! - [`decode()`]: UTF-8/16/32 input with byte order mark detection.
//! - [`events`] / [`parse`]: the lossless event stream, whose trivia events
//!   tile every input byte (white space, comments, indicators).
//! - [`SyntaxTree`]: the arena syntax tree; an untouched tree emits the
//!   input byte for byte, and its structural edits are checked so that an
//!   edited tree always equals the parse of its own text.
//!
//! No `unsafe`, no panics on any input, no recursion on the input-driven
//! path. MSRV: Rust 1.85.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod decode;
pub mod event;
pub mod parser;
pub mod tree;

pub use decode::{
    Decoded, Encoder, Encoding, InvalidRun, InvalidRuns, decode, decode_strict, decode_with_limit,
    detect_encoding, encode, first_error, run_diagnostic,
};
pub use event::{Event, EventKind, EventList, Sourced, check_contract, check_tiling};
pub use oneyaml_core::Span;
pub use parser::{Events, events, events_with, parse, parse_with};
pub use tree::SyntaxTree;
