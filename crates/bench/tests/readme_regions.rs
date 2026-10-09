//! The committed benchmark regions are what `bench readme` writes from the
//! committed tables, and have the shape readers rely on: each README shows
//! the compact chart and one link, no table; the summary in
//! `docs/performance.md` holds the tables; and the README image's alt text
//! carries every number the chart shows.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use bench::readme::{
    self, CHART_SECTION, PERFORMANCE, README, README_SECTION, ROOT_README, SUMMARY_SECTION,
};

fn read(path: &str) -> String {
    std::fs::read_to_string(readme::workspace_root().join(path)).unwrap()
}

/// The git ref the README region links name (`main` day to day, the tag on
/// a release commit).
fn current_ref(region: &str) -> String {
    let needle = format!("{}/blob/", readme::REPO);
    let at = region.find(&needle).unwrap() + needle.len();
    region[at..].split('/').next().unwrap().to_owned()
}

/// The `alt` attribute of the region's image, unescaped.
fn alt(region: &str) -> String {
    let at = region.find("<img alt=\"").unwrap() + "<img alt=\"".len();
    let end = region[at..].find('"').unwrap();
    region[at..at + end]
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[test]
fn committed_regions_are_generated_and_shaped_for_readers() {
    let doc = read(PERFORMANCE);
    let sections = readme::summary(&doc).unwrap();
    let summary = readme::section_body(&doc, SUMMARY_SECTION).unwrap();
    assert_eq!(
        summary,
        readme::summary_body(&doc).unwrap(),
        "run `bench readme`"
    );
    // The summary holds one table per operation, the notes and the run.
    assert_eq!(readme::tables(summary).len(), sections.len());
    assert!(summary.contains("- **× faster** = "));
    assert!(summary.contains("- **× faster (wall-clock)** = "));
    assert!(summary.contains("- **Measured** "));
    assert!(!summary.contains("https://"), "relative links only");

    for (path, section) in [(README, README_SECTION), (ROOT_README, CHART_SECTION)] {
        let text = read(path);
        let region = readme::section_body(&text, section).unwrap();
        let git_ref = current_ref(region);
        assert_eq!(
            region,
            readme::readme_body(&doc, &git_ref).unwrap(),
            "{path}: run `bench readme`"
        );
        // The chart and one link to the summary, nothing else.
        assert!(readme::tables(region).is_empty(), "{path}: no tables");
        assert_eq!(region.matches("](").count(), 1, "{path}: one link");
        assert!(region.contains(&format!(
            "({}/blob/{git_ref}/{PERFORMANCE}#results-at-a-glance)",
            readme::REPO
        )));
        assert!(!region.contains("**Measured**") && !region.contains("wall-clock)** ="));
        // The alt text carries the chart's numbers: per operation its
        // range, its counts and each slower row with its ratio.
        let alt = alt(region);
        assert_eq!(alt, readme::compact_alt_text(&sections));
        assert!(alt.contains("CPU instruction counts, not wall-clock time"));
        for sec in &sections {
            let sp = readme::spread(sec);
            let at = alt.find(&format!(" {} vs ", sec.short)).unwrap();
            assert!(alt[at..].contains(&format!(": {}; ", sp.range)), "{alt}");
            for (count, _) in &sp.counts {
                assert!(alt.contains(count.as_str()), "{count}");
            }
            for slower in &sp.slower {
                assert!(alt.contains(slower.as_str()), "{slower}");
            }
        }
    }
}

/// The committed charts are what the generator draws from the committed
/// tables: the compact chart is the only benchmark content on the READMEs,
/// so a stale or hand-edited SVG must not pass.
#[test]
fn committed_charts_are_generated() {
    let doc = read(PERFORMANCE);
    let charts = readme::charts(&doc).unwrap();
    let paths: Vec<&str> = charts.iter().map(|(p, _)| *p).collect();
    assert_eq!(
        paths,
        [
            readme::CHART_LIGHT,
            readme::CHART_DARK,
            readme::DETAIL_LIGHT,
            readme::DETAIL_DARK
        ]
    );
    for (path, svg) in &charts {
        assert!(
            read(path) == *svg,
            "{path} is not what `bench readme` draws from {PERFORMANCE}; run `bench readme`"
        );
    }
}
