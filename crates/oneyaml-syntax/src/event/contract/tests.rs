#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

use oneyaml_core::{Code, Diagnostic, Label, Span};

use super::*;
use crate::event::{Directive, Properties};

/// Builds events left to right; spans are computed from the texts, so every
/// fixture tiles by construction unless a test breaks it on purpose.
struct B<'a> {
    src: &'a str,
    pos: usize,
    events: Vec<Event<'a>>,
}

fn b(src: &str) -> B<'_> {
    B {
        src,
        pos: 0,
        events: vec![Event::new(EventKind::StreamStart, Span::empty(0))],
    }
}

impl<'a> B<'a> {
    fn t(mut self, kind: EventKind, text: &str) -> Self {
        assert!(
            self.src[self.pos..].starts_with(text),
            "fixture text {text:?} not at {}",
            self.pos
        );
        let end = self.pos + text.len();
        self.events.push(Event::new(kind, Span::new(self.pos, end)));
        self.pos = end;
        self
    }
    /// A node event built for the span of `text` (its properties and
    /// header are packed relative to that span).
    fn e(mut self, make: impl FnOnce(Span) -> Event<'a>, text: &str) -> Self {
        let span = self.span_of(text);
        assert!(
            self.src[self.pos..].starts_with(text),
            "fixture text {text:?} not at {}",
            self.pos
        );
        self.events.push(make(span));
        self.pos = span.end;
        self
    }
    /// A zero-width node event built at the current position.
    fn ze(mut self, make: impl FnOnce(Span) -> Event<'a>) -> Self {
        self.events.push(make(Span::empty(self.pos)));
        self
    }
    fn z(mut self, kind: EventKind) -> Self {
        self.events.push(Event::new(kind, Span::empty(self.pos)));
        self
    }
    fn span_of(&self, text: &str) -> Span {
        Span::new(self.pos, self.pos + text.len())
    }
    fn tr(self, kind: TriviaKind, text: &str) -> Self {
        self.t(EventKind::Trivia(kind), text)
    }
    fn ws(self, text: &str) -> Self {
        self.tr(TriviaKind::Whitespace, text)
    }
    fn nl(self) -> Self {
        self.tr(TriviaKind::LineBreak, "\n")
    }
    fn ind(self, i: Indicator) -> Self {
        self.tr(TriviaKind::Indicator(i), i.as_str())
    }
    fn plain(self, text: &'a str) -> Self {
        self.t(EventKind::plain(), text)
    }
    fn empty(self) -> Self {
        self.z(EventKind::plain())
    }
    fn doc(self) -> Self {
        self.z(EventKind::document_start(false))
    }
    fn end_doc(self) -> Self {
        self.z(EventKind::document_end(false))
    }
    fn bmap(self) -> Self {
        self.z(EventKind::mapping_start(CollectionStyle::Block))
    }
    fn bseq(self) -> Self {
        self.z(EventKind::sequence_start(CollectionStyle::Block))
    }
    fn done(self) -> Vec<Event<'a>> {
        assert_eq!(
            self.pos,
            self.src.len(),
            "fixture does not cover the source"
        );
        let mut events = self.events;
        events.push(Event::new(EventKind::StreamEnd, Span::empty(self.pos)));
        events
    }
}

fn diag(severity: Severity) -> Box<Diagnostic> {
    Box::new(Diagnostic::new(
        Code::new(1).unwrap(),
        severity,
        "test",
        Label::new(Span::empty(0), ""),
    ))
}

fn err_at(src: &str, events: &[Event<'_>]) -> (usize, Violation) {
    let e = check_contract(src, events).unwrap_err();
    (e.index, e.violation)
}

// ------------------------------------------------------------ valid streams

const BLOCK: &str = "%YAML 1.2\n--- &m !!map\n# c\nkey: &a value\nseq:\n- *a\n- |+\n  lit\n\n...\n";

fn block_events() -> Vec<Event<'static>> {
    let s = BLOCK;
    let mut x = b(s)
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.2")
        .nl()
        .t(EventKind::document_start(true), "---")
        .ws(" ");
    let anchor_m = x.span_of("&m").start;
    x = x.tr(TriviaKind::Anchor, "&m").ws(" ");
    let tag = x.span_of("!!map").start;
    x = x
        .tr(TriviaKind::Tag, "!!map")
        .nl()
        .tr(TriviaKind::Comment, "# c")
        .nl()
        .ze(|s| {
            Event::mapping_start(
                s,
                CollectionStyle::Block,
                Properties::new(Some(anchor_m), Some(tag), None),
            )
        })
        .plain("key")
        .ind(Indicator::MappingValue)
        .ws(" ");
    let anchor_a = x.span_of("&a").start;
    x = x.tr(TriviaKind::Anchor, "&a").ws(" ").e(
        |s| {
            Event::scalar(
                s,
                ScalarStyle::Plain,
                Properties::new(Some(anchor_a), None, None),
                None,
            )
        },
        "value",
    );
    x = x
        .nl()
        .plain("seq")
        .ind(Indicator::MappingValue)
        .nl()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .ws(" ")
        .t(EventKind::Alias, "*a")
        .nl()
        .ind(Indicator::SequenceEntry)
        .ws(" ");
    let header = BlockHeader::new(Chomping::Keep, None, 2, x.span_of("|+"));
    x.tr(TriviaKind::BlockScalarHeader, "|+")
        .nl()
        .e(
            |s| Event::scalar(s, ScalarStyle::Literal, Properties::none(), Some(header)),
            "  lit\n\n",
        )
        .z(EventKind::SequenceEnd)
        .z(EventKind::MappingEnd)
        .t(EventKind::document_end(true), "...")
        .nl()
        .done()
}

const FLOW: &str = "[a: b, {c: d}, 'x''y', \"q\"]\n";

fn flow_events() -> Vec<Event<'static>> {
    b(FLOW)
        .doc()
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .z(EventKind::mapping_start(CollectionStyle::FlowPair))
        .plain("a")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .plain("b")
        .z(EventKind::MappingEnd)
        .ind(Indicator::FlowEntry)
        .ws(" ")
        .t(EventKind::mapping_start(CollectionStyle::Flow), "{")
        .plain("c")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .plain("d")
        .t(EventKind::MappingEnd, "}")
        .ind(Indicator::FlowEntry)
        .ws(" ")
        .e(
            |s| Event::scalar(s, ScalarStyle::SingleQuoted, Properties::none(), None),
            "'x''y'",
        )
        .ind(Indicator::FlowEntry)
        .ws(" ")
        .t(EventKind::scalar(ScalarStyle::DoubleQuoted), "\"q\"")
        .t(EventKind::SequenceEnd, "]")
        .end_doc()
        .nl()
        .done()
}

const RECOVERY: &str = "[a\n}x\n";

fn recovery_events() -> Vec<Event<'static>> {
    b(RECOVERY)
        .doc()
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .plain("a")
        .nl()
        .t(EventKind::Error(diag(Severity::Error)), "}")
        .tr(TriviaKind::Skipped, "x")
        .nl()
        .z(EventKind::SequenceEnd)
        .end_doc()
        .done()
}

const DIRECTIVES: &str = "%FOO  bar baz # c\n%TAG !e! tag:e,2000:\n--- \"foo\"\n...\n...\n";

fn directive_events() -> Vec<Event<'static>> {
    b(DIRECTIVES)
        .t(EventKind::Directive(Directive::Reserved), "%FOO  bar baz")
        .z(EventKind::Warning(diag(Severity::Warning)))
        .ws(" ")
        .tr(TriviaKind::Comment, "# c")
        .nl()
        .t(EventKind::Directive(Directive::Tag), "%TAG !e! tag:e,2000:")
        .nl()
        .t(EventKind::document_start(true), "---")
        .ws(" ")
        .t(EventKind::scalar(ScalarStyle::DoubleQuoted), "\"foo\"")
        .nl()
        .t(EventKind::document_end(true), "...")
        .nl()
        .tr(TriviaKind::DocumentEndMarker, "...")
        .nl()
        .done()
}

const MULTI: &str = "\u{feff}a\n...\n- \n";

fn multi_events() -> Vec<Event<'static>> {
    b(MULTI)
        .tr(TriviaKind::ByteOrderMark, "\u{feff}")
        .doc()
        .plain("a")
        .nl()
        .t(EventKind::document_end(true), "...")
        .nl()
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .empty()
        .z(EventKind::SequenceEnd)
        .end_doc()
        .ws(" ")
        .nl()
        .done()
}

#[test]
fn valid_fixtures_pass() {
    for (src, events) in [
        (BLOCK, block_events()),
        (FLOW, flow_events()),
        (RECOVERY, recovery_events()),
        (DIRECTIVES, directive_events()),
        (MULTI, multi_events()),
    ] {
        check_contract(src, &events).unwrap_or_else(|e| panic!("{src:?}: {e}"));
        let rebuilt: String = events.iter().map(|e| e.source(src).unwrap()).collect();
        assert_eq!(rebuilt.as_bytes(), src.as_bytes());
    }
}

#[test]
fn verbatim_non_specific_and_crlf() {
    let src = "!<x> a\r\n";
    let mut x = b(src).doc();
    let tag = x.span_of("!<x>").start;
    x = x.tr(TriviaKind::Tag, "!<x>").ws(" ").e(
        |s| {
            Event::scalar(
                s,
                ScalarStyle::Plain,
                Properties::new(None, Some(tag), None),
                None,
            )
        },
        "a",
    );
    let events = x.end_doc().tr(TriviaKind::LineBreak, "\r\n").done();
    check_contract(src, &events).unwrap();

    let src = "! a\n";
    let mut x = b(src).doc();
    let tag = x.span_of("!").start;
    x = x.tr(TriviaKind::Tag, "!").ws(" ").e(
        |s| {
            Event::scalar(
                s,
                ScalarStyle::Plain,
                Properties::new(None, Some(tag), None),
                None,
            )
        },
        "a",
    );
    check_contract(src, &x.end_doc().nl().done()).unwrap();
}

#[test]
fn empty_block_scalar_at_eof_and_strip() {
    let src = "--- |-";
    let mut x = b(src).t(EventKind::document_start(true), "---").ws(" ");
    let header = BlockHeader::new(Chomping::Strip, None, 0, x.span_of("|-"));
    x = x
        .tr(TriviaKind::BlockScalarHeader, "|-")
        .ze(|s| Event::scalar(s, ScalarStyle::Literal, Properties::none(), Some(header)));
    check_contract(src, &x.end_doc().done()).unwrap();

    let src = "--- >2-\n   a\n  b\n\n";
    let mut x = b(src).t(EventKind::document_start(true), "---").ws(" ");
    let header = BlockHeader::new(Chomping::Strip, Some(2), 2, x.span_of(">2-"));
    x = x.tr(TriviaKind::BlockScalarHeader, ">2-").nl().e(
        |s| Event::scalar(s, ScalarStyle::Folded, Properties::none(), Some(header)),
        "   a\n  b\n",
    );
    check_contract(src, &x.end_doc().nl().done()).unwrap();
}

// ------------------------------------------------------------ C1

#[test]
fn tiling_violations() {
    let src = FLOW;
    let mut ev = flow_events();
    ev.remove(5); // `:` indicator
    assert_eq!(
        err_at(src, &ev),
        (
            5,
            Violation::Discontiguous {
                expected: 2,
                found: 3
            }
        )
    );

    let mut ev = flow_events();
    ev.truncate(ev.len() - 2);
    assert!(matches!(err_at(src, &ev).1, Violation::Incomplete { .. }));

    let ev = [Event::new(EventKind::plain(), Span::new(0, 1))];
    assert!(matches!(
        err_at("é", &ev),
        (0, Violation::InvalidSpan { .. })
    ));

    let ev = [Event::new(
        EventKind::Trivia(TriviaKind::Whitespace),
        Span::empty(0),
    )];
    assert_eq!(err_at("", &ev), (0, Violation::EmptyTrivia));
    assert!(check_tiling("", &[]).is_ok());
}

// ------------------------------------------------------------ C2

#[test]
fn bytes_must_match_kind() {
    let src = "[a}";
    let ev = b(src)
        .doc()
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .plain("a")
        .t(EventKind::SequenceEnd, "}")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (4, Violation::BadBytes { expected: "]" }));

    let mut ev = flow_events();
    ev[2].kind = EventKind::mapping_start(CollectionStyle::Flow);
    assert_eq!(
        err_at(FLOW, &ev),
        (2, Violation::BadBytes { expected: "{" })
    );

    let src = "a b\n";
    let ev = b(src)
        .doc()
        .plain("a")
        .tr(TriviaKind::Whitespace, " b")
        .end_doc()
        .nl()
        .done();
    assert_eq!(
        err_at(src, &ev).1,
        Violation::BadBytes {
            expected: "spaces and tabs"
        }
    );

    let src = "a#c\n";
    let ev = b(src)
        .doc()
        .plain("a")
        .tr(TriviaKind::Comment, "#c")
        .end_doc()
        .nl()
        .done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    let src = " a\n";
    let ev = b(src).ws(" ").doc().plain("a").end_doc().nl().done();
    check_contract(src, &ev).unwrap();
    let ev = b(src)
        .doc()
        .t(EventKind::plain(), " a")
        .end_doc()
        .nl()
        .done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    let src = "'a\n";
    let ev = b(src)
        .doc()
        .t(EventKind::scalar(ScalarStyle::SingleQuoted), "'a")
        .end_doc()
        .nl()
        .done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    let src = "\"a\\\"\n";
    let ev = b(src)
        .doc()
        .t(EventKind::scalar(ScalarStyle::DoubleQuoted), "\"a\\\"")
        .end_doc()
        .nl()
        .done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    let src = "*\n";
    let ev = b(src).doc().t(EventKind::Alias, "*").end_doc().nl().done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    let src = "-\n";
    let ev = b(src)
        .doc()
        .bseq()
        .tr(TriviaKind::Indicator(Indicator::SequenceEntry), "-")
        .empty()
        .z(EventKind::SequenceEnd)
        .end_doc()
        .nl()
        .done();
    check_contract(src, &ev).unwrap();
    let mut bad = ev.clone();
    bad[3].kind = EventKind::Trivia(TriviaKind::Indicator(Indicator::MappingKey));
    assert!(matches!(err_at(src, &bad).1, Violation::BadBytes { .. }));

    let src = "\r";
    let ev = b(src).tr(TriviaKind::LineBreak, "\r").done();
    check_contract(src, &ev).unwrap();
    let src = "\r\n";
    let ev = b(src)
        .tr(TriviaKind::LineBreak, "\r")
        .tr(TriviaKind::LineBreak, "\n")
        .done();
    assert!(matches!(err_at(src, &ev), (1, Violation::BadBytes { .. })));

    let src = "x";
    let ev = b(src).tr(TriviaKind::ByteOrderMark, "x").done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));
    let ev = b(src).tr(TriviaKind::DocumentEndMarker, "x").done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));
    let ev = b(src)
        .t(EventKind::document_start(true), "x")
        .empty()
        .end_doc()
        .done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    let src = "a\n";
    let ev = b(src)
        .doc()
        .plain("a")
        .end_doc()
        .z(EventKind::Warning(diag(Severity::Warning)))
        .nl()
        .done();
    check_contract(src, &ev).unwrap();
    // Stream start and warnings must be zero-width.
    let ev2 = [
        Event::new(EventKind::StreamStart, Span::new(0, 1)),
        Event::new(EventKind::StreamEnd, Span::empty(1)),
    ];
    assert!(matches!(err_at("a", &ev2), (0, Violation::BadBytes { .. })));
    let src = "a";
    let ev3 = [
        Event::new(EventKind::StreamStart, Span::empty(0)),
        Event::new(EventKind::Warning(diag(Severity::Warning)), Span::new(0, 1)),
        Event::new(EventKind::StreamEnd, Span::empty(1)),
    ];
    assert!(matches!(err_at(src, &ev3), (1, Violation::BadBytes { .. })));
}

#[test]
fn line_breaks_only_in_breaks_and_scalars() {
    let src = "#\n";
    let ev = b(src).tr(TriviaKind::Comment, "#\n").done();
    assert_eq!(err_at(src, &ev), (1, Violation::LineBreak));
    let src = "a\nb\n";
    let ev = b(src)
        .doc()
        .t(EventKind::scalar(ScalarStyle::Plain), "a\nb")
        .end_doc()
        .nl()
        .done();
    check_contract(src, &ev).unwrap();
}

#[test]
fn payloads_are_read_from_the_bytes() {
    // Values, names, and directive parameters are read from the bytes (module
    // docs, *Representation*), so they cannot disagree with them. What can
    // go wrong is bytes that are not what the event says they are.

    // A scalar whose bytes are not one complete scalar of its style.
    for (src, style, text) in [
        ("\"a\\q\"\n", ScalarStyle::DoubleQuoted, "\"a\\q\""),
        ("'a'b'\n", ScalarStyle::SingleQuoted, "'a'b'"),
        ("\"a\"b\"\n", ScalarStyle::DoubleQuoted, "\"a\"b\""),
    ] {
        let ev = b(src)
            .doc()
            .t(EventKind::scalar(style), text)
            .end_doc()
            .nl()
            .done();
        assert_eq!(
            err_at(src, &ev),
            (
                2,
                Violation::ValueMismatch {
                    detail: "scalar value"
                }
            ),
            "{src:?}"
        );
    }
    // Values decode from the fixture's bytes.
    let ev = flow_events();
    assert_eq!(ev[19].value(FLOW).unwrap(), "x'y");
    assert_eq!(ev[4].value(FLOW).unwrap(), "a");

    // The directive kind must be the directive the bytes are.
    let mismatch = Violation::ValueMismatch {
        detail: "directive payload",
    };
    for which in [Directive::Tag, Directive::Reserved] {
        let mut ev = block_events();
        ev[1] = Event::new(EventKind::Directive(which), ev[1].span());
        assert_eq!(err_at(BLOCK, &ev), (1, mismatch.clone()));
    }
    let mut ev = directive_events();
    ev[1] = Event::new(EventKind::Directive(Directive::Yaml), ev[1].span());
    assert_eq!(err_at(DIRECTIVES, &ev), (1, mismatch.clone()));
    let mut ev = directive_events();
    ev[6] = Event::new(EventKind::Directive(Directive::Reserved), ev[6].span());
    assert_eq!(err_at(DIRECTIVES, &ev), (6, mismatch.clone()));
    // Bytes that are no valid directive.
    let src = "%YAML 1.x\n--- a\n";
    let ev = b(src)
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.x")
        .nl()
        .t(EventKind::document_start(true), "---")
        .ws(" ")
        .plain("a")
        .end_doc()
        .nl()
        .done();
    assert_eq!(err_at(src, &ev), (1, mismatch));

    // Property positions must point at the property trivia: an anchor
    // position at the tag reads no anchor.
    let mut ev = block_events();
    let (span, props) = (ev[11].span(), ev[11].properties().unwrap());
    let moved = Properties::new(props.tag, props.tag, None);
    ev[11] = Event::mapping_start(span, CollectionStyle::Block, moved);
    assert_eq!(err_at(BLOCK, &ev), (11, Violation::PropertyMismatch));
    // A `%TAG` prefix without a tag.
    let src = "a";
    let ev = b(src)
        .doc()
        .e(
            |s| {
                Event::scalar(
                    s,
                    ScalarStyle::Plain,
                    Properties::new(None, None, Some(0)),
                    None,
                )
            },
            "a",
        )
        .end_doc()
        .done();
    assert_eq!(
        err_at(src, &ev),
        (2, Violation::ValueMismatch { detail: "tag" })
    );
    // A named handle whose prefix position is not a prefix reads no tag.
    let src = "!e!foo a";
    let ev = |prefix: Option<usize>| {
        let x = b(src).doc();
        let tag = x.pos;
        x.tr(TriviaKind::Tag, "!e!foo")
            .ws(" ")
            .e(
                |s| {
                    Event::scalar(
                        s,
                        ScalarStyle::Plain,
                        Properties::new(None, Some(tag), prefix),
                        None,
                    )
                },
                "a",
            )
            .end_doc()
            .done()
    };
    // (`foo` stands in for a `%TAG` prefix: which directive declared it is
    // not machine-checked.)
    check_contract(src, &ev(Some(3))).unwrap();
    assert_eq!(ev(Some(3))[4].tag(src).unwrap().resolved, "foofoo");
    assert_eq!(err_at(src, &ev(None)), (4, Violation::PropertyMismatch));
    assert_eq!(err_at(src, &ev(Some(6))), (4, Violation::PropertyMismatch));
}

// ------------------------------------------------------------ C3

#[test]
fn structure_violations() {
    // Two nodes in one document.
    let src = "a b";
    let ev = b(src).doc().plain("a").ws(" ").plain("b").end_doc().done();
    assert_eq!(
        err_at(src, &ev),
        (
            4,
            Violation::ChildCount {
                container: "DOC",
                count: 2
            }
        )
    );
    // Empty document.
    let ev = [
        Event::new(EventKind::StreamStart, Span::empty(0)),
        Event::new(EventKind::document_start(true), Span::new(0, 3)),
        Event::new(EventKind::document_end(false), Span::empty(3)),
        Event::new(EventKind::StreamEnd, Span::empty(3)),
    ];
    assert_eq!(
        err_at("---", &ev),
        (
            2,
            Violation::ChildCount {
                container: "DOC",
                count: 0
            }
        )
    );
    // Odd mapping.
    let src = "a";
    let ev = b(src)
        .doc()
        .bmap()
        .plain("a")
        .z(EventKind::MappingEnd)
        .end_doc()
        .done();
    assert_eq!(
        err_at(src, &ev),
        (
            4,
            Violation::ChildCount {
                container: "MAP",
                count: 1
            }
        )
    );
    // Flow pair with three nodes; flow pair outside a flow sequence.
    let mut ev = flow_events();
    ev.insert(8, Event::new(EventKind::plain(), Span::empty(5)));
    assert_eq!(
        err_at(FLOW, &ev),
        (
            8,
            Violation::ChildCount {
                container: "PAIR",
                count: 3
            }
        )
    );
    let src = "a";
    let ev = b(src)
        .doc()
        .z(EventKind::mapping_start(CollectionStyle::FlowPair))
        .plain("a")
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (
            2,
            Violation::Misplaced {
                found: "+MAP (flow pair)",
                ..
            }
        )
    ));
    let ev = b(src)
        .doc()
        .z(EventKind::sequence_start(CollectionStyle::FlowPair))
        .plain("a")
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (
            2,
            Violation::Misplaced {
                found: "+SEQ (flow pair)",
                ..
            }
        )
    ));

    // Node outside a document, ends that do not match.
    let ev = b(src).plain("a").done();
    assert!(matches!(
        err_at(src, &ev),
        (
            1,
            Violation::Misplaced {
                found: "=VAL",
                open: "STR"
            }
        )
    ));
    let mut ev = flow_events();
    ev[23].kind = EventKind::MappingEnd;
    assert!(matches!(
        err_at(FLOW, &ev),
        (
            23,
            Violation::Misplaced {
                found: "-MAP",
                open: "SEQ"
            }
        )
    ));
    let ev = [
        Event::new(EventKind::StreamStart, Span::empty(0)),
        Event::new(EventKind::document_end(false), Span::empty(0)),
    ];
    assert!(matches!(
        err_at("", &ev),
        (1, Violation::Misplaced { found: "-DOC", .. })
    ));
    let ev = [
        Event::new(EventKind::StreamStart, Span::empty(0)),
        Event::new(EventKind::StreamStart, Span::empty(0)),
    ];
    assert!(matches!(
        err_at("", &ev),
        (1, Violation::Misplaced { found: "+STR", .. })
    ));
    let src = "-\u{feff}";
    let ev = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .tr(TriviaKind::ByteOrderMark, "\u{feff}")
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (4, Violation::Misplaced { found: "BOM", .. })
    ));

    // Directive / document start / document-end marker placement.
    let src = "a\n%X";
    let ev = b(src)
        .doc()
        .plain("a")
        .nl()
        .t(EventKind::Directive(Directive::Reserved), "%X")
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (4, Violation::Misplaced { found: "%", .. })
    ));
    let src = "---";
    let ev = b(src)
        .doc()
        .t(EventKind::document_start(true), "---")
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (2, Violation::Misplaced { found: "+DOC", .. })
    ));
    let src = "- ...";
    let ev = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .ws(" ")
        .tr(TriviaKind::DocumentEndMarker, "...")
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (5, Violation::Misplaced { found: "...", .. })
    ));

    // Indicators in the wrong collection.
    let src = "-";
    let ev = b(src).doc().bmap().ind(Indicator::SequenceEntry).done();
    assert!(matches!(
        err_at(src, &ev),
        (3, Violation::Misplaced { found: "-", .. })
    ));
    let src = ",";
    let ev = b(src).doc().bseq().ind(Indicator::FlowEntry).done();
    assert!(matches!(
        err_at(src, &ev),
        (3, Violation::Misplaced { found: ",", .. })
    ));
    let src = ":";
    let ev = b(src).doc().bseq().ind(Indicator::MappingValue).done();
    assert!(matches!(
        err_at(src, &ev),
        (3, Violation::Misplaced { found: ":", .. })
    ));

    // Properties at stream level.
    let src = "&a";
    let ev = b(src).tr(TriviaKind::Anchor, "&a").done();
    assert!(matches!(
        err_at(src, &ev),
        (1, Violation::Misplaced { found: "&", .. })
    ));
    let ev = b("&a").doc().tr(TriviaKind::Tag, "&a").done();
    assert!(matches!(err_at("&a", &ev).1, Violation::BadBytes { .. }));
}

#[test]
fn stream_boundaries() {
    assert_eq!(err_at("", &[]), (0, Violation::MissingStreamStart));
    let ev = [Event::new(EventKind::StreamEnd, Span::empty(0))];
    assert_eq!(err_at("", &ev), (0, Violation::MissingStreamStart));
    let ev = [Event::new(EventKind::StreamStart, Span::empty(0))];
    assert_eq!(err_at("", &ev), (1, Violation::MissingStreamEnd));
    let mut ev = flow_events();
    ev.push(Event::new(EventKind::StreamEnd, Span::empty(FLOW.len())));
    assert_eq!(err_at(FLOW, &ev), (ev.len() - 1, Violation::AfterStreamEnd));
    let mut ev = flow_events();
    let n = ev.len();
    ev.remove(n - 3); // implicit document end
    assert_eq!(
        err_at(FLOW, &ev),
        (n - 2, Violation::Unclosed { open: "DOC" })
    );
}

#[test]
fn property_rules() {
    // Node claims an anchor with no trivia.
    let src = "a";
    let ev = b(src)
        .doc()
        .e(
            |s| {
                let props = Properties::new(Some(0), None, None);
                Event::scalar(s, ScalarStyle::Plain, props, None)
            },
            "a",
        )
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (2, Violation::PropertyMismatch));
    // Anchor trivia with a node that ignores it.
    let src = "&a b";
    let ev = b(src)
        .doc()
        .tr(TriviaKind::Anchor, "&a")
        .ws(" ")
        .plain("b")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (4, Violation::PropertyMismatch));
    // Anchor on an alias.
    let ev = b("&a *b")
        .doc()
        .tr(TriviaKind::Anchor, "&a")
        .ws(" ")
        .t(EventKind::Alias, "*b")
        .done();
    assert_eq!(err_at("&a *b", &ev), (4, Violation::PropertyMismatch));
    // Dangling and duplicate.
    let src = "- &a\n";
    let ev = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .ws(" ")
        .tr(TriviaKind::Anchor, "&a")
        .z(EventKind::SequenceEnd)
        .nl()
        .done();
    assert_eq!(err_at(src, &ev), (6, Violation::DanglingProperty));
    let src = "&a &b c";
    let ev = b(src)
        .doc()
        .tr(TriviaKind::Anchor, "&a")
        .ws(" ")
        .tr(TriviaKind::Anchor, "&b")
        .ws(" ")
        .plain("c")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (4, Violation::DuplicateProperty));
}

#[test]
fn document_rules() {
    let src = "%YAML 1.2\na";
    let ev = b(src)
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.2")
        .nl()
        .doc()
        .plain("a")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (3, Violation::DirectiveWithoutDocument));
    let src = "%YAML 1.2\n";
    let ev = b(src)
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.2")
        .nl()
        .done();
    assert_eq!(err_at(src, &ev), (3, Violation::DirectiveWithoutDocument));
    // ...but with an error it is a legitimate recovery stream (9MMA).
    let ev = b(src)
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.2")
        .nl()
        .z(EventKind::Error(diag(Severity::Error)))
        .done();
    check_contract(src, &ev).unwrap();

    let src = "a\n";
    let ev = b(src)
        .doc()
        .plain("a")
        .end_doc()
        .nl()
        .doc()
        .empty()
        .end_doc()
        .done();
    assert!(matches!(
        err_at(src, &ev),
        (
            5,
            Violation::ImplicitDocumentStart | Violation::StartNotLate
        )
    ));
    let ev = b("a\n...")
        .doc()
        .plain("a")
        .end_doc()
        .nl()
        .tr(TriviaKind::DocumentEndMarker, "...")
        .doc()
        .empty()
        .end_doc()
        .done();
    // After an implicitly ended document, `...` must be its explicit end.
    assert!(matches!(
        err_at("a\n...", &ev),
        (5, Violation::Misplaced { found: "...", .. })
    ));
    let ev = b("ab")
        .doc()
        .plain("a")
        .end_doc()
        .doc()
        .plain("b")
        .end_doc()
        .done();
    assert_eq!(err_at("ab", &ev), (4, Violation::ImplicitDocumentStart));
}

// ------------------------------------------------------------ C4

#[test]
fn placement_rules() {
    let src = " a";
    let ev = b(src).doc().ws(" ").plain("a").end_doc().done();
    assert_eq!(err_at(src, &ev), (1, Violation::StartNotLate));
    let src = "a:";
    let ev = b(src)
        .doc()
        .bmap()
        .plain("a")
        .ind(Indicator::MappingValue)
        .empty()
        .z(EventKind::MappingEnd)
        .end_doc()
        .done();
    check_contract(src, &ev).unwrap();
    let ev = [
        Event::new(EventKind::StreamStart, Span::empty(0)),
        Event::new(EventKind::document_start(false), Span::empty(0)),
    ];
    assert_eq!(err_at("", &ev), (1, Violation::StartNotLate));

    let src = "a\n";
    let ev = b(src).doc().plain("a").nl().end_doc().done();
    assert_eq!(err_at(src, &ev), (4, Violation::EndNotEarly));
    let src = "- a\n";
    let ev = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .ws(" ")
        .plain("a")
        .nl()
        .z(EventKind::SequenceEnd)
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (7, Violation::EndNotEarly));
    // Warnings are transparent to placement.
    let ev = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .ws(" ")
        .plain("a")
        .z(EventKind::Warning(diag(Severity::Warning)))
        .z(EventKind::SequenceEnd)
        .end_doc()
        .nl()
        .done();
    check_contract(src, &ev).unwrap();
}

// ------------------------------------------------------------ C5

fn literal<'a>(x: B<'a>, header: &str, chomping: Chomping, indent: usize, body: &str) -> B<'a> {
    let h = BlockHeader::new(chomping, None, indent, x.span_of(header));
    x.tr(TriviaKind::BlockScalarHeader, header).nl().e(
        |s| Event::scalar(s, ScalarStyle::Literal, Properties::none(), Some(h)),
        body,
    )
}

#[test]
fn block_scalar_rules() {
    let src = "|\n a\n\n";
    let ok = literal(b(src).doc(), "|", Chomping::Clip, 1, " a\n")
        .end_doc()
        .nl()
        .done();
    check_contract(src, &ok).unwrap();

    // Clip span swallowing a trailing empty line.
    let bad = literal(b(src).doc(), "|", Chomping::Clip, 1, " a\n\n")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &bad), (4, Violation::BlockScalarPlacement));
    // Content less indented than declared.
    let bad = literal(b(src).doc(), "|", Chomping::Clip, 2, " a\n")
        .end_doc()
        .nl()
        .done();
    assert_eq!(err_at(src, &bad), (4, Violation::BlockScalarPlacement));
    // Values follow the chomping: they are decoded from the span.
    assert_eq!(ok[4].value(src).unwrap(), "a\n");
    let keep = "|+\n a\n\n";
    let ev = literal(b(keep).doc(), "|+", Chomping::Keep, 1, " a\n\n")
        .end_doc()
        .done();
    check_contract(keep, &ev).unwrap();
    assert_eq!(ev[4].value(keep).unwrap(), "a\n\n");
    let src2 = "|-\n a\n";
    let ev = literal(b(src2).doc(), "|-", Chomping::Strip, 1, " a\n")
        .end_doc()
        .done();
    check_contract(src2, &ev).unwrap();
    assert_eq!(ev[4].value(src2).unwrap(), "a");
    // A document marker at indentation 0 is not content (C5): a span
    // holding one is not a block scalar's content, and does not decode.
    let src6 = "|\na\n--- b\n";
    let bad = literal(b(src6).doc(), "|", Chomping::Clip, 0, "a\n--- b\n")
        .end_doc()
        .done();
    assert_eq!(
        err_at(src6, &bad),
        (
            4,
            Violation::ValueMismatch {
                detail: "block scalar value"
            }
        )
    );
    // Header payload disagrees with header text.
    let bad = literal(b(src).doc(), "|", Chomping::Keep, 1, " a\n")
        .end_doc()
        .nl()
        .done();
    assert_eq!(err_at(src, &bad), (4, Violation::BlockHeaderMismatch));
    let src3 = "|x\n a\n";
    let bad = literal(b(src3).doc(), "|x", Chomping::Clip, 1, " a\n")
        .end_doc()
        .done();
    assert_eq!(err_at(src3, &bad), (4, Violation::BlockHeaderMismatch));
    // Block style without header payload / plain with header payload.
    let mut bad = ok.clone();
    let (span, header) = (bad[4].span(), bad[4].block_header());
    bad[4] = Event::scalar(span, ScalarStyle::Literal, Properties::none(), None);
    assert_eq!(err_at(src, &bad), (4, Violation::BlockHeaderMismatch));
    let mut bad = ok.clone();
    bad[4] = Event::scalar(span, ScalarStyle::Plain, Properties::none(), header);
    assert_eq!(err_at(src, &bad), (4, Violation::DanglingBlockHeader));
    let src4 = "a";
    let bad = b(src4)
        .doc()
        .e(
            |s| {
                let h = BlockHeader::new(Chomping::Clip, None, 0, Span::empty(0));
                Event::scalar(s, ScalarStyle::Plain, Properties::none(), Some(h))
            },
            "a",
        )
        .end_doc()
        .done();
    assert_eq!(err_at(src4, &bad), (2, Violation::BlockHeaderMismatch));
    // Header then something other than whitespace/comment/break.
    let src5 = "| &a";
    let bad = b(src5)
        .doc()
        .tr(TriviaKind::BlockScalarHeader, "|")
        .ws(" ")
        .tr(TriviaKind::Anchor, "&a")
        .done();
    assert_eq!(err_at(src5, &bad), (4, Violation::DanglingBlockHeader));
    let src6 = "|";
    let bad = b(src6)
        .doc()
        .tr(TriviaKind::BlockScalarHeader, "|")
        .end_doc()
        .done();
    assert_eq!(err_at(src6, &bad), (3, Violation::DanglingBlockHeader));
    // Scalar not right after the header line's break.
    let src7 = "|\n\n a\n";
    let bad = {
        let x = b(src7).doc();
        let h = BlockHeader::new(Chomping::Clip, None, 1, x.span_of("|"));
        x.tr(TriviaKind::BlockScalarHeader, "|")
            .nl()
            .nl()
            .e(
                |s| Event::scalar(s, ScalarStyle::Literal, Properties::none(), Some(h)),
                " a\n",
            )
            .end_doc()
            .done()
    };
    assert_eq!(err_at(src7, &bad), (5, Violation::BlockScalarPlacement));
    // Span ending mid-line.
    let src8 = "|\n a b";
    let bad = {
        let x = b(src8).doc();
        let h = BlockHeader::new(Chomping::Clip, None, 1, x.span_of("|"));
        x.tr(TriviaKind::BlockScalarHeader, "|")
            .nl()
            .e(
                |s| Event::scalar(s, ScalarStyle::Literal, Properties::none(), Some(h)),
                " a",
            )
            .ws(" ")
            .tr(TriviaKind::Skipped, "b")
            .done()
    };
    assert_eq!(err_at(src8, &bad), (4, Violation::BlockScalarPlacement));
}

// ------------------------------------------------------------ C6

#[test]
fn diagnostics_and_recovery_rules() {
    let src = "a";
    let ev = b(src)
        .doc()
        .plain("a")
        .end_doc()
        .z(EventKind::Warning(diag(Severity::Error)))
        .done();
    assert_eq!(err_at(src, &ev), (4, Violation::SeverityMismatch));
    let ev = b(src)
        .doc()
        .plain("a")
        .end_doc()
        .z(EventKind::Error(diag(Severity::Warning)))
        .done();
    assert_eq!(err_at(src, &ev), (4, Violation::SeverityMismatch));

    let src = "a x";
    let ev = b(src)
        .doc()
        .plain("a")
        .end_doc()
        .ws(" ")
        .tr(TriviaKind::Skipped, "x")
        .done();
    assert_eq!(err_at(src, &ev), (5, Violation::SkippedWithoutError));

    let src = "[a";
    let ev = b(src)
        .doc()
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .plain("a")
        .z(EventKind::SequenceEnd)
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (4, Violation::ZeroWidthFlowEnd));

    // Error spans never contain line breaks (R4): the "always error" stream.
    let src = "a\nb\n";
    let ev = b(src)
        .t(EventKind::Error(diag(Severity::Error)), "a\nb\n")
        .done();
    assert_eq!(err_at(src, &ev), (1, Violation::LineBreak));

    // Recovery relaxes "ends early" and permits a synthesized flow end.
    check_contract(RECOVERY, &recovery_events()).unwrap();
    let mut ev = recovery_events();
    ev[5].kind = EventKind::Trivia(TriviaKind::Skipped);
    // Without an error, skipped bytes are illegal.
    assert_eq!(err_at(RECOVERY, &ev), (5, Violation::SkippedWithoutError));
}

#[test]
fn display_texts() {
    let all = [
        (
            Violation::Discontiguous {
                expected: 1,
                found: 2,
            },
            "starts at byte 2, expected 1",
        ),
        (
            Violation::InvalidSpan {
                span: Span::new(3, 1),
            },
            "invalid span 3..1",
        ),
        (
            Violation::Incomplete { covered: 1, len: 2 },
            "events cover 1 of 2 source bytes",
        ),
        (Violation::EmptyTrivia, "zero-width trivia"),
        (
            Violation::BadBytes { expected: "`---`" },
            "bytes are not `---`",
        ),
        (
            Violation::LineBreak,
            "line break outside LineBreak trivia or a scalar",
        ),
        (
            Violation::ValueMismatch { detail: "tag" },
            "payload disagrees with bytes: tag",
        ),
        (
            Violation::Misplaced {
                found: "-MAP",
                open: "SEQ",
            },
            "unexpected -MAP while SEQ is open",
        ),
        (
            Violation::ChildCount {
                container: "DOC",
                count: 2,
            },
            "DOC has 2 nodes",
        ),
        (
            Violation::MissingStreamStart,
            "stream does not begin with StreamStart",
        ),
        (
            Violation::MissingStreamEnd,
            "stream does not end with StreamEnd",
        ),
        (Violation::AfterStreamEnd, "event after StreamEnd"),
        (
            Violation::Unclosed { open: "DOC" },
            "stream ended while DOC is open",
        ),
        (
            Violation::PropertyMismatch,
            "node properties do not match property trivia",
        ),
        (
            Violation::DanglingProperty,
            "property trivia without a node",
        ),
        (Violation::DuplicateProperty, "duplicate property"),
        (
            Violation::BlockHeaderMismatch,
            "block header payload disagrees with header",
        ),
        (
            Violation::DanglingBlockHeader,
            "block header without its block scalar",
        ),
        (
            Violation::BlockScalarPlacement,
            "block scalar span misplaced",
        ),
        (
            Violation::StartNotLate,
            "zero-width start is followed by trivia",
        ),
        (
            Violation::EndNotEarly,
            "zero-width end is preceded by trivia",
        ),
        (
            Violation::DirectiveWithoutDocument,
            "directive without explicit document",
        ),
        (
            Violation::ImplicitDocumentStart,
            "implicit document start after a document without `...`",
        ),
        (
            Violation::SeverityMismatch,
            "diagnostic severity does not match event",
        ),
        (
            Violation::SkippedWithoutError,
            "skipped bytes before any error",
        ),
        (
            Violation::ZeroWidthFlowEnd,
            "zero-width flow end without an error",
        ),
    ];
    for (v, text) in all {
        assert_eq!(v.to_string(), text);
    }
    let e = ContractError {
        index: 3,
        violation: Violation::EmptyTrivia,
    };
    assert_eq!(e.to_string(), "event 3: zero-width trivia");
}

// ------------------------------------------------------------ C4 empty nodes

fn anchored<'a>(x: B<'a>, name: &'a str) -> (B<'a>, Properties) {
    let text_len = name.len() + 1;
    let props = Properties::new(Some(x.pos), None, None);
    let text = &x.src[x.pos..x.pos + text_len];
    (x.t(EventKind::Trivia(TriviaKind::Anchor), text), props)
}

fn empty_with(x: B<'_>, props: Properties) -> B<'_> {
    x.ze(|s| Event::scalar(s, ScalarStyle::Plain, props, None))
}

/// Asserts that `good` passes and `bad` fails with `EmptyNodePlacement`.
fn one_placement(src: &str, good: &[Event<'_>], bad: &[Event<'_>]) {
    check_contract(src, good).unwrap_or_else(|e| panic!("{src:?} good: {e}"));
    let e = check_contract(src, bad).unwrap_err();
    assert_eq!(
        e.violation,
        Violation::EmptyNodePlacement,
        "{src:?} bad: {e}"
    );
}

#[test]
fn empty_node_after_last_property() {
    let src = "a: &x \n";
    let pre = || {
        b(src)
            .doc()
            .bmap()
            .plain("a")
            .ind(Indicator::MappingValue)
            .ws(" ")
    };
    let (x, p) = anchored(pre(), "x");
    let good = empty_with(x, p)
        .z(EventKind::MappingEnd)
        .end_doc()
        .ws(" ")
        .nl()
        .done();
    let (x, p) = anchored(pre(), "x");
    let bad = empty_with(x.ws(" "), p)
        .z(EventKind::MappingEnd)
        .end_doc()
        .nl()
        .done();
    one_placement(src, &good, &bad);

    let src = "a: !!null\n";
    let x = b(src)
        .doc()
        .bmap()
        .plain("a")
        .ind(Indicator::MappingValue)
        .ws(" ");
    let tag = x.span_of("!!null").start;
    let good = x
        .tr(TriviaKind::Tag, "!!null")
        .ze(|s| {
            Event::scalar(
                s,
                ScalarStyle::Plain,
                Properties::new(None, Some(tag), None),
                None,
            )
        })
        .z(EventKind::MappingEnd)
        .end_doc()
        .nl()
        .done();
    check_contract(src, &good).unwrap();
}

#[test]
fn empty_node_after_introducing_indicator() {
    let src = "-   # c\n";
    let good = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .empty()
        .z(EventKind::SequenceEnd)
        .end_doc()
        .ws("   ")
        .tr(TriviaKind::Comment, "# c")
        .nl()
        .done();
    let bad = b(src)
        .doc()
        .bseq()
        .ind(Indicator::SequenceEntry)
        .ws("   ")
        .empty()
        .z(EventKind::SequenceEnd)
        .end_doc()
        .tr(TriviaKind::Comment, "# c")
        .nl()
        .done();
    one_placement(src, &good, &bad);

    let src = "---\n";
    let good = b(src)
        .t(EventKind::document_start(true), "---")
        .empty()
        .end_doc()
        .nl()
        .done();
    let bad = b(src)
        .t(EventKind::document_start(true), "---")
        .nl()
        .empty()
        .end_doc()
        .done();
    one_placement(src, &good, &bad);

    let src = "?\n: v\n";
    let tail = |x: B<'static>| {
        x.ind(Indicator::MappingValue)
            .ws(" ")
            .plain("v")
            .z(EventKind::MappingEnd)
            .end_doc()
            .nl()
            .done()
    };
    let good = tail(b(src).doc().bmap().ind(Indicator::MappingKey).empty().nl());
    let bad = tail(b(src).doc().bmap().ind(Indicator::MappingKey).nl().empty());
    one_placement(src, &good, &bad);

    // An implicit document never starts with an empty node.
    let ev = [
        Event::new(EventKind::StreamStart, Span::empty(0)),
        Event::new(EventKind::document_start(false), Span::empty(0)),
        Event::new(EventKind::plain(), Span::empty(0)),
        Event::new(EventKind::document_end(false), Span::empty(0)),
        Event::new(EventKind::StreamEnd, Span::empty(0)),
    ];
    assert_eq!(err_at("", &ev), (2, Violation::EmptyNodePlacement));
}

#[test]
fn empty_value_without_colon_follows_its_key() {
    let src = "? a\n? b\n";
    let good = b(src)
        .doc()
        .bmap()
        .ind(Indicator::MappingKey)
        .ws(" ")
        .plain("a")
        .empty()
        .nl()
        .ind(Indicator::MappingKey)
        .ws(" ")
        .plain("b")
        .empty()
        .z(EventKind::MappingEnd)
        .end_doc()
        .nl()
        .done();
    let bad = b(src)
        .doc()
        .bmap()
        .ind(Indicator::MappingKey)
        .ws(" ")
        .plain("a")
        .nl()
        .empty()
        .ind(Indicator::MappingKey)
        .ws(" ")
        .plain("b")
        .empty()
        .z(EventKind::MappingEnd)
        .end_doc()
        .nl()
        .done();
    one_placement(src, &good, &bad);

    let src = "{a , b: c}";
    let flow_map = || EventKind::mapping_start(CollectionStyle::Flow);
    let tail = |x: B<'static>| {
        x.ind(Indicator::FlowEntry)
            .ws(" ")
            .plain("b")
            .ind(Indicator::MappingValue)
            .ws(" ")
            .plain("c")
            .t(EventKind::MappingEnd, "}")
            .end_doc()
            .done()
    };
    let good = tail(b(src).doc().t(flow_map(), "{").plain("a").empty().ws(" "));
    let bad = tail(b(src).doc().t(flow_map(), "{").plain("a").ws(" ").empty());
    one_placement(src, &good, &bad);

    let src = "[ ? a ]";
    let pre = || {
        b(src)
            .doc()
            .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
            .ws(" ")
            .z(EventKind::mapping_start(CollectionStyle::FlowPair))
            .ind(Indicator::MappingKey)
            .ws(" ")
            .plain("a")
    };
    let good = pre()
        .empty()
        .z(EventKind::MappingEnd)
        .ws(" ")
        .t(EventKind::SequenceEnd, "]")
        .end_doc()
        .done();
    let bad = pre()
        .ws(" ")
        .empty()
        .z(EventKind::MappingEnd)
        .t(EventKind::SequenceEnd, "]")
        .end_doc()
        .done();
    one_placement(src, &good, &bad);
}

#[test]
fn empty_key_precedes_its_colon() {
    let src = "{ : v}";
    let flow_map = || EventKind::mapping_start(CollectionStyle::Flow);
    let tail = |x: B<'static>| {
        x.ind(Indicator::MappingValue)
            .ws(" ")
            .plain("v")
            .t(EventKind::MappingEnd, "}")
            .end_doc()
            .done()
    };
    let good = tail(b(src).doc().t(flow_map(), "{").ws(" ").empty());
    let bad = tail(b(src).doc().t(flow_map(), "{").empty().ws(" "));
    one_placement(src, &good, &bad);
}

#[test]
fn empty_nodes_elsewhere_need_an_error() {
    let src = "[]";
    let ev = b(src)
        .doc()
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .empty()
        .t(EventKind::SequenceEnd, "]")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev), (3, Violation::EmptyNodePlacement));
    // A repair node right after an error is fine (R2).
    let src = "a: ]";
    let ev = b(src)
        .doc()
        .bmap()
        .plain("a")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .t(EventKind::Error(diag(Severity::Error)), "]")
        .empty()
        .z(EventKind::MappingEnd)
        .end_doc()
        .done();
    check_contract(src, &ev).unwrap();
}

// ------------------------------------------------------------ C5 extent

#[test]
fn keep_chomping_includes_trailing_empty_lines() {
    let src = "a: |+\n  x\n\nb: c\n";
    let pre = || {
        b(src)
            .doc()
            .bmap()
            .plain("a")
            .ind(Indicator::MappingValue)
            .ws(" ")
    };
    let tail = |x: B<'static>| {
        x.plain("b")
            .ind(Indicator::MappingValue)
            .ws(" ")
            .plain("c")
            .z(EventKind::MappingEnd)
            .end_doc()
            .nl()
            .done()
    };
    let good = tail(literal(pre(), "|+", Chomping::Keep, 2, "  x\n\n"));
    check_contract(src, &good).unwrap();
    let bad = tail(literal(pre(), "|+", Chomping::Keep, 2, "  x\n").nl());
    assert_eq!(err_at(src, &bad).1, Violation::BlockScalarPlacement);
}

#[test]
fn eof_fragment_of_indentation_is_trivia() {
    let src = "|+\n a\n ";
    let good = literal(b(src).doc(), "|+", Chomping::Keep, 1, " a\n")
        .end_doc()
        .ws(" ")
        .done();
    check_contract(src, &good).unwrap();
    let bad = literal(b(src).doc(), "|+", Chomping::Keep, 1, " a\n ")
        .end_doc()
        .done();
    assert_eq!(err_at(src, &bad), (4, Violation::BlockScalarPlacement));

    // More spaces than the indentation are content, even at EOF.
    let src = "|\n a\n   ";
    let good = literal(b(src).doc(), "|", Chomping::Clip, 1, " a\n   ")
        .end_doc()
        .done();
    check_contract(src, &good).unwrap();
    let bad = literal(b(src).doc(), "|", Chomping::Clip, 1, " a\n")
        .end_doc()
        .ws("   ")
        .done();
    assert_eq!(err_at(src, &bad), (4, Violation::BlockScalarPlacement));
}

#[test]
fn clip_scalar_cannot_stop_before_more_content() {
    let src = "|\n a\n\n b\n";
    let good = literal(b(src).doc(), "|", Chomping::Clip, 1, " a\n\n b\n")
        .end_doc()
        .done();
    check_contract(src, &good).unwrap();
    let bad = literal(b(src).doc(), "|", Chomping::Clip, 1, " a\n")
        .end_doc()
        .nl()
        .ws(" ")
        .plain("b")
        .nl()
        .done();
    assert_eq!(err_at(src, &bad), (4, Violation::BlockScalarPlacement));
}

fn cr_literal(src: &'static str, body: &'static str) -> Vec<Event<'static>> {
    let x = b(src).doc();
    let h = BlockHeader::new(Chomping::Clip, None, 1, x.span_of("|"));
    x.tr(TriviaKind::BlockScalarHeader, "|")
        .tr(TriviaKind::LineBreak, "\r")
        .e(
            |s| Event::scalar(s, ScalarStyle::Literal, Properties::none(), Some(h)),
            body,
        )
        .end_doc()
        .done()
}

#[test]
fn block_scalar_lines_split_on_every_break_style() {
    let src = "|\r a\r\n  b\r";
    let ev = cr_literal(src, " a\r\n  b\r");
    check_contract(src, &ev).unwrap();
    assert_eq!(ev[4].value(src).unwrap(), "a\n b\n");
    // A `\r`-separated less-indented line is not content.
    let src = "|\r a\rb\r";
    assert_eq!(
        err_at(src, &cr_literal(src, " a\rb\r")),
        (4, Violation::BlockScalarPlacement)
    );
}

// ------------------------------------------------------------ C6 header errors

#[test]
fn error_abandons_a_pending_block_header() {
    // 5LLU-shaped: the error is on a content line after the header.
    let src = "k: >\n \n invalid\n";
    let pre = || {
        b(src)
            .doc()
            .bmap()
            .plain("k")
            .ind(Indicator::MappingValue)
            .ws(" ")
            .tr(TriviaKind::BlockScalarHeader, ">")
            .nl()
            .ws(" ")
            .nl()
            .ws(" ")
    };
    let good = pre()
        .t(EventKind::Error(diag(Severity::Error)), "invalid")
        .empty()
        .z(EventKind::MappingEnd)
        .end_doc()
        .nl()
        .done();
    check_contract(src, &good).unwrap();
    // Skipped bytes are allowed once the error has abandoned the header.
    let skipped = pre()
        .z(EventKind::Error(diag(Severity::Error)))
        .empty()
        .tr(TriviaKind::Skipped, "invalid")
        .z(EventKind::MappingEnd)
        .end_doc()
        .nl()
        .done();
    check_contract(src, &skipped).unwrap();
    // The abandoned header cannot be picked up by a block scalar afterwards.
    let h = BlockHeader::new(Chomping::Clip, None, 1, Span::new(3, 4));
    let resumed = pre()
        .z(EventKind::Error(diag(Severity::Error)))
        .e(
            |s| Event::scalar(s, ScalarStyle::Folded, Properties::none(), Some(h)),
            "invalid\n",
        )
        .z(EventKind::MappingEnd)
        .end_doc()
        .done();
    assert_eq!(err_at(src, &resumed).1, Violation::BlockHeaderMismatch);
}

// ------------------------------------------------------------ C3/C2 extras

#[test]
fn directive_after_unterminated_document() {
    let src = "a\n%YAML 1.2\n--- b\n";
    let ev = b(src)
        .doc()
        .plain("a")
        .end_doc()
        .nl()
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.2")
        .nl()
        .t(EventKind::document_start(true), "---")
        .ws(" ")
        .plain("b")
        .end_doc()
        .nl()
        .done();
    assert_eq!(
        err_at(src, &ev),
        (5, Violation::DirectiveAfterUnterminatedDocument)
    );
}

#[test]
fn tag_split_and_anchor_names_are_unique() {
    // The handle/suffix split is read from the bytes (production 40 makes
    // it unique): `!e!foo` is `!e!` + `foo`, never `!` + `e!foo`.
    let src = "%TAG !e! p:\n--- !e!foo a\n";
    let ev = crate::parse(src);
    check_contract(src, &ev).unwrap();
    let tag = ev.iter().find_map(|e| e.tag(src)).unwrap();
    assert_eq!(
        (tag.handle, tag.suffix, tag.resolved.as_ref()),
        ("!e!", "foo", "p:foo")
    );

    // An anchor name ends at a flow indicator (production 102), so the
    // anchor read at the node's anchor position is `&a`, not the whole
    // `&a,b` trivia: the node's property does not match the trivia.
    let src = "&a,b c";
    let ev = b(src)
        .doc()
        .tr(TriviaKind::Anchor, "&a,b")
        .ws(" ")
        .e(
            |s| {
                Event::scalar(
                    s,
                    ScalarStyle::Plain,
                    Properties::new(Some(0), None, None),
                    None,
                )
            },
            "c",
        )
        .end_doc()
        .done();
    assert_eq!(err_at(src, &ev).1, Violation::PropertyMismatch);
    let src = "*a,";
    let ev = b(src).doc().t(EventKind::Alias, "*a,").end_doc().done();
    assert!(matches!(err_at(src, &ev).1, Violation::BadBytes { .. }));

    // ns-anchor-char is ns-char minus flow indicators: Unicode white space
    // other than space/tab is allowed, control characters and BOMs are not.
    let src = "*a\u{a0}b\u{2028}";
    let ev = b(src).doc().t(EventKind::Alias, src).end_doc().done();
    check_contract(src, &ev).unwrap();
    for bad in ["*a\u{1}", "*a\u{feff}", "*a\u{7f}"] {
        let ev = b(bad).doc().t(EventKind::Alias, bad).end_doc().done();
        assert!(
            matches!(err_at(bad, &ev).1, Violation::BadBytes { .. }),
            "{bad:?}"
        );
    }
}

#[test]
fn new_violation_texts() {
    assert_eq!(
        Violation::EmptyNodePlacement.to_string(),
        "empty node misplaced"
    );
    assert_eq!(
        Violation::DirectiveAfterUnterminatedDocument.to_string(),
        "directive after a document not ended with `...`"
    );
}
