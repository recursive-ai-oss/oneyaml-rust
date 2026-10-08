//! Records which Cargo profile built `bench`, so the tool can refuse to
//! publish tables from any build but the default release build, and keep
//! the `measure` profile (see the workspace `Cargo.toml`) to comparing
//! oneyaml with itself; and records with the numbers what it can see of
//! the build's settings (`src/build_notes.rs`).
//!
//! Cargo does not tell a build script the profile's name; the output
//! directory does: `<target>/[<triple>/]<profile>/build/<package>/out`.
//! Nor does it pass every setting: overrides through `--config` or a
//! config file are invisible here. Profile overrides by environment
//! variable are visible, and Cargo accepts them only in the documented
//! forms `CARGO_PROFILE_<profile>_<key>` and
//! `CARGO_PROFILE_<profile>_BUILD_OVERRIDE_<key>` (not per package), so
//! the script reruns when any of those changes for a profile that builds
//! into the `release` or `measure` directory: a note is never stale for a
//! build that may produce tables or probes.

use std::path::Path;

#[path = "src/build_notes.rs"]
mod build_notes;

/// Every profile setting Cargo reads from an environment variable.
const PROFILE_KEYS: [&str; 13] = [
    "CODEGEN_UNITS",
    "DEBUG",
    "SPLIT_DEBUGINFO",
    "DEBUG_ASSERTIONS",
    "INCREMENTAL",
    "LTO",
    "OVERFLOW_CHECKS",
    "OPT_LEVEL",
    "PANIC",
    "RPATH",
    "STRIP",
    "INHERITS",
    "TRIM_PATHS",
];

/// The profiles whose variables can reach a build in the `release` or
/// `measure` directory: `release`, `bench` (which builds into
/// `release`), and `measure`.
const PROFILES: [&str; 3] = ["RELEASE", "BENCH", "MEASURE"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/build_notes.rs");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    for profile in PROFILES {
        for key in PROFILE_KEYS {
            println!("cargo:rerun-if-env-changed=CARGO_PROFILE_{profile}_{key}");
            println!("cargo:rerun-if-env-changed=CARGO_PROFILE_{profile}_BUILD_OVERRIDE_{key}");
        }
    }
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    let profile = Path::new(&out)
        .ancestors()
        .nth(3)
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_owned();
    println!("cargo:rustc-env=BENCH_BUILD_PROFILE={profile}");
    let opt_level = std::env::var("OPT_LEVEL").ok();
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let notes = build_notes::notes(opt_level.as_deref(), std::env::vars(), &flags);
    println!("cargo:rustc-env=BENCH_BUILD_NOTES={notes}");
}
