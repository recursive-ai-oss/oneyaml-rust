//! The location of the error after a document ended by
//! a byte order mark (U+FEFF, invisible) counts no mark, and recovery keeps
//! the content after it as the next document.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml::{Document, Stream, code};

#[test]
fn the_location_counts_no_invisible_mark() {
    let src = "a: 1\n\u{feff}b: 2\nc: 3\n";
    let e = Stream::parse(src).unwrap_err();
    assert_eq!(e.code(), code::CONTENT_AFTER_DOCUMENT);
    let at = e.location().unwrap();
    assert_eq!((at.line(), at.column()), (2, 1));
    assert!(e.to_string().ends_with("(2:1)"), "{e}");
    let d = e.diagnostic();
    assert!(d.help.as_deref().unwrap().contains("U+FEFF"));
    assert_eq!(
        &src[d.secondary[0].span.start..d.secondary[0].span.end],
        "\u{feff}"
    );
}

#[test]
fn recovery_keeps_the_content_after_the_mark() {
    let src = "a: 1\n\u{feff}b: 2\nc: 3\n";
    let s = Stream::parse_recovering(src).into_value();
    assert_eq!(s.len(), 2);
    let second = s.document(1).unwrap().to_value().unwrap();
    assert_eq!(oneyaml::to_string(&second).unwrap(), "b: 2\nc: 3\n");
    assert_eq!(s.to_string(), src);
    let d = Document::parse_recovering("k:\n- a\n\u{feff}- b\n").into_value();
    assert_eq!(
        d.diagnostics()[0].code,
        code::CONTENT_AFTER_DOCUMENT,
        "{:?}",
        d.diagnostics()
    );
}
