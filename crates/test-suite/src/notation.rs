//! Conversion of our event stream into yaml-test-suite's canonical event-tree
//! notation (`test.event`).
//!
//! Only semantic events are printed; trivia, directives, warnings, and errors
//! have no line in the notation. Nesting is validated with an explicit stack
//! (no recursion), so a malformed stream is reported rather than printed.

use oneyaml_syntax::event::{CollectionStyle, Event, EventKind, EventList, ScalarStyle, Sourced};

/// Why an event stream could not be converted to tree notation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotationError {
    /// The semantic event at `index` is not allowed where it occurs.
    Unexpected {
        /// Index into the event slice.
        index: usize,
        /// What was found, in notation form (`-MAP`, `+DOC`, …).
        found: &'static str,
        /// What was open (`STR`, `DOC`, `MAP`, `SEQ`, `-STR`, or `none`).
        open: &'static str,
    },
    /// A complete stream was required but something is still open.
    Unterminated {
        /// What was still open (`STR`, `DOC`, `MAP`, `SEQ`, or `none`).
        open: &'static str,
    },
    /// An event kind or style this harness version does not know about.
    UnknownEvent {
        /// Index into the event slice.
        index: usize,
    },
    /// The event's value, name, or tag cannot be read from the source
    /// (its bytes do not decode).
    Unreadable {
        /// Index into the event slice.
        index: usize,
    },
}

impl core::fmt::Display for NotationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unexpected { index, found, open } => {
                write!(f, "event {index}: unexpected {found} while {open} is open")
            }
            Self::Unterminated { open } => {
                write!(f, "event stream ended while {open} is open")
            }
            Self::UnknownEvent { index } => write!(f, "event {index}: unknown event kind"),
            Self::Unreadable { index } => {
                write!(f, "event {index}: payload cannot be read from the source")
            }
        }
    }
}

impl core::error::Error for NotationError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    Stream,
    Document,
    Mapping,
    Sequence,
}

impl Frame {
    const fn name(self) -> &'static str {
        match self {
            Self::Stream => "STR",
            Self::Document => "DOC",
            Self::Mapping => "MAP",
            Self::Sequence => "SEQ",
        }
    }
}

fn open_name(stack: &[Frame]) -> &'static str {
    stack.last().map_or("none", |f| f.name())
}

/// Converts a complete stream to yaml-test-suite tree notation, one event
/// per line, each terminated by `\n`. Values, anchors, tags, and alias names
/// are read through [`Sourced`], from the list's
/// own source.
///
/// # Errors
///
/// A [`NotationError`] when the semantic events are not a well-nested,
/// complete stream, or a payload cannot be read.
pub fn to_tree_notation(events: &EventList<'_>) -> Result<String, NotationError> {
    render(events, events.len(), true)
}

/// Like [`to_tree_notation`] but for the stream *prefix* of the first `len`
/// events (e.g. the events before the first error): open frames at the end
/// are allowed.
///
/// # Errors
///
/// A [`NotationError`] when the events are not well-nested so far.
pub fn to_tree_notation_prefix(
    events: &EventList<'_>,
    len: usize,
) -> Result<String, NotationError> {
    render(events, len, false)
}

fn render(events: &EventList<'_>, len: usize, complete: bool) -> Result<String, NotationError> {
    let mut out = String::new();
    // Explicit stack, no recursion on input-driven nesting.
    let mut stack: Vec<Frame> = Vec::new();
    let mut stream_done = false;

    for (index, event) in events.sourced().take(len).enumerate() {
        let top = stack.last().copied();
        let unexpected = |found: &'static str, stack: &[Frame]| NotationError::Unexpected {
            index,
            found,
            open: if stream_done {
                "-STR"
            } else {
                open_name(stack)
            },
        };
        let in_node_context = matches!(
            top,
            Some(Frame::Document | Frame::Mapping | Frame::Sequence)
        );
        let unreadable = NotationError::Unreadable { index };
        match event.kind() {
            EventKind::Trivia(_)
            | EventKind::Directive(_)
            | EventKind::Warning(_)
            | EventKind::Error(_) => {}
            EventKind::StreamStart => {
                if top.is_some() || stream_done {
                    return Err(unexpected("+STR", &stack));
                }
                stack.push(Frame::Stream);
                out.push_str("+STR\n");
            }
            EventKind::StreamEnd => {
                if top != Some(Frame::Stream) {
                    return Err(unexpected("-STR", &stack));
                }
                stack.pop();
                stream_done = true;
                out.push_str("-STR\n");
            }
            EventKind::DocumentStart { explicit, .. } => {
                if top != Some(Frame::Stream) {
                    return Err(unexpected("+DOC", &stack));
                }
                stack.push(Frame::Document);
                out.push_str(if *explicit { "+DOC ---\n" } else { "+DOC\n" });
            }
            EventKind::DocumentEnd { explicit, .. } => {
                if top != Some(Frame::Document) {
                    return Err(unexpected("-DOC", &stack));
                }
                stack.pop();
                out.push_str(if *explicit { "-DOC ...\n" } else { "-DOC\n" });
            }
            EventKind::MappingStart { style, .. } => {
                if !in_node_context {
                    return Err(unexpected("+MAP", &stack));
                }
                stack.push(Frame::Mapping);
                out.push_str("+MAP");
                out.push_str(flow_marker(*style, " {}", index)?);
                push_props(&mut out, &event).ok_or(unreadable)?;
                out.push('\n');
            }
            EventKind::MappingEnd => {
                if top != Some(Frame::Mapping) {
                    return Err(unexpected("-MAP", &stack));
                }
                stack.pop();
                out.push_str("-MAP\n");
            }
            EventKind::SequenceStart { style, .. } => {
                if !in_node_context {
                    return Err(unexpected("+SEQ", &stack));
                }
                stack.push(Frame::Sequence);
                out.push_str("+SEQ");
                out.push_str(flow_marker(*style, " []", index)?);
                push_props(&mut out, &event).ok_or(unreadable)?;
                out.push('\n');
            }
            EventKind::SequenceEnd => {
                if top != Some(Frame::Sequence) {
                    return Err(unexpected("-SEQ", &stack));
                }
                stack.pop();
                out.push_str("-SEQ\n");
            }
            EventKind::Scalar { style, .. } => {
                if !in_node_context {
                    return Err(unexpected("=VAL", &stack));
                }
                out.push_str("=VAL");
                push_props(&mut out, &event).ok_or(unreadable)?;
                out.push(' ');
                out.push(style_char(*style).ok_or(NotationError::UnknownEvent { index })?);
                let value = event.value().ok_or(NotationError::Unreadable { index })?;
                push_escaped(&mut out, &value);
                out.push('\n');
            }
            EventKind::Alias => {
                if !in_node_context {
                    return Err(unexpected("=ALI", &stack));
                }
                out.push_str("=ALI *");
                out.push_str(event.alias_name().ok_or(unreadable)?);
                out.push('\n');
            }
            _ => return Err(NotationError::UnknownEvent { index }),
        }
    }

    if complete && (!stream_done || !stack.is_empty()) {
        return Err(NotationError::Unterminated {
            open: open_name(&stack),
        });
    }
    Ok(out)
}

/// `{}`/`[]` for flow collections (including implicit flow pairs, which the
/// suite prints as flow mappings), nothing for block ones.
fn flow_marker(
    style: CollectionStyle,
    marker: &'static str,
    index: usize,
) -> Result<&'static str, NotationError> {
    match style {
        CollectionStyle::Block => Ok(""),
        CollectionStyle::Flow | CollectionStyle::FlowPair => Ok(marker),
        _ => Err(NotationError::UnknownEvent { index }),
    }
}

/// The node's anchor and resolved tag; `None` when a property the node
/// carries cannot be read from the source.
fn push_props(out: &mut String, event: &Sourced<'_, &Event<'_>>) -> Option<()> {
    let props = event.properties()?;
    if props.anchor.is_some() {
        out.push_str(" &");
        out.push_str(event.anchor()?.name);
    }
    if props.tag.is_some() {
        out.push_str(" <");
        out.push_str(&event.tag()?.resolved);
        out.push('>');
    }
    Some(())
}

const fn style_char(style: ScalarStyle) -> Option<char> {
    match style {
        ScalarStyle::Plain => Some(':'),
        ScalarStyle::SingleQuoted => Some('\''),
        ScalarStyle::DoubleQuoted => Some('"'),
        ScalarStyle::Literal => Some('|'),
        ScalarStyle::Folded => Some('>'),
        _ => None,
    }
}

/// Escapes a scalar value the way yaml-test-suite's `test.event` files do.
fn push_escaped(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use oneyaml_core::{Code, Diagnostic, Label, Span};
    use oneyaml_syntax::event::{BlockHeader, Chomping, Properties, TriviaKind};

    /// Hand-built fixtures are paired with their source explicitly.
    fn tree<'a>(src: &'a str, events: &[Event<'a>]) -> Result<String, NotationError> {
        to_tree_notation(&EventList::from_parts_unchecked(src, events.to_vec()))
    }

    fn tree_prefix<'a>(src: &'a str, events: &[Event<'a>]) -> Result<String, NotationError> {
        let list = EventList::from_parts_unchecked(src, events.to_vec());
        to_tree_notation_prefix(&list, list.len())
    }

    /// The source of the fixtures that need only one plain scalar.
    const X: &str = "x";

    fn ev(kind: EventKind) -> Event<'static> {
        Event::new(kind, Span::empty(0))
    }

    /// A flow scalar covering `start..end`.
    fn scalar(style: ScalarStyle, start: usize, end: usize) -> Event<'static> {
        Event::scalar(Span::new(start, end), style, Properties::none(), None)
    }

    /// The plain scalar `x` of [`X`].
    fn plain() -> Event<'static> {
        scalar(ScalarStyle::Plain, 0, 1)
    }

    fn wrap(inner: Vec<Event<'_>>) -> Vec<Event<'_>> {
        let mut v = vec![
            ev(EventKind::StreamStart),
            ev(EventKind::document_start(false)),
        ];
        v.extend(inner);
        v.push(ev(EventKind::document_end(false)));
        v.push(ev(EventKind::StreamEnd));
        v
    }

    #[test]
    fn empty_stream() {
        let events = [ev(EventKind::StreamStart), ev(EventKind::StreamEnd)];
        assert_eq!(tree("", &events).unwrap(), "+STR\n-STR\n");
    }

    #[test]
    fn explicit_markers_flow_styles_and_pairs() {
        let events = vec![
            ev(EventKind::StreamStart),
            ev(EventKind::document_start(true)),
            ev(EventKind::mapping_start(CollectionStyle::Flow)),
            ev(EventKind::sequence_start(CollectionStyle::Flow)),
            ev(EventKind::mapping_start(CollectionStyle::FlowPair)),
            scalar(ScalarStyle::Plain, 0, 1),
            scalar(ScalarStyle::Plain, 1, 2),
            ev(EventKind::MappingEnd),
            ev(EventKind::SequenceEnd),
            ev(EventKind::sequence_start(CollectionStyle::Block)),
            ev(EventKind::SequenceEnd),
            ev(EventKind::MappingEnd),
            ev(EventKind::document_end(true)),
            ev(EventKind::StreamEnd),
        ];
        assert_eq!(
            tree("kv", &events).unwrap(),
            "+STR\n+DOC ---\n+MAP {}\n+SEQ []\n+MAP {}\n=VAL :k\n=VAL :v\n-MAP\n-SEQ\n\
             +SEQ\n-SEQ\n-MAP\n-DOC ...\n-STR\n"
        );
    }

    #[test]
    fn properties_scalar_styles_and_alias() {
        // Values, names, and tags are read from the source.
        let src = "&a !!str p 's' \"d\" *a|\nl\n>\nf\n";
        let props = Properties::new(Some(0), Some(3), None);
        let clip = |at: usize| {
            Some(BlockHeader::new(
                Chomping::Clip,
                None,
                0,
                Span::new(at, at + 1),
            ))
        };
        let events = wrap(vec![
            Event::sequence_start(Span::empty(9), CollectionStyle::Block, props),
            Event::scalar(Span::new(9, 10), ScalarStyle::Plain, props, None),
            scalar(ScalarStyle::SingleQuoted, 11, 14),
            scalar(ScalarStyle::DoubleQuoted, 15, 18),
            Event::scalar(
                Span::new(23, 25),
                ScalarStyle::Literal,
                Properties::none(),
                clip(21),
            ),
            Event::scalar(
                Span::new(27, 29),
                ScalarStyle::Folded,
                Properties::none(),
                clip(25),
            ),
            scalar(ScalarStyle::Plain, 29, 29),
            Event::new(EventKind::Alias, Span::new(19, 21)),
            ev(EventKind::SequenceEnd),
        ]);
        assert_eq!(
            tree(src, &events).unwrap(),
            "+STR\n+DOC\n+SEQ &a <tag:yaml.org,2002:str>\n\
             =VAL &a <tag:yaml.org,2002:str> :p\n=VAL 's\n=VAL \"d\n\
             =VAL |l\\n\n=VAL >f\\n\n=VAL :\n=ALI *a\n-SEQ\n-DOC\n-STR\n"
        );
    }

    #[test]
    fn value_escaping_matches_suite() {
        let src = "\"a\\\\b\\b\\t\\n\\r\\0\u{e9}\"";
        let events = wrap(vec![scalar(ScalarStyle::DoubleQuoted, 0, src.len())]);
        assert_eq!(
            tree(src, &events).unwrap(),
            "+STR\n+DOC\n=VAL \"a\\\\b\\b\\t\\n\\r\\0\u{e9}\n-DOC\n-STR\n"
        );
    }

    #[test]
    fn unreadable_payloads_are_reported() {
        // A double-quoted scalar whose bytes do not decode, an alias that is
        // not `*name`, and an anchor position that is not an anchor.
        let src = "\"\\q\" x &";
        let cases = [
            scalar(ScalarStyle::DoubleQuoted, 0, 4),
            Event::new(EventKind::Alias, Span::new(5, 6)),
            Event::scalar(
                Span::new(5, 6),
                ScalarStyle::Plain,
                Properties::new(Some(1), None, None),
                None,
            ),
            Event::mapping_start(
                Span::empty(7),
                CollectionStyle::Block,
                Properties::new(Some(5), None, None),
            ),
        ];
        for event in cases {
            let events = wrap(vec![event]);
            assert_eq!(
                tree_prefix(src, &events),
                Err(NotationError::Unreadable { index: 2 })
            );
        }
        assert_eq!(
            NotationError::Unreadable { index: 2 }.to_string(),
            "event 2: payload cannot be read from the source"
        );
    }

    #[test]
    fn non_semantic_events_are_not_printed() {
        let d = Diagnostic::warning(
            Code::new(1003).unwrap(),
            "w",
            Label::new(Span::empty(0), ""),
        );
        let mut events = wrap(vec![plain()]);
        events.insert(1, ev(EventKind::Trivia(TriviaKind::Comment)));
        events.insert(1, ev(EventKind::Warning(Box::new(d.clone()))));
        events.insert(1, ev(EventKind::Error(Box::new(d))));
        assert_eq!(
            tree(X, &events).unwrap(),
            "+STR\n+DOC\n=VAL :x\n-DOC\n-STR\n"
        );
    }

    #[test]
    fn prefix_allows_open_frames() {
        let events = [
            ev(EventKind::StreamStart),
            ev(EventKind::document_start(true)),
            ev(EventKind::sequence_start(CollectionStyle::Flow)),
        ];
        assert_eq!(
            tree_prefix(X, &events).unwrap(),
            "+STR\n+DOC ---\n+SEQ []\n"
        );
        assert_eq!(
            tree(X, &events),
            Err(NotationError::Unterminated { open: "SEQ" })
        );
        assert_eq!(tree_prefix(X, &[]).unwrap(), "");
    }

    #[test]
    fn mismatched_end_is_rejected() {
        let events = wrap(vec![
            ev(EventKind::sequence_start(CollectionStyle::Block)),
            ev(EventKind::MappingEnd),
        ]);
        let err = tree(X, &events).unwrap_err();
        assert_eq!(
            err,
            NotationError::Unexpected {
                index: 3,
                found: "-MAP",
                open: "SEQ"
            }
        );
        assert_eq!(
            err.to_string(),
            "event 3: unexpected -MAP while SEQ is open"
        );
    }

    #[test]
    fn node_outside_document_is_rejected() {
        let events = [ev(EventKind::StreamStart), plain()];
        assert!(matches!(
            tree(X, &events),
            Err(NotationError::Unexpected {
                found: "=VAL",
                open: "STR",
                ..
            })
        ));
        let alias = [ev(EventKind::StreamStart), ev(EventKind::Alias)];
        assert!(matches!(
            tree(X, &alias),
            Err(NotationError::Unexpected { found: "=ALI", .. })
        ));
        for kind in [
            EventKind::mapping_start(CollectionStyle::Block),
            EventKind::sequence_start(CollectionStyle::Block),
        ] {
            let events = [ev(EventKind::StreamStart), ev(kind)];
            assert!(matches!(
                tree(X, &events),
                Err(NotationError::Unexpected { open: "STR", .. })
            ));
        }
    }

    #[test]
    fn stream_order_errors() {
        let before_start = [ev(EventKind::document_start(false))];
        assert!(matches!(
            tree(X, &before_start),
            Err(NotationError::Unexpected {
                found: "+DOC",
                open: "none",
                ..
            })
        ));
        let twice = [
            ev(EventKind::StreamStart),
            ev(EventKind::StreamEnd),
            ev(EventKind::StreamStart),
        ];
        assert!(matches!(
            tree(X, &twice),
            Err(NotationError::Unexpected {
                found: "+STR",
                open: "-STR",
                ..
            })
        ));
        let end_inside_doc = [
            ev(EventKind::StreamStart),
            ev(EventKind::document_start(false)),
            ev(EventKind::StreamEnd),
        ];
        assert!(matches!(
            tree(X, &end_inside_doc),
            Err(NotationError::Unexpected { found: "-STR", .. })
        ));
        let doc_end_outside = [
            ev(EventKind::StreamStart),
            ev(EventKind::document_end(true)),
        ];
        assert!(matches!(
            tree(X, &doc_end_outside),
            Err(NotationError::Unexpected { found: "-DOC", .. })
        ));
        let seq_end_outside = [ev(EventKind::StreamStart), ev(EventKind::SequenceEnd)];
        assert!(matches!(
            tree(X, &seq_end_outside),
            Err(NotationError::Unexpected { found: "-SEQ", .. })
        ));
    }

    #[test]
    fn unterminated_stream_is_rejected() {
        let err = tree(X, &[ev(EventKind::StreamStart)]).unwrap_err();
        assert_eq!(err, NotationError::Unterminated { open: "STR" });
        assert_eq!(err.to_string(), "event stream ended while STR is open");
        assert_eq!(
            tree(X, &[]),
            Err(NotationError::Unterminated { open: "none" })
        );
        assert_eq!(
            NotationError::UnknownEvent { index: 2 }.to_string(),
            "event 2: unknown event kind"
        );
    }
}
