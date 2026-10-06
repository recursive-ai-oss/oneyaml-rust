# Vendored yaml-test-suite data

- **Upstream:** https://github.com/yaml/yaml-test-suite
- **Release tag:** `data-2022-01-17` (the latest data release; the `data` branch head carries the same date)
- **Tag commit:** `6e6c296ae9c9d2d5c4134b4b64d01b29ac19ff6f`
- **Downloaded from:** https://github.com/yaml/yaml-test-suite/archive/refs/tags/data-2022-01-17.tar.gz
- **Tarball SHA-256:** `ccae49c1d010d27c20d315969a97752f95b14642fc1e73755db4eb2df4bb0256`
- **Retrieved:** 2026-09-26
- **License:** MIT. The data tarball carries no license file, so `LICENSE` here was
  fetched from the matching source tag:
  https://raw.githubusercontent.com/yaml/yaml-test-suite/v2022-01-17/License

## What was vendored

Every test-case directory (`XXXX/` and multi-case `XXXX/NN/`) from the tarball
root, byte for byte: `===`, `in.yaml`, `test.event`, `error`, and where present
`in.json`, `out.yaml`, `emit.yaml`, `lex.token`.

**Not vendored:** the `name/` and `tags/` directories, which contain only
symlinks back to the case directories (an index by name and by tag).

Counts: 350 top-level directories, 402 cases, of which 94 are error cases
(an `error` file is present).

## Updating

Replace this directory's contents with a newer data release, update the tag,
commit, URL, checksum, and counts above, and re-run the harness. The
expected-count assertions in `crates/test-suite/tests/harness.rs` must be
updated to match, which makes a corpus change visible in review.

## Known defects

Checked 2026-09-27: `data-2022-01-17` is still the newest data release (the
`data` branch head, 6ad3d2c, carries the same files), and the generator
sources on `main` (da267a5) still contain the defects, so no upgrade fixes
them. Nine cases have expected events no correct parser can produce
(Y79Y/004-009 inherit Y79Y/003's tree and MUS6/01 inherits MUS6/00's,
because their source entries have no `tree:`; 4H7K and T833 omit flow
markers).
They are listed in `crates/test-suite/known-defects.txt` with a reason and a
corrected expectation (for 4H7K and T833, the recorded prefix with the
marker restored and nothing else changed; for MUS6/01, the explicit
document start before the directive; for Y79Y, the prefix derived by hand
from the spec), and the harness judges
them exactly against it like any other case; nothing is excused. The
vendored files themselves are unmodified.

### How error-case trees are generated

Checked 2026-09-27: for `fail: true` cases, `bin/yaml-to-test` fills `tree:`
with the output of `yamlpp-events` (the YAML::PP Perl parser), not the YAML
reference parser, and `bin/YAMLTestSuite.pm` (lines 83-90) copies the
previous entry's `tree:` into an entry that has none (the source of the
Y79Y/004-009 and MUS6/01 defects; MUS6/01's corrected `+STR; +DOC ---` is
what YAML::PP v0.41.0 outputs for its input). Today's YAML::PP (v0.41.0) reproduces
66 of the 94 recorded error trees, so the recorded trees mix output from
several generator versions, and today's YAML::PP is not by itself evidence
that a recorded tree is defective. 4H7K's recorded `-DOC` before the error
is therefore kept: the spec reference parser (YAML::Parser 0.0.5 via
YAML::PP::Ref 0.02) emits it (as it does for 9JBA, whose recorded tree has
no `-DOC`), yaml-rust2 0.13.0 reproduces the recorded prefix, and BS4K and
EB22 record the same `-DOC`-before-the-error pattern. Only its missing `[]`
marker is corrected.
