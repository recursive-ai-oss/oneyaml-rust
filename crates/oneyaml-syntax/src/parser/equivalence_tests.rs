//! The byte-run fast paths against plain character-at-a-time versions.
//!
//! Each fast path in `scan`, `scalar` and `run` has a one-line
//! specification written here the slow way (one `char` at a time, no
//! tricks); random text built from the characters that matter (quotes,
//! backslashes, `:`, `#`, flow indicators, tabs, every break style, non-ASCII
//! characters, a byte order mark, controls) is run through both.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use oneyaml_core::Code;
use oneyaml_core::code::{INVALID_ESCAPE, INVALID_QUOTED_CHARACTER, UNTERMINATED_QUOTED_SCALAR};

use super::scalar::{quoted, split_lines, trim_white};
use super::scan::{
    any_marker_at, char_at, comment_at, comment_extent, is_flow_indicator, is_ns_char, is_white,
    key_too_long, line_end, mark_at, plain_line_end, plain_line_end_lenient,
};

/// A tiny deterministic generator: no clock, no global state.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap()
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.next() % items.len()]
    }
}

/// Text from pieces that make the scanners change their mind.
fn random_text(rng: &mut Lcg, pieces: &[&str], max: usize) -> String {
    (0..rng.next() % max).map(|_| rng.pick(pieces)).collect()
}

const MIXED: &[&str] = &[
    "a",
    "bc",
    "word",
    " ",
    "  ",
    "\t",
    ":",
    ": ",
    "#",
    " #",
    ",",
    "[",
    "]",
    "{",
    "}",
    "-",
    "?",
    "\n",
    "\r",
    "\r\n",
    "\u{e9}",
    "\u{1f600}",
    "\u{feff}",
    "\u{7f}",
    "\u{1}",
    "\u{85}",
    "\u{2028}",
    "---",
    "...",
    "'",
    "\"",
    "\\",
];

/// Every char boundary of `s`, and one past the end.
fn offsets(s: &str) -> impl Iterator<Item = usize> + '_ {
    s.char_indices().map(|(i, _)| i).chain([s.len()])
}

// ---- the plain scalar line --------------------------------------------------------------

/// `plain_line_end` and its lenient form as one character loop.
fn plain_line_end_plain(src: &str, start: usize, flow: bool, colons: bool) -> usize {
    let mut i = start;
    let mut end = start;
    let char_after = |i: usize| src[i..].chars().next();
    let safe_at =
        |i: usize| char_after(i).is_some_and(|c| is_ns_char(c) && !(flow && is_flow_indicator(c)));
    while let Some(c) = char_after(i) {
        match c {
            ' ' | '\t' => {
                i += 1;
                continue;
            }
            ':' if !colons && !safe_at(i + 1) => break,
            '#' if i > start && is_white(src.as_bytes()[i - 1]) => break,
            c if !is_ns_char(c) || (flow && is_flow_indicator(c)) => break,
            _ => {}
        }
        i += c.len_utf8();
        end = i;
    }
    end
}

#[test]
fn plain_line_end_matches_the_character_loop() {
    let mut rng = Lcg(11);
    for round in 0..3000 {
        let src = random_text(&mut rng, MIXED, 40);
        for start in offsets(&src) {
            for flow in [false, true] {
                assert_eq!(
                    plain_line_end(&src, start, flow),
                    plain_line_end_plain(&src, start, flow, false),
                    "round {round}: {src:?} from {start} flow {flow}"
                );
            }
            assert_eq!(
                plain_line_end_lenient(&src, start),
                plain_line_end_plain(&src, start, false, true),
                "round {round}: {src:?} from {start} lenient"
            );
        }
    }
}

#[test]
fn long_plain_lines_match_the_character_loop() {
    // Long enough for the block scan, with the stop byte at every offset
    // around the word and block boundaries.
    let words = ["alpha", "beta", " ", "gamma", "d\u{e9}lta", "x"];
    let mut rng = Lcg(5);
    for _ in 0..300 {
        let mut src = random_text(&mut rng, &words, 120);
        let at = rng.next() % (src.len() + 1);
        let at = (0..=at).rev().find(|k| src.is_char_boundary(*k)).unwrap();
        src.insert_str(at, rng.pick(&[": x", " # c", ",", "\t", "\n", ":"]));
        for flow in [false, true] {
            assert_eq!(
                plain_line_end(&src, 0, flow),
                plain_line_end_plain(&src, 0, flow, false),
                "{src:?} flow {flow}"
            );
        }
    }
}

// ---- small readers ----------------------------------------------------------------------

#[test]
fn small_readers_match_their_plain_definitions() {
    let mut rng = Lcg(23);
    for _ in 0..3000 {
        let src = random_text(&mut rng, MIXED, 24);
        for i in 0..=src.len() + 1 {
            // A position inside a character is no character.
            let char_ref = src.get(i..).and_then(|s| s.chars().next());
            assert_eq!(char_at(&src, i), char_ref, "{src:?} {i}");
            let mark_ref = src.get(i..).is_some_and(|r| r.starts_with('\u{feff}'));
            assert_eq!(mark_at(&src, i), mark_ref, "{src:?} {i}");
            let marker_ref = src.get(i..).is_some_and(|r| {
                (r.starts_with("---") || r.starts_with("..."))
                    && r.chars()
                        .nth(3)
                        .is_none_or(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
            });
            assert_eq!(any_marker_at(&src, i), marker_ref, "{src:?} {i}");
            let comment_ref = src.as_bytes().get(i) == Some(&b'#')
                && src.get(..i).is_some_and(|before| {
                    matches!(
                        before.chars().next_back(),
                        None | Some(' ' | '\t' | '\n' | '\r' | '\u{feff}')
                    )
                });
            assert_eq!(comment_at(&src, i), comment_ref, "{src:?} {i}");
        }
    }
}

#[test]
fn key_length_check_counts_characters_not_bytes() {
    let wide = "\u{e9}".repeat(1024);
    assert!(!key_too_long(&wide, 0, wide.len()));
    let wide = "\u{e9}".repeat(1025);
    assert!(key_too_long(&wide, 0, wide.len()));
    let narrow = "a".repeat(1024);
    assert!(!key_too_long(&narrow, 0, narrow.len()));
    let narrow = "a".repeat(1025);
    assert!(key_too_long(&narrow, 0, narrow.len()));
    // Not on a character boundary: not too long, never a panic.
    assert!(!key_too_long(&wide, 1, wide.len()));
    assert!(!key_too_long("ab", 3, 1));
}

// ---- comment lines ----------------------------------------------------------------------

#[test]
fn comment_extent_is_the_line_end_and_whether_it_is_plain() {
    let mut rng = Lcg(31);
    for round in 0..3000 {
        let src = format!("# {}", random_text(&mut rng, MIXED, 40));
        let (end, clean) = comment_extent(&src, 0);
        assert_eq!(end, line_end(&src, 0), "round {round}: {src:?}");
        let plain = src[..end]
            .bytes()
            .all(|b| b == b'\t' || (0x20..=0x7e).contains(&b));
        assert_eq!(clean, plain, "round {round}: {src:?}");
    }
    // Long comments, the odd byte at every offset around the block size.
    for len in 0..70usize {
        for at in 0..len {
            for odd in ["\u{e9}", "\u{feff}", "\u{1}", "\u{7f}", "\t"] {
                let mut text = "x".repeat(len);
                text.replace_range(at..=at, odd);
                let src = format!("#{text}\nnext");
                let (end, clean) = comment_extent(&src, 0);
                assert_eq!(end, src.find('\n').unwrap(), "{src:?}");
                assert_eq!(clean, odd == "\t", "{src:?}");
            }
        }
    }
}

// ---- quoted scalars ---------------------------------------------------------------------

/// A quoted scalar (opening quote at 0, no indentation rules) read one
/// character at a time: its end, value, and whether it spans lines.
fn quoted_plain(src: &str, double: bool) -> Result<(usize, String, bool), Code> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut k = 1;
    let mut out = String::new();
    // Bytes of `out` that trailing-white trimming at a fold must keep.
    let mut keep = 0;
    let mut multiline = false;
    let at = |k: usize| chars.get(k).map(|(_, c)| *c);
    // After a break at `k`: skips the white space of the following lines
    // and returns how many empty lines it passed.
    let skip_lines = |k: &mut usize| -> usize {
        let mut empties = 0;
        *k += 1;
        loop {
            let mut j = *k;
            while matches!(at(j), Some(' ' | '\t')) {
                j += 1;
            }
            match at(j) {
                Some(b @ ('\n' | '\r')) => {
                    empties += 1;
                    // `\r\n` is one break.
                    *k = j + if b == '\r' && at(j + 1) == Some('\n') {
                        2
                    } else {
                        1
                    };
                }
                _ => {
                    *k = j;
                    return empties;
                }
            }
        }
    };
    loop {
        let Some(c) = at(k) else {
            return Err(UNTERMINATED_QUOTED_SCALAR);
        };
        match c {
            '\'' if !double && at(k + 1) == Some('\'') => {
                out.push('\'');
                keep = out.len();
                k += 2;
            }
            '\'' if !double => break,
            '"' if double => break,
            '\\' if double => {
                let e = at(k + 1).ok_or(UNTERMINATED_QUOTED_SCALAR)?;
                match e {
                    '\n' | '\r' => {
                        multiline = true;
                        // `\r\n` is one break.
                        if e == '\r' && at(k + 2) == Some('\n') {
                            k += 1;
                        }
                        let mut j = k + 1;
                        let empties = skip_lines(&mut j);
                        out.push_str(&"\n".repeat(empties));
                        keep = out.len();
                        k = j;
                        continue;
                    }
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '/' => out.push('/'),
                    ' ' => out.push(' '),
                    'x' => {
                        out.push('A');
                        k += 2;
                    }
                    'u' => {
                        out.push('\u{e9}');
                        k += 4;
                    }
                    'U' => {
                        out.push('\u{1f600}');
                        k += 8;
                    }
                    _ => return Err(INVALID_ESCAPE),
                }
                keep = out.len();
                k += 2;
            }
            '\n' | '\r' => {
                multiline = true;
                while out.len() > keep && out.ends_with([' ', '\t']) {
                    out.pop();
                }
                if c == '\r' && at(k + 1) == Some('\n') {
                    k += 1;
                }
                let empties = skip_lines(&mut k);
                if at(k).is_none() {
                    return Err(UNTERMINATED_QUOTED_SCALAR);
                }
                if empties == 0 {
                    out.push(' ');
                } else {
                    out.push_str(&"\n".repeat(empties));
                }
                keep = out.len();
            }
            c if c < ' ' && c != '\t' => return Err(INVALID_QUOTED_CHARACTER),
            c => {
                out.push(c);
                if c != ' ' && c != '\t' {
                    keep = out.len();
                }
                k += 1;
            }
        }
    }
    let end = chars.get(k).map_or(src.len(), |(i, _)| *i) + 1;
    Ok((end, out, multiline))
}

fn quoted_fast(src: &str, double: bool) -> Result<(usize, String, bool), Code> {
    quoted::<String>(src, 0, None, double)
        .map(|f| {
            let raw = src.get(1..f.end.saturating_sub(1)).unwrap_or("").to_owned();
            let value = f.value.unwrap_or(raw);
            assert_eq!(f.len, value.len(), "{src:?}");
            (f.end, value, f.multiline)
        })
        .map_err(|(code, _)| code)
}

const SINGLE_PIECES: &[&str] = &[
    "a",
    "bc",
    "word",
    "\u{e9}",
    "\u{1f600}",
    "\u{7f}",
    " ",
    "   ",
    "\t",
    " \t ",
    "''",
    ":",
    "#",
    ",",
    "\\",
    "\"",
    "\n",
    "\n  ",
    "\n\n",
    "\r\n",
    " \n   ",
    "\r",
    "\n \t \n  ",
    "x",
    "y",
];

const DOUBLE_PIECES: &[&str] = &[
    "a",
    "bc",
    "word",
    "\u{e9}",
    "\u{1f600}",
    "\u{7f}",
    " ",
    "   ",
    "\t",
    " \t ",
    "'",
    ":",
    "#",
    ",",
    "\\n",
    "\\t",
    "\\\\",
    "\\\"",
    "\\x41",
    "\\u00e9",
    "\\U0001F600",
    "\\ ",
    "\\/",
    "\n",
    "\n  ",
    "\n\n",
    "\r\n",
    " \n   ",
    "\\\n  ",
    "\\\n\n  ",
    "\\\r\n",
    " \\\n",
    "\n \t \n  ",
    "x",
    "y",
];

#[test]
fn quoted_scalars_match_the_character_reader() {
    let mut rng = Lcg(41);
    for round in 0..4000 {
        for double in [false, true] {
            let pieces = if double { DOUBLE_PIECES } else { SINGLE_PIECES };
            let body = random_text(&mut rng, pieces, 40);
            let q = if double { '"' } else { '\'' };
            let src = format!("{q}{body}{q} tail");
            assert_eq!(
                quoted_fast(&src, double),
                quoted_plain(&src, double),
                "round {round}: {src:?}"
            );
        }
    }
}

#[test]
fn long_quoted_scalars_match_the_character_reader() {
    // Runs long enough for the block scan, with every kind of special piece
    // at every offset around the block boundaries.
    for len in 0..70usize {
        for special in ["''", "\n  ", " \n", "\t", "\u{e9}", "\\n", "\\\n  "] {
            for at in 0..len {
                for double in [false, true] {
                    if !double && special.starts_with('\\') {
                        continue;
                    }
                    let q = if double { '"' } else { '\'' };
                    let mut body = "x".repeat(len);
                    body.insert_str(at, special);
                    let src = format!("{q}{body}{q}");
                    assert_eq!(
                        quoted_fast(&src, double),
                        quoted_plain(&src, double),
                        "{src:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn quoted_scalar_errors_match_the_character_reader() {
    for src in [
        "'abc",
        "'abc\n",
        "'abc\n  def",
        "\"abc",
        "\"a\u{1}b\"",
        "'a\u{1}b'",
        "\"a\\qb\"",
        "'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\u{2}xxxxxxxxxx\"",
    ] {
        for double in [false, true] {
            let fast = quoted_fast(src, double).err();
            let plain = quoted_plain(src, double).err();
            assert_eq!(fast, plain, "{src:?} double {double}");
        }
    }
}

#[test]
fn split_lines_matches_the_character_split() {
    let mut rng = Lcg(53);
    for _ in 0..2000 {
        let text = random_text(&mut rng, MIXED, 40);
        let fast: Vec<&str> = split_lines(&text).collect();
        let mut plain = Vec::new();
        let mut line_start = 0;
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                    plain.push(&text[line_start..i]);
                    i += 2;
                    line_start = i;
                }
                b'\n' | b'\r' => {
                    plain.push(&text[line_start..i]);
                    i += 1;
                    line_start = i;
                }
                _ => i += 1,
            }
        }
        plain.push(&text[line_start..]);
        assert_eq!(fast, plain, "{text:?}");
    }
}

#[test]
fn trim_white_matches_trim_matches() {
    let mut rng = Lcg(67);
    for _ in 0..2000 {
        let text = random_text(&mut rng, &[" ", "\t", "a", "\u{e9}", "\u{feff}", ":"], 12);
        assert_eq!(
            trim_white(&text),
            text.trim_matches([' ', '\t']),
            "{text:?}"
        );
    }
    assert_eq!(trim_white(""), "");
    assert_eq!(trim_white(" \t "), "");
}

#[test]
fn counted_plain_length_is_the_length_of_the_value() {
    use super::scalar::{Count, plain, plain_value};
    let pieces = [
        "a",
        "bc",
        " ",
        "  ",
        "\t",
        "\n",
        "\n  ",
        "\n\n",
        "\r\n",
        " \n ",
        "\n \t \n  ",
        "\r",
        "\u{e9}",
        ":",
        "x y",
        " #c",
        "\n#c",
        "\n---\n",
        "\n  \tz",
        ",",
        "]",
    ];
    let mut rng = Lcg(71);
    for round in 0..6000 {
        let src = format!("w{}", random_text(&mut rng, &pieces, 24));
        for (parent, flow) in [
            (None, false),
            (Some(0), false),
            (Some(1), true),
            (None, true),
        ] {
            let first_end = plain_line_end(&src, 0, flow);
            let f = plain(&src, 0, first_end, parent, flow);
            let value: Count = plain_value(&src[..f.end]);
            assert_eq!(
                f.len, value.0,
                "round {round}: {src:?} parent {parent:?} flow {flow}"
            );
        }
    }
}
