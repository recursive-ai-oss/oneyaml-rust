//! The memory compactions change
//! no outcome. A strict `from_slice` that stops at the first invalid run
//! reports the error a whole decode reports; compactly stored runs still
//! give the input back, per document of a stream too; and lookup
//! indexes held as slots stay exact across clones and edits.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml::{Document, Settings, Stream, Value, path};

/// Inputs with invalid bytes in each encoding, runs merged and apart,
/// at both ends and after line breaks.
const INVALID: [&[u8]; 8] = [
    b"\x80a: \xff\xfe\n- \xc3\n\xe2\x82",
    b"a: 1\nb: [2,\n  \xff]\nc: \xfe\n",
    b"\xff\xfe\x00\xd8a\x00:\x00 \x00\x00\xdc",
    b"\xfe\xff\xd8\x00\x00a\x00\n\xdc\x00\xdc\x00",
    b"\x00\x00\xfe\xff\x00\x11\x00\x00\x00\x00\x00a\xff",
    b"\x80",
    b"a: b\n\xc0\xc1\xf5\xff",
    b"a\n---\nb: \xff\n---\n\xfe c\n",
];

/// The 1-based line and column (in characters) of byte `at` of `text`; a
/// byte order mark at the start of the text is not counted.
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at];
    let line = before.matches('\n').count() + 1;
    let mut col = before.rsplit('\n').next().unwrap().chars().count() + 1;
    if line == 1 && at > 0 && text.starts_with('\u{feff}') {
        col -= 1;
    }
    (line, col)
}

#[test]
fn strict_from_slice_reports_the_first_run_as_a_whole_decode_does() {
    let s = Settings::new();
    for bytes in INVALID {
        let recovering = Stream::from_slice_recovering(bytes, &s);
        let first = recovering.diagnostics()[0].clone();
        let text = recovering.value().to_string();
        let (line, column) = line_col(&text, first.primary.span.start);
        for e in [
            Document::from_slice_with(bytes, &s).map(drop).unwrap_err(),
            Stream::from_slice_with(bytes, &s).map(drop).unwrap_err(),
            oneyaml::from_slice_with::<Value>(bytes, &s).unwrap_err(),
        ] {
            assert_eq!(e.diagnostic(), &first, "{bytes:?}");
            let at = e.location().unwrap();
            assert_eq!((at.line(), at.column()), (line, column), "{bytes:?}");
            assert!(e.warnings().is_empty());
        }
    }
    // Valid input still parses, in every encoding.
    let bom = b"\xff\xfea\x00:\x00 \x001\x00\n\x00";
    assert_eq!(
        Document::from_slice(bom).unwrap().to_bytes().as_slice(),
        bom
    );
}

#[test]
fn compact_runs_give_the_input_back() {
    let s = Settings::new();
    for bytes in INVALID {
        let p = Document::from_slice_recovering(bytes, &s);
        assert!(p.has_errors(), "{bytes:?}");
        // (A second document is an error of `Document`; the bytes stay.)
        assert_eq!(p.value().to_bytes().as_slice(), bytes, "{bytes:?}");
        let st = Stream::from_slice_recovering(bytes, &s).into_value();
        let joined: Vec<u8> = st.documents().flat_map(|d| d.to_bytes()).collect();
        assert_eq!(joined.as_slice(), bytes, "{bytes:?}");
    }
    // Each document of a stream gives back its own runs only.
    let bytes = b"a: \xff\n---\nb: \xfe\xfd\n";
    let st = Stream::from_slice_recovering(bytes, &s).into_value();
    let docs: Vec<Vec<u8>> = st.documents().map(|d| d.to_bytes()).collect();
    assert_eq!(
        docs,
        [b"a: \xff\n".to_vec(), b"---\nb: \xfe\xfd\n".to_vec()]
    );
    let per_doc: Vec<usize> = st.documents().map(|d| d.diagnostics().len()).collect();
    assert_eq!(per_doc, [1, 1]);
}

/// Lookup indexes hold slots: a clone shares them for the elements it was
/// cloned with, and each copy's edits keep its own exact.
#[test]
fn slot_indexes_survive_clones_and_edits() {
    let src: String = (0..200).map(|i| format!("k{i}: {i}\n")).collect();
    let mut a = Document::parse(src.as_str()).unwrap();
    assert_eq!(a.get_path(&path!["k150"]).unwrap().text(), "150");
    let mut b = a.clone();
    let _ = a.remove_path(&path!["k3"]).unwrap();
    b.set_path(&path!["k3"], "x").unwrap();
    b.set_path(&path!["new"], 7).unwrap();
    a.set_path(&path!["k199"], [1, 2]).unwrap();
    for (d, k3) in [(&a, None), (&b, Some("x"))] {
        assert_eq!(
            d.get_path(&path!["k3"])
                .map(|n| n.text().into_owned())
                .as_deref(),
            k3
        );
        assert_eq!(d.get_path(&path!["k150"]).unwrap().text(), "150");
        assert_eq!(d.check_lookup_indexes(), Ok(1 + usize::from(k3.is_none())));
    }
    assert_eq!(b.get_path(&path!["new"]).unwrap().text(), "7");
    assert!(a.get_path(&path!["new"]).is_none());
}
