//! Block scalars (YAML 1.2.2 §8.1): the header (production 162), content
//! indentation detection (§8.1.1.1), chomping (§8.1.1.2), literal (§8.1.2)
//! and folded (§8.1.3) content, and the scalar's span under event-contract
//! rule C5.
//!
//! One forward pass over the lines; no recursion, O(n). The
//! parser runs [`body`] with a counting [`Sink`]; [`block_value`] runs it
//! again over the scalar's own span with a `String` sink to decode the value
//! on demand (one implementation).

use oneyaml_core::Span;
use oneyaml_core::code::{
    BLOCK_SCALAR_INDENTATION, DUPLICATE_PROPERTY, INVALID_BLOCK_HEADER, SCALAR_TOO_LONG,
    TAB_INDENTATION,
};

use crate::event::{BlockHeader, Chomping, Event, EventKind, Properties, ScalarStyle, TriviaKind};

use super::scalar::{Count, Sink};
use super::scan::{
    any_marker_at, blank_or_end, break_len, break_or_end, byte, first_mark, line_end, mark_at,
    skip_white,
};
use super::{NodeCtx, Parser};

/// A line starting at `i` that ends a block scalar indented by 0: a
/// document marker, or a byte order mark, which is no `nb-char` (production
/// 27) and so starts a document prefix (production 211).
fn prefix_break_at(src: &str, i: usize) -> bool {
    any_marker_at(src, i) || mark_at(src, i)
}

/// Where the indicators of the header whose `|` or `>` is at `start` end: the
/// first character that is not an indentation indicator (a digit from 1 to
/// 9) or a chomping indicator, or is a second one of its kind.
pub(super) fn header_stop(src: &str, start: usize) -> usize {
    let mut i = start.saturating_add(1);
    let (mut chomping, mut indicator) = (false, false);
    loop {
        match byte(src, i) {
            Some(b'1'..=b'9') if !indicator => indicator = true,
            Some(b'+' | b'-') if !chomping => chomping = true,
            _ => return i,
        }
        i = i.saturating_add(1);
    }
}

/// A parsed header: its end, chomping, and explicit indentation indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) end: usize,
    pub(super) chomping: Chomping,
    pub(super) indicator: Option<u8>,
}

/// Parses `c-b-block-header` (production 162) after the `|`/`>` at `start`.
/// `None` for `0`, repeated indicators, or no separation after the header.
pub(super) fn header(src: &str, start: usize) -> Option<Header> {
    let i = header_stop(src, start);
    let mut chomping = None;
    let mut indicator = None;
    for k in start.saturating_add(1)..i {
        match byte(src, k) {
            Some(d @ b'1'..=b'9') => indicator = Some(d.saturating_sub(b'0')),
            Some(b'+') => chomping = Some(Chomping::Keep),
            _ => chomping = Some(Chomping::Strip),
        }
    }
    blank_or_end(src, i).then_some(Header {
        end: i,
        chomping: chomping.unwrap_or(Chomping::Clip),
        indicator,
    })
}

/// The scanned body of a block scalar.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Body<S> {
    /// Content indentation (explicit or detected).
    pub(super) indent: usize,
    /// End of the scalar's span (C5).
    pub(super) end: usize,
    /// The value after folding and chomping.
    pub(super) value: S,
}

/// Why a body is invalid; `line` is the start of the offending line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BodyError {
    /// A leading empty line is more indented than the first content line.
    LeadingIndentation { line: usize },
    /// The line ending the scalar is white space containing a tab: it is
    /// neither an empty line nor a trailing comment (`l-chomped-empty`/`l-trail-comments`, productions 166-169).
    Tab { line: usize },
    /// The value is longer than `Limits::max_scalar_len`; `line` is
    /// the first body line.
    TooLong { line: usize },
}

/// One source line.
struct Line {
    start: usize,
    /// Leading spaces.
    spaces: usize,
    /// End of the line's text (before its break).
    end: usize,
    /// Start of the next line, or `end` at end of input.
    next: usize,
    has_break: bool,
    /// Only spaces (possibly none).
    all_spaces: bool,
}

fn line_at(src: &str, start: usize) -> Line {
    let end = line_end(src, start);
    let spaces = src
        .as_bytes()
        .get(start..end)
        .map_or(0, |r| r.iter().take_while(|b| **b == b' ').count());
    let blen = break_len(src, end);
    Line {
        start,
        spaces,
        end,
        next: end.saturating_add(blen),
        has_break: blen > 0,
        all_spaces: start.saturating_add(spaces) == end,
    }
}

/// Where a body's content lines are.
#[derive(Clone, Copy, Debug)]
pub(super) struct Indent {
    /// Content lines need at least this many spaces while detecting.
    pub(super) min: usize,
    /// The content indentation, when known in advance.
    pub(super) known: Option<usize>,
}

impl Indent {
    /// For a block scalar whose parent has indentation `parent` (`None` =
    /// -1): content at n+m with m >= 1 (`c-l+literal`/`c-l+folded`,
    /// productions 170 and 174), m given by the header's indicator if any.
    pub(super) fn of(parent: Option<usize>, header: Header) -> Self {
        Self {
            min: parent.map_or(0, |n| n.saturating_add(1)),
            known: header.indicator.map(|m| {
                parent.map_or(usize::from(m).saturating_sub(1), |n| {
                    n.saturating_add(usize::from(m))
                })
            }),
        }
    }
}

/// From the line starting at `from`, only white space and comment lines
/// remain before the end of the input or a line that starts a document
/// marker. Such lines after a complete node are `l-comment` (production 78)
/// before the end of the document (`l-document-prefix`, production 202), so
/// a line of white space with a tab is valid there; between two entries it
/// is not, because an entry starts with spaces.
fn only_blank_lines_follow(src: &str, from: usize) -> bool {
    let mut i = from;
    while i < src.len() {
        if any_marker_at(src, i) {
            return true;
        }
        let line = line_end(src, i);
        let text = skip_white(src, i);
        if text < line && byte(src, text) != Some(b'#') {
            return false;
        }
        i = line.saturating_add(break_len(src, line));
        if i == line {
            break;
        }
    }
    true
}

/// Scans the body starting at `body` (the line after the header) of a block
/// scalar with the given indentation and chomping.
pub(super) fn body<S: Sink>(
    src: &str,
    body: usize,
    indentation: Indent,
    chomping: Chomping,
    folded: bool,
) -> Result<Body<S>, BodyError> {
    let min = indentation.min;
    let mut indent = indentation.known;
    let mut leading_max = 0usize;
    let mut value = S::from_raw("");
    let mut empties = 0usize;
    // Kind of the previous content line: `true` for folded text (starts
    // with a non-white character), `false` for spaced text.
    let mut prev: Option<bool> = None;
    let mut content_end = body;
    let mut keep_end = body;
    let mut i = body;
    let mut stop: Option<Line> = None;
    while i < src.len() {
        let line = line_at(src, i);
        let n = match indent {
            Some(n) => n,
            None if line.all_spaces => {
                // A leading empty line (production 70) while detecting. A
                // final unterminated one counts for the value (end of input
                // ends the line) but stays outside the span (C5).
                leading_max = leading_max.max(line.spaces);
                empties = empties.saturating_add(1);
                if !line.has_break {
                    break;
                }
                keep_end = line.next;
                i = line.next;
                continue;
            }
            None => {
                let marker = line.spaces == 0 && prefix_break_at(src, line.start);
                if line.spaces < min || marker {
                    stop = Some(line);
                    break;
                }
                if leading_max > line.spaces {
                    return Err(BodyError::LeadingIndentation { line: line.start });
                }
                indent = Some(line.spaces);
                line.spaces
            }
        };
        if line.all_spaces && line.spaces <= n {
            // An empty line (l-empty): kept only if content follows, or
            // under keep chomping. See above for an unterminated one.
            empties = empties.saturating_add(1);
            if !line.has_break {
                break;
            }
            keep_end = line.next;
            i = line.next;
            continue;
        }
        let marker = n == 0 && prefix_break_at(src, line.start);
        if line.spaces < n || marker {
            stop = Some(line);
            break;
        }
        // A content line: its text after the indentation.
        let text = src
            .get(line.start.saturating_add(n)..line.end)
            .unwrap_or("");
        let folded_text = !text.starts_with([' ', '\t']);
        match prev {
            None => value.push_repeat('\n', empties),
            // b-l-folded (production 73) between two folded-text lines.
            Some(true) if folded && folded_text && empties == 0 => value.push_char(' '),
            Some(true) if folded && folded_text => {
                value.push_repeat('\n', empties);
            }
            Some(_) => value.push_repeat('\n', empties.saturating_add(1)),
        }
        value.push_str(text);
        prev = Some(folded_text);
        empties = 0;
        content_end = line.next;
        keep_end = line.next;
        i = line.next;
        if !line.has_break {
            break;
        }
    }
    if let Some(line) = stop {
        // What ends the scalar must be an empty line, a comment, or less
        // indented content; white space with a tab is none of these.
        let ws_end = skip_white(src, line.start);
        if ws_end == line.end
            && line.end > line.start.saturating_add(line.spaces)
            && !only_blank_lines_follow(src, line.start)
        {
            return Err(BodyError::Tab { line: line.start });
        }
    }
    let indent = indent.unwrap_or(min.max(leading_max));
    // b-chomped-last (production 165): the end of input ends the last line
    // like a break does (as yaml-test-suite L24T/01 and JEF9/02 record).
    let content = usize::from(prev.is_some());
    let (end, tail) = match chomping {
        Chomping::Keep => (keep_end, empties.saturating_add(content)),
        Chomping::Clip => (if prev.is_some() { content_end } else { body }, content),
        _ => (if prev.is_some() { content_end } else { body }, 0),
    };
    value.push_repeat('\n', tail);
    Ok(Body { indent, end, value })
}

/// The value of the block scalar covering `span` with `header`, decoded on
/// demand ([`Sourced::value`](crate::event::Sourced::value)): [`body`] over the
/// span alone with the content indentation already known.
///
/// A final unterminated line of at most `n` spaces lies outside the span
/// (C5) but ends a line of the value under keep chomping, as [`body`] saw
/// it at parse time (end of input ends the line, production 165).
pub(super) fn block_value(
    src: &str,
    span: Span,
    header: BlockHeader,
    folded: bool,
) -> Option<String> {
    let text = src.get(span.start..span.end)?;
    let n = header.content_indent;
    let indent = Indent {
        min: 0,
        known: Some(n),
    };
    // The span must be exactly the scalar's content lines (C5).
    let mut value = body::<String>(text, 0, indent, header.chomping, folded)
        .ok()
        .filter(|b| b.end == text.len())?
        .value;
    let rest = src.get(span.end..).unwrap_or("");
    let unterminated_empty = !rest.is_empty() && rest.len() <= n && rest.bytes().all(|b| b == b' ');
    if header.chomping == Chomping::Keep && unterminated_empty {
        value.push('\n');
    }
    Some(value)
}

impl<'a> Parser<'a> {
    /// A block scalar node (`s-l+block-scalar`, production 199) whose header
    /// is at the end of `events` (its properties and white space, if any).
    pub(super) fn block_scalar(
        &mut self,
        ctx: NodeCtx,
        mut events: Vec<Event<'a>>,
        props: Properties,
    ) {
        let src = self.src;
        let start = events.last().map_or(self.pos, |e| e.span().end);
        let folded = byte(src, start) == Some(b'>');
        // A byte order mark in the header, or between it and its
        // comment, is reported with the header error it causes.
        let header_mark = first_mark(src, start, line_end(src, start)).filter(|m| {
            src.as_bytes()
                .get(start.saturating_add(1)..*m)
                .is_some_and(|r| {
                    r.iter()
                        .all(|b| matches!(b, b'1'..=b'9' | b'+' | b'-' | b' ' | b'\t'))
                })
        });
        let Some(hdr) = header(src, start) else {
            self.blame.set(Some(header_stop(src, start)));
            return self.mark_then_fail(
                header_mark,
                INVALID_BLOCK_HEADER,
                "malformed block scalar header",
                true,
            );
        };
        // The rest of the header line: white space and a comment (production
        // 165, s-b-comment).
        let ws_end = skip_white(src, hdr.end);
        let comment = ws_end > hdr.end && byte(src, ws_end) == Some(b'#');
        let text_end = if comment {
            super::scan::comment_end(src, ws_end)
        } else {
            ws_end
        };
        if !break_or_end(src, text_end) {
            self.blame.set(Some(text_end));
            return self.mark_then_fail(
                header_mark,
                INVALID_BLOCK_HEADER,
                "text after a block scalar header",
                true,
            );
        }
        let body_start = text_end.saturating_add(break_len(src, text_end));
        if !self.merge_props(props) {
            return self.fail(DUPLICATE_PROPERTY, "duplicate anchor or tag", true);
        }
        let indent = Indent::of(ctx.parent, hdr);
        let scanned = body::<Count>(src, body_start, indent, hdr.chomping, folded).and_then(|b| {
            if self.scalar_len_ok(b.value.0) {
                Ok(b)
            } else {
                Err(BodyError::TooLong { line: body_start })
            }
        });
        // Emit properties, the header, and the rest of its line.
        let trivia =
            |kind, a: usize, b: usize| Event::new(EventKind::Trivia(kind), Span::new(a, b));
        events.push(trivia(TriviaKind::BlockScalarHeader, start, hdr.end));
        if ws_end > hdr.end {
            events.push(trivia(TriviaKind::Whitespace, hdr.end, ws_end));
        }
        if comment {
            events.push(trivia(TriviaKind::Comment, ws_end, text_end));
        }
        if body_start > text_end {
            events.push(trivia(TriviaKind::LineBreak, text_end, body_start));
        }
        self.begin_content();
        self.commit_scratch(events);
        match scanned {
            Ok(b) => {
                let props = core::mem::take(&mut self.props);
                let style = if folded {
                    ScalarStyle::Folded
                } else {
                    ScalarStyle::Literal
                };
                let header = BlockHeader::new(
                    hdr.chomping,
                    hdr.indicator,
                    b.indent,
                    Span::new(start, hdr.end),
                );
                let span = Span::new(body_start, b.end);
                self.push_event(Event::scalar(span, style, props, Some(header)));
                self.pos = b.end;
                // A mark in the header's comment or in the
                // content (`l-nb-literal-text` is `nb-char`s, production
                // 27) is reported after the scalar, which keeps it: the
                // recovered value holds the mark as written.
                let bad = if comment {
                    super::comment_char_diagnostic(src, ws_end, text_end)
                } else {
                    None
                };
                self.marks_in_token(start, b.end, bad);
                let at_line_start = b.end == body_start && body_start > text_end
                    || src
                        .as_bytes()
                        .get(..b.end)
                        .is_some_and(|r| r.ends_with(b"\n") || r.ends_with(b"\r"));
                if at_line_start {
                    self.line_start = b.end;
                    self.line_indent = 0;
                    self.fresh = true;
                }
                self.child_done();
            }
            Err(err) => self.block_scalar_error(body_start, err),
        }
    }

    /// Abandons the header after a body error (C6 R2): white-space lines up
    /// to the offending line stay trivia, then the error.
    fn block_scalar_error(&mut self, body_start: usize, err: BodyError) {
        let src = self.src;
        let (line, code, message) = match err {
            BodyError::LeadingIndentation { line } => (
                line,
                BLOCK_SCALAR_INDENTATION,
                "a leading empty line is indented more than the block scalar's content",
            ),
            BodyError::Tab { line } => (
                line,
                TAB_INDENTATION,
                "tabs cannot be used for block indentation",
            ),
            BodyError::TooLong { line } => (line, SCALAR_TOO_LONG, "the scalar is too long"),
        };
        // Lines before the offending one are trivia only if all are white.
        let mut i = body_start;
        while i < line {
            let l = line_at(src, i);
            if skip_white(src, i) != l.end {
                break;
            }
            i = l.next;
        }
        let target = if i >= line { line } else { body_start };
        // The white lines are one lazily lexed trivia run (one queue
        // item), not an event pair per line, so this step's output does not
        // grow with the number of lines.
        self.out.trivia(body_start, target);
        self.emit_pos = target;
        let lead = src
            .as_bytes()
            .get(target..)
            .map_or(0, |r| r.iter().take_while(|b| **b == b' ').count());
        let text = target.saturating_add(lead);
        if matches!(err, BodyError::LeadingIndentation { .. }) && target == line && lead > 0 {
            self.push(EventKind::Trivia(TriviaKind::Whitespace), target, text);
            self.pos = text;
        } else {
            self.pos = target;
        }
        self.emit_pos = self.pos;
        self.line_start = target;
        // A byte order mark on the offending line is reported
        // too.
        self.mark_before_error = first_mark(src, line, line_end(src, line));
        self.fail(code, message, true);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    fn run(
        src: &str,
        parent: Option<usize>,
        folded: bool,
    ) -> Result<(usize, usize, String), BodyError> {
        let h = header(src, 0).unwrap();
        let body_start = line_at(src, 0).next;
        let indent = Indent::of(parent, h);
        let counted = body::<Count>(src, body_start, indent, h.chomping, folded);
        let b = body::<String>(src, body_start, indent, h.chomping, folded)?;
        // The parser's count is the decoded length...
        assert_eq!(counted.map(|c| c.value.0), Ok(b.value.len()));
        // ...and lazy decoding over the span alone gives the same value.
        let hdr = BlockHeader::new(h.chomping, h.indicator, b.indent, Span::new(0, h.end));
        let span = Span::new(body_start, b.end);
        assert_eq!(
            block_value(src, span, hdr, folded).as_deref(),
            Some(b.value.as_str()),
            "{src:?}"
        );
        Ok((b.indent, b.end, b.value))
    }

    #[test]
    fn headers() {
        assert_eq!(header("|", 0).unwrap().chomping, Chomping::Clip);
        let h = header(">2- # c", 0).unwrap();
        assert_eq!(
            (h.end, h.chomping, h.indicator),
            (3, Chomping::Strip, Some(2))
        );
        let h = header("|+9\n", 0).unwrap();
        assert_eq!(
            (h.end, h.chomping, h.indicator),
            (3, Chomping::Keep, Some(9))
        );
        assert!(header("|0", 0).is_none());
        assert!(header("|22", 0).is_none());
        assert!(header("|x", 0).is_none());
        assert!(header("|--", 0).is_none());
    }

    #[test]
    fn literal_chomping() {
        let src = "|\n a\n  b\n\n c\n\n\nd";
        assert_eq!(run(src, Some(0), false), Ok((1, 13, "a\n b\n\nc\n".into())));
        let keep = "|+\n a\n\n\nd";
        assert_eq!(run(keep, Some(0), false), Ok((1, 8, "a\n\n\n".into())));
        let strip = "|-\n a\n\n";
        assert_eq!(run(strip, Some(0), false), Ok((1, 6, "a".into())));
        // End of input on a content line; trailing short line is trivia.
        assert_eq!(run("|\n a", Some(0), false), Ok((1, 4, "a\n".into())));
        assert_eq!(run("|-\n a", Some(0), false), Ok((1, 5, "a".into())));
        assert_eq!(run("|+\n a\n ", Some(0), false), Ok((1, 6, "a\n\n".into())));
        assert_eq!(run("|+\n   ", Some(0), false), Ok((3, 3, "\n".into())));
    }

    #[test]
    fn empty_bodies() {
        assert_eq!(run("|\n", Some(0), false), Ok((1, 2, String::new())));
        assert_eq!(
            run("|+\n\n  \nx", Some(0), false),
            Ok((2, 7, "\n\n".into()))
        );
        assert_eq!(
            run("|+\n\n  \nx", None, false),
            Err(BodyError::LeadingIndentation { line: 7 })
        );
        assert_eq!(run("|\n\nx", Some(0), false), Ok((1, 2, String::new())));
    }

    #[test]
    fn folding() {
        let src = ">\n a\n b\n\n c\n   d\n e\n";
        assert_eq!(run(src, None, true).unwrap().2, "a b\nc\n  d\ne\n");
        assert_eq!(run(">\n\n a\n", None, true).unwrap().2, "\na\n");
    }

    #[test]
    fn explicit_indentation_and_errors() {
        assert_eq!(run("|2\n   a\n  b\n", Some(0), false).unwrap().2, " a\nb\n");
        assert_eq!(run("|1\n  a\n", None, false).unwrap().2, "  a\n");
        assert_eq!(
            run(">\n  \n a\n", Some(0), true),
            Err(BodyError::LeadingIndentation { line: 5 })
        );
        assert_eq!(
            run("|\n\t\nb", Some(0), false),
            Err(BodyError::Tab { line: 2 })
        );
        assert_eq!(run("|\n---\n", None, false), Ok((0, 2, String::new())));
    }
}
