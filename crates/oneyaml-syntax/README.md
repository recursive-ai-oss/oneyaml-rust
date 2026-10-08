# oneyaml-syntax

The lossless syntax layer of [oneyaml](https://crates.io/crates/oneyaml):
the UTF-8/16/32 decoder, the explicit-stack YAML 1.2.2 parser, its public
lossless event stream, and the arena syntax tree whose untouched emit is
byte-identical to the input.

For tooling (formatters, language servers, linters). **This crate has its
own semver track** and may break more often than `oneyaml`; most users
want `oneyaml`'s `Document` instead.

**Editing.** `SyntaxTree` has structural edits (`remove`, `replace`,
`insert_before`/`insert_after`, `append_child`, `replace_token`, and
the line-end forms `replace_to_line_end`/`insert_line_after`) that splice
`Fragment`s parsed by the same parser. Each edit is checked in its real
context and refused with a `TreeEditError` (the tree unchanged) when its
text would read back as a different tree, so an edited tree always equals
the parse of its own `emit()`. `NodeId` handles stay valid across edits
until their element is removed (`StaleNode` after), edits cost O(depth +
local), never O(document), and `fork()` copies a tree so that every
handle issued before the fork resolves in both copies.

No `unsafe`, no panics on any input, and no recursion on the
input-driven path. Its yaml-test-suite results are in
[docs/conformance.md](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/conformance.md).

MSRV: Rust 1.85 (rolling policy in the
[oneyaml README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/crates/oneyaml/README.md#minimum-supported-rust-version)).
License: MIT OR Apache-2.0.
