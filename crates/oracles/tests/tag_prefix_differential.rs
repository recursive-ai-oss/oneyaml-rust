//! Tag prefix differential (YAML 1.2.2 §6.8.2.2): `%TAG` prefix and
//! shorthand suffix escape combinations, read by our parser, by a model of
//! the spec rule, and by saphyr-parser.
//!
//! The rule: a shorthand tag is the URI prefix + suffix, with the `%XX`
//! escapes of ASCII characters decoded as **one** string; the escapes of
//! bytes above 0x7F and `%25` stay as written, because tag characters are
//! compared as presented (§5.6), so a tag is never an error for the bytes it
//! escapes, and decoding `%25` would make two different spellings one tag.
//! saphyr-parser agrees wherever the tag has no such escape; it decodes
//! the prefix and the suffix each on its own, reads a multi-byte escape
//! as one character and `%25` as `%`, so tags with such an escape are
//! checked against the model only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use oracles::saphyr::Saphyr;
use test_suite::standard::{Oracle, ParserOracle};

/// Prefix starts (a prefix is never empty; `!` makes it local).
const PREFIX_HEADS: &[&str] = &["tag:x,1:", "!", "a:"];
/// Pieces of a prefix: plain text, ASCII escapes (`%25` is kept),
/// lead and continuation bytes of multi-byte characters, invalid bytes.
const PREFIX_PIECES: &[&str] = &[
    "",
    "p",
    "%21",
    "%25",
    "%2541",
    "%2F",
    "%20",
    "%7E",
    "%3A",
    "%2B",
    "%24",
    "%40",
    "%C3",
    "%E2%82",
    "%F0",
    "%F0%9F%98",
    "%FF",
    "%80",
    "%C3%A9",
];
/// Pieces of a suffix (`ns-tag-char`: no `!`, no flow indicators).
const SUFFIX_PIECES: &[&str] = &[
    "x",
    "%21",
    "%41",
    "%25",
    "%A9",
    "%AC",
    "%82%AC",
    "%9F%98%80",
    "%80",
    "%C3",
    "%FF",
    "%2C",
    "%3D",
    "%7C",
    "%26",
];
const SUFFIX_TAILS: &[&str] = &["", "x", "%A9", "%80"];
const HANDLES: &[&str] = &["!e!", "!", "!!"];

/// The `=VAL` line of the only scalar, or the error.
fn tag_line(o: &mut dyn Oracle, src: &str) -> Result<String, String> {
    let obs = o.observe(src);
    if let Some(e) = obs.error {
        return Err(e);
    }
    obs.events
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.starts_with("=VAL"))
        .ok_or_else(|| "no scalar".to_owned())
}

/// The model: `text` with the `%XX` escapes of ASCII characters other than
/// `%` decoded and the others kept as written. (Every piece above holds only complete
/// escapes.)
fn decode(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
            let v = u8::from_str_radix(h, 16).unwrap();
            if v.is_ascii() && v != b'%' {
                out.push(char::from(v));
            } else {
                out.push_str(&text[i..i + 3]);
            }
            i += 3;
        } else {
            out.push(char::from(b[i]));
            i += 1;
        }
    }
    out
}

/// Whether `text` has an escape that is kept as written: of a byte above
/// 0x7F, or `%25`.
fn has_kept_escape(text: &str) -> bool {
    text.as_bytes().windows(3).any(|w| {
        w[0] == b'%'
            && std::str::from_utf8(&w[1..])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .is_some_and(|v| v >= 0x80 || v == 0x25)
    })
}

#[test]
fn tag_prefix_escapes_agree_with_the_rule_and_saphyr_parser() {
    let (mut accepted, mut kept, mut compared) = (0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    for handle in HANDLES {
        for head in PREFIX_HEADS {
            for p1 in PREFIX_PIECES {
                for p2 in PREFIX_PIECES {
                    for s1 in SUFFIX_PIECES {
                        for s2 in SUFFIX_TAILS {
                            let prefix = format!("{head}{p1}{p2}");
                            let suffix = format!("{s1}{s2}");
                            let src = format!("%TAG {handle} {prefix}\n--- {handle}{suffix} v\n");
                            let ours = tag_line(&mut ParserOracle, &src);
                            // Ours follows the rule exactly.
                            let want = decode(&format!("{prefix}{suffix}"));
                            let line = format!("=VAL <{want}> :v");
                            if ours.as_ref() != Ok(&line) {
                                failures.push(format!("{src:?}: want {line:?}, ours {ours:?}"));
                                continue;
                            }
                            accepted += 1;
                            if has_kept_escape(&prefix) || has_kept_escape(&suffix) {
                                // saphyr-parser decodes these differently.
                                kept += 1;
                                continue;
                            }
                            compared += 1;
                            let theirs = tag_line(&mut Saphyr, &src);
                            if theirs.as_ref() != Ok(&line) {
                                failures.push(format!(
                                    "{src:?}: ours {line:?}, saphyr-parser {theirs:?}"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Every class is well covered.
    assert!(accepted > 10_000, "only {accepted} accepted");
    assert!(kept > 10_000, "only {kept} with an escape kept as written");
    assert!(
        compared > 5_000,
        "only {compared} compared with saphyr-parser"
    );
}

/// The review's cases, compared with saphyr-parser directly: the three
/// spellings of one URI agree with it.
#[test]
fn known_cases_agree_with_saphyr_parser() {
    for src in [
        "%TAG !e! tag:x,1:%21\n--- !e!a v\n",
        "--- !<tag:x,1:%21a> v\n",
        "%TAG !e! tag:x,1:\n--- !e!%21a v\n",
    ] {
        let ours = tag_line(&mut ParserOracle, src).unwrap();
        assert_eq!(ours, "=VAL <tag:x,1:!a> :v", "{src:?}");
        assert_eq!(tag_line(&mut Saphyr, src).unwrap(), ours, "{src:?}");
    }
}
