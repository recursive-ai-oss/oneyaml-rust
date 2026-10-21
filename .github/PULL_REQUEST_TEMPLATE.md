## What this changes

<!-- One or two sentences. Link the issue (`Fixes #…`). -->

## How it is tested

<!-- The test that fails without this change. For a bug fix, the input
     that showed the bug is now in the corpus. -->

## Checklist

- [ ] `cargo t1` passes (CI runs the full set).
- [ ] `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` pass.
- [ ] Any change to golden output, corpora or benchmark baselines is explained.
- [ ] No new runtime dependency, no `unsafe`, no panicking path.
- [ ] `CHANGELOG.md` has an entry; a breaking change is flagged as one.
- [ ] Any instruction-count or allocation regression is explained.
- [ ] Documentation is updated, and no generated `bench:` region was edited by hand.
