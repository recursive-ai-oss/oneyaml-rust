//! Known-defect corrections: listed cases are judged exactly against their
//! corrected expectation (nothing is excused), codes are checked
//! independently of the event comparison, and the shipped list is exact.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::process::Command;

use oneyaml_core::{Code, Span};
use oneyaml_syntax::event::{Event, EventKind, EventList};
use test_suite::producer::ParserProducer;
use test_suite::{
    Correction, EventProducer, ExpectedCodes, Failure, KnownDefects, Produced, default_codes_path,
    default_data_dir, default_known_defects_path, load_suite, run_case, run_suite, run_suite_with,
};

const SHIPPED: [&str; 9] = [
    "4H7K", "MUS6/01", "T833", "Y79Y/004", "Y79Y/005", "Y79Y/006", "Y79Y/007", "Y79Y/008",
    "Y79Y/009",
];

fn shipped() -> KnownDefects {
    KnownDefects::load(&default_known_defects_path()).unwrap()
}

#[test]
fn shipped_list_is_valid_exact_and_pinned() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let known = shipped();
    known.validate(&cases).unwrap();
    let mut expected = SHIPPED.to_vec();
    expected.sort_unstable();
    assert_eq!(known.ids(), expected);
    // Every listed case has its diagnosis pinned.
    let codes = ExpectedCodes::load(&default_codes_path()).unwrap();
    for id in SHIPPED {
        assert!(codes.get(id).is_some(), "{id} has no expected codes");
    }
    // The corrections are the recorded prefix with the `{}`/`[]` marker
    // restored, and nothing else changed (4H7K keeps its recorded `-DOC`).
    let t833 = cases.iter().find(|c| c.id == "T833").unwrap();
    assert_eq!(
        known.corrected(t833).unwrap(),
        "+STR\n+DOC ---\n+MAP {}\n=VAL :foo\n"
    );
    let h4h7k = cases.iter().find(|c| c.id == "4H7K").unwrap();
    let corrected = known.corrected(h4h7k).unwrap();
    assert_eq!(
        corrected,
        "+STR\n+DOC ---\n+SEQ []\n=VAL :a\n=VAL :b\n=VAL :c\n-SEQ\n-DOC\n"
    );
    assert_eq!(
        corrected.replace("+SEQ []", "+SEQ"),
        h4h7k.expected_events,
        "4H7K's correction only restores the marker"
    );
}

#[test]
fn corrected_cases_are_judged_exactly() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let codes = ExpectedCodes::load(&default_codes_path()).unwrap();
    let report = run_suite_with(&mut ParserProducer, &cases, None, &codes, &shipped());
    assert_eq!(report.corrected(), SHIPPED.len());
    assert_eq!(report.passed() + report.failed(), report.total());
    for id in SHIPPED {
        let r = report.results.iter().find(|r| r.id == *id).unwrap();
        if id == "4H7K" {
            // A real failure against its corrected expectation: we do not
            // emit the recorded `-DOC` before the error (tests/parser.rs).
            assert_eq!(
                r.outcome.as_ref().unwrap_err().label(),
                "error-prefix-mismatch",
                "{id}"
            );
        } else {
            assert_eq!(r.outcome, Ok(()), "{id}");
        }
    }
    // Without corrections the defective data fails every listed case (4H7K
    // fails either way, so it is counted once).
    let plain = run_suite(&mut ParserProducer, &cases, None, &codes);
    assert_eq!(plain.corrected(), 0);
    assert_eq!(plain.failed(), report.failed() + SHIPPED.len() - 1);
    for id in SHIPPED {
        let r = plain.results.iter().find(|r| r.id == *id).unwrap();
        assert!(r.outcome.is_err(), "{id}");
    }
}

/// Emits a zero-width error then skips nothing: conforming, wrong prefix
/// for most cases, and a code of our choosing.
struct ErrorWithCode(u16);

impl EventProducer for ErrorWithCode {
    fn name(&self) -> &'static str {
        "error-with-code"
    }
    fn produce<'a>(&mut self, input: &'a str) -> Produced<'a> {
        let d = oneyaml_core::Diagnostic::error(
            Code::new(self.0).unwrap(),
            "x",
            oneyaml_core::Label::new(Span::empty(0), ""),
        );
        let mut events = vec![
            Event::new(EventKind::StreamStart, Span::empty(0)),
            Event::new(EventKind::Error(Box::new(d)), Span::empty(0)),
        ];
        if !input.is_empty() {
            // Cover the input as skipped lines so the stream tiles.
            let mut pos = 0;
            for line in input.split_inclusive('\n') {
                let text = line.trim_end_matches('\n');
                if !text.is_empty() {
                    let end = pos + text.len();
                    events.push(Event::new(
                        EventKind::Trivia(oneyaml_syntax::event::TriviaKind::Skipped),
                        Span::new(pos, end),
                    ));
                    pos = end;
                }
                if line.ends_with('\n') {
                    events.push(Event::new(
                        EventKind::Trivia(oneyaml_syntax::event::TriviaKind::LineBreak),
                        Span::new(pos, pos + 1),
                    ));
                    pos += 1;
                }
            }
        }
        events.push(Event::new(EventKind::StreamEnd, Span::empty(input.len())));
        Produced::Events(EventList::from_parts_unchecked(input, events))
    }
}

#[test]
fn codes_are_checked_independently_of_the_prefix() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let t833 = cases.iter().find(|c| c.id == "T833").unwrap();
    let pinned = [Code::parse("Y0023").unwrap()];
    // Wrong prefix and wrong code: the code mismatch is reported.
    let r = run_case(&mut ErrorWithCode(17), t833, Some(&pinned));
    assert!(matches!(r, Err(Failure::CodeMismatch { .. })), "{r:?}");
    // Right code, wrong prefix: still a failure (nothing is excused).
    let r = run_case(&mut ErrorWithCode(23), t833, Some(&pinned));
    assert_eq!(r.unwrap_err().label(), "error-prefix-mismatch");
}

#[test]
fn an_empty_list_changes_nothing() {
    let cases = load_suite(&default_data_dir()).unwrap();
    let codes = ExpectedCodes::empty();
    let a = run_suite(&mut ParserProducer, &cases, None, &codes);
    let b = run_suite_with(
        &mut ParserProducer,
        &cases,
        None,
        &codes,
        &KnownDefects::empty(),
    );
    assert_eq!(a, b);
    let mut one = KnownDefects::empty();
    one.insert("D88J", Correction::Events("+STR\n".to_owned()), "test");
    let c = run_suite_with(&mut ParserProducer, &cases, Some("D88J"), &codes, &one);
    assert!(c.results[0].corrected);
    assert_eq!(
        c.results[0].outcome.as_ref().unwrap_err().label(),
        "event-mismatch"
    );
}

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
fn cli_marks_corrected_cases_and_rejects_bad_lists() {
    let (code, stdout, _) = cli(&["--producer", "parser", "--failures"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("known-defect entries: 9"));
    assert!(stdout.contains("FAIL 4H7K "));
    assert_eq!(stdout.matches("FAIL ").count(), 1);
    assert!(stdout.contains("judged against corrected expectations: 9 (8 passed"));

    let path = std::env::temp_dir().join(format!("oneyaml-known-{}.txt", std::process::id()));
    let d88j = "+STR; +DOC; +MAP; =VAL :a; +SEQ []; =VAL :b; =VAL :c; -SEQ; -MAP; -DOC; -STR";
    std::fs::write(&path, format!("D88J events {d88j} -- not defective\n")).unwrap();
    let (code, _, stderr) = cli(&["--known-defects", path.to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(stderr.contains("stale entry `D88J`"));
    std::fs::write(&path, "NOPE events +STR -- reason\n").unwrap();
    let (code, _, stderr) = cli(&["--known-defects", path.to_str().unwrap()]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown case `NOPE`"));
    let (code, _, stderr) = cli(&["--known-defects", "/nonexistent/k.txt"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("/nonexistent/k.txt"));
}
