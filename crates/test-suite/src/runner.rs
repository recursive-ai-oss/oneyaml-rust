//! Scoring a producer against the suite.
//!
//! Every stream, valid or not, must satisfy the event contract
//! (`check_contract`, C1–C6) and build a syntax tree that agrees with it and
//! emits the input byte for byte (`check_tree`, consistency check (1),
//! a byte-identical round trip).
//!
//! A **valid** case passes when, in addition, the stream contains no `Error` event, its tree notation
//! equals `test.event`, and (if the expected-codes table has an entry) its
//! warning codes match.
//!
//! An **invalid** case passes when the stream satisfies the contract (recovery
//! streams are held to the same rules, C6/R1), contains at least one `Error`,
//! the semantic events *before the first error* render exactly to
//! `test.event` (the suite records the events a correct parser emits before
//! failing), and (if the table has an entry) its diagnostic codes match.

use oneyaml_core::Code;
use oneyaml_syntax::event::{ContractError, check_contract};
use oneyaml_syntax::tree::{SyntaxTree, TreeMismatch, check_tree};

use crate::case::Case;
use crate::codes::ExpectedCodes;
use crate::known::KnownDefects;
use crate::notation::{NotationError, to_tree_notation, to_tree_notation_prefix};
use crate::producer::{EventProducer, Produced};

/// Why a case failed.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// `in.yaml` is not UTF-8, so it cannot be handed to the producer.
    InputNotUtf8,
    /// The producer returned [`Produced::Unsupported`].
    Unsupported,
    /// The producer's events are paired with a source other than the input.
    WrongSource,
    /// The events violate the event-stream contract.
    Contract(ContractError),
    /// The syntax tree built from the events disagrees with them or does
    /// not emit the input byte for byte (consistency check (1)).
    Tree(TreeMismatch),
    /// A valid case produced an error event.
    UnexpectedError {
        /// The first error, displayed.
        message: String,
    },
    /// An invalid case produced no error event.
    MissingError,
    /// The events could not be rendered (only reachable for streams the
    /// contract accepts but this harness version does not understand).
    BadStream(NotationError),
    /// A valid case's event tree differs from `test.event`.
    EventMismatch {
        /// Expected notation (`test.event`).
        expected: String,
        /// Our notation.
        actual: String,
    },
    /// An invalid case's events before the first error differ from
    /// `test.event`.
    ErrorPrefixMismatch {
        /// Expected notation (`test.event`).
        expected: String,
        /// Our notation of the pre-error events.
        actual: String,
    },
    /// The diagnostic codes differ from the expected-codes table.
    CodeMismatch {
        /// From the table.
        expected: Vec<Code>,
        /// Emitted, in stream order.
        actual: Vec<Code>,
    },
}

impl Failure {
    /// A short stable label for grouping in reports.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::InputNotUtf8 => "input-not-utf8",
            Self::Unsupported => "unsupported",
            Self::WrongSource => "wrong-source",
            Self::Contract(_) => "contract",
            Self::Tree(_) => "tree",
            Self::UnexpectedError { .. } => "unexpected-error",
            Self::MissingError => "missing-error",
            Self::BadStream(_) => "bad-stream",
            Self::EventMismatch { .. } => "event-mismatch",
            Self::ErrorPrefixMismatch { .. } => "error-prefix-mismatch",
            Self::CodeMismatch { .. } => "code-mismatch",
        }
    }
}

fn join_codes(codes: &[Code]) -> String {
    let parts: Vec<String> = codes.iter().map(ToString::to_string).collect();
    parts.join(" ")
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InputNotUtf8 => f.write_str("in.yaml is not valid UTF-8"),
            Self::Unsupported => f.write_str("producer does not support this input"),
            Self::WrongSource => {
                f.write_str("events are paired with a source other than the input")
            }
            Self::Contract(err) => write!(f, "event contract violated: {err}"),
            Self::Tree(err) => write!(f, "syntax tree check failed: {err}"),
            Self::UnexpectedError { message } => {
                write!(f, "valid input produced an error: {message}")
            }
            Self::MissingError => f.write_str("invalid input produced no error event"),
            Self::BadStream(err) => write!(f, "malformed event stream: {err}"),
            Self::EventMismatch { expected, actual } => {
                write!(
                    f,
                    "event tree mismatch\n--- expected\n{expected}--- actual\n{actual}"
                )
            }
            Self::ErrorPrefixMismatch { expected, actual } => write!(
                f,
                "events before the first error differ\n--- expected\n{expected}--- actual\n{actual}"
            ),
            Self::CodeMismatch { expected, actual } => write!(
                f,
                "diagnostic codes differ: expected [{}], got [{}]",
                join_codes(expected),
                join_codes(actual)
            ),
        }
    }
}

fn same_lines(a: &str, b: &str) -> bool {
    a.lines().eq(b.lines())
}

/// Runs `producer` on one case against its own `test.event`.
///
/// # Errors
///
/// The [`Failure`] when the case does not pass.
pub fn run_case(
    producer: &mut dyn EventProducer,
    case: &Case,
    expected_codes: Option<&[Code]>,
) -> Result<(), Failure> {
    run_case_expecting(producer, case, expected_codes, &case.expected_events)
}

/// Runs `producer` on one case against `expected` (the case's `test.event`,
/// or a corrected expectation from the known-defects list). The expected
/// codes are checked before, and independently of, the event comparison.
///
/// # Errors
///
/// The [`Failure`] when the case does not pass.
pub fn run_case_expecting(
    producer: &mut dyn EventProducer,
    case: &Case,
    expected_codes: Option<&[Code]>,
    expected: &str,
) -> Result<(), Failure> {
    let input = core::str::from_utf8(&case.input).map_err(|_| Failure::InputNotUtf8)?;
    let events = match producer.produce(input) {
        Produced::Events(events) => events,
        Produced::Unsupported => return Err(Failure::Unsupported),
    };
    if !core::ptr::eq(events.source(), input) {
        return Err(Failure::WrongSource);
    }
    check_contract(input, &events).map_err(Failure::Contract)?;
    let tree = SyntaxTree::from_events(events.sourced());
    check_tree(&events, &tree).map_err(Failure::Tree)?;

    let first_error = events.iter().position(|e| e.is_error());
    match (case.expect_error, first_error) {
        (true, None) => return Err(Failure::MissingError),
        (false, Some(i)) => {
            let message = events
                .get(i)
                .and_then(|e| e.diagnostic())
                .map(ToString::to_string)
                .unwrap_or_default();
            return Err(Failure::UnexpectedError { message });
        }
        _ => {}
    }

    if let Some(expected) = expected_codes {
        let actual: Vec<Code> = events
            .iter()
            .filter_map(|e| e.diagnostic().map(|d| d.code))
            .collect();
        if actual != expected {
            return Err(Failure::CodeMismatch {
                expected: expected.to_vec(),
                actual,
            });
        }
    }

    if let Some(i) = first_error {
        let actual = to_tree_notation_prefix(&events, i).map_err(Failure::BadStream)?;
        if !same_lines(&actual, expected) {
            return Err(Failure::ErrorPrefixMismatch {
                expected: expected.to_owned(),
                actual,
            });
        }
    } else {
        let actual = to_tree_notation(&events).map_err(Failure::BadStream)?;
        if !same_lines(&actual, expected) {
            return Err(Failure::EventMismatch {
                expected: expected.to_owned(),
                actual,
            });
        }
    }
    Ok(())
}

/// Result for one case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseResult {
    /// The case id.
    pub id: String,
    /// The case name.
    pub name: String,
    /// Whether the case is an error case.
    pub expect_error: bool,
    /// `Ok` on pass.
    pub outcome: Result<(), Failure>,
    /// Judged against a corrected expectation (known-defects list).
    pub corrected: bool,
}

/// Results of a run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// One entry per case run, in id order.
    pub results: Vec<CaseResult>,
}

impl Report {
    /// Number of cases run.
    #[must_use]
    pub fn total(&self) -> usize {
        self.results.len()
    }

    /// Number of passing cases.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|r| r.outcome.is_ok()).count()
    }

    /// Number of failing cases.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.total().saturating_sub(self.passed())
    }

    /// Number of cases judged against a corrected expectation.
    #[must_use]
    pub fn corrected(&self) -> usize {
        self.results.iter().filter(|r| r.corrected).count()
    }

    /// Ids of the passing cases.
    #[must_use]
    pub fn passing_ids(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|r| r.outcome.is_ok())
            .map(|r| r.id.as_str())
            .collect()
    }

    /// `(passed, total)` restricted to valid (`false`) or error (`true`) cases.
    #[must_use]
    pub fn split(&self, expect_error: bool) -> (usize, usize) {
        let subset = self
            .results
            .iter()
            .filter(|r| r.expect_error == expect_error);
        let total = subset.clone().count();
        let passed = subset.filter(|r| r.outcome.is_ok()).count();
        (passed, total)
    }

    /// Failure counts grouped by [`Failure::label`], sorted by label.
    #[must_use]
    pub fn failure_breakdown(&self) -> Vec<(&'static str, usize)> {
        let mut counts: std::collections::BTreeMap<&'static str, usize> =
            std::collections::BTreeMap::new();
        for failure in self.results.iter().filter_map(|r| r.outcome.as_ref().err()) {
            let count = counts.entry(failure.label()).or_insert(0);
            *count = count.saturating_add(1);
        }
        counts.into_iter().collect()
    }
}

/// Runs `producer` over every case selected by `filter` (all cases when
/// `None`; see [`Case::matches`]), checking codes against `codes`, with no
/// corrections; see [`run_suite_with`].
pub fn run_suite(
    producer: &mut dyn EventProducer,
    cases: &[Case],
    filter: Option<&str>,
    codes: &ExpectedCodes,
) -> Report {
    run_suite_with(producer, cases, filter, codes, &KnownDefects::empty())
}

/// [`run_suite`], judging each case listed in `known` against its corrected
/// expectation, exactly (nothing is excused).
pub fn run_suite_with(
    producer: &mut dyn EventProducer,
    cases: &[Case],
    filter: Option<&str>,
    codes: &ExpectedCodes,
    known: &KnownDefects,
) -> Report {
    let results = cases
        .iter()
        .filter(|c| filter.is_none_or(|f| c.matches(f)))
        .map(|case| {
            let corrected = known.corrected(case);
            let expected = corrected.as_deref().unwrap_or(&case.expected_events);
            CaseResult {
                id: case.id.clone(),
                name: case.name.clone(),
                expect_error: case.expect_error,
                outcome: run_case_expecting(producer, case, codes.get(&case.id), expected),
                corrected: corrected.is_some(),
            }
        })
        .collect();
    Report { results }
}
