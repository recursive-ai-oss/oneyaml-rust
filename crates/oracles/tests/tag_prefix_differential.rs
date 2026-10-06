//! Tag prefix differential (YAML 1.2.2 §6.8.2.2): `%TAG` prefix and
//! shorthand suffix escape combinations, read by our parser, by a model of
//! the spec rule, and by saphyr-parser.
//!
//! The rule: a shorthand tag is the URI prefix + suffix, `%XX`-decoded as
//! **one** string; when the bytes are not UTF-8 the tag is `Y0013`.
//! saphyr-parser agrees wherever it reads the tag correctly; it has two
//! defects this corpus meets, kept apart and counted:
//!
//! * it decodes the prefix and the suffix each on its own, so a character
//!   split across them (`%C3` + `%A9`) is an error there;
//! * it decodes a multi-byte escape wrongly, in the prefix or the suffix
//!   (`%C3%A9` gives U+C3A9 `쎩`, not `é`), so tags that decode to
//!   non-ASCII are checked against the model only.
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
/// Pieces of a prefix: plain text, ASCII escapes (`%25` is a literal `%`),
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

/// The model: `text` with its `%XX` escapes decoded, if UTF-8. (Every
/// piece above holds only complete escapes.)
fn decode(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
            out.push(u8::from_str_radix(h, 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[test]
fn tag_prefix_escapes_agree_with_the_rule_and_saphyr_parser() {
    let (mut accepted, mut rejected, mut compared) = (0usize, 0usize, 0usize);
    let (mut split, mut multibyte) = (0usize, 0usize);
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
                            let Some(want) = decode(&format!("{prefix}{suffix}")) else {
                                match &ours {
                                    Err(e) if e.contains("Y0013") => rejected += 1,
                                    _ => {
                                        failures.push(format!("{src:?}: want Y0013, ours {ours:?}"))
                                    }
                                }
                                continue;
                            };
                            let line = format!("=VAL <{want}> :v");
                            if ours.as_ref() != Ok(&line) {
                                failures.push(format!("{src:?}: want {line:?}, ours {ours:?}"));
                                continue;
                            }
                            accepted += 1;
                            let theirs = tag_line(&mut Saphyr, &src);
                            let (dp, ds) = (decode(&prefix), decode(&suffix));
                            if dp.is_none() || ds.is_none() {
                                // Defect 1: a character split across the two.
                                split += 1;
                                if theirs.is_ok() {
                                    failures.push(format!("{src:?}: saphyr {theirs:?} (split)"));
                                }
                            } else if !want.is_ascii() {
                                // Defect 2: a multi-byte escape.
                                multibyte += 1;
                            } else {
                                compared += 1;
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
    assert!(rejected > 10_000, "only {rejected} rejected");
    assert!(
        compared > 5_000,
        "only {compared} compared with saphyr-parser"
    );
    assert!(split > 500, "only {split} split characters");
    assert!(multibyte > 500, "only {multibyte} multi-byte escapes");
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
