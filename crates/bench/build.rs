//! Records which Cargo profile built `bench`, so the tool can refuse to
//! measure from any build but the `measure` profile (see the workspace
//! `Cargo.toml`) and record that build with the numbers.
//!
//! Cargo does not tell a build script the profile's name; the output
//! directory does: `<target>/[<triple>/]<profile>/build/<package>/out`.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = std::env::var("OUT_DIR").unwrap_or_default();
    let profile = Path::new(&out)
        .ancestors()
        .nth(3)
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_owned();
    println!("cargo:rustc-env=BENCH_BUILD_PROFILE={profile}");
}
