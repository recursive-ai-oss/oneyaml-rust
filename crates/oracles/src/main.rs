//! `cargo run -p oracles --release -- [--failures NAME] [--case ID]`
//!
//! Default: prints the comparison tables (markdown) for `docs/conformance.md`.
//! `--failures NAME`: lists every case the named oracle fails under the
//! standard criterion, with its observation, for spot checks.
//! `--prefix NAME`: lists rejected error cases whose pre-error events differ
//! from the vendored or corrected expectation.
//! `--case ID`: shows every oracle's observation for one case.
//! `--readme [--ref GIT-REF]`: rewrites the generated conformance line of
//! `crates/oneyaml/README.md` (`oracles::readme`) from this run; its link
//! points at `GIT-REF` (default `main`; on a tree `bench pin-links` pinned
//! to a tag, that tag, so regenerating keeps the tree on one ref). Releases
//! pin every published link with `bench pin-links <tag>`, not with `--ref`.

use std::process::ExitCode;

use oracles::report::{classification_table, standard_table};
use test_suite::producer::ParserProducer;
use test_suite::standard::{CaseScore, StandardReport, score};
use test_suite::{
    Case, ExpectedCodes, KnownDefects, default_codes_path, default_data_dir,
    default_known_defects_path, load_suite, run_suite_with,
};

fn show(case: &Case, s: &CaseScore) {
    let kind = if s.expect_error { "error" } else { "valid" };
    println!("== {} [{kind}] {}", s.id, case.name);
    println!(
        "classified={} events={:?} prefix(vendored/corrected)={:?}/{:?} round_trip={:?}",
        s.classified, s.events, s.prefix_vendored, s.prefix_corrected, s.round_trip
    );
    if let Some(p) = &s.panic {
        println!("PANIC: {p}");
    }
    if let Some(e) = &s.observation.error {
        println!("error: {e}");
    }
    println!("--- in.yaml\n{}", String::from_utf8_lossy(&case.input));
    println!("--- expected\n{}", case.expected_events);
    if let Some(ev) = &s.observation.events {
        println!("--- actual\n{}\n", ev.join("\n"));
    }
}

const USAGE: &str =
    "usage: oracles [--failures NAME | --prefix NAME | --case ID | --readme [--ref GIT-REF]]";

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cases = load_suite(&default_data_dir()).map_err(|e| e.to_string())?;
    let known = KnownDefects::load(&default_known_defects_path()).map_err(|e| e.to_string())?;
    known.validate(&cases).map_err(|e| e.to_string())?;
    // Panics are caught and scored; keep their default messages off stderr.
    std::panic::set_hook(Box::new(|_| {}));
    let reports: Vec<StandardReport> = oracles::all()
        .iter_mut()
        .map(|o| score(o.as_mut(), &cases, &known))
        .collect();
    let _ = std::panic::take_hook();

    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            let codes = ExpectedCodes::load(&default_codes_path()).map_err(|e| e.to_string())?;
            let strict = run_suite_with(&mut ParserProducer, &cases, None, &codes, &known);
            println!("{}", standard_table(&reports));
            println!("{}", classification_table(&reports));
            println!(
                "oneyaml-syntax strict (contract + exact error prefixes + pinned codes): \
                 {}/{} (valid {}/{}, error {}/{}; {} judged against corrected expectations)",
                strict.passed(),
                strict.total(),
                strict.split(false).0,
                strict.split(false).1,
                strict.split(true).0,
                strict.split(true).1,
                strict.corrected()
            );
        }
        [mode @ ("--failures" | "--prefix"), name] => {
            let r = reports
                .iter()
                .find(|r| r.name.starts_with(name))
                .ok_or_else(|| format!("unknown oracle `{name}`"))?;
            for (case, s) in cases.iter().zip(&r.cases) {
                let prefix_miss = s.classified
                    && (s.prefix_corrected == Some(false) || s.prefix_vendored == Some(false));
                if (*mode == "--failures" && !s.passed()) || (*mode == "--prefix" && prefix_miss) {
                    show(case, s);
                }
            }
        }
        ["--case", id] => {
            let i = cases
                .iter()
                .position(|c| c.id == *id)
                .ok_or_else(|| format!("unknown case `{id}`"))?;
            for r in &reports {
                if let (Some(case), Some(s)) = (cases.get(i), r.cases.get(i)) {
                    println!("#### {}", r.name);
                    show(case, s);
                }
            }
        }
        ["--readme", rest @ ..] => {
            let git_ref = match rest {
                [] => oracles::readme::DEFAULT_REF,
                ["--ref", r] => r,
                _ => return Err(USAGE.to_owned()),
            };
            oracles::readme::validate_ref(git_ref)?;
            let codes = ExpectedCodes::load(&default_codes_path()).map_err(|e| e.to_string())?;
            let strict = run_suite_with(&mut ParserProducer, &cases, None, &codes, &known);
            let ours = reports
                .first()
                .ok_or_else(|| "no oracle reports".to_owned())?;
            let release = oracles::readme::release(&default_data_dir())?;
            let c = oracles::readme::conformance(release, ours, &strict)?;
            let body = oracles::readme::line(&c, git_ref)?;
            let path = oracles::readme::write(&oracles::readme::workspace_root(), &body)?;
            eprintln!("updated {}", path.display());
        }
        _ => return Err(USAGE.to_owned()),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}
