# Changelog

All notable changes to the published crates (`oneyaml`, `oneyaml-core`,
`oneyaml-syntax`). The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow [semver](https://semver.org/). `oneyaml` and `oneyaml-core` share
a version; `oneyaml-syntax` has its own, looser track (its syntax tree
and event stream may change in a minor release). Within a major version,
default output bytes change only to fix incorrect output or to follow the
documented edit and quoting rules, and every such change is listed in the
release's *Upgrading* notes. Breaking changes are flagged **Breaking**; an
MSRV change is always announced here (the MSRV policy is in the
[README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/crates/oneyaml/README.md#minimum-supported-rust-version)).

Moving from `serde_yaml`, `serde_yaml_ng` or `serde_norway`? See the
[migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/migrating-from-serde-yaml.md).

## 1.2.0 — 2026-10-21

`oneyaml` 1.2.0, `oneyaml-core` 1.2.0, `oneyaml-syntax` 0.2.0. MSRV
unchanged (Rust 1.85). No public item is removed or changed, so nothing
here is flagged **Breaking**; `Y1005` is deprecated. `oneyaml-syntax`
moves to 0.2 because its event stream changed for some inputs (see
*Changed*) and it has a new public method; `oneyaml` 1.2.0 needs it, and a
project still on `oneyaml` 1.1.0 keeps `oneyaml-syntax` 0.1.2 after
`cargo update`.

**Upgrading from 1.1.** No public item is removed or changed, so code
that builds against 1.1 builds against 1.2. Check these if you compare
output bytes or match diagnostic codes:

- Default output (each follows the documented edit or quoting rules, or
  fixes output; details under *Changed*):
  - Edits write `no`, `yes`, `on`, `y` and `n` plain when the document is
    read with the default Core schema, under which they are strings:
    `restart: no`, where 1.1 wrote `restart: 'no'`. YAML 1.1 settings and
    `%YAML 1.1` documents keep the quotes on new entries (see *Plain
    strings in edits*).
  - `to_string` quotes strings that other YAML readers read as numbers
    (`+-1`, `_1`, `0b-1`, `0X-F`), as its rule to quote every string some
    reader would read as something else says. A `%` in a tag, and an enum
    variant with non-ASCII characters written as a tag, are written as
    escapes.
  - Setting a scalar keeps its style: a block scalar stays a block scalar
    of its kind, quotes and a fitting tag stay, and so does the white space
    after `:`. A plain or quoted scalar set to text with a line break is
    written double-quoted on its line, where 1.1 wrote a `|` block.
  - A multi-line string appended after footer comment lines is a literal
    block, where 1.1 wrote it double-quoted.
  - Comments that 1.1 dropped or moved stay: the comment lines above a
    replaced root, and the footer comments of a collection whose only
    entry is removed.
- Diagnostic codes:
  - `Y1004` now means only a repeated `%TAG` handle. A second `%YAML`
    directive is the new `Y0033`.
  - `Y1005` is deprecated and no longer reported. An undeclared tag handle
    is the new `Y0035`, reported by the parser, so serde reads report it
    too.
  - `Y0034` is new: a character that cannot appear in a comment (a
    control character, U+007F or a C1 control other than U+0085).
- Errors: some invalid inputs that 1.1 accepted are now errors (see
  *Fixed*), and so is a comment holding a control character (`Y0034`);
  `to_string` refuses (`Y3015`) a `Value::Tagged` whose tag cannot be
  written so that it reads back (a non-ASCII character, or a stray `%`);
  many parse errors point at a later character
  than before; `LineIndex` character and UTF-16 columns no longer count a
  leading byte order mark.
- `oneyaml-syntax` 0.2: the event stream of some inputs changed (document
  boundaries, error positions and codes).

Highlights: `Document`, `DocumentMut` and `Stream` apply JSON Patch
(RFC 6902) and merge patches (RFC 7396) all or nothing, keeping comments
and layout, and read nodes by JSON Pointer (RFC 6901). Setting a value
now writes exactly that value and changes only its text: a block scalar
stays a block scalar of the same kind, quotes stay, and so do a tag that
still fits, the white space after `:` and the document's line breaks.
Many more edits that used to be refused now work. The parser passes long
scalars, comments and line ends sixteen bytes at a time, and parse errors
point at the character that cannot belong.

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
  - `test` compares by the YAML data model under the document's schema
    (Core by default), which departs from RFC 6902 section 4.6: `1` is not
    `1.0`, and `0x1F` is `31`. A mapping key in a compared value matches a
    JSON object key only if it is a string under that schema, so the
    integer key `1` does not match `"1"`.
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
  - `copy` and `move` write the value they read, not its text: an alias
    inside it becomes its content, and the comments inside a moved value
    and around its old entry, and its anchors, are not kept. To move an
    entry with its comments, use `NodeMut::move_entry` or `Removed`.
  - `Patch::from_json` and `MergePatch::from_json` read JSON or YAML text
    with the default settings and limits. For untrusted text, read a
    `Value` with `from_str_with` and your own `Limits`, then use
    `Patch::from_value` or `MergePatch::new`.

- `oneyaml-syntax`: `SyntaxTree::close_block_scalar` adds the missing
  final line break to a block scalar that ends the text without one, so
  that lines can go after it (`SyntaxTree::insert_line_after`).
- `oneyaml-syntax` has an `__internal-checks` feature, as `oneyaml` does:
  work counters for this project's own tests. It is not covered by
  semver and may change or go in any release; never enable it.
- `Y0033` (`DUPLICATE_YAML_DIRECTIVE`): a second `%YAML` directive in one
  document prefix. It is reported where the second directive starts.
  `Y1004` stays the code for a repeated `%TAG` handle.
- `Y0034` (`INVALID_COMMENT_CHARACTER`): a character that cannot appear in
  a comment (a control character, U+007F, or one of U+0080 to U+009F other
  than U+0085). The error is at the first such character; the comment is
  kept as written and the parse goes on.
- `Y0035` (`UNDECLARED_TAG_HANDLE`): a tag that uses a handle no `%TAG`
  directive declares (`!e!x` with no `%TAG !e!`). It is a syntax error, found
  by the parser, so the event stream, `Document` and the serde reader all
  report it the same way. It replaces `Y1005` (see *Deprecated*).

### Deprecated

- `Y1005` (`UNDEFINED_TAG_HANDLE`): no longer reported; an undeclared tag
  handle is `Y0035`. The constant and its registry entry stay, marked
  deprecated, and the number is never reused.

- `MappingRef::is_indexed`: whether a mapping's lookup index is built now. It
  never builds one.
- `oneyaml-syntax`: `SyntaxTree::text`, the tree's text as `emit` gives it,
  borrowed when the tree was not edited.

### Changed

- Appending a multi-line string to a block collection whose last entry is
  followed by its footer comment lines (comment lines indented under it, with
  no `---` after them) writes a literal block (`n: |-`) below those lines.
  It was written as a double-quoted string. Moving a literal block scalar
  to the end of such a collection keeps it a literal block too. The comment
  lines stay where they were, above the new entry.
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
  - A string is written plain or quoted as *Plain strings in edits*
    below says.
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
- A merge patch applied to a sequence writes the keys it adds under the
  rule of *Plain strings in edits* below, instead of quoting them for every
  reader.
- Replacing a root node keeps the comment lines above it (`# head` above
  `a: 1`). Before, they were dropped with the old value.
- Removing the only entry of a nested collection (which becomes `{}` or
  `[]`) leaves the collection's footer comment lines where they are, below
  the new `{}` or `[]`. Before, they were moved inside the new brackets, and
  the edit was refused when the collection also had a comment on its properties
  line. An empty `{}` or `[]`
  owns no footer, so the comment changes owner: it leads the next entry, or,
  when no entry follows, it is the enclosing collection's footer. A later
  removal of that entry takes the comment along. A root collection's footer
  comments still become the document footer.
- A merge patch (or a root replacement by a collection) on an empty document
  whose start marker line holds only properties and a comment
  (`--- !!null # c`) keeps the comment on the marker line:
  `--- # c` followed by the new mapping. Every other root that starts on the
  marker line (`--- foo # c`, `--- [a] # c`) still refuses to be replaced,
  because the comment would change owner.
- Setting the value of a flow entry written with an empty value (`{b:,
  c: 1}`) works. Before it was refused.
- Adding an entry or item after a block scalar whose text has no final line
  break (`|-`, or a keep scalar `|+` of one line of spaces) works: the
  scalar is closed with a line break first, and the new line follows. The
  input keeps ending without a line break, except when the new value is a
  multi-line string, which is written as a block scalar that ends with its
  own line break. Before, these edits were refused.
- Any number of byte order marks may precede a document (`\ufeff\ufeffa: 1`,
  marks on lines of their own, marks after `...`): every document prefix may
  start with one (YAML 1.2.2 productions 202 and 211), and the events and
  values are those of the single-mark form. A mark anywhere else, such as in
  the middle of a line or inside content, is still `Y0032`. Edits accept the
  same text.
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
- Content an edit adds now follows the document around it:
  - A new string takes the quotes of the strings at the end of the collection
    it joins (`a: "x"` then `b: "y"` gains `c: "z"`, not `c: z`); the nearest
    string decides, and a plain one means plain. A new mapping key takes the
    quotes of the keys beside it. Numbers, booleans and nulls are never
    quoted, and text that needs escapes or a block is written as it must be.
    The strings of a new collection are not copied from its neighbours.
  - Whether a new string is plain is decided as *Plain strings in edits*
    below says.
  - Nested block content copies the indentation of a nested collection even
    when 300 plain entries come before it (the first 1,024 entries are
    read, and only when the text written has a layout to copy; one line of
    text reads none, which makes inserting a scalar cheaper).
- **Plain strings in edits.** A string an edit writes (a set value, a new
  entry, key or item, a patch result) is plain when the document's schema
  reads the plain text back as that string, and quoted when it would read
  as another type or plain text cannot hold it. Under the default Core
  schema `yes`, `no`, `on`, `y` and `n` are strings, so inserting `no`
  writes `restart: no`, where 1.1 wrote `restart: 'no'`; `'123'` and
  `'true'` stay quoted. 1.1 quoted such words for YAML 1.1 readers, and
  documents meant for them keep the quotes:
  - A document read with a YAML 1.1 setting (`yaml11_booleans`,
    `legacy_octal`, `sexagesimal`, `digit_separators`, `yaml11_int_forms`,
    or `Compat::yaml_1_1()`): new string values are quoted wherever a
    YAML 1.1 reader would read another type. New keys and values set over
    an existing node are quoted when the document's schema reads them as
    another type (with `yaml11_booleans`, `no` and `y` are booleans).
  - A document that declares `%YAML 1.1`: new string values and new keys
    are quoted as in 1.1. A value set over an existing node is quoted when
    the document's schema reads it as another type: with
    `honor_version_directive` on, `%YAML 1.1` switches on the YAML 1.1
    behaviours, so `no` is quoted; without it, `no` is written plain.
- Removing an entry, or replacing a node with a kept (`|+`) block scalar, no
  longer changes the value of a kept scalar before it: the blank lines that
  follow the entry go with it (`x: |+` then `two`, an empty line and `y: 1`
  followed by an empty line used to be refused). Setting or replacing a root
  with a kept scalar before a document marker writes no extra empty line.
- `Document::remove_node` and `remove_keep_comments` now remove an entry whose
  key is a collection, block or flow, and `Removed::key` gives the key's
  source text for it (with its anchor and tag, if any). The `Removed` entry can
  be put back, into the same mapping or another, and is refused as
  `KeyExists` when the mapping has an equal collection key (the order of a
  mapping's entries, the spelling of its numbers and the aliases it names do
  not matter). A *new* entry with a collection key still cannot be inserted.
- A flow entry with an explicit key (`{? k : v}`) moved into a block mapping
  is written as the two lines `? k` and `: v`. It used to be written as
  `? k : v`, which a block mapping reads as a different key, and so changed
  the document's meaning. A key or value that spans lines is refused.
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
  written: forms with two signs (`+-1`), with a leading `_` (`_1`), and
  with a sign after a radix prefix in either case (`0b-1`, `0X-F`). Before,
  `to_string` wrote them plain.

### Fixed

- Inputs the parser accepted though they are invalid are errors now: a
  flow collection directly after a property, an empty key, an explicit key
  or a plain key (`&a[b]`, `{:{0}}`), a verbatim tag that is not a URI, and
  a second anchor on one node.

### Performance

- Edits and key lookups on a document that is parsed, edited and written out:
  - A wide mapping (64 or more keys and values) is read by a scan of its
    entries for its first few lookups and gets its lookup index only when it
    is read more than that, so the one-shot edit of a wide mapping no longer
    pays for an index it uses twice. Answers are the same either way.
  - A key lookup holds the string asked for against the key's source text
    before it decodes, resolves or reads the tag of anything: a key that
    cannot be that string is passed after a few bytes, so a 4 MB key in a
    mapping no longer costs milliseconds on every lookup of its neighbours.
    Index builds digest a plain or quoted key from
    its text without resolving it when the text alone settles that it is a
    string.
  - `Document::to_bytes`, `to_string` and `SyntaxTree::emit` copy the text of
    an unedited tree in one piece, and an edited tree's text run by run,
    instead of token by token; a UTF-8 document with nothing invalid in it
    is returned without a second copy.
  - Instructions to edit and write out (set and remove, then write the
    text): a 64 KiB mapping of 3,000 keys needs 73% fewer, a 64 KiB Helm
    values file 71% fewer, a 4 KiB list of records 29% fewer, 256 KiB of
    records 91% fewer. Writing out an unedited document is a copy: 4 KiB
    takes about a hundred nanoseconds. No row needs more. The text re-read to
    check an edit was already limited to the edited entry and its
    neighbours, and still is.

- `oneyaml-syntax`: the parser passes long runs of plain, quoted and block
  scalar text, comments and line ends sixteen bytes at a time instead of one
  character at a time, and reads the value of a quoted or plain scalar from
  such runs. Every event, span, value and diagnostic is the same as before
  (checked over the whole yaml-test-suite, the fuzz corpora and CRLF, CR,
  byte-order-mark and trailing-space variants of them, under four settings).
  In the benchmark tables of
  [`docs/performance.md`](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/performance.md)
  (CPU instructions counted by valgrind, not wall-clock time; 1.2.0 against
  the 1.1.0 tables), every parse row needs fewer instructions: block scalars
  54% fewer, long scalars 52%, Helm values 41%, comment runs 39%, deeply
  nested block mappings 30%, the Kubernetes, OpenAPI, compose, GitHub
  Actions, wide-map and whitespace rows 17% to 25%, anchors 13%, flow-heavy
  input 8% and deeply nested flow sequences 5%. Serde reads need 5% to 25%
  fewer, `Document` parses 7% to 20% fewer and edit scripts 1% to 26% fewer;
  serde writes need up to 1% more. Allocation counts are unchanged; a parsed
  `Document` of each of the three inputs below 100 KB retains up to 2% more
  bytes.

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
  non-published files or development history.
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
