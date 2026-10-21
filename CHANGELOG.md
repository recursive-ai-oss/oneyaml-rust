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
[README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/crates/oneyaml/README.md#minimum-supported-rust-version)).

Moving from `serde_yaml`, `serde_yaml_ng` or `serde_norway`? See the
[migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/migrating-from-serde-yaml.md).

## Unreleased

Changes to what the parser reports and accepts, how errors are placed, and
how a few scalars are written. Each follows YAML 1.2.2 more closely.

### Added

- JSON Patch (RFC 6902), merge patch (RFC 7396) and JSON Pointer (RFC 6901)
  on `Document`, `DocumentMut` and `Stream`: `apply_patch` and
  `apply_merge_patch`, with the types `Patch`, `PatchOp`, `MergePatch`,
  `Pointer`, `PatchError` and `PatchErrorKind`, and `Document::get_pointer`
  and `get_pointer_tokens` to read a node by pointer.
  - A patch is applied as a whole: if any operation fails, the document is
    exactly as it was, including its comments and layout. A patch changes only
    the text of the values it names.
  - A pointer token matches a mapping key by the key's scalar text (`/80`
    finds the key `80`, `'80'` and `"80"` alike, whatever tag the key has; an
    alias key has the text of the node it names), through an index of the
    mapping's keys built once per patch, so a patch of many operations on a
    wide mapping costs the operations, not the operations times the width,
    and the answer does not depend on the mapping's width. Two keys with the
    same text are refused as ambiguous, and so is a key that a later duplicate
    of the same meaning overrides (`DuplicateKeys::LastWins`). With
    `merge_keys` on, keys merged in by `<<` are found, as `get_path` finds
    them.
  - The node an alias key names is read once per patch, however many alias
    keys name it (again only if the patch removes or replaces it), and only
    a digest and the length of its text are kept, never a copy, so a patch of many operations
    on a mapping with alias keys does not read them again for each operation.
    The bytes read are charged to the alias byte budget, as are the bytes of
    the alias keys that an `add` into the mapping, a `LastWins` duplicate
    check or a search of merged mappings has to read afresh. Looking at alias
    keys and merged mappings is also counted in steps, and a patch that spends
    more than about a million of them (plus twice the document's length in
    bytes) fails with a `Limit` error.
  - `Document::get_pointer` keeps the key indexes it builds until the document
    is next edited, so reading many pointers in a wide mapping indexes it
    once.
  - Writes refuse to go through an alias, to change a key that exists only in
    a mapping merged in by `<<`, and to remove an anchored node that an alias
    still uses. These are `RefusedAlias` errors.
  - `test` compares by the YAML data model under the Core schema, which
    departs from RFC 6902 section 4.6: `1` is not `1.0`, and `0x1F` is `31`.
    A mapping key in a compared value matches a JSON object key only if it is
    a string under the Core schema, so the integer key `1` does not match
    `"1"`.
  - The bytes a patch copies are charged to the alias byte budget
    (`Limits::alias_byte_budget`; by default the larger of ten times the
    document's length and 64 MiB), three times each, because a copied value is
    held in memory as a value while it is written and takes several times its
    text. A `copy` charges the length of the text it reads, and a `copy` or
    `move` that has to write aliases out also charges the size of the
    written-out value beyond that text, at the same weight. At the default
    limits a patch can copy about 21 MiB of text in all (a third of 64 MiB; a
    third of ten times the document when that is larger). A patch over the
    budget fails with a limit error and changes nothing. The document size
    limit applies after every write.
  - A copied or moved value is written out in full, so an alias inside it
    becomes its content, and comments inside a moved value are not kept.

- `Y0033` (`DUPLICATE_YAML_DIRECTIVE`): a second `%YAML` directive in one
  document prefix. It is reported where the second directive starts.
  `Y1004` stays the code for a repeated `%TAG` handle.
- `Y0034` (`INVALID_COMMENT_CHARACTER`): a character that cannot appear in
  a comment (a control character, U+007F, or one of U+0080 to U+009F other
  than U+0085). The error is at the first such character; the comment is
  kept as written and the parse goes on.

### Changed

- yaml-test-suite case 4H7K (`[ a, b, c ] ]`): its corrected expectation no
  longer has `-DOC` before the error, the same rule as BS4K and EB22 (a
  document ends only at a marker or at the end of the stream). The parser
  is unchanged; the strict score is now 402/402.
- Setting a value over an existing node (`set`, `set_scalar`, `set_path`,
  `replace_at`, `insert` on an existing key) now writes exactly that value
  and changes only that value's text:
  - A literal (`|`) or folded (`>`) block scalar stays a block scalar of the
    same kind. Its chomping indicator changes only as far as the new text
    needs: no final line break gives `-`, exactly one gives the clear form
    (no indicator), more give `+`, and an indicator that already holds the
    text stays (a `|+` set to one final line break stays `|+`, without the
    empty line the old text ended with). The content keeps the indentation
    the old content had. A folded scalar writes one line break between
    lines of text as an empty line, so it reads back as written.
    Before, any block scalar was rewritten as a literal block with the
    default indentation, or as a plain scalar when the new text had no
    final line break.
  - A plain, single-quoted or double-quoted scalar set to text with a line
    break becomes a double-quoted scalar with `\n` escapes on the same line.
    Before it became a literal block scalar.
  - A string set over a node is written plain when the document's schema
    reads it as a string (`yes`, `no`, `on` are strings under the default
    Core schema), and quoted when it would read as another type or plain
    syntax cannot hold it. Before, such words were quoted for the sake of
    YAML 1.1 readers. Documents read with `Settings::yaml11_booleans` (or
    `Compat::yaml_1_1()`) still get them quoted. New entries and items
    keep the stricter quoting for every reader.
  - A tag stays on a scalar while the new value reads back under it as
    exactly that value: a custom tag on a string, `!!str` on a string,
    `!!int` on an integer, `!!float` on a float, `!!bool` and `!!null`.
    Any other tag (`!!binary` on a string, `!!int` on a string) is dropped
    as before. The anchor and tag keep the order the source had.
  - The white space between `:` (or `-`) and the value stays as the
    document wrote it (a tab, several spaces), unless the new value is a
    block collection, which starts after one space.
  - A block scalar that ends the file without a line break, set to text
    that needs none, leaves the line break missing (`a: |` with `old` set
    to `n` gives `a: |-` with `n`).
  - A text too long to write as a block scalar within
    `max_document_size` at the scalar's indentation is written another way
    instead of being built first.
- Removing the last entry of a block collection after an entry that ends
  with a block scalar also removes the lines right under it that would
  otherwise become part of that scalar's text: a comment indented as far as
  the scalar's content, or white space wider than it. A line that is not
  empty and narrower than the content ends the scalar, so it and the lines
  after it stay. The comments taken are in `Removed::comments`, and
  `insert_removed_at` writes them back. Before, the removal was refused as
  not reading back. Removing the only entry of a mapping whose key is a
  block scalar, when a `:` at the margin follows, now leaves `{}` and keeps
  that entry.
- A path segment of `""` names the entry whose key has no content (`: v`,
  `[: v]`, `&a : v`), and the entry with a quoted empty key (`'': v`). When
  both are present the segment is refused as ambiguous (lookups return
  `None`, edits `EditError::Unsupported`), and `set_path` never adds a second
  empty key.

- Parse errors point at the first character that cannot belong, not at the
  start of the construct around it. This moves the position of many
  diagnostics (the code and the message are unchanged unless listed below):
  a bad escape, an invalid anchor or tag, a flow collection glued to a key
  or property, a missing `:`, an unterminated quoted scalar, a plain
  scalar with a second `: `, a bad block scalar header, an implicit key that
  is too long, a line break inside an implicit key, and so on. A quoted
  scalar that is still open on a line that is not indented more than its
  parent is reported at the start of that line (the closing quote of a
  nested scalar at the margin included), and properties on a line of their
  own before a key of an open mapping are reported where the key starts.
  Rendered snippets and `(line:column)` texts move with them.
- Columns reported by `LineIndex` for characters and UTF-16 units leave out
  a byte order mark at the start of the text: the first character after it
  is column 0 (byte columns still count it).
- A document ends only at its end marker, at the next document start, or at
  the end of the stream. Content on a later line after a plain scalar root
  is an error inside that document; it no longer starts a second one. The
  events of such inputs no longer contain an early document end.
- A line holding only a tab after a block scalar, at the end of the text or
  before a document marker (`---` or `...`), is no longer an error.
- A second `%YAML` directive in one document prefix is reported as `Y0033`
  (it was `Y1004`), at the second directive. `Y1004` now covers only a
  repeated `%TAG` handle.
- A percent escape in a tag (`!a%41`, `!<tag:x,1:%41>`) is decoded when it
  is the escape of an ASCII character other than `%`. The escape of a
  non-ASCII byte (`%C3`) and `%25` are kept as written, so that tags that
  differ only in such an escape stay different tags (`!a%25C3%25A9` and
  `!a%C3%A9` are two tags, and as mapping keys not duplicates). A tag read
  from a document is written back as it was read. The writer refuses
  (`Y3015`) a `Value::Tagged` whose tag has a non-ASCII character, or a `%`
  that no reader produces (a bare `%`, or the escape of an ASCII character
  other than `%`), because no spelling reads back as that tag.
- An enum variant named with non-ASCII characters (`#[serde(rename =
  "café")]`) is written, in the layouts that write a variant as a tag, as the
  escaped name (`!caf%C3%A9 1`), and a `%` in a variant name as `%25`; a
  tag is matched to a variant by its name with every `%XX` escape decoded,
  so output of earlier releases still reads. A tag that names no variant is
  handed to the target decoded as well.
- An escaped surrogate pair in a double-quoted scalar (`"\uD83D\uDE00"`)
  reads as one character.
- A float that is outside the range of a double (`1e400`, `1e-400`) keeps
  its exact value as a key, so `1e400` and `2e400` are different keys
  instead of both reading as infinity; they still read as the double they
  round to.
- Strings that other readers would read as numbers are quoted when
  written: forms with two signs (`+-1`), with a leading `_`, with an
  inner sign after a radix prefix (`0b-1`, `0X-F`), and radix prefixes in
  any case.

### Fixed
- Inputs the parser accepted though they are invalid are errors now: a
  flow collection directly after a property, an empty key, an explicit key
  or a plain key (`&a[b]`, `{:{0}}`), a verbatim tag that is not a URI, and
  a second anchor on one node.

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
  [migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/migrating-from-serde-yaml.md#yaml-11-behaviours)
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
  [Results at a glance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/performance.md#results-at-a-glance)
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
  [rulebook](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/comment-attachment.md);
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
[yaml-test-suite conformance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/conformance.md),
[performance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/performance.md)
and [fuzzing](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/crates/fuzz/README.md).
