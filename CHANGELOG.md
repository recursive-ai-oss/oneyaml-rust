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
[README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/crates/oneyaml/README.md#minimum-supported-rust-version)).

Moving from `serde_yaml`, `serde_yaml_ng` or `serde_norway`? See the
[migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/docs/migrating-from-serde-yaml.md).

## 1.1.0 — 2026-10-10

`oneyaml` 1.1.0, `oneyaml-core` 1.1.0, `oneyaml-syntax` 0.1.2.
No public item is added, removed or changed (cargo-semver-checks finds no
change); MSRV unchanged (Rust 1.85). The reworded parse error messages
come from `oneyaml-syntax` 0.1.2 and the `Debug` form of `Code` from
`oneyaml-core` 1.1.0, so a project on `oneyaml` 1.0.1 also sees them
after `cargo update`.

### Changed

- Reading a YAML 1.1 boolean word (`y`, `yes`, `on`, `n`, `no`, `off` and
  their capitalised forms) into a `bool` explains the YAML 1.2 reading:
  the message says the word is a string in YAML 1.2, the help says to
  write `true` or `false` or to turn on `Settings::yaml11_booleans` (or
  use `Compat::yaml_1_1()`), and a suggested fix writes the 1.2 boolean.
  The code is unchanged (`Y3000`), and so is what is accepted. The
  explanation is given when reading YAML text (`from_str`, `from_slice`,
  `from_reader`, `Document` and node reads) for a plain, untagged word
  under the default Core schema; `from_value` keeps the plain
  `invalid type` message, as a `Value` records no source style. The
  [migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/docs/migrating-from-serde-yaml.md#yaml-11-behaviours)
  shows the new message for `debug: yes` read into a `bool`.
- Clearer plain messages for parse errors whose old one-line form hid
  the likely cause; codes and spans are unchanged:
  - a line inside a flow collection that is not indented more than the
    collection's block parent: when the line is the closing bracket, or
    closes the collection itself (common in JSON-style YAML), the message
    says that only its indentation is short and the suggested fix indents
    it (it used to suggest a second closing bracket); when the collection
    is still open at that line, the message says a `]` or `}` may be
    missing before it;
  - a quoted scalar still open at a less-indented line says a closing
    quote may be missing;
  - text glued to a closing quote names the cause: a quote inside the
    scalar (`'it's'`) only when the text runs to the same quote again, a
    `#` without the space a comment needs (`"a"#c`), or other text, whose
    help no longer suggests escaping a quote;
  - a plain value that runs onto the next line and is then followed by
    `:` says how it became a key, with help.
- `Debug` for `Code` prints the `Y####` form (`Y0005`), as `Display`
  does, so codes read the same inside a derived `Debug` such as a
  `Diagnostic`'s.
- Documentation: both READMEs open with a compact benchmark chart, a
  short *Why oneyaml* and the quick start. The chart counts CPU
  instructions, not wall-clock time: one bar per workload for serde
  reads, serde writes, `Document` parsing and an edit script, with each
  group's range and the number of workloads where oneyaml is slower; its
  alt text carries every number it shows. Under it is one link to
  [Results at a glance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/docs/performance.md#results-at-a-glance)
  in `docs/performance.md`, which now holds the full chart and the
  tables: both libraries' instruction counts, the instruction ratio, a
  wall-clock “× faster” column from the measured medians, a note naming
  every row where oneyaml is slower in wall-clock time, and the machine
  and versions measured. The charts and tables are generated from the
  same run as the rest of that page, re-measured on the current code (so
  they include the faster typed reads with merge keys described below).
  In both charts, bars longer than 3.5× are drawn broken, so one large
  ratio no longer squashes the others (the full chart prints each such
  bar's value; the compact chart's group range gives the largest), and
  slower bars are hatched as well as coloured.
- Documentation: the crate README has a new *Diagnostics* section that
  shows a rendered error with its suggested fix, and the repository's
  front page now has the quick start.
- Typed reads with `Settings::merge_keys` on (and duplicate keys refused,
  the default) no longer buffer each mapping before handing it to the
  target: entries stream as they are read, and only the value of each
  `<<` is held until the mapping ends. Typed reads of files with merge
  keys are faster and use less memory. The same inputs are accepted and
  rejected, with the same values. What changes is which error a typed
  read reports when an input has more than one problem: it is now
  the first one met while reading, as with merge keys off. For example,
  `a: x` followed later by a duplicate key, read into a struct with
  `a: u32`, now reports the type error at `a` rather than the duplicate
  key. Error paths use the key as written (`~`, `0x10`), as with merge
  keys off. An error in content the target skips (an unknown field, the
  rest of a mapping after the target stops) carries the path of the
  entry being skipped rather than the full path into it, as with merge
  keys off. A target that ignores an error in the input now gets
  `Y3018` on its next read, as with merge keys off. An error inside a
  `<<` value is reported when that value is read, at a path through
  `<<` (for example `services.web.<<.ports[0]`). `Value` reads,
  `Document`, and reads with `DuplicateKeys::LastWins` are unchanged.

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
  [rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/docs/comment-attachment.md);
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
[yaml-test-suite conformance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/docs/conformance.md),
[performance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/docs/performance.md)
and [fuzzing](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.1.0/crates/fuzz/README.md).
