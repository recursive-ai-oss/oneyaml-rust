//! The edit oracles excuse exactly the documented exception
//! (the `-syntax` tree docs, *Recovered input*, "Known exception"): its
//! repro is recognised, and the same edit without the shape's
//! ingredients (lenient settings, the byte order marks) is not excused.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use oneyaml_core::Settings;
use oneyaml_syntax::tree::{Fragment, SyntaxTree, TreeEditError, compare_trees};

/// The PLAN repro: lenient, a failed directive block whose document ends
/// in a `Y0028` warning, quotes and byte order marks, then
/// `append_child(root, "---\nq\n")`.
const REPRO: &str = "%YAML 1.2\nc: x\n\u{feff}'q\n\u{feff}---\na: b: z\nc: d\n\u{feff}...\nw: x\n";

fn edited(src: &str, s: &Settings) -> Option<(SyntaxTree, SyntaxTree)> {
    let mut t = SyntaxTree::parse_with(src, s);
    let root = t.root().id();
    t.append_child(root, Fragment::parse_with("---\nq\n", s).unwrap())
        .ok()?;
    let again = SyntaxTree::parse_with(t.emit(), s);
    Some((t, again))
}

#[test]
fn the_documented_repro_is_excused() {
    let s = Settings::new().lenient_inline_colons(true);
    let (t, again) = edited(REPRO, &s).expect("the repro's edit is accepted");
    assert!(
        compare_trees(&t, &again).is_err(),
        "the repro no longer differs: the backlog item may be fixed; drop the filter"
    );
    assert!(fuzz::known_exception(&t, &again));
}

#[test]
fn the_shape_needs_every_ingredient() {
    // Without lenient settings, or without byte order marks, the edit
    // agrees with its re-parse (or is refused), and nothing is excused.
    let strict = Settings::new();
    let lenient = Settings::new().lenient_inline_colons(true);
    // The strict case is not vacuous. Without
    // lenient settings the failed directive block's document is an error
    // region, so the repro's edit beside it is refused (*Recovered
    // input*): nothing to excuse. If this ever changes, the edit must
    // agree with its re-parse, unexcused.
    let mut t = SyntaxTree::parse_with(REPRO, &strict);
    let root = t.root().id();
    let r = t.append_child(root, Fragment::parse_with("---\nq\n", &strict).unwrap());
    assert!(
        matches!(r, Err(TreeEditError::ReadOnly { .. })),
        "strict: {:?}",
        r.map(|_| ())
    );
    assert_eq!(t.emit(), REPRO);
    let no_bom = REPRO.replace('\u{feff}', "");
    if let Some((t, again)) = edited(&no_bom, &lenient) {
        assert!(compare_trees(&t, &again).is_ok(), "{no_bom:?}");
        assert!(!fuzz::known_exception(&t, &again), "{no_bom:?}");
    }
}

/// The shape alone is not enough; the mismatch must involve a
/// quoted scalar or a byte order mark. The repro's tree compared with
/// itself (the same tokens) is not excused; nor is the repro's edited
/// tree (which shows the shape: it is excused against its re-parse, see
/// above) against a re-parse whose text also differs at a plain scalar
/// before any quote or mark differs (trees that differ away
/// from any quote or mark).
#[test]
fn the_mismatch_must_be_at_a_quote_or_a_byte_order_mark() {
    let s = Settings::new().lenient_inline_colons(true);
    let t = SyntaxTree::parse_with(REPRO, &s);
    let same = SyntaxTree::parse_with(REPRO, &s);
    assert!(!fuzz::known_exception(&t, &same));
    let (edited, again) = edited(REPRO, &s).expect("the repro's edit is accepted");
    assert!(fuzz::known_exception(&edited, &again));
    // `c: x` (before the quote) read as `c: y`: the first differing token
    // is a plain scalar.
    let text = edited.emit();
    assert!(text.starts_with("%YAML 1.2\nc: x\n"));
    let other = SyntaxTree::parse_with(text.replacen("c: x", "c: y", 1), &s);
    assert!(compare_trees(&edited, &other).is_err());
    assert!(!fuzz::known_exception(&edited, &other));
}
