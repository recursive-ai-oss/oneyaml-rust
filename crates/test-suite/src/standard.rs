//! **Standard** scoring: the criterion other YAML implementations publish,
//! so that any parser — ours or a competitor — can be scored the same way.
//!
//! This mode is deliberately weaker than [`crate::runner`] (the strict mode,
//! which is unchanged): no event contract, no diagnostic codes, and an error
//! case passes on *any* reported error. Per case it records:
//!
//! * **classified** — a valid case was accepted / an error case was rejected;
//! * **events** — (valid cases, parsers with an event API) the event tree
//!   matches `test.event` after [`normalize`] drops what the parser's API
//!   cannot express ([`Profile`]); applied to both sides;
//! * **prefix** — (error cases, event API) the events emitted before the error
//!   match `test.event`, once against the vendored expectation and once
//!   against the corrected one ([`KnownDefects`]); informational only;
//! * **round trip** — (valid cases) whatever the oracle reports, e.g. its
//!   lossless tree printed back is byte-identical to `in.yaml`.
//!
//! A panic inside an oracle is caught and scored as a failure (a panic is not
//! a reported error). So is a **truncated** observation: an adapter that
//! stops at its event cap without reaching the end of the stream has neither
//! accepted nor rejected the input.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::case::Case;
use crate::known::KnownDefects;
use crate::notation::{to_tree_notation, to_tree_notation_prefix};

/// What a parser's event API can express. A `false` field means the
/// information is absent from its events, so [`normalize`] drops it from both
/// the expected and the actual notation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// `+DOC ---` vs `+DOC`.
    pub doc_start_marker: bool,
    /// `-DOC ...` vs `-DOC`.
    pub doc_end_marker: bool,
    /// `+MAP {}` / `+SEQ []` flow markers.
    pub flow_markers: bool,
    /// Anchor names; when `false`, anchors are compared by definition order
    /// (`&1`, `&2`, … and aliases to the latest definition of that name).
    pub anchor_names: bool,
}

impl Profile {
    /// Everything the suite notation records.
    pub const FULL: Self = Self {
        doc_start_marker: true,
        doc_end_marker: true,
        flow_markers: true,
        anchor_names: true,
    };

    /// Short description of what is dropped, for report notes.
    #[must_use]
    pub fn dropped(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.doc_start_marker {
            out.push("`---` marker");
        }
        if !self.doc_end_marker {
            out.push("`...` marker");
        }
        if !self.flow_markers {
            out.push("flow `{}`/`[]` markers");
        }
        if !self.anchor_names {
            out.push("anchor names (compared by order)");
        }
        out
    }
}

/// What an oracle observed for one input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// Suite-notation lines emitted (before the error, if any); `None` when
    /// the parser has no event API.
    pub events: Option<Vec<String>>,
    /// `Some(message)` when the parser reported the input as invalid.
    pub error: Option<String>,
    /// Byte-identical round trip, when the oracle measures it.
    pub round_trip: Option<bool>,
    /// The adapter stopped (e.g. at its event cap) before the parser either
    /// reached the end of the stream or reported an error: no verdict, scored
    /// as a failure like a panic, never as a rejection.
    pub truncated: bool,
}

/// A parser under standard scoring.
pub trait Oracle {
    /// Name for reports.
    fn name(&self) -> &'static str;
    /// Exact version under test.
    fn version(&self) -> &'static str;
    /// What the event API expresses; `None` when there is no event API.
    fn profile(&self) -> Option<Profile>;
    /// Parses `input`.
    fn observe(&mut self, input: &str) -> Observation;
}

/// The standard score of one case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseScore {
    /// Case id.
    pub id: String,
    /// Error case.
    pub expect_error: bool,
    /// The oracle panicked (payload text).
    pub panic: Option<String>,
    /// Accepted when valid / rejected when invalid (and did not panic).
    pub classified: bool,
    /// Valid case with an event API: the normalized trees are equal.
    pub events: Option<bool>,
    /// Error case with an event API: pre-error events match the vendored
    /// `test.event`.
    pub prefix_vendored: Option<bool>,
    /// Same, against the corrected expectation where one exists.
    pub prefix_corrected: Option<bool>,
    /// Valid case: round trip, if measured.
    pub round_trip: Option<bool>,
    /// The raw observation, for spot checks.
    pub observation: Observation,
}

impl CaseScore {
    /// The standard verdict: valid ⇒ accepted and (if an event API exists)
    /// events match; error ⇒ an error was reported.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.classified && self.events.unwrap_or(true)
    }
}

/// Standard scores of one oracle over the suite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StandardReport {
    /// Oracle name.
    pub name: &'static str,
    /// Oracle version.
    pub version: &'static str,
    /// Event-API profile (`None`: no event API).
    pub profile: Option<Profile>,
    /// One per case, in id order.
    pub cases: Vec<CaseScore>,
}

fn count(cases: &[CaseScore], pred: impl Fn(&CaseScore) -> bool) -> usize {
    cases.iter().filter(|c| pred(c)).count()
}

impl StandardReport {
    /// `(passed, total)` of valid (`false`) or error (`true`) cases under the
    /// standard verdict ([`CaseScore::passed`]).
    #[must_use]
    pub fn standard(&self, expect_error: bool) -> (usize, usize) {
        let pass = count(&self.cases, |c| {
            c.expect_error == expect_error && c.passed()
        });
        (pass, count(&self.cases, |c| c.expect_error == expect_error))
    }

    /// Cases classified correctly (accept valid / reject invalid).
    #[must_use]
    pub fn classified(&self, expect_error: bool) -> usize {
        count(&self.cases, |c| {
            c.expect_error == expect_error && c.classified
        })
    }

    /// Rejected error cases whose pre-error events match, against the
    /// vendored (`corrected == false`) or corrected expectations.
    #[must_use]
    pub fn prefix(&self, corrected: bool) -> usize {
        count(&self.cases, |c| {
            c.classified
                && if corrected {
                    c.prefix_corrected == Some(true)
                } else {
                    c.prefix_vendored == Some(true)
                }
        })
    }

    /// Valid cases that round-trip byte-identically.
    #[must_use]
    pub fn round_trips(&self) -> usize {
        count(&self.cases, |c| {
            !c.expect_error && c.round_trip == Some(true)
        })
    }

    /// Cases on which the oracle panicked.
    #[must_use]
    pub fn panics(&self) -> usize {
        count(&self.cases, |c| c.panic.is_some())
    }

    /// Cases the adapter cut off before a verdict ([`Observation::truncated`]).
    #[must_use]
    pub fn truncations(&self) -> usize {
        count(&self.cases, |c| c.observation.truncated)
    }
}

/// Drops what `profile` cannot express from suite-notation `lines`.
#[must_use]
pub fn normalize<S: AsRef<str>>(lines: &[S], profile: Profile) -> Vec<String> {
    let mut anchors: Vec<String> = Vec::new();
    lines
        .iter()
        .map(|l| normalize_line(l.as_ref(), profile, &mut anchors))
        .collect()
}

fn normalize_line(line: &str, profile: Profile, anchors: &mut Vec<String>) -> String {
    match line {
        "+DOC ---" if !profile.doc_start_marker => return "+DOC".to_owned(),
        "-DOC ..." if !profile.doc_end_marker => return "-DOC".to_owned(),
        _ => {}
    }
    if let Some(name) = line.strip_prefix("=ALI *") {
        if profile.anchor_names {
            return line.to_owned();
        }
        return match anchors.iter().rposition(|a| a == name) {
            Some(i) => format!("=ALI *{}", i.saturating_add(1)),
            None => format!("=ALI *?{name}"),
        };
    }
    let (head, mut rest) = line.split_at_checked(4).unwrap_or((line, ""));
    if !matches!(head, "+MAP" | "+SEQ" | "=VAL") {
        return line.to_owned();
    }
    let mut out = head.to_owned();
    while let Some(after) = rest.strip_prefix(' ') {
        if head == "=VAL" && !after.starts_with(['&', '<']) {
            // The value token runs to the end of the line (it may hold spaces).
            out.push(' ');
            out.push_str(after);
            break;
        }
        let split = after.find(' ').unwrap_or(after.len());
        let (token, tail) = after.split_at_checked(split).unwrap_or((after, ""));
        rest = tail;
        match token {
            "{}" | "[]" if !profile.flow_markers => {}
            t if t.starts_with('&') && !profile.anchor_names => {
                anchors.push(t.get(1..).unwrap_or("").to_owned());
                out.push_str(&format!(" &{}", anchors.len()));
            }
            t => {
                out.push(' ');
                out.push_str(t);
            }
        }
    }
    out
}

fn panic_text(payload: &(dyn core::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// Scores `oracle` over `cases` (all of them) under the standard criterion.
pub fn score(oracle: &mut dyn Oracle, cases: &[Case], known: &KnownDefects) -> StandardReport {
    let profile = oracle.profile();
    let mut out = Vec::with_capacity(cases.len());
    for case in cases {
        let input = core::str::from_utf8(&case.input).ok();
        let observed = input.map(|i| catch_unwind(AssertUnwindSafe(|| oracle.observe(i))));
        let (observation, panic) = match observed {
            Some(Ok(o)) => (o, None),
            Some(Err(p)) => (Observation::default(), Some(panic_text(p.as_ref()))),
            None => (
                Observation::default(),
                Some("in.yaml is not UTF-8".to_owned()),
            ),
        };
        let rejected = observation.error.is_some();
        let verdict = panic.is_none() && !observation.truncated;
        let classified = verdict && rejected == case.expect_error;
        let compare = |expected: &str| -> Option<bool> {
            let (profile, events) = (profile?, observation.events.as_ref()?);
            Some(normalize(events, profile) == normalize(&lines(expected), profile))
        };
        let (mut events, mut prefix_vendored, mut prefix_corrected) = (None, None, None);
        if verdict {
            if case.expect_error {
                prefix_vendored = compare(&case.expected_events);
                prefix_corrected = match known.corrected(case) {
                    Some(fixed) => compare(&fixed),
                    None => prefix_vendored,
                };
            } else {
                events = compare(&case.expected_events);
            }
        }
        out.push(CaseScore {
            id: case.id.clone(),
            expect_error: case.expect_error,
            panic,
            classified,
            events,
            prefix_vendored,
            prefix_corrected,
            round_trip: if case.expect_error {
                None
            } else {
                observation.round_trip
            },
            observation,
        });
    }
    StandardReport {
        name: oracle.name(),
        version: oracle.version(),
        profile,
        cases: out,
    }
}

/// The version of `oneyaml-syntax`, the crate [`ParserOracle`] scores. It
/// has its own semver track, so this crate's `CARGO_PKG_VERSION`
/// (the workspace version) is not it; a test checks this against
/// `crates/oneyaml-syntax/Cargo.toml`.
pub const SYNTAX_VERSION: &str = "0.1.1";

/// Our parser under the standard criterion. Events come from the same
/// notation renderer the strict mode uses; the round trip concatenates the
/// source slices of the event stream (contract C1).
#[derive(Clone, Copy, Debug, Default)]
pub struct ParserOracle;

impl Oracle for ParserOracle {
    fn name(&self) -> &'static str {
        "oneyaml-syntax"
    }

    fn version(&self) -> &'static str {
        SYNTAX_VERSION
    }

    fn profile(&self) -> Option<Profile> {
        Some(Profile::FULL)
    }

    fn observe(&mut self, input: &str) -> Observation {
        let events = oneyaml_syntax::parse(input);
        let first_error = events.iter().position(|e| e.is_error());
        let error = first_error.map(|i| {
            events
                .get(i)
                .and_then(|e| e.diagnostic())
                .map_or_else(|| "error".to_owned(), ToString::to_string)
        });
        let rendered = match first_error {
            Some(i) => to_tree_notation_prefix(&events, i),
            None => to_tree_notation(&events),
        };
        let notation = rendered.unwrap_or_else(|e| format!("<notation error: {e}>\n"));
        let mut copy = String::with_capacity(input.len());
        for event in events.sourced() {
            copy.push_str(event.text().unwrap_or("\u{fffd}"));
        }
        Observation {
            events: Some(notation.lines().map(str::to_owned).collect()),
            error,
            round_trip: Some(copy.as_bytes() == input.as_bytes()),
            truncated: false,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::known::Correction;

    const BARE: Profile = Profile {
        doc_start_marker: false,
        doc_end_marker: false,
        flow_markers: false,
        anchor_names: false,
    };

    #[test]
    fn syntax_version_is_the_manifest_version() {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../oneyaml-syntax/Cargo.toml");
        let text = std::fs::read_to_string(manifest).unwrap();
        let version = text
            .lines()
            .find_map(|l| l.strip_prefix("version = "))
            .map(|v| v.trim_matches('"'));
        assert_eq!(version, Some(SYNTAX_VERSION));
        assert_eq!(ParserOracle.version(), SYNTAX_VERSION);
    }

    #[test]
    fn full_profile_is_identity() {
        let l = [
            "+DOC ---",
            "+MAP {} &a <t>",
            "=VAL &b :x &y <z>",
            "=ALI *a",
            "-DOC ...",
        ];
        assert_eq!(normalize(&l, Profile::FULL), l);
    }

    #[test]
    fn bare_profile_drops_markers_and_numbers_anchors() {
        let l = [
            "+DOC ---",
            "+SEQ [] &s <tag:x>",
            "=VAL &a 'q &x <y>",
            "=VAL &a :re",
            "=ALI *a",
            "=ALI *s",
            "=ALI *nope",
            "=VAL <!> :",
            "-SEQ",
            "-DOC ...",
        ];
        assert_eq!(
            normalize(&l, BARE),
            [
                "+DOC",
                "+SEQ &1 <tag:x>",
                "=VAL &2 'q &x <y>",
                "=VAL &3 :re",
                "=ALI *3",
                "=ALI *1",
                "=ALI *?nope",
                "=VAL <!> :",
                "-SEQ",
                "-DOC",
            ]
        );
        assert_eq!(BARE.dropped().len(), 4);
        assert!(Profile::FULL.dropped().is_empty());
    }

    fn case(id: &str, error: bool, events: &str) -> Case {
        Case {
            id: id.to_owned(),
            name: String::new(),
            input: b"x".to_vec(),
            expected_events: events.to_owned(),
            expect_error: error,
        }
    }

    /// Replays a fixed observation; panics when `events` is `"PANIC"`.
    struct Fixed(Observation, Option<Profile>);

    impl Oracle for Fixed {
        fn name(&self) -> &'static str {
            "fixed"
        }
        fn version(&self) -> &'static str {
            "0"
        }
        fn profile(&self) -> Option<Profile> {
            self.1
        }
        #[allow(clippy::panic)]
        fn observe(&mut self, _: &str) -> Observation {
            if self.0.error.as_deref() == Some("PANIC") {
                panic!("boom");
            }
            self.0.clone()
        }
    }

    fn obs(events: Option<&[&str]>, error: bool) -> Observation {
        Observation {
            events: events.map(|e| e.iter().map(|s| (*s).to_owned()).collect()),
            error: error.then(|| "e".to_owned()),
            round_trip: Some(true),
            truncated: false,
        }
    }

    #[test]
    fn scoring_rules() {
        let valid = case("V", false, "+STR\n-STR\n");
        let invalid = case("E", true, "+STR\n+SEQ\n");
        let mut known = KnownDefects::empty();
        known.insert("E", Correction::Events("+STR\n+SEQ []\n".to_owned()), "r");
        let cases = [valid, invalid];

        let mut accept = Fixed(obs(Some(&["+STR", "-STR"]), false), Some(Profile::FULL));
        let r = score(&mut accept, &cases, &known);
        assert_eq!((r.standard(false), r.standard(true)), ((1, 1), (0, 1)));
        assert_eq!((r.round_trips(), r.prefix(false), r.panics()), (1, 0, 0));

        let mut reject = Fixed(obs(Some(&["+STR", "+SEQ []"]), true), Some(Profile::FULL));
        let r = score(&mut reject, &cases, &known);
        assert_eq!((r.standard(false), r.standard(true)), ((0, 1), (1, 1)));
        // The correction only affects the prefix column.
        assert_eq!((r.prefix(false), r.prefix(true)), (0, 1));

        // No event API: classification only, and the verdict is not events.
        let mut blind = Fixed(obs(None, false), None);
        let r = score(&mut blind, &cases, &known);
        assert_eq!(r.classified(false), 1);
        assert_eq!(r.cases[0].events, None);

        let mut boom = Fixed(
            Observation {
                error: Some("PANIC".to_owned()),
                ..Observation::default()
            },
            None,
        );
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r = score(&mut boom, &cases, &known);
        std::panic::set_hook(hook);
        assert_eq!(r.panics(), 2);
        assert_eq!(r.standard(true).0, 0, "a panic is not a reported error");
        assert_eq!(r.cases[0].panic.as_deref(), Some("boom"));
    }

    #[test]
    fn truncation_is_a_failure_not_a_rejection() {
        let valid = case("V", false, "+STR\n-STR\n");
        let invalid = case("E", true, "+STR\n");
        let cases = [valid, invalid];
        let cut = Observation {
            events: Some(vec!["+STR".to_owned()]),
            truncated: true,
            ..Observation::default()
        };
        let mut cut_off = Fixed(cut, Some(Profile::FULL));
        let r = score(&mut cut_off, &cases, &KnownDefects::empty());
        assert_eq!(r.truncations(), 2);
        assert_eq!(r.panics(), 0);
        assert_eq!((r.standard(false).0, r.standard(true).0), (0, 0));
        assert_eq!((r.classified(false), r.classified(true)), (0, 0));
        // No prefix credit either, although the events match the prefix.
        assert_eq!((r.prefix(false), r.prefix(true)), (0, 0));
        // Even with an error message set, a truncated run is no rejection.
        let cut = Observation {
            error: Some("limit".to_owned()),
            truncated: true,
            ..Observation::default()
        };
        let r = score(&mut Fixed(cut, None), &cases, &KnownDefects::empty());
        assert_eq!(r.standard(true).0, 0);
    }

    #[test]
    fn non_utf8_input_is_a_failure() {
        let mut c = case("V", false, "+STR\n-STR\n");
        c.input = vec![0xff];
        let mut accept = Fixed(obs(Some(&["+STR", "-STR"]), false), Some(Profile::FULL));
        let r = score(&mut accept, &[c], &KnownDefects::empty());
        assert_eq!(r.standard(false).0, 0);
    }
}
