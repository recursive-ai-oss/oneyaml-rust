//! libFuzzer entry point of the `tree_edit` target (see `fuzz::extra::tree_edit`).
#![no_main]
// Cargo.toml: `fuzz_target!` expands to an `#[unsafe(no_mangle)]` entry
// point; the target body is in the `forbid(unsafe_code)` library.
#![allow(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| fuzz::extra::tree_edit(data));
