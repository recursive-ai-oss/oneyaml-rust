//! rustc-style source snippets for diagnostics, with no dependencies
//! (the default rendering); see [`Render`].

use std::fmt::Write as _;

use oneyaml_core::{Applicability, ColumnUnit, Diagnostic, LineIndex, Span};

/// Widest source window shown per snippet row, in display columns.
const MAX_COLS: usize = 100;
/// Columns kept before the first label when a line is cut.
const CONTEXT_COLS: usize = 24;
/// Most bytes scanned from a label to find its line's start or end.
const SCAN_CAP: usize = 1024;
/// Display columns of a tab.
const TAB_COLS: usize = 4;
/// Most characters of old or new text quoted in a suggestion line.
const TEXT_MAX: usize = 60;

/// Rendering of a diagnostic-bearing value as a rustc-style snippet of its
/// source. Implemented for [`Diagnostic`] and [`crate::Error`].
///
/// `Display` stays the compact one-line form; [`Render::render`] draws the
/// code, message, the labelled source lines with carets, help, path, and
/// suggested fixes:
///
/// ```text
/// error[Y0027]: `: ` inside an unquoted value starts a nested mapping, which cannot begin on this line
///  --> 1:4
///   |
/// 1 | a: b: c
///   |    ^^^^
///   |     - this `: ` starts a mapping
///   |
///   = help: to keep the `: ` as text, quote the whole value; …
///   = suggestion: quote the value: replace `b: c` with `'b: c'`
/// ```
///
/// # What is shown
///
/// * Every label's lines, in line order; lines that are not adjacent are
///   separated by `...`. A label spanning several lines is drawn on its
///   first line (to the end of the line) and on its last line (from the
///   start of the line), with its message on the last one.
/// * Primary labels are underlined with `^`, secondary ones with `-`; an
///   empty span (e.g. at the end of the input) gets one caret.
/// * Tabs are shown as four spaces; wide characters (CJK, emoji) count two
///   columns and combining marks none, so carets line up in a terminal.
///   Control characters, bidirectional overrides and Unicode line
///   separators are shown as `\u{FFFD}` so they cannot reorder or break the
///   snippet. `\n`, `\r\n` and a lone `\r` all end a line (§5.4).
/// * A line wider than 100 columns is cut to a window around its
///   labels, marked with `...` on the cut side.
///
/// # Cost
///
/// With an index ([`Render::render_indexed`]) a call costs O(labels ×
/// (log n + 100)) — the lines shown, never the input. Line starts
/// and ends are found by scans capped at 1024 bytes, so a
/// 100 MB single-line input costs the same as a short line.
/// [`Render::render`] on a [`Diagnostic`] builds a [`LineIndex`] first
/// (one O(n) pass); to render many diagnostics of one source, build the
/// index once and use `render_indexed`. [`Error::render`](crate::Error::render)
/// needs no index: an error already knows the line and column of its
/// primary span, and other labels are numbered by counting line breaks
/// from it.
///
/// # Robustness
///
/// Rendering never panics: spans past the end are clamped to it, spans
/// off `char` boundaries are moved back to one, inverted spans are read
/// as empty, and a [`LineIndex`] built from a different text only gives
/// wrong line numbers.
///
/// ```
/// use oneyaml::{Document, Render};
///
/// let src = "a: b: c\n";
/// let parsed = Document::parse_recovering(src);
/// let snippet = parsed.diagnostics()[0].render(src);
/// assert!(snippet.starts_with("error[Y0027]"));
/// assert!(snippet.contains("1 | a: b: c"));
/// ```
///
/// To render many diagnostics of one source (a recovering parse's, or the
/// warnings `from_str` skips), build its [`LineIndex`] once and use
/// [`Render::render_indexed`]:
///
/// ```
/// use oneyaml::{Document, LineIndex, Render};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let src = "%YAML 1.1\n---\nquote: 'it's'\n";
/// let parsed = Document::parse_recovering(src);
/// let index = LineIndex::new(src);
/// let shown: Vec<String> = parsed
///     .diagnostics()
///     .iter()
///     .map(|d| d.render_indexed(src, &index))
///     .collect();
/// assert!(shown[0].starts_with("warning[Y1000]"));
/// assert!(shown[1].contains("write `''` for a literal `'`"));
///
/// // The strict entry points keep the warnings recorded before the error.
/// let err = oneyaml::from_str::<oneyaml::Value>(src).err().ok_or("expected an error")?;
/// assert_eq!(err.warnings()[0].code, oneyaml::code::YAML_VERSION_1_1);
/// assert!(err.render_indexed(src, &index).contains("3 | quote: 'it's'"));
/// # Ok(())
/// # }
/// ```
///
/// With the `miette` feature, `DiagnosticReport` wraps any [`Diagnostic`]
/// (with its source) as a `miette::Diagnostic`.
pub trait Render {
    /// The snippet for `src`, the text the spans point into. Never panics,
    /// whatever `src` is (see the trait docs for the cost).
    ///
    /// For a [`Diagnostic`] this builds a [`LineIndex`] of `src` on every
    /// call (O(n)): to render several diagnostics of one text, build the
    /// index once and call [`Render::render_indexed`], or use
    /// [`Document::render_diagnostics`](crate::Document::render_diagnostics)
    /// / [`Parsed::render_diagnostics`](crate::Parsed::render_diagnostics).
    #[must_use]
    fn render(&self, src: &str) -> String;

    /// [`Render::render`] with line numbers from `index`, an index of
    /// `src` built once for many calls: O(lines shown), never O(input).
    #[must_use]
    fn render_indexed(&self, src: &str, index: &LineIndex) -> String;
}

impl Render for Diagnostic {
    fn render(&self, src: &str) -> String {
        render(self, src, Numbers::Index(&LineIndex::new(src)), true)
    }

    fn render_indexed(&self, src: &str, index: &LineIndex) -> String {
        render(self, src, Numbers::Index(index), true)
    }
}

/// Where line numbers come from.
#[derive(Clone, Copy)]
pub(crate) enum Numbers<'a> {
    /// An index of the source (possibly of another text: then the numbers
    /// are wrong, but nothing fails).
    Index(&'a LineIndex),
    /// The 0-based line and `char` column of byte `at`; other offsets are
    /// numbered by counting line breaks from it.
    Anchor { at: usize, line: usize, col: usize },
}

impl Numbers<'_> {
    /// The 0-based line of byte `x` (a clamped `char` boundary of `src`).
    fn line(self, src: &str, x: usize) -> usize {
        match self {
            Self::Index(index) => index.line_col(x, ColumnUnit::Utf8).map_or(0, |lc| lc.line),
            Self::Anchor { at, line, .. } => {
                if x >= at {
                    line.saturating_add(breaks(src, at, x))
                } else {
                    line.saturating_sub(breaks(src, x, at))
                }
            }
        }
    }

    /// The 0-based line and `char` column of byte `x`.
    fn position(self, src: &str, x: usize) -> (usize, usize) {
        match self {
            Self::Index(index) => index
                .line_col(x, ColumnUnit::Char)
                .map_or((0, 0), |lc| (lc.line, lc.column)),
            Self::Anchor { at, line, col } if at == x => (line, col),
            Self::Anchor { at, col, .. } => {
                // Exact, never capped: counted from the anchor when `x`
                // is on its line or after it, else from `x`'s line start.
                // O(distance to the anchor + the line), no index needed.
                let bytes = src.as_bytes();
                let chars = |a: usize, b: usize| src.get(a..b).map_or(0, |s| s.chars().count());
                let column = if x > at {
                    match bytes
                        .get(at..x)
                        .and_then(|p| p.iter().rposition(|b| is_break(*b)))
                    {
                        Some(k) => chars(at.saturating_add(k).saturating_add(1), x),
                        None => col.saturating_add(chars(at, x)),
                    }
                } else {
                    let start = bytes
                        .get(..x)
                        .and_then(|p| p.iter().rposition(|b| is_break(*b)))
                        .map_or(0, |k| k.saturating_add(1));
                    // A byte order mark at the start of the text is no column.
                    let mark = usize::from(start == 0 && x > 0 && src.starts_with('\u{feff}'));
                    chars(start, x).saturating_sub(mark)
                };
                (self.line(src, x), column)
            }
        }
    }
}

/// Line breaks (`\n`, `\r\n`, lone `\r`) in `src[from..to]`; a `\r\n`
/// split by `to` counts once, at its `\r`.
fn breaks(src: &str, from: usize, to: usize) -> usize {
    let bytes = src.as_bytes();
    let Some(part) = bytes.get(from..to) else {
        return 0;
    };
    let mut n = 0usize;
    for (k, b) in part.iter().enumerate() {
        let lf_after_cr = *b == b'\n'
            && k.checked_add(from)
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| bytes.get(i))
                == Some(&b'\r');
        if *b == b'\r' || (*b == b'\n' && !lf_after_cr) {
            n = n.saturating_add(1);
        }
    }
    n
}

/// `x` clamped to `src` and moved back to a `char` boundary.
fn clamp(src: &str, x: usize) -> usize {
    let mut x = x.min(src.len());
    while x > 0 && !src.is_char_boundary(x) {
        x = x.saturating_sub(1);
    }
    x
}

/// A span clamped to `src`: in bounds, on `char` boundaries, not
/// inverted.
fn clamp_span(src: &str, span: Span) -> (usize, usize) {
    let start = clamp(src, span.start);
    let end = clamp(src, span.end).max(start);
    (start, end)
}

fn is_break(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// The start of the line holding `x`, scanning back at most [`SCAN_CAP`]
/// bytes; `true` when the scan was cut off.
fn line_start(src: &str, x: usize) -> (usize, bool) {
    let from = x.saturating_sub(SCAN_CAP);
    let found = src
        .as_bytes()
        .get(from..x)
        .and_then(|part| part.iter().rposition(|b| is_break(*b)));
    match found {
        Some(k) => (from.saturating_add(k).saturating_add(1), false),
        None if from == 0 => (0, false),
        None => (clamp_up(src, from), true),
    }
}

/// The end of the line holding `x` (before its break), scanning forward
/// at most [`SCAN_CAP`] bytes; `true` when the scan was cut off.
fn line_end(src: &str, x: usize) -> (usize, bool) {
    let to = x.saturating_add(SCAN_CAP).min(src.len());
    let found = src
        .as_bytes()
        .get(x..to)
        .and_then(|part| part.iter().position(|b| is_break(*b)));
    match found {
        Some(k) => (x.saturating_add(k), false),
        None if to == src.len() => (to, false),
        None => (clamp(src, to), true),
    }
}

/// `x` moved forward to a `char` boundary (at most `src.len()`).
fn clamp_up(src: &str, x: usize) -> usize {
    let mut x = x.min(src.len());
    while x < src.len() && !src.is_char_boundary(x) {
        x = x.saturating_add(1);
    }
    x
}

/// If `end` (after `start`) is the first byte of a line, the end of the
/// previous line: a span that includes its trailing line break is drawn on
/// the line it belongs to.
fn before_break(src: &str, start: usize, end: usize) -> usize {
    let bytes = src.as_bytes();
    let Some(prev) = end.checked_sub(1) else {
        return end;
    };
    if end <= start || !bytes.get(prev).copied().is_some_and(is_break) {
        return end;
    }
    let crlf = bytes.get(prev) == Some(&b'\n')
        && prev
            .checked_sub(1)
            .and_then(|i| bytes.get(i))
            .is_some_and(|b| *b == b'\r');
    let at = if crlf { prev.saturating_sub(1) } else { prev };
    at.max(start)
}

/// How a character is shown: its replacement text and display width.
fn shown(c: char) -> (Option<char>, usize) {
    let u = u32::from(c);
    match c {
        '\t' => (Some(' '), TAB_COLS),
        '\u{feff}' => (None, 0),
        _ if is_unsafe_to_show(u) => (Some('\u{fffd}'), 1),
        _ if is_zero_width(u) => (Some(c), 0),
        _ if is_wide(u) => (Some(c), 2),
        _ => (Some(c), 1),
    }
}

/// Characters that could break or reorder a terminal line: controls,
/// bidirectional formatting, and Unicode line/paragraph separators.
fn is_unsafe_to_show(u: u32) -> bool {
    matches!(u,
        0x00..=0x08 | 0x0a..=0x1f | 0x7f..=0x9f
        | 0x061c | 0x200e | 0x200f | 0x2028..=0x202e | 0x2066..=0x2069)
}

/// Combining marks and other zero-width characters (the common blocks;
/// an approximation of `wcwidth`, enough for alignment in practice).
fn is_zero_width(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036f | 0x0483..=0x0489 | 0x0591..=0x05bd | 0x0610..=0x061a
        | 0x064b..=0x065f | 0x0670 | 0x06d6..=0x06dc | 0x0900..=0x0902
        | 0x093a..=0x094f | 0x0e31 | 0x0e34..=0x0e3a | 0x0e47..=0x0e4e
        | 0x1ab0..=0x1aff | 0x1dc0..=0x1dff | 0x200b..=0x200d | 0x20d0..=0x20ff
        | 0xfe00..=0xfe0f | 0xfe20..=0xfe2f | 0xe0100..=0xe01ef)
}

/// East Asian Wide and Fullwidth characters and emoji: two columns.
fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115f | 0x231a..=0x231b | 0x2329..=0x232a | 0x23e9..=0x23ec
        | 0x23f0 | 0x23f3 | 0x25fd..=0x25fe | 0x2614..=0x2615 | 0x2648..=0x2653
        | 0x267f | 0x2693 | 0x26a1 | 0x26aa..=0x26ab | 0x26bd..=0x26be
        | 0x26c4..=0x26c5 | 0x26ce | 0x26d4 | 0x26ea | 0x26f2..=0x26f3 | 0x26f5
        | 0x26fa | 0x26fd | 0x2705 | 0x270a..=0x270b | 0x2728 | 0x274c | 0x274e
        | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27b0 | 0x27bf
        | 0x2b1b..=0x2b1c | 0x2b50 | 0x2b55 | 0x2e80..=0x303e | 0x3041..=0x33ff
        | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xa000..=0xa4cf | 0xa960..=0xa97f
        | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe10..=0xfe19 | 0xfe30..=0xfe6f
        | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x16fe0..=0x16fe4 | 0x17000..=0x18cff
        | 0x1b000..=0x1b2ff | 0x1f004 | 0x1f0cf | 0x1f18e | 0x1f191..=0x1f19a
        | 0x1f200..=0x1f251 | 0x1f300..=0x1f64f | 0x1f680..=0x1f6ff
        | 0x1f7e0..=0x1f7eb | 0x1f90c..=0x1f9ff | 0x1fa70..=0x1faff
        | 0x20000..=0x2fffd | 0x30000..=0x3fffd)
}

/// Display width of `text`, stopping early once past `limit`.
fn width(text: &str, limit: usize) -> usize {
    let mut w = 0usize;
    for c in text.chars() {
        w = w.saturating_add(shown(c).1);
        if w > limit {
            break;
        }
    }
    w
}

/// Walks back from `x` over at most `cols` display columns, not before
/// `floor`.
fn walk_back(src: &str, x: usize, floor: usize, cols: usize) -> usize {
    let Some(part) = src.get(floor..x) else {
        return x;
    };
    let mut w = 0usize;
    let mut at = x;
    for (k, c) in part.char_indices().rev() {
        w = w.saturating_add(shown(c).1);
        if w > cols {
            break;
        }
        at = floor.saturating_add(k);
    }
    at
}

/// Walks forward from `x` over at most `cols` display columns, not past
/// `ceil`.
fn walk_forward(src: &str, x: usize, ceil: usize, cols: usize) -> usize {
    let Some(part) = src.get(x..ceil) else {
        return x;
    };
    let mut w = 0usize;
    for (k, c) in part.char_indices() {
        w = w.saturating_add(shown(c).1);
        if w > cols {
            return x.saturating_add(k);
        }
    }
    ceil
}

/// Messages on one row: line breaks and controls become spaces or
/// `\u{FFFD}`.
fn one_line(text: &str) -> String {
    use crate::error::{MAX_MESSAGE_BYTES, TRUNCATED};
    // At most `MAX_MESSAGE_BYTES` of any one text (a caller's
    // own `Diagnostic` may hold a longer one), then the marker.
    let room = if text.len() > MAX_MESSAGE_BYTES {
        MAX_MESSAGE_BYTES.saturating_sub(TRUNCATED.len())
    } else {
        MAX_MESSAGE_BYTES
    };
    let mut out = String::new();
    for c in text.chars() {
        let c = match c {
            '\n' | '\r' | '\t' => ' ',
            c if is_unsafe_to_show(u32::from(c)) => '\u{fffd}',
            c => c,
        };
        if out.len().saturating_add(c.len_utf8()) > room {
            out.push_str(TRUNCATED);
            break;
        }
        out.push(c);
    }
    out
}

/// A quoted excerpt of `text` for a suggestion line: at most [`TEXT_MAX`]
/// characters, line breaks written `\n`.
pub(crate) fn excerpt(text: &str) -> String {
    let mut out = String::new();
    for (n, c) in text.chars().enumerate() {
        if n >= TEXT_MAX {
            out.push('…');
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_unsafe_to_show(u32::from(c)) => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

/// One label's part on one line.
#[derive(Clone, Copy)]
struct Segment<'d> {
    line: usize,
    start: usize,
    end: usize,
    primary: bool,
    /// Shown on the label's last line only.
    message: Option<&'d str>,
}

/// Splits a clamped label into its per-line segments.
fn segments<'d>(
    src: &str,
    numbers: Numbers<'_>,
    span: Span,
    primary: bool,
    message: &'d str,
    out: &mut Vec<Segment<'d>>,
) {
    let (start, end) = clamp_span(src, span);
    let end = before_break(src, start, end);
    let message = (!message.is_empty()).then_some(message);
    let first = numbers.line(src, start);
    let last = if end == start {
        first
    } else {
        numbers.line(src, end)
    };
    if first == last {
        out.push(Segment {
            line: first,
            start,
            end,
            primary,
            message,
        });
        return;
    }
    let (first_end, _) = line_end(src, start);
    let (last_start, _) = line_start(src, end);
    out.push(Segment {
        line: first,
        start,
        end: first_end.max(start),
        primary,
        message: None,
    });
    out.push(Segment {
        line: last,
        start: last_start.min(end),
        end,
        primary,
        message,
    });
}

/// The rows of one line: one or more windows, each a source row and its
/// marker rows. `segs` are this line's segments, sorted by start.
fn line_rows(src: &str, gutter: usize, number: usize, segs: &[Segment<'_>], out: &mut String) {
    let mut rest = segs;
    while let Some(first) = rest.first() {
        let anchor = first.start;
        let (ls, ls_cut) = line_start(src, anchor);
        let (le, le_cut) = line_end(src, anchor);
        let whole = !ls_cut
            && !le_cut
            && src
                .get(ls..le)
                .is_some_and(|l| width(l, MAX_COLS) <= MAX_COLS);
        let (ws, we) = if whole {
            (ls, le)
        } else {
            let ws = walk_back(src, anchor, ls, CONTEXT_COLS);
            (ws, walk_forward(src, ws, le, MAX_COLS))
        };
        let taken = rest
            .iter()
            .skip(1)
            .position(|s| s.start > we)
            .map_or(rest.len(), |k| k.saturating_add(1));
        let (window, after) = rest.split_at(taken.min(rest.len()));
        rest = after;
        let cut_left = ws > ls || ls_cut;
        let cut_right = we < le || le_cut;
        window_rows(
            src,
            gutter,
            number,
            (ws, we),
            (cut_left, cut_right),
            window,
            out,
        );
    }
}

/// Draws one window: the source row, then the marker row(s).
fn window_rows(
    src: &str,
    gutter: usize,
    number: usize,
    (ws, we): (usize, usize),
    (cut_left, cut_right): (bool, bool),
    segs: &[Segment<'_>],
    out: &mut String,
) {
    let lead = if cut_left { 3 } else { 0 };
    let mut text = String::new();
    if cut_left {
        text.push_str("...");
    }
    // Display column (after `lead`) of each char start, to place markers.
    let mut cols: Vec<(usize, usize)> = Vec::new();
    let mut col = 0usize;
    let body = src.get(ws..we).unwrap_or("");
    for (k, c) in body.char_indices() {
        cols.push((ws.saturating_add(k), col));
        let (shown_as, w) = shown(c);
        if let Some(s) = shown_as {
            if c == '\t' {
                text.extend(core::iter::repeat_n(' ', TAB_COLS));
            } else {
                text.push(s);
            }
        }
        col = col.saturating_add(w);
    }
    let body_cols = col;
    if cut_right {
        text.push_str("...");
    }
    let col_of = |x: usize| -> usize {
        if x >= we {
            return body_cols;
        }
        let k = cols.partition_point(|(at, _)| *at <= x);
        k.checked_sub(1)
            .and_then(|k| cols.get(k))
            .map_or(0, |(_, c)| *c)
    };
    let _ = writeln!(out, "{}", format!("{number:>gutter$} | {text}").trim_end());
    // (start col, end col, primary) per segment, clipped to the window.
    let marks: Vec<(usize, usize, bool, Option<&str>)> = segs
        .iter()
        .map(|s| {
            let c0 = col_of(s.start.max(ws));
            let c1 = col_of(s.end.min(we)).max(c0);
            (
                c0.saturating_add(lead),
                c1.saturating_add(lead),
                s.primary,
                s.message,
            )
        })
        .collect();
    let with_message = marks.iter().filter(|m| m.3.is_some()).count();
    // A message goes after the markers, so it may share their row only if
    // its label's markers are the rightmost; otherwise it would read as
    // another label's.
    let right = marks
        .iter()
        .map(|m| m.1.max(m.0.saturating_add(1)))
        .max()
        .unwrap_or(0);
    let message_last = marks
        .iter()
        .filter(|m| m.3.is_some())
        .all(|m| m.1.max(m.0.saturating_add(1)) == right);
    // Overlapping markers would hide each other: one row per label then.
    let overlap = marks.iter().enumerate().any(|(k, a)| {
        marks
            .iter()
            .skip(k.saturating_add(1))
            .any(|b| a.0 < b.1.max(b.0.saturating_add(1)) && b.0 < a.1.max(a.0.saturating_add(1)))
    });
    if with_message <= 1 && !overlap && message_last {
        let message = marks.iter().find_map(|m| m.3);
        marker_row(gutter, &marks, message, out);
    } else {
        for m in &marks {
            marker_row(gutter, core::slice::from_ref(m), m.3, out);
        }
    }
}

/// One row of `^` (primary) and `-` (secondary) markers and a message.
fn marker_row(
    gutter: usize,
    marks: &[(usize, usize, bool, Option<&str>)],
    message: Option<&str>,
    out: &mut String,
) {
    let len = marks
        .iter()
        .map(|m| m.1.max(m.0.saturating_add(1)))
        .max()
        .unwrap_or(0);
    let mut row = vec![' '; len];
    for (c0, c1, primary, _) in marks {
        let stop = (*c1).max(c0.saturating_add(1));
        for slot in row.iter_mut().take(stop).skip(*c0) {
            if *primary {
                *slot = '^';
            } else if *slot == ' ' {
                *slot = '-';
            }
        }
    }
    let mut line: String = row.into_iter().collect();
    if let Some(m) = message {
        line.push(' ');
        line.push_str(&one_line(m));
    }
    let _ = writeln!(out, "{}", format!("{:gutter$} | {line}", "").trim_end());
}

/// Renders `diag` against `src`. `has_span`: the primary label points at
/// real source (an error without a known position shows no snippet).
pub(crate) fn render(diag: &Diagnostic, src: &str, numbers: Numbers<'_>, has_span: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}[{}]: {}",
        diag.severity,
        diag.code,
        one_line(&diag.message)
    );
    let mut gutter = 1usize;
    if has_span {
        let mut segs = Vec::new();
        segments(
            src,
            numbers,
            diag.primary.span,
            true,
            &diag.primary.message,
            &mut segs,
        );
        for label in &diag.secondary {
            segments(src, numbers, label.span, false, &label.message, &mut segs);
        }
        segs.sort_by_key(|s| (s.line, s.start, !s.primary));
        let max_line = segs.iter().map(|s| s.line).max().unwrap_or(0);
        gutter = max_line.saturating_add(1).to_string().len();
        let (start, _) = clamp_span(src, diag.primary.span);
        let (line, col) = numbers.position(src, start);
        let _ = writeln!(
            out,
            "{:gutter$}--> {}:{}",
            "",
            line.saturating_add(1),
            col.saturating_add(1)
        );
        let _ = writeln!(out, "{:gutter$} |", "");
        let mut prev: Option<usize> = None;
        let mut rest = &segs[..];
        while let Some(first) = rest.first() {
            let n = rest
                .iter()
                .position(|s| s.line != first.line)
                .unwrap_or(rest.len());
            let (group, after) = rest.split_at(n);
            rest = after;
            if prev.is_some_and(|p| first.line > p.saturating_add(1)) {
                let _ = writeln!(out, "{:gutter$}...", "");
            }
            line_rows(src, gutter, first.line.saturating_add(1), group, &mut out);
            prev = Some(first.line);
        }
    }
    let notes = diag.help.is_some() || diag.path.is_some() || !diag.suggestions.is_empty();
    if notes && has_span {
        let _ = writeln!(out, "{:gutter$} |", "");
    }
    if let Some(help) = &diag.help {
        let _ = writeln!(out, "{:gutter$} = help: {}", "", one_line(help));
    }
    if let Some(path) = diag.path.as_ref().filter(|p| !p.is_empty()) {
        let _ = writeln!(
            out,
            "{:gutter$} = path: {}",
            "",
            one_line(&crate::error::shown_path(path))
        );
    }
    for s in &diag.suggestions {
        // A fix that is not machine-applicable says so.
        let check = if s.applicability == Applicability::MachineApplicable {
            ""
        } else {
            " (may be incorrect)"
        };
        let _ = write!(
            out,
            "{:gutter$} = suggestion{check}: {}",
            "",
            one_line(&s.message)
        );
        match s.edits.as_slice() {
            [edit] => {
                let (a, b) = clamp_span(src, edit.span);
                let old = src.get(a..b).unwrap_or("");
                let new = &edit.text;
                let _ = match (old.is_empty(), new.is_empty()) {
                    (true, _) => {
                        let (line, col) = numbers.position(src, a);
                        write!(
                            out,
                            ": insert `{}` at {}:{}",
                            excerpt(new),
                            line.saturating_add(1),
                            col.saturating_add(1)
                        )
                    }
                    (false, true) => write!(out, ": remove `{}`", excerpt(old)),
                    (false, false) => {
                        write!(out, ": replace `{}` with `{}`", excerpt(old), excerpt(new))
                    }
                };
            }
            edits => {
                let _ = write!(out, " ({} edits)", edits.len());
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn breaks_count_crlf_once() {
        let src = "a\r\nb\rc\nd";
        assert_eq!(breaks(src, 0, src.len()), 3);
        assert_eq!(breaks(src, 2, src.len()), 2); // the `\n` of `\r\n`
        assert_eq!(breaks(src, 0, 2), 1);
        assert_eq!(breaks(src, 5, 2), 0);
    }

    #[test]
    fn clamping_never_leaves_the_text() {
        let src = "é\u{1F600}";
        assert_eq!(clamp(src, 1), 0);
        assert_eq!(clamp(src, 99), src.len());
        assert_eq!(clamp_up(src, 3), 6);
        assert_eq!(clamp_span(src, Span::new(5, 1)), (2, 2));
        assert_eq!(before_break("ab\r\ncd", 0, 4), 2);
        assert_eq!(before_break("ab\ncd", 3, 3), 3);
    }

    #[test]
    fn widths() {
        assert_eq!(width("a\tb", 99), 6);
        assert_eq!(width("中文", 99), 4);
        assert_eq!(width("e\u{301}", 99), 1);
        assert_eq!(width("\u{1F600}", 99), 2);
        assert_eq!(shown('\u{202e}'), (Some('\u{fffd}'), 1));
    }

    #[test]
    fn capped_scans() {
        let long = "x".repeat(3 * SCAN_CAP);
        assert_eq!(line_start(&long, 2 * SCAN_CAP), (SCAN_CAP, true));
        assert_eq!(line_end(&long, 0), (SCAN_CAP, true));
        assert_eq!(line_start("ab\ncd", 4), (3, false));
        assert_eq!(line_end("ab\ncd", 0), (2, false));
    }
}
