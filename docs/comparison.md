# How oneyaml compares

This page compares oneyaml with the other Rust YAML libraries you are
likely to consider. **We wrote it, so it is biased**: we have tried to be
factual, to cite evidence for every claim, and to say plainly where the
others are the better choice. If something here is wrong or out of date,
please [open an issue](https://github.com/recursive-ai-oss/oneyaml-rust/issues/new/choose).

Facts about other libraries are limited to what this project has
measured or tested, at the versions named in
[docs/performance.md](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/performance.md#how-to-read-these-numbers)
and [docs/conformance.md](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md#parsers-under-test).
This page restates no benchmark number; it links to the tables.

## The libraries

- **`serde_yaml_ng`** and **`serde_norway`**: maintained forks of the
  archived `serde_yaml`. serde reads and writes, a `Value` type, and the
  `serde_yaml` API that most Rust code using YAML was written against.
  They parse with `unsafe-libyaml` (`serde_norway`: its fork
  `unsafe-libyaml-norway`), a C-to-Rust translation of libyaml, a
  YAML 1.1 parser.
- **serde-saphyr**: a serde layer, independent of the saphyr project,
  built on `granit-parser` (a fork of `saphyr-parser`).
- **saphyr** (`saphyr`, `saphyr-parser`) and **yaml-rust2**: pure-Rust
  event parsers with their own document types; no serde layer of their
  own (saphyr's `saphyr-serde` is a placeholder).
- **yaml-edit**: a lossless, comment-preserving YAML editor built on a
  rowan syntax tree; no serde layer.

## At a glance

| | oneyaml | serde_yaml_ng / serde_norway | serde-saphyr | yaml-edit |
|---|---|---|---|---|
| serde read and write | yes | yes | yes | no |
| Lossless editing that keeps comments | yes (`Document`) | no | no | yes |
| One parser behind both serde and editing | yes | n/a | n/a | n/a |
| YAML version read by default | 1.2.2, Core schema | 1.1 (libyaml) | see its docs | see its docs |
| Merge keys (`<<`) in typed reads | opt-in setting `merge_keys` | only through `Value::apply_merge` | expanded by default | n/a |
| Several documents in one stream | yes | yes | yes | see its docs |
| `unsafe` in the parser | none (`forbid(unsafe_code)`) | yes (`unsafe-libyaml`, `unsafe-libyaml-norway`) | not checked here | not checked here |
| yaml-test-suite | [conformance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md) | libyaml's row in [conformance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md#results-standard-criterion) | not measured | [conformance](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md#results-classification-and-round-trip) |

Sources: the comparator table in
[*Serde read*](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/performance.md#serde-read)
(documents, merge keys, value types),
[*What each parser does*](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/performance.md#what-each-parser-does)
(what each parser produces) and
[*Parsers under test*](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md#parsers-under-test).
"See its docs" means we haven't verified it, not that it's missing.

## Correctness

oneyaml, saphyr-parser and yaml-rust2 classify the yaml-test-suite the
same way under the standard criterion; libyaml, the parser behind
`serde_yaml_ng` and `serde_norway`, rejects some valid YAML 1.2 and
accepts some invalid input; yaml-edit's parser is lenient by design and
ships a separate validator. The counts, and why the saphyr and yaml-rust2
scores are a weaker claim than ours (their event APIs can't express
everything the suite checks), are in
[docs/conformance.md](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/conformance.md).
Every difference between oneyaml's `Compat::serde_yaml()` reading and
`serde_yaml_ng`'s is listed, with its reason, in the
[migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/migrating-from-serde-yaml.md#divergences).

## Performance

The headline comparison is CPU instructions (valgrind), which are
deterministic; wall-clock times are measured too, and reported where they
disagree. Each serde row is compared with the fastest feature-complete
serde library on that row, and `Document` rows with yaml-edit.

**Where oneyaml is ahead:** serde writes, against every feature-complete
comparator, in instructions and in wall-clock time; `Document` parsing
against yaml-edit in instructions on every workload (wall-clock is at
parity or slightly slower on three of the five in the README table);
and the edit script against yaml-edit on every workload above a few
kilobytes.

**Where the others win:**

- **Wall-clock serde reads.** oneyaml's typed reads use fewer
  instructions than `serde_yaml_ng` and `serde_norway` on most workloads,
  but in wall-clock time oneyaml is slower on four of the five
  workloads of the README table, merge-key files included; only on the
  largest (k8s-multi) is it level.
  If wall-clock read speed is what you are optimising, measure on your
  own files.
- **Some serde reads at parity.** `Value` reads of docker-compose-shaped
  files with merge keys, and typed reads of OpenAPI-shaped files, take a
  few percent more instructions in oneyaml than in `serde_yaml_ng` or
  `serde_norway`.
- **Edits on small documents.** Below a few kilobytes, yaml-edit's edit
  script costs fewer instructions: oneyaml pays a fixed cost per edit to
  decide which entry owns each nearby comment and to check that the
  edited text still reads as the tree says. That cost doesn't grow with
  the document, which is why oneyaml wins on larger files.
- **Partial libraries.** serde_yaml2 is cheaper on some reads, but it
  reads only the first document and has no merge keys, so it isn't
  ranked.

The tables: the README's
[benchmark summary](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/crates/oneyaml/README.md),
and every workload in
[docs/performance.md](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/performance.md),
with the commands to reproduce them.

## Which one to choose

- **You read and write config files with serde, and also need to edit
  some without losing comments:** oneyaml; that is the case it is built
  for, one interpretation for both.
- **You have working `serde_yaml` code and no reason to change:** stay on
  `serde_yaml_ng` or `serde_norway`. If you want YAML 1.2, no `unsafe`, or
  diagnostics with codes and fixes, the
  [migration guide](https://github.com/recursive-ai-oss/oneyaml-rust/blob/v1.0.1/docs/migrating-from-serde-yaml.md)
  starts with a `use` swap.
- **You only edit YAML, mostly small files, and don't need serde:**
  yaml-edit is a reasonable choice; oneyaml's edits refuse, with a
  named error, a few comment layouts it can't edit safely (listed on
  `Document`).
- **You need an event parser or your own document model:**
  `oneyaml-syntax` exposes a lossless event stream and syntax tree;
  saphyr-parser and yaml-rust2 give lighter events without comments or
  white space.
