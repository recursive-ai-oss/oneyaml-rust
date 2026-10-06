# Changelog

All notable changes to the published crates (`oneyaml`, `oneyaml-core`,
`oneyaml-syntax`). The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [semver](https://semver.org/). `oneyaml` and `oneyaml-core` share
a version; `oneyaml-syntax` has its own, looser track (its syntax tree
and event stream may change in a minor release). Default output bytes
never change within a major version except to fix incorrect output,
which is called out here. Breaking changes are flagged **Breaking**; an
MSRV change is always announced here (the MSRV policy is in the
[README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/crates/oneyaml/README.md#minimum-supported-rust-version)).

Moving from `serde_yaml`, `serde_yaml_ng` or `serde_norway`? See the
[migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/migrating-from-serde-yaml.md).

## 1.0.1 — 2026-10-06

`oneyaml` 1.0.1, `oneyaml-core` 1.0.1, `oneyaml-syntax` 0.1.1.
Documentation and packaging only: no API, behaviour or output change.

### Changed

- Documentation rewritten for readers of the published crates: the API
  reference, the READMEs, the performance, conformance, comment-attachment
  and migration documents, and the contributor guide no longer refer to
  internal planning documents or development history.
- The benchmark tables in `docs/performance.md`, the README summary and
  its chart were re-measured on the 1.0.1 release tree. One serde row,
  typed reads of a docker-compose file with merge keys, is now just over
  oneyaml's own 1.25× target; the library code is the same as in 1.0.0.
- The changelog starts at 1.0.0; earlier versions were never published.

## 1.0.0 — 2026-10-05

Initial release: `oneyaml` 1.0.0, `oneyaml-core` 1.0.0 and
`oneyaml-syntax` 0.1.0. MSRV: Rust 1.85.

oneyaml reads and writes YAML 1.2 through serde, and edits YAML files
losslessly, keeping comments, with one parser behind both, so the two
never disagree about what a file means.

### Added

- **Serde** reads and writes: `from_str`, `from_slice`, `from_reader`,
  `to_string`, `to_writer`, `from_value`, `to_value`, with
  `serde_yaml`-compatible names; multi-document streams through
  `Deserializer`; borrowing deserialization; `Spanned<T>`; tagged enums.
- **`Value`**, a dynamic value type that keeps tags and allows any value
  as a mapping key, with non-panicking indexing.
- **Lossless editing** with `Document` and `Stream`: parsing and writing
  back an untouched file gives the same bytes; typed reads and edits
  through `NodeRef`/`NodeMut` handles and string paths
  (`"spec.containers[0].image"`); every edit keeps the rest of the file,
  comments included, byte for byte, and lays out new content like its
  neighbours.
- **Comments**: every comment has an owner set by a numbered
  [rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/comment-attachment.md);
  edits move comments with their owner, and `explain_comment` says which
  rule decided.
- **Anchors and aliases** in edits: editing an anchored node is seen
  through every alias, and removing a node an alias still uses is
  refused.
- **YAML 1.2.2 by default**, with the Core schema (Failsafe and JSON
  built in). Every YAML 1.1 behaviour is an opt-in, named setting:
  **merge keys (`<<`)** with `Settings::merge_keys`, 1.1 booleans, legacy
  octal, sexagesimal numbers and more; `Compat::yaml_1_1()` and
  `Compat::serde_yaml()` presets.
- **Diagnostics**: every error has a stable `Y####` code, a span and a
  path; common mistakes come with a suggested fix; `Render` prints a
  rustc-style snippet; `Document::parse_recovering` never fails.
- **Resource limits** on every entry point (depth, alias expansion in
  nodes and bytes, document size, scalar length, document count,
  diagnostics), with `Limits::default()`, `Limits::untrusted()` for
  input from the network, and `Limits::unlimited()`.
- **No `unsafe`, no panics**: every published crate is
  `#![forbid(unsafe_code)]`; every input gives a value or an error, and
  deep nesting can't overflow the stack.
- **Feature flags**, off by default: `miette` (diagnostics as
  `miette::Diagnostic`) and `serde_json` (conversions between `Value`
  and `serde_json::Value`).
- **`oneyaml-syntax`**: the lossless decoder, event stream and syntax
  tree underneath, for tooling.

Evidence for these claims:
[yaml-test-suite conformance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md),
[performance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/performance.md)
and [fuzzing](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/crates/fuzz/README.md).
