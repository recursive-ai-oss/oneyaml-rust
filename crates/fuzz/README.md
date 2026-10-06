# fuzz — coverage-guided fuzzing for oneyaml (unpublished)

The evidence behind oneyaml's "no panics on any input" claim: ten
libFuzzer targets covering parsing, raw-byte decoding, serde reads and
writes, `Document` and `Stream` edits, comments, diagnostics rendering
and the diagnostics cap, each checking oneyaml's invariants, not just the
absence of crashes. What was found is under [Findings](#findings); what
the fuzzers don't reach is under
[Coverage notes](#coverage-notes-what-the-fuzzer-does-not-reach). To
replay the committed corpora without a fuzzer, run `cargo test -p fuzz`;
to fuzz, see [Commands](#commands).

Every target asserts that no input panics, aborts or overflows the stack,
that no public API panics or returns an internal error, and the
invariants listed below; a refused input (`Err`) passes as long as the
invariants around it hold.

## Tool: libFuzzer on stable rustc

`libfuzzer-sys` (feature `libfuzzer`, off by default) links libFuzzer,
built from source with the host C++ compiler. Coverage instrumentation
uses stable codegen flags (`-Cpasses=sancov-module` and the
`-sanitizer-coverage-*` LLVM options: what `cargo fuzz` passes), so no
nightly toolchain and no `cargo-fuzz` are needed. There is no
AddressSanitizer (nightly-only `-Zsanitizer`): every crate under test is
`forbid(unsafe_code)`, so memory errors are not the failure mode; panics,
aborts, stack overflows, timeouts and OOMs are, and libFuzzer catches all
of them. The fuzz build turns on debug assertions and overflow checks, so
a failed internal `debug_assert!` or an arithmetic overflow is a finding.

## Targets

| target | input | checks |
|---|---|---|
| `parse` | YAML text (lossy UTF-8) and raw bytes | the event stream's contract; streamed = collected events; `SyntaxTree` emits the input byte for byte and has errors exactly when the events do; the decoder re-encodes raw bytes exactly; default, `untrusted()`, `lenient_inline_colons` |
| `consistency` | YAML text | the serde/`Document` consistency checks of `bench/tests/consistency.rs` (byte-identical `Document`/`Stream`/per-document emit, tree vs serde, strict `Document` vs `from_str`, typed maps, spans, navigation, stream documents) under `untrusted()`, default + `merge_keys` + `LastWins`, `Compat::yaml_1_1()` + `untrusted()`, `lenient_inline_colons` + `untrusted()`; disagreements pass only if classified expansion-only |
| `from_slice` | raw bytes | `Document`/`Stream::from_slice_recovering` give the bytes back, per document too; tree ≡ `from_slice`; valid text reads the same in UTF-8/16LE/16BE/32LE/32BE with and without BOM; default and `untrusted()` |
| `serde` | YAML text | `bench/tests/value_differential.rs`: `from_value_with` ≡ `from_str_with` over ~40 target types (scalars, collections, the four enum representations, flatten, untagged, tuples, `char`, `u128`/`i128`, the k8s/GHA/compose/OpenAPI models), node reads ≡ whole-document reads, `serde_json` round trip; the four reading settings |
| `edit` | YAML text | an edit script, driver and seed from the input's FNV hash: `edit_fuzz::run_one` (`Document` edits), `comment_fuzz::run` / `run_stream` (comment API), `stream_edit_fuzz::run` (`Stream` edits); every oracle of those fuzzes (serde and `Document` agree after each accepted call, refused calls change nothing, handles, a meaning model, comment ownership by the rulebook, clones) |
| `render` | text, NUL, diagnostic bytes | `Render` on arbitrary diagnostics (out-of-range, inverted, `usize::MAX` spans; labels, help, suggestions) against its source, another source, and a wrong index; `Suggestion::apply`; every real diagnostic and error renders the same with and without an index |
| `ser` | YAML text | every `Value` the input reads as (default and 1.1 readings) is written under each output setting and each reading setting, reads back equal, writes identically again, and `Document` reads the same; refusals only `Y2xxx` or `Y3015` |
| `tree_edit` | YAML text | the edit battery of `oneyaml-syntax/tests/reparse_sweep.rs` (removal, copies, moves, rolled-back groups, inserted white space, tabs, breaks, comment lines, entries and markers, token rewrites, node replacements) at sampled elements, each on a fork, then a chain of 8 on one tree; default and `lenient_inline_colons`; every accepted edit equals a re-parse of its text (`compare_trees`), recovered input included; no `Internal` |
| `cap` | YAML text | `bench/tests/cap_invariance.rs`: every meaning-level outcome (reads, strict and recovering parses, `has_errors`, error regions, a sample edit script per document) is the same under `max_diagnostics` 0, 1, 2, 16, the profile's and none; the capped `Stream`/`Document` lists are the uncapped list cut as documented (prefix, first error, one exact `Y2010` summary at the first left out); a 4-step edit script on a capped and an uncapped stream is accepted alike, and the capped diagnostics then equal a re-parse with the same limits; default, `serde_yaml`, lenient + merge keys, all `untrusted()` |
| `handles` | YAML text | the node-handle API: setting each node to its own value (`NodeMut::set`, the parent's `insert`/`replace_at`, `set_root`) changes no byte and keeps the handle, unless the node is or holds an alias, a merge key (with `merge_keys` on: an untagged plain `<<`, or any scalar key tagged `!!merge`) or repeated keys; each mapping value handle's `set_leading`/`set_trailing`/`set_blank_lines_before` equal the same call on its key, and `clear_comments` equals clearing the key's then the value's comments as one edit; serde and `Document` agree after each accepted edit; no `Internal` |

The oracles are the deterministic fuzzes of `bench/tests`, included by
path (`src/reuse.rs`): one source of truth for each invariant.

**The known exception.** Under `lenient_inline_colons`, a document
holding a failed directive block (`Y0001`) whose diagnostics end in a
`Y0028` warning can hide an unclosed quote's read-on past a byte order
mark from the edit validator, so an edit there may be accepted and read
differently after a re-parse. The edit oracles that read lenient trees
(`stream_edit_fuzz`, `tree_edit`, `handles`) excuse exactly that shape
(`bench/tests/known/mod.rs`: lenient settings, a byte order mark in the
text, a document whose diagnostics hold a `Y0001` and end in a `Y0028`),
print `KNOWN-EXCEPTION lenient-directive-bom` on stderr for each hit and
count them (`fuzz::known_exception_hits`, printed by the replay);
`tests/known_exception.rs` pins the filter to a reproducing input.

## Commands

```sh
crates/fuzz/fuzz.sh build                        # instrumented binaries (target/fuzz)
crates/fuzz/fuzz.sh run parse 3600 4             # 1 h, 4 fork workers; findings in target/fuzz/work/parse-artifacts
crates/fuzz/fuzz.sh repro parse <file>           # re-run an artifact
crates/fuzz/fuzz.sh cmin parse                   # merge work/parse into corpus/parse: edge-only set cover
cargo run -p oracles --example dump_migration_corpus -- target/fuzz/migration
crates/fuzz/fuzz.sh seed target/fuzz/migration   # rebuild every corpus from the seeds
cargo test -p fuzz                               # smoke: replay corpus/ and regressions/ (no fuzzer)
cargo run -p fuzz --bin fuzz-seed -- run <target> <file>...   # one input, uninstrumented
```

`fuzz.sh run` uses fork mode with `-ignore_crashes/-ignore_timeouts/-ignore_ooms`
(keeps going, keeps every artifact), the dictionary `dict/yaml.dict`,
`-timeout=10`, `-rss_limit_mb=4096`, and a per-target `-max_len` (8192
parse/from_slice/render, 4096 consistency/serde/ser/cap, 2048
edit/tree_edit/handles). `ONEYAML_FUZZ_TARGET_DIR` moves the build (and
`ONEYAML_FUZZ_WORK` the work directories) out of `target/fuzz`.

## Layout

* `corpus/<target>/` — minimised corpora (libFuzzer `-merge=1` when
  seeded; `cmin` keeps an edge-only set cover, `-set_cover_merge=1
  -use_counters=0`: the fewest inputs that cover every edge, about
  0.5 MB per target; hit-count features are not kept, and a full-feature
  corpus regrows in `work/<target>` as `fuzz.sh run` fuzzes from it), seeded from
  yaml-test-suite, the benchmark corpus, the adversarial corpus, and the
  migration corpus (`oracles/tests/compat_corpus`), each cut to the
  target's `-max_len`, plus what the fuzzer found.
* `regressions/<target>/` — one repro per fixed finding, named after it;
  replayed by every `cargo test -p fuzz`.
* `dict/yaml.dict` — YAML tokens.

## Coverage notes: what the fuzzer does not reach

* **Edit scripts are 12 calls long** (`STEPS` in `src/targets.rs`), with
  the driver and every choice seeded from the input's hash; the input
  only picks the document. Longer histories are the deterministic
  fuzzes' job (`bench/tests/edit_fuzz.rs` and friends, and
  `oneyaml-syntax/tests/edit_history.rs`).
* **Reader and writer I/O errors**: `from_reader`/`to_writer` are not
  targets, and no target injects a failing `io::Read`/`io::Write`; the
  byte paths behind them are `from_slice` and `ser`.
* **The `miette` feature** is not enabled in the fuzz build, so
  `miette::Diagnostic` for `Error` and `DiagnosticReport` are not
  fuzzed; `render` covers the dependency-free rendering.
* **Typed reads skipped by `serde`**: an expansion bomb within the
  default alias budget (refused as a limit, `Y2xxx`, under `untrusted()`)
  gets no typed reads, only its `Value` read and JSON round trip; the ~40
  whole-input typed reads run only on values of at most
  `TYPED_CHECK_NODES` (16,384) nodes, and the per-node reads on at most
  `NODE_CHECK_NODES` (1,024), so that alias-heavy inputs stay well under
  the 10 s libFuzzer timeout. These are oracle costs, not library ones:
  each read is bounded by the alias budget, and the `consistency` target
  reads those inputs.
* **`cap` is slow on expansion bombs**: `cap_invariance::outcome` reads
  the input about 30 times (strict and recovering parses, `Value` and
  typed reads, a sample edit script per document) under each of six
  `max_diagnostics` caps, all with `untrusted()` alias budgets. A
  billion-laughs input within those budgets makes every `Value`/serde
  read expand up to the budget (100k nodes, 8 MiB), so such an input
  takes seconds instrumented, beside milliseconds for others. Each read
  is bounded and linear in what it expands; the cost is the oracle's
  repetition of bounded reads, so libFuzzer may report such an input as
  a slow unit or a timeout, never a crash. Bounding it would mean
  skipping the cap check on exactly the alias-heavy inputs whose budget
  refusals it compares.

## Findings

Two fuzzing campaigns ran before 1.0.0; the second ran all ten targets
for about 15 CPU-hours and 11.9 million executions (per-target figures
below). Every library bug they found is fixed in 1.0.0, except the
[known open issues](#known-open-issues), with its repro in `regressions/<target>/` and a test in
`oneyaml/tests/fuzz_findings.rs`; the rest were bugs in the test oracles
or the harness, fixed there.

**Library bugs found and fixed**, by area:

* **Byte order marks**: documents behind a byte order mark could not be
  edited; removing an entry after a comment and a byte order mark, or
  `set_root` under such a comment, returned `Internal`; a byte order mark
  counted as a column for the comment rules.
* **Comments under edits**: a refused node edit dropped explicitly
  re-attached comments; `remove_keep_comments` left a comment behind;
  inserting into, or replacing, a single-pair flow mapping (`[k: v]`)
  re-owned or dropped its comments (now refused or kept, respectively);
  `set_anchor(None)` could move a same-line comment to another owner (now
  refused with `CommentReattaches`).
* **Edits next to errors and recovered input**: an edit beside an error
  region changed which error the text read; carried comment lines that no
  longer read as comments returned `Internal` (now `NotWellFormed`);
  removing a root's anchor line after a tab was accepted though the text
  then read as an error (now refused).
* **Serde and output**: a stray `...` between a directive and `---` hid
  the directive's error from the serde read; enum mappings under
  `LastWins` or merge keys counted raw entries (duplicates are now
  `Y1013` for every typed target); a string ending in two or more line
  breaks, written last, lost one; a final line of spaces after a kept
  (`+`) block scalar moved under edits.
* **Equal-value sets**: with `merge_keys` on, setting a node holding a
  `<<` written as a value (not a key) to its own value rewrote it;
  `clear_comments` with nothing to clear was refused beside an error
  region.

**Unexplained.** One libFuzzer crash in the first campaign
(`regressions/serde/u1-alias-keys-instrumented-crash`: nested mappings
with anchored scalars and alias keys) has never reproduced, over 1.45
million further executions, the instrumented binary at stack limits from
8 MiB down to 256 KiB, the uninstrumented `fuzz-seed run`, and the
`cargo test -p fuzz` replay. Its log was not kept, so the cause is not
known. It stays in `regressions/`, so a recurrence fails
`cargo test -p fuzz`.

### Known open issues

None of these panics or corrupts a value read; each affects an edge of
the edit validator or the order of diagnostics. Their repros are not in
`regressions/`, which must replay green.

* **Properties split over two lines** (`oneyaml-syntax`):
  `SyntaxTree::insert_before(Tag, "\n")` in `": &a !\n :"` is accepted
  (`": &a \n!\n :"`) but a re-parse reads a different structure: the
  line break moves the tag to column 0, where it starts another node,
  outside the window the validator checks.
* **White space before content after a tab** (`oneyaml-syntax`):
  `SyntaxTree::remove_range` of a root's anchor line after a tab
  (`"\t&n1\n p: 1"` → `"\tp: 1"`) is accepted, and the re-parse has an
  error the tree lacks. `oneyaml`'s edits avoid this path (see above).
* **Capped diagnostics summary position**: on
  `"&l {0 \n--- !\u{feff}v uuuuuuuuuuuuu"` read as a recovering `Stream`
  under `max_diagnostics(1)`, the `Y2010` "more diagnostics suppressed"
  summary is placed one entry late, because the parser reports the byte
  order mark inside a tag before the tag's own error.
* **Diagnostic order after an edit**: with merge keys and `LastWins`,
  `"m:\n  <<: {a: 1}\n  <<:\nz: 0\n"` parses with `[Y1014, Y1016]` at the
  same position; after any accepted edit, `diagnostics()` lists them as
  `[Y1016, Y1014]`, while a re-parse gives the parse order.

### Campaign numbers

The second campaign: single-process libFuzzer, up to 4 at once, one CPU
each; executions summed over every run of a target, coverage (`cov`/`ft`)
from the first `INITED` to the last status line.

| target | executions | CPU-h | cov/ft start → end | artefacts |
|---|---:|---:|---|---:|
| `parse` | 1,129,719 | 0.69 | 4241/18788 → 4524/28572 | 0 |
| `consistency` | 197,027 | 0.81 | 12331/46756 → 13024/66391 | 3 |
| `from_slice` | 767,117 | 1.00 | 8964/31728 → 9518/48589 | 0 |
| `serde` | 509,460 | 1.31 | 24218/106704 → 25365/144339 | 0 |
| `edit` | 1,283,026 | 3.10 | 34355/180415 → 35587/214792 | 24 |
| `render` | 5,439,762 | 0.47 | 8118/31052 → 8855/48229 | 0 |
| `ser` | 867,463 | 0.61 | 8313/34019 → 8639/45553 | 0 |
| `tree_edit` | 607,558 | 1.84 | 8359/55842 → 8571/62512 | 1 |
| `cap` | 419,785 | 2.61 | 21689/96649 → 23260/124464 | 6 |
| `handles` | 632,400 | 2.44 | 19555/105983 → 21935/142669 | 4 |
| **total** | **11,853,317** | **14.88** | | **38** |

Every artefact was re-run uninstrumented on the final code; all pass
except the known open issues' repros, and the slow-unit and timeout
artefacts run in time. The known exception had no hit in the campaign.

**Corpus**: after `fuzz.sh cmin` of every target (edge-only set cover):
5,300 files, 3.38 MB. Eleven inputs that
`oneyaml-syntax/tests/reparse_sweep.rs` reads by name were put back after
the merge dropped them.
