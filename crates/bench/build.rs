//! Records which Cargo profile built `bench`, so the tool can refuse to
//! measure from any build but the `measure` profile (see the workspace
//! `Cargo.toml`), and records with the numbers what it can see of the
//! build's settings.
//!
//! Cargo does not tell a build script the profile's name; the output
//! directory does: `<target>/[<triple>/]<profile>/build/<package>/out`.
//! Nor does it pass every setting: overrides through `--config` or a
//! config file are invisible here. What is visible (an optimization level
//! other than 3, `CARGO_PROFILE_MEASURE_*` variables, extra compiler
//! flags) is recorded, so the `environment` table shows it; the table
//! names the profile and never asserts settings it cannot see.

use std::path::Path;

/// The profile settings that can be overridden by environment variable.
const PROFILE_KEYS: [&str; 10] = [
    "CODEGEN_UNITS",
    "OPT_LEVEL",
    "LTO",
    "DEBUG",
    "DEBUG_ASSERTIONS",
    "OVERFLOW_CHECKS",
    "INCREMENTAL",
    "PANIC",
    "STRIP",
    "INHERITS",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    for key in PROFILE_KEYS {
        println!("cargo:rerun-if-env-changed=CARGO_PROFILE_MEASURE_{key}");
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

    let mut notes = Vec::new();
    match std::env::var("OPT_LEVEL") {
        Ok(level) if level == "3" => {}
        Ok(level) => notes.push(format!("opt-level {level}")),
        Err(_) => notes.push("opt-level unknown".to_owned()),
    }
    let mut overrides: Vec<String> = std::env::vars()
        .filter_map(|(k, v)| {
            let key = k.strip_prefix("CARGO_PROFILE_MEASURE_")?;
            Some(format!("{key}={v}"))
        })
        .collect();
    overrides.sort();
    if !overrides.is_empty() {
        notes.push(format!("overridden: {}", overrides.join(" ")));
    }
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    let flags: Vec<&str> = flags.split('\u{1f}').filter(|f| !f.is_empty()).collect();
    if !flags.is_empty() {
        notes.push(format!("compiler flags: {}", flags.join(" ")));
    }
    // One line: `|` and newlines would break the table cell.
    let notes = notes.join("; ").replace(['|', '\n', '\r'], " ");
    println!("cargo:rustc-env=BENCH_BUILD_NOTES={notes}");
}
