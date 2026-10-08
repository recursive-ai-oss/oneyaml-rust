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
//! `target/release/bench update docs/performance.md` regenerates every table,
//! then the README summary and its chart ([`readme`]); `bench readme`
//! regenerates only the latter, from the tables already in the file.
//! `bench pin-links <ref>` ([`pin`]) repoints every repository link of the
//! published docs (generated regions included) to a release tag, and back
//! to `main`.
//!
//! Tables come only from the default release build ([`build`]), the one
//! users compile every library with, so the comparators' numbers are
//! theirs. The `measure` profile (one codegen unit) only probes, for
//! comparing oneyaml with itself.
#![forbid(unsafe_code)]

pub mod build {
    //! Which build this `bench` binary is, and what it may do.
    //!
    //! * **`release`**, with no override this binary can see: everything,
    //!   tables included. Users compile oneyaml and the comparators with
    //!   the default release settings, so published numbers come from
    //!   that build.
    //! * **`measure`** (release settings in one codegen unit, the
    //!   workspace `Cargo.toml`): `probe` and `time` only, for comparing
    //!   oneyaml with itself before and after a change. In a release
    //!   build, how the compiler splits code into codegen units moves some
    //!   counts by up to about 1% when unrelated code changes; one unit
    //!   removes that, but it also changes every library's counts, the
    //!   comparators' by up to about 14%, so its numbers never go into a
    //!   table.
    //! * Any other build (`debug`, …): nothing that measures.

    /// The Cargo profile that built this binary (`release`, `debug`,
    /// `measure`, …), recorded by the build script.
    pub const PROFILE: &str = env!("BENCH_BUILD_PROFILE");

    /// The profile published tables come from.
    pub const RELEASE: &str = "release";

    /// The profile for comparing oneyaml with itself.
    pub const MEASURE: &str = "measure";

    /// What the build script saw of the build's settings that may move
    /// counts (an optimization level other than 3, the profile's
    /// `CARGO_PROFILE_*` variables, extra compiler flags); empty when it
    /// saw none. Overrides through `--config` or a config file are
    /// invisible to it.
    pub const NOTES: &str = env!("BENCH_BUILD_NOTES");

    /// The command that makes the build tables come from.
    pub const RELEASE_COMMAND: &str = "cargo build -p bench --release";

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

    /// `Ok` when this binary may produce tables (`update`, `table`): the
    /// release build, with no override it can see.
    ///
    /// # Errors
    /// Any other build, or a release build with a visible override, with
    /// the command that makes the right one.
    pub fn check_tables() -> Result<(), String> {
        check_tables_of(PROFILE, NOTES)
    }

    /// `Ok` when this binary may probe or time one input: the release
    /// build, or the `measure` build for comparing oneyaml with itself.
    ///
    /// # Errors
    /// Any other build, with the commands that make the right ones.
    pub fn check_probes() -> Result<(), String> {
        check_probes_of(PROFILE)
    }

    pub(crate) fn check_tables_of(profile: &str, notes: &str) -> Result<(), String> {
        if profile == RELEASE && notes.is_empty() {
            return Ok(());
        }
        let why = if profile == MEASURE {
            format!(
                "the `{MEASURE}` profile compiles every library in one codegen unit, which \
                 changes the comparators' counts by up to about 14%; it is for comparing \
                 oneyaml with itself (`probe`, `time`), never for tables"
            )
        } else if profile == RELEASE {
            format!("this release build has settings that move counts ({notes})")
        } else {
            format!("this bench binary was built with the `{profile}` profile")
        };
        Err(format!(
            "{why}; tables come only from the default release build, the one users \
             compile every library with: run `{RELEASE_COMMAND}` with no profile \
             overrides or extra compiler flags, then `target/{RELEASE}/bench`"
        ))
    }

    pub(crate) fn check_probes_of(profile: &str) -> Result<(), String> {
        if profile == RELEASE || profile == MEASURE {
            return Ok(());
        }
        Err(format!(
            "this bench binary was built with the `{profile}` profile; probes run from \
             the release build (`{RELEASE_COMMAND}`), or from the `{MEASURE}` build \
             (`cargo build -p bench --profile {MEASURE}`) to compare oneyaml with itself"
        ))
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::unwrap_used)]

        #[test]
        fn the_build_is_named_with_what_was_seen() {
            assert_eq!(super::describe("release", ""), "`release` profile");
            assert_eq!(
                super::describe("release", "opt-level 2"),
                "`release` profile (opt-level 2)"
            );
        }

        #[test]
        fn tables_come_only_from_a_plain_release_build() {
            assert!(super::check_tables_of("release", "").is_ok());
            for (profile, notes) in [
                ("measure", ""),
                (
                    "release",
                    "overridden: CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1",
                ),
                ("release", "compiler flags: -C target-cpu=native"),
                ("debug", "opt-level 0"),
                ("unknown", ""),
            ] {
                let e = super::check_tables_of(profile, notes).unwrap_err();
                assert!(e.contains("cargo build -p bench --release"), "{e}");
            }
            let e = super::check_tables_of("measure", "").unwrap_err();
            assert!(e.contains("never for tables"), "{e}");
        }

        #[test]
        fn probes_run_from_release_or_measure() {
            assert!(super::check_probes_of("release").is_ok());
            assert!(super::check_probes_of("measure").is_ok());
            for other in ["debug", "unknown"] {
                let e = super::check_probes_of(other).unwrap_err();
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
