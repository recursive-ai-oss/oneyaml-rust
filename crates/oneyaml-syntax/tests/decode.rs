//! Decoding: every encoding, with and without a byte
//! order mark, detection per YAML 1.2.2 §5.2, malformed input, and the
//! byte-exact round trip through the parser.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use oneyaml_core::Span;
use oneyaml_core::code::{INVALID_UTF8, INVALID_UTF16, INVALID_UTF32};
use oneyaml_syntax::event::{EventKind, ScalarStyle};
use oneyaml_syntax::{Encoding, check_contract, decode, decode_strict, encode, first_error, parse};

const ALL: [Encoding; 5] = [
    Encoding::Utf8,
    Encoding::Utf16Le,
    Encoding::Utf16Be,
    Encoding::Utf32Le,
    Encoding::Utf32Be,
];

/// ASCII first (detection without a BOM needs it), then BMP, astral, and
/// CRLF content, and a second document with its own BOM (§9.1.1).
const SAMPLE: &str = "a: é\r\nb: [\"\u{1F600}\", 'ü']\n# ✓\n...\n\u{feff}--- |\n  x\u{10FFFF}\n";

/// Parses the decoded text and checks the contract and the tiling.
fn parse_round_trip(text: &str) -> Vec<String> {
    let events = parse(text);
    check_contract(text, &events).unwrap();
    let rebuilt: String = events.sourced().map(|e| e.text().unwrap()).collect();
    assert_eq!(rebuilt.as_bytes(), text.as_bytes());
    events
        .sourced()
        .filter_map(|e| match e.kind() {
            EventKind::Scalar { .. } => e.value().map(|v| v.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn every_encoding_round_trips_byte_for_byte() {
    let expected = parse_round_trip(SAMPLE);
    assert_eq!(expected, ["a", "é", "b", "\u{1F600}", "ü", "x\u{10FFFF}\n"]);
    for enc in ALL {
        for bom in [false, true] {
            let text = if bom {
                format!("\u{feff}{SAMPLE}")
            } else {
                SAMPLE.to_owned()
            };
            let bytes = encode(&text, enc).unwrap();
            let d = decode(&bytes);
            assert_eq!(d.encoding(), enc, "{enc:?} bom={bom}");
            assert!(d.is_valid() && d.diagnostics().is_empty());
            assert_eq!(d.has_bom(), bom);
            assert_eq!(d.text(), text);
            // The parser sees the BOM as trivia; values are unaffected.
            assert_eq!(parse_round_trip(d.text()), expected);
            // The encoding is all that is needed to re-encode exactly.
            assert_eq!(encode(d.text(), d.encoding()).unwrap(), bytes);
            assert_eq!(d.reencode(), bytes);
        }
    }
}

#[test]
fn valid_utf8_is_borrowed() {
    let bytes = b"\xEF\xBB\xBFa: b\n";
    let d = decode(bytes);
    assert!(matches!(
        d.clone().into_text(),
        std::borrow::Cow::Borrowed(_)
    ));
    assert_eq!(d.encoding(), Encoding::Utf8);
    assert!(d.has_bom());
    assert_eq!(d.text().as_bytes(), bytes);
}

#[test]
fn detection_follows_the_spec_table() {
    let cases: &[(&[u8], Encoding)] = &[
        (b"", Encoding::Utf8),
        (b"a", Encoding::Utf8),
        (b"\xEF\xBB\xBFa", Encoding::Utf8),
        (b"\x00\x00\xFE\xFF", Encoding::Utf32Be),
        (b"\x00\x00\x00a", Encoding::Utf32Be),
        (b"\xFF\xFE\x00\x00", Encoding::Utf32Le),
        (b"a\x00\x00\x00", Encoding::Utf32Le),
        (b"\xFE\xFF", Encoding::Utf16Be),
        (b"\x00a", Encoding::Utf16Be),
        (b"\xFF\xFE", Encoding::Utf16Le),
        (b"a\x00", Encoding::Utf16Le),
        // Three bytes cannot be UTF-32 (the table needs four).
        (b"\x00\x00\x00", Encoding::Utf16Be),
        // "any, x00" is UTF-16LE whatever the first byte.
        (b"\xE9\x00", Encoding::Utf16Le),
        // No BOM and no null byte: UTF-8.
        (b"\xC3\xA9", Encoding::Utf8),
    ];
    for (bytes, enc) in cases {
        assert_eq!(oneyaml_syntax::detect_encoding(bytes), *enc, "{bytes:?}");
    }
    // UTF-16 without a BOM whose first character is not ASCII is not
    // detectable (§5.2): it is read as UTF-8, and is invalid there.
    let bytes = encode("\u{e9e9}: x", Encoding::Utf16Be).unwrap();
    let d = decode(&bytes);
    assert_eq!(d.encoding(), Encoding::Utf8);
    assert!(!d.is_valid());
}

/// Decodes `bytes`, asserting the expected encoding and error runs, the
/// diagnostics, contract-valid parsing, and the exact re-encoding.
fn check_invalid(bytes: &[u8], enc: Encoding, text: &str, runs: &[(usize, usize)]) {
    let d = decode(bytes);
    assert_eq!(d.encoding(), enc, "{bytes:?}");
    assert_eq!(d.text(), text, "{bytes:?}");
    assert!(!d.is_valid());
    let got: Vec<(usize, usize)> = d
        .invalid_runs()
        .iter()
        .map(|r| (r.input.start, r.input.end))
        .collect();
    assert_eq!(got, runs, "{bytes:?}");
    let code = match enc {
        Encoding::Utf8 => INVALID_UTF8,
        Encoding::Utf16Le | Encoding::Utf16Be => INVALID_UTF16,
        _ => INVALID_UTF32,
    };
    assert_eq!(d.diagnostics().len(), runs.len());
    for (diag, run) in d.diagnostics().iter().zip(d.invalid_runs()) {
        assert_eq!(diag.code, code);
        assert_eq!(diag.severity, oneyaml_core::Severity::Error);
        assert_eq!(diag.primary.span, run.text);
        assert_eq!(d.text().get(run.text.start..run.text.end), Some("\u{FFFD}"));
        assert_eq!(&bytes[run.input.start..run.input.end], run.bytes);
        assert!(diag.help.as_deref().unwrap().contains("§5.2"));
    }
    parse_round_trip(d.text());
    assert_eq!(d.reencode(), bytes, "byte-identical for {bytes:?}");
}

#[test]
fn malformed_utf8() {
    // A lone continuation byte, a truncated sequence, an overlong form, a
    // surrogate, and a truncated sequence at the end.
    check_invalid(b"a: \x80b\n", Encoding::Utf8, "a: \u{FFFD}b\n", &[(3, 4)]);
    check_invalid(b"a\xE2\x82", Encoding::Utf8, "a\u{FFFD}", &[(1, 3)]);
    check_invalid(b"a\xC0\xAFb", Encoding::Utf8, "a\u{FFFD}b", &[(1, 3)]);
    check_invalid(b"\xED\xA0\x80", Encoding::Utf8, "\u{FFFD}", &[(0, 3)]);
    // Separate runs stay separate; adjacent ones merge.
    check_invalid(
        b"\xFFa\xFE\xFF",
        Encoding::Utf8,
        "\u{FFFD}a\u{FFFD}",
        &[(0, 1), (2, 4)],
    );
    check_invalid(
        b"x\xFFy\xFF\xFE",
        Encoding::Utf8,
        "x\u{FFFD}y\u{FFFD}",
        &[(1, 2), (3, 5)],
    );
}

#[test]
fn malformed_utf16() {
    // An unpaired high surrogate, an unpaired low surrogate, a reversed
    // pair, and a trailing odd byte.
    let hi = [b'a', 0, 0x00, 0xD8, b'b', 0];
    check_invalid(&hi, Encoding::Utf16Le, "a\u{FFFD}b", &[(2, 4)]);
    let lo = [0, b'a', 0xDC, 0x00];
    check_invalid(&lo, Encoding::Utf16Be, "a\u{FFFD}", &[(2, 4)]);
    let reversed = [0, b'a', 0xDC, 0x00, 0xD8, 0x00, 0, b'b'];
    check_invalid(&reversed, Encoding::Utf16Be, "a\u{FFFD}b", &[(2, 6)]);
    let odd = [b'a', 0, b'b'];
    check_invalid(&odd, Encoding::Utf16Le, "a\u{FFFD}", &[(2, 3)]);
    let bom_odd = [0xFE, 0xFF, 0];
    check_invalid(&bom_odd, Encoding::Utf16Be, "\u{feff}\u{FFFD}", &[(2, 3)]);
}

#[test]
fn malformed_utf32() {
    // A surrogate, a value above U+10FFFF, and trailing bytes.
    let surrogate = [b'a', 0, 0, 0, 0x00, 0xD8, 0, 0];
    check_invalid(&surrogate, Encoding::Utf32Le, "a\u{FFFD}", &[(4, 8)]);
    let big = [0, 0, 0, b'a', 0x00, 0x11, 0x00, 0x00, 0, 0, 0, b'b'];
    check_invalid(&big, Encoding::Utf32Be, "a\u{FFFD}b", &[(4, 8)]);
    let tail = [0, 0, 0, b'a', 0, 0];
    check_invalid(&tail, Encoding::Utf32Be, "a\u{FFFD}", &[(4, 6)]);
    let both = [b'a', 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 1];
    check_invalid(&both, Encoding::Utf32Le, "a\u{FFFD}", &[(4, 9)]);
}

#[test]
fn garbage_never_panics_and_always_reencodes() {
    // Deterministic pseudo-random bytes, biased towards nulls and surrogate
    // and BOM bytes so every decoder path runs.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let pool = [
        0u8, 0, 0xD8, 0xDC, 0xFE, 0xFF, 0xEF, 0xBB, 0xBF, b'a', b':', b' ', b'\n', 0x80, 0xC3,
    ];
    for _ in 0..5_000 {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let len = usize::try_from(x.wrapping_shr(59)).unwrap();
        let bytes: Vec<u8> = (0..len)
            .map(|k| {
                x = x.rotate_left(7) ^ u64::try_from(k).unwrap();
                pool[usize::try_from(x.checked_rem(15).unwrap()).unwrap()]
            })
            .collect();
        let d = decode(&bytes);
        assert_eq!(d.reencode(), bytes);
        assert_eq!(d.diagnostics().len(), d.invalid_runs().len());
        // The strict one-pass decode agrees with both.
        match decode_strict(&bytes) {
            Ok(s) => {
                assert!(d.is_valid());
                assert_eq!((s.text(), s.encoding()), (d.text(), d.encoding()));
            }
            Err(e) => {
                assert_eq!(Some(*e.clone()), first_error(&bytes));
                assert_eq!(e.0, d.diagnostics()[0]);
            }
        }
        let events = parse(d.text());
        check_contract(d.text(), &events).unwrap();
    }
}

#[test]
fn decoded_text_feeds_the_parser() {
    let bytes = encode("\u{feff}k: 'v'\n", Encoding::Utf16Be).unwrap();
    let d = decode(&bytes);
    let events = parse(d.text());
    let scalar = events
        .sourced()
        .find_map(|e| match e.kind() {
            EventKind::Scalar {
                style: ScalarStyle::SingleQuoted,
                ..
            } => Some((e.value().unwrap(), e.span())),
            _ => None,
        })
        .unwrap();
    assert_eq!(scalar.0, "v");
    // Spans are UTF-8 byte ranges into the decoded text.
    assert_eq!(scalar.1, Span::new(6, 9));
}
