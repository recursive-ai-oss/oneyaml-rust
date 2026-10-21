//! Release-gate fixes:
//! stream documents give their bytes back, alias navigation scales, strict
//! errors carry serde's path, and diagnostics are listed consistently.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

mod work;

use std::collections::BTreeMap;

use oneyaml::{Document, Settings, Stream, Value, code};

#[test]
fn stream_documents_restore_invalid_bytes() {
    let bytes = b"a: \xff\n--- b: \xfe\n";
    let st = Stream::from_slice_recovering(bytes, &Settings::new()).into_value();
    assert_eq!(st.to_bytes(), bytes);
    let docs: Vec<_> = st.documents().collect();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs[0].to_bytes(), b"a: \xff\n"); // its region
    assert_eq!(docs[1].to_bytes(), b"--- b: \xfe\n");
    // UTF-16: re-encoded, with the invalid unit restored.
    let mut utf16 = oneyaml_syntax::encode("a\n--- ", oneyaml::Encoding::Utf16Le).unwrap();
    utf16.extend_from_slice(&[0x00, 0xd8]); // a lone surrogate
    utf16.extend(oneyaml_syntax::encode("\n", oneyaml::Encoding::Utf16Le).unwrap());
    let st = Stream::from_slice_recovering(&utf16, &Settings::new()).into_value();
    let mut second = oneyaml_syntax::encode("--- ", oneyaml::Encoding::Utf16Le).unwrap();
    second.extend_from_slice(&[0x00, 0xd8]);
    // The document's region ends with its line break.
    second.extend(oneyaml_syntax::encode("\n", oneyaml::Encoding::Utf16Le).unwrap());
    assert_eq!(st.document(1).unwrap().to_bytes(), second);
    assert_eq!(st.to_bytes(), utf16);
}

#[test]
fn alias_navigation_scales_linearly() {
    // `n` anchors, then `n` aliases, then a miss that visits every
    // alias. The alias target is found in O(log n) (it was a scan back over
    // every anchor: O(aliases × anchors)).
    fn doc(n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!("a{i}: &x{i} {{v: {i}}}\n"));
        }
        s.push_str("all: [");
        for i in 0..n {
            s.push_str(&format!("*x{i}, "));
        }
        s.push_str("]\n");
        s
    }
    let time = |n: usize| {
        let d = Document::parse(doc(n)).unwrap();
        let all = d.get("all").unwrap().as_sequence().unwrap();
        work::reset();
        let mut found = 0usize;
        for item in all.iter() {
            let target = item.alias_target().unwrap();
            found += usize::from(target.get("v").is_some());
            assert!(item.get("nope").is_none());
        }
        assert_eq!(found, n);
        work::taken()
    };
    let small = time(2_000);
    let large = time(16_000);
    // 8× the input: linear is ~8×, the old scan ~64×.
    let bound = small.saturating_mul(24);
    assert!(large <= bound, "2000: {small:?}, 16000: {large:?}");
    // Redefinitions: the closest preceding anchor wins (§3.2.2.2), and an
    // alias in another document never sees this one's anchors.
    let d = Document::parse("a: &x 1\nb: *x\nc: &x 2\nd: *x\n").unwrap();
    assert_eq!(
        d.get("b")
            .unwrap()
            .alias_target()
            .unwrap()
            .as_scalar()
            .unwrap()
            .value(),
        "1"
    );
    assert_eq!(
        d.get("d")
            .unwrap()
            .alias_target()
            .unwrap()
            .as_scalar()
            .unwrap()
            .value(),
        "2"
    );
    let st = Stream::parse_recovering("a: &x 1\n---\nb: *x\n").into_value();
    let second = st.document(1).unwrap();
    assert!(second.get("b").unwrap().alias_target().is_none());
}

#[test]
fn strict_and_typed_errors_carry_the_path() {
    let s = Settings::new().merge_keys(true);
    for (src, path) in [
        ("a: 1\nb: {x: [1, {k: 1, k: 2}]}\n", "b.x[1]"),
        ("a: 1\nb: {x: [1, {<<: 5}]}\n", "b.x[1]"),
        ("b: {x: [1, {k: !!int z}]}\n", "b.x[1].k"),
    ] {
        let serde = oneyaml::from_str_with::<Value>(src, &s).unwrap_err();
        let strict = Document::parse_with(src, &s).unwrap_err();
        // A typed read reports the first error it meets: with `a: 1`
        // first, a map target's type error at `a`, as with merge keys off.
        let nested = oneyaml::from_str_with::<BTreeMap<String, BTreeMap<String, Value>>>(src, &s)
            .unwrap_err();
        if src.starts_with("a: 1") {
            assert_eq!(
                nested.code(),
                oneyaml::code::TYPE_MISMATCH,
                "{src:?}: {nested}"
            );
            assert_eq!(nested.path().unwrap().to_string(), "a");
        } else {
            assert_eq!(nested.to_string(), serde.to_string());
        }
        let typed =
            oneyaml::from_str_with::<BTreeMap<String, serde_json::Value>>(src, &s).unwrap_err();
        for e in [&serde, &strict, &typed] {
            assert_eq!(e.path().unwrap().to_string(), path, "{src:?}: {e}");
            assert!(e.to_string().contains(&format!(" at {path} (")), "{e}");
        }
        assert_eq!(strict.to_string(), serde.to_string());
        assert_eq!(typed.to_string(), serde.to_string());
    }
    // A known example.
    let e = Document::parse("a: 1\nb: {k: 1, k: 2}\n").unwrap_err();
    assert_eq!(e.to_string(), "duplicate key `k` at b (2:11)");
}

#[test]
fn recovering_parse_always_reports_a_second_document() {
    // With an earlier error, the second document is still reported, in
    // source order with the rest.
    for src in ["a: [1\n--- b\n", "!!int x\n--- b\n", "a\n--- b\n"] {
        let p = Document::parse_recovering(src);
        let codes: Vec<_> = p.diagnostics().iter().map(|d| d.code).collect();
        assert!(
            codes.contains(&code::MULTIPLE_DOCUMENTS),
            "{src:?}: {codes:?}"
        );
        let starts: Vec<_> = p
            .diagnostics()
            .iter()
            .map(|d| d.primary.span.start)
            .collect();
        assert!(
            starts.windows(2).all(|w| w[0] <= w[1]),
            "{src:?}: {starts:?}"
        );
        assert_eq!(p.value().diagnostics(), p.diagnostics());
        // The strict parse reports the same error at the same place when
        // it comes first.
        if let Err(e) = Document::parse(src) {
            if e.code() == code::MULTIPLE_DOCUMENTS {
                let d = p.diagnostics().iter().find(|d| d.code == e.code()).unwrap();
                assert_eq!(Some(d.primary.span), e.span());
            }
        }
    }
    assert!(!Document::parse_recovering("a\n").has_errors());
}

#[test]
fn stream_from_slice() {
    let st = Stream::from_slice(b"a: 1\n--- b\n").unwrap();
    assert_eq!(st.len(), 2);
    assert_eq!(st.settings(), &Settings::new());
    let e = Stream::from_slice(b"a: \xff\n").unwrap_err();
    assert_eq!(e.code(), code::INVALID_UTF8);
    let utf16 = oneyaml_syntax::encode("\u{feff}x\n--- y\n", oneyaml::Encoding::Utf16Be).unwrap();
    let st = Stream::from_slice(&utf16).unwrap();
    assert_eq!(
        st.document(1).unwrap().to_value().unwrap()["y"],
        Value::Null
    );
    assert_eq!(
        st.document(1).unwrap().to_value().unwrap(),
        Value::from("y")
    );
    assert_eq!(st.to_bytes(), utf16);
}

#[test]
fn boundary_diagnostics_belong_to_one_document() {
    // Real (suite N782 truncated): the first document ends exactly where
    // the second starts, and a diagnostic starts there too. It was listed
    // under both; it belongs to the document that starts there.
    for src in ["[\n---", "[\n--- ", "[\n--- ,"] {
        let st = Stream::parse_recovering(src).into_value();
        let docs: Vec<_> = st.documents().collect();
        assert_eq!(docs[0].span().end, docs[1].span().start, "{src:?}");
        let at = docs[1].span().start;
        assert!(st.diagnostics().iter().any(|d| d.primary.span.start == at));
        for d in &st.diagnostics() {
            let owners = docs
                .iter()
                .filter(|doc| doc.diagnostics().contains(d))
                .count();
            assert!(owners <= 1, "{src:?}: {d:?} listed {owners} times");
            if d.primary.span.start == at {
                assert!(docs[1].diagnostics().contains(d), "{src:?}");
            }
        }
    }
    // A diagnostic at the very end of the input stays with the last
    // document.
    let st = Stream::parse_recovering("a\n--- [b\n").into_value();
    let last = st.document(1).unwrap();
    assert!(last.has_errors());
    let listed: usize = st.documents().map(|d| d.diagnostics().len()).sum();
    assert_eq!(listed, st.diagnostics().len());
    assert!(!st.document(0).unwrap().has_errors());
}
