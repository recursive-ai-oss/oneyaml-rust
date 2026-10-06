//! Collectors keep the diagnostics
//! that start first, not the first to arrive, so warnings alias replay
//! reports late (with early spans) still give the capped list exactly the
//! uncapped list's first entries, and the summary its place.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Compat, Limits, Value, code};

#[test]
fn alias_replayed_warnings_keep_the_capped_prefix_in_order() {
    // An alias replay no longer repeats the anchored node's
    // duplicate warning (each source duplicate is reported once), so the
    // late arrivals here are the builder's own order: a duplicate key's
    // warning (`d`) is recorded after its value's (`y`, `z`), which start
    // later.
    let src = "a: &a {x: 1, x: 2}\nd: 1\nd: {y: 1, y: 2, z: 1, z: 2}\nb: *a\n";
    let full = oneyaml::from_str_with_diagnostics::<Value>(
        src,
        &Compat::serde_yaml().limits(Limits::default().max_diagnostics(usize::MAX)),
    )
    .unwrap()
    .1;
    assert_eq!(full.len(), 4, "{full:?}");
    for limit in [0usize, 1, 2, 3] {
        let s = Compat::serde_yaml().limits(Limits::default().max_diagnostics(limit));
        let (_, w) = oneyaml::from_str_with_diagnostics::<Value>(src, &s).unwrap();
        let key = |d: &oneyaml::Diagnostic| (d.code, d.primary.span, d.message.clone());
        let kept: Vec<_> = w[..limit].iter().map(key).collect();
        let want: Vec<_> = full[..limit].iter().map(key).collect();
        assert_eq!(kept, want, "limit {limit}");
        let summary = &w[limit];
        assert_eq!(summary.code, code::DIAGNOSTICS_SUPPRESSED);
        assert_eq!(
            summary.primary.span.start, full[limit].primary.span.start,
            "limit {limit}"
        );
        let n = full.len() - limit;
        let noun = if n == 1 { "diagnostic" } else { "diagnostics" };
        assert_eq!(summary.message, format!("{n} more {noun} suppressed"));
    }
}
