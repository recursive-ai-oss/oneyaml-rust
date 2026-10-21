//! The CI smoke mode of the fuzz targets: every committed input
//! of `corpus/<target>` and `regressions/<target>` runs through its target,
//! without the fuzzer toolchain. A regression repro stays here for good
//! (a bug is closed when its repro is in the corpus and green).
//!
//! `ONEYAML_FUZZ_REPLAY=<target>` replays one target only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

fn dir(kind: &str, target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(kind)
        .join(target)
}

fn files(d: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(d) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

/// One replay job: target name, target entry point, input path.
type Job = (&'static str, fn(&[u8]), PathBuf);

#[test]
fn every_target_has_a_corpus() {
    for (name, _) in fuzz::TARGETS {
        assert!(
            !files(&dir("corpus", name)).is_empty(),
            "corpus/{name} is empty"
        );
        assert!(fuzz::target(name).is_some());
    }
    assert!(fuzz::target("nope").is_none());
}

/// Every input on worker threads with a generous stack (debug builds of
/// the typed serde reads use more stack per level than the release
/// fuzzer; the `max_typed_depth` docs give the requirement); a
/// failing input is reported by path, and every one is run.
#[test]
#[ignore = "slow: runs in CI"]
fn committed_corpora_and_regressions_replay_green() {
    let only = std::env::var("ONEYAML_FUZZ_REPLAY").ok();
    let mut jobs: Vec<Job> = Vec::new();
    for (name, f) in fuzz::TARGETS {
        if only.as_deref().is_some_and(|o| o != *name) {
            continue;
        }
        for kind in ["corpus", "regressions"] {
            for path in files(&dir(kind, name)) {
                jobs.push((name, *f, path));
            }
        }
    }
    let next = AtomicUsize::new(0);
    let failed = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get().min(16));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            std::thread::Builder::new()
                .stack_size(64 << 20)
                .spawn_scoped(scope, || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((name, f, path)) = jobs.get(i) else {
                            break;
                        };
                        let bytes = std::fs::read(path).unwrap();
                        if catch_unwind(AssertUnwindSafe(|| f(&bytes))).is_err() {
                            failed
                                .lock()
                                .unwrap()
                                .push(format!("{name}: {}", path.display()));
                        }
                    }
                })
                .unwrap();
        }
    });
    let failed = failed.into_inner().unwrap();
    eprintln!(
        "{} inputs replayed, {} failed, {} mismatches excused as the known exception",
        jobs.len(),
        failed.len(),
        fuzz::known_exception_hits()
    );
    assert!(failed.is_empty(), "failing inputs:\n{}", failed.join("\n"));
}
