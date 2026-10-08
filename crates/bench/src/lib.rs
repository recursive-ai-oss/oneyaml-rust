//! Parser benchmarks for oneyaml, unpublished.
//!
//! Metrics, all deterministic except wall-clock:
//!
//! * **instructions**: callgrind with `--toggle-collect` on [`measured_drain`],
//!   so only the parse (and dropping its output) is counted;
//! * **allocations**: valgrind DHAT totals, minus a [`Parser::Baseline`] run
//!   that generates the same input and skips the parse;
//! * **wall-clock**: a `std::time::Instant` harness in a child process,
//!   reported separately and never used as a gate.
//!
//! Every measurement runs in a child process (`bench probe` / `bench time`),
//! so a comparator that crashes (e.g. overflows its stack) is reported as a
//! crash instead of taking the harness down.
//!
//! The `serde-*` and `ser-*` sections ([`serde_report`]) measure `oneyaml`'s
//! serde path, reading and writing, against the YAML serde libraries on the
//! same probes; the `document` and `edits` sections ([`document`]) measure
//! `Document` parse, emit and edits against yaml-edit.
//!
//! `target/measure/bench update docs/performance.md` regenerates every table,
//! then the README summary and its chart ([`readme`]); `bench readme`
//! regenerates only the latter, from the tables already in the file.
//! `bench pin-links <ref>` ([`pin`]) repoints every repository link of the
//! published docs (generated regions included) to a release tag, and back
//! to `main`.
//!
//! Numbers come only from the `measure` build (`cargo build -p bench
//! --profile measure`, [`build::check`]): release settings in a single
//! codegen unit, so what the compiler inlines, and with it the counts,
//! does not shift when unrelated code changes.
#![forbid(unsafe_code)]

pub mod build {
    //! Which build this `bench` binary is, and the one that may measure.

    /// The Cargo profile that built this binary (`release`, `debug`,
    /// `measure`, …), recorded by the build script.
    pub const PROFILE: &str = env!("BENCH_BUILD_PROFILE");

    /// The profile every published number comes from: release settings
    /// in one codegen unit (the workspace `Cargo.toml`).
    pub const MEASURE: &str = "measure";

    /// What the build script saw of the build's settings that may move
    /// counts (an optimization level other than 3, profile overrides by
    /// environment variable, extra compiler flags); empty when it saw
    /// none. Overrides through `--config` or a config file are invisible
    /// to it.
    pub const NOTES: &str = env!("BENCH_BUILD_NOTES");

    /// How the `environment` section describes this build: the profile's
    /// name, and what [`NOTES`] saw. It never states the profile's
    /// settings, which a build can override where this binary can't see.
    pub fn description() -> String {
        describe(PROFILE, NOTES)
    }

    pub(crate) fn describe(profile: &str, notes: &str) -> String {
        if notes.is_empty() {
            format!("`{profile}` profile")
        } else {
            format!("`{profile}` profile ({notes})")
        }
    }

    /// The command that makes the measuring build.
    pub const COMMAND: &str = "cargo build -p bench --profile measure";

    /// `Ok` when this binary may measure: built with [`MEASURE`].
    ///
    /// # Errors
    /// Any other build, with the command that makes the right one: its
    /// counts move with how the compiler splits and inlines code.
    pub fn check() -> Result<(), String> {
        check_profile(PROFILE)
    }

    pub(crate) fn check_profile(profile: &str) -> Result<(), String> {
        if profile == MEASURE {
            return Ok(());
        }
        Err(format!(
            "this bench binary was built with the `{profile}` profile; numbers come only \
             from the `{MEASURE}` profile, whose single codegen unit keeps them from \
             moving with unrelated code: run `{COMMAND}`, then `target/{MEASURE}/bench`"
        ))
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        #[test]
        fn the_build_is_named_with_what_was_seen() {
            assert_eq!(super::describe("measure", ""), "`measure` profile");
            assert_eq!(
                super::describe("measure", "opt-level 2"),
                "`measure` profile (opt-level 2)"
            );
        }

        #[test]
        fn only_the_measure_build_may_measure() {
            assert!(super::check_profile("measure").is_ok());
            for other in ["release", "debug", "unknown"] {
                let e = super::check_profile(other).unwrap_err();
                assert!(e.contains("cargo build -p bench --profile measure"), "{e}");
                assert!(e.contains(other), "{e}");
            }
        }
    }
}

pub mod chart;
pub mod corpus;
pub mod document;
pub mod markdown;
pub mod models;
pub mod pin;
pub mod readme;
pub mod report;
pub mod ser_run;
pub mod serde_report;
pub mod serde_run;
pub mod valgrind;
pub mod wall;

use std::hint::black_box;

/// A parser under measurement.
///
/// Every variant drains its **whole** output for the input: no event cap and
/// no rendering, so the work measured is the parser's own. Comparators are
/// reached only through the `oracles` crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Parser {
    /// Generates the input and does nothing else; subtracted from the
    /// allocation figures of every other parser.
    Baseline,
    /// `oneyaml_syntax::events_with` under [`bench_settings`], pulled and
    /// dropped one event at a time like every event comparator: the full
    /// lossless event stream, including trivia (whitespace, comments,
    /// indicators) and diagnostics. (`parse_with` collects the same stream
    /// into an `EventList`, adding 48 bytes per event: see
    /// [`Parser::OneYamlCollect`].)
    OneYaml,
    /// `oneyaml_syntax::parse_with` under [`bench_settings`]: the same
    /// stream collected into an `EventList`, then walked and dropped. Not a
    /// table column; the `collect` section and `bench probe` use it to
    /// measure what collecting costs.
    OneYamlCollect,
    /// `oneyaml_syntax::SyntaxTree::parse_with` under [`bench_settings`]:
    /// the lossless syntax tree built from the pull parser (the tree owns a
    /// copy of the input, as a `Document` will), then a full preorder walk,
    /// then dropped. Not a table column; the `tree` section uses it.
    OneYamlTree,
    /// [`Parser::OneYamlTree`] without the drop: the tree is leaked, so
    /// DHAT's end-of-run figure is exactly what a built tree retains (its
    /// steady-state memory). Only for `bench probe` under DHAT.
    OneYamlTreeRetained,
    /// saphyr-parser 0.1.0: YAML events only (no trivia).
    Saphyr,
    /// yaml-rust2 0.13.0: YAML events only (no trivia).
    YamlRust2,
    /// libyaml via unsafe-libyaml 0.2.11: YAML events only (no trivia).
    LibYaml,
    /// yaml-edit 0.3.2: lossless rowan CST, then a full preorder walk.
    YamlEdit,
}

impl Parser {
    /// Every parser that appears in the tables (not the baseline), ours first.
    pub const MEASURED: &'static [Parser] = &[
        Parser::OneYaml,
        Parser::Saphyr,
        Parser::YamlRust2,
        Parser::LibYaml,
        Parser::YamlEdit,
    ];

    /// The parsers that are not table columns.
    pub const EXTRA: &'static [Parser] = &[
        Parser::Baseline,
        Parser::OneYamlCollect,
        Parser::OneYamlTree,
        Parser::OneYamlTreeRetained,
    ];

    /// The event-stream (non-lossless) comparators.
    pub const EVENT_COMPARATORS: &'static [Parser] =
        &[Parser::Saphyr, Parser::YamlRust2, Parser::LibYaml];

    /// Stable command-line / table name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Parser::Baseline => "baseline",
            Parser::OneYaml => "oneyaml",
            Parser::OneYamlCollect => "oneyaml-collect",
            Parser::OneYamlTree => "oneyaml-tree",
            Parser::OneYamlTreeRetained => "oneyaml-tree-retained",
            Parser::Saphyr => "saphyr",
            Parser::YamlRust2 => "yaml-rust2",
            Parser::LibYaml => "libyaml",
            Parser::YamlEdit => "yaml-edit",
        }
    }

    /// Looks a parser up by [`Parser::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Parser> {
        Parser::EXTRA
            .iter()
            .copied()
            .chain(Parser::MEASURED.iter().copied())
            .find(|p| p.name() == name)
    }
}

/// What a drain saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drained {
    /// Events produced (oneyaml: every event, trivia included; yaml-edit:
    /// CST nodes and tokens visited; others: YAML events).
    pub events: usize,
    /// The parser reported an error (oneyaml: any error event).
    pub rejected: bool,
}

/// The settings our parser is benchmarked with: the defaults, except
/// [`Limits::unlimited`](oneyaml_core::Limits::unlimited), so the deep stress
/// shapes (nesting far beyond the default `max_depth` of 1024) are parsed
/// rather than rejected and the linear-scaling check covers them.
/// Limit checks still run; they just never trip.
#[must_use]
pub const fn bench_settings() -> oneyaml_core::Settings {
    oneyaml_core::Settings::new().limits(oneyaml_core::Limits::unlimited())
}

fn from_result(r: Result<usize, String>) -> Drained {
    match r {
        Ok(events) => Drained {
            events,
            rejected: false,
        },
        Err(_) => Drained {
            events: 0,
            rejected: true,
        },
    }
}

/// Runs `parser` over `input` and drops its output.
///
/// `#[inline(never)]` and a stable name: callgrind counts instructions only
/// while this function is on the stack (`--toggle-collect=*measured_drain*`).
#[inline(never)]
#[must_use]
pub fn measured_drain(parser: Parser, input: &str) -> Drained {
    let input = black_box(input);
    let drained = match parser {
        Parser::Baseline => Drained {
            events: input.len(),
            rejected: false,
        },
        Parser::OneYaml => {
            let settings = bench_settings();
            let mut drained = Drained::default();
            for event in oneyaml_syntax::events_with(input, &settings) {
                let event = black_box(event);
                drained.events = drained.events.saturating_add(1);
                drained.rejected |= event.is_error();
            }
            drained
        }
        Parser::OneYamlCollect => {
            let list = oneyaml_syntax::parse_with(input, &bench_settings());
            let mut drained = Drained::default();
            for event in black_box(&list).iter() {
                drained.events = drained.events.saturating_add(1);
                drained.rejected |= event.is_error();
            }
            drained
        }
        Parser::OneYamlTree | Parser::OneYamlTreeRetained => {
            let tree = oneyaml_syntax::SyntaxTree::parse_with(input, &bench_settings());
            let tree = black_box(tree);
            let drained = Drained {
                events: tree.root().descendants().count(),
                rejected: tree.has_errors(),
            };
            if parser == Parser::OneYamlTreeRetained {
                std::mem::forget(tree);
            }
            drained
        }
        Parser::Saphyr => from_result(oracles::saphyr::drain(input)),
        Parser::YamlRust2 => from_result(oracles::yaml_rust2::drain(input)),
        Parser::LibYaml => from_result(oracles::libyaml::drain(input)),
        Parser::YamlEdit => {
            let d = oracles::yaml_edit::drain(input);
            Drained {
                events: d.elements,
                rejected: d.has_errors,
            }
        }
    };
    black_box(drained)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use crate::corpus::CASES;

    #[test]
    fn names_round_trip() {
        for p in Parser::EXTRA.iter().chain(Parser::MEASURED) {
            assert_eq!(Parser::from_name(p.name()), Some(*p));
        }
        assert!(!Parser::MEASURED.contains(&Parser::OneYamlCollect));
        assert_eq!(Parser::from_name("nope"), None);
        assert!(!Parser::MEASURED.contains(&Parser::Baseline));
        assert_eq!(Parser::MEASURED.first(), Some(&Parser::OneYaml));
    }

    /// Every corpus input is valid YAML to our parser, and the event stream
    /// tiles the input (lossless), so the benchmark measures the
    /// success path on exactly the bytes given.
    #[test]
    fn corpus_parses_cleanly_and_losslessly() {
        for c in CASES.iter().filter(|c| c.base <= 1024 * 1024) {
            let input = c.generate(1);
            let d = measured_drain(Parser::OneYaml, &input);
            assert!(!d.rejected, "{} has parse errors", c.name);
            assert!(d.events > 0);
            let events = oneyaml_syntax::parse_with(&input, &bench_settings());
            let tiling = oneyaml_syntax::check_tiling(&input, &events);
            assert!(tiling.is_ok(), "{}: {tiling:?}", c.name);
        }
    }

    /// Consistency check (1) on the bench corpus: every shape's syntax tree
    /// agrees with its stream and emits the input byte for byte.
    #[test]
    fn corpus_trees_round_trip() {
        for c in CASES.iter().filter(|c| c.base <= 1024 * 1024) {
            let scale = if c.kind == corpus::Kind::Stress {
                10
            } else {
                1
            };
            let input = c.generate(scale);
            let events = oneyaml_syntax::parse_with(&input, &bench_settings());
            let tree = oneyaml_syntax::SyntaxTree::from_events(events.sourced());
            let check = oneyaml_syntax::tree::check_tree(&events, &tree);
            assert!(check.is_ok(), "{}: {check:?}", c.name);
            let d = measured_drain(Parser::OneYamlTree, &input);
            assert_eq!((d.events, d.rejected), (tree.len(), false), "{}", c.name);
        }
    }

    #[test]
    fn stress_shapes_parse_at_10x() {
        for c in CASES.iter().filter(|c| c.kind == corpus::Kind::Stress) {
            let input = c.generate(10);
            assert!(
                !measured_drain(Parser::OneYaml, &input).rejected,
                "{}",
                c.name
            );
        }
    }

    /// Every comparator does real work on a small real-world input and
    /// rejects invalid input, so a column can't silently measure nothing.
    #[test]
    fn comparators_drain_and_reject() {
        let input = corpus::find("compose-merge").unwrap().generate(1);
        for p in Parser::MEASURED {
            let d = measured_drain(*p, &input);
            assert!(!d.rejected, "{} rejects compose-merge", p.name());
            assert!(d.events > 100, "{}: {d:?}", p.name());
            assert!(
                measured_drain(*p, "[a, b\n").rejected,
                "{} accepts `[a, b`",
                p.name()
            );
        }
    }

    /// The default limits reject the deep-flow shape (depth 5,120 > 1,024),
    /// which is why the benchmark lifts them.
    #[test]
    fn default_limits_reject_deep_flow() {
        let input = corpus::find("deep-flow").unwrap().generate(1);
        assert!(
            oneyaml_syntax::parse(&input)
                .iter()
                .any(oneyaml_syntax::Event::is_error)
        );
        assert!(!measured_drain(Parser::OneYaml, &input).rejected);
    }

    #[test]
    fn baseline_does_no_parsing() {
        let d = measured_drain(Parser::Baseline, "a: b\n");
        assert_eq!(
            d,
            Drained {
                events: 5,
                rejected: false
            }
        );
    }
}
