//! libFuzzer entry point of the `consistency` target (see `fuzz::targets::consistency`).
#![no_main]
// Cargo.toml: `fuzz_target!` expands to an `#[unsafe(no_mangle)]` entry
// point; the target body is in the `forbid(unsafe_code)` library.
#![allow(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| fuzz::targets::consistency(data));
