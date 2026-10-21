//! Lexical layer: character classes (YAML 1.2.2 §5) and single-line scanners
//! for indicators, properties (§6.9), and plain scalar lines (§7.3.3).
//!
//! Every function takes the source and a byte offset on a `char` boundary and
//! returns offsets on `char` boundaries. None of them allocate or recurse, and
//! each scans forward only, so the parser stays O(n).

use crate::event::{TagForm, TriviaKind};

/// The byte at `i`, if any.
pub(super) fn byte(src: &str, i: usize) -> Option<u8> {
    src.as_bytes().get(i).copied()
}

/// The `char` starting at byte `i`, if any.
pub(super) fn char_at(src: &str, i: usize) -> Option<char> {
    src.get(i..).and_then(|s| s.chars().next())
}

/// `s-white` (production 33).
pub(super) const fn is_white(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// `b-char` (production 26).
pub(super) const fn is_break(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// White space, a line break, or the end of input at `i`.
pub(super) fn blank_or_end(src: &str, i: usize) -> bool {
    byte(src, i).is_none_or(|b| is_white(b) || is_break(b))
}

/// A line break or the end of input at `i`.
pub(super) fn break_or_end(src: &str, i: usize) -> bool {
    byte(src, i).is_none_or(is_break)
}

/// Length of the line break at `i` (`\r\n` is one break); `0` if none.
pub(super) fn break_len(src: &str, i: usize) -> usize {
    match byte(src, i) {
        Some(b'\r') if byte(src, i.saturating_add(1)) == Some(b'\n') => 2,
        Some(b'\n' | b'\r') => 1,
        _ => 0,
    }
}

/// The offset of the first line break at or after `i`, or `src.len()`.
pub(super) fn line_end(src: &str, i: usize) -> usize {
    src.get(i..)
        .and_then(|rest| rest.find(['\n', '\r']))
        .map_or(src.len(), |k| i.saturating_add(k))
}

/// [`line_end`], eight bytes at a time: for the comments, which are
/// scanned on every parse and are mostly long.
#[inline(never)]
pub(super) fn comment_end(src: &str, i: usize) -> usize {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    let Some(rest) = src.get(i..).map(str::as_bytes) else {
        return src.len();
    };
    // Eight bytes at a time while none is a line break. In a word, the
    // lowest flagged byte is the first line break (a borrow only ever
    // flags bytes above a real match).
    let mut at: usize = 0;
    for chunk in rest.chunks_exact(8) {
        let Ok(word) = <[u8; 8]>::try_from(chunk) else {
            break;
        };
        let w = u64::from_le_bytes(word);
        let (lf, cr) = (w ^ (ONES * 0x0a), w ^ (ONES * 0x0d));
        let found = ((lf.wrapping_sub(ONES) & !lf) | (cr.wrapping_sub(ONES) & !cr)) & HIGH;
        if found != 0 {
            let first = usize::try_from(found.trailing_zeros() / 8).unwrap_or(0);
            return i.saturating_add(at).saturating_add(first);
        }
        at = at.saturating_add(8);
    }
    let run = rest
        .get(at..)
        .and_then(|tail| tail.iter().position(|b| matches!(b, b'\n' | b'\r')))
        .map_or(rest.len(), |k| at.saturating_add(k));
    i.saturating_add(run)
}

/// The offset after the run of spaces and tabs starting at `i`.
pub(super) fn skip_white(src: &str, i: usize) -> usize {
    let run = src
        .as_bytes()
        .get(i..)
        .map_or(0, |rest| rest.iter().take_while(|b| is_white(**b)).count());
    i.saturating_add(run)
}

/// The first byte order mark in `from..to`, if any.
pub(super) fn first_mark(src: &str, from: usize, to: usize) -> Option<usize> {
    let text = src.get(from..to)?;
    text.find('\u{feff}').map(|k| from.saturating_add(k))
}

/// A byte order mark starts at `i`.
pub(super) fn mark_at(src: &str, i: usize) -> bool {
    src.get(i..).is_some_and(|r| r.starts_with('\u{feff}'))
}

/// `c-printable` (production 1).
pub(super) const fn is_printable(c: char) -> bool {
    matches!(c,
        '\t' | '\n' | '\r' | '\x20'..='\x7e' | '\u{85}' | '\u{a0}'..='\u{d7ff}'
        | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
}

/// `nb-json` (production 2): allowed inside quoted scalars.
pub(super) const fn is_nb_json(c: char) -> bool {
    c == '\t' || c >= '\x20'
}

/// `ns-char` (production 34): printable, not white space, not a break, not
/// a byte order mark.
pub(super) const fn is_ns_char(c: char) -> bool {
    is_printable(c) && !matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{feff}')
}

/// `c-flow-indicator` (production 23).
pub(super) const fn is_flow_indicator(c: char) -> bool {
    matches!(c, ',' | '[' | ']' | '{' | '}')
}

/// `c-indicator` (production 22).
const fn is_indicator(c: char) -> bool {
    matches!(
        c,
        '-' | '?'
            | ':'
            | ','
            | '['
            | ']'
            | '{'
            | '}'
            | '#'
            | '&'
            | '*'
            | '!'
            | '|'
            | '>'
            | '\''
            | '"'
            | '%'
            | '@'
            | '`'
    )
}

/// `ns-plain-safe(c)` (productions 127-129) for the char at `i`; block
/// context (`flow == false`) allows flow indicators.
fn plain_safe_at(src: &str, i: usize, flow: bool) -> bool {
    char_at(src, i).is_some_and(|c| is_ns_char(c) && !(flow && is_flow_indicator(c)))
}

/// `ns-plain-first(c)` (production 126) at `i`.
pub(super) fn is_plain_first(src: &str, i: usize, flow: bool) -> bool {
    match char_at(src, i) {
        Some('-' | '?' | ':') => plain_safe_at(src, i.saturating_add(1), flow),
        Some(c) => is_ns_char(c) && !is_indicator(c),
        None => false,
    }
}

/// An indicator `c` at `i` followed by white space, a break, or the end of
/// input (e.g. `- `, `? `, `: ` in block context).
pub(super) fn indicator_at(src: &str, i: usize, c: u8) -> bool {
    byte(src, i) == Some(c) && blank_or_end(src, i.saturating_add(1))
}

/// A document marker (`---` or `...`, productions 203 and 204) at `i`, which
/// must be the start of a line: the three characters followed by white space,
/// a break, or the end of input.
pub(super) fn marker_at(src: &str, i: usize, marker: &str) -> bool {
    src.get(i..)
        .is_some_and(|rest| rest.starts_with(marker) && blank_or_end(src, i.saturating_add(3)))
}

/// Either document marker at `i`.
pub(super) fn any_marker_at(src: &str, i: usize) -> bool {
    marker_at(src, i, "---") || marker_at(src, i, "...")
}

/// End of the plain scalar text on the line starting at `start`
/// (`nb-ns-plain-in-line`, production 132, after `ns-plain-first`), with
/// trailing white space excluded. Stops before `: `, ` #`, a line break, a
/// non-printable character, and (in flow context) flow indicators.
pub(super) fn plain_line_end(src: &str, start: usize, flow: bool) -> usize {
    line_text_end(src, start, flow, false)
}

/// [`plain_line_end`] in block context, except that `: ` does not end the
/// text: the rest of the line up to a comment (` #`), a line break, or a
/// non-printable character. For `lenient_inline_colons` and for the
/// quoting fix of `INLINE_COLON_IN_PLAIN_VALUE`.
pub(super) fn plain_line_end_lenient(src: &str, start: usize) -> usize {
    line_text_end(src, start, false, true)
}

fn line_text_end(src: &str, start: usize, flow: bool, colons: bool) -> usize {
    let mut i = start;
    let mut end = start;
    while let Some(c) = char_at(src, i) {
        match c {
            ' ' | '\t' => {
                i = i.saturating_add(1);
                continue;
            }
            // ns-plain-char (production 130): `:` only before a safe char.
            ':' if !colons && !plain_safe_at(src, i.saturating_add(1), flow) => break,
            // `#` only directly after a non-space character.
            '#' if i > start
                && src
                    .as_bytes()
                    .get(i.saturating_sub(1))
                    .is_some_and(|b| is_white(*b)) =>
            {
                break;
            }
            c if !is_ns_char(c) || (flow && is_flow_indicator(c)) => break,
            _ => {}
        }
        i = i.saturating_add(c.len_utf8());
        end = i;
    }
    end
}

/// End of an anchor or alias name starting at `i` (`ns-anchor-name`,
/// production 103: `ns-char` minus flow indicators).
pub(super) fn anchor_end(src: &str, i: usize) -> usize {
    let mut j = i;
    while let Some(c) = char_at(src, j) {
        if !is_ns_char(c) || is_flow_indicator(c) {
            break;
        }
        j = j.saturating_add(c.len_utf8());
    }
    j
}

/// `ns-word-char` (production 38).
const fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// Length of one `ns-uri-char` (production 39) at `i` (3 for `%XX`), or 0.
/// `tag_char` excludes `!` and flow indicators (`ns-tag-char`, production 40).
fn uri_char_len(src: &str, i: usize, tag_char: bool) -> usize {
    let Some(b) = byte(src, i) else { return 0 };
    let hex = |k: usize| byte(src, i.saturating_add(k)).is_some_and(|h| h.is_ascii_hexdigit());
    match b {
        b'%' if hex(1) && hex(2) => 3,
        b'!' | b',' | b'[' | b']' if tag_char => 0,
        _ if is_word_char(b) => 1,
        b'#' | b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b'_' | b'.'
        | b'~' | b'*' | b'\'' | b'(' | b')' | b'!' | b',' | b'[' | b']' => 1,
        _ => 0,
    }
}

/// The offset after a run of URI (or tag) characters starting at `i`.
pub(super) fn uri_end(src: &str, i: usize, tag_char: bool) -> usize {
    let mut j = i;
    loop {
        let n = uri_char_len(src, j, tag_char);
        if n == 0 {
            return j;
        }
        j = j.saturating_add(n);
    }
}

/// A scanned tag property (`c-ns-tag-property`, production 97): byte ranges
/// of the handle and suffix as written.
#[derive(Clone, Copy, Debug)]
pub(super) struct TagScan {
    pub(super) form: TagForm,
    pub(super) handle: (usize, usize),
    pub(super) suffix: (usize, usize),
    pub(super) end: usize,
}

/// Where the text of a verbatim tag, `from..to` (the characters between
/// `!<` and `>`), stops being a tag, if it does: §6.9.1 wants a local tag
/// (`!` and at least one more character) or a global one, a URI that begins
/// with a scheme (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":"`).
fn verbatim_fault(src: &str, from: usize, to: usize) -> Option<usize> {
    if byte(src, from) == Some(b'!') {
        return (to == from.saturating_add(1)).then_some(to);
    }
    if !byte(src, from).is_some_and(|b| b.is_ascii_alphabetic()) {
        return Some(from);
    }
    let mut j = from.saturating_add(1);
    while j < to
        && byte(src, j)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        j = j.saturating_add(1);
    }
    (byte(src, j) != Some(b':') || j >= to).then_some(j)
}

/// Where a `%` that does not start an escape (`%` and two hexadecimal
/// digits) stops being a URI character: the first character that cannot
/// continue it. Any other position is returned as it is.
fn after_partial_escape(src: &str, at: usize) -> usize {
    if byte(src, at) != Some(b'%') {
        return at;
    }
    let hex = |k: usize| byte(src, at.saturating_add(k)).is_some_and(|h| h.is_ascii_hexdigit());
    at.saturating_add(if hex(1) { 2 } else { 1 })
}

/// The first character that cannot belong to the tag starting with `!` at
/// `start`, when the tag is malformed or not followed by white space or a
/// flow indicator (`scanned` is what [`scan_tag`] made of it).
pub(super) fn tag_blame(src: &str, start: usize, scanned: Option<TagScan>) -> usize {
    if let Some(t) = scanned {
        return after_partial_escape(src, t.end);
    }
    let after = start.saturating_add(1);
    match byte(src, after) {
        Some(b'<') => {
            let from = after.saturating_add(1);
            let to = uri_end(src, from, false);
            if to > from && byte(src, to) == Some(b'>') {
                verbatim_fault(src, from, to).unwrap_or(to)
            } else {
                after_partial_escape(src, to)
            }
        }
        Some(b'!') => {
            let from = after.saturating_add(1);
            after_partial_escape(src, uri_end(src, from, true))
        }
        _ => {
            let words = src
                .as_bytes()
                .get(after..)
                .map_or(0, |r| r.iter().take_while(|b| is_word_char(**b)).count());
            let word_end = after.saturating_add(words);
            let from = if words > 0 && byte(src, word_end) == Some(b'!') {
                word_end.saturating_add(1)
            } else {
                after
            };
            after_partial_escape(src, uri_end(src, from, true))
        }
    }
}

/// Scans a tag starting with `!` at `start`. `None` when malformed.
pub(super) fn scan_tag(src: &str, start: usize) -> Option<TagScan> {
    let after = start.saturating_add(1);
    match byte(src, after) {
        // c-verbatim-tag (production 98).
        Some(b'<') => {
            let from = after.saturating_add(1);
            let to = uri_end(src, from, false);
            (to > from && byte(src, to) == Some(b'>') && verbatim_fault(src, from, to).is_none())
                .then_some(TagScan {
                    form: TagForm::Verbatim,
                    handle: (start, start),
                    suffix: (from, to),
                    end: to.saturating_add(1),
                })
        }
        // c-ns-shorthand-tag with the secondary handle `!!` (production 99).
        Some(b'!') => {
            let from = after.saturating_add(1);
            let to = uri_end(src, from, true);
            (to > from).then_some(TagScan {
                form: TagForm::Shorthand,
                handle: (start, from),
                suffix: (from, to),
                end: to,
            })
        }
        _ => {
            let words = src
                .as_bytes()
                .get(after..)
                .map_or(0, |r| r.iter().take_while(|b| is_word_char(**b)).count());
            let word_end = after.saturating_add(words);
            // Named handle `!word!` (production 92).
            let (handle_end, from) = if words > 0 && byte(src, word_end) == Some(b'!') {
                let h = word_end.saturating_add(1);
                (h, h)
            } else {
                (after, after)
            };
            let to = uri_end(src, from, true);
            if to == from {
                // c-non-specific-tag (production 100): a lone `!`.
                return (handle_end == after).then_some(TagScan {
                    form: TagForm::NonSpecific,
                    handle: (start, after),
                    suffix: (after, after),
                    end: after,
                });
            }
            Some(TagScan {
                form: TagForm::Shorthand,
                handle: (start, handle_end),
                suffix: (from, to),
                end: to,
            })
        }
    }
}

/// The tag URI `prefix` + `suffix` with its `%XX` escapes of ASCII
/// characters decoded, built in one allocation.
///
/// Escapes of non-ASCII bytes and `%25` (the escaped `%`) stay as written,
/// so that two spellings that differ only in such an escape stay two tags:
/// decoding `%25` would make `%25C3%25A9` read as the same text as the
/// kept escapes `%C3%A9`. Tag characters are preserved and compared as
/// presented (§5.6), and only the other escaped characters of the
/// restricted ASCII range are expanded (§6.9.1). The result is always
/// valid UTF-8, so a prefix and suffix that split a multi-byte escape
/// (`%C3` + `%A9`) are simply kept as that text.
pub(super) fn percent_decode_concat(prefix: &str, suffix: &str) -> String {
    let mut out = Vec::with_capacity(prefix.len().saturating_add(suffix.len()));
    out.extend(decoded(prefix));
    out.extend(decoded(suffix));
    // Only ASCII bytes were decoded; the rest is the original text.
    String::from_utf8_lossy(&out).into_owned()
}

/// The bytes of `s` with its `%XX` escapes of ASCII characters decoded, one at
/// a time (no allocation). An escape of a byte above 0x7F, and `%25`, are
/// passed through as written (see [`percent_decode_concat`]).
pub(super) fn decoded(s: &str) -> impl Iterator<Item = u8> + '_ {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    core::iter::from_fn(move || {
        let b = *bytes.get(i)?;
        let hex = bytes
            .get(i.saturating_add(1)..i.saturating_add(3))
            .and_then(|h| core::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(v)) if v.is_ascii() && v != b'%' => {
                i = i.saturating_add(3);
                Some(v)
            }
            _ => {
                i = i.saturating_add(1);
                Some(b)
            }
        }
    })
}

/// A URI part read on: its decoded length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct UriRead {
    pub(super) len: usize,
}

impl UriRead {
    /// Nothing read yet.
    pub(super) const EMPTY: Self = Self { len: 0 };

    /// This length continued over the decoded bytes of `s`: O(`s`).
    pub(super) fn then(self, s: &str) -> Self {
        Self {
            len: self.len.saturating_add(decoded(s).count()),
        }
    }
}

/// A `%TAG` prefix, read once at its directive: where it
/// starts, and its decoded length and UTF-8 state, so that a tag using it
/// is checked in O(its suffix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TagPrefix {
    pub(super) start: usize,
    pub(super) read: UriRead,
}

impl TagPrefix {
    /// The prefix starting at `start` (`None`: empty, which a declared
    /// prefix never is). O(prefix), once per directive.
    pub(super) fn read(src: &str, start: usize) -> Option<Self> {
        let text = tag_prefix_at(src, start)?;
        Some(Self {
            start,
            read: UriRead::EMPTY.then(text),
        })
    }
}

/// The `%TAG` prefix (`ns-tag-prefix`, production 93) starting at `p`, as
/// written; `None` when empty (a declared prefix never is).
pub(super) fn tag_prefix_at(src: &str, p: usize) -> Option<&str> {
    src.get(p..uri_end(src, p, false)).filter(|t| !t.is_empty())
}

/// `#` at `i` starts a comment: preceded by white space, a break, a byte
/// order mark, or the start of input (production 75).
pub(super) fn comment_at(src: &str, i: usize) -> bool {
    byte(src, i) == Some(b'#')
        && src.get(..i).is_some_and(|before| {
            matches!(
                before.chars().next_back(),
                None | Some(' ' | '\t' | '\n' | '\r' | '\u{feff}')
            )
        })
}

/// Lexes the first trivia event of `src[i..end]`, a run of white space,
/// line breaks, comments, and byte order marks that the trivia scanners
/// (`s-separate`, `s-l-comments`, productions 79-80) accepted: its kind and
/// end. The parser queues pending trivia as such a run and lexes it only as
/// the consumer pulls events, so the events are exactly the ones the
/// scanners saw: white space is one maximal run, a comment runs to the end
/// of its line, a break is one break.
///
/// `None` if `src[i]` cannot start trivia (or `i..end` is empty): only
/// trivia is ever queued as a run, so that is a broken internal invariant,
/// which the caller reports as `Y9000`.
pub(super) fn lex_trivia(src: &str, i: usize, end: usize) -> Option<(TriviaKind, usize)> {
    if i >= end {
        return None;
    }
    let (kind, stop) = match byte(src, i) {
        Some(b' ' | b'\t') => (TriviaKind::Whitespace, skip_white(src, i)),
        Some(b'\n' | b'\r') => (TriviaKind::LineBreak, i.saturating_add(break_len(src, i))),
        Some(b'#') => (TriviaKind::Comment, line_end(src, i)),
        _ if src.get(i..).is_some_and(|r| r.starts_with('\u{feff}')) => (
            TriviaKind::ByteOrderMark,
            i.saturating_add('\u{feff}'.len_utf8()),
        ),
        _ => return None,
    };
    Some((kind, stop.min(end).max(i.saturating_add(1)).min(end)))
}

/// The implicit-key limit (productions 154-155): at most 1024 characters,
/// counting properties, content, and white space before the `:`.
pub(super) const IMPLICIT_KEY_MAX_CHARS: usize = 1024;

/// `src[start..end]` is longer than [`IMPLICIT_KEY_MAX_CHARS`] characters.
/// O(1024): short byte ranges need no counting.
pub(super) fn key_too_long(src: &str, start: usize, end: usize) -> bool {
    src.get(start..end).is_some_and(|key| {
        key.len() > IMPLICIT_KEY_MAX_CHARS && key.chars().nth(IMPLICIT_KEY_MAX_CHARS).is_some()
    })
}

/// `!`, `!!`, or `!word!` (`c-tag-handle`, production 89).
pub(super) fn valid_handle(h: &str) -> bool {
    if h == "!" || h == "!!" {
        return true;
    }
    h.strip_prefix('!')
        .and_then(|r| r.strip_suffix('!'))
        .is_some_and(|w| !w.is_empty() && w.bytes().all(is_word_char))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn breaks_and_lines() {
        assert_eq!(break_len("a\r\nb", 1), 2);
        assert_eq!(break_len("a\rb", 1), 1);
        assert_eq!(break_len("a\nb", 0), 0);
        assert_eq!(line_end("ab\ncd", 0), 2);
        assert_eq!(line_end("ab", 1), 2);
        assert_eq!(skip_white("a \t b", 1), 4);
    }

    #[test]
    fn plain_lines() {
        let s = "a b: c";
        assert_eq!(plain_line_end(s, 0, false), 3);
        assert_eq!(plain_line_end("a:b #c", 0, false), 3);
        assert_eq!(plain_line_end("a#b  ", 0, false), 3);
        assert_eq!(plain_line_end("a,b]", 0, false), 4);
        assert_eq!(plain_line_end("a,b]", 0, true), 1);
        assert!(is_plain_first("-a", 0, false));
        assert!(!is_plain_first("- a", 0, false));
        assert!(!is_plain_first("[a", 0, false));
        assert!(!is_plain_first("@a", 0, false));
        assert_eq!(plain_line_end_lenient("a: b: c  # d", 0), 7);
        assert_eq!(plain_line_end_lenient("a:\tb:", 0), 5);
        assert_eq!(plain_line_end_lenient("a: [b, c] :\nx", 0), 11);
        assert_eq!(plain_line_end_lenient("a: b\u{1}c", 0), 4);
    }

    #[test]
    fn markers_and_indicators() {
        assert!(marker_at("--- a", 0, "---"));
        assert!(marker_at("...", 0, "..."));
        assert!(!marker_at("---a", 0, "---"));
        assert!(indicator_at("- a", 0, b'-'));
        assert!(indicator_at("-", 0, b'-'));
        assert!(!indicator_at("-a", 0, b'-'));
    }

    #[test]
    fn tags() {
        let t = scan_tag("!!str x", 0).unwrap();
        assert_eq!(
            (t.form, t.handle, t.suffix, t.end),
            (TagForm::Shorthand, (0, 2), (2, 5), 5)
        );
        let t = scan_tag("!e!foo", 0).unwrap();
        assert_eq!((t.handle, t.suffix), ((0, 3), (3, 6)));
        let t = scan_tag("!foo", 0).unwrap();
        assert_eq!((t.handle, t.suffix), ((0, 1), (1, 4)));
        let t = scan_tag("! a", 0).unwrap();
        assert_eq!((t.form, t.end), (TagForm::NonSpecific, 1));
        let t = scan_tag("!<tag:a,b> x", 0).unwrap();
        assert_eq!((t.form, t.suffix, t.end), (TagForm::Verbatim, (2, 9), 10));
        assert!(scan_tag("!<> x", 0).is_none());
        assert!(scan_tag("!! x", 0).is_none());
        assert!(scan_tag("!e! x", 0).is_none());
        assert_eq!(percent_decode_concat("p:", "a%21b"), "p:a!b");
        assert_eq!(percent_decode_concat("", "%4"), "%4");
        // Escapes of non-ASCII bytes stay as written.
        assert_eq!(percent_decode_concat("", "%ff"), "%ff");
        // The prefix is decoded too.
        assert_eq!(percent_decode_concat("p:%21", "a"), "p:!a");
        assert_eq!(percent_decode_concat("p:%C3", "%A9"), "p:%C3%A9");
        // `%25` stays too: it is not the same text as the escapes it spells.
        assert_eq!(percent_decode_concat("", "a%25C3%25A9"), "a%25C3%25A9");
        assert_ne!(
            percent_decode_concat("", "a%25C3%25A9"),
            percent_decode_concat("", "a%C3%A9")
        );
        assert_eq!(tag_prefix_at("%TAG !e! a:%21 ", 9), Some("a:%21"));
        assert_eq!(tag_prefix_at("x ", 1), None);
        // A prefix read once, continued by each suffix.
        let p = TagPrefix::read("%TAG !e! a:%C3 ", 9).unwrap();
        assert_eq!(p.read.len, 5);
        assert_eq!(p.read.then("%A9x").len, 9);
        assert!(TagPrefix::read("x", 1).is_none());
        assert!(valid_handle("!e!") && !valid_handle("!e") && !valid_handle("!!!"));
    }

    #[test]
    fn anchors() {
        assert_eq!(anchor_end("&a: b", 1), 3);
        assert_eq!(anchor_end("&a,b", 1), 2);
        assert_eq!(anchor_end("&a\u{a0}b c", 1), 5);
        assert_eq!(anchor_end("&a\u{feff}", 1), 2);
    }

    /// The word-at-a-time scans agree with the plain ones at every length,
    /// offset and break position.
    #[test]
    fn word_scans_agree_with_the_plain_ones() {
        for len in 0..40 {
            for brk in 0..=len {
                for sep in ["\n", "\r", "\r\n"] {
                    let mut s = "x".repeat(brk);
                    s.push_str(sep);
                    s.push_str(&"y".repeat(len - brk));
                    for i in 0..=s.len() {
                        assert_eq!(comment_end(&s, i), line_end(&s, i), "{s:?} {i}");
                    }
                }
            }
            // No break at all; a multi-byte character straddling a word.
            let s = format!("{}\u{e9}\u{1F600}", "z".repeat(len));
            for i in (0..=s.len()).filter(|i| s.is_char_boundary(*i)) {
                assert_eq!(comment_end(&s, i), line_end(&s, i), "{s:?} {i}");
            }
            assert_eq!(comment_end(&s, s.len() + 1), line_end(&s, s.len() + 1));
        }
    }
}
