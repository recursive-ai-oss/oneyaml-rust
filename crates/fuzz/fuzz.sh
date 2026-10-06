#!/usr/bin/env bash
# Coverage-guided fuzzing of oneyaml with libFuzzer on the *stable*
# toolchain. See README.md.
#
#   crates/fuzz/fuzz.sh build                       # instrumented binaries
#   crates/fuzz/fuzz.sh run <target> <secs> [jobs]  # fuzz, new inputs in work/<target>
#   crates/fuzz/fuzz.sh seed [<migration-dir>]      # raw seeds -> minimised corpus/<target>
#   crates/fuzz/fuzz.sh cmin <target>               # re-minimise corpus/<target> (+ work/<target>)
#   crates/fuzz/fuzz.sh repro <target> <file>...    # run inputs through the instrumented binary
#
# `run` uses libFuzzer fork mode: it keeps going after a crash, timeout
# or OOM and leaves each one in work/<target>-artifacts/.
#
# Smoke mode, no fuzzer toolchain: `cargo test -p fuzz` replays the
# committed corpora and every regressions/<target>/ repro.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
triple="$(rustc -vV | sed -n 's/^host: //p')"
tdir="$(realpath -m "${ONEYAML_FUZZ_TARGET_DIR:-$root/target/fuzz}")"
bin="$tdir/$triple/release"
work="$(realpath -m "${ONEYAML_FUZZ_WORK:-$tdir/work}")"

# Per-target input size cap and per-input timeout (seconds). A timeout
# is a finding: every path is meant to be O(n).
targets="parse consistency from_slice serde edit render ser tree_edit cap handles"

max_len() {
  case "$1" in
    edit | tree_edit | handles) echo 2048 ;;
    consistency | serde | ser | cap) echo 4096 ;;
    *) echo 8192 ;;
  esac
}

build() {
  # SanitizerCoverage through stable codegen flags (what cargo-fuzz passes,
  # minus the nightly-only -Zsanitizer: the crates are forbid(unsafe_code)).
  # Debug assertions and overflow checks on: an internal `debug_assert!` or an
  # arithmetic overflow is a finding.
  RUSTFLAGS="-Cpasses=sancov-module \
    -Cllvm-args=-sanitizer-coverage-level=4 \
    -Cllvm-args=-sanitizer-coverage-inline-8bit-counters \
    -Cllvm-args=-sanitizer-coverage-pc-table \
    -Cllvm-args=-sanitizer-coverage-trace-compares \
    --cfg fuzzing -Cdebug-assertions -Coverflow-checks" \
    cargo build --release -p fuzz --features libfuzzer \
      $(printf -- "--bin %s " $targets) --target "$triple" --target-dir "$tdir"
}

cmd="${1:-}"
shift || true
case "$cmd" in
  build) build ;;
  run)
    t="$1"; secs="$2"; jobs="${3:-1}"
    mkdir -p "$work/$t" "$work/$t-artifacts"
    cd "$work/$t-artifacts"
    "$bin/$t" "$work/$t" "$here/corpus/$t" \
      -dict="$here/dict/yaml.dict" -max_len="$(max_len "$t")" \
      -timeout=10 -rss_limit_mb=4096 -max_total_time="$secs" \
      -artifact_prefix="$work/$t-artifacts/" -print_final_stats=1 \
      -fork="$jobs" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1
    ;;
  seed)
    raw="$tdir/seeds"
    rm -rf "$raw"
    cargo run --release -p fuzz --bin fuzz-seed -- seed "$raw" ${1:+"$1"}
    for t in $targets; do
      mkdir -p "$here/corpus/$t"
      "$bin/$t" -merge=1 -max_len="$(max_len "$t")" -timeout=10 -rss_limit_mb=4096 \
        -dict="$here/dict/yaml.dict" "$here/corpus/$t" "$raw/$t"
    done
    ;;
  cmin)
    # Edge-only set-cover merge: the fewest inputs that
    # keep every covered edge; hit-count features are not kept (about
    # 0.5 MB per target instead of 1-8 MB). `run` regrows a full-feature
    # corpus in work/<target> from this one.
    t="$1"
    new="$tdir/cmin-$t"
    rm -rf "$new"; mkdir -p "$new" "$work/$t"
    "$bin/$t" -set_cover_merge=1 -use_counters=0 -max_len="$(max_len "$t")" \
      -timeout=10 -rss_limit_mb=4096 \
      "$new" "$here/corpus/$t" "$work/$t"
    rm -rf "$here/corpus/$t"; mv "$new" "$here/corpus/$t"
    ;;
  repro)
    t="$1"; shift
    "$bin/$t" -rss_limit_mb=4096 -timeout=10 "$@"
    ;;
  *)
    sed -n '2,13p' "$0"
    exit 2
    ;;
esac
