# yaml-test-suite conformance: oneyaml vs. other Rust YAML parsers

Measured on 2026-10-03 at workspace revision `4c6d70c`, on the vendored
yaml-test-suite (release `data-2022-01-17`, commit `6e6c296a`; 402 cases:
308 valid, 94 error; see `crates/test-suite/data/yaml-test-suite/SOURCE.md`),
with rustc 1.97.1.

Reproduce (the tables below are this command's output, verbatim):

```sh
cargo run -p oracles --release              # the tables
cargo run -p oracles --release -- --failures libyaml   # every failure, with input/expected/actual
cargo run -p oracles --release -- --prefix saphyr      # error cases whose pre-error events differ
cargo run -p oracles --release -- --case 652Z          # one case, every parser
cargo run -p oracles --release -- --readme             # the crate README's conformance line
cargo run -p oracles --release -- --readme --ref v1.0.0   # the same, on a tree `bench pin-links` pinned to v1.0.0
```

The pass counts in `crates/oneyaml/README.md` (between
`<!-- oracles:conformance:begin/end -->`) are generated, never typed:
`--readme` writes them from this same run of the standard scorer for
`oneyaml-syntax`, the strict harness run and the release recorded in
`SOURCE.md`, and a test in `crates/oracles/tests/scores.rs` fails when
the README's line differs from what a fresh run would write.

`crates/oracles/tests/scores.rs` pins every number below, so a change shows up
in `cargo test`.

## Parsers under test

| Parser | Crate and exact version | What is measured |
|---|---|---|
| oneyaml-syntax | this workspace, `oneyaml_syntax::parse` | events, classification, round trip |
| saphyr | `saphyr-parser = "=0.1.0"` (the event parser under `saphyr`) | events, classification |
| yaml-rust2 | `yaml-rust2 = "=0.13.0"`, `parser::Parser::next_token` | events, classification |
| libyaml | `unsafe-libyaml = "=0.2.11"` (c2rust translation of libyaml at commit `2c891fc7`; what `serde_yaml` and `serde_yaml_ng` 0.10.0 parse with) | events, classification |
| yaml-edit | `yaml-edit = "=0.3.2"`, `YamlFile::parse` (+ optional `validator::Validator`) | classification, round trip |

All are the newest crates.io releases at the time of measurement. They are
dependencies of the unpublished `oracles` crate only;
`cargo tree -i oracles --workspace` shows nothing depends on it, and no
published crate's `cargo tree -e all` contains any of them.

## Results: standard criterion

The criterion other projects publish (and the one matrix.yaml.info uses):

* **valid case:** no error, and the event tree equals `test.event`;
* **error case:** the parser reports an error (any error, anywhere).

| Parser | Version | Valid | Error | Total | Error + prefix (vendored / corrected) | Notes |
|---|---|---|---|---|---|---|
| oneyaml-syntax | 0.1.1 | 308/308 | 94/94 | 402/402 | 85 / 93 of 94 |  |
| saphyr-parser | 0.1.0 | 308/308 | 94/94 | 402/402 | 55 / 55 of 94 | API lacks `...` marker, flow `{}`/`[]` markers, anchor names (compared by order); ignored |
| yaml-rust2 | 0.13.0 | 308/308 | 94/94 | 402/402 | 57 / 57 of 94 | API lacks `---` marker, `...` marker, flow `{}`/`[]` markers, anchor names (compared by order); ignored |
| libyaml (unsafe-libyaml) | 0.2.11 | 252/308 | 78/94 | 330/402 | 50 / 57 of 94 |  |
| yaml-edit | 0.3.2 | n/a | 25/94 | n/a | n/a | no event API: valid column is n/a (see classification table) |
| yaml-edit + Validator | 0.3.2 | n/a | 94/94 | n/a | n/a | no event API: valid column is n/a (see classification table) |

## Results: classification and round trip

Every parser, including yaml-edit, can be held to these.

| Parser | Accepts valid | Rejects invalid | Both | Round trip (valid) |
|---|---|---|---|---|
| oneyaml-syntax | 308/308 | 94/94 | 402/402 | 308/308 |
| saphyr-parser | 308/308 | 94/94 | 402/402 | not measured |
| yaml-rust2 | 308/308 | 94/94 | 402/402 | not measured |
| libyaml (unsafe-libyaml) | 257/308 | 78/94 | 335/402 | not measured |
| yaml-edit | 308/308 | 25/94 | 333/402 | 308/308 |
| yaml-edit + Validator | 290/308 | 94/94 | 384/402 | 308/308 |

## Our strict score (not comparable; shown separately)

oneyaml-syntax strict (contract + exact error prefixes + pinned codes):
**401/402** (valid 308/308, error 93/94; 9 judged against corrected expectations).

The one failure is 4H7K (`[ a, b, c ] ]`): its recorded tree ends the
document (`-DOC`) before the error on the extra `]`, and we report the `]`
with the document still open. No timing rule was adopted for it, because
the recorded trees do not agree with each other. 9JBA
(`[ a, b, c, ]#invalid`) records no `-DOC`, although the reference parser
emits one there too. KS4U (a flow root followed by `invalid item` on the
next line) records no `-DOC`, although libyaml, yaml-rust2 and saphyr all
emit one. A rule that yields `-DOC` for 4H7K alone would be fitted to that
input. The case is an honest failure.

This is `cargo run -p test-suite -- --producer parser`: every stream must also
pass the lossless event contract (`check_contract`), error cases must emit
exactly the `test.event` prefix before the first error, and every error case's
diagnostic codes are pinned. No competitor emits our lossless events, so none
can be scored this way.

## Known deviations

This section is the one home for what oneyaml reads differently from
YAML 1.2.2; the README and the migration guide link here.

**With default settings: none known.** The one strict-score failure
above (4H7K) is about *when* an event is reported on invalid input, not
about what any input means. Cases whose recorded expectations are
themselves defective are corrected, with reasons, in
[`crates/test-suite/known-defects.txt`](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/crates/test-suite/known-defects.txt)
(see *Methodology*).

**Opt-in departures.** Every behaviour that departs from YAML 1.2.2 is
a named `Settings` method, **off by default**:

| Setting | What it changes | On in |
|---|---|---|
| `merge_keys` | `<<` merges mappings (YAML 1.1 merge-key extension) | `Compat::yaml_1_1()` |
| `yaml11_booleans` | `yes`/`no`/`on`/`off`/`y`/`n`, … are booleans | `Compat::yaml_1_1()` |
| `legacy_octal` | `0777` is octal | `Compat::yaml_1_1()` |
| `sexagesimal` | `12:30` is a base-60 number | `Compat::yaml_1_1()` |
| `digit_separators` | `1_000` is a number | `Compat::yaml_1_1()` |
| `yaml11_int_forms` | `0b1010` and signed hexadecimal (`-0x1F`) are integers | `Compat::yaml_1_1()`, `Compat::serde_yaml()` |
| `honor_version_directive` | `%YAML 1.1` switches its document to the `Compat::yaml_1_1()` behaviours (otherwise: read as 1.2, with a warning) | none |
| `resolve_tagged_variant_content` | a typed enum reads `!Square 2.0` as `Square(2.0)` | `Compat::serde_yaml()` |
| `lenient_inline_colons` | a same-line `: ` inside an unquoted block value is text, always with a warning (otherwise: error `Y0027` with a quoting fix) | none |

The YAML 1.1 settings only add meanings on top of the Core schema. The
settings themselves are documented on
[`Settings`](https://docs.rs/oneyaml/latest/oneyaml/struct.Settings.html);
how they differ from `serde_yaml`'s reading is in the
[migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/docs/migrating-from-serde-yaml.md#divergences).

**Known defect.** Under `lenient_inline_colons` only, one rare shape of
recovered input (a failed directive block next to a byte order mark) can
let an edit through that reads back differently; the fuzzers count it,
and it is described in
[crates/fuzz/README.md](https://github.com/recursive-ai-oss/oneyaml-rust/blob/main/crates/fuzz/README.md).

## Methodology

* **One scorer for everyone.** `test_suite::standard::score` runs any
  `Oracle` over the suite. Each adapter only converts the parser's own events
  to suite-notation lines; scoring, normalisation, and panic handling are
  shared. A panic is caught and scored as a failure (a panic is not a reported
  error). So is a run that the adapter cuts off at its event cap
  (100,000 events) before the parser reaches the end of the stream or
  reports an error: that run is "truncated", not a rejection. There were no
  panics and no truncated runs.
* **Normalisation by API profile.** Some event APIs cannot express parts of
  the notation (saphyr: `...`, flow style, anchor names; yaml-rust2: also
  `---`). Each adapter declares a `Profile`; only the absent information is
  dropped, from **both** the expected and actual lines. Anchors are then
  compared structurally: `&name` becomes `&N` by definition order and
  aliases point at the latest definition of that name. The line tokeniser
  only reads properties before the value, so a `&` inside a scalar value is
  never renumbered. oneyaml-syntax and libyaml are compared with nothing
  dropped. So saphyr's and yaml-rust2's 402/402 are **weaker claims** than
  ours or libyaml's: they do not cover document markers, flow/block style, or
  anchor spelling.
* **"Error + prefix" (not part of the standard verdict).** For rejected
  error cases, it checks that the events emitted before the error equal
  `test.event`: first the vendored file, then with the 9
  `known-defects.txt` corrections applied. This column exists so the
  corrections cannot hide anything. **The corrections cannot affect the
  standard columns:** all 9 corrected cases are error cases, and the standard
  error verdict ignores events. For us the vendored and corrected results
  differ on exactly 8 of the 9 corrected cases (85 vs 93, asserted in
  `crates/test-suite/tests/standard.rs`). The ninth, 4H7K, misses both, see
  above. For streaming parsers the prefix
  mostly measures lookahead (how far they read before reporting), so read it as
  information, not correctness.
* **yaml-edit.** It has no event API, and no event mapping was invented for
  it. What it does expose is measured: whether `YamlFile::parse` reports
  errors, and whether `parse(input).tree().to_string()` is byte-identical to
  the input for valid cases. Its parser is lenient by design and it ships a
  separate strict `validator::Validator`, so a second row adds
  error-severity violations from `Validator::new().validate_syntax(root)`
  (warnings are ignored). That is the documented whole-tree entry point
  and includes the directive checks.
* **Our round trip** is the concatenation of every event's source slice
  (contract C1), compared byte for byte with `in.yaml`.
* **Inputs.** All 402 `in.yaml` files are UTF-8 and none has a BOM. libyaml's
  own encoding detection is left on. Forcing UTF-8, as `serde_yaml` does, gives
  the same scores.

## Adapter checks

Every competitor failure was classified by hand from `--failures` output:

* **libyaml:** its 72 failures are 51 valid inputs rejected with libyaml
  errors (e.g. 2JQS "did not find expected key", 2SXE anchors containing `:`,
  tab-related cases, reserved directives, 4MUZ/5MUD flow `:` on the next
  line), 16 error inputs accepted, and 5 valid inputs accepted with different
  events. All 5 were checked by hand and all are libyaml's behaviour:
  652Z and HM87/01 (`?foo` in flow read as an explicit key), Y2GN
  (`&an:chor` split at the `:`), and JEF9/02 and L24T/01 (trailing spaces in
  a block scalar at EOF without a final newline). As an independent check,
  our libyaml numbers equal matrix.yaml.info's `c-libyaml-event` row exactly
  (below). That row is scored against the same suite commit with its own
  printer.
* **saphyr, yaml-rust2:** there are no standard failures to classify. A
  systematic adapter bug in tag, anchor, or scalar-style formatting would
  have shown up as valid-case mismatches. Tag handling was spot-checked on
  6CK3 (`%TAG`, `!local`, `!e!tag%21`), and a sample of prefix mismatches
  (2CMS, 3HFZ, 4H7K) was checked: all are lookahead or timing differences.
* **yaml-edit:** the 69 accepted invalid inputs have no parse errors, and the
  18 valid inputs the Validator rejects are the Validator's own messages:
  tabs flagged as indentation (6HB6, DK95/00, /04, /05, Y79Y/002),
  "Directive without document content" (MUS6/02–06, DK95/07), duplicate or
  ordering rules on multi-document directives (5TYM, W4TN, 6WLZ, 9WXW),
  2JQS "Duplicate key" on two empty keys, 9MQT/00 `...` in a quoted scalar,
  and C4HZ indentation.
* **No adapter bugs were found.** No event conversion was changed after the
  first full run. The only later addition was the `yaml-edit + Validator` row. The adapters for libyaml, saphyr, and yaml-rust2 each have a
  unit test rendering anchors, tags, styles, and an error.

## Published figures (matrix.yaml.info)

matrix.yaml.info (fetched 2026-09-27) was generated from yaml-test-suite
commit `6e6c296a` (2022-01-17), the same data we vendor. It compares full
event output, and its error criterion is "reports an error", like ours.

| matrix.yaml.info row | Valid ok | n/i | diff | error | Invalid pass / fail | Total pass | Our figure |
|---|---|---|---|---|---|---|---|
| `c-libyaml-event` (C libyaml) | 252 | 9 | 5 | 42 | 78 / 16 | 330 | libyaml: 252 / 78 / 330, **identical** (our 5 "accepted, events differ" = their 5 diff; our 51 rejections = their 9 n/i + 42 error) |
| `rust-yamlrust-event` (yaml-rust, the original crate) | 251 | – | 28 | 29 | 66 / 28 | 317 | not comparable: yaml-rust2 is the maintained fork, 402/402 here (with its profile's normalisation) |

saphyr, yaml-rust2, yaml-edit, and oneyaml are not in the matrix. Its
reference parser and libfyaml score 402/402 there.

**Where our numbers differ from matrix-style reporting, and why:** they don't,
under the standard criterion. The known-defect corrections change only
error-case prefixes, which the standard criterion does not check. Our 402/402
is the same number a matrix run would report. The only place the
corrections show is the informational prefix column (85 vendored vs 93
corrected for us).

## Observations

* saphyr-parser 0.1.0 and yaml-rust2 0.13.0 now classify every case correctly
  and match every valid event tree, as far as their event APIs can express
  it. yaml-rust (the original) was at 317 on the matrix.
* yaml-rust2 and libyaml reproduce the recorded 4H7K prefix: they emit
  `-DOC` before failing on `[ a, b, c ] ]`. The spec reference parser
  (YAML::Parser 0.0.5 via YAML::PP::Ref 0.02) emits it too. The correction
  keeps that `-DOC` and restores only the missing `[]` marker. yaml-rust2
  cannot express the marker, so both its counts include 4H7K (57 / 57).
  libyaml can, so only its corrected count includes 4H7K (50 / 57). We
  do not emit the `-DOC` (see "Our strict score"), and saphyr reports the
  error before the sequence.
* libyaml 0.2.5-era behaviour is unchanged: 330/402, the same as the C
  library on the matrix. This is the parser behind `serde_yaml` and
  `serde_yaml_ng`.
* yaml-edit round-trips every valid input byte for byte, as we do. Its parser
  alone rejects 25/94 invalid inputs. With the Validator it rejects all 94
  but also 18 valid ones.
