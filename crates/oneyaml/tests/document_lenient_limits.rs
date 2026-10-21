//! A known limitation under
//! `lenient_inline_colons`, pinned so that relaxing it is deliberate.
//!
//! An edit whose validation window holds a `Y0028` warning attached to an
//! element the edit moves may be refused as `NotWellFormed` even when its
//! result is well formed. The three refused cases below all have a
//! well-formed result (checked: it parses cleanly under the same settings);
//! the accepted neighbours show the refusal is specific to them. When the
//! syntax layer's diagnostic-attachment check is relaxed, the
//! refused cases here should start to pass and this file is updated on
//! purpose.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, EditError, Settings, code, path};

fn lenient() -> Settings {
    Settings::new().lenient_inline_colons(true)
}

fn doc(src: &str) -> Document {
    let d = Document::parse_with(src, &lenient()).unwrap();
    assert!(
        d.diagnostics()
            .iter()
            .any(|w| w.code == code::LENIENT_INLINE_COLON),
        "{src:?} has a Y0028 warning"
    );
    d
}

/// `want` is well formed: it parses without errors under the same
/// settings (so a refusal of the edit producing it is conservative).
fn well_formed(want: &str) {
    let d = Document::parse_with(want, &lenient()).unwrap();
    assert!(!d.has_errors(), "{want:?}");
}

fn refused(r: Result<String, EditError>, want: &str, src: &str) {
    well_formed(want);
    assert!(
        matches!(r, Err(EditError::NotWellFormed { .. })),
        "{src:?}: expected NotWellFormed, got {r:?}"
    );
}

#[test]
fn case_1_appending_after_a_lenient_last_entry_is_refused() {
    let src = "b: 1\nk: x: z\n";
    let mut d = doc(src);
    let r = d.set_path(&path!["n"], 1).map(|_| ());
    let r = r.map(|()| d.to_string());
    refused(r, "b: 1\nk: x: z\nn: 1\n", src);
    // Nothing changed.
    assert_eq!(d.to_string(), src);
    // Accepted neighbours: the same append when an entry follows the
    // lenient one, and when it comes first.
    let mut d = doc("b: 1\nk: x: z\nc: 2\n");
    d.set_path(&path!["n"], 1).unwrap();
    assert_eq!(d.to_string(), "b: 1\nk: x: z\nc: 2\nn: 1\n");
    let mut d = doc("k: x: z\nb: 1\n");
    d.set_path(&path!["n"], 1).unwrap();
    assert_eq!(d.to_string(), "k: x: z\nb: 1\nn: 1\n");
}

#[test]
fn case_2_replacing_a_lenient_value_that_ends_the_text_is_refused() {
    // No final line break: the warning is attached after the last element.
    let src = "b: 1\nk: x: z";
    let mut d = doc(src);
    let r = d.set_path(&path!["k"], "v").map(|_| ());
    let r = r.map(|()| d.to_string());
    refused(r, "b: 1\nk: v", src);
    assert_eq!(d.to_string(), src);
    // With a final line break the same replacement is accepted (as
    // the warning leaves with its text).
    let mut d = doc("b: 1\nk: x: z\n");
    d.set_path(&path!["k"], "v").unwrap();
    assert_eq!(d.to_string(), "b: 1\nk: v\n");
    assert!(d.diagnostics().is_empty());
    // An edit before the warning that moves nothing it is attached to.
    let mut d = doc(src);
    d.set_path(&path!["b"], 12345).unwrap();
    assert_eq!(d.to_string(), "b: 12345\nk: x: z");
}

#[test]
fn case_3_removing_the_entry_after_a_lenient_one_is_refused() {
    let src = "k: x: z\nb: 1\n";
    let mut d = doc(src);
    let r = d.remove_path(&path!["b"]).map(|_| ());
    let r = r.map(|()| d.to_string());
    refused(r, "k: x: z\n", src);
    assert_eq!(d.to_string(), src);
    // Removing the entry before it is accepted.
    let mut d = doc("b: 1\nk: x: z\n");
    let _removed = d.remove_path(&path!["b"]).unwrap();
    assert_eq!(d.to_string(), "k: x: z\n");
}
