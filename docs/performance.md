# Performance

What every layer of oneyaml costs against the other Rust YAML libraries,
on a fixed, generated corpus of configuration-file shapes. Looking for
the summary? The
[README](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/crates/oneyaml/README.md)
shows five workloads against the fastest feature-complete competitor;
[Results at a glance](#results-at-a-glance) below says where oneyaml wins
and where it loses, and [Regenerate](#regenerate) has the commands that
produce every table here.

This page covers:

- **the parser**: `oneyaml-syntax`'s lossless event stream against
  saphyr-parser, yaml-rust2, libyaml and yaml-edit, and its syntax tree
  ([Parser](#parser));
- **serde**: reading ([Serde read](#serde-read)) and writing
  ([Serde write](#serde-write)) against serde_yaml_ng, serde_norway,
  serde-saphyr and serde_yaml2, with serde_json as a reference;
- **`Document`**: parse, emit and edits against yaml-edit
  ([`Document`](#document)).

## How to read these numbers

- **The primary metric is CPU instructions**, counted by valgrind
  (callgrind), per input byte (Ir/B) or per operation. Instruction counts
  are deterministic for a given build: repeated runs of the same binary
  give the same count (to within 0.1% where a hash map with per-process
  random keys is involved), so a difference between two cells of one run
  is real, not noise. Rebuilding (a different toolchain, or a dependency
  rebuilt with other code generation) can shift a library's counts by a
  few percent, so compare numbers from one run, as every table here
  does. Counts cover user-space work only, not cache misses, page faults
  or kernel time.
- **Allocations** (blocks, bytes, peak heap) are counted by valgrind DHAT
  and are deterministic too.
- **Wall-clock times are informational.** They were taken on a shared,
  loaded 16-core desktop (each wall-clock table prints the load average),
  so read them as ratios and orders of magnitude. Where wall-clock and
  instructions point in different directions, both are shown.
- **Ratios are oneyaml ÷ competitor** in these tables, so **below 1.0×
  oneyaml is cheaper** (the README inverts them into "× faster").
- **Serde ratios are against the fastest feature-complete library on
  each row**: serde_yaml_ng, serde_norway and serde-saphyr all support
  aliases, multi-document streams and merge keys. serde_yaml2 does not
  (no multi-document API, no merge keys); its column and ratio are shown,
  marked *partial, no target*, but it never decides a verdict.
- **Every competitor's output is checked before it is measured** (the
  correctness-guard tables): a library that rejects an input, gives a
  different result, or can't do the equivalent work gets a `rejects`,
  `differs` or `n/a` cell instead of a number.

**Which build measured a table matters.** Tables measured up to and
including oneyaml 1.0.1 come from the default release build; their
`environment` table has no `Build` row. Later tables come from the
`measure` build (release settings in a single codegen unit, see
[Regenerate](#regenerate)). One codegen unit changes how the compiler
optimizes every library, the comparators included, so the switch alone
moves counts by several percent on some rows: serde_yaml_ng's typed read
of compose-merge takes about 5.5% more instructions (1,521,971 →
1,606,108) while oneyaml's stays within 0.2%, which lowers that row's
ratio by about 5% with no change to oneyaml. Compare numbers only
between tables whose `environment` table records the same `Build`.

The run's provenance (the README summary's method strip is read from
it):

<!-- bench:environment:begin -->
| Measured | |
|---|---|
| Revision | oneyaml 1.0.1 release tree |
| Date | 2026-10-06 |
| CPU | AMD Ryzen 9 7950X3D 16-Core Processor |
| OS | Ubuntu 26.04 LTS (x86_64) |
| rustc | rustc 1.97.1 (8bab26f4f 2026-07-14) |
| valgrind | valgrind-3.27.1 |
| Comparators | serde_yaml_ng 0.10.0, serde_norway 0.9.42, serde-saphyr 1.3.0, serde_yaml2 0.1.3, serde_json 1.0.151, yaml-edit 0.3.2, saphyr-parser 0.1.0, yaml-rust2 0.13.0, unsafe-libyaml 0.2.11 |
<!-- bench:environment:end -->

## Results at a glance

**Where oneyaml wins**

- **Parsing.** The lossless event parser uses fewer instructions per
  byte than saphyr-parser, yaml-rust2 and libyaml on every data-heavy
  real-world input, although it does more work: it also produces events
  for every comment, space and indicator. Against yaml-edit, the other
  lossless parser, it is cheaper on every input yaml-edit accepts
  ([Instructions](#instructions)).
- **Serde writes.** `to_string` uses fewer instructions than every
  feature-complete writer on every input, typed and untyped
  ([Serde write](#serde-write)).
- **Serde reads** use fewer instructions than the fastest
  feature-complete reader on most `value` and `typed` rows, and at most
  1.25× its instructions on every row but one
  ([Serde read](#serde-read); the targets are in
  [Targets and results](#targets-and-results)).
- **`Document`.** Parsing a `Document` costs fewer instructions than
  yaml-edit's parse on every input, although oneyaml's parse also checks
  the document's meaning (duplicate keys, merge keys, aliases) and
  yaml-edit's checks syntax only. Writing an unchanged document back is
  an order of magnitude cheaper, byte-identical on every input, and a
  parsed document keeps less memory than yaml-edit's tree
  ([`Document`](#document)).
- **Edits stay local.** An edit script costs about the same number of
  instructions on a 2.4 KB file and a 10 MB one, where yaml-edit's grows
  with the document ([Edits](#edits)). helm-values costs more per script
  because each edit there decides the owners of a long run of comment
  lines, but that cost doesn't grow with the file either.
- **Allocations.** The parser borrows from the input and decodes scalars
  on request, so it makes a handful of allocations per input where event
  parsers make one or more per event; serde reads into borrowing types
  stream with O(depth) state ([Allocations](#allocations),
  [serde allocations](#allocations-1)).
- **No pathological input.** oneyaml's instruction count grows linearly
  with input size on every stress shape, including 512k levels of flow
  nesting, which saphyr, yaml-rust2 and yaml-edit reject
  ([Linear scaling](#linear-scaling)).

**Where oneyaml loses or is at parity**

- **Typed reads of files with merge keys cost the most.**
  compose-merge `typed` takes 1.26× serde_yaml_ng's instructions, the
  one serde row above 1.25×. Merged mappings are buffered so that
  later keys can override merged ones, and that costs instructions,
  allocations and more wall-clock time than instructions suggest.
- **`sparse` reads** (a struct that reads a few fields) cost more than
  serde_yaml_ng on several rows: oneyaml still resolves and checks the
  content a struct ignores (duplicate keys, merge keys), so a typed read
  accepts exactly the documents a `Value` read accepts.
- **In wall-clock time, serde reads are at parity or slower** than
  serde_yaml_ng and serde_norway on most rows, including rows where
  oneyaml uses fewer instructions
  ([serde wall-clock](#wall-clock-informational-only-1)). If wall-clock
  read speed is what matters most to you, measure on your own data.
- **Edits on documents below a few kilobytes** (compose-merge, 2.4 KB)
  cost more than yaml-edit's: each edit has a fixed cost for deciding
  which entry owns each nearby comment and checking that the edited text
  still reads as the tree says, which exceeds yaml-edit's whole-document
  cost on a small file. That cost doesn't grow with the document, so
  the loss is limited to small files.
- **Comment- and whitespace-heavy inputs** (helm-values, comment-heavy,
  whitespace) cost slightly more instructions to parse than the fastest
  event parser, which skips comments and spaces; oneyaml produces an
  event for each of them.
- **serde_yaml2** is cheaper than oneyaml on the rows it can run, and
  **serde_json** reading the equivalent JSON is several times cheaper
  than any YAML library; both do less work (see their columns).

## Targets and results

The performance targets oneyaml holds itself to, and how the tables
below measure them. "met" means every row of the measure is within the
target.

| Target | Measured by | Result |
|---|---|---|
| Typed serde reads (`from_str::<T>`) ≤ 1.25× the instructions of the fastest feature-complete YAML serde library | [serde read instructions](#instructions-1), `typed` and `sparse` rows | **missed** on one row: compose-merge `typed` (merge keys) is 1.26×; met on every other row |
| `from_str::<Value>` ≤ 1.25× | [serde read instructions](#instructions-1), `value` rows | met on every row |
| `to_string` ≤ 1.25× | [serde write instructions](#instructions-write) | met on every row; cheaper than every feature-complete writer |
| `Document` parse at parity with yaml-edit | [`Document`](#document), parse | met on every row |
| Unchanged `Document` emit at parity with yaml-edit | [`Document`](#document), emit | met on every row, byte-identical output |
| Edit operations at parity with yaml-edit | [Edits](#edits), script | met, except on documents below a few kilobytes (compose-merge); see above |
| Local edits never cost O(document) | [Edits](#edits), script instructions and wall-clock | met: a script costs about the same from 2.4 KB to 10 MB |
| A parsed `Document` retains ≤ 8× its input (real-world inputs of 100 KB or more) | [`Document`](#document), memory | met; smaller files and dense synthetic shapes are reported, without a target |
| Serde reads into borrowing types use O(depth + anchored content) memory | [serde allocations](#allocations-1), `sparse` rows | met |
| Parsing is O(n) for every input | [Linear scaling](#linear-scaling) | met |

## Regenerate

You need valgrind 3.27 or later on `PATH` (or pass `--valgrind PATH`)
and, for the wall-clock tables, a machine as quiet as you can get.

```sh
cargo build -p bench --profile measure
target/measure/bench update docs/performance.md --timeout 300    # every table (~24 min with --jobs 4)
target/measure/bench update docs/performance.md --section wall   # one section
target/measure/bench table scaling                               # print a table, don't write
target/measure/bench probe yaml-edit deep-flow 10                # one parser, one input
target/measure/bench serde-check serde_yaml2 value gha-workflow  # one read-guard verdict
target/measure/bench ser-check serde-saphyr typed k8s-multi      # one write-guard verdict (+ output bytes)
target/measure/bench probe serde:oneyaml:typed openapi-10m 1     # one serde read probe
target/measure/bench probe ser:oneyaml:typed openapi-10m 1       # one serde write probe
target/measure/bench probe doc:yaml-edit:script openapi-100k 1   # one Document probe
target/measure/bench readme                                      # README summary + chart only, from the tables below
```

- **Measure with the `measure` build only.** The `measure` profile
  (workspace `Cargo.toml`) is the release profile in a single codegen
  unit, so what the compiler inlines, and with it the instruction counts,
  doesn't shift when unrelated code changes; in the default release
  build such a change moved some rows by up to about 1%. Switching to
  this build also changes the counts themselves, every library's, by
  several percent on some rows, so tables from different builds aren't
  comparable (see the note above the `environment` table). `probe`,
  `time`, `table` and `update` refuse to run from any other build. The
  `environment` table's `Build` row names the profile and any override
  the tool can see (an optimization level other than 3,
  `CARGO_PROFILE_MEASURE_*` variables, `RUSTFLAGS`); it can't see
  overrides passed with `--config` or set in a Cargo config file, so
  don't measure with those.
- Every table sits between `<!-- bench:NAME:begin/end -->` markers.
  `update` rewrites only what's between them, one section at a time
  (every section, in document order, without `--section`), and writes
  the file after each section, so don't edit it while `update` runs.
- `--jobs N` limits how many valgrind processes run at once (default:
  every core). `--timeout SECS` (default 100) kills a probe that runs too
  long; wall-clock children get a quarter of it. These tables used 300:
  at 100, libyaml's `deep-flow` at 100× and yaml-edit's `deep-flow` at
  10× time out instead of finishing.
- **The README summary is generated from this file.** After every
  `update`, and on its own with `bench readme` (which measures nothing),
  `bench` rewrites the `bench:readme` region of `crates/oneyaml/README.md`,
  the `bench:chart` region of the repository `README.md` and the charts
  `docs/img/bench-summary-{light,dark}.svg` from the `serde-instructions`,
  `ser-instructions`, `document` and `edits` tables, their wall-clock
  tables and the `environment` table. Every number in the summary is a
  cell of those tables or the ratio of two, so the README can't disagree
  with this file.
- The `environment` table records the run's revision, date, CPU, OS,
  rustc, build, valgrind and the comparator versions (from `Cargo.lock`).
  `update` re-records it whenever it re-measures a table the summary
  reads. `--revision TEXT`, `--revision-note TEXT` and
  `--date YYYY-MM-DD` override what it records.
- **Releases pin the links.** The READMEs' chart URLs and links to this
  file name a git ref, `main` by default. A published crate's README is
  frozen at release, so a release runs
  `cargo run -p bench --profile measure -- pin-links <tag>` on the release
  commit to point every repository link of the published docs at the
  tag (`--dry-run` shows what would change), and `pin-links main`
  afterwards to point them back.

How long each section took in this run (`--timeout 300 --jobs 4`):

| Section | Time |
|---|---|
| `environment` | < 0.1 s |
| `corpus` | 0.4 s |
| `instructions` | 211 s (one yaml-edit `deep-flow` probe dominates) |
| `allocations` | 112 s |
| `collect` | 6 s |
| `tree` | 14 s |
| `scaling` | 305 s (the 300 s timeout of libyaml `deep-flow` at 100× dominates) |
| `wall` | 111 s |
| `wall-scaling` | 41 s |
| `serde-check` | 5 s |
| `serde-instructions` | 91 s |
| `serde-allocations` | 98 s |
| `serde-wall` | 64 s |
| `ser-check` | 5 s |
| `ser-instructions` | 70 s |
| `ser-allocations` | 138 s |
| `ser-wall` | 28 s |
| `document` | 64 s |
| `edits` | 63 s |
| **total** | **23 min 45 s** |

valgrind 3.27.1 was built from source
(`https://sourceware.org/pub/valgrind/valgrind-3.27.1.tar.bz2`, sha256
`5d589152eb8071c02feab8ce6ab719e431a1fbc3e2b1700f5432632a8b9264dc`)
with `./configure --prefix=$HOME/.local && make && make install`.

## Method

**Instructions.** The bench binary runs itself as
`bench probe <parser> <case> <scale>` under
`valgrind --tool=callgrind --toggle-collect=*measured_drain*`. Only
instructions executed while the measured function is on the stack are
counted: the parse (or read, write, edit), consuming the whole output,
and dropping it. Input generation and start-up are excluded. This is the
`iai-callgrind`/`gungraun` technique without their runner.

**Allocations.** valgrind DHAT (`--tool=dhat`) reports total blocks,
total bytes and the heap peak (`t-gmax`) of the whole probe. A
`baseline` probe generates the same input and does nothing else; its
blocks and bytes are subtracted from each probe's, which leaves exactly
the measured work's allocations (a `realloc` counts as one). The peak
can't be subtracted, because the baseline's peak happens during input
generation, so peak tables show the whole-process peak ÷ input with the
baseline beside it.

**Wall-clock.** `bench time` runs one child process per library and
input, one after another: one validating run, one warm-up, then at least
5 and at most 1,000 timed runs, stopping once 300 ms have been spent.
Tables show the median.

**Linear scaling.** Every parser runs each stress shape at 1×, 10× and
100× of 10 KB (about 1 MB at 100×), measured in instructions. A cell is
`(Ir_k / bytes_k) / (Ir_1 / bytes_1)`; 1.00 is perfectly linear, and a
ratio above **1.25** would be flagged **SUPERLINEAR**.

**Failures are cells.** Every measurement runs in a child process, so a
library that errors, crashes or hangs becomes a `rejects`, `crash` or
`timeout` cell, not a failed run. Every corpus input is valid YAML 1.2,
and a test checks that oneyaml accepts it.

**Isolation.** The competitors are reached only through the unpublished
`oracles` crate; no published oneyaml crate depends on any of them
(`cargo tree` shows none of them in any published crate's graph).

## Corpus

`crates/bench/src/corpus.rs` generates every input from templates written
for this project. There is no third-party text, so the corpus is
license-clean and versioned with the code; the FNV-1a column shows when
a generator changes. The real-world shapes follow the public structure of
each format; they aren't copies of real files:

- **Kubernetes** (k8s-manifest, and k8s-multi with over a thousand
  documents): Deployment, Service, ConfigMap and Ingress, as Helm renders
  them.
- **GitHub Actions** (gha-workflow): `on`, `env`, matrix jobs,
  `uses`/`with`, and `run: |`/`>-`.
- **docker-compose** (compose-merge): `x-` extension fields with
  `&anchors`, and `<<: *merge` keys.
- **OpenAPI 3.1** (openapi-100k, openapi-10m): `paths` with parameters,
  responses and `$ref`s, plus `components/schemas`.
- **Helm values** (helm-values): an umbrella chart's `values.yaml`
  documented the helm-docs way (`# --` lines), with commented-out example
  blocks; about 70% of its bytes are comment lines.

The synthetic stress shapes (`deep-block`, `deep-flow`, `wide-map`,
`long-scalars`, `block-scalars`, `flow-heavy`, `comment-heavy`,
`anchors`, `whitespace`) appear at 10× (about 100 KB) in the
deterministic tables.

<!-- bench:corpus:begin -->
`size_of::<oneyaml_syntax::Event>()` = 48 bytes.

| Case | Kind | Scale | Bytes | FNV-1a 64 | oneyaml events (with trivia) | saphyr events (YAML only) | Contents |
|---|---|---|---|---|---|---|---|
| k8s-manifest | real-world | 1× | 5,633 | `055defb9d646c354` | 1,721 | 752 | Deployment + Service + ConfigMap + Ingress, one set |
| gha-workflow | real-world | 1× | 3,078 | `02c8bd00cd7377f3` | 666 | 267 | GitHub Actions workflow: matrix, steps, literal-block `run` scripts |
| compose-merge | real-world | 1× | 2,442 | `3099f74496483a97` | 812 | 333 | docker-compose with `x-` anchors and `<<: *` merge keys |
| k8s-multi | real-world | 1× | 1,050,323 | `d4e1c07e13a3d3b2` | 316,871 | 138,252 | ~300 `---`-separated Kubernetes documents |
| openapi-100k | real-world | 1× | 102,778 | `1d0382cd74933d58` | 30,974 | 13,112 | OpenAPI-style spec: paths, schemas, `$ref`s |
| openapi-10m | real-world | 1× | 10,486,088 | `f1ada321f5a8840e` | 3,114,534 | 1,318,016 | the same OpenAPI shape at ~10 MB |
| helm-values | real-world | 1× | 102,418 | `dc0a6f4a2039c07b` | 14,268 | 3,400 | Helm umbrella-chart values: long `# --` comment runs, commented-out blocks, comments under `{}`/`[]` |
| deep-block | stress | 10× | 103,739 | `7a1e7da75647cb05` | 2,718 | 1,361 | one chain of nested block mappings (depth = sqrt(2 × size)) |
| deep-flow | stress | 10× | 102,402 | `161a42f706068b17` | 102,406 | rejects | `[[[...]]]` nested flow sequences (depth = size/2) |
| wide-map | stress | 10× | 102,414 | `0320a218d7370faa` | 26,781 | 10,716 | one flat block mapping with many keys |
| long-scalars | stress | 10× | 102,444 | `550a8ced7e575398` | 26 | 14 | four huge scalars: plain, single, double (escapes), multi-line |
| block-scalars | stress | 10× | 102,508 | `5ccc9aacc9ce1375` | 1,620 | 544 | many literal and folded scalars, every chomping indicator |
| flow-heavy | stress | 10× | 102,495 | `182d89629cba5cca` | 72,681 | 31,371 | block sequence of nested flow mappings/sequences |
| comment-heavy | stress | 10× | 102,400 | `89d4337fd94d69d9` | 19,044 | 4,014 | comment blocks and trailing comments on every line |
| anchors | stress | 10× | 102,474 | `fdc35eda9c97bb49` | 54,556 | 21,826 | many anchors, aliases and `<<` merges |
| whitespace | stress | 10× | 102,456 | `6bbeaf871cb95ae5` | 41,242 | 7,936 | blank lines, trailing spaces, wide indentation |
<!-- bench:corpus:end -->

## Parser

### What each parser does

Every column consumes its parser's **complete** output for the whole
input; no parser has an event cap, and none renders anything.

| Column | Call | What it produces |
|---|---|---|
| **oneyaml** | `oneyaml_syntax::events_with(input, &settings)` iterated to `StreamEnd`, each event counted and dropped | **Lossless events with trivia.** Every YAML event, plus trivia events that tile every input byte (whitespace, line breaks, comments, and indicators such as `:`, `-`, `[`, `,`, `---`), with spans and in-band diagnostics. Scalar values are decoded from the source on demand, not during the parse. Streamed. |
| saphyr | `saphyr_parser::Parser::new_from_str` iterated to `StreamEnd` | **YAML events only, no trivia and no comments.** Streamed. |
| yaml-rust2 | `yaml_rust2::parser::Parser::next_token` to `StreamEnd` | **YAML events only, no trivia and no comments.** Owned scalar strings, streamed. |
| libyaml | `unsafe_libyaml::yaml_parser_parse` to `STREAM-END`, with `yaml_event_delete` after each event | **YAML events only, no trivia and no comments.** C-heap scalar copies, streamed. |
| yaml-edit | `YamlFile::parse`, then a full preorder walk of the rowan CST (every node and token) | **Lossless CST**: the like-for-like comparator. |

**They do different work.** oneyaml and yaml-edit are lossless: they keep
every byte, including comments and whitespace. saphyr, yaml-rust2 and
libyaml throw that away, so on the same input oneyaml's stream has 2–5×
more events (see the corpus table's event counts).

**Settings.** oneyaml runs under
`Settings::new().limits(Limits::unlimited())`: the default `max_depth` of
1,024 would reject `deep-flow` at every scale and `deep-block` at 100×.
The limit checks still run; they just never trip. The competitors run
with their defaults. saphyr and yaml-rust2 hold the flow level in a `u8`,
so they reject flow nesting deeper than 255; yaml-edit rejects
`deep-block` from 10× and `deep-flow` at every scale; libyaml rejects
`whitespace`, which has a tab after `- ` (valid in YAML 1.2).

### Instructions

Lower is better. The ratio columns compare oneyaml with the fastest of
the three event parsers, and with yaml-edit.

<!-- bench:instructions:begin -->
| Case | Bytes | oneyaml Ir | oneyaml Ir/B | saphyr Ir/B | yaml-rust2 Ir/B | libyaml Ir/B | yaml-edit Ir/B | oneyaml ÷ fastest event parser | oneyaml ÷ yaml-edit |
|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 955,520 | 169.6 | 265.1 | 215.1 | 243.9 | 842.5 | 0.79× | 0.20× |
| gha-workflow | 3,078 | 403,175 | 131.0 | 198.2 | 163.6 | 215.6 | 665.3 | 0.80× | 0.20× |
| compose-merge | 2,442 | 447,749 | 183.4 | 265.8 | 225.4 | 244.4 | 858.0 | 0.81× | 0.21× |
| k8s-multi | 1,050,323 | 175,508,412 | 167.1 | 256.2 | 212.7 | 243.2 | 777.5 | 0.79× | 0.21× |
| openapi-100k | 102,778 | 17,552,205 | 170.8 | 269.5 | 216.7 | 244.2 | 827.1 | 0.79× | 0.21× |
| openapi-10m | 10,486,088 | 1,769,430,109 | 168.7 | 257.6 | 214.3 | 241.2 | 811.3 | 0.79× | 0.21× |
| helm-values | 102,418 | 8,319,783 | 81.2 | 75.2 | 77.7 | 137.3 | 304.2 | 1.08× | 0.27× |
| deep-block | 103,739 | 2,887,169 | 27.8 | 47.5 | 48.4 | 101.1 | rejects | 0.59× | – |
| deep-flow | 102,402 | 71,302,195 | 696.3 | rejects | rejects | 26952.1 | rejects | 0.026× | – |
| wide-map | 102,414 | 18,658,100 | 182.2 | 286.3 | 226.6 | 251.7 | 821.3 | 0.80× | 0.22× |
| long-scalars | 102,444 | 4,687,049 | 45.8 | 63.2 | 71.6 | 144.5 | 209.6 | 0.72× | 0.22× |
| block-scalars | 102,508 | 3,084,401 | 30.1 | 51.9 | 56.6 | 133.7 | 340.0 | 0.58× | 0.089× |
| flow-heavy | 102,495 | 40,283,712 | 393.0 | 586.0 | 479.5 | 449.0 | 1669.5 | 0.88× | 0.24× |
| comment-heavy | 102,400 | 9,762,728 | 95.3 | 84.4 | 87.2 | 138.3 | 361.6 | 1.13× | 0.26× |
| anchors | 102,474 | 30,079,624 | 293.5 | 454.4 | 365.1 | 312.9 | 1338.9 | 0.94× | 0.22× |
| whitespace | 102,456 | 16,470,838 | 160.8 | 158.0 | 150.5 | rejects | 752.2 | 1.07× | 0.21× |
<!-- bench:instructions:end -->

### Allocations

<!-- bench:allocations:begin -->
**Allocation count (blocks, `realloc` included; baseline subtracted)**

| Case | Bytes | oneyaml | saphyr | yaml-rust2 | libyaml | yaml-edit | oneyaml ÷ fewest event parser |
|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 15 | 885 | 1,710 | 1,869 | 5,358 | 0.017× |
| gha-workflow | 3,078 | 14 | 396 | 666 | 726 | 2,202 | 0.035× |
| compose-merge | 2,442 | 15 | 392 | 668 | 817 | 2,594 | 0.038× |
| k8s-multi | 1,050,323 | 15 | 161,753 | 314,378 | 343,014 | 921,272 | 0.000093× |
| openapi-100k | 102,778 | 16 | 16,405 | 30,947 | 33,356 | 94,374 | 0.00098× |
| openapi-10m | 10,486,088 | 16 | 1,652,331 | 3,114,921 | 3,363,735 | 9,452,449 | 0.0000097× |
| helm-values | 102,418 | 11 | 3,728 | 7,406 | 8,249 | 36,470 | 0.0030× |
| deep-block | 103,739 | 14 | 932 | 1,836 | 1,846 | rejects | 0.015× |
| deep-flow | 102,402 | 53 | rejects | rejects | 61 | rejects | 0.87× |
| wide-map | 102,414 | 8 | 21,425 | 42,844 | 42,854 | 101,861 | 0.00037× |
| long-scalars | 102,444 | 6 | 62 | 76 | 90 | 19,056 | 0.097× |
| block-scalars | 102,508 | 8 | 3,501 | 3,836 | 3,242 | 30,864 | 0.0025× |
| flow-heavy | 102,495 | 16 | 34,438 | 66,565 | 70,397 | 225,099 | 0.00046× |
| comment-heavy | 102,400 | 8 | 5,350 | 10,692 | 10,702 | 47,901 | 0.0015× |
| anchors | 102,474 | 13 | 27,618 | 46,928 | 46,927 | 187,786 | 0.00047× |
| whitespace | 102,456 | 8 | 9,523 | 19,036 | rejects | 106,334 | 0.00084× |

**Bytes allocated ÷ input bytes (baseline subtracted)**

| Case | Bytes | oneyaml | saphyr | yaml-rust2 | libyaml | yaml-edit | oneyaml ÷ fewest event parser |
|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 2.56 | 12.73 | 10.11 | 20.84 | 93.88 | 0.25× |
| gha-workflow | 3,078 | 4.59 | 10.87 | 7.39 | 28.57 | 67.95 | 0.62× |
| compose-merge | 2,442 | 5.84 | 10.41 | 8.22 | 36.32 | 102.15 | 0.71× |
| k8s-multi | 1,050,323 | 0.01 | 11.84 | 9.53 | 8.61 | 96.43 | 0.0016× |
| openapi-100k | 102,778 | 0.16 | 11.70 | 9.34 | 8.68 | 86.99 | 0.018× |
| openapi-10m | 10,486,088 | 0.00 | 11.51 | 9.21 | 7.95 | 91.49 | 0.00019× |
| helm-values | 102,418 | 0.13 | 2.92 | 2.32 | 2.68 | 38.16 | 0.054× |
| deep-block | 103,739 | 0.22 | 2.20 | 1.51 | 2.16 | rejects | 0.14× |
| deep-flow | 102,402 | 107.51 | rejects | rejects | 90.87 | rejects | 1.18× |
| wide-map | 102,414 | 0.12 | 16.84 | 13.39 | 10.71 | 97.67 | 0.011× |
| long-scalars | 102,444 | 0.03 | 2.58 | 2.57 | 3.24 | 31.10 | 0.012× |
| block-scalars | 102,508 | 0.12 | 3.42 | 2.84 | 3.82 | 29.08 | 0.042× |
| flow-heavy | 102,495 | 0.24 | 25.68 | 20.44 | 17.33 | 230.15 | 0.014× |
| comment-heavy | 102,400 | 0.12 | 4.21 | 3.35 | 3.18 | 54.05 | 0.038× |
| anchors | 102,474 | 0.14 | 17.29 | 15.51 | 11.66 | 181.56 | 0.012× |
| whitespace | 102,456 | 0.12 | 7.48 | 5.95 | rejects | 111.40 | 0.020× |

**Whole-process peak heap ÷ input bytes (the baseline column is input generation alone)**

| Case | Bytes | baseline | oneyaml | saphyr | yaml-rust2 | libyaml | yaml-edit | oneyaml ÷ yaml-edit |
|---|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 1.94 | 3.12 | 2.25 | 2.09 | 14.02 | 27.09 | 0.12× |
| gha-workflow | 3,078 | 3.49 | 3.67 | 3.49 | 3.49 | 23.67 | 23.03 | 0.16× |
| compose-merge | 2,442 | 4.59 | 5.30 | 4.58 | 4.59 | 30.46 | 30.65 | 0.17× |
| k8s-multi | 1,050,323 | 1.00 | 1.01 | 1.01 | 1.00 | 1.07 | 29.98 | 0.034× |
| openapi-100k | 102,778 | 1.71 | 1.71 | 1.71 | 1.71 | 2.01 | 24.54 | 0.070× |
| openapi-10m | 10,486,088 | 1.67 | 1.67 | 1.67 | 1.67 | 1.67 | 27.13 | 0.061× |
| helm-values | 102,418 | 3.03 | 3.03 | 3.03 | 3.03 | 3.03 | 14.12 | 0.21× |
| deep-block | 103,739 | 1.04 | 1.14 | 1.78 | 1.51 | 2.23 | rejects | – |
| deep-flow | 102,402 | 1.02 | 54.77 | rejects | rejects | 46.78 | rejects | – |
| wide-map | 102,414 | 1.06 | 1.11 | 1.06 | 1.06 | 1.72 | 28.94 | 0.038× |
| long-scalars | 102,444 | 1.30 | 1.30 | 1.34 | 1.33 | 2.00 | 13.22 | 0.098× |
| block-scalars | 102,508 | 1.06 | 1.11 | 1.06 | 1.06 | 1.72 | 11.92 | 0.093× |
| flow-heavy | 102,495 | 1.06 | 1.17 | 1.21 | 1.16 | 1.81 | 66.97 | 0.017× |
| comment-heavy | 102,400 | 1.06 | 1.11 | 1.06 | 1.06 | 1.72 | 18.29 | 0.061× |
| anchors | 102,474 | 1.06 | 1.12 | 2.48 | 3.17 | 1.72 | 50.22 | 0.022× |
| whitespace | 102,456 | 1.06 | 1.11 | 1.06 | 1.06 | rejects | 35.72 | 0.031× |

<!-- bench:allocations:end -->

### Collecting (`parse_with`)

What keeping the whole stream costs: `oneyaml-collect` runs `parse_with`
(an `EventList`, 48 bytes per event) and walks it, against the pull
parser dropping each event. DHAT peak heap and bytes allocated, net of
the input-generation baseline, divided by the input size.

<!-- bench:collect:begin -->
| Case | Bytes | pulled: peak ÷ input | collected: peak ÷ input | collected: bytes allocated ÷ input |
|---|---|---|---|---|
| k8s-manifest | 5,633 | 1.18 | 18.63 | 37.43 |
| gha-workflow | 3,078 | 0.18 | 16.15 | 36.47 |
| compose-merge | 2,442 | 0.72 | 20.85 | 46.02 |
| k8s-multi | 1,050,323 | 0.01 | 23.97 | 47.93 |
| openapi-100k | 102,778 | 0.00 | 15.02 | 30.76 |
| openapi-10m | 10,486,088 | 0.00 | 18.87 | 38.40 |
| helm-values | 102,418 | 0.00 | 6.72 | 15.48 |
| deep-block | 103,739 | 0.10 | 2.00 | 4.01 |
| deep-flow | 102,402 | 53.75 | 115.19 | 230.39 |
| wide-map | 102,414 | 0.05 | 15.41 | 30.83 |
| long-scalars | 102,444 | 0.00 | 0.00 | 0.06 |
| block-scalars | 102,508 | 0.05 | 1.01 | 2.04 |
| flow-heavy | 102,495 | 0.12 | 61.50 | 123.01 |
| comment-heavy | 102,400 | 0.05 | 15.41 | 30.84 |
| anchors | 102,474 | 0.06 | 30.76 | 61.53 |
| whitespace | 102,456 | 0.05 | 30.76 | 61.53 |
<!-- bench:collect:end -->

### Syntax tree (`SyntaxTree::parse_with`)

What the lossless syntax tree costs. `oneyaml-tree` builds the tree from
the pull parser (the tree owns a copy of the input, as a `Document`
does), walks every element, and drops it; `oneyaml-tree-retained` keeps
the built tree, so DHAT's end-of-run figure is exactly what a tree
retains. Both are divided by the input size and **include the tree's own
copy of the input (1.00×)**. "peak" is the whole-process peak minus the
heap the baseline still holds at its end (the generated input). The
arena stores 9 bytes per element, so retained ≈ 1 + 9 × elements ÷
byte; the densest synthetic shapes, where nearly every byte is its own
element, retain the most. "stream Ir/B" is the pull parser drained;
"tree Ir/B" is parse + build + walk + drop.

<!-- bench:tree:begin -->
| Case | Bytes | tree elements | elements ÷ byte | peak ÷ input | retained ÷ input | stream Ir/B | tree Ir/B | tree ÷ stream |
|---|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 2,053 | 0.36 | 10.64 | 4.31 | 169.6 | 232.7 | 1.37× |
| gha-workflow | 3,078 | 793 | 0.26 | 7.84 | 3.48 | 131.0 | 177.2 | 1.35× |
| compose-merge | 2,442 | 997 | 0.41 | 9.92 | 4.67 | 183.4 | 251.6 | 1.37× |
| k8s-multi | 1,050,323 | 378,253 | 0.36 | 6.56 | 4.27 | 167.1 | 222.8 | 1.33× |
| openapi-100k | 102,778 | 38,392 | 0.37 | 8.16 | 4.36 | 170.8 | 233.1 | 1.37× |
| openapi-10m | 10,486,088 | 3,860,896 | 0.37 | 5.93 | 4.31 | 168.7 | 224.3 | 1.33× |
| helm-values | 102,418 | 16,215 | 0.16 | 4.50 | 2.42 | 81.2 | 107.8 | 1.33× |
| deep-block | 103,739 | 2,717 | 0.03 | 2.51 | 1.24 | 27.8 | 33.6 | 1.21× |
| deep-flow | 102,402 | 153,605 | 1.50 | 81.36 | 14.50 | 696.3 | 919.7 | 1.32× |
| wide-map | 102,414 | 37,488 | 0.37 | 7.86 | 4.29 | 182.2 | 238.8 | 1.31× |
| long-scalars | 102,444 | 31 | 0.00 | 2.02 | 1.00 | 45.8 | 46.9 | 1.02× |
| block-scalars | 102,508 | 2,155 | 0.02 | 2.74 | 1.34 | 30.1 | 35.4 | 1.18× |
| flow-heavy | 102,495 | 97,158 | 0.95 | 13.68 | 9.53 | 393.0 | 534.8 | 1.36× |
| comment-heavy | 102,400 | 21,045 | 0.21 | 4.98 | 2.85 | 95.3 | 130.0 | 1.36× |
| anchors | 102,474 | 70,918 | 0.69 | 13.63 | 7.23 | 293.5 | 402.2 | 1.37× |
| whitespace | 102,456 | 44,411 | 0.43 | 7.86 | 4.90 | 160.8 | 233.3 | 1.45× |
<!-- bench:tree:end -->

### Linear scaling

<!-- bench:scaling:begin -->
| Shape | Bytes 1× / 10× / 100× | oneyaml Ir at 100× | oneyaml (10× / 100×) | saphyr (10× / 100×) | yaml-rust2 (10× / 100×) | libyaml (10× / 100×) | yaml-edit (10× / 100×) |
|---|---|---|---|---|---|---|---|
| deep-block | 10,730 / 103,739 / 1,028,894 | 21,778,545 | 0.58 / 0.44 | 0.55 / 0.41 | 0.63 / 0.51 | 0.84 / 0.78 | rejects at 10× |
| deep-flow | 10,242 / 102,402 / 1,024,002 | 708,610,222 | 0.88 / 0.87 | rejects at 1× | rejects at 1× | 1.09 / 1.10 | rejects at 1× |
| wide-map | 10,249 / 102,414 / 1,024,009 | 186,515,875 | 1.00 / 1.00 | 1.00 / 1.00 | 1.00 / 1.00 | 1.00 / 1.00 | 0.98 / 0.97 |
| long-scalars | 10,301 / 102,444 / 1,024,061 | 46,726,757 | 0.97 / 0.97 | 0.97 / 0.96 | 0.97 / 0.97 | 0.99 / 0.99 | 0.98 / 0.97 |
| block-scalars | 10,329 / 102,508 / 1,024,352 | 30,378,509 | 0.98 / 0.96 | 0.99 / 0.97 | 0.98 / 0.96 | 0.99 / 0.99 | 0.97 / 0.95 |
| flow-heavy | 10,240 / 102,495 / 1,024,082 | 399,850,329 | 0.99 / 0.99 | 0.99 / 0.99 | 0.99 / 0.99 | 0.99 / 0.99 | 0.98 / 0.97 |
| comment-heavy | 10,356 / 102,400 / 1,024,179 | 97,200,804 | 0.99 / 0.99 | 0.99 / 0.99 | 0.99 / 0.99 | 0.99 / 1.00 | 0.97 / 0.96 |
| anchors | 10,268 / 102,474 / 1,024,001 | 275,392,757 | 0.91 / 0.83 | 0.95 / 0.92 | 0.91 / 0.83 | 0.93 / 0.88 | 0.90 / 0.82 |
| whitespace | 10,276 / 102,456 / 1,024,028 | 160,891,464 | 0.97 / 0.95 | 0.98 / 0.96 | 0.98 / 0.96 | rejects at 1× | 0.97 / 0.94 |
<!-- bench:scaling:end -->

### Wall-clock (informational only)

Real-world inputs run at 1× and stress shapes at 100× (about 1 MB).
Higher MB/s is better. The ratio columns are time ratios, so lower is
better for oneyaml.

<!-- bench:wall:begin -->
Host: AMD Ryzen 9 7950X3D 16-Core Processor; load average (1/5/15 min) at start: 3.28 / 4.50 / 4.36. MB/s = 10⁶ bytes/s, median of ≥ 5 runs.

| Case | Bytes | oneyaml MB/s | saphyr MB/s | yaml-rust2 MB/s | libyaml MB/s | yaml-edit MB/s | oneyaml time ÷ fastest event parser | oneyaml time ÷ yaml-edit |
|---|---|---|---|---|---|---|---|---|
| k8s-manifest@1× | 5,633 | 98 | 56 | 66 | 81 | 23 | 0.83× | 0.23× |
| gha-workflow@1× | 3,078 | 129 | 78 | 92 | 102 | 27 | 0.79× | 0.21× |
| compose-merge@1× | 2,442 | 95 | 54 | 61 | 94 | 23 | 1.00× | 0.24× |
| k8s-multi@1× | 1,050,323 | 104 | 57 | 68 | 91 | 22 | 0.88× | 0.21× |
| openapi-100k@1× | 102,778 | 98 | 56 | 64 | 96 | 22 | 0.98× | 0.23× |
| openapi-10m@1× | 10,486,088 | 105 | 60 | 68 | 97 | 19 | 0.92× | 0.18× |
| helm-values@1× | 102,418 | 234 | 212 | 217 | 185 | 61 | 0.93× | 0.26× |
| deep-block@100× | 1,028,894 | 973 | 637 | 581 | 333 | rejects | 0.66× | – |
| deep-flow@100× | 1,024,002 | 20 | rejects | rejects | 1 | timeout (> 75 s) | 0.044× | – |
| wide-map@100× | 1,024,009 | 99 | 60 | 70 | 86 | 21 | 0.87× | 0.21× |
| long-scalars@100× | 1,024,061 | 528 | 473 | 377 | 170 | 116 | 0.90× | 0.22× |
| block-scalars@100× | 1,024,352 | 879 | 463 | 388 | 211 | 69 | 0.53× | 0.078× |
| flow-heavy@100× | 1,024,082 | 45 | 25 | 30 | 49 | 11 | 1.10× | 0.24× |
| comment-heavy@100× | 1,024,179 | 208 | 169 | 187 | 195 | 52 | 0.93× | 0.25× |
| anchors@100× | 1,024,001 | 66 | 30 | 40 | 77 | 13 | 1.16× | 0.19× |
| whitespace@100× | 1,024,028 | 115 | 90 | 107 | rejects | 27 | 0.93× | 0.23× |
<!-- bench:wall:end -->

Each stress shape's throughput at 1×, 10× and 100× (10 KB, 100 KB and
1 MB):

<!-- bench:wall-scaling:begin -->
Host: AMD Ryzen 9 7950X3D 16-Core Processor; load average (1/5/15 min) at start: 2.87 / 4.02 / 4.20. MB/s = 10⁶ bytes/s, median of ≥ 5 runs.

| Shape | oneyaml MB/s 1× / 10× / 100× | saphyr MB/s 1× / 10× / 100× | yaml-rust2 MB/s 1× / 10× / 100× | libyaml MB/s 1× / 10× / 100× | yaml-edit MB/s 1× / 10× / 100× |
|---|---|---|---|---|---|
| deep-block | 369 / 695 / 997 | 183 / 366 / 622 | 197 / 411 / 585 | 227 / 310 / 344 | 16 / rejects at 10× |
| deep-flow | 12 / 19 / 20 | rejects at 1× | rejects at 1× | 1 / 1 / 1 | rejects at 1× |
| wide-map | 102 / 102 / 102 | 62 / 58 / 61 | 67 / 70 / 68 | 87 / 80 / 87 | 27 / 25 / 21 |
| long-scalars | 480 / 527 / 539 | 418 / 468 / 471 | 351 / 383 / 383 | 167 / 173 / 169 | 111 / 94 / 110 |
| block-scalars | 830 / 834 / 861 | 428 / 430 / 440 | 368 / 396 / 411 | 215 / 210 / 207 | 67 / 69 / 69 |
| flow-heavy | 45 / 40 / 47 | 26 / 26 / 25 | 30 / 30 / 31 | 49 / 44 / 49 | 12 / 11 / 11 |
| comment-heavy | 210 / 206 / 203 | 164 / 161 / 161 | 183 / 186 / 180 | 193 / 198 / 198 | 60 / 54 / 50 |
| anchors | 56 / 58 / 65 | 28 / 29 / 30 | 33 / 37 / 40 | 69 / 72 / 74 | 13 / 13 / 12 |
| whitespace | 108 / 113 / 116 | 84 / 85 / 79 | 102 / 105 / 103 | rejects at 1× | 25 / 25 / 28 |
<!-- bench:wall-scaling:end -->

## Serde read

**Comparators**, pinned in `crates/oracles/Cargo.toml`:

| Library | Version | Documents | Merge keys (`<<`) | Dynamic value type |
|---|---|---|---|---|
| **oneyaml** | this tree | `Deserializer::from_str_with` iterator | `Settings::merge_keys(true)`, on compose-merge only (off by default) | `oneyaml::Value` |
| serde_yaml_ng | 0.10.0 | `Deserializer::from_str` iterator | not in its deserializer; only `Value::apply_merge`, so typed reads go `Value` → `apply_merge` → `from_value` (the extra work it needs to be equivalent) | `serde_yaml_ng::Value` |
| serde_norway | 0.9.42 | as serde_yaml_ng | as serde_yaml_ng | `serde_norway::Value` |
| serde_yaml2 | 0.1.3 | **none**: reads the first document only | **none** | `yaml_rust2::Yaml` |
| serde-saphyr | 1.3.0 | `from_multiple` | expanded by default | none of its own: `serde_json::Value` |
| serde_json | 1.0.151 | the JSON is an array of the documents | already expanded in the JSON | `serde_json::Value` |

serde-saphyr's default resource budget rejects k8s-multi (more than
1,024 documents) and openapi-10m (more than 250,000 nodes and 1,000,000
events), inputs oneyaml's `Limits::default()` accepts, so it runs with
those three caps lifted and every other budget check on. The saphyr
project's own `saphyr-serde` crate is still a placeholder; serde-saphyr is
a third-party crate built on a fork of saphyr-parser.

**Workloads**, each on every real-world corpus input at 1×:

- `value`: every document into the library's dynamic value type;
- `typed`: every document into realistic derived structs
  (`crates/bench/src/models.rs`: Kubernetes objects as a `kind`-tagged
  enum with an untagged `IntOrString`; a GitHub Actions workflow; a
  compose file whose services get `restart`/`logging`/`networks` from
  `<<: *common`; an OpenAPI document with nested schemas; Helm values
  read through `#[serde(flatten)]`);
- `sparse`: every document into a struct that reads a few fields and
  skips the rest.

oneyaml runs with `Settings::new()` (so `Limits::default()`, unlike the
parser tables), plus `merge_keys` on compose-merge.

**serde_json** reads the equivalent JSON (oneyaml's `Value` of each
document, merges expanded, serialized compactly and cached so the probe
reads a file). It is a reference ceiling, not a YAML comparator: JSON has
no plain scalars to resolve, no indentation and no anchors. Its Ir/B and
MB/s are per byte of the YAML input.

**Metrics**: callgrind instructions inside the measured function
(deserialize every document, then drop the result); DHAT totals minus a
baseline probe that only prepares the input; the whole-process DHAT peak
÷ input beside the baseline's; and wall-clock medians.

### Correctness guard

Before anything is measured, `bench serde-check <lib> <workload> <case>`
runs oneyaml and the competitor, converts every document of both results
to `serde_json::Value`, and compares them. A competitor that differs,
rejects or can't do the equivalent work is not measured on that row; the
reason is listed under the table. Two notes on the reasons:
serde_yaml2 reads the *quoted* scalar `"1.85"` as a float, where YAML 1.2
(and 1.1) make it a string; and serde_json rejects gha-workflow `typed`
because in the JSON equivalent `RUST_BACKTRACE: 1` is a number, while
every YAML library reads the plain scalar `1` into a `String` from its
source text. The second is a property of JSON as a reference, not a
disagreement between YAML libraries.

<!-- bench:serde-check:begin -->
| Case | Workload | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json |
|---|---|---|---|---|---|---|---|
| k8s-manifest | value | agrees | agrees | agrees | n/a [1] | agrees | agrees |
| k8s-manifest | typed | agrees | agrees | agrees | n/a [2] | agrees | agrees |
| k8s-manifest | sparse | agrees | agrees | agrees | n/a [3] | agrees | agrees |
| gha-workflow | value | agrees | agrees | agrees | differs [4] | agrees | agrees |
| gha-workflow | typed | agrees | agrees | agrees | agrees | agrees | rejects [5] |
| gha-workflow | sparse | agrees | agrees | agrees | agrees | agrees | agrees |
| compose-merge | value | agrees | agrees | agrees | n/a [6] | agrees | agrees |
| compose-merge | typed | agrees | agrees | agrees | n/a [7] | agrees | agrees |
| compose-merge | sparse | agrees | agrees | agrees | n/a [8] | agrees | agrees |
| k8s-multi | value | agrees | agrees | agrees | n/a [9] | agrees | agrees |
| k8s-multi | typed | agrees | agrees | agrees | n/a [10] | agrees | agrees |
| k8s-multi | sparse | agrees | agrees | agrees | n/a [11] | agrees | agrees |
| openapi-100k | value | agrees | agrees | agrees | agrees | agrees | agrees |
| openapi-100k | typed | agrees | agrees | agrees | agrees | agrees | agrees |
| openapi-100k | sparse | agrees | agrees | agrees | agrees | agrees | agrees |
| openapi-10m | value | agrees | agrees | agrees | agrees | agrees | agrees |
| openapi-10m | typed | agrees | agrees | agrees | agrees | agrees | agrees |
| openapi-10m | sparse | agrees | agrees | agrees | agrees | agrees | agrees |
| helm-values | value | agrees | agrees | agrees | agrees | agrees | agrees |
| helm-values | typed | agrees | agrees | agrees | agrees | agrees | agrees |
| helm-values | sparse | agrees | agrees | agrees | agrees | agrees | agrees |

1. serde_yaml2, k8s-manifest value: no multi-document API (reads the first document only)
2. serde_yaml2, k8s-manifest typed: no multi-document API (reads the first document only)
3. serde_yaml2, k8s-manifest sparse: no multi-document API (reads the first document only)
4. serde_yaml2, gha-workflow value: doc 0 /jobs/bravo-2/strategy/matrix/toolchain/2: 1.85 (ours "1.85")
5. serde_json, gha-workflow typed: invalid type: integer '1', expected a string at line 1 column 845
6. serde_yaml2, compose-merge value: no merge-key support
7. serde_yaml2, compose-merge typed: no merge-key support
8. serde_yaml2, compose-merge sparse: no merge-key support
9. serde_yaml2, k8s-multi value: no multi-document API (reads the first document only)
10. serde_yaml2, k8s-multi typed: no multi-document API (reads the first document only)
11. serde_yaml2, k8s-multi sparse: no multi-document API (reads the first document only)
<!-- bench:serde-check:end -->

### Instructions

Lower is better. The ratio columns compare oneyaml with the fastest
feature-complete library on that row (named in the next column; the
target applies to this ratio), with serde_yaml2 (partial, no target),
and with serde_json.

<!-- bench:serde-instructions:begin -->
| Case | Workload | Bytes | oneyaml Ir/B | serde_yaml_ng Ir/B | serde_norway Ir/B | serde_yaml2 Ir/B | serde-saphyr Ir/B | serde_json Ir/B | oneyaml ÷ fastest feature-complete (target applies) | fastest feature-complete | target ≤ 1.25× | oneyaml ÷ serde_yaml2 (partial, no target) | oneyaml ÷ serde_json |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 414.9 | 457.3 | 458.5 | n/a | 729.7 | 70.2 | 0.91× | serde_yaml_ng | met | – | 5.91× |
| k8s-manifest | typed | 5,633 | 376.6 | 423.1 | 423.1 | n/a | 819.1 | 65.0 | 0.89× | serde_norway | met | – | 5.80× |
| k8s-manifest | sparse | 5,633 | 339.0 | 309.9 | 309.9 | n/a | 722.8 | 16.1 | 1.09× | serde_yaml_ng | met | – | 21.03× |
| gha-workflow | value | 3,078 | 288.4 | 353.5 | 354.4 | differs | 499.4 | 46.3 | 0.82× | serde_yaml_ng | met | – | 6.22× |
| gha-workflow | typed | 3,078 | 264.7 | 292.3 | 292.4 | 183.3 | 440.3 | rejects | 0.91× | serde_yaml_ng | met | 1.44× | – |
| gha-workflow | sparse | 3,078 | 249.8 | 259.2 | 259.2 | 183.4 | 499.2 | 11.2 | 0.96× | serde_yaml_ng | met | 1.36× | 22.22× |
| compose-merge | value | 2,442 | 614.8 | 593.2 | 595.4 | n/a | 1117.5 | 115.3 | 1.04× | serde_yaml_ng | met | – | 5.33× |
| compose-merge | typed | 2,442 | 784.3 | 623.0 | 625.2 | n/a | 997.8 | 76.3 | 1.26× | serde_yaml_ng | **missed** | – | 10.28× |
| compose-merge | sparse | 2,442 | 673.9 | 599.6 | 601.7 | n/a | 1069.9 | 29.6 | 1.12× | serde_yaml_ng | met | – | 22.76× |
| k8s-multi | value | 1,050,323 | 410.7 | 457.5 | 458.8 | n/a | 721.5 | 70.3 | 0.90× | serde_yaml_ng | met | – | 5.84× |
| k8s-multi | typed | 1,050,323 | 373.1 | 422.1 | 422.1 | n/a | 808.7 | 66.2 | 0.88× | serde_norway | met | – | 5.63× |
| k8s-multi | sparse | 1,050,323 | 332.6 | 355.7 | 355.7 | n/a | 726.4 | 16.2 | 0.93× | serde_yaml_ng | met | – | 20.55× |
| openapi-100k | value | 102,778 | 410.0 | 454.1 | 455.4 | 328.8 | 730.0 | 71.0 | 0.90× | serde_yaml_ng | met | 1.25× | 5.78× |
| openapi-100k | typed | 102,778 | 362.9 | 349.5 | 348.4 | 244.0 | 649.2 | 44.2 | 1.04× | serde_norway | met | 1.49× | 8.22× |
| openapi-100k | sparse | 102,778 | 335.1 | 298.3 | 298.3 | 247.1 | 752.4 | 12.9 | 1.12× | serde_yaml_ng | met | 1.36× | 25.93× |
| openapi-10m | value | 10,486,088 | 404.2 | 447.6 | 448.9 | 324.4 | 720.6 | 70.9 | 0.90× | serde_yaml_ng | met | 1.25× | 5.70× |
| openapi-10m | typed | 10,486,088 | 359.4 | 346.4 | 346.4 | 256.3 | 641.2 | 44.3 | 1.04× | serde_yaml_ng | met | 1.40× | 8.11× |
| openapi-10m | sparse | 10,486,088 | 330.6 | 294.7 | 294.7 | 244.2 | 741.5 | 12.8 | 1.12× | serde_yaml_ng | met | 1.35× | 25.92× |
| helm-values | value | 102,418 | 142.3 | 186.8 | 187.1 | 103.3 | 236.9 | 16.7 | 0.76× | serde_yaml_ng | met | 1.38× | 8.54× |
| helm-values | typed | 102,418 | 134.2 | 174.7 | 174.8 | 96.7 | 263.2 | 11.9 | 0.77× | serde_yaml_ng | met | 1.39× | 11.26× |
| helm-values | sparse | 102,418 | 127.1 | 151.2 | 151.2 | 84.7 | 245.1 | 3.1 | 0.84× | serde_yaml_ng | met | 1.50× | 40.41× |
<!-- bench:serde-instructions:end -->

### Allocations

<!-- bench:serde-allocations:begin -->
**Allocation count (blocks, `realloc` included; baseline subtracted)**

| Case | Workload | Bytes | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json | oneyaml ÷ best feature-complete | best feature-complete | oneyaml ÷ serde_yaml2 (partial) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 1,038 | 3,099 | 3,099 | n/a | 1,422 | 567 | 0.73× | serde-saphyr | – |
| k8s-manifest | typed | 5,633 | 620 | 2,805 | 2,805 | n/a | 2,313 | 459 | 0.27× | serde-saphyr | – |
| k8s-manifest | sparse | 5,633 | 105 | 2,415 | 2,415 | n/a | 1,716 | 28 | 0.061× | serde-saphyr | – |
| gha-workflow | value | 3,078 | 365 | 1,155 | 1,155 | differs | 597 | 215 | 0.61× | serde-saphyr | – |
| gha-workflow | typed | 3,078 | 201 | 1,031 | 1,031 | 698 | 337 | rejects | 0.60× | serde-saphyr | 0.29× |
| gha-workflow | sparse | 3,078 | 58 | 914 | 914 | 802 | 718 | 10 | 0.081× | serde-saphyr | 0.072× |
| compose-merge | value | 2,442 | 764 | 1,584 | 1,584 | n/a | 1,496 | 401 | 0.51× | serde-saphyr | – |
| compose-merge | typed | 2,442 | 617 | 1,639 | 1,639 | n/a | 1,172 | 245 | 0.53× | serde-saphyr | – |
| compose-merge | sparse | 2,442 | 366 | 1,585 | 1,585 | n/a | 1,506 | 30 | 0.24× | serde-saphyr | – |
| k8s-multi | value | 1,050,323 | 187,496 | 569,202 | 569,202 | n/a | 259,380 | 103,425 | 0.72× | serde-saphyr | – |
| k8s-multi | typed | 1,050,323 | 110,628 | 515,008 | 515,008 | n/a | 423,621 | 83,517 | 0.26× | serde-saphyr | – |
| k8s-multi | sparse | 1,050,323 | 10,563 | 443,118 | 443,118 | n/a | 313,574 | 4,436 | 0.034× | serde-saphyr | – |
| openapi-100k | value | 102,778 | 17,151 | 54,250 | 54,250 | 48,318 | 24,687 | 9,965 | 0.69× | serde-saphyr | 0.35× |
| openapi-100k | typed | 102,778 | 6,583 | 45,636 | 45,636 | 31,934 | 12,385 | 4,189 | 0.53× | serde-saphyr | 0.21× |
| openapi-100k | sparse | 102,778 | 167 | 41,456 | 41,456 | 38,091 | 35,491 | 5 | 0.0047× | serde-saphyr | 0.0044× |
| openapi-10m | value | 10,486,088 | 1,718,287 | 5,461,198 | 5,461,198 | 4,860,265 | 2,479,934 | 1,002,271 | 0.69× | serde-saphyr | 0.35× |
| openapi-10m | typed | 10,486,088 | 659,270 | 4,597,784 | 4,597,784 | 3,214,454 | 1,243,104 | 421,511 | 0.53× | serde-saphyr | 0.21× |
| openapi-10m | sparse | 10,486,088 | 9,138 | 4,176,282 | 4,176,282 | 3,834,137 | 3,569,112 | 5 | 0.0026× | serde-saphyr | 0.0024× |
| helm-values | value | 102,418 | 3,466 | 12,639 | 12,639 | 11,060 | 7,282 | 1,992 | 0.48× | serde-saphyr | 0.31× |
| helm-values | typed | 102,418 | 1,254 | 10,977 | 10,977 | 9,793 | 11,419 | 840 | 0.11× | serde_yaml_ng | 0.13× |
| helm-values | sparse | 102,418 | 41 | 10,176 | 10,176 | 8,919 | 10,581 | 3 | 0.0040× | serde_yaml_ng | 0.0046× |

**Bytes allocated ÷ YAML input bytes (baseline subtracted)**

| Case | Workload | Bytes | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json | oneyaml ÷ best feature-complete | best feature-complete | oneyaml ÷ serde_yaml2 (partial) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 38.83 | 78.76 | 78.76 | n/a | 48.37 | 15.37 | 0.80× | serde-saphyr | – |
| k8s-manifest | typed | 5,633 | 25.61 | 72.95 | 72.95 | n/a | 49.32 | 13.10 | 0.52× | serde-saphyr | – |
| k8s-manifest | sparse | 5,633 | 6.74 | 60.20 | 60.20 | n/a | 35.93 | 0.28 | 0.19× | serde-saphyr | – |
| gha-workflow | value | 3,078 | 23.63 | 71.14 | 71.14 | differs | 34.07 | 9.40 | 0.69× | serde-saphyr | – |
| gha-workflow | typed | 3,078 | 15.86 | 66.95 | 66.95 | 12.83 | 28.96 | rejects | 0.55× | serde-saphyr | 1.24× |
| gha-workflow | sparse | 3,078 | 6.51 | 61.23 | 61.23 | 8.13 | 26.78 | 0.22 | 0.24× | serde-saphyr | 0.80× |
| compose-merge | value | 2,442 | 66.94 | 110.24 | 110.24 | n/a | 163.34 | 15.95 | 0.61× | serde_norway | – |
| compose-merge | typed | 2,442 | 238.26 | 116.54 | 116.54 | n/a | 153.33 | 7.49 | 2.04× | serde_norway | – |
| compose-merge | sparse | 2,442 | 225.60 | 110.58 | 110.57 | n/a | 150.19 | 0.48 | 2.04× | serde_norway | – |
| k8s-multi | value | 1,050,323 | 35.67 | 66.09 | 66.09 | n/a | 44.57 | 15.28 | 0.80× | serde-saphyr | – |
| k8s-multi | typed | 1,050,323 | 23.14 | 60.90 | 60.90 | n/a | 46.18 | 13.70 | 0.50× | serde-saphyr | – |
| k8s-multi | sparse | 1,050,323 | 2.12 | 47.66 | 47.66 | n/a | 32.30 | 0.41 | 0.066× | serde-saphyr | – |
| openapi-100k | value | 102,778 | 30.29 | 56.83 | 56.83 | 21.11 | 43.34 | 14.55 | 0.70× | serde-saphyr | 1.44× |
| openapi-100k | typed | 102,778 | 18.72 | 48.83 | 48.83 | 18.09 | 36.55 | 9.00 | 0.51× | serde-saphyr | 1.03× |
| openapi-100k | sparse | 102,778 | 1.05 | 39.84 | 39.84 | 9.87 | 32.14 | 0.00 | 0.033× | serde-saphyr | 0.11× |
| openapi-10m | value | 10,486,088 | 29.97 | 63.53 | 63.53 | 20.80 | 42.54 | 14.38 | 0.70× | serde-saphyr | 1.44× |
| openapi-10m | typed | 10,486,088 | 18.28 | 55.87 | 55.87 | 17.91 | 35.89 | 8.96 | 0.51× | serde-saphyr | 1.02× |
| openapi-10m | sparse | 10,486,088 | 0.72 | 46.91 | 46.91 | 9.74 | 31.48 | 0.00 | 0.023× | serde-saphyr | 0.073× |
| helm-values | value | 102,418 | 7.55 | 14.48 | 14.48 | 4.91 | 14.26 | 2.53 | 0.53× | serde-saphyr | 1.54× |
| helm-values | typed | 102,418 | 4.36 | 12.45 | 12.45 | 4.40 | 14.68 | 1.94 | 0.35× | serde_norway | 0.99× |
| helm-values | sparse | 102,418 | 0.33 | 10.51 | 10.51 | 2.46 | 12.74 | 0.00 | 0.031× | serde_norway | 0.13× |

**Whole-process peak heap ÷ YAML input bytes (the baseline column is YAML generation alone)**

| Case | Workload | Bytes | baseline | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json | oneyaml ÷ best feature-complete | best feature-complete | oneyaml ÷ serde_yaml2 (partial) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 1.94 | 26.04 | 32.37 | 32.37 | n/a | 21.58 | 16.27 | 1.21× | serde-saphyr | – |
| k8s-manifest | typed | 5,633 | 1.94 | 9.76 | 23.39 | 23.39 | n/a | 12.55 | 7.16 | 0.78× | serde-saphyr | – |
| k8s-manifest | sparse | 5,633 | 1.94 | 5.27 | 18.57 | 18.57 | n/a | 6.88 | 2.64 | 0.77× | serde-saphyr | – |
| gha-workflow | value | 3,078 | 3.50 | 17.42 | 49.35 | 49.35 | differs | 18.07 | 10.24 | 0.96× | serde-saphyr | – |
| gha-workflow | typed | 3,078 | 3.50 | 9.67 | 45.44 | 45.44 | 7.48 | 14.23 | rejects | 0.68× | serde-saphyr | 1.29× |
| gha-workflow | sparse | 3,078 | 3.50 | 5.02 | 40.35 | 40.35 | 3.50 | 9.41 | 3.50 | 0.53× | serde-saphyr | 1.43× |
| compose-merge | value | 2,442 | 4.59 | 36.32 | 73.41 | 73.41 | n/a | 31.43 | 17.60 | 1.16× | serde-saphyr | – |
| compose-merge | typed | 2,442 | 4.59 | 91.32 | 73.41 | 73.41 | n/a | 23.58 | 9.13 | 3.87× | serde-saphyr | – |
| compose-merge | sparse | 2,442 | 4.59 | 91.32 | 73.42 | 73.41 | n/a | 17.42 | 4.59 | 5.24× | serde-saphyr | – |
| k8s-multi | value | 1,050,323 | 1.00 | 22.94 | 17.21 | 17.21 | n/a | 16.18 | 16.01 | 1.42× | serde-saphyr | – |
| k8s-multi | typed | 1,050,323 | 1.00 | 7.02 | 7.08 | 7.08 | n/a | 7.02 | 6.85 | 1.00× | serde-saphyr | – |
| k8s-multi | sparse | 1,050,323 | 1.00 | 1.25 | 1.32 | 1.32 | n/a | 1.26 | 1.86 | 0.99× | serde-saphyr | – |
| openapi-100k | value | 102,778 | 1.71 | 22.73 | 32.71 | 32.71 | 12.91 | 16.21 | 15.39 | 1.40× | serde-saphyr | 1.76× |
| openapi-100k | typed | 102,778 | 1.71 | 10.57 | 26.86 | 26.86 | 11.04 | 10.65 | 9.83 | 0.99× | serde-saphyr | 0.96× |
| openapi-100k | sparse | 102,778 | 1.71 | 1.96 | 17.87 | 17.87 | 1.71 | 1.78 | 2.17 | 1.10× | serde-saphyr | 1.15× |
| openapi-10m | value | 10,486,088 | 1.67 | 22.52 | 35.67 | 35.67 | 12.73 | 15.82 | 15.21 | 1.42× | serde-saphyr | 1.77× |
| openapi-10m | typed | 10,486,088 | 1.67 | 10.45 | 30.06 | 30.06 | 10.98 | 10.40 | 9.79 | 1.00× | serde-saphyr | 0.95× |
| openapi-10m | sparse | 10,486,088 | 1.67 | 1.75 | 21.10 | 21.10 | 1.67 | 1.67 | 2.16 | 1.05× | serde-saphyr | 1.05× |
| helm-values | value | 102,418 | 3.03 | 6.22 | 9.61 | 9.61 | 4.51 | 4.78 | 3.03 | 1.30× | serde-saphyr | 1.38× |
| helm-values | typed | 102,418 | 3.03 | 3.68 | 8.20 | 8.20 | 3.69 | 3.91 | 3.03 | 0.94× | serde-saphyr | 1.00× |
| helm-values | sparse | 102,418 | 3.03 | 3.03 | 6.67 | 6.67 | 3.03 | 3.03 | 3.03 | 1.00× | serde-saphyr | 1.00× |

<!-- bench:serde-allocations:end -->

### Wall-clock (informational only)

Higher MB/s is better (per byte of YAML input, also for serde_json); the
ratio columns are time ratios, lower is better for oneyaml.

<!-- bench:serde-wall:begin -->
Host: AMD Ryzen 9 7950X3D 16-Core Processor; load average (1/5/15 min) at start: 5.65 / 5.40 / 4.77. MB/s = 10⁶ bytes/s, median of ≥ 5 runs.

| Case | Workload | Bytes | oneyaml MB/s | serde_yaml_ng MB/s | serde_norway MB/s | serde_yaml2 MB/s | serde-saphyr MB/s | serde_json MB/s | oneyaml ÷ fastest feature-complete | fastest feature-complete | oneyaml ÷ serde_yaml2 (partial) | oneyaml ÷ serde_json |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 35 | 35 | 36 | n/a | 17 | 262 | 1.02× | serde_norway | – | 7.39× |
| k8s-manifest | typed | 5,633 | 38 | 38 | 40 | n/a | 15 | 238 | 1.06× | serde_norway | – | 6.26× |
| k8s-manifest | sparse | 5,633 | 46 | 59 | 60 | n/a | 18 | 1437 | 1.30× | serde_norway | – | 31.37× |
| gha-workflow | value | 3,078 | 54 | 50 | 50 | differs | 26 | 360 | 0.94× | serde_yaml_ng | – | 6.69× |
| gha-workflow | typed | 3,078 | 58 | 67 | 67 | 73 | 29 | rejects | 1.15× | serde_yaml_ng | 1.26× | – |
| gha-workflow | sparse | 3,078 | 64 | 78 | 78 | 82 | 25 | 2214 | 1.22× | serde_yaml_ng | 1.28× | 34.61× |
| compose-merge | value | 2,442 | 25 | 29 | 29 | n/a | 12 | 144 | 1.16× | serde_norway | – | 5.79× |
| compose-merge | typed | 2,442 | 16 | 26 | 27 | n/a | 13 | 228 | 1.68× | serde_norway | – | 14.36× |
| compose-merge | sparse | 2,442 | 18 | 29 | 27 | n/a | 13 | 731 | 1.65× | serde_yaml_ng | – | 41.76× |
| k8s-multi | value | 1,050,323 | 30 | 33 | 34 | n/a | 15 | 98 | 1.12× | serde_norway | – | 3.28× |
| k8s-multi | typed | 1,050,323 | 39 | 39 | 37 | n/a | 14 | 244 | 1.00× | serde_yaml_ng | – | 6.28× |
| k8s-multi | sparse | 1,050,323 | 46 | 55 | 55 | n/a | 18 | 1558 | 1.19× | serde_norway | – | 33.74× |
| openapi-100k | value | 102,778 | 39 | 43 | 44 | 45 | 18 | 276 | 1.12× | serde_norway | 1.16× | 7.10× |
| openapi-100k | typed | 102,778 | 41 | 55 | 57 | 55 | 20 | 298 | 1.38× | serde_norway | 1.35× | 7.27× |
| openapi-100k | sparse | 102,778 | 47 | 69 | 69 | 60 | 17 | 2176 | 1.47× | serde_norway | 1.27× | 46.26× |
| openapi-10m | value | 10,486,088 | 31 | 29 | 30 | 42 | 15 | 124 | 0.98× | serde_norway | 1.38× | 4.02× |
| openapi-10m | typed | 10,486,088 | 36 | 37 | 39 | 51 | 18 | 153 | 1.07× | serde_norway | 1.40× | 4.23× |
| openapi-10m | sparse | 10,486,088 | 44 | 51 | 50 | 62 | 18 | 2233 | 1.15× | serde_yaml_ng | 1.42× | 50.72× |
| helm-values | value | 102,418 | 119 | 114 | 115 | 151 | 58 | 1177 | 0.97× | serde_norway | 1.27× | 9.91× |
| helm-values | typed | 102,418 | 122 | 129 | 131 | 168 | 52 | 1197 | 1.07× | serde_norway | 1.37× | 9.79× |
| helm-values | sparse | 102,418 | 134 | 161 | 159 | 197 | 57 | 8702 | 1.20× | serde_yaml_ng | 1.47× | 64.93× |
<!-- bench:serde-wall:end -->

## Serde write

The same comparators, each through its own writer with its defaults:

| Library | One document | Several documents (k8s-manifest, k8s-multi) |
|---|---|---|
| **oneyaml** | `value`: `Value::to_yaml_string`; `typed`: `oneyaml::to_string` | `value`: `to_yaml_string` per document, `---` between them (what `Serializer` writes); `typed`: one `oneyaml::Serializer` over a `Vec<u8>`, a `serialize` per document |
| serde_yaml_ng 0.10.0, serde_norway 0.9.42 | `to_string` | one `Serializer` over a `Vec<u8>`, a `serialize` per document (it writes the `---`) |
| serde-saphyr 1.3.0 | `to_string` | `to_string_multiple` |
| serde_yaml2 0.1.3 | `to_string` | **none**: `n/a` |
| serde_json 1.0.151 | `to_string` | `to_string` of the array of documents |

`to_writer` is `to_string` plus one `write_all` of the result, so it is
not a separate row. Every side uses its default settings (oneyaml's
quotes every string that some YAML reader would read as something else).

**Workloads**, each on every real-world corpus input at 1× (every
document):

- `value`: every document as the library's **own** dynamic value, read
  beforehand by that library (serde_json: from the equivalent JSON), so
  each library writes its natural in-memory form;
- `typed`: every document as the derived structs of
  `crates/bench/src/models.rs`, read beforehand by oneyaml, so every
  library writes the same Rust values.

Reading the data is preparation, outside the measured function (write,
then drop the output); each cell's DHAT baseline prepares the same data
and writes nothing.

### Correctness guard (write)

`bench ser-check <lib> <workload> <case>`: each library writes its data,
and the output is read back with oneyaml's reader (serde_json's for
serde_json), into `Value` for `value` (compared with oneyaml's reading of
the input) or into the same structs for `typed` (compared with `==`). A
library whose output reads back as something else (a lost or extra
document, a changed scalar type, an escape that doesn't read back, an
empty collection turned into null) is `differs` and is not measured;
oneyaml is checked the same way. An `agrees` cell also shows the output
size ÷ the input size.

<!-- bench:ser-check:begin -->
| Case | Workload | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json |
|---|---|---|---|---|---|---|---|
| k8s-manifest | value | agrees (1.06×) | agrees (0.99×) | agrees (0.99×) | n/a [1] | agrees (0.99×) | agrees (0.86×) |
| k8s-manifest | typed | agrees (1.16×) | agrees (1.09×) | agrees (1.09×) | n/a [2] | agrees (1.09×) | agrees (0.94×) |
| gha-workflow | value | agrees (1.00×) | agrees (0.95×) | agrees (0.95×) | differs [3] | agrees (0.95×) | agrees (0.79×) |
| gha-workflow | typed | agrees (1.32×) | agrees (1.23×) | agrees (1.23×) | differs [4] | agrees (1.23×) | agrees (0.99×) |
| compose-merge | value | agrees (1.69×) | agrees (1.62×) | agrees (1.62×) | n/a [5] | agrees (1.62×) | agrees (1.37×) |
| compose-merge | typed | agrees (1.69×) | agrees (1.63×) | agrees (1.63×) | differs [6] | agrees (1.63×) | agrees (1.36×) |
| k8s-multi | value | agrees (1.06×) | agrees (0.99×) | agrees (0.99×) | n/a [7] | agrees (0.99×) | agrees (0.86×) |
| k8s-multi | typed | agrees (1.16×) | agrees (1.09×) | agrees (1.09×) | n/a [8] | agrees (1.09×) | agrees (0.94×) |
| openapi-100k | value | agrees (1.13×) | agrees (1.11×) | agrees (1.11×) | differs [9] | agrees (1.12×) | agrees (0.83×) |
| openapi-100k | typed | agrees (2.36×) | agrees (2.33×) | agrees (2.33×) | differs [10] | agrees (2.33×) | agrees (1.57×) |
| openapi-10m | value | agrees (1.13×) | agrees (1.11×) | agrees (1.11×) | differs [11] | agrees (1.11×) | agrees (0.83×) |
| openapi-10m | typed | agrees (2.34×) | agrees (2.31×) | agrees (2.31×) | differs [12] | agrees (2.31×) | agrees (1.56×) |
| helm-values | value | agrees (0.24×) | agrees (0.24×) | agrees (0.24×) | differs [13] | agrees (0.24×) | agrees (0.22×) |
| helm-values | typed | agrees (0.24×) | agrees (0.24×) | agrees (0.24×) | differs [14] | agrees (0.24×) | agrees (0.22×) |

1. serde_yaml2, k8s-manifest value: no multi-document API (writes one document only)
2. serde_yaml2, k8s-manifest typed: no multi-document API (writes one document only)
3. serde_yaml2, gha-workflow value: output does not read back: unexpected content after a node at jobs.delta-0.steps[2].with (81:48)
4. serde_yaml2, gha-workflow typed: output does not read back: unexpected content after a node at jobs.bravo-2.steps[2].with (95:48)
5. serde_yaml2, compose-merge value: no merge-key support
6. serde_yaml2, compose-merge typed: output does not read back: invalid type: unit value, expected a map at services.db.environment (75:18)
7. serde_yaml2, k8s-multi value: no multi-document API (writes one document only)
8. serde_yaml2, k8s-multi typed: no multi-document API (writes one document only)
9. serde_yaml2, openapi-100k value: doc 0 /components/schemas/delta0/properties/ratio/example: 0 (ours 0.0)
10. serde_yaml2, openapi-100k typed: output does not read back: invalid type: unit value, expected a map at paths./alphas/107/{id}.parameters[0].schema.properties (37:20)
11. serde_yaml2, openapi-10m value: doc 0 /components/schemas/delta0/properties/ratio/example: 0 (ours 0.0)
12. serde_yaml2, openapi-10m typed: output does not read back: invalid type: unit value, expected a map at paths./alphas/10011/{id}.parameters[0].schema.properties (37:20)
13. serde_yaml2, helm-values value: doc 0 /alpha11/affinity: null (ours {})
14. serde_yaml2, helm-values typed: output does not read back: invalid type: unit value, expected a map (1:1)
<!-- bench:ser-check:end -->

### Instructions (write)

Lower is better; columns as in the read table.

<!-- bench:ser-instructions:begin -->
| Case | Workload | Bytes | oneyaml Ir/B | serde_yaml_ng Ir/B | serde_norway Ir/B | serde_yaml2 Ir/B | serde-saphyr Ir/B | serde_json Ir/B | oneyaml ÷ fastest feature-complete (target applies) | fastest feature-complete | target ≤ 1.25× | oneyaml ÷ serde_yaml2 (partial, no target) | oneyaml ÷ serde_json |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 123.5 | 230.0 | 230.0 | n/a | 195.0 | 16.4 | 0.63× | serde-saphyr | met | – | 7.51× |
| k8s-manifest | typed | 5,633 | 143.5 | 248.0 | 248.0 | n/a | 216.6 | 14.5 | 0.66× | serde-saphyr | met | – | 9.87× |
| gha-workflow | value | 3,078 | 88.0 | 184.3 | 184.3 | differs | 152.6 | 13.6 | 0.58× | serde-saphyr | met | – | 6.48× |
| gha-workflow | typed | 3,078 | 127.7 | 242.8 | 242.8 | differs | 193.5 | 14.9 | 0.66× | serde-saphyr | met | – | 8.55× |
| compose-merge | value | 2,442 | 191.0 | 369.1 | 369.1 | n/a | 335.5 | 26.4 | 0.57× | serde-saphyr | met | – | 7.23× |
| compose-merge | typed | 2,442 | 234.4 | 363.4 | 363.4 | differs | 342.8 | 22.9 | 0.68× | serde-saphyr | met | – | 10.24× |
| k8s-multi | value | 1,050,323 | 124.1 | 228.7 | 228.7 | n/a | 193.8 | 16.2 | 0.64× | serde-saphyr | met | – | 7.64× |
| k8s-multi | typed | 1,050,323 | 141.5 | 246.1 | 246.1 | n/a | 215.0 | 13.7 | 0.66× | serde-saphyr | met | – | 10.33× |
| openapi-100k | value | 102,778 | 106.1 | 226.0 | 226.0 | differs | 180.4 | 16.2 | 0.59× | serde-saphyr | met | – | 6.53× |
| openapi-100k | typed | 102,778 | 217.7 | 425.5 | 425.5 | differs | 311.2 | 24.7 | 0.70× | serde-saphyr | met | – | 8.83× |
| openapi-10m | value | 10,486,088 | 104.3 | 223.9 | 223.9 | differs | 178.2 | 15.7 | 0.59× | serde-saphyr | met | – | 6.66× |
| openapi-10m | typed | 10,486,088 | 214.6 | 420.5 | 420.5 | differs | 307.3 | 23.2 | 0.70× | serde-saphyr | met | – | 9.26× |
| helm-values | value | 102,418 | 26.4 | 56.1 | 56.1 | differs | 42.3 | 4.1 | 0.62× | serde-saphyr | met | – | 6.50× |
| helm-values | typed | 102,418 | 32.1 | 55.1 | 55.1 | differs | 43.2 | 3.5 | 0.74× | serde-saphyr | met | – | 9.29× |
<!-- bench:ser-instructions:end -->

### Allocations (write)

<!-- bench:ser-allocations:begin -->
**Allocation count (blocks, `realloc` included; baseline subtracted)**

| Case | Workload | Bytes | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json | oneyaml ÷ best feature-complete | best feature-complete | oneyaml ÷ serde_yaml2 (partial) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 153 | 492 | 492 | n/a | 839 | 6 | 0.31× | serde_yaml_ng | – |
| k8s-manifest | typed | 5,633 | 126 | 537 | 537 | n/a | 899 | 6 | 0.23× | serde_yaml_ng | – |
| gha-workflow | value | 3,078 | 19 | 177 | 177 | differs | 334 | 5 | 0.11× | serde_yaml_ng | – |
| gha-workflow | typed | 3,078 | 59 | 285 | 285 | differs | 443 | 5 | 0.21× | serde_yaml_ng | – |
| compose-merge | value | 2,442 | 19 | 329 | 329 | n/a | 618 | 5 | 0.058× | serde_yaml_ng | – |
| compose-merge | typed | 2,442 | 95 | 324 | 324 | differs | 608 | 5 | 0.29× | serde_yaml_ng | – |
| k8s-multi | value | 1,050,323 | 27,718 | 88,499 | 88,499 | n/a | 152,646 | 13 | 0.31× | serde_yaml_ng | – |
| k8s-multi | typed | 1,050,323 | 21,033 | 96,794 | 96,794 | n/a | 163,707 | 13 | 0.22× | serde_yaml_ng | – |
| openapi-100k | value | 102,778 | 26 | 8,097 | 8,097 | differs | 15,171 | 10 | 0.0032× | serde_yaml_ng | – |
| openapi-100k | typed | 102,778 | 1,702 | 17,127 | 17,127 | differs | 25,516 | 11 | 0.099× | serde_yaml_ng | – |
| openapi-10m | value | 10,486,088 | 39 | 812,544 | 812,544 | differs | 1,528,642 | 17 | 0.000048× | serde_yaml_ng | – |
| openapi-10m | typed | 10,486,088 | 168,570 | 1,721,470 | 1,721,470 | differs | 2,569,995 | 17 | 0.098× | serde_yaml_ng | – |
| helm-values | value | 102,418 | 21 | 1,965 | 1,965 | differs | 3,083 | 8 | 0.011× | serde_yaml_ng | – |
| helm-values | typed | 102,418 | 90 | 1,965 | 1,965 | differs | 3,083 | 8 | 0.046× | serde_yaml_ng | – |

**Bytes allocated ÷ YAML input bytes (baseline subtracted)**

| Case | Workload | Bytes | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json | oneyaml ÷ best feature-complete | best feature-complete | oneyaml ÷ serde_yaml2 (partial) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 11.35 | 13.75 | 13.75 | n/a | 9.17 | 2.88 | 1.24× | serde-saphyr | – |
| k8s-manifest | typed | 5,633 | 6.99 | 14.00 | 14.00 | n/a | 8.76 | 2.88 | 0.80× | serde-saphyr | – |
| gha-workflow | value | 3,078 | 4.18 | 18.83 | 18.83 | differs | 6.31 | 2.60 | 0.66× | serde-saphyr | – |
| gha-workflow | typed | 3,078 | 4.49 | 19.56 | 19.56 | differs | 8.08 | 2.60 | 0.56× | serde-saphyr | – |
| compose-merge | value | 2,442 | 9.46 | 24.88 | 24.88 | n/a | 12.93 | 3.28 | 0.73× | serde-saphyr | – |
| compose-merge | typed | 2,442 | 11.42 | 24.86 | 24.86 | differs | 12.71 | 3.28 | 0.90× | serde-saphyr | – |
| k8s-multi | value | 1,050,323 | 10.36 | 3.78 | 3.78 | n/a | 7.97 | 2.00 | 2.74× | serde_yaml_ng | – |
| k8s-multi | typed | 1,050,323 | 5.28 | 4.00 | 4.00 | n/a | 9.78 | 2.00 | 1.32× | serde_yaml_ng | – |
| openapi-100k | value | 102,778 | 3.86 | 4.16 | 4.16 | differs | 8.69 | 2.55 | 0.93× | serde_yaml_ng | – |
| openapi-100k | typed | 102,778 | 7.84 | 8.01 | 8.01 | differs | 13.51 | 5.10 | 0.98× | serde_yaml_ng | – |
| openapi-10m | value | 10,486,088 | 4.43 | 4.46 | 4.46 | differs | 9.44 | 3.20 | 0.99× | serde_yaml_ng | – |
| openapi-10m | typed | 10,486,088 | 9.01 | 8.94 | 8.94 | differs | 14.71 | 3.20 | 1.01× | serde_yaml_ng | – |
| helm-values | value | 102,418 | 0.93 | 1.31 | 1.31 | differs | 1.65 | 0.64 | 0.71× | serde_yaml_ng | – |
| helm-values | typed | 102,418 | 0.86 | 1.31 | 1.31 | differs | 1.65 | 0.64 | 0.66× | serde_yaml_ng | – |

**Peak heap added by writing ÷ YAML input bytes (whole-process peak minus the baseline's, which holds the prepared data)**

| Case | Workload | Bytes | oneyaml | serde_yaml_ng | serde_norway | serde_yaml2 | serde-saphyr | serde_json |
|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | ≤ prep | ≤ prep | ≤ prep | n/a | ≤ prep | 0.58 |
| k8s-manifest | typed | 5,633 | ≤ prep | 7.08 | 7.08 | n/a | ≤ prep | ≤ prep |
| gha-workflow | value | 3,078 | ≤ prep | ≤ prep | ≤ prep | differs | ≤ prep | 0.50 |
| gha-workflow | typed | 3,078 | ≤ prep | 13.97 | 13.97 | differs | ≤ prep | ≤ prep |
| compose-merge | value | 2,442 | ≤ prep | ≤ prep | ≤ prep | n/a | ≤ prep | 0.31 |
| compose-merge | typed | 2,442 | ≤ prep | ≤ prep | ≤ prep | differs | ≤ prep | ≤ prep |
| k8s-multi | value | 1,050,323 | 0.25 | 0.14 | 0.14 | n/a | 0.22 | 0.14 |
| k8s-multi | typed | 1,050,323 | 0.30 | 0.25 | 0.25 | n/a | 0.98 | ≤ prep |
| openapi-100k | value | 102,778 | 0.95 | ≤ prep | ≤ prep | differs | ≤ prep | 0.45 |
| openapi-100k | typed | 102,778 | 1.25 | 1.49 | 1.49 | differs | 0.99 | 0.99 |
| openapi-10m | value | 10,486,088 | 1.37 | ≤ prep | ≤ prep | differs | 0.56 | 0.77 |
| openapi-10m | typed | 10,486,088 | 1.95 | 1.72 | 1.72 | differs | 1.71 | 0.11 |
| helm-values | value | 102,418 | ≤ prep | ≤ prep | ≤ prep | differs | ≤ prep | ≤ prep |
| helm-values | typed | 102,418 | ≤ prep | ≤ prep | ≤ prep | differs | ≤ prep | ≤ prep |

<!-- bench:ser-allocations:end -->

The third table replaces the read path's whole-process peak: the
baseline's peak is the prepared data, so the difference is what writing
adds at its peak, the output plus the writer's working set. `≤ prep`
means writing never rose above the preparation's peak, so DHAT can't tell
what it added. This table has no ratio columns: a hidden cell could be
below any visible one.

### Wall-clock (write, informational only)

Higher MB/s is better (MB of YAML input, for every library); the ratio
columns are time ratios.

<!-- bench:ser-wall:begin -->
Host: AMD Ryzen 9 7950X3D 16-Core Processor; load average (1/5/15 min) at start: 4.68 / 5.38 / 4.93. MB/s = 10⁶ bytes/s, median of ≥ 5 runs.

| Case | Workload | Bytes | oneyaml MB/s | serde_yaml_ng MB/s | serde_norway MB/s | serde_yaml2 MB/s | serde-saphyr MB/s | serde_json MB/s | oneyaml ÷ fastest feature-complete | fastest feature-complete | oneyaml ÷ serde_yaml2 (partial) | oneyaml ÷ serde_json |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | value | 5,633 | 171 | 90 | 89 | n/a | 105 | 1307 | 0.61× | serde-saphyr | – | 7.64× |
| k8s-manifest | typed | 5,633 | 131 | 83 | 84 | n/a | 94 | 1441 | 0.71× | serde-saphyr | – | 10.97× |
| gha-workflow | value | 3,078 | 267 | 115 | 112 | differs | 130 | 1682 | 0.49× | serde-saphyr | – | 6.31× |
| gha-workflow | typed | 3,078 | 155 | 86 | 86 | differs | 103 | 1380 | 0.66× | serde-saphyr | – | 8.92× |
| compose-merge | value | 2,442 | 123 | 57 | 57 | n/a | 62 | 798 | 0.51× | serde-saphyr | – | 6.51× |
| compose-merge | typed | 2,442 | 83 | 58 | 59 | differs | 61 | 898 | 0.74× | serde-saphyr | – | 10.77× |
| k8s-multi | value | 1,050,323 | 134 | 83 | 66 | n/a | 84 | 1100 | 0.62× | serde-saphyr | – | 8.19× |
| k8s-multi | typed | 1,050,323 | 137 | 84 | 85 | n/a | 97 | 1497 | 0.70× | serde-saphyr | – | 10.89× |
| openapi-100k | value | 102,778 | 211 | 95 | 93 | differs | 115 | 1306 | 0.55× | serde-saphyr | – | 6.18× |
| openapi-100k | typed | 102,778 | 94 | 47 | 49 | differs | 65 | 853 | 0.70× | serde-saphyr | – | 9.11× |
| openapi-10m | value | 10,486,088 | 107 | 79 | 78 | differs | 62 | 286 | 0.73× | serde_yaml_ng | – | 2.67× |
| openapi-10m | typed | 10,486,088 | 71 | 47 | 46 | differs | 58 | 442 | 0.81× | serde-saphyr | – | 6.21× |
| helm-values | value | 102,418 | 912 | 392 | 388 | differs | 482 | 5924 | 0.53× | serde-saphyr | – | 6.49× |
| helm-values | typed | 102,418 | 622 | 400 | 405 | differs | 464 | 5780 | 0.75× | serde-saphyr | – | 9.29× |
<!-- bench:ser-wall:end -->

## `Document`

yaml-edit 0.3.2 is the comparator: the other lossless YAML editor for
Rust. Both sides run on the real-world corpus at 1×; oneyaml under the
default settings with `Limits::unlimited()`, as in the parser tables.

- *parse*: oneyaml `Stream::parse_with(input, &settings)`, the strict
  entry point (a single-document input costs the same through
  `Document::parse_with`): the syntax tree plus the meaning check, so
  that `Document::parse` fails exactly where `from_str::<Value>` fails
  (duplicate keys, merge values, aliases, tags). yaml-edit:
  `YamlFile::parse`, which checks syntax only. The result is dropped;
  neither side walks it.
- *emit*: `to_string()` of the parsed, untouched document (parsing is
  preparation, not measured). The *emit = input* column checks that the
  output is byte-identical to the input, for both libraries.
- *retained*: parse, then keep the result, so DHAT's end-of-run heap (net
  of the baseline's) is what a parsed document keeps, including its own
  copy of the input. *parse blocks* are the allocations of a parse.

<!-- bench:document:begin -->
**Instructions (Ir/B; lower is better)**

| Case | Bytes | parse oneyaml | parse yaml-edit | parse ÷ yaml-edit | parity | emit oneyaml | emit yaml-edit | emit ÷ yaml-edit | parity | emit = input (oneyaml / yaml-edit) |
|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 432.7 | 495.2 | 0.87× | met | 47.2 | 442.9 | 0.11× | met | = / = |
| gha-workflow | 3,078 | 319.4 | 418.1 | 0.76× | met | 34.8 | 322.9 | 0.11× | met | = / = |
| compose-merge | 2,442 | 471.7 | 514.3 | 0.92× | met | 55.1 | 441.6 | 0.12× | met | = / = |
| k8s-multi | 1,050,323 | 417.6 | 435.2 | 0.96× | met | 46.0 | 436.9 | 0.11× | met | = / = |
| openapi-100k | 102,778 | 431.2 | 449.0 | 0.96× | met | 48.7 | 485.4 | 0.10× | met | = / = |
| openapi-10m | 10,486,088 | 419.4 | 438.6 | 0.96× | met | 47.7 | 477.8 | 0.100× | met | = / = |
| helm-values | 102,418 | 167.7 | 196.0 | 0.86× | met | 24.1 | 146.7 | 0.16× | met | = / = |

**Memory (DHAT; parse allocations net of the input-generation baseline; retained = the parsed stream/file kept, net of the baseline's end-of-run heap, including the parser's copy of the input where it keeps one)**

| Case | Bytes | parse blocks oneyaml | parse blocks yaml-edit | retained ÷ input oneyaml | retained ÷ input yaml-edit | target: retained ≤ 8× (≥ 100 KB) |
|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 104 | 2,454 | 4.74 | 10.76 | no target (< 100 KB) |
| gha-workflow | 3,078 | 97 | 1,062 | 3.99 | 8.31 | no target (< 100 KB) |
| compose-merge | 2,442 | 121 | 1,221 | 5.31 | 11.73 | no target (< 100 KB) |
| k8s-multi | 1,050,323 | 1,791 | 385,968 | 4.47 | 8.47 | met |
| openapi-100k | 102,778 | 225 | 40,298 | 4.38 | 9.12 | met |
| openapi-10m | 10,486,088 | 9,214 | 4,014,717 | 4.31 | 8.88 | met |
| helm-values | 102,418 | 94 | 16,284 | 2.44 | 3.16 | met |

Host: AMD Ryzen 9 7950X3D 16-Core Processor; load average (1/5/15 min) at start: 4.33 / 5.19 / 4.90. Median of ≥ 5 runs, in ms.

**Wall-clock (informational only)**

| Case | Bytes | parse ms oneyaml | parse ms yaml-edit | parse ÷ yaml-edit | emit ms oneyaml | emit ms yaml-edit | emit ÷ yaml-edit | emit ÷ parse (oneyaml) |
|---|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 0.138 | 0.141 | 0.98× | 0.012 | 0.133 | 0.094× | 0.090× |
| gha-workflow | 3,078 | 0.054 | 0.066 | 0.82× | 0.005 | 0.053 | 0.099× | 0.095× |
| compose-merge | 2,442 | 0.064 | 0.063 | 1.02× | 0.007 | 0.055 | 0.12× | 0.10× |
| k8s-multi | 1,050,323 | 28.385 | 26.555 | 1.07× | 2.200 | 25.478 | 0.086× | 0.078× |
| openapi-100k | 102,778 | 2.531 | 2.623 | 0.96× | 0.220 | 2.780 | 0.079× | 0.087× |
| openapi-10m | 10,486,088 | 279.430 | 320.958 | 0.87× | 22.308 | 279.914 | 0.080× | 0.080× |
| helm-values | 102,418 | 0.914 | 1.123 | 0.81× | 0.115 | 0.733 | 0.16× | 0.13× |
<!-- bench:document:end -->

### Edits

yaml-edit has a comparable edit API (`Document::set`/`remove` on its
rowan CST).

- *script*: a fixed script on the first document's root mapping: insert
  a new last entry `bench_edit_key: x`, replace its value with `'y'`,
  remove it, on a parsed document (parsing is preparation, not
  measured). oneyaml: `Stream::document_mut(0)`, `insert`, `set_path`,
  `remove`; yaml-edit: `Document::set` twice, `Document::remove`. The
  instruction columns are totals for one script on a fresh parse, not
  per byte: a local edit should cost the same at any document size. The
  script leaves the document as it found it, so the wall-clock columns
  repeat it on one tree.
- *round*: parse + script + `to_string`, from the input text.
- *output*: `=` when the text after the round is byte-identical to the
  input.

<!-- bench:edits:begin -->
**Instructions (lower is better)**

| Case | Bytes | script Ir oneyaml | script Ir yaml-edit | script ÷ yaml-edit | parity | round Ir/B oneyaml | round Ir/B yaml-edit | round ÷ yaml-edit | parity | output = input (oneyaml / yaml-edit) |
|---|---|---|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 567,325 | 1,058,947 | 0.54× | met | 586.4 | 1126.2 | 0.52× | met | = / = |
| gha-workflow | 3,078 | 851,028 | 1,870,315 | 0.46× | met | 634.4 | 1349.4 | 0.47× | met | = / = |
| compose-merge | 2,442 | 543,910 | 302,513 | 1.80× | **missed** | 756.5 | 1081.0 | 0.70× | met | = / = |
| k8s-multi | 1,050,323 | 568,328 | 1,786,870 | 0.32× | met | 469.1 | 873.7 | 0.54× | met | = / = |
| openapi-100k | 102,778 | 596,342 | 37,349,293 | 0.016× | met | 490.7 | 1297.3 | 0.38× | met | = / = |
| openapi-10m | 10,486,088 | 597,514 | 3,717,124,522 | 0.00016× | met | 472.1 | 1270.6 | 0.37× | met | = / = |
| helm-values | 102,418 | 1,620,284 | 11,823,172 | 0.14× | met | 209.6 | 457.5 | 0.46× | met | = / = |

Host: AMD Ryzen 9 7950X3D 16-Core Processor; load average (1/5/15 min) at start: 3.90 / 4.94 / 4.84. Median of ≥ 5 runs, in ms.

**Wall-clock (informational only)**

| Case | Bytes | script ms oneyaml | script ms yaml-edit | script ÷ yaml-edit | round ms oneyaml | round ms yaml-edit | round ÷ yaml-edit |
|---|---|---|---|---|---|---|---|
| k8s-manifest | 5,633 | 0.037 | 0.059 | 0.63× | 0.200 | 0.350 | 0.57× |
| gha-workflow | 3,078 | 0.061 | 0.101 | 0.61× | 0.130 | 0.221 | 0.59× |
| compose-merge | 2,442 | 0.037 | 0.017 | 2.15× | 0.118 | 0.136 | 0.86× |
| k8s-multi | 1,050,323 | 0.040 | 0.161 | 0.25× | 28.245 | 53.445 | 0.53× |
| openapi-100k | 102,778 | 0.041 | 2.008 | 0.020× | 2.908 | 7.208 | 0.40× |
| openapi-10m | 10,486,088 | 0.042 | 212.907 | 0.00020× | 290.360 | 823.438 | 0.35× |
| helm-values | 102,418 | 0.096 | 1.546 | 0.062× | 1.163 | 3.425 | 0.34× |
<!-- bench:edits:end -->
