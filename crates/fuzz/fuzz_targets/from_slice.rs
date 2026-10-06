//! libFuzzer entry point of the `from_slice` target (see `fuzz::targets::from_slice`).
#![no_main]
// Cargo.toml: `fuzz_target!` expands to an `#[unsafe(no_mangle)]` entry
// point; the target body is in the `forbid(unsafe_code)` library.
#![allow(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| fuzz::targets::from_slice(data));
