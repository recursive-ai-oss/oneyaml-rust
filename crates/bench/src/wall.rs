//! Wall-clock tables (informational, never a gate; a "periodic
//! wall-clock report"). Each timing runs in its own child process
//! (`bench time`), one after another, so runs don't disturb each other and a
//! crashing or hanging comparator becomes a cell.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::corpus::{CASES, Case, Kind};
use crate::markdown::{header, thousands};
use crate::report::{Env, SCALES, names, ratio, show, wall_scale};
use crate::valgrind::{self, Outcome};
use crate::{Parser, measured_drain};

/// Median wall-clock time of one `measured_drain` call, with the sample
/// count: at least 5 samples, then more until `budget` has elapsed (≤ 1000).
#[must_use]
pub fn wall_clock(parser: Parser, input: &str, budget: Duration) -> (Duration, usize) {
    let _ = measured_drain(parser, input); // warm-up
    let start = Instant::now();
    let mut samples = Vec::new();
    while samples.len() < 5 || (start.elapsed() < budget && samples.len() < 1000) {
        let t = Instant::now();
        let _ = std::hint::black_box(measured_drain(parser, input));
        samples.push(t.elapsed());
    }
    samples.sort_unstable();
    let median = samples.get(samples.len() / 2).copied().unwrap_or_default();
    (median, samples.len())
}

/// MB/s for `parser` on `case@scale`, timed in a child process (`bench time`).
fn throughput(
    env: &Env,
    parser: Parser,
    case: &Case,
    scale: usize,
) -> Result<Outcome<f64>, String> {
    let bytes = case.generate(scale).len();
    let scale = scale.to_string();
    let args = [case.name, scale.as_str(), "300"];
    // `run_child(program, pre_args, arg, args)`: here `program` is this binary.
    let limit = env.timeout.checked_div(4).unwrap_or_default();
    let out = valgrind::run_child(&env.exe, &["time"], Path::new(parser.name()), &args, limit)?;
    match out {
        Outcome::Ok((stdout, _)) => {
            let ns: f64 = stdout
                .split_whitespace()
                .next()
                .and_then(|w| w.parse().ok())
                .ok_or_else(|| format!("bad `time` output: {stdout:?}"))?;
            Ok(Outcome::Ok(bytes as f64 * 1e3 / ns.max(1.0)))
        }
        other => Ok(other.map(|_| 0.0)),
    }
}

fn proc_field(path: &str, prefix: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.strip_prefix(prefix)
                    .map(|v| v.trim().trim_start_matches(':').trim().to_owned())
            })
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) fn host_line() -> String {
    host_line_with("MB/s = 10⁶ bytes/s, median of ≥ 5 runs.")
}

/// The host and load-average line of a wall-clock table, then `units`
/// (what the table's numbers are), then a blank line.
pub(crate) fn host_line_with(units: &str) -> String {
    let load = proc_field("/proc/loadavg", "");
    let load: Vec<&str> = load.split_whitespace().take(3).collect();
    format!(
        "Host: {}; load average (1/5/15 min) at start: {}. {units}\n\n",
        proc_field("/proc/cpuinfo", "model name"),
        load.join(" / ")
    )
}

pub(crate) fn wall_table(env: &Env) -> Result<String, String> {
    let mut s = host_line();
    let mut cols = names(&["Case", "Bytes"], " MB/s");
    cols.push("oneyaml time ÷ fastest event parser".to_owned());
    cols.push("oneyaml time ÷ yaml-edit".to_owned());
    s.push_str(&header(&cols));
    for c in CASES {
        let k = wall_scale(c);
        // Serial on purpose: concurrent timing runs would disturb each other.
        let row: Vec<Outcome<f64>> = Parser::MEASURED
            .iter()
            .map(|p| throughput(env, *p, c, k))
            .collect::<Result<_, _>>()?;
        // Time ratios are inverse throughput ratios.
        let secs: Vec<Outcome<f64>> = row
            .iter()
            .map(|o| o.clone().map(|r| 1.0 / r.max(1e-12)))
            .collect();
        let _ = write!(
            s,
            "| {}@{k}× | {} |",
            c.name,
            thousands(c.generate(k).len() as u64)
        );
        for o in &row {
            let _ = write!(s, " {} |", show(o, |v| format!("{v:.0}")));
        }
        let _ = writeln!(
            s,
            " {} | {} |",
            ratio(&secs, Parser::EVENT_COMPARATORS),
            ratio(&secs, &[Parser::YamlEdit])
        );
    }
    Ok(s)
}

pub(crate) fn wall_scaling_table(env: &Env) -> Result<String, String> {
    let mut s = host_line();
    s.push_str(&header(&names(&["Shape"], " MB/s 1× / 10× / 100×")));
    for c in CASES.iter().filter(|c| c.kind == Kind::Stress) {
        let _ = write!(s, "| {} |", c.name);
        for p in Parser::MEASURED {
            let mut cells = Vec::new();
            for k in SCALES {
                let o = throughput(env, *p, c, k)?;
                if let Outcome::Ok(v) = o {
                    cells.push(format!("{v:.0}"));
                } else {
                    cells.push(format!("{} at {k}×", show(&o, |_| String::new())));
                    break;
                }
            }
            let _ = write!(s, " {} |", cells.join(" / "));
        }
        s.push('\n');
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_lines_name_their_units() {
        let ms = host_line_with("Median of ≥ 5 runs, in ms.");
        assert!(ms.starts_with("Host: "), "{ms}");
        assert!(ms.ends_with("Median of ≥ 5 runs, in ms.\n\n"), "{ms}");
        assert!(!ms.contains("MB/s"), "{ms}");
        assert!(host_line().ends_with("MB/s = 10⁶ bytes/s, median of ≥ 5 runs.\n\n"));
    }

    #[test]
    fn wall_clock_takes_samples() {
        let (_, n) = wall_clock(Parser::OneYaml, "a: b\n", Duration::from_millis(1));
        assert!(n >= 5);
    }
}
