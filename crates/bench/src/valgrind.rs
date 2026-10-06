//! Child-process measurements: valgrind (deterministic, the PR-gate
//! metrics) and wall-clock timing.
//!
//! The bench binary re-runs itself as `bench probe <parser> <case> <scale>`
//! (or `bench time …`), optionally under valgrind:
//!
//! * `callgrind --toggle-collect=*measured_drain*` counts instructions only
//!   inside [`crate::measured_drain`], so input generation and process
//!   start-up are excluded exactly (the method `iai-callgrind`/`gungraun`
//!   use, without their runner binary);
//! * `dhat` reports total and peak heap for the whole process; the same probe
//!   with [`crate::Parser::Baseline`] is subtracted, which leaves exactly the
//!   parser's allocations because generation is deterministic.
//!
//! This crate cannot count allocations in-process: a counting
//! `#[global_allocator]` is an `unsafe impl GlobalAlloc`, and outside the
//! `oracles` crate neither our own `unsafe` (`forbid(unsafe_code)`) nor an
//! `unsafe`-containing dependency is allowed, hence DHAT. The
//! exact per-thread allocation-count gate for the serializer lives in
//! `crates/oracles/tests/ser_allocations.rs`, with `allocation-counter`
//! as an `oracles` dev-dependency.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Probe exit code for "the parser reported an error on this input".
pub const EXIT_REJECTED: i32 = 3;

/// How one child-process measurement ended.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome<T> {
    /// The parser accepted the input; the measurement.
    Ok(T),
    /// The parser reported an error (the input is valid YAML 1.2).
    Rejected,
    /// The child died (signal, stack overflow, abort); a short reason.
    Crashed(String),
    /// The child was killed after running longer than this many seconds.
    TimedOut(u64),
}

impl<T> Outcome<T> {
    /// Maps the measurement, keeping rejections and crashes.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Outcome<U> {
        match self {
            Outcome::Ok(v) => Outcome::Ok(f(v)),
            Outcome::Rejected => Outcome::Rejected,
            Outcome::Crashed(r) => Outcome::Crashed(r),
            Outcome::TimedOut(t) => Outcome::TimedOut(t),
        }
    }

    /// The measurement, if any.
    pub fn ok(self) -> Option<T> {
        match self {
            Outcome::Ok(v) => Some(v),
            Outcome::Rejected | Outcome::Crashed(_) | Outcome::TimedOut(_) => None,
        }
    }
}

/// Runs `program pre_args… arg args…`, killing it after `timeout`, and
/// classifies the result. (`arg` is the probe binary under valgrind, or the
/// parser name when `program` is the probe binary itself.)
///
/// # Errors
/// Only if the program cannot be started or waited for.
pub fn run_child(
    program: &Path,
    pre_args: &[&str],
    arg: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Outcome<(String, String)>, String> {
    let mut child = Command::new(program)
        .args(pre_args)
        .arg(arg)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    let started = Instant::now();
    // Probe output is a few lines, far below the pipe buffer, so polling
    // before reading cannot deadlock.
    while child
        .try_wait()
        .map_err(|e| format!("wait: {e}"))?
        .is_none()
    {
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Outcome::TimedOut(timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok(match out.status.code() {
        Some(0) => Outcome::Ok((stdout, stderr)),
        Some(EXIT_REJECTED) => Outcome::Rejected,
        _ if stderr.contains("overflowed its stack") || stderr.contains("Stack overflow") => {
            Outcome::Crashed("stack overflow".to_owned())
        }
        _ => Outcome::Crashed(format!("{}", out.status)),
    })
}

fn probe(
    valgrind: &Path,
    tool_args: &[&str],
    exe: &Path,
    parser: &str,
    case: &str,
    scale: usize,
    timeout: Duration,
) -> Result<Outcome<String>, String> {
    let scale = scale.to_string();
    let outcome = run_child(
        valgrind,
        tool_args,
        exe,
        &["probe", parser, case, &scale],
        timeout,
    )?;
    Ok(outcome.map(|(_, stderr)| stderr))
}

/// Instructions executed inside `measured_drain` (callgrind `Ir`).
///
/// # Errors
/// If valgrind cannot be started, or a successful probe has no summary.
pub fn instructions(
    valgrind: &Path,
    exe: &Path,
    parser: &str,
    case: &str,
    scale: usize,
    timeout: Duration,
) -> Result<Outcome<u64>, String> {
    let args = [
        "--tool=callgrind",
        "--callgrind-out-file=/dev/null",
        "--toggle-collect=*measured_drain*",
    ];
    match probe(valgrind, &args, exe, parser, case, scale, timeout)? {
        Outcome::Ok(stderr) => parse_collected(&stderr)
            .map(Outcome::Ok)
            .ok_or_else(|| format!("no `Collected :` line in:\n{stderr}")),
        other => Ok(other.map(|_| 0)),
    }
}

/// Heap totals for a whole probe run, from DHAT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heap {
    /// Bytes allocated over the run (every block, `realloc` included).
    pub total_bytes: u64,
    /// Blocks allocated over the run.
    pub total_blocks: u64,
    /// Bytes live at the global heap peak.
    pub peak_bytes: u64,
    /// Bytes still live when the run ends (what a probe leaked on purpose,
    /// e.g. [`crate::Parser::OneYamlTreeRetained`]).
    pub end_bytes: u64,
}

impl Heap {
    /// `self - base`, field by field, saturating at zero.
    #[must_use]
    pub const fn minus(self, base: Heap) -> Heap {
        Heap {
            total_bytes: self.total_bytes.saturating_sub(base.total_bytes),
            total_blocks: self.total_blocks.saturating_sub(base.total_blocks),
            peak_bytes: self.peak_bytes.saturating_sub(base.peak_bytes),
            end_bytes: self.end_bytes.saturating_sub(base.end_bytes),
        }
    }
}

/// DHAT heap totals for one probe run.
///
/// # Errors
/// As [`instructions`].
pub fn heap(
    valgrind: &Path,
    exe: &Path,
    parser: &str,
    case: &str,
    scale: usize,
    timeout: Duration,
) -> Result<Outcome<Heap>, String> {
    let args = ["--tool=dhat", "--dhat-out-file=/dev/null"];
    match probe(valgrind, &args, exe, parser, case, scale, timeout)? {
        Outcome::Ok(stderr) => parse_dhat(&stderr)
            .map(Outcome::Ok)
            .ok_or_else(|| format!("no DHAT summary in:\n{stderr}")),
        other => Ok(other.map(|_| Heap::default())),
    }
}

/// `1,234,567` → 1234567.
fn number(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|c| *c != ',').collect();
    digits.parse().ok()
}

/// The rest of the first line containing `label`, after it.
fn after<'a>(stderr: &'a str, label: &str) -> Option<&'a str> {
    stderr
        .lines()
        .find_map(|l| l.split_once(label).map(|(_, rest)| rest.trim()))
}

/// Parses callgrind's `==pid== Collected : N`.
#[must_use]
pub fn parse_collected(stderr: &str) -> Option<u64> {
    after(stderr, "Collected :").and_then(|r| r.split_whitespace().next().and_then(number))
}

/// Parses DHAT's `Total: B bytes in N blocks`, `At t-gmax: B bytes …`, and
/// `At t-end: B bytes …`.
#[must_use]
pub fn parse_dhat(stderr: &str) -> Option<Heap> {
    let bytes_blocks = |label: &str| -> Option<(u64, u64)> {
        let rest = after(stderr, label)?;
        let mut words = rest.split_whitespace();
        let bytes = number(words.next()?)?;
        // "bytes in N blocks"
        let blocks = number(words.nth(2)?)?;
        Some((bytes, blocks))
    };
    let (total_bytes, total_blocks) = bytes_blocks("Total:")?;
    let (peak_bytes, _) = bytes_blocks("At t-gmax:")?;
    let (end_bytes, _) = bytes_blocks("At t-end:")?;
    Some(Heap {
        total_bytes,
        total_blocks,
        peak_bytes,
        end_bytes,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn parses_callgrind_summary() {
        let s = "==12== Events    : Ir\n==12== Collected : 1,234,567\n==12==\n==12== I   refs:      9,999\n";
        assert_eq!(parse_collected(s), Some(1_234_567));
        assert_eq!(parse_collected("==1== nothing\n"), None);
        assert_eq!(parse_collected("==1== Collected : lots\n"), None);
    }

    #[test]
    fn parses_dhat_summary() {
        let s = "==7== Total:     2,048 bytes in 10 blocks\n==7== At t-gmax: 1,024 bytes in 3 blocks\n==7== At t-end:   0 bytes in 0 blocks\n";
        let h = parse_dhat(s).unwrap();
        assert_eq!(
            h,
            Heap {
                total_bytes: 2048,
                total_blocks: 10,
                peak_bytes: 1024,
                end_bytes: 0
            }
        );
        assert_eq!(parse_dhat("==7== Total: 5 bytes in 1 blocks\n"), None);
        assert_eq!(parse_dhat(""), None);
    }

    #[test]
    fn heap_difference_saturates() {
        let a = Heap {
            total_bytes: 10,
            total_blocks: 2,
            peak_bytes: 1,
            end_bytes: 3,
        };
        let b = Heap {
            total_bytes: 4,
            total_blocks: 5,
            peak_bytes: 1,
            end_bytes: 1,
        };
        assert_eq!(
            a.minus(b),
            Heap {
                total_bytes: 6,
                total_blocks: 0,
                peak_bytes: 0,
                end_bytes: 2
            }
        );
    }

    #[test]
    fn missing_valgrind_is_an_error() {
        let err = instructions(
            Path::new("/nonexistent/valgrind"),
            Path::new("/bin/true"),
            "oneyaml",
            "wide-map",
            1,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.contains("cannot run"), "{err}");
    }

    /// `sh -c` stands in for the probe: exit codes map to outcomes.
    #[test]
    fn child_outcomes_are_classified() {
        let sh = Path::new("/bin/sh");
        let run = |script: &str| {
            run_child(
                sh,
                &["-c", script],
                Path::new("sh"),
                &[],
                Duration::from_secs(10),
            )
            .unwrap()
        };
        assert_eq!(
            run("echo out; echo err >&2"),
            Outcome::Ok(("out\n".to_owned(), "err\n".to_owned()))
        );
        assert_eq!(run("exit 3"), Outcome::<(String, String)>::Rejected);
        assert_eq!(
            run("echo \"thread 'main' has overflowed its stack\" >&2; kill -TERM $$"),
            Outcome::Crashed("stack overflow".to_owned())
        );
        assert!(matches!(run("exit 9"), Outcome::Crashed(r) if r.contains('9')));
        let slow = run_child(
            sh,
            &["-c", "sleep 5"],
            Path::new("sh"),
            &[],
            Duration::from_millis(100),
        );
        assert_eq!(slow.unwrap(), Outcome::TimedOut(0));
    }

    #[test]
    fn outcome_map_and_ok() {
        assert_eq!(Outcome::Ok(2).map(|x| x * 2).ok(), Some(4));
        assert_eq!(Outcome::<u8>::Rejected.map(|x| x).ok(), None);
        assert_eq!(
            Outcome::<u8>::Crashed("x".into()).map(u16::from),
            Outcome::Crashed("x".into())
        );
    }
}
