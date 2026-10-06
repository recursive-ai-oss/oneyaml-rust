//! `bench` command line.
//!
//! ```text
//! bench update <file.md> [--section NAME]... [--valgrind PATH] [--jobs N] [--timeout SECS]
//!              [--revision TEXT] [--revision-note TEXT] [--date YYYY-MM-DD]
//!              [--ref GIT-REF]
//! bench readme [<performance.md>] [--ref GIT-REF]
//! bench pin-links <GIT-REF> [--dry-run]
//! bench table <section>  [--valgrind PATH] [--jobs N] [--timeout SECS]
//! bench probe <parser> <case> <scale>
//! bench time  <parser> <case> <scale> <budget-ms>
//! bench serde-check <lib> <workload> <case>
//! bench ser-check <lib> <workload> <case>
//! ```
//!
//! A `<parser>` of the form `serde:<lib>:<workload>` (e.g.
//! `serde:oneyaml:typed`) probes or times the serde read path instead
//! (`bench::serde_run`), `ser:<lib>:<workload>[:baseline]` the write path
//! (`bench::ser_run`), and `doc:<lib>:<op>` the `Document` layer
//! (`bench::document`); their scale must be 1.
//!
//! `update` rewrites the named sections (default: all) of a markdown file
//! between their `<!-- bench:NAME:begin/end -->` markers, then regenerates
//! the README summary and chart from the updated file (`bench::readme`).
//! `readme` does only the latter (default file: the workspace's
//! `docs/performance.md`); it measures nothing. Both write the crate
//! README's `bench:readme` region and the repo-root README's `bench:chart`
//! region. `--ref` is the git ref the
//! READMEs' chart URLs and links point at (default `main`; on a tree
//! `pin-links` pinned to a tag, that tag, so regenerating keeps every link
//! on one ref; releases pin with `pin-links`; a branch or tag name, checked by
//! `oracles::readme::validate_ref`). When `update` re-measures a section the README
//! reads, it re-records `environment` in the same run. `--revision`/`--date`
//! override what the `environment` section records, for re-recording an
//! earlier run's provenance; `--revision-note` records a reader-facing note
//! the READMEs show beside the commit instead of the recorded revision
//! text (which may carry internal detail). `probe` is what
//! runs under valgrind: generate one input, drain it once, print a summary.
//! `time` prints `<median-ns> <samples>` for the wall-clock tables. Both exit
//! with `EXIT_REJECTED` if the parser reports an error.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use bench::document::{self, DocProbe};
use bench::readme::{self, Provenance};
use bench::report::{self, Env, SECTIONS};
use bench::serde_report::{self, parse_probe_name};
use bench::serde_run::{self, SerdeLib, Workload};
use bench::valgrind::EXIT_REJECTED;
use bench::{Parser, corpus, measured_drain};

fn usage() -> String {
    format!(
        "usage:\n  bench update <file.md> [--section NAME]... [--valgrind PATH] [--jobs N] [--timeout SECS] [--revision TEXT] [--revision-note TEXT] [--date YYYY-MM-DD] [--ref GIT-REF]\n  bench readme [<performance.md>] [--ref GIT-REF]\n  bench pin-links <GIT-REF> [--dry-run]\n  bench table <section> [--valgrind PATH] [--jobs N] [--timeout SECS]\n  bench probe <parser> <case> <scale>\n  bench time <parser> <case> <scale> <budget-ms>\nsections: {}",
        SECTIONS.join(", ")
    )
}

/// Why a subcommand stopped.
enum Stop {
    Usage(String),
    Rejected,
}

impl From<String> for Stop {
    fn from(e: String) -> Stop {
        Stop::Usage(e)
    }
}

type Options = (Env, Vec<String>, Vec<String>);

fn env(opts: &[String]) -> Result<Options, String> {
    let mut valgrind = PathBuf::from("valgrind");
    let mut jobs = std::thread::available_parallelism().map_or(4, usize::from);
    let mut timeout = Duration::from_secs(100);
    let mut sections = Vec::new();
    let mut provenance = Provenance::default();
    let mut rest = Vec::new();
    let mut it = opts.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--valgrind" => valgrind = PathBuf::from(value()?),
            "--jobs" => jobs = value()?.parse().map_err(|e| format!("--jobs: {e}"))?,
            "--section" => sections.push(value()?),
            "--revision" => provenance.revision = Some(value()?),
            "--revision-note" => provenance.revision_note = Some(value()?),
            "--date" => provenance.date = Some(value()?),
            "--timeout" => {
                let secs: u64 = value()?.parse().map_err(|e| format!("--timeout: {e}"))?;
                timeout = Duration::from_secs(secs);
            }
            _ => rest.push(arg.clone()),
        }
    }
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    Ok((
        Env {
            valgrind,
            exe,
            jobs,
            timeout,
            provenance,
        },
        sections,
        rest,
    ))
}

type Input = (Parser, &'static corpus::Case, usize, String);

fn input_for(args: &[String]) -> Result<Input, String> {
    let [parser, case, scale, ..] = args else {
        return Err(usage());
    };
    let parser = Parser::from_name(parser).ok_or_else(|| format!("unknown parser `{parser}`"))?;
    let case = corpus::find(case).ok_or_else(|| format!("unknown case `{case}`"))?;
    let scale: usize = scale.parse().map_err(|e| format!("scale: {e}"))?;
    Ok((parser, case, scale, case.generate(scale)))
}

/// A `serde:<lib>:<workload>` probe: the library, workload, case and the
/// input that library reads.
type SerdeInput = (SerdeLib, Workload, &'static corpus::Case, String);

fn serde_input(args: &[String]) -> Result<Option<SerdeInput>, String> {
    let [name, case, scale, ..] = args else {
        return Err(usage());
    };
    let Some((lib, workload)) = parse_probe_name(name) else {
        return Ok(None);
    };
    let case = corpus::find(case).ok_or_else(|| format!("unknown case `{case}`"))?;
    if scale != "1" {
        return Err("serde probes run at scale 1".to_owned());
    }
    let input = serde_run::probe_input(lib, case)?;
    Ok(Some((lib, workload, case, input)))
}

/// A `doc:` probe: the probe, the case and its input.
fn doc_input(args: &[String]) -> Result<Option<(DocProbe, String)>, String> {
    let [name, case, scale, ..] = args else {
        return Err(usage());
    };
    let Some(p) = DocProbe::parse(name) else {
        return Ok(None);
    };
    let case = corpus::find(case).ok_or_else(|| format!("unknown case `{case}`"))?;
    if scale != "1" {
        return Err("document probes run at scale 1".to_owned());
    }
    Ok(Some((p, case.generate(1))))
}

fn probe(args: &[String]) -> Result<(), Stop> {
    if args.len() != 3 {
        return Err(Stop::Usage(usage()));
    }
    if let Some((p, case, data)) = serde_report::ser_input(args)? {
        let o = bench::ser_run::opts(case);
        let n = if p.baseline {
            Ok(data.len())
        } else {
            bench::ser_run::measured_drain_ser(p.lib, &data, o)
        };
        println!("{} {}: {n:?}", p.name(), case.name);
        drop(std::hint::black_box(data));
        return n.map(|_| ()).map_err(|_| Stop::Rejected);
    }
    if let Some((p, input)) = doc_input(args)? {
        let Ok(mut prepared) = document::prepare(p, &input) else {
            return Err(Stop::Rejected);
        };
        let n = document::measured_drain_doc(p, &input, &mut prepared);
        println!("{}: {} bytes, {n:?}", p.name(), input.len());
        return n.map(|_| ()).map_err(|_| Stop::Rejected);
    }
    if let Some((lib, workload, case, input)) = serde_input(args)? {
        let docs = serde_run::measured_drain_serde(lib, workload, case, &input);
        println!(
            "{} {}: {} bytes, {docs:?}",
            lib.name(),
            case.name,
            input.len()
        );
        return docs.map(|_| ()).map_err(|_| Stop::Rejected);
    }
    let (parser, case, scale, input) = input_for(args)?;
    let drained = measured_drain(parser, &input);
    println!(
        "{} {}@{scale}: {} bytes, {} events, rejected={}",
        parser.name(),
        case.name,
        input.len(),
        drained.events,
        drained.rejected
    );
    if drained.rejected {
        Err(Stop::Rejected)
    } else {
        Ok(())
    }
}

fn time(args: &[String]) -> Result<(), Stop> {
    let [_, _, _, budget] = args else {
        return Err(Stop::Usage(usage()));
    };
    let budget: u64 = budget.parse().map_err(|e| format!("budget: {e}"))?;
    if let Some((p, case, data)) = serde_report::ser_input(args)? {
        let o = bench::ser_run::opts(case);
        if p.baseline || bench::ser_run::measured_drain_ser(p.lib, &data, o).is_err() {
            return Err(Stop::Rejected);
        }
        let (median, samples) = serde_report::median(Duration::from_millis(budget), || {
            let _ = bench::ser_run::measured_drain_ser(p.lib, &data, o);
        });
        println!("{} {samples}", median.as_nanos());
        return Ok(());
    }
    if let Some((p, input)) = doc_input(args)? {
        let (median, samples) = document::wall_clock(p, &input, Duration::from_millis(budget))
            .map_err(|_| Stop::Rejected)?;
        println!("{} {samples}", median.as_nanos());
        return Ok(());
    }
    if let Some((lib, workload, case, input)) = serde_input(args)? {
        if serde_run::measured_drain_serde(lib, workload, case, &input).is_err() {
            return Err(Stop::Rejected);
        }
        let budget = Duration::from_millis(budget);
        let (median, samples) = serde_report::wall_clock(lib, workload, case, &input, budget);
        println!("{} {samples}", median.as_nanos());
        return Ok(());
    }
    let (parser, _, _, input) = input_for(args)?;
    if measured_drain(parser, &input).rejected {
        return Err(Stop::Rejected);
    }
    let (median, samples) = bench::wall::wall_clock(parser, &input, Duration::from_millis(budget));
    println!("{} {samples}", median.as_nanos());
    Ok(())
}

fn run(args: &[String]) -> Result<(), Stop> {
    let Some((command, rest)) = args.split_first() else {
        return Err(Stop::Usage(usage()));
    };
    match command.as_str() {
        "probe" => probe(rest),
        "time" => time(rest),
        "serde-check" => {
            println!("{}", serde_report::check_command(rest)?);
            Ok(())
        }
        "ser-check" => {
            println!("{}", serde_report::ser_check_command(rest)?);
            Ok(())
        }
        "table" => {
            let (env, _, rest) = env(rest)?;
            let [name] = rest.as_slice() else {
                return Err(Stop::Usage(usage()));
            };
            print!("{}", report::section(name, &env)?);
            Ok(())
        }
        "update" => {
            let (git_ref, rest) = take_ref(rest)?;
            let (env, sections, rest) = env(&rest)?;
            let [path] = rest.as_slice() else {
                return Err(Stop::Usage(usage()));
            };
            let names: Vec<&str> = if sections.is_empty() {
                SECTIONS.to_vec()
            } else {
                sections.iter().map(String::as_str).collect()
            };
            // Provenance is re-recorded with any number the README reads.
            let names = readme::with_environment(&names);
            report::update(path.as_ref(), &names, &env)?;
            write_readme(path.as_ref(), &git_ref)
        }
        "readme" => {
            let (git_ref, rest) = take_ref(rest)?;
            let root = readme::workspace_root();
            match rest.as_slice() {
                [] => write_readme(&root.join(readme::PERFORMANCE), &git_ref),
                [path] => write_readme(path.as_ref(), &git_ref),
                _ => Err(Stop::Usage(usage())),
            }
        }
        "pin-links" => pin_links(rest),
        _ => Err(Stop::Usage(usage())),
    }
}

/// Splits `--ref GIT-REF` (default [`readme::DEFAULT_REF`]) off `args`.
fn take_ref(args: &[String]) -> Result<(String, Vec<String>), String> {
    let mut git_ref = readme::DEFAULT_REF.to_owned();
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--ref" {
            git_ref = it.next().cloned().ok_or("--ref needs a value")?;
            // Before anything is measured: the ref goes into URLs and HTML.
            oracles::readme::validate_ref(&git_ref)?;
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((git_ref, rest))
}

/// `pin-links <ref> [--dry-run]`: repoints every repository link of the
/// published docs to `ref` (`bench::pin`) and reports what changed.
fn pin_links(rest: &[String]) -> Result<(), Stop> {
    let (to, dry) = match rest {
        [to] => (to, false),
        [to, flag] | [flag, to] if flag == "--dry-run" => (to, true),
        _ => return Err(Stop::Usage(usage())),
    };
    let root = readme::workspace_root();
    let plan = bench::pin::plan(&root, to)?;
    if !dry {
        bench::pin::apply(&root, &plan)?;
    }
    print!("{}", plan.report(!dry));
    Ok(())
}

/// Regenerates the README summary and chart from `performance`.
fn write_readme(performance: &std::path::Path, git_ref: &str) -> Result<(), Stop> {
    let w = readme::generate(&readme::workspace_root(), performance, git_ref)?;
    eprintln!(
        "updated {}, {}, {} and {}",
        w.readme.display(),
        w.root_readme.display(),
        w.charts[0].display(),
        w.charts[1].display()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop::Rejected) => {
            eprintln!("the parser rejected the input");
            ExitCode::from(u8::try_from(EXIT_REJECTED).unwrap_or(1))
        }
        Err(Stop::Usage(e)) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn ref_is_taken_and_checked() {
        let (r, rest) = take_ref(&args(&["x.md", "--ref", "v1.0.0"])).unwrap();
        assert_eq!((r.as_str(), rest), ("v1.0.0", args(&["x.md"])));
        let (r, _) = take_ref(&args(&[])).unwrap();
        assert_eq!(r, readme::DEFAULT_REF);
        assert!(
            take_ref(&args(&["--ref"]))
                .unwrap_err()
                .contains("needs a value")
        );
        for bad in ["", " ", "a\"b", "<x>", "a`b", "a b", "../x"] {
            assert!(take_ref(&args(&["--ref", bad])).is_err(), "{bad:?}");
        }
    }
}
