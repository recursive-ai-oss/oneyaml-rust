//! Flow scalars as they appear in block context: plain (§7.3.3), single-quoted
//! (§7.3.2), and double-quoted (§7.3.1), including line folding (§6.5) and
//! escapes (§5.7).
//!
//! Each scanner walks forward once; lookahead past empty lines is re-scanned
//! at most once more by the trivia scanner, so the total work stays O(n)
//! overall. No recursion.
//!
//! Values are produced through a [`Sink`]: the parser only **counts** a
//! value's bytes (for `Limits::max_scalar_len`), and [`Sourced::value`]
//! decodes it later by running the *same* function with a `String` sink
//! (one implementation, so the length checked is the length
//! decoded).
//!
//! [`Sourced::value`]: crate::event::Sourced::value

use oneyaml_core::code::{
    INVALID_ESCAPE, INVALID_QUOTED_CHARACTER, QUOTED_SCALAR_INDENTATION, UNTERMINATED_QUOTED_SCALAR,
};
use oneyaml_core::{Code, Replacement, Span};

use super::scan::{
    any_marker_at, break_len, byte, char_at, is_nb_json, line_end, plain_line_end, skip_white,
};

/// Where a scalar value is built: a `String`, or a byte count.
pub(super) trait Sink {
    /// A value starting as `raw`.
    fn from_raw(raw: &str) -> Self;
    /// Appends `s`.
    fn push_str(&mut self, s: &str);
    /// Appends `c`.
    fn push_char(&mut self, c: char);
    /// Appends `n` copies of `c`.
    fn push_repeat(&mut self, c: char, n: usize);
    /// Length in bytes.
    fn len(&self) -> usize;
    /// Shortens to `len` bytes (a `char` boundary).
    fn truncate(&mut self, len: usize);
}

impl Sink for String {
    fn from_raw(raw: &str) -> Self {
        raw.to_owned()
    }
    fn push_str(&mut self, s: &str) {
        Self::push_str(self, s);
    }
    fn push_char(&mut self, c: char) {
        self.push(c);
    }
    fn push_repeat(&mut self, c: char, n: usize) {
        self.extend(core::iter::repeat_n(c, n));
    }
    fn len(&self) -> usize {
        Self::len(self)
    }
    fn truncate(&mut self, len: usize) {
        if self.is_char_boundary(len) {
            Self::truncate(self, len);
        }
    }
}

/// A sink that only counts bytes: the parser's view of a value (no
/// allocation to check `max_scalar_len`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Count(pub(super) usize);

impl Sink for Count {
    fn from_raw(raw: &str) -> Self {
        Self(raw.len())
    }
    fn push_str(&mut self, s: &str) {
        self.0 = self.0.saturating_add(s.len());
    }
    fn push_char(&mut self, c: char) {
        self.0 = self.0.saturating_add(c.len_utf8());
    }
    fn push_repeat(&mut self, c: char, n: usize) {
        self.0 = self.0.saturating_add(c.len_utf8().saturating_mul(n));
    }
    fn len(&self) -> usize {
        self.0
    }
    fn truncate(&mut self, len: usize) {
        self.0 = self.0.min(len);
    }
}

/// Splits `text` into lines at `\n`, `\r\n`, or `\r` (a final break yields
/// a final empty line).
pub(super) fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(text);
    core::iter::from_fn(move || {
        let t = rest?;
        let Some(at) = t.find(['\n', '\r']) else {
            rest = None;
            return Some(t);
        };
        let width = break_len(t, at);
        rest = t.get(at.saturating_add(width)..);
        t.get(..at)
    })
}

/// A scanned flow scalar.
pub(super) struct Flow<S> {
    /// One past the last byte (after the closing quote for quoted scalars).
    pub(super) end: usize,
    /// The value when it differs from the raw text between the quotes;
    /// `None` when it *is* that text (it can be borrowed), and always
    /// for [`plain`].
    pub(super) value: Option<S>,
    /// The value's length in bytes.
    pub(super) len: usize,
    /// Whether the scalar's bytes span more than one line.
    pub(super) multiline: bool,
}

/// The line after the break at `i`, past any empty lines (`l-empty`,
/// production 70): its start, its count of leading spaces, the offset of its
/// first non-white character, and how many empty lines were skipped.
///
/// An empty line is `s-indent(<n)` or `s-flow-line-prefix(n)` (production
/// 69: `s-indent(n)` then optional white space) before its break, so a line
/// holding a tab is empty only when its leading *spaces* exceed `parent`.
/// Such a line with too few spaces stops the scan with `bad_empty` set.
struct NextLine {
    start: usize,
    spaces: usize,
    content: usize,
    empties: usize,
    bad_empty: bool,
}

fn next_line(src: &str, i: usize, parent: Option<usize>) -> NextLine {
    let mut j = i.saturating_add(break_len(src, i));
    let mut empties = 0usize;
    loop {
        let spaces = src
            .as_bytes()
            .get(j..)
            .map_or(0, |r| r.iter().take_while(|b| **b == b' ').count());
        let content = skip_white(src, j);
        let blen = break_len(src, content);
        let tabbed = content > j.saturating_add(spaces);
        let bad_empty = blen > 0 && tabbed && parent.is_some_and(|n| spaces <= n);
        if blen == 0 || bad_empty {
            return NextLine {
                start: j,
                spaces,
                content,
                empties,
                bad_empty,
            };
        }
        empties = empties.saturating_add(1);
        j = content.saturating_add(blen);
    }
}

/// Appends the folded form of a line break followed by `empties` empty lines
/// (`b-l-folded`, production 73): a space, or one `\n` per empty line.
fn push_fold<S: Sink>(out: &mut S, empties: usize) {
    if empties == 0 {
        out.push_char(' ');
    } else {
        out.push_repeat('\n', empties);
    }
}

/// Continues a plain scalar whose first line is `start..first_end` over
/// following lines (`ns-plain-multi-line`, production 135) that are indented
/// more than `parent` (`None` = the document level, indentation -1). In flow
/// context (`flow`) the text also stops at flow indicators. The value is
/// measured with [`plain_value`].
pub(super) fn plain(
    src: &str,
    start: usize,
    first_end: usize,
    parent: Option<usize>,
    flow: bool,
) -> Flow<Count> {
    let mut end = first_end;
    let mut multiline = false;
    loop {
        let after = skip_white(src, end);
        if break_len(src, after) == 0 {
            break;
        }
        let line = next_line(src, after, parent);
        let stop = line.bad_empty
            || line.content >= src.len()
            || any_marker_at(src, line.start) && line.start == line.content
            || parent.is_some_and(|n| line.spaces <= n)
            || byte(src, line.content) == Some(b'#');
        if stop {
            break;
        }
        let line_end = plain_line_end(src, line.content, flow);
        if line_end <= line.content {
            break;
        }
        multiline = true;
        end = line_end;
    }
    let text = src.get(start..end).unwrap_or("");
    let len = if multiline {
        plain_value::<Count>(text).0
    } else {
        text.len()
    };
    Flow {
        end,
        value: None,
        len,
        multiline,
    }
}

/// The value of the plain scalar whose bytes are `text` (§7.3.3, §6.5): its
/// lines, stripped of surrounding white space, joined by a space, or by one
/// `\n` per empty line between them (`b-l-folded`, production 73). A plain
/// scalar's text starts and ends with content and holds no comment, so
/// this is exactly the value of what [`plain`] scanned.
pub(super) fn plain_value<S: Sink>(text: &str) -> S {
    let mut lines = split_lines(text);
    let first = lines.next().unwrap_or("").trim_matches([' ', '\t']);
    let mut out = S::from_raw(first);
    let mut empties = 0usize;
    for line in lines {
        let line = line.trim_matches([' ', '\t']);
        if line.is_empty() {
            empties = empties.saturating_add(1);
        } else {
            push_fold(&mut out, empties);
            out.push_str(line);
            empties = 0;
        }
    }
    out
}

/// The value of the `n` hexadecimal digits at `from`, if they are all there.
#[inline]
fn hex_value(src: &str, from: usize, n: usize) -> Option<u32> {
    let digits = src.get(from..from.saturating_add(n))?;
    if digits.len() != n || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// The character a `\uD83D\uDE00`-style pair of escapes encodes: `high` is
/// the first escape's value (its four digits at `from`), and the second must
/// follow directly. `None` for a lone surrogate or any other bad value.
#[cold]
#[inline(never)]
fn surrogate_pair(src: &str, from: usize, high: u32) -> Option<(char, usize)> {
    if !(0xD800..0xDC00).contains(&high) {
        return None;
    }
    // A high surrogate escape directly followed by a low surrogate escape is
    // the one character they encode, as JSON writes a character outside the
    // Basic Multilingual Plane (RFC 8259 §7).
    let next = from.saturating_add(4);
    if src.get(next..next.saturating_add(2)) != Some("\\u") {
        return None;
    }
    let low = hex_value(src, next.saturating_add(2), 4)?;
    if !(0xDC00..0xE000).contains(&low) {
        return None;
    }
    let c = high
        .saturating_sub(0xD800)
        .checked_shl(10)?
        .saturating_add(low.saturating_sub(0xDC00))
        .saturating_add(0x10000);
    Some((char::from_u32(c)?, 11))
}

/// Decodes the escape whose character follows the `\` at `e - 1`
/// (`c-ns-esc-char`, production 62): the char and the escape's length after
/// the backslash.
fn escape(src: &str, e: usize) -> Option<(char, usize)> {
    let hex = |n: usize| -> Option<(char, usize)> {
        let from = e.saturating_add(1);
        let v = hex_value(src, from, n)?;
        // Nearly every escape is a scalar value; only a surrogate is not.
        if let Some(c) = char::from_u32(v) {
            return Some((c, n.saturating_add(1)));
        }
        if n == 4 {
            return surrogate_pair(src, from, v);
        }
        None
    };
    let simple = match char_at(src, e)? {
        '0' => '\0',
        'a' => '\x07',
        'b' => '\x08',
        't' | '\t' => '\t',
        'n' => '\n',
        'v' => '\x0b',
        'f' => '\x0c',
        'r' => '\r',
        'e' => '\x1b',
        ' ' => ' ',
        '"' => '"',
        '/' => '/',
        '\\' => '\\',
        'N' => '\u{85}',
        '_' => '\u{a0}',
        'L' => '\u{2028}',
        'P' => '\u{2029}',
        'x' => return hex(2),
        'u' => return hex(4),
        'U' => return hex(8),
        _ => return None,
    };
    Some((simple, 1))
}

/// Where the escape whose character is at `e` (`escape` refused it) stops
/// being one: the unknown letter, the first character that is no hexadecimal
/// digit, or the character after digits that name no character.
fn escape_fault(src: &str, e: usize) -> usize {
    let digits = match char_at(src, e) {
        Some('x') => 2,
        Some('u') => 4,
        Some('U') => 8,
        _ => return e,
    };
    let from = e.saturating_add(1);
    (0..digits)
        .map(|k| from.saturating_add(k))
        .find(|k| !byte(src, *k).is_some_and(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| from.saturating_add(digits))
}

/// The first character that cannot belong to the quoted scalar that failed
/// with `code` (an invalid escape, or no closing quote), for a scalar
/// whose opening quote is the first quote at or after `start`. `None` for
/// any other code.
pub(super) fn quoted_fault(src: &str, start: usize, code: Code) -> Option<usize> {
    if code != INVALID_ESCAPE && code != UNTERMINATED_QUOTED_SCALAR {
        return None;
    }
    let line = src.get(start..line_end(src, start))?;
    let open = start.saturating_add(line.find(['"', '\''])?);
    if code == INVALID_ESCAPE {
        let mut i = open.saturating_add(1);
        while let Some(c) = char_at(src, i) {
            if c == '"' {
                return None;
            }
            if c != '\\' {
                i = i.saturating_add(c.len_utf8());
                continue;
            }
            let e = i.saturating_add(1);
            let br = break_len(src, e);
            if br > 0 {
                i = e.saturating_add(br);
            } else if let Some((_, len)) = escape(src, e) {
                i = e.saturating_add(len);
            } else {
                return Some(escape_fault(src, e));
            }
        }
        return None;
    }
    // No closing quote: the scalar runs to the end of the input, or to a
    // document marker at the start of a line, whichever comes first.
    let mut i = line_end(src, open);
    loop {
        let br = break_len(src, i);
        if br == 0 {
            return Some(src.len());
        }
        let next = i.saturating_add(br);
        if any_marker_at(src, next) {
            return Some(next);
        }
        i = line_end(src, next);
    }
}

/// Where a quoted scalar fails with `QUOTED_SCALAR_INDENTATION`: the first
/// character of the first continuation line that is not indented more than
/// `parent`, for the scalar whose opening quote is at `open`. That character
/// (often the closing quote itself) is the first that cannot belong.
/// `None` when the scalar does not fail that way.
pub(super) fn indentation_fault(
    src: &str,
    open: usize,
    parent: Option<usize>,
    double: bool,
) -> Option<usize> {
    let mut i = open.saturating_add(1);
    while let Some(c) = char_at(src, i) {
        match c {
            '\'' if !double && byte(src, i.saturating_add(1)) == Some(b'\'') => {
                i = i.saturating_add(2);
            }
            '\'' if !double => return None,
            '"' if double => return None,
            '\\' if double => {
                let e = i.saturating_add(1);
                if break_len(src, e) > 0 {
                    let line = next_line(src, e, parent);
                    match check_line(src, &line, parent, double) {
                        Ok(()) => i = line.content,
                        Err((QUOTED_SCALAR_INDENTATION, _)) => return Some(line.content),
                        Err(_) => return None,
                    }
                } else {
                    i = e.saturating_add(escape(src, e).map_or(1, |(_, len)| len));
                }
            }
            '\n' | '\r' => {
                let line = next_line(src, i, parent);
                match check_line(src, &line, parent, double) {
                    Ok(()) => i = line.content,
                    Err((QUOTED_SCALAR_INDENTATION, _)) => return Some(line.content),
                    Err(_) => return None,
                }
            }
            c => i = i.saturating_add(c.len_utf8()),
        }
    }
    None
}

/// What [`escape_scan`] found on the first line of a double-quoted scalar
/// with an invalid escape (diagnostic polish).
#[derive(Debug, Default)]
pub(super) struct EscapeScan {
    /// The first invalid escape (`\` and the character after it).
    pub(super) first: Option<Span>,
    /// Edits making every invalid escape on the line literal: `\'` loses
    /// its backslash (a `'` needs none), any other gets a second `\`.
    pub(super) edits: Vec<Replacement>,
    /// A valid escape was seen (so single quotes would change the value).
    pub(super) valid: bool,
    /// A valid escape other than `\\` and `\"` was seen: the author may
    /// have meant its backslash literally too (`"C:\data\new"`), so a fix
    /// that keeps it cannot be sure of the value.
    pub(super) ambiguous: bool,
    /// A `'` was seen (single quotes would need it doubled).
    pub(super) apostrophe: bool,
    /// End of the closing quote, when it is on the same line.
    pub(super) end: Option<usize>,
}

/// Scans the double-quoted scalar opened at `start` to its closing quote
/// or the end of its first line, whichever comes first (O(line), so an
/// unclosed scalar never costs the rest of the input), collecting its
/// invalid escapes.
pub(super) fn escape_scan(src: &str, start: usize) -> EscapeScan {
    let mut scan = EscapeScan::default();
    let mut i = start.saturating_add(1);
    while let Some(c) = char_at(src, i) {
        match c {
            '"' => {
                scan.end = Some(i.saturating_add(1));
                break;
            }
            '\n' | '\r' => break,
            '\\' => {
                let e = i.saturating_add(1);
                if let Some((_, len)) = escape(src, e) {
                    scan.valid = true;
                    scan.ambiguous |= !matches!(char_at(src, e), Some('\\' | '"'));
                    i = e.saturating_add(len);
                    continue;
                }
                let Some(next) = char_at(src, e).filter(|n| !matches!(n, '\n' | '\r')) else {
                    // An escaped line break is valid; the end of input is
                    // reported as an unclosed scalar.
                    break;
                };
                let after = e.saturating_add(next.len_utf8());
                scan.first.get_or_insert(Span::new(i, after));
                scan.apostrophe |= next == '\'';
                scan.edits.push(if next == '\'' {
                    Replacement::new(Span::new(i, e), "")
                } else {
                    Replacement::new(Span::empty(i), "\\")
                });
                i = after;
            }
            c => {
                scan.apostrophe |= c == '\'';
                i = i.saturating_add(c.len_utf8());
            }
        }
    }
    scan
}

/// A quoted-scalar error: the code and a message.
pub(super) type QuotedError = (Code, &'static str);

/// Checks a continuation line of a quoted scalar (`s-flow-line-prefix`,
/// production 69): not the end of input, not a document marker, and
/// indented more than `parent`, with no badly indented empty line before it.
fn check_line(
    src: &str,
    line: &NextLine,
    parent: Option<usize>,
    double: bool,
) -> Result<(), QuotedError> {
    if line.bad_empty {
        return Err((
            QUOTED_SCALAR_INDENTATION,
            "an empty line with a tab must be indented like the scalar's content",
        ));
    }
    if line.content >= src.len() {
        return Err((UNTERMINATED_QUOTED_SCALAR, "quoted scalar is not closed"));
    }
    if line.start == line.content && any_marker_at(src, line.start) {
        return Err((
            UNTERMINATED_QUOTED_SCALAR,
            "document marker inside a quoted scalar",
        ));
    }
    if parent.is_some_and(|n| line.spaces <= n) {
        // Often a forgotten closing quote, which only the labels would
        // show, so the message says the scalar is still open.
        let message = if double {
            "a double-quoted scalar is still open at a line that is not indented more than its parent; a closing `\"` may be missing"
        } else {
            "a single-quoted scalar is still open at a line that is not indented more than its parent; a closing `'` may be missing"
        };
        return Err((QUOTED_SCALAR_INDENTATION, message));
    }
    Ok(())
}

/// Scans a single- or double-quoted scalar whose opening quote is at `start`.
///
/// The value is the raw text between the quotes (borrowable) unless
/// an escape, a `''`, or a line fold changes it; only then is a sink
/// started, from the text scanned so far.
pub(super) fn quoted<S: Sink>(
    src: &str,
    start: usize,
    parent: Option<usize>,
    double: bool,
) -> Result<Flow<S>, QuotedError> {
    let body = start.saturating_add(1);
    // `None` while the value is still exactly `src[body..i]`.
    let mut out: Option<S> = None;
    // Start (in `out`) of trailing unescaped white space, stripped at a fold.
    let mut ws_mark: Option<usize> = None;
    let mut multiline = false;
    let mut i = body;
    // Begins the owned copy with the raw text before `upto`.
    let own = |out: &mut Option<S>, upto: usize| -> S {
        out.take()
            .unwrap_or_else(|| S::from_raw(src.get(body..upto).unwrap_or("")))
    };
    loop {
        let Some(c) = char_at(src, i) else {
            return Err((UNTERMINATED_QUOTED_SCALAR, "quoted scalar is not closed"));
        };
        match c {
            // c-quoted-quote (production 117).
            '\'' if !double && byte(src, i.saturating_add(1)) == Some(b'\'') => {
                let mut o = own(&mut out, i);
                o.push_char('\'');
                out = Some(o);
                ws_mark = None;
                i = i.saturating_add(2);
            }
            '\'' if !double => break,
            '"' if double => break,
            '\\' if double => {
                let e = i.saturating_add(1);
                let mut o = own(&mut out, i);
                ws_mark = None;
                if break_len(src, e) > 0 {
                    // s-double-escaped (production 112): no folding space.
                    multiline = true;
                    let line = next_line(src, e, parent);
                    check_line(src, &line, parent, double)?;
                    o.push_repeat('\n', line.empties);
                    i = line.content;
                } else {
                    let (ch, len) =
                        escape(src, e).ok_or((INVALID_ESCAPE, "invalid escape sequence"))?;
                    o.push_char(ch);
                    i = e.saturating_add(len);
                }
                out = Some(o);
            }
            '\n' | '\r' => {
                // s-flow-folded (production 74): trailing unescaped white
                // space before the break is dropped.
                multiline = true;
                let mut o = match out.take() {
                    Some(mut o) => {
                        if let Some(mark) = ws_mark {
                            o.truncate(mark);
                        }
                        o
                    }
                    None => {
                        let text = src.get(body..i).unwrap_or("");
                        S::from_raw(text.trim_end_matches([' ', '\t']))
                    }
                };
                ws_mark = None;
                let line = next_line(src, i, parent);
                check_line(src, &line, parent, double)?;
                push_fold(&mut o, line.empties);
                out = Some(o);
                i = line.content;
            }
            c if is_nb_json(c) => {
                if let Some(o) = out.as_mut() {
                    if c == ' ' || c == '\t' {
                        ws_mark.get_or_insert(o.len());
                    } else {
                        ws_mark = None;
                    }
                    o.push_char(c);
                }
                i = i.saturating_add(c.len_utf8());
            }
            _ => {
                return Err((
                    INVALID_QUOTED_CHARACTER,
                    "control character in a quoted scalar",
                ));
            }
        }
    }
    let len = out.as_ref().map_or(i.saturating_sub(body), Sink::len);
    Ok(Flow {
        end: i.saturating_add(1),
        value: out,
        len,
        multiline,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    fn q(src: &str, double: bool) -> Result<(usize, String, bool), Code> {
        quoted::<String>(src, 0, None, double)
            .map(|f| {
                let raw = src.get(1..f.end.saturating_sub(1)).unwrap_or("").to_owned();
                let v = f.value.unwrap_or(raw);
                assert_eq!(f.len, v.len());
                (f.end, v, f.multiline)
            })
            .map_err(|(c, _)| c)
    }

    #[test]
    fn single_quoted() {
        assert_eq!(q("'a''b' x", false), Ok((6, "a'b".into(), false)));
        assert_eq!(
            q("'a  \n  b\n\n c'", false),
            Ok((13, "a b\nc".into(), true))
        );
        assert_eq!(q("'a", false), Err(UNTERMINATED_QUOTED_SCALAR));
        assert_eq!(q("'a\n---\n'", false), Err(UNTERMINATED_QUOTED_SCALAR));
    }

    #[test]
    fn double_quoted() {
        assert_eq!(q(r#""a\tb\x41\u00e9\"""#, true).unwrap().1, "a\tbAé\"");
        assert_eq!(q("\"a \\\n   b\"", true).unwrap().1, "a b");
        assert_eq!(q("\"a\\\n\n b\"", true).unwrap().1, "a\nb");
        assert_eq!(q("\"a \\t\n b\"", true).unwrap().1, "a \t b");
        assert_eq!(q(r#""\q""#, true), Err(INVALID_ESCAPE));
        assert_eq!(q(r#""\x4""#, true), Err(INVALID_ESCAPE));
        assert_eq!(
            quoted::<String>("\"a\nb\"", 0, Some(0), true)
                .map_err(|e| e.0)
                .err(),
            Some(QUOTED_SCALAR_INDENTATION)
        );
    }

    /// `plain` then `plain_value` over the scanned bytes: end, value,
    /// multiline; the counted length must be the decoded length.
    fn p(src: &str, first_end: usize, parent: Option<usize>, flow: bool) -> (usize, String, bool) {
        let f = plain(src, 0, first_end, parent, flow);
        let v: String = plain_value(&src[..f.end]);
        assert_eq!(f.len, v.len(), "{src:?}");
        (f.end, v, f.multiline)
    }

    #[test]
    fn plain_multi_line() {
        assert_eq!(
            p("a b\n  c\n\n  d\ne", 3, Some(0), false),
            (12, "a b c\nd".into(), true)
        );
        assert_eq!(p("a\n# c\n b", 1, None, false), (1, "a".into(), false));
        assert_eq!(p("a\n---\n", 1, None, false).0, 1);
        assert_eq!(p("a\n- b", 1, None, false).1, "a - b");
        assert_eq!(p("a\n b]", 1, None, true).1, "a b");
        assert_eq!(p("a\n ]", 1, None, true).0, 1);
        // Every break style; white space around breaks is folded away.
        assert_eq!(p("a \r\n b\r\r c\t\n\t d", 1, None, false).1, "a b\nc d");
    }

    #[test]
    fn sinks_agree() {
        let mut s = String::from_raw("ab");
        let mut c = Count::from_raw("ab");
        s.push_char('\u{e9}');
        c.push_char('\u{e9}');
        s.push_repeat('\n', 3);
        c.push_repeat('\n', 3);
        Sink::push_str(&mut s, "xyz");
        c.push_str("xyz");
        assert_eq!((Sink::len(&s), c.len()), (10, 10));
        Sink::truncate(&mut s, 2);
        c.truncate(2);
        assert_eq!((s.as_str(), c.0), ("ab", 2));
        // Not a char boundary: unchanged rather than a panic.
        let mut e = String::from("\u{e9}");
        Sink::truncate(&mut e, 1);
        assert_eq!(e, "\u{e9}");
        let lines: Vec<&str> = split_lines("a\r\nb\rc\n").collect();
        assert_eq!(lines, ["a", "b", "c", ""]);
    }
}
