//! Command-line runner: `cargo run -p test-suite -- [OPTIONS]`.

use std::path::PathBuf;
use std::process::ExitCode;

use test_suite::producer::{PRODUCER_NAMES, by_name};
use test_suite::{
    ExpectedCodes, KnownDefects, Report, default_codes_path, default_data_dir,
    default_known_defects_path, load_suite, run_suite_with,
};

const USAGE: &str = "\
usage: yaml-test-suite [OPTIONS]

Scores an event producer against the vendored yaml-test-suite.

options:
  --case ID          run only case ID (e.g. D88J, 2G84/00; 2G84 selects all its sub-cases)
  --producer NAME    event producer to score (default: stub)
  --data DIR         suite data directory (default: the vendored copy)
  --codes FILE       expected-codes table (default: crates/test-suite/expected-codes.txt)
  --known-defects FILE
                     known suite data defects (default: crates/test-suite/known-defects.txt)
  -v, --verbose      print every case's outcome, with diffs for mismatches
  --failures         print each failing case id and reason
  --strict           exit with status 1 if any case fails
  -h, --help         show this help";

struct Options {
    case: Option<String>,
    producer: String,
    data: PathBuf,
    codes: PathBuf,
    known: PathBuf,
    verbose: bool,
    failures: bool,
    strict: bool,
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut opts = Options {
        case: None,
        producer: "stub".to_owned(),
        data: default_data_dir(),
        codes: default_codes_path(),
        known: default_known_defects_path(),
        verbose: false,
        failures: false,
        strict: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--case" => opts.case = Some(value("--case")?),
            "--producer" => opts.producer = value("--producer")?,
            "--data" => opts.data = PathBuf::from(value("--data")?),
            "--codes" => opts.codes = PathBuf::from(value("--codes")?),
            "--known-defects" => opts.known = PathBuf::from(value("--known-defects")?),
            "-v" | "--verbose" => opts.verbose = true,
            "--failures" => opts.failures = true,
            "--strict" => opts.strict = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(Some(opts))
}

fn print_report(report: &Report, producer: &str, opts: &Options) {
    for r in &report.results {
        let kind = if r.expect_error { "error" } else { "valid" };
        // Cases judged against a corrected expectation are marked.
        let mark = if r.corrected {
            " (corrected expectation)"
        } else {
            ""
        };
        match &r.outcome {
            Ok(()) if opts.verbose => println!("PASS {} [{kind}] {}{mark}", r.id, r.name),
            Ok(()) => {}
            Err(failure) if opts.verbose => {
                println!("FAIL {} [{kind}] {}{mark}: {failure}", r.id, r.name);
            }
            Err(failure) if opts.failures => {
                println!("FAIL {} [{kind}] {}{mark}", r.id, failure.label());
            }
            Err(_) => {}
        }
    }
    let (valid_passed, valid_total) = report.split(false);
    let (error_passed, error_total) = report.split(true);
    println!("producer: {producer}");
    println!(
        "passed: {}  failed: {}  total: {}",
        report.passed(),
        report.failed(),
        report.total()
    );
    println!("  valid cases: {valid_passed}/{valid_total} passed");
    println!("  error cases: {error_passed}/{error_total} passed");
    let corrected = report.corrected();
    if corrected > 0 {
        let passed = report
            .results
            .iter()
            .filter(|r| r.corrected && r.outcome.is_ok())
            .count();
        println!(
            "  judged against corrected expectations: {corrected} ({passed} passed; see known-defects.txt)"
        );
    }
    let breakdown = report.failure_breakdown();
    if !breakdown.is_empty() {
        let parts: Vec<String> = breakdown
            .iter()
            .map(|(label, n)| format!("{label}={n}"))
            .collect();
        println!("  failures by reason: {}", parts.join(" "));
    }
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(Some(opts)) => opts,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("error: {msg}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(mut producer) = by_name(&opts.producer) else {
        eprintln!(
            "error: unknown producer `{}` (known: {})",
            opts.producer,
            PRODUCER_NAMES.join(", ")
        );
        return ExitCode::from(2);
    };
    let cases = match load_suite(&opts.data) {
        Ok(cases) => cases,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    let codes = match ExpectedCodes::load(&opts.codes).and_then(|c| c.validate(&cases).map(|()| c))
    {
        Ok(codes) => codes,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    let known = match KnownDefects::load(&opts.known).and_then(|k| k.validate(&cases).map(|()| k)) {
        Ok(known) => known,
        Err(err) => {
            eprintln!("error: {err}");
            return ExitCode::from(2);
        }
    };
    let report = run_suite_with(
        producer.as_mut(),
        &cases,
        opts.case.as_deref(),
        &codes,
        &known,
    );
    if report.total() == 0 {
        eprintln!(
            "error: no case matches `{}`",
            opts.case.as_deref().unwrap_or_default()
        );
        return ExitCode::from(2);
    }
    println!("expected-codes entries: {}", codes.len());
    println!("known-defect entries: {}", known.len());
    print_report(&report, producer.name(), &opts);
    if opts.strict && report.failed() > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
