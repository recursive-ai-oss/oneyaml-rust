//! Reading a float costs the same allocations whatever its value: a zero
//! (`0.0`, `0e5`) takes no extra step for the exact-value form that only a
//! nonzero value outside the range of a double needs.
//!
//! Counting is per thread (`allocation-counter`, an `unsafe impl
//! GlobalAlloc` dev-dependency, so here: the only crate allowed one).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Value, from_str};

/// Allocations made by reading `src` on this thread (after a warm-up).
fn reads(src: &str) -> u64 {
    let run = || {
        std::hint::black_box(from_str::<Value>(std::hint::black_box(src)).unwrap());
    };
    run();
    allocation_counter::measure(run).count_total
}

#[test]
fn zero_floats_allocate_as_much_as_any_float() {
    let base = reads("a: 1.5\n");
    for zero in ["0.0", "0e5", "-0.0", ".0", "0.000e-3", "0_0.0_0"] {
        assert_eq!(reads(&format!("a: {zero}\n")), base, "{zero}");
    }
}

#[test]
fn out_of_range_floats_keep_their_value() {
    // The exact-value form is still taken where it is needed.
    let a: Value = from_str("{1e400: x, 2e400: y, 1e-400: z, 0e400: w}\n").unwrap();
    assert_eq!(a.as_mapping().unwrap().len(), 4);
    let b: Value = from_str("{1e-400: x, 0.0: y}\n").unwrap();
    assert_eq!(b.as_mapping().unwrap().len(), 2);
}
