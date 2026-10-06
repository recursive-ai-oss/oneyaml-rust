//! Cross-module tests for the conformance harness: suite loading, scoring
//! rules for every outcome, the stub baseline, anti-gaming producers, and the
//! CLI.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::path::Path;
use std::process::Command;

use oneyaml_core::code::RESERVED_DIRECTIVE_IGNORED;
use oneyaml_core::{Code, Diagnostic, Label, Severity, Span};
use oneyaml_syntax::event::{
    CollectionStyle, Directive, Event, EventKind, EventList, Indicator, ScalarStyle, TriviaKind,
    Violation,
};
use test_suite::{
    Case, EventProducer, ExpectedCodes, Failure, Produced, StubProducer, default_codes_path,
    default_data_dir, load_suite, run_case, run_suite,
};

// Counts of the vendored release; see data/yaml-test-suite/SOURCE.md.
const TOTAL_CASES: usize = 402;
const ERROR_CASES: usize = 94;

fn cases() -> Vec<Case> {
    load_suite(&default_data_dir()).unwrap()
}

fn case(id: &str) -> Case {
    cases().into_iter().find(|c| c.id == id).unwrap()
}

// ---------------------------------------------------------------- fixtures

/// Builds events left to right with spans computed from the texts.
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
            "{text:?} at {}",
            self.pos
        );
        let end = self.pos + text.len();
        self.events.push(Event::new(kind, Span::new(self.pos, end)));
        self.pos = end;
        self
    }
    fn z(mut self, kind: EventKind) -> Self {
        self.events.push(Event::new(kind, Span::empty(self.pos)));
        self
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
    fn done(self) -> Vec<Event<'a>> {
        assert_eq!(self.pos, self.src.len());
        let mut events = self.events;
        events.push(Event::new(EventKind::StreamEnd, Span::empty(self.pos)));
        events
    }
}

fn diagnostic(code: Code, severity: Severity) -> Box<Diagnostic> {
    Box::new(Diagnostic::new(
        code,
        severity,
        "fixture",
        Label::new(Span::empty(0), ""),
    ))
}

fn syntax_error() -> EventKind {
    EventKind::Error(diagnostic(Code::new(1).unwrap(), Severity::Error))
}

/// D88J `a: [b, c]\n` — correct and contract-conforming.
fn d88j_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .z(EventKind::document_start(false))
        .z(EventKind::mapping_start(CollectionStyle::Block))
        .plain("a")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .plain("b")
        .ind(Indicator::FlowEntry)
        .ws(" ")
        .plain("c")
        .t(EventKind::SequenceEnd, "]")
        .z(EventKind::MappingEnd)
        .z(EventKind::document_end(false))
        .nl()
        .done()
}

/// 9MMA `%YAML 1.2\n` (invalid: directive without document).
fn nine_mma_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .t(EventKind::Directive(Directive::Yaml), "%YAML 1.2")
        .nl()
        .z(syntax_error())
        .done()
}

/// 2G84/00 `--- |0\n` (invalid indentation indicator): a non-trivial prefix,
/// then recovery by synthesis (C6 R2).
fn two_g84_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .t(EventKind::document_start(true), "---")
        .ws(" ")
        .t(syntax_error(), "|0")
        .z(EventKind::plain())
        .z(EventKind::document_end(false))
        .nl()
        .done()
}

/// 6LVF: reserved directive with a warning.
fn six_lvf_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .t(EventKind::Directive(Directive::Reserved), "%FOO  bar baz")
        .z(EventKind::Warning(diagnostic(
            RESERVED_DIRECTIVE_IGNORED,
            Severity::Warning,
        )))
        .ws(" ")
        .tr(TriviaKind::Comment, "# Should be ignored")
        .nl()
        .ws("              ")
        .tr(TriviaKind::Comment, "# with a warning.")
        .nl()
        .t(EventKind::document_start(true), "---")
        .ws(" ")
        .t(EventKind::scalar(ScalarStyle::DoubleQuoted), "\"foo\"")
        .z(EventKind::document_end(false))
        .nl()
        .done()
}

/// CT4Q: single-pair explicit entry in a flow sequence (implicit flow pair).
fn ct4q_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .z(EventKind::document_start(false))
        .t(EventKind::sequence_start(CollectionStyle::Flow), "[")
        .nl()
        .z(EventKind::mapping_start(CollectionStyle::FlowPair))
        .ind(Indicator::MappingKey)
        .ws(" ")
        .t(EventKind::scalar(ScalarStyle::Plain), "foo\n bar")
        .ws(" ")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .plain("baz")
        .z(EventKind::MappingEnd)
        .nl()
        .t(EventKind::SequenceEnd, "]")
        .z(EventKind::document_end(false))
        .nl()
        .done()
}

/// HWV9 `...\n`: a bare document-end marker, no document.
fn hwv9_events(src: &str) -> Vec<Event<'_>> {
    b(src).tr(TriviaKind::DocumentEndMarker, "...").nl().done()
}

/// 5LLU: an error on a content line after a block scalar header (C6 R2: the
/// error abandons the header; the repair is an empty plain scalar).
fn five_llu_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .z(EventKind::document_start(false))
        .z(EventKind::mapping_start(CollectionStyle::Block))
        .plain("block scalar")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .tr(TriviaKind::BlockScalarHeader, ">")
        .nl()
        .ws(" ")
        .nl()
        .ws("  ")
        .nl()
        .ws("   ")
        .nl()
        .ws(" ")
        .t(syntax_error(), "invalid")
        .z(EventKind::plain())
        .z(EventKind::MappingEnd)
        .z(EventKind::document_end(false))
        .nl()
        .done()
}

/// Y79Y/000: a tab on the first content line; recovery continues the mapping.
fn y79y_events(src: &str) -> Vec<Event<'_>> {
    b(src)
        .z(EventKind::document_start(false))
        .z(EventKind::mapping_start(CollectionStyle::Block))
        .plain("foo")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .tr(TriviaKind::BlockScalarHeader, "|")
        .nl()
        .t(syntax_error(), "\t")
        .z(EventKind::plain())
        .nl()
        .plain("bar")
        .ind(Indicator::MappingValue)
        .ws(" ")
        .plain("1")
        .z(EventKind::MappingEnd)
        .z(EventKind::document_end(false))
        .nl()
        .done()
}

/// A producer that knows exactly one input and applies `mutate` to its
/// hand-built events. Every other input is unsupported.
struct Fixture {
    input: &'static str,
    build: for<'a> fn(&'a str) -> Vec<Event<'a>>,
    mutate: fn(&mut Vec<Event<'_>>),
}

impl EventProducer for Fixture {
    fn name(&self) -> &'static str {
        "fixture"
    }

    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        if input != self.input {
            return Produced::Unsupported;
        }
        let mut events = (self.build)(input);
        (self.mutate)(&mut events);
        Produced::Events(EventList::from_parts_unchecked(input, events))
    }
}

fn fixture(id: &str, mutate: fn(&mut Vec<Event<'_>>)) -> (Fixture, Case) {
    let c = case(id);
    let input: &'static str =
        Box::leak(String::from_utf8(c.input.clone()).unwrap().into_boxed_str());
    let build: for<'a> fn(&'a str) -> Vec<Event<'a>> = match id {
        "D88J" => d88j_events,
        "9MMA" => nine_mma_events,
        "2G84/00" => two_g84_events,
        "6LVF" => six_lvf_events,
        "CT4Q" => ct4q_events,
        "HWV9" => hwv9_events,
        "5LLU" => five_llu_events,
        "Y79Y/000" => y79y_events,
        _ => panic!("no fixture for {id}"),
    };
    (
        Fixture {
            input,
            build,
            mutate,
        },
        c,
    )
}

fn run(id: &str, mutate: fn(&mut Vec<Event<'_>>)) -> Result<(), Failure> {
    let (mut p, c) = fixture(id, mutate);
    run_case(&mut p, &c, None)
}

// ---------------------------------------------------------------- loading

#[test]
fn vendored_suite_loads_completely() {
    let cases = cases();
    assert_eq!(cases.len(), TOTAL_CASES);
    assert_eq!(cases.iter().filter(|c| c.expect_error).count(), ERROR_CASES);
    let mut ids: Vec<&str> = cases.iter().map(|c| c.id.as_str()).collect();
    ids.dedup();
    assert_eq!(ids.len(), TOTAL_CASES, "ids are sorted and unique");
    assert!(cases.iter().any(|c| c.id == "2G84/00"), "sub-cases load");
    for c in &cases {
        assert!(!c.name.is_empty(), "{} has a name", c.id);
        assert!(
            c.expected_events.starts_with("+STR\n"),
            "{} test.event starts with +STR",
            c.id
        );
        if !c.expect_error {
            assert!(
                c.expected_events.trim_end().ends_with("-STR"),
                "{} test.event ends with -STR",
                c.id
            );
        }
    }
}

// ------------------------------------------------------------ passing fixtures

#[test]
fn correct_streams_pass() {
    for id in [
        "D88J", "CT4Q", "HWV9", "6LVF", "9MMA", "2G84/00", "5LLU", "Y79Y/000",
    ] {
        assert_eq!(run(id, |_| {}), Ok(()), "{id}");
    }
}

#[test]
fn warnings_do_not_fail_valid_cases_and_codes_are_checked() {
    let (mut p, c) = fixture("6LVF", |_| {});
    assert_eq!(
        run_case(&mut p, &c, Some(&[RESERVED_DIRECTIVE_IGNORED])),
        Ok(())
    );
    let other = Code::new(1000).unwrap();
    assert_eq!(
        run_case(&mut p, &c, Some(&[other])),
        Err(Failure::CodeMismatch {
            expected: vec![other],
            actual: vec![RESERVED_DIRECTIVE_IGNORED],
        })
    );
    // An empty entry asserts "no diagnostics".
    assert!(matches!(
        run_case(&mut p, &c, Some(&[])),
        Err(Failure::CodeMismatch { .. })
    ));
    // Error cases are code-checked too.
    let (mut p, c) = fixture("9MMA", |_| {});
    let one = Code::new(1).unwrap();
    assert_eq!(run_case(&mut p, &c, Some(&[one])), Ok(()));
    assert!(matches!(
        run_case(&mut p, &c, Some(&[Code::new(2).unwrap()])),
        Err(Failure::CodeMismatch { .. })
    ));
}

// ------------------------------------------------------------ failing fixtures

#[test]
fn meaning_contradicting_bytes_is_a_contract_failure() {
    // `c` claimed as single-quoted: the payload disagrees with the bytes.
    let r = run("D88J", |events| {
        events[10] = Event::new(
            EventKind::scalar(ScalarStyle::SingleQuoted),
            events[10].span(),
        );
    });
    assert_eq!(r.unwrap_err().label(), "contract");
}

#[test]
fn byte_consistent_wrong_tree_is_an_event_mismatch() {
    // Values are decoded from the bytes, so they cannot contradict them; but
    // the contract does not check the grammar, e.g. that a zero-width
    // mapping directly in a flow sequence is a flow pair (production 150).
    // A block mapping there is byte-consistent; the tree comparison catches
    // it.
    let r = run("CT4Q", |events| {
        let i = events
            .iter()
            .position(|e| {
                matches!(
                    e.kind(),
                    EventKind::MappingStart {
                        style: CollectionStyle::FlowPair,
                        ..
                    }
                )
            })
            .unwrap();
        events[i] = Event::new(
            EventKind::mapping_start(CollectionStyle::Block),
            events[i].span(),
        );
    });
    match r {
        Err(Failure::EventMismatch { expected, actual }) => {
            assert!(expected.contains("+MAP {}\n=VAL :foo bar\n"));
            assert!(actual.contains("+MAP\n=VAL :foo bar\n"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn contract_violations_fail_valid_and_error_cases() {
    let err = run("D88J", |events| {
        events.remove(5); // whitespace after `:`
    })
    .unwrap_err();
    assert!(matches!(
        err,
        Failure::Contract(ref e) if matches!(e.violation, Violation::Discontiguous { .. })
    ));
    assert!(
        err.to_string()
            .starts_with("event contract violated: event 5:")
    );

    let err = run("9MMA", |events| {
        events.remove(2);
        events[2] = Event::new(syntax_error(), Span::new(9, 10));
    })
    .unwrap_err();
    assert!(matches!(
        err,
        Failure::Contract(ref e) if e.violation == Violation::LineBreak
    ));
}

#[test]
fn error_on_valid_input_fails() {
    let r = run("D88J", |events| {
        events.insert(3, Event::new(syntax_error(), Span::empty(0)));
    });
    match r {
        Err(Failure::UnexpectedError { message }) => {
            assert_eq!(message, "error[Y0001]: fixture");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn invalid_input_without_error_fails() {
    // Remove the error; recovery-only constructs become contract violations,
    // so use a stream that is otherwise fine: 9MMA without its error violates
    // "directive without document". 2G84/00 without the error becomes a
    // contract failure too (skipped bytes as the error region).
    assert_eq!(
        run("9MMA", |events| {
            events.remove(3);
        })
        .unwrap_err()
        .label(),
        "contract"
    );
    let r = run("2G84/00", |events| {
        events[3] = Event::new(EventKind::scalar(ScalarStyle::Plain), events[3].span());
        events.remove(4);
    });
    assert_eq!(r, Err(Failure::MissingError));
}

#[test]
fn error_prefix_must_match() {
    // Emitting the document start *after* the error loses `+DOC ---`.
    let r = run("2G84/00", |events| {
        events.insert(1, Event::new(syntax_error(), Span::empty(0)));
    });
    match r {
        Err(Failure::ErrorPrefixMismatch { expected, actual }) => {
            assert_eq!(expected, "+STR\n+DOC ---\n");
            assert_eq!(actual, "+STR\n");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn non_utf8_input_fails() {
    let mut c = case("D88J");
    c.input = vec![0xff, 0x0a];
    assert_eq!(
        run_case(&mut StubProducer, &c, None),
        Err(Failure::InputNotUtf8)
    );
}

#[test]
fn failure_labels_and_texts() {
    let code = Code::new(1).unwrap();
    let cases: Vec<(Failure, &str, &str)> = vec![
        (
            Failure::InputNotUtf8,
            "input-not-utf8",
            "in.yaml is not valid UTF-8",
        ),
        (
            Failure::Unsupported,
            "unsupported",
            "producer does not support this input",
        ),
        (
            Failure::MissingError,
            "missing-error",
            "invalid input produced no error event",
        ),
        (
            Failure::UnexpectedError {
                message: "m".into(),
            },
            "unexpected-error",
            "valid input produced an error: m",
        ),
        (
            Failure::EventMismatch {
                expected: "+STR\n".into(),
                actual: "-STR\n".into(),
            },
            "event-mismatch",
            "event tree mismatch\n--- expected\n+STR\n--- actual\n-STR\n",
        ),
        (
            Failure::ErrorPrefixMismatch {
                expected: "a\n".into(),
                actual: "b\n".into(),
            },
            "error-prefix-mismatch",
            "events before the first error differ\n--- expected\na\n--- actual\nb\n",
        ),
        (
            Failure::CodeMismatch {
                expected: vec![code, code],
                actual: vec![],
            },
            "code-mismatch",
            "diagnostic codes differ: expected [Y0001 Y0001], got []",
        ),
        (
            Failure::BadStream(test_suite::NotationError::UnknownEvent { index: 1 }),
            "bad-stream",
            "malformed event stream: event 1: unknown event kind",
        ),
    ];
    for (failure, label, text) in cases {
        assert_eq!(failure.label(), label);
        assert_eq!(failure.to_string(), text);
    }
}

// ------------------------------------------------------------ whole-suite runs

#[test]
fn stub_baseline_is_zero() {
    let report = run_suite(&mut StubProducer, &cases(), None, &ExpectedCodes::empty());
    assert_eq!(report.total(), TOTAL_CASES);
    assert_eq!(report.passed(), 0);
    assert_eq!(report.failed(), TOTAL_CASES);
    assert_eq!(report.split(false), (0, TOTAL_CASES - ERROR_CASES));
    assert_eq!(report.split(true), (0, ERROR_CASES));
    assert_eq!(
        report.failure_breakdown(),
        vec![("unsupported", TOTAL_CASES)]
    );
}

#[test]
fn only_genuinely_correct_cases_score() {
    let (mut p, _) = fixture("D88J", |_| {});
    let report = run_suite(&mut p, &cases(), None, &ExpectedCodes::empty());
    assert_eq!(report.passing_ids(), ["D88J"]);
}

/// Claims every input is one big error.
struct AlwaysError;

impl EventProducer for AlwaysError {
    fn name(&self) -> &'static str {
        "always-error"
    }
    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        let len = input.len();
        Produced::Events(EventList::from_parts_unchecked(
            input,
            vec![
                Event::new(EventKind::StreamStart, Span::empty(0)),
                Event::new(syntax_error(), Span::new(0, len)),
                Event::new(EventKind::StreamEnd, Span::empty(len)),
            ],
        ))
    }
}

/// Structural events all zero-width, the whole input as one whitespace blob.
struct ZeroWidthBlob;

impl EventProducer for ZeroWidthBlob {
    fn name(&self) -> &'static str {
        "zero-width-blob"
    }
    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        let len = input.len();
        let mut events = vec![
            Event::new(EventKind::StreamStart, Span::empty(0)),
            Event::new(EventKind::document_start(false), Span::empty(0)),
            Event::new(EventKind::plain(), Span::empty(0)),
            Event::new(EventKind::document_end(false), Span::empty(0)),
        ];
        if len > 0 {
            events.push(Event::new(
                EventKind::Trivia(TriviaKind::Whitespace),
                Span::new(0, len),
            ));
        }
        events.push(Event::new(EventKind::StreamEnd, Span::empty(len)));
        Produced::Events(EventList::from_parts_unchecked(input, events))
    }
}

/// The strongest cheat the suite's data admits: an immediate zero-width error,
/// then every line skipped. Contract-conforming by construction.
struct ErrorThenSkipLines;

impl EventProducer for ErrorThenSkipLines {
    fn name(&self) -> &'static str {
        "error-then-skip"
    }
    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        let mut events = vec![
            Event::new(EventKind::StreamStart, Span::empty(0)),
            Event::new(syntax_error(), Span::empty(0)),
        ];
        let bytes = input.as_bytes();
        let (mut start, mut i) = (0, 0);
        while i < bytes.len() {
            if bytes[i] == b'\n' || bytes[i] == b'\r' {
                if start < i {
                    events.push(Event::new(
                        EventKind::Trivia(TriviaKind::Skipped),
                        Span::new(start, i),
                    ));
                }
                let end = if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    i + 2
                } else {
                    i + 1
                };
                events.push(Event::new(
                    EventKind::Trivia(TriviaKind::LineBreak),
                    Span::new(i, end),
                ));
                start = end;
                i = end;
            } else {
                i += 1;
            }
        }
        if start < bytes.len() {
            events.push(Event::new(
                EventKind::Trivia(TriviaKind::Skipped),
                Span::new(start, bytes.len()),
            ));
        }
        events.push(Event::new(EventKind::StreamEnd, Span::empty(bytes.len())));
        Produced::Events(EventList::from_parts_unchecked(input, events))
    }
}

#[test]
fn always_error_producer_scores_zero() {
    let report = run_suite(&mut AlwaysError, &cases(), None, &ExpectedCodes::empty());
    assert_eq!(report.passed(), 0, "{:?}", report.passing_ids());
}

#[test]
fn zero_width_blob_producer_scores_zero() {
    let report = run_suite(&mut ZeroWidthBlob, &cases(), None, &ExpectedCodes::empty());
    assert_eq!(report.passed(), 0, "{:?}", report.passing_ids());
}

#[test]
fn error_then_skip_is_bounded_and_closed_by_codes() {
    // Known residual: without per-case codes the
    // suite cannot distinguish a correct early error from this cheat on cases
    // whose recorded prefix is just `+STR`. It can score only those.
    let cases = cases();
    let report = run_suite(
        &mut ErrorThenSkipLines,
        &cases,
        None,
        &ExpectedCodes::empty(),
    );
    let bare: Vec<&str> = cases
        .iter()
        .filter(|c| c.expect_error && c.expected_events == "+STR\n")
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(bare.len(), 7);
    assert_eq!(report.passing_ids(), bare);
    assert_eq!(report.split(false).0, 0);
    // Expected-code entries close it.
    let mut codes = ExpectedCodes::empty();
    for id in &bare {
        codes.insert(*id, vec![Code::new(2).unwrap()]);
    }
    let report = run_suite(&mut ErrorThenSkipLines, &cases, None, &codes);
    assert_eq!(report.passed(), 0);
}

#[test]
fn filter_selects_single_case_and_subcases() {
    let cases = cases();
    let none = ExpectedCodes::empty();
    let one = run_suite(&mut StubProducer, &cases, Some("D88J"), &none);
    assert_eq!(one.total(), 1);
    assert_eq!(one.results[0].id, "D88J");
    let sub = run_suite(&mut StubProducer, &cases, Some("2G84/01"), &none);
    assert_eq!(sub.total(), 1);
    let family = run_suite(&mut StubProducer, &cases, Some("2G84"), &none);
    assert_eq!(family.total(), 4);
    let nothing = run_suite(&mut StubProducer, &cases, Some("NOPE"), &none);
    assert_eq!(nothing.total(), 0);
}

#[test]
fn shipped_codes_table_is_valid_and_lists_known_warnings() {
    let cases = cases();
    let table = ExpectedCodes::load(&default_codes_path()).unwrap();
    table.validate(&cases).unwrap();
    let y1000 = Code::parse("Y1000").unwrap();
    let y1001 = Code::parse("Y1001").unwrap();
    for (id, code) in [
        ("MUS6/02", y1000),
        ("MUS6/03", y1000),
        ("MUS6/04", y1000),
        ("BEC7", y1001),
        ("2LFX", RESERVED_DIRECTIVE_IGNORED),
        ("6LVF", RESERVED_DIRECTIVE_IGNORED),
        ("MUS6/05", RESERVED_DIRECTIVE_IGNORED),
        ("MUS6/06", RESERVED_DIRECTIVE_IGNORED),
    ] {
        assert_eq!(table.get(id), Some(&[code][..]), "{id}");
    }
    assert_eq!(table.len(), 102);
    // A warning-less producer fails these cases once the table applies.
    let (mut p, c) = fixture("6LVF", |events| {
        events.retain(|e| !e.is_warning());
    });
    assert!(matches!(
        run_case(&mut p, &c, table.get("6LVF")),
        Err(Failure::CodeMismatch { .. })
    ));
    assert!(ExpectedCodes::load(Path::new("/nonexistent")).is_err());
}

// ---------------------------------------------------------------- CLI

fn cli(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_yaml-test-suite"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn cli_full_run_reports_numbers() {
    let (code, stdout, _) = cli(&[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("producer: stub"));
    assert!(stdout.contains("expected-codes entries: 102"));
    assert!(stdout.contains("passed: 0  failed: 402  total: 402"));
    assert!(stdout.contains("valid cases: 0/308 passed"));
    assert!(stdout.contains("error cases: 0/94 passed"));
    assert!(stdout.contains("failures by reason: unsupported=402"));
}

#[test]
fn cli_single_case_verbose() {
    let (code, stdout, _) = cli(&["--case", "D88J", "--verbose"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("FAIL D88J [valid] Flow Sequence in Block Mapping"));
    assert!(stdout.contains("passed: 0  failed: 1  total: 1"));
}

#[test]
fn cli_failures_listing_and_strict() {
    let (code, stdout, _) = cli(&["--case", "9MMA", "--failures", "--strict"]);
    assert_eq!(code, 1);
    assert!(stdout.contains("FAIL 9MMA [error] unsupported"));
}

#[test]
fn cli_usage_errors() {
    let (code, _, stderr) = cli(&["--case", "NOPE"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("no case matches `NOPE`"));

    let (code, _, stderr) = cli(&["--producer", "nope"]);
    assert_eq!(code, 2);
    assert!(stderr.contains(
        "unknown producer `nope` (known: stub, parser, parser-lenient, parser-untrusted)"
    ));

    let (code, _, stderr) = cli(&["--bogus"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown argument `--bogus`"));

    let (code, _, stderr) = cli(&["--case"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--case needs a value"));

    let (code, _, stderr) = cli(&["--data", "/nonexistent/oneyaml"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("/nonexistent/oneyaml"));

    let (code, _, stderr) = cli(&["--codes", "/nonexistent/codes.txt"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("/nonexistent/codes.txt"));

    let bad_table = std::env::temp_dir().join(format!("oneyaml-codes-{}.txt", std::process::id()));
    std::fs::write(&bad_table, "NOPE Y1003\n").unwrap();
    let (code, _, stderr) = cli(&["--codes", bad_table.to_str().unwrap()]);
    std::fs::remove_file(&bad_table).unwrap();
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown case `NOPE`"));

    let (code, stdout, _) = cli(&["--help"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("usage: yaml-test-suite"));
}

/// Reads its events from a copy of the input: right bytes, wrong source.
struct CopiedSource(String);

impl EventProducer for CopiedSource {
    fn name(&self) -> &'static str {
        "copied-source"
    }
    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        self.0 = input.to_owned();
        // The parser's own events, re-paired with a string that is not the
        // input the harness handed over.
        let events = oneyaml_syntax::parse(input).into_events();
        let leaked: &'a str = Box::leak(self.0.clone().into_boxed_str());
        Produced::Events(EventList::from_parts_unchecked(leaked, events))
    }
}

/// The harness reads through the producer's `EventList`, so it
/// insists that list is paired with the very input it handed over.
#[test]
fn events_paired_with_another_source_fail() {
    let c = case("D88J");
    assert_eq!(
        run_case(&mut CopiedSource(String::new()), &c, None),
        Err(Failure::WrongSource)
    );
    assert_eq!(Failure::WrongSource.label(), "wrong-source");
    assert!(Failure::WrongSource.to_string().contains("source"));
}
